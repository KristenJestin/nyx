//! DISPATCH-LEVEL integration test for the IDEMPOTENCY-KEY mechanism (Phase 4 —
//! `01KWCJPFP9JSJMRDZGW2E0N905`, N905 scope "operations mutantes avec cle d'idempotence").
//!
//! The idempotency guard lives in `crates/nyx-sidecar/src/runtime/db_command.rs`: a guarded
//! mutating `db_command` may carry an `idempotencyKey`, recorded ONLY AFTER a successful apply
//! so a retry after a crash is applied AT MOST ONCE while a FAILED op stays retriable. Those
//! two properties are correct by code inspection, but were previously unexercised THROUGH THE
//! DISPATCH BOUNDARY. This test drives the REAL `nyx-sidecar` binary over the framed pipe (the
//! same transport Electron main speaks) and asserts both end-to-end:
//!
//!   * DEDUP-ON-REPLAY: a guarded mutation (`create_project`) sent TWICE with the SAME
//!     `idempotencyKey` applies EXACTLY ONCE — the second send is a no-op success (`"null"`),
//!     and `list_projects` shows a single project (no double-apply).
//!   * FAILED-OP-STAYS-RETRIABLE: a guarded mutation that FAILS (`create_workspace` against an
//!     unknown `projectId` → foreign-key violation) does NOT record its key, so a RETRY with
//!     the SAME key and a VALID payload still EXECUTES (the workspace is actually created).

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

fn temp_data_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-idem-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn(data_dir: &std::path::Path) -> (Child, ChildStdin, ChildStdout) {
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

/// Read framed messages until one satisfies `want`, returning it. Drains intervening events.
fn recv_until<F>(stdout: &mut ChildStdout, dec: &mut FrameDecoder, mut want: F) -> Message
where
    F: FnMut(&Message) -> bool,
{
    let mut byte = [0u8; 1];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        while let Some(payload) = dec.next_frame().expect("frame decode") {
            let msg = decode_message(&payload).expect("decode message");
            if want(&msg) {
                return msg;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for a message");
        let n = stdout.read(&mut byte).expect("read stdout");
        assert_ne!(n, 0, "sidecar closed stdout before the awaited message");
        dec.push(&byte[..n]);
    }
}

fn handshake(stdin: &mut ChildStdin, stdout: &mut ChildStdout, dec: &mut FrameDecoder, id: u64) {
    send(
        stdin,
        Request::Handshake {
            id,
            protocol_version: PROTOCOL_VERSION,
        },
    );
    let ok = recv_until(stdout, dec, |m| {
        matches!(m, Message::Response(Response::HandshakeOk { .. }))
    });
    assert!(matches!(
        ok,
        Message::Response(Response::HandshakeOk { .. })
    ));
}

/// Issue a `db_command` and return `Ok(result_json)` on success or `Err(message)` on a
/// structured `Response::Error` — the dispatch outcome the supervisor would see.
fn db_command(
    stdin: &mut ChildStdin,
    stdout: &mut ChildStdout,
    dec: &mut FrameDecoder,
    id: u64,
    command: &str,
    args_json: &str,
) -> Result<String, String> {
    send(
        stdin,
        Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args_json.to_string(),
        },
    );
    let resp = recv_until(stdout, dec, |m| {
        matches!(
            m,
            Message::Response(Response::DbResult { id: rid, .. }) if *rid == id
        ) || matches!(
            m,
            Message::Response(Response::Error { id: rid, .. }) if *rid == id
        )
    });
    match resp {
        Message::Response(Response::DbResult { result_json, .. }) => Ok(result_json),
        Message::Response(Response::Error { message, .. }) => Err(message),
        other => panic!("unexpected response to {command}: {other:?}"),
    }
}

/// `create_project` requires success — panics with the dispatch error otherwise.
fn create_project(
    stdin: &mut ChildStdin,
    stdout: &mut ChildStdout,
    dec: &mut FrameDecoder,
    id: u64,
    name: &str,
    idempotency_key: Option<&str>,
) -> serde_json::Value {
    let args = match idempotency_key {
        Some(key) => serde_json::json!({
            "name": name,
            "rootPath": "/tmp/idem-test",
            "rootName": "root",
            "idempotencyKey": key,
        }),
        None => serde_json::json!({
            "name": name,
            "rootPath": "/tmp/idem-test",
            "rootName": "root",
        }),
    };
    let raw = db_command(stdin, stdout, dec, id, "create_project", &args.to_string())
        .expect("create_project must succeed");
    serde_json::from_str(&raw).expect("create_project result is valid json")
}

/// How many projects exist right now (via the `list_projects` read command).
fn project_count(
    stdin: &mut ChildStdin,
    stdout: &mut ChildStdout,
    dec: &mut FrameDecoder,
    id: u64,
) -> usize {
    let raw =
        db_command(stdin, stdout, dec, id, "list_projects", "{}").expect("list_projects succeeds");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v.as_array().map(|a| a.len()).unwrap_or(0)
}

/// How many NON-ROOT workspaces exist in `project_id` (the root is created with the project;
/// every additional `create_workspace` adds a non-root one).
fn non_root_workspace_count(
    stdin: &mut ChildStdin,
    stdout: &mut ChildStdout,
    dec: &mut FrameDecoder,
    id: u64,
    project_id: &str,
) -> usize {
    let raw = db_command(
        stdin,
        stdout,
        dec,
        id,
        "list_workspaces",
        &serde_json::json!({ "projectId": project_id }).to_string(),
    )
    .expect("list_workspaces succeeds");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v.as_array()
        .map(|a| {
            a.iter()
                .filter(|w| w.get("is_root").and_then(|b| b.as_bool()) == Some(false))
                .count()
        })
        .unwrap_or(0)
}

fn clean_shutdown(stdin: &mut ChildStdin, stdout: &mut ChildStdout, dec: &mut FrameDecoder) {
    send(stdin, Request::Shutdown { id: 999 });
    let _ = recv_until(stdout, dec, |m| {
        matches!(m, Message::Response(Response::ShutdownAck { .. }))
    });
}

/// DEDUP-ON-REPLAY: the SAME `idempotencyKey` on a guarded mutation applies EXACTLY ONCE. The
/// replayed send returns a no-op success WITHOUT re-running the mutation, so the contract-level
/// effect (the row inserted) happens once, not twice.
#[test]
fn same_idempotency_key_dedupes_a_guarded_mutation_through_dispatch() {
    let data_dir = temp_data_dir();
    let (mut child, mut stdin, mut stdout) = spawn(&data_dir);
    let mut dec = FrameDecoder::new();
    handshake(&mut stdin, &mut stdout, &mut dec, 1);

    assert_eq!(
        project_count(&mut stdin, &mut stdout, &mut dec, 2),
        0,
        "fresh data dir has no projects"
    );

    // FIRST apply with key "k-dup": the project is created and the result is the
    // `{ project, root }` shape.
    let first = create_project(
        &mut stdin,
        &mut stdout,
        &mut dec,
        3,
        "dup-proj",
        Some("k-dup"),
    );
    assert!(
        first.get("project").and_then(|p| p.get("id")).is_some(),
        "first create_project returns the created project: {first}"
    );
    assert_eq!(
        project_count(&mut stdin, &mut stdout, &mut dec, 4),
        1,
        "the first guarded create applied exactly one project"
    );

    // REPLAY the SAME key: a no-op success (`null`) WITHOUT a second insert. The dispatch
    // dedup short-circuits before `create_project` runs again.
    let replay = db_command(
        &mut stdin,
        &mut stdout,
        &mut dec,
        5,
        "create_project",
        &serde_json::json!({
            "name": "dup-proj",
            "rootPath": "/tmp/idem-test",
            "rootName": "root",
            "idempotencyKey": "k-dup",
        })
        .to_string(),
    )
    .expect("the replayed create is a no-op SUCCESS, not an error");
    assert_eq!(
        replay, "null",
        "a replayed guarded mutation with the same key is a no-op success (null), got {replay}"
    );

    // The crux: the project count is STILL 1 — the replay did NOT double-apply.
    assert_eq!(
        project_count(&mut stdin, &mut stdout, &mut dec, 6),
        1,
        "the replayed key did NOT create a second project (dedup-on-replay holds)"
    );

    // A DIFFERENT key is NOT deduped — it applies a fresh mutation (sanity that the guard keys
    // on the value, not on the command).
    let _ = create_project(
        &mut stdin,
        &mut stdout,
        &mut dec,
        7,
        "other-proj",
        Some("k-other"),
    );
    assert_eq!(
        project_count(&mut stdin, &mut stdout, &mut dec, 8),
        2,
        "a DIFFERENT idempotency key applies a fresh mutation"
    );

    clean_shutdown(&mut stdin, &mut stdout, &mut dec);
    let start = Instant::now();
    while child.try_wait().expect("try_wait").is_none() {
        assert!(start.elapsed() < Duration::from_secs(5), "did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::remove_dir_all(&data_dir).ok();
}

/// FAILED-OP-STAYS-RETRIABLE: a guarded mutation that FAILS does not record its key, so a retry
/// with the SAME key and a VALID payload still EXECUTES. We make `create_workspace` fail first
/// (an unknown `projectId` → foreign-key violation), then retry the same key against a REAL
/// project and assert the workspace is actually created.
#[test]
fn failed_guarded_mutation_leaves_the_key_retriable_through_dispatch() {
    let data_dir = temp_data_dir();
    let (mut child, mut stdin, mut stdout) = spawn(&data_dir);
    let mut dec = FrameDecoder::new();
    handshake(&mut stdin, &mut stdout, &mut dec, 1);

    // A real project to attach the eventual workspace to.
    let proj = create_project(&mut stdin, &mut stdout, &mut dec, 2, "retry-proj", None);
    let project_id = proj
        .get("project")
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .expect("project id")
        .to_string();
    assert_eq!(
        non_root_workspace_count(&mut stdin, &mut stdout, &mut dec, 3, &project_id),
        0,
        "no extra workspace yet (only the root)"
    );

    // FIRST attempt with key "k-retry" against an UNKNOWN project id → the FK insert FAILS. The
    // guard must NOT record the key (the op never succeeded).
    let failed = db_command(
        &mut stdin,
        &mut stdout,
        &mut dec,
        4,
        "create_workspace",
        &serde_json::json!({
            "projectId": "does-not-exist",
            "name": "ws",
            "path": "/tmp/idem-test/ws",
            "idempotencyKey": "k-retry",
        })
        .to_string(),
    );
    assert!(
        failed.is_err(),
        "create_workspace against an unknown project must FAIL (got {failed:?})"
    );

    // RETRY with the SAME key but a VALID project id. Because the failed attempt did NOT record
    // the key, this is NOT deduped — it actually creates the workspace.
    let retried = db_command(
        &mut stdin,
        &mut stdout,
        &mut dec,
        5,
        "create_workspace",
        &serde_json::json!({
            "projectId": project_id,
            "name": "ws",
            "path": "/tmp/idem-test/ws",
            "idempotencyKey": "k-retry",
        })
        .to_string(),
    )
    .expect("the retry with the same key EXECUTES (the failed op left the key unrecorded)");
    // create_workspace returns the created workspace row, not a no-op `null`.
    assert_ne!(
        retried, "null",
        "the retry actually executed (returned the created workspace), not a deduped no-op"
    );
    let row: serde_json::Value = serde_json::from_str(&retried).unwrap();
    assert!(
        row.get("id").and_then(|v| v.as_str()).is_some(),
        "the retry returned a real workspace row: {retried}"
    );

    // The crux: the workspace WAS created — proof the failed-then-retried op was retriable.
    assert_eq!(
        non_root_workspace_count(&mut stdin, &mut stdout, &mut dec, 6, &project_id),
        1,
        "the retry created exactly one workspace (failed-op-stays-retriable holds)"
    );

    clean_shutdown(&mut stdin, &mut stdout, &mut dec);
    let start = Instant::now();
    while child.try_wait().expect("try_wait").is_none() {
        assert!(start.elapsed() < Duration::from_secs(5), "did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::remove_dir_all(&data_dir).ok();
}
