/* eslint-disable */
// v2.5.6 cockpit gate — DRAG-REORDER + RELAUNCH PERSISTENCE, VERIFY half (session 2). Boots a
// FRESH app on the SAME NYX_DATA_DIR the seed used (shared key "cockpit-v256-reorder") and asserts
// the reorder PERSISTED across the real relaunch, plus the v2.2 / v2.5.5 non-regression:
//   - the rail project order == the seed's observed post-drag order;
//   - project B's terminal row order == the seed's observed post-drag order;
//   - the terminal RECORDS restored (persisted manual labels) and their panes re-mount + accept
//     input (the PTYs re-spawned — v2.2 workbench "reprise");
//   - project B's "Resume agent sessions" setting persisted ON (v2.5.5).

const assert = require("assert");
const h = require("./cockpit-v256-helpers.cjs");

const { readHandoff } = h.makeHandoff("cockpit-v256-reorder-handoff.json");

describe("v2.5.6 cockpit — reorder verify (relaunch: orders + terminals persisted)", function () {
  let expected;

  before(async function () {
    expected = readHandoff();
    await h.waitForWorkShell();
    // The relaunch restores the last-entered project; enter B explicitly to be deterministic.
    await h.waitAria("Project " + expected.projectB, 15000);
    await h.enterProject(expected.projectB);
    await browser.pause(500);
  });

  it("restored the rail PROJECT order from before the relaunch", async function () {
    const order = await h.railProjectOrder();
    assert.deepStrictEqual(
      order,
      expected.expectedProjectOrder,
      "the reordered project order must persist across a relaunch",
    );
  });

  it("restored project B's TERMINAL row order from before the relaunch", async function () {
    await browser.waitUntil(
      async () => {
        const names = await h.terminalRowNames();
        return names.length === expected.expectedTerminalOrder.length;
      },
      { timeout: 15000, timeoutMsg: "project B's terminals did not restore" },
    );
    const rows = await h.terminalRowNames();
    assert.deepStrictEqual(
      rows,
      expected.expectedTerminalOrder,
      "the reordered terminal order must persist across a relaunch",
    );
  });

  it("restored the terminal RECORDS as LIVE panes (workbench reprise, v2.2)", async function () {
    const ids = Object.values(expected.terminalIds);
    const deckIds = await h.deckTerminalIds();
    for (const id of ids) {
      assert(deckIds.indexOf(id) !== -1, "terminal " + id + " must restore a deck pane");
    }
    // The restored PTYs are live: type a fresh marker into one and read it back.
    const oneId = ids[0];
    const marker = "REORD_VERIFY_" + Date.now();
    assert(await h.typeDeck(oneId, 'echo "' + marker + '"\n'), "restored terminal must accept input");
    await h.waitForDeck(oneId, marker, 15000);
  });

  it("persisted project B's 'Resume agent sessions' setting (v2.5.5)", async function () {
    const treeB = (await h.listProjectTrees()).find((t) => t.project.name === expected.projectB);
    assert(treeB, "project B must still exist after relaunch");
    assert(
      treeB.project.resume_agent_sessions === true,
      "the resume-agent-sessions setting must persist ON across a relaunch",
    );
  });
});
