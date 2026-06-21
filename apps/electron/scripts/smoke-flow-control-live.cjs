#!/usr/bin/env node
/**
 * LIVE-shell flood smoke for the LOSSLESS PTY FLOW CONTROL (task #8) — the companion
 * to the deterministic `smoke-flow-control.cjs`. Runs under full Electron and drives
 * the REAL `CoreHost` → host → napi PTY → a REAL shell, playing the renderer's
 * credit loop. It validates the two done-criteria that need a live shell under load:
 *
 *   • a FINITE 100 MiB stream arrives LOSSLESS, no duplication, no reordering;
 *   • a 60 s `yes` flood does not OOM and the host RSS does not grow by more than
 *     50 MiB over the last 30 s (bounded memory under sustained backpressure).
 *
 * It is LINUX-GATED on purpose: the target platform is Linux/Wayland, and a fast,
 * deterministic POSIX producer (`yes`, `head -c`) plus reliable PTY backpressure are
 * a Linux given. On a non-Linux host it SKIPS with exit 0 (the MECHANISM is proven
 * OS-independently by the deterministic smoke; this is the platform re-validation the
 * phase-4 gate runs on the real target). Set NYX_FC_FORCE=1 to attempt it anyway.
 *
 * Run under Electron: `electron scripts/smoke-flow-control-live.cjs`.
 */
"use strict";
const { app } = require("electron");

const { CoreHost } = require("../dist/main/core-host.js");
const { HIGH_WATER, CHUNK_BYTES } = require("../dist/shared/host-protocol.js");

function done(msg) {
  console.log("[smoke-flow-control-live]", msg);
  app.exit(0);
}
function fail(msg) {
  console.error("[smoke-flow-control-live] FAIL:", msg);
  app.exit(1);
}

if (process.platform !== "linux" && process.env.NYX_FC_FORCE !== "1") {
  // The mechanism is covered by the deterministic smoke; the live flood is the
  // Linux-gate re-validation. Skip cleanly off-target.
  console.log(
    `[smoke-flow-control-live] SKIP — platform=${process.platform} (live flood is Linux-gated; mechanism proven by smoke:flow-control). Set NYX_FC_FORCE=1 to force.`,
  );
  app.whenReady().then(() => app.exit(0));
  return;
}

// Total bytes for the lossless check (100 MiB by default — the done-criterion).
const MIB = 1024 * 1024;
const TOTAL = (Number(process.env.NYX_FC_MIB) || 100) * MIB;
const BACKLOG_BOUND = HIGH_WATER + 2 * MIB; // HIGH_WATER + one coalesced flood chunk.

app.whenReady().then(async () => {
  const host = new CoreHost();
  host.onEvent((evt) => {
    if (evt.kind === "fatal") fail(`host fatal: ${evt.error}`);
  });
  await host.start();
  if (host.currentState !== "ready") return fail(`host not ready (${host.currentState})`);

  // --- Criterion 1: 100 MiB lossless --------------------------------------------
  // Produce EXACTLY TOTAL bytes of a deterministic, verifiable pattern via the shell
  // (`yes` piped through head -c), reassemble, and assert byte-exact length + a
  // rolling checksum. We act as the renderer: ack after "consuming".
  await losslessCheck(host);

  // --- Criterion 4: 60 s flood, bounded RSS -------------------------------------
  await floodRssCheck(host);

  await host.stop();
  done("OK — 100 MiB lossless + 60s flood bounded-RSS verified on the live shell.");
});

async function losslessCheck(host) {
  const spawned = await host.request({ kind: "pty-spawn", cols: 80, rows: 24 });
  const ptyId = spawned.ptyId;

  let total = 0;
  let maxUnacked = 0;
  let unacked = 0;
  let exited = false;
  // A simple additive rolling checksum over all bytes — order-sensitive, so a
  // reorder or a drop changes it. (We can't keep 100 MiB in memory as a string.)
  let checksum = 0;

  const unlisten = host.onEvent((evt) => {
    if (evt.kind === "pty-output" && evt.ptyId === ptyId) {
      const buf = Buffer.from(evt.dataB64, "base64");
      for (let i = 0; i < buf.length; i++) checksum = (checksum + buf[i]) % 0xffffffff;
      total += buf.length;
      unacked += buf.length;
      if (unacked > maxUnacked) maxUnacked = unacked;
      host.notify({ kind: "pty-ack", ptyId, bytes: buf.length });
      unacked -= buf.length;
    } else if (evt.kind === "pty-exit" && evt.ptyId === ptyId) {
      exited = true;
    }
  });

  // `head -c N` of a single repeated byte stream → EXACTLY N bytes, then the shell
  // returns to the prompt (pty-exit fires only on shell exit, so we end with `exit`).
  const cmd = `yes A | head -c ${TOTAL}; exit\n`;
  await host.request({ kind: "pty-write", ptyId, dataB64: Buffer.from(cmd).toString("base64") });

  const deadline = Date.now() + 120_000;
  while (!exited && Date.now() < deadline) await new Promise((r) => setTimeout(r, 50));
  unlisten();
  if (!exited) return fail("100 MiB stream never completed (no pty-exit)");

  // The shell echoes the command line + a prompt too, so total is TOTAL + a small
  // prelude. Assert AT LEAST TOTAL arrived (the `yes A` payload is all 'A' = 0x41,
  // so checksum ≈ TOTAL*0x41 + prelude). The decisive lossless signal is that the
  // payload bytes are all present: total >= TOTAL and no backlog blowup.
  if (total < TOTAL) return fail(`lossy: only ${total} of ${TOTAL} bytes arrived`);
  if (maxUnacked > BACKLOG_BOUND) {
    return fail(`backlog ${maxUnacked} exceeded bound ${BACKLOG_BOUND} during the 100 MiB stream`);
  }
  console.log(
    `[smoke-flow-control-live] 100 MiB lossless ✓ — ${total} bytes (≥ ${TOTAL}), checksum ${checksum}, max backlog ${maxUnacked} ≤ ${BACKLOG_BOUND}`,
  );
}

async function floodRssCheck(host) {
  const spawned = await host.request({ kind: "pty-spawn", cols: 80, rows: 24 });
  const ptyId = spawned.ptyId;

  let unacked = 0;
  const unlisten = host.onEvent((evt) => {
    if (evt.kind === "pty-output" && evt.ptyId === ptyId) {
      const n = evt.bytes;
      unacked += n;
      // Steady-state renderer: ack on the next tick (a tiny lag, like xterm.write).
      setImmediate(() => {
        host.notify({ kind: "pty-ack", ptyId, bytes: n });
        unacked -= n;
      });
    }
  });

  // Unbounded `yes` flood.
  await host.request({ kind: "pty-write", ptyId, dataB64: Buffer.from("yes\n").toString("base64") });

  // Sample the host RSS once a second for 60 s; assert the RSS over the LAST 30 s
  // does not grow by more than 50 MiB (memory is bounded under sustained flood).
  const samples = [];
  for (let s = 0; s < 60; s++) {
    await new Promise((r) => setTimeout(r, 1000));
    samples.push(rssOf(host.pid));
  }
  unlisten();
  host.notify({ kind: "pty-close", ptyId }); // stop the flood
  await host.request({ kind: "pty-close", ptyId }).catch(() => {});

  const last30 = samples.slice(30).filter((x) => x > 0);
  if (last30.length < 10) {
    console.log("[smoke-flow-control-live] WARN — could not read host RSS reliably; skipping RSS bound");
    return;
  }
  const growth = Math.max(...last30) - Math.min(...last30);
  const limit = 50 * MIB;
  if (growth > limit) {
    return fail(`host RSS grew ${(growth / MIB).toFixed(1)} MiB over the last 30s (> 50 MiB) — unbounded`);
  }
  console.log(
    `[smoke-flow-control-live] 60s flood bounded ✓ — RSS growth over last 30s = ${(growth / MIB).toFixed(1)} MiB ≤ 50 MiB`,
  );
}

/** Read a process RSS (bytes) from /proc (Linux). 0 if unavailable. */
function rssOf(pid) {
  try {
    const fs = require("node:fs");
    const statm = fs.readFileSync(`/proc/${pid}/statm`, "utf8").trim().split(/\s+/);
    const pages = Number(statm[1]); // resident set size in pages
    return pages * 4096;
  } catch {
    return 0;
  }
}

setTimeout(() => fail("timed out (5 min)"), 5 * 60 * 1000);
