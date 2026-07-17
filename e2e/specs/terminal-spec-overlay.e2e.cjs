/* eslint-disable */
// v2.5.10 TERMINAL↔SPEC OVERLAY GATE — the whole surcouche flow driven against the REAL Electron
// app (PRD 01KXKM4X65P9CVQMX3ZBZY7H6H, Phase 3 gate task 01KXN0HXT43ZCG1RMBG7EKPKVV).
//
// DONE-CRITERION (verbatim): "Scénario e2e vert : auto-map multi-Spec + overlay live + empilement
// + rail avancement + clear démappe (pas compact/resume) + temps réel + non-régression."
//
// A single-session, CDP-piloted gate that proves the full terminal↔Spec overlay end-to-end,
// exactly as an agent + a user would exercise it — no test-only backend plumbing:
//
//   1. AUTO-MAP multi-Spec — a live agent session on a terminal CREATES ≥2 Specs via the real
//      MCP tools (carrying the `X-Nyx-Terminal-Id` header the plugin `.mcp.json` sends, so
//      nyx-core attributes the work to the terminal's session and ADDS each worked Spec to its
//      set). A READ tool from the same session adds NOTHING (accumulation ciblée).
//   2. CREATION overlay live + EMPILEMENT — the two authored Specs surface as a STACK of sober
//      drawer tabs (N Specs = N tabs, fixed size, cap+scroll); clicking one opens its LIVE
//      building panel (sections + streamed tasks).
//   3. RAIL avancement — moving the Specs into DEV (in_progress) flips the overlay from the
//      creation tiroir to the advancement RAIL (a single closed tab with a gauge + phase pips;
//      its open panel renders the task decomposition fidèle to the real Spec detail).
//   4. /clear DÉMAPPE (but compact/resume do NOT) — a `SessionEnd reason=resume` keeps the set
//      (the overlay stays); a `SessionEnd reason=clear` vacates it (the overlay disappears).
//      `/compact` fires NO SessionEnd at all, so it never un-maps by construction.
//   5. TEMPS RÉEL sans polling — every step above lands in the overlay purely via the
//      `specs://changed` / `agent-sessions://changed` pushes a mutation broadcasts (no reload,
//      no polling): a task added to an open Spec streams into the panel; a status change flips
//      the overlay mode; the un-map makes it vanish.
//   6. NON-RÉGRESSION — a terminal with no mapped Spec (before any Spec is worked, and again
//      after /clear) renders its live pane with NO overlay; the Terminals page stays coherent.
//
// SEAMS (all pre-existing, ZERO new production plumbing — the same seams the cockpit + spec-
// workflow gates use):
//   - `window.__nyxWork` (base-layout, VITE_NYX_E2E) — seed the anchor project + workspace.
//   - the REAL MCP loopback (`http://127.0.0.1:<NYX_MCP_PORT>/mcp`) — start the agent session
//     (`agent_session_event`) and drive the Spec authoring/transition tools, exactly as an
//     agent's Claude-Code hook + MCP client would. We add the `X-Nyx-Terminal-Id` request
//     HEADER (the auto-map correlation the plugin `.mcp.json` carries) so the work maps to the
//     session — the ONE detail the shared `mcpCall` helper omits (it never seeds a mapping).
//   - the real navigation shell + the overlay's own aria-labelled chrome (the closed tabs, the
//     stack container, the open panels) for the assertions.
//
// ANTI-FALSE-GREEN: the before-hook FAILS HARD if the app/seam/MCP never comes up; every MCP
// call rejects on a JSON-RPC error envelope; every overlay assertion has a finite timeout that
// THROWS on miss. A build that does not mount the overlay, a broken auto-map, or a dead
// real-time bus FAILS the suite rather than passing silently.

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");
const http = require("http");
const { execFileSync } = require("child_process");

// Reuse the v2.5.6 shell helpers (all shell-agnostic seams): the `__nyxWork` project seed, the
// rail/section navigation, the workspace-terminal creation, the deck-pane readout, the MCP
// readiness + agent-session start. Only the header-carrying MCP call (below) is new.
const h = require("./cockpit-v256-helpers.cjs");

// -------------------------------------------------------------------------------------------
// MCP loopback WITH the auto-map correlation header (`X-Nyx-Terminal-Id`)
// -------------------------------------------------------------------------------------------

function mcpPort() {
  return process.env.NYX_E2E_MCP_PORT || process.env.NYX_MCP_PORT || "8779";
}

// POST one JSON-RPC 2.0 request to the launched app's MCP loopback, OPTIONALLY carrying extra
// headers. The auto-map keys on the `X-Nyx-Terminal-Id` header (the plugin `.mcp.json` binds it
// to `${NYX_TERMINAL_ID}`), so a spec-authoring mutation carrying it maps the worked Spec onto
// that terminal's live session. Rejects on a transport error or a JSON-RPC error envelope so a
// broken round-trip FAILS the gate (never a false green).
function mcpCallWithHeaders(method, params, headers) {
  const body = JSON.stringify({ jsonrpc: "2.0", id: Date.now(), method, params });
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        host: "127.0.0.1",
        port: Number(mcpPort()),
        path: "/mcp",
        method: "POST",
        headers: Object.assign(
          { "content-type": "application/json", "content-length": Buffer.byteLength(body) },
          headers || {},
        ),
      },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          let parsed;
          try {
            parsed = JSON.parse(data);
          } catch (e) {
            return reject(
              new Error("MCP response was not JSON (status " + res.statusCode + "): " + data),
            );
          }
          if (parsed.error) {
            return reject(new Error("MCP " + method + " error: " + JSON.stringify(parsed.error)));
          }
          resolve(parsed.result);
        });
      },
    );
    req.on("error", reject);
    req.write(body);
    req.end();
  });
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

// Call a Spec MCP tool AS the given terminal's session (carrying the auto-map header) → the
// worked Spec joins that session's set. Returns the parsed tool payload.
async function specToolAsOnce(terminalId, name, args) {
  const result = await mcpCallWithHeaders(
    "tools/call",
    { name, arguments: args },
    { "X-Nyx-Terminal-Id": terminalId },
  );
  return toolPayload(result);
}

// A TRANSIENT backend error: an entity the seam/deck already surfaced in the renderer (the
// project row, the terminal record) has not yet COMMITTED in the sidecar's SQLite when the MCP
// call fires — a `create_*` round-trips the renderer optimistically ahead of the DB write. These
// clear within a beat, so we retry; a NON-transient error (a real bug) throws immediately.
function isTransient(message) {
  return /FOREIGN KEY|unknown terminal|unknown project|no active session|not found/i.test(
    message || "",
  );
}

// Call a Spec MCP tool, RETRYING only while the backend is racing an entity commit (see
// `isTransient`). Spec mutations are transactional (a failed insert rolls back cleanly), so a
// retry never double-commits. Any non-transient error surfaces at once.
async function specToolAs(terminalId, name, args, tries) {
  const attempts = tries || 20;
  let lastErr;
  for (let i = 0; i < attempts; i++) {
    try {
      return await specToolAsOnce(terminalId, name, args);
    } catch (e) {
      lastErr = e;
      if (!isTransient(e && e.message)) throw e;
      await browser.pause(300);
    }
  }
  throw lastErr;
}

// Start the agent session, RETRYING while the freshly-created terminal record is still racing its
// SQLite commit (`unknown terminal`) — the deck surfaces the pane id optimistically before the
// backend `create_terminal` lands. Returns the session payload once the terminal is known.
async function startSession(terminalId, cwd, externalSessionId) {
  let lastErr;
  for (let i = 0; i < 25; i++) {
    try {
      return await h.startAgentSession(terminalId, cwd, externalSessionId);
    } catch (e) {
      lastErr = e;
      if (!isTransient(e && e.message)) throw e;
      await browser.pause(300);
    }
  }
  throw lastErr;
}

// Fire a Claude-Code `SessionEnd { reason }` hook for the terminal's session over MCP — the EXACT
// payload the nyx SessionEnd hook posts. `reason=clear` RESETS the working context (un-map);
// `reason=resume` is an internal transition that KEEPS it. `agent_session_event` reads the
// terminal from the `NYX_TERMINAL_ID` argument (the header is harmless here).
async function sessionEnd(terminalId, externalSessionId, reason) {
  return mcpCallWithHeaders(
    "tools/call",
    {
      name: "agent_session_event",
      arguments: {
        hook_event_name: "SessionEnd",
        session_id: externalSessionId,
        reason,
        NYX_TERMINAL_ID: terminalId,
      },
    },
    { "X-Nyx-Terminal-Id": terminalId },
  );
}

// -------------------------------------------------------------------------------------------
// A real git repo on the host so the seeded project/workspace derives a real repo (the Spec's
// workspace context resolves). Fixed name under tmp, idempotent.
// -------------------------------------------------------------------------------------------

function ensureOverlayRepo() {
  const root = fs.realpathSync(os.tmpdir());
  const dir = path.join(root, "nyx-terminal-spec-overlay-e2e-repo");
  fs.mkdirSync(dir, { recursive: true });
  const git = (...args) => execFileSync("git", ["-C", dir, ...args], { stdio: "pipe" });
  if (!fs.existsSync(path.join(dir, ".git"))) {
    git("init", "-q");
    git("config", "user.email", "e2e@nyx.local");
    git("config", "user.name", "nyx e2e");
    git("config", "commit.gpgsign", "false");
    fs.writeFileSync(path.join(dir, "README.md"), "# nyx terminal-spec-overlay e2e repo\n");
    git("add", "-A");
    git("commit", "-q", "-m", "initial");
  }
  return fs.realpathSync(dir);
}

// -------------------------------------------------------------------------------------------
// Overlay DOM readers (the surcouche's own aria-labelled chrome)
// -------------------------------------------------------------------------------------------

// The names on the CREATION tiroir's stacked closed tabs (`Open <name> live spec`), in DOM order.
async function drawerTabNames() {
  return browser.execute(function () {
    return Array.from(
      document.querySelectorAll('button[aria-label^="Open "][aria-label$=" live spec"]'),
    ).map(function (el) {
      return (el.getAttribute("aria-label") || "")
        .replace(/^Open /, "")
        .replace(/ live spec$/, "");
    });
  });
}

// How many advancement RAIL closed tabs are present (`Open <title> progress`) — the rail is
// always a SINGLE Spec (no stacking), so this is 0 (no dev overlay) or 1 (rail showing).
async function railTabCount() {
  return browser.execute(function () {
    return document.querySelectorAll('button[aria-label^="Open "][aria-label$=" progress"]').length;
  });
}

// Is ANY overlay chrome (drawer stack, a drawer tab/panel, or a rail tab/panel) on screen?
async function overlayPresent() {
  return browser.execute(function () {
    return !!(
      document.querySelector('[aria-label="Live specs"]') ||
      document.querySelector('button[aria-label$=" live spec"]') ||
      document.querySelector('section[aria-label$=" live spec"]') ||
      document.querySelector('button[aria-label$=" progress"]') ||
      document.querySelector('section[aria-label$=" progress"]')
    );
  });
}

// The COMPUTED shape of the drawer stack (fixed tab size + cap/scroll proof), read from geometry
// and `getComputedStyle` rather than Tailwind class literals — so a purely cosmetic class rename
// (e.g. h-36 → h-32, or a move to CSS modules) can never redden this gate without a real layout
// regression. Same hedging convention as review-gate-tab.e2e.cjs:309 (getComputedStyle fallback).
async function drawerStackShape() {
  return browser.execute(function () {
    var tabs = Array.from(
      document.querySelectorAll('button[aria-label^="Open "][aria-label$=" live spec"]'),
    );
    var stack = document.querySelector('[aria-label="Live specs"]');
    var stackCs = stack ? getComputedStyle(stack) : null;
    return {
      tabCount: tabs.length,
      // Rendered height (px) of each stacked tab — a FIXED tab size makes these equal + bounded,
      // independent of the Spec name each tab carries (content-independent = fixed layout).
      tabHeights: tabs.map(function (t) {
        return Math.round(t.getBoundingClientRect().height);
      }),
      // The first tab's RESOLVED height: a fixed sizing utility resolves to a concrete px value
      // (a content-driven auto height would not be a stable px literal).
      tabHeightComputed: tabs[0] ? getComputedStyle(tabs[0]).height : null,
      // The stack container's scroll config + height cap (behavioral cap+scroll, not class names):
      // overflow-y auto/scroll + a bounded max-height/height ⇒ it scrolls when N grows, never
      // overflowing the pane.
      stackOverflowY: stackCs ? stackCs.overflowY : null,
      stackMaxHeight: stackCs ? stackCs.maxHeight : null,
      stackHeight: stackCs ? stackCs.height : null,
    };
  });
}

// Click an element by CSS selector directly in the renderer (the overlay is absolutely
// positioned over the deck; a direct DOM `.click()` avoids any hit-test flakiness). THROWS if
// absent so a missing affordance fails honestly.
async function clickSelector(sel) {
  const ok = await browser.execute(function (s) {
    var el = document.querySelector(s);
    if (el) {
      el.click();
      return true;
    }
    return false;
  }, sel);
  assert(ok, "expected to click " + sel);
}

async function bodyText() {
  return browser.execute(() => document.body.innerText || "");
}

async function bodyIncludes(needle) {
  return browser.execute((n) => (document.body.innerText || "").indexOf(n) !== -1, needle);
}

async function waitForBody(needle, timeoutMs, msg) {
  await browser.waitUntil(async () => bodyIncludes(needle), {
    timeout: timeoutMs || 15000,
    timeoutMsg: msg || 'text "' + needle + '" never appeared',
  });
}

// Whether the selected terminal's LIVE deck pane is mounted (non-regression: the Terminals page
// renders its terminal even with no overlay).
async function deckPaneMounted(terminalId) {
  return browser.execute(function (id) {
    return !!document.querySelector('[data-terminal-id="' + id + '"]');
  }, terminalId);
}

describe("v2.5.10 terminal↔Spec overlay gate — auto-map multi-Spec + live/rail overlay + clear + temps réel", function () {
  // Shared across the ordered `it` blocks (one app session, one terminal, one agent session).
  const EXTERNAL_SESSION_ID = "overlay-e2e-" + Date.now();
  const SPEC_A_TITLE = "Overlay Ingest Spec";
  const SPEC_B_TITLE = "Overlay Export Spec";

  let repoDir;
  let projectId;
  let terminalId;
  let specA;
  let specB;

  before(async function () {
    // Anti-false-green gates: the shell seam AND the MCP loopback must both come up.
    await h.waitForWorkShell();
    await h.waitForMcp(30000);
    repoDir = ensureOverlayRepo();
  });

  it("non-régression baseline: a terminal with a live session but no worked Spec renders NO overlay", async function () {
    // --- Seed the anchor project + its root workspace (real repo → workspace context resolves). ---
    const created = await h.seedProject("terminal-spec-overlay-e2e", repoDir);
    assert(created && created.project && created.root, "seedProject returns project + root");
    projectId = created.project.id;

    // --- Enter the project + land on the Terminals page, then open ONE workspace terminal. A
    //     fresh project has no terminals, so this is the only one → the page auto-selects it and
    //     the overlay mounts for it. ---
    await h.enterProject("terminal-spec-overlay-e2e");
    await h.gotoSection("Terminals");
    const before = await h.deckTerminalIds();
    assert.strictEqual(before.length, 0, "a fresh project starts with no terminals");
    terminalId = await h.newWorkspaceTerminal();
    assert(terminalId, "a workspace terminal was created");
    await browser.waitUntil(async () => (await h.deckTerminalIds()).length === 1, {
      timeout: 15000,
      timeoutMsg: "the created terminal never became the single deck pane",
    });

    // The live pane is mounted (page is coherent) …
    await browser.waitUntil(async () => deckPaneMounted(terminalId), {
      timeout: 15000,
      timeoutMsg: "the selected terminal's live deck pane never mounted",
    });
    // … and with NO Spec mapped yet there is NO overlay at all (non-régression floor).
    assert.strictEqual(await overlayPresent(), false, "no mapped Spec ⇒ no overlay on the terminal");

    // --- Start a REAL agent session on the terminal (its workspace is derived from the terminal
    //     binding). A live session ALONE (empty Spec set) still yields NO overlay. ---
    const session = await startSession(terminalId, repoDir, EXTERNAL_SESSION_ID);
    assert(session && (session.session_id || session.state), "the agent session started");
    // The overlay re-pulls on `agent-sessions://changed`; give it a beat, then assert still empty.
    await browser.pause(600);
    assert.strictEqual(
      await overlayPresent(),
      false,
      "a live session with an empty Spec set ⇒ still no overlay",
    );
  });

  it("auto-map multi-Spec + création live + empilement: 2 Specs worked via MCP stack as 2 drawer tabs (a read maps nothing)", async function () {
    // --- Work Spec A via MCP AS this terminal's session (the X-Nyx-Terminal-Id header) → auto-map
    //     ADDS A to the session's set. The overlay re-pulls off `specs://changed` (temps réel). ---
    const outA = await specToolAs(terminalId, "spec_create", {
      project_id: projectId,
      title: SPEC_A_TITLE,
      problem: "Data lands in no canonical store.",
      solution: "Introduce an ingest pipeline into the store.",
    });
    assert(outA && outA.spec && outA.spec.id, "spec_create A returned a spec");
    specA = outA.spec.id;
    assert.strictEqual(outA.spec.status, "draft", "Spec A is born in draft (creation)");

    // The creation tiroir appears with EXACTLY ONE sober tab carrying A's name — no reload.
    await browser.waitUntil(
      async () => {
        const names = await drawerTabNames();
        return names.length === 1 && names[0] === SPEC_A_TITLE;
      },
      {
        timeout: 15000,
        timeoutMsg: "Spec A never surfaced as a live-creation drawer tab (auto-map / temps réel)",
      },
    );

    // --- A READ tool from the SAME session maps NOTHING (accumulation ciblée: une lecture
    //     n'ajoute pas). The set stays at 1 tab. ---
    await specToolAs(terminalId, "list_projects", {});
    await browser.pause(500);
    let names = await drawerTabNames();
    assert.strictEqual(names.length, 1, "a read tool did NOT add a Spec to the set");

    // --- Work a SECOND Spec B → the set is N=2 → the tiroir STACKS 2 tabs (empilement). ---
    const outB = await specToolAs(terminalId, "spec_create", {
      project_id: projectId,
      title: SPEC_B_TITLE,
      problem: "The store has no export path.",
      solution: "Add an export projection off the store.",
    });
    assert(outB && outB.spec && outB.spec.id, "spec_create B returned a spec");
    specB = outB.spec.id;

    await browser.waitUntil(
      async () => {
        const n = await drawerTabNames();
        return n.length === 2 && n.indexOf(SPEC_A_TITLE) !== -1 && n.indexOf(SPEC_B_TITLE) !== -1;
      },
      {
        timeout: 15000,
        timeoutMsg: "the second Spec never stacked as a 2nd drawer tab (multi-Spec empilement)",
      },
    );

    // The stack is a uniform pile (fixed tab size) that caps + scrolls (never overflows the pane).
    // Proven BEHAVIORALLY (geometry + getComputedStyle), not by asserting Tailwind class literals,
    // so a cosmetic class rename can't red the gate without an actual layout regression.
    const shape = await drawerStackShape();
    assert.strictEqual(shape.tabCount, 2, "the creation tiroir rendered the 2 stacked drawer tabs");
    // FIXED tab size: both stacked tabs render at the SAME, bounded height regardless of their
    // (differently named) Spec — a content-independent, fixed layout.
    assert(
      shape.tabHeights.length === 2 &&
        shape.tabHeights[0] > 0 &&
        shape.tabHeights[0] === shape.tabHeights[1],
      "each drawer tab has the same fixed, content-independent height: " +
        JSON.stringify(shape.tabHeights),
    );
    // …and that height resolves to a concrete px value (a fixed utility, not a fluid auto height).
    assert(
      /^\d+(\.\d+)?px$/.test(shape.tabHeightComputed || ""),
      "the drawer tab height resolves to a fixed px value: " + shape.tabHeightComputed,
    );
    // CAP + SCROLL: the stack container is scrollable on Y AND carries a height cap, so a session
    // cutting a feature into many Specs scrolls instead of overflowing the terminal pane.
    assert(
      shape.stackOverflowY === "auto" || shape.stackOverflowY === "scroll",
      "the drawer stack scrolls vertically (overflow-y is auto/scroll): " + shape.stackOverflowY,
    );
    assert(
      shape.stackMaxHeight != null && shape.stackMaxHeight !== "none",
      "the drawer stack caps its height to the pane (bounded max-height, not unbounded): " +
        shape.stackMaxHeight,
    );
  });

  it("temps réel: opening Spec A's live drawer, a task added via MCP streams into the OPEN panel (no reload)", async function () {
    // Open Spec A's live building panel (click its sober tab).
    await clickSelector(
      'button[aria-label="Open ' + SPEC_A_TITLE + ' live spec"]',
    );
    // The open drawer is the live view: the CREATING chip + A's real Problem section fill in.
    await waitForBody("Creating", 15000, "the live drawer never showed the CREATING state chip");
    await waitForBody(
      "Data lands in no canonical store.",
      15000,
      "the live drawer never rendered Spec A's Problem section",
    );

    // --- Add a task to A via MCP; the OPEN panel must re-pull off `specs://changed` and show it
    //     WITHOUT any reload/navigation (temps réel sans polling). ---
    const taskA = await specToolAs(terminalId, "task_add", {
      project_id: projectId,
      spec_id: specA,
      title: "Alpha wiring task",
      kind: "slice",
      phase: "Implementation",
    });
    assert(taskA && taskA.task && taskA.task.id, "task_add on Spec A succeeded");
    await waitForBody(
      "Alpha wiring task",
      15000,
      "the open live drawer did NOT stream the new task in (real-time bus broken)",
    );

    // Close the drawer panel (chevron) so the next phase reads a clean overlay.
    await clickSelector('button[aria-label="Close live spec panel"]');
  });

  it("rail avancement: moving both Specs into dev flips the overlay from the creation tiroir to the advancement rail", async function () {
    // Give Spec B a task too, so whichever Spec the single-Spec rail surfaces shows a fidèle
    // decomposition. (task_add also keeps B mapped — idempotent.)
    const taskB = await specToolAs(terminalId, "task_add", {
      project_id: projectId,
      spec_id: specB,
      title: "Beta wiring task",
      kind: "slice",
      phase: "Implementation",
    });
    assert(taskB && taskB.task && taskB.task.id, "task_add on Spec B succeeded");

    // --- Move BOTH Specs draft→ready→in_progress via MCP. A Spec is `creation` while draft OR
    //     ready, and `dev` once in_progress; the overlay only flips to the RAIL once NO Spec in
    //     the set is still in creation. ---
    for (const id of [specA, specB]) {
      const ready = await specToolAs(terminalId, "spec_transition", {
        spec_id: id,
        transition: "ready",
        user_confirmed: true,
      });
      assert.strictEqual(ready.spec.status, "ready", "Spec transitioned draft→ready");
    }
    for (const id of [specA, specB]) {
      const started = await specToolAs(terminalId, "spec_transition", {
        spec_id: id,
        transition: "start",
        user_confirmed: true,
      });
      assert.strictEqual(started.spec.status, "in_progress", "Spec transitioned ready→in_progress");
    }

    // The overlay flips: the creation drawer is GONE, and a SINGLE advancement rail tab appears
    // (the rail is one Spec, no stacking) — all live off `specs://changed`, no reload.
    await browser.waitUntil(
      async () => (await drawerTabNames()).length === 0 && (await railTabCount()) === 1,
      {
        timeout: 15000,
        timeoutMsg: "the overlay never flipped from the creation tiroir to the single advancement rail",
      },
    );

    // Open the rail's progress panel → it renders the task decomposition FIDÈLE (the real
    // `<TasksTab>`) with the In-progress status badge and a "wiring" task row.
    await clickSelector('button[aria-label$=" progress"]');
    await waitForBody("In progress", 15000, "the rail panel never showed the In-progress status");
    await waitForBody(
      "wiring task",
      15000,
      "the rail panel never rendered the Spec's task decomposition (fidèle tasks-tab)",
    );
    // Close the rail panel again.
    await clickSelector('button[aria-label="Close progress panel"]');
  });

  it("/clear démappe (compact/resume ne démappent pas): resume keeps the set, clear vacates it — the overlay disappears", async function () {
    // Sanity: the rail overlay is showing before we probe the un-map.
    assert.strictEqual(await railTabCount(), 1, "the rail overlay is present before the un-map probes");

    // --- A `/resume` (SessionEnd reason=resume) is an internal transition that CONTINUES the same
    //     work — it must NOT un-map. `/compact` fires NO SessionEnd at all, so it never un-maps by
    //     construction (nothing to send). The overlay stays. ---
    await sessionEnd(terminalId, EXTERNAL_SESSION_ID, "resume");
    await browser.pause(800);
    assert.strictEqual(
      await railTabCount(),
      1,
      "a /resume kept the Spec set → the overlay is still present (compact/resume ne démappent pas)",
    );

    // --- A `/clear` (SessionEnd reason=clear) RESETS the working context → the set is vacated →
    //     the overlay disappears. Pushed via `specs://changed`, so the overlay re-pulls empty. ---
    await sessionEnd(terminalId, EXTERNAL_SESSION_ID, "clear");
    await browser.waitUntil(async () => (await overlayPresent()) === false, {
      timeout: 15000,
      timeoutMsg: "a /clear did not vacate the Spec set → the overlay never disappeared (un-map broken)",
    });

    // --- NON-RÉGRESSION after the un-map: the terminal's live pane is still mounted and the page
    //     is coherent — the overlay simply left. ---
    assert.strictEqual(
      await deckPaneMounted(terminalId),
      true,
      "the terminal's live pane survives the un-map (Terminals page still coherent)",
    );
    const title = await browser.getTitle();
    assert(typeof title === "string", "the Electron app is still responsive after the full flow");
  });
});
