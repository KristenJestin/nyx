//! Managed-command RUNTIME exposed over napi (review task — the extracted
//! `ManagedCommandRunner`). The Electron core-host owns ONE [`NyxCommandRunner`]
//! over the SHARED r2d2 pool, exactly as the Tauri adapter owns ONE
//! `ManagedCommandRunner` over the managed `Db`. The runner runtime itself lives in
//! `nyx-core` ([`nyx_core::command::CommandRunner`] + [`RunnerSink`]); this module is
//! the thin napi adapter that:
//!
//! - implements [`RunnerSink`] over **Node callbacks** (`command://state`,
//!   `command://output`, `command://ack`, `command://output-cleared`) delivered on
//!   the Node loop via `ThreadsafeFunction`, and persists `last_state`/scrollback to
//!   the SAME pool the napi DB tasks + the MCP server use (no second authority) — the
//!   napi twin of `bridge::TauriRunnerSink`;
//! - exposes `start` / `stop` / `relaunch` / `get_output` / `restore_on_boot` /
//!   `snapshot_on_shutdown` / `begin_shutdown` / `kill_all_running` / `is_running`,
//!   each resolving its command line + cwd from the DB through the shell-agnostic
//!   [`nyx_core::command::resolve_command_and_cwd`].
//!
//! It is loaded ONLY in the core-host (the `.node` never enters main/renderer), so the
//! managed-command runtime is OWNED by the core-host, at parity with Tauri.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use portable_pty::PtySize;

use nyx_core::command::{self, CommandRunner, RunState, RunnerSink};
use nyx_core::db::{self, Db};

/// A command-runtime event surfaced to Node (the `command://state` / `command://ack`
/// transitions, plus an output-cleared tick). The host maps `kind` to the matching
/// Tauri event name so the renderer's command band behaves identically.
#[napi(object)]
pub struct CommandStateEvent {
    /// `"state"` (a run-state transition), `"ack"` (unread cleared), or
    /// `"output-cleared"` (the captured buffer was wiped).
    pub kind: String,
    /// The `command_instances.id` the event is about.
    pub instance_id: String,
    /// The `idle|running|success|error` string for a `"state"` event; empty otherwise.
    pub state: String,
    /// The natural exit code on a `success`/`error` finish; `None` otherwise.
    pub exit_code: Option<i32>,
}

/// A coalesced command-output chunk surfaced to Node (`command://output`). Bytes are
/// delivered as a `Buffer` on the Node loop, ordered per instance.
#[napi(object)]
pub struct CommandOutputEvent {
    /// The `command_instances.id` whose output this is.
    pub instance_id: String,
    /// The coalesced output bytes (raw — the renderer's xterm renders them as-is).
    pub bytes: Buffer,
}

/// The status of a command instance after a lifecycle call, mirrored to Node — the
/// napi shape of the runner's live `outcome` (or the cold default). `running` is the
/// live flag the renderer's dot reads; `state`/`exit_code` are the factual outcome.
#[napi(object)]
pub struct CommandStatus {
    /// The `command_instances.id`.
    pub instance_id: String,
    /// `idle|running|success|error`.
    pub state: String,
    /// Whether the instance is LIVE-running right now.
    pub running: bool,
    /// The natural exit code of the last finished run, if any.
    pub exit_code: Option<i32>,
    /// Whether the last finished run is still an unseen result.
    pub unread: bool,
    /// Whether the instance was ALREADY running before a `start`/`relaunch` call
    /// (`true` ⇒ a `start` was an idempotent no-op). Defaults to `false` for reads.
    pub was_running: bool,
    /// Whether this call RESTARTED the instance (`relaunch` ⇒ `true`, `start` ⇒
    /// `false`). Defaults to `false` for reads.
    pub restarted: bool,
}

/// The [`RunnerSink`] backed by Node callbacks + the SHARED pool — the napi twin of
/// `bridge::TauriRunnerSink`. Every DB write here checks a connection out of the SAME
/// `Arc<Db>` the napi DB tasks + the MCP server use; every emit hops to the Node loop
/// through a `ThreadsafeFunction`.
pub(crate) struct NodeRunnerSink {
    db: Arc<Db>,
    state_tsfn: ThreadsafeFunction<CommandStateEvent, ErrorStrategy::Fatal>,
    output_tsfn: ThreadsafeFunction<CommandOutputEvent, ErrorStrategy::Fatal>,
}

impl RunnerSink for NodeRunnerSink {
    fn on_state(&self, instance_id: &str, state: RunState, exit_code: Option<i32>) {
        // Persist the FACTUAL outcome BEFORE emitting (a listener re-reading the row on
        // the event sees the committed value) — the SAME order + helper as the Tauri
        // sink. A success/error records the v4 outcome columns; a `running` clears the
        // prior code; `idle` touches only `last_state`.
        let db_state = state.as_db_str();
        let _ = self
            .db
            .with_conn(|c| db::set_run_state(c, instance_id, db_state, exit_code));
        self.state_tsfn.call(
            CommandStateEvent {
                kind: "state".to_string(),
                instance_id: instance_id.to_string(),
                state: db_state.to_string(),
                exit_code,
            },
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    fn on_acknowledge(&self, instance_id: &str) {
        // Clear ONLY the persisted `unread` flag (the factual outcome is untouched),
        // then emit the ack so the UI hides the settled badge with NO state change.
        let _ = self
            .db
            .with_conn(|c| db::acknowledge_instance(c, instance_id));
        self.state_tsfn.call(
            CommandStateEvent {
                kind: "ack".to_string(),
                instance_id: instance_id.to_string(),
                state: String::new(),
                exit_code: None,
            },
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    fn on_output(&self, instance_id: &str, bytes: &[u8]) {
        self.output_tsfn.call(
            CommandOutputEvent {
                instance_id: instance_id.to_string(),
                bytes: Buffer::from(bytes.to_vec()),
            },
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    fn persist_scrollback(&self, instance_id: &str, serialized: &str) {
        let _ = self
            .db
            .with_conn(|c| db::persist_instance_scrollback(c, instance_id, serialized));
    }

    fn archive_previous_run(&self, instance_id: &str) {
        // A fresh (re)launch: archive the completing run into the bounded `prev_*`
        // columns (N=1) and reset the current run to a clean `running` row, in one
        // transaction — so a `get_command_output(run="previous")` still reads it while
        // the new run starts unpolluted.
        let _ = self
            .db
            .with_conn(|c| db::archive_and_reset_for_relaunch(c, instance_id));
    }

    fn clear_output(&self, instance_id: &str) {
        // Empty the persisted scrollback (current + retained prior run) WITHOUT touching
        // the factual outcome columns, then emit the dedicated clear tick so the
        // read-only output panel wipes its xterm.
        let _ = self
            .db
            .with_conn(|c| db::clear_instance_scrollback(c, instance_id));
        self.state_tsfn.call(
            CommandStateEvent {
                kind: "output-cleared".to_string(),
                instance_id: instance_id.to_string(),
                state: String::new(),
                exit_code: None,
            },
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }
}

/// The managed-command runner the Electron core-host owns (parity with the Tauri
/// `ManagedCommandRunner`). Built once at boot over the SHARED pool + the Node event
/// callbacks; drives the off-screen command PTYs.
#[napi]
pub struct NyxCommandRunner {
    runner: Arc<CommandRunner<NodeRunnerSink>>,
    db: Arc<Db>,
}

impl NyxCommandRunner {
    /// Build a runner over the shared pool + Node callbacks. Used internally by the
    /// `NyxCore` factory (the host never holds two pools).
    pub(crate) fn build(
        db: Arc<Db>,
        state_tsfn: ThreadsafeFunction<CommandStateEvent, ErrorStrategy::Fatal>,
        output_tsfn: ThreadsafeFunction<CommandOutputEvent, ErrorStrategy::Fatal>,
    ) -> Self {
        // A modest off-screen size: managed commands are watch-only services, not
        // interactive full-screen TUIs (80x24 — the SAME size the Tauri runner uses).
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let sink = NodeRunnerSink {
            db: Arc::clone(&db),
            state_tsfn,
            output_tsfn,
        };
        NyxCommandRunner {
            runner: Arc::new(CommandRunner::new(sink, size)),
            db,
        }
    }

    /// The shared `Arc<CommandRunner>` so the MCP dispatcher can route the runtime
    /// command tools onto the SAME runner the host's lifecycle calls drive.
    pub(crate) fn runner(&self) -> Arc<CommandRunner<NodeRunnerSink>> {
        Arc::clone(&self.runner)
    }
}

/// Build a [`CommandStatus`] from the runner's live outcome (or the cold default).
fn status_of(runner: &CommandRunner<NodeRunnerSink>, instance_id: &str) -> CommandStatus {
    let (state, exit_code, unread) =
        runner
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

#[napi]
impl NyxCommandRunner {
    /// Start an instance (idempotent on an already-running one — the no-double-spawn
    /// guard lives at the runner boundary). Resolves the command line + cwd from the DB
    /// (validated subfolder) BEFORE spawning, so an unknown instance / bad subfolder is
    /// a readable error and never a half-spawn. Returns the status with `was_running`
    /// (the call was a no-op) and `restarted:false` (a start is never a restart).
    #[napi]
    pub fn start(&self, instance_id: String) -> Result<CommandStatus> {
        let (command, cwd) =
            command::resolve_command_and_cwd(&self.db, &instance_id).map_err(Error::from_reason)?;
        let outcome = self
            .runner
            .start_with_env(&instance_id, &command, Some(&cwd), &[])
            .map_err(|e| Error::from_reason(format!("start failed: {e}")))?;
        let mut status = status_of(&self.runner, &instance_id);
        status.was_running = outcome.was_running;
        status.restarted = false;
        Ok(status)
    }

    /// Stop a running instance (tree-kill of the whole process group). Idempotent on a
    /// non-running instance (returns the current status, never an error). `was_running`
    /// reports liveness BEFORE the stop.
    #[napi]
    pub fn stop(&self, instance_id: String) -> Result<CommandStatus> {
        let was_running = self.runner.is_running(&instance_id);
        self.runner
            .stop(&instance_id)
            .map_err(|e| Error::from_reason(format!("stop failed: {e}")))?;
        let mut status = status_of(&self.runner, &instance_id);
        status.was_running = was_running;
        Ok(status)
    }

    /// Relaunch (the EXPLICIT restart — stop-then-start if running, else a direct
    /// start; never leaves two live processes). Resolves command + cwd like `start`.
    /// `restarted:true`; `was_running` reports whether a live process was stopped first.
    #[napi]
    pub fn relaunch(&self, instance_id: String) -> Result<CommandStatus> {
        let (command, cwd) =
            command::resolve_command_and_cwd(&self.db, &instance_id).map_err(Error::from_reason)?;
        let outcome = self
            .runner
            .relaunch_with_env(&instance_id, &command, Some(&cwd), &[])
            .map_err(|e| Error::from_reason(format!("relaunch failed: {e}")))?;
        let mut status = status_of(&self.runner, &instance_id);
        status.was_running = outcome.was_running;
        status.restarted = true;
        Ok(status)
    }

    /// Read an instance's captured output: the runner's LIVE in-memory tail while
    /// running, else the persisted scrollback rehydrated from the DB (the SAME source
    /// precedence as `bridge::command_output`). Unknown instance → a readable error.
    #[napi]
    pub fn get_output(&self, instance_id: String) -> Result<String> {
        if let Some(live) = self.runner.live_output(&instance_id) {
            return Ok(live);
        }
        self.db
            .with_conn(|c| db::get_instance(c, &instance_id))
            .map_err(|e| Error::from_reason(format!("db error: {e}")))?
            .map(|inst| inst.scrollback)
            .ok_or_else(|| Error::from_reason(format!("unknown command instance {instance_id}")))
    }

    /// The live run status of an instance (no mutation) — the runner outcome or the
    /// cold default. Used by the host / MCP `get_command_output` status block.
    #[napi]
    pub fn status(&self, instance_id: String) -> CommandStatus {
        status_of(&self.runner, &instance_id)
    }

    /// Whether the instance currently has a LIVE running process in the runner.
    #[napi]
    pub fn is_running(&self, instance_id: String) -> bool {
        self.runner.is_running(&instance_id)
    }

    /// Acknowledge a FINISHED one-shot's "unseen result" (parity with the Tauri
    /// `command_acknowledge` / `acknowledge_unread`): clear ONLY its `unread` flag and
    /// emit `command://ack`, NEVER its factual outcome. Two paths, both clearing only the
    /// unread flag:
    ///   - LIVE entry (a run that finished this session): the runner flips its in-memory
    ///     `unread` and (via the sink) persists `unread=0` + emits `command://ack`;
    ///   - PERSISTED-only state (no live entry, e.g. a success/error restored at boot):
    ///     clear the persisted `unread` here and emit `command://ack` via the sink — so a
    ///     restored, still-unread badge also hides on select.
    ///
    /// A `running` instance is never acknowledged (no unseen result yet). Returns the
    /// FACTUAL `last_state` string after the call (unchanged by the ack). Unknown id → a
    /// readable error.
    #[napi]
    pub fn acknowledge(&self, instance_id: String) -> Result<String> {
        use nyx_core::command::RunState;
        // Never acknowledge a live process — it has no unseen result yet.
        if self.runner.is_running(&instance_id) {
            return Ok(RunState::Running.as_db_str().to_string());
        }
        // LIVE entry: the runner clears its in-memory `unread` and (via the sink)
        // persists `unread=0` + emits `command://ack`. `is_unread` tells us whether the
        // runner just handled it so we don't double-emit for the same acknowledge.
        let runner_had_unread = self.runner.is_unread(&instance_id);
        self.runner.acknowledge(&instance_id);
        if runner_had_unread {
            return Ok(self.runner.state_of(&instance_id).as_db_str().to_string());
        }
        // PERSISTED-only state with no live entry: clear its `unread` here so a restored,
        // still-unread success/error badge also hides on select — WITHOUT touching the
        // factual outcome. Emit `command://ack` through the sink so the badge hides.
        let inst = self
            .db
            .with_conn(|c| db::get_instance(c, &instance_id))
            .map_err(|e| Error::from_reason(format!("db error: {e}")))?
            .ok_or_else(|| Error::from_reason(format!("unknown command instance {instance_id}")))?;
        if inst.unread
            && (inst.last_state == db::STATE_SUCCESS || inst.last_state == db::STATE_ERROR)
        {
            self.db
                .with_conn(|c| db::acknowledge_instance(c, &instance_id))
                .map_err(|e| Error::from_reason(format!("db error: {e}")))?;
            self.runner.sink().on_acknowledge(&instance_id);
        }
        Ok(inst.last_state)
    }

    /// BOOT RESTORE (parity with the Tauri `setup`): relaunch every instance whose
    /// template `restart_on_startup` is ON and whose `was_running_on_shutdown` snapshot
    /// is true, normalize any orphaned persisted-`running` to idle, and reset the
    /// snapshots. Returns the relaunched instance ids. Drives the SHELL-AGNOSTIC
    /// `nyx_core::command::restore_commands_on_boot` (the identical code the Tauri
    /// adapter delegates to).
    #[napi]
    pub fn restore_on_boot(&self) -> Vec<String> {
        command::restore_commands_on_boot(&self.db, &self.runner)
    }

    /// SHUTDOWN SNAPSHOT (parity with the Tauri close hook): for every instance persist
    /// `was_running_on_shutdown = is_running`. Drives the shell-agnostic
    /// `nyx_core::command::snapshot_commands_on_shutdown`. Idempotent under the
    /// `begin_shutdown` latch (call it before `kill_all_running`).
    #[napi]
    pub fn snapshot_on_shutdown(&self) {
        command::snapshot_commands_on_shutdown(&self.db, &self.runner);
    }

    /// Latch the shutdown so the snapshot+reap run EXACTLY once (the close-request +
    /// destroy double-event guard). Returns `true` only the first time.
    #[napi]
    pub fn begin_shutdown(&self) -> bool {
        self.runner.begin_shutdown()
    }

    /// Hard-kill EVERY running instance's process tree on shutdown so a managed command
    /// (its shell + child dev server + the Windows conhost) is REAPED, never orphaned.
    /// Best-effort + non-blocking.
    #[napi]
    pub fn kill_all_running(&self) {
        self.runner.kill_all_running();
    }
}
