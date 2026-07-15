//! Integration tests for the IDLE agent-session UNLOAD + resume-au-retour (v2.5.5, task
//! `01KX8Z6T1J2AC65DHHZTCQ26Z4`; review `01KX9CP96KVK6BTHY3ZSWY1E67`), driving a REAL `sh` PTY
//! through the in-process [`nyx_sidecar::runtime::Runtime`] — the exact services the pipe dispatch
//! calls.
//!
//! They prove, end to end on a real PTY + real SQLite + the real runtime activity store:
//!
//!   * **idle beyond the threshold → unloaded** (`idle_session_beyond_threshold_is_unloaded`):
//!     an idle, unvisited, resume-eligible agent session whose PTY has been inactive past the
//!     threshold is unloaded (its PTY closed, resources freed).
//!   * **IN-RUN revisit → resume** (`unloaded_session_resumes_in_run_on_revisit`): after an unload,
//!     re-spawning the terminal RE-INJECTS `claude --resume <id>` through the EXACT production
//!     PtySpawn path — WITHOUT any manual re-park. The sidecar's unload itself re-parks the resume
//!     (rebuilt via `decide_resume`, `ResumeParks::rearm`), so the in-run return resumes without
//!     the renderer re-pushing the plan (review finding 1). The DB session stays `active`.
//!   * **resume-eligibility gates unload** (`toggle_off_session_is_never_unloaded_toggle_on_is`):
//!     a `resume_agent_sessions`-OFF project's idle session is NEVER unloaded (never killed without
//!     recourse — the master-switch decision, review finding 2); a toggle-ON one IS unloaded.
//!   * **ACTIVE is never unloaded** (`active_session_is_never_unloaded`): a session that is
//!     generating (`Working`) — even with the idle clock past the threshold — is never unloaded.
//!   * **exceptions respected** (`action_in_flight_is_never_unloaded`): a session with a tool in
//!     flight (`Working`, 0-stale) is kept until the action completes; the visited terminal is
//!     kept too (`visited_session_is_never_unloaded`).
//!
//! The re-injected resume is the REAL `claude --resume <id>` line; the shell echoes it, so its
//! prefix is read back off the headless VT SCREEN snapshot (the same trick `pty.rs`'s unit tests
//! use), so no wire decoding is needed. Gated `#[cfg(not(windows))]` (spawns a real `sh`), GREEN on
//! Linux — same category as `pty_flow.rs`.
#![cfg(not(windows))]

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nyx_core::agent_activity::ActivityEvent;
use nyx_core::db::{self, Db};
use nyx_sidecar::runtime::events::EventSink;
use nyx_sidecar::runtime::{db_command, Runtime};

/// A discard event sink: the pump's `TerminalOutput`/exit frames go nowhere — the tests assert
/// on the runtime state + the VT SCREEN snapshot, not on the wire.
#[derive(Clone)]
struct Sink(Arc<Mutex<Vec<u8>>>);
impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn discard_sink() -> EventSink {
    EventSink::new(Box::new(Sink(Arc::new(Mutex::new(Vec::new())))))
}

fn temp_data_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-unload-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build an in-process runtime over an isolated temp DB, plus its discard event sink.
fn runtime() -> (Runtime, EventSink, std::path::PathBuf) {
    let dir = temp_data_dir();
    let db = Db::open(&dir.join("nyx.db")).expect("open temp db");
    (Runtime::with_db(Arc::new(db)), discard_sink(), dir)
}

/// Seed a terminal RECORD hosting an ACTIVE Claude session that is RESUME-ELIGIBLE: a project whose
/// `resume_agent_sessions` toggle is `resume_on`, a workspace the session is anchored to, and a
/// transcript file ON DISK (all three are what `decide_resume` needs to allow the resume). Returns
/// the record id (the key shared by the PTY registry, the activity store, and the resume parks).
///
/// The unload now gates on resume-eligibility (review finding 2), so a candidate is only unloadable
/// when the policy would resume it: this seed produces one with `resume_on = true`; passing `false`
/// produces a toggle-OFF session the sweep must KEEP.
fn seed_resumable_agent_terminal(rt: &Runtime, dir: &Path, resume_on: bool) -> String {
    rt.db().with_conn(|c| {
        // A project + its root workspace; the toggle governs whether the session resumes at all.
        let (project, workspace) = db::create_project(c, "proj", "/tmp/work", None).unwrap();
        db::set_project_resume_agent_sessions(c, &project.id, resume_on).unwrap();
        let term = db::create_terminal(c, "/tmp/work", Some("agent".into())).unwrap();
        // `decide_resume` requires a transcript on disk (finding #53) — write a real one.
        let transcript = dir.join(format!("transcript-{}.jsonl", term.id));
        std::fs::write(&transcript, b"{}\n").unwrap();
        db::record_session_start(
            c,
            &term.id,
            db::AGENT_KIND_CLAUDE_CODE,
            db::SessionCapture {
                // Anchor to the workspace so the project toggle is derived (workspace → project).
                workspace_id: Some(workspace.id.clone()),
                external_session_id: format!("ext-{}", term.id),
                cwd: "/tmp/work".into(),
                transcript_path: Some(transcript.to_string_lossy().into_owned()),
                metadata_json: None,
            },
        )
        .unwrap();
        term.id
    })
}

/// Poll the terminal's VT SCREEN snapshot (by record id) until `needle` appears on a row, or
/// time out. Proves the shell actually RAN the injected line (its printf output painted).
fn wait_for_marker(rt: &Runtime, record_id: &str, needle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(snap) = rt.pty().snapshot_by_record(record_id) {
            if (0..snap.main.rows).any(|r| snap.main.row_text(r).contains(needle)) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

/// Poll the terminal's VT SCREEN snapshot for a short window and return `true` iff `needle` NEVER
/// appears — the NEGATIVE of [`wait_for_marker`], for proving a GATED inject did not run. A short
/// window suffices: an injected `printf` paints within tens of ms (that is exactly what
/// `wait_for_marker` relies on), so the marker's ABSENCE across this window means it was never
/// written to the shell.
fn marker_absent(rt: &Runtime, record_id: &str, needle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_millis(750);
    while Instant::now() < deadline {
        if let Some(snap) = rt.pty().snapshot_by_record(record_id) {
            if (0..snap.main.rows).any(|r| snap.main.row_text(r).contains(needle)) {
                return false;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    true
}

fn no_visits() -> HashSet<String> {
    HashSet::new()
}

/// DONE-CRITERION 1 — "une session idle non visitée au-delà du seuil est déchargée". An idle,
/// unvisited agent session whose PTY has been inactive past the threshold is unloaded (its PTY is
/// closed → resources freed). A LARGE threshold keeps it (still within the window); a ZERO
/// threshold (past the window) unloads it.
#[test]
fn idle_session_beyond_threshold_is_unloaded() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");
    assert!(
        rt.pty().is_live_record(&record),
        "the PTY is live after spawn"
    );

    // Activity is Idle (no prompt was ever submitted → no entry → treated as Idle).
    // WITHIN the threshold: kept.
    let kept = rt.unload_idle_sessions(Duration::from_secs(3600), &no_visits(), &events);
    assert!(
        kept.is_empty() && rt.pty().is_live_record(&record),
        "an idle session still within the inactivity threshold is KEPT"
    );

    // PAST the threshold (ZERO): unloaded — the PTY is closed, the record no longer live.
    let unloaded = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert_eq!(
        unloaded,
        vec![record.clone()],
        "the idle session is unloaded"
    );
    assert!(
        !rt.pty().is_live_record(&record),
        "unloading closed the PTY (resources freed / out of the TUI)"
    );

    // The DB session STAYS active (unload does not end it → it remains a resume candidate).
    let still_active = rt
        .db()
        .with_conn(db::active_agent_sessions)
        .unwrap()
        .into_iter()
        .any(|s| s.terminal_id == record);
    assert!(
        still_active,
        "the unloaded session stays `active` in the DB"
    );
}

/// DONE-CRITERION 2 — "y revenir la resume (mécanisme lazy)", proven for an IN-RUN return (review
/// finding 1). After the unload, RE-SPAWNING the terminal RE-INJECTS `claude --resume <id>` through
/// the EXACT production PtySpawn path — with NO manual re-park. The sidecar's unload itself re-parks
/// the resume (rebuilt via `decide_resume`, `ResumeParks::rearm`), so the in-run revisit resumes
/// without the renderer re-pushing the plan (it pushes each plan at most once per run).
#[test]
fn unloaded_session_resumes_in_run_on_revisit() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");

    // A FIRST resume happens via the real renderer push (`set_resume_parks`/`park_resumes`):
    // injected immediately into the live shell → marks the record `attempted` (the one-shot guard).
    // So the re-resume below must survive that guard — which is exactly what the unload re-park does.
    rt.park_resumes(
        &[(
            record.clone(),
            "sess".into(),
            "printf 'NYX_FIRST\\n'".into(),
        )],
        &events,
    );
    assert!(
        wait_for_marker(&rt, &record, "NYX_FIRST"),
        "the first resume was injected into the live shell (the record is now `attempted`)"
    );

    // Unload it (idle, ZERO threshold). The unload ITSELF re-parks `claude --resume <ext>` (rebuilt
    // via decide_resume, re-arming the already-`attempted` record) — no renderer re-push, no manual
    // `park_resumes` simulating one.
    let unloaded = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert_eq!(unloaded, vec![record.clone()]);
    assert!(!rt.pty().is_live_record(&record), "unloaded → PTY closed");

    // REVISIT IN-RUN: re-spawn the terminal and drain the park at spawn — the EXACT production path
    // (`dispatch_runtime` spawns then calls `inject_resume_on_spawn`). The resume the UNLOAD re-parked
    // is re-injected automatically → the shell echoes `claude --resume ext-<id>` onto its screen.
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("re-spawn pty");
    rt.inject_resume_on_spawn(&record, &events);
    assert!(
        wait_for_marker(&rt, &record, "claude --resume ext-"),
        "the in-run revisit RE-INJECTS `claude --resume <id>` via the real spawn path (the sidecar's \
         unload re-park), with NO manual re-park"
    );
}

/// FINDING 2 — resume-eligibility GATES the unload (the "toggle = interrupteur maître" decision
/// extended to unload): a `resume_agent_sessions`-OFF project's idle session is NEVER unloaded (it
/// would never resume, so killing it would lose its context without recourse); a toggle-ON one is
/// unloaded exactly as before.
#[test]
fn toggle_off_session_is_never_unloaded_toggle_on_is() {
    let (rt, events, dir) = runtime();

    // Toggle OFF (the DEFAULT for a project): idle past the threshold, but KEPT loaded.
    let off = seed_resumable_agent_terminal(&rt, &dir, false);
    rt.pty()
        .spawn(80, 24, "", &off, 0, events.clone())
        .expect("spawn off");
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert!(
        u.is_empty() && rt.pty().is_live_record(&off),
        "a toggle-OFF idle session is KEPT (never killed without recourse — finding 2)"
    );

    // Toggle ON: the SAME idle-past-threshold session IS unloaded (existing behavior preserved),
    // while the toggle-OFF one STILL survives the same sweep.
    let on = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &on, 0, events.clone())
        .expect("spawn on");
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert_eq!(u, vec![on.clone()], "a toggle-ON idle session is unloaded");
    assert!(!rt.pty().is_live_record(&on));
    assert!(
        rt.pty().is_live_record(&off),
        "the toggle-OFF session stays loaded across the sweep"
    );
}

/// DONE-CRITERION 3 — "une session ACTIVE n'est JAMAIS unloadée". A session that is generating
/// (`Working`) is never unloaded, even with the idle clock (ZERO threshold) past the window and
/// unvisited. Only once it goes `Idle` (turn finished) does it become unload-eligible.
#[test]
fn active_session_is_never_unloaded() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");

    // Claude is GENERATING (a prompt was submitted).
    rt.activity().apply(&record, ActivityEvent::PromptSubmitted);
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert!(
        u.is_empty() && rt.pty().is_live_record(&record),
        "a WORKING session is NEVER unloaded (the safety invariant)"
    );

    // The turn ends → Idle → now it is unload-eligible.
    rt.activity().apply(&record, ActivityEvent::TurnFinished);
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert_eq!(u, vec![record.clone()], "a finished (Idle) turn unloads");
    assert!(!rt.pty().is_live_record(&record));
}

/// DONE-CRITERION 4 — "exceptions (action en cours) respectées". A session with a long tool in
/// flight reads `Working` (0-stale, no timer), so it is kept until the action completes — even
/// with the idle clock past the threshold and unvisited.
#[test]
fn action_in_flight_is_never_unloaded() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");

    rt.activity().apply(&record, ActivityEvent::PromptSubmitted);
    rt.activity().apply(&record, ActivityEvent::ToolStarted); // a long action begins.
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert!(
        u.is_empty() && rt.pty().is_live_record(&record),
        "a session with an action in flight is KEPT (the exception)"
    );

    // The action returns and the turn ends → Idle → now unload-eligible.
    rt.activity().apply(&record, ActivityEvent::ToolFinished);
    rt.activity().apply(&record, ActivityEvent::TurnFinished);
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert_eq!(u, vec![record.clone()]);
}

/// #28 — the RESUME INJECTION is gated on the CURRENT activity (belt-and-suspenders that holds AT
/// THE MOMENT OF INJECT). A parked resume is NEVER typed into a session that is right now `Working`.
/// A LIVE, WORKING terminal that receives a `set_resume_parks` push (the immediate `park_resumes`
/// inject path — the PTY is already live) must NOT get the resume line written into it — otherwise
/// it would land as keystrokes inside the running agent (the dogfood bug "resume relancé sur une
/// session vivante"). The park is left PENDING on the skip, so once the turn ends (Idle) it drains
/// and injects exactly once.
#[test]
fn resume_is_not_injected_into_a_working_session() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");

    // The session is WORKING (a prompt was submitted → Claude is generating).
    rt.activity().apply(&record, ActivityEvent::PromptSubmitted);

    // The renderer pushes the restore plan; the PTY is already live, so `park_resumes` WOULD inject
    // immediately — but the activity gate SKIPS it because the session is Working.
    rt.park_resumes(
        &[(
            record.clone(),
            "sess".into(),
            "printf 'NYX_GATED_RESUME\\n'".into(),
        )],
        &events,
    );
    assert!(
        marker_absent(&rt, &record, "NYX_GATED_RESUME"),
        "a resume is NOT injected into a WORKING session (gated on the CURRENT activity at inject)"
    );

    // The turn ends → Idle. The park was LEFT PENDING (not consumed by the skip), so the spawn-drain
    // path a revisit runs now injects it exactly once into the now-idle shell.
    rt.activity().apply(&record, ActivityEvent::TurnFinished);
    rt.inject_resume_on_spawn(&record, &events);
    assert!(
        wait_for_marker(&rt, &record, "NYX_GATED_RESUME"),
        "once the session is Idle, the still-pending resume injects exactly once"
    );
}

/// #28, item 3 — the idle-unload ↔ REACTIVATION race: a session UNLOADED (its resume re-armed) that
/// becomes ACTIVE again BEFORE the re-injection must NOT get a `claude --resume` typed into it. After
/// the unload re-parks the resume, we re-spawn the terminal and mark the session `Working` before the
/// spawn-drain inject runs; the inject gate then SKIPS — no resume fires on the live, working session.
/// (Contrast `unloaded_session_resumes_in_run_on_revisit`, where the revisit stays Idle and the
/// resume DOES inject.) Once it goes Idle again, the still-pending re-armed resume injects cleanly.
#[test]
fn reactivated_session_after_unload_is_not_resumed() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");

    // Idle → unloaded (ZERO threshold): the PTY closes and the unload RE-PARKS `claude --resume`.
    let unloaded = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert_eq!(unloaded, vec![record.clone()]);
    assert!(!rt.pty().is_live_record(&record), "unloaded → PTY closed");

    // REVISIT, but the session REACTIVATES before the resume is injected (a hook / keystroke arrived
    // between the unload and the drain): re-spawn, mark it Working, THEN attempt the spawn-drain.
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("re-spawn pty");
    rt.activity().apply(&record, ActivityEvent::PromptSubmitted); // reactivated → Working.
    rt.inject_resume_on_spawn(&record, &events);
    assert!(
        marker_absent(&rt, &record, "claude --resume ext-"),
        "a session that reactivated before the injection does NOT get a resume typed into it"
    );

    // When it later goes Idle, the still-pending (never consumed) re-armed resume injects once.
    rt.activity().apply(&record, ActivityEvent::TurnFinished);
    rt.inject_resume_on_spawn(&record, &events);
    assert!(
        wait_for_marker(&rt, &record, "claude --resume ext-"),
        "once Idle again, the re-armed resume injects exactly once"
    );
}

/// The currently-VISITED (focused) terminal is never unloaded (réveil sur ouverture) — even idle
/// past the threshold. Removing it from the visited set makes it unload-eligible.
#[test]
fn visited_session_is_never_unloaded() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");

    let visited: HashSet<String> = [record.clone()].into_iter().collect();
    let u = rt.unload_idle_sessions(Duration::ZERO, &visited, &events);
    assert!(
        u.is_empty() && rt.pty().is_live_record(&record),
        "the visited terminal is never unloaded"
    );

    // No longer visited → unloaded.
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert_eq!(u, vec![record]);
}

/// REVIEW `01KX9CP96KVK6BTHY3ZSWY1E67`, finding `01KX9GM6T3ZPESW0BJT2J0VJCX` — the idle-session
/// unload MUST be reachable OVER THE TRANSPORT, not only in-process. Every test above calls
/// `rt.unload_idle_sessions(...)` DIRECTLY, which short-circuits the wire dispatch; the production
/// path is the renderer's `nyxBridge.invoke("unload_idle_sessions", { thresholdSecs, visited })`,
/// which the sidecar routes through [`db_command::dispatch`] — the EXACT function
/// `Runtime::dispatch_runtime` calls for a `Request::DbCommand` (`runtime/mod.rs:810`).
///
/// This test drives THAT function with the EXACT args shape the renderer sends, and proves:
///   * the command dispatches to `Runtime::unload_idle_sessions` (NOT the fallback
///     `"... is not available over the sidecar transport"` Error — the bug this finding fixes);
///   * `thresholdSecs` + `visited` are mapped correctly (within-threshold KEEPS, past-threshold
///     UNLOADS; a `visited` id is spared);
///   * the returned JSON is the unloaded record ids (contract return parity).
#[test]
fn unload_idle_sessions_is_unloaded_via_db_command_dispatch() {
    let (rt, events, dir) = runtime();
    let record = seed_resumable_agent_terminal(&rt, &dir, true);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");
    assert!(
        rt.pty().is_live_record(&record),
        "the PTY is live after spawn"
    );

    // Drive the REAL dispatch exactly as the pipe run loop does — the renderer's args shape
    // (`{ thresholdSecs, visited }`, `use-idle-unload.ts:57`), serialized to JSON.
    let dispatch_unload = |threshold_secs: u64, visited: &[&str]| -> Result<Vec<String>, String> {
        let args = serde_json::json!({ "thresholdSecs": threshold_secs, "visited": visited });
        let out = db_command::dispatch(&rt, "unload_idle_sessions", &args.to_string(), &events)?;
        Ok(serde_json::from_str(&out).expect("dispatch returns a JSON string[] of record ids"))
    };

    // WITHIN the threshold (large `thresholdSecs`) → KEPT (proves `thresholdSecs` is parsed and
    // wired through to `decide_unload`, not ignored).
    let kept = dispatch_unload(3600, &[]).expect("dispatch is wired, not the fallback Error");
    assert!(
        kept.is_empty() && rt.pty().is_live_record(&record),
        "within the threshold the db-command KEEPS the session"
    );

    // VISITED (focused) → spared even past the threshold (proves `visited` is parsed + honored).
    let kept = dispatch_unload(0, &[record.as_str()]).expect("dispatch is wired");
    assert!(
        kept.is_empty() && rt.pty().is_live_record(&record),
        "a visited terminal is never unloaded via the db-command"
    );

    // PAST the threshold, unvisited → UNLOADED via the db-command: the PTY is closed and the
    // returned JSON carries the record id (the contract's documented `string[]` return).
    let unloaded = dispatch_unload(0, &[]).expect("dispatch is wired");
    assert_eq!(
        unloaded,
        vec![record.clone()],
        "the idle session is unloaded THROUGH the `unload_idle_sessions` db-command dispatch"
    );
    assert!(
        !rt.pty().is_live_record(&record),
        "the db-command dispatch actually closed the PTY (it reached Runtime::unload_idle_sessions)"
    );

    // The DB session STAYS active (unload does not end it → still a resume candidate), same as the
    // in-process invariant — proven here to hold across the transport dispatch too.
    let still_active = rt
        .db()
        .with_conn(db::active_agent_sessions)
        .unwrap()
        .into_iter()
        .any(|s| s.terminal_id == record);
    assert!(
        still_active,
        "the unloaded session stays `active` in the DB"
    );
}

/// GUARD (regression fence): the `unload_idle_sessions` db-command MUST NOT fall into the
/// dispatcher's unknown-command fallback (`db_command.rs` `other => Err("... is not available over
/// the sidecar transport")`) — the exact failure this finding fixes. A missing arm returns that
/// Error; a wired arm returns `Ok` (a JSON `[]` for a no-candidate sweep).
#[test]
fn unload_idle_sessions_db_command_is_not_the_transport_fallback() {
    let (rt, events, _dir) = runtime();
    let args = serde_json::json!({ "thresholdSecs": 0, "visited": [] });
    let result = db_command::dispatch(&rt, "unload_idle_sessions", &args.to_string(), &events);
    assert_eq!(
        result.as_deref(),
        Ok("[]"),
        "the db-command is DISPATCHED (empty sweep → `[]`), not the 'not available over the sidecar \
         transport' fallback Error"
    );
}

/// A live terminal with NO active agent session is not an unload candidate (the unload is about
/// resumable agent sessions, not bare shells).
#[test]
fn bare_terminal_without_session_is_ignored() {
    let (rt, events, _dir) = runtime();
    // A plain terminal record (no agent session seeded).
    let record = rt
        .db()
        .with_conn(|c| db::create_terminal(c, "/tmp/work", None).unwrap().id);
    rt.pty()
        .spawn(80, 24, "", &record, 0, events.clone())
        .expect("spawn pty");
    let u = rt.unload_idle_sessions(Duration::ZERO, &no_visits(), &events);
    assert!(
        u.is_empty() && rt.pty().is_live_record(&record),
        "a bare shell (no agent session) is never unloaded"
    );
}
