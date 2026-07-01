#!/usr/bin/env node
/**
 * EASG0 DONE-CRITERION (a) PROOF — a LIVE TERMINAL via the bridge, end to end through the
 * REAL Electron app + the SUPERVISED nyx-sidecar.
 *
 * Boots the REAL stack: full Electron `app`, the REAL `Sidecar` manager (which spawns the
 * REAL `nyx-sidecar` Rust binary over the stdin/stdout MessagePack pipe), the REAL
 * `registerCoreIpc` relay, and a REAL hardened `BrowserWindow` (`contextIsolation:true`,
 * `sandbox:true`, `nodeIntegration:false`) loading the REAL preload. A renderer-side PROBE —
 * run via `executeJavaScript` IN the sandboxed renderer, so it can touch ONLY the
 * allowlisted `window.nyxCore` — drives the live terminal.
 *
 * Proves criterion (a): the renderer spawns a PTY via the bridge, TYPES a command, and SEES
 * the PTY output STREAMED from the sidecar (the `pty://output` event reaches the renderer)
 * — "type → see PTY output streamed from the sidecar". Also proves the DB-backed boot reads
 * round-trip (`list_terminals`/`create_terminal`) since the live terminal binds to a record.
 *
 * Run under Electron with a display: `electron scripts/smoke-sidecar-live-terminal.cjs`
 * (the harness env sets ELECTRON_RUN_AS_NODE=1, which forces plain-Node; unset it first).
 * Exits 0 on full pass.
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const { app, BrowserWindow } = require("electron");

const { Sidecar } = require("../dist/main/sidecar.js");
const { registerCoreIpc } = require("../dist/main/core-ipc.js");

function fail(msg) {
  console.error("[smoke-sidecar-live] FAIL:", msg);
  app.exit(1);
}

// A deterministic, empty data dir so the DB is created fresh by the sidecar.
const pinnedDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-sidecar-live-"));
process.env.NYX_DATA_DIR = pinnedDataDir;
process.env.NYX_HOST_BOOT_TIMEOUT_MS = process.env.NYX_HOST_BOOT_TIMEOUT_MS || "15000";
process.env.SHELL = process.env.SHELL || "/bin/sh";

// Point the Sidecar manager at the built binary (dev: cargo target). The manager honors
// NYX_SIDECAR_BIN first, so pin it here for a dev run.
if (!process.env.NYX_SIDECAR_BIN) {
  const exe = process.platform === "win32" ? "nyx-sidecar.exe" : "nyx-sidecar";
  const staged = path.join(__dirname, "..", "dist", "sidecar", exe);
  const targetDbg = path.join(__dirname, "..", "..", "..", "target", "debug", exe);
  process.env.NYX_SIDECAR_BIN = fs.existsSync(staged) ? staged : targetDbg;
}

/**
 * The renderer-side PROBE. Runs IN the sandboxed renderer (only `window.nyxCore` reachable),
 * spawns a PTY, types `echo`, and waits for the streamed `pty://output` event.
 */
const PROBE = `(async () => {
  const out = { steps: [], events: {} };
  const core = window.nyxCore;
  if (!core || typeof core.invoke !== "function" || typeof core.onEvent !== "function") {
    return { error: "window.nyxCore bridge is missing (preload did not install it)" };
  }

  // Subscribe to the single relayed event channel and capture both the event names and the
  // decoded output bytes for our pty.
  let outputText = "";
  const unsub = core.onEvent((envelope) => {
    if (!envelope || !envelope.event) return;
    out.events[envelope.event] = (out.events[envelope.event] || 0) + 1;
    if (envelope.event === "pty://output" && envelope.payload && Array.isArray(envelope.payload.bytes)) {
      outputText += new TextDecoder().decode(Uint8Array.from(envelope.payload.bytes));
    }
  });

  // The boot DB read round-trips (the renderer's TerminalManager lists terminals on mount).
  const before = await core.invoke("list_terminals");
  out.beforeCount = Array.isArray(before) ? before.length : -1;
  // Create a record so the PTY binds to it (exec-state mapping), like a real new terminal.
  const created = await core.invoke("create_terminal", { cwd: "/tmp", label: "live" });
  out.recordId = created && created.id;

  // SPAWN the live PTY via the bridge (the renderer's usePty path).
  const ptyId = await core.invoke("pty_spawn", { cols: 80, rows: 24, terminalId: created.id });
  out.ptyId = ptyId;

  // TYPE a command into it (keystrokes -> pty_write).
  const line = "echo NYX_LIVE_TERMINAL_OK\\n";
  await core.invoke("pty_write", { id: ptyId, data: Array.from(new TextEncoder().encode(line)) });

  // WAIT up to ~6s for the streamed output to carry our marker (the proof the bytes flowed
  // sidecar -> pipe -> main -> bridge -> renderer).
  const deadline = Date.now() + 6000;
  while (Date.now() < deadline && !outputText.includes("NYX_LIVE_TERMINAL_OK")) {
    await new Promise((r) => setTimeout(r, 50));
  }
  out.sawMarker = outputText.includes("NYX_LIVE_TERMINAL_OK");
  out.outputSample = outputText.slice(-200);

  await core.invoke("pty_close", { id: ptyId }).catch(() => {});
  unsub();
  return out;
})();`;

let win = null;

app.whenReady().then(async () => {
  if (typeof app.whenReady !== "function") return fail("not running as Electron main");

  const sidecar = new Sidecar();
  registerCoreIpc(sidecar, () => win);
  let fatal = null;
  sidecar.onState(({ state, reason }) => {
    if (state === "fatal") fatal = reason || "fatal";
  });

  await sidecar.start();
  if (sidecar.currentState !== "ready") return fail(`sidecar state=${sidecar.currentState}, expected ready`);
  if (fatal) return fail(`sidecar fatal: ${fatal}`);
  console.log(`[smoke-sidecar-live] real nyx-sidecar booted (pid=${sidecar.pid}) ✓`);

  win = new BrowserWindow({
    show: false,
    webPreferences: {
      preload: path.join(__dirname, "..", "dist", "preload", "index.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      nodeIntegrationInWorker: false,
      webviewTag: false,
    },
  });
  const blank = path.join(pinnedDataDir, "probe.html");
  fs.writeFileSync(blank, "<!doctype html><meta charset=utf-8><title>nyx-live</title><body>probe</body>");
  await win.loadFile(blank);
  console.log("[smoke-sidecar-live] hardened window + real preload loaded ✓");

  let result;
  try {
    result = await win.webContents.executeJavaScript(PROBE, true);
  } catch (e) {
    return fail(`probe threw in the renderer: ${e && e.message ? e.message : e}`);
  }
  if (!result || result.error) return fail(`probe error: ${result ? result.error : "no result"}`);

  // --- Assertions -----------------------------------------------------------
  if (result.beforeCount !== 0) return fail(`expected 0 terminals before, got ${result.beforeCount}`);
  if (typeof result.recordId !== "string") return fail("create_terminal did not return a record id");
  if (typeof result.ptyId !== "number") return fail(`pty_spawn did not return a numeric id (${result.ptyId})`);
  const ptyOut = result.events && result.events["pty://output"];
  if (!(ptyOut > 0)) return fail("no pty://output event reached the renderer");
  if (!result.sawMarker) {
    return fail(`the typed command's output did not stream back (last bytes: ${JSON.stringify(result.outputSample)})`);
  }
  console.log(
    `[smoke-sidecar-live] LIVE TERMINAL ✓ — pty_spawn id=${result.ptyId}, typed 'echo', ` +
      `saw 'NYX_LIVE_TERMINAL_OK' in streamed pty://output (x${ptyOut}) ` +
      `[renderer -> preload -> main -> sidecar pipe -> back]`,
  );

  await sidecar.stop();
  if (sidecar.alive) return fail("sidecar still alive after stop()");
  if (!win.isDestroyed()) win.destroy();
  try {
    fs.rmSync(pinnedDataDir, { recursive: true, force: true });
  } catch {
    /* best-effort */
  }
  console.log("[smoke-sidecar-live] OK — the live terminal streams over the bridge end-to-end.");
  app.exit(0);
});

app.on("window-all-closed", () => {});
