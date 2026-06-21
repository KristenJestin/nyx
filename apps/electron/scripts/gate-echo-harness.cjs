#!/usr/bin/env node
/**
 * GATE PHASE 4 — instrumented, reproducible ECHO + FPS measurement harness for the
 * REAL Electron front (task 01KVGHVR631AY8KANFGANWDFZV).
 *
 * WHAT IT MEASURES (the gate's done-criteria)
 *   The phase-4 gate re-validates, ON THE TARGET HARDWARE, that the migration killed
 *   the Tauri/WebKitGTK echo lag. This harness drives the SAME end-to-end path the
 *   shipped app uses and times it from the renderer's single clock:
 *
 *     keystroke (renderer)  ──window.nyxCore.invoke('pty_write')──►  main relay
 *        ──► core-host (Node)  ──► nyx-napi  ──► PTY  ──► shell echoes the byte
 *        ──► Rust pump  ──► host event  ──► main relay  ──► renderer `pty://output`
 *        ──► the byte is handed to xterm.write
 *
 *   It records, per condition, the echo latency (`performance.now()` delta between
 *   issuing the write and the marker byte arriving on the output channel that feeds
 *   xterm), over ≥ N samples (default 120 ≥ the gate's 100):
 *     (a) AT REST          — quiescent renderer.
 *     (b) DURING ANIMATION — a continuous Motion-style transform/opacity animation
 *                            runs on a layer of DOM cards while we type.
 *     (c) UNDER FLOOD      — a `yes`-style flood runs through the SAME flow-control
 *                            credit loop the real front uses, and we still type.
 *   Plus a 10 s rAF FPS run during the animation (mean fps + worst frame).
 *
 * WHY rAF, NOT setTimeout (annexe §E, verbatim): under flood the `setTimeout`
 *   macrotask queue is STARVED by the IPC torrent while the render pipeline (rAF)
 *   keeps priority. The probe schedules every "type the next marker" tick on
 *   requestAnimationFrame so the flood condition does not hang the measurement.
 *
 * REALNESS: the probe runs INSIDE the renderer and speaks ONLY `window.nyxCore`
 *   (the deep-frozen preload allowlist) — exactly what `apps/frontend`'s electron
 *   adapter uses. It boots the REAL `CoreHost`, the REAL `registerCoreIpc` relay, the
 *   REAL preload, and the REAL napi PTY. No mock, no shortcut: the number it prints is
 *   the number the user feels.
 *
 * ──────────────────────────────────────────────────────────────────────────────────
 * ⚠ PLATFORM REALITY — READ THIS BEFORE TRUSTING ANY NUMBER
 * ──────────────────────────────────────────────────────────────────────────────────
 * The gate's THRESHOLDS (echo ≤5/10 ms rest+anim, ≤25/40 ms flood, mean ≥110 fps on a
 * 120 Hz screen, no frame >50 ms) are defined for the user's TARGET machine:
 * Linux / Wayland / Hyprland, 2880×1800 @120 Hz. They can ONLY be validated there.
 *
 *   • On a HEADLESS or non-120 Hz host the FPS ceiling is whatever the offscreen
 *     compositor ticks at (often 60, sometimes unthrottled/garbage) — NOT 120.
 *   • On Windows the PTY spawns PowerShell/cmd over ConPTY, whose echo characteristics
 *     differ from a Linux PTY; the absolute numbers are NOT comparable to the target.
 *
 * Therefore this harness has two modes:
 *   --mode=gate   (default) Asserts the thresholds. INTENDED FOR THE TARGET MACHINE.
 *                 Off-target it will print numbers and FAIL — that failure is EXPECTED
 *                 and is NOT the validation; only a run on the target machine counts.
 *   --mode=sanity Runs the exact same measurement but does NOT assert thresholds.
 *                 Use this for a clearly-labelled Windows/headless SANITY run that
 *                 proves the harness drives the real path and produces numbers, while
 *                 making zero claim about the gate.
 *
 * REPRODUCTION (the command to run on the TARGET machine — see GATE-PHASE4.md):
 *   bun run --filter @nyx/electron build        # build main + renderer + copy napi
 *   cd apps/electron && electron scripts/gate-echo-harness.cjs --mode=gate
 *
 *   Knobs (env): NYX_GATE_SAMPLES (default 120), NYX_GATE_FPS_MS (default 10000),
 *   NYX_GATE_OUT (path to also write the JSON report).
 *
 * Run it under Electron's CHROME renderer (it needs a real WebContents):
 *   electron scripts/gate-echo-harness.cjs [--mode=gate|sanity]
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const { app, BrowserWindow } = require("electron");

const { registerWindowIpc } = require("../dist/main/window-ipc.js");
const { registerCoreIpc } = require("../dist/main/core-ipc.js");
const { CoreHost } = require("../dist/main/core-host.js");

// ── CLI / env knobs ────────────────────────────────────────────────────────────────
const MODE = (argFlag("--mode") || process.env.NYX_GATE_MODE || "gate").toLowerCase();
const SAMPLES = intEnv("NYX_GATE_SAMPLES", 120); // ≥100 per the gate
const FPS_MS = intEnv("NYX_GATE_FPS_MS", 10_000); // 10 s Motion animation
// Per-condition wall-clock budget: caps each echo condition so a non-echoing shell
// (e.g. a Windows ConPTY sanity run) terminates instead of hanging. On the Linux
// target every sample lands in <10 ms so the budget is never the limiter.
const COND_BUDGET_MS = intEnv("NYX_GATE_COND_BUDGET_MS", 25_000);
const OUT = argFlag("--out") || process.env.NYX_GATE_OUT || "";

// The gate thresholds (from the task's done_criteria, verbatim values).
const THRESHOLDS = {
  restAnimEchoMedianMs: 5,
  restAnimEchoP95Ms: 10,
  floodEchoMedianMs: 25,
  floodEchoP95Ms: 40,
  fpsMeanMin: 110,
  worstFrameMaxMs: 50,
};

const preload = path.join(__dirname, "..", "dist", "preload", "index.js");
const indexHtml = path.join(__dirname, "..", "dist", "renderer", "index.html");

function argFlag(name) {
  const i = process.argv.findIndex((a) => a === name || a.startsWith(`${name}=`));
  if (i === -1) return null;
  const hit = process.argv[i];
  const eq = hit.indexOf("=");
  if (eq !== -1) return hit.slice(eq + 1); // --flag=value
  // --flag value (next token, unless it is itself a flag)
  const next = process.argv[i + 1];
  if (next && !next.startsWith("-")) return next;
  return "1"; // bare boolean flag
}
function intEnv(name, def) {
  const v = Number(process.env[name]);
  return Number.isFinite(v) && v > 0 ? Math.floor(v) : def;
}
function log(...a) {
  console.log("[gate-echo-harness]", ...a);
}
function fail(msg, code = 1) {
  console.error("[gate-echo-harness] FAIL:", msg);
  app.exit(code);
}

if (MODE !== "gate" && MODE !== "sanity") {
  console.error(`[gate-echo-harness] unknown --mode=${MODE} (use gate|sanity)`);
  process.exit(2);
}
if (!fs.existsSync(indexHtml)) {
  console.error(
    `[gate-echo-harness] ${indexHtml} not found — build first: bun run --filter @nyx/electron build`,
  );
  process.exit(1);
}

// Hard stop so the harness never hangs. Generously larger than the worst-case sum of
// all phases: 3 echo conditions each capped at COND_BUDGET_MS + the 10 s FPS window +
// boot/ramp/teardown slack — so the per-condition budgets always finish FIRST and the
// run reports its numbers instead of being killed by this backstop.
const HARNESS_TIMEOUT_MS = 3 * COND_BUDGET_MS + FPS_MS + 90_000;
setTimeout(() => fail(`timed out after ${HARNESS_TIMEOUT_MS}ms`), HARNESS_TIMEOUT_MS);

// ── The renderer-side probe (runs INSIDE the page, speaks only window.nyxCore) ──────
//
// Returned as a string and injected with executeJavaScript so it executes in the REAL
// renderer context (sandbox: true, contextIsolation: true) against the deep-frozen
// preload bridge — the identical surface apps/frontend uses. It resolves to the full
// measurement report. Everything here is plain ES the sandboxed renderer allows.
function probeSource(samples, fpsMs, condBudgetMs) {
  return `(async () => {
  const hb = (m) => { try { console.log('[probe] ' + m); } catch {} };
  const nyxCore = window.nyxCore;
  if (!nyxCore || typeof nyxCore.invoke !== 'function') {
    return { error: 'window.nyxCore bridge absent — preload did not install it' };
  }
  hb('probe started');

  // --- stats helpers ---------------------------------------------------------------
  const pct = (arr, p) => {
    if (!arr.length) return NaN;
    const s = [...arr].sort((a, b) => a - b);
    const i = Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1));
    return s[i];
  };
  const summarize = (arr) => ({
    samples: arr.length,
    median: +pct(arr, 50).toFixed(3),
    p95: +pct(arr, 95).toFixed(3),
    min: +(arr.length ? Math.min(...arr) : NaN).toFixed(3),
    max: +(arr.length ? Math.max(...arr) : NaN).toFixed(3),
    mean: +(arr.length ? arr.reduce((a, b) => a + b, 0) / arr.length : NaN).toFixed(3),
  });

  // --- output demux: one nyxCore.onEvent listener, fan pty://output to waiters -----
  // This mirrors the real electron adapter's EventDemux. We watch the raw output
  // bytes for our per-sample marker. We ALSO drive the flow-control ack loop exactly
  // like use-pty does (ack after "consuming" each chunk) so the flood condition sees
  // the SAME backpressure the shipped app applies.
  let ptyId = -1;
  const outWaiters = []; // {needle:string, resolve, t0}
  let pendingText = ''; // rolling tail so a marker split across chunks still matches
  let totalOutBytes = 0; // every output byte ever seen for our pty (sanity signal)

  const decoder = new TextDecoder();
  nyxCore.onEvent((env) => {
    if (!env || env.event !== 'pty://output') return;
    const p = env.payload;
    if (!p || p.id !== ptyId) return;
    const bytes = p.bytes instanceof Uint8Array ? p.bytes : Uint8Array.from(p.bytes || []);
    totalOutBytes += bytes.length;
    // Credit the bytes back (flow control) — fire-and-forget, like xterm.write's cb.
    try { nyxCore.ptyAck(ptyId, bytes.length); } catch {}
    if (!outWaiters.length) return; // not currently measuring — skip text work
    pendingText += decoder.decode(bytes, { stream: true });
    if (pendingText.length > 4096) pendingText = pendingText.slice(-4096);
    for (let i = outWaiters.length - 1; i >= 0; i--) {
      const w = outWaiters[i];
      if (pendingText.includes(w.needle)) {
        outWaiters.splice(i, 1);
        w.resolve(performance.now());
      }
    }
  });

  const rAF = () => new Promise((r) => requestAnimationFrame(() => r()));

  // Spawn a real shell PTY through the real path.
  hb('spawning pty');
  ptyId = await nyxCore.invoke('pty_spawn', { cols: 120, rows: 40 });
  if (typeof ptyId !== 'number') return { error: 'pty_spawn did not return a numeric id' };
  hb('pty spawned id=' + ptyId);

  // Let the shell reach its first prompt (drain the banner) before measuring.
  await new Promise((r) => setTimeout(r, 1200));
  hb('post-spawn drain: ' + totalOutBytes + ' output bytes seen');

  const enc = new TextEncoder();
  // Write a byte sequence to the PTY's stdin. We type a UNIQUE printable marker and
  // a newline; the shell ECHOES the marker (the echo we are timing) — independent of
  // the OS shell (bash/zsh/pwsh/cmd all echo typed input on a PTY).
  const writeMarker = (marker) =>
    nyxCore.invoke('pty_write', { id: ptyId, data: Array.from(enc.encode(marker)) });

  // Measure ONE echo round-trip: pick a marker not yet on screen, register a waiter,
  // stamp t0, write it, await its arrival. Timing is rAF-paced by the caller.
  let seq = 0;
  function dropWaiter(w) {
    const i = outWaiters.indexOf(w);
    if (i !== -1) outWaiters.splice(i, 1);
  }
  async function oneEcho(perSampleMs) {
    // A BEL prefix keeps the marker out of any prompt-completion noise; the unique
    // tag survives chunk splits (the demux matches on a rolling text tail).
    const marker = '\\u0007NYXm' + (seq++).toString(36) + 'Z';
    const w = { needle: marker, resolve: null };
    const arrival = new Promise((resolve) => { w.resolve = resolve; });
    outWaiters.push(w);
    const t0 = performance.now();
    await writeMarker(marker);
    try {
      const t1 = await Promise.race([
        arrival,
        new Promise((_, rej) => setTimeout(() => rej(new Error('echo-timeout')), perSampleMs)),
      ]);
      return t1 - t0;
    } finally {
      dropWaiter(w); // never leak a waiter that timed out (would match a stale marker)
    }
  }

  // Run N echo samples, one per rAF tick (so a render frame always interleaves and
  // the flood path cannot starve the measurement). Two guards keep the run BOUNDED:
  //  - a per-sample timeout (a lost/never-echoed marker is dropped, not awaited
  //    forever);
  //  - an overall WALL-CLOCK budget for the whole condition, so a shell that does NOT
  //    echo typed bytes (e.g. a non-canonical Windows ConPTY) makes the run TERMINATE
  //    with however many samples it got — surfacing as a sample-count fail in the
  //    verdict — rather than hanging. On the Linux target every sample lands fast, so
  //    the budget is never the limiter and the full N is collected.
  async function runEchoSamples(n, perSampleMs = 1500, budgetMs = ${condBudgetMs}) {
    const out = [];
    const deadline = performance.now() + budgetMs;
    for (let k = 0; k < n; k++) {
      if (performance.now() > deadline) break;
      await rAF();
      let v;
      try {
        v = await oneEcho(perSampleMs);
      } catch {
        v = null; // timed out / lost marker — drop and continue
      }
      if (v != null && isFinite(v)) out.push(v);
    }
    return out;
  }

  // --- animation layer: a continuous Motion-style transform/opacity loop -----------
  // We don't pull in the React app here (the harness loads the real renderer bundle,
  // but the deterministic, comparable signal is a known animation we control). A grid
  // of cards animates transform + opacity every frame — the same kind of GPU work the
  // app's Motion springs produce — so "during animation" is a real moving scene.
  function startAnimation() {
    const host = document.createElement('div');
    host.style.cssText = 'position:fixed;inset:0;z-index:0;pointer-events:none;overflow:hidden';
    const N = 40, cards = [];
    for (let i = 0; i < N; i++) {
      const c = document.createElement('div');
      c.style.cssText =
        'position:absolute;width:120px;height:80px;border-radius:12px;' +
        'background:linear-gradient(135deg,#5b8cff,#a05bff);will-change:transform,opacity';
      host.appendChild(c);
      cards.push(c);
    }
    document.body.appendChild(host);
    let raf = 0, running = true;
    const t0 = performance.now();
    const tick = () => {
      if (!running) return;
      const t = (performance.now() - t0) / 1000;
      for (let i = 0; i < N; i++) {
        const a = t * 1.3 + i * 0.5;
        const x = (Math.sin(a) * 0.5 + 0.5) * (innerWidth - 140) + 10;
        const y = (Math.cos(a * 0.9) * 0.5 + 0.5) * (innerHeight - 100) + 10;
        const s = 0.8 + 0.4 * (Math.sin(a * 1.7) * 0.5 + 0.5);
        const r = (a * 40) % 360;
        cards[i].style.transform =
          'translate(' + x.toFixed(1) + 'px,' + y.toFixed(1) + 'px) scale(' + s.toFixed(3) + ') rotate(' + r.toFixed(1) + 'deg)';
        cards[i].style.opacity = (0.5 + 0.5 * (Math.sin(a * 2.1) * 0.5 + 0.5)).toFixed(3);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => { running = false; cancelAnimationFrame(raf); host.remove(); };
  }

  // --- FPS over a window, rAF-sampled (mean fps + worst frame) ----------------------
  function measureFps(ms) {
    return new Promise((resolve) => {
      const frames = [];
      let last = performance.now();
      const start = last;
      const tick = (now) => {
        frames.push(now - last);
        last = now;
        if (now - start >= ms) {
          const total = now - start;
          const fps = (frames.length / total) * 1000;
          const worst = frames.length ? Math.max(...frames) : NaN;
          resolve({
            durationMs: +total.toFixed(1),
            frames: frames.length,
            meanFps: +fps.toFixed(2),
            worstFrameMs: +worst.toFixed(2),
          });
          return;
        }
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
  }

  // ── (a) AT REST ──────────────────────────────────────────────────────────────────
  hb('rest condition: measuring ' + ${samples} + ' echo samples');
  const rest = await runEchoSamples(${samples});
  hb('rest done: ' + rest.length + ' samples');

  // ── (b) DURING ANIMATION ──────────────────────────────────────────────────────────
  hb('anim condition: starting animation + measuring');
  const stopAnim = startAnimation();
  await rAF(); await rAF();
  const animEcho = await runEchoSamples(${samples});
  hb('anim echo done: ' + animEcho.length + ' samples; measuring FPS ' + ${fpsMs} + 'ms');
  // ── FPS during the SAME animation, over the 10 s window ───────────────────────────
  const fps = await measureFps(${fpsMs});
  hb('fps done: mean=' + fps.meanFps);
  stopAnim();

  // ── (c) UNDER FLOOD ───────────────────────────────────────────────────────────────
  // Start a real flood on the shell (POSIX 'yes', else a PowerShell/cmd equivalent —
  // the shell name is unknown here so we send a portable loop that works on both).
  // The flow-control ack loop above regulates it (the SAME backpressure the app uses).
  // We keep typing markers through the torrent; rAF pacing keeps the probe alive.
  const floodCmd =
    "yes 2>/dev/null || while true; do echo yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy; done\\n";
  hb('flood condition: launching flood + measuring');
  await writeMarker(floodCmd);
  await new Promise((r) => setTimeout(r, 1500)); // let the torrent ramp up
  const flood = await runEchoSamples(${samples});
  hb('flood done: ' + flood.length + ' samples');

  // Stop the flood + close the PTY so the host tears down cleanly.
  try { await nyxCore.invoke('pty_write', { id: ptyId, data: Array.from(enc.encode('\\u0003')) }); } catch {}
  try { await nyxCore.invoke('pty_close', { id: ptyId }); } catch {}

  return {
    ok: true,
    devicePixelRatio: window.devicePixelRatio,
    screen: { w: screen.width, h: screen.height },
    // The shell DID stream output (banner/echo) — proves the path is live even if a
    // platform's keystroke-echo semantics differ (Windows ConPTY in a sanity run).
    totalOutBytes,
    echo: { rest: summarize(rest), anim: summarize(animEcho), flood: summarize(flood) },
    fps,
  };
})();`;
}

// ── Threshold verdict (gate mode only) ──────────────────────────────────────────────
function verdict(report) {
  const t = THRESHOLDS;
  const checks = [];
  const add = (name, pass, detail) => checks.push({ name, pass, detail });
  const e = report.echo;
  add(
    "rest echo median ≤ 5 ms",
    e.rest.median <= t.restAnimEchoMedianMs,
    `${e.rest.median} ms (n=${e.rest.samples})`,
  );
  add("rest echo p95 ≤ 10 ms", e.rest.p95 <= t.restAnimEchoP95Ms, `${e.rest.p95} ms`);
  add(
    "anim echo median ≤ 5 ms",
    e.anim.median <= t.restAnimEchoMedianMs,
    `${e.anim.median} ms (n=${e.anim.samples})`,
  );
  add("anim echo p95 ≤ 10 ms", e.anim.p95 <= t.restAnimEchoP95Ms, `${e.anim.p95} ms`);
  add(
    "flood echo median ≤ 25 ms",
    e.flood.median <= t.floodEchoMedianMs,
    `${e.flood.median} ms (n=${e.flood.samples})`,
  );
  add("flood echo p95 ≤ 40 ms", e.flood.p95 <= t.floodEchoP95Ms, `${e.flood.p95} ms`);
  add(
    "mean fps ≥ 110",
    report.fps.meanFps >= t.fpsMeanMin,
    `${report.fps.meanFps} fps over ${report.fps.durationMs} ms`,
  );
  add(
    "no frame > 50 ms",
    report.fps.worstFrameMs <= t.worstFrameMaxMs,
    `worst ${report.fps.worstFrameMs} ms`,
  );
  add(
    "≥ 100 samples / condition",
    e.rest.samples >= 100 && e.anim.samples >= 100 && e.flood.samples >= 100,
    `rest ${e.rest.samples} / anim ${e.anim.samples} / flood ${e.flood.samples}`,
  );
  return checks;
}

function printReport(report) {
  const e = report.echo;
  const row = (label, s) =>
    `  ${label.padEnd(8)} median ${String(s.median).padStart(7)} ms  p95 ${String(s.p95).padStart(7)} ms  ` +
    `min ${String(s.min).padStart(6)}  max ${String(s.max).padStart(8)}  n=${s.samples}`;
  log("══════════════════════════════════════════════════════════════════════");
  log(`  MODE=${MODE}   dpr=${report.devicePixelRatio}   screen=${report.screen.w}×${report.screen.h}`);
  log("  ECHO LATENCY (keystroke → byte handed to xterm), renderer clock:");
  log(row("rest", e.rest));
  log(row("anim", e.anim));
  log(row("flood", e.flood));
  log(
    `  FPS (Motion animation): mean ${report.fps.meanFps} fps over ${report.fps.durationMs} ms, ` +
      `worst frame ${report.fps.worstFrameMs} ms, frames ${report.fps.frames}`,
  );
  log(`  shell output streamed: ${report.totalOutBytes} bytes (proves the path is live)`);
  log("══════════════════════════════════════════════════════════════════════");
}

// ── Boot the REAL stack and run ─────────────────────────────────────────────────────
app.whenReady().then(async () => {
  registerWindowIpc();

  const coreHost = new CoreHost();
  let mainWindow = null;
  registerCoreIpc(coreHost, () => mainWindow);

  coreHost.onState(({ state, reason }) => {
    log(`core-host state=${state}${reason ? ` (${reason})` : ""}`);
  });

  try {
    await coreHost.start();
  } catch (e) {
    return fail(`core-host failed to boot: ${e && e.message ? e.message : e}`);
  }
  if (coreHost.currentState !== "ready") {
    return fail(`core-host not ready (state=${coreHost.currentState})`);
  }
  log(`core-host ready (pid=${coreHost.pid})`);

  const win = new BrowserWindow({
    width: 1280,
    height: 800,
    frame: false,
    show: false,
    backgroundColor: "#000000",
    webPreferences: {
      preload,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  mainWindow = win;

  let mainFrameFailed = null;
  win.webContents.on("did-fail-load", (_e, code, desc, _url, isMainFrame) => {
    if (isMainFrame) mainFrameFailed = `${code} ${desc}`;
  });

  // Surface the probe's in-renderer heartbeats (the `[probe] …` console.log lines) so
  // the run shows live progress and a stall is diagnosable, not a silent timeout.
  win.webContents.on("console-message", (_e, _level, message) => {
    if (typeof message === "string" && message.startsWith("[probe]")) log(message);
  });

  await new Promise((resolve) => {
    win.webContents.once("did-finish-load", () => resolve());
    win.loadFile(indexHtml);
  });
  if (mainFrameFailed) return fail(`renderer main frame failed to load: ${mainFrameFailed}`);
  log(`real front loaded from ${win.webContents.getURL()}`);

  let report;
  try {
    report = await win.webContents.executeJavaScript(
      probeSource(SAMPLES, FPS_MS, COND_BUDGET_MS),
      true,
    );
  } catch (e) {
    return fail(`probe threw in the renderer: ${e && e.message ? e.message : e}`);
  }
  if (!report || report.error) {
    return fail(`probe error: ${report ? report.error : "no report"}`);
  }

  printReport(report);

  // Persist the JSON report if requested (and always print it for capture).
  const json = JSON.stringify({ mode: MODE, thresholds: THRESHOLDS, ...report }, null, 2);
  log("REPORT-JSON-BEGIN");
  console.log(json);
  log("REPORT-JSON-END");
  if (OUT) {
    try {
      fs.writeFileSync(OUT, json);
      log(`report written to ${OUT}`);
    } catch (e) {
      log(`WARN — could not write ${OUT}: ${e && e.message ? e.message : e}`);
    }
  }

  // Verdict.
  const checks = verdict(report);
  let allPass = true;
  log("THRESHOLD CHECKS:");
  for (const c of checks) {
    log(`  ${c.pass ? "PASS" : "FAIL"}  ${c.name}  —  ${c.detail}`);
    if (!c.pass) allPass = false;
  }

  await coreHost.stop().catch(() => {});

  if (MODE === "sanity") {
    log(
      "SANITY MODE — thresholds NOT enforced. These are WINDOWS/HEADLESS numbers and " +
        "are NOT the gate validation. Run --mode=gate on the Linux/Wayland 120 Hz target.",
    );
    return app.exit(0);
  }

  if (allPass) {
    log("GATE PASS — all thresholds met on this host.");
    return app.exit(0);
  }
  return fail(
    "GATE NOT MET on this host. If this is NOT the Linux/Wayland 120 Hz target, that " +
      "is EXPECTED — only a --mode=gate run on the target machine validates the gate.",
    1,
  );
});
