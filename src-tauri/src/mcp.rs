//! Local MCP HTTP server (PRD-4, ADR-0003).
//!
//! nyx hosts a SINGLE loopback HTTP server that exposes its MCP surface to local
//! agents (Claude Code & other MCP clients). This module owns the transport and
//! single-instance lifecycle decided in ADR-0003:
//!
//! - **D1 — transport**: HTTP, bind STRICTLY to `127.0.0.1`. `GET /health` for a
//!   liveness probe; `POST /mcp` for JSON-RPC 2.0 (`initialize`, `tools/list`,
//!   `tools/call`). No LAN / public exposure, no daemon.
//! - **D2 — port**: fixed & configurable, STABLE between launches. Default
//!   [`DEFAULT_PORT`], overridable via `NYX_MCP_PORT`. Ephemeral / port 0 are
//!   rejected — a stable port is what keeps an onboarded client's config valid
//!   across restarts.
//! - **D3 — single-instance**: at most ONE server per machine. nyx is already
//!   single-instance (`tauri-plugin-single-instance`), and [`McpServer::start`]
//!   is itself start-once: a second call is a no-op, never a second listener.
//! - **D9 — loopback security**: bound to loopback only; `Origin`/`Host` are
//!   checked to reject cross-origin browser requests (anti DNS-rebinding).
//!
//! The command tools (`start_command`, …) delegate to the PRD-3 runtime/DB layer
//! (ADR-0003 D6); their bodies land in phase 2. This phase-1 slice stands up the
//! server, the health probe, and the MCP handshake (`initialize` + `tools/list`
//! advertising the frozen v1 tool surface).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};

/// Default fixed MCP port (ADR-0003 D2). Above the ephemeral range, low collision
/// odds. Overridable via `NYX_MCP_PORT`, but STABLE between launches by default so
/// an onboarded client's `http://127.0.0.1:<port>/mcp` config stays valid.
pub const DEFAULT_PORT: u16 = 8765;

/// Env var that overrides [`DEFAULT_PORT`] (ADR-0003 D2). Must parse to a valid
/// non-zero `u16`; an invalid/zero value falls back to the default (port 0, i.e.
/// "pick any free port", is explicitly rejected so the port stays stable).
pub const PORT_ENV: &str = "NYX_MCP_PORT";

/// The `probe` spike tool name (PRD-4 #7, ADR-0004). A trivial no-op tool used to
/// validate that a Claude Code `SessionStart` hook of type `mcp_tool` can reach
/// nyx's MCP surface. It is NOT part of the frozen v1 surface ([`V1_TOOLS`]) — it is
/// a diagnostic/liveness probe, advertised in `tools/list` alongside the v1 tools so
/// a hook can discover and call it, but kept separate so it can be removed/renamed
/// without touching the v1 contract.
pub const PROBE_TOOL: &str = "probe";

/// The `wait_for_command` long-poll tool name (PRD-4 dogfood, review
/// `01KV91ZY1K8VCFQ44PQ5631WK0`, ADR-0003 D12). A BOUNDED long-poll that returns at
/// the first of (a) the instance's state entering the caller's `until` set, or (b)
/// `timeout_ms` elapsing. Like [`PROBE_TOOL`] it is advertised in `tools/list` but is
/// deliberately NOT part of the frozen v1 surface ([`V1_TOOLS`]): it is a purely
/// observational convenience layered over the same runner/db read paths the v1 tools
/// use, so it can evolve without touching the v1 contract clients are onboarded to.
pub const WAIT_FOR_COMMAND_TOOL: &str = "wait_for_command";

/// The `add_command` tool name (PRD-4 dogfood, review `01KV9614CHC4092P05DV9R5KPG`,
/// finding `01KV9615DM7DN2DV0D0XGKJGVW`, ADR-0003 D13). Creates a per-project command
/// TEMPLATE via the SAME path as the UI's `bridge::command_create`. Like [`PROBE_TOOL`]
/// / [`WAIT_FOR_COMMAND_TOOL`] it is advertised in `tools/list` but kept OUT of the
/// frozen v1 surface ([`V1_TOOLS`]): the mutating command-CRUD extension can evolve
/// without touching the v1 read/lifecycle contract clients are onboarded to.
pub const ADD_COMMAND_TOOL: &str = "add_command";

/// The `update_command` tool name (same finding / D13). Modifies an existing template's
/// editable fields (`name`/`command`/`subfolder`) via the SAME path — including the
/// package.json source-detach rule — as the UI's `bridge::command_update`. Advertised
/// in `tools/list`, kept out of [`V1_TOOLS`] for the SAME reason as `add_command`.
pub const UPDATE_COMMAND_TOOL: &str = "update_command";

/// The `import_commands` tool name (same finding / D13). Imports a project workspace's
/// `package.json` scripts as templates, reusing the EXISTING import logic
/// (`pkgjson::discover_package_scripts` + `pkgjson::import_command`) — the SAME path as
/// the UI's `command_import_scripts`/`command_import_create`. Advertised in
/// `tools/list`, kept out of [`V1_TOOLS`] for the SAME reason as `add_command`.
pub const IMPORT_COMMANDS_TOOL: &str = "import_commands";

/// The `remove_workspace` tool name (review 01KV9CWK A2). Deletes a workspace by id,
/// guarding against live running instances and cascading to command instances. Advertised
/// in `tools/list`, kept out of [`V1_TOOLS`] (same pattern as CRUD tools).
pub const REMOVE_WORKSPACE_TOOL: &str = "remove_workspace";

/// The `remove_command` tool name (review 01KV9CWK A2). Deletes a command TEMPLATE and
/// its instances, guarding against running instances. Must NOT be called with an
/// `instance_id` — it operates on templates (`command_id`). Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`] (same pattern as CRUD tools).
pub const REMOVE_COMMAND_TOOL: &str = "remove_command";

/// The `clear_command_output` tool name (PRD-4 review R-OUTPUT). Clears an instance's
/// captured output BUFFER (current + retained prior run) so an idle instance's stale
/// scrollback does not stay attached indefinitely, delegating to the PRD-3 runner
/// buffer and emitting the refresh event so the UI output panel reflects the clear. The
/// factual run outcome is left intact. Advertised in `tools/list`, kept out of
/// [`V1_TOOLS`] (same pattern as the other extension tools).
pub const CLEAR_COMMAND_OUTPUT_TOOL: &str = "clear_command_output";

/// The `list_importable_scripts` tool name (PRD-4 review R-IMPORT #5). Surfaces the
/// FILTERED, monorepo-aware import-discovery preview — the discoverable package.json
/// scripts (name, package, script_name, body, command) WITHOUT creating any template.
/// The read-only companion to `import_commands(preview:true)`. Advertised in
/// `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the other extension tools).
pub const LIST_IMPORTABLE_SCRIPTS_TOOL: &str = "list_importable_scripts";

/// The `remove_commands` tool name (PRD-4 review R-IMPORT #5). GROUPED deletion of
/// command TEMPLATES by id — the batch mirror of `remove_command`, so a mass import can
/// be undone in one call. Returns the removed count + per-id acks. Advertised in
/// `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the other extension tools).
pub const REMOVE_COMMANDS_TOOL: &str = "remove_commands";

/// The `create_terminal` tool name (PRD-4 review R-TERM). Creates an INTERACTIVE terminal:
/// a terminal record (the front mounts an xterm + spawns the PTY on the `terminals://changed`
/// reconciliation), optionally auto-attached to a workspace via a `cwd`, and optionally
/// running a `command` at opening (injected once via the front's PTY, then the terminal stays
/// interactive). Advertised in `tools/list`, kept out of [`V1_TOOLS`] (same pattern as the
/// other extension tools — it reuses the existing terminal/PTY primitives, no second
/// lifecycle).
pub const CREATE_TERMINAL_TOOL: &str = "create_terminal";

/// The `send_to_terminal` tool name (PRD-4 review R-TERM). Writes a command + newline into an
/// already-open terminal (resolved by its terminal id → live PTY) via the existing `pty_write`
/// path; the output streams back through `pty://output` as usual. Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`].
pub const SEND_TO_TERMINAL_TOOL: &str = "send_to_terminal";

/// The `list_terminals` tool name (PRD-4 review R-TERM). Lists the OPEN (alive) terminals with
/// their terminal id, cwd, label, workspace and the live terminal↔PTY id mapping, so an agent
/// knows what it can write to. Read-only. Advertised in `tools/list`, kept out of [`V1_TOOLS`].
pub const LIST_TERMINALS_TOOL: &str = "list_terminals";

/// The `close_terminal` tool name (PRD-4 review R-TERM). Closes a terminal by id, wrapping the
/// existing `close_terminal` record helper (record → closed) + `pty_close` (kill the PTY) and
/// emitting `terminals://changed` so the front retires the pane. Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`].
pub const CLOSE_TERMINAL_TOOL: &str = "close_terminal";

/// The `read_terminal` tool name (PRD-4.1 task #1). Reads the BOUNDED tail of a terminal's
/// scrollback by terminal id — symmetric to `get_command_output`, so an agent that
/// `send_to_terminal`-ed can now read the result. It reuses the EXISTING front-serialized
/// scrollback (the blob the xterm `SerializeAddon` persists via `db::persist_scrollback`); it
/// introduces NO second buffer and NO backend PTY capture. Reads reflect the LAST front-persisted
/// (debounced) scrollback, so a read immediately after `send_to_terminal` may be slightly behind —
/// a documented trade-off of reusing the front serializer, not a bug. Advertised in `tools/list`,
/// kept out of [`V1_TOOLS`] (same pattern as the other interactive-terminal tools).
pub const READ_TERMINAL_TOOL: &str = "read_terminal";

/// The `agent_session_event` tool name (PRD-5 #4, ADR-0004 / ADR-0010). The channel
/// a Claude Code `SessionStart`/`SessionEnd` hook of type `mcp_tool` reaches nyx on:
/// it is addressed `mcp__nyx__agent_session_event` and carries the agent's raw hook
/// payload (`hook_event_name`, `session_id`, `cwd`, `transcript_path`, `source`,
/// `NYX_TERMINAL_ID`). nyx normalizes it through the `claude_code` adapter and
/// upserts/ends the matching `agent_sessions` row. Advertised in `tools/list`, kept
/// OUT of [`V1_TOOLS`] (same pattern as `probe` and the other extension tools: it is
/// a PRD-5 capability layered over the existing DB, not part of the frozen contract
/// clients onboard against). Like `probe` it answers even with no managed runtime —
/// it just reports `mcp_unavailable` so the best-effort hook degrades cleanly.
pub const AGENT_SESSION_EVENT_TOOL: &str = "agent_session_event";

/// The frozen MCP v1 tool surface (ADR-0003). Phase-1 advertises these names in
/// the `tools/list` handshake; the call bodies are wired in phase 2 over the PRD-3
/// runtime/DB layer (D6). Order is the ADR's listing order.
pub const V1_TOOLS: [&str; 9] = [
    "list_projects",
    "list_workspaces",
    "list_commands",
    "start_command",
    "stop_command",
    "relaunch_command",
    "get_command_output",
    "workspace_add",
    "create_workspace",
];

/// Reported MCP protocol/server version in the `initialize` handshake.
const SERVER_NAME: &str = "nyx";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// MCP protocol revision nyx speaks (date-versioned, per the MCP spec).
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Server-level guidance returned in the `initialize` result's `instructions`
/// field (per the MCP spec). This is the FIRST thing a connecting agent reads, so
/// it states — in plain, consumer-facing terms — what nyx is and when/why to reach
/// for it, before the agent even looks at the tool list. Kept concise and free of
/// internal/dev jargon.
const SERVER_INSTRUCTIONS: &str = "\
nyx is a local terminal and development cockpit running on the user's machine. It \
organizes the user's coding projects into projects and workspaces (a workspace is a \
folder on disk), and it launches and supervises long-running development commands \
(dev servers, build watchers, test runners, and the like), capturing their output.

Use nyx's tools to:
- Discover what the user is working on: list their projects, the workspaces inside a \
project, and the commands available in (or running for) a workspace.
- Launch, stop, and relaunch a managed command, then read its captured output to see \
what happened (startup logs, errors, a dev server's URL, test results).
- Register or create a workspace folder when the user wants nyx to track a new one.

Reach for nyx whenever a request is about the user's local projects or about running, \
inspecting, or controlling development commands on their machine. Commands are \
identified by an `instance_id` (a specific launchable command in a workspace) — use \
that to start, stop, relaunch, or read output. Output reads are incremental: a result \
returns a `cursor` you can pass back to fetch only what is new.

Do NOT use nyx when:
- The task is a one-off shell command unrelated to a registered project or workspace \
(e.g. \"run echo hello\" or a quick file rename with no nyx project context) — run it \
directly in the shell instead.
- The user wants to create a new project or add it to nyx for the first time — that \
is a UI gesture done in the nyx application itself; nyx tools operate on projects that \
already exist in nyx.";

/// Resolve the MCP port (ADR-0003 D2): `NYX_MCP_PORT` when it parses to a non-zero
/// `u16`, else [`DEFAULT_PORT`]. Port 0 ("any free port") is rejected so the port
/// is stable between launches.
pub fn resolve_port() -> u16 {
    match std::env::var(PORT_ENV) {
        Ok(raw) => raw.trim().parse::<u16>().ok().filter(|p| *p != 0).unwrap_or(DEFAULT_PORT),
        Err(_) => DEFAULT_PORT,
    }
}

/// Managed Tauri state: the single, loopback-bound MCP server.
///
/// `start`-once is enforced by `started`: the first [`McpServer::start`] binds the
/// listener and spawns the accept loop; any later call is a no-op (ADR-0003 D3).
/// Combined with nyx's process-level single-instance, there is at most one server.
pub struct McpServer {
    /// Latches `true` on the first successful [`McpServer::start`]; guards against a
    /// second listener within the same process.
    started: AtomicBool,
    /// The port the listener actually bound (0 until started). Read by tests/health.
    bound_port: AtomicU16,
    /// The accept-loop thread handle, kept so the server's lifetime is tied to the
    /// process and the thread can be joined on shutdown if needed.
    handle: Mutex<Option<JoinHandle<()>>>,
    /// The tool dispatcher invoked for `tools/call`. `None` until phase-2 wires the
    /// PRD-3-backed tools; the handshake (`initialize`/`tools/list`) works without it.
    dispatcher: Mutex<Option<Arc<dyn ToolDispatcher>>>,
}

impl Default for McpServer {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            bound_port: AtomicU16::new(0),
            handle: Mutex::new(None),
            dispatcher: Mutex::new(None),
        }
    }
}

/// The phase-2 extension point: dispatches a `tools/call` to the PRD-3-backed tool
/// implementations (ADR-0003 D6). Phase-1 leaves it unset; `tools/call` then
/// returns a `method not yet available` error rather than a second lifecycle.
pub trait ToolDispatcher: Send + Sync + 'static {
    /// Invoke tool `name` with JSON `arguments`; return the tool's JSON result, or a
    /// [`RpcError`] (ADR-0003 D8 error vocabulary) on failure.
    fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError>;
}

/// A standardized MCP/JSON-RPC error (ADR-0003 D8). `code` is the stable string
/// vocabulary; `message` is human-readable; `data` is OPTIONAL structured detail
/// merged into the wire envelope's `error.data` ALONGSIDE the string `code` (e.g.
/// `output_too_large` carries `{ requested, limit }` so an agent can react without
/// parsing the prose). It must be a JSON object (its keys are merged in); a non-object
/// `data` is ignored.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: &'static str,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    /// Construct an error with the ADR-0003 D8 string `code` and a message.
    /// Phase-2 tool implementations build their failures through this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }

    /// Attach structured `data` (a JSON OBJECT) carried into the wire envelope's
    /// `error.data` alongside the string `code` (PRD-4.1 task #3). The object's keys are
    /// merged into `error.data` so e.g. `output_too_large` exposes machine-readable
    /// `requested`/`limit`. Builder form so existing `RpcError::new(...)` sites are
    /// unaffected.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
    /// JSON-RPC numeric code for the transport envelope. We keep all tool/domain
    /// errors as `-32000` (server error) and reserve the reserved range for
    /// protocol-level faults (parse / invalid request / method not found).
    fn rpc_code(&self) -> i64 {
        match self.code {
            "method_not_found" => -32601,
            "invalid_argument" => -32602,
            _ => -32000,
        }
    }
}

impl McpServer {
    /// Bind the loopback listener on the resolved port and spawn the accept loop.
    /// START-ONCE (ADR-0003 D3): the first call wins; a later call is a no-op and
    /// returns the already-bound port. Returns the bound port on success.
    ///
    /// Binds STRICTLY to `127.0.0.1` (ADR-0003 D1): the socket address is
    /// `Ipv4Addr::LOCALHOST`, never `0.0.0.0`, so the surface is unreachable off-box.
    pub fn start(self: &Arc<Self>) -> std::io::Result<u16> {
        // Latch the start: only the first caller proceeds to bind a listener.
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(self.bound_port.load(Ordering::SeqCst));
        }
        let port = resolve_port();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let server = match tiny_http::Server::http(addr) {
            Ok(s) => s,
            Err(e) => {
                // Unlatch so a later retry (e.g. after freeing the port) can bind.
                self.started.store(false, Ordering::SeqCst);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("MCP server could not bind 127.0.0.1:{port}: {e}"),
                ));
            }
        };
        // tiny_http picks the real port when several share a default; record what we
        // actually bound so health/onboarding reflect reality.
        let real_port = server
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .unwrap_or(port);
        self.bound_port.store(real_port, Ordering::SeqCst);

        let this = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name("nyx-mcp-http".into())
            .spawn(move || this.accept_loop(server))
            .expect("spawn nyx-mcp-http thread");
        *self.handle.lock().unwrap() = Some(handle);
        Ok(real_port)
    }

    /// Install the phase-2 tool dispatcher (ADR-0003 D6). Wired from the setup hook
    /// once the PRD-3 runtime/DB managed state exists.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_dispatcher(&self, dispatcher: Arc<dyn ToolDispatcher>) {
        *self.dispatcher.lock().unwrap() = Some(dispatcher);
    }

    /// The port the listener actually bound, or 0 if not started yet.
    pub fn bound_port(&self) -> u16 {
        self.bound_port.load(Ordering::SeqCst)
    }

    /// Whether the server has been started (latched).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// The blocking accept loop: serve each request on the calling thread. Requests
    /// are short synchronous JSON-RPC calls, so a single accept thread is adequate
    /// for the local cas nominal.
    fn accept_loop(self: Arc<Self>, server: tiny_http::Server) {
        for request in server.incoming_requests() {
            self.handle_request(request);
        }
    }

    /// Route one HTTP request: `GET /health`, `POST /mcp`, else 404. Enforces the
    /// loopback `Origin`/`Host` guard (ADR-0003 D9) before any MCP handling.
    fn handle_request(&self, mut request: tiny_http::Request) {
        let method = request.method().clone();
        let url = request.url().to_string();
        // Anti DNS-rebinding (D9): reject a cross-origin browser request before it
        // reaches the MCP surface. Non-browser clients send no Origin → allowed.
        if !origin_is_local(&request) {
            let _ = request.respond(text_response(403, "forbidden origin"));
            return;
        }
        let path = url.split('?').next().unwrap_or("/");
        match (method, path) {
            (tiny_http::Method::Get, "/health") => {
                let body = json!({
                    "status": "ok",
                    "server": SERVER_NAME,
                    "version": SERVER_VERSION,
                    "port": self.bound_port(),
                });
                let _ = request.respond(json_response(200, &body));
            }
            (tiny_http::Method::Post, "/mcp") => {
                let mut buf = String::new();
                if request.as_reader().read_to_string(&mut buf).is_err() {
                    let _ = request.respond(json_response(
                        400,
                        &rpc_error_envelope(Value::Null, -32700, "could not read request body"),
                    ));
                    return;
                }
                let response = self.handle_rpc(&buf);
                let _ = request.respond(json_response(200, &response));
            }
            _ => {
                let _ = request.respond(text_response(404, "not found"));
            }
        }
    }

    /// Handle one JSON-RPC 2.0 request body and produce the response value. Supports
    /// the MCP handshake (`initialize`, `tools/list`) and dispatches `tools/call` to
    /// the phase-2 dispatcher when installed.
    fn handle_rpc(&self, body: &str) -> Value {
        let parsed: Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(_) => return rpc_error_envelope(Value::Null, -32700, "parse error"),
        };
        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = parsed.get("method").and_then(Value::as_str) else {
            return rpc_error_envelope(id, -32600, "invalid request: missing method");
        };
        let params = parsed.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => rpc_result_envelope(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                    "instructions": SERVER_INSTRUCTIONS,
                }),
            ),
            // Notifications carry no id; acknowledge with an empty success envelope.
            "notifications/initialized" | "initialized" => {
                rpc_result_envelope(id, json!({}))
            }
            "ping" => rpc_result_envelope(id, json!({})),
            "tools/list" => rpc_result_envelope(id, json!({ "tools": tool_descriptors() })),
            "tools/call" => self.handle_tools_call(id, &params),
            other => rpc_error_envelope(
                id,
                -32601,
                &format!("method not found: {other}"),
            ),
        }
    }

    /// Dispatch a `tools/call`. Phase-1 has no dispatcher installed, so this returns
    /// a `method_not_found` error rather than inventing a second command lifecycle
    /// (ADR-0003 D6). Phase-2 installs the PRD-3-backed dispatcher.
    fn handle_tools_call(&self, id: Value, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() {
            return rpc_error_envelope(id, -32602, "invalid params: missing tool name");
        }
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let dispatcher = self.dispatcher.lock().unwrap().clone();
        match dispatcher {
            None => rpc_error_envelope(
                id,
                -32601,
                &format!("tool '{name}' not yet available (phase 2)"),
            ),
            Some(d) => match d.call(name, &arguments) {
                Ok(result) => rpc_result_envelope(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": result.to_string() }],
                        "structuredContent": result,
                        "isError": false,
                    }),
                ),
                Err(e) => {
                    let mut env = rpc_error_envelope(id, e.rpc_code(), &e.message);
                    // Carry the ADR-0003 D8 string code in `error.data.code`, plus any
                    // structured detail the error attached (e.g. output_too_large's
                    // `requested`/`limit`), merged in alongside `code` (PRD-4.1 task #3).
                    // We seed `data` from the attached object (when present) and stamp
                    // `code` LAST, so the stable string code can never be clobbered by a
                    // `data` key that happens to be named "code".
                    if let Some(err) = env.get_mut("error") {
                        let mut data = match e.data.as_ref().and_then(Value::as_object) {
                            Some(obj) => Value::Object(obj.clone()),
                            None => json!({}),
                        };
                        data["code"] = json!(e.code);
                        err["data"] = data;
                    }
                    env
                }
            },
        }
    }
}

/// Whether the request's `Origin`/`Host` is local (ADR-0003 D9). A missing Origin
/// (non-browser clients, incl. MCP CLIs) is allowed; a present Origin must point at
/// `localhost`/`127.0.0.1`. This blocks a malicious web page from driving the
/// loopback server via the user's browser (DNS-rebinding).
fn origin_is_local(request: &tiny_http::Request) -> bool {
    let origin = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Origin"))
        .map(|h| h.value.as_str().to_string());
    match origin {
        None => true,
        Some(o) => {
            let host = o
                .split("://")
                .nth(1)
                .unwrap_or(&o)
                .split('/')
                .next()
                .unwrap_or("");
            let host = host.split(':').next().unwrap_or("");
            host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
        }
    }
}

/// The `tools/list` descriptors for the frozen v1 surface (ADR-0003). Phase-2 fills
/// the per-tool argument schemas from the ADR's tool surface so a client can form
/// valid calls; the call bodies live in [`crate::mcp_tools`]. Every descriptor's
/// `name` stays in [`V1_TOOLS`] order; the schema mirrors each tool's `args` table.
fn tool_descriptors() -> Vec<Value> {
    // Reusable JSON-Schema fragments for the common id/string arguments.
    let str_prop = |desc: &str| json!({ "type": "string", "description": desc });
    let int_prop = |desc: &str| json!({ "type": "integer", "minimum": 0, "description": desc });
    // The per-run `env` map (R-WSCMD #7): an object of KEY→VALUE strings merged onto the
    // inherited environment for the launched process. Values are strings (env values).
    let env_prop = || json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": "optional per-run environment variables (KEY→VALUE string map) merged \
                        onto the inherited environment for this run — e.g. {\"VAULT_ENV\":\"dev\"} \
                        or values from a .env. Each value must be a string. Applied only to the \
                        process this call spawns; on a no-op start (already running) it is ignored.",
    });
    // Agent-facing one-liner per v1 tool: WHEN/WHY to use it, in plain terms. These
    // mirror the order of [`V1_TOOLS`].
    let description = |name: &str| -> &'static str {
        match name {
            "list_projects" => "List the user's projects in nyx. Start here to discover what the user is working on; each project groups one or more workspaces (folders on disk).",
            "list_workspaces" => "List the workspaces (on-disk folders) inside a project. Use after list_projects to find the workspace you want to inspect or run commands in.",
            "list_commands" => "List the development commands for a workspace (or the command templates for a project). Use the workspace form to get each command's `instance_id` — the id you pass to start/stop/relaunch/get_command_output. The project form returns reusable templates that are NOT launchable on their own.",
            "start_command" => "Start a managed command (e.g. a dev server, build watcher, or test run) so nyx supervises it and captures its output. Identify it by `instance_id` from list_commands, or by `name` within a `workspace_id`. Starting an already-running command is a no-op (it does NOT launch a second process): the result reports `was_running:true, restarted:false` so you can tell — use relaunch_command to actually restart. Pass an optional `env` map (KEY→VALUE strings, e.g. {\"VAULT_ENV\":\"dev\"}) to add environment variables for this run, merged onto the inherited environment. After starting, read its output with get_command_output to see logs, errors, or a server URL.",
            "stop_command" => "Stop a running managed command. Identify it by its `instance_id` from list_commands. The result reports `changed` (whether a live process was actually stopped) and `was_running`, so a stop on an already-idle command is a clear no-op rather than a false success.",
            "relaunch_command" => "Restart a managed command in place (stop then start the same instance) — useful after a config or code change. This ALWAYS restarts (unlike start_command, which no-ops on a running command); the result reports `restarted:true`. Identify it by its `instance_id` from list_commands. Pass an optional `env` map (KEY→VALUE strings) to set environment variables for the fresh run, merged onto the inherited environment.",
            "get_command_output" => "Read a managed command's captured output (logs, errors, a dev server's URL, test results). Identify the command by `instance_id`, or by `name` within a `workspace_id`. Returns a token-safe tail by default, with terminal color/control codes already stripped (set strip_ansi:false for raw bytes). Use `grep` to return only matching lines (e.g. errors) or `tail_lines` for the last N lines. Reads are incremental: the result includes a `cursor` you can pass back as `since` to fetch only new output on the next read. Oversize handling: a request whose window exceeds the 1 MiB ceiling fails with a structured error carrying `error.data.code=\"output_too_large\"` plus `error.data.requested`/`error.data.limit` (bytes) — detect that code and retry with a smaller `tail_bytes` rather than guessing.",
            "workspace_add" => "Register an EXISTING on-disk folder as a workspace under a project, so nyx tracks it and can run commands in it. The folder must already exist — a non-existent path or a file (not a directory) is rejected. Use create_workspace instead when nyx should CREATE the folder.",
            "create_workspace" => "CREATE a new folder on disk (mkdir -p, including any missing parents) and register it as a workspace under a project. Use when the folder does not exist yet and the user wants nyx to start tracking a brand-new working folder. To register a folder that already exists, use workspace_add (which does NOT create anything).",
            _ => "",
        }
    };
    let mut descriptors: Vec<Value> = V1_TOOLS
        .iter()
        .map(|name| {
            // Build the per-tool `(properties, required)` from the ADR-0003 surface.
            let (properties, required): (Value, Vec<&str>) = match *name {
                "list_projects" => (json!({}), vec![]),
                "list_workspaces" => (
                    json!({
                        "project_id": str_prop("project whose workspaces to list"),
                        "cwd": str_prop("optional cwd FILTER (does not resolve a current workspace)"),
                    }),
                    vec!["project_id"],
                ),
                "list_commands" => (
                    json!({
                        "workspace_id": str_prop("workspace whose command INSTANCES to list (nominal form): each row's instance_id is the LAUNCHABLE id for start/stop/relaunch/get_command_output"),
                        "project_id": str_prop("project whose command TEMPLATES to list (alternative): each row's command_id is a template id and is NOT launchable — use the instance_id from the workspace_id form to act on a command"),
                    }),
                    // Neither is universally required (one OR the other); the tool
                    // validates the at-least-one rule, so leave `required` empty here.
                    vec![],
                ),
                // start_command also accepts { name, workspace_id } to resolve the
                // instance by name (#16); stop/relaunch keep the instance_id-only form.
                "start_command" => (
                    json!({
                        "instance_id": str_prop("LAUNCHABLE command instance id (from list_commands(workspace_id=…)), NOT a template command_id"),
                        "name": str_prop("command name to resolve within workspace_id (alternative to instance_id; needs workspace_id; ambiguous/unknown → error)"),
                        "workspace_id": str_prop("workspace to resolve `name` in (required when using `name`)"),
                        "env": env_prop(),
                    }),
                    // instance_id OR (name + workspace_id); the tool validates.
                    vec![],
                ),
                "stop_command" => (
                    json!({ "instance_id": str_prop("LAUNCHABLE command instance id (from list_commands(workspace_id=…)), NOT a template command_id") }),
                    vec!["instance_id"],
                ),
                "relaunch_command" => (
                    json!({
                        "instance_id": str_prop("LAUNCHABLE command instance id (from list_commands(workspace_id=…)), NOT a template command_id"),
                        "env": env_prop(),
                    }),
                    vec!["instance_id"],
                ),
                "get_command_output" => (
                    json!({
                        "instance_id": str_prop("LAUNCHABLE command instance id whose output to read (NOT a template command_id)"),
                        "name": str_prop("command name to resolve within workspace_id (alternative to instance_id; needs workspace_id)"),
                        "workspace_id": str_prop("workspace to resolve `name` in (required when using `name`)"),
                        "tail_bytes": int_prop("how many bytes to return from the tail of the output \
                                               (default 12288, a token-safe window; max 1048576). Use this \
                                               for the typical case. Semantics: effective window = \
                                               min(tail_bytes, max_bytes)."),
                        "since": int_prop("byte offset from a previous `cursor` for incremental polling. If the buffer was cleared since (your `since` is past the new end) the response carries `reset:true` and returns the fresh output from the start instead of an empty window"),
                        "max_bytes": int_prop("alternative hard ceiling on the returned window \
                                              (default 1048576). If both tail_bytes and max_bytes are set, \
                                              the smaller wins. Prefer tail_bytes for normal use; max_bytes \
                                              is a safety guard. Either above 1048576 → output_too_large."),
                        "strip_ansi": json!({ "type": "boolean", "description": "when true (the DEFAULT), `output` is the window with ANSI/terminal control sequences stripped — a single readable field. Set false to get the raw bytes in `output` instead. `cursor`/`total_bytes` are byte-exact either way." }),
                        "mark_read": json!({ "type": "boolean", "description": "when true, this read ALSO acknowledges the command's unseen result — flipping `unread` to false EXACTLY as a UI acknowledge does, while the factual outcome (state/exit_code) is left intact. Default false: a passive (polling) read NEVER consumes the `unread` notification. Only meaningful for the current run (a `previous`-selector read targets a settled prior run with no live `unread`)." }),
                        "grep": str_prop("optional regular expression; when set, `output` contains only the lines matching it (matched on the ANSI-stripped text). Use it to pull just the error lines instead of the whole tail."),
                        "tail_lines": int_prop("optional: keep only the last N lines of the window (applied after `grep`). A line-based alternative to tail_bytes for \"show me the last 20 lines\"."),
                        "run": json!({ "description": "which run to read: 0/\"current\" (default, the latest run) or -1/\"previous\" (the one retained prior run, with its own exit_code/state). History is bounded to N=1; any other value is rejected. The result echoes `run`.", "oneOf": [ { "type": "integer" }, { "type": "string", "enum": ["current", "latest", "previous", "prev"] } ] }),
                    }),
                    // instance_id OR (name + workspace_id); the tool validates.
                    vec![],
                ),
                "workspace_add" => (
                    json!({
                        "project_id": str_prop("project to register the workspace in"),
                        "path": str_prop("path to an EXISTING on-disk folder (must already exist and be a directory; not created)"),
                        "name": str_prop("optional display name (defaults to the path's basename)"),
                    }),
                    vec!["project_id", "path"],
                ),
                "create_workspace" => (
                    json!({
                        "project_id": str_prop("project to create the workspace in"),
                        "name": str_prop("workspace display name"),
                        "path": str_prop("folder path to CREATE (mkdir -p, including missing parents) then register; use workspace_add if the folder already exists"),
                    }),
                    vec!["project_id", "name", "path"],
                ),
                // Defensive default: should be unreachable while V1_TOOLS is the
                // source of truth, but keep a valid object schema rather than panic.
                _ => (json!({}), vec![]),
            };
            json!({
                "name": name,
                "description": description(name),
                "inputSchema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            })
        })
        .collect();
    // Append the `probe` tool: a no-op health/connectivity check. Listed AFTER the
    // frozen v1 surface and kept out of [`V1_TOOLS`], so it never mutates the v1
    // contract clients are onboarded against.
    descriptors.push(json!({
        "name": PROBE_TOOL,
        "description": "Health/connectivity check: verifies nyx's MCP server is reachable. \
                        Returns { ok: true } with the server name and version, and has no \
                        side effects. Use it to confirm nyx is up before relying on the \
                        other tools.",
        "inputSchema": { "type": "object", "properties": {}, "required": [] },
    }));
    // Append the `wait_for_command` long-poll tool: a BOUNDED await of a command's
    // state change so an agent does not blind-poll. Listed AFTER the v1 surface and
    // kept out of [`V1_TOOLS`] for the SAME reason as `probe`: it is an observational
    // convenience, not a frozen-contract tool.
    descriptors.push(json!({
        "name": WAIT_FOR_COMMAND_TOOL,
        "description": "Wait for a managed command to reach a given state, instead of \
                        repeatedly polling. Blocks (up to timeout_ms, max ~60s) until the \
                        command instance's state enters `until`, then returns resolved:true; \
                        if the timeout elapses first it returns resolved:false (a normal \
                        outcome — call again to keep waiting). Read-only: it does not start, \
                        stop, or change the command. By default `output_tail` carries only \
                        the output produced AFTER this call (not the whole existing \
                        scrollback), with terminal control codes stripped. The returned \
                        `cursor` chains directly into get_command_output(since=cursor) so \
                        you can fetch the new output with no gap or duplication. Oversize \
                        handling: a tail_bytes/max_bytes request above the 1 MiB ceiling fails \
                        with a structured error carrying `error.data.code=\"output_too_large\"` \
                        plus `error.data.requested`/`error.data.limit` (bytes) — detect that code \
                        and retry with a smaller `tail_bytes` rather than guessing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "instance_id": str_prop("LAUNCHABLE command instance id to wait on (NOT a template command_id)"),
                "until": json!({
                    "type": "array",
                    "items": { "type": "string", "enum": ["idle", "running", "success", "error", "exited"] },
                    "description": "states that resolve the wait (default: settled states success+error). Runner vocabulary idle|running|success|error; \"exited\" is an alias for success|error.",
                }),
                "timeout_ms": int_prop("max wait in ms before resolved:false (default 30000, clamped to ~60000 max)"),
                "since": int_prop("byte offset from a previous get_command_output/wait_for_command `cursor`, so output_tail/cursor resume incrementally. OMIT on the first call: it defaults to the current end-of-buffer, so output_tail returns only output produced after this call (never the whole existing scrollback)."),
                "tail_bytes": int_prop("how many bytes of new output to return in output_tail (default 12288, a token-safe window; max 1048576)"),
                "max_bytes": int_prop("alternative hard ceiling on output_tail (default 1048576; the smaller of tail_bytes/max_bytes wins; either above 1048576 → output_too_large)"),
                "strip_ansi": json!({ "type": "boolean", "description": "when true (the DEFAULT), output_tail has ANSI/terminal control sequences stripped; set false for raw bytes. cursor is byte-exact either way." }),
            },
            "required": ["instance_id"],
        },
    }));
    // Append the command-CRUD extension (PRD-4 dogfood, review
    // `01KV9614CHC4092P05DV9R5KPG`, ADR-0003 D13): the MUTATING command tools an agent
    // needs to create/modify/import commands, which the read/lifecycle v1 surface lacks.
    // Listed AFTER the v1 surface and kept OUT of [`V1_TOOLS`] for the SAME reason as
    // `probe`/`wait_for_command`: they delegate to the existing PRD-3 layer (no parallel
    // logic) and can evolve without touching the frozen contract clients onboard against.
    descriptors.push(json!({
        "name": ADD_COMMAND_TOOL,
        "description": "Create a per-project command TEMPLATE (name + command line, \
                        optional run subfolder), via the SAME path as the UI's \
                        command_create — one instance is materialized per existing \
                        workspace of the project. A command line that is itself a package \
                        manager invocation (`pnpm dev`, …) has its provenance inferred. \
                        Returns the created template. A name already used in the project → \
                        invalid_state.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project to create the command template in"),
                "name": str_prop("command display name (must be unique within the project)"),
                "command": str_prop("the command line to run"),
                "subfolder": str_prop("optional run path relative to the workspace (default: workspace root)"),
            },
            "required": ["project_id", "name", "command"],
        },
    }));
    descriptors.push(json!({
        "name": UPDATE_COMMAND_TOOL,
        "description": "Modify an existing command TEMPLATE's editable fields (name, \
                        command, subfolder), via the SAME path as the UI's command_update. \
                        Only the fields supplied are changed; omitted fields keep their \
                        current value (pass subfolder:\"\" to clear it). Editing the command \
                        of a package.json-linked template away from its canonical call \
                        DETACHES the source (same rule as the UI). Refused while any of the \
                        template's instances is running (invalid_state).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command_id": str_prop("TEMPLATE id to modify (the command_id from list_commands(project_id=…))"),
                "name": str_prop("new display name (optional; unchanged if omitted)"),
                "command": str_prop("new command line (optional; unchanged if omitted)"),
                "subfolder": str_prop("new run subfolder (optional; unchanged if omitted; empty string clears it to the workspace root)"),
            },
            "required": ["command_id"],
        },
    }));
    descriptors.push(json!({
        "name": IMPORT_COMMANDS_TOOL,
        "description": "Import a project's package.json scripts as command templates, \
                        reusing the SAME discovery + import logic as the UI \
                        (command_import_scripts/command_import_create). Scans the project's \
                        workspace(s) with a FILTERED, monorepo-aware walk (skips \
                        node_modules, hidden dotdirs, and .gitignored paths; uses the root \
                        workspaces manifest globs when present, else a bounded depth) and \
                        creates a template per script (npm/pnpm/yarn/bun runner command, \
                        source provenance linked). Scripts whose proposed name is already \
                        used in the project are skipped (reason:\"already_exists\"). Returns \
                        { imported, skipped, manifests_found, preview }; manifests_found:0 \
                        (plus a skipped entry reason:\"no_manifest\") means no package.json \
                        was found — distinct from \"all already imported\". Provide \
                        project_id (scans every workspace) or workspace_id (scans that one). \
                        Use the optional `names` array to import only specific scripts \
                        (matched by the raw script name OR its proposed name, e.g. \
                        [\"dev\", \"build\"]); a requested name that matches nothing is \
                        reported in skipped with reason:\"not_found\". Pass preview:true to \
                        LIST the discoverable scripts (name, package, script_name, body, \
                        command) WITHOUT creating any template (see also \
                        list_importable_scripts).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project whose workspaces to scan for package.json scripts"),
                "workspace_id": str_prop("a single workspace to scan (alternative to project_id)"),
                "names": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "optional list of script names to import (e.g. [\"dev\", \"build\"]). \
                                   A name matches a script by its raw script_name OR its proposed name \
                                   (so [\"build\"] matches a build script in every package). Scripts not \
                                   requested are silently bypassed; a requested name that matches no \
                                   discovered script is returned in skipped with reason:\"not_found\". \
                                   Absent or null = import all discovered scripts.",
                }),
                "preview": json!({
                    "type": "boolean",
                    "description": "when true, list the discoverable scripts (name, package, \
                                   script_name, body, command) WITHOUT creating any template — a \
                                   dry-run. Default false (create the templates).",
                }),
            },
            // project_id OR workspace_id; the tool validates the at-least-one rule.
            "required": [],
        },
    }));
    // Append remove_workspace + remove_command (A2): the D of CRUD — advertised
    // alongside the v1 surface, kept OUT of V1_TOOLS for the same reason as the other
    // CRUD tools.
    descriptors.push(json!({
        "name": REMOVE_WORKSPACE_TOOL,
        "description": "Remove a workspace from nyx. Deletes the workspace and its command \
                        instances permanently. Terminals attached to the workspace are \
                        detached (they become unattached terminals). Refused if any command \
                        in the workspace is currently running — stop the commands first. \
                        This action cannot be undone.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace_id": str_prop("id of the workspace to remove (from list_workspaces)"),
            },
            "required": ["workspace_id"],
        },
    }));
    descriptors.push(json!({
        "name": REMOVE_COMMAND_TOOL,
        "description": "Remove a command template from nyx. Deletes the template and all its \
                        workspace instances permanently. Refused if any instance is currently \
                        running — stop the command first. Pass the `command_id` from \
                        list_commands(project_id=…), NOT an instance_id. This action cannot \
                        be undone.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command_id": str_prop("TEMPLATE id to remove (from list_commands(project_id=…)), NOT an instance_id"),
            },
            "required": ["command_id"],
        },
    }));
    // Append clear_command_output (review R-OUTPUT): reset a command instance's captured
    // output buffer. Advertised alongside the v1 surface, kept OUT of V1_TOOLS like the
    // other extension tools.
    descriptors.push(json!({
        "name": CLEAR_COMMAND_OUTPUT_TOOL,
        "description": "Clear a command instance's captured output (its scrollback log), \
                        so the next get_command_output starts from an empty buffer. Useful \
                        to reset a long-running instance's accumulated log before watching \
                        for fresh output. Identify the command by its `instance_id` from \
                        list_commands. This clears only the captured TEXT — it does not \
                        stop, relaunch, or change the command, and the last run's result \
                        (success/error, exit code) is preserved.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "instance_id": str_prop("LAUNCHABLE command instance id whose output buffer to clear (from list_commands(workspace_id=…)), NOT a template command_id"),
            },
            "required": ["instance_id"],
        },
    }));
    // Append list_importable_scripts + remove_commands (review R-IMPORT #5): the import
    // preview surface and the grouped-delete mirror of remove_command. Advertised
    // alongside the v1 surface, kept OUT of V1_TOOLS like the other extension tools.
    descriptors.push(json!({
        "name": LIST_IMPORTABLE_SCRIPTS_TOOL,
        "description": "List the package.json scripts that could be imported as command \
                        templates, WITHOUT creating anything (a read-only preview). Uses the \
                        SAME filtered, monorepo-aware discovery as import_commands (skips \
                        node_modules, hidden dotdirs, and .gitignored paths; honors the root \
                        workspaces manifest globs when present). Each entry has the proposed \
                        `name`, its `package` (subfolder, \"\" = root), the raw `script_name`, \
                        the script `body`, the runner `command` an import would create, and \
                        the detected `package_manager`. Also returns `manifests_found` \
                        (0 means no package.json was found). Provide project_id (scans every \
                        workspace) or workspace_id (scans that one). Use this to decide what \
                        to import, then call import_commands with the chosen `names`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": str_prop("project whose workspaces to scan for importable scripts"),
                "workspace_id": str_prop("a single workspace to scan (alternative to project_id)"),
            },
            // project_id OR workspace_id; the tool validates the at-least-one rule.
            "required": [],
        },
    }));
    descriptors.push(json!({
        "name": REMOVE_COMMANDS_TOOL,
        "description": "Remove several command TEMPLATES at once (the grouped mirror of \
                        remove_command), so a mass import can be undone in a single call. \
                        Pass `command_ids`: an array of template ids (the command_id from \
                        list_commands(project_id=…), NOT instance_ids). Each is deleted with \
                        its workspace instances (permanently). A template that is currently \
                        running in any workspace is REFUSED for that id (stop it first) but \
                        does not block the others. Returns `removed` (count actually deleted) \
                        and `results` (a per-id ack with removed:true/false and, on failure, \
                        an error). This action cannot be undone.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command_ids": json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "the TEMPLATE ids to remove (each a command_id from \
                                   list_commands(project_id=…), NOT an instance_id).",
                }),
            },
            "required": ["command_ids"],
        },
    }));
    // Append the interactive-terminal extension (PRD-4 review R-TERM): create / write to /
    // list / close an interactive terminal in nyx. Advertised alongside the v1 surface,
    // kept OUT of V1_TOOLS like the other extension tools. They reuse the existing terminal
    // + PTY primitives (no second terminal lifecycle).
    descriptors.push(json!({
        "name": CREATE_TERMINAL_TOOL,
        "description": "Open a new interactive terminal in nyx (a real shell the user can see \
                        and type into). Pass an optional `cwd` (a folder path): when it sits \
                        inside a known workspace the terminal is attached to that workspace and \
                        filed under it in the sidebar, otherwise it opens as a loose terminal at \
                        that directory. Pass an optional `command` to run a command the moment \
                        the shell opens (it is typed in and executed, and the terminal stays \
                        interactive afterwards) — omit it for a bare shell. The terminal's \
                        output streams live in nyx; use list_terminals to get its id, \
                        send_to_terminal to type more commands into it, and close_terminal to \
                        close it. Returns the new terminal's id.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "cwd": str_prop("optional working directory: if it is inside a known workspace the terminal is attached there, otherwise it opens loose at this directory (omitted = the default working directory, like a terminal the user opens by hand)"),
                "command": str_prop("optional command to run as soon as the terminal opens; the terminal stays interactive afterwards (omit for a bare shell)"),
                "label": str_prop("optional display label for the terminal in the sidebar"),
            },
            "required": [],
        },
    }));
    descriptors.push(json!({
        "name": SEND_TO_TERMINAL_TOOL,
        "description": "Run a command in an EXISTING open terminal: types the command plus a \
                        newline into the terminal so the shell executes it. Identify the \
                        terminal by its `terminal_id` from list_terminals. The command's output \
                        appears live in that terminal in nyx — read it there. Use create_terminal \
                        first if no suitable terminal is open.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the open terminal to run the command in (from list_terminals)"),
                "command": str_prop("the command line to run (a newline is appended so the shell executes it)"),
            },
            "required": ["terminal_id", "command"],
        },
    }));
    descriptors.push(json!({
        "name": LIST_TERMINALS_TOOL,
        "description": "List the terminals in nyx, each with its `terminal_id` \
                        (the id you pass to send_to_terminal / close_terminal / read_terminal), its \
                        working \
                        directory, label, and workspace (if attached). Each entry also reports its \
                        `status` (\"alive\" or \"closed\"), whether it is `live` (its shell has fully \
                        started and can accept input) and its internal `pty_id`. By default only \
                        OPEN terminals are listed; pass `include_closed:true` to ALSO list closed \
                        ones — a closed terminal can no longer be written to, but read_terminal \
                        still returns its last saved scrollback, so this is how you rediscover a \
                        finished terminal's id to read its output. Read-only.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "include_closed": { "type": "boolean", "description": "when true, also list CLOSED terminals (each carries status:\"closed\"); default false lists only open terminals" },
            },
            "required": [],
        },
    }));
    descriptors.push(json!({
        "name": CLOSE_TERMINAL_TOOL,
        "description": "Close an open terminal by id: ends its shell and removes it from the \
                        nyx sidebar. Identify it by its `terminal_id` from list_terminals. This \
                        cannot be undone (the terminal's live session is gone), but it does not \
                        affect any other terminal.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the open terminal to close (from list_terminals)"),
            },
            "required": ["terminal_id"],
        },
    }));
    descriptors.push(json!({
        "name": READ_TERMINAL_TOOL,
        "description": "Read the recent output (scrollback) of a terminal by its `terminal_id` \
                        — the read counterpart to send_to_terminal. Get the id from list_terminals \
                        (use list_terminals with include_closed:true to also find CLOSED \
                        terminals) or from the create_terminal response (which you can keep \
                        to read a terminal even AFTER it closes). Returns \
                        a bounded tail of the terminal's text (~12 KB by default, ANSI colors \
                        stripped), so after send_to_terminal you can read what the command \
                        produced. Tune the window with `tail_bytes` / `max_bytes`, set \
                        `strip_ansi:false` for raw output, and (for steady line output) page with \
                        `since` (pass back the previous call's `cursor`). NOTE: the read reflects \
                        the LAST \
                        scrollback nyx persisted for the terminal, which is updated a moment after \
                        new output appears (debounced by the UI); a read immediately after \
                        send_to_terminal may be slightly behind — retry to see the latest. A \
                        closed terminal still returns its last saved scrollback as long as nyx \
                        remembers it; an unknown id is an error. INCREMENTAL READS ARE BEST-EFFORT: \
                        scrollback is the re-serialized terminal grid, not an append-only log, so a \
                        resize/reflow, a full-screen (alt-screen) app, a `clear`, or eviction can \
                        rewrite it; an incremental read pages forward without dropping the head of \
                        a burst, and if the buffer shrank below your cursor the result carries \
                        `reset:true` with the fresh content (re-read WITHOUT `since` to resync). \
                        FIDELITY: normal command output \
                        (line-based echo) is reproduced faithfully; output from a full-screen \
                        (alt-screen) TUI app is rendered by cursor positioning rather than literal \
                        spaces, so reading it with strip_ansi may COALESCE runs of spaces (e.g. \
                        adjacent words may join) — not a bug, just the limit of serializing a TUI \
                        grid; pixel-perfect TUI capture is out of scope. Oversize handling: a \
                        tail_bytes/max_bytes request above the 1 MiB ceiling fails with a \
                        structured error carrying `error.data.code=\"output_too_large\"` plus \
                        `error.data.requested`/`error.data.limit` (bytes) — detect that code and \
                        retry with a smaller `tail_bytes` rather than guessing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "terminal_id": str_prop("id of the terminal to read (from list_terminals)"),
                "tail_bytes": int_prop("how many bytes from the END of the scrollback to return (default 12288, a token-safe window). Capped by max_bytes; above 1048576 → output_too_large"),
                "max_bytes": int_prop("alternative hard ceiling on the returned window (default 1048576; the smaller of tail_bytes/max_bytes wins; either above 1048576 → output_too_large)"),
                "since": int_prop("byte offset to resume from (pass back a previous call's `cursor` to read only what is new); omit to read the tail. BEST-EFFORT for terminals — reliable for steady line output, but a reflow/clear can rewrite the scrollback; check `reset` in the response and re-read without `since` if it is true"),
                "strip_ansi": { "type": "boolean", "description": "strip ANSI escape sequences from the returned output (default true). false returns the raw bytes" },
            },
            "required": ["terminal_id"],
        },
    }));
    // Append the agent-session-event channel (PRD-5 #4, ADR-0004 / ADR-0010): the MCP
    // tool a Claude Code SessionStart/SessionEnd hook calls so nyx can capture/end the
    // agent session bound to a terminal. Advertised alongside the v1 surface, kept OUT
    // of V1_TOOLS for the SAME reason as `probe`: it is a PRD-5 capability over the
    // existing DB, not part of the frozen contract clients onboard against. The schema
    // accepts the raw Claude hook fields directly so a `mcp_tool` hook can forward its
    // input verbatim.
    descriptors.push(json!({
        "name": AGENT_SESSION_EVENT_TOOL,
        "description": "Report an agent session lifecycle event to nyx (used by the nyx Claude \
                        Code plugin's SessionStart/SessionEnd hooks, not normally called by \
                        hand). Pass the hook's fields — `hook_event_name` (SessionStart or \
                        SessionEnd), `session_id`, and on start `cwd`, `transcript_path`, \
                        `source` — plus the `NYX_TERMINAL_ID` of the terminal the agent runs in \
                        so nyx can bind the session to that terminal. nyx records the session so \
                        it can be resumed later; SessionEnd marks it cleanly ended.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "hook_event_name": str_prop("the Claude hook event: \"SessionStart\" or \"SessionEnd\""),
                "session_id": str_prop("the agent's own session id (Claude session_id) — what resume is built from"),
                "NYX_TERMINAL_ID": str_prop("the nyx terminal record id the agent runs in (exported into the shell by nyx); used to correlate the session to a terminal"),
                "cwd": str_prop("working directory of the session (SessionStart)"),
                "transcript_path": str_prop("path to the agent transcript file (SessionStart, optional)"),
                "source": str_prop("SessionStart source: startup | resume | clear (optional)"),
                "agent_kind": str_prop("which agent this is (default \"claude_code\")"),
            },
            "required": ["hook_event_name", "session_id"],
        },
    }));
    descriptors
}

/// Build a JSON-RPC 2.0 success envelope.
fn rpc_result_envelope(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC 2.0 error envelope.
fn rpc_error_envelope(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A `200`/`4xx` JSON response for tiny_http.
fn json_response(status: u16, body: &Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static content-type header");
    tiny_http::Response::from_data(bytes)
        .with_status_code(status)
        .with_header(header)
}

/// A plain-text response for tiny_http (errors / 404).
fn text_response(status: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body).with_status_code(status)
}

#[cfg(test)]
mod tests {
    //! Loopback integration tests: drive a REAL `McpServer` over a TCP socket on
    //! `127.0.0.1`, exercising the health probe, the MCP handshake, single-instance
    //! start-once, and port stability/configurability.

    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Once;
    use std::time::Duration;

    /// Serialize the env-var mutations (`NYX_MCP_PORT`) across tests in this binary,
    /// since the process environment is shared. Each test that sets the var clears
    /// it before/after under this guard.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Send a raw HTTP request to `127.0.0.1:<port>` and return the (status_line,
    /// body) pair. A tiny hand-rolled client so the test needs no HTTP client dep.
    fn http_request(port: u16, method: &str, path: &str, body: Option<&str>) -> (String, String) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let body = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status_line = head.lines().next().unwrap_or("").to_string();
        (status_line, body.to_string())
    }

    /// Start a server on a deterministic free-but-fixed test port and wait until it
    /// accepts connections, so the tests don't race the accept loop's bind.
    fn start_on_port(port: u16) -> Arc<McpServer> {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(PORT_ENV, port.to_string());
        let server = Arc::new(McpServer::default());
        let bound = server.start().expect("server starts");
        std::env::remove_var(PORT_ENV);
        assert_eq!(bound, port, "must bind the configured port");
        // Spin until the listener is actually accepting.
        for _ in 0..100 {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        server
    }

    #[test]
    fn resolve_port_defaults_and_honors_env() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var(PORT_ENV);
        assert_eq!(resolve_port(), DEFAULT_PORT, "default when unset");
        std::env::set_var(PORT_ENV, "19001");
        assert_eq!(resolve_port(), 19001, "honors a valid override");
        // Port 0 ("any free port") is rejected → stays stable on the default.
        std::env::set_var(PORT_ENV, "0");
        assert_eq!(resolve_port(), DEFAULT_PORT, "rejects port 0 for stability");
        std::env::set_var(PORT_ENV, "not-a-port");
        assert_eq!(resolve_port(), DEFAULT_PORT, "rejects garbage");
        std::env::remove_var(PORT_ENV);
    }

    #[test]
    fn binds_loopback_and_serves_health() {
        let server = start_on_port(19011);
        let (status, body) = http_request(19011, "GET", "/health", None);
        assert!(status.contains("200"), "health 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["port"], 19011);
        assert!(server.is_started());
    }

    #[test]
    fn mcp_initialize_handshake() {
        start_on_port(19012);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let (status, body) = http_request(19012, "POST", "/mcp", Some(req));
        assert!(status.contains("200"), "initialize 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(v["result"]["protocolVersion"].is_string());
    }

    #[test]
    fn initialize_returns_consumer_facing_instructions() {
        // The handshake must carry a non-empty, consumer-facing `instructions` string
        // (MCP spec initialize result) telling an agent what nyx is and when to use it,
        // and it must be free of internal/dev jargon.
        start_on_port(19021);
        let req = r#"{"jsonrpc":"2.0","id":9,"method":"initialize","params":{}}"#;
        let (status, body) = http_request(19021, "POST", "/mcp", Some(req));
        assert!(status.contains("200"), "initialize 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        let instructions = v["result"]["instructions"]
            .as_str()
            .expect("initialize result carries an `instructions` string");
        assert!(!instructions.trim().is_empty(), "instructions must be non-empty");
        // Consumer-facing content: states what nyx is and points at its core verbs.
        let lc = instructions.to_lowercase();
        assert!(lc.contains("nyx"), "instructions name the product");
        assert!(
            lc.contains("project") && lc.contains("workspace") && lc.contains("command"),
            "instructions cover nyx's core concepts (projects/workspaces/commands)"
        );
        assert_no_internal_jargon("instructions", instructions);
    }

    /// Internal/dev jargon markers that must never appear in agent-facing strings
    /// (the `instructions` and every advertised tool description). These are words
    /// written for nyx developers, not the consuming agent.
    ///
    /// NOTE: `SessionStart`/`SessionEnd` are deliberately NOT blacklisted — they are
    /// Claude Code's PUBLIC hook event names, i.e. the literal `hook_event_name`
    /// values an agent forwards to the PRD-5 `agent_session_event` tool. Naming them
    /// in that tool's description is agent-facing guidance, not internal dev-speak.
    fn assert_no_internal_jargon(what: &str, text: &str) {
        let lc = text.to_lowercase();
        for marker in ["prd", "spike", "adr-", "dogfood", "phase-2", "phase 2"] {
            assert!(
                !lc.contains(marker),
                "{what} leaks internal jargon marker {marker:?}: {text}"
            );
        }
    }

    #[test]
    fn advertised_tool_descriptions_are_agent_facing() {
        // Every advertised tool MUST carry a non-empty description, free of internal
        // jargon (no PRD / spike / SessionStart / ADR / dogfood / phase-2 dev-speak),
        // while preserving the useful instance_id-vs-command_id guidance for the agent.
        start_on_port(19022);
        let req = r#"{"jsonrpc":"2.0","id":10,"method":"tools/list"}"#;
        let (_status, body) = http_request(19022, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        let tools = v["result"]["tools"].as_array().expect("tools array");
        for tool in tools {
            let name = tool["name"].as_str().expect("tool name");
            let desc = tool["description"]
                .as_str()
                .unwrap_or_else(|| panic!("tool {name} must advertise a description"));
            assert!(!desc.trim().is_empty(), "tool {name} description must be non-empty");
            assert_no_internal_jargon(&format!("tool {name} description"), desc);
        }
        // The instance_id vs command_id guidance is preserved (in agent terms): the
        // tools that act on an instance mention instance_id, and list_commands explains
        // command templates are not launchable.
        let desc_of = |n: &str| -> String {
            tools
                .iter()
                .find(|t| t["name"] == n)
                .and_then(|t| t["description"].as_str())
                .unwrap_or("")
                .to_lowercase()
        };
        assert!(
            desc_of("start_command").contains("instance_id"),
            "start_command keeps the instance_id guidance"
        );
        assert!(
            desc_of("list_commands").contains("instance_id"),
            "list_commands explains where instance_id comes from"
        );
        // D2: the two workspace tools must CONTRAST clearly — workspace_add registers an
        // existing folder, create_workspace creates a new one. Each names its own
        // intent and points at the sibling so an agent picks the right one.
        let add = desc_of("workspace_add");
        let create = desc_of("create_workspace");
        assert!(
            add.contains("existing") && add.contains("create_workspace"),
            "workspace_add says it registers an EXISTING folder and points at create_workspace, got: {add}"
        );
        assert!(
            create.contains("create") && create.contains("workspace_add"),
            "create_workspace says it CREATES a folder and points at workspace_add, got: {create}"
        );
    }

    #[test]
    fn tools_list_advertises_v1_surface() {
        start_on_port(19013);
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let (_status, body) = http_request(19013, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        let tools = v["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for expected in V1_TOOLS {
            assert!(names.contains(&expected), "v1 tool {expected} must be listed");
        }
        // The frozen v1 surface, plus the spike `probe` tool (ADR-0004) and the
        // `wait_for_command` long-poll (ADR-0003 D12) advertised alongside — both
        // kept OUT of V1_TOOLS so the frozen contract is untouched, yet discoverable.
        assert!(names.contains(&PROBE_TOOL), "probe spike tool must be listed");
        assert!(
            names.contains(&WAIT_FOR_COMMAND_TOOL),
            "wait_for_command long-poll tool must be listed"
        );
        // The command-CRUD extension (ADR-0003 D13): mutating tools advertised alongside
        // the v1 surface, kept OUT of V1_TOOLS like probe/wait_for_command.
        assert!(names.contains(&ADD_COMMAND_TOOL), "add_command tool must be listed");
        assert!(names.contains(&UPDATE_COMMAND_TOOL), "update_command tool must be listed");
        assert!(
            names.contains(&IMPORT_COMMANDS_TOOL),
            "import_commands tool must be listed"
        );
        // A2: remove tools advertised alongside CRUD.
        assert!(
            names.contains(&REMOVE_WORKSPACE_TOOL),
            "remove_workspace tool must be listed"
        );
        assert!(
            names.contains(&REMOVE_COMMAND_TOOL),
            "remove_command tool must be listed"
        );
        // R-OUTPUT: the clear_command_output buffer-reset tool advertised alongside.
        assert!(
            names.contains(&CLEAR_COMMAND_OUTPUT_TOOL),
            "clear_command_output tool must be listed"
        );
        // R-IMPORT #5: the import-preview + grouped-delete tools advertised alongside.
        assert!(
            names.contains(&LIST_IMPORTABLE_SCRIPTS_TOOL),
            "list_importable_scripts tool must be listed"
        );
        assert!(
            names.contains(&REMOVE_COMMANDS_TOOL),
            "remove_commands tool must be listed"
        );
        // R-TERM: the interactive-terminal tools advertised alongside, kept OUT of V1_TOOLS.
        assert!(names.contains(&CREATE_TERMINAL_TOOL), "create_terminal tool must be listed");
        assert!(
            names.contains(&SEND_TO_TERMINAL_TOOL),
            "send_to_terminal tool must be listed"
        );
        assert!(names.contains(&LIST_TERMINALS_TOOL), "list_terminals tool must be listed");
        assert!(names.contains(&CLOSE_TERMINAL_TOOL), "close_terminal tool must be listed");
        // PRD-4.1 #1: the read_terminal scrollback-read tool advertised alongside, kept OUT of V1_TOOLS.
        assert!(names.contains(&READ_TERMINAL_TOOL), "read_terminal tool must be listed");
        // PRD-5 #4 (ADR-0004): the agent-session channel advertised alongside, kept OUT of V1_TOOLS.
        assert!(
            names.contains(&AGENT_SESSION_EVENT_TOOL),
            "agent_session_event tool must be listed"
        );
        assert_eq!(
            names.len(),
            V1_TOOLS.len() + 16,
            "exactly the v1 surface plus probe + wait_for_command + \
             add_command/update_command/import_commands + remove_workspace/remove_command + \
             clear_command_output + list_importable_scripts/remove_commands + \
             create_terminal/send_to_terminal/list_terminals/close_terminal/read_terminal + \
             agent_session_event extension tools"
        );
    }

    #[test]
    fn tools_call_without_dispatcher_is_method_not_found() {
        start_on_port(19014);
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_projects","arguments":{}}}"#;
        let (_status, body) = http_request(19014, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["code"], -32601, "no dispatcher → method not found");
    }

    #[test]
    fn dispatcher_routes_tools_call() {
        // A recording dispatcher proves the phase-2 extension point is wired and
        // that a tool result is wrapped in the MCP `tools/call` envelope.
        struct Echo;
        impl ToolDispatcher for Echo {
            fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
                Ok(json!({ "tool": name, "args": arguments }))
            }
        }
        let server = start_on_port(19015);
        server.set_dispatcher(Arc::new(Echo));
        let req = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_projects","arguments":{"k":1}}}"#;
        let (_status, body) = http_request(19015, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["isError"], false);
        assert_eq!(v["result"]["structuredContent"]["tool"], "list_projects");
        assert_eq!(v["result"]["structuredContent"]["args"]["k"], 1);
    }

    #[test]
    fn tools_call_error_carries_named_d8_code_in_error_data() {
        // Review R-OUTPUT task #3: a tool error must surface its NAMED D8 code in the
        // structured `error.data.code` field so an agent can distinguish e.g.
        // `output_too_large` from any other server error — while the numeric JSON-RPC
        // `error.code` stays -32000 (the server-error envelope). A tiny dispatcher
        // returns each D8 code; we assert both fields over the REAL loopback wire.
        struct D8Errors;
        impl ToolDispatcher for D8Errors {
            fn call(&self, name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                // The tool name doubles as the D8 code to emit, so one dispatcher
                // covers every code in the vocabulary.
                let code: &'static str = match name {
                    "output_too_large" => "output_too_large",
                    "invalid_id" => "invalid_id",
                    "invalid_state" => "invalid_state",
                    "mcp_unavailable" => "mcp_unavailable",
                    "internal" => "internal",
                    _ => "invalid_argument",
                };
                Err(RpcError::new(code, format!("synthetic {code}")))
            }
        }
        let server = start_on_port(19023);
        server.set_dispatcher(Arc::new(D8Errors));

        let call_err = |id: u32, name: &str| -> Value {
            let req = format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#
            );
            let (_status, body) = http_request(19023, "POST", "/mcp", Some(&req));
            serde_json::from_str::<Value>(&body).unwrap()
        };

        // get_command_output over the cap returns error.data.code = output_too_large,
        // numeric code -32000 (server error), per the task's primary done_criterion.
        let v = call_err(1, "output_too_large");
        assert_eq!(
            v["error"]["data"]["code"], "output_too_large",
            "the named D8 code is on the wire in error.data.code"
        );
        assert_eq!(
            v["error"]["code"], -32000,
            "the numeric JSON-RPC code stays -32000 for a domain error"
        );

        // Every other D8 code surfaces its name in error.data.code too. invalid_argument
        // keeps the JSON-RPC -32602 numeric code (it is a params fault) but still names
        // its D8 code in data; the rest are -32000.
        for (name, numeric) in [
            ("invalid_id", -32000),
            ("invalid_state", -32000),
            ("mcp_unavailable", -32000),
            ("internal", -32000),
            ("invalid_argument", -32602),
        ] {
            let v = call_err(2, name);
            assert_eq!(
                v["error"]["data"]["code"], name,
                "{name} must surface in error.data.code"
            );
            assert_eq!(v["error"]["code"], numeric, "{name} numeric JSON-RPC code");
        }
    }

    #[test]
    fn tools_call_error_data_merges_structured_detail_alongside_code() {
        // PRD-4.1 #3: an RpcError's structured `data` (e.g. output_too_large's
        // `requested`/`limit`) is merged into the wire `error.data` ALONGSIDE the string
        // `code`, so an agent can react to size info without parsing the prose message.
        struct OversizeWithData;
        impl ToolDispatcher for OversizeWithData {
            fn call(&self, _name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                Err(RpcError::new("output_too_large", "requested window exceeds max_bytes (1048576)")
                    .with_data(json!({ "requested": 2_000_000, "limit": 1_048_576 })))
            }
        }
        let server = start_on_port(19031);
        server.set_dispatcher(Arc::new(OversizeWithData));
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_terminal","arguments":{}}}"#;
        let (_status, body) = http_request(19031, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        // The string code is still present (not clobbered by the merge)...
        assert_eq!(v["error"]["data"]["code"], "output_too_large", "code stays on error.data");
        // ...and the structured detail is merged in alongside it.
        assert_eq!(v["error"]["data"]["requested"], 2_000_000, "requested is on the wire");
        assert_eq!(v["error"]["data"]["limit"], 1_048_576, "limit is on the wire");
        assert_eq!(v["error"]["code"], -32000, "numeric JSON-RPC code unchanged");
    }

    #[test]
    fn tools_call_error_data_code_is_never_clobbered_by_attached_data() {
        // Defense in depth: even if an error attaches a `data` object that itself
        // carries a key named "code", the canonical ADR-0003 D8 string code is stamped
        // LAST and wins — an agent dispatching on `error.data.code` is never misled.
        struct CodeKeyInData;
        impl ToolDispatcher for CodeKeyInData {
            fn call(&self, _name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                Err(RpcError::new("invalid_argument", "bad input")
                    .with_data(json!({ "code": "ATTACKER_OVERRIDE", "field": "x" })))
            }
        }
        let server = start_on_port(19032);
        server.set_dispatcher(Arc::new(CodeKeyInData));
        let req = r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"probe","arguments":{}}}"#;
        let (_status, body) = http_request(19032, "POST", "/mcp", Some(req));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["error"]["data"]["code"], "invalid_argument",
            "the canonical string code wins over a `code` key in attached data"
        );
        assert_eq!(v["error"]["data"]["field"], "x", "other attached keys still merge through");
    }

    #[test]
    fn probe_tool_round_trips_over_loopback() {
        // The spike's loopback proof (PRD-4 #7, ADR-0004): drive `tools/call probe`
        // over a REAL HTTP socket — exactly the JSON-RPC a Claude Code SessionStart
        // `mcp_tool` hook would send — and confirm the `{ ok: true }` no-op result is
        // wrapped in the MCP envelope. A tiny in-test dispatcher reproduces the real
        // `NyxToolDispatcher::probe` body (a no-op needing no managed state), so this
        // test stays free of the Tauri runtime while proving the transport path.
        struct ProbeOnly;
        impl ToolDispatcher for ProbeOnly {
            fn call(&self, name: &str, _arguments: &Value) -> Result<Value, RpcError> {
                if name == PROBE_TOOL {
                    Ok(json!({ "ok": true, "server": SERVER_NAME, "version": SERVER_VERSION }))
                } else {
                    Err(RpcError::new("method_not_found", format!("unknown tool '{name}'")))
                }
            }
        }
        let server = start_on_port(19019);
        server.set_dispatcher(Arc::new(ProbeOnly));
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"probe","arguments":{}}}"#;
        let (status, body) = http_request(19019, "POST", "/mcp", Some(req));
        assert!(status.contains("200"), "probe call 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["isError"], false);
        assert_eq!(v["result"]["structuredContent"]["ok"], true);
        assert_eq!(v["result"]["structuredContent"]["server"], SERVER_NAME);
    }

    #[test]
    fn agent_session_event_round_trips_over_loopback() {
        // PRD-5 #4 (ADR-0004): drive `tools/call agent_session_event` over a REAL
        // 127.0.0.1 HTTP socket — exactly the JSON-RPC a Claude Code SessionStart
        // `mcp_tool` hook (`mcp__nyx__agent_session_event`) sends, carrying the hook
        // payload (`hook_event_name`, `session_id`, `cwd`, `source`, `NYX_TERMINAL_ID`)
        // as the tool `arguments` — and confirm the captured-session result is wrapped
        // in the MCP envelope. A tiny in-test dispatcher echoes the captured fields so
        // this proves the TRANSPORT path (the same wire the real hook uses); the
        // SAME tool against the real `NyxToolDispatcher` + a live DB is proven in
        // `mcp_tools::tests::session_start_creates_active_row`. Together they cover the
        // loopback transport here and the DB-backed capture there.
        struct SessionChannel;
        impl ToolDispatcher for SessionChannel {
            fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
                if name == "agent_session_event" {
                    // Echo the correlation + capture the hook sent (the shape the real
                    // tool persists), proving the arguments reached the dispatcher.
                    Ok(json!({
                        "event": arguments.get("hook_event_name"),
                        "terminal_id": arguments.get("NYX_TERMINAL_ID"),
                        "external_session_id": arguments.get("session_id"),
                        "state": "active",
                    }))
                } else {
                    Err(RpcError::new("method_not_found", format!("unknown tool '{name}'")))
                }
            }
        }
        let server = start_on_port(19024);
        server.set_dispatcher(Arc::new(SessionChannel));
        // The EXACT JSON-RPC a SessionStart `mcp_tool` hook would send.
        let req = r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"agent_session_event","arguments":{"hook_event_name":"SessionStart","session_id":"claude-sid-1","cwd":"/work/proj","source":"startup","NYX_TERMINAL_ID":"term-xyz"}}}"#;
        let (status, body) = http_request(19024, "POST", "/mcp", Some(req));
        assert!(status.contains("200"), "session event call 200, got {status}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["isError"], false);
        let sc = &v["result"]["structuredContent"];
        assert_eq!(sc["event"], "SessionStart");
        assert_eq!(sc["terminal_id"], "term-xyz", "NYX_TERMINAL_ID round-trips as the correlation key");
        assert_eq!(sc["external_session_id"], "claude-sid-1");
        assert_eq!(sc["state"], "active");
    }

    #[test]
    fn mcp_command_lifecycle_round_trips_over_loopback() {
        // PRD-4 phase-5 GATE (#8), the TRANSPORT half of done_criterion #1
        // ("E2E ou dogfood prouve list/start/relaunch/output"): drive the full
        // command lifecycle — `list_commands` → `start_command` →
        // `relaunch_command` → `get_command_output` — as `tools/call` JSON-RPC
        // over a REAL `127.0.0.1` HTTP socket (the exact wire an agent in nyx
        // uses), and confirm each tool's request/result round-trips inside the MCP
        // envelope. A tiny stateful dispatcher stands in for the Tauri runtime so
        // this test stays free of `tauri::test`; the SAME lifecycle against the
        // REAL `NyxToolDispatcher` + a live `ManagedCommandRunner` + DB is proven
        // in `bridge::tests::mcp_dogfood_lifecycle_is_the_same_instance_the_ui_sees`.
        // Together they cover both seams the gate asks for: the loopback transport
        // here, the same-state-as-the-UI invariant there.
        struct Lifecycle {
            // The single instance's last_state, mutated by start/relaunch so the
            // round-trip reflects a real state transition across calls.
            state: Mutex<String>,
        }
        impl ToolDispatcher for Lifecycle {
            fn call(&self, name: &str, arguments: &Value) -> Result<Value, RpcError> {
                let id = arguments
                    .get("instance_id")
                    .and_then(Value::as_str)
                    .unwrap_or("inst-1");
                match name {
                    "list_commands" => {
                        let st = self.state.lock().unwrap().clone();
                        Ok(json!({ "commands": [
                            { "instance_id": "inst-1", "command": "echo HELLO", "last_state": st }
                        ] }))
                    }
                    "start_command" | "relaunch_command" => {
                        *self.state.lock().unwrap() = "running".to_string();
                        Ok(json!({ "instance_id": id, "state": "running" }))
                    }
                    "get_command_output" => Ok(json!({
                        "instance_id": id,
                        "output": "HELLO\n",
                        "total_bytes": 6,
                        "returned_bytes": 6,
                        "truncated": false,
                        "cursor": 6,
                    })),
                    other => Err(RpcError::new(
                        "method_not_found",
                        format!("unknown tool '{other}'"),
                    )),
                }
            }
        }
        let server = start_on_port(19020);
        server.set_dispatcher(Arc::new(Lifecycle {
            state: Mutex::new("idle".to_string()),
        }));

        // Helper: POST one `tools/call` over the socket and return its
        // `structuredContent` (the tool's JSON result), asserting a clean envelope.
        let call = |id: u32, name: &str, args: &str| -> Value {
            let req = format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{args}}}}}"#
            );
            let (status, body) = http_request(19020, "POST", "/mcp", Some(&req));
            assert!(status.contains("200"), "{name} call 200, got {status}");
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["result"]["isError"], false, "{name} must not be an error");
            v["result"]["structuredContent"].clone()
        };

        // 1) list_commands — the instance is discoverable and idle before start.
        let listed = call(1, "list_commands", r#"{"workspace_id":"w1"}"#);
        assert_eq!(listed["commands"][0]["instance_id"], "inst-1");
        assert_eq!(listed["commands"][0]["last_state"], "idle");

        // 2) start_command — the instance goes running.
        let started = call(2, "start_command", r#"{"instance_id":"inst-1"}"#);
        assert_eq!(started["state"], "running");

        // 3) relaunch_command — the SAME instance, still one process, still running.
        let relaunched = call(3, "relaunch_command", r#"{"instance_id":"inst-1"}"#);
        assert_eq!(relaunched["instance_id"], "inst-1");
        assert_eq!(relaunched["state"], "running");

        // list now reflects the transition the prior calls drove (shared state).
        let listed2 = call(4, "list_commands", r#"{"workspace_id":"w1"}"#);
        assert_eq!(listed2["commands"][0]["last_state"], "running");

        // 4) get_command_output — the bounded window + integer cursor round-trips.
        let out = call(5, "get_command_output", r#"{"instance_id":"inst-1"}"#);
        assert_eq!(out["output"], "HELLO\n");
        assert!(out["cursor"].is_u64(), "cursor is an integer for incremental polling");
    }

    #[test]
    fn start_is_single_instance_no_op_on_second_call() {
        let server = start_on_port(19016);
        // A second start() on the SAME server is a no-op (latched): returns the same
        // bound port, does not bind a second listener.
        let again = server.start().expect("second start is a no-op");
        assert_eq!(again, 19016);

        // A SECOND McpServer trying to bind the SAME fixed port fails to bind (the
        // OS refuses the duplicate listener) — proving a second nyx would not create
        // a second server on the fixed port.
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(PORT_ENV, "19016");
        let second = Arc::new(McpServer::default());
        let result = second.start();
        std::env::remove_var(PORT_ENV);
        assert!(result.is_err(), "a second server cannot bind the in-use fixed port");
    }

    #[test]
    fn port_is_stable_across_restarts() {
        // Two sequential lifecycles with the same config resolve to the SAME port —
        // the property an onboarded client's config relies on.
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(PORT_ENV, "19017");
        let first = resolve_port();
        let second = resolve_port();
        std::env::remove_var(PORT_ENV);
        assert_eq!(first, second, "port resolution is stable for the same config");

        static ONCE: Once = Once::new();
        ONCE.call_once(|| { /* document: no global state leaks between resolves */ });
    }

    #[test]
    fn rejects_cross_origin_browser_request() {
        let port = 19018;
        start_on_port(port);
        // A browser Origin pointing off-localhost is rejected (D9, anti-rebinding).
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let req = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example.com\r\nConnection: close\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let status = raw.lines().next().unwrap_or("");
        assert!(status.contains("403"), "off-localhost Origin → 403, got {status}");

        // A localhost Origin is accepted.
        let (status_ok, _) = {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            let r = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://localhost:1420\r\nConnection: close\r\n\r\n";
            s.write_all(r.as_bytes()).unwrap();
            let mut raw2 = String::new();
            s.read_to_string(&mut raw2).unwrap();
            (raw2.lines().next().unwrap_or("").to_string(), ())
        };
        assert!(status_ok.contains("200"), "localhost Origin allowed, got {status_ok}");
    }
}
