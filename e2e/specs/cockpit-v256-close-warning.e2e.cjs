/* eslint-disable */
// v2.5.6 cockpit gate — CLOSE-WARNING CHROME with a LIVE agent session (task
// 01KXBNX51EPQ69MVKYG4W960HE, PRD #1). This is the "chrome" leg of the gate: the frameless
// window's min/max/close + drag belong to the WM (OUT OF SCOPE, not tested); the chrome nyx
// OWNS is the CLOSE-WARNING guard that intercepts an OS close when a live agent session would be
// silently dropped.
//
// It drives the REAL guard chain end-to-end:
//   1. seed a project (resume OFF by default) rooted at a real git repo + a workspace terminal;
//   2. start a REAL Claude agent session on that terminal via the MCP `agent_session_event` tool
//      over the sidecar loopback — the EXACT transport a Claude Code hook uses (zero test plumbing);
//   3. fire the OS "close-requested" push the frameless window's `close` withhold emits;
//   4. assert the guard WITHHELD the close and opened the confirm dialog listing the session;
//   5. "Keep open" leaves the window intact (the guard did NOT drop the live session).
//
// A live agent session on a NON-resuming project is exactly what `agent_close_warnings` warns on,
// so the dialog is deterministic. We validate the non-destructive path ("Keep open") in full and
// assert the "Close anyway" affordance is present; we do NOT actually tear the app down mid-spec.

const assert = require("assert");
const h = require("./cockpit-v256-helpers.cjs");

describe("v2.5.6 cockpit — close-warning with a live agent session", function () {
  const PROJECT = "CloseWarnProj";
  let repoDir;
  let termId;

  before(async function () {
    // A real git repo dir so the workspace root resolves like a real project root.
    repoDir = h.ensureGitRepo();
    await h.waitForWorkShell();
    // The sidecar MCP loopback must be up before we POST the session event.
    await h.waitForMcp(30000);
    // Resume is OFF by default → a live session here is a session a close would DROP → it warns.
    await h.seedProject(PROJECT, repoDir, "main");
    await h.waitAria("Project " + PROJECT, 15000);
    await h.enterProject(PROJECT);
    // A workspace terminal (attached to the root workspace), then a REAL agent session on it.
    termId = await h.newWorkspaceTerminal();
    assert(termId, "a workspace terminal is required to host the agent session");
    const sessionId = "e2e-close-warn-" + Date.now();
    const res = await h.startAgentSession(termId, repoDir, sessionId);
    assert(res, "the MCP agent_session_event tool must return a session payload");
    // Give the DB write + read model a beat to settle before the guard reads it.
    await browser.pause(600);
  });

  it("withholds an OS close and opens the confirm dialog listing the live session", async function () {
    await h.requestOsClose();
    // The confirm dialog surfaces: title + the resume-off rationale + both action buttons.
    await h.waitForText("doesn't resume agent sessions on relaunch", 10000);
    assert(
      (await h.bodyIncludes("still active")) === true,
      "the dialog title must announce the still-active session(s)",
    );
    assert(
      await browser.execute(function () {
        return Array.from(document.querySelectorAll("button")).some(function (b) {
          return (b.textContent || "").trim() === "Close anyway";
        });
      }),
      "the dialog must offer 'Close anyway' (the confirm-to-drop path)",
    );
    // The window is STILL alive at this point — the guard withheld the real close.
    assert(await h.windowStillAlive(), "the guard must withhold the close until the user decides");
  });

  it("'Keep open' leaves the window intact (the live session is not dropped)", async function () {
    await h.clickButtonByText("Keep open");
    await browser.pause(500);
    // The dialog is dismissed and the app is still running (no session dropped, no close).
    assert(
      !(await h.bodyIncludes("doesn't resume agent sessions on relaunch")),
      "the close-warning dialog must dismiss on 'Keep open'",
    );
    assert(await h.windowStillAlive(), "'Keep open' must NOT close the window");
    // Re-firing the guard still warns (the session is still live) — proving the guard is durable.
    await h.requestOsClose();
    await h.waitForText("doesn't resume agent sessions on relaunch", 10000);
    await h.clickButtonByText("Keep open");
    await browser.pause(300);
    assert(await h.windowStillAlive(), "the window must survive a second withheld close");
  });
});
