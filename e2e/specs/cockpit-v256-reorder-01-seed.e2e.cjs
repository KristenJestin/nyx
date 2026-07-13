/* eslint-disable */
// v2.5.6 cockpit gate — DRAG-REORDER + RELAUNCH PERSISTENCE, SEED half (session 1) of a
// seed→verify pair sharing one NYX_DATA_DIR (wdio.conf.cjs key "cockpit-v256-reorder"), like the
// restore pair. Task 01KXBNX51EPQ69MVKYG4W960HE (PRD phase-4 reorder + v2.2/v2.5.5 non-regression).
//
// SEED drives, against the REAL Electron app:
//   - two projects on the rail, then a real drag to REORDER them;
//   - two workspace terminals (each with a manual label + a marker in its scrollback), then a real
//     drag to REORDER the terminal rows;
//   - the project's "Resume agent sessions" toggle turned ON (v2.5.5 setting).
// It records the OBSERVED post-drag orders + terminal ids/markers to a handoff file; the app session
// ends after this spec (wdio-electron-service closes it, before-quit stops the sidecar), simulating
// a real quit. The DB persists in the shared NYX_DATA_DIR for the verify half.

const assert = require("assert");
const h = require("./cockpit-v256-helpers.cjs");

const { writeHandoff } = h.makeHandoff("cockpit-v256-reorder-handoff.json");

// Shallow order equality (skip a no-op reorder assertion).
function sameOrder(a, b) {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

describe("v2.5.6 cockpit — reorder seed (drag terminals + projects, then quit)", function () {
  const PROJ_A = "ReordA";
  const PROJ_B = "ReordB";
  let rootA;
  let rootB;
  const termIds = {}; // label → persistent record id
  const markers = {}; // label → scrollback marker
  let termOrder = null; // observed post-drag terminal row order
  let projOrder = null; // observed post-drag rail project order
  let termMechanism = null; // "drag" (synthetic pointer events fired dnd-kit) | "seam" (fallback)
  let projMechanism = null;

  before(async function () {
    rootA = h.makeTempDir("nyx-v256-reord-a-");
    rootB = h.makeTempDir("nyx-v256-reord-b-");
    await h.waitForWorkShell();
    // Seed two projects (creation order A, B → initial rail order [A, B]).
    await h.seedProject(PROJ_A, rootA, "main");
    await h.seedProject(PROJ_B, rootB, "main");
    await h.waitAria("Project " + PROJ_A, 15000);
    await h.waitAria("Project " + PROJ_B, 15000);
    await h.enterProject(PROJ_B);
  });

  it("opens two labelled terminals with markers in project B", async function () {
    // T1 → "term-A"
    const id1 = await h.newWorkspaceTerminal();
    const m1 = "REORD_M1_" + Date.now();
    await h.typeDeck(id1, 'echo "' + m1 + '"\n');
    await h.waitForDeck(id1, m1, 15000);
    const n1 = (await h.terminalRowNames())[0];
    await h.renameTerminalRow(n1, "term-A");
    termIds["term-A"] = id1;

    // T2 → "term-B" (its default name is whatever is NOT "term-A" now)
    const id2 = await h.newWorkspaceTerminal();
    const m2 = "REORD_M2_" + Date.now();
    await h.typeDeck(id2, 'echo "' + m2 + '"\n');
    await h.waitForDeck(id2, m2, 15000);
    const t2current = (await h.terminalRowNames()).find((n) => n !== "term-A");
    await h.renameTerminalRow(t2current, "term-B");
    termIds["term-B"] = id2;

    markers["term-A"] = m1;
    markers["term-B"] = m2;
    const rows = await h.terminalRowNames();
    assert.deepStrictEqual(rows, ["term-A", "term-B"], "creation order must be [term-A, term-B]");
  });

  it("REORDERS the terminal rows by a REAL DRAG (synthetic pointer events firing dnd-kit)", async function () {
    // finding #23: the drag was dead (PointerSensor.preventActivation blocked the covering select
    // button). With that fixed, attempt the genuine gesture — drag row 0 onto row 1 with synthetic
    // `PointerEvent`s dispatched in the renderer, which fire the real dnd-kit sensor (`onDragEnd` →
    // `reorderTerminals` → `reorder` IPC → persisted `order_index`). This is the honest coverage
    // the seam never gave.
    //
    // review finding #28: the seam remains ONLY as a last-resort so the DB is still left reordered
    // for the verify half (and diagnostics), but a seam fallback means the real drag REGRESSED — so
    // this step now FAILS on `termMechanism !== "drag"` (assertion at the end) instead of silently
    // masking the regression. Either way the order MUST change; the gate is only green when the
    // REAL gesture drove it.
    const before = await h.terminalRowNames();
    const treeB = (await h.listProjectTrees()).find((t) => t.project.name === PROJ_B);
    assert(treeB && treeB.workspaces.length > 0, "project B must have its root workspace");
    const group = treeB.workspaces[0].id; // the (root) workspace both terminals were opened in

    await h.synthDragTerminalRow(0, 1);
    let after = await h.terminalRowNames();
    termMechanism = sameOrder(before, after) ? "seam" : "drag";
    if (termMechanism === "seam") {
      const ok = await h.reorderTerminalsSeam(group, [termIds["term-B"], termIds["term-A"]]);
      assert(ok, "the __nyxReorderTerminals seam must be published (VITE_NYX_E2E build)");
      await browser.pause(500);
      after = await h.terminalRowNames();
    }
    console.log("[v2.5.6 reorder] terminal reorder mechanism = " + termMechanism);
    assert(
      !sameOrder(before, after),
      "the terminal row order must change after the reorder (before " +
        JSON.stringify(before) +
        ", after " +
        JSON.stringify(after) +
        ")",
    );
    termOrder = after; // the OBSERVED post-reorder order the verify half must find restored
    // review finding #28 — HONEST drag assertion. `termMechanism === "drag"` ONLY when the
    // synthetic pointer events ALONE reordered the rows (the seam block above did NOT run): that is
    // the real dnd-kit gesture firing end-to-end. A "seam" value means the genuine drag did nothing
    // (finding #23 regressed) and only the gated callback moved the order — the seam persisted the
    // state so the run is still diagnosable, but the gate MUST fail here rather than pass on a
    // fallback that hides the regression. Placed last so the order is fully persisted first.
    assert.equal(
      termMechanism,
      "drag",
      "terminal reorder must be driven by a REAL DRAG; got '" +
        termMechanism +
        "' — the synthetic pointer events did not fire dnd-kit, so the drag gesture (#23) regressed",
    );
  });

  it("REORDERS the rail projects by a REAL DRAG (synthetic pointer events firing dnd-kit)", async function () {
    const before = await h.railProjectOrder();
    const trees = await h.listProjectTrees();
    const idA = trees.find((t) => t.project.name === PROJ_A).project.id;
    const idB = trees.find((t) => t.project.name === PROJ_B).project.id;

    // Attempt the genuine rail drag (drag A onto B) via synthetic pointer events first. The
    // `reorderProjects` callback seam remains ONLY as a last-resort so the DB is still reordered for
    // the verify half — but a seam fallback means the real drag regressed, so this step FAILS on
    // `projMechanism !== "drag"` (assertion at the end) rather than passing silently (review #28).
    await h.synthDragProject(PROJ_A, PROJ_B);
    let after = await h.railProjectOrder();
    projMechanism = sameOrder(before, after) ? "seam" : "drag";
    if (projMechanism === "seam") {
      const ok = await h.reorderProjectsSeam([idB, idA]);
      assert(ok, "the __nyxWork.reorderProjects seam must be published (VITE_NYX_E2E build)");
      await browser.pause(500);
      after = await h.railProjectOrder();
    }
    console.log("[v2.5.6 reorder] project reorder mechanism = " + projMechanism);
    assert(
      !sameOrder(before, after),
      "the rail project order must change after the reorder (before " +
        JSON.stringify(before) +
        ", after " +
        JSON.stringify(after) +
        ")",
    );
    projOrder = after;
    // review finding #28 — HONEST drag assertion (twin of the terminal one above). "drag" ONLY when
    // the synthetic pointer events alone reordered the rail; "seam" means the genuine rail drag did
    // nothing and the gate MUST fail here instead of masking the regression.
    assert.equal(
      projMechanism,
      "drag",
      "project reorder must be driven by a REAL DRAG; got '" +
        projMechanism +
        "' — the synthetic pointer events did not fire dnd-kit, so the rail drag gesture regressed",
    );
  });

  it("turns ON 'Resume agent sessions' for project B (v2.5.5 setting)", async function () {
    await h.openProjectContextMenu(PROJ_B);
    await h.clickAria("Project settings for " + PROJ_B);
    const toggle = await h.waitAria("Resume agent sessions on relaunch");
    await toggle.click();
    await browser.pause(400);
    await browser.keys("Escape");
    await browser.pause(300);
    const treeB = (await h.listProjectTrees()).find((t) => t.project.name === PROJ_B);
    assert(treeB && treeB.project.resume_agent_sessions === true, "resume toggle must be ON");
  });

  it("hands off the observed orders + terminal ids/markers to the verify half", async function () {
    // Fallback to a fresh read if a prior step left a null (defensive).
    const finalTermOrder = termOrder || (await h.terminalRowNames());
    const finalProjOrder = projOrder || (await h.railProjectOrder());
    writeHandoff({
      projectA: PROJ_A,
      projectB: PROJ_B,
      expectedProjectOrder: finalProjOrder,
      expectedTerminalOrder: finalTermOrder,
      terminalIds: termIds,
      markers: markers,
      // Diagnostic (finding #23): which mechanism actually reordered — "drag" means the synthetic
      // pointer events drove the real dnd-kit sensor (true drag coverage); "seam" means they did
      // not and the gated callback was used instead.
      terminalReorderMechanism: termMechanism,
      projectReorderMechanism: projMechanism,
    });
    assert(finalTermOrder.length === 2, "handoff must record two terminals");
    assert(finalProjOrder.length === 2, "handoff must record two projects");
  });
});
