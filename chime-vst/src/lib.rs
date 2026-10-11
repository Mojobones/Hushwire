//! VST3/CLAP noise suppression plugin running Amazon Voice Focus via vf-core.
//!
//! Engines (a compiled model instance plus its latency) are built on a
//! background thread and handed to the audio thread through a lock-free
//! queue; replaced engines go back the same way to be dropped off the audio
//! thread. Until an engine is warm, the plugin passes the input through
//! delayed by the reported latency, so latency never changes under the host.

use crossbeam_queue::ArrayQueue;
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use vf_core::{Model, QuantumAdapter, Variant, VoiceFocus};

/// Fade time when the model output is switched in or out.
const FADE_SECONDS: f32 = 0.02;
/// Longest latency we preallocate the dry delay line for (192 kHz needs ~8400).
const MAX_DELAY_SECONDS: f32 = 0.25;

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
enum ModelChoice {
    #[name = "Light (c10)"]
    C10,
    #[name = "Standard (c20)"]
    C20,
    #[name = "High (c50)"]
    C50,
    #[name = "Maximum (c100)"]
    C100,
}

impl ModelChoice {
    fn variant(self) -> Variant {
        match self {
            ModelChoice::C10 => Variant::C10,
            ModelChoice::C20 => Variant::C20,
            ModelChoice::C50 => Variant::C50,
            ModelChoice::C100 => Variant::C100,
        }
    }
}

#[derive(Params)]
struct VfParams {
    #[id = "bypass"]
    bypass: BoolParam,
    #[id = "mix"]
    mix: FloatParam,
    #[id = "model"]
    model: EnumParam<ModelChoice>,
}

impl Default for VfParams {
    fn default() -> Self {
        Self {
            bypass: BoolParam::new("Bypass", false).make_bypass(),
            mix: FloatParam::new("Mix", 1.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(20.0))
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            model: EnumParam::new("Model", ModelChoice::C20),
        }
    }
}

struct Engine {
    vf: VoiceFocus,
    adapter: QuantumAdapter,
    latency: usize,
    generation: u64,
}

struct LoadRequest {
    variant: Variant,
    sample_rate: u32,
    generation: u64,
}

/// State shared between the audio thread and the loader thread.
struct Shared {
    requests: ArrayQueue<LoadRequest>,
    ready: ArrayQueue<Box<Engine>>,
    retired: ArrayQueue<Box<Engine>>,
    /// Bumped whenever a new engine is requested; stale loads are discarded.
    generation: AtomicU64,
    /// Highest generation whose load finished (successfully or not). Only one
    /// load is in flight at a time, so automating the model choice can't pile
    /// up requests.
    finished: AtomicU64,
    shutdown: AtomicBool,
}

impl Shared {
    fn load(&self, req: LoadRequest) {
        let LoadRequest { variant, sample_rate, generation } = req;
        let build = || -> anyhow::Result<Engine> {
            let model = Model::load(variant)?;
            let latency = vf_core::latency_samples(&model, sample_rate)?;
            let vf = VoiceFocus::new(&model, sample_rate)?;
            Ok(Engine { vf, adapter: QuantumAdapter::default(), latency, generation })
        };
        match build() {
            Ok(engine) if generation == self.generation.load(Ordering::Acquire) => {
                nih_log!("Voice Focus {} ready at {sample_rate} Hz, latency {} samples", variant.name(), engine.latency);
                // Only fails if the audio thread hasn't picked up two earlier engines; drop it here then.
                let _ = self.ready.push(Box::new(engine));
            }
            Ok(_) => {}
            Err(err) => nih_error!("Failed to load Voice Focus {}: {err:#}", variant.name()),
        }
        self.finished.fetch_max(generation, Ordering::AcqRel);
    }
}

/// Per-instance thread that builds engines and frees retired ones, so neither
/// happens on the audio thread. (nih-plug's shared background worker shuts
/// down for every instance once one instance with queued tasks is destroyed.)
struct Loader {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Loader {
    fn spawn(shared: Arc<Shared>) -> Loader {
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("chime-vst-loader".into())
            .spawn(move || {
                while !worker.shutdown.load(Ordering::Acquire) {
                    while let Some(req) = worker.requests.pop() {
                        worker.load(req);
                    }
                    while worker.retired.pop().is_some() {}
                    // Woken by `wake`; the timeout is only a fallback.
                    thread::park_timeout(Duration::from_millis(100));
                }
            })
            .expect("spawning loader thread");
        Loader { shared, thread: Some(thread) }
    }

    /// Real-time safe: unparking doesn't allocate.
    fn wake(&self) {
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct ChimeVst {
    params: Arc<VfParams>,
    shared: Arc<Shared>,
    loader: Loader,
    engine: Option<Box<Engine>>,
    loaded_choice: Option<ModelChoice>,
    sample_rate: u32,
    latency: usize,
    /// Samples the current engine has processed; its output is used once this
    /// passes `latency + settle`.
    engine_age: usize,
    settle: usize,
    /// 0..1 crossfade between delayed dry and model output.
    wet_gain: f32,
    fade_step: f32,
    /// Per-channel dry delay lines (preallocated, length MAX_DELAY_SECONDS).
    dry: Vec<Vec<f32>>,
    dry_pos: usize,
}

impl Default for ChimeVst {
    fn default() -> Self {
        let shared = Arc::new(Shared {
            requests: ArrayQueue::new(2),
            ready: ArrayQueue::new(2),
            retired: ArrayQueue::new(8),
            generation: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        });
        Self {
            params: Arc::new(VfParams::default()),
            loader: Loader::spawn(shared.clone()),
            shared,
            engine: None,
            loaded_choice: None,
            sample_rate: 48_000,
            latency: 0,
            engine_age: 0,
            settle: 0,
            wet_gain: 0.0,
            fade_step: 0.0,
            dry: Vec::new(),
            dry_pos: 0,
        }
    }
}

impl ChimeVst {
    fn new_request(&mut self, choice: ModelChoice) -> LoadRequest {
        self.loaded_choice = Some(choice);
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        LoadRequest { variant: choice.variant(), sample_rate: self.sample_rate, generation }
    }

    /// Picks up a freshly built engine, if any, retiring the one it replaces.
    fn adopt_ready_engine(&mut self, context: &mut impl ProcessContext<Self>) {
        let current = self.shared.generation.load(Ordering::Acquire);
        while let Some(engine) = self.shared.ready.pop() {
            if engine.generation != current {
                self.retire(engine);
                continue;
            }
            if engine.latency != self.latency {
                self.latency = engine.latency;
                context.set_latency_samples(self.latency as u32);
            }
            if let Some(old) = self.engine.replace(engine) {
                self.retire(old);
            }
            self.engine_age = 0;
            self.wet_gain = 0.0;
        }
    }

    fn retire(&mut self, engine: Box<Engine>) {
        // The queue is sized well beyond what can pile up between loader
        // runs; if it ever fills, leak rather than free on the audio thread.
        if let Err(engine) = self.shared.retired.push(engine) {
            std::mem::forget(engine);
        }
        self.loader.wake();
    }
}

impl Plugin for ChimeVst {
    const NAME: &'static str = "Chime Voice Focus";
    const VENDOR: &'static str = "ChimeVST";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
    ];

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn initialize(&mut self, layout: &AudioIOLayout, config: &BufferConfig, context: &mut impl InitContext<Self>) -> bool {
        self.sample_rate = config.sample_rate as u32;
        let channels = layout.main_output_channels.map_or(1, |c| c.get() as usize);
        let max_delay = (config.sample_rate * MAX_DELAY_SECONDS) as usize;
        self.dry = vec![vec![0.0; max_delay]; channels];
        self.dry_pos = 0;
        self.settle = self.sample_rate as usize / 5;
        self.fade_step = 1.0 / (config.sample_rate * FADE_SECONDS);

        // Drop any engine built for a previous sample rate, then build one
        // synchronously so the host sees the real latency from the start.
        // (The first run downloads the model, ~1 MB for c20.)
        self.engine = None;
        while self.shared.ready.pop().is_some() {}
        let req = self.new_request(self.params.model.value());
        self.shared.load(req);

        match self.shared.ready.pop() {
            Some(engine) => {
                self.latency = engine.latency;
                self.engine = Some(engine);
            }
            // Model unavailable (offline on first run?): pass audio through with
            // the usual latency so a later successful load doesn't shift timing.
            None => self.latency = (2651.0 * config.sample_rate / 48_000.0) as usize,
        }
        self.latency = self.latency.min(max_delay - 1);
        context.set_latency_samples(self.latency as u32);
        self.engine_age = 0;
        self.wet_gain = 0.0;
        true
    }

    fn reset(&mut self) {
        for ch in &mut self.dry {
            ch.fill(0.0);
        }
        // The model keeps internal state; let it settle again before fading in.
        self.engine_age = 0;
        self.wet_gain = 0.0;
    }

    fn process(&mut self, buffer: &mut Buffer, _aux: &mut AuxiliaryBuffers, context: &mut impl ProcessContext<Self>) -> ProcessStatus {
        let choice = self.params.model.value();
        let in_flight = self.shared.generation.load(Ordering::Acquire) != self.shared.finished.load(Ordering::Acquire);
        if self.loaded_choice != Some(choice) && !in_flight {
            let req = self.new_request(choice);
            // Can't fail: with one load in flight at most, the queue never holds more than one.
            let _ = self.shared.requests.push(req);
            self.loader.wake();
        }
        self.adopt_ready_engine(context);

        let bypass = self.params.bypass.value();
        let delay = self.latency;
        let len = self.dry.first().map_or(0, Vec::len);
        for mut frame in buffer.iter_samples() {
            let mix = self.params.mix.smoothed.next();
            let channels = frame.len();
            let mono = frame.iter_mut().map(|s| *s).sum::<f32>() / channels as f32;

            // Run the model even while bypassed so it stays warm.
            let mut wet = 0.0;
            let mut wet_ok = false;
            let result = self.engine.as_mut().map(|e| (e.adapter.tick(mono, &mut e.vf), e.latency));
            match result {
                // Voice Focus inverts polarity; flip it back so dry/wet mixing works.
                Some((Ok(y), latency)) => {
                    wet = -y;
                    self.engine_age += 1;
                    wet_ok = self.engine_age > latency + self.settle;
                }
                Some((Err(err), _)) => {
                    nih_error!("Voice Focus failed, passing audio through: {err:#}");
                    let engine = self.engine.take().unwrap();
                    self.retire(engine);
                }
                None => {}
            }
            let target = if wet_ok && !bypass { 1.0 } else { 0.0 };
            if self.wet_gain < target {
                self.wet_gain = (self.wet_gain + self.fade_step).min(1.0);
            } else if self.wet_gain > target {
                self.wet_gain = (self.wet_gain - self.fade_step).max(0.0);
            }
            let w = mix * self.wet_gain;

            let read = (self.dry_pos + len - delay) % len;
            for (ch, sample) in frame.iter_mut().enumerate() {
                let line = &mut self.dry[ch];
                line[self.dry_pos] = *sample;
                *sample = line[read] * (1.0 - w) + wet * w;
            }
            self.dry_pos = (self.dry_pos + 1) % len;
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for ChimeVst {
    const CLAP_ID: &'static str = "dev.chimevst.voice-focus";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Amazon Voice Focus noise suppression");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] =
        &[ClapFeature::AudioEffect, ClapFeature::Restoration, ClapFeature::Mono, ClapFeature::Stereo];
}

impl Vst3Plugin for ChimeVst {
    const VST3_CLASS_ID: [u8; 16] = *b"ChimeVoiceFocus1";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Fx, Vst3SubCategory::Restoration];
}

nih_export_clap!(ChimeVst);
nih_export_vst3!(ChimeVst);
