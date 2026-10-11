class Recorder extends AudioWorkletProcessor {
  constructor() {
    super();
    this.on = false;
    this.chunks = [[], []];
    this.port.onmessage = (e) => {
      if (e.data === 'start') this.on = true;
      if (e.data === 'stop') {
        this.on = false;
        const out = this.chunks.map((c) => {
          const a = new Float32Array(c.reduce((n, x) => n + x.length, 0));
          let o = 0;
          for (const x of c) { a.set(x, o); o += x.length; }
          return a;
        });
        this.port.postMessage(out);
        this.chunks = [[], []];
      }
    };
  }
  process(inputs) {
    if (this.on) {
      const inp = inputs[0];
      for (let ch = 0; ch < 2; ch++) this.chunks[ch].push(new Float32Array(inp[ch] || 128));
    }
    return true;
  }
}
registerProcessor('recorder', Recorder);
