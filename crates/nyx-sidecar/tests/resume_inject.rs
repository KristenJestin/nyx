//! Integration tests for the LAZY agent-session resume INJECTION (v2.5.5), driving the REAL
//! `nyx-sidecar` binary over the framed pipe exactly as Electron main does. They prove the
//! park + inject mechanism end-to-end through a real `sh`:
//!
//!   * **spawn → park (immediate inject)** — a terminal's PTY is spawned, then `set_resume_parks`
//!     pushes a resume for its record: the terminal is already live, so the command is injected
//!     immediately and its output is observed. (The deck eager-mounts every terminal, so at
//!     relaunch the spawn usually precedes the restore plan — this ordering must resume.)
//!   * **park → spawn (spawn drain)** — `set_resume_parks` is pushed BEFORE the PTY spawns; the
//!     park is drained and injected at the spawn. (A terminal opened after the plan arrives.)
//!   * **idempotent (one-shot)** — a second park for the same record after the first injection is
//!     ignored: no double resume.
//!
//! The injected line is a benign `printf` whose marker is observed in the binary output (the
//! shell RUNS the injected command exactly as it runs a typed one). `claude` need not exist — the
//! point is that the parked command reaches the shell exactly once. Gated `#[cfg(not(windows))]`
//! (spawns a real `sh`), GREEN on Linux — the same category as `pty_flow.rs`.
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
        "nyx-sidecar-resume-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Harness over the real binary — a background reader thread drains stdout into a channel of
/// decoded `Message`s (mirrors `pty_flow.rs`).
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
            .name("resume-test-reader".into())
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
        match h.recv(Duration::from_secs(5)) {
            Some(Message::Event(Event::Log { .. })) => {}
            other => panic!("expected ready event, got {other:?}"),
        }
        h.handshake();
        h
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn send(&mut self, req: Request) {
        let frame = encode_frame(&encode_message(&Message::Request(req))).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let Some(remaining) = deadline.checked_sub(start.elapsed()) else {
                panic!("timed out waiting for a matching message");
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
        let id = self.id();
        self.send(Request::Handshake {
            id,
            protocol_version: PROTOCOL_VERSION,
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::HandshakeOk { .. }) => Some(()),
            _ => None,
        });
    }

    /// Spawn a PTY bound to a persistent RECORD id (so a parked resume can key onto it).
    fn spawn_pty_record(&mut self, record_id: &str) -> u32 {
        let id = self.id();
        self.send(Request::PtySpawn {
            id,
            cols: 80,
            rows: 24,
            cwd: String::new(),
            record_id: record_id.to_string(),
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::PtySpawned { terminal_id, .. }) => Some(*terminal_id),
            _ => None,
        })
    }

    /// Push a single resume park (the `set_resume_parks` db-command the renderer drives).
    fn set_resume_park(&mut self, record_id: &str, session_id: &str, command: &str) {
        let id = self.id();
        let args = serde_json::json!({
            "parks": [ { "terminalId": record_id, "sessionId": session_id, "command": command } ]
        });
        self.send(Request::DbCommand {
            id,
            command: "set_resume_parks".to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::DbResult { .. }) => Some(()),
            _ => None,
        });
    }

    fn write_line(&mut self, terminal_id: u32, line: &str) {
        let id = self.id();
        self.send(Request::PtyWrite {
            id,
            terminal_id,
            data: line.as_bytes().to_vec(),
        });
    }

    /// Collect binary terminal output until `needle` appears; returns everything seen so far.
    fn read_until_marker(&mut self, needle: &str) -> String {
        let mut seen = Vec::new();
        self.recv_until(Duration::from_secs(10), |m| match m {
            Message::Event(Event::TerminalOutput { data, .. }) => {
                seen.extend_from_slice(data);
                String::from_utf8_lossy(&seen)
                    .contains(needle)
                    .then_some(())
            }
            _ => None,
        });
        String::from_utf8_lossy(&seen).into_owned()
    }

    fn shutdown(mut self) {
        let id = self.id();
        self.send(Request::Shutdown { id });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::ShutdownAck { .. }) => Some(()),
            _ => None,
        });
        let start = Instant::now();
        loop {
            if let Some(_status) = self.child.try_wait().expect("try_wait") {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(&self.data_dir).ok();
    }
}

/// SPAWN then PARK: the terminal's PTY is already live when the resume is pushed, so it is
/// injected IMMEDIATELY and its output is observed. Then a SECOND park for the same record is
/// pushed and a sentinel typed: the second resume must NOT appear before the sentinel (the
/// one-shot guard blocks a double resume).
#[test]
fn spawn_then_park_injects_once_and_is_idempotent() {
    let mut h = Harness::spawn();
    let record = "rec-immediate";
    let terminal_id = h.spawn_pty_record(record);

    // Push the resume AFTER spawn → immediate inject into the live shell.
    h.set_resume_park(record, "sess-1", "printf 'NYX_RESUME_ONE\\n'");
    let out = h.read_until_marker("NYX_RESUME_ONE");
    assert!(
        out.contains("NYX_RESUME_ONE"),
        "the parked resume was injected into the already-live terminal"
    );

    // A SECOND park for the same record is ignored (one-shot). Prove it via ordering: type a
    // sentinel AFTER the second park; the sentinel must show, the second resume must NOT precede it.
    h.set_resume_park(record, "sess-1", "printf 'NYX_RESUME_TWO\\n'");
    h.write_line(terminal_id, "printf 'NYX_SENTINEL\\n'\n");
    let out2 = h.read_until_marker("NYX_SENTINEL");
    assert!(
        !out2.contains("NYX_RESUME_TWO"),
        "a second park for the same record must not re-inject (one-shot idempotent): {out2:?}"
    );

    h.shutdown();
}

/// PARK then SPAWN: the resume is pushed BEFORE the terminal's PTY exists, so it is DRAINED and
/// injected at the spawn (the spawn-drain path). Its output is then observed.
#[test]
fn park_then_spawn_injects_at_spawn() {
    let mut h = Harness::spawn();
    let record = "rec-parked";

    // Park BEFORE the terminal is live → stored, awaiting the spawn.
    h.set_resume_park(record, "sess-2", "printf 'NYX_RESUME_ATSPAWN\\n'");
    // Now spawn the terminal's PTY → the park drains and injects.
    let _terminal_id = h.spawn_pty_record(record);
    let out = h.read_until_marker("NYX_RESUME_ATSPAWN");
    assert!(
        out.contains("NYX_RESUME_ATSPAWN"),
        "a resume parked before spawn is injected when the PTY spawns"
    );

    h.shutdown();
}
