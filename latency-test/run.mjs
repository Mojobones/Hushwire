// Measures Amazon Voice Focus latency by running it in headless Chromium and
// cross-correlating the dry input against the processed output.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { chromium } from 'playwright';

const ROOT = path.resolve('public');
const OUT = path.resolve('results');
fs.mkdirSync(OUT, { recursive: true });

const types = { '.html': 'text/html', '.js': 'text/javascript', '.wav': 'audio/wav' };
const server = http.createServer((req, res) => {
  const file = path.join(ROOT, path.normalize(req.url.split('?')[0]).replace(/^\/+$/, '/index.html'));
  if (!file.startsWith(ROOT) || !fs.existsSync(file)) { res.writeHead(404).end(); return; }
  res.writeHead(200, {
    'Content-Type': types[path.extname(file)] || 'application/octet-stream',
    // Cross-origin isolation so the SharedArrayBuffer worker mode is available.
    'Cross-Origin-Opener-Policy': 'same-origin',
    'Cross-Origin-Embedder-Policy': 'credentialless',
  });
  fs.createReadStream(file).pipe(res);
}).listen(0, '127.0.0.1');
await new Promise((r) => server.once('listening', r));
const url = `http://127.0.0.1:${server.address().port}/index.html`;

// Lag (in samples) of wet relative to dry, via normalized cross-correlation over
// the loudest 1.5s window of the dry signal.
function measureLag(dry, wet, maxLag = 9600) {
  const W = 72000, hop = 4800;
  let best = 0, bestE = -1;
  for (let s = 0; s + W + maxLag < dry.length; s += hop) {
    let e = 0;
    for (let i = s; i < s + W; i += 8) e += dry[i] * dry[i];
    if (e > bestE) { bestE = e; best = s; }
  }
  let ed = 0;
  for (let i = best; i < best + W; i++) ed += dry[i] * dry[i];
  let peak = 0, lag = 0;
  for (let k = 0; k <= maxLag; k++) {
    let c = 0, ew = 0;
    for (let i = best; i < best + W; i++) { const w = wet[i + k]; c += dry[i] * w; ew += w * w; }
    const r = ew > 0 ? c / Math.sqrt(ed * ew) : 0;
    if (Math.abs(r) > Math.abs(peak)) { peak = r; lag = k; }
  }
  return { lag, corr: peak };
}

function writeWav(file, samples, rate = 48000) {
  const buf = Buffer.alloc(44 + samples.length * 4);
  buf.write('RIFF', 0); buf.writeUInt32LE(36 + samples.length * 4, 4); buf.write('WAVE', 8);
  buf.write('fmt ', 12); buf.writeUInt32LE(16, 16); buf.writeUInt16LE(3, 20); buf.writeUInt16LE(1, 22);
  buf.writeUInt32LE(rate, 24); buf.writeUInt32LE(rate * 4, 28); buf.writeUInt16LE(4, 32); buf.writeUInt16LE(32, 34);
  buf.write('data', 36); buf.writeUInt32LE(samples.length * 4, 40);
  samples.forEach((v, i) => buf.writeFloatLE(v, 44 + i * 4));
  fs.writeFileSync(file, buf);
}

const browser = await chromium.launch({ args: ['--autoplay-policy=no-user-gesture-required'] });
const page = await browser.newPage();
page.on('console', (m) => { if (m.type() === 'error' || m.type() === 'warning') console.log(`  [page ${m.type()}] ${m.text()}`); });
page.on('pageerror', (e) => console.log(`  [page error] ${e.message}`));
await page.goto(url);
await page.waitForFunction(() => window.ready);
console.log('crossOriginIsolated:', await page.evaluate(() => crossOriginIsolated));

const cases = [];
for (const execution of ['inline', 'worker-sab', 'worker-postMessage'])
  for (const variant of ['c10', 'c20', 'c50', 'c100']) cases.push({ execution, variant });
cases.push({ execution: 'inline', variant: 'c20', enabled: false });

const rows = [];
for (const c of cases) {
  const label = `${c.execution}/${c.variant}${c.enabled === false ? '/bypass' : ''}`;
  process.stdout.write(`${label} ... `);
  let r;
  try { r = await page.evaluate((c) => window.runTest(c), c); }
  catch (e) { console.log('FAILED:', e.message.split('\n')[0]); continue; }
  if (r.error) { console.log(r.error); continue; }
  const dry = Float32Array.from(r.dry), wet = Float32Array.from(r.wet);
  const { lag, corr } = measureLag(dry, wet);
  const row = {
    label, processor: r.processor, variant: r.variant, simd: r.simd,
    lagSamples: lag, lagMs: +(lag / 48).toFixed(2), corr: +corr.toFixed(3),
    cpuWarnings: r.cpuWarnings,
    modelLatencyMsAvg: r.metrics?.latencyMillisAverage ?? null,
    lateInvoke: r.metrics?.cpu?.lateInvoke ?? null, longInvoke: r.metrics?.cpu?.longInvoke ?? null,
  };
  rows.push(row);
  console.log(`${row.lagMs} ms (${lag} samples, r=${row.corr}) cpuWarnings=${row.cpuWarnings} modelLatencyAvg=${row.modelLatencyMsAvg}`);
  writeWav(path.join(OUT, `${label.replaceAll('/', '_')}.wav`), wet);
  if (!fs.existsSync(path.join(OUT, 'dry.wav'))) writeWav(path.join(OUT, 'dry.wav'), dry);
}
fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(rows, null, 2));
await browser.close();
server.close();
