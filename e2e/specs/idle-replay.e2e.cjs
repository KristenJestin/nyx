/* eslint-disable */
// Investigation evidence for "terminals periodically self-replay the previous
// session" (human review finding 01KV1J6M9Z3XXQ84VZG5D2MXQR), against the REAL
// nyx app (clean RELEASE build) via tauri-driver + WebKitWebDriver under WSLg.
//
// Hypothesis (from code analysis): the periodic remount/self-replay the user saw
// (Image 8) was a `tauri dev` HMR artifact — each hot reload re-bootstraps the
// app, the TerminalDeck remounts, and each restored terminal re-injects its
// "previous session" dead-history. In a production/release build there is NO HMR,
// and the production auto-attach loop mutates terminal records IN PLACE (same
// React key = record id) so it never remounts a terminal, and it stops touching a
// terminal once it is attached. So at idle, nothing should remount/replay.
//
// This spec provides the RELEASE-build evidence: open several terminals
// (incl. a workspace-attached one), write known content into them, idle ~35s,
// and assert NOTHING self-grows — no terminal accumulates a repeated "previous
// session" separator, and each buffer keeps exactly the content we put there.
//
// (NB: under the e2e build flag the live background auto-attach loop is disabled
// so the specs can drive auto-attach deterministically; this idle check therefore
// proves the DECK/render layer never self-remounts at idle. The production loop's
// non-churn is established separately by code analysis — it only setState()s on a
// real one-time match and never remounts, keying each <Terminal> on its stable
// record id.)

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");

const ROOT = fs.realpathSync(os.tmpdir());
const RUN = "nyx-idle-e2e-" + process.pid;
const WS_ROOT = (() => {
  const p = path.join(ROOT, RUN, "ws-root");
  fs.mkdirSync(p, { recursive: true });
  return fs.realpathSync(p);
})();

const IDLE_MS = 35000;
const PREV_SESSION = "previous session"; // the dead-history separator label

async function waitForApp() {
  await browser.waitUntil(
    async () => browser.execute(() => !!(window.__nyx && window.__nyx.activeId() != null)),
    { timeout: 30000, timeoutMsg: "window.__nyx active terminal never appeared" },
  );
  await browser.pause(1200);
}

const listRecords = () => browser.execute(() => (window.__nyx ? window.__nyx.list() : []));
const activeId = () => browser.execute(() => (window.__nyx ? window.__nyx.activeId() : null));
const typeInto = (id, text) => browser.execute((i, t) => window.__nyx.typeInto(i, t), id, text);
const readBuffer = (id) =>
  browser.execute((i) => (window.__nyx ? window.__nyx.readBuffer(i) : ""), id);

async function openTerminalAt(dir) {
  const before = (await listRecords()).map((r) => r.id);
  await browser.execute((c) => window.__nyx.create(c), dir);
  await browser.waitUntil(async () => (await listRecords()).length > before.length, {
    timeout: 15000,
    timeoutMsg: "new terminal record did not appear",
  });
  await browser.pause(700);
  const after = await listRecords();
  const fresh = after.filter((r) => before.indexOf(r.id) === -1);
  return fresh[fresh.length - 1].id;
}

async function waitForBuffer(id, needle, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = "";
  while (Date.now() < deadline) {
    last = await readBuffer(id);
    if (last.indexOf(needle) !== -1) return last;
    await browser.pause(200);
  }
  throw new Error("timed out waiting for '" + needle + "' in " + id);
}

function countOccurrences(haystack, needle) {
  if (!haystack) return 0;
  let n = 0;
  let i = 0;
  while ((i = haystack.indexOf(needle, i)) !== -1) {
    n++;
    i += needle.length;
  }
  return n;
}

describe("idle: terminals do NOT self-replay previous session (release build)", function () {
  this.timeout(120000);

  before(async function () {
    await waitForApp();
  });

  it("stays idle ~35s with several terminals open and nothing self-replays/grows", async function () {
    // Terminal 1: the bootstrap (loose) terminal.
    const t1 = await activeId();
    // Terminal 2: a workspace-attached terminal (the kind the user saw replaying).
    const proj = await browser.execute(
      (n, r) => window.__nyx.createProject(n, r, "root"),
      "idle-proj",
      WS_ROOT,
    );
    const wsId = proj.root.id;
    const t2 = await openTerminalAt(WS_ROOT);
    await browser.execute((i) => window.__nyx.autoAttach(i), t2);
    await browser.waitUntil(
      async () => {
        const rec = (await listRecords()).find((r) => r.id === t2);
        return rec && rec.workspace_id === wsId;
      },
      { timeout: 10000, timeoutMsg: "t2 never attached to its workspace" },
    );

    const ids = [t1, t2];

    // Write a known marker into each so each buffer has stable, identifiable
    // content we can re-check after idling.
    const markers = {};
    for (const id of ids) {
      await browser.execute((i) => window.__nyx.setActive(i), id);
      await browser.pause(250);
      const marker = "IDLE_MARK_" + id;
      markers[id] = marker;
      await typeInto(id, 'printf "%s\\n" ' + marker + "\n");
      await waitForBuffer(id, marker, 15000);
    }

    // Snapshot BEFORE idling: buffer text, its length, and the count of any
    // "previous session" separators (fresh terminals have NONE).
    const before = {};
    for (const id of ids) {
      const buf = await readBuffer(id);
      before[id] = {
        len: buf.length,
        prev: countOccurrences(buf, PREV_SESSION),
        hasMarker: buf.indexOf(markers[id]) !== -1,
      };
      assert(before[id].hasMarker, "marker present in " + id + " before idle");
    }

    // IDLE. If terminals were periodically remounting/self-replaying, new
    // "previous session" separators or duplicated content would accumulate.
    await browser.pause(IDLE_MS);

    // The app must still be alive and on the same set of terminals (no remount
    // churn dropping/recreating records).
    const afterIds = (await listRecords()).map((r) => r.id);
    for (const id of ids) {
      assert(afterIds.indexOf(id) !== -1, "terminal " + id + " still present after idle");
    }

    // Re-check each buffer: the marker is still there exactly once, no NEW
    // "previous session" separator appeared, and the buffer did not balloon with
    // self-injected content (allow a small slack for cursor/prompt repaint).
    for (const id of ids) {
      const buf = await readBuffer(id);
      const prev = countOccurrences(buf, PREV_SESSION);
      const marks = countOccurrences(buf, markers[id]);
      assert.strictEqual(
        prev,
        before[id].prev,
        "terminal " +
          id +
          " must NOT accumulate 'previous session' separators " +
          "(before=" +
          before[id].prev +
          " after=" +
          prev +
          ")",
      );
      assert.strictEqual(
        marks,
        1,
        "terminal " + id + " marker must appear exactly once (no self-replay); got " + marks,
      );
      // Buffer length should be essentially stable (idle shell). A self-replay
      // would re-inject the whole scrollback and roughly double it; allow a
      // generous 256-char slack for benign repaint/prompt redraw.
      assert(
        Math.abs(buf.length - before[id].len) <= 256,
        "terminal " +
          id +
          " buffer length must be stable at idle " +
          "(before=" +
          before[id].len +
          " after=" +
          buf.length +
          ")",
      );
    }
  });

  after(function () {
    try {
      fs.rmSync(path.join(ROOT, RUN), { recursive: true, force: true });
    } catch {}
  });
});
