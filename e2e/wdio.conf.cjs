/* eslint-disable */
// WebdriverIO config for nyx end-to-end tests driving the REAL Tauri app via
// tauri-driver + WebKitWebDriver on Linux.
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

// Absolute path to the release binary built by `tauri build`. The Cargo package
// is named "nyx", so the binary is target/release/nyx.
const REPO_ROOT = path.resolve(__dirname, "..");
const APPLICATION = path.resolve(
  REPO_ROOT,
  "src-tauri/target/release/nyx",
);

// tauri-driver is installed by `cargo install tauri-driver` into ~/.cargo/bin.
const TAURI_DRIVER = path.resolve(
  os.homedir(),
  ".cargo/bin/tauri-driver",
);

let tauriDriver;

exports.config = {
  // tauri-driver listens on 4444 by default; point WDIO at it directly.
  hostname: "127.0.0.1",
  port: 4444,
  path: "/",

  specs: ["./specs/**/*.e2e.cjs"],
  maxInstances: 1,

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
  beforeSession: function () {
    tauriDriver = spawn(TAURI_DRIVER, [], {
      stdio: [null, process.stdout, process.stderr],
    });
    return waitForPort(4444, "127.0.0.1", 20000);
  },

  // Tear tauri-driver (and the app it spawned) down after each session.
  afterSession: function () {
    if (tauriDriver) tauriDriver.kill();
  },
};
