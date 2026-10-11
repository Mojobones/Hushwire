# ChimeVST

A VST3/CLAP noise-suppression plugin for Linux and Windows that runs
**Amazon Voice Focus**, the model behind Amazon Chime's noise cancellation.

The model is a self-contained WebAssembly module. The plugin runs it
natively with [wasmtime](https://wasmtime.dev/), calling it the same way the
Chime SDK's own AudioWorklet does. The output matches the browser SDK
bit for bit. No browser or helper process is involved.

> **Licensing:** the model is Amazon's proprietary code, covered by the AWS
> terms. It is **not** bundled with this repo. The plugin downloads it from
> Amazon's CDN on first use, the same way the SDK does. Treat this as a
> personal tool: don't redistribute the model files or builds that contain them.
> The VST3 build also links nih-plug's GPLv3 VST3 bindings.

## Using it

Mono and stereo tracks are supported. Stereo is summed to mono for the model,
and the cleaned signal goes to both channels.

| Parameter | |
|---|---|
| **Model** | `Light (c10)` · `Standard (c20)` (default) · `High (c50)` · `Maximum (c100)` |
| **Mix** | Dry/wet blend. Dry is latency-aligned and Voice Focus's inverted polarity is corrected, so partial mixes don't comb-filter. |
| **Bypass** | Host bypass. Latency stays constant. |

- **Latency:** about 55 ms at 48 kHz (2,651 samples), reported to the host so
  it's compensated during playback and mixdown. That's too much for live
  monitoring through the plugin. Latency is measured on load for the host's
  sample rate; every rate from 8 kHz to 384 kHz works.
- **CPU use:** measured on one core at 48 kHz, about 2 % for c20 and 8 % for
  c100 of real time.
- **First load:** downloads the selected model (0.5 MB for c10 up to 8 MB for
  c100) into `~/.local/share/ChimeVST/models` or `%APPDATA%\ChimeVST\models`.
  After that it works offline. If the download fails, the plugin passes audio
  through unprocessed.
- **Switching models:** the plugin passes dry audio for about a quarter
  second while the new model loads and settles, then crossfades in.

## Building

Requires Rust (stable).

**Linux** (outputs to `target/bundled/`):
```sh
cargo xtask bundle chime-vst --release
cp -r "target/bundled/Chime Voice Focus.vst3" ~/.vst3/
cp "target/bundled/Chime Voice Focus.clap" ~/.clap/
```

**Windows, cross-compiled from Linux** (outputs to `target/bundled-windows/`):
```sh
rustup target add x86_64-pc-windows-gnullvm
# Needs llvm-mingw unpacked at tools/llvm-mingw
# (https://github.com/mstorsjo/llvm-mingw/releases, ucrt-ubuntu-x86_64 build).
./bundle-windows.sh
```
Copy the `.vst3` folder to `C:\Program Files\Common Files\VST3\` and the
`.clap` file to `C:\Program Files\Common Files\CLAP\`. The DLL only depends on
standard Windows 10+ system libraries.

**Windows, native:** `cargo xtask bundle chime-vst --release` with the MSVC
toolchain should also work, but it hasn't been tested.

## Layout

| Path | |
|---|---|
| `vf-core/` | Model download/cache, the wasmtime engine, latency measurement |
| `chime-vst/` | The nih-plug plugin |
| `vf-cli/` | Offline test tool: `vf-cli fetch`, `vf-cli latency`, `vf-cli process in.wav out.wav [c10\|c20\|c50\|c100] [--bypass]` |
| `latency-test/` | The original in-browser measurement harness (headless Chromium + Chime SDK) |
| `tools/` | clap-validator binaries (Linux/Windows) and llvm-mingw |

## Testing status

- `vf-cli` output is identical to the Chime SDK running in Chromium. Chunk
  alignment differs, so it's offset by a few samples; the residual is −114 dB.
- The built VST3, hosted in pedalboard, produces output identical to `vf-cli`
  (−277 dB residual). The host compensates latency correctly. With Mix at 0 %
  or Bypass on, the output is the exact input.
- clap-validator on Linux, and on the Windows build under Wine: everything
  plugin-specific passes, including no allocations on the audio thread
  (checked in debug builds). Four state-related tests fail, but nih-plug's
  stock `gain` example fails the same four with this clap-validator version,
  so it's an upstream issue.
- Not yet tried in a real DAW, or on real Windows.
