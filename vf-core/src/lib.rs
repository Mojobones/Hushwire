//! Runs Amazon Voice Focus noise suppression natively with wasmtime.
//!
//! The model WASM is fetched from Amazon's CDN (the same files the Chime SDK
//! loads in the browser) and cached locally; it is never bundled. The call
//! sequence mirrors the SDK's `worklet-inline-processor-v1.js`.

use anyhow::{bail, ensure, Context, Result};
use std::path::PathBuf;
use wasmtime::{Config, Engine, Instance, Linker, Memory, Module, OptLevel, Store, TypedFunc};

/// The SDK feeds the model in Web Audio render quanta of this many frames.
pub const QUANTUM: usize = 128;
/// Internal model chunk: 10 ms at 48 kHz.
const PROCESS_FRAMES: i32 = 480;
const CDN: &str = "https://static.sdkassets.chime.aws/wasm";
const ASSET_GROUP: &str = "stable-v1";
/// AGC target level the SDK passes to `create_processor` (unused while AGC is off).
const AGC_LEVEL: i32 = -15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    C10,
    C20,
    C50,
    C100,
}

impl Variant {
    pub const ALL: [Variant; 4] = [Variant::C10, Variant::C20, Variant::C50, Variant::C100];

    pub fn name(self) -> &'static str {
        match self {
            Variant::C10 => "c10",
            Variant::C20 => "c20",
            Variant::C50 => "c50",
            Variant::C100 => "c100",
        }
    }

    pub fn parse(s: &str) -> Option<Variant> {
        Variant::ALL.into_iter().find(|v| v.name() == s)
    }

    fn file_name(self) -> String {
        format!("voicefocus-default-{}-v1_simd.wasm", self.name())
    }
}

/// Directory holding downloaded models: `<data dir>/ChimeVST/models`
/// (`~/.local/share/...` on Linux, `%APPDATA%\...` on Windows).
pub fn cache_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("no user data directory")?;
    Ok(base.join("ChimeVST").join("models"))
}

/// Returns the cached model path, downloading it from the CDN on first use.
pub fn ensure_model(variant: Variant) -> Result<PathBuf> {
    let dir = cache_dir()?;
    let path = dir.join(variant.file_name());
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let url = format!("{CDN}/{}?assetGroup={ASSET_GROUP}", variant.file_name());
    let mut resp = ureq::get(&url).call().with_context(|| format!("downloading {url}"))?;
    let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    ensure!(content_type == "application/wasm", "unexpected content type {content_type:?} from {url}");
    let bytes = resp.body_mut().with_config().limit(64 << 20).read_to_vec()?;
    ensure!(bytes.starts_with(b"\0asm"), "download from {url} is not a WASM module");

    let tmp = path.with_extension("wasm.part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// A compiled model. Compiling is slow (hundreds of ms for the larger
/// variants), so do it off the audio thread and share it between instances.
#[derive(Clone)]
pub struct Model {
    engine: Engine,
    module: Module,
}

impl Model {
    pub fn load(variant: Variant) -> Result<Model> {
        Model::from_file(&ensure_model(variant)?)
    }

    pub fn from_file(path: &std::path::Path) -> Result<Model> {
        let mut config = Config::new();
        config.cranelift_opt_level(OptLevel::Speed);
        let engine = Engine::new(&config)?;
        let module = Module::from_file(&engine, path).map_err(|e| anyhow::anyhow!("compiling {}: {e:?}", path.display()))?;
        Ok(Model { engine, module })
    }
}

/// One mono Voice Focus processor. All allocation happens in [`VoiceFocus::new`];
/// [`VoiceFocus::process_quantum`] is safe to call from the audio thread.
pub struct VoiceFocus {
    store: Store<()>,
    memory: Memory,
    step: TypedFunc<(i32, i32, i32, i32), ()>,
    provide_chunk: TypedFunc<(i32, i32), i32>,
    receive_chunk: TypedFunc<(i32, i32), i32>,
    process_chunk: TypedFunc<(i32, i32), ()>,
    audio_buffer: i32,
    processor: i32,
    in_ptr: usize,
    out_ptr: usize,
    chunk_ptr: i32,
    /// When false the audio still passes through the model's buffering (so
    /// latency is unchanged) but noise suppression is skipped.
    pub enabled: bool,
}

impl VoiceFocus {
    pub fn new(model: &Model, sample_rate: u32) -> Result<VoiceFocus> {
        let mut store = Store::new(&model.engine, ());
        let mut linker = Linker::new(&model.engine);
        // The model only imports WASI stubs; the SDK answers them with no-ops.
        let wasi = "wasi_snapshot_preview1";
        linker.func_wrap(wasi, "fd_write", |_: i32, _: i32, _: i32, _: i32| 0i32)?;
        linker.func_wrap(wasi, "fd_seek", |_: i32, _: i64, _: i32, _: i32| 0i32)?;
        linker.func_wrap(wasi, "fd_close", |_: i32| 0i32)?;
        linker.func_wrap(wasi, "proc_exit", |_: i32| {})?;
        let instance: Instance = linker.instantiate(&mut store, &model.module)?;

        let memory = instance.get_memory(&mut store, "memory").context("model exports no memory")?;
        let malloc = instance.get_typed_func::<i32, i32>(&mut store, "malloc")?;
        let create_processor = instance.get_typed_func::<(i32, i32), i32>(&mut store, "create_processor")?;

        // Matches the worklet: native path at 48 kHz, built-in resampler otherwise.
        let resample = sample_rate != 48_000;
        let (audio_buffer, step) = if resample {
            let create = instance.get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "create_audio_buffer_with_resample")?;
            let buf = create.call(&mut store, (PROCESS_FRAMES, 4, 8, sample_rate as i32))?;
            (buf, instance.get_typed_func(&mut store, "step_with_resample")?)
        } else {
            let create = instance.get_typed_func::<(i32, i32, i32), i32>(&mut store, "create_audio_buffer")?;
            let buf = create.call(&mut store, (PROCESS_FRAMES, 4, 8))?;
            (buf, instance.get_typed_func(&mut store, "step")?)
        };
        if audio_buffer == 0 {
            bail!("model refused sample rate {sample_rate}");
        }
        let processor = create_processor.call(&mut store, (0, AGC_LEVEL))?;
        ensure!(processor != 0, "create_processor failed");

        let quantum_bytes = (QUANTUM * 4) as i32;
        let in_ptr = malloc.call(&mut store, quantum_bytes)? as usize;
        let out_ptr = malloc.call(&mut store, quantum_bytes)? as usize;
        let chunk_ptr = malloc.call(&mut store, PROCESS_FRAMES * 4)?;
        ensure!(in_ptr != 0 && out_ptr != 0 && chunk_ptr != 0, "model malloc failed");

        Ok(VoiceFocus {
            step,
            provide_chunk: instance.get_typed_func(&mut store, "provide_chunk")?,
            receive_chunk: instance.get_typed_func(&mut store, "receive_chunk")?,
            process_chunk: instance.get_typed_func(&mut store, "process_fullband_chunk")?,
            store,
            memory,
            audio_buffer,
            processor,
            in_ptr,
            out_ptr,
            chunk_ptr,
            enabled: true,
        })
    }

    /// Processes one 128-frame quantum, exactly like one AudioWorklet `process()` call.
    pub fn process_quantum(&mut self, input: &[f32; QUANTUM], output: &mut [f32; QUANTUM]) -> Result<()> {
        write_f32s(self.memory.data_mut(&mut self.store), self.in_ptr, input);
        let (buf, out, inp) = (self.audio_buffer, self.out_ptr as i32, self.in_ptr as i32);
        self.step.call(&mut self.store, (buf, QUANTUM as i32, out, inp))?;
        read_f32s(self.memory.data(&self.store), self.out_ptr, output);

        if self.provide_chunk.call(&mut self.store, (buf, self.chunk_ptr))? == 1 {
            if self.enabled {
                self.process_chunk.call(&mut self.store, (self.processor, self.chunk_ptr))?;
            }
            self.receive_chunk.call(&mut self.store, (buf, self.chunk_ptr))?;
        }
        Ok(())
    }
}

/// The network's own delay at 48 kHz, on top of the buffering path. Measured
/// by cross-correlation (identical in the browser and here): 2523 samples
/// total minus the 1024-sample bypass path.
const MODEL_DELAY_48K: f64 = 1499.0;

/// Total latency in samples of a [`VoiceFocus`] driven through a
/// [`QuantumAdapter`] at `sample_rate`. The buffering/resampling path is
/// measured directly with an impulse (the model is bypassed, so it is linear);
/// the network delay is scaled from 48 kHz.
pub fn latency_samples(model: &Model, sample_rate: u32) -> Result<usize> {
    let mut vf = VoiceFocus::new(model, sample_rate)?;
    vf.enabled = false;
    let mut adapter = QuantumAdapter::default();
    // Fire the impulse after the buffers have settled; the first few chunks
    // after creation behave differently.
    let at = sample_rate as usize / 5;
    let mut peak = (0, 0.0f32);
    for i in 0..at * 2 {
        let y = adapter.tick(if i == at { 1.0 } else { 0.0 }, &mut vf)?;
        if i >= at && y.abs() > peak.1 {
            peak = (i - at, y.abs());
        }
    }
    ensure!(peak.1 > 0.1, "no impulse through the model's buffer path");
    Ok(peak.0 + (MODEL_DELAY_48K * sample_rate as f64 / 48_000.0).round() as usize)
}

fn write_f32s(mem: &mut [u8], ptr: usize, src: &[f32]) {
    for (dst, v) in mem[ptr..ptr + src.len() * 4].chunks_exact_mut(4).zip(src) {
        dst.copy_from_slice(&v.to_le_bytes());
    }
}

fn read_f32s(mem: &[u8], ptr: usize, dst: &mut [f32]) {
    let len = dst.len();
    for (v, src) in dst.iter_mut().zip(mem[ptr..ptr + len * 4].chunks_exact(4)) {
        *v = f32::from_le_bytes(src.try_into().unwrap());
    }
}

/// Adapts arbitrary host block sizes to the model's fixed 128-frame quantum,
/// adding exactly [`QUANTUM`] frames of latency.
pub struct QuantumAdapter {
    input: [f32; QUANTUM],
    output: [f32; QUANTUM],
    pos: usize,
}

impl Default for QuantumAdapter {
    fn default() -> Self {
        QuantumAdapter { input: [0.0; QUANTUM], output: [0.0; QUANTUM], pos: 0 }
    }
}

impl QuantumAdapter {
    /// Pushes one sample and returns the processed sample from `QUANTUM` frames ago.
    #[inline]
    pub fn tick(&mut self, x: f32, vf: &mut VoiceFocus) -> Result<f32> {
        self.input[self.pos] = x;
        let y = self.output[self.pos];
        self.pos += 1;
        if self.pos == QUANTUM {
            self.pos = 0;
            vf.process_quantum(&self.input, &mut self.output)?;
        }
        Ok(y)
    }
}
