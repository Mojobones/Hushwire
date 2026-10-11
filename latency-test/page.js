import { VoiceFocus } from 'amazon-chime-sdk-js/libs/voicefocus/voicefocus.js';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const quiet = { debug() {}, info() {}, warn: console.warn, error: console.error };

// Speech clip, decoded once.
let speech;
async function loadSpeech(ctx) {
  if (!speech) speech = await (await fetch('/speech.wav')).arrayBuffer();
  return ctx.decodeAudioData(speech.slice(0));
}

window.runTest = async ({ variant, execution, enabled = true, noiseLevel = 0.03 }) => {
  const ctx = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
  await ctx.resume();
  await ctx.audioWorklet.addModule('/recorder.js');

  let cpuWarnings = 0;
  const config = await VoiceFocus.configure(
    { variant, executionPreference: execution, simd: 'detect' },
    { logger: quiet },
  );
  if (!config.supported) return { error: `unsupported: ${config.reason}` };
  const vf = await VoiceFocus.init(config, {
    delegate: { onCPUWarning() { cpuWarnings++; } },
    preload: true,
    logger: quiet,
  });
  const vfNode = await vf.createNode(ctx, { enabled, agc: { useVoiceFocusAGC: false } });

  // Dry signal = speech + white noise. Both dry and wet go into one recorder
  // so they are captured in the same render quantum.
  const clip = await loadSpeech(ctx);
  const len = clip.length + ctx.sampleRate; // 1s tail
  const dry = ctx.createBuffer(1, len, ctx.sampleRate);
  const d = dry.getChannelData(0), s = clip.getChannelData(0);
  for (let i = 0; i < len; i++) d[i] = (i < s.length ? s[i] : 0) + noiseLevel * (Math.random() * 2 - 1);

  const merger = ctx.createChannelMerger(2);
  const recorder = new AudioWorkletNode(ctx, 'recorder', { numberOfInputs: 1, channelCount: 2, channelCountMode: 'explicit' });
  merger.connect(recorder);
  vfNode.connect(merger, 0, 1);

  // Let the model download/compile, feeding silence meanwhile.
  await sleep(4000);

  const src = ctx.createBufferSource();
  src.buffer = dry;
  src.connect(merger, 0, 0);
  src.connect(vfNode);
  recorder.port.postMessage('start');
  src.start();
  await sleep((len / ctx.sampleRate) * 1000 + 300);

  const rec = await new Promise((resolve) => {
    recorder.port.onmessage = (e) => resolve(e.data);
    recorder.port.postMessage('stop');
  });
  const metrics = vfNode.getModelMetrics();
  const result = {
    processor: config.processor,
    variant: config.model.variant,
    simd: config.model.simd,
    executionQuanta: config.executionQuanta ?? null,
    baseLatency: ctx.baseLatency,
    cpuWarnings,
    metrics,
    dry: Array.from(rec[0]),
    wet: Array.from(rec[1]),
  };
  await vfNode.stop?.();
  await vf.destroy();
  await ctx.close();
  return result;
};
window.ready = true;
