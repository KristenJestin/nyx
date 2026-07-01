//! Integration tests that spawn the REAL `nyx-sidecar` binary and drive its
//! supervised lifecycle over the framed MessagePack pipe (PDR-0007), exactly the
//! way Electron main will: write length-prefixed MessagePack request frames to
//! its `stdin`, read framed responses from its `stdout`.
//!
//! Covered (task 522 + 523 done-criteria):
//!   * the sidecar starts, responds to `handshake` and `ping` over the pipe;
//!   * a request/response round-trip AND an unsolicited-event encode/decode;
//!   * `shutdown` is clean (process exits 0 after acking);
//!   * a version-incompatible first frame yields a READABLE error at boot;
//!   * the sidecar DIES WITH ITS PARENT — when the supervisor closes the pipe
//!     (drops the sidecar's stdin), the sidecar detects the parent is gone and
//!     exits without a graceful shutdown request.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    self, decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

/// A UNIQUE temp data dir per spawn. The sidecar persists into `NYX_DATA_DIR` (SQLite + the
/// Phase-4 `sidecar-recovery.json`); pinning a fresh dir per child ISOLATES each test from the
/// others (and from the user's real default data dir) so concurrent integration binaries never
/// share one `nyx.db` / recovery file — which would otherwise make a prior child's non-clean
/// exit surface as a crash-recovery log on the next boot and desync the framed reads.
fn temp_data_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-lifecycle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn() -> (Child, ChildStdin, ChildStdout) {
    let mut child = Command::new(sidecar_bin())
        .env("NYX_DATA_DIR", temp_data_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn nyx-sidecar");
    let stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    (child, stdin, stdout)
}

/// Frame-encode a request message and write it to the sidecar's stdin.
fn send(stdin: &mut ChildStdin, req: Request) {
    let frame = encode_frame(&encode_message(&Message::Request(req))).expect("encode frame");
    stdin.write_all(&frame).expect("write frame");
    stdin.flush().expect("flush");
}

/// Read exactly one framed message from the sidecar's stdout (blocking until a
/// whole frame is available).
fn recv(stdout: &mut ChildStdout, decoder: &mut FrameDecoder) -> Message {
    let mut byte = [0u8; 1];
    loop {
        if let Some(payload) = decoder.next_frame().expect("frame decode") {
            return decode_message(&payload).expect("decode message");
        }
        let n = stdout.read(&mut byte).expect("read stdout");
        assert_ne!(n, 0, "sidecar closed stdout before a full frame arrived");
        decoder.push(&byte[..n]);
    }
}

/// Drain the unsolicited `ready` Log event the sidecar emits at startup.
fn drain_ready_event(stdout: &mut ChildStdout, dec: &mut FrameDecoder) {
    match recv(stdout, dec) {
        Message::Event(Event::Log { .. }) => {}
        other => panic!("expected leading ready event, got {other:?}"),
    }
}

fn wait_exit(child: &mut Child, deadline: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            panic!("sidecar did not exit within {deadline:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn handshake_ping_roundtrip_then_clean_shutdown() {
    let (mut child, mut stdin, mut stdout) = spawn();
    let mut dec = FrameDecoder::new();

    // --- unsolicited event over the pipe (sidecar -> UI) ---
    // The sidecar emits a `ready` Log event before any request; this exercises the
    // event channel of the transport end-to-end (done-criterion: "event sur le pipe").
    match recv(&mut stdout, &mut dec) {
        Message::Event(Event::Log { level, message }) => {
            assert_eq!(level, "info");
            assert!(message.contains("sidecar ready"), "ready event: {message}");
        }
        other => panic!("expected unsolicited ready Log event first, got {other:?}"),
    }

    // --- handshake round-trip ---
    send(
        &mut stdin,
        Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        },
    );
    match recv(&mut stdout, &mut dec) {
        Message::Response(Response::HandshakeOk {
            id,
            protocol_version,
            capabilities,
            max_frame_len,
        }) => {
            assert_eq!(id, 1);
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert!(capabilities.iter().any(|c| c == "ping"));
            assert!(max_frame_len > 0);
        }
        other => panic!("expected HandshakeOk, got {other:?}"),
    }

    // --- ping round-trip (nonce echoed) ---
    send(
        &mut stdin,
        Request::Ping {
            id: 2,
            nonce: 0xDEAD,
        },
    );
    match recv(&mut stdout, &mut dec) {
        Message::Response(Response::Pong { id, nonce }) => {
            assert_eq!(id, 2);
            assert_eq!(nonce, 0xDEAD);
        }
        other => panic!("expected Pong, got {other:?}"),
    }

    // --- clean shutdown ---
    send(&mut stdin, Request::Shutdown { id: 3 });
    match recv(&mut stdout, &mut dec) {
        Message::Response(Response::ShutdownAck { id }) => assert_eq!(id, 3),
        other => panic!("expected ShutdownAck, got {other:?}"),
    }
    let status = wait_exit(&mut child, Duration::from_secs(5));
    assert!(
        status.success(),
        "clean shutdown should exit 0, got {status:?}"
    );
}

#[test]
fn version_incompatible_first_frame_is_readable_error_at_boot() {
    let (mut child, mut stdin, mut stdout) = spawn();
    let mut dec = FrameDecoder::new();
    drain_ready_event(&mut stdout, &mut dec);

    // Stamp an envelope with an INCOMPATIBLE protocol version directly (a real
    // peer on a different protocol). The sidecar must reply with a readable error,
    // not crash.
    let env: (u32, u8, u8, rmpv::Value) = (
        PROTOCOL_VERSION + 7,
        wire::CAT_REQUEST,
        wire::REQ_HANDSHAKE,
        rmpv::Value::Array(vec![1u64.into(), (PROTOCOL_VERSION + 7).into()]),
    );
    let bad = rmp_serde::to_vec(&env).unwrap();
    let frame = encode_frame(&bad).unwrap();
    stdin.write_all(&frame).unwrap();
    stdin.flush().unwrap();

    match recv(&mut stdout, &mut dec) {
        Message::Response(Response::Error { code, message, .. }) => {
            assert_eq!(code, wire::ErrorCode::VersionIncompatible);
            assert!(
                message.contains("protocol version mismatch"),
                "error must be human-readable: {message}"
            );
        }
        other => panic!("expected version-incompatible Error, got {other:?}"),
    }

    // Sidecar survives the readable error; shut it down cleanly.
    send(&mut stdin, Request::Shutdown { id: 9 });
    let _ = recv(&mut stdout, &mut dec);
    let status = wait_exit(&mut child, Duration::from_secs(5));
    assert!(status.success());
}

#[test]
fn dies_when_parent_closes_the_pipe() {
    // Dropping the sidecar's stdin closes the pipe — exactly what the OS does when
    // the real parent (Electron main) dies. The sidecar's stdin hits EOF and it
    // must exit ON ITS OWN. Portable parent-death guarantee.
    let (mut child, stdin, mut stdout) = spawn();
    let mut dec = FrameDecoder::new();
    drain_ready_event(&mut stdout, &mut dec);

    {
        let mut stdin = stdin;
        send(
            &mut stdin,
            Request::Handshake {
                id: 1,
                protocol_version: PROTOCOL_VERSION,
            },
        );
        assert!(matches!(
            recv(&mut stdout, &mut dec),
            Message::Response(Response::HandshakeOk { .. })
        ));
        // stdin dropped here -> pipe write end closed -> EOF on sidecar stdin.
    }

    let status = wait_exit(&mut child, Duration::from_secs(5));
    assert!(
        status.success(),
        "sidecar must exit cleanly on parent pipe-close, got {status:?}"
    );
}
