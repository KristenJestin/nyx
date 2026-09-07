//! PUSH-channel e2e (v2.5.8 agent harness, PRD 01KXC80E2CDSNN9PPMJFJFE3X7 / PDR-0020, re-pointed
//! at PDR-0031 — `docs/product/agent-manuals.md` §4 and §5 rows 5b/6/7).
//!
//! The gate (`gate_v2_5_8_agent_harness_e2e.rs`) proves the harness STRUCTURALLY (the stub DECLARES
//! its `NYX-AGENT-CONTEXT role=…` sentinel) and via the universal PULL channel (`agent_context`
//! renders the manual). It never exercises the REAL PUSH path — the branches of the bundled
//! `resources/claude-plugin/hooks/nyx-session-hook.cjs` that talk BACK to Claude Code. This file
//! spawns that `.cjs` under `node` (the hook's own runtime — Claude Code IS node) against an
//! in-process mock sidecar and pins the contract that now holds.
//!
//! WHAT CHANGED, AND WHY THIS FILE WAS REWRITTEN. The orchestration engine is retired: there is no
//! `run_start`, no `run_id`, no `nyx_next`. The hook used to POST `run_start {scope,cwd}` before
//! `agent_context`, and inject a `=== NYX RUN ===` block naming the run to tick. This test pinned
//! all of it — so it was a test of a build that no longer exists, and it asserted on
//! `nyx_next(run_id="…")`, a literal nyx no longer emits anywhere. What the hook does now:
//!
//!   * `UserPromptSubmit` on `/nspec` `/ndev` `/ndoc` → ONE call, `agent_context {role, cwd,
//!     spec_id?}`, where `role` is `spec` / `dev` / `doc` — one per command, the whole of what
//!     selects a manual (§5 row 6) — and the manual comes back wrapped:
//!     `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":…}}`.
//!   * a Spec reference the human typed rides BESIDE the `cwd`, forwarded VERBATIM in all four
//!     forms; a REFUSED reference injects nyx's own refusal sentence instead of a manual, and never
//!     a manual rendered for some other Spec.
//!   * **`SubagentStart` → the WRAPPED `{"hookSpecificOutput":{"hookEventName":"SubagentStart",
//!     "additionalContext":…}}`, exactly like the other two.** This leg used to pin the OPPOSITE —
//!     a bare `{"additionalContext": …}`, on the belief that a wrapped payload was dropped
//!     silently. MEASURED 2026-09-07 against Claude Code v2.1.246 on Windows, driving the real app:
//!     it is the BARE shape that is dropped. The observable is the `hook_additional_context`
//!     attachment in the sub-agent's transcript, written when and only when the text reached the
//!     model; the bare run left only a `hook_success` (so the hook fired and succeeded) and the
//!     sub-agent confirmed it had received no nyx context, while the wrapped `SessionStart` and
//!     `UserPromptSubmit` of that same session left both attachments and were quoted back verbatim.
//!     The shape stays under test because it is invisible to every other test in the repo —
//!     `mcp_tools_core` proves the charter is RENDERED, nothing else proves it is SHAPED right on
//!     the wire.
//!   * `SessionStart` → the ambient context, wrapped like `UserPromptSubmit`, relayed byte for byte.
//!   * every non-injecting outcome leaves its OWN one-line trace on stderr — a foreign slash
//!     command, an unreachable sidecar, a refused `agent_context`, an answer with no context — and
//!     a failed injection is FILED in nyx through `agent_session_event`. That silence is what let a
//!     dead injection survive a release, so it is itself under test.
//!
//! NOTHING REPLAYS THIS FILE AUTOMATICALLY — run it by hand with
//! `cargo test -p nyx-core --test hook_push_injection_e2e` (review finding
//! `01M152SCJ8QHK5C2P002GWZAGA`). Neither `bun run test:core` nor the cross-OS gate workflow covers
//! it: both invoke `cargo test --workspace --lib`, and `--lib` builds only the library targets,
//! excluding every integration binary under `crates/nyx-core/tests/` — this one included. If `node`
//! is not on PATH the test SKIPS with a note (node-agnostic by contract, mirroring how the sidecar
//! gates skip their git legs when git is absent) — but node is the hook's mandatory runtime.

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

/// The ROLE a nyx command stub DECLARES in its `<!-- NYX-AGENT-CONTEXT role=… -->` sentinel
/// (`resources/claude-commands/<file>`). The hook keys on the command NAME (all a
/// `UserPromptSubmit` hook ever receives), so the stub's declaration and the hook's table are two
/// artifacts that must agree: this is what binds them — a drift on either side fails here.
///
/// THERE IS NO `scope=` ANY MORE (§5 row 6). A scope named a node of the retired engine; the role
/// is the whole of what selects a manual, so this reads one field and asserting on it is asserting
/// on the whole declaration.
fn stub_role(file: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources/claude-commands")
        .join(file);
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read stub {p:?}: {e}"));
    let line = text
        .lines()
        .find(|l| l.contains("NYX-AGENT-CONTEXT"))
        .unwrap_or_else(|| panic!("{file} declares a NYX-AGENT-CONTEXT sentinel"));
    assert!(
        !line.contains("scope="),
        "{file}'s sentinel must declare no `scope=` — the engine that gave one meaning is retired: \
         {line}"
    );
    line.split_whitespace()
        .find_map(|t| t.strip_prefix("role=").map(str::to_string))
        .unwrap_or_else(|| panic!("{file}'s sentinel declares role=…"))
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

/// What the mock answers with — one variant per outcome the hook must tell APART in its trace.
#[derive(Clone, Copy, PartialEq)]
enum Reply {
    /// `agent_context` renders the manual (the happy path).
    Manual,
    /// A JSON-RPC error, as the real sidecar returns when `agent_context` refuses: no Spec
    /// resolves, the reference designates several, an unknown role, or the tool is missing
    /// entirely on an older sidecar.
    Refused,
    /// A 200 result whose shape carries no `structuredContent.context` at all.
    NoContext,
    /// `agent_session_event` answers a `SessionStart` with the AMBIENT context — the orientation
    /// line PLUS the workspace's managed-command block.
    SessionContext,
    /// `agent_session_event` answers a `SubagentStart` with the §4 CHARTER, in the very same field
    /// (`structuredContent.context`) `SessionStart` uses. What differs is the shape the hook writes
    /// it back out in, which is the whole point of the leg that drives this variant.
    SubagentCharter,
}

/// The refusal wording the mock echoes — the REAL `agent_context` message for an unresolved Spec
/// (`mcp_context::agent_context`), so the test proves the SERVER's reason reaches both the trace and
/// the injected block.
const REFUSAL: &str =
    "could not resolve a Spec: pass spec_id, or a workspace_id/cwd anchored on one";

/// The AMBIENT session context the mock's `agent_session_event` answers a `SessionStart` with — the
/// shape `mcp_tools_core::session_context_line` really produces: the `You're in nyx — …` orientation
/// line, the workspace's managed-command block, the SHARED System half, then the directive.
///
/// It is deliberately HOSTILE to a naive relay, because that is the point of this leg: it is
/// MULTI-LINE (the `.cjs` relays one JSON string; an implementation that split, trimmed or
/// line-joined would be caught here), it carries the `·` separator and accented text (non-ASCII, so
/// a byte-length `Content-Length` or a latin-1 read would corrupt it), and it names a `group_id`
/// (the token an agent needs to start a whole multi-pane group in ONE call).
///
/// The SHARED half is here for the same reason (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`): its marker
/// (`nature=system`) and the sentence that says stopping one service stops it for everyone are the
/// payload with the most to lose from a mangled relay — an agent that receives the list without the
/// warning treats a machine-wide service as its own dev server.
const MOCK_SESSION_CONTEXT: &str = "You're in nyx — project \"Café\" (project_id=proj-café) · \
     workspace \"main\" (workspace_id=ws-1) · terminal_id=term-push-test\n\
     Managed commands of this workspace:\n\
     - dev · bun run dev · running · instance_id=inst-1 · group_id=grp-1\n\
     - api · bun run api --réglages=élevés · idle · instance_id=inst-2 · group_id=grp-1\n\
     - build · bun run build · success · instance_id=inst-3\n\
     +2 more commands (list_commands)\n\
     Shared System commands — machine-wide, NOT this workspace's:\n\
     - Commande factice · node ticker.js · running · instance_id=inst-sys-1 · nature=system\n\
     Prefer start_command over launching these in a raw shell: nyx then supervises the run.\n\
     start_command is a NO-OP on a command already running (was_running:true, restarted:false).\n\
     Those System commands are SHARED: machine-wide services, the same ones for every project and \
     every agent on this machine. Stopping one stops it for ALL of them at once.";

/// The SUB-AGENT CHARTER the mock's `agent_session_event` answers a `SubagentStart` with — the head
/// of the §4 text `mcp_context::render_subagent_charter` really produces, kept multi-line and
/// accented for the same reason [`MOCK_SESSION_CONTEXT`] is: a charter that arrives joined, trimmed
/// or mangled is a charter that arrives wrong, and nothing downstream would say so.
const MOCK_SUBAGENT_CHARTER: &str =
    "# nyx — you are a sub-agent in workspace \"main\" (/tmp/nyx-push-test-workspace), Spec NYX-3.\n\
     \n\
     - Your mission is the prompt above. You do not decide the scope; you do not transition a task\n\
     \x20 or the Spec; you do not resolve or validate anything human.\n\
     - Need the why? spec_get(\"01KMOCKSPEC0000000000000000\"). Need the plan? task_list.\n\
     \n\
     Managed commands of this workspace:\n\
     - dev · bun run dev --réglages=élevés · running · instance_id=inst-1 · group_id=grp-1";

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

    /// The tool names the hook POSTed, in order (`agent_session_event`, then `agent_context` only
    /// when the injection branch fires).
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

/// Read one HTTP request off `stream`, record its JSON-RPC body, and answer the `agent_context` /
/// `agent_session_event` calls per `reply` (any other tool gets a bare ok). Best-effort: a
/// malformed request is answered with an empty ok so the hook still resolves.
fn handle_conn(stream: &mut TcpStream, calls: &Arc<Mutex<Vec<Value>>>, reply: Reply) {
    let Some(body) = read_http_body(stream) else {
        return;
    };
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let name = parsed["params"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let hook_event = parsed["params"]["arguments"]["hook_event_name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if parsed.is_object() {
        calls.lock().unwrap().push(parsed.clone());
    }
    let id = parsed["id"].clone();
    let payload = if name == "agent_context" {
        match reply {
            Reply::Manual | Reply::SessionContext | Reply::SubagentCharter => {
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
        // managed-command block.
        json!({ "jsonrpc": "2.0", "id": id, "result": {
            "structuredContent": { "context": MOCK_SESSION_CONTEXT }
        }})
    } else if name == "agent_session_event"
        && reply == Reply::SubagentCharter
        && hook_event == "SubagentStart"
    {
        // The §4 CHARTER, in the SAME field the ambient context uses — one mechanism, and the
        // difference lives in the SHAPE the hook writes back, not in the field it reads.
        json!({ "jsonrpc": "2.0", "id": id, "result": {
            "structuredContent": { "context": MOCK_SUBAGENT_CHARTER }
        }})
    } else {
        // agent_session_event (or anything else) — a bare ok; UserPromptSubmit ignores it, and a
        // SessionStart / SubagentStart takes it as "nothing to inject".
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

    /// The WRAPPED `additionalContext` of a `SubagentStart`, with the BARE top-level key asserted
    /// ABSENT. The bare shape is what this file used to demand; the real app drops it (see the
    /// module note), so its absence is asserted here rather than merely tolerated — a payload
    /// carrying both keys would pass a wrapper check and still tell the next reader the wrong
    /// thing.
    fn injected_charter(&self) -> Option<String> {
        let trimmed = self.stdout.trim();
        if trimmed.is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(trimmed).ok()?;
        assert!(
            v.get("additionalContext").is_none(),
            "a SubagentStart payload must carry NO top-level `additionalContext` — that BARE shape \
             is the one Claude Code drops SILENTLY, taking the whole charter with it. Got: {trimmed}",
        );
        self.injected_for("SubagentStart")
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
/// `compact`) for a `SessionStart`, the spawned agent's type for a `SubagentStart`. Modelled as an
/// enum rather than a free-form JSON so a leg cannot silently fabricate a field a real hook never
/// receives — the F1 mistake this file exists to guard.
#[derive(Clone, Copy)]
enum HookEvent<'a> {
    UserPromptSubmit(&'a str),
    SessionStart(&'a str),
    SubagentStart(&'a str),
}

impl HookEvent<'_> {
    /// The `hook_event_name` (and argv[2]) this event is delivered under.
    fn name(self) -> &'static str {
        match self {
            HookEvent::UserPromptSubmit(_) => "UserPromptSubmit",
            HookEvent::SessionStart(_) => "SessionStart",
            HookEvent::SubagentStart(_) => "SubagentStart",
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
            HookEvent::SubagentStart(agent_type) => json!({
                "hook_event_name": "SubagentStart",
                "session_id": "sess-push-test",
                "cwd": cwd,
                "agent_type": agent_type,
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
// The PUSH contract — `UserPromptSubmit`.
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
        "/nspec add backpressure",
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
    assert_eq!(
        injected,
        mock_manual("spec"),
        "the /nspec manual was relayed, whole",
    );
    assert!(
        mock.tool_names().contains(&"agent_context".to_string()),
        "the calls landed on THIS mock — the port came from the plugin root",
    );
}

/// EACH COMMAND ASKS FOR ITS OWN MANUAL. Invoking a nyx command makes the hook POST exactly ONE
/// `agent_context {role, cwd}` — the role that command's stub DECLARES (`/nspec → spec`,
/// `/ndev → dev`, `/ndoc → doc`, §5 row 6) — and relay what comes back as `additionalContext`
/// INSIDE the `hookSpecificOutput` wrapper `UserPromptSubmit` requires.
///
/// Two guards in one. The F1 one: a `UserPromptSubmit` hook receives the prompt the human TYPED,
/// never the expanded command body, so every prompt below is a RAW invocation string; this fails
/// the day the hook keys on anything the payload does not carry. The PDR-0031 one: there is no run
/// to open any more, so the call list must be exactly the session event plus the manual fetch — an
/// extra call here would mean a dead `run_start` came back from the grave.
#[test]
fn each_command_pushes_the_manual_for_the_role_its_stub_declares() {
    if !node_available() {
        eprintln!("node unavailable: skipped the PUSH-injection e2e (node-agnostic by contract)");
        return;
    }
    let cwd = "/tmp/nyx-push-test-workspace";
    for (command, file, expected_role) in [
        ("nspec", "nspec.md", "spec"),
        ("ndev", "ndev.md", "dev"),
        ("ndoc", "ndoc.md", "doc"),
    ] {
        let role = stub_role(file);
        assert_eq!(
            role, expected_role,
            "{file} declares the role PDR-0031 gives /{command}",
        );
        let mock = MockSidecar::start();
        // The submitted prompt = what the human typed: the command, then their free text.
        let run = run_hook(&format!("/{command} add backpressure"), cwd, mock.port);
        assert_eq!(run.code, 0, "the hook always exits 0 (/{command})");

        // The injection actually fired: the mock's role-tagged manual reached additionalContext,
        // WHOLE — a manual is a document, and a relay that trimmed or truncated it would be a
        // silent corruption of the only instruction the session gets.
        let injected = run.injected().unwrap_or_else(|| {
            panic!(
                "/{command}: expected a manual injected, got stdout {:?} stderr {:?}",
                run.stdout, run.stderr
            )
        });
        assert_eq!(
            injected,
            mock_manual(&role),
            "/{command}: the hook relayed the manual for the declared role, byte for byte",
        );
        assert!(
            run.stderr.trim().is_empty(),
            "/{command}: the happy path is silent, got stderr {:?}",
            run.stderr,
        );

        // ONE fetch, on the anchor Claude Code handed the hook. No run is opened, and no location
        // is resolved twice.
        assert_eq!(
            mock.tool_names(),
            vec![
                "agent_session_event".to_string(),
                "agent_context".to_string()
            ],
            "/{command}: the hook posts the session event, then pulls its manual — nothing else",
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
            args["cwd"],
            json!(cwd),
            "/{command}: the hook anchors on the cwd Claude Code handed it",
        );
        assert_eq!(
            args.get("spec_id"),
            None,
            "/{command}: no reference was typed, so none is invented: {args}",
        );
        assert_eq!(
            args.get("run_id"),
            None,
            "/{command}: there is no run any more — a `run_id` here would be a resurrected engine",
        );
        assert_eq!(
            args.get("scope"),
            None,
            "/{command}: and no `scope` — the role is the whole of what selects a manual",
        );
        mock.stop();
    }
}

/// THE SPEC REFERENCE THE HUMAN TYPED REACHES `agent_context` VERBATIM, IN ALL FOUR FORMS.
///
/// The three nyx commands advertise a leading Spec reference. The `cwd` alone cannot honour that:
/// a real project holds several active Specs, so resolving one by location is REFUSED as ambiguous
/// and, for `/ndev` and `/ndoc`, nothing renders. The typed reference is the only anchor that
/// disambiguates.
///
/// AND THE UUID USED TO BE THE ONLY FORM THE HOOK COULD SEE — the one shape a human never
/// encounters, since every render, warning and list speaks in `#3`. The hook recognises the four
/// SHAPES and forwards the token RAW: the byte equality below is the whole contract on this side,
/// because the TRANSLATION is `spec_ref`'s and the hook must not own a second copy of it (the
/// grammar itself is pinned token by token against the real hook in `spec_ref`'s own corpus test).
///
/// The reference rides BESIDE the `cwd`, never instead of it — it says WHICH Spec, the cwd still
/// anchors the workspace whose commands the manual renders.
#[test]
fn the_spec_reference_the_human_typed_reaches_agent_context_verbatim() {
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
                .args_for("agent_context")
                .expect("the hook fetches the manual itself");
            assert_eq!(
                args["spec_id"],
                json!(reference),
                "/{command}: '{reference}' reaches agent_context EXACTLY as typed — the hook \
                 resolves nothing: {args}",
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
    // reference, and the render falls back to the `cwd` exactly as before. Widening the grammar did
    // not widen what is READ — still the first token, still only when it is a whole reference.
    for prose_prompt in [
        "/ndev corrige la sonde de santé",
        "/nspec ajoute le retry sur la Spec 3",
    ] {
        let mock2 = MockSidecar::start();
        let prose = run_hook(prose_prompt, cwd, mock2.port);
        let args2 = mock2
            .args_for("agent_context")
            .expect("the manual is still fetched");
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
/// Before, an unrecognised first token simply left `spec_id` unset and the manual rendered off the
/// `cwd`. Now `/ndev 999` NAMES a Spec, and a Spec that does not exist earns a refusal from the
/// core — by design, since rendering /ndev's manual for a Spec nobody named is exactly what ordered
/// a duplicate. What the hook must NOT do is take that on the mute degradation path: the trace goes
/// to stderr, which Claude Code never feeds to the model, so the session would see nyx say nothing
/// at all about a command it just refused. Any first token that merely LOOKS like a reference
/// reaches here — a branch name, a ticket id, `/ndev 3 fois plus rapide` — so this is the common
/// case, not a corner.
///
/// So: NO manual for a DIFFERENT Spec and no manual at all, but an injected INSTRUCTION carrying
/// the core's own sentence and the stubs' own rule — stop, ask, never author a second Spec. Exit 0,
/// and the human's prompt reaches Claude untouched.
#[test]
fn a_refused_reference_injects_nyxs_refusal_and_never_a_manual() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::Refused);
    let run = run_hook("/ndev 999 corrige la sonde", "/tmp/x", mock.port);

    assert_eq!(run.code, 0, "a refused reference never breaks the prompt");
    let args = mock.args_for("agent_context").expect("agent_context was tried");
    assert_eq!(
        args["spec_id"],
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
        injected.contains(REFUSAL),
        "…and carries nyx's OWN sentence, the one that names what the project actually holds: \
         {injected}",
    );
    // THE DEFECT THIS FORBIDS: a manual for SOME OTHER Spec. A render that fell back to the cwd
    // after a named reference was refused would hand the session a plan for a Spec the human never
    // asked about, and nothing in the text would say so.
    assert!(
        !injected.contains("=== NYX MOCK MANUAL"),
        "no manual is injected — least of all one rendered for a different Spec: {injected}",
    );
    assert!(
        injected.contains("STOP and ASK the human which Spec they meant")
            && injected.contains("never author a new one to get past a refusal"),
        "…and gives the SAME instruction the command stubs do: {injected}",
    );
    let trace = run.trace();
    assert!(
        trace.contains("agent_context refused") && trace.contains(REFUSAL),
        "the degradation SAYS why, on stderr too, as every other one does: {trace:?}",
    );
    mock.stop();
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
    // `agent_context` refuses — the live `/ndev`-outside-an-anchored-workspace failure.
    let mock = MockSidecar::start_with(Reply::Refused);
    let run = run_hook(
        "/ndev corrige la sonde",
        "/tmp/nyx-push-test-workspace",
        mock.port,
    );
    assert_eq!(run.code, 0, "the hook always exits 0");
    assert!(
        run.injected().is_none(),
        "no reference was typed, so there is nothing to relay and nothing is injected: {:?}",
        run.stdout,
    );

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
        filed.as_str().unwrap_or_default().contains(REFUSAL),
        "…with the server's OWN sentence, the one stderr got: {filed}",
    );
    assert!(
        run.trace().contains(REFUSAL),
        "and the two ends say the same thing",
    );

    // THE HAPPY PATH FILES NOTHING: one session event (the turn), and no report.
    let ok = MockSidecar::start();
    let good = run_hook(
        "/nspec add backpressure",
        "/tmp/nyx-push-test-workspace",
        ok.port,
    );
    assert!(good.injected().is_some(), "the manual was injected");
    assert!(
        ok.calls_for("agent_session_event")
            .iter()
            .all(|c| c.get("nyx_injection_failed").is_none()),
        "a success is not an event: {:?}",
        ok.tool_names(),
    );
}

/// A plain prompt (not a command) injects NOTHING, never calls `agent_context`, and says NOTHING:
/// the overwhelmingly common case must cost one regex and leave no noise behind.
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
        "no command → the session event is posted, and no manual pulled",
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
    let lines: Vec<&str> = run
        .stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "exactly one trace line: {:?}", run.stderr);
    assert!(
        lines[0].contains("/clear is not a nyx command") && lines[0].contains("/nspec"),
        "the trace names the command it saw and the ones it knows: {:?}",
        lines[0],
    );
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "foreign command → agent_context is NOT called",
    );
    mock.stop();
}

/// Sidecar DOWN (nothing listening): the hook still exits 0 and injects nothing — and its trace
/// says UNREACHABLE with the port, naming the call that could not be made, which is a different
/// diagnosis from "the tool refused".
#[test]
fn a_down_sidecar_is_traced_as_unreachable() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let port = closed_port();
    let run = run_hook("/nspec add backpressure", "/tmp/x", port);
    assert_eq!(run.code, 0, "sidecar down → the hook still exits 0");
    assert!(
        run.injected().is_none(),
        "sidecar down → nothing injected (stdout: {:?})",
        run.stdout,
    );
    let trace = run.trace();
    assert!(
        trace.contains("agent_context unreachable") && trace.contains(&port.to_string()),
        "the trace says which call found the sidecar unreachable, on which port: {trace:?}",
    );
}

/// The sidecar ANSWERS but `agent_context` refuses on a prompt that named NO reference: the hook
/// injects nothing at all and relays the SERVER's own reason on stderr.
///
/// THE PROMPT CARRIES NO REFERENCE, and that is load-bearing rather than incidental: `feature-7` is
/// a `KEY-N` under the widened grammar, so a prompt naming one exercises the OTHER branch — the one
/// [`a_refused_reference_injects_nyxs_refusal_and_never_a_manual`] owns, where the refusal IS
/// relayed into the session. The two are complementary and neither may borrow the other's prompt:
/// nothing at all when nyx was told no Spec, nyx's own sentence when it was told one it could not
/// resolve.
#[test]
fn a_refused_agent_context_with_no_typed_reference_is_traced_and_injects_nothing() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::Refused);
    let run = run_hook("/ndev corrige la sonde de santé", "/tmp/x", mock.port);
    assert_eq!(run.code, 0, "a refusal → the hook still exits 0");
    assert!(
        run.injected().is_none(),
        "a refusal with nothing to quote back → nothing injected (stdout: {:?})",
        run.stdout,
    );
    let trace = run.trace();
    assert!(
        trace.contains("agent_context refused") && trace.contains(REFUSAL),
        "the trace carries the server's own reason: {trace:?}",
    );
    // The call WAS made — the failure is the ANSWER, not the call. Asserted on the prefix: the
    // failure also files a report after it (see
    // `a_failed_injection_is_reported_to_nyx_and_a_successful_one_is_not`).
    assert_eq!(
        &mock.tool_names()[..2],
        ["agent_session_event", "agent_context"],
        "the call WAS made — the failure is the answer, not the call",
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
// The SUB-AGENT CHARTER — `SubagentStart`, and the SHAPE it must take
// (PDR-0031 / `docs/product/agent-manuals.md` §4, §5 row 5b).
// ---------------------------------------------------------------------------

/// **A `SubagentStart` answers with the WRAPPED
/// `{"hookSpecificOutput":{"hookEventName":"SubagentStart","additionalContext":…}}`** — the same
/// shape as the other two branches. This leg exists for that one key, and nothing else in the repo
/// covers it.
///
/// A sub-agent the orchestrator spawned inherits its MISSION from the orchestrator and nothing
/// else: not the workspace, not the Spec, not the managed commands. nyx knows all three from the
/// parent terminal, so the `agent_session_event` the hook POSTs on EVERY hook event carries the §4
/// charter back in `structuredContent.context` — the very field `SessionStart` uses, because the
/// shapes are identical on the way IN.
///
/// THEY ARE IDENTICAL ON THE WAY OUT TOO, and this test used to say the exact opposite: it demanded
/// a BARE `{"additionalContext": …}` and FAILED on the wrapper, on the belief that Claude Code
/// dropped a wrapped `SubagentStart` silently. That was inverted. MEASURED 2026-09-07, Claude Code
/// v2.1.246 on Windows, driving the real app — a real sub-agent spawned from a real nyx terminal,
/// plugin installed from this build. The observable is the `hook_additional_context` attachment in
/// the sub-agent's transcript (`~/.claude/projects/<proj>/<session>/subagents/agent-*.jsonl`),
/// written when and only when the text reached the model. The BARE run left a `hook_success`
/// attachment carrying the hook's stdout — it fired, and succeeded — and NO
/// `hook_additional_context`; the live sub-agent, asked to repeat the first line of its nyx
/// context, answered that it had received none. In the SAME parent transcript the WRAPPED
/// `SessionStart` and `UserPromptSubmit` each left both attachments, and the manuals they carried
/// were quoted back verbatim by the model. So the wrapper is what carries, the bare key is what is
/// dropped SILENTLY — no error on stderr, no rejection from the sidecar, no missing call for any
/// other test to notice — and the absence of the bare key is asserted here explicitly, not merely
/// tolerated.
#[test]
fn a_subagent_start_returns_a_wrapped_additional_context_like_the_other_hooks() {
    if !node_available() {
        eprintln!("node unavailable: skipped the SubagentStart charter leg (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start_with(Reply::SubagentCharter);
    let run = run_hook_event(
        HookEvent::SubagentStart("general-purpose"),
        "/tmp/nyx-push-test-workspace",
        Some(mock.port),
        None,
    );

    assert_eq!(run.code, 0, "the hook always exits 0");

    // THE SHAPE. `injected_charter` fails if the BARE top-level key is present at all, and asserts
    // the wrapper is TAGGED `SubagentStart` — a block tagged with the wrong event is dropped just
    // as silently as an untagged one.
    let charter = run.injected_charter().unwrap_or_else(|| {
        panic!(
            "a SubagentStart carrying a charter must inject it WRAPPED: stdout {:?} stderr {:?}",
            run.stdout, run.stderr
        )
    });
    // And, stated the other way round, so a stray bare key cannot slip in as an EXTRA beside a
    // still-correct wrapper.
    let payload: Value = serde_json::from_str(run.stdout.trim()).expect("the payload is JSON");
    assert_eq!(
        payload,
        json!({
            "hookSpecificOutput": {
                "hookEventName": "SubagentStart",
                "additionalContext": MOCK_SUBAGENT_CHARTER,
            }
        }),
        "the whole payload is the wrapper and nothing else",
    );

    // THE CONTENT crosses byte for byte — multi-line, accented, separators intact. A charter that
    // arrives joined or trimmed is a charter that arrives wrong, and nothing downstream says so.
    assert_eq!(
        charter, MOCK_SUBAGENT_CHARTER,
        "the charter crosses the hook byte for byte",
    );
    assert!(
        charter.starts_with("# nyx — you are a sub-agent in workspace"),
        "…opening on the §4 heading: {charter:?}",
    );
    assert!(
        charter.lines().count() >= 5,
        "…as MULTI-LINE text, not a single joined line: {charter:?}",
    );
    assert!(
        charter.contains("bun run dev --réglages=élevés")
            && charter.contains("· instance_id=inst-1 · group_id=grp-1"),
        "…managed-command lines, separators and non-ASCII included: {charter:?}",
    );
    assert!(
        run.stderr.trim().is_empty(),
        "the happy path is silent, got stderr {:?}",
        run.stderr,
    );

    // It got there through `agent_session_event` ALONE: a SubagentStart pulls no manual — the
    // charter is not a role manual and `agent_context` has no role for a sub-agent.
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "a SubagentStart POSTs exactly one tool call",
    );
    let args = mock
        .args_for("agent_session_event")
        .expect("the session event was POSTed");
    assert_eq!(
        args["hook_event_name"],
        json!("SubagentStart"),
        "…the event nyx resolves the charter for",
    );
    assert_eq!(
        args["NYX_TERMINAL_ID"],
        json!("term-push-test"),
        "…on the PARENT terminal, which is the only thing nyx can resolve a workspace from",
    );
    mock.stop();
}

/// THE DEGRADED `SubagentStart`, which must stay harmless: an answer carrying NO `context` (an
/// older sidecar, a terminal bound to no workspace) injects NOTHING and still exits 0. The
/// sub-agent runs on its mission alone; anything else would make a nyx-less answer fatal to every
/// sub-agent spawn, which is a far worse failure than a missing charter.
#[test]
fn a_subagent_start_without_a_charter_injects_nothing_and_still_exits_zero() {
    if !node_available() {
        eprintln!("node unavailable: skipped the SubagentStart charter leg (node-agnostic by contract)");
        return;
    }
    // Every non-`SubagentCharter` variant answers `agent_session_event` with a bare `{}`.
    let mock = MockSidecar::start_with(Reply::Manual);
    let run = run_hook_event(
        HookEvent::SubagentStart("Explore"),
        "/tmp/nyx-push-test-workspace",
        Some(mock.port),
        None,
    );

    assert_eq!(run.code, 0, "no charter is not a failure");
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

// ---------------------------------------------------------------------------
// The SessionStart relay — the AMBIENT context (task 01M14TZZJVEG87D1JAZA6NRCR2).
// ---------------------------------------------------------------------------

/// THE BLOCK REACHES THE SESSION, or it does not exist. A `SessionStart` whose
/// `agent_session_event` answers with the ambient context — the orientation line PLUS the
/// workspace's managed-command block — comes back out of the REAL hook as `additionalContext`,
/// BYTE FOR BYTE: newlines, `·` separators, accented words and the `group_id` included.
///
/// It is the WRAPPED shape — the same one the `SubagentStart` leg above now pins. Asserting the
/// three side by side is what makes the UNIFORMITY a fact of the test suite rather than a comment
/// in the `.cjs`, which is how the inverted claim survived as long as it did.
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
        injected
            .contains("- Commande factice · node ticker.js · running · instance_id=inst-sys-1 · nature=system"),
        "…and so does a SHARED command line, its nature marker included: {injected:?}",
    );
    assert!(
        injected.contains("Stopping one stops it for ALL of them at once"),
        "…and the sentence that says what sharing costs, which is the half an agent must not \
         receive without: {injected:?}",
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

    // It got there through `agent_session_event` ALONE: a SessionStart pulls no manual.
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
