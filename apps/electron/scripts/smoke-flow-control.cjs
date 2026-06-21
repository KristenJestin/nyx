#!/usr/bin/env node
/**
 * DETERMINISTIC smoke for the LOSSLESS PTY FLOW CONTROL (task #8). Runs as PLAIN
 * NODE (no Electron, no live shell) so the MECHANISM is proven the same on every OS —
 * exactly the right altitude for logic that must not depend on a particular shell's
 * flood behavior. It drives the REAL `PtyManager` (`dist/core-host/pty-manager.js`)
 * against a FAKE `NyxPty` that lets the test PRODUCE bytes and OBSERVE `setPaused`.
 *
 * A complementary LIVE-shell flood (the full 100 MiB stream + the 60 s RSS bound)
 * runs on the Linux/Wayland gate (phase 4) where the target shells exist; this smoke
 * proves the credit/backpressure/chunking LOGIC deterministically here and now.
 *
 * Proves the done-criteria, in terms the mechanism controls:
 *
 *   1. LOSSLESS + ORDERED + 64 KiB CHUNKING: feeding a large, deterministic byte
 *      stream through the manager, the renderer reassembles EVERY byte in order
 *      (none dropped/duped/reordered), and NO emitted chunk exceeds 64 KiB.
 *
 *   2. BOUNDED BACKLOG: with the renderer WITHHOLDING acks, the unacked backlog the
 *      manager tells the renderer about never exceeds HIGH_WATER + one fed chunk, and
 *      the manager calls `setPaused(true)` at the high-water mark.
 *
 *   3. PAUSE/RESUME really gates the reader: while paused the fake reader is told to
 *      STOP producing (mirroring the Rust reader parking at its gate); once the
 *      renderer acks below the low-water mark the manager calls `setPaused(false)`
 *      and production resumes — and the WHOLE stream arrives losslessly, proving
 *      nothing was dropped across the pause.
 *
 * Exit 0 on full pass.
 */
"use strict";
const { PtyManager } = require("../dist/core-host/pty-manager.js");
const { HIGH_WATER, LOW_WATER, CHUNK_BYTES } = require("../dist/shared/host-protocol.js");

function fail(msg) {
  console.error("[smoke-flow-control] FAIL:", msg);
  process.exit(1);
}

// --- A FAKE NyxPty: the test feeds it bytes; it forwards onData and honors paused.
// `setPaused(true)` makes it DROP NOTHING — it just stops the test's producer (the
// producer checks `paused` before each feed), mirroring the Rust reader parking at
// its gate while the kernel buffers the child's output losslessly.
let fakePaused = false;
let setPausedCalls = [];
let onDataCb = null;
let onExitCb = null;

class FakeNyxPty {
  constructor(_cols, _rows, _cwd, _terminalId, onData, onExit) {
    onDataCb = onData;
    onExitCb = onExit;
  }
  write() {}
  resize() {}
  kill() {
    if (onExitCb) onExitCb(null, 0);
  }
  setPaused(p) {
    fakePaused = p;
    setPausedCalls.push(p);
  }
  id() {
    return 1;
  }
}

const fakeNapi = { version: () => "test", NyxPty: FakeNyxPty };

// --- A capturing EventSink that plays the renderer: reassemble + observe backlog.
let received = Buffer.alloc(0);
let unackedReported = 0; // what the manager has emitted but the renderer hasn't acked
let maxUnacked = 0;
let maxChunk = 0;
let chunkCount = 0;

const sink = {
  ptyOutput(_ptyId, bytes) {
    received = Buffer.concat([received, bytes]);
    chunkCount += 1;
    if (bytes.length > maxChunk) maxChunk = bytes.length;
    unackedReported += bytes.length;
    if (unackedReported > maxUnacked) maxUnacked = unackedReported;
  },
  ptyExit() {},
  ptyCwd() {},
  ptyExecState() {},
  changed() {},
};

async function main() {
  const mgr = new PtyManager(fakeNapi, sink);
  const ptyId = mgr.spawn({ cols: 80, rows: 24 });
  if (typeof ptyId !== "number") return fail("spawn did not return a numeric ptyId");

  // The deterministic stream: a sequence of distinct bytes so reassembly order is
  // verifiable. We use a rolling pattern (i % 251, a prime, so period doesn't align
  // with any power-of-two chunk boundary) over a total well past several HIGH_WATERs.
  const TOTAL = 8 * 1024 * 1024; // 8 MiB — crosses 512 KiB sixteen times.
  const expected = Buffer.alloc(TOTAL);
  for (let i = 0; i < TOTAL; i++) expected[i] = i % 251;

  // Feed in 200 KiB slices (each larger than CHUNK_BYTES so the manager MUST split
  // to ≤64 KiB — exercising the chunking) so a single fed slice past the water mark
  // bounds the backlog to HIGH_WATER + this feed size.
  const FEED = 200 * 1024;
  let fedOffset = 0;

  // The renderer's ack policy: we WITHHOLD acks until the manager pauses (to prove
  // the bound + pause), then ack everything (to prove lossless resume). A small
  // delay models the renderer's xterm.write turnaround.
  let ackingEnabled = false;

  // Producer: feeds the next slice whenever NOT paused. Runs on a timer so the event
  // loop interleaves with acks, like the real Rust reader thread vs the Node loop.
  await new Promise((resolve) => {
    const tick = () => {
      // Honor the gate: a paused "reader" produces nothing (lossless — the bytes
      // wait in `expected`, exactly as the kernel buffers them for the real reader).
      if (!fakePaused) {
        if (fedOffset < TOTAL) {
          const slice = expected.subarray(fedOffset, Math.min(fedOffset + FEED, TOTAL));
          fedOffset += slice.length;
          onDataCb(null, Buffer.from(slice));
        } else {
          // All produced: signal exit and finish.
          onExitCb(null, 0);
          clearInterval(timer);
          clearInterval(ackTimer);
          return resolve();
        }
      }

      // Once the manager has paused at the high-water mark (proving the bound), turn
      // on acking so the backlog drains, the manager resumes, and the stream
      // completes losslessly.
      if (!ackingEnabled && fakePaused) ackingEnabled = true;
    };

    // The renderer's ack loop: credit consumed bytes back so the manager can resume.
    const ackTimer = setInterval(() => {
      if (ackingEnabled && unackedReported > 0) {
        const credit = unackedReported;
        unackedReported = 0;
        mgr.ack(ptyId, credit);
      }
    }, 5);

    const timer = setInterval(tick, 1);
  });

  // --- Assertions ----------------------------------------------------------------

  // (1) 64 KiB chunking: no emitted chunk exceeds CHUNK_BYTES.
  if (maxChunk > CHUNK_BYTES) {
    return fail(`a chunk of ${maxChunk} bytes exceeded the ${CHUNK_BYTES} cap`);
  }
  console.log(
    `[smoke-flow-control] 64 KiB chunking ✓ — ${chunkCount} chunks, max ${maxChunk} ≤ ${CHUNK_BYTES}`,
  );

  // (2) the manager paused at the high-water mark, and the reported backlog stayed
  // bounded by HIGH_WATER + one fed slice.
  if (!setPausedCalls.includes(true)) {
    return fail("manager never paused — flow control did not engage at the high-water mark");
  }
  const bound = HIGH_WATER + FEED;
  if (maxUnacked > bound) {
    return fail(`unacked backlog ${maxUnacked} exceeded the bound ${bound} (HIGH_WATER + one feed)`);
  }
  console.log(
    `[smoke-flow-control] bounded backlog ✓ — max unacked ${maxUnacked} ≤ ${bound} (HIGH_WATER ${HIGH_WATER} + feed ${FEED})`,
  );

  // (3) pause AND resume both fired (the gate engaged then released).
  if (!setPausedCalls.includes(false)) {
    return fail("manager paused but never resumed — the reader would stay parked forever");
  }
  const pauses = setPausedCalls.filter((p) => p).length;
  const resumes = setPausedCalls.filter((p) => !p).length;
  console.log(
    `[smoke-flow-control] pause/resume ✓ — setPaused(true)×${pauses}, setPaused(false)×${resumes} (LOW_WATER ${LOW_WATER})`,
  );

  // (1, finale) LOSSLESS + ORDERED: every byte arrived, in order, none dropped/duped.
  if (received.length !== TOTAL) {
    return fail(`lossy: reassembled ${received.length} bytes, expected ${TOTAL}`);
  }
  if (!received.equals(expected)) {
    // Find the first divergence for a precise message.
    let at = -1;
    for (let i = 0; i < TOTAL; i++) if (received[i] !== expected[i]) { at = i; break; }
    return fail(`stream corrupted/reordered at byte ${at}`);
  }
  console.log(
    `[smoke-flow-control] lossless + ordered ✓ — all ${TOTAL} bytes reassembled exactly across the pause`,
  );

  console.log(
    "[smoke-flow-control] OK — lossless flow control verified deterministically (chunking, bounded backlog, real pause/resume).",
  );
  process.exit(0);
}

setTimeout(() => fail("timed out (60s)"), 60_000);
main().catch((e) => fail(e.stack || String(e)));
