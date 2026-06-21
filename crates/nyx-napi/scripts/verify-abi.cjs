#!/usr/bin/env node
/**
 * napi ABI + load + stream verification (full phase-3 PTY surface).
 *
 * Loads the freshly built `.node` THROUGH THE ELECTRON BINARY running as Node
 * (`ELECTRON_RUN_AS_NODE=1`), NOT the system Node — that is the whole point of the
 * task: the addon must load under Electron's embedded Node ABI
 * (`process.versions.modules`), because the packaged Electron core-host is where it
 * runs in production. A successful load under that ABI is itself the proof the ABI
 * matches.
 *
 * Run via `bun run --filter @nyx/napi verify:abi` (which runs this with system
 * Node); this script then RE-EXECS itself under the Electron binary with
 * `ELECTRON_RUN_AS_NODE=1` and performs the actual checks there.
 *
 * Checks, under Electron-as-Node:
 *   1. the `.node` loads (require succeeds) → ABI matches the host Electron;
 *   2. `version()` returns the nyx-core version string;
 *   3. a minimal `NyxPty` spawns and STREAMS at least one output chunk to the Node
 *      callback (the EventSink path).
 */
"use strict";
const path = require("node:path");
const { spawnSync } = require("node:child_process");

// Load through the napi-rs OFFICIAL generated loader (`../index.js`), NOT a
// hard-coded `.node` path: `napi build --platform` produces a platform-SUFFIXED
// artifact (e.g. `nyx-napi.win32-x64-msvc.node`), and index.js is what resolves
// the correct suffix for the host. A bare `../nyx-napi.node` never exists.
const ADDON = path.join(__dirname, "..", "index.js");

// --- Phase A: re-exec under the Electron binary as Node ---------------------
if (!process.versions.electron) {
  let electronBin;
  try {
    electronBin = require("electron"); // the electron npm package exports the binary path
  } catch {
    console.error(
      "[verify-abi] the `electron` devDependency is not installed — cannot validate the .node against Electron's ABI.",
    );
    process.exit(2);
  }
  const res = spawnSync(electronBin, [__filename], {
    stdio: "inherit",
    env: { ...process.env, ELECTRON_RUN_AS_NODE: "1" },
  });
  process.exit(res.status === null ? 1 : res.status);
}

// --- Phase B: running under Electron-as-Node --------------------------------
console.log(
  `[verify-abi] under Electron ${process.versions.electron} · Node ${process.versions.node} · ABI ${process.versions.modules} (ELECTRON_RUN_AS_NODE=${process.env.ELECTRON_RUN_AS_NODE})`,
);

let addon;
try {
  addon = require(ADDON);
} catch (e) {
  console.error(`[verify-abi] FAILED to load ${ADDON} under Electron's ABI:`, e.message);
  process.exit(1);
}

// 2. version()
const v = addon.version();
console.log(`[verify-abi] addon.version() = ${v}`);
if (typeof v !== "string" || v.length === 0) {
  console.error("[verify-abi] version() did not return a non-empty string");
  process.exit(1);
}

// 3. minimal PTY stream
let chunks = 0;
const done = () => {
  if (chunks > 0) {
    console.log(`[verify-abi] OK — streamed ${chunks} chunk(s) from the PTY. ABI + load + stream verified.`);
    process.exit(0);
  } else {
    console.error("[verify-abi] FAILED — no PTY output streamed within the window");
    process.exit(1);
  }
};

try {
  // The data callback's payload is the output Buffer. The addon builds its
  // `ThreadsafeFunction` with `ErrorStrategy::Fatal`, so the chunk is the ONLY
  // argument (no leading `err`); read the Buffer as the LAST argument so this stays
  // correct whether the binding delivers `(bytes)` or `(err, bytes)`.
  //
  // Phase-3 FULL constructor: (cols, rows, cwd?, terminalId?, onData, onExit,
  // onCwd, onExecState). We pass null for cwd/terminalId and no-op the auxiliary
  // callbacks — this verification only proves the ABI + load + output stream.
  const noop = () => {};
  const pty = new addon.NyxPty(
    80,
    24,
    null,
    null,
    (...args) => {
      const bytes = args[args.length - 1];
      if (Buffer.isBuffer(bytes) && bytes.length > 0) chunks += 1;
    },
    noop, // onExit
    noop, // onCwd (OSC 7)
    noop, // onExecState (OSC 133)
  );
  // Nudge the shell so it emits a prompt / echo. Use CRLF so the line is submitted
  // on Windows shells (pwsh/cmd) as well as POSIX ones.
  pty.write(Buffer.from("echo NYX_OK\r\n"));
  // Exercise the new surface so a regression in resize/setPaused fails the ABI
  // check too (idempotent / safe on a live PTY).
  pty.resize(100, 30);
  pty.setPaused(false);
  setTimeout(done, 1500);
} catch (e) {
  console.error("[verify-abi] FAILED constructing NyxPty:", e.message);
  process.exit(1);
}
