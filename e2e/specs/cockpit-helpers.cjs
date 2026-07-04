/* eslint-disable */
// Shared helpers for the v2.2 "travail actif" COCKPIT gate e2e (task
// 01KWHPV33TY0VM284TJEEBGP8K). The gate drives the FULL free-work journey against the REAL
// Electron app and asserts it end-to-end THROUGH THE COCKPIT, then across a real restart.
//
// It is split across two app sessions (a real close + reopen), like the restore pair:
//   - cockpit-01-seed  (session 1) opens the free-work workspace, runs managed commands
//     (services) + terminals with observable output, makes a Git change, and starts a REAL
//     agent session, then navigates into the cockpit and asserts every region is live.
//   - cockpit-02-verify (session 2) boots a fresh app on the SAME NYX_DATA_DIR (same DB) and
//     asserts the workspace CONTEXT is resumed after the restart, back in the cockpit.
// The two share one NYX_DATA_DIR (see wdio.conf.cjs specDataDir "cockpit") and hand off the
// seeded ids/markers through a small JSON in that dir.
//
// TWO seams are used, exactly as the existing specs do — ZERO new production plumbing:
//   1. `window.__nyx` (the shell view's inert e2e seam, terminal-manager.tsx) to SEED the
//      free-work runtime: create the project/workspace, create + run managed commands, open
//      terminals, drive typing. This is the SAME seam commands.e2e / workspace.e2e drive.
//   2. The REAL MCP `agent_session_event` tool over the sidecar's loopback HTTP
//      (`http://127.0.0.1:<NYX_MCP_PORT>/mcp`) — the EXACT transport a real Claude Code hook
//      uses to record a session. The e2e MCP port is pinned by wdio.conf.cjs, so the Node
//      side of the spec reaches the launched app's MCP server with no discovery. This writes a
//      genuine `agent_sessions` row + its `agent_session.started` domain event, so the session
//      surfaces in the runtime aggregate, the cockpit Agents panel, and the timeline — proving
//      the "session agent" leg of the gate with no test-only backend command.
//
// The COCKPIT is a TOP-LEVEL view keyed on the URL hash (use-app-view.ts); we navigate into
// it with `#/cockpit/project/<id>` (free-work on the project's PRIMARY workspace). In the
// cockpit view the shell's `window.__nyx` is unmounted, so cockpit assertions read the REAL
// rendered DOM (the panels render legible French labels + reserved-seam markers) — plus the
// per-terminal `window.__nyxDeck` seam (published by the SAME <TerminalDeck> the cockpit's
// pane host reuses) to prove a LIVE terminal pane inside the cockpit.

const fs = require("fs");
const os = require("os");
const path = require("path");
const http = require("http");
// Shared E2E primitives live in restore-helpers.cjs (single source of truth). The cockpit gate
// reuses them rather than re-implementing them; only the truly-new seams (MCP loopback,
// deck-seam panes, cockpit navigation) are defined below.
const restore = require("./restore-helpers.cjs");

// -------------------------------------------------------------------------------------------
// Handoff between the seed (session 1) and verify (session 2) specs, in the shared data dir.
// Built from restore-helpers' shared write/read logic but bound to a DISTINCT basename so the
// cockpit pair keeps its OWN handoff file — the two seed→verify pairs run independently and must
// never share a handoff (isolation preserved).
// -------------------------------------------------------------------------------------------

const { handoffPath, writeHandoff, readHandoff } = restore.makeHandoff("cockpit-handoff.json");

// -------------------------------------------------------------------------------------------
// A stable, REAL git repo dir on the host the app shares (spec + app run on the same machine).
// Fixed name under tmp so it SURVIVES across the seed→verify restart (onPrepare wipes only the
// e2e DB root, not this). A mono-repo work tree with ONE untracked file so `git status
// --porcelain` reports the tree DIRTY → the cockpit Changements panel is non-empty.
// -------------------------------------------------------------------------------------------

function gitRepoDir() {
  const root = fs.realpathSync(os.tmpdir()); // resolve any /tmp symlink up front
  return path.join(root, "nyx-cockpit-e2e-repo");
}

const CHANGED_FILE = "cockpit-change.txt";

// Materialize (or refresh) the git repo the free-work project points at. Idempotent: safe to
// call in BOTH sessions (verify re-ensures it exists so the restored workspace's Git summary
// still resolves). Uses the host `git` (present on every CI OS matrix runner).
function ensureGitRepo() {
  const { execFileSync } = require("child_process");
  const dir = gitRepoDir();
  fs.mkdirSync(dir, { recursive: true });
  const git = (...args) => execFileSync("git", ["-C", dir, ...args], { stdio: "pipe" });
  if (!fs.existsSync(path.join(dir, ".git"))) {
    git("init", "-q");
    git("config", "user.email", "e2e@nyx.local");
    git("config", "user.name", "nyx e2e");
    git("config", "commit.gpgsign", "false");
    fs.writeFileSync(path.join(dir, "README.md"), "# nyx cockpit e2e repo\n");
    git("add", "-A");
    git("commit", "-q", "-m", "initial");
  }
  // An UNTRACKED file so the work tree is dirty → the Changements volet lists a changed file.
  fs.writeFileSync(path.join(dir, CHANGED_FILE), "free-work change " + Date.now() + "\n");
  return fs.realpathSync(dir);
}

// -------------------------------------------------------------------------------------------
// SHELL-view seam (`window.__nyx`) helpers — seed the free-work runtime.
// -------------------------------------------------------------------------------------------

// Wait for the shell seam — React mounted AND the full seed seam is published (gated
// VITE_NYX_E2E), covering the terminal + project/workspace + managed-command surfaces the seed
// needs. Anti-false-green: a build that never mounts / never publishes the seam FAILS here
// rather than silently passing.
async function waitForShellSeam() {
  await browser.waitUntil(
    async () =>
      browser.execute(function () {
        return !!(
          window.__nyx &&
          typeof window.__nyx.createProject === "function" &&
          typeof window.__nyx.createCommand === "function" &&
          typeof window.__nyx.create === "function"
        );
      }),
    { timeout: 30000, timeoutMsg: "window.__nyx seed seam never appeared (app/seam did not start)" },
  );
}

const createProject = (name, rootPath) =>
  browser.execute((n, r) => window.__nyx.createProject(n, r), name, rootPath);

const createCommand = (projectId, name, command) =>
  browser.execute(
    (p, n, c) => window.__nyx.createCommand(p, n, c),
    projectId,
    name,
    command,
  );

const listInstances = (workspaceId) =>
  browser.execute((w) => window.__nyx.listCommandInstances(w), workspaceId);

const commandStart = (instanceId) => browser.execute((i) => window.__nyx.commandStart(i), instanceId);

const commandState = (workspaceId, instanceId) =>
  browser.execute(
    function (workspaceId, instanceId) {
      var live = window.__nyx.commandState(instanceId);
      if (live && live !== "idle") return Promise.resolve(live);
      return window.__nyx.listCommandInstances(workspaceId).then(function (rows) {
        var row = (rows || []).find(function (r) {
          return r.id === instanceId;
        });
        return row ? row.last_state : "idle";
      });
    },
    workspaceId,
    instanceId,
  );

async function waitForCommandState(workspaceId, instanceId, want, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = "";
  while (Date.now() < deadline) {
    last = await commandState(workspaceId, instanceId);
    if (last === want) return last;
    await browser.pause(150);
  }
  throw new Error(
    "timed out waiting for command state '" + want + "' on " + instanceId + " (last '" + last + "')",
  );
}

async function waitForCommandOutput(instanceId, needle, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = "";
  while (Date.now() < deadline) {
    last = await browser.execute((i) => window.__nyx.commandOutput(i), instanceId);
    if (last && last.includes(needle)) return last;
    await browser.pause(150);
  }
  throw new Error(
    "timed out waiting for command output '" + needle + "' on " + instanceId + ". Buffer:\n" + last,
  );
}

// Shared with restore-helpers (byte-identical): snapshot the current terminal records.
const listRecords = restore.listRecords;

// Open a terminal at `cwd`, waiting for the record + its shell. Returns the new record id.
async function openTerminalAt(cwd) {
  const before = (await listRecords()).map((r) => r.id);
  await browser.execute((c) => window.__nyx.create(c), cwd);
  await browser.waitUntil(async () => (await listRecords()).length > before.length, {
    timeout: 15000,
    timeoutMsg: "new terminal record did not appear",
  });
  await browser.pause(700); // let the shell spawn + print a prompt
  const after = await listRecords();
  const fresh = after.filter((r) => before.indexOf(r.id) === -1);
  return fresh[fresh.length - 1].id;
}

// Shared with restore-helpers: type a line into terminal `id` (keystrokes → PTY). The cockpit
// seed only calls this after waitForShellSeam(), so restore's `window.__nyx` guard is a
// harmless superset — behaviorally identical for the cockpit specs.
const typeInto = restore.typeInto;

// Run the REAL auto-attach for a record (reads its live /proc cwd → resolves the longest-
// ancestor known workspace) and reflect the binding. Returns `{ workspace_id, changed }`.
const autoAttach = (recordId) => browser.execute((i) => window.__nyx.autoAttach(i), recordId);

// Poll the front list until terminal `id` reflects `workspaceId` as its binding (what the
// runtime aggregate anchors on). The backend attach already succeeded; this waits for React to
// commit the reflected `workspace_id`.
async function waitForBinding(id, workspaceId) {
  let last;
  await browser.waitUntil(
    async () => {
      const rec = (await listRecords()).find((r) => r.id === id);
      last = rec && rec.workspace_id;
      return last === workspaceId;
    },
    {
      timeout: 10000,
      timeoutMsg: "record " + id + " workspace_id never became " + workspaceId + " (got " + last + ")",
    },
  );
}

// Shared with restore-helpers (byte-identical): poll terminal `id`'s buffer until `needle`.
const waitForBuffer = restore.waitForBuffer;

// -------------------------------------------------------------------------------------------
// REAL MCP agent-session seam — POST `agent_session_event` to the sidecar loopback.
// -------------------------------------------------------------------------------------------

function mcpPort() {
  return process.env.NYX_E2E_MCP_PORT || process.env.NYX_MCP_PORT || "8779";
}

// POST one JSON-RPC 2.0 request to the launched app's MCP server (loopback). Node's `http` is
// used (portable across the OS matrix); Host is `127.0.0.1:<port>` (local) and no Origin is
// sent, so the sidecar's anti-DNS-rebinding guard passes. Rejects on a transport error or a
// JSON-RPC error envelope so a broken MCP round-trip FAILS the gate (never a false green).
function mcpCall(method, params) {
  const body = JSON.stringify({ jsonrpc: "2.0", id: Date.now(), method, params });
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        host: "127.0.0.1",
        port: Number(mcpPort()),
        path: "/mcp",
        method: "POST",
        headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body) },
      },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          let parsed;
          try {
            parsed = JSON.parse(data);
          } catch (e) {
            return reject(new Error("MCP response was not JSON (status " + res.statusCode + "): " + data));
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

// Wait for the app's MCP server to answer `GET /health` on the pinned port, so the session
// POST is not racing the boot handshake (the app starts the MCP server after the sidecar boot).
async function waitForMcp(timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 30000);
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      await new Promise((resolve, reject) => {
        const req = http.request(
          { host: "127.0.0.1", port: Number(mcpPort()), path: "/health", method: "GET" },
          (res) => {
            res.resume();
            res.statusCode === 200 ? resolve() : reject(new Error("health " + res.statusCode));
          },
        );
        req.on("error", reject);
        req.end();
      });
      return;
    } catch (e) {
      lastErr = e;
      await browser.pause(300);
    }
  }
  throw new Error(
    "MCP server never became healthy on 127.0.0.1:" + mcpPort() + " (" + (lastErr && lastErr.message) + ")",
  );
}

// Start a REAL Claude-Code agent session on `terminalId` via the MCP `agent_session_event`
// tool (a `SessionStart` hook payload). Returns the tool's structuredContent (session_id,
// workspace_id, agent_kind, …). The session's workspace is derived from the TERMINAL's binding,
// so the terminal must already be attached to the free-work workspace.
async function startAgentSession(terminalId, cwd, externalSessionId) {
  const result = await mcpCall("tools/call", {
    name: "agent_session_event",
    arguments: {
      hook_event_name: "SessionStart",
      session_id: externalSessionId,
      source: "startup",
      cwd,
      NYX_TERMINAL_ID: terminalId,
    },
  });
  // The MCP envelope carries the tool payload under structuredContent (and mirrors it in the
  // text content). Prefer structuredContent; fall back to parsing the text block.
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

// -------------------------------------------------------------------------------------------
// COCKPIT-view helpers — navigate the top-level hash + read the rendered cockpit DOM.
// -------------------------------------------------------------------------------------------

// Route the top-level view to the FREE-WORK cockpit on a project's primary workspace. Writes
// the hash and dispatches `hashchange` (mirrors use-app-view.setView) so the app swaps to
// <WorkspaceCockpit> without a router.
async function openFreeWorkCockpit(projectId) {
  await browser.execute(function (pid) {
    window.location.hash = "#/cockpit/project/" + pid;
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  }, projectId);
}

// Route back to the shell (clears the hash), so a follow-up seed step can reach `window.__nyx`.
async function openShell() {
  await browser.execute(function () {
    history.replaceState(null, "", window.location.pathname + window.location.search);
    window.dispatchEvent(new HashChangeEvent("hashchange"));
  });
}

// Wait for the cockpit header to render the workspace identity (name + path). Proves the
// cockpit is MOUNTED and centred on the workspace. Returns the header text.
async function waitForCockpit(timeoutMs) {
  await browser.waitUntil(
    async () => {
      const has = await browser.execute(function () {
        // The cockpit header renders the workspace name in a font-medium truncate span over a
        // font-mono path line. Detect the reserved Spec seam (unique to the cockpit header) as
        // the mount signal — it is present only in <CockpitHeader>.
        return !!document.querySelector('[data-reserved-seam="Spec"]');
      });
      return has;
    },
    { timeout: timeoutMs || 20000, timeoutMsg: "cockpit did not mount (no reserved Spec seam)" },
  );
}

// The visible text of the whole cockpit surface (for text-presence assertions on the French
// panel labels). Reads document.body — in the cockpit view the shell is unmounted.
async function cockpitText() {
  return browser.execute(() => document.body.innerText || "");
}

// Query helper: does the cockpit DOM contain an element matching `selector`?
async function hasElement(selector) {
  return browser.execute((s) => !!document.querySelector(s), selector);
}

// The count-pill value rendered next to a cockpit section/panel micro-label `title` (e.g.
// "Terminals", "Services", "Agents", "Changements", "Timeline"). Returns the integer, or null
// when the labelled section is absent. Robust to layout: it finds the micro-label by text then
// reads the adjacent count badge digits.
async function sectionCount(title) {
  return browser.execute(function (title) {
    const labels = Array.from(document.querySelectorAll("span")).filter(
      (el) => (el.textContent || "").trim() === title,
    );
    for (const label of labels) {
      // The count badge is a SIBLING pill right after the label in the header row.
      const row = label.parentElement;
      if (!row) continue;
      const pill = Array.from(row.querySelectorAll("span")).find(
        (el) => el !== label && /^\d+$/.test((el.textContent || "").trim()),
      );
      if (pill) return Number((pill.textContent || "").trim());
    }
    return null;
  }, title);
}

// Read a live terminal's xterm buffer through the per-terminal deck seam — published by the
// SAME <TerminalDeck> the cockpit's pane host reuses, so it works IN the cockpit view. Proves
// a LIVE terminal pane is mounted inside the cockpit (not a snapshot / screenshot). Returns
// `null` when no pane is mounted for the id.
async function readDeckBuffer(terminalId) {
  return browser.execute(function (id) {
    const deck = window.__nyxDeck || {};
    return typeof deck[id] === "function" ? deck[id]() : null;
  }, terminalId);
}

// Type into a live terminal pane through the deck INPUT seam (published by the SAME
// <TerminalDeck> the cockpit reuses). Lets the spec prove the cockpit's pane is a LIVE,
// TYPABLE terminal (its PTY echoes the keystrokes), not a static render. Returns whether the
// input seam was present for the id.
async function typeIntoDeck(terminalId, data) {
  return browser.execute(
    function (id, d) {
      const seam = window.__nyxDeckInput || {};
      if (typeof seam[id] === "function") {
        seam[id](d);
        return true;
      }
      return false;
    },
    terminalId,
    data,
  );
}

// Poll a cockpit pane's buffer (via readDeckBuffer) until `needle` appears, or throw. Used to
// assert a freshly-typed marker echoes back through the LIVE pane mounted in the cockpit.
async function waitForDeckBuffer(terminalId, needle, timeoutMs) {
  const deadline = Date.now() + (timeoutMs || 15000);
  let last = null;
  while (Date.now() < deadline) {
    last = await readDeckBuffer(terminalId);
    if (last != null && last.indexOf(needle) !== -1) return last;
    await browser.pause(200);
  }
  throw new Error(
    "timed out waiting for '" + needle + "' in cockpit pane " + terminalId + ". Buffer:\n" + last,
  );
}

module.exports = {
  handoffPath,
  writeHandoff,
  readHandoff,
  gitRepoDir,
  ensureGitRepo,
  CHANGED_FILE,
  waitForShellSeam,
  createProject,
  createCommand,
  listInstances,
  commandStart,
  waitForCommandState,
  waitForCommandOutput,
  listRecords,
  openTerminalAt,
  typeInto,
  autoAttach,
  waitForBinding,
  waitForBuffer,
  mcpPort,
  mcpCall,
  waitForMcp,
  startAgentSession,
  openFreeWorkCockpit,
  openShell,
  waitForCockpit,
  cockpitText,
  hasElement,
  sectionCount,
  readDeckBuffer,
  typeIntoDeck,
  waitForDeckBuffer,
};
