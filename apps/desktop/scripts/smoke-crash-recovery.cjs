#!/usr/bin/env node
/**
 * CRASH-RECOVERY smoke (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N905`). This script is the
 * AUTHORITATIVE coverage for the SUPERVISOR's single-relaunch latch (there is no separate TS
 * unit test for it); it proves that half of the done-criterion end-to-end against the REAL
 * `nyx-sidecar` binary driven by the REAL `Sidecar` manager: killing the sidecar triggers
 * EXACTLY ONE automatic relaunch (never a blind/looping restart), and the relaunched sidecar
 * reopens SQLite + rebuilds the persisted state. A crash is treated as a CLEAN shutdown (the
 * v2.5.5 `interrupted`/restore-card handling was reverted in v2.5.8): there is NO interrupted
 * flag and NO card — the normal silent, toggle-gated resume path applies. The sidecar-side
 * reconcile half is additionally covered by the Rust integration test
 * `crates/nyx-sidecar/tests/crash_recovery.rs`.
 *
 * What it drives (all through the REAL `Sidecar` manager + the REAL binary over the pipe):
 *   1. boot the sidecar against a pinned, empty data dir; seed in-flight state (a managed-
 *      command instance persisted `running`) by talking to the sidecar's OWN SQLite (the same
 *      `nyx.db`) — i.e. work that looks live at crash time;
 *   2. CRASH it: `process.kill(pid, 'SIGKILL')` — NOT the ordered `stop()`, so the sidecar's
 *      clean-shutdown marker is never written;
 *   3. assert the manager relaunched EXACTLY ONCE: it returns to `ready`, the new pid differs
 *      from the dead one, and the relaunch count is exactly 1 (observed via the state stream:
 *      a single `recovering` transition);
 *   4. assert the relaunched sidecar rebuilt state: the previously-seeded instance is still
 *      queryable (SQLite reopened) and was NOT blindly relaunched, with NO interrupted surface
 *      (crash treated as a clean shutdown — v2.5.8 revert);
 *   5. CRASH it a SECOND time and assert NO further relaunch (the single-relaunch latch holds:
 *      the manager goes `degraded`, never loops back to `recovering`/`ready`).
 *
 * Run under Electron: `electron scripts/smoke-crash-recovery.cjs`. Exits 0 on full pass.
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const { app } = require("electron");

const { Sidecar } = require("../dist/main/sidecar.js");

// Point the Sidecar manager at the built sidecar binary (dev: cargo target). The manager's
// resolveSidecarBin honors NYX_SIDECAR_BIN first, so we pin it for a dev run.
if (!process.env.NYX_SIDECAR_BIN) {
  const exe = process.platform === "win32" ? "nyx-sidecar.exe" : "nyx-sidecar";
  const staged = path.join(__dirname, "..", "dist", "sidecar", exe);
  const targetDbg = path.join(__dirname, "..", "..", "..", "target", "debug", exe);
  process.env.NYX_SIDECAR_BIN = fs.existsSync(staged) ? staged : targetDbg;
}

// Pin a deterministic, empty data dir so the seeded state is the only state, and the data dir
// SURVIVES the crash (the same dir is reused by the relaunch — that is the whole point).
const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-crash-recovery-"));
process.env.NYX_DATA_DIR = dataDir;
// Keep the boot handshake snappy.
process.env.NYX_HOST_BOOT_TIMEOUT_MS = process.env.NYX_HOST_BOOT_TIMEOUT_MS || "15000";

function fail(msg) {
  console.error("[smoke-crash-recovery] FAIL:", msg);
  cleanup();
  app.exit(1);
}

function cleanup() {
  try {
    fs.rmSync(dataDir, { recursive: true, force: true });
  } catch {
    /* best-effort */
  }
}

/** A db_command round-trip through the manager's invoke surface. */
async function invoke(sidecar, command, args) {
  return sidecar.invoke(command, args || {});
}

/** Wait until `predicate()` is true or the deadline elapses (poll the manager state). */
async function waitFor(predicate, ms, label) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((r) => setTimeout(r, 25));
  }
  throw new Error(`timeout waiting for ${label}`);
}

app.whenReady().then(async () => {
  const sidecar = new Sidecar();

  // Observe the FULL state stream so we can count `recovering` transitions (= relaunches).
  const states = [];
  let recoveringCount = 0;
  sidecar.onState(({ state, reason }) => {
    states.push(state);
    if (state === "recovering") recoveringCount += 1;
    console.log(`[smoke-crash-recovery] state=${state}${reason ? ` (${reason})` : ""}`);
  });

  // --- 1. Boot + seed in-flight state -----------------------------------------------------
  await sidecar.start();
  if (sidecar.currentState !== "ready") return fail(`initial state=${sidecar.currentState}, expected ready`);
  const firstPid = sidecar.pid;
  console.log(`[smoke-crash-recovery] sidecar booted (pid=${firstPid}) ✓`);

  // Seed via the sidecar's OWN command surface so the rows land in the same nyx.db: a project
  // + a managed command (which materializes one instance), then start that instance so it is
  // persisted `running`, and create a terminal that will host an agent session.
  let instanceId;
  let rootWsId;
  try {
    const proj = await invoke(sidecar, "create_project", {
      name: "crash-proj",
      rootPath: dataDir, // a real, existing dir so the subfolder resolver is happy
      rootName: "root",
    });
    const projectId = proj && proj.project ? proj.project.id : null;
    if (!projectId) return fail("create_project returned no project id");
    const tmpl = await invoke(sidecar, "command_create", {
      projectId,
      name: "watch",
      command: "sleep 999",
      subfolder: null,
      restartOnStartup: false,
    });
    // The template materialized one instance per workspace (root only here). Find it.
    rootWsId = proj.root && proj.root.id ? proj.root.id : null;
    const instances = await invoke(sidecar, "command_instance_list", { workspaceId: rootWsId });
    if (!Array.isArray(instances) || instances.length === 0) {
      return fail(`no command instance materialized (got ${JSON.stringify(instances)})`);
    }
    instanceId = instances[0].id;
    // START it -> the sidecar spawns the off-screen PTY and persists last_state='running'.
    const started = await invoke(sidecar, "command_start", { instanceId });
    if (started !== "running") return fail(`command_start did not report running (got ${JSON.stringify(started)})`);
    console.log(`[smoke-crash-recovery] seeded a RUNNING managed-command instance ${instanceId} ✓`);
    void tmpl;
  } catch (e) {
    return fail(`seeding in-flight state threw: ${e && e.message ? e.message : e}`);
  }

  // Give the running marker a beat to persist.
  await new Promise((r) => setTimeout(r, 200));

  // --- 2. CRASH the sidecar (SIGKILL, not the ordered stop) -------------------------------
  console.log(`[smoke-crash-recovery] CRASHING the sidecar (SIGKILL pid=${firstPid}) …`);
  try {
    process.kill(firstPid, "SIGKILL");
  } catch (e) {
    return fail(`could not SIGKILL the sidecar pid ${firstPid}: ${e && e.message ? e.message : e}`);
  }

  // --- 3. Assert EXACTLY ONE automatic relaunch ------------------------------------------
  // The manager should transition: ready -> recovering -> starting -> ready, with a NEW pid.
  await waitFor(() => sidecar.currentState === "ready" && sidecar.pid !== firstPid, 20000, "relaunch to ready");
  const secondPid = sidecar.pid;
  if (secondPid === firstPid) return fail("relaunch reused the dead pid (no real respawn)");
  if (recoveringCount !== 1) return fail(`expected exactly ONE relaunch (recovering x1), saw ${recoveringCount}`);
  console.log(
    `[smoke-crash-recovery] EXACTLY ONE relaunch ✓ — pid ${firstPid} -> ${secondPid}, recovering x${recoveringCount}`,
  );

  // --- 4. Assert state rebuilt (crash treated as a clean shutdown — v2.5.8 revert) --------
  // SQLite was reopened + state rebuilt: the previously-seeded instance is still queryable via
  // command_instance_list. A crash carries NO interrupted surface anymore (the `recovery_snapshot`
  // db-command was removed), and nothing is blindly relaunched: the fresh runner is empty on boot
  // (restart_on_startup is OFF on the seeded template), so the instance is not live-running.
  let instancesAfter;
  try {
    instancesAfter = await invoke(sidecar, "command_instance_list", { workspaceId: rootWsId });
  } catch (e) {
    return fail(`post-relaunch command_instance_list threw: ${e && e.message ? e.message : e}`);
  }
  const rebuilt =
    Array.isArray(instancesAfter) && instancesAfter.find((i) => i && i.id === instanceId);
  if (!rebuilt) {
    return fail(`the seeded instance ${instanceId} was not rebuilt after the crash relaunch (got ${JSON.stringify(instancesAfter)})`);
  }
  console.log(`[smoke-crash-recovery] SQLite reopened + instance ${instanceId} rebuilt ✓ (crash treated as clean; no interrupted surface)`);

  // --- 5. CRASH again -> assert NO second relaunch (the latch holds) ---------------------
  console.log(`[smoke-crash-recovery] CRASHING again (SIGKILL pid=${secondPid}) to prove the single-relaunch latch …`);
  try {
    process.kill(secondPid, "SIGKILL");
  } catch (e) {
    return fail(`could not SIGKILL the relaunched sidecar pid ${secondPid}: ${e && e.message ? e.message : e}`);
  }
  // It must settle in `degraded` (surfaced, not relaunched) and NOT loop back to recovering.
  await waitFor(() => sidecar.currentState === "degraded", 10000, "degraded after the second crash");
  // No new relaunch happened: the recovering count is still 1, and the sidecar is not alive.
  if (recoveringCount !== 1) return fail(`a SECOND relaunch happened (recovering x${recoveringCount}) — the latch failed`);
  if (sidecar.alive) return fail("sidecar relaunched after the second crash (no single-relaunch latch)");
  console.log(`[smoke-crash-recovery] single-relaunch latch holds ✓ — second crash -> degraded, recovering x${recoveringCount}`);

  console.log(
    "[smoke-crash-recovery] OK — kill -> exactly one relaunch, SQLite reopened + state rebuilt, " +
      "crash treated as a clean shutdown (no interrupted surface, silent resume); a second crash does NOT relaunch.",
  );
  cleanup();
  app.exit(0);
});

setTimeout(() => fail("timed out"), 60000);
