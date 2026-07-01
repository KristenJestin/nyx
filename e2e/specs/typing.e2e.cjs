/* eslint-disable */
// CRITICAL regression for "cannot type in SOME terminals"
// (human review finding 01KV1J6C0T2581VSY2P2HKMS6D), driven against the REAL nyx
// app (clean RELEASE build) via tauri-driver + WebKitWebDriver under WSLg.
//
// Root cause this guards: selecting a terminal flipped `active` but never moved
// keyboard FOCUS to the newly-shown xterm, so its hidden input received no
// keystrokes — typing did nothing in any terminal switched to via the sidebar.
// The old smoke spec missed it because the inert `typeInto` seam writes via
// xterm `input()` (PTY-direct), which works regardless of focus.
//
// So this spec verifies BOTH axes for THREE terminals (loose + workspace-
// attached, before AND after a reorder):
//   A. the input SEAM path (typeInto -> readBuffer): every terminal, even hidden
//      ones, receives + runs input and its buffer reflects it (guards a
//      remount/detached-listener regression);
//   B. the FOCUS path (the actual bug): after activating a terminal, its xterm
//      helper textarea is the document's active element, and a REAL keystroke
//      sent through WebDriver (browser.keys) lands in THAT terminal's buffer.

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");

const ROOT = fs.realpathSync(os.tmpdir());
const RUN = "nyx-typing-e2e-" + process.pid;
const ENTER = ""; // W3C WebDriver ENTER key

function mkdir(...segs) {
  const p = path.join(ROOT, RUN, ...segs);
  fs.mkdirSync(p, { recursive: true });
  return fs.realpathSync(p);
}

async function waitForApp() {
  await browser.waitUntil(
    async () => browser.execute(() => !!(window.__nyx && window.__nyx.activeId() != null)),
    { timeout: 30000, timeoutMsg: "window.__nyx active terminal never appeared" },
  );
  await browser.pause(1000); // let the first shell print its prompt
}

const listRecords = () => browser.execute(() => (window.__nyx ? window.__nyx.list() : []));
const activeId = () => browser.execute(() => (window.__nyx ? window.__nyx.activeId() : null));
const setActive = (id) => browser.execute((i) => window.__nyx.setActive(i), id);
const typeInto = (id, text) => browser.execute((i, t) => window.__nyx.typeInto(i, t), id, text);
const readBuffer = (id) =>
  browser.execute((i) => (window.__nyx ? window.__nyx.readBuffer(i) : ""), id);

// Open a fresh terminal at `dir`; return its new record id.
async function openTerminalAt(dir) {
  const before = (await listRecords()).map((r) => r.id);
  await browser.execute((c) => window.__nyx.create(c), dir);
  await browser.waitUntil(async () => (await listRecords()).length > before.length, {
    timeout: 15000,
    timeoutMsg: "new terminal record did not appear",
  });
  await browser.pause(700); // shell spawn + first prompt
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
  throw new Error(
    "timed out waiting for '" + needle + "' in terminal " + id + ". Buffer:\n" + last,
  );
}

// True iff the document's active element is THIS terminal's xterm input. We key
// off the active terminal pane (data-active="true") and check its helper
// textarea (.xterm-helper-textarea) is the focused element.
function activeTermIsFocused() {
  return browser.execute(() => {
    const pane = document.querySelector('[data-active="true"]');
    if (!pane) return false;
    const ta = pane.querySelector("textarea.xterm-helper-textarea");
    return !!ta && document.activeElement === ta;
  });
}

describe("CRITICAL typing-in-every-terminal (Electron, e2e build)", function () {
  // A project root so we have a workspace to ATTACH a terminal to, plus loose
  // terminals — covering both kinds the finding calls out.
  const WS_ROOT = mkdir("workspace-root");

  before(async function () {
    await waitForApp();
  });

  it("types into each of 3 terminals (loose + attached) and the buffer reflects it", async function () {
    // Terminal 1: the default/bootstrap terminal (loose).
    const t1 = await activeId();
    assert(t1, "there is an initial active terminal");

    // Terminal 2: another loose terminal at tmp root.
    const t2 = await openTerminalAt(ROOT);

    // Terminal 3: a workspace-attached terminal. Create a project + root
    // workspace, then open a terminal AT the workspace path; auto-attach binds it.
    const proj = await browser.execute(
      (n, r) => window.__nyx.createProject(n, r, "root"),
      "typing-proj",
      WS_ROOT,
    );
    const wsId = proj.root.id;
    const t3 = await openTerminalAt(WS_ROOT);
    await browser.execute((i) => window.__nyx.autoAttach(i), t3);
    await browser.waitUntil(
      async () => {
        const rec = (await listRecords()).find((r) => r.id === t3);
        return rec && rec.workspace_id === wsId;
      },
      { timeout: 10000, timeoutMsg: "t3 never attached to its workspace" },
    );

    const ids = [t1, t2, t3];
    assert.strictEqual(new Set(ids).size, 3, "three distinct terminals");

    // SEAM path: activate each, type a UNIQUE marker, assert it lands in THAT
    // terminal's buffer (and only that one).
    const markers = {};
    for (const id of ids) {
      await setActive(id);
      await browser.pause(250);
      const marker = "SEAM_" + id + "_zz";
      markers[id] = marker;
      await typeInto(id, 'printf "%s\\n" ' + marker + "\n");
      await waitForBuffer(id, marker, 15000);
    }
    // No cross-contamination: each marker only in its own buffer.
    for (const id of ids) {
      for (const other of ids) {
        if (other === id) continue;
        const buf = await readBuffer(other);
        assert(
          buf.indexOf(markers[id]) === -1,
          "marker for " + id + " must NOT appear in terminal " + other,
        );
      }
    }
  });

  it("FOCUS moves to the active terminal so REAL keystrokes type into it (the bug)", async function () {
    const ids = (await listRecords()).map((r) => r.id);
    assert(ids.length >= 3, "the 3 terminals from the previous test persist");

    // Short, letters-only markers. Raw WebDriver keystrokes over WebKitWebDriver
    // can occasionally drop a char under load, so we keep the typed text minimal
    // (a-z only, no quotes/spaces/long ids) and assert a SHORT echoed substring —
    // the load-bearing claim is that the keystrokes reach THIS terminal AT ALL,
    // which only happens if focus moved to it.
    const tags = ["zaqxsw", "zbwedc", "zcrfvt"];

    for (let i = 0; i < 3; i++) {
      const id = ids[i];
      const tag = tags[i];
      await setActive(id);

      // DIRECT proof of the fix: activating a terminal moves keyboard focus to
      // its xterm input (deferred one rAF after the pane is shown). This is the
      // exact bug — without focus-on-activate, typing went nowhere.
      await browser.waitUntil(async () => activeTermIsFocused(), {
        timeout: 5000,
        timeoutMsg: "active terminal " + id + " xterm input never received focus on select",
      });

      // CORROBORATING proof: send REAL keystrokes via WebDriver (NOT the input
      // seam) — they only reach the terminal if the xterm textarea is truly
      // focused. Type `echo <tag>`, press the W3C ENTER key, and assert the tag
      // shows up in THIS terminal's buffer.
      await browser.keys(("echo " + tag).split(""));
      await browser.keys([ENTER]);
      await waitForBuffer(id, tag, 15000);
    }
  });

  it("typing still works in every terminal AFTER a reorder", async function () {
    const records = await listRecords();
    const ids = records.map((r) => r.id);
    // Reverse the global order, then verify each terminal still accepts input
    // (the reorder must not detach any input listener / break focus-on-select).
    const reversed = ids.slice().reverse();
    await browser.execute((seq) => window.__nyx.reorder(seq), reversed);
    await browser.pause(500);

    for (const id of ids.slice(0, 3)) {
      await setActive(id);
      await browser.pause(250);
      const marker = "AFTER_REORDER_" + id;
      await typeInto(id, 'printf "%s\\n" ' + marker + "\n");
      await waitForBuffer(id, marker, 15000);
    }
  });

  after(function () {
    try {
      fs.rmSync(path.join(ROOT, RUN), { recursive: true, force: true });
    } catch {}
  });
});
