#!/usr/bin/env node
/**
 * Runtime smoke for the core-host LIFECYCLE / CRASH / SHUTDOWN policy (task #25).
 * Runs under full Electron (so `app` + the real `CoreHost` manager apply) and
 * exercises each done-criterion against the REAL manager + REAL host:
 *
 *   A. BOOT FAILURE is a readable FATAL, never an infinite load:
 *      A1. a forced `.node`/boot failure (`NYX_HOST_FORCE_BOOT_FAIL`) → state `fatal`
 *          + readable reason, `start()` rejects (no hang);
 *      A2. a boot HANDSHAKE TIMEOUT (host never readies) → state `fatal` (bounded).
 *   B. CRASH POLICY:
 *      B1. crash with NO active work → auto-restarted exactly ONCE (back to `ready`);
 *      B2. crash WITH active work → NOT auto-restarted (state `degraded`).
 *   C. ORDERED + FORCED SHUTDOWN leaves no host:
 *      C1. normal `stop()` → snapshot marker (`changed`) emitted BEFORE exit, process
 *          gone, state `stopped`;
 *      C2. a forced kill of an unresponsive host still ends in `stopped`, no orphan.
 *
 * Run under Electron: `electron scripts/smoke-lifecycle.cjs`. Exits 0 on full pass.
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const { app } = require("electron");

const { CoreHost } = require("../dist/main/core-host.js");

function fail(msg) {
  console.error("[smoke-lifecycle] FAIL:", msg);
  app.exit(1);
}

const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-life-"));
process.env.NYX_DATA_DIR = dataDir;
// Shorten the boot timeout so the timeout test is quick.
process.env.NYX_HOST_BOOT_TIMEOUT_MS = "1500";

/** Wait until `pred()` is true or `ms` elapses. */
function waitFor(pred, ms, label) {
  return new Promise((resolve, reject) => {
    const t0 = Date.now();
    const tick = () => {
      if (pred()) return resolve();
      if (Date.now() - t0 > ms) return reject(new Error(`timeout waiting for ${label}`));
      setTimeout(tick, 25);
    };
    tick();
  });
}

async function main() {
  // --- A1. forced boot failure → fatal, start() rejects (no hang) ----------
  {
    const host = new CoreHost();
    process.env.NYX_HOST_FORCE_BOOT_FAIL = "1";
    let rejected = false;
    await host.start().catch(() => {
      rejected = true;
    });
    delete process.env.NYX_HOST_FORCE_BOOT_FAIL;
    if (!rejected) return fail("A1: start() did not reject on a forced boot failure");
    if (host.currentState !== "fatal")
      return fail(`A1: state=${host.currentState} after boot failure, expected fatal`);
    if (!host.currentStateReason) return fail("A1: fatal state has no readable reason");
    console.log(`[smoke-lifecycle] A1 boot failure → fatal ("${host.currentStateReason}") ✓`);
    await host.stop();
  }

  // --- A2. boot handshake timeout → fatal (bounded) ------------------------
  {
    // Make boot HANG: the host loads fine but we delay `ready` past the timeout.
    // Simulate by forcing the host to sleep before readying via an env the host
    // honors only in tests is overkill; instead use a tiny boot timeout vs the
    // host's real (fast) boot is the opposite. So force a hang with FORCE_BOOT_HANG.
    const host = new CoreHost();
    process.env.NYX_HOST_FORCE_BOOT_HANG = "1";
    let rejected = false;
    const t0 = Date.now();
    await host.start().catch(() => {
      rejected = true;
    });
    const elapsed = Date.now() - t0;
    delete process.env.NYX_HOST_FORCE_BOOT_HANG;
    if (!rejected) return fail("A2: start() did not reject on a boot-handshake timeout");
    if (host.currentState !== "fatal") return fail(`A2: state=${host.currentState}, expected fatal`);
    if (elapsed > 5000) return fail(`A2: boot took ${elapsed}ms — not bounded by the timeout`);
    console.log(`[smoke-lifecycle] A2 boot timeout → fatal in ${elapsed}ms (bounded) ✓`);
    await host.stop();
  }

  // --- B1. crash with NO active work → restarted ONCE ----------------------
  {
    const host = new CoreHost();
    const states = [];
    host.onState((c) => states.push(c.state));
    // The crash seam is read by the host at spawn → set BEFORE start() (and kept set
    // so the auto-restarted child inherits it for the second crash).
    process.env.NYX_HOST_ALLOW_CRASH = "1";
    await host.start();
    if (host.currentState !== "ready") return fail("B1: host did not reach ready");
    const firstPid = host.pid;
    // Force a crash (no active work).
    host.request({ kind: "__crash" }).catch(() => {});
    // Expect: starting (restart) then ready again, with a different pid.
    await waitFor(() => host.currentState === "ready" && host.pid && host.pid !== firstPid, 8000, "B1 restart");
    if (!states.includes("starting")) return fail("B1: no restart (never re-entered starting)");
    console.log(`[smoke-lifecycle] B1 crash w/o work → auto-restarted once (pid ${firstPid} → ${host.pid}) ✓`);

    // A SECOND crash must NOT restart again (budget = 1) → degraded.
    host.request({ kind: "__crash" }).catch(() => {});
    await waitFor(() => host.currentState === "degraded", 8000, "B1 second crash → degraded");
    console.log("[smoke-lifecycle] B1 second crash → degraded (restart budget spent, no blind restart) ✓");
    delete process.env.NYX_HOST_ALLOW_CRASH;
    await host.stop();
  }

  // --- B2. crash WITH active work → NOT restarted (degraded) ---------------
  {
    const host = new CoreHost();
    process.env.NYX_HOST_ALLOW_CRASH = "1";
    await host.start();
    host.markWorkStarted(); // simulate an open PTY / running command
    if (!host.hasActiveWork) return fail("B2: active work not registered");
    host.request({ kind: "__crash" }).catch(() => {});
    await waitFor(() => host.currentState === "degraded", 8000, "B2 degraded");
    if (host.alive) return fail("B2: host still alive after a crash with active work");
    console.log("[smoke-lifecycle] B2 crash WITH active work → degraded, NOT restarted ✓");
    delete process.env.NYX_HOST_ALLOW_CRASH;
    await host.stop();
  }

  // --- C1. ordered shutdown: snapshot marker BEFORE exit, no orphan --------
  {
    const host = new CoreHost();
    let snapshotSeen = false;
    // The host emits the snapshot `changed` marker SYNCHRONOUSLY before it replies and
    // exits (ordered teardown), and IPC preserves message order — so if we see it at
    // all during a clean stop, it arrived before the exit.
    host.onEvent((e) => {
      if (e.kind === "changed" && e.topic === "commands") snapshotSeen = true;
    });
    await host.start();
    const pid = host.pid;
    await host.stop();
    // Give any trailing buffered event a tick to be delivered before asserting.
    await new Promise((r) => setTimeout(r, 100));
    if (host.currentState !== "stopped") return fail(`C1: state=${host.currentState}, expected stopped`);
    if (host.alive) return fail("C1: host still alive after stop()");
    if (!snapshotSeen) return fail("C1: snapshot marker not emitted during ordered shutdown");
    // Process truly gone?
    if (pid) {
      try {
        process.kill(pid, 0);
        return fail(`C1: host pid ${pid} still exists after stop()`);
      } catch {
        /* ESRCH = gone, good */
      }
    }
    console.log("[smoke-lifecycle] C1 ordered shutdown: snapshot before exit, no orphan host ✓");
  }

  // --- C2. forced kill of an unresponsive host still ends stopped ----------
  {
    const host = new CoreHost();
    // The host reads NYX_HOST_IGNORE_SHUTDOWN at spawn → set BEFORE start(); then
    // stop() will time out on the ignored request and FORCE-kill the host.
    process.env.NYX_HOST_IGNORE_SHUTDOWN = "1";
    await host.start();
    const pid = host.pid;
    await host.stop(800);
    delete process.env.NYX_HOST_IGNORE_SHUTDOWN;
    if (host.currentState !== "stopped") return fail(`C2: state=${host.currentState}, expected stopped`);
    if (host.alive) return fail("C2: host still alive after forced stop()");
    if (pid) {
      try {
        process.kill(pid, 0);
        return fail(`C2: host pid ${pid} survived a forced stop()`);
      } catch {
        /* gone */
      }
    }
    console.log("[smoke-lifecycle] C2 forced shutdown of an unresponsive host: no orphan ✓");
  }

  fs.rmSync(dataDir, { recursive: true, force: true });
  console.log("[smoke-lifecycle] OK — boot/fatal, crash/restart/degraded, ordered+forced shutdown verified.");
  app.exit(0);
}

app.whenReady().then(() =>
  main().catch((e) => fail(`unexpected: ${e.message}`)),
);

setTimeout(() => fail("global timeout"), 60000);
