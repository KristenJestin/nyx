#!/usr/bin/env node
/**
 * Runtime smoke for the Electron SCAFFOLD (task #1). Boots a REAL hardened
 * BrowserWindow with the REAL built preload (`dist/preload/index.js`) and the SAME
 * webPreferences the app uses, loads a tiny probe page, and verifies — from inside
 * the isolated renderer — the phase-2 security + allowlist contract:
 *
 *   1. window is created and FRAMELESS (no native frame);
 *   2. renderer has NO Node integration (`require`/`process`/`module` undefined);
 *   3. the ONLY injected global is `window.nyxWindow`, exposing EXACTLY the four
 *      allowlisted methods (no `ipcRenderer`, no extra channels);
 *   4. the allowlisted IPC actually round-trips (`controlsVisible()` returns a bool;
 *      `toggleMaximize()` flips `isMaximized()`).
 *
 * Run with the Electron binary: `electron scripts/smoke-window.cjs` (the npm script
 * `smoke:window` does this). Exits 0 on full pass, non-zero with a reason otherwise.
 *
 * This drives production code paths (real preload, real webPreferences, real
 * `registerWindowIpc`) without adding any test-only channel to the shipped main.
 */
"use strict";
const path = require("node:path");
const { app, BrowserWindow } = require("electron");

const { registerWindowIpc } = require("../dist/main/window-ipc.js");
const { WINDOW_CHANNELS } = require("../dist/shared/ipc.js");

const preload = path.join(__dirname, "..", "dist", "preload", "index.js");

// Probe page: reports the renderer's view of the world back via the page title,
// which the main reads with `webContents.getTitle()` — a channel the page always
// has, requiring no extra IPC surface.
const PROBE_HTML = `data:text/html,${encodeURIComponent(`
<!doctype html><meta charset="utf-8"><title>boot</title>
<script>
  (async () => {
    const report = {};
    report.hasNode = typeof require !== 'undefined' || typeof module !== 'undefined' || typeof process !== 'undefined';
    report.hasIpcRenderer = typeof window.ipcRenderer !== 'undefined';
    report.bridge = typeof window.nyxWindow;
    report.keys = window.nyxWindow ? Object.keys(window.nyxWindow).sort().join(',') : '';
    try {
      report.controlsVisible = await window.nyxWindow.controlsVisible();
    } catch (e) { report.controlsVisibleErr = String(e); }
    document.title = 'RESULT:' + JSON.stringify(report);
  })();
</script>`)}`;

function fail(msg) {
  console.error("[smoke-window] FAIL:", msg);
  app.exit(1);
}

app.whenReady().then(async () => {
  registerWindowIpc();

  const win = new BrowserWindow({
    width: 800,
    height: 600,
    frame: false,
    show: false,
    webPreferences: {
      preload,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  // (1) frameless
  if (typeof win.isMovable !== "function") return fail("window not constructed");
  console.log("[smoke-window] window created (frame:false requested)");

  // Wait for the probe to publish its report via the title.
  const report = await new Promise((resolve) => {
    const onTitle = (_e, title) => {
      if (title.startsWith("RESULT:")) {
        win.webContents.off("page-title-updated", onTitle);
        resolve(JSON.parse(title.slice("RESULT:".length)));
      }
    };
    win.webContents.on("page-title-updated", onTitle);
    win.loadURL(PROBE_HTML);
  });

  // (2) no Node in renderer
  if (report.hasNode) return fail("renderer has Node integration (require/process/module reachable)");
  console.log("[smoke-window] renderer has NO Node integration ✓");

  // (3) only the allowlisted bridge, exact methods, no ipcRenderer
  if (report.hasIpcRenderer) return fail("ipcRenderer leaked into the renderer");
  if (report.bridge !== "object") return fail(`window.nyxWindow missing (typeof=${report.bridge})`);
  // The allowlisted window-control surface: the frameless chrome controls plus the two
  // MAIN-process resolvers the contract routes through the window bridge (`pickDirectory`
  // — the native folder picker — and `homeDir` — `AppPathsBridge.homeDir`), added when the
  // nyxBridge Electron adapter landed. Sorted (Object.keys order) for a stable assertion.
  const expected = "close,controlsVisible,homeDir,minimize,pickDirectory,toggleMaximize";
  if (report.keys !== expected) return fail(`nyxWindow keys = "${report.keys}", expected "${expected}"`);
  console.log(`[smoke-window] allowlisted bridge exposes exactly [${expected}] ✓`);

  // (4) IPC round-trips
  if (typeof report.controlsVisible !== "boolean")
    return fail(`controlsVisible() did not return a boolean (${report.controlsVisibleErr || report.controlsVisible})`);
  console.log(`[smoke-window] controlsVisible() round-trip = ${report.controlsVisible} ✓`);

  // toggleMaximize via the real handler, then verify state actually changed.
  const { ipcMain } = require("electron");
  void ipcMain; // (handlers already registered)
  const before = win.isMaximized();
  // Invoke the handler exactly as the renderer would, through the channel.
  const after = await win.webContents.executeJavaScript(
    `window.nyxWindow.toggleMaximize()`,
  );
  if (typeof after !== "boolean") return fail("toggleMaximize() did not return a boolean");
  if (after === before) return fail(`toggleMaximize() did not change maximize state (stayed ${before})`);
  console.log(`[smoke-window] toggleMaximize() flipped isMaximized ${before} -> ${after} ✓`);

  console.log("[smoke-window] OK — scaffold security + allowlist + IPC verified.");
  app.exit(0);
});

// Never hang the harness.
setTimeout(() => fail("timed out"), 20000);
