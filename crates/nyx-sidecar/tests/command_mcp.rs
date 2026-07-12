//! Integration tests that spawn the REAL `nyx-sidecar` binary and drive the
//! managed-command runtime + the MCP HTTP server (HHD) — proving the runtime/business
//! logic now lives IN THE SIDECAR (not the TS core-host).
//!
//! Done-criteria covered:
//!   * **start/stop/relaunch a managed command via the sidecar** — the lifecycle is
//!     driven over the pipe (`command-start`/`stop`/`relaunch`), the sidecar resolves
//!     the command + cwd from its own SQLite, spawns/kills the off-screen command PTY,
//!     and replies with the `CommandStatus` + streams `command-state`/`command-output`.
//!   * **MCP HTTP answers health + initialize + tools/list** — the in-sidecar MCP
//!     server, started over the pipe (`mcp-start`), serves the v1 surface on loopback.
//!
//! The lifecycle test spawns a real `sh` (the command PTY), so it is gated
//! `#[cfg(not(windows))]`; the MCP test is shell-free and runs on every platform.
#![cfg(not(windows))]

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Seed the sidecar's DB (under `data_dir`) BEFORE spawning the binary: open the SAME
/// `nyx.db` with nyx-core's `Db` (migrations run here), create a project rooted at a
/// real dir + a managed-command template, and return the materialized instance id. The
/// sidecar then opens the already-seeded, already-migrated DB at boot.
fn seed_managed_command(
    data_dir: &std::path::Path,
    root: &std::path::Path,
    command: &str,
) -> String {
    use nyx_core::db::{self, Db};
    // This seed CREATES a legitimate v2 nyx.db, so claim the dir for v2 first (write the
    // ownership marker) — otherwise the sidecar's boot guard (#38) would refuse this dir as
    // a foreign/unmarked DB.
    nyx_sidecar::runtime::db::guard_data_dir_ownership(data_dir).expect("claim v2 data dir");
    let db = Db::open(&data_dir.join("nyx.db")).expect("seed: open db");
    let ids = db.with_conn(|c| {
        let (project, _root_ws) =
            db::create_project(c, "proj", root.to_str().unwrap(), None).unwrap();
        db::create_template(
            c,
            &project.id,
            "dev",
            command,
            None,
            db::CommandSource::default(),
        )
        .unwrap();
        // `create_template` materialized one instance per workspace (just the root).
        db::instance_ids_for_project(c, &project.id).unwrap()
    });
    ids.into_iter().next().expect("a materialized instance id")
}

/// Pick a likely-free fixed port for the in-test MCP server. The nyx MCP server
/// rejects ephemeral port 0 (it must advertise a stable port), so we choose a high
/// port derived from the OS-assigned free port of a throwaway bind — released
/// immediately so the sidecar can claim it.
fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().unwrap().port();
    drop(listener); // release it so the sidecar can bind the same port
    port
}

struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    data_dir: std::path::PathBuf,
}

impl Harness {
    fn spawn(data_dir: std::path::PathBuf) -> Self {
        Self::spawn_with_mcp_port(data_dir, None)
    }

    fn spawn_with_mcp_port(data_dir: std::path::PathBuf, mcp_port: Option<u16>) -> Self {
        let mut cmd = Command::new(sidecar_bin());
        cmd.env("NYX_DATA_DIR", &data_dir).env("SHELL", "/bin/sh");
        // Scrub any AMBIENT `NYX_MCP_PORT` from the test runner's env first: the sidecar seeds every
        // shell it spawns from its OWN env, so a leaked var would make the spawned shell inherit
        // `NYX_MCP_PORT` independently of the pty injection (defeating the `spawned_terminal_*` guard).
        // A test that needs a fixed port then sets it back EXPLICITLY.
        cmd.env_remove("NYX_MCP_PORT");
        if let Some(port) = mcp_port {
            cmd.env("NYX_MCP_PORT", port.to_string());
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut dec = FrameDecoder::new();
            let mut buf = [0u8; 64 * 1024];
            loop {
                loop {
                    match dec.next_frame() {
                        Ok(Some(p)) => {
                            if let Ok(m) = decode_message(&p) {
                                if tx.send(m).is_err() {
                                    return;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(_) => return,
                    }
                }
                match stdout.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => dec.push(&buf[..n]),
                    Err(_) => return,
                }
            }
        });
        let mut h = Harness {
            child,
            stdin,
            rx,
            data_dir,
        };
        match h.recv(Duration::from_secs(5)) {
            Some(Message::Event(Event::Log { .. })) => {}
            other => panic!("expected ready event, got {other:?}"),
        }
        h
    }

    fn send(&mut self, req: Request) {
        let frame = encode_frame(&encode_message(&Message::Request(req))).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        // Timeout and Disconnected both map to None (no message available).
        self.rx.recv_timeout(timeout).ok()
    }

    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let remaining = deadline
                .checked_sub(start.elapsed())
                .expect("timed out waiting for a matching message");
            match self.recv(remaining) {
                Some(msg) => {
                    if let Some(v) = f(&msg) {
                        return v;
                    }
                }
                None => panic!("timed out waiting for a matching message"),
            }
        }
    }

    fn handshake(&mut self) -> Vec<String> {
        self.send(Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::HandshakeOk { capabilities, .. }) => {
                Some(capabilities.clone())
            }
            _ => None,
        })
    }

    /// Drive a `db_command` over the pipe and return its parsed JSON result (or panic on an
    /// error response). Used to set up a terminal record for the SessionStart context test.
    fn db_command(&mut self, id: u64, command: &str, args: serde_json::Value) -> serde_json::Value {
        self.send(Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::DbResult {
                id: rid,
                result_json,
            }) if *rid == id => Some(serde_json::from_str(result_json).unwrap()),
            Message::Response(Response::Error {
                id: rid, message, ..
            }) if *rid == id => panic!("db_command {command} errored: {message}"),
            _ => None,
        })
    }

    /// Drive a `db_command` over the pipe and return the RAW `result_json` string the sidecar
    /// put on the wire — WITHOUT parsing it. The transport invariant is that this is always
    /// valid JSON; the `command_output` regression test asserts that directly (a raw,
    /// un-encoded scrollback would fail `JSON.parse` in the relay, the bug it reproduces).
    fn db_command_raw(&mut self, id: u64, command: &str, args: serde_json::Value) -> String {
        self.send(Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::DbResult {
                id: rid,
                result_json,
            }) if *rid == id => Some(result_json.clone()),
            Message::Response(Response::Error {
                id: rid, message, ..
            }) if *rid == id => panic!("db_command {command} errored: {message}"),
            _ => None,
        })
    }

    fn shutdown(mut self) {
        self.request_shutdown_and_wait(Duration::from_secs(5));
        std::fs::remove_dir_all(&self.data_dir).ok();
    }

    /// Send `Shutdown`, wait for the `ShutdownAck` and the process to exit — but do NOT
    /// delete `data_dir`, so a test can re-open the sidecar's `nyx.db` afterwards and assert
    /// on the DB state the clean shutdown left behind. Borrows `self` (does not consume it).
    fn request_shutdown_and_wait(&mut self, exit_timeout: Duration) {
        self.send(Request::Shutdown { id: 9999 });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::ShutdownAck { .. }) => Some(()),
            _ => None,
        });
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                assert!(status.success(), "clean shutdown exits 0, got {status:?}");
                break;
            }
            if start.elapsed() > exit_timeout {
                let _ = self.child.kill();
                panic!("sidecar did not exit after shutdown");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Start / stop / relaunch a managed command via the sidecar pipe.
#[test]
fn managed_command_lifecycle_via_sidecar() {
    let data_dir = temp_dir("cmd-data");
    let root = temp_dir("cmd-root");
    // A long-lived command so it is observably RUNNING between start and stop.
    let instance_id = seed_managed_command(&data_dir, &root, "sleep 30");

    let mut h = Harness::spawn(data_dir);
    let caps = h.handshake();
    assert!(
        caps.iter().any(|c| c == "commands"),
        "commands capability advertised"
    );

    // START -> the sidecar resolves the command + cwd from its DB, spawns the command
    // PTY, and reports running:true.
    h.send(Request::CommandStart {
        id: 10,
        instance_id: instance_id.clone(),
    });
    let started = h.recv_until(Duration::from_secs(10), |m| match m {
        Message::Response(Response::CommandStatus {
            instance_id: iid,
            running,
            ..
        }) if *iid == instance_id => Some(*running),
        _ => None,
    });
    assert!(started, "the managed command is running after start");

    // RELAUNCH -> restarted:true, still running.
    h.send(Request::CommandRelaunch {
        id: 11,
        instance_id: instance_id.clone(),
    });
    let (restarted, running) = h.recv_until(Duration::from_secs(10), |m| match m {
        Message::Response(Response::CommandStatus {
            instance_id: iid,
            restarted,
            running,
            ..
        }) if *iid == instance_id => Some((*restarted, *running)),
        _ => None,
    });
    assert!(restarted, "relaunch reports restarted:true");
    assert!(running, "the command is running again after relaunch");

    // STOP -> running:false.
    h.send(Request::CommandStop {
        id: 12,
        instance_id: instance_id.clone(),
    });
    let stopped_running = h.recv_until(Duration::from_secs(10), |m| match m {
        Message::Response(Response::CommandStatus {
            instance_id: iid,
            running,
            ..
        }) if *iid == instance_id => Some(*running),
        _ => None,
    });
    assert!(
        !stopped_running,
        "the command is stopped (running:false) after stop"
    );

    h.shutdown();
    std::fs::remove_dir_all(&root).ok();
}

/// REGRESSION #43: a managed command's persisted output reaches a LATE SUBSCRIBER over the
/// transport. Reproduces the user's scenario at the transport level: start a managed command
/// that emits scrollback, let it run and persist, then fetch `command_output` for that instance
/// as a "late subscriber" (the renderer's rehydrate path) and assert the wire result is
/// TRANSPORT-CORRECT.
///
/// The bug was that `command_output` was the only `db_command` arm returning a RAW, un-JSON-
/// encoded String; the relay (`sidecar.ts`) `JSON.parse`s EVERY `db_result.resultJson`, so raw
/// scrollback threw and the renderer swallowed it (`.catch(() => "")`) -> empty pane. This test
/// asserts the wire `result_json` (a) is VALID JSON (the assertion that would have caught the
/// bug — pre-fix it is raw text that fails `serde_json::from_str`) and (b) decodes back to the
/// non-empty scrollback containing the emitted marker, exactly as the renderer's
/// `invoke::<string>("command_output")` receives it.
#[test]
fn command_output_reaches_late_subscriber_as_valid_json() {
    let data_dir = temp_dir("cmd-output-data");
    let root = temp_dir("cmd-output-root");
    // A one-shot command that emits a recognizable marker, then exits — so the runner finishes,
    // the live buffer drains, and the scrollback is persisted to the DB (the cold-history source
    // a late subscriber rehydrates from). The trailing newline mirrors real PTY output.
    const MARKER: &str = "nyx-scrollback-marker-7f3a";
    let instance_id = seed_managed_command(&data_dir, &root, &format!("printf '{MARKER}\\n'"));

    let mut h = Harness::spawn(data_dir);
    let caps = h.handshake();
    assert!(
        caps.iter().any(|c| c == "commands"),
        "commands capability advertised"
    );

    // START -> the command emits the marker and exits. Wait for the terminal-state transition
    // OUT of `running` (success/idle/error) — the point at which the live buffer has drained and
    // the scrollback is persisted, so the subsequent fetch is a genuine LATE subscriber reading
    // cold history (not the live in-memory tail).
    h.send(Request::CommandStart {
        id: 20,
        instance_id: instance_id.clone(),
    });
    let final_state = h.recv_until(Duration::from_secs(10), |m| match m {
        Message::Event(Event::CommandState {
            event,
            instance_id: iid,
            state,
            ..
        }) if event == "state" && *iid == instance_id && state != "running" => Some(state.clone()),
        _ => None,
    });
    assert_eq!(
        final_state, "success",
        "the one-shot command finished successfully"
    );

    // LATE SUBSCRIBER: fetch the RAW wire `result_json` for `command_output` without parsing it.
    let raw = h.db_command_raw(
        21,
        "command_output",
        serde_json::json!({ "instanceId": instance_id }),
    );

    // (a) The wire result is VALID JSON — this is the assertion that catches the regression. The
    // relay does exactly this `JSON.parse`; pre-fix `raw` was the bare scrollback text (e.g.
    // `nyx-scrollback-marker-7f3a\n`) which is NOT valid JSON and throws here.
    let decoded: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!("command_output result_json must be valid JSON (the relay JSON.parses it); got raw {raw:?}: {e}")
    });

    // (b) It decodes to a JSON STRING (a string literal), and that string is the non-empty
    // scrollback containing the emitted marker — exactly what `invoke::<string>(...)` receives.
    let scrollback = decoded
        .as_str()
        .unwrap_or_else(|| panic!("command_output decodes to a JSON string, got {decoded:?}"));
    assert!(
        !scrollback.is_empty(),
        "the late subscriber gets the NON-EMPTY persisted scrollback (backlog replays)"
    );
    assert!(
        scrollback.contains(MARKER),
        "the rehydrated scrollback contains the emitted output; got {scrollback:?}"
    );

    // Belt-and-suspenders: the result round-trips through the SAME parse the renderer's relay
    // does (`sidecar.ts:609`), proving the pre-fix raw-String transport is now impossible.
    let reparsed: String = serde_json::from_str(&raw).expect("round-trips through JSON.parse");
    assert_eq!(
        reparsed, scrollback,
        "JSON.parse(result_json) yields the exact scrollback string"
    );

    h.shutdown();
    std::fs::remove_dir_all(&root).ok();
}

/// The in-sidecar MCP HTTP server answers health + initialize + tools/list.
#[test]
fn mcp_http_answers_health_initialize_and_tools_list() {
    let data_dir = temp_dir("mcp-data");
    let mcp_port = pick_free_port();
    let mut h = Harness::spawn_with_mcp_port(data_dir, Some(mcp_port));
    let caps = h.handshake();
    assert!(caps.iter().any(|c| c == "mcp"), "mcp capability advertised");

    // Start the MCP server over the pipe; it returns the bound loopback port.
    h.send(Request::McpStart { id: 50 });
    let port = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Response(Response::McpStarted { port, .. }) => Some(*port),
        _ => None,
    });
    assert_eq!(
        port, mcp_port,
        "MCP server bound the requested loopback port"
    );

    let base = format!("http://127.0.0.1:{port}");

    // HEALTH — a plain GET that proves the server is up. Assert the real response
    // shape: a `200 OK` status line plus the JSON health body `{"status":"ok",...}`
    // (not just any non-empty bytes).
    let health = http_get(&base, "/health");
    assert!(
        health.starts_with("HTTP/1.1 200"),
        "health endpoint returns a 200 status line: {health}"
    );
    assert!(
        health.contains("\"status\":\"ok\""),
        "health endpoint body reports status ok: {health}"
    );

    // INITIALIZE — the MCP handshake (JSON-RPC over POST /mcp).
    let init = http_post_jsonrpc(
        &base,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
    );
    assert!(
        init.contains("\"result\"") && init.contains("protocolVersion")
            || init.contains("serverInfo"),
        "initialize returned a result: {init}"
    );

    // TOOLS/LIST — the advertised tool surface.
    let tools = http_post_jsonrpc(
        &base,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );
    assert!(
        tools.contains("\"tools\""),
        "tools/list returned a tools array: {tools}"
    );
    // A representative v1 tool must be advertised (proves the FULL surface, not a stub).
    assert!(
        tools.contains("list_projects") || tools.contains("create_terminal"),
        "tools/list advertises the v1 tool surface: {tools}"
    );

    h.shutdown();
}

/// REVIEW `01KXBKHDC7V605B318YM0KPHDH` / task `01KXBKJKTNN5WQYTH8D9G5310H` (CRITICAL): a clean
/// QUIT must NOT destroy an active session's resume. The v2 bug: `Runtime::shutdown` closed the
/// PTYs (SIGKILL each interactive shell) while the MCP HTTP server was STILL listening, so the
/// `SessionEnd(reason=other)` hook the dying Claude fires reached the live server and
/// `mark_session_ended` the row — decide_resume then skipped it forever. The fix stops the MCP
/// listener FIRST (before `self.pty.close_all()`), so that late hook hits a REFUSED socket and the
/// session stays `active` (parity with v1 + the crash reaper).
///
/// This exercises the REAL shutdown ORDER end-to-end (not a seeded session): a live interactive
/// PTY hosts a bash descendant whose `SIGHUP` trap POSTs the EXACT `agent_session_event` SessionEnd
/// the Claude hook posts. When `Runtime::shutdown` SIGKILLs the PTY's session-leader shell, the
/// kernel hangs up the terminal → the descendant gets SIGHUP → it tries to POST the end. Because
/// `close_all` joins the PTY pump (which only EOFs after the descendant exits), the shutdown BLOCKS
/// until that POST attempt resolves — making the order deterministic:
///   * FIX (MCP stopped first): the POST hits a refused socket → the session stays `active`.
///   * OLD ORDER (MCP stopped after `close_all`): the POST lands on the live server → `ended`.
///
/// So this test goes RED if the stop is moved back after `close_all`.
#[test]
fn clean_shutdown_stops_mcp_before_pty_so_session_end_hook_cannot_end_the_session() {
    // The trap POSTs via bash's `/dev/tcp`; skip cleanly where bash is unavailable.
    if Command::new("bash")
        .arg("-c")
        .arg("exit 0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: bash not available for the /dev/tcp SessionEnd trap");
        return;
    }

    let data_dir = temp_dir("mcp-shutdown-order");
    let scratch = temp_dir("mcp-shutdown-scratch");
    let mcp_port = pick_free_port();
    let mut h = Harness::spawn_with_mcp_port(data_dir, Some(mcp_port));
    h.handshake();

    // A real terminal record the session (and the PTY) bind to.
    let terminal = h.db_command(
        60,
        "create_terminal",
        serde_json::json!({ "cwd": scratch.to_str().unwrap() }),
    );
    let record_id = terminal
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    // Start the in-sidecar MCP server; capture the bound loopback port.
    h.send(Request::McpStart { id: 61 });
    let port = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Response(Response::McpStarted { port, .. }) => Some(*port),
        _ => None,
    });
    assert_eq!(
        port, mcp_port,
        "MCP bound the requested port (0 ⇒ bind failed)"
    );
    let base = format!("http://127.0.0.1:{port}");

    // POST a real SessionStart so the terminal owns an ACTIVE claude_code session (the exact row
    // the resume decision reads). This is the genuine hook path, not a DB seed.
    const EXTERNAL_ID: &str = "sess-shutdown-order-1";
    let start_body = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"agent_session_event","arguments":{{"hook_event_name":"SessionStart","session_id":"{EXTERNAL_ID}","source":"startup","cwd":"{cwd}","NYX_TERMINAL_ID":"{record_id}"}}}}}}"#,
        cwd = scratch.to_str().unwrap(),
    );
    let start_resp = http_post_jsonrpc(&base, &start_body);
    assert!(
        start_resp.contains("\"event\":\"SessionStart\""),
        "SessionStart recorded the active session: {start_resp}"
    );

    // Pre-build the EXACT bytes of the SessionEnd(reason=other) request the dying Claude's hook
    // would POST, and stage them in a file the trap streams to the socket.
    let end_body = format!(
        r#"{{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{{"name":"agent_session_event","arguments":{{"hook_event_name":"SessionEnd","session_id":"{EXTERNAL_ID}","reason":"other","NYX_TERMINAL_ID":"{record_id}"}}}}}}"#,
    );
    let end_request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{end_body}",
        end_body.len(),
    );
    let req_path = scratch.join("session_end.http");
    std::fs::write(&req_path, end_request.as_bytes()).unwrap();

    // The SessionEnd-on-hangup hook, faithful to the Claude SessionEnd hook: on SIGHUP (the
    // terminal hangup the sidecar's PTY kill triggers) stream the staged request to the MCP port.
    // A write-only `/dev/tcp` redirection on a SIMPLE command (not `exec`) fails HARMLESSLY when
    // the port is refused — the shell just moves on to `exit 0`, so the trap is bounded either way.
    let hook_path = scratch.join("session_end_hook.sh");
    std::fs::write(
        &hook_path,
        format!(
            "post_end() {{ cat {req} > /dev/tcp/127.0.0.1/{port} 2>/dev/null; exit 0; }}\n\
             trap post_end HUP\n\
             echo TRAP_READY\n\
             sleep 30\n",
            req = req_path.to_str().unwrap(),
        ),
    )
    .unwrap();

    // Spawn a REAL interactive PTY for the record and run the hook host inside it (a bash CHILD of
    // the leader shell, so it receives the terminal SIGHUP when the leader is killed at shutdown).
    h.send(Request::PtySpawn {
        id: 62,
        cols: 80,
        rows: 24,
        cwd: String::new(),
        record_id: record_id.clone(),
    });
    let pty_id = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Response(Response::PtySpawned { terminal_id, .. }) => Some(*terminal_id),
        _ => None,
    });
    h.send(Request::PtyWrite {
        id: 63,
        terminal_id: pty_id,
        data: format!("bash {}\n", hook_path.to_str().unwrap()).into_bytes(),
    });
    // Wait until the trap is armed (the hook printed TRAP_READY) so shutdown cannot race ahead of
    // it — the SessionEnd must be able to fire, exactly as a live Claude's would.
    let mut seen: Vec<u8> = Vec::new();
    let armed = h.recv_until(Duration::from_secs(10), |m| match m {
        Message::Event(Event::TerminalOutput { data, .. }) => {
            seen.extend_from_slice(data);
            String::from_utf8_lossy(&seen)
                .contains("TRAP_READY")
                .then_some(true)
        }
        _ => None,
    });
    assert!(
        armed,
        "the SessionEnd-on-SIGHUP trap is armed in the live PTY"
    );

    // CLEAN QUIT: drive the real `Runtime::shutdown`. With the fix, the MCP listener is closed
    // BEFORE the PTYs, so the hook's SessionEnd POST hits a refused socket. Give the exit a
    // generous window (the shutdown blocks on the PTY pump join, which waits for the trap).
    h.request_shutdown_and_wait(Duration::from_secs(20));

    // Re-open the DB the (now-exited) sidecar left behind and assert the session is STILL active —
    // the SessionEnd could NOT reach the already-closed MCP, so decide_resume can still resume it.
    // Pre-fix (MCP stopped after close_all) the hook would have landed and marked it `ended`.
    {
        use nyx_core::db::{self, Db};
        let db = Db::open(&h.data_dir.join("nyx.db")).expect("re-open nyx.db after shutdown");
        let sessions = db
            .with_conn(|c| db::sessions_for_terminal(c, &record_id))
            .expect("read sessions for terminal");
        let target = sessions
            .iter()
            .find(|s| s.external_session_id == EXTERNAL_ID)
            .unwrap_or_else(|| panic!("the started session must exist; got {sessions:?}"));
        assert_eq!(
            target.state,
            db::SESSION_STATE_ACTIVE,
            "a clean quit must leave the running session ACTIVE (the SessionEnd hook of the killed \
             Claude could not reach the MCP, which was stopped before the PTYs); got {:?}",
            target.state
        );
    }

    std::fs::remove_dir_all(&h.data_dir).ok();
    std::fs::remove_dir_all(&scratch).ok();
}

/// Minimal HTTP/1.1 GET over a raw TCP socket (no http client dep). Returns the full
/// response (status line + headers + body).
fn http_get(base: &str, path: &str) -> String {
    use std::net::TcpStream;
    let addr = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).expect("connect mcp");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    out
}

/// Minimal HTTP/1.1 POST of a JSON-RPC body to `POST /mcp`, returning the response.
fn http_post_jsonrpc(base: &str, body: &str) -> String {
    use std::net::TcpStream;
    let addr = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).expect("connect mcp");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    out
}

/// EASG1 DONE-CRITERION 2: a started session RECEIVES the context injected by the hook.
///
/// The plugin's SessionStart hook POSTs `agent_session_event` to the sidecar's MCP server and
/// relays the returned `structuredContent.context` to Claude as `additionalContext`. This
/// drives that exact path: create a terminal record (so the session binds to a real terminal),
/// then POST a SessionStart `agent_session_event` to the in-sidecar MCP server and assert the
/// response carries the injected `context` line — the proof the hook's context injection works
/// end-to-end against the runtime that now lives IN the sidecar.
#[test]
fn session_start_returns_injected_context_for_the_hook() {
    let data_dir = temp_dir("mcp-ctx");
    let mcp_port = pick_free_port();
    let mut h = Harness::spawn_with_mcp_port(data_dir, Some(mcp_port));
    h.handshake();

    // A real terminal record the session binds to (the hook passes its id as NYX_TERMINAL_ID).
    let terminal = h.db_command(
        60,
        "create_terminal",
        serde_json::json!({ "cwd": "/tmp/work" }),
    );
    let terminal_id = terminal
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    // Start the in-sidecar MCP server (the plugin's `.mcp.json` points the hook at it).
    h.send(Request::McpStart { id: 61 });
    let port = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Response(Response::McpStarted { port, .. }) => Some(*port),
        _ => None,
    });
    let base = format!("http://127.0.0.1:{port}");

    // POST the SessionStart `agent_session_event` exactly as the hook does (a JSON-RPC
    // tools/call with the Claude hook payload + the injected NYX_TERMINAL_ID).
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"agent_session_event","arguments":{{"hook_event_name":"SessionStart","session_id":"sess-ctx-1","source":"startup","cwd":"/tmp/work","NYX_TERMINAL_ID":"{terminal_id}"}}}}}}"#,
    );
    let resp = http_post_jsonrpc(&base, &body);

    // The response carries the injected context line in `structuredContent.context` — the
    // exact field the hook forwards as `additionalContext`. For a (loose) terminal it is
    // "You're in nyx — terminal_id=<tid>".
    assert!(
        resp.contains("\"context\"") && resp.contains("You're in nyx"),
        "SessionStart returned the injected context for the hook to relay: {resp}"
    );
    assert!(
        resp.contains(&format!("terminal_id={terminal_id}")),
        "the injected context situates the session by its terminal id: {resp}"
    );

    h.shutdown();
}

/// REVIEW `01KXAXQN32GASMEB05PDW5BZSR` (agent icon dark in v2): the plugin's SessionStart hook
/// POSTs `agent_session_event` to `http://127.0.0.1:${NYX_MCP_PORT||8765}/mcp`, but the PTY spawn
/// never exported `NYX_MCP_PORT`, so the hook fell back to 8765 — the v1 cockpit under a port
/// collision — and v2's `agent_sessions` stayed empty. This proves the fix END-TO-END through the
/// REAL dispatch/MCP (no in-process bypass):
///   (A) a terminal spawned by the sidecar exports `NYX_MCP_PORT` == the port the MCP server
///       ACTUALLY bound (read straight off the live shell's env, off the response's `McpStarted`
///       port). The sidecar is launched with a FREE/ephemeral `NYX_MCP_PORT` (so this test binds an
///       OS-picked free port and never collides with a co-running nyx-v2 on the fixed 8766). Even
///       though the sidecar therefore CARRIES `NYX_MCP_PORT` in its own env — which `CommandBuilder`
///       would otherwise seed into the shell — the pty spawn SCRUBS any inherited `NYX_MCP_PORT`
///       before injecting the authoritative bound port, so the shell cannot obtain the port by
///       INHERITANCE and its only remaining source is the pty injection (`pty.rs`). That keeps (A) a
///       real GUARD: delete the injection and the scrubbed child sees no `NYX_MCP_PORT`, so (A) turns
///       red. (This part does NOT prove "not a hard-coded 8765/8766"; the strict "dynamic port,
///       outside the parent env" proof lives in the `nyx-core` unit test
///       `nyx_mcp_port_is_exported_to_unix_shell`, port 49234.)
///   (B) a `SessionStart` `agent_session_event` POSTed to that SAME bound port (exactly as the
///       hook does) is dispatched by the REAL in-sidecar MCP and returns a `claude_code`
///       SessionStart record for the spawned terminal; AND
///   (C) that row PERSISTED in THIS instance's `agent_sessions` — re-read straight off the on-disk
///       DB — which is the exact data the sidebar reads to light the agent icon (done_criterion 3).
#[test]
fn spawned_terminal_exports_bound_mcp_port_and_records_session_on_it() {
    // Bind a FREE/ephemeral port (OS-picked) instead of the fixed v2 `DEFAULT_PORT` (8766): a
    // co-running nyx-v2 app already holds 8766, so a fixed bind here would fail with `AddrInUse` and
    // make this test flaky-red whenever a live nyx is up. `CommandBuilder::new` seeds the spawned
    // shell's env from the sidecar's OWN `std::env::vars_os()`, so the sidecar DOES carry
    // `NYX_MCP_PORT` (this free port) and the shell would normally INHERIT it — which would let part
    // (A) below pass even with the pty injection deleted (defeating the guard). The pty spawn defends
    // against exactly that by SCRUBBING any inherited `NYX_MCP_PORT` before injecting the authoritative
    // bound port, so the shell's `NYX_MCP_PORT` can ONLY come from the pty injection — (A) stays a true
    // guard (delete the injection → the scrubbed child sees no port → (A) goes red).
    let data_dir = temp_dir("mcp-agent-route");
    let mcp_port = pick_free_port();
    let mut h = Harness::spawn_with_mcp_port(data_dir, Some(mcp_port));
    h.handshake();

    // A real terminal record the session binds to (its id is the hook's NYX_TERMINAL_ID).
    let terminal = h.db_command(
        60,
        "create_terminal",
        serde_json::json!({ "cwd": "/tmp/route" }),
    );
    let record_id = terminal
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    // Start the in-sidecar MCP server; it returns the port it ACTUALLY bound. This is the value
    // the sidecar injects into every terminal it spawns and re-templates into the `.mcp.json`.
    h.send(Request::McpStart { id: 61 });
    let port = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Response(Response::McpStarted { port, .. }) => Some(*port),
        _ => None,
    });
    // The sidecar binds the FREE port we requested (an OS-picked ephemeral port, released just before
    // the sidecar re-binds it) rather than the fixed 8766 a co-running nyx-v2 may already hold — so
    // this test never collides with a live nyx. A returned 0 means the bind FAILED (e.g. another
    // process grabbed the port between pick and bind); the assert surfaces that legibly instead of a
    // confusing needle mismatch downstream.
    assert_eq!(
        port, mcp_port,
        "the sidecar binds the requested free MCP port (0 ⇒ bind failed)"
    );
    assert_ne!(port, 8765, "the bound port is never the v1 cockpit's port");

    // (A) Spawn a REAL interactive PTY for that record and have the live shell echo NYX_MCP_PORT.
    //     This exercises the exact sidecar→pty wiring the fix adds: the bound port reaches the
    //     shell env (and thus the Claude plugin's hook running inside it). Because the pty spawn
    //     SCRUBS any inherited NYX_MCP_PORT before injecting the bound port, the ONLY way the shell
    //     can print the bound port is the pty injection — so this assertion GUARDS `pty.rs`'s export
    //     (delete it → the scrubbed child prints nothing → this goes red).
    h.send(Request::PtySpawn {
        id: 62,
        cols: 80,
        rows: 24,
        cwd: String::new(),
        record_id: record_id.clone(),
    });
    let pty_id = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Response(Response::PtySpawned { terminal_id, .. }) => Some(*terminal_id),
        _ => None,
    });
    h.send(Request::PtyWrite {
        id: 63,
        terminal_id: pty_id,
        // The expanded value `NYXPORT=[<port>]` appears only in the OUTPUT, never in the echoed
        // command line (which carries the literal `$NYX_MCP_PORT`), so the needle can't false-match.
        data: b"echo \"NYXPORT=[$NYX_MCP_PORT]\"\n".to_vec(),
    });
    let needle = format!("NYXPORT=[{port}]");
    let mut seen: Vec<u8> = Vec::new();
    let injected = h.recv_until(Duration::from_secs(10), |m| match m {
        Message::Event(Event::TerminalOutput { data, .. }) => {
            seen.extend_from_slice(data);
            String::from_utf8_lossy(&seen)
                .contains(&needle)
                .then_some(true)
        }
        _ => None,
    });
    assert!(
        injected,
        "the spawned terminal must export NYX_MCP_PORT == the bound port {port} (its only source \
         is the pty injection — the pty spawn scrubs any inherited NYX_MCP_PORT); got: {}",
        String::from_utf8_lossy(&seen)
    );

    // (B) POST the SessionStart `agent_session_event` to the BOUND port exactly as the hook does.
    let base = format!("http://127.0.0.1:{port}");
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"agent_session_event","arguments":{{"hook_event_name":"SessionStart","session_id":"sess-route-1","source":"startup","cwd":"/tmp/route","NYX_TERMINAL_ID":"{record_id}"}}}}}}"#,
    );
    let resp = http_post_jsonrpc(&base, &body);
    assert!(
        resp.contains("\"event\":\"SessionStart\""),
        "the tool on the bound port recorded the session start: {resp}"
    );
    assert!(
        resp.contains("\"agent_kind\":\"claude_code\""),
        "the recorded session is a claude_code session: {resp}"
    );
    assert!(
        resp.contains(&format!("\"terminal_id\":\"{record_id}\"")),
        "the session binds to the spawned terminal record: {resp}"
    );

    // (C) Confirm the row PERSISTED in THIS instance's DB (not merely echoed): re-open the same
    //     nyx.db (WAL allows a concurrent reader) and assert active_agent_sessions carries a
    //     claude_code row for the record — the exact data the sidebar reads to light the icon.
    {
        use nyx_core::db::{self, Db};
        let db = Db::open(&h.data_dir.join("nyx.db")).expect("re-open nyx.db for readback");
        let rows = db
            .with_conn(db::active_agent_sessions)
            .expect("read active agent sessions");
        assert!(
            rows.iter()
                .any(|s| s.terminal_id == record_id && s.agent_kind == db::AGENT_KIND_CLAUDE_CODE),
            "agent_session_event on the bound port wrote a claude_code agent_sessions row for {record_id}; got {rows:?}"
        );
    }

    h.send(Request::PtyClose {
        id: 64,
        terminal_id: pty_id,
    });
    h.shutdown();
}
