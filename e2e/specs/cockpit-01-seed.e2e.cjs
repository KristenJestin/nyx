/* eslint-disable */
// v2.2 "travail actif" COCKPIT GATE — SEED half (session 1). Task 01KWHPV33TY0VM284TJEEBGP8K.
//
// Drives a REAL free-work feature end-to-end THROUGH THE COCKPIT against the real Electron app:
//   1. open a free-work workspace (a project's PRIMARY workspace) at a REAL git repo;
//   2. run managed commands (SERVICES) + a terminal with observable output, make a Git change;
//   3. start a REAL agent session (MCP `agent_session_event` over the sidecar loopback);
//   4. navigate INTO the cockpit (`#/cockpit/project/<id>`) and assert every region is live:
//        - the runtime NAVIGATOR tree (Terminals + Commandes), no cosmetic Agents category;
//        - the live terminal PANE (its buffer readable through the reused deck seam);
//        - the SERVICES volet, the Git CHANGES volet, the TIMELINE (with runtime events),
//          the transverse AGENTS supervision panel;
//   5. assert the full v2.2 GATE CHECKLIST (see the dedicated `it`).
// The app session then ends (wdio-electron-service closes it) — cockpit-02-verify relaunches on
// the SAME DB and asserts the workspace context resumes.
//
// ANTI-FALSE-GREEN: the before-hook FAILS HARD if the app/seam/MCP never appears, and every
// assertion has a finite timeout that THROWS on miss — a build that does not mount, a missing
// seam, or an unreachable MCP FAILS the suite rather than passing silently.

const assert = require("assert");
const h = require("./cockpit-helpers.cjs");

describe("v2.2 cockpit gate — seed (free-work journey end-to-end through the cockpit)", function () {
  let projectId;
  let workspaceId;
  let repoDir;
  let terminalId;
  let serviceInstanceId;
  const AGENT_SID = "cockpit-e2e-sid-" + Date.now();
  const TERM_MARKER = "COCKPIT_TERM_MARK_7";
  const SVC_MARKER = "COCKPIT_SVC_UP_42";

  before(async function () {
    // Anti-false-green gates: the shell seam AND the MCP loopback must both come up.
    await h.waitForShellSeam();
    await h.waitForMcp(30000);
    repoDir = h.ensureGitRepo();
  });

  it("opens a free-work workspace at a real git repo and runs a feature (services + terminal + agent + git change)", async function () {
    // --- 1) FREE-WORK workspace: a project whose root IS the git repo. `create_project`
    // auto-detects the repo under the root, so the workspace's Git summary is non-empty. ---
    const created = await h.createProject("cockpit-e2e", repoDir);
    assert(created && created.project && created.root, "createProject returns project + root");
    projectId = created.project.id;
    workspaceId = created.root.id;
    assert(projectId && workspaceId, "project + primary(root) workspace ids present");

    // --- 2a) SERVICES: a long-lived managed command (a dev-server-like service). ---
    await h.createCommand(projectId, "svc", 'echo "' + SVC_MARKER + '"; sleep 120');
    const svc = (await h.listInstances(workspaceId)).find((i) => i.name === "svc");
    assert(svc, "the svc command materialized an instance in the workspace");
    serviceInstanceId = svc.id;
    const st = await h.commandStart(serviceInstanceId);
    assert(st === "running", "the service starts running, got: " + st);
    await h.waitForCommandState(workspaceId, serviceInstanceId, "running", 10000);
    await h.waitForCommandOutput(serviceInstanceId, SVC_MARKER, 15000);

    // --- 2b) A TERMINAL bound to the free-work workspace, with observable output. ---
    terminalId = await h.openTerminalAt(repoDir);
    const bind = await h.autoAttach(terminalId);
    assert.strictEqual(bind.changed, true, "the terminal auto-attaches to the free-work workspace");
    assert.strictEqual(bind.workspace_id, workspaceId, "…bound to the primary workspace");
    await h.waitForBinding(terminalId, workspaceId);
    await h.typeInto(terminalId, 'echo "' + TERM_MARKER + '"\n');
    await h.waitForBuffer(terminalId, TERM_MARKER, 15000);

    // --- 3) A REAL agent session on that terminal, via the MCP `agent_session_event` tool. ---
    const session = await h.startAgentSession(terminalId, repoDir, AGENT_SID);
    assert(session, "agent_session_event returned a session payload");
    assert.strictEqual(session.terminal_id, terminalId, "the session is hosted on the terminal");
    assert.strictEqual(
      session.workspace_id,
      workspaceId,
      "the session anchors to the free-work workspace (derived from the terminal binding)",
    );
    assert.strictEqual(session.agent_kind, "claude_code", "the session is a Claude Code session");

    // Hand off for the verify (relaunch) spec.
    h.writeHandoff({
      projectId,
      workspaceId,
      repoDir,
      terminalId,
      serviceInstanceId,
      externalSessionId: AGENT_SID,
      termMarker: TERM_MARKER,
      changedFile: h.CHANGED_FILE,
    });
  });

  it("navigates INTO the cockpit and shows the LIVE runtime, panes, services, Git changes, timeline, and agents", async function () {
    // Navigate to FREE-WORK cockpit on the project's primary workspace (top-level hash view).
    await h.openFreeWorkCockpit(projectId);
    await h.waitForCockpit(20000);

    const text = await h.cockpitText();

    // --- Cockpit is CENTRED ON THE WORKSPACE: header shows the workspace name + on-disk path. ---
    assert(
      text.indexOf(repoDir) !== -1,
      "the cockpit header shows the workspace on-disk path (workspace-centric)",
    );

    // --- Runtime NAVIGATOR tree: Terminals + Commandes sections, both non-empty (live). ---
    const termCount = await h.sectionCount("Terminals");
    const cmdCount = await h.sectionCount("Commandes");
    assert(termCount != null && termCount >= 1, "navigator lists >=1 terminal (got " + termCount + ")");
    assert(cmdCount != null && cmdCount >= 1, "navigator lists >=1 managed command (got " + cmdCount + ")");

    // --- LIVE terminal PANE inside the cockpit: the pane host reuses the real <TerminalDeck>,
    // which re-spawns a live PTY at the terminal's cwd (it projects an empty scrollback, so the
    // SEED marker is not carried — the pane is a fresh live session, not a snapshot). Prove it
    // is a LIVE, TYPABLE pane: type a FRESH marker through the deck input seam and assert the
    // PTY echoes it back into the cockpit pane's buffer. ---
    await browser.waitUntil(
      async () => (await h.readDeckBuffer(terminalId)) != null,
      { timeout: 15000, timeoutMsg: "the cockpit never mounted a live terminal pane for the terminal" },
    );
    const livePaneMarker = "COCKPIT_LIVE_" + Date.now();
    const typed = await h.typeIntoDeck(terminalId, 'echo "' + livePaneMarker + '"\n');
    assert(typed, "the cockpit's live pane exposes an input seam (a real terminal pane is mounted)");
    await h.waitForDeckBuffer(terminalId, livePaneMarker, 15000);

    // --- SERVICES volet: the managed command surfaces as a running service. ---
    const svcCount = await h.sectionCount("Services");
    assert(svcCount != null && svcCount >= 1, "Services volet lists the managed command (got " + svcCount + ")");
    // The service is RUNNING → a live-state dot rendered as data-state="running" is present.
    assert(
      await h.hasElement('[data-state="running"]'),
      "a running service run-state dot is rendered (live aggregate, not a snapshot)",
    );

    // --- Git CHANGES volet: the untracked file shows in the summary (dirty work tree). ---
    const changesCount = await h.sectionCount("Changements");
    assert(
      changesCount != null && changesCount >= 1,
      "Changements volet lists >=1 changed file (got " + changesCount + ")",
    );
    assert(
      text.indexOf(h.CHANGED_FILE) !== -1,
      "the changed file " + h.CHANGED_FILE + " is listed in the Git changes volet",
    );

    // --- TIMELINE volet: bounded fil of runtime domain_events (has the agent-session start,
    // command start, etc.) — non-empty, proving runtime events reached the timeline. ---
    const timelineCount = await h.sectionCount("Timeline");
    assert(
      timelineCount != null && timelineCount >= 1,
      "Timeline volet lists >=1 runtime event (got " + timelineCount + ")",
    );

    // --- AGENTS transverse panel: the real agent session is listed with its provider label. ---
    const agentsCount = await h.sectionCount("Agents");
    assert(agentsCount != null && agentsCount >= 1, "Agents panel lists the session (got " + agentsCount + ")");
    assert(
      text.indexOf("Claude Code") !== -1,
      "the Agents panel shows the Claude Code provider label for the session",
    );
    assert(
      text.indexOf(AGENT_SID) !== -1,
      "the Agents panel shows the session's external id",
    );
  });

  it("passes the v2.2 GATE CHECKLIST (workspace-centric, live-no-polling, no Execution, agents w/o 3rd category, no Attention Inbox, reserved review/Spec seams)", async function () {
    // We are in the cockpit view (previous test navigated here). Re-assert the mount.
    await h.waitForCockpit(20000);
    const text = await h.cockpitText();

    // [1] Cockpit CENTRED ON WORKSPACE — the workspace path anchors the header.
    assert(text.indexOf(repoDir) !== -1, "CHECKLIST: cockpit is centred on the workspace");

    // [2] Runtime aggregated LIVE (no polling) — a live run-state dot is rendered off the
    // aggregate the backend PUSHED (the `useWorkspaceRuntime` hook re-pulls on `*://changed`,
    // never on a timer; source of truth cited in the report). The running service dot proves a
    // live state reached the DOM.
    assert(
      await h.hasElement('[data-state="running"]'),
      "CHECKLIST: runtime is live (a pushed run-state reached the cockpit) — no polling",
    );

    // [3] NO Execution entity — the cockpit exposes no "Execution"/"Exécution" surface; the
    // runtime is aggregated from the read model, not an Execution record.
    assert(
      !/\bexecution\b/i.test(text) && text.indexOf("Exécution") === -1,
      "CHECKLIST: no Execution entity surfaced in the cockpit",
    );

    // [4] Agents visible WITHOUT a 3rd category (Q-013) — the transverse Agents PANEL exists
    // (in the aside), but the navigator TREE has EXACTLY Terminals + Commandes as its sections;
    // there is NO third "Agents" section header in the runtime tree.
    assert(text.indexOf("Claude Code") !== -1, "CHECKLIST: the agent session is visible (Agents panel)");
    const navSectionTitles = await browser.execute(function () {
      // The navigator is the left <nav>; its section micro-labels are the count-badged rows.
      const nav = document.querySelector("nav");
      if (!nav) return [];
      const titles = [];
      nav.querySelectorAll("span").forEach(function (el) {
        const t = (el.textContent || "").trim();
        if (t === "Terminals" || t === "Commandes" || t === "Agents") titles.push(t);
      });
      return titles;
    });
    assert(
      navSectionTitles.indexOf("Terminals") !== -1 && navSectionTitles.indexOf("Commandes") !== -1,
      "CHECKLIST: the navigator has the Terminals + Commandes sections",
    );
    assert(
      navSectionTitles.indexOf("Agents") === -1,
      "CHECKLIST: the navigator TREE has NO third 'Agents' category (Q-013) — got " +
        JSON.stringify(navSectionTitles),
    );

    // [5] Attention Inbox ABSENT (deferred Q-007) — no "Attention"/"Inbox" surface anywhere.
    assert(
      !/attention/i.test(text) && !/inbox/i.test(text),
      "CHECKLIST: no Attention Inbox in the cockpit (deferred Q-007)",
    );

    // [6] Review / Spec volets RESERVED (seams visible but empty) — the header renders the
    // reserved Spec + Tâche seams as placeholders, and no v2.3 review / v2.4 Spec content.
    assert(
      await h.hasElement('[data-reserved-seam="Spec"]'),
      "CHECKLIST: the reserved Spec seam is present (visible, empty — v2.4)",
    );
    assert(
      await h.hasElement('[data-reserved-seam="Tâche"]'),
      "CHECKLIST: the reserved task seam is present (visible, empty — v2.4)",
    );
    // No diff/review surface leaked in from v2.3: the Changements volet is a SUMMARY (branch +
    // porcelain status code + path), NOT a line-level diff. The panel is present (its count was
    // asserted above) and no diff hunk markers ("@@", "+++/---" headers) appear in the cockpit.
    assert(
      (await h.sectionCount("Changements")) != null,
      "CHECKLIST: the Changements volet is present (a summary; review/diff is v2.3, out of scope)",
    );
    assert(
      text.indexOf("@@") === -1,
      "CHECKLIST: no line-level diff (v2.3) leaked into the cockpit's changes summary",
    );
  });
});
