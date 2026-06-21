#!/usr/bin/env node
/**
 * Runtime smoke for SINGLE-INSTANCE (task #1). Launches the REAL packaged main
 * (`electron .`) twice against the same `userData` and asserts:
 *   - the FIRST process acquires the single-instance lock and KEEPS running;
 *   - the SECOND process fails the lock and EXITS promptly on its own (it calls
 *     `app.quit()`), i.e. no duplicate instance survives.
 *
 * Run with system Node (it spawns the Electron binary itself): `node
 * scripts/smoke-single-instance.cjs`. Exits 0 on pass.
 *
 * A throwaway `userData` dir (via `--user-data-dir`) keeps the lock isolated from a
 * real nyx install and from parallel runs. The renderer never needs to load — the
 * lock is acquired before the window content, so we deliberately point at no dev
 * server (the first window just won't paint; we kill it after the assertion).
 */
"use strict";
const { spawn } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");

const electronBin = require("electron");
const appDir = path.resolve(__dirname, "..");
const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-si-"));

// Force prod load path (no dev server) so neither process depends on Vite.
const env = { ...process.env };
delete env.NYX_DEV_SERVER_URL;

function launch(tag) {
  const child = spawn(electronBin, [".", `--user-data-dir=${userDataDir}`], {
    cwd: appDir,
    env,
    stdio: "ignore",
  });
  child.on("error", (e) => console.error(`[${tag}] spawn error`, e));
  return child;
}

function fail(msg) {
  console.error("[smoke-single-instance] FAIL:", msg);
  cleanup(1);
}

let first, second;
function cleanup(code) {
  for (const c of [first, second]) {
    try {
      if (c && c.exitCode === null) c.kill();
    } catch {}
  }
  try {
    fs.rmSync(userDataDir, { recursive: true, force: true });
  } catch {}
  process.exit(code);
}

first = launch("first");
let firstExited = false;
first.on("exit", () => {
  firstExited = true;
});

// Give the first process time to acquire the lock, then launch the second.
setTimeout(() => {
  if (firstExited) return fail("first instance exited before acquiring the lock");
  second = launch("second");
  let secondExited = false;
  second.on("exit", (code) => {
    secondExited = true;
    // The second instance must quit on its own (lock not acquired).
    if (firstExited) return fail("first instance also exited — lock not held");
    console.log(`[smoke-single-instance] second instance exited on its own (code=${code}) ✓`);
    console.log("[smoke-single-instance] first instance still running, holds the lock ✓");
    console.log("[smoke-single-instance] OK — single-instance lock verified.");
    cleanup(0);
  });
  // The second should exit FAST; if it lingers, single-instance failed.
  setTimeout(() => {
    if (!secondExited) fail("second instance did not exit — single-instance NOT enforced");
  }, 8000);
}, 2500);

// Global safety timeout.
setTimeout(() => fail("timed out"), 20000);
