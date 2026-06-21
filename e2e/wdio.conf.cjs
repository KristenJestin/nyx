/* eslint-disable */
// WebdriverIO config for the nyx end-to-end tests driving the REAL ELECTRON app
// (Chromium) through `wdio-electron-service` + Chromedriver.
//
// PORTED from the Tauri harness (task #27). The Tauri version drove the WebKitGTK /
// WebView2 app through `tauri-driver`; this one drives the Electron app directly:
// `wdio-electron-service` discovers the Electron Chromium, spins up the matching
// Chromedriver, launches our built app (`appBinaryPath`/`appEntryPoint`), and exposes
// the standard `browser`. The SPEC FILES are unchanged: they drive + read terminals
// through the inert `window.__nyx` / `window.__nyxDeck` seams (xterm paints to a WebGL
// canvas, so the text is not in the DOM) — those seams are renderer-side React and
// SHELL-AGNOSTIC, so nothing in the specs is Tauri- or Electron-specific.
//
// Why WDIO v9 here (the Tauri harness pinned v7): `wdio-electron-service` targets the
// modern WDIO (v8/v9) session stack. The Tauri pin existed because `tauri-driver`
// needed the classic v7 protocol; the Electron service does not. The two harnesses are
// independent (each has its own `e2e/bun.lock` install set).
//
// Run:  cd e2e && bun install && bun run test
// (See e2e/README.md for the full local procedure and the Linux/Wayland notes.)

const os = require("os");
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

// Windows builds produce `.exe`-suffixed binaries; Linux/macOS do not.
const IS_WIN = process.platform === "win32";
const EXE = IS_WIN ? ".exe" : "";

const REPO_ROOT = path.resolve(__dirname, "..");
const ELECTRON_APP_DIR = path.resolve(REPO_ROOT, "apps/electron");

// We drive the app through its BUILT main entry (`dist/main/index.js`) using the
// project's own Electron binary — `appEntryPoint` makes wdio-electron-service launch
// `electron <entry>` (no packaging step required for the smoke). This is the same code
// the packaged app runs; it spawns the dedicated core-host exactly as in production.
const APP_ENTRY_POINT = path.resolve(ELECTRON_APP_DIR, "dist/main/index.js");
// The Electron binary the project depends on (resolved from the Electron app's deps).
function resolveElectronBinary() {
  // `electron` (the npm package) exports the absolute path to its binary as the
  // default export of its `index.js`; require it from the Electron app's context.
  const electronPkgMain = require.resolve("electron", { paths: [ELECTRON_APP_DIR] });
  // The package's main module returns the binary path string when required.
  return require(electronPkgMain);
}

// The shells in the specs type POSIX commands (`export`, `echo "$FOO"`, `printf`).
// The core-host's PTY backend honors $SHELL first, then a per-OS default (PowerShell
// on Windows). To keep the specs deterministic on Windows we point $SHELL at Git Bash
// when present and $SHELL is unset; the launched app inherits it. Linux is untouched.
if (IS_WIN && !process.env.SHELL) {
  const bashCandidates = [
    path.join(process.env["ProgramFiles"] || "C:/Program Files", "Git/bin/bash.exe"),
    path.join(process.env["ProgramFiles(x86)"] || "C:/Program Files (x86)", "Git/bin/bash.exe"),
    path.join(process.env["LOCALAPPDATA"] || "", "Programs/Git/bin/bash.exe"),
  ];
  const bash = bashCandidates.find((p) => p && fs.existsSync(p));
  if (bash) process.env.SHELL = bash;
}

// Per-run root for the app's data dirs. nyx stores its SQLite DB under
// `<dataDir>/nyx.db`. We pin `NYX_DATA_DIR` (the portable override honored by BOTH
// shells) per spec so each spec runs on an ISOLATED, TEMPORARY userData:
//   - cleaned at onPrepare so every run starts from an empty DB;
//   - PERSISTED across the restore seed→verify pair (they SHARE one dir) so the
//     relaunch reads the persisted DB and the restore contract can be verified.
const E2E_DATA_ROOT = path.join(os.tmpdir(), "nyx-e2e-data");

// Map a spec file to the temp userData it runs under. The restore seed/verify pair
// (`restore-01-*`, `restore-02-*`) share the `restore` dir; any other spec gets an
// isolated dir keyed by its basename.
function specDataDir(specPath) {
  const base = specPath ? path.basename(specPath) : "default";
  const key = /^restore-\d+/.test(base) ? "restore" : base.replace(/\W+/g, "_");
  return path.join(E2E_DATA_ROOT, key);
}

exports.config = {
  runner: "local",

  specs: ["./specs/**/*.e2e.cjs"],
  // ONE spec at a time, never two spec files in the same session, so each spec file is
  // its own Electron app process (its own core-host + PTYs) — required for the restore
  // seed/verify split and for per-spec DB isolation.
  maxInstances: 1,
  specFileRetries: 0,

  capabilities: [
    {
      browserName: "electron",
      "wdio:electronServiceOptions": {
        // Launch the project's Electron binary against the BUILT main entry. The app's
        // own `before-quit` tears the core-host down (stopping PTYs) when the service
        // closes the session — no orphan host/PTY survives.
        appEntryPoint: APP_ENTRY_POINT,
        appArgs: [],
      },
    },
  ],

  services: [
    [
      "electron",
      {
        // Point the service at the project's pinned Electron (42.4.1) so the
        // Chromedriver it provisions matches the embedded Chromium.
        appBinaryPath: resolveElectronBinary(),
      },
    ],
  ],

  reporters: ["spec"],
  framework: "mocha",
  mochaOpts: {
    ui: "bdd",
    timeout: 120000,
  },

  logLevel: "info",

  // Build the Electron app (with the E2E seam) once before the run, so the suite is
  // self-contained. Set NYX_E2E_SKIP_BUILD=1 to reuse an existing build. The renderer
  // MUST be built with `VITE_NYX_E2E=1` (the `build:e2e` script) so `window.__nyx` is
  // exposed — the specs drive the app through it.
  onPrepare: function () {
    // Start every run from a CLEAN data root so the DB is deterministic (the restore
    // scenario asserts exact terminal counts/order/scrollback).
    fs.rmSync(E2E_DATA_ROOT, { recursive: true, force: true });
    fs.mkdirSync(E2E_DATA_ROOT, { recursive: true });

    if (process.env.NYX_E2E_SKIP_BUILD === "1") return;
    const res = spawnSync("bun", ["run", "build:e2e"], {
      cwd: ELECTRON_APP_DIR,
      stdio: "inherit",
      shell: IS_WIN, // `bun` on Windows is resolved via the shell PATH.
      env: process.env,
    });
    if (res.status !== 0) {
      throw new Error("electron e2e build failed (exit " + res.status + "); cannot run e2e");
    }
  },

  // Pin THIS spec's isolated userData before the app launches: `NYX_DATA_DIR` steers
  // both the launched app (it inherits this env) and the spec process (the restore
  // pair hands off a small JSON through this dir). The restore pair shares one dir so
  // the relaunch sees the persisted DB (see specDataDir).
  beforeSession: function (config, capabilities, specs) {
    const dataDir = specDataDir(specs && specs[0]);
    fs.mkdirSync(dataDir, { recursive: true });
    process.env.NYX_DATA_DIR = dataDir;
    process.env.NYX_E2E_DATA_DIR = dataDir;
    // Thread the per-spec userData into the launched Electron app's env. The electron
    // service forwards `wdio:electronServiceOptions.appEnv` to the spawned app.
    if (capabilities && capabilities["wdio:electronServiceOptions"]) {
      capabilities["wdio:electronServiceOptions"].appEnv = Object.assign(
        {},
        capabilities["wdio:electronServiceOptions"].appEnv,
        { NYX_DATA_DIR: dataDir, NYX_E2E_DATA_DIR: dataDir },
      );
    }
  },

  // The electron service closes the app session after each spec; the app's `before-quit`
  // stops the core-host (killing its PTYs), so nothing leaks between sessions.
  onComplete: function () {
    // Best-effort sweep of the per-run data root (the OS reaps tmp anyway).
    try {
      fs.rmSync(E2E_DATA_ROOT, { recursive: true, force: true });
    } catch {}
  },
};
