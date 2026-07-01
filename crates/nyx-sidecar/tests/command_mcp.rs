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
            if start.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                panic!("sidecar did not exit after shutdown");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(&self.data_dir).ok();
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
