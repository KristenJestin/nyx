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
        // the event sees the committed value) — the SAME order + helper as the v1 sink.
        let db_state = state.as_db_str();
        let _ = self
            .db
            .with_conn(|c| db::set_run_state(c, instance_id, db_state, exit_code));
        self.events.emit(Event::CommandState {
            event: "state".to_string(),
            instance_id: instance_id.to_string(),
            state: db_state.to_string(),
            exit_code,
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

    /// Start an instance (idempotent on an already-running one). Resolves the command
    /// line + cwd from the DB BEFORE spawning, so an unknown instance / bad subfolder is
    /// a readable error and never a half-spawn. Mirrors the v1 `NyxCommandRunner::start`.
    pub fn start(&self, instance_id: &str) -> anyhow::Result<CommandStatus> {
        let (command, cwd) = command::resolve_command_and_cwd(&self.db, instance_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let outcome = self
            .runner
            .start_with_env(instance_id, &command, Some(&cwd), &[])
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

    /// Relaunch (the explicit restart: stop-then-start if running, else a direct
    /// start). Mirrors the v1 `NyxCommandRunner::relaunch`.
    pub fn relaunch(&self, instance_id: &str) -> anyhow::Result<CommandStatus> {
        let (command, cwd) = command::resolve_command_and_cwd(&self.db, instance_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let outcome = self
            .runner
            .relaunch_with_env(instance_id, &command, Some(&cwd), &[])
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
