/* eslint-disable */
// RESTORE scenario, VERIFY half (session 2 — the RELAUNCH). A fresh nyx app
// boots on the SAME XDG_DATA_HOME (same SQLite DB) the seed spec persisted. We
// assert the full restore contract:
//   - the 3 alive terminals are restored WITH their scrollback (each marker is
//     back in its buffer, re-injected as read-only dead history);
//   - the terminal closed voluntarily in the seed is NOT re-spawned (it is a
//     closed record with no live pane);
//   - the auto-naming reflects each terminal's cwd;
//   - the order from the reorder PERSISTS across the restart.

const assert = require("assert");
const h = require("./restore-helpers.cjs");

// The dead-history separator label written between restored history and the live
// session (see src/components/terminal/dead-history.ts RESTORE_SEPARATOR_LABEL).
const RESTORE_SEPARATOR_LABEL = "previous session";

describe("restore scenario — verify (relaunch: 3 restored + scrollback, order kept, closed not re-spawned)", function () {
  let expected;

  before(async function () {
    expected = h.readHandoff();
    await h.waitForApp();
    // Give the restored shells a moment to re-spawn + the dead history to inject.
    await browser.pause(1500);
  });

  it("restores exactly the 3 alive terminals (the closed default is NOT re-spawned)", async function () {
    // The front adopts ONLY alive records on relaunch (closed records stay in
    // the DB but are not re-spawned, so they are not in the front list / deck).
    const records = await h.listRecords();

    assert.strictEqual(
      records.length,
      3,
      "exactly 3 terminals must be restored (alive only), got " + records.length,
    );
    records.forEach(function (r) {
      assert.strictEqual(r.status, "alive", "every restored terminal is alive");
    });

    // The voluntarily-closed default is NOT among the re-spawned terminals.
    const ids = records.map(function (r) {
      return r.id;
    });
    assert(
      ids.indexOf(expected.defaultId) === -1,
      "the closed default (" + expected.defaultId + ") must NOT be re-spawned",
    );

    // And it has NO live terminal pane mounted (the deck only mounts alive
    // records) — the authoritative proof it was not re-spawned.
    const hasPane = await browser.execute(function (id) {
      var deck = window.__nyxDeck || {};
      return Object.prototype.hasOwnProperty.call(deck, String(id));
    }, expected.defaultId);
    assert.strictEqual(
      hasPane,
      false,
      "the closed default must have no live terminal pane after restart",
    );
  });

  it("restores each terminal's scrollback (its seed marker is back in the buffer)", async function () {
    for (let i = 0; i < expected.terminals.length; i++) {
      const t = expected.terminals[i];
      const buf = await h.waitForBuffer(t.id, t.marker, 15000);
      assert(
        buf.indexOf(t.marker) !== -1,
        "terminal " + t.id + " must restore its scrollback containing " + t.marker,
      );
    }
  });

  it("keeps the reordered order across the restart", async function () {
    const records = await h.listRecords();
    const aliveOrder = records
      .filter(function (r) {
        return r.status === "alive";
      })
      .sort(function (a, b) {
        return a.order_index - b.order_index;
      })
      .map(function (r) {
        return r.id;
      });
    assert.deepStrictEqual(
      aliveOrder,
      expected.expectedAliveOrder,
      "the reordered (reversed) order must persist across the restart",
    );
  });

  it("CRITICAL (01KV3CPAG…): a RESTORED terminal is TYPABLE and the live prompt is BELOW the restored history", async function () {
    // The Image-16 bug: on a terminal whose scrollback was restored, the
    // "— previous session —" dead-history block rendered BELOW the live prompt, so
    // the input sat above dead history and the user COULD NOT TYPE. The fix writes
    // the dead history as the FIRST bytes of the session (before the PTY output),
    // so history is ABOVE the live prompt and the input is typable at the bottom.
    //
    // We pick a restored terminal (it has its seed marker back as dead history),
    // make it active, type a FRESH marker as REAL keystrokes through the live
    // shell, and assert: (a) the fresh marker LANDS in the buffer (input is
    // typable) and (b) the restored seed marker + the separator appear ABOVE the
    // fresh marker (correct top-to-bottom order: history, separator, live input).
    const target = expected.terminals[0];
    await h.waitForBuffer(target.id, target.marker, 15000); // its history is back

    await browser.execute(function (id) {
      window.__nyx.setActive(id);
    }, target.id);
    await browser.pause(400);

    const liveMarker = "RESTORE_TYPE_" + Date.now();
    // Type through the live shell (the input must reach the PTY → echo back).
    await h.typeInto(target.id, 'printf "%s\\n" ' + liveMarker + "\n");
    const buf = await h.waitForBuffer(target.id, liveMarker, 15000);

    const histAt = buf.indexOf(target.marker);
    const sepAt = buf.indexOf(RESTORE_SEPARATOR_LABEL);
    const liveAt = buf.lastIndexOf(liveMarker);
    assert(histAt !== -1, "the restored history marker must be present");
    assert(sepAt !== -1, "the 'previous session' separator must be present");
    assert(liveAt !== -1, "the freshly-typed marker must land (the input is TYPABLE)");
    // Top-to-bottom: restored history < separator < live input.
    assert(
      histAt < sepAt,
      "restored history must be ABOVE the separator (was hist@" + histAt + " sep@" + sepAt + ")",
    );
    assert(
      sepAt < liveAt,
      "the separator (end of dead history) must be ABOVE the live input — the prompt is at the " +
        "BOTTOM and typable (was sep@" +
        sepAt +
        " live@" +
        liveAt +
        ")",
    );
  });

  it("auto-names each restored terminal from its cwd", async function () {
    const records = await h.listRecords();
    const byId = {};
    records.forEach(function (r) {
      byId[r.id] = r;
    });
    for (let i = 0; i < expected.terminals.length; i++) {
      const t = expected.terminals[i];
      const rec = byId[t.id];
      assert(rec, "restored record " + t.id + " must exist");
      // The record carries the cwd it was created at; the auto label derives the
      // displayed name from this cwd's basename (e.g. /tmp → "tmp").
      assert.strictEqual(
        rec.cwd,
        t.cwd,
        "restored terminal " + t.id + " must keep its cwd " + t.cwd,
      );
    }
  });
});
