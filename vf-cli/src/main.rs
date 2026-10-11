//! Offline test tool for vf-core.
//!
//!   vf-cli fetch [--accept-download-notice]       download all model variants
//!   vf-cli latency                                plugin latency at common sample rates
//!   vf-cli process <in.wav> <out.wav> [c10|c20|c50|c100] [--bypass]

use anyhow::{bail, Context, Result};
use std::time::Instant;
use vf_core::{Model, QuantumAdapter, Variant, VoiceFocus, QUANTUM};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fetch") => {
            if !vf_core::download_accepted() {
                if !args.iter().any(|a| a == "--accept-download-notice") {
                    bail!("{}\n\nRerun with --accept-download-notice to accept and download.", vf_core::DOWNLOAD_NOTICE);
                }
                vf_core::accept_download()?;
            }
            for v in Variant::ALL {
                println!("{} -> {}", v.name(), vf_core::ensure_model(v)?.display());
            }
            Ok(())
        }
        Some("latency") => {
            let model = Model::load(Variant::C20)?;
            for rate in [16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
                println!("{rate:>6} Hz: {} samples", vf_core::latency_samples(&model, rate)?);
            }
            Ok(())
        }
        Some("process") if args.len() >= 3 => {
            let variant = args.get(3).filter(|a| !a.starts_with("--")).map_or(Some(Variant::C20), |s| Variant::parse(s));
            let variant = variant.context("unknown variant")?;
            process(&args[1], &args[2], variant, args.iter().any(|a| a == "--bypass"))
        }
        _ => bail!("usage: vf-cli fetch [--accept-download-notice] | vf-cli latency | vf-cli process <in.wav> <out.wav> [c10|c20|c50|c100] [--bypass]"),
    }
}

fn read_mono(path: &str) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {path}"))?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let ch = spec.channels as usize;
    let mono = interleaved.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    Ok((mono, spec.sample_rate))
}

fn process(input: &str, output: &str, variant: Variant, bypass: bool) -> Result<()> {
    let (dry, rate) = read_mono(input)?;

    let t = Instant::now();
    let model = Model::load(variant)?;
    println!("compile {}: {:.0} ms", variant.name(), t.elapsed().as_secs_f64() * 1e3);
    let mut vf = VoiceFocus::new(&model, rate)?;
    vf.enabled = !bypass;

    // Pad with one second of silence so the delayed tail comes out.
    let mut adapter = QuantumAdapter::default();
    let t = Instant::now();
    let wet: Vec<f32> = dry
        .iter()
        .copied()
        .chain(std::iter::repeat_n(0.0, rate as usize))
        .map(|x| adapter.tick(x, &mut vf))
        .collect::<Result<_>>()?;
    let secs = t.elapsed().as_secs_f64();
    let audio_secs = wet.len() as f64 / rate as f64;
    println!(
        "cpu: {:.1} ms for {:.1} s of audio ({:.2}% of real time, {:.1} us per {QUANTUM}-frame quantum)",
        secs * 1e3,
        audio_secs,
        100.0 * secs / audio_secs,
        1e6 * secs / (wet.len() / QUANTUM) as f64
    );

    let (lag, corr) = measure_lag(&dry, &wet, rate as usize / 5);
    println!(
        "latency: {lag} samples = {:.2} ms (incl. {QUANTUM}-frame adapter), correlation {corr:.3}",
        lag as f64 * 1e3 / rate as f64
    );

    let spec = hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut writer = hound::WavWriter::create(output, spec)?;
    for s in &wet {
        writer.write_sample(*s)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Lag of `wet` behind `dry`, by normalized cross-correlation over the
/// loudest 1.5 s window of `dry`.
fn measure_lag(dry: &[f32], wet: &[f32], max_lag: usize) -> (usize, f64) {
    let w = dry.len().min(wet.len().saturating_sub(max_lag)).min(72_000);
    let energy = |s: usize| dry[s..s + w].iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
    let start = (0..=dry.len() - w).step_by(4800).max_by(|a, b| energy(*a).total_cmp(&energy(*b))).unwrap_or(0);
    let ed = energy(start);
    let mut best = (0, 0.0f64);
    for k in 0..=max_lag {
        let (mut c, mut ew) = (0.0f64, 0.0f64);
        for i in start..start + w {
            let x = wet.get(i + k).copied().unwrap_or(0.0) as f64;
            c += dry[i] as f64 * x;
            ew += x * x;
        }
        let r = if ew > 0.0 { c / (ed * ew).sqrt() } else { 0.0 };
        if r.abs() > best.1.abs() {
            best = (k, r);
        }
    }
    best
}
