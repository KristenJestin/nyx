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
// V2 topology (PDR-0006): the single Electron desktop shell lives at apps/desktop.
const ELECTRON_APP_DIR = path.resolve(REPO_ROOT, "apps/desktop");

// We drive the app through its BUILT main entry (`dist/main/index.js`) using the
// project's own Electron binary — `appEntryPoint` makes wdio-electron-service launch
// `electron <entry>` (no packaging step required for the smoke). This is the same code
// the packaged app runs; it spawns the nyx-sidecar binary exactly as in production.
const APP_ENTRY_POINT = path.resolve(ELECTRON_APP_DIR, "dist/main/index.js");
// The Electron binary the project depends on (resolved from the Electron app's deps).
function resolveElectronBinary() {
  // `electron` (the npm package) exports the absolute path to its binary as the
  // default export of its `index.js`; require it from the Electron app's context.
  const electronPkgMain = require.resolve("electron", { paths: [ELECTRON_APP_DIR] });
  // The package's main module returns the binary path string when required.
  return require(electronPkgMain);
}

// The Electron version the app depends on. `wdio-electron-service` provisions the matching
// Chromedriver from this. Electron is a dependency of the APP (apps/desktop), NOT of this e2e
// package, so the service — which reads the version from THIS folder's package by default —
// cannot detect it and aborts with "You must install Electron locally". We resolve the version
// from the app's `electron` package here and set it as the capability's `browserVersion` so the
// service provisions the right Chromedriver regardless of where the service looks.
function resolveElectronVersion() {
  const pkgPath = require.resolve("electron/package.json", { paths: [ELECTRON_APP_DIR] });
  return require(pkgPath).version;
}

// The shells in the specs type POSIX commands (`export`, `echo "$FOO"`, `printf`).
// The sidecar's PTY backend honors $SHELL first, then a per-OS default (PowerShell
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

// The sidecar's MCP HTTP server binds a STABLE port (`nyx_core::mcp::DEFAULT_PORT` = 8766)
// overridable via `NYX_MCP_PORT` (ADR-0003 D2). We pin a DEDICATED, high e2e port here (well
// away from the 8766 default a running v2 dev build may already hold) and thread it into the
// launched app's env so the v2.2 cockpit gate spec can drive the REAL `agent_session_event`
// MCP tool over `http://127.0.0.1:<port>/mcp` — exactly how a real Claude Code hook records a
// session — to seed a workspace agent session with ZERO production plumbing. A dedicated port
// keeps the e2e app's MCP loopback isolated from any other nyx instance on the host.
const E2E_MCP_PORT = "8779";

// The app pins its Electron userData IN CODE to `<appData>/nyx-v2` (identity.ts — a safety guard
// so v2 never opens the v1 DB), which IGNORES Chromedriver's own `--user-data-dir`. Chromedriver
// then looks for `DevToolsActivePort` in a dir the app never writes to → "DevToolsActivePort file
// doesn't exist" and the session never starts. We reconcile the two WITHOUT touching the app:
// steer the app's `getPath('appData')` to a fixed e2e XDG root (so it pins `userData =
// <xdgRoot>/nyx-v2`) AND pass that SAME path as Chromium's `--user-data-dir`. Electron then writes
// `DevToolsActivePort` exactly where Chromedriver reads it, and userData stays `nyx-v2`-namespaced
// (the safety property is preserved). This is set at CONFIG-LOAD time because `wdio-electron-
// service` bakes `appArgs` + reads the env in its own `onPrepare`, BEFORE per-session hooks.
// The nyx DB is isolated PER SPEC via `NYX_DATA_DIR` (below), so a shared Electron userData —
// which only holds window/session chrome, never the nyx DB — is fine across specs.
const E2E_XDG_ROOT = path.join(os.tmpdir(), "nyx-e2e-xdg");
const E2E_USER_DATA = path.join(E2E_XDG_ROOT, "nyx-v2");
fs.rmSync(E2E_XDG_ROOT, { recursive: true, force: true });
fs.mkdirSync(E2E_USER_DATA, { recursive: true });
process.env.XDG_CONFIG_HOME = E2E_XDG_ROOT; // Linux appData root
process.env.APPDATA = E2E_XDG_ROOT; // Windows appData root
// A shell profile / Electron-based CLI that exports `ELECTRON_RUN_AS_NODE=1` makes every
// `electron <entry>` run as PLAIN NODE (the app's `require("electron")` returns the CLI shim,
// `app` is undefined, boot crashes before the WebDriver session attaches). Strip it at load.
delete process.env.ELECTRON_RUN_AS_NODE;
// Keep the run HERMETIC: never re-template the user's REAL `~/.claude` plugin config.
process.env.NYX_RECONCILE_CLAUDE_PLUGIN = "0";

// Map a spec file to the temp userData it runs under. A seed/verify PAIR that spans two app
// sessions on the SAME persisted DB shares ONE dir: the restore pair (`restore-01-*`,
// `restore-02-*`) shares `restore`; the v2.2 cockpit-gate pair (`cockpit-01-*`, `cockpit-02-*`)
// shares `cockpit`. Any other spec gets an isolated dir keyed by its basename.
function specDataDir(specPath) {
  const base = specPath ? path.basename(specPath) : "default";
  let key;
  if (/^restore-\d+/.test(base)) key = "restore";
  else if (/^cockpit-\d+/.test(base)) key = "cockpit";
  else key = base.replace(/\W+/g, "_");
  return path.join(E2E_DATA_ROOT, key);
}

exports.config = {
  runner: "local",

  specs: ["./specs/**/*.e2e.cjs"],
  // ONE spec at a time, never two spec files in the same session, so each spec file is
  // its own Electron app process (its own sidecar + PTYs) — required for the restore
  // seed/verify split and for per-spec DB isolation.
  maxInstances: 1,
  specFileRetries: 0,

  capabilities: [
    {
      browserName: "electron",
      // The app's Electron version — pin it explicitly so the service provisions the matching
      // Chromedriver even though `electron` is a dep of apps/desktop, not of this e2e package.
      browserVersion: resolveElectronVersion(),
      "wdio:electronServiceOptions": {
        // Launch the project's Electron binary against the BUILT main entry. The app's
        // own `before-quit` tears the sidecar down (stopping PTYs) when the service
        // closes the session — no orphan host/PTY survives.
        appEntryPoint: APP_ENTRY_POINT,
        // Chromium launch flags for headed CI / dev boxes:
        //   --no-sandbox            Electron's sandbox needs a setuid helper / user namespaces
        //                           many runners lack; without it the renderer aborts.
        //   --disable-dev-shm-usage small /dev/shm (containers) → Chromium crashes; use /tmp.
        //   --disable-gpu           avoid GPU/zygote init hangs that make the DevTools port
        //                           never appear (the `DevToolsActivePort file doesn't exist`
        //                           / session-creation timeout under some Wayland/GL stacks).
        //   --ozone-platform=x11    force X11 (DISPLAY) over the compositor's Wayland backend
        //                           so Chromedriver's DevTools handshake is stable; Electron's
        //                           Chromium runs fine on XWayland. The Wayland-native surface
        //                           is validated separately at the phase-7 native gate.
        appArgs: [
          "--no-sandbox",
          "--disable-dev-shm-usage",
          "--disable-gpu",
          "--ozone-platform=x11",
          // Align with the app's PINNED Electron userData so Chromedriver finds
          // `DevToolsActivePort` (see E2E_XDG_ROOT / E2E_USER_DATA above).
          "--user-data-dir=" + E2E_USER_DATA,
        ],
      },
    },
  ],

  services: [
    [
      "electron",
      {
        // Resolve Electron + the app build from the APP dir (apps/desktop), where `electron`
        // and the app's package.json live — not from this e2e folder (the service's default
        // lookup root, which has no Electron dep).
        rootDir: ELECTRON_APP_DIR,
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
    // A shell profile / tool that exports `ELECTRON_RUN_AS_NODE=1` (common on dev boxes that
    // use Electron-based CLIs) makes EVERY `electron <entry>` run as PLAIN NODE — so the app's
    // main-process `require("electron")` returns the CLI shim (`app` is undefined) and the app
    // crashes on boot before the WebDriver session attaches ("Chrome instance exited"). Strip
    // it from the WDIO process env so the Electron app the service spawns runs as a real
    // Electron main process. (The build below does not need it.)
    delete process.env.ELECTRON_RUN_AS_NODE;

    // Start every run from a CLEAN data root so the DB is deterministic (the restore
    // scenario asserts exact terminal counts/order/scrollback).
    fs.rmSync(E2E_DATA_ROOT, { recursive: true, force: true });
    fs.mkdirSync(E2E_DATA_ROOT, { recursive: true });

    // Stage the sidecar the build:e2e chain JUST built — never a stale one.
    // `build:e2e` runs `build:sidecar` (= `cargo build -p nyx-sidecar`, which writes
    // `target/DEBUG/nyx-sidecar`) and then `copy:sidecar`. But `copy-sidecar.cjs`, with no
    // `NYX_SIDECAR_PROFILE`, prefers `target/release` when present — so a stale `target/release`
    // binary left over from a prior packaged build gets staged INSTEAD of the fresh debug one,
    // and the e2e app launches an out-of-date sidecar (e.g. missing the review commands
    // `repo_diff`/`review_comment_*` → "not available over the sidecar transport"). Pin the
    // profile to `debug` for the e2e build so `copy:sidecar` stages exactly what `build:sidecar`
    // just produced. `spawnSync` below inherits `process.env`, so the child `copy:sidecar` sees
    // it. This is scoped to the e2e path only — the packaged `build` keeps its own profile.
    process.env.NYX_SIDECAR_PROFILE = "debug";

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
    // Belt-and-suspenders: never let the launched Electron run as plain Node (see onPrepare).
    delete process.env.ELECTRON_RUN_AS_NODE;

    const dataDir = specDataDir(specs && specs[0]);
    fs.mkdirSync(dataDir, { recursive: true });
    process.env.NYX_DATA_DIR = dataDir;
    process.env.NYX_E2E_DATA_DIR = dataDir;
    // The dedicated e2e MCP port is stable across the run so the spec (Node side) knows where
    // the launched app's MCP loopback lives without any discovery.
    process.env.NYX_MCP_PORT = E2E_MCP_PORT;
    process.env.NYX_E2E_MCP_PORT = E2E_MCP_PORT;

    // --- Align Chromedriver's `--user-data-dir` with the app's PINNED Electron userData ---
    // The app pins its Electron userData IN CODE to `<appData>/nyx-v2` (identity.ts, a safety
    // guard so v2 never opens the v1 DB). That override IGNORES Chromedriver's own
    // `--user-data-dir`, so Chromedriver looks for `DevToolsActivePort` in a dir the app never
    // writes to → "DevToolsActivePort file doesn't exist" and the session never starts.
    // We reconcile the two WITHOUT touching the app: set `appData` (via `XDG_CONFIG_HOME` on
    // Linux / `APPDATA` on Windows / `HOME`→Application Support on macOS) to a per-spec XDG
    // root so the app pins `userData = <xdgRoot>/nyx-v2`, and pass that SAME path as Chromium's
    // `--user-data-dir`. Now Electron writes `DevToolsActivePort` exactly where Chromedriver
    // reads it, and the userData stays `nyx-v2`-namespaced (the safety property is preserved).
    // `wdio-electron-service` spawns the Electron child INHERITING this launcher's `process.env`
    // (it does not read a `wdio:electronServiceOptions.appEnv` bag), so all per-spec app-facing
    // env is set HERE on `process.env`. The appData root + `--user-data-dir` are fixed for the
    // whole run (set at config load, above), since the service bakes them in its own onPrepare.
    process.env.NYX_RECONCILE_CLAUDE_PLUGIN = "0";
  },

  // The electron service closes the app session after each spec; the app's `before-quit`
  // stops the sidecar (killing its PTYs), so nothing leaks between sessions.
  onComplete: function () {
    // Best-effort sweep of the per-run data root (the OS reaps tmp anyway).
    try {
      fs.rmSync(E2E_DATA_ROOT, { recursive: true, force: true });
    } catch {}
  },
};
