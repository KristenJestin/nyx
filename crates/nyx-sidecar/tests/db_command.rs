//! Integration tests that spawn the REAL `nyx-sidecar` binary and drive the GENERIC
//! `db_command` path (G0) over the framed MessagePack pipe — exactly the way Electron main
//! will through the `Sidecar` manager + `core-ipc` relay.
//!
//! This is the end-to-end proof of the EASG0 wiring at the pipe boundary: the supervisor
//! handshakes, then drives the WHOLE non-PTY contract surface the renderer's nyxBridge
//! invokes (`list_terminals`, `create_terminal`, `list_projects`, `integration_list`,
//! `agent_activity_snapshot`, `terminal_info`, …) as `db_command` requests and reads back
//! the JSON result string the relay parses into the contract shape — AND it proves a LIVE
//! terminal (a `pty_spawn` + a typed command) streams binary `terminal_output`, the exact
//! path the renderer's live terminal rides.
//!
//! Spawns a real `sh`, so it is gated `#[cfg(not(windows))]` (same environmental category as
//! the other `sh`-spawning sidecar tests); runs GREEN on the Linux target.
#![cfg(not(windows))]

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

fn temp_data_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-dbcmd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Harness over the real binary with a background stdout reader (same shape as the PTY-flow
/// harness): bounded, timeout-respecting receives.
struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    data_dir: std::path::PathBuf,
    next_id: u64,
}

impl Harness {
    fn spawn() -> Self {
        let data_dir = temp_data_dir();
        let mut child = Command::new(sidecar_bin())
            .env("NYX_DATA_DIR", &data_dir)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("dbcmd-test-reader".into())
            .spawn(move || {
                let mut dec = FrameDecoder::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    loop {
                        match dec.next_frame() {
                            Ok(Some(payload)) => {
                                if let Ok(msg) = decode_message(&payload) {
                                    if tx.send(msg).is_err() {
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
            })
            .unwrap();

        let mut h = Harness {
            child,
            stdin,
            rx,
            data_dir,
            next_id: 100,
        };
        // Drain the leading `ready` Log event.
        match h.recv(Duration::from_secs(5)) {
            Some(Message::Event(Event::Log { .. })) => {}
            other => panic!("expected ready event, got {other:?}"),
        }
        h.handshake();
        h
    }

    fn send(&mut self, req: Request) {
        let frame = encode_frame(&encode_message(&Message::Request(req))).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let remaining = match deadline.checked_sub(start.elapsed()) {
                Some(r) => r,
                None => panic!("timed out waiting for a matching message"),
            };
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

    fn handshake(&mut self) {
        self.send(Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::HandshakeOk { .. }) => Some(()),
            _ => None,
        });
    }

    /// Drive a `db_command` request and return the parsed JSON result. A `db_result` ->
    /// `Ok(value)`; an `error` -> `Err(message)`.
    fn db_command(
        &mut self,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::DbResult {
                id: rid,
                result_json,
            }) if *rid == id => Some(Ok(serde_json::from_str(result_json).unwrap())),
            Message::Response(Response::Error {
                id: rid, message, ..
            }) if *rid == id => Some(Err(message.clone())),
            _ => None,
        })
    }

    fn shutdown(mut self) {
        self.send(Request::Shutdown { id: 9999 });
        self.recv_until(Duration::from_secs(5), |m| {
            matches!(m, Message::Response(Response::ShutdownAck { .. })).then_some(())
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

/// The renderer boots by reading the DB-backed collections, then mutating them. Drive the
/// SAME `db_command` calls the renderer's nyxBridge issues and prove they round-trip the real
/// sidecar runtime end-to-end (the path that was missing — the pipe carried only PTY before).
#[test]
fn db_command_round_trips_the_renderer_boot_surface() {
    let mut h = Harness::spawn();

    // 1. The boot reads: an empty DB returns empty collections (never an error).
    let terms = h
        .db_command("list_terminals", serde_json::json!({}))
        .unwrap();
    assert_eq!(
        terms.as_array().unwrap().len(),
        0,
        "fresh DB has no terminals"
    );
    let projects = h
        .db_command("list_projects", serde_json::json!({}))
        .unwrap();
    assert_eq!(
        projects.as_array().unwrap().len(),
        0,
        "fresh DB has no projects"
    );

    // 2. A mutation persists and the next read reflects it (the create_terminal CRUD the
    //    sidebar's "new terminal" gesture invokes).
    let created = h
        .db_command(
            "create_terminal",
            serde_json::json!({ "cwd": "/tmp", "label": "t1" }),
        )
        .unwrap();
    let term_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert!(!term_id.is_empty(), "create_terminal returned a record id");
    let terms = h
        .db_command("list_terminals", serde_json::json!({}))
        .unwrap();
    assert_eq!(
        terms.as_array().unwrap().len(),
        1,
        "the created terminal is listed"
    );

    // 3. The integrations surface (Settings -> Integrations) lists the providers WITHOUT a
    //    home dir / claude install (a pure status read), proving the integrations core is
    //    reachable over the pipe (G1 installs through the same seam).
    let integrations = h
        .db_command("integration_list", serde_json::json!({}))
        .unwrap();
    assert!(
        integrations
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.get("provider").and_then(|p| p.as_str()).is_some()),
        "integration_list returns at least one provider status"
    );

    // 4. The runtime agent-activity snapshot (the live dot) is EMPTY at boot (the
    //    anti-phantom guarantee) — a synchronous in-memory read over the pipe.
    let activity = h
        .db_command("agent_activity_snapshot", serde_json::json!({}))
        .unwrap();
    assert_eq!(
        activity.as_array().unwrap().len(),
        0,
        "no live activity at boot"
    );

    // 5. An UNKNOWN command is a readable structured error, never a hang (the allowlist stays
    //    honest — the relay surfaces it as a `command` BridgeError).
    let err = h
        .db_command("definitely_not_a_command", serde_json::json!({}))
        .unwrap_err();
    assert!(
        err.contains("not available"),
        "unknown command yields a readable error, got {err}"
    );

    h.shutdown();
}

/// The LIVE TERMINAL path the renderer rides for DONE criterion (a): a `pty_spawn` over the
/// pipe yields a live terminal id, a typed command streams BINARY `terminal_output`, and
/// `terminal_info` reads the live cwd back through the same `db_command` seam the auto-label
/// uses. This is the exact sequence the `Sidecar` manager performs for the renderer's xterm.
#[test]
fn live_terminal_streams_output_and_terminal_info_reads_cwd() {
    let mut h = Harness::spawn();

    // Spawn an interactive PTY (the renderer's `pty_spawn`).
    let spawn_id = h.next_id;
    h.next_id += 1;
    h.send(Request::PtySpawn {
        id: spawn_id,
        cols: 80,
        rows: 24,
        cwd: String::new(),
        record_id: String::new(),
    });
    let pty_id = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Response(Response::PtySpawned { terminal_id, .. }) => Some(*terminal_id),
        _ => None,
    });

    // Type a command (the renderer's keystroke -> `pty_write`), then observe its output as
    // BINARY `terminal_output` frames (the path the renderer's `pty://output` rides).
    let write_id = h.next_id;
    h.next_id += 1;
    h.send(Request::PtyWrite {
        id: write_id,
        terminal_id: pty_id,
        data: b"echo nyx-live-ok\n".to_vec(),
    });
    let mut seen = Vec::new();
    h.recv_until(Duration::from_secs(8), |m| match m {
        Message::Event(Event::TerminalOutput { data, .. }) => {
            seen.extend_from_slice(data);
            if String::from_utf8_lossy(&seen).contains("nyx-live-ok") {
                Some(())
            } else {
                None
            }
        }
        _ => None,
    });

    // `terminal_info` over the `db_command` seam reads the live cwd from `/proc` (the
    // auto-label source) — proving the live-PTY introspection traverses the same pipe.
    let info = h
        .db_command("terminal_info", serde_json::json!({ "id": pty_id }))
        .unwrap();
    // On Linux the cwd is present; the field always exists (null on a non-/proc platform).
    assert!(
        info.get("cwd").is_some(),
        "terminal_info returns a cwd field"
    );

    h.shutdown();
}
