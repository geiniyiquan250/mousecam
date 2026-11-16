//! Command-line control for a running MouseCam, over the WebView2 devtools
//! protocol.
//
// Everything here goes through the app's own Tauri commands, so what a script
// sees is exactly what the UI sees - no second control channel to keep in sync,
// and nothing listening on a port in a shipped build.
//
//   node tools/mc.mjs state                 app + recorder state
//   node tools/mc.mjs record 5              record N seconds, then report
//   node tools/mc.mjs probe [dir]           health report of a take (default: newest)
//   node tools/mc.mjs clips                 list recorded takes
//   node tools/mc.mjs still <dir> <frame> <out.png>   render one frame
//   node tools/mc.mjs bake <dir>            dump the per-frame camera rect
//
// Start the app with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222.

import fs from "node:fs";

const BASE = process.env.MC_CDP || "http://127.0.0.1:9222";

async function connect() {
  const targets = await (await fetch(`${BASE}/json`)).json();
  const page = targets.find((t) => t.type === "page" && !t.url.startsWith("devtools://"));
  if (!page) throw new Error("no page target; is the app running with the debug port?");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  let id = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) {
      pending.get(m.id)(m.result);
      pending.delete(m.id);
    }
  };
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  const send = (method, params = {}) =>
    new Promise((resolve) => {
      const i = ++id;
      pending.set(i, resolve);
      ws.send(JSON.stringify({ id: i, method, params }));
    });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description || "evaluate failed");
    }
    return r.result?.value;
  };
  const invoke = (cmd, args = {}) =>
    evaluate(
      `window.__TAURI__.core.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`
    );
  return { ws, evaluate, invoke };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function report(p) {
  const head = p.good ? "✓ 无异常" : "✗ 有问题";
  console.log(`${head}  ${p.dir}`);
  console.log(
    `  帧 ${p.frames} · 补帧 ${p.duplicates} · 刻度倒退 ${p.tick_regressions} · 开头静止 ${Math.round(
      p.leading_frozen_ms
    )}ms`
  );
  console.log(
    `  时长 ${p.video_secs.toFixed(2)}s · 音频 ${p.audio_secs.toFixed(2)}s · 贴边 ${p.clamped_frames} 帧 · 丢帧 ${p.dropped}`
  );
  for (const i of p.issues) console.log(`  · ${i}`);
}

const cmd = process.argv[2];
const cdp = await connect();

try {
  switch (cmd) {
    case "state": {
      const s = await cdp.invoke("debug_snapshot");
      console.log(JSON.stringify(s, null, 2));
      break;
    }
    case "clips": {
      const clips = await cdp.invoke("list_clips");
      for (const c of clips) {
        console.log(`${c.name || c.dir}\t${(c.duration ?? 0).toFixed(2)}s\t${c.dir}`);
      }
      break;
    }
    case "probe": {
      const dir = process.argv[3];
      const p = dir ? await cdp.invoke("probe_clip", { dir }) : await cdp.invoke("probe_latest");
      report(p);
      break;
    }
    case "record": {
      const args = process.argv.slice(3);
      const synthetic = args.includes("--synthetic");
      const monIdx = args.indexOf("--monitor");
      const monitor = monIdx >= 0 ? Number(args[monIdx + 1]) : 0;
      const seconds = Number(args.find((a) => !a.startsWith("--") && !/^\d+$/.test(args[args.indexOf(a) - 1] || "")) || 5);
      await cdp.evaluate(`document.querySelector('.tab[data-stage="record"]')?.click()`);
      await sleep(600);
      const start = await cdp.invoke("record_start", {
        edit: "",
        monitor,
        hw: null,
        audioSystem: null,
        audioMic: null,
        synthetic,
      });
      console.log(`recording -> ${start}`);
      await sleep(seconds * 1000);
      const msg = await cdp.invoke("record_stop");
      console.log(`stopped: ${msg}`);
      await sleep(800);
      const p = await cdp.invoke("probe_clip", { dir: start });
      report(p);
      break;
    }
    case "bake": {
      const dir = process.argv[3];
      const edit = process.argv[4] || "";
      const b = await cdp.invoke("api_bake", { dir, edit });
      console.log(`frames ${b.frames} @ ${b.fps}fps`);
      for (let i = 0; i < Math.min(b.path.length, 12); i++) {
        const [cx, cy, w, h] = b.path[i];
        console.log(`  ${String(i).padStart(3)}  centre ${cx.toFixed(1)},${cy.toFixed(1)}  ${w.toFixed(0)}x${h.toFixed(0)}`);
      }
      break;
    }
    case "still": {
      const [dir, frame, out] = process.argv.slice(3);
      const data = await cdp.invoke("still", {
        dir,
        edit: process.argv[6] || "",
        frame: Number(frame),
        maxW: 900,
        kind: "crop",
      });
      const b64 = String(data).split(",")[1];
      fs.writeFileSync(out, Buffer.from(b64, "base64"));
      console.log(`wrote ${out}`);
      break;
    }
    default:
      console.log(
        [
          "usage: node tools/mc.mjs <command>",
          "  state             app and recorder state",
          "  record [sec] [--synthetic]   record, then run the health report",
          "  record [sec] [--synthetic]   record, then run the health report",
          "                    --synthetic: generated frames and a scripted",
          "                    cursor; no desktop or real pointer involved",
          "  probe [dir]       health report of a take (default: newest)",
          "  clips             list takes",
          "  bake <dir> [edit] per-frame camera rectangle",
          "  still <dir> <frame> <out.png> [edit]",
        ].join("\n")
      );
  }
} finally {
  cdp.ws.close();
}
