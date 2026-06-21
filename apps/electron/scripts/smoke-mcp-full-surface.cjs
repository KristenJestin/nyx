#!/usr/bin/env node
/**
 * MCP FULL-SURFACE smoke (PRD-5 review #68, finding 01KVJNVVJ2A8GPMVRM8FCEZ637).
 *
 * Proves the Electron MCP server serves the FULL advertised tool surface at Tauri parity
 * — no advertised tool falls into the dispatcher's `unknown tool` (-32601) arm anymore —
 * and that the `list_commands` routing bug is fixed (it returns COMMANDS filtered by
 * project/workspace, NOT the terminals table).
 *
 * It boots the REAL stack — full Electron `app` + the REAL `CoreHost`, which spawns the
 * REAL Node-pure core-host that loads the `.node` and starts the REAL nyx MCP server
 * (loopback `tiny_http`) on a PINNED port (`NYX_MCP_PORT`). No mock: the assertions go
 * over actual JSON-RPC HTTP to `http://127.0.0.1:<port>/mcp`.
 *
 * Steps:
 *   1. Boot the host (MCP starts on the pinned port). Seed a real project + root workspace
 *      through the SAME `core-command` path the UI uses, so `list_commands`/`list_workspaces`
 *      have real ids to filter on.
 *   2. `tools/list` → enumerate every advertised tool.
 *   3. `tools/call` EACH advertised tool with representative args; assert NO response is a
 *      `-32601` (method_not_found / "unknown tool"). A domain error (invalid_id / …) is a
 *      PASS — the tool was SERVED. This is the core regression guard.
 *   4. Assert `list_commands(project_id)` returns a `commands` array (the templates) and
 *      NO `terminals` key — the bug fix — and `list_commands(workspace_id)` likewise returns
 *      `commands`, honoring the filter.
 *
 * Run under Electron: `electron scripts/smoke-mcp-full-surface.cjs`. Exits 0 on full pass.
 */
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const http = require("node:http");
const { app } = require("electron");

const { CoreHost } = require("../dist/main/core-host.js");

function fail(msg) {
  console.error("[smoke-mcp-full-surface] FAIL:", msg);
  app.exit(1);
}

// Pin a fresh data dir + a deterministic MCP port BEFORE the host spawns (the host process
// reads NYX_MCP_PORT). A high port unlikely to collide on a dev box / CI.
const pinnedDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nyx-mcp-"));
process.env.NYX_DATA_DIR = pinnedDataDir;
const MCP_PORT = 8799;
process.env.NYX_MCP_PORT = String(MCP_PORT);

let rpcId = 1;

/** One JSON-RPC POST to the loopback MCP server. Resolves the parsed envelope. */
function rpc(method, params) {
  return new Promise((resolve, reject) => {
    const body = JSON.stringify({ jsonrpc: "2.0", id: rpcId++, method, params });
    const req = http.request(
      {
        host: "127.0.0.1",
        port: MCP_PORT,
        path: "/mcp",
        method: "POST",
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) },
      },
      (res) => {
        let buf = "";
        res.on("data", (d) => (buf += d));
        res.on("end", () => {
          try {
            resolve(JSON.parse(buf));
          } catch (e) {
            reject(new Error(`bad JSON from MCP (${res.statusCode}): ${buf.slice(0, 200)}`));
          }
        });
      },
    );
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}

/** Call a tool; return the raw JSON-RPC envelope (result OR error). */
function callTool(name, args) {
  return rpc("tools/call", { name, arguments: args || {} });
}

/** The transport `error.code` of an envelope, or null on success. */
function errCode(env) {
  return env && env.error ? env.error.code : null;
}

/** The structured (D8) string code, when present. */
function dataCode(env) {
  return env && env.error && env.error.data ? env.error.data.code : null;
}

/** Representative args per advertised tool so each is genuinely EXERCISED. A domain error
 * is fine; we only forbid -32601. Filled with the seeded ids at runtime. */
function argsFor(name, ctx) {
  switch (name) {
    case "list_workspaces":
      return { project_id: ctx.projectId };
    case "list_commands":
      return { workspace_id: ctx.workspaceId };
    case "list_importable_scripts":
      return { project_id: ctx.projectId };
    case "import_commands":
      return { project_id: ctx.projectId, preview: true };
    case "add_command":
      return { project_id: ctx.projectId, name: `smoke-${Date.now()}`, command: "echo hi" };
    case "update_command":
      return { command_id: "does-not-exist", name: "x" };
    case "remove_command":
      return { command_id: "does-not-exist" };
    case "remove_commands":
      return { command_ids: ["does-not-exist"] };
    case "remove_workspace":
      return { workspace_id: "does-not-exist" };
    case "clear_command_output":
      return { instance_id: "does-not-exist" };
    case "start_command":
    case "stop_command":
    case "relaunch_command":
    case "get_command_output":
      return { instance_id: "does-not-exist" };
    case "wait_for_command":
      return { instance_id: "does-not-exist", timeout_ms: 50 };
    case "workspace_add":
      return { project_id: ctx.projectId, path: pinnedDataDir };
    case "create_workspace":
      return { project_id: ctx.projectId, name: "ws-new", path: path.join(pinnedDataDir, "ws-new") };
    case "send_to_terminal":
      return { terminal_id: "does-not-exist", command: "ls" };
    case "close_terminal":
      return { terminal_id: "does-not-exist" };
    case "read_terminal":
      return { terminal_id: "does-not-exist" };
    case "create_terminal":
      return { label: "smoke" };
    case "list_terminals":
      return { include_closed: true };
    case "agent_session_event":
      return { hook_event_name: "SessionStart", session_id: "s1", NYX_TERMINAL_ID: "does-not-exist" };
    case "probe":
    case "list_projects":
    default:
      return {};
  }
}

app.whenReady().then(async () => {
  const host = new CoreHost();
  host.onEvent((evt) => {
    if (evt.kind === "fatal") fail(`host fatal: ${evt.error}`);
  });
  await host.start();
  if (host.currentState !== "ready") return fail(`host state = ${host.currentState}, expected ready`);
  console.log(`[smoke-mcp-full-surface] host booted (pid=${host.pid}) ✓`);

  // Wait until the MCP server is accepting (it starts during boot, but the accept loop
  // races the `ready` event). Poll /health.
  let up = false;
  for (let i = 0; i < 50 && !up; i++) {
    try {
      const env = await rpc("ping", {});
      up = !!env;
    } catch {
      await new Promise((r) => setTimeout(r, 100));
    }
  }
  if (!up) return fail(`MCP server never accepted on 127.0.0.1:${MCP_PORT}`);
  console.log(`[smoke-mcp-full-surface] MCP server up on http://127.0.0.1:${MCP_PORT}/mcp ✓`);

  // Seed a real project (+ its root workspace) over the SAME core-command path the UI uses.
  const seed = await host.request({
    kind: "core-command",
    command: "create_project",
    argsJson: JSON.stringify({ name: "SmokeProj", rootPath: pinnedDataDir, rootName: "root" }),
  });
  const ctx = { projectId: seed?.project?.id, workspaceId: seed?.root?.id };
  if (!ctx.projectId || !ctx.workspaceId)
    return fail(`seed did not return project + root workspace ids (got ${JSON.stringify(seed)})`);
  console.log(`[smoke-mcp-full-surface] seeded project=${ctx.projectId} workspace=${ctx.workspaceId} ✓`);

  // 1) tools/list — enumerate the advertised surface.
  const listEnv = await rpc("tools/list", {});
  const tools = listEnv?.result?.tools;
  if (!Array.isArray(tools) || tools.length === 0)
    return fail(`tools/list returned no tools (got ${JSON.stringify(listEnv).slice(0, 200)})`);
  const names = tools.map((t) => t.name);
  console.log(`[smoke-mcp-full-surface] tools/list advertises ${names.length} tools: ${names.join(", ")}`);

  // 2) Call EVERY advertised tool — assert NONE returns -32601 (unknown tool).
  const unknown = [];
  const served = [];
  for (const name of names) {
    const env = await callTool(name, argsFor(name, ctx));
    const code = errCode(env);
    if (code === -32601) {
      unknown.push(name);
      console.error(`[smoke-mcp-full-surface]   ${name} → UNKNOWN TOOL (-32601): ${env.error.message}`);
    } else if (code) {
      served.push(`${name} (served, domain error: ${dataCode(env) || code})`);
    } else {
      served.push(`${name} (ok)`);
    }
  }
  console.log("[smoke-mcp-full-surface] per-tool results:");
  for (const s of served) console.log(`[smoke-mcp-full-surface]   ✓ ${s}`);
  if (unknown.length > 0)
    return fail(`${unknown.length} advertised tool(s) returned -32601 (unknown tool): ${unknown.join(", ")}`);
  console.log(`[smoke-mcp-full-surface] 0 unknown tools across ${names.length} advertised tools ✓`);

  // 3) list_commands routing: returns COMMANDS (not terminals), honoring the filter.
  const byProject = await callTool("list_commands", { project_id: ctx.projectId });
  const projSC = byProject?.result?.structuredContent;
  if (!projSC || !Array.isArray(projSC.commands))
    return fail(`list_commands(project_id) did not return a commands array (got ${JSON.stringify(byProject).slice(0, 200)})`);
  if ("terminals" in projSC)
    return fail("list_commands STILL returns a `terminals` key — the routing bug is not fixed");
  console.log(`[smoke-mcp-full-surface] list_commands(project_id) → commands[] (no terminals key) ✓`);

  const byWs = await callTool("list_commands", { workspace_id: ctx.workspaceId });
  const wsSC = byWs?.result?.structuredContent;
  if (!wsSC || !Array.isArray(wsSC.commands))
    return fail(`list_commands(workspace_id) did not return a commands array (got ${JSON.stringify(byWs).slice(0, 200)})`);
  if ("terminals" in wsSC)
    return fail("list_commands(workspace_id) returns a `terminals` key — wrong routing");
  console.log(`[smoke-mcp-full-surface] list_commands(workspace_id) → commands[] (filtered) ✓`);

  // 4) CLAUDE SIDEBAR ICON chain (PRD-5 review #67, finding 01KVJNQMYF2A2M8FB84H211V01).
  // The provider icon in the sidebar is driven by an ACTIVE agent session: a Claude
  // SessionStart hook calls the MCP `agent_session_event` tool, which records the session
  // and emits `agent-sessions://changed`; the front re-pulls `agent_active_sessions` and
  // swaps the terminal row to the Claude logo. In Electron this whole chain was DEAD
  // because `agent_session_event` fell into `unknown tool`. Prove it end-to-end now.
  //
  // 4a. Create a REAL terminal record (via the MCP create_terminal tool) to bind to.
  const ct = await callTool("create_terminal", { label: "claude-term" });
  const termId = ct?.result?.structuredContent?.terminal_id;
  if (!termId) return fail(`create_terminal did not return a terminal_id (got ${JSON.stringify(ct).slice(0, 200)})`);

  // 4a-bis. send_to_terminal PARITY (PRD-5 review finding 01KVJRZDYW848HH50BR8J61WB1): this
  // record is ALIVE but has NO live PTY (this smoke never mounts a renderer xterm, so the
  // front never `register_terminal_pty`'d a live shell). The Tauri server returns
  // `invalid_state` ("no live shell yet"); the Electron server must do the SAME instead of a
  // mendacious `sent: true`. The synchronous liveness registry the dispatcher now consults
  // is empty for this record → invalid_state. Prove it through the REAL MCP server.
  const sendNoPty = await callTool("send_to_terminal", { terminal_id: termId, command: "echo hi" });
  if (!errCode(sendNoPty))
    return fail(
      `send_to_terminal on an alive record with NO live PTY returned success (${JSON.stringify(
        sendNoPty.result,
      ).slice(0, 200)}) — expected invalid_state (Tauri parity)`,
    );
  if (dataCode(sendNoPty) !== "invalid_state")
    return fail(
      `send_to_terminal without a live PTY returned code=${dataCode(sendNoPty) || errCode(sendNoPty)}, expected invalid_state`,
    );
  console.log(
    `[smoke-mcp-full-surface] send_to_terminal (alive record, no live PTY) → invalid_state — Tauri parity, no false success ✓`,
  );

  // 4a-ter. list_terminals reports the record as NOT live (the liveness registry is empty for
  // it) — the `live` bit is now read from the registry, not hard-coded false-but-meaningless.
  const lt = await callTool("list_terminals", { include_closed: true });
  const row = lt?.result?.structuredContent?.terminals?.find((t) => t.terminal_id === termId);
  if (!row) return fail(`list_terminals did not list the new terminal ${termId}`);
  if (row.live !== false)
    return fail(`list_terminals reports live=${row.live} for a record with no live PTY, expected false`);
  console.log(`[smoke-mcp-full-surface] list_terminals → terminal ${termId} live=false (no PTY registered) ✓`);

  // 4b. Watch for the `agent-sessions` changed invalidation the tool emits.
  let agentSessionsChanged = false;
  host.onEvent((evt) => {
    if (evt.kind === "changed" && evt.topic === "agent-sessions") agentSessionsChanged = true;
  });

  // 4c. Fire the Claude SessionStart hook payload through the MCP tool.
  const sessEnv = await callTool("agent_session_event", {
    hook_event_name: "SessionStart",
    session_id: "claude-session-xyz",
    NYX_TERMINAL_ID: termId,
    cwd: pinnedDataDir,
    source: "startup",
  });
  if (errCode(sessEnv))
    return fail(`agent_session_event(SessionStart) errored: ${JSON.stringify(sessEnv.error)}`);
  const sc = sessEnv?.result?.structuredContent;
  if (sc?.event !== "SessionStart" || sc?.terminal_id !== termId)
    return fail(`agent_session_event did not record the session (got ${JSON.stringify(sc)})`);
  console.log(`[smoke-mcp-full-surface] agent_session_event(SessionStart) recorded for terminal ${termId} ✓`);

  // 4d. The icon's DATA SOURCE: `agent_active_sessions` now returns (terminal_id, claude_code).
  // This is the exact command `use-agent-sessions` pulls to drive the row icon.
  const active = await host.request({
    kind: "core-command",
    command: "agent_active_sessions",
    argsJson: "{}",
  });
  const pair = Array.isArray(active) ? active.find((r) => r.terminal_id === termId) : undefined;
  if (!pair) return fail(`agent_active_sessions did not list the new session (got ${JSON.stringify(active)})`);
  if (pair.agent_kind !== "claude_code")
    return fail(`active session agent_kind=${pair.agent_kind}, expected claude_code (the icon key)`);
  console.log(
    `[smoke-mcp-full-surface] agent_active_sessions → (${pair.terminal_id}, ${pair.agent_kind}) — the sidebar icon data source ✓`,
  );

  // 4d-bis. CLOSE-WARNING chain (PRD-5 #6, review finding agent_close_warnings stubbed `[]`).
  // The same live session above belongs to a LOOSE terminal (no project ⇒ resume OFF by
  // COALESCE) — exactly the case a window-close would silently drop. In Electron this was
  // DEAD: `agent_close_warnings` was stubbed `[]` in main's LOCAL_FALLBACKS, so the close
  // dialog never showed. It now round-trips to the host's DbCommandTask arm (the real
  // `close_warning_candidates` + `should_warn_on_close` policy). Prove it returns the REAL
  // candidate — NOT `[]` — over the SAME `core-command` path `close-warning.ts` uses.
  const warnings = await host.request({
    kind: "core-command",
    command: "agent_close_warnings",
    argsJson: "{}",
  });
  console.log(`[smoke-mcp-full-surface] agent_close_warnings raw → ${JSON.stringify(warnings)}`);
  if (!Array.isArray(warnings) || warnings.length === 0)
    return fail(
      `agent_close_warnings returned ${JSON.stringify(warnings)} — expected a NON-empty list ` +
        "(the live session of a non-resuming project must warn; the [] stub regression is back)",
    );
  const warn = warnings.find((w) => w.terminal_id === termId);
  if (!warn) return fail(`agent_close_warnings did not include terminal ${termId} (got ${JSON.stringify(warnings)})`);
  if (warn.agent_kind !== "claude_code")
    return fail(`close warning agent_kind=${warn.agent_kind}, expected claude_code`);
  if (typeof warn.message !== "string" || !/Claude Code/.test(warn.message))
    return fail(`close warning message did not name the agent (got ${JSON.stringify(warn.message)})`);
  console.log(
    `[smoke-mcp-full-surface] agent_close_warnings → NON-empty, (${warn.terminal_id}, ${warn.agent_kind}) ` +
      `"${warn.message}" — the close dialog data source is ALIVE (no more [] stub) ✓`,
  );

  // 4e. The change invalidation that makes the front re-pull was emitted to the renderer.
  await new Promise((r) => setTimeout(r, 200));
  if (!agentSessionsChanged)
    return fail("no `agent-sessions://changed` event reached the renderer — the front would not re-pull");
  console.log("[smoke-mcp-full-surface] `agent-sessions://changed` emitted → front re-pulls → Claude icon shows ✓");

  await host.stop();
  fs.rmSync(pinnedDataDir, { recursive: true, force: true });
  console.log("[smoke-mcp-full-surface] OK — full MCP surface served, 0 unknown tools, list_commands fixed, Claude icon chain alive.");
  app.exit(0);
});

setTimeout(() => fail("timed out"), 40000);
