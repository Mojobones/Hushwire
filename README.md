# Hushwire

A VST3/CLAP voice noise-suppression plugin for Linux and Windows. It's an
unofficial host for **Amazon Voice Focus**, the noise-suppression model used
by the Amazon Chime SDK.

> **Not affiliated with Amazon.** Hushwire is an independent project, not
> made, endorsed or supported by Amazon. "Amazon", "Amazon Chime" and
> "Amazon Voice Focus" are Amazon's trademarks and are used here only to
> describe what the plugin works with.
>
> **The model isn't included.** Voice Focus is Amazon's proprietary software.
> Neither this repository nor its builds contain it. On first use, the
> plugin shows a notice and downloads the model from Amazon's servers only if
> you accept. Your use of the model is subject to Amazon's terms.

The model is a self-contained WebAssembly module. Hushwire runs it natively
with [wasmtime](https://wasmtime.dev/), calling it the same way the Chime
SDK's own AudioWorklet does. The output matches the browser SDK bit for bit.
No browser or helper process is involved.

## Using it

Mono and stereo tracks are supported. Stereo is summed to mono for the model,
and the cleaned signal goes to both channels.

| Parameter | |
|---|---|
| **Model** | `Light (c10)` · `Standard (c20)` (default) · `High (c50)` · `Maximum (c100)` |
| **Mix** | Dry/wet blend. Dry is latency-aligned and Voice Focus's inverted polarity is corrected, so partial mixes don't comb-filter. |
| **Bypass** | Host bypass. Latency stays constant. |

The plugin window shows these controls plus a status line: loading, active,
bypassed, or an error such as a failed download. It also shows the current
latency.

**Standalone app:** to try the plugin with your microphone without a DAW,
run `cargo run --release -p hushwire`. Add `-- --help` to see the audio
backend and device options.

- **Latency:** about 55 ms at 48 kHz (2,651 samples), reported to the host so
  it's compensated during playback and mixdown. That's too much for live
  monitoring through the plugin. Latency is measured on load for the host's
  sample rate; every rate from 8 kHz to 384 kHz works.
- **CPU use:** measured on one core at 48 kHz, about 2 % for c20 and 8 % for
  c100 of real time.
- **First load:** the plugin window shows a notice explaining that the model
  comes from Amazon. Nothing downloads until you click **Accept and
  download**; you're asked once. Models are 0.5 MB for c10 up to 8 MB for c100,
  saved into `~/.local/share/Hushwire/models` or `%APPDATA%\Hushwire\models`.
  After that it works offline. Until a model is available, or if a download
  fails, the plugin passes audio through unprocessed. A **Retry** button
  appears after a failed download. If your DAW can't show the plugin window,
  run `vf-cli fetch --accept-download-notice` once instead.
- **Switching models:** the plugin passes dry audio for about a quarter
  second while the new model loads and settles, then crossfades in.

## Building

Requires Rust (stable).

**Linux** (outputs to `target/bundled/`):
```sh
cargo xtask bundle hushwire --release
cp -r target/bundled/Hushwire.vst3 ~/.vst3/
cp target/bundled/Hushwire.clap ~/.clap/
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

**Windows, native:** `cargo xtask bundle hushwire --release` with the MSVC
toolchain should also work, but it hasn't been tested.

## Layout

| Path | |
|---|---|
| `vf-core/` | Model download/cache, the wasmtime engine, latency measurement |
| `hushwire/` | The nih-plug plugin (`editor.rs` is the egui GUI, `main.rs` the standalone app) |
| `vf-cli/` | Offline test tool: `vf-cli fetch [--accept-download-notice]`, `vf-cli latency`, `vf-cli process in.wav out.wav [c10\|c20\|c50\|c100] [--bypass]` |
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

## License

Hushwire's own code is GPLv3; see `LICENSE`. GPLv3 is required because the
VST3 build links nih-plug's GPLv3 VST3 bindings. Amazon's model isn't covered
by this license.
