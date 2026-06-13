/* eslint-disable */
// RESTORE scenario, SEED half (session 1). Drives the REAL nyx app:
//   - opens 3 terminals at DISTINCT cwds, each running a command whose output is
//     observable (a unique marker echoed into its scrollback);
//   - reorders them to a known non-creation order;
//   - closes the app's auto-created default terminal VOLUNTARILY (so the verify
//     half can prove it is NOT re-spawned).
// It records the expected ids / markers / order to a handoff file in the shared
// data dir; the app session ends after this spec (tauri-driver kills it),
// simulating nyx quitting. The DB persists in XDG_DATA_HOME for restore-02.

const assert = require("assert");
const os = require("os");
const fs = require("fs");
const path = require("path");
const h = require("./restore-helpers.cjs");

// Three real, distinct directories so the auto-naming (cwd basename) differs per
// terminal and the per-record cwd is meaningfully distinct. The Linux system
// dirs don't exist on Windows, so there we materialize three stable,
// distinct-basename dirs under the system temp. They must exist at BOTH the seed
// and the verify session (restore re-spawns each shell at its stored cwd), so we
// use fixed names under tmp that persist across the run (not cleaned by
// onPrepare, which only wipes the e2e DB root).
const CWDS =
  process.platform === "win32"
    ? ["alpha", "beta", "gamma"].map(function (name) {
        const dir = path.join(os.tmpdir(), "nyx-e2e-cwds", name);
        fs.mkdirSync(dir, { recursive: true });
        return dir;
      })
    : ["/tmp", "/usr", "/etc"];

describe("restore scenario — seed (open 3 @ distinct cwd, reorder, close one)", function () {
  it("opens 3 terminals at distinct cwds with running commands, reorders, closes the default", async function () {
    await h.waitForApp();

    // The app auto-creates ONE default terminal on first boot (empty DB). That
    // is the one we will close voluntarily so the restore proves it is not
    // re-spawned.
    const initial = await h.listRecords();
    assert(initial.length >= 1, "app should boot with at least the default terminal");
    const defaultId = initial[0].id;

    // Open 3 terminals at distinct cwds. Each gets a UNIQUE marker echoed into
    // its scrollback (the "running command with observable output").
    const opened = [];
    for (let i = 0; i < CWDS.length; i++) {
      const id = await h.createAt(CWDS[i]);
      assert(id != null, "createAt(" + CWDS[i] + ") must return a record id");
      const marker = "SEED_MARK_" + i + "_zz" + (i + 1) * 7;
      await h.typeInto(id, 'echo "' + marker + '"\n');
      await h.waitForBuffer(id, marker, 15000);
      opened.push({ id: id, cwd: CWDS[i], marker: marker });
    }

    // Reorder the 3 opened terminals to a known NON-creation order: reverse them
    // (T3, T2, T1). We reorder among ALL current ids but place the 3 opened in
    // reverse; the default stays wherever — it is about to be closed anyway.
    const reversedOpened = opened.map(function (o) {
      return o.id;
    }).reverse();
    // Full id list with the opened ones reversed at the front, default last.
    const fullOrder = reversedOpened.concat([defaultId]);
    await h.reorder(fullOrder);

    // Close the auto-created default terminal VOLUNTARILY.
    await h.closeTerminal(defaultId);

    // Sanity: after closing the default, exactly the 3 opened remain alive.
    const after = await h.listRecords();
    const aliveIds = after
      .filter(function (r) {
        return r.status === "alive";
      })
      .map(function (r) {
        return r.id;
      });
    assert.strictEqual(
      aliveIds.length,
      3,
      "exactly 3 terminals must be alive after closing the default (got " +
        aliveIds.length +
        ")",
    );
    assert(
      aliveIds.indexOf(defaultId) === -1,
      "the closed default must no longer be alive",
    );

    // The persisted ALIVE order is the reversed-opened order (default removed).
    const aliveOrder = after
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
      reversedOpened,
      "the alive terminals must be in the reordered (reversed) order before restart",
    );

    // Hand off the expectations to the verify spec.
    h.writeHandoff({
      defaultId: defaultId,
      expectedAliveOrder: reversedOpened,
      terminals: opened, // {id, cwd, marker} in original creation order
    });
  });
});
