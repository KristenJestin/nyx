//! Integration tests that spawn the REAL `nyx-sidecar` binary and drive the
//! interactive-PTY path (HHC) over the framed MessagePack pipe — exactly the way
//! Electron main will: `pty-spawn`, `pty-write`, the binary `terminal_output`
//! events, the flow-control `pty-ack` credit, and `pty-close`.
//!
//! Done-criteria covered:
//!   * **open + type + observe** — a terminal is opened, a command is typed into it,
//!     and its output is observed as BINARY `terminal_output` frames (no base64).
//!   * **flow-control under load** — a flood (`yes`) is started and NOT acked; the
//!     per-terminal in-flight quota saturates at the high-water mark and the pump
//!     PAUSES the reader (the pause-gate holds): in-flight bytes stay BOUNDED (no
//!     unbounded memory growth) and the stream is LOSSLESS (the contiguous `seq`
//!     ordering is preserved). Then an ack drains the backlog and the pump RESUMES.
//!
//! These spawn a real `sh`, so they are gated `#[cfg(not(windows))]` (joining the
//! same environmental-Windows-red category as the `sh`-spawning `nyx-core` tests);
//! they run GREEN on the Linux target.
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
        "nyx-sidecar-pty-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Harness over the real binary. A background reader thread continuously drains the
/// sidecar's stdout into a channel of decoded `Message`s, so the test can do BOUNDED,
/// timeout-respecting receives even while the producer is paused at the gate (a
/// blocking OS read can never wedge the test).
struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    data_dir: std::path::PathBuf,
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
            .name("pty-test-reader".into())
            .spawn(move || {
                let mut dec = FrameDecoder::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    // Drain all fully-buffered frames first.
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
                        Ok(0) => return, // EOF: sidecar closed stdout
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
        };
        // Drain the leading `ready` Log event.
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

    /// Receive one decoded message within `timeout`, or `None` if the channel was idle
    /// for the whole window (e.g. the producer is paused at the gate).
    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Receive messages until one maps to `Some`, bounded by `deadline`.
    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let remaining = deadline.checked_sub(start.elapsed());
            let Some(remaining) = remaining else {
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
        self.send(Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        });
        let caps = self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::HandshakeOk { capabilities, .. }) => {
                Some(capabilities.clone())
            }
            _ => None,
        });
        assert!(caps.iter().any(|c| c == "pty"), "pty capability advertised");
    }

    fn spawn_pty(&mut self, id: u64) -> u32 {
        self.send(Request::PtySpawn {
            id,
            cols: 80,
            rows: 24,
            cwd: String::new(),
            record_id: String::new(),
        });
        self.recv_until(Duration::from_secs(5), |m| match m {
            Message::Response(Response::PtySpawned { terminal_id, .. }) => Some(*terminal_id),
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

/// Open a terminal, type a command, and observe its echoed output as BINARY
/// `terminal_output` frames. Acks each chunk so the stream keeps flowing.
#[test]
fn open_type_observe_binary_output() {
    let mut h = Harness::spawn();
    h.handshake();
    let terminal_id = h.spawn_pty(10);

    // Type a command whose output is a unique marker. The bytes are written verbatim
    // (binary, no base64) to the child's stdin.
    let marker = "NYX_PTY_MARKER_12345";
    let line = format!("printf '{marker}\\n'\n");
    h.send(Request::PtyWrite {
        id: 11,
        terminal_id,
        data: line.into_bytes(),
    });

    // Observe binary terminal_output frames until the marker shows up, acking each.
    let mut seen = Vec::new();
    let found = h.recv_until(Duration::from_secs(10), |m| match m {
        Message::Event(Event::TerminalOutput { data, .. }) => {
            seen.extend_from_slice(data);
            String::from_utf8_lossy(&seen)
                .contains(marker)
                .then_some(true)
        }
        _ => None,
    });
    assert!(
        found,
        "the typed command's output was observed over the binary stream"
    );

    h.send(Request::PtyClose {
        id: 12,
        terminal_id,
    });
    h.shutdown();
}

/// FLOW-CONTROL UNDER LOAD. Start a flood (`yes`) and DO NOT ack: the per-terminal
/// in-flight quota saturates at the high-water mark, the pump PAUSES the reader (the
/// pause-gate holds), so the in-flight backlog stays BOUNDED (no unbounded memory)
/// and the `seq` ordering is contiguous (lossless). Then ack to drain and confirm the
/// stream RESUMES.
#[test]
fn flow_control_pause_gate_holds_under_flood() {
    let mut h = Harness::spawn();
    h.handshake();
    let terminal_id = h.spawn_pty(20);

    // Unleash an unbounded flood. The child produces output far faster than we drain
    // it; with NO acks the pump must pause the reader at the high-water mark.
    h.send(Request::PtyWrite {
        id: 21,
        terminal_id,
        data: b"yes NYXFLOOD\n".to_vec(),
    });

    // Drain output WITHOUT acking until the frames STOP for an idle window (the
    // pause-gate engaged: the reader is blocked, the kernel buffer is full, the child
    // blocks on write). Assert the total un-acked bytes stayed BOUNDED and the seq
    // ordering was contiguous (lossless, in-order).
    let mut total_unacked: u64 = 0;
    let mut next_seq: u64 = 0;
    let mut frames = 0u64;
    let overall_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match h.recv(Duration::from_millis(750)) {
            Some(Message::Event(Event::TerminalOutput { seq, data, .. })) => {
                assert_eq!(
                    seq, next_seq,
                    "terminal_output seq must be contiguous (lossless, ordered)"
                );
                next_seq += 1;
                total_unacked += data.len() as u64;
                frames += 1;
                // The pause-gate bounds the unacked backlog to ~HIGH_WATER (512 KiB) +
                // one in-flight 64 KiB chunk. A few MiB of slack proves it is BOUNDED
                // (not unbounded growth); an unbounded flood would blow past this.
                assert!(
                    total_unacked < 4 * 1024 * 1024,
                    "unacked backlog must stay bounded under the pause-gate, got {total_unacked} bytes"
                );
            }
            // Skip non-output frames (cwd / exec-state / log).
            Some(_) => {}
            // Idle window with no frame: the pause-gate has engaged (producer stalled).
            None => break,
        }
        if Instant::now() > overall_deadline {
            panic!(
                "flood never paused: read {frames} frames totalling {total_unacked} unacked bytes \
                 without the gate engaging — the pause-gate did not hold"
            );
        }
    }
    assert!(frames > 0, "the flood produced at least one output frame");
    let paused_backlog = total_unacked;

    // ACK the entire backlog: frees the quota, drops below the low-water mark, RESUMES
    // the reader — more output must flow again (lossless: continues where it stalled).
    h.send(Request::PtyAck {
        id: 22,
        terminal_id,
        bytes: paused_backlog,
    });
    let resumed = h.recv_until(Duration::from_secs(5), |m| match m {
        Message::Event(Event::TerminalOutput { .. }) => Some(true),
        _ => None,
    });
    assert!(
        resumed,
        "after acking the backlog, the pump resumed and more output flowed"
    );

    h.send(Request::PtyClose {
        id: 23,
        terminal_id,
    });
    h.shutdown();
}
