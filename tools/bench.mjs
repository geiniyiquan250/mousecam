const [,, mode, seconds] = process.argv;
const base = "http://127.0.0.1:9222";
const ts = await (await fetch(`${base}/json`)).json();
const page = ts.find((t) => t.type === "page" && !t.url.startsWith("devtools://"));
const ws = new WebSocket(page.webSocketDebuggerUrl);
let id = 0; const pend = new Map();
ws.onmessage = (e) => { const m = JSON.parse(e.data); if (m.id && pend.has(m.id)) { pend.get(m.id)(m.result); pend.delete(m.id); } };
await new Promise((r) => (ws.onopen = r));
const send = (m, p = {}) => new Promise((r) => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method: m, params: p })); });
const ev = async (x) => { const r = await send("Runtime.evaluate", { expression: x, returnByValue: true, awaitPromise: true }); return r.result?.value; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
await ev(`document.querySelector('.tab[data-tab="record"]')?.click()`);
await sleep(600);
await ev(`window.__TAURI__.core.invoke('record_start', { edit: '', monitor: 0, hw: null, audioSystem: false, audioMic: false, synthetic: ${mode === 'synthetic'} })`);
const rows = [];
const t0 = Date.now();
while (Date.now() - t0 < Number(seconds) * 1000) {
  await sleep(300);
  const s = await ev(`window.__TAURI__.core.invoke('debug_snapshot')`);
  if (s && s.recording && s.frames > 15) rows.push(s);
}
await ev(`window.__TAURI__.core.invoke('record_stop')`);
const med = (f) => { const a = rows.map(f).sort((x, y) => x - y); return a.length ? a[Math.floor(a.length / 2)] : 0; };
console.log(`${mode.padEnd(9)} 抓取 ${med(r=>r.grab_ms).toFixed(1)}  母版 ${med(r=>r.master_ms).toFixed(1)}  重采样 ${med(r=>r.resample_ms).toFixed(1)}  绘制 ${med(r=>r.write_ms).toFixed(1)}  跟拍编码 ${med(r=>r.camera_ms).toFixed(1)} | 单帧 ${med(r=>r.last_frame_ms).toFixed(1)}ms  p95 ${med(r=>r.p95_frame_ms).toFixed(1)}ms`);
ws.close();
