#!/usr/bin/env node
/**
 * Exec-state PERSISTENCE round-trip (PRD-5 task #1) under Electron-as-Node. Proves the
 * DB half of exec-state at parity with the Tauri `persist_and_emit_exec_state` +
 * `normalize_exec_state_on_exit` + the boot `normalize_phantom_running_terminals`:
 *
 *   1. A `success`/`error` settled transition is PERSISTED (state + exit code + the
 *      UNREAD flag) and reads back with the stamped `updatedAt` (the value the host's
 *      `terminal://exec-state` event carries) — DB is the authority for the badge
 *      after a restart.
 *   2. `setExecState` on an UNKNOWN id reports `updated=false` (the host then skips the
 *      emit — never announces a state the DB does not hold).
 *   3. A terminal left at `running` (a force-quit artefact) is settled to `idle` by the
 *      boot normalization (`normalizePhantomTerminals`), so no phantom running badge
 *      survives a restart; a SETTLED `success`/`error` is LEFT UNTOUCHED.
 *
 * Re-execs under the Electron binary (ELECTRON_RUN_AS_NODE=1), like verify-abi.cjs.
 */
"use strict";
const path = require("node:path");
const os = require("node:os");
const fs = require("node:fs");
const { spawnSync } = require("node:child_process");

const ADDON = path.join(__dirname, "..", "index.js");

if (!process.versions.electron) {
  let electronBin;
  try { electronBin = require("electron"); } catch { process.exit(2); }
  const res = spawnSync(electronBin, [__filename], {
    stdio: "inherit",
    env: { ...process.env, ELECTRON_RUN_AS_NODE: "1" },
  });
  process.exit(res.status === null ? 1 : res.status);
}

function fail(m) { console.error(`[verify-exec-state] FAILED — ${m}`); process.exit(1); }

const addon = require(ADDON);
const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-exec-smoke-"));

async function main() {
  const core = new addon.NyxCore(dataDir);

  // 1. settled success persists + reads back with unread + timestamp.
  const t = await core.createTerminal("/tmp/work", "term-a");
  const persist = await core.setExecState(t.id, "success", 0, true);
  if (!persist.updated) fail("setExecState on a real id reported updated=false");
  if (typeof persist.updatedAt !== "number" || persist.updatedAt <= 0) fail("updatedAt not stamped");
  const after = await core.getTerminal(t.id);
  if (!after) fail("terminal vanished");
  if (after.execState !== "success" || after.execExitCode !== 0 || after.execStateUnread !== true) {
    fail(`persisted exec-state wrong: ${JSON.stringify(after)}`);
  }
  if (after.execStateUpdatedAt !== persist.updatedAt) fail("read-back timestamp != stamped");
  console.log("[verify-exec-state] settled success persisted + read back (unread + stamped) ✓");

  // 2. unknown id → updated=false.
  const miss = await core.setExecState("nope-no-such-id", "error", 1, true);
  if (miss.updated !== false) fail("unknown id should report updated=false");
  console.log("[verify-exec-state] unknown id → updated=false (host skips emit) ✓");

  // 3a. a `running` terminal is normalized to idle at boot...
  const running = await core.createTerminal("/tmp/run", "term-b");
  await core.setExecState(running.id, "running", null, false);
  // ...but a SETTLED success (term-a) must NOT be touched.
  const normalized = await core.normalizePhantomTerminals();
  if (normalized < 1) fail(`expected >=1 normalized running terminal, got ${normalized}`);
  const runAfter = await core.getTerminal(running.id);
  if (runAfter.execState !== "idle") fail(`running terminal not settled to idle: ${runAfter.execState}`);
  const settledAfter = await core.getTerminal(t.id);
  if (settledAfter.execState !== "success") fail("a settled success was wrongly normalized");
  console.log("[verify-exec-state] phantom `running` settled to idle; settled `success` untouched ✓");

  console.log("[verify-exec-state] OK — exec-state DB persistence + boot normalization verified.");
  process.exit(0);
}

main().catch((e) => fail(e.stack || String(e)));
