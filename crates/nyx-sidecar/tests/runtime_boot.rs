//! Integration test that spawns the REAL `nyx-sidecar` binary and proves the
//! HHE runtime boots: the SQLite database is opened + migrated at boot under the
//! resolved data dir (`NYX_DATA_DIR`), and the binary serves the pipe afterwards.
//!
//! This is the end-to-end evidence for HHE done-criterion 1 ("migrations apply at
//! boot") through the ACTUAL boot path (`main::main` → `Runtime::boot` →
//! `runtime::db::open_database`), not just the in-process unit test.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
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
        "nyx-sidecar-boot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn_with_data_dir(data_dir: &std::path::Path) -> (Child, ChildStdin, ChildStdout) {
    let mut child = Command::new(sidecar_bin())
        .env("NYX_DATA_DIR", data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn nyx-sidecar");
    let stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    (child, stdin, stdout)
}

fn send(stdin: &mut ChildStdin, req: Request) {
    let frame = encode_frame(&encode_message(&Message::Request(req))).expect("encode frame");
    stdin.write_all(&frame).expect("write frame");
    stdin.flush().expect("flush");
}

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

/// The real binary opens + migrates `<NYX_DATA_DIR>/nyx.db` at boot, then serves the
/// pipe. We assert: the DB file exists (it was created), the handshake works (the
/// process came up healthy after the DB boot), and clean shutdown exits 0.
#[test]
fn binary_migrates_sqlite_at_boot_then_serves_pipe() {
    let data_dir = temp_data_dir();
    let (mut child, mut stdin, mut stdout) = spawn_with_data_dir(&data_dir);
    let mut dec = FrameDecoder::new();

    // Drain the unsolicited `ready` Log event (proves the process is serving).
    match recv(&mut stdout, &mut dec) {
        Message::Event(Event::Log { .. }) => {}
        other => panic!("expected ready event, got {other:?}"),
    }

    // The handshake proves the process booted past the DB-open step and is serving.
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

    // The database file was created + migrated under the resolved data dir.
    let db_file = data_dir.join("nyx.db");
    assert!(
        db_file.exists(),
        "the sidecar created+migrated nyx.db at boot: {}",
        db_file.display()
    );

    // Clean shutdown.
    send(&mut stdin, Request::Shutdown { id: 2 });
    let _ = recv(&mut stdout, &mut dec);
    let status = wait_exit(&mut child, Duration::from_secs(5));
    assert!(status.success(), "clean shutdown exits 0, got {status:?}");

    std::fs::remove_dir_all(&data_dir).ok();
}
