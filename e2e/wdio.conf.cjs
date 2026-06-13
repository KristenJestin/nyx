/* eslint-disable */
// WebdriverIO config for nyx end-to-end tests driving the REAL Tauri app via
// tauri-driver: WebKitWebDriver on Linux, Microsoft Edge WebDriver (WebView2)
// on Windows.
//
// Why WebdriverIO v7 (and not v8/v9): tauri-driver speaks the classic JSON Wire
// / W3C session protocol that WDIO v7 targets directly. Newer WDIO majors
// bundle a different webdriver/session-management stack that has repeatedly
// mismatched tauri-driver's intermediary (session-id and capabilities
// negotiation), so the Tauri docs and this project pin WDIO v7. If you upgrade
// WDIO, expect to revisit beforeSession/afterSession and capabilities below.
//
// Run:  cd e2e && bun install && bun run test
// (See e2e/README.md for the full local procedure and CI dependencies.)

const os = require("os");
const fs = require("fs");
const net = require("net");
const path = require("path");
const { spawn, spawnSync } = require("child_process");

// Resolve once the given TCP port is accepting connections, or reject on
// timeout. tauri-driver takes a beat to bind 4444 after spawn; WDIO would
// otherwise race it and fail with ECONNREFUSED.
function waitForPort(port, host, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  return new Promise(function (resolve, reject) {
    (function attempt() {
      const sock = net.connect(port, host);
      sock.once("connect", function () {
        sock.destroy();
        resolve();
      });
      sock.once("error", function () {
        sock.destroy();
        if (Date.now() > deadline) {
          reject(new Error("tauri-driver did not open " + host + ":" + port));
        } else {
          setTimeout(attempt, 150);
        }
      });
    })();
  });
}

// Windows builds produce `.exe`-suffixed binaries; Linux/macOS do not.
const IS_WIN = process.platform === "win32";
const EXE = IS_WIN ? ".exe" : "";

// Absolute path to the release binary built by `tauri build`. The Cargo package
// is named "nyx", so the binary is target/release/nyx (nyx.exe on Windows).
const REPO_ROOT = path.resolve(__dirname, "..");
const APPLICATION = path.resolve(
  REPO_ROOT,
  "src-tauri/target/release/nyx" + EXE,
);

// tauri-driver is installed by `cargo install tauri-driver` into ~/.cargo/bin
// (tauri-driver.exe on Windows).
const TAURI_DRIVER = path.resolve(
  os.homedir(),
  ".cargo/bin/tauri-driver" + EXE,
);

// On Windows tauri-driver drives WebView2 through Microsoft Edge WebDriver
// (msedgedriver.exe) instead of Linux's WebKitWebDriver. tauri-driver finds it
// on $PATH; if it lives elsewhere, point $MSEDGEDRIVER at it and we forward the
// path via `--native-driver`. The driver version MUST match the installed
// WebView2 runtime or the WebDriver session hangs. Empty on Linux.
const NATIVE_DRIVER_ARGS =
  IS_WIN && process.env.MSEDGEDRIVER
    ? ["--native-driver", process.env.MSEDGEDRIVER]
    : [];

// The specs type POSIX shell commands (`export`, `echo "$FOO"`, `printf`, …).
// The app's PTY backend (src-tauri/src/pty.rs `resolve_shell`) honors $SHELL
// first, then a per-OS default (PowerShell on Windows). To keep the specs
// deterministic on Windows we point $SHELL at a real POSIX bash (Git Bash) when
// one is present and $SHELL is unset; the tauri-driver -> app process inherits
// it. Linux is untouched.
if (IS_WIN && !process.env.SHELL) {
  const bashCandidates = [
    path.join(process.env["ProgramFiles"] || "C:/Program Files", "Git/bin/bash.exe"),
    path.join(process.env["ProgramFiles(x86)"] || "C:/Program Files (x86)", "Git/bin/bash.exe"),
    path.join(process.env["LOCALAPPDATA"] || "", "Programs/Git/bin/bash.exe"),
  ];
  const bash = bashCandidates.find(function (p) {
    return p && fs.existsSync(p);
  });
  if (bash) process.env.SHELL = bash;
}

// Per-run root for the app's data dirs. nyx stores its SQLite DB under
// `$XDG_DATA_HOME/com.netsirk.nyx/nyx.db` (Tauri's app_data_dir on Linux). By
// pointing XDG_DATA_HOME at a temp dir we control, we get a DETERMINISTIC DB:
//   - cleaned at onPrepare so every run starts from an empty DB;
//   - PERSISTED across a session restart (app kill → relaunch) so the restore
//     scenario can verify state survives a real close/reopen.
// Each spec file gets its OWN data dir (keyed by its basename) so specs don't
// contaminate each other's DB — EXCEPT the restore pair, which deliberately
// shares ONE dir (see specDataDir) so spec 02 reads what spec 01 persisted.
const E2E_DATA_ROOT = path.join(os.tmpdir(), "nyx-e2e-data");

// Map a spec file to the XDG_DATA_HOME it runs under. The restore seed/verify
// pair (`restore-01-*`, `restore-02-*`) share the `restore` dir so the relaunch
// reads the persisted DB; any other spec gets an isolated dir by its basename.
function specDataDir(specPath) {
  const base = specPath ? path.basename(specPath) : "default";
  const key = /^restore-\d+/.test(base) ? "restore" : base.replace(/\W+/g, "_");
  return path.join(E2E_DATA_ROOT, key);
}

let tauriDriver;

exports.config = {
  // tauri-driver listens on 4444 by default; point WDIO at it directly.
  hostname: "127.0.0.1",
  port: 4444,
  path: "/",

  specs: ["./specs/**/*.e2e.cjs"],
  // ONE spec at a time, and never two spec files in the same session, so each
  // spec file is its own app process (its own session) — required for the
  // restore scenario's seed/verify split and for per-spec DB isolation.
  maxInstances: 1,
  specFileRetries: 0,

  capabilities: [
    {
      // tauri-driver reads `tauri:options.application` to know which binary to
      // launch under WebKitWebDriver.
      "tauri:options": {
        application: APPLICATION,
      },
      // Keep the matched-cap name explicit for clarity in logs.
      browserName: "wry",
    },
  ],

  reporters: ["spec"],
  framework: "mocha",
  mochaOpts: {
    ui: "bdd",
    timeout: 120000,
  },

  logLevel: "info",
  // Headless: relies on $DISPLAY (a real X server, or xvfb-run in CI — see
  // README). WebKitWebDriver needs a display; it has no built-in headless mode.

  // Ensure the release binary exists before the run; building it here keeps the
  // suite self-contained (long the first time). Set NYX_E2E_SKIP_BUILD=1 to
  // reuse an existing build.
  onPrepare: function () {
    // Start every run from a CLEAN data root so the DB is deterministic (the
    // restore scenario asserts exact terminal counts/order/scrollback).
    const fs = require("fs");
    fs.rmSync(E2E_DATA_ROOT, { recursive: true, force: true });
    fs.mkdirSync(E2E_DATA_ROOT, { recursive: true });

    if (process.env.NYX_E2E_SKIP_BUILD === "1") return;
    const res = spawnSync(
      "bun",
      ["run", "tauri", "build", "--no-bundle"],
      { cwd: REPO_ROOT, stdio: "inherit" },
    );
    if (res.status !== 0) {
      throw new Error(
        "release build failed (exit " + res.status + "); cannot run e2e",
      );
    }
  },

  // Start tauri-driver before each session; it spawns WebKitWebDriver and the
  // app, and proxies WebDriver commands to the WebView. Wait until it is
  // actually listening on 4444 so WDIO does not race it (ECONNREFUSED).
  //
  // We inject XDG_DATA_HOME for THIS spec so the spawned app (a child of
  // tauri-driver → it inherits this env) stores its SQLite DB in the spec's
  // dedicated dir. The restore pair shares one dir so the relaunch sees the
  // persisted DB (see specDataDir).
  beforeSession: function (config, capabilities, specs) {
    const xdgDataHome = specDataDir(specs && specs[0]);
    require("fs").mkdirSync(xdgDataHome, { recursive: true });
    // Also expose the dir to the spec process so the restore seed/verify pair
    // can hand off a small JSON of expected ids/markers/order via this dir.
    process.env.NYX_E2E_DATA_DIR = xdgDataHome;
    tauriDriver = spawn(TAURI_DRIVER, NATIVE_DRIVER_ARGS, {
      stdio: [null, process.stdout, process.stderr],
      // NYX_DATA_DIR is the portable DB-location override (honored on every OS by
      // src-tauri `resolve_data_dir`); XDG_DATA_HOME is kept for Linux belt-and-
      // suspenders. Both point at this spec's dedicated dir so the DB is
      // deterministic on Windows too (where XDG_DATA_HOME alone has no effect).
      env: Object.assign({}, process.env, {
        NYX_DATA_DIR: xdgDataHome,
        XDG_DATA_HOME: xdgDataHome,
      }),
    });
    return waitForPort(4444, "127.0.0.1", 20000);
  },

  // Tear tauri-driver (and the app it spawned) down after each session.
  afterSession: function () {
    if (tauriDriver) tauriDriver.kill();
  },
};
