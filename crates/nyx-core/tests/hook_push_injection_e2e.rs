//! PUSH-channel e2e (v2.5.8 agent harness, PRD 01KXC80E2CDSNN9PPMJFJFE3X7 / PDR-0020,
//! review 01KXEF1NJ0P2V5NCYV2QRYFSFS finding 01KXEFFTBEC6MC8S892NEFMJMN).
//!
//! The gate (`gate_v2_5_8_agent_harness_e2e.rs`) proves the harness STRUCTURALLY (the stub
//! CARRIES the sentinel) and via the universal PULL channel (`agent_context` renders the
//! manual). It never exercises the REAL PUSH path — the branch of the bundled
//! `resources/claude-plugin/hooks/nyx-session-hook.cjs` that, on a `UserPromptSubmit` whose
//! prompt carries the `NYX-AGENT-CONTEXT role=<role>` sentinel, derives the role, POSTs a
//! `agent_context` `tools/call` to the sidecar, and relays the rendered manual back to Claude
//! as `additionalContext`. A drift between the stub's sentinel format and the hook's regex,
//! or a regression of that branch, would pass the gate in silence.
//!
//! This PERSISTENT test closes that gap: it spawns the ACTUAL hook `.cjs` under `node` (the
//! hook's own runtime — Claude Code IS node) against an in-process mock sidecar (a bare
//! `std::net::TcpListener` speaking just enough HTTP/JSON-RPC), feeds it the REAL command-stub
//! bodies (`nspec.md` / `ndev.md` / `ndoc.md`), and asserts the end-to-end PUSH contract:
//!   * a triggerable-role sentinel  → the hook POSTs `agent_context {role,cwd}` and emits the
//!     returned manual as `UserPromptSubmit` `additionalContext` (the injection actually fires);
//!   * a plain prompt (no sentinel) → NOTHING is injected and `agent_context` is never called;
//!   * an unknown / pull-only role (`coder`) → ignored (no injection, no `agent_context`);
//!   * the sidecar DOWN               → the hook still exits 0 and injects nothing (best-effort).
//!
//! It runs in the SAME reproducible CI as the gate (`cargo test -p nyx-core`). If `node` is not
//! on PATH the test SKIPS with a note (node-agnostic by contract, mirroring how the sidecar
//! gates skip their git legs when git is absent) — but node is the hook's mandatory runtime.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
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

/// The REAL body of a nyx command stub (`resources/claude-commands/<file>`), i.e. the
/// expanded prompt Claude submits when the human runs the BARE user-scope slash command —
/// the exact text the hook's `roleFromPrompt` regex must key on. Since finding 01KXEJVZ the
/// stubs are deployed USER-SCOPE (`~/.claude/commands/`, invocable un-namespaced) from this
/// canonical source, NOT bundled into the plugin (which would namespace them `/<plugin>:<cmd>`).
fn stub_body(file: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources/claude-commands")
        .join(file);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read stub {p:?}: {e}"))
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

/// A minimal in-process mock of the nyx sidecar's `/mcp` JSON-RPC endpoint. It answers every
/// `agent_context` `tools/call` with a role-tagged manual (so the test can prove THAT manual
/// flowed through to `additionalContext`) and records every call it receives, so the test can
/// assert which tools the hook did — and did not — invoke.
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
                        handle_conn(&mut stream, &calls_t);
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

    /// The tool names the hook POSTed, in order (`agent_session_event`, then `agent_context`
    /// only when the injection branch fires).
    fn tool_names(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c["params"]["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The first recorded `agent_context` call's `arguments` (`{ role, cwd }`), if any.
    fn agent_context_args(&self) -> Option<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c["params"]["name"] == json!("agent_context"))
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

/// Read one HTTP request off `stream`, record its JSON-RPC body, and answer with a role-tagged
/// `agent_context` manual (or a bare ok for any other tool). Best-effort: a malformed request
/// is answered with an empty ok so the hook still resolves.
fn handle_conn(stream: &mut TcpStream, calls: &Arc<Mutex<Vec<Value>>>) {
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
    let result = if name == "agent_context" {
        let role = parsed["params"]["arguments"]["role"]
            .as_str()
            .unwrap_or_default();
        json!({ "structuredContent": { "context": mock_manual(role) } })
    } else {
        // agent_session_event (or anything else) — a bare ok; UserPromptSubmit ignores it.
        json!({ "structuredContent": {} })
    };
    let payload = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
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

/// Run the REAL hook as `node nyx-session-hook.cjs UserPromptSubmit`, piping the given prompt
/// (as the `UserPromptSubmit` hook stdin) with the mock's port in the environment. Returns
/// `(exit_code, stdout)`.
fn run_hook(prompt: &str, cwd: &str, port: u16) -> (i32, String) {
    let input = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-push-test",
        "cwd": cwd,
        "prompt": prompt,
    })
    .to_string();

    let mut child = Command::new("node")
        .arg(hook_path())
        .arg("UserPromptSubmit")
        .env("NYX_MCP_PORT", port.to_string())
        .env("NYX_TERMINAL_ID", "term-push-test")
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
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

/// Extract `hookSpecificOutput.additionalContext` from the hook's stdout, or `None` when the
/// hook emitted nothing (a plain / ignored prompt).
fn additional_context(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(trimmed).ok()?;
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        json!("UserPromptSubmit"),
        "the injected block is tagged UserPromptSubmit",
    );
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .map(str::to_string)
}

/// A pick a definitely-closed port (bind then drop) so the "sidecar down" leg gets ECONNREFUSED.
fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

// ---------------------------------------------------------------------------
// The PUSH contract.
// ---------------------------------------------------------------------------

/// A `UserPromptSubmit` carrying a triggerable-role sentinel makes the hook POST
/// `agent_context {role,cwd}` and relay the returned manual as `additionalContext` — proven
/// on the REAL `nspec.md` / `ndev.md` / `ndoc.md` stub bodies, one role per iteration.
#[test]
fn push_injects_the_manual_for_each_triggerable_stub() {
    if !node_available() {
        eprintln!("node unavailable: skipped the PUSH-injection e2e (node-agnostic by contract)");
        return;
    }
    let cwd = "/tmp/nyx-push-test-workspace";
    for (file, role) in [("nspec.md", "spec"), ("ndev.md", "dev"), ("ndoc.md", "doc")] {
        let mock = MockSidecar::start();
        // The submitted prompt = the REAL expanded stub body + the human's free text.
        let prompt = format!("{}\n\nfeature-42 add backpressure", stub_body(file));
        let (code, stdout) = run_hook(&prompt, cwd, mock.port);
        assert_eq!(code, 0, "the hook always exits 0 ({file})");

        // The injection actually fired: the mock's role-tagged manual reached additionalContext.
        let injected = additional_context(&stdout).unwrap_or_else(|| {
            panic!("{file}: expected a manual injected, got stdout: {stdout:?}")
        });
        assert_eq!(
            injected,
            mock_manual(role),
            "{file}: the hook relayed the agent_context manual for role={role}",
        );

        // It got there by POSTing agent_context with the role parsed from the sentinel + the cwd.
        assert_eq!(
            mock.tool_names(),
            vec![
                "agent_session_event".to_string(),
                "agent_context".to_string()
            ],
            "{file}: hook posts the session event, then the agent_context injection",
        );
        let args = mock
            .agent_context_args()
            .unwrap_or_else(|| panic!("{file}: agent_context was called"));
        assert_eq!(
            args["role"],
            json!(role),
            "{file}: role parsed from the sentinel"
        );
        assert_eq!(
            args["cwd"],
            json!(cwd),
            "{file}: cwd forwarded for Spec resolution"
        );
        mock.stop();
    }
}

/// A plain prompt (no nyx sentinel) injects NOTHING and never triggers an `agent_context` call
/// — the injection is scoped strictly to the slash-command marker.
#[test]
fn plain_prompt_injects_nothing() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start();
    let (code, stdout) = run_hook("just a normal question about the code", "/tmp/x", mock.port);
    assert_eq!(code, 0, "the hook exits 0");
    assert!(
        additional_context(&stdout).is_none(),
        "no marker → no additionalContext (stdout: {stdout:?})",
    );
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "no marker → the session event is posted but agent_context is NOT",
    );
    mock.stop();
}

/// A sentinel naming a PULL-ONLY role (`coder`, not one of spec/dev/doc) is IGNORED: the hook
/// derives no injectable role, so it neither calls `agent_context` nor injects anything. This
/// is the hook's own guard (`TRIGGERABLE_ROLES`), independent of the stub set.
#[test]
fn unknown_or_pull_only_role_marker_is_ignored() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let mock = MockSidecar::start();
    let prompt = "<!-- NYX-AGENT-CONTEXT role=coder -->\n\ndo the thing";
    let (code, stdout) = run_hook(prompt, "/tmp/x", mock.port);
    assert_eq!(code, 0, "the hook exits 0");
    assert!(
        additional_context(&stdout).is_none(),
        "pull-only role → no injection (stdout: {stdout:?})",
    );
    assert_eq!(
        mock.tool_names(),
        vec!["agent_session_event".to_string()],
        "unknown role → agent_context is NOT called",
    );
    mock.stop();
}

/// Robustness: with the sidecar DOWN (nothing listening), a sentinel prompt still exits 0 and
/// injects nothing — the hook must never disturb the session when the server is unreachable.
#[test]
fn sidecar_down_exits_zero_and_injects_nothing() {
    if !node_available() {
        eprintln!("node unavailable: skipped (node-agnostic by contract)");
        return;
    }
    let prompt = format!("{}\n\nfeature-7", stub_body("nspec.md"));
    let (code, stdout) = run_hook(&prompt, "/tmp/x", closed_port());
    assert_eq!(code, 0, "sidecar down → the hook still exits 0");
    assert!(
        additional_context(&stdout).is_none(),
        "sidecar down → nothing injected (stdout: {stdout:?})",
    );
}
