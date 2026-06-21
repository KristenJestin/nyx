#!/usr/bin/env node
/**
 * REAL-LAUNCH smoke (review 01KVJEY0BX9ZZ83J40WJ2NT931, task
 * 01KVJEYPKPBZDSDJKA6XHA8YAK). Closes the gap that let the dev shell ship broken: NO
 * existing smoke exercised the actual `bun run --filter @nyx/electron dev` LAUNCH
 * path, so a preload that was merely TRANSPILED (`tsc`) instead of BUNDLED (esbuild) —
 * leaving a bare `require("../shared/ipc.js")` a `sandbox:true` preload can NOT resolve
 * — slipped through. That dead preload never installs `window.nyxCore`, so the front's
 * `isElectronShell()` is false → it falls back to the TAURI adapter → every bridge call
 * (`pickDirectory`, `list_terminals`, …) hits `@tauri-apps invoke` on a non-existent
 * `__TAURI_INTERNALS__` and crashes on the first screen / first click.
 *
 * This smoke launches the REAL app and asserts, from inside the REAL hardened renderer
 * the production main creates, that:
 *   (a) the preload loaded WITHOUT error  ('Unable to load preload script' = FAIL);
 *   (b) `window.nyxCore` IS defined (the allowlisted bridge the preload installs);
 *   (c) the REAL front's shell selector (`isElectronShell()` — replicated verbatim)
 *       picks the ELECTRON adapter, NOT the Tauri fallback;
 *   (d) a REAL bridge call round-trips through preload → main → core-host → reply
 *       (`pickDirectory` canceled cleanly + a DB-backed `list_terminals`).
 *
 * Two modes, both driving the SAME real `webPreferences.preload`:
 *
 *  - DEV  (default): spawns the REAL production main entry `electron .`
 *    (= `dist/main/index.js`) as a CHILD with `NYX_DEV_SERVER_URL` pointed at a tiny
 *    local probe server — the EXACT path `scripts/dev.cjs` drives, minus Vite. The real
 *    main runs `createMainWindow`, loads the probe over `loadURL`, the preload installs
 *    `window.nyxCore`, and the probe POSTs its verdict back. This is the path that
 *    crashed with the `tsc` preload and passes with the esbuild bundle.
 *
 *  - PROD (`--prod`): drives the production load path IN-PROCESS — a hardened
 *    `BrowserWindow` with the EXACT `webPreferences` of `createMainWindow`, loading the
 *    REAL built front (`dist/renderer/index.html`) — and probes it via
 *    `executeJavaScript`. Proves the same class of bug can't hide behind `electron .`.
 *
 * Run:  electron scripts/smoke-dev-launch.cjs            (DEV — real `electron .` child)
 *       electron scripts/smoke-dev-launch.cjs --prod     (PROD — real built front)
 * Exits 0 on full pass, non-zero with a reason otherwise.
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const http = require("node:http");
const { spawn } = require("node:child_process");

const appDir = path.resolve(__dirname, "..");
const preload = path.join(appDir, "dist", "preload", "index.js");
const mainEntry = path.join(appDir, "dist", "main", "index.js");
const indexHtml = path.join(appDir, "dist", "renderer", "index.html");

const PROD = process.argv.includes("--prod");
const TAG = PROD ? "smoke-dev-launch:prod" : "smoke-dev-launch:dev";

function fail(msg) {
  console.error(`[${TAG}] FAIL:`, msg);
  process.exit(1);
}

/**
 * The renderer-side PROBE — a STRING evaluated inside the REAL hardened renderer (it
 * can touch ONLY the allowlisted `window.nyxCore` / `window.nyxWindow`). It replicates
 * the front's REAL shell selector verbatim (apps/frontend/src/bridge/index.ts:33-34) so
 * a green probe proves the front would pick the ELECTRON adapter — and round-trips two
 * real bridge calls. Resolves a JSON-able verdict.
 */
const PROBE = `(async () => {
  const out = { ok: false, steps: [] };
  // (a) preload loaded without error → the two allowlisted globals exist.
  out.hasNyxCore = typeof window.nyxCore !== "undefined";
  out.hasNyxWindow = typeof window.nyxWindow !== "undefined";
  if (!out.hasNyxCore) {
    out.error = "window.nyxCore is undefined — preload did NOT install the bridge (dead preload).";
    return out;
  }
  // (c) the REAL front selector (bridge/index.ts isElectronShell), replicated verbatim.
  const isElectronShell = (typeof window !== "undefined" && typeof window.nyxCore !== "undefined");
  out.selectedAdapter = isElectronShell ? "electron" : "tauri";
  if (!isElectronShell) {
    out.error = "isElectronShell() is false — front would fall back to the TAURI adapter.";
    return out;
  }
  // The bridge surface must be the allowlisted shape (not an accidental global).
  const core = window.nyxCore;
  if (typeof core.invoke !== "function" || typeof core.onEvent !== "function" || typeof core.ptyAck !== "function") {
    out.error = "window.nyxCore is present but not the allowlisted shape (invoke/onEvent/ptyAck).";
    return out;
  }
  const win = window.nyxWindow;
  if (!win || typeof win.controlsVisible !== "function") {
    out.error = "window.nyxWindow.controlsVisible missing — window bridge not installed.";
    return out;
  }
  // (d) REAL bridge round-trips — BOTH bridges, dialog-free so the run stays headless:
  //   - window.nyxWindow.controlsVisible(): preload → main (window-ipc) → boolean reply;
  //   - window.nyxCore.invoke("list_terminals"): the exact first-screen DB-backed call
  //     that, under the Tauri fallback, crashed on @tauri-apps invoke. A clean array back
  //     proves preload → main (core-ipc) → core-host → nyx-core → DB → reply is alive.
  try {
    out.controlsVisible = await win.controlsVisible();
    out.steps.push("controlsVisible -> " + JSON.stringify(out.controlsVisible));
  } catch (e) {
    out.error = "controlsVisible threw: " + (e && e.message ? e.message : String(e));
    return out;
  }
  try {
    const terms = await core.invoke("list_terminals");
    out.listTerminalsIsArray = Array.isArray(terms);
    out.steps.push("list_terminals -> " + (Array.isArray(terms) ? terms.length + " rows" : "non-array"));
  } catch (e) {
    out.error = "list_terminals threw: " + (e && e.message ? e.message : String(e));
    return out;
  }
  out.ok = out.hasNyxCore && out.hasNyxWindow && out.selectedAdapter === "electron"
    && typeof out.controlsVisible === "boolean" && out.listTerminalsIsArray === true;
  return out;
})();`;

/** Assert a verdict object the probe produced (shared by both modes). */
function assertVerdict(v) {
  if (!v) return fail("probe returned no verdict");
  if (v.error) return fail(v.error);
  if (!v.hasNyxCore) return fail("window.nyxCore undefined (preload dead)");
  console.log(`[${TAG}] (a) preload loaded — window.nyxCore + window.nyxWindow present ✓`);
  if (v.selectedAdapter !== "electron") {
    return fail(`shell selector picked '${v.selectedAdapter}', expected 'electron'`);
  }
  console.log(`[${TAG}] (c) isElectronShell() → ELECTRON adapter selected (not Tauri) ✓`);
  if (typeof v.controlsVisible !== "boolean") return fail(`controlsVisible did not round-trip a boolean (got ${JSON.stringify(v.controlsVisible)})`);
  if (v.listTerminalsIsArray !== true) return fail("list_terminals did not round-trip an array");
  console.log(`[${TAG}] (d) bridge round-trip ✓ — ${v.steps.join("  |  ")}`);
  if (!v.ok) return fail(`verdict.ok=false (${JSON.stringify(v)})`);
}

// ===========================================================================
// PROD mode — in-process real preload against the REAL built front.
// ===========================================================================
if (PROD) {
  const { app, BrowserWindow } = require("electron");

  if (!fs.existsSync(indexHtml)) {
    fail(`${indexHtml} not found — build + copy the renderer first (bun run build)`);
  }

  const pinnedDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-dev-launch-prod-"));
  process.env.NYX_DATA_DIR = pinnedDataDir;
  process.env.NYX_HOST_BOOT_TIMEOUT_MS = process.env.NYX_HOST_BOOT_TIMEOUT_MS || "15000";

  // Bring up the REAL window IPC + core relay the real main wires, so the probe's
  // window.pickDirectory + core.invoke have a live main side (this is the prod main's
  // own wiring, imported from the built modules — not a duplicate).
  const { registerWindowIpc } = require("../dist/main/window-ipc.js");
  const { CoreHost } = require("../dist/main/core-host.js");
  const { registerCoreIpc } = require("../dist/main/core-ipc.js");

  app.whenReady().then(async () => {
    registerWindowIpc();
    const coreHost = new CoreHost();
    let win = null;
    registerCoreIpc(coreHost, () => win);
    await coreHost.start();
    if (coreHost.currentState !== "ready") return fail(`core-host state=${coreHost.currentState}, expected ready`);

    // EXACT webPreferences of createMainWindow (window.ts) + the real preload.
    win = new BrowserWindow({
      width: 800,
      height: 600,
      frame: false,
      show: false,
      webPreferences: {
        preload,
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
        nodeIntegrationInWorker: false,
        webviewTag: false,
      },
    });

    let preloadError = null;
    win.webContents.on("preload-error", (_e, p, err) => {
      preloadError = `${p}: ${err && err.message ? err.message : err}`;
    });
    let mainFrameFailed = null;
    win.webContents.on("did-fail-load", (_e, code, desc, _url, isMainFrame) => {
      if (isMainFrame) mainFrameFailed = `${code} ${desc}`;
    });

    await win.loadFile(indexHtml);
    if (preloadError) return fail(`preload failed to load: ${preloadError}`);
    if (mainFrameFailed) return fail(`real front failed to load: ${mainFrameFailed}`);
    console.log(`[${TAG}] real built front loaded from ${win.webContents.getURL()} ✓`);

    let verdict;
    try {
      verdict = await win.webContents.executeJavaScript(PROBE, true);
    } catch (e) {
      return fail(`probe threw in renderer: ${e && e.message ? e.message : e}`);
    }
    assertVerdict(verdict);

    await coreHost.stop().catch(() => {});
    if (!win.isDestroyed()) win.destroy();
    try { fs.rmSync(pinnedDataDir, { recursive: true, force: true }); } catch {}
    console.log(`[${TAG}] OK — prod load path: real preload + real front + Electron adapter + bridge round-trip.`);
    app.exit(0);
  });

  setTimeout(() => fail("timed out (prod)"), 45000);
  return;
}

// ===========================================================================
// DEV mode — the REAL `electron .` child against a local probe server.
// This is the exact `bun run --filter @nyx/electron dev` launch path (minus Vite):
// the production main creates its window and loads NYX_DEV_SERVER_URL.
// ===========================================================================
const electronBin = require("electron"); // path to the electron binary

if (!fs.existsSync(mainEntry)) {
  fail(`${mainEntry} not found — bundle first (node scripts/bundle.cjs)`);
}

const pinnedDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-dev-launch-"));

// The probe page the real main will load over NYX_DEV_SERVER_URL. It runs the PROBE in
// the real renderer (real preload bridge) and POSTs the verdict to /result. A status
// query-poll page is unnecessary; the POST is the channel back to this process.
const PROBE_HTML = `<!doctype html><meta charset="utf-8"><title>nyx-dev-launch</title>
<body>probe</body>
<script>
  (async () => {
    let verdict;
    try {
      verdict = await ${PROBE};
    } catch (e) {
      verdict = { ok: false, error: "probe threw: " + (e && e.message ? e.message : String(e)) };
    }
    try {
      await fetch("/result", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(verdict) });
    } catch (e) {
      // If the POST itself fails, set the title so a future hook could read it; the
      // server-side timeout will otherwise fail the smoke with a clear reason.
      document.title = "POST_FAILED:" + (e && e.message ? e.message : String(e));
    }
  })();
</script>`;

let verdict = null;
let child = null;

// A tiny static server: GET / → the probe page; POST /result → capture the verdict.
const server = http.createServer((req, res) => {
  if (req.method === "POST" && req.url === "/result") {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      try {
        verdict = JSON.parse(body);
      } catch (e) {
        verdict = { ok: false, error: "bad verdict JSON: " + e.message };
      }
      res.writeHead(204).end();
      finish();
    });
    return;
  }
  res.writeHead(200, { "content-type": "text/html; charset=utf-8" }).end(PROBE_HTML);
});

let finished = false;
function finish() {
  if (finished) return;
  finished = true;
  try { if (child && child.exitCode === null) child.kill(); } catch {}
  try { server.close(); } catch {}
  try { fs.rmSync(pinnedDataDir, { recursive: true, force: true }); } catch {}
  assertVerdict(verdict);
  console.log(`[${TAG}] OK — real \`electron .\` dev launch: preload installed window.nyxCore, Electron adapter, bridge round-trip.`);
  process.exit(0);
}

server.listen(0, "127.0.0.1", () => {
  const port = server.address().port;
  const devUrl = `http://127.0.0.1:${port}/`;

  // Launch the REAL production main entry exactly as the dev script does: `electron .`
  // with NYX_DEV_SERVER_URL set. The main's createMainWindow loads this URL through the
  // real hardened webPreferences + the real preload.
  const env = {
    ...process.env,
    NYX_DEV_SERVER_URL: devUrl,
    NYX_DATA_DIR: pinnedDataDir,
    NYX_HOST_BOOT_TIMEOUT_MS: process.env.NYX_HOST_BOOT_TIMEOUT_MS || "15000",
  };

  child = spawn(electronBin, [".", `--user-data-dir=${pinnedDataDir}`], {
    cwd: appDir,
    env,
    stdio: ["ignore", "inherit", "inherit"],
  });
  child.on("error", (e) => fail(`failed to spawn electron: ${e.message}`));
  child.on("exit", (code) => {
    // The child exiting before POSTing a verdict means the launch itself died.
    if (!finished && verdict === null) {
      fail(`real \`electron .\` exited (code=${code}) before the probe reported — launch crashed`);
    }
  });
});

setTimeout(() => {
  if (!verdict) fail("timed out waiting for the renderer probe verdict (dev launch never reported)");
}, 45000);
