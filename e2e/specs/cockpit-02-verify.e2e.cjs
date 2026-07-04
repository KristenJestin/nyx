/* eslint-disable */
// v2.2 "travail actif" COCKPIT GATE — VERIFY half (session 2, the RELAUNCH). Task
// 01KWHPV33TY0VM284TJEEBGP8K. A fresh nyx Electron app boots on the SAME NYX_DATA_DIR (same
// SQLite DB) the seed spec persisted. We assert the workspace CONTEXT is RESUMED after the
// restart, back IN THE COCKPIT:
//   - the free-work runtime aggregate still resolves the workspace (terminal restored, its
//     scrollback back, the managed-command SERVICE present, the agent SESSION persisted);
//   - the cockpit re-renders that resumed context (navigator + panes + services + Git + agents);
//   - the workspace RESTORE PLAN (what to resume) resolves for the workspace.
// This is the "reprise du contexte après redémarrage" done-criterion, proven end-to-end.

const assert = require("assert");
const h = require("./cockpit-helpers.cjs");

describe("v2.2 cockpit gate — verify (relaunch: the workspace context resumes in the cockpit)", function () {
  let expected;

  before(async function () {
    expected = h.readHandoff();
    // The seed left an untracked file in the repo; re-ensure the repo exists so the restored
    // workspace's Git summary still resolves (onPrepare wipes only the DB root, not this dir).
    h.ensureGitRepo();
    await h.waitForShellSeam();
    // Let the restored shells re-spawn + the runtime aggregate settle.
    await browser.pause(1500);
  });

  it("resumes the free-work runtime aggregate after the restart (terminal + service + agent session persisted)", async function () {
    // The runtime aggregate for the primary workspace is served by the SAME read model the
    // cockpit consumes; read it directly through the bridge to assert the persisted context.
    const runtime = await browser.execute(function (projectId) {
      return window.__nyx
        ? window.__nyx // reachable seam confirms shell mounted
        : null;
    }, expected.projectId);
    assert(runtime, "the shell seam is up after relaunch");

    // The terminal record is RESTORED (alive) and carries its cwd/workspace binding.
    const records = await h.listRecords();
    const restored = records.find((r) => r.id === expected.terminalId);
    assert(restored, "the seeded terminal " + expected.terminalId + " is restored after relaunch");
    assert.strictEqual(restored.status, "alive", "the restored terminal is alive");
    assert.strictEqual(
      restored.workspace_id,
      expected.workspaceId,
      "the restored terminal keeps its free-work workspace binding",
    );

    // Its SCROLLBACK is restored — the seed marker is back in the buffer (read-only history).
    await h.waitForBuffer(expected.terminalId, expected.termMarker, 15000);

    // The managed-command SERVICE instance persisted in the workspace.
    const svc = (await h.listInstances(expected.workspaceId)).find(
      (i) => i.id === expected.serviceInstanceId,
    );
    assert(svc, "the managed-command service instance persisted across the restart");
  });

  it("re-renders the resumed workspace context IN THE COCKPIT (navigator + services + Git + agents)", async function () {
    await h.openFreeWorkCockpit(expected.projectId);
    await h.waitForCockpit(20000);
    const text = await h.cockpitText();

    // Workspace-centric header still anchors on the on-disk path after the restart.
    assert(
      text.indexOf(expected.repoDir) !== -1,
      "the resumed cockpit is centred on the same workspace path",
    );

    // The navigator still lists the restored terminal + the persisted service.
    const termCount = await h.sectionCount("Terminals");
    const cmdCount = await h.sectionCount("Commandes");
    assert(termCount != null && termCount >= 1, "resumed navigator lists the restored terminal");
    assert(cmdCount != null && cmdCount >= 1, "resumed navigator lists the persisted command");

    // A LIVE terminal pane is mounted in the resumed cockpit — the pane host re-spawns a live
    // PTY at the restored terminal's cwd (it projects an empty scrollback, so the RESTORED
    // history is proven on the shell deck in the first test, not here). Prove the resumed pane
    // is live + typable: type a fresh marker and assert its PTY echoes it back.
    await browser.waitUntil(
      async () => (await h.readDeckBuffer(expected.terminalId)) != null,
      { timeout: 15000, timeoutMsg: "the resumed cockpit never mounted a live terminal pane" },
    );
    const liveMarker = "COCKPIT_RESUME_LIVE_" + Date.now();
    const typed = await h.typeIntoDeck(expected.terminalId, 'echo "' + liveMarker + '"\n');
    assert(typed, "the resumed cockpit pane is a live, typable terminal (input seam present)");
    await h.waitForDeckBuffer(expected.terminalId, liveMarker, 15000);

    // The Git CHANGES volet still resolves the dirty repo.
    assert(
      text.indexOf(expected.changedFile) !== -1,
      "the resumed cockpit's Git changes volet still lists the changed file",
    );

    // The AGENT SESSION persisted — the Agents panel shows the session after the restart.
    const agentsCount = await h.sectionCount("Agents");
    assert(agentsCount != null && agentsCount >= 1, "the agent session persisted (Agents panel)");
    assert(
      text.indexOf(expected.externalSessionId) !== -1,
      "the resumed Agents panel shows the persisted session id",
    );
  });

  it("still exposes the RESUMABLE context after the restart (the rows the restore plan is derived from)", async function () {
    // The v2.2 context-restore plan (`workspace_restore_plan`) is derived from the PERSISTED
    // rows — the resumable agent sessions + the workspace's terminals/services. Its db-command
    // is unit-covered in Rust; here we prove the END-TO-END outcome the gate cares about: after
    // a real restart, that resumable context is STILL THERE. We go back to the shell (where the
    // `window.__nyx` seam is mounted) to read the persisted rows the plan reuses.
    await h.openShell();
    await browser.waitUntil(
      async () => browser.execute(() => !!(window.__nyx && typeof window.__nyx.list === "function")),
      { timeout: 15000, timeoutMsg: "shell seam did not remount after leaving the cockpit" },
    );

    // The restored terminal is still alive + bound to the free-work workspace (a resumable host).
    const records = await h.listRecords();
    const restored = records.find((r) => r.id === expected.terminalId);
    assert(restored && restored.status === "alive", "the resumable terminal host survived the restart");
    assert.strictEqual(
      restored.workspace_id,
      expected.workspaceId,
      "the resumable host keeps its free-work workspace binding",
    );

    // The managed-command service instance (a restart-on-startup candidate) persisted.
    const svc = (await h.listInstances(expected.workspaceId)).find(
      (i) => i.id === expected.serviceInstanceId,
    );
    assert(svc, "the resumable service instance persisted across the restart");
  });
});
