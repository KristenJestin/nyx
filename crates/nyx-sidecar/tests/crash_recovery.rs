//! Integration test for SIDECAR CRASH RECOVERY (Phase 4 — `01KWCJPFP9JSJMRDZGW2E0N905`).
//!
//! Spawns the REAL `nyx-sidecar` binary against a fixed data dir, drives it to a serving
//! state, seeds persisted runtime state that looks like work-in-progress (a managed-command
//! instance persisted as `running` + an `active` agent session), then KILLS the process
//! (`SIGKILL` — a CRASH, not the ordered `shutdown` request, so the sidecar's clean-shutdown
//! marker is never written). It then RELAUNCHES the binary ONCE against the SAME data dir and
//! asserts, over the pipe, that the relaunched sidecar:
//!
//!   * reopened SQLite and rebuilt the persisted state (the seeded rows are queryable again);
//!   * flagged the previously-`running` instance `interrupted` (surfaced via the
//!     `recovery_snapshot` db-command) — its liveness could not be confirmed and it was NOT
//!     blindly relaunched;
//!   * flipped the previously-`active` agent session to `unknown` (no session is `active`
//!     after the crash reconcile, because the runtime cannot re-verify it).
//!
//! The "exactly one relaunch" half of the done-criterion is the SUPERVISOR's single-relaunch
//! latch, owned by the `Sidecar` manager (`apps/desktop/src/main/sidecar.ts`) and proven
//! end-to-end by the Electron smoke `apps/desktop/scripts/smoke-crash-recovery.cjs` (steps 3
//! and 5: a kill yields exactly one `recovering` transition, and a second crash settles
//! `degraded` without looping). This integration test owns the sidecar-side half: a relaunch
//! after a real `SIGKILL` reopens the DB, rebuilds state, and marks the doubtful `running`
//! work interrupted/unknown.

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
        "nyx-sidecar-crash-{}-{}",
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

/// Issue a `db_command` and return its JSON result string.
fn db_command(
    stdin: &mut ChildStdin,
    stdout: &mut ChildStdout,
    dec: &mut FrameDecoder,
    id: u64,
    command: &str,
    args_json: &str,
) -> String {
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
        Message::Response(Response::DbResult { result_json, .. }) => result_json,
        Message::Response(Response::Error { message, .. }) => {
            panic!("db_command {command} errored: {message}")
        }
        other => panic!("unexpected response to {command}: {other:?}"),
    }
}

/// Seed the SAME nyx.db the sidecar uses with a project + template (one instance), set that
/// instance to `running`, snapshot it `was_running_on_shutdown=true` (as a real crash would
/// leave it), and start an `active` agent session — i.e. work-in-progress at crash time.
/// Returns the instance id.
fn seed_inflight_state(data_dir: &std::path::Path) -> String {
    use nyx_core::db::{self, Db};
    // This seed CREATES a legitimate v2 nyx.db, so claim the dir for v2 first (write the
    // ownership marker) — otherwise the sidecar's boot guard (#38) would refuse this dir as
    // a foreign/unmarked DB.
    nyx_sidecar::runtime::db::guard_data_dir_ownership(data_dir).expect("claim v2 data dir");
    let db = Db::open(&data_dir.join("nyx.db")).expect("open seed db");
    db.with_conn(|c| {
        let (project, _root) = db::create_project(c, "proj", "/tmp/work", None).unwrap();
        let tmpl = db::create_template(
            c,
            &project.id,
            "dev",
            "sleep 999",
            None,
            db::CommandSource::default(),
        )
        .unwrap();
        let ids = db::instance_ids_for_template(c, &tmpl.id).unwrap();
        let id = ids[0].clone();
        // Persist it as `running` + snapshot it as was-running (a crash leaves both, since
        // nothing ran the ordered shutdown to reset them).
        db::set_run_state(c, &id, db::STATE_RUNNING, None).unwrap();
        db::set_was_running_on_shutdown(c, &id, true).unwrap();
        // An ACTIVE agent session bound to a fresh terminal.
        let term = db::create_terminal(c, "/tmp/work", Some("t".into())).unwrap();
        db::record_session_start(
            c,
            &term.id,
            db::AGENT_KIND_CLAUDE_CODE,
            db::SessionCapture {
                workspace_id: None,
                external_session_id: "ext-crash-1".into(),
                cwd: "/tmp/work".into(),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        id
    })
}

/// Kill the sidecar process HARD (SIGKILL) — a crash, NOT the ordered shutdown. The
/// clean-shutdown marker is therefore never written, so the next boot runs the crash reconcile.
fn kill_hard(child: &mut Child) {
    child.kill().expect("SIGKILL the sidecar");
    let start = Instant::now();
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "sidecar did not die after SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The end-to-end crash-recovery criterion: KILL the running sidecar, RELAUNCH it once
/// against the same data dir, and assert the reopened+rebuilt state marks the doubtful
/// `running` work interrupted and the active session unknown.
#[test]
fn kill_then_relaunch_rebuilds_state_and_flags_interrupted_unknown() {
    let data_dir = temp_data_dir();

    // --- 1. First sidecar run: boot, then seed in-flight state, then CRASH it ---------
    let (mut child, mut stdin, mut stdout) = spawn(&data_dir);
    let mut dec = FrameDecoder::new();
    // The very first boot is CLEAN-by-absence (no recovery file) — fine; we seed AFTER boot
    // so the seeded `running`/`active` rows are present at the crash moment.
    handshake(&mut stdin, &mut stdout, &mut dec, 1);
    let instance_id = seed_inflight_state(&data_dir);

    // Sanity: the seeded session is `active` (one entry) right now. `agent_active_sessions`
    // returns `[{terminal_id, agent_kind}]` for every ACTIVE session.
    let active_before = db_command(
        &mut stdin,
        &mut stdout,
        &mut dec,
        2,
        "agent_active_sessions",
        "{}",
    );
    let before: serde_json::Value = serde_json::from_str(&active_before).unwrap();
    assert_eq!(
        before.as_array().map(|a| a.len()),
        Some(1),
        "exactly one ACTIVE session exists pre-crash: {active_before}"
    );
    assert!(
        active_before.contains("claude_code"),
        "the active session is the seeded claude_code one: {active_before}"
    );

    // CRASH: SIGKILL the sidecar. No ordered shutdown ran, so no clean-shutdown marker is
    // written and the seeded rows stay `running`/`active` on disk.
    kill_hard(&mut child);
    drop(stdin);
    drop(stdout);

    // The recovery file, if any, must NOT say clean_shutdown (a crash never writes it).
    let rec_path = data_dir.join("sidecar-recovery.json");
    if rec_path.exists() {
        let body = std::fs::read_to_string(&rec_path).unwrap();
        assert!(
            !body.contains("\"clean_shutdown\": true"),
            "a crash must NOT leave clean_shutdown=true: {body}"
        );
    }

    // --- 2. RELAUNCH (exactly once) against the SAME data dir --------------------------
    let (mut child2, mut stdin2, mut stdout2) = spawn(&data_dir);
    let mut dec2 = FrameDecoder::new();
    handshake(&mut stdin2, &mut stdout2, &mut dec2, 1);

    // SQLite was reopened + the persisted state rebuilt: the seeded session row is queryable
    // again (the DB survived the crash), but it is NO LONGER `active` — the crash reconcile
    // flipped it to `unknown` (liveness unconfirmed).
    let active_after = db_command(
        &mut stdin2,
        &mut stdout2,
        &mut dec2,
        2,
        "agent_active_sessions",
        "{}",
    );
    let after: serde_json::Value = serde_json::from_str(&active_after).unwrap();
    assert_eq!(
        after.as_array().map(|a| a.len()),
        Some(0),
        "NO session is `active` after the crash reconcile (the previously-active one was \
         flipped to `unknown`): {active_after}"
    );

    // The previously-`running` instance is flagged `interrupted` — surfaced for the user to
    // choose, NOT blindly relaunched.
    let recovery = db_command(
        &mut stdin2,
        &mut stdout2,
        &mut dec2,
        3,
        "recovery_snapshot",
        "{}",
    );
    let parsed: serde_json::Value = serde_json::from_str(&recovery).expect("parse recovery json");
    let interrupted = parsed
        .get("interrupted")
        .and_then(|v| v.as_array())
        .expect("interrupted array");
    assert!(
        interrupted.iter().any(|v| v.as_str() == Some(&instance_id)),
        "the previously-running instance {instance_id} is flagged interrupted: {recovery}"
    );

    // The instance row is rebuilt + queryable (proves SQLite was reopened and state rebuilt).
    // We confirm via the DB directly that the snapshot was cleared (no blind relaunch).
    {
        use nyx_core::db::{self, Db};
        let db = Db::open(&data_dir.join("nyx.db")).expect("reopen db for assertion");
        let row = db
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .expect("the seeded instance row survived the crash + relaunch");
        assert!(
            !row.was_running_on_shutdown,
            "the crash reconcile cleared the was_running snapshot so the boot restore cannot \
             blindly relaunch the service"
        );
    }

    // --- 3. Clean shutdown of the relaunched sidecar ----------------------------------
    send(&mut stdin2, Request::Shutdown { id: 99 });
    let _ = recv_until(&mut stdout2, &mut dec2, |m| {
        matches!(m, Message::Response(Response::ShutdownAck { .. }))
    });
    let start = Instant::now();
    while child2.try_wait().expect("try_wait").is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "relaunched sidecar did not exit on clean shutdown"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // A clean shutdown now WROTE the marker, so a subsequent boot would not re-reconcile.
    let body = std::fs::read_to_string(&rec_path).expect("recovery file after clean shutdown");
    assert!(
        body.contains("\"clean_shutdown\": true"),
        "the ordered shutdown set clean_shutdown=true: {body}"
    );

    std::fs::remove_dir_all(&data_dir).ok();
}
