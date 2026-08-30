//! PUSH-channel e2e (v2.5.8 agent harness, PRD 01KXC80E2CDSNN9PPMJFJFE3X7 / PDR-0020,
//! review 01KXEF1NJ0P2V5NCYV2QRYFSFS finding 01KXEFFTBEC6MC8S892NEFMJMN).
//!
//! The gate (`gate_v2_5_8_agent_harness_e2e.rs`) proves the harness STRUCTURALLY (the stub
//! DECLARES its `NYX-AGENT-CONTEXT role=… scope=…` sentinel) and via the universal PULL channel
//! (`agent_context` renders the manual). It never exercises the REAL PUSH path — the branch of
//! the bundled `resources/claude-plugin/hooks/nyx-session-hook.cjs` that, on a `UserPromptSubmit`
//! invoking `/nspec` `/ndev` `/ndoc`, POSTs an `agent_context` `tools/call` to the sidecar and
//! relays the rendered manual back to Claude as `additionalContext`.
//!
//! THE PAYLOAD IS THE CONTRACT (v2.8 finding F1). A `UserPromptSubmit` hook receives the prompt
//! the human TYPED, never the expanded command body: Claude Code hands the hook
//! `{"hook_event_name":"UserPromptSubmit","cwd":…,"prompt":"/nspec feature-42 add backpressure"}`
//! — captured verbatim from a live run. The first version of this branch keyed on the stub's
//! sentinel, i.e. on text that NEVER reaches a hook, so the injection was dead from the day it
//! shipped while this test passed on a fabricated payload. Every prompt fed to the hook below is
//! now a RAW invocation string, exactly as Claude Code sends it.
//!
//! THE HOOK OPENS THE RUN (v2.8 G6). The session's first act must be `nyx_next(run_id)`: a live
//! `/nspec` used to spend two calls before its first tick (`agent_context` to fetch its own manual,
//! then `run_start` to open its own run), re-resolving anchors the hook already receives and leaving
//! "who opens the run" to whichever agent got there first. So the hook now POSTs `run_start
//! {scope,cwd}` ITSELF and injects the resulting `run_id` — together with `run_start`'s `warnings`,
//! which are written for a human to act on — in the SAME `additionalContext` as the manual.
//!
//! This PERSISTENT test spawns the ACTUAL hook `.cjs` under `node` (the hook's own runtime —
//! Claude Code IS node) against an in-process mock sidecar (a bare `std::net::TcpListener`
//! speaking just enough HTTP/JSON-RPC) and asserts:
//!   * a nyx command → the hook POSTs `run_start {scope,cwd}` then `agent_context {role,scope,
//!     run_id}` carrying the (role, scope) the matching stub DECLARES, and emits the run_id AND the
//!     returned manual in ONE `additionalContext` — with NOT ONE byte on stderr (cheap and silent
//!     on the happy path);
//!   * `run_start`'s `warnings` reach that same block, told to be relayed to the user;
//!   * a plain prompt → nothing injected, neither tool called, stderr empty;
//!   * every non-injecting outcome leaves its OWN one-line trace on stderr — a foreign slash
//!     command, an unreachable sidecar, a REFUSED `run_start` (which injects NOTHING, never a
//!     manual with no run), a refused `agent_context`, an answer with no context. That silence is
//!     what let the dead injection survive a release, so it is itself under test;
//!   * the OTHER relay, and the only other one the hook has: a `SessionStart` whose
//!     `agent_session_event` answers with the AMBIENT context — the `You're in nyx — …` orientation
//!     line plus the workspace's managed-command block — emits it as `additionalContext` BYTE FOR
//!     BYTE (multi-line, `·` separators, accents, `group_id` included), and an answer carrying no
//!     context injects nothing and still exits 0. Every other leg here is a `UserPromptSubmit`; this
//!     branch of the `.cjs` was covered by nothing, which is exactly where a silent corruption of
//!     the injected block would have lived.
//!
//! NOTHING REPLAYS THIS FILE AUTOMATICALLY — run it by hand with
//! `cargo test -p nyx-core --test hook_push_injection_e2e` (review finding
//! `01M152SCJ8QHK5C2P002GWZAGA`). Neither `bun run test:core` nor the cross-OS gate workflow
//! covers it: both invoke `cargo test --workspace --lib`, and `--lib` builds only the library
//! targets, excluding every integration binary under `crates/nyx-core/tests/` — this one included.
//! Wiring a declared command and a CI job to these binaries is tracked separately; until that
//! lands, a regression here breaks in silence. If `node` is not on PATH the test SKIPS with a note
//! (node-agnostic by contract, mirroring how the sidecar gates skip their git legs when git is
//! absent) — but node is the hook's mandatory runtime.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

/// Path to the bundled hook script under test.
fn hook_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources/claude-plugin/hooks/nyx-session-hook.cjs")
}

/// The (role, scope) a nyx command stub DECLARES in its `<!-- NYX-AGENT-CONTEXT role=… scope=… -->`
/// sentinel (`resources/claude-commands/<file>`). The hook keys on the command NAME (all a
/// `UserPromptSubmit` hook ever receives), so the stub's declaration and the hook's table are two
/// artifacts that must agree: this is what binds them — a drift on either side fails here.
fn stub_sentinel(file: &str) -> (String, String) {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources/claude-commands")
        .join(file);
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read stub {p:?}: {e}"));
    let line = text
        .lines()
        .find(|l| l.contains("NYX-AGENT-CONTEXT"))
        .unwrap_or_else(|| panic!("{file} declares a NYX-AGENT-CONTEXT sentinel"));
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(key).map(str::to_string))
            .unwrap_or_else(|| panic!("{file}'s sentinel declares {key}…"))
    };
    (field("role="), field("scope="))
}

/// `true` when `node` is runnable — otherwise the test SKIPS (node-agnostic by contract).
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// What the mock answers the hook's two calls with — one variant per outcome the hook must tell
/// APART in its trace. `run_start` succeeds in every variant but [`Reply::RunStartRefused`], so the
/// `agent_context` legs below all get past the run opening.
#[derive(Clone, Copy, PartialEq)]
enum Reply {
    /// A run opened with no warnings, then the rendered manual (the happy path).
    Manual,
    /// A run opened WITH warnings — the human-facing signals `run_start` returns beside the id.
    RunWarnings,
    /// `run_start` itself refuses, as the real one does for a `/ndev` on a location anchoring no
    /// Spec. Nothing is injected AT ALL: `agent_context` is never even called.
    RunStartRefused,
    /// A JSON-RPC error, as the real sidecar returns when the tool refuses (no Spec resolves, an
    /// unknown role, or `agent_context` missing entirely on an older sidecar).
    Refused,
    /// A 200 result whose shape carries no `structuredContent.context` at all.
    NoContext,
    /// `agent_session_event` answers a `SessionStart` with the AMBIENT context — the orientation
    /// line PLUS the workspace's managed-command block. The other variants answer it with the bare
    /// `{}` a `UserPromptSubmit` gets, which is also the DEGRADED SessionStart case (nothing to
    /// inject).
    SessionContext,
}

/// The refusal wording the mock echoes — the REAL `agent_context` message for an unresolved Spec
/// (`mcp_context::agent_context`), so the test proves the SERVER's reason reaches the trace.
const REFUSAL: &str =
    "could not resolve a Spec: pass spec_id, or a workspace_id/cwd anchored on one";

/// The refusal wording the mock echoes for `run_start` — the REAL `mcp_engine::run_start` message
/// for a Spec-scoped flow whose location anchors none, which is EXACTLY what a `/ndev` typed
/// outside an anchored workspace produces. The hook has one anchor (the `cwd` Claude Code hands
/// it), so this refusal is a live failure mode, and the server's own sentence is the only text that
/// names which anchor was missing.
const RUN_START_REFUSAL: &str =
    "'ndev' works ON a Spec and none could be resolved — pass an explicit `spec_id` (or a \
     `cwd`/`workspace_id` anchoring exactly one active Spec)";

/// The AMBIENT session context the mock's `agent_session_event` answers a `SessionStart` with — the
/// shape `mcp_tools_core::session_context_line` really produces: the `You're in nyx — …` orientation
/// line, then the workspace's managed-command block, then the directive.
///
/// It is deliberately HOSTILE to a naive relay, because that is the point of this leg: it is
/// MULTI-LINE (the `.cjs` relays one JSON string; an implementation that split, trimmed or
/// line-joined would be caught here), it carries the `·` separator and accented text (non-ASCII, so
/// a byte-length `Content-Length` or a latin-1 read would corrupt it), and it names a `group_id`
/// (the token an agent needs to start a whole multi-pane group in ONE call).
const MOCK_SESSION_CONTEXT: &str = "You're in nyx — project \"Café\" (project_id=proj-café) · \
     workspace \"main\" (workspace_id=ws-1) · terminal_id=term-push-test\n\
     Managed commands of this workspace:\n\
     - dev · bun run dev · running · instance_id=inst-1 · group_id=grp-1\n\
     - api · bun run api --réglages=élevés · idle · instance_id=inst-2 · group_id=grp-1\n\
     - build · bun run build · success · instance_id=inst-3\n\
     +2 more commands (list_commands)\n\
     Prefer start_command over launching these in a raw shell: nyx then supervises the run.\n\
     start_command is a NO-OP on a command already running (was_running:true, restarted:false).";

/// The run the mock's `run_start` opens. A literal the assertions look for VERBATIM in the injected
/// text: the `run_id` is the one value every subsequent call takes, and the session now gets it
/// from nowhere else.
const MOCK_RUN_ID: &str = "run-01KMOCKRUNSTART0000000000";

/// The warnings the mock's `run_start` returns under [`Reply::RunWarnings`] — the two real codes,
/// with the real wordings shortened: a run DESTROYED because its graph changed under it, and a gate
/// that verifies nothing. Both are written for a human to act on, which is why they may not be
/// swallowed when the hook opens the run in the session's place.
const MOCK_WARNINGS: [(&str, &str); 2] = [
    (
        "run_replaced_graph_changed",
        "run run-01KDEAD was DROPPED, not resumed: its workflow's graph changed under it, so its \
         progress could not be picked up",
    ),
    (
        "gate_unconfigured",
        "gate_check 'Quality gate' has no command_ids — it runs vacuously GREEN and verifies nothing",
    ),
];

/// A minimal in-process mock of the nyx sidecar's `/mcp` JSON-RPC endpoint. It answers every
/// `agent_context` `tools/call` per its [`Reply`] mode (so the test can prove WHICH manual — or
/// which failure — flowed through) and records every call it receives, so the test can assert
/// which tools the hook did, and did not, invoke.
struct MockSidecar {
    port: u16,
    stop: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<Value>>>,
    handle: Option<thread::JoinHandle<()>>,
}

/// The manual the mock returns for `agent_context {role}` — role-tagged so the assertion
/// proves the round-trip (this exact string is what must reach `additionalContext`).
fn mock_manual(role: &str) -> String {
    format!("=== NYX MOCK MANUAL role={role} === injected-by-push")
}

impl MockSidecar {
    fn start() -> MockSidecar {
        MockSidecar::start_with(Reply::Manual)
    }

    fn start_with(reply: Reply) -> MockSidecar {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock sidecar");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let calls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let stop_t = stop.clone();
        let calls_t = calls.clone();
        let handle = thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        stream
                            .set_read_timeout(Some(Duration::from_millis(1000)))
                            .ok();
                        handle_conn(&mut stream, &calls_t, reply);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        MockSidecar {
            port,
            stop,
            calls,
            handle: Some(handle),
        }
    }

    /// The tool names the hook POSTed, in order (`agent_session_event`, then `run_start` and
    /// `agent_context` only when the injection branch fires).
    fn tool_names(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c["params"]["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// EVERY recorded call to `tool`, with its `arguments`, in order. `agent_session_event` is
    /// POSTed once for the turn and once more to FILE a failed injection, so the first call is not
    /// the whole story for it.
    fn calls_for(&self, tool: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["params"]["name"] == json!(tool))
            .map(|c| c["params"]["arguments"].clone())
            .collect()
    }

    /// The first recorded call to `tool`, with its `arguments` — `None` when the hook never made it.
    fn args_for(&self, tool: &str) -> Option<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c["params"]["name"] == json!(tool))
            .map(|c| c["params"]["arguments"].clone())
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

impl Drop for MockSidecar {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

/// Read one HTTP request off `stream`, record its JSON-RPC body, and answer the `run_start` /
/// `agent_context` calls per `reply` (any other tool gets a bare ok). Best-effort: a malformed
/// request is answered with an empty ok so the hook still resolves.
fn handle_conn(stream: &mut TcpStream, calls: &Arc<Mutex<Vec<Value>>>, reply: Reply) {
    let Some(body) = read_http_body(stream) else {
        return;
    };
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let name = parsed["params"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if parsed.is_object() {
        calls.lock().unwrap().push(parsed.clone());
    }
    let id = parsed["id"].clone();
    let payload = if name == "run_start" {
        match reply {
            Reply::RunStartRefused => json!({ "jsonrpc": "2.0", "id": id, "error": {
                "code": -32602, "message": RUN_START_REFUSAL
            }}),
            Reply::RunWarnings => json!({ "jsonrpc": "2.0", "id": id, "result": {
                "structuredContent": {
                    "run_id": MOCK_RUN_ID,
                    "warnings": MOCK_WARNINGS
                        .iter()
                        .map(|(code, message)| json!({ "code": code, "message": message }))
                        .collect::<Vec<Value>>(),
                }
            }}),
            _ => json!({ "jsonrpc": "2.0", "id": id, "result": {
                "structuredContent": { "run_id": MOCK_RUN_ID, "warnings": [] }
            }}),
        }
    } else if name == "agent_context" {
        match reply {
            // The run-opening variants all render the manual: what they vary is `run_start`.
            Reply::Manual | Reply::RunWarnings | Reply::RunStartRefused | Reply::SessionContext => {
                let role = parsed["params"]["arguments"]["role"]
                    .as_str()
                    .unwrap_or_default();
                json!({ "jsonrpc": "2.0", "id": id, "result": {
                    "structuredContent": { "context": mock_manual(role) }
                }})
            }
            Reply::Refused => json!({ "jsonrpc": "2.0", "id": id, "error": {
                "code": -32602, "message": REFUSAL
            }}),
            Reply::NoContext => json!({ "jsonrpc": "2.0", "id": id, "result": {
                "structuredContent": { "role": "dev" }
            }}),
        }
    } else if name == "agent_session_event" && reply == Reply::SessionContext {
        // The AMBIENT context a `SessionStart` gets back — the orientation line plus the workspace's
        // managed-command block. This is the ONLY thing the hook's SessionStart branch reads.
        json!({ "jsonrpc": "2.0", "id": id, "result": {
            "structuredContent": { "context": MOCK_SESSION_CONTEXT }
        }})
    } else {
        // agent_session_event (or anything else) — a bare ok; UserPromptSubmit ignores it, and a
        // SessionStart takes it as "nothing to inject".
        json!({ "jsonrpc": "2.0", "id": id, "result": { "structuredContent": {} } })
    }
    .to_string();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Read an HTTP request's body honoring `Content-Length` (the hook always sets it). Returns
/// `None` if the socket closes before a full request arrives.
fn read_http_body(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            let content_length = content_length(&buf[..pos]);
            let body_start = pos + 4;
            if buf.len() - body_start >= content_length {
                return Some(buf[body_start..body_start + content_length].to_vec());
            }
        }
        match stream.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => return None,
        }
    }
}

/// Case-insensitive `Content-Length` from a raw header block (0 when absent).
fn content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    for line in text.split("\r\n") {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                return v.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

/// First index of `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The hook's observable output: its exit code, its stdout (the injection) and its stderr (the
/// trace saying why there was none).
struct HookRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl HookRun {
    /// `hookSpecificOutput.additionalContext` from stdout, or `None` when the hook injected
    /// nothing (a plain / foreign / failed prompt).
    fn injected(&self) -> Option<String> {
        self.injected_for("UserPromptSubmit")
    }

    /// The same, for an arbitrary hook event — the injected block is TAGGED with the event it
    /// answers, and Claude Code drops one tagged with the wrong name, so the tag is asserted here
    /// rather than taken on faith.
    fn injected_for(&self, event: &str) -> Option<String> {
        let trimmed = self.stdout.trim();
        if trimmed.is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(trimmed).ok()?;
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            json!(event),
            "the injected block is tagged {event}",
        );
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .map(str::to_string)
    }

    /// The single trace line the hook left, asserting there is EXACTLY one (a trace is a
    /// diagnosis, not a log stream: one prompt, one reason).
    fn trace(&self) -> String {
        let lines: Vec<&str> = self
            .stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "exactly one trace line, got stderr: {:?}",
            self.stderr
        );
        assert!(
            lines[0].starts_with("nyx-hook UserPromptSubmit: no manual injected —"),
            "the trace is tagged and says it injected nothing: {:?}",
            lines[0]
        );
        lines[0].to_string()
    }
}

/// The hook events these legs drive, each with the payload field Claude Code actually stamps on it:
/// the human's raw `prompt` for a `UserPromptSubmit`, the `source` (`startup` | `resume` | `clear` |
/// `compact`) for a `SessionStart`. Modelled as an enum rather than a free-form JSON so a leg cannot
/// silently fabricate a field a real hook never receives — the F1 mistake this file exists to guard.
#[derive(Clone, Copy)]
enum HookEvent<'a> {
    UserPromptSubmit(&'a str),
    SessionStart(&'a str),
}

impl HookEvent<'_> {
    /// The `hook_event_name` (and argv[2]) this event is delivered under.
    fn name(self) -> &'static str {
        match self {
            HookEvent::UserPromptSubmit(_) => "UserPromptSubmit",
            HookEvent::SessionStart(_) => "SessionStart",
        }
    }

    /// The stdin payload Claude Code hands the hook for this event.
    fn payload(self, cwd: &str) -> Value {
        match self {
            HookEvent::UserPromptSubmit(prompt) => json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": "sess-push-test",
                "cwd": cwd,
                "prompt": prompt,
            }),
            HookEvent::SessionStart(source) => json!({
                "hook_event_name": "SessionStart",
                "session_id": "sess-push-test",
                "cwd": cwd,
                "source": source,
            }),
        }
    }
}

/// Run the REAL hook as `node nyx-session-hook.cjs UserPromptSubmit`, piping the given prompt in
/// the payload shape Claude Code sends, with the mock's port in the environment.
fn run_hook(prompt: &str, cwd: &str, port: u16) -> HookRun {
    run_hook_env(prompt, cwd, Some(port), None)
}

/// The same, with the two ways the hook can learn its port left open: `NYX_MCP_PORT` (what nyx
/// exports into the shells it spawns) and `CLAUDE_PLUGIN_ROOT` (the plugin dir whose `.mcp.json`
/// nyx templated its port into at install). `None` for both is a session that knows neither.
fn run_hook_env(prompt: &str, cwd: &str, port: Option<u16>, plugin_root: Option<&Path>) -> HookRun {
    run_hook_event(
        HookEvent::UserPromptSubmit(prompt),
        cwd,
        port,
        plugin_root,
    )
}

/// Run the REAL hook under `node` for ANY of the events above — same spawn, same env, same stdin
/// contract; only the payload and argv[2] change.
fn run_hook_event(
    event: HookEvent<'_>,
    cwd: &str,
    port: Option<u16>,
    plugin_root: Option<&Path>,
) -> HookRun {
    let input = event.payload(cwd).to_string();

    let mut cmd = Command::new("node");
    cmd.arg(hook_path())
        .arg(event.name())
        .env_remove("NYX_MCP_PORT")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env("NYX_TERMINAL_ID", "term-push-test");
    if let Some(port) = port {
        cmd.env("NYX_MCP_PORT", port.to_string());
    }
    if let Some(root) = plugin_root {
        cmd.env("CLAUDE_PLUGIN_ROOT", root);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn node hook");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .expect("write hook stdin");

    let out = child.wait_with_output().expect("wait hook");
    HookRun {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

/// A definitely-closed port (bind then drop) so the "sidecar down" leg gets ECONNREFUSED.
fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

/// A throwaway PLUGIN ROOT holding the `.mcp.json` nyx writes at install, with `port` templated
/// in — the shape [`resolvePort`] reads when the shell carries no `NYX_MCP_PORT`.
fn plugin_root_on(port: u16) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-hook-plugin-root-{port}"));
    std::fs::create_dir_all(&dir).expect("plugin root");
    std::fs::write(
        dir.join(".mcp.json"),
        json!({
            "nyx-v2": { "type": "http", "url": format!("http://127.0.0.1:{port}/mcp") }
        })
        .to_string(),
    )
    .expect("write .mcp.json");
    dir
}

// ---------------------------------------------------------------------------
// The PUSH contract.
// ---------------------------------------------------------------------------

/// The hook reaches THE nyx that installed it, even with no `NYX_MCP_PORT` in the shell — it reads
/// the port out of its own plugin's `.mcp.json`, the same file the MCP client uses.
///
/// Why this is not a nicety: nyx exports `NYX_MCP_PORT` only when the port was already bound as the
/// shell was spawned, so a terminal restored at boot can carry none. The literal fallback is 8765 —
/// which EVERY nyx defaults to, so on a machine running two of them (a released nyx and the one
/// being built) the hook would POST into the other application, get `unknown tool` for calls meant
/// for this one, and say nothing. Reading the sibling `.mcp.json` is what keeps the hook and the
/// MCP tools on the same nyx.
#[test]
fn push_reaches_the_nyx_that_installed_the_plugin_without_an_env_port() {
    if !node_available() {
        eprintln!("node unavailable: skipped the PUSH-injection e2e (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start();
    let root = plugin_root_on(mock.port);
    let run = run_hook_env(
        "/nspec feature-42 add backpressure",
        "/tmp/nyx-push-test-workspace",
        None, // no NYX_MCP_PORT: the shell knows nothing…
        Some(&root),
    );

    assert_eq!(run.code, 0, "the hook always exits 0");
    let injected = run.injected().unwrap_or_else(|| {
        panic!(
            "…yet the plugin's own .mcp.json says where to go: stdout {:?} stderr {:?}",
            run.stdout, run.stderr
        )
    });
    assert!(
        injected.contains(&mock_manual("dev")),
        "the manual was relayed"
    );
    assert!(
        injected.contains(MOCK_RUN_ID),
        "…with the run THIS mock opened: {injected}",
    );
    assert!(
        mock.tool_names().contains(&"agent_context".to_string()),
        "the calls landed on THIS mock — the port came from the plugin root",
    );
}

/// Invoking a nyx command makes the hook OPEN THE RUN (`run_start {scope,cwd}`) and POST
/// `agent_context {role,scope,run_id}`, then relay the run_id AND the returned manual in ONE
/// `additionalContext` — driven with the RAW invocation text Claude Code actually sends
/// (`/nspec feature-42 …`), and asserted against the (role, scope) each shipped stub DECLARES.
///
/// Two guards in one. The F1 one: it fails the day the hook keys on anything a `UserPromptSubmit`
/// payload does not carry. The G6 one: the session's FIRST act must be `nyx_next(run_id)`, so the
/// run has to be open and its id in the injected text before the agent reads a word — and the
/// `cwd` must reach `run_start`, NOT `agent_context`, because the run is the anchor from there on.
#[test]
fn push_opens_the_run_and_injects_its_id_with_the_manual() {
    if !node_available() {
        eprintln!("node unavailable: skipped the PUSH-injection e2e (node-agnostic by contract)");
        return;
    }
    let cwd = "/tmp/nyx-push-test-workspace";
    for (command, file) in [
        ("nspec", "nspec.md"),
        ("ndev", "ndev.md"),
        ("ndoc", "ndoc.md"),
    ] {
        let (role, scope) = stub_sentinel(file);
        let mock = MockSidecar::start();
        // The submitted prompt = what the human typed: the command, then their free text.
        let run = run_hook(
            &format!("/{command} feature-42 add backpressure"),
            cwd,
            mock.port,
        );
        assert_eq!(run.code, 0, "the hook always exits 0 (/{command})");

        // The injection actually fired: the mock's role-tagged manual reached additionalContext.
        let injected = run.injected().unwrap_or_else(|| {
            panic!(
                "/{command}: expected a manual injected, got stdout {:?} stderr {:?}",
                run.stdout, run.stderr
            )
        });
        assert!(
            injected.contains(&mock_manual(&role)),
            "/{command}: the hook relayed the manual for the declared role: {injected}",
        );
        // The run_id LEADS the injection and is spelled inside the call that consumes it: it is the
        // one value every later call takes, and the session has no other way to learn it.
        assert!(
            injected.starts_with("=== NYX RUN"),
            "/{command}: the injection OPENS on the run block: {injected}",
        );
        assert!(
            injected.contains(&format!("run_id: {MOCK_RUN_ID}"))
                && injected.contains(&format!(r#"nyx_next(run_id="{MOCK_RUN_ID}")"#)),
            "/{command}: the injected run block names the run to tick: {injected}",
        );
        assert!(
            run.stderr.trim().is_empty(),
            "/{command}: the happy path is silent, got stderr {:?}",
            run.stderr,
        );

        // It got there by OPENING the run with the stub-declared scope + the cwd, then asking for
        // the manual OF THAT RUN — in that order, before the session's first tick.
        assert_eq!(
            mock.tool_names(),
            vec![
                "agent_session_event".to_string(),
                "run_start".to_string(),
                "agent_context".to_string()
            ],
            "/{command}: hook posts the session event, opens the run, then pulls its manual",
        );
        let started = mock
            .args_for("run_start")
            .unwrap_or_else(|| panic!("/{command}: run_start was called"));
        assert_eq!(
            started["scope"],
            json!(scope),
            "/{command}: the scope {file} declares is the flow the hook opens a run for",
        );
        assert_eq!(
            started["cwd"],
            json!(cwd),
            "/{command}: the hook anchors the run on the cwd Claude Code handed it",
        );
        let args = mock
            .args_for("agent_context")
            .unwrap_or_else(|| panic!("/{command}: agent_context was called"));
        assert_eq!(
            args["role"],
            json!(role),
            "/{command}: the role {file} declares is what the hook asks for",
        );
        assert_eq!(
            args["scope"],
            json!(scope),
            "/{command}: the scope {file} declares is what the hook asks for",
        );
        assert_eq!(
            args["run_id"],
            json!(MOCK_RUN_ID),
            "/{command}: the manual is rendered FOR the run just opened",
        );
        assert_eq!(
            args["cwd"],
            Value::Null,
            "/{command}: the run is the anchor now — no location is resolved a second time",
        );
        mock.stop();
    }
}

/// A FAILED INJECTION IS FILED IN NYX, not only on a stderr nobody reads (H8).
///
/// Claude Code hands a hook's stderr to nothing nyx can read, so a failure was visible only to a
/// human already running in debug mode — which is exactly how a dead injection survived a whole
/// release (F1). The report rides `agent_session_event`, the channel this hook already POSTs on
/// every prompt, carrying the SAME sentence it wrote to stderr so the two ends cannot drift.
///
/// The happy path files nothing: an audit trail that also records successes is a log, and this one
/// exists to make a rare failure findable.
#[test]
fn a_failed_injection_is_reported_to_nyx_and_a_successful_one_is_not() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    // `run_start` refuses — the live `/ndev`-in-a-multi-Spec-workspace failure.
    let mock = MockSidecar::start_with(Reply::RunStartRefused);
    let run = run_hook("/ndev fix the probe", "/tmp/nyx-push-test-workspace", mock.port);
    assert_eq!(run.code, 0, "the hook always exits 0");
    assert!(run.injected().is_none(), "nothing was injected");

    // The LAST call is the report, and it carries the reason the trace already gave the human.
    let names = mock.tool_names();
    assert_eq!(
        names.last().map(String::as_str),
        Some("agent_session_event"),
        "the failure is filed after the attempt: {names:?}",
    );
    let filed = mock
        .calls_for("agent_session_event")
        .into_iter()
        .find_map(|c| c.get("nyx_injection_failed").cloned())
        .expect("the report names the failure");
    assert!(
        filed.as_str().unwrap_or_default().contains(RUN_START_REFUSAL),
        "…with the server's OWN sentence, the one stderr got: {filed}",
    );
    assert_eq!(
        run.trace().contains(RUN_START_REFUSAL),
        true,
        "and the two ends say the same thing",
    );

    // THE HAPPY PATH FILES NOTHING: one session event (the turn), and no report.
    let ok = MockSidecar::start();
    let good = run_hook("/nspec add backpressure", "/tmp/nyx-push-test-workspace", ok.port);
    assert!(good.injected().is_some(), "the manual was injected");
    assert!(
        ok.calls_for("agent_session_event")
            .iter()
            .all(|c| c.get("nyx_injection_failed").is_none()),
        "a success is not an event: {:?}",
        ok.tool_names(),
    );
}

/// THE SPEC REFERENCE THE HUMAN TYPED REACHES `run_start` (H8), IN ALL FOUR FORMS.
///
/// The three nyx commands advertise a leading Spec reference. The `cwd` alone cannot honour that:
/// a real project holds several active Specs, so resolving one by location is REFUSED as ambiguous
/// (`spec_ambiguous_for_workspace`) and, for `/ndev` and `/ndoc`, the run never opens — replayed
/// live on a workspace with three. The typed reference is the only anchor that disambiguates, and
/// the hook was not reading it: a session had to work out on its own that "the UUID in your command
/// IS the spec_id" and open the run itself, which is the two-call sequence G6 exists to remove.
///
/// AND THE UUID WAS THE ONLY FORM IT COULD SEE — the one shape a human never encounters, since
/// every render, warning and list speaks in `#3`. So the fix landed on the form nobody types while
/// `/nspec 3` still opened an unanchored run. The hook recognises the four SHAPES now and forwards
/// the token RAW: the byte equality below is the whole contract on this side, because the
/// TRANSLATION is `spec_ref`'s and the hook must not own a second copy of it (the grammar itself is
/// pinned token by token against the real hook in `spec_ref`'s own corpus test).
///
/// The reference rides BESIDE the `cwd`, never instead of it — it says WHICH Spec, the cwd still
/// anchors the run's dev workspace.
#[test]
fn the_spec_id_the_human_typed_is_what_the_hook_opens_the_run_on() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let cwd = "/tmp/nyx-push-test-workspace";
    // Every form, on every nyx command: the recogniser is the same one for the three, so a form
    // that only worked on one would be a table the stubs do not describe.
    for command in ["nspec", "ndev", "ndoc"] {
        for reference in [
            // The canonical id — the form the hook already knew.
            "019ff517-3c1a-7b40-9f21-6d5e0a4c8b73",
            // The human key, the sigil'd number and the bare number — the forms a human actually
            // reads off a nyx render, and the ones that used to be dropped on the floor.
            "NYX-3",
            "#3",
            "3",
        ] {
            let mock = MockSidecar::start();
            let run = run_hook(
                &format!("/{command} {reference} corrige la sonde de santé"),
                cwd,
                mock.port,
            );

            assert_eq!(run.code, 0, "the hook always exits 0");
            let args = mock
                .args_for("run_start")
                .expect("the hook opens the run itself");
            assert_eq!(
                args["spec_id"],
                json!(reference),
                "/{command}: '{reference}' reaches run_start EXACTLY as typed — the hook resolves \
                 nothing: {args}",
            );
            assert_eq!(
                args["cwd"],
                json!(cwd),
                "…and the cwd still anchors the workspace — the reference says WHICH Spec, not WHERE",
            );
            assert!(
                run.injected().is_some(),
                "so the injection happens: stdout {:?} stderr {:?}",
                run.stdout,
                run.stderr,
            );
        }
    }

    // The human's TEXT is never mistaken for an anchor: prose in first position resolves NO
    // reference, and the run falls back to the `cwd` exactly as before. Widening the grammar did
    // not widen what is READ — still the first token, still only when it is a whole reference.
    for prose_prompt in [
        "/ndev corrige la sonde de santé",
        "/nspec ajoute le retry sur la Spec 3",
    ] {
        let mock2 = MockSidecar::start();
        let prose = run_hook(prose_prompt, cwd, mock2.port);
        let args2 = mock2.args_for("run_start").expect("the run still opens");
        assert_eq!(
            args2.get("spec_id"),
            None,
            "'{prose_prompt}' carries no Spec reference, and none is invented: {args2}",
        );
        assert_eq!(prose.code, 0);
    }
}

/// A REFERENCE THAT DESIGNATES NOTHING IS RELAYED, NOT SWALLOWED — the failure mode widening the
/// grammar CREATES, and the one that must not cost the human their prompt OR their context.
///
/// Before, an unrecognised first token simply left `spec_id` unset and `run_start` resolved from
/// the `cwd`. Now `/ndev 999` names a Spec, and a Spec that does not exist earns an `invalid_id`
/// from the core — by design, since opening an unanchored run on a named Spec is exactly what
/// ordered a duplicate. What the hook must NOT do is take that on the mute degradation path: the
/// trace goes to stderr, which Claude Code never feeds to the model, so the session would see nyx
/// say nothing at all about a command it just refused. Any first token that merely LOOKS like a
/// reference reaches here — a branch name, a ticket id, `/ndev 3 fois plus rapide` — so this is the
/// common case, not a corner.
///
/// So: NO run and NO manual (a manual with no `run_id` would send the session hunting for one), but
/// an injected INSTRUCTION carrying the core's own sentence and the stubs' own rule — stop, ask,
/// never author a second Spec. Exit 0, and the human's prompt reaches Claude untouched.
#[test]
fn a_reference_the_core_refuses_is_relayed_to_the_session() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::RunStartRefused);
    let run = run_hook("/ndev 999 corrige la sonde", "/tmp/x", mock.port);

    assert_eq!(run.code, 0, "a refused reference never breaks the prompt");
    assert_eq!(
        mock.args_for("run_start").expect("run_start was tried")["spec_id"],
        json!("999"),
        "the token still travelled raw — the refusal is the CORE's verdict, not the hook's",
    );
    let injected = run
        .injected()
        .expect("the refusal is relayed: stderr is not read by the model");
    assert!(
        injected.contains("999"),
        "the block quotes the token as typed, so the human is asked about THEIR word: {injected}",
    );
    assert!(
        !injected.contains("run_id"),
        "…and carries NO run_id: there is no run, and an empty one sends the session hunting: \
         {injected}",
    );
    assert!(
        injected.contains("STOP and ASK the human which Spec they meant")
            && injected.contains("never author a new one"),
        "…and gives the SAME instruction the command stubs do: {injected}",
    );
    assert!(
        !mock.tool_names().iter().any(|n| n == "agent_context"),
        "…and the manual is not even fetched: {:?}",
        mock.tool_names(),
    );
    assert!(
        run.stderr.contains("run_start refused"),
        "the degradation SAYS why, on stderr too, as every other one does: {:?}",
        run.stderr,
    );
}

/// `run_start`'s WARNINGS ride into the same injected block, told to be relayed to the user.
///
/// They are the price of the hook opening the run in the session's place: `run_start` answers a
/// dropped run (`run_replaced_graph_changed`), a vacuous gate (`gate_unconfigured`) or a run started
/// bound to no Spec in a `warnings` array written for a HUMAN to act on. The agent used to read them
/// off its own call; taking that call away and keeping only the id would delete them silently —
/// which is exactly what the human needs, gone.
#[test]
fn run_start_warnings_reach_the_injection_to_be_relayed() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::RunWarnings);
    let run = run_hook("/ndev feature-7", "/tmp/x", mock.port);
    assert_eq!(run.code, 0, "the hook exits 0");
    let injected = run
        .injected()
        .unwrap_or_else(|| panic!("warnings are not a failure: stderr {:?}", run.stderr));
    for (code, message) in MOCK_WARNINGS {
        assert!(
            injected.contains(code) && injected.contains(message),
            "the {code} warning reached the injection verbatim: {injected}",
        );
    }
    assert!(
        injected.contains("RELAY"),
        "…and the block says to relay them to the user: {injected}",
    );
    assert!(
        injected.contains(&mock_manual("dev")),
        "the manual is still there beside them: {injected}",
    );
    assert!(
        run.stderr.trim().is_empty(),
        "a warned run is not a hook failure: {:?}",
        run.stderr,
    );
    mock.stop();
}

/// `run_start` REFUSES a command that NAMED NO SPEC (the real message for a `/ndev` whose location
/// anchors no Spec): the hook injects NOTHING and traces the server's own reason.
///
/// The refusal is the failure mode the hook GAINED by opening the run, and its handling is the
/// point: no run means no `agent_context` call at all. Injecting the manual anyway would hand the
/// session a dispatcher contract whose every step needs a `run_id` it does not have — sending it
/// back to the pre-tick scramble this whole change removes.
///
/// THE PROMPT CARRIES NO REFERENCE, and that is now load-bearing rather than incidental. It used to
/// read `/ndev feature-7`, chosen when the recogniser knew only the canonical id and any other
/// first token was inert; `feature-7` is a `KEY-N` under the widened grammar, so that prompt now
/// exercises the OTHER branch — the one
/// [`a_reference_the_core_refuses_is_relayed_to_the_session`] owns, where the refusal IS relayed.
/// The two are complementary and neither may borrow the other's prompt: nothing at all when nyx
/// was told no Spec, nyx's own sentence when it was told one it could not resolve.
#[test]
fn a_refused_run_start_injects_nothing_and_never_falls_back_to_a_run_less_manual() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::RunStartRefused);
    let run = run_hook("/ndev corrige la sonde de santé", "/tmp/x", mock.port);
    assert_eq!(run.code, 0, "a refused run_start → the hook still exits 0");
    assert_eq!(
        mock.args_for("run_start").expect("run_start was tried")["spec_id"],
        json!(null),
        "the prompt names no Spec — this is the branch where there is nothing to relay",
    );
    assert!(
        run.injected().is_none(),
        "no run → nothing injected at all (stdout: {:?})",
        run.stdout,
    );
    let trace = run.trace();
    assert!(
        trace.contains("run_start refused") && trace.contains(RUN_START_REFUSAL),
        "the trace names run_start and carries the server's own reason: {trace:?}",
    );
    // `agent_context` is NOT called — there is no run to render a manual for. Asserted on the
    // ABSENCE rather than on the whole sequence: a failed injection also files a report
    // (`agent_session_event`), and pinning the exact call list would make this test about the
    // reporting channel instead of about the refusal.
    assert!(
        !mock.tool_names().iter().any(|n| n == "agent_context"),
        "agent_context is NOT called — there is no run to render a manual for: {:?}",
        mock.tool_names(),
    );
    mock.stop();
}

/// A plain prompt (not a command) injects NOTHING, never calls `run_start`/`agent_context`, and says
/// NOTHING: the overwhelmingly common case must cost one regex and leave no noise behind.
#[test]
fn plain_prompt_injects_nothing_and_stays_silent() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start();
    let run = run_hook("just a normal question about the code", "/tmp/x", mock.port);
    assert_eq!(run.code, 0, "the hook exits 0");
    assert!(
        run.injected().is_none(),
        "no command → no additionalContext (stdout: {:?})",
        run.stdout,
    );
    assert!(
        run.stderr.trim().is_empty(),
        "a plain prompt is not a failure: nothing on stderr, got {:?}",
        run.stderr,
    );
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "no command → the session event is posted, but no run is opened and no manual pulled",
    );
    mock.stop();
}

/// A slash command that is not ours (`/clear`) injects nothing — and SAYS so, naming the command
/// it saw and the ones it knows. This trace is the one that would have exposed F1 in one line:
/// a `/nspec` reaching the hook in a shape the table does not recognise cannot stay mute.
#[test]
fn a_foreign_slash_command_is_traced_not_silently_dropped() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start();
    let run = run_hook("/clear", "/tmp/x", mock.port);
    assert_eq!(run.code, 0, "the hook exits 0");
    assert!(
        run.injected().is_none(),
        "foreign command → no injection (stdout: {:?})",
        run.stdout,
    );
    let trace = run.trace();
    assert!(
        trace.contains("/clear is not a nyx command") && trace.contains("/nspec"),
        "the trace names the command it saw and the ones it knows: {trace:?}",
    );
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "foreign command → no run is opened and agent_context is NOT called",
    );
    mock.stop();
}

/// Sidecar DOWN (nothing listening): the hook still exits 0 and injects nothing — and its trace
/// says UNREACHABLE with the port, naming the call that could not be made (`run_start`, the first
/// one now), which is a different diagnosis from "the tool refused".
#[test]
fn a_down_sidecar_is_traced_as_unreachable() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let port = closed_port();
    let run = run_hook("/nspec feature-7", "/tmp/x", port);
    assert_eq!(run.code, 0, "sidecar down → the hook still exits 0");
    assert!(
        run.injected().is_none(),
        "sidecar down → nothing injected (stdout: {:?})",
        run.stdout,
    );
    let trace = run.trace();
    assert!(
        trace.contains("run_start unreachable") && trace.contains(&port.to_string()),
        "the trace says which call found the sidecar unreachable, on which port: {trace:?}",
    );
}

/// The sidecar ANSWERS but `agent_context` refuses (no Spec resolves, unknown role, or the tool
/// is missing on an older sidecar): the hook injects nothing and relays the SERVER's own reason,
/// so the trace names the actual problem instead of a generic failure.
#[test]
fn a_refused_agent_context_is_traced_with_the_server_reason() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::Refused);
    let run = run_hook("/ndev feature-7", "/tmp/x", mock.port);
    assert_eq!(run.code, 0, "a refusal → the hook still exits 0");
    assert!(
        run.injected().is_none(),
        "a refusal → nothing injected (stdout: {:?})",
        run.stdout,
    );
    let trace = run.trace();
    assert!(
        trace.contains("agent_context refused") && trace.contains(REFUSAL),
        "the trace carries the server's own reason: {trace:?}",
    );
    // The run WAS opened and the call WAS made — the failure is the ANSWER, not the call. Asserted
    // on the prefix: the failure also files a report after it (see
    // `a_failed_injection_is_reported_to_nyx_and_a_successful_one_is_not`).
    assert_eq!(
        &mock.tool_names()[..3],
        ["agent_session_event", "run_start", "agent_context"],
        "the run WAS opened and the call WAS made — the failure is the answer, not the call",
    );
    mock.stop();
}

/// The sidecar answers 200 with a result that carries no `structuredContent.context` (a response
/// shape the hook cannot read): traced as such — the third failure, distinct from unreachable and
/// from refused, because a shape drift and a dead sidecar are not the same bug.
#[test]
fn an_answer_without_context_is_traced_as_a_shape_problem() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::NoContext);
    let run = run_hook("/ndoc sync the docs", "/tmp/x", mock.port);
    assert_eq!(run.code, 0, "a shapeless answer → the hook still exits 0");
    assert!(
        run.injected().is_none(),
        "no context in the answer → nothing injected (stdout: {:?})",
        run.stdout,
    );
    let trace = run.trace();
    assert!(
        trace.contains("result.structuredContent.context"),
        "the trace names the field it did not find: {trace:?}",
    );
    mock.stop();
}

// ---------------------------------------------------------------------------
// The SessionStart relay — the AMBIENT context (task 01M14TZZJVEG87D1JAZA6NRCR2).
// ---------------------------------------------------------------------------

/// THE BLOCK REACHES THE SESSION, or it does not exist. A `SessionStart` whose
/// `agent_session_event` answers with the ambient context — the orientation line PLUS the
/// workspace's managed-command block — comes back out of the REAL hook as `additionalContext`,
/// BYTE FOR BYTE: newlines, `·` separators, accented words and the `group_id` included.
///
/// This is the one link between the Rust renderer and a live session that nothing covered: the nine
/// legs above are all `UserPromptSubmit`, and the SessionStart relay — a dozen lines of `.cjs` — was
/// asserted nowhere. It is also the link where a corruption would be SILENT: the hook swallows its
/// own errors by design, so a mangled block would simply arrive mangled.
///
/// It does NOT change the hook: `JSON.stringify` already relays any string. This PROVES it.
#[test]
fn a_session_start_relays_the_ambient_command_block_verbatim() {
    if !node_available() {
        eprintln!("node unavailable: skipped the SessionStart relay leg (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::SessionContext);
    let run = run_hook_event(
        HookEvent::SessionStart("startup"),
        "/tmp/nyx-push-test-workspace",
        Some(mock.port),
        None,
    );

    assert_eq!(run.code, 0, "the hook always exits 0");
    let injected = run.injected_for("SessionStart").unwrap_or_else(|| {
        panic!(
            "a SessionStart carrying a context must inject it: stdout {:?} stderr {:?}",
            run.stdout, run.stderr
        )
    });
    assert_eq!(
        injected, MOCK_SESSION_CONTEXT,
        "the context crosses the hook byte for byte",
    );
    // …and, spelled out, the properties that make "byte for byte" worth asserting.
    assert!(
        injected.lines().count() >= 5,
        "the block survives as MULTI-LINE text, not a single joined line: {injected:?}",
    );
    assert!(
        injected.contains("- dev · bun run dev · running · instance_id=inst-1 · group_id=grp-1"),
        "a whole command line — separators and group_id included — arrives intact: {injected:?}",
    );
    assert!(
        injected.contains("réglages=élevés") && injected.contains("Café"),
        "non-ASCII text is not mangled on the way: {injected:?}",
    );
    assert!(
        run.stderr.trim().is_empty(),
        "the happy path is silent, got stderr {:?}",
        run.stderr,
    );

    // It got there through `agent_session_event` ALONE: a SessionStart opens no run and pulls no
    // manual, so the two run-opening calls must not fire.
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "a SessionStart POSTs exactly one tool call",
    );
    let args = mock
        .args_for("agent_session_event")
        .expect("the session event was POSTed");
    assert_eq!(
        args["hook_event_name"],
        json!("SessionStart"),
        "…the event nyx resolves the context for",
    );
    assert_eq!(
        args["NYX_TERMINAL_ID"],
        json!("term-push-test"),
        "…on the terminal whose workspace the block belongs to",
    );
    mock.stop();
}

/// THE DEGRADED CASE, which must stay harmless: a `SessionStart` whose answer carries NO `context`
/// (an older sidecar, a loose terminal, a workspace with no managed command) injects NOTHING and
/// still exits 0 — the session works, it just starts without the block. Anything else would make a
/// nyx-less answer fatal to every session start.
#[test]
fn a_session_start_without_a_context_injects_nothing_and_still_exits_zero() {
    if !node_available() {
        eprintln!("node unavailable: skipped the SessionStart relay leg (node-agnostic by contract)");
        return;
    }
    // Every non-`SessionContext` variant answers `agent_session_event` with a bare `{}`.
    let mock = MockSidecar::start_with(Reply::Manual);
    let run = run_hook_event(
        HookEvent::SessionStart("resume"),
        "/tmp/nyx-push-test-workspace",
        Some(mock.port),
        None,
    );

    assert_eq!(run.code, 0, "no context is not a failure");
    assert!(
        run.stdout.trim().is_empty(),
        "nothing is injected at all (stdout: {:?})",
        run.stdout,
    );
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "the session event still reached nyx — only the injection is empty",
    );
    mock.stop();
}
