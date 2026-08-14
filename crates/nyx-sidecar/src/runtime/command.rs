//! Managed-command RUNTIME in the sidecar (HHD — `01KWCJPFP72FP5CDVWSB8DTHHD`).
//!
//! Ports the v1 `nyx-napi` `NyxCommandRunner` + `NodeRunnerSink` (which the TS
//! core-host owned) into the sidecar. The runner runtime itself lives in `nyx-core`
//! ([`nyx_core::command::CommandRunner`] + [`RunnerSink`]); this module is the thin
//! adapter that:
//!   * implements [`RunnerSink`] over the SHARED SQLite pool (persisting
//!     `last_state`/scrollback/outcome through the SAME `nyx_core::db` CRUD) AND the
//!     pipe [`EventSink`] (emitting `command-state` / `command-output` /
//!     `output-cleared` events to the supervisor) — the sidecar twin of the v1
//!     `NodeRunnerSink`, with binary output frames instead of base64;
//!   * exposes start / stop / relaunch / restore-on-boot / snapshot-on-shutdown /
//!     kill-all, each resolving the command line + cwd from the DB through the
//!     shell-agnostic [`nyx_core::command::resolve_command_and_cwd`].
//!
//! It is owned by the SIDECAR now (no longer the TS core-host), at parity with v1.

use std::sync::Arc;

use nyx_core::command::{self, CommandRunner, RunState, RunnerSink};
use nyx_core::db::{self, Db};
use portable_pty::PtySize;

use super::events::EventSink;
use crate::pipe::wire::{Event, Response};

/// The status of a command instance after a lifecycle call — the wire shape of the v1
/// `CommandStatus`. Built from the runner's live outcome (or the cold default).
pub struct CommandStatus {
    pub instance_id: String,
    pub state: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub unread: bool,
    pub was_running: bool,
    pub restarted: bool,
}

impl CommandStatus {
    /// Map to the wire [`Response::CommandStatus`] echoing the request `id`.
    pub fn into_response(self, id: u64) -> Response {
        Response::CommandStatus {
            id,
            instance_id: self.instance_id,
            state: self.state,
            running: self.running,
            exit_code: self.exit_code,
            unread: self.unread,
            was_running: self.was_running,
            restarted: self.restarted,
        }
    }
}

/// The [`RunnerSink`] backed by the SHARED pool + the pipe event sink — the sidecar
/// twin of the v1 `NodeRunnerSink`. Every DB write checks a connection out of the SAME
/// `Arc<Db>` the pipe dispatch + the MCP server use; every emit frames a managed-command
/// event onto the pipe.
pub struct SidecarRunnerSink {
    db: Arc<Db>,
    events: EventSink,
}

impl RunnerSink for SidecarRunnerSink {
    fn on_state(&self, instance_id: &str, state: RunState, exit_code: Option<i32>) {
        // Persist the FACTUAL outcome BEFORE emitting (a listener re-reading the row on
        // the event sees the committed value) — the SAME order as the v1 sink, but now
        // through the shared `usecase::record_command_run_state`, which writes the run state
        // AND its `command.started`/`command.stopped` `domain_events` row IN ONE transaction
        // (the outbox pattern, correlated to the instance's workspace). This is what makes a
        // start/stop appear on the workspace timeline (finding 01KWMCT58832JZW7M5GQDYZZ4B);
        // the atomicity means the timeline row can never disagree with the run-state row.
        let db_state = state.as_db_str();
        let _ = self.db.with_conn(|c| {
            nyx_core::usecase::record_command_run_state(c, instance_id, state, exit_code)
        });
        // Fine-grained per-instance transition (the existing command-band consumers patch
        // the matching dot in place from this).
        self.events.emit(Event::CommandState {
            event: "state".to_string(),
            instance_id: instance_id.to_string(),
            state: db_state.to_string(),
            exit_code,
        });
        // COARSE collection invalidation (v2.2 cockpit real-time wiring, task
        // 01KWHPV33F3Y2X4YDFPXPKJE9V): a RUNTIME start/stop/exit changes a workspace's
        // aggregate `services[].run_state`, so nudge `commands://changed` too. This is the
        // SAME "X changed → re-fetch" channel the template mutations already use, so the
        // cockpit read model (which re-pulls on the coarse topics, no polling) reflects a
        // runtime transition live — whether it was driven by the UI, an MCP tool, or the
        // boot/lifecycle. `on_state` fires only on a real transition (not on output), so
        // this is one invalidation per start/stop/exit, not per output chunk.
        self.events.emit(Event::Changed {
            topic: "commands".to_string(),
        });
    }

    fn on_acknowledge(&self, instance_id: &str) {
        let _ = self
            .db
            .with_conn(|c| db::acknowledge_instance(c, instance_id));
        self.events.emit(Event::CommandState {
            event: "ack".to_string(),
            instance_id: instance_id.to_string(),
            state: String::new(),
            exit_code: None,
        });
    }

    fn on_output(&self, instance_id: &str, bytes: &[u8]) {
        // BINARY output frame (no base64) — the sidecar improvement over the v1
        // base64-over-IPC path.
        self.events.emit(Event::CommandOutput {
            instance_id: instance_id.to_string(),
            data: bytes.to_vec(),
        });
    }

    fn persist_scrollback(&self, instance_id: &str, serialized: &str) {
        let _ = self
            .db
            .with_conn(|c| db::persist_instance_scrollback(c, instance_id, serialized));
    }

    fn archive_previous_run(&self, instance_id: &str) {
        let _ = self
            .db
            .with_conn(|c| db::archive_and_reset_for_relaunch(c, instance_id));
    }

    fn clear_output(&self, instance_id: &str) {
        let _ = self
            .db
            .with_conn(|c| db::clear_instance_scrollback(c, instance_id));
        self.events.emit(Event::CommandState {
            event: "output-cleared".to_string(),
            instance_id: instance_id.to_string(),
            state: String::new(),
            exit_code: None,
        });
    }
}

/// The managed-command runtime the sidecar owns (parity with the v1
/// `NyxCommandRunner`). Built once at boot over the SHARED pool + the pipe event sink;
/// drives the off-screen command PTYs.
pub struct CommandRuntime {
    runner: Arc<CommandRunner<SidecarRunnerSink>>,
    db: Arc<Db>,
}

impl CommandRuntime {
    /// Build the runtime over the shared pool + the pipe event sink.
    pub fn new(db: Arc<Db>, events: EventSink) -> Self {
        // A modest off-screen size: managed commands are watch-only services, not
        // interactive full-screen TUIs (80x24 — the SAME size the v1 runner uses).
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let sink = SidecarRunnerSink {
            db: Arc::clone(&db),
            events,
        };
        CommandRuntime {
            runner: Arc::new(CommandRunner::new(sink, size)),
            db,
        }
    }

    /// The shared `Arc<CommandRunner>` so the MCP dispatcher routes the runtime command
    /// tools onto the SAME runner the pipe lifecycle calls drive (no second runtime).
    pub fn runner(&self) -> Arc<CommandRunner<SidecarRunnerSink>> {
        Arc::clone(&self.runner)
    }

    /// The shared DB pool.
    pub fn db(&self) -> &Arc<Db> {
        &self.db
    }

    /// Build a [`CommandStatus`] from the runner's live outcome (or the cold default).
    fn status_of(&self, instance_id: &str) -> CommandStatus {
        let (state, exit_code, unread) =
            self.runner
                .outcome(instance_id)
                .unwrap_or((RunState::Idle, None, false));
        CommandStatus {
            instance_id: instance_id.to_string(),
            state: state.as_db_str().to_string(),
            running: state == RunState::Running,
            exit_code,
            unread,
            was_running: false,
            restarted: false,
        }
    }

    /// Start an instance (idempotent on an already-running one) with an OPTIONAL per-run
    /// `env` merged onto the inherited environment. Resolves the command line + cwd from
    /// the DB BEFORE spawning, so an unknown instance / bad subfolder is a readable error
    /// and never a half-spawn. Mirrors the v1 `NyxCommandRunner::start`.
    ///
    /// The per-run `env` is the CALLER's to supply (never hardcoded here): the MCP `start_command`
    /// carries the agent's map through [`nyx_core::mcp_runtime`] on the shared runner, and
    /// the pipe lifecycle request carries none. It is merged ON TOP of the command's PERSISTED
    /// env, which the resolution returns and which applies on every path (N31) — so a pipe start
    /// with NO per-run map still gives the command the environment its definition declares. The
    /// VALUES are never logged.
    pub fn start(
        &self,
        instance_id: &str,
        env: &[(String, String)],
    ) -> anyhow::Result<CommandStatus> {
        let (command, cwd, mut merged) = command::resolve_command_and_cwd(&self.db, instance_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        merged.extend_from_slice(env);
        let outcome = self
            .runner
            .start_with_env(instance_id, &command, Some(&cwd), &merged)
            .map_err(|e| anyhow::anyhow!("start failed: {e}"))?;
        let mut status = self.status_of(instance_id);
        status.was_running = outcome.was_running;
        status.restarted = false;
        Ok(status)
    }

    /// Stop a running instance (tree-kill of the whole process group). Idempotent.
    /// Mirrors the v1 `NyxCommandRunner::stop`.
    pub fn stop(&self, instance_id: &str) -> anyhow::Result<CommandStatus> {
        let was_running = self.runner.is_running(instance_id);
        self.runner
            .stop(instance_id)
            .map_err(|e| anyhow::anyhow!("stop failed: {e}"))?;
        let mut status = self.status_of(instance_id);
        status.was_running = was_running;
        Ok(status)
    }

    /// Relaunch (the explicit restart: stop-then-start if running, else a direct start) with an
    /// OPTIONAL per-run `env` merged ON TOP of the command's PERSISTED env (N31), both applied
    /// onto the inherited environment of the FRESH process. Mirrors the v1
    /// `NyxCommandRunner::relaunch`. The VALUES are never logged.
    pub fn relaunch(
        &self,
        instance_id: &str,
        env: &[(String, String)],
    ) -> anyhow::Result<CommandStatus> {
        let (command, cwd, mut merged) = command::resolve_command_and_cwd(&self.db, instance_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        merged.extend_from_slice(env);
        let outcome = self
            .runner
            .relaunch_with_env(instance_id, &command, Some(&cwd), &merged)
            .map_err(|e| anyhow::anyhow!("relaunch failed: {e}"))?;
        let mut status = self.status_of(instance_id);
        status.was_running = outcome.was_running;
        status.restarted = true;
        Ok(status)
    }

    /// Read an instance's captured output: the runner's LIVE in-memory tail while
    /// running, else the persisted scrollback rehydrated from the DB (the SAME source
    /// precedence as the v1 `command_output` / `get_output`). Unknown instance -> error.
    pub fn get_output(&self, instance_id: &str) -> anyhow::Result<String> {
        if let Some(live) = self.runner.live_output(instance_id) {
            return Ok(live);
        }
        self.db
            .with_conn(|c| db::get_instance(c, instance_id))
            .map_err(|e| anyhow::anyhow!("db error: {e}"))?
            .map(|inst| inst.scrollback)
            .ok_or_else(|| anyhow::anyhow!("unknown command instance {instance_id}"))
    }

    /// Acknowledge a FINISHED one-shot's unseen result: clear ONLY its `unread` flag and
    /// emit `command://ack`, never the factual outcome (parity with the v1
    /// `command_acknowledge`). Returns the factual state string after the call.
    pub fn acknowledge(&self, instance_id: &str) -> String {
        self.runner.acknowledge(instance_id).as_db_str().to_string()
    }

    /// BOOT RESTORE (reprises): relaunch every instance whose template
    /// `restart_on_startup` is ON and whose `was_running_on_shutdown` snapshot is true,
    /// normalize orphaned persisted-`running` to idle, reset the snapshots. Drives the
    /// shell-agnostic [`nyx_core::command::restore_commands_on_boot`]. Returns the
    /// relaunched instance ids.
    pub fn restore_on_boot(&self) -> Vec<String> {
        command::restore_commands_on_boot(&self.db, &self.runner)
    }

    /// SHUTDOWN SNAPSHOT (reprises): persist `was_running_on_shutdown = is_running` for
    /// every instance so the next boot can relaunch exactly those. Drives the
    /// shell-agnostic [`nyx_core::command::snapshot_commands_on_shutdown`].
    pub fn snapshot_on_shutdown(&self) {
        command::snapshot_commands_on_shutdown(&self.db, &self.runner);
    }

    /// Latch the shutdown so the snapshot + reap run EXACTLY once.
    pub fn begin_shutdown(&self) -> bool {
        self.runner.begin_shutdown()
    }

    /// Hard-kill EVERY running instance's process tree on shutdown so a managed command
    /// (its shell + child dev server) is REAPED, never orphaned.
    pub fn kill_all_running(&self) {
        self.runner.kill_all_running();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::codec::FrameDecoder;
    use crate::pipe::wire::{decode_message, Message};
    use std::sync::Mutex;

    /// A fresh on-disk migrated `Db` under a unique temp dir (the sidecar cannot use
    /// nyx-core's `#[cfg(test)]` `Db::in_memory`, which is not compiled into this crate).
    fn temp_db() -> Db {
        let dir = std::env::temp_dir().join(format!(
            "nyx-runner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("nyx.db")).expect("open db")
    }

    /// A `Write` that appends to a shared buffer, so a test can read back exactly the
    /// frames the sink pushed onto the pipe.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Decode every complete `Event` frame the shared buffer captured.
    fn captured_events(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<Event> {
        let bytes = buf.lock().unwrap().clone();
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        let mut events = Vec::new();
        while let Some(payload) = dec.next_frame().expect("decode frame") {
            if let Ok(Message::Event(e)) = decode_message(&payload) {
                events.push(e);
            }
        }
        events
    }

    /// done_criterion (task 01KWHPV33F3Y2X4YDFPXPKJE9V — "Une mutation runtime d'un
    /// workspace met à jour le cockpit en live, sans rechargement ni polling."): a managed
    /// command RUNTIME transition (start/stop/exit) — surfaced by the runner as
    /// `RunnerSink::on_state` — pushes the v2.0 UNSOLICITED coarse `commands://changed`
    /// invalidation on the pipe, the SAME channel the cockpit read model re-pulls on. That
    /// is the runtime-mutation half of the "domain_events → live" wiring: the aggregate's
    /// `services[].run_state` changes, so the sink nudges `commands` and the cockpit reflects
    /// it live. Proof: drive `on_state` for a real transition and assert the pushed frames.
    #[test]
    fn on_state_pushes_a_commands_changed_invalidation_on_a_runtime_transition() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let db = temp_db();

        // A real instance to mutate (so the persist half is a genuine write, not a no-op).
        // `create_project` yields the root workspace; adding a template materializes one
        // instance per existing workspace — so the root workspace gets one instance.
        let (project, root_ws) = db
            .with_conn(|c| db::create_project(c, "Nyx", "/tmp/runner-p", None))
            .unwrap();
        db.with_conn(|c| {
            db::create_template(
                c,
                &project.id,
                "dev",
                "npm run dev",
                None,
                db::CommandSource::default(),
            )
        })
        .unwrap();
        let instance_id = db
            .with_conn(|c| db::instance_ids_for_workspace(c, &root_ws.id))
            .unwrap()
            .pop()
            .expect("the template materialized one instance in the root workspace");

        let sink = SidecarRunnerSink {
            db: Arc::new(db),
            events,
        };

        // A RUNTIME start transition: running, no exit code yet.
        sink.on_state(&instance_id, RunState::Running, None);
        // A RUNTIME exit transition: success with an exit code.
        sink.on_state(&instance_id, RunState::Success, Some(0));

        let pushed = captured_events(&buf);
        // Each transition pushes BOTH the fine-grained per-instance `command-state` AND the
        // coarse `commands://changed` collection invalidation the cockpit re-pulls on.
        let changed = pushed
            .iter()
            .filter(|e| matches!(e, Event::Changed { topic } if topic == "commands"))
            .count();
        assert_eq!(
            changed, 2,
            "each of the two runtime transitions pushed exactly one `commands://changed`"
        );
        // And no OTHER coarse topic was invalidated (this seam is scoped to `commands`).
        assert!(
            pushed
                .iter()
                .all(|e| !matches!(e, Event::Changed { topic } if topic != "commands")),
            "on_state only invalidates the `commands` collection"
        );
    }

    /// Finding 01KWMCT58832JZW7M5GQDYZZ4B: a runtime start/stop of a managed command now
    /// persists a `command.started`/`command.stopped` `domain_events` row — correlated to the
    /// instance's workspace — through the production sink's `on_state`, IN the same transaction
    /// as the `command_instances` run-state write (via `usecase::record_command_run_state`). So
    /// the workspace timeline (which reads `domain_events` only) surfaces the runtime activity,
    /// and no terminal byte is persisted in the payload.
    #[test]
    fn on_state_persists_command_lifecycle_domain_events_for_the_timeline() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
        let db = Arc::new(temp_db());

        let (project, root_ws) = db
            .with_conn(|c| db::create_project(c, "Nyx", "/tmp/runner-tl", None))
            .unwrap();
        db.with_conn(|c| {
            db::create_template(
                c,
                &project.id,
                "dev",
                "npm run dev",
                None,
                db::CommandSource::default(),
            )
        })
        .unwrap();
        let instance_id = db
            .with_conn(|c| db::instance_ids_for_workspace(c, &root_ws.id))
            .unwrap()
            .pop()
            .expect("the template materialized one instance in the root workspace");

        let sink = SidecarRunnerSink {
            db: Arc::clone(&db),
            events,
        };
        // START then STOP.
        sink.on_state(&instance_id, RunState::Running, None);
        sink.on_state(&instance_id, RunState::Idle, None);

        // The outbox now carries the two runtime events, correlated to the workspace.
        let tl = db
            .with_conn(|c| nyx_core::workspace_timeline::timeline(c, &root_ws.id, 50))
            .unwrap()
            .unwrap();
        let kinds: Vec<&str> = tl.entries.iter().map(|e| e.event_type.as_str()).collect();
        assert!(
            kinds.contains(&"command.started"),
            "timeline surfaces command.started — got {kinds:?}"
        );
        assert!(
            kinds.contains(&"command.stopped"),
            "timeline surfaces command.stopped — got {kinds:?}"
        );
        // No terminal byte in any runtime payload.
        for e in &tl.entries {
            assert!(!e.payload.contains("\\x1b"), "no terminal escape bytes");
            assert!(!e.payload.to_lowercase().contains("scrollback"));
        }
        // The factual run-state write happened atomically with the event.
        let inst = db
            .with_conn(|c| db::get_instance(c, &instance_id))
            .unwrap()
            .unwrap();
        assert_eq!(inst.last_state, "idle");
    }
}
