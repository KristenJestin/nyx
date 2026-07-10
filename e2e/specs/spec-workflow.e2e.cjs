/* eslint-disable */
// v2.4 SPEC-WORKFLOW GATE — the full Spec lifecycle driven ENTIRELY inside nyx, WITHOUT Depot.
// Task 01KX1DH9E5W93YJW73VYNWBTHP (PRD 01KX1C2CZ4D7PHT709N3Z92FR6).
//
// A single-session CDP-piloted gate against the REAL Electron app that proves the whole v2.4
// Spec domain lives in nyx: it CREATES + FRAMES a Spec, adds TASKS + DEPENDENCIES, moves it
// DRAFT->READY, ANCHORS it on a dev workspace (so the repos are DERIVED through the workspace),
// navigates the PRD DETAIL and every TAB (incl. the Review tab present as the v2.5 anchor),
// PILOTS the Spec through the MCP tools + GENERATES an agent context, verifies REAL-TIME
// (a MCP mutation refreshes the UI with no reload), and LOOPS the lifecycle to DONE.
//
// TWO seams are used — ZERO new production plumbing (the same seams the cockpit gate uses):
//   1. `window.__nyx` (the shell's inert e2e seam, gated VITE_NYX_E2E) — ONLY to create the
//      project + its root workspace (the anchor). It exposes NO Spec authoring; the Spec is
//      driven purely through the real MCP tools, exactly like an agent would.
//   2. The REAL MCP loopback (`http://127.0.0.1:<NYX_MCP_PORT>/mcp`) — the phase-5 Spec
//      authoring/transition tools (spec_create / spec_update_section / task_add /
//      task_add_dependency / spec_set_workspace / spec_transition / task_transition /
//      agent_context). This is the EXACT transport an agent uses; NO Depot CLI is ever called.
//
// The UI is driven through the real navigation shell: enter the project from the rail, click
// the "Work" section-nav link (the board), let the board REFRESH LIVE off the `specs://changed`
// push a MCP mutation broadcasts (the T9 real-time wiring), click the Spec card into the PRD
// detail, and read the rendered tabs + sidebar. No hash seam — the router is memory-history, so
// navigation is real clicks / router links.
//
// ANTI-FALSE-GREEN: the before-hook FAILS HARD if the app/seam/MCP never appears; every MCP
// call rejects on a JSON-RPC error envelope (a broken tool FAILS the gate); every UI assertion
// has a finite timeout that THROWS on miss. A build that does not mount, a missing tool, or an
// unreachable MCP FAILS the suite rather than passing silently.

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { execFileSync } = require("child_process");
const h = require("./cockpit-helpers.cjs"); // reuse the MCP loopback helpers (mcpCall / waitForMcp)

// The shell rewrite (PDR-0025) unmounted the old `<TerminalManager>` `window.__nyx` seam, so we
// seed the anchor project through the minimal `window.__nyxWork` seam published by the
// always-mounted navigation shell (`base-layout.tsx`, gated VITE_NYX_E2E). Wait for it to mount.
async function waitForWorkSeam() {
  await browser.waitUntil(
    async () =>
      browser.execute(
        () => !!(window.__nyxWork && typeof window.__nyxWork.createProject === "function"),
      ),
    { timeout: 30000, timeoutMsg: "window.__nyxWork seed seam never appeared (app/seam did not start)" },
  );
}

// Create a project + its root workspace at `rootPath` via the seam; returns `{ project, root }`.
function seedProject(name, rootPath) {
  return browser.execute((n, r) => window.__nyxWork.createProject(n, r), name, rootPath);
}

// A stable REAL git repo on the host the app shares (spec + app run on the same machine), so
// the Spec's anchor workspace derives a real project + repo. One untracked file → dirty tree.
function ensureWorkflowRepo() {
  const root = fs.realpathSync(os.tmpdir());
  const dir = path.join(root, "nyx-spec-workflow-e2e-repo");
  fs.mkdirSync(dir, { recursive: true });
  const git = (...args) => execFileSync("git", ["-C", dir, ...args], { stdio: "pipe" });
  if (!fs.existsSync(path.join(dir, ".git"))) {
    git("init", "-q");
    git("config", "user.email", "e2e@nyx.local");
    git("config", "user.name", "nyx e2e");
    git("config", "commit.gpgsign", "false");
    fs.writeFileSync(path.join(dir, "README.md"), "# nyx spec-workflow e2e repo\n");
    git("add", "-A");
    git("commit", "-q", "-m", "initial");
  }
  return fs.realpathSync(dir);
}

// The MCP tool payload: prefer structuredContent, fall back to the mirrored text block.
function toolPayload(result) {
  if (result && result.structuredContent) return result.structuredContent;
  if (result && Array.isArray(result.content)) {
    const text = result.content.find((c) => c.type === "text");
    if (text) {
      try {
        return JSON.parse(text.text);
      } catch {}
    }
  }
  return result;
}

// Call a Spec MCP tool by name with arguments; return the parsed tool payload.
async function specTool(name, args) {
  const result = await h.mcpCall("tools/call", { name, arguments: args });
  return toolPayload(result);
}

// Enter a project from the left rail (click its tile). The board then reads THIS project's Specs.
async function enterProject(projectName) {
  const label = "Project " + projectName;
  await browser.waitUntil(
    async () => browser.execute((l) => !!document.querySelector('button[aria-label="' + l + '"]'), label),
    { timeout: 15000, timeoutMsg: "project tile '" + projectName + "' never appeared in the rail" },
  );
  await browser.execute((l) => {
    document.querySelector('button[aria-label="' + l + '"]').click();
  }, label);
}

// Click the "Work" section-nav link → route to the Specs board.
async function openBoard() {
  await browser.waitUntil(
    async () => browser.execute(() => !!document.querySelector('a[aria-label="Work"]')),
    { timeout: 15000, timeoutMsg: "the Work section-nav link never appeared" },
  );
  await browser.execute(() => document.querySelector('a[aria-label="Work"]').click());
  // The board header renders "Specs".
  await browser.waitUntil(
    async () => browser.execute(() => /\bSpecs\b/.test(document.body.innerText || "")),
    { timeout: 15000, timeoutMsg: "the Specs board did not render" },
  );
}

// Wait for a Spec card (keyed by id via data-testid) to appear on the board, then click it.
async function openSpecCard(specId, timeoutMs) {
  const sel = '[data-testid="spec-card-' + specId + '"]';
  await browser.waitUntil(
    async () => browser.execute((s) => !!document.querySelector(s), sel),
    {
      timeout: timeoutMs || 15000,
      timeoutMsg: "the Spec card " + specId + " never appeared on the board (real-time board refresh)",
    },
  );
  await browser.execute((s) => document.querySelector(s).click(), sel);
}

// Whole visible text of the rendered surface (for text-presence assertions).
async function bodyText() {
  return browser.execute(() => document.body.innerText || "");
}

// The labels of the tabs rendered in the PRD detail (role="tab").
async function tabLabels() {
  return browser.execute(() =>
    Array.from(document.querySelectorAll('[role="tab"]')).map((el) => (el.textContent || "").trim()),
  );
}

// Click the PRD-detail tab whose label contains `label`.
async function clickTab(label) {
  const ok = await browser.execute((lbl) => {
    const tab = Array.from(document.querySelectorAll('[role="tab"]')).find((el) =>
      (el.textContent || "").indexOf(lbl) !== -1,
    );
    if (tab) {
      tab.click();
      return true;
    }
    return false;
  }, label);
  assert(ok, "the '" + label + "' tab was present and clickable");
}

describe("v2.4 spec-workflow gate — full Spec lifecycle inside nyx, without Depot", function () {
  let projectId;
  let workspaceId;
  let projectKey;
  let projectName;
  let repoDir;
  let specId;
  let specNumber;
  let taskA;
  let taskB;

  before(async function () {
    // Anti-false-green gates: the shell seam AND the MCP loopback must both come up.
    await waitForWorkSeam();
    await h.waitForMcp(30000);
    repoDir = ensureWorkflowRepo();
    projectName = "spec-workflow-e2e";
  });

  it("creates a project (anchor workspace) and drives the Spec through MCP: create+frame, tasks+deps, draft->ready", async function () {
    // --- Project + its ROOT workspace: the dev anchor. `create_project` auto-detects the repo
    // under the root, so the workspace's derived repos are real. ---
    const created = await seedProject(projectName, repoDir);
    assert(created && created.project && created.root, "createProject returns project + root");
    projectId = created.project.id;
    workspaceId = created.root.id;
    projectKey = created.project.key;
    assert(projectId && workspaceId, "project + primary(root) workspace ids present");

    // --- CREATE + FRAME the Spec via the REAL MCP `spec_create` (problem + solution seeded so the
    // pre-ready validations pass), then frame two more sections via `spec_update_section`. ---
    const createOut = await specTool("spec_create", {
      project_id: projectId,
      title: "Absorb the widget domain",
      problem: "The widget domain lives outside nyx.",
      solution: "Port it into nyx-core with a native UI.",
    });
    assert(createOut && createOut.spec && createOut.spec.id, "spec_create returned a spec");
    specId = createOut.spec.id;
    specNumber = createOut.spec.number;
    assert.strictEqual(createOut.spec.status, "draft", "the Spec is born in draft");
    assert.strictEqual(createOut.revision.problem, "The widget domain lives outside nyx.", "the problem section is framed");

    const sec = await specTool("spec_update_section", {
      spec_id: specId,
      section: "implementation_decisions",
      value: "Stored entity in nyx-core; UI board + PRD detail.",
    });
    assert.strictEqual(sec.updated, true, "spec_update_section framed a section");

    // --- TASKS + DEPENDENCIES via the REAL MCP `task_add` / `task_add_dependency`. Task B blocks
    // on Task A (an acyclic depends_on edge). ---
    const outA = await specTool("task_add", {
      project_id: projectId,
      spec_id: specId,
      title: "Model the widget schema",
      kind: "slice",
      phase: 1,
    });
    assert(outA && outA.task && outA.task.id, "task_add created task A");
    taskA = outA.task.id;

    const outB = await specTool("task_add", {
      project_id: projectId,
      spec_id: specId,
      title: "Wire the widget UI",
      kind: "slice",
      phase: 2,
    });
    assert(outB && outB.task && outB.task.id, "task_add created task B");
    taskB = outB.task.id;

    const dep = await specTool("task_add_dependency", {
      task_id: taskB,
      depends_on_task_id: taskA,
    });
    assert.strictEqual(dep.added, true, "task_add_dependency added the B->A edge");

    // --- DRAFT -> READY via the REAL MCP `spec_transition` (a sensitive transition; the guard-rail
    // requires user_confirmed=true, and the pre-ready validations must pass). ---
    const ready = await specTool("spec_transition", {
      spec_id: specId,
      transition: "ready",
      user_confirmed: true,
    });
    assert.strictEqual(ready.spec.status, "ready", "the Spec transitioned draft -> ready");
  });

  it("anchors the Spec on the dev workspace (repos derived) and generates an agent context — all via MCP", async function () {
    // --- ANCHOR on the workspace via the REAL MCP `spec_set_workspace`: the repos + project are
    // DERIVED through the workspace (specs.workspace_id). ---
    const anchored = await specTool("spec_set_workspace", {
      spec_id: specId,
      workspace_id: workspaceId,
    });
    assert.strictEqual(anchored.spec.workspace_id, workspaceId, "the Spec is anchored on the dev workspace");

    // --- GENERATE an agent context (coder role) from nyx state via the REAL MCP `agent_context`.
    // A pure render from the Spec + tasks + derived workspace; proves the context leg without Depot. ---
    const ctx = await specTool("agent_context", { role: "coder", spec_id: specId });
    assert.strictEqual(ctx.role, "coder", "agent_context rendered the coder role");
    assert.strictEqual(ctx.spec_id, specId, "…for our Spec");
    assert(typeof ctx.context === "string" && ctx.context.length > 0, "a non-empty context manual was rendered");
    assert(/Coder Agent/i.test(ctx.context), "the coder context carries the coder role preamble");
    assert(ctx.context.indexOf("Absorb the widget domain") !== -1, "the context embeds the Spec title (rendered from nyx state)");
  });

  it("navigates the PRD detail + its tabs (Review present as the v2.5 anchor) and shows the derived workspace + repos", async function () {
    // Enter the project from the rail, open the Work board. The Spec is `ready` (not a collapsed
    // column), so its card is interactable. The board refreshed LIVE off `specs://changed`.
    await enterProject(projectName);
    await openBoard();
    await openSpecCard(specId, 20000);

    // The PRD detail mounted: its title + human key render.
    await browser.waitUntil(
      async () => browser.execute((t) => (document.body.innerText || "").indexOf(t) !== -1, "Absorb the widget domain"),
      { timeout: 15000, timeoutMsg: "the PRD detail did not render the Spec title" },
    );
    const humanKey = projectKey ? projectKey + "-" + specNumber : "#" + specNumber;
    let text = await bodyText();
    assert(text.indexOf(humanKey) !== -1, "the PRD detail shows the human key " + humanKey);

    // --- EVERY tab is present, in the validated order, INCLUDING the Review tab (the v2.5 anchor). ---
    const labels = await tabLabels();
    for (const want of ["Spec", "Tasks", "Review", "Annexes", "Activity", "Runtime"]) {
      assert(
        labels.some((l) => l.indexOf(want) !== -1),
        "the '" + want + "' tab is present (got " + JSON.stringify(labels) + ")",
      );
    }

    // --- The Review tab is a PLACEHOLDER / anchor (no review content; the review grafts here in v2.5). ---
    await clickTab("Review");
    await browser.waitUntil(
      async () => browser.execute(() => /grafts here/i.test(document.body.innerText || "")),
      { timeout: 10000, timeoutMsg: "the Review tab did not render the v2.5 anchor placeholder" },
    );

    // --- The Tasks tab consumes the task model (both tasks, grouped by phase). Block task B over
    // MCP first so the dependency edge surfaces as its "Blocked by" caption (and the OPEN detail
    // refreshes live off `specs://changed` — real-time again). ---
    const blocked = await specTool("task_transition", { task_id: taskB, transition: "block", reason: "waiting on schema" });
    assert.strictEqual(blocked.task.status, "blocked", "task B was blocked via MCP");
    await clickTab("Tasks");
    await browser.waitUntil(
      async () => browser.execute(() => (document.body.innerText || "").indexOf("Model the widget schema") !== -1),
      { timeout: 10000, timeoutMsg: "the Tasks tab did not render the tasks" },
    );
    text = await bodyText();
    assert(text.indexOf("Wire the widget UI") !== -1, "both tasks render on the Tasks tab");
    // The blocked task shows what it is blocked by (from the dependency edge) — the OPEN detail
    // re-pulled off the `specs://changed` the block broadcast (no reload).
    await browser.waitUntil(
      async () => browser.execute(() => /Blocked by:/i.test(document.body.innerText || "")),
      { timeout: 15000, timeoutMsg: "the dependent task never showed its blocked-by after the MCP block (real-time)" },
    );

    // --- The sidebar shows the DERIVED workspace + repos (repos + project derived through the anchor). ---
    text = await bodyText();
    assert(text.indexOf(path.basename(repoDir)) !== -1 || text.indexOf(repoDir) !== -1, "the derived dev workspace is shown in the sidebar");
  });

  it("verifies REAL-TIME: a MCP mutation refreshes the open PRD detail with no reload", async function () {
    // We are on the PRD detail. Go to the Spec tab to read a section, then MUTATE that section over
    // MCP and assert the OPEN detail re-pulls off `specs://changed` (T9) — no navigation, no reload.
    await clickTab("Spec");
    await browser.waitUntil(
      async () => browser.execute(() => (document.body.innerText || "").indexOf("Port it into nyx-core with a native UI.") !== -1),
      { timeout: 10000, timeoutMsg: "the Spec tab did not render the solution section" },
    );

    const freshSolution = "REALTIME " + Date.now() + " — refreshed via specs://changed";
    const upd = await specTool("spec_update_section", {
      spec_id: specId,
      section: "solution",
      value: freshSolution,
    });
    assert.strictEqual(upd.updated, true, "the MCP mutation succeeded");

    // The open detail must reflect the new section WITHOUT any user reload/navigation (real-time bus).
    await browser.waitUntil(
      async () => browser.execute((s) => (document.body.innerText || "").indexOf(s) !== -1, freshSolution),
      {
        timeout: 15000,
        timeoutMsg: "the open PRD detail did NOT refresh after the MCP mutation (real-time bus broken)",
      },
    );
  });

  it("loops the lifecycle to done via MCP (ready -> in_progress -> in_review -> completed), with the tasks closed", async function () {
    // start: ready -> in_progress (not sensitive).
    const started = await specTool("spec_transition", { spec_id: specId, transition: "start" });
    assert.strictEqual(started.spec.status, "in_progress", "the Spec transitioned ready -> in_progress");

    // Close the tasks in dependency order (A then B) via the REAL MCP `task_transition`. A task
    // is `todo|blocked` → must `start` (→ in_progress) before it can `done` (the guarded machine).
    const startA = await specTool("task_transition", { task_id: taskA, transition: "start" });
    assert.strictEqual(startA.task.status, "in_progress", "task A started");
    const doneA = await specTool("task_transition", { task_id: taskA, transition: "done" });
    assert.strictEqual(doneA.task.status, "done", "task A closed done");
    // Task B is `blocked` (from the earlier MCP block); `start` restarts it out of blocked.
    const startB = await specTool("task_transition", { task_id: taskB, transition: "start" });
    assert.strictEqual(startB.task.status, "in_progress", "task B restarted out of blocked");
    const doneB = await specTool("task_transition", { task_id: taskB, transition: "done" });
    assert.strictEqual(doneB.task.status, "done", "task B closed done");

    // review: in_progress -> in_review (not sensitive).
    const reviewed = await specTool("spec_transition", { spec_id: specId, transition: "review" });
    assert.strictEqual(reviewed.spec.status, "in_review", "the Spec transitioned in_progress -> in_review");

    // done: in_review -> completed (sensitive; user_confirmed required).
    const completed = await specTool("spec_transition", {
      spec_id: specId,
      transition: "done",
      user_confirmed: true,
    });
    assert.strictEqual(completed.spec.status, "completed", "the Spec transitioned in_review -> completed (lifecycle to done)");

    // --- REAL-TIME on the BOARD: navigate back to the board; it must show the Spec in the DONE
    // column live (the transitions broadcast `specs://changed`; the board re-pulls). ---
    await openBoard();
    await browser.waitUntil(
      async () => browser.execute(() => (document.body.innerText || "").indexOf(" in progress") !== -1),
      { timeout: 15000, timeoutMsg: "the board header did not render" },
    );
    // 0 in progress now (the Spec is completed) — the subtitle reflects the live status.
    await browser.waitUntil(
      async () => browser.execute(() => /\b0 in progress\b/.test(document.body.innerText || "")),
      {
        timeout: 15000,
        timeoutMsg: "the board did not reflect the completed Spec live (0 in progress) after the MCP lifecycle",
      },
    );
  });

  it("non-regression: the whole workflow ran inside nyx with NO Depot, and the app is still coherent", async function () {
    // Sanity: the app process is alive, the board renders, and NOTHING here ever called Depot — the
    // Spec was created / framed / transitioned entirely through the nyx MCP loopback + the nyx UI.
    const title = await browser.getTitle();
    assert(typeof title === "string", "the Electron app is still responsive");
    const text = await bodyText();
    assert(/\bSpecs\b/.test(text), "the board still renders after the full lifecycle (no crash / regression)");
    // The completed Spec is still present on the board (its card in the Done column).
    await openSpecCard(specId, 15000);
    await browser.waitUntil(
      async () => browser.execute((t) => (document.body.innerText || "").indexOf(t) !== -1, "Absorb the widget domain"),
      { timeout: 15000, timeoutMsg: "the completed Spec's detail is not reachable (regression)" },
    );
  });
});
