//! v2.5.5 VALIDATION GATE — end-to-end smoke of the AGENT-LIFECYCLE parcours
//! (task `01KX8Z6TH9RW19XYDK74CAD8BT`, PRD `01KX8Z4E7B3PYW2DGCTTKXCHYC`, Phase 5).
//!
//! This is the executable half of the gate: it drives the four v2.5.5 lifecycle journeys the
//! PRD calls out — **lazy resume on relaunch, crash → interrupted + restore, idle unload +
//! resume-on-revisit, plugin installable** — through the REAL `nyx-sidecar` binary over the
//! REAL framed pipe, EXACTLY as Electron main speaks to it. It exercises, in real runs, the
//! four done-criteria points the checklist (`apps/desktop/GATE-v2.5.5.md`) otherwise proves by
//! `file:line`:
//!
//!   1. **Reprise LAZY (toggle ON → relance → ouverture → resume ; OFF → rien)** — a
//!      resume-eligible session's `workspace_restore_plan` (read over the pipe) yields
//!      `claude --resume <id>`; parking it (`set_resume_parks`) then spawning the terminal's
//!      PTY injects that exact line into the REAL shell (observed on the wire). A toggle-OFF
//!      project's plan offers NO resume (`option_off`) and a spawn injects nothing.
//!   2. **Crash → interrupted + restore** — a seeded persisted-`running` instance + a
//!      not-clean recovery marker makes the sidecar's BOOT reconcile flag it `interrupted` and
//!      flip the active session to `unknown`; `recovery_snapshot` + `workspace_restore_plan`
//!      (both over the pipe) surface the interrupted set + the doubtful session for the user to
//!      restore — with NO blind relaunch.
//!   3. **Idle unload → resume-au-retour ; ACTIVE jamais unloadée** — `unload_idle_sessions`
//!      (the db-command the renderer's sweep drives) KEEPS a session within its threshold and
//!      the currently-visited one, UNLOADS an idle one past the threshold (its PTY closed →
//!      resources freed), and the sidecar's unload re-parks the resume so RE-SPAWNING the
//!      terminal re-injects `claude --resume <id>` — all over the real pipe.
//!   4. **Plugin installable depuis l'UI** — `integration_list` (over the pipe) lists the four
//!      providers with Claude Code `available`, and `integration_install` is DISPATCHED to the
//!      real integrations core over the wire (proven via the `codex` "not supported" arm, which
//!      short-circuits before touching any real config).
//!
//! WHY THE REAL PIPE (not the in-process `Runtime`): two critical transport-wiring bugs were
//! found in phase 4 precisely because in-process tests bypassed the wire dispatch (a db-command
//! that fell into the "not available over the sidecar transport" fallback and never reached its
//! authority). This gate drives every scenario through `Request::DbCommand`/`Request::PtySpawn`
//! frames on the spawned binary's stdin/stdout, so a broken dispatch arm FAILS the gate here.
//!
//! The on-disk authority is SEEDED with the SAME `nyx_core::db` API the app uses, BEFORE the
//! sidecar boots (the reverse of `gate_v2_1_identity_e2e.rs`, which re-opens the file to READ) —
//! because an ACTIVE agent session is normally written by the MCP session-capture hooks, a
//! surface off the pipe. The sidecar then serves that seeded state over the wire.
//!
//! What is NOT executable here (documented, a CI concern — NOT a gate failure), covered by the
//! checklist's `file:line` + CI recipe:
//!   * The full Electron renderer E2E (the CDP-driven `restore-*`/`idle-replay`/`terminal`
//!     specs, the restore panel and lifecycle badges rendering) needs a real display (xvfb) and
//!     a network install of the WDIO toolchain — the phase-7 gate's CI matrix, per
//!     `e2e/README.md` and `apps/desktop/GATE-PHASE7.md`.
//!   * "ACTIVE (Working) session jamais unloadée": setting a session `Working` is an MCP
//!     turn-hook signal (HTTP loopback, off the pipe), so the safety invariant is proven GREEN
//!     by the in-process real-PTY test `agent_unload.rs::active_session_is_never_unloaded`; this
//!     gate proves the transport-reachable unload gating (threshold / visited / toggle-OFF).
//!   * The full Claude plugin install/remove round-trip shells out to the real `claude` CLI and
//!     writes Claude's real config; it is proven by `nyx_core::integrations` unit tests (a fake
//!     CLI on temp paths) + the Windows-validated `smoke:bridge-e2e` (GATE-PHASE7 item #6). This
//!     gate proves the surface is LISTABLE + the install command DISPATCHES over the wire.
//!
//! Gated `#[cfg(not(windows))]` (spawns a real `sh`), GREEN on Linux — same category as
//! `resume_inject.rs` / `pty_flow.rs`.
#![cfg(not(windows))]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use nyx_core::db::{self, Db};
use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

/// A fresh, unique temp data dir (each run isolates its own `nyx.db` + recovery marker).
fn temp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-gate255-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Open the persisted `nyx.db` directly with the SAME `nyx_core::db` API the app uses and run a
/// seed closure against it BEFORE the sidecar boots, then CLOSE it (drop the pool) so the
/// spawned sidecar opens the same on-disk file cleanly. This seeds the authority the sidecar
/// then serves over the pipe — the reverse of `gate_v2_1`'s re-open-to-read.
fn seed_db<R>(data_dir: &std::path::Path, f: impl FnOnce(&Db) -> R) -> R {
    let db = Db::open(&data_dir.join("nyx.db")).expect("open temp nyx.db for seeding");
    // Claim the dir for nyx-v2 (the `.nyx-v2-owner` marker the sidecar's boot ownership guard
    // requires) — seeding writes `nyx.db` directly, so without this the sidecar would refuse the
    // dir as a foreign/v1 base (`runtime::db::guard_data_dir_ownership`).
    std::fs::write(data_dir.join(".nyx-v2-owner"), b"nyx-v2 gate seed\n")
        .expect("write .nyx-v2-owner marker");
    let out = f(&db);
    drop(db);
    out
}

/// Write the sidecar-owned recovery marker (`<data_dir>/sidecar-recovery.json`, the format
/// `runtime::recovery::RecoveryState` reads). `clean_shutdown=true` → the boot reconcile treats
/// the prior run as a CLEAN shutdown (seeded `active` sessions stay `active`). `clean_shutdown
/// =false` WITH the file present → a CRASH (running instances flagged `interrupted`, active
/// sessions flipped `unknown`) — the exact signal `recovery.rs` keys on.
fn write_recovery_marker(data_dir: &std::path::Path, clean_shutdown: bool) {
    let json = serde_json::json!({
        "clean_shutdown": clean_shutdown,
        "interrupted": [],
        "idempotency": [],
    });
    std::fs::write(
        data_dir.join("sidecar-recovery.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .expect("write sidecar-recovery.json");
}

/// The root `workspace_id` and terminal `record_id` produced by [`seed_resumable_session`].
struct Seed {
    workspace_id: String,
    record_id: String,
}

/// Seed a project + its root workspace + a terminal hosting an ACTIVE Claude session that is
/// resume-eligible: `resume_agent_sessions` per `resume_on`, a real transcript file on disk
/// (what `decide_resume` probes), and `external_session_id = ext_id` (so the resume command is
/// the deterministic `claude --resume <ext_id>`). Returns the root `workspace_id` and the
/// terminal `record_id`.
fn seed_resumable_session(
    data_dir: &std::path::Path,
    root_path: &str,
    ext_id: &str,
    resume_on: bool,
) -> Seed {
    // The transcript must exist on disk for `decide_resume` to allow the resume (finding #53).
    let transcript = data_dir.join(format!("transcript-{ext_id}.jsonl"));
    std::fs::write(&transcript, b"{}\n").unwrap();
    seed_db(data_dir, |db| {
        db.with_conn(|c| {
            let (project, root) = db::create_project(c, "GateProj", root_path, None).unwrap();
            db::set_project_resume_agent_sessions(c, &project.id, resume_on).unwrap();
            let term = db::create_terminal(c, root_path, Some("agent".into())).unwrap();
            db::attach_terminal(c, &term.id, &root.id, "auto").unwrap();
            db::record_session_start(
                c,
                &term.id,
                db::AGENT_KIND_CLAUDE_CODE,
                db::SessionCapture {
                    workspace_id: Some(root.id.clone()),
                    external_session_id: ext_id.to_string(),
                    cwd: root_path.to_string(),
                    transcript_path: Some(transcript.to_string_lossy().into_owned()),
                    metadata_json: None,
                },
            )
            .unwrap();
            Seed {
                workspace_id: root.id,
                record_id: term.id,
            }
        })
    })
}

/// A pipe harness over the REAL sidecar binary — a background reader thread drains stdout into a
/// channel of decoded `Message`s (mirrors `resume_inject.rs` / `pty_flow.rs`).
struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    data_dir: PathBuf,
    next_id: u64,
}

impl Harness {
    /// Spawn the sidecar over an ALREADY-SEEDED `data_dir`, plus any extra env (e.g. an isolated
    /// `NYX_CLAUDE_CONFIG` so the integrations read never touches the user's real `~/.claude`).
    fn spawn(data_dir: PathBuf, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(sidecar_bin());
        cmd.env("NYX_DATA_DIR", &data_dir)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("gate255-reader".into())
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
        // Drain the unsolicited `ready` Log event — the sidecar has booted PAST `attach_events`
        // (so the crash reconcile has already run) and is serving the pipe.
        match h.recv(Duration::from_secs(10)) {
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
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
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

    /// Drive a UI-bridge `db_command` over the pipe and return its parsed JSON result — the EXACT
    /// path the renderer's `nyxBridge.invoke(...)` reaches. Panics on a `Response::Error`.
    fn db_command(&mut self, command: &str, args: serde_json::Value) -> serde_json::Value {
        match self.db_command_try(command, args) {
            Ok(v) => v,
            Err(e) => panic!("db_command {command} errored over the pipe: {e}"),
        }
    }

    /// Like [`Self::db_command`] but returns `Err(message)` on a `Response::Error` — for the
    /// arms whose CONTRACT is a readable error (`integration_install(codex)` → "not supported").
    fn db_command_try(
        &mut self,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.id();
        self.send(Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(10), |m| match m {
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

    /// Spawn a PTY bound to a persistent RECORD id (so a parked resume can key onto it) and
    /// return the live `terminal_id`. This is the EXACT production spawn path that also drains
    /// any parked resume (`inject_resume_on_spawn`).
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

    fn write_line(&mut self, terminal_id: u32, line: &str) {
        let id = self.id();
        self.send(Request::PtyWrite {
            id,
            terminal_id,
            data: line.as_bytes().to_vec(),
        });
    }

    /// Collect binary terminal output (from the wire, only NEW frames) until `needle` appears;
    /// returns everything seen in THIS call. The shell echoes an injected/typed line, so its
    /// text lands in `TerminalOutput` even when the program (`claude`) is not installed.
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
            if self.child.try_wait().expect("try_wait").is_some() {
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

    /// Parse an `unload_idle_sessions` JSON result (a `string[]` of unloaded record ids).
    fn unload(&mut self, threshold_secs: u64, visited: &[&str]) -> Vec<String> {
        let out = self.db_command(
            "unload_idle_sessions",
            serde_json::json!({ "thresholdSecs": threshold_secs, "visited": visited }),
        );
        serde_json::from_value(out).expect("unload_idle_sessions returns a JSON string[]")
    }
}

/// DONE-CRITERION 1 (toggle ON) — a resume-eligible session's `workspace_restore_plan` (read
/// over the pipe) yields `claude --resume <id>`; PARKING it (`set_resume_parks`) then SPAWNING
/// the terminal's PTY injects that exact line into the REAL shell, observed on the wire. This is
/// the "toggle ON → relance → ouverture terminal → resume" loop, end-to-end over the transport.
#[test]
fn resume_toggle_on_resumes_over_the_real_pipe() {
    let data_dir = temp_data_dir("resume-on");
    let ext = "ext-gate-on";
    let seed = seed_resumable_session(&data_dir, "/tmp/nyx-gate-resume-on", ext, true);
    // Clean prior shutdown → the boot reconcile keeps the seeded session `active`.
    write_recovery_marker(&data_dir, true);

    let mut h = Harness::spawn(data_dir, &[]);

    // The renderer reads WHAT to resume from the restore plan (over the pipe). Toggle ON + a
    // transcript on disk → the active session resumes with the EXACT `claude --resume <ext>`.
    let plan = h.db_command(
        "workspace_restore_plan",
        serde_json::json!({ "workspaceId": seed.workspace_id }),
    );
    let sessions = plan["resume_sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "one resume candidate in the workspace");
    assert_eq!(
        sessions[0]["resume"],
        serde_json::json!(true),
        "toggle ON + transcript → the session is offered to resume"
    );
    assert_eq!(
        sessions[0]["resume_command"],
        serde_json::json!(format!("claude --resume {ext}")),
        "the plan yields the exact `claude --resume <id>` line"
    );
    assert_eq!(sessions[0]["uncertain"], serde_json::json!(false));

    // PARK that plan's resume (the renderer's `set_resume_parks` push), keyed by the record id.
    h.db_command(
        "set_resume_parks",
        serde_json::json!({
            "parks": [ {
                "terminalId": seed.record_id,
                "sessionId": sessions[0]["session_id"],
                "command": format!("claude --resume {ext}"),
            } ]
        }),
    );

    // OPEN the terminal (relaunch → the deck mounts the terminal): the parked resume drains at
    // the spawn and the shell echoes `claude --resume <ext>` onto its output (observed on wire).
    let _tid = h.spawn_pty_record(&seed.record_id);
    let out = h.read_until_marker(&format!("claude --resume {ext}"));
    assert!(
        out.contains(&format!("claude --resume {ext}")),
        "opening the terminal injected the parked `claude --resume <id>` into the real shell: {out:?}"
    );

    h.shutdown();
}

/// DONE-CRITERION 1 (toggle OFF) — a `resume_agent_sessions`-OFF project offers NO resume in its
/// restore plan (`option_off`), and OPENING its terminal injects NOTHING (no `claude --resume`).
/// "toggle OFF → rien", end-to-end over the transport.
#[test]
fn resume_toggle_off_injects_nothing_over_the_real_pipe() {
    let data_dir = temp_data_dir("resume-off");
    let ext = "ext-gate-off";
    let seed = seed_resumable_session(&data_dir, "/tmp/nyx-gate-resume-off", ext, false);
    write_recovery_marker(&data_dir, true);

    let mut h = Harness::spawn(data_dir, &[]);

    // Toggle OFF → the plan surfaces the session but offers NO auto-resume (no blind resume).
    let plan = h.db_command(
        "workspace_restore_plan",
        serde_json::json!({ "workspaceId": seed.workspace_id }),
    );
    let sessions = plan["resume_sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "the session is surfaced");
    assert_eq!(
        sessions[0]["resume"],
        serde_json::json!(false),
        "toggle OFF → not auto-resumed"
    );
    assert_eq!(sessions[0]["resume_command"], serde_json::Value::Null);
    assert_eq!(sessions[0]["skip_reason"], serde_json::json!("option_off"));

    // Nothing was parked (the plan offered no resume). Opening the terminal must inject NOTHING:
    // type a sentinel AFTER the spawn and prove no `claude --resume` precedes it on the wire.
    let tid = h.spawn_pty_record(&seed.record_id);
    h.write_line(tid, "printf 'NYX_OFF_SENTINEL\\n'\n");
    let out = h.read_until_marker("NYX_OFF_SENTINEL");
    assert!(
        !out.contains("claude --resume"),
        "toggle OFF injected no resume on open (rien): {out:?}"
    );

    h.shutdown();
}

/// DONE-CRITERION 2 — a CRASH surfaces `interrupted` instances + the doubtful session, offered
/// for RESTORE, with NO blind relaunch. A persisted-`running` instance + a NOT-clean recovery
/// marker makes the sidecar's BOOT reconcile flag it `interrupted` and flip the active session
/// `unknown`; `recovery_snapshot` + `workspace_restore_plan` (over the pipe) surface both.
#[test]
fn crash_surfaces_interrupted_and_restore_plan_over_the_real_pipe() {
    let data_dir = temp_data_dir("crash");
    let ext = "ext-gate-crash";

    // Seed a workspace with (a) a service instance persisted `running` + running-at-shutdown, and
    // (b) an active Claude session — the two things a crash leaves doubtful.
    let transcript = data_dir.join("transcript-crash.jsonl");
    std::fs::write(&transcript, b"{}\n").unwrap();
    let (workspace_id, running_instance) = seed_db(&data_dir, |db| {
        db.with_conn(|c| {
            let (project, root) =
                db::create_project(c, "GateCrash", "/tmp/nyx-gate-crash", None).unwrap();
            // A restart_on_startup-agnostic service persisted `running` (the crash orphan).
            let tmpl = db::create_template(
                c,
                &project.id,
                "dev",
                "sleep 999",
                None,
                db::CommandSource::default(),
            )
            .unwrap();
            let inst = db::instance_ids_for_template(c, &tmpl.id).unwrap()[0].clone();
            db::set_run_state(c, &inst, db::STATE_RUNNING, None).unwrap();
            db::set_was_running_on_shutdown(c, &inst, true).unwrap();
            // An active Claude session in the workspace.
            let term = db::create_terminal(c, "/tmp/nyx-gate-crash", Some("agent".into())).unwrap();
            db::attach_terminal(c, &term.id, &root.id, "auto").unwrap();
            db::record_session_start(
                c,
                &term.id,
                db::AGENT_KIND_CLAUDE_CODE,
                db::SessionCapture {
                    workspace_id: Some(root.id.clone()),
                    external_session_id: ext.to_string(),
                    cwd: "/tmp/nyx-gate-crash".to_string(),
                    transcript_path: Some(transcript.to_string_lossy().into_owned()),
                    metadata_json: None,
                },
            )
            .unwrap();
            (root.id, inst)
        })
    });
    // A recovery file PRESENT but NOT clean → the prior run CRASHED (never flipped it to true).
    write_recovery_marker(&data_dir, false);

    let mut h = Harness::spawn(data_dir, &[]);

    // `recovery_snapshot` (over the pipe) surfaces the interrupted set for the renderer to badge.
    let recovery = h.db_command("recovery_snapshot", serde_json::json!({}));
    let interrupted: Vec<String> = serde_json::from_value(recovery["interrupted"].clone()).unwrap();
    assert!(
        interrupted.contains(&running_instance),
        "the crashed running instance is flagged interrupted: {interrupted:?}"
    );

    // `workspace_restore_plan` (over the pipe) offers the RESTORE UI's data: the interrupted set
    // scoped to this workspace + the doubtful session, surfaced for the user to choose — NO blind
    // relaunch (nothing was auto-started; the plan is a pure read of what COULD resume).
    let plan = h.db_command(
        "workspace_restore_plan",
        serde_json::json!({ "workspaceId": workspace_id }),
    );
    let plan_interrupted: Vec<String> =
        serde_json::from_value(plan["interrupted"].clone()).unwrap();
    assert!(
        plan_interrupted.contains(&running_instance),
        "the restore plan surfaces the interrupted instance (user restores it explicitly)"
    );
    // The active session was flipped `unknown` by the reconcile but is STILL a candidate the
    // plan surfaces (a doubtful state offered for restore, never blind-resumed).
    let sessions = plan["resume_sessions"].as_array().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "the doubtful (now `unknown`) session is surfaced for restore"
    );

    h.shutdown();
}

/// DONE-CRITERION 3 — idle UNLOAD (resources freed) + resume-au-retour, all over the real pipe.
/// `unload_idle_sessions` (the db-command the renderer's sweep drives): KEEPS within the
/// threshold, SPARES the visited terminal, UNLOADS an idle one past the threshold, and the
/// sidecar's unload re-parks the resume so RE-SPAWNING re-injects `claude --resume <id>`.
#[test]
fn idle_unload_and_revisit_resume_over_the_real_pipe() {
    let data_dir = temp_data_dir("unload");
    let ext = "ext-gate-unload";
    let seed = seed_resumable_session(&data_dir, "/tmp/nyx-gate-unload", ext, true);
    write_recovery_marker(&data_dir, true);

    let mut h = Harness::spawn(data_dir, &[]);

    // OPEN the terminal → its PTY is live (a loaded, idle agent session).
    let _tid = h.spawn_pty_record(&seed.record_id);

    // WITHIN the threshold (large `thresholdSecs`) → KEPT (the sweep respects the grace window).
    let kept = h.unload(3600, &[]);
    assert!(
        kept.is_empty(),
        "within the threshold the idle session is KEPT: {kept:?}"
    );

    // VISITED (focused) → spared even past the threshold.
    let kept = h.unload(0, &[seed.record_id.as_str()]);
    assert!(
        kept.is_empty(),
        "the visited terminal is never unloaded: {kept:?}"
    );

    // PAST the threshold, unvisited → UNLOADED (its PTY closed → resources freed). The db-command
    // returns the unloaded record id (contract parity) and the unload re-parks the resume.
    let unloaded = h.unload(0, &[]);
    assert_eq!(
        unloaded,
        vec![seed.record_id.clone()],
        "the idle session is unloaded THROUGH the `unload_idle_sessions` db-command over the pipe"
    );

    // The DB session STAYS active (unload does not end it → still a resume candidate): the plan
    // still offers its resume after the unload.
    let plan = h.db_command(
        "workspace_restore_plan",
        serde_json::json!({ "workspaceId": seed.workspace_id }),
    );
    assert_eq!(
        plan["resume_sessions"][0]["resume"],
        serde_json::json!(true),
        "the unloaded session stays active + resume-eligible"
    );

    // REVISIT: re-open the terminal → the sidecar's unload re-park drains at the spawn and
    // re-injects `claude --resume <ext>` (resume-au-retour) — WITH NO renderer re-push.
    let _tid2 = h.spawn_pty_record(&seed.record_id);
    let out = h.read_until_marker(&format!("claude --resume {ext}"));
    assert!(
        out.contains(&format!("claude --resume {ext}")),
        "revisiting the unloaded terminal re-injected `claude --resume <id>` over the real spawn path: {out:?}"
    );

    h.shutdown();
}

/// DONE-CRITERION 4 — the plugin is installable from the UI: the integrations surface is LISTABLE
/// over the pipe (`integration_list` → the four providers, Claude Code `available`), and the
/// install command DISPATCHES to the real integrations core over the wire (proven via the `codex`
/// "not supported" arm, which short-circuits before touching any real config — so this gate never
/// mutates the user's `~/.claude`). The full Claude install round-trip is a documented CI concern.
#[test]
fn integrations_listable_and_install_dispatched_over_the_real_pipe() {
    let data_dir = temp_data_dir("integrations");
    // Point the Claude config seam at an isolated (nonexistent) temp path so the status read
    // never touches the user's real `~/.claude`.
    let claude_config = data_dir.join("claude-config.json");
    let mut h = Harness::spawn(
        data_dir,
        &[("NYX_CLAUDE_CONFIG", claude_config.to_str().unwrap())],
    );

    // `integration_list` (over the pipe) → the four registry providers. Claude Code is the ONE
    // installable provider (`available`); codex/opencode/custom are "coming soon".
    let list = h.db_command("integration_list", serde_json::json!({}));
    let providers = list.as_array().unwrap();
    assert_eq!(providers.len(), 4, "the four registry providers are listed");
    let claude = providers
        .iter()
        .find(|p| p["provider"] == "claude_code")
        .expect("claude_code is listed as the installable plugin");
    assert_eq!(
        claude["available"],
        serde_json::json!(true),
        "Claude Code is the installable (available) plugin the UI's Install button drives"
    );
    let codex = providers.iter().find(|p| p["provider"] == "codex").unwrap();
    assert_eq!(codex["available"], serde_json::json!(false));

    // `integration_install` is DISPATCHED to the real integrations core over the wire (NOT the
    // "not available over the sidecar transport" fallback — the phase-4 bug class). The `codex`
    // provider short-circuits with a readable "not supported" error BEFORE any CLI/config touch,
    // proving the arm is wired without mutating the real Claude config.
    let err = h
        .db_command_try(
            "integration_install",
            serde_json::json!({ "provider": "codex" }),
        )
        .expect_err("codex install is a readable domain error");
    assert!(
        err.contains("not supported"),
        "integration_install is dispatched to the integrations core (codex → not supported): {err}"
    );
    assert!(
        !err.contains("not available over the sidecar transport"),
        "the command must be DISPATCHED, not the transport fallback: {err}"
    );

    h.shutdown();
}
