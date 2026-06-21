//! `NyxCore` — the napi handle the Electron core-host owns over the SHARED nyx-core
//! state. It holds the **r2d2 DB pool** (`nyx_core::db::Db`, WAL + foreign_keys +
//! busy_timeout per connection) and the **MCP server**, both built once at boot and
//! shared: the napi DB tasks and the MCP server threads check connections out of the
//! SAME pool, with NO global `Mutex<SqliteConnection>` (PRD-5 task #2 / #3).
//!
//! ## Why `AsyncTask`
//!
//! Every DB call the host makes is exposed as a napi `AsyncTask` (`-> AsyncTask<…>`,
//! surfaced to Node as a `Promise`). napi-rs schedules the task's `compute()` on the
//! **libuv worker pool**, NOT the Node main loop — so a Diesel query (even a slow one)
//! runs on a worker thread and the host's Node event loop keeps servicing IPC messages
//! and PTY output callbacks while it is in flight. This is the frozen decision: *no
//! Diesel query ever blocks the Node loop*, and *no Tokio runtime is introduced* — the
//! concurrency comes from libuv's existing worker pool plus the connection pool.
//!
//! The pool lives in an `Arc<Db>` so the SAME pool backs both the async DB tasks here
//! and the MCP server ([`crate::mcp`]); cloning `NyxCore`'s `Arc` hands out another
//! reference to the one pool, never a second connection authority.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction};
use napi::Task;
use napi_derive::napi;

use nyx_core::command::CommandRunner;
use nyx_core::db::{self, Db};

use crate::command::{CommandOutputEvent, CommandStateEvent, NodeRunnerSink, NyxCommandRunner};

/// A terminal record surfaced to Node for the restore flow (PRD-5 task #5). A
/// snake-free subset of `nyx_core::db::Terminal` — the fields the host needs to
/// re-open a terminal and re-evaluate its exec-state at boot. napi maps this to a
/// plain JS object.
#[napi(object)]
pub struct TerminalRow {
    /// The persistent `terminals.id` (the sidebar / event key).
    pub id: String,
    /// `alive` | `closed` — only `alive` terminals are re-spawned at boot.
    pub status: String,
    /// The terminal's last working dir (for the re-spawned shell's cwd).
    pub cwd: String,
    /// Optional user label.
    pub label: Option<String>,
    /// Sidebar order.
    pub order_index: i32,
    /// The workspace this terminal is attached to (`None` = loose).
    pub workspace_id: Option<String>,
    /// `idle` | `running` | `success` | `error` — the persisted exec-state badge.
    pub exec_state: String,
    /// Settled exit code, if any.
    pub exec_exit_code: Option<i32>,
    /// Whether the settled exec-state is an unread notification.
    pub exec_state_unread: bool,
    /// Epoch-ms of the last exec-state transition.
    pub exec_state_updated_at: i64,
}

impl From<db::Terminal> for TerminalRow {
    fn from(t: db::Terminal) -> Self {
        TerminalRow {
            id: t.id,
            status: t.status,
            cwd: t.cwd,
            label: t.label,
            order_index: t.order_index,
            workspace_id: t.workspace_id,
            exec_state: t.exec_state,
            exec_exit_code: t.exec_exit_code,
            exec_state_unread: t.exec_state_unread,
            exec_state_updated_at: t.exec_state_updated_at,
        }
    }
}

/// The result of persisting an exec-state transition (PRD-5 task #1): the stamped
/// `exec_state_updated_at` so the host's `terminal://exec-state` event carries the
/// exact persisted timestamp (parity with the Tauri `persist_and_emit_exec_state`).
/// `updated` is `false` when the terminal id was unknown (no row updated) — the host
/// then SKIPS the emit, never announcing a state the DB does not hold.
#[napi(object)]
pub struct ExecStatePersist {
    /// Whether a row was actually updated (the terminal id exists).
    pub updated: bool,
    /// The stamped `exec_state_updated_at` (epoch-ms); `0` when `updated` is false.
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// AsyncTask implementations — each runs `compute()` on a libuv worker thread.
// ---------------------------------------------------------------------------

/// Persist a terminal exec-state transition off the Node loop, then read back the
/// stamped timestamp (the SAME order as the Tauri `persist_and_emit_exec_state`: DB
/// write FIRST so a listener re-reading the row on the event sees the committed value).
pub struct SetExecStateTask {
    db: Arc<Db>,
    terminal_id: String,
    state: String,
    exit_code: Option<i32>,
    unread: bool,
}

impl Task for SetExecStateTask {
    type Output = ExecStatePersist;
    type JsValue = ExecStatePersist;

    fn compute(&mut self) -> Result<Self::Output> {
        self.db.with_conn(|c| {
            let updated = db::set_exec_state(
                c,
                &self.terminal_id,
                &self.state,
                self.exit_code,
                self.unread,
            )
            .map_err(db_err)?;
            if updated == 0 {
                return Ok(ExecStatePersist {
                    updated: false,
                    updated_at: 0,
                });
            }
            // Read back the stamped timestamp so the host's event `updated_at`
            // matches the persisted `exec_state_updated_at` exactly.
            let updated_at = db::get_terminal(c, &self.terminal_id)
                .map_err(db_err)?
                .map(|t| t.exec_state_updated_at)
                .unwrap_or(0);
            Ok(ExecStatePersist {
                updated: true,
                updated_at,
            })
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// Read the current persisted exec-state of a terminal (so the host can settle a
/// `running` ghost to idle on PTY exit — the napi mirror of `normalize_exec_state_on_exit`).
pub struct GetTerminalTask {
    db: Arc<Db>,
    terminal_id: String,
}

impl Task for GetTerminalTask {
    type Output = Option<TerminalRow>;
    type JsValue = Option<TerminalRow>;

    fn compute(&mut self) -> Result<Self::Output> {
        self.db
            .with_conn(|c| db::get_terminal(c, &self.terminal_id))
            .map(|opt| opt.map(TerminalRow::from))
            .map_err(db_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// List every terminal in sidebar order (the boot restore read — PRD-5 task #5).
pub struct ListTerminalsTask {
    db: Arc<Db>,
}

impl Task for ListTerminalsTask {
    type Output = Vec<TerminalRow>;
    type JsValue = Vec<TerminalRow>;

    fn compute(&mut self) -> Result<Self::Output> {
        self.db
            .with_conn(db::list_terminals)
            .map(|rows| rows.into_iter().map(TerminalRow::from).collect())
            .map_err(db_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// BOOT NORMALIZATION (PRD-5 task #5 / task #2): settle every terminal stuck at a
/// persisted `exec_state = running` (force-quit artefact) down to idle. Returns the
/// count normalized.
pub struct NormalizeTerminalsTask {
    db: Arc<Db>,
}

impl Task for NormalizeTerminalsTask {
    type Output = u32;
    type JsValue = u32;

    fn compute(&mut self) -> Result<Self::Output> {
        self.db
            .with_conn(db::normalize_phantom_running_terminals)
            .map(|n| n as u32)
            .map_err(db_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// Create a new alive terminal record (PRD-5 task #5 — the host persists a new
/// terminal so restore can re-open it). Returns the created row.
pub struct CreateTerminalTask {
    db: Arc<Db>,
    cwd: String,
    label: Option<String>,
}

impl Task for CreateTerminalTask {
    type Output = TerminalRow;
    type JsValue = TerminalRow;

    fn compute(&mut self) -> Result<Self::Output> {
        self.db
            .with_conn(|c| db::create_terminal(c, &self.cwd, self.label.clone()))
            .map(TerminalRow::from)
            .map_err(db_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// The result of an auto-attach pass surfaced to Node (the `auto_attach_terminal` command
/// result) — mirrors `nyx_core::resolve::AutoAttachResult`. The front reflects the new
/// binding ONLY when `changed`, so it never silently un-pins a manual terminal.
#[napi(object)]
pub struct AutoAttachResultJs {
    /// The workspace the terminal is bound to after the pass (`null` = loose).
    pub workspace_id: Option<String>,
    /// Whether the binding actually changed.
    pub changed: bool,
}

/// Auto-attach a terminal RECORD to a workspace by its live `cwd`, off the Node loop
/// (PRD-5 auto-attach revival). The front reads the live cwd via `terminal_info` then
/// passes it here; this runs the SHARED `nyx_core::resolve::auto_attach_terminal` (the
/// SAME body the Tauri command runs) which applies `decide_attachment` + persists the
/// decided binding. Resolves to `{ workspaceId, changed }`.
pub struct AutoAttachTask {
    db: Arc<Db>,
    terminal_id: String,
    cwd: Option<String>,
}

impl Task for AutoAttachTask {
    type Output = AutoAttachResultJs;
    type JsValue = AutoAttachResultJs;

    fn compute(&mut self) -> Result<Self::Output> {
        self.db
            .with_conn(|c| {
                nyx_core::resolve::auto_attach_terminal(c, &self.terminal_id, self.cwd.as_deref())
            })
            .map(|r| AutoAttachResultJs {
                workspace_id: r.workspace_id,
                changed: r.changed,
            })
            .map_err(db_err)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// The GENERIC DB-COMMAND dispatcher task (PRD-5 review — full nyxBridge surface
/// end-to-end over the real Electron IPC). The Electron core-host owns NO per-command
/// napi method for the DB-backed long tail (projects / workspaces / managed-command
/// templates / agent sessions / terminal CRUD); instead it forwards the contract's
/// `BackendCommand` NAME + a JSON args blob through this ONE allowlisted, typed task,
/// which dispatches to the SAME unit-tested `nyx_core::db` CRUD the Tauri bridge calls.
///
/// Why a single dispatcher (not 40 napi methods): the surface is a closed,
/// compile-checked match on the command name (an unknown name is a readable error,
/// never a silent no-op), every arm runs on a libuv WORKER thread (off the Node loop,
/// same non-blocking guarantee as every other DB task), and the JSON in/out keeps the
/// wire shape identical to the Tauri `invoke` the front already speaks — so the
/// renderer's `nyxBridge.invoke(name, args)` round-trips end to end with NO per-command
/// host code. The result is serialized to a JSON STRING (parsed back to the contract
/// shape by the host); a `()` command resolves to JSON `null`.
///
/// The Settings → Integrations commands (`integration_list` / `integration_install` /
/// `integration_remove`) ARE dispatched here (PRD-5 review #58): they route onto the
/// SHARED `nyx_core::integrations` cores, so the Electron Install/Uninstall button drives
/// the EXACT same `claude` plugin install/uninstall the Tauri command body drives. The
/// `claude` CLI shell-out is wall-clock bounded in nyx-core, and the whole task runs on a
/// libuv worker (off the Node loop), so a stuck `claude` can never freeze the host.
///
/// The FULL managed-command TEMPLATE surface is dispatched here now (PRD-5 review #63 —
/// the "Could not save the command" gap): create / update / delete, the package.json
/// SOURCE actions (`command_source_refresh` / `command_resync_source` /
/// `command_unlink_source`), and the IMPORT actions (`command_import_scripts` /
/// `command_import_create`). Each mirrors the Tauri `command_*` body exactly — the SAME
/// `nyx_core::db` CRUD, the SAME `nyx_core::pkgjson` source-infer/detach/refresh/import
/// helpers (now shared across both shells), and the SAME running-guard (via the shared
/// runner cell) — so no command of the contract used by the UI returns "not available".
///
/// `agent_close_warnings` is dispatched here too (PRD-5 #6 — the "close warning is dead on
/// Electron" gap): it is a PURE DB read (the live `active`/`unknown` sessions of a
/// non-resuming project, joined for the warning message), exactly the same nature as
/// `agent_active_sessions` — NOT a live-runtime scan. It mirrors the Tauri command body so
/// the Electron CloseWarningDialog fires at parity instead of the window always closing.
///
/// Commands that genuinely need live PTY/process state are the ONLY ones still N/A over
/// this transport (they return a readable "not available" error so the allowlist stays
/// honest): `terminal_info` / `register_terminal_pty` / `auto_attach_terminal` (live PTY
/// introspection — handled by the host's PTY manager / not surfaced on Electron yet)
/// and `window_controls_visible` (window chrome, served by the host window bridge, never
/// the DB dispatcher). The managed-command
/// RUNTIME (`command_start`/`stop`/`relaunch`/`output`/`acknowledge`) is routed by the host
/// onto the live `CommandManager` (the runner that owns the off-screen PTYs), NOT here.
pub struct DbCommandTask {
    db: Arc<Db>,
    command: String,
    /// The contract args, as a parsed JSON object (`{}` when the front passed none).
    args: serde_json::Value,
    /// The host's resolved data dir — the `integrations.json` cache lives under it, and it
    /// is the stable app-data target the bundled Claude plugin is copied INTO at install
    /// (parity with the Tauri `resolve_data_dir`).
    data_dir: String,
    /// The host's read-only bundled-resources dir (the bundled Claude plugin SOURCE in a
    /// packaged build; `None` falls back to the dev source tree). Captured at boot from the
    /// host's `mcpReconcile(dataDir, resourceDir)` call — the SAME resource base the Tauri
    /// `claude_plugin_install` resolves.
    resource_dir: Option<String>,
    /// The shared managed-command runner cell — the SAME `Arc` `NyxCore` holds (the host
    /// builds the runner via `createCommandRunner`). Used by the template-mutation arms
    /// (`command_update`/`command_delete`/`command_resync_source`) to REFUSE a mutation
    /// while any of the template's instances is running (parity with the Tauri
    /// `guard_template_not_running`). `None`/un-built runner ⇒ nothing is running yet.
    runner: Arc<std::sync::Mutex<Option<Arc<CommandRunner<NodeRunnerSink>>>>>,
}

impl DbCommandTask {
    /// Read a required string arg (`name`) from the JSON object, or a readable error.
    fn arg_str(&self, name: &str) -> Result<String> {
        self.args
            .get(name)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                Error::from_reason(format!("{}: missing string arg '{name}'", self.command))
            })
    }

    /// Read an optional string arg (`null`/absent → `None`).
    fn arg_opt_str(&self, name: &str) -> Option<String> {
        self.args
            .get(name)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Read a required bool arg, or a readable error.
    fn arg_bool(&self, name: &str) -> Result<bool> {
        self.args
            .get(name)
            .and_then(|v| v.as_bool())
            .ok_or_else(|| {
                Error::from_reason(format!("{}: missing bool arg '{name}'", self.command))
            })
    }

    /// Read an optional bool arg (`null`/absent → `None`).
    fn arg_opt_bool(&self, name: &str) -> Option<bool> {
        self.args.get(name).and_then(|v| v.as_bool())
    }

    /// Read a required `string[]` arg, or a readable error.
    fn arg_str_vec(&self, name: &str) -> Result<Vec<String>> {
        self.args
            .get(name)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .ok_or_else(|| {
                Error::from_reason(format!("{}: missing string[] arg '{name}'", self.command))
            })
    }

    /// Serialize a value to the JSON-string result the host parses back.
    fn json<T: serde::Serialize>(v: &T) -> Result<String> {
        serde_json::to_string(v).map_err(|e| Error::from_reason(format!("serialize result: {e}")))
    }

    /// Resolve the Claude plugin install descriptor + CLI driver from the host's resolved
    /// dirs — the napi mirror of the Tauri `claude_plugin_install` / `claude_plugin_cli`:
    /// the bundled plugin SOURCE (via `resource_dir`), the STABLE install dir (under
    /// `data_dir`), the settings path, and the `claude plugin` CLI driver. Returns the
    /// descriptor + CLI (each `None` when its paths cannot be resolved — the integrations
    /// core then surfaces a readable error rather than a fake success).
    fn claude_plugin(
        &self,
    ) -> (
        Option<nyx_core::plugin::PluginInstall>,
        Option<Box<dyn nyx_core::plugin::PluginCli>>,
    ) {
        use nyx_core::agent::AgentAdapter;
        let resource_dir = self.resource_dir.as_ref().map(std::path::PathBuf::from);
        let app_data_dir = std::path::PathBuf::from(&self.data_dir);
        let install = nyx_core::agent::ClaudeCodeAdapter
            .plugin_install(resource_dir.as_deref(), Some(&app_data_dir));
        let cli = nyx_core::agent::ClaudeCodeAdapter.plugin_cli();
        (install, cli)
    }

    /// Refuse a template mutation while any of its instances is running (parity with the
    /// Tauri `guard_template_not_running`). The live runner's map is authoritative; the
    /// persisted `last_state` is only a mirror. A `None`/un-built runner means nothing is
    /// running yet, so the guard passes. Returns a clear user-facing error when blocked.
    fn guard_template_not_running(&self, template_id: &str) -> Result<()> {
        let instance_ids = self
            .db
            .with_conn(|c| db::instance_ids_for_template(c, template_id))
            .map_err(db_err)?;
        let running = {
            let guard = self.runner.lock().unwrap();
            guard
                .as_ref()
                .map(|r| r.any_running(&instance_ids))
                .unwrap_or(false)
        };
        if running {
            return Err(Error::from_reason(
                "this command is running in at least one workspace — stop it before editing or deleting it".to_string(),
            ));
        }
        Ok(())
    }

    /// Install the integration for `provider` (parity with the Tauri `integration_install`):
    /// resolve the Claude plugin descriptor + the legacy-MCP `OnboardingTarget`, then drive
    /// the SHARED [`nyx_core::integrations::install`] core. Returns the post-mutation status
    /// read from Claude's real config.
    fn integration_install(
        &self,
        provider: &str,
    ) -> Result<nyx_core::integrations::IntegrationStatus> {
        let data_dir = std::path::PathBuf::from(&self.data_dir);
        let state_path = nyx_core::integrations::state_path_in(&data_dir);
        let target = nyx_core::onboarding::OnboardingTarget::claude_code().ok_or_else(|| {
            Error::from_reason(
                "Could not resolve Claude Code config path (no home dir)".to_string(),
            )
        })?;
        let (install, cli) = self.claude_plugin();
        nyx_core::integrations::install(
            provider,
            &target,
            install.as_ref(),
            cli.as_deref(),
            &state_path,
        )
        .map_err(Error::from_reason)
    }

    /// Uninstall the integration for `provider` (parity with the Tauri `integration_remove`):
    /// the mirror of [`Self::integration_install`], driving the SHARED
    /// [`nyx_core::integrations::remove`] core. Returns the post-mutation status.
    fn integration_remove(
        &self,
        provider: &str,
    ) -> Result<nyx_core::integrations::IntegrationStatus> {
        let data_dir = std::path::PathBuf::from(&self.data_dir);
        let state_path = nyx_core::integrations::state_path_in(&data_dir);
        let target = nyx_core::onboarding::OnboardingTarget::claude_code().ok_or_else(|| {
            Error::from_reason(
                "Could not resolve Claude Code config path (no home dir)".to_string(),
            )
        })?;
        let (install, cli) = self.claude_plugin();
        nyx_core::integrations::remove(
            provider,
            &target,
            install.as_ref(),
            cli.as_deref(),
            &state_path,
        )
        .map_err(Error::from_reason)
    }
}

impl Task for DbCommandTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        // ONE allowlisted match on the contract command name. Each arm is a thin wrapper
        // over the SAME `nyx_core::db` CRUD the Tauri bridge command body calls, run here
        // on a libuv worker thread (off the Node loop). `()` → JSON `null`.
        match self.command.as_str() {
            // --- terminals -----------------------------------------------------
            "list_terminals" => self
                .db
                .with_conn(db::list_terminals)
                .map_err(db_err)
                .and_then(|rows| Self::json(&rows)),
            "create_terminal" => {
                let cwd = self.arg_str("cwd")?;
                let label = self.arg_opt_str("label");
                self.db
                    .with_conn(|c| db::create_terminal(c, &cwd, label.clone()))
                    .map_err(db_err)
                    .and_then(|row| Self::json(&row))
            }
            "close_terminal" => {
                let id = self.arg_str("id")?;
                self.db
                    .with_conn(|c| db::close_terminal(c, &id))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "reorder" => {
                let ids = self.arg_str_vec("ids")?;
                self.db
                    .with_conn(|c| db::reorder(c, &ids))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "rename" => {
                let id = self.arg_str("id")?;
                let label = self.arg_opt_str("label");
                self.db
                    .with_conn(|c| db::rename(c, &id, label.clone()))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "set_active" => {
                let id = self.arg_str("id")?;
                self.db
                    .with_conn(|c| db::set_active(c, &id))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "persist_scrollback" => {
                let id = self.arg_str("id")?;
                let serialized = self.arg_str("serialized")?;
                self.db
                    .with_conn(|c| db::persist_scrollback(c, &id, &serialized))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "terminal_exec_mark_read" => {
                let id = self.arg_str("id")?;
                self.db
                    .with_conn(|c| db::mark_exec_state_read(c, &id))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "attach_terminal" => {
                let terminal_id = self.arg_str("terminalId")?;
                let workspace_id = self.arg_str("workspaceId")?;
                let mode = self.arg_str("mode")?;
                self.db
                    .with_conn(|c| db::attach_terminal(c, &terminal_id, &workspace_id, &mode))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            // --- projects ------------------------------------------------------
            "list_projects" => self
                .db
                .with_conn(db::list_projects)
                .map_err(db_err)
                .and_then(|rows| Self::json(&rows)),
            "create_project" => {
                let name = self.arg_str("name")?;
                let root_path = self.arg_str("rootPath")?;
                let root_name = self.arg_opt_str("rootName");
                self.db
                    .with_conn(|c| db::create_project(c, &name, &root_path, root_name.as_deref()))
                    .map_err(db_err)
                    .and_then(|(project, root)| {
                        Self::json(&serde_json::json!({ "project": project, "root": root }))
                    })
            }
            "update_project" => {
                let id = self.arg_str("id")?;
                let name = self.arg_str("name")?;
                self.db
                    .with_conn(|c| db::update_project(c, &id, &name))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "delete_project" => {
                let id = self.arg_str("id")?;
                self.db
                    .with_conn(|c| db::delete_project(c, &id))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "set_project_collapsed" => {
                let id = self.arg_str("id")?;
                let collapsed = self.arg_bool("collapsed")?;
                self.db
                    .with_conn(|c| db::set_project_collapsed(c, &id, collapsed))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "set_project_resume_agent_sessions" => {
                let id = self.arg_str("id")?;
                let resume = self.arg_bool("resume")?;
                self.db
                    .with_conn(|c| db::set_project_resume_agent_sessions(c, &id, resume))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            // --- workspaces ----------------------------------------------------
            "list_workspaces" => {
                let project_id = self.arg_str("projectId")?;
                self.db
                    .with_conn(|c| db::list_workspaces(c, &project_id))
                    .map_err(db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            "create_workspace" => {
                let project_id = self.arg_str("projectId")?;
                let name = self.arg_str("name")?;
                let path = self.arg_str("path")?;
                self.db
                    .with_conn(|c| db::create_workspace(c, &project_id, &name, &path))
                    .map_err(db_err)
                    .and_then(|row| Self::json(&row))
            }
            "rename_workspace" => {
                let id = self.arg_str("id")?;
                let name = self.arg_str("name")?;
                self.db
                    .with_conn(|c| db::rename_workspace(c, &id, &name))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "set_workspace_collapsed" => {
                let id = self.arg_str("id")?;
                let collapsed = self.arg_bool("collapsed")?;
                self.db
                    .with_conn(|c| db::set_workspace_collapsed(c, &id, collapsed))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            // --- managed-command templates / instances (read + CRUD + source/import;
            //     the RUNTIME start/stop/relaunch/output/acknowledge is routed onto the
            //     live CommandManager by the host, not here. The TEMPLATE-mutating arms
            //     below mirror the Tauri `command_*` bodies exactly — same nyx-core CRUD,
            //     same source-detach rule, same running-guard — so the Electron core-host
            //     drives the full command surface end-to-end. After a successful mutation
            //     the host emits `commands://changed` from `core-command.ts`, parity with
            //     the Tauri `emit_commands_changed`.) ---------------------------------
            "command_list" => {
                let project_id = self.arg_str("projectId")?;
                self.db
                    .with_conn(|c| db::list_templates(c, &project_id))
                    .map_err(db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            "command_create" => {
                let project_id = self.arg_str("projectId")?;
                let name = self.arg_str("name")?;
                let command = self.arg_str("command")?;
                let subfolder = self.arg_opt_str("subfolder");
                let restart_on_startup = self.arg_opt_bool("restartOnStartup");
                // Optional package.json provenance (the import path supplies these; a
                // manual create leaves them null and lets `infer_command_source` fill in
                // a manager when the line is itself a PM invocation).
                let source_kind = self.arg_opt_str("sourceKind");
                let source_package_json_path = self.arg_opt_str("sourcePackageJsonPath");
                let source_script_name = self.arg_opt_str("sourceScriptName");
                let source_script_command_snapshot =
                    self.arg_opt_str("sourceScriptCommandSnapshot");
                let package_manager = self.arg_opt_str("packageManager");
                let (source_kind, package_manager) =
                    nyx_core::pkgjson::infer_command_source(&command, source_kind, package_manager);
                let source = db::CommandSource {
                    source_kind,
                    source_package_json_path,
                    source_script_name,
                    source_script_command_snapshot,
                    package_manager,
                };
                self.db
                    .with_conn(|c| {
                        let created = db::create_template(
                            c,
                            &project_id,
                            &name,
                            &command,
                            subfolder.as_deref(),
                            source.clone(),
                        )?;
                        if restart_on_startup == Some(true) {
                            db::set_restart_on_startup(c, &created.id, true)?;
                        }
                        db::get_template(c, &created.id).map(|t| t.unwrap_or(created))
                    })
                    .map_err(db_err)
                    .and_then(|row| Self::json(&row))
            }
            "command_update" => {
                let id = self.arg_str("id")?;
                let name = self.arg_str("name")?;
                let command = self.arg_str("command")?;
                let subfolder = self.arg_opt_str("subfolder");
                let restart_on_startup = self.arg_opt_bool("restartOnStartup");
                // Refuse the edit while any instance of this template is running.
                self.guard_template_not_running(&id)?;
                self.db
                    .with_conn(|c| {
                        // Detach a package.json source only when the new command drifts
                        // from BOTH the runner call and the raw script snapshot.
                        let detach = match db::get_template(c, &id)? {
                            Some(t) if t.source_script_name.is_some() => {
                                nyx_core::pkgjson::command_detaches_source(&t, &command)
                            }
                            _ => false,
                        };
                        db::update_template(c, &id, &name, &command, subfolder.as_deref())?;
                        if detach {
                            db::set_template_source(c, &id, db::CommandSource::default())?;
                        }
                        if let Some(flag) = restart_on_startup {
                            db::set_restart_on_startup(c, &id, flag)?;
                        }
                        Ok::<_, diesel::result::Error>(())
                    })
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            "command_delete" => {
                let id = self.arg_str("id")?;
                self.guard_template_not_running(&id)?;
                self.db
                    .with_conn(|c| db::delete_template(c, &id))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            // --- managed-command package.json source actions -------------------
            "command_source_refresh" => {
                let id = self.arg_str("id")?;
                self.db
                    .with_conn(|c| nyx_core::pkgjson::source_refresh(c, &id))
                    .map_err(Error::from_reason)
                    .and_then(|r| Self::json(&r))
            }
            "command_resync_source" => {
                let id = self.arg_str("id")?;
                // Resync rewrites `command`, so it is also refused while running.
                self.guard_template_not_running(&id)?;
                self.db
                    .with_conn(|c| nyx_core::pkgjson::source_resync(c, &id))
                    .map_err(Error::from_reason)
                    .and_then(|body| Self::json(&body))
            }
            "command_unlink_source" => {
                let id = self.arg_str("id")?;
                self.db
                    .with_conn(|c| db::set_template_source(c, &id, db::CommandSource::default()))
                    .map_err(db_err)?;
                Ok("null".to_string())
            }
            // --- managed-command package.json import (discover + create) -------
            "command_import_scripts" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let path = self
                    .db
                    .with_conn(|c| db::workspace_path(c, &workspace_id))
                    .map_err(db_err)?
                    .ok_or_else(|| {
                        Error::from_reason(format!("unknown workspace {workspace_id}"))
                    })?;
                Self::json(&nyx_core::pkgjson::discover_package_scripts(&path))
            }
            "command_import_create" => {
                let project_id = self.arg_str("projectId")?;
                let name = self.arg_str("name")?;
                let command = self.arg_str("command")?;
                let subfolder = self.arg_str("subfolder")?;
                let source_package_json_path = self.arg_str("sourcePackageJsonPath")?;
                let source_script_name = self.arg_str("sourceScriptName")?;
                let source_script_command_snapshot = self.arg_str("sourceScriptCommandSnapshot")?;
                let package_manager = self.arg_str("packageManager")?;
                let source = db::CommandSource {
                    source_kind: Some(db::SOURCE_KIND_PACKAGE_JSON.to_string()),
                    source_package_json_path: Some(source_package_json_path),
                    source_script_name: Some(source_script_name),
                    source_script_command_snapshot: Some(source_script_command_snapshot),
                    package_manager: Some(package_manager),
                };
                self.db
                    .with_conn(|c| {
                        nyx_core::pkgjson::import_command(
                            c,
                            &project_id,
                            &name,
                            &command,
                            &subfolder,
                            source.clone(),
                        )
                    })
                    .map_err(Error::from_reason)
                    .and_then(|row| Self::json(&row))
            }
            "command_instance_list" => {
                let workspace_id = self.arg_str("workspaceId")?;
                self.db
                    .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
                    .map_err(db_err)
                    .and_then(|mut rows| {
                        // Fill each row's resolved run dir for display (same best-effort
                        // join the Tauri `command_instance_list` applies). Infallible.
                        for row in &mut rows {
                            row.cwd = Some(nyx_core::subfolder::resolve_run_dir_lossy(
                                &row.workspace_path,
                                row.subfolder.as_deref(),
                            ));
                        }
                        Self::json(&rows)
                    })
            }
            // --- agents --------------------------------------------------------
            "agent_active_sessions" => self
                .db
                .with_conn(db::active_agent_sessions)
                .map_err(db_err)
                .and_then(|rows| Self::json(&rows)),
            // The agent-session CLOSE WARNINGS (PRD-5 #6): the live (`active`/`unknown`)
            // sessions whose project does NOT auto-resume — the ones a window-close would
            // drop without nyx bringing them back. A PURE DB read (same nature as
            // `agent_active_sessions`), NOT a live-runtime scan: gather the candidates, then
            // apply the single warn/no-warn policy point `should_warn_on_close` per row and
            // map each survivor to `{ terminal_id, agent_kind, message }` via
            // `close_warning_message`. An EMPTY list ⇒ "close freely". Mirrors the Tauri
            // `agent_close_warnings` body byte-for-byte so the Electron CloseWarningDialog
            // fires at parity. (Was wrongly stubbed `[]` in the Electron main fallback.)
            "agent_close_warnings" => {
                use nyx_core::agent_resume::{
                    close_warning_message, should_warn_on_close, SessionState,
                };
                /// The close-warning entry serialized to the contract's `CloseWarning[]`
                /// shape (`{ terminal_id, agent_kind, message }`) the front's
                /// `close-warning.ts` expects — identical to the Tauri `CloseWarningEntry`.
                #[derive(serde::Serialize)]
                struct CloseWarningEntry {
                    terminal_id: String,
                    agent_kind: String,
                    message: String,
                }
                self.db
                    .with_conn(db::close_warning_candidates)
                    .map_err(db_err)
                    .and_then(|rows| {
                        let entries: Vec<CloseWarningEntry> = rows
                            .into_iter()
                            .filter(|w| {
                                // The single warn/no-warn policy point: a resume-ON project
                                // never warns; only a live (active/unknown) session in a
                                // non-resuming project does. An unrecognized state string is
                                // treated as not-warnable (defensive).
                                SessionState::from_db(&w.session_state).is_some_and(|s| {
                                    should_warn_on_close(s, w.project_resume_on)
                                })
                            })
                            .map(|w| CloseWarningEntry {
                                message: close_warning_message(
                                    &w.agent_kind,
                                    w.terminal_label.as_deref(),
                                    &w.terminal_id,
                                    w.workspace_name.as_deref(),
                                ),
                                terminal_id: w.terminal_id,
                                agent_kind: w.agent_kind,
                            })
                            .collect();
                        Self::json(&entries)
                    })
            }
            // --- integrations (Settings → Integrations: the ONE bundled Claude plugin =
            //     MCP + session-capture hooks; PRD-5 review #58). Routed onto the SHARED
            //     nyx_core::integrations cores so the Electron Install/Uninstall button
            //     drives the EXACT same `claude` plugin install/uninstall the Tauri command
            //     body drives — at parity, end-to-end. install/remove SHELL OUT to `claude`,
            //     which the host already runs off the Node loop (this is a libuv worker). --
            "integration_list" => Self::json(&nyx_core::integrations::status_list()),
            "integration_install" => {
                let provider = self.arg_str("provider")?;
                self.integration_install(&provider)
                    .and_then(|s| Self::json(&s))
            }
            "integration_remove" => {
                let provider = self.arg_str("provider")?;
                self.integration_remove(&provider)
                    .and_then(|s| Self::json(&s))
            }
            // A command the front DOES invoke but whose backend is not (yet) DB-only
            // over this transport — return a readable, structural error so the host
            // surfaces it as a `command` BridgeError rather than a silent wrong answer.
            other => Err(Error::from_reason(format!(
                "db command '{other}' is not available over the core-host transport"
            ))),
        }
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// A DELIBERATELY SLOW DB query (test/proof only) used to demonstrate the
/// non-blocking guarantee: it checks a connection out of the SAME pool and holds it
/// for `delay_ms` (a real DB-bound stall) on a libuv WORKER thread. While it is in
/// flight the Node loop keeps servicing IPC + PTY output, and OTHER pooled reads still
/// progress (a different pool connection). This is the observable evidence for the
/// done-criterion "une requete Diesel lente ne bloque ni les messages du core-host ni
/// la sortie PTY".
pub struct SlowQueryTask {
    db: Arc<Db>,
    delay_ms: u32,
}

impl Task for SlowQueryTask {
    type Output = i64;
    type JsValue = i64;

    fn compute(&mut self) -> Result<Self::Output> {
        // Hold a real pooled connection for the whole stall (so the stall is genuinely
        // DB-bound: this connection is unavailable to others, but the pool's OTHER
        // connections still serve concurrent reads), then run a trivial query so the
        // round-trip is a real Diesel call, not just a sleep.
        self.db.with_conn(|c| {
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms as u64));
            let n = db::list_terminals(c).map_err(db_err)?.len() as i64;
            Ok(n)
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// The napi handle the core-host owns over shared nyx-core state. Created once at boot
/// (`NyxCore::open(dataDir)`); every DB method returns an `AsyncTask` (a `Promise` in
/// JS) so the query runs on a libuv worker, never the Node loop.
#[napi]
pub struct NyxCore {
    /// The SHARED r2d2 pool. `Arc` so the MCP server (started from here) borrows the
    /// SAME pool — one connection authority for napi + MCP (no global `Mutex` conn).
    pub(crate) db: Arc<Db>,
    /// The MCP server, started lazily by `mcpStart`. `None` until then.
    pub(crate) mcp: std::sync::Mutex<Option<Arc<nyx_core::mcp::McpServer>>>,
    /// The managed-command runner the host built via `createCommandRunner` (over Node
    /// callbacks + this SAME pool). `None` until built; once present the MCP runtime
    /// command tools (`start_command`/`stop_command`/…) route onto it (no more
    /// `mcp_unavailable`). One runner shared by the host's lifecycle calls + the MCP
    /// dispatcher (no second runtime). `Arc` so the installed dispatcher reads the SAME
    /// cell — the runner can be built before OR after `mcpStart`.
    pub(crate) runner: Arc<std::sync::Mutex<Option<Arc<CommandRunner<NodeRunnerSink>>>>>,
    /// The data dir this core was opened under (`NyxCore::open(data_dir)`). The
    /// `integration_*` dispatcher arms resolve `integrations.json` + the stable Claude
    /// plugin install dir under it (parity with the Tauri `resolve_data_dir`).
    pub(crate) data_dir: String,
    /// The host's read-only bundled-resources dir (the bundled Claude plugin SOURCE in a
    /// packaged build). Captured from the host's `mcpReconcile(dataDir, resourceDir)` call
    /// (the same resource base the Tauri shell resolves); `None` until then / in dev, where
    /// the integrations core falls back to the dev source tree.
    pub(crate) resource_dir: std::sync::Mutex<Option<String>>,
    /// The SYNCHRONOUS record↔live-PTY liveness registry — the Electron mirror of the Tauri
    /// `TerminalPtyMap`. Maps every terminal RECORD id whose shell currently has a live PTY
    /// to its live `pty_id`. Fed by `register_terminal_pty` (the front publishes the join on
    /// PTY mount) and cleared on PTY exit; read SYNCHRONOUSLY by the MCP dispatcher's
    /// `NodeTerminalHost` (the Rust MCP thread) so `send_to_terminal` returns `invalid_state`
    /// (Tauri parity) when no PTY is live, instead of a mendacious `sent: true`, and so
    /// `list_terminals` reports the true `live` bit. `Arc` so the installed MCP dispatcher
    /// shares the SAME map (built before OR after `mcpStart`).
    pub(crate) live_terminals: Arc<std::sync::Mutex<std::collections::HashMap<String, u32>>>,
}

#[napi]
impl NyxCore {
    /// Open (creating if absent) the SQLite DB under `data_dir` and build the shared
    /// r2d2 pool (WAL + foreign_keys + busy_timeout per connection). Migrations run
    /// once here. A failure (bad path / migration error) throws a readable error the
    /// host turns into a fatal boot state — never a silent hang.
    ///
    /// `data_dir` must already exist (main creates `userData`); the DB file is
    /// `data_dir/nyx.db`, the SAME name + location convention as the Tauri shell.
    #[napi(constructor)]
    pub fn open(data_dir: String) -> Result<Self> {
        let db_path = std::path::Path::new(&data_dir).join("nyx.db");
        let db =
            Db::open(&db_path).map_err(|e| Error::from_reason(format!("db open failed: {e}")))?;
        Ok(NyxCore {
            db: Arc::new(db),
            mcp: std::sync::Mutex::new(None),
            runner: Arc::new(std::sync::Mutex::new(None)),
            data_dir,
            resource_dir: std::sync::Mutex::new(None),
            live_terminals: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        })
    }

    /// Build the managed-command runner over Node event callbacks + the SHARED pool,
    /// stash its `Arc<CommandRunner>` so the MCP dispatcher can route the runtime
    /// command tools onto it, and return the [`NyxCommandRunner`] handle the host owns.
    ///
    /// Idempotent-ish: a second call rebuilds the runner (the host builds it exactly
    /// once at boot). The two callbacks are the `command://state`+`ack`+`output-cleared`
    /// sink and the `command://output` sink — delivered on the Node loop, mapped by the
    /// host to the Tauri event names so the renderer's command band is at parity.
    #[napi]
    pub fn create_command_runner(
        &self,
        #[napi(ts_arg_type = "(err: null | Error, ev: CommandStateEvent) => void")]
        on_state: JsFunction,
        #[napi(ts_arg_type = "(err: null | Error, ev: CommandOutputEvent) => void")]
        on_output: JsFunction,
    ) -> Result<NyxCommandRunner> {
        let state_tsfn: ThreadsafeFunction<CommandStateEvent, ErrorStrategy::Fatal> =
            on_state.create_threadsafe_function(0, |ctx| Ok(vec![ctx.value]))?;
        let output_tsfn: ThreadsafeFunction<CommandOutputEvent, ErrorStrategy::Fatal> =
            on_output.create_threadsafe_function(0, |ctx| Ok(vec![ctx.value]))?;
        let handle = NyxCommandRunner::build(Arc::clone(&self.db), state_tsfn, output_tsfn);
        // Stash the shared runner so the MCP dispatcher routes runtime tools onto it.
        *self.runner.lock().unwrap() = Some(handle.runner());
        Ok(handle)
    }

    /// Publish the record↔live-PTY join into the SYNCHRONOUS liveness registry (the
    /// `register_terminal_pty` command — the Electron mirror of the Tauri `TerminalPtyMap`
    /// set). The front calls this once its `<Terminal>` has spawned the PTY for `record_id`,
    /// binding the durable record id to the live `pty_id`. After this, the MCP terminal
    /// tools resolve the record to a LIVE shell synchronously: `send_to_terminal` writes
    /// (instead of `invalid_state`), and `list_terminals` reports `live: true`. Idempotent —
    /// re-registering the same record overwrites with the latest pty id.
    #[napi]
    pub fn register_terminal_pty(&self, record_id: String, pty_id: u32) {
        self.live_terminals
            .lock()
            .unwrap()
            .insert(record_id, pty_id);
    }

    /// Retract the record↔live-PTY join (the `register_terminal_pty` command with a null
    /// pty, and the PTY-exit cleanup). The record's shell is no longer live, so the MCP
    /// terminal tools fall back to `invalid_state` for `send_to_terminal` and `live: false`
    /// for `list_terminals` — exactly the Tauri `TerminalPtyMap::clear` behaviour. Idempotent.
    #[napi]
    pub fn unregister_terminal_pty(&self, record_id: String) {
        self.live_terminals.lock().unwrap().remove(&record_id);
    }

    /// Auto-attach a terminal RECORD to a workspace by its live `cwd`, off the Node loop
    /// (the `auto_attach_terminal` command). Runs the SHARED nyx-core resolver body at
    /// Tauri parity; resolves to `{ workspaceId, changed }`.
    #[napi(ts_return_type = "Promise<AutoAttachResultJs>")]
    pub fn auto_attach_terminal(
        &self,
        terminal_id: String,
        cwd: Option<String>,
    ) -> AsyncTask<AutoAttachTask> {
        AsyncTask::new(AutoAttachTask {
            db: Arc::clone(&self.db),
            terminal_id,
            cwd,
        })
    }

    /// Persist a terminal exec-state transition (OSC 133 result annotation) off the
    /// Node loop. Resolves to `{ updated, updatedAt }` so the host emits
    /// `terminal://exec-state` with the exact persisted timestamp, and skips the emit
    /// when `updated` is false (unknown id). Mirrors `persist_and_emit_exec_state`.
    #[napi(ts_return_type = "Promise<ExecStatePersist>")]
    pub fn set_exec_state(
        &self,
        terminal_id: String,
        state: String,
        exit_code: Option<i32>,
        unread: bool,
    ) -> AsyncTask<SetExecStateTask> {
        AsyncTask::new(SetExecStateTask {
            db: Arc::clone(&self.db),
            terminal_id,
            state,
            exit_code,
            unread,
        })
    }

    /// Read a terminal record (so the host can settle a stale `running` to idle on PTY
    /// exit). Resolves to the row or `null`.
    #[napi(ts_return_type = "Promise<TerminalRow | null>")]
    pub fn get_terminal(&self, terminal_id: String) -> AsyncTask<GetTerminalTask> {
        AsyncTask::new(GetTerminalTask {
            db: Arc::clone(&self.db),
            terminal_id,
        })
    }

    /// List every terminal in sidebar order (the boot restore read). Resolves to the
    /// rows.
    #[napi(ts_return_type = "Promise<Array<TerminalRow>>")]
    pub fn list_terminals(&self) -> AsyncTask<ListTerminalsTask> {
        AsyncTask::new(ListTerminalsTask {
            db: Arc::clone(&self.db),
        })
    }

    /// Create a new alive terminal record (the host persists it so restore can re-open
    /// it). Resolves to the created row.
    #[napi(ts_return_type = "Promise<TerminalRow>")]
    pub fn create_terminal(
        &self,
        cwd: String,
        label: Option<String>,
    ) -> AsyncTask<CreateTerminalTask> {
        AsyncTask::new(CreateTerminalTask {
            db: Arc::clone(&self.db),
            cwd,
            label,
        })
    }

    /// Settle every terminal stuck at a persisted `running` down to idle (boot
    /// normalization). Resolves to the count normalized.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn normalize_phantom_terminals(&self) -> AsyncTask<NormalizeTerminalsTask> {
        AsyncTask::new(NormalizeTerminalsTask {
            db: Arc::clone(&self.db),
        })
    }

    /// BOOT agent-session RESUME scan (PRD-5 task #5): sweep stale sessions, gather
    /// resumable candidates, and return the `claude --resume` parks to inject at each
    /// eligible terminal's first respawn. Resolves to the parks.
    #[napi(ts_return_type = "Promise<Array<ResumePark>>")]
    pub fn resume_scan_on_boot(&self) -> AsyncTask<ResumeScanTask> {
        AsyncTask::new(ResumeScanTask {
            db: Arc::clone(&self.db),
        })
    }

    /// Mark an agent session `resume_failed` (PRD-5 task #5) when a parked resume could
    /// not be injected (PTY gone / write failed), so the next launch won't retry.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn mark_resume_failed(&self, session_id: String) -> AsyncTask<MarkResumeFailedTask> {
        AsyncTask::new(MarkResumeFailedTask {
            db: Arc::clone(&self.db),
            session_id,
        })
    }

    /// TEST/PROOF ONLY: run a deliberately slow DB query (`delay_ms`) on a libuv worker
    /// to demonstrate it does NOT block the Node loop or PTY output. Resolves to the
    /// terminal count once the stall elapses.
    #[napi(ts_return_type = "Promise<number>")]
    pub fn db_slow_query(&self, delay_ms: u32) -> AsyncTask<SlowQueryTask> {
        AsyncTask::new(SlowQueryTask {
            db: Arc::clone(&self.db),
            delay_ms,
        })
    }

    /// The GENERIC, allowlisted DB-command dispatcher (PRD-5 review — full nyxBridge
    /// surface end-to-end). The Electron core-host forwards the contract's
    /// `BackendCommand` NAME + a JSON args STRING here; the task dispatches to the SAME
    /// `nyx_core::db` CRUD the Tauri bridge calls and resolves a JSON RESULT STRING (the
    /// host `JSON.parse`s it back to the contract shape). Runs on a libuv worker (off the
    /// Node loop), so even a slow query never blocks IPC or PTY output — the SAME
    /// non-blocking guarantee as every other DB task. An unknown command, or one not
    /// available over this transport, rejects with a readable error.
    ///
    /// `args_json` MUST be a JSON object (`"{}"` for the no-arg commands); a malformed
    /// blob is a readable parse error, never a panic.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn db_command(
        &self,
        command: String,
        args_json: String,
    ) -> Result<AsyncTask<DbCommandTask>> {
        let args: serde_json::Value = serde_json::from_str(&args_json)
            .map_err(|e| Error::from_reason(format!("{command}: bad args json: {e}")))?;
        if !args.is_object() {
            return Err(Error::from_reason(format!(
                "{command}: args must be a JSON object, got {args_json}"
            )));
        }
        Ok(AsyncTask::new(DbCommandTask {
            db: Arc::clone(&self.db),
            command,
            args,
            data_dir: self.data_dir.clone(),
            resource_dir: self.resource_dir.lock().unwrap().clone(),
            runner: Arc::clone(&self.runner),
        }))
    }
}

/// A parked agent-session resume (PRD-5 task #5). The host stashes these keyed by
/// `terminalId` at boot and INJECTS `command` (`claude --resume <id>`, CR-terminated) when that
/// terminal's PTY next spawns — the napi mirror of the Tauri `PendingResumes` +
/// `register_terminal_pty` injection.
#[napi(object)]
pub struct ResumePark {
    /// The persistent terminal record id whose first respawn should inject the resume.
    pub terminal_id: String,
    /// The exact line to inject (e.g. `claude --resume <external_session_id>`).
    pub command: String,
    /// The `agent_sessions.id` — so a failed injection can mark it `resume_failed`.
    pub session_id: String,
    /// Whether the resume is UNCERTAIN (best-effort; surfaced for parity/diagnostics).
    pub uncertain: bool,
}

/// BOOT AGENT-SESSION RESUME SCAN (PRD-5 task #5) — the napi mirror of the Tauri
/// `restore_agent_sessions_on_boot`. In order: (1) sweep stale `active` sessions to
/// `unknown`; (2) gather resume candidates (alive terminal + active/unknown session +
/// project opt-in); (3) run the PURE `decide_resume` per candidate with the resolved
/// shell target + the agent adapter, and collect a [`ResumePark`] for each that says
/// RESUME. The host parks these and injects at the terminal's first respawn.
pub struct ResumeScanTask {
    db: Arc<Db>,
}

impl Task for ResumeScanTask {
    type Output = Vec<ResumePark>;
    type JsValue = Vec<ResumePark>;

    fn compute(&mut self) -> Result<Self::Output> {
        use nyx_core::agent::AgentRegistry;
        use nyx_core::agent_resume::{
            decide_resume, ResumeDecision, ResumeInputs, ResumeTarget, SessionState,
        };

        // 1. Sweep stale active → unknown (probable kills since the last clean run).
        let _ = self
            .db
            .with_conn(|c| db::sweep_stale_active_sessions(c, db::SESSION_STALE_AFTER_MS));

        // 2. Gather candidates.
        let candidates = self
            .db
            .with_conn(db::resume_candidates_on_boot)
            .map_err(db_err)?;

        // The execution target is fixed per run by the resolved default shell.
        let target = ResumeTarget::classify_shell(&nyx_core::pty::resolve_shell());
        let registry = AgentRegistry::default();

        let mut parks = Vec::new();
        // BOOT CLEANUP (finding #82): every candidate we DON'T resume is provably dead —
        // claude was not relaunched after the restart, so its `active`/`unknown` row is a
        // phantom that would warn on every subsequent close. Collect those session ids and
        // mark them `ended` below. A Resume decision leaves the row `active` (the park
        // revives it at injection), so it is NOT collected here.
        let mut dead_session_ids: Vec<String> = Vec::new();
        for cand in candidates {
            let Some(state) = SessionState::from_db(&cand.session_state) else {
                continue;
            };
            let Some(adapter) = registry.get(&cand.agent_kind) else {
                continue;
            };
            // A candidate is only resumable if its conversation EXISTS on disk (a single
            // `stat` on the captured transcript path; finding #53).
            let transcript_exists = cand
                .transcript_path
                .as_deref()
                .map(|p| std::path::Path::new(p).exists())
                .unwrap_or(false);
            let inputs = ResumeInputs {
                project_resume_on: cand.project_resume_on,
                // A candidate is an ALIVE terminal by construction; a voluntary close
                // flips it to `closed` (excluded by the candidate query).
                closed_voluntarily: false,
                session_state: state,
                external_session_id: &cand.external_session_id,
                transcript_exists,
                target,
            };
            match decide_resume(&inputs, adapter) {
                ResumeDecision::Resume {
                    command,
                    resume_uncertain,
                } => {
                    parks.push(ResumePark {
                        terminal_id: cand.terminal_id,
                        command,
                        session_id: cand.session_id,
                        uncertain: resume_uncertain,
                    });
                }
                // Skip(_) for ANY reason (resume OFF / not a candidate / no conversation /
                // unsupported target / no command): the session will NOT be brought back,
                // so it is dead — retire it so it stops warning.
                ResumeDecision::Skip(_) => dead_session_ids.push(cand.session_id),
            }
        }

        // Retire the non-resumed sessions in one batch (best-effort: a cleanup failure must
        // not abort the boot scan — the parks still matter most). After this, a fresh boot
        // leaves only sessions started in the CURRENT run live, so the close-warning fires
        // only for genuinely live sessions (no more phantom warnings in a loop).
        if !dead_session_ids.is_empty() {
            let _ = self
                .db
                .with_conn(|c| db::mark_sessions_ended(c, &dead_session_ids));
        }

        Ok(parks)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// Mark an agent session `resume_failed` (PRD-5 task #5) — the host calls this when a
/// parked resume could NOT be injected (the PTY was gone / the write failed), so the
/// next launch will not retry it. Mirrors the Tauri `mark_session_resume_failed`.
pub struct MarkResumeFailedTask {
    db: Arc<Db>,
    session_id: String,
}

impl Task for MarkResumeFailedTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        self.db
            .with_conn(|c| db::mark_session_resume_failed(c, &self.session_id))
            .map(|_| ())
            .map_err(db_err)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

/// Map a Diesel error into a napi error (a readable string the host surfaces).
fn db_err(e: diesel::result::Error) -> Error {
    Error::from_reason(format!("db error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_core::db::{self, SessionCapture};

    /// A unique, file-backed `Db` in the OS temp dir (the pool's pragmas need a real
    /// journal; `:memory:` has no shared file across pool checkouts). Uniqueness comes from
    /// the pid + a nanosecond timestamp (no `uuid`/`tempfile` dev-dep added for one test).
    fn temp_db() -> Db {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "nyx-napi-closewarn-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp db dir");
        Db::open(&dir.join("nyx.db")).expect("open file-backed db")
    }

    /// Build a `DbCommandTask` for `command` with `args` against `db` (no runner, no
    /// integrations dirs — the close-warning arm is a pure DB read).
    fn task(db: Arc<Db>, command: &str, args: serde_json::Value) -> DbCommandTask {
        DbCommandTask {
            db,
            command: command.to_string(),
            args,
            data_dir: String::new(),
            resource_dir: None,
            runner: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// PROOF for the review finding: the `agent_close_warnings` arm of `DbCommandTask`
    /// (the EXACT code path the Electron core-host dispatches now that the `[]` stub is
    /// gone) returns the REAL candidates — NOT `[]` — when a live agent session belongs to
    /// a NON-resuming project, and the JSON message names the agent + terminal. Turning the
    /// project's resume ON makes the SAME session stop warning (parity with the Tauri
    /// `agent_close_warnings_warns_only_when_resume_off` test). This is the regression that
    /// killed the close-warning dialog on Electron.
    #[test]
    fn agent_close_warnings_arm_returns_real_candidates_not_empty() {
        let db = Arc::new(temp_db());

        // A project (resume OFF by default) + a terminal attached to its root workspace,
        // hosting a LIVE Claude session — the exact shape a window-close would drop.
        let (project, root) = db
            .with_conn(|c| db::create_project(c, "demo", "/home/kris/demo", None))
            .expect("create_project");
        let term = db
            .with_conn(|c| db::create_terminal(c, "/home/kris/demo", Some("build".into())))
            .expect("create_terminal");
        db.with_conn(|c| db::attach_terminal(c, &term.id, &root.id, db::BINDING_AUTO))
            .expect("attach_terminal");
        db.with_conn(|c| {
            db::record_session_start(
                c,
                &term.id,
                db::AGENT_KIND_CLAUDE_CODE,
                SessionCapture {
                    workspace_id: Some(root.id.clone()),
                    external_session_id: "sid-1".into(),
                    cwd: "/home/kris/demo".into(),
                    transcript_path: None,
                    metadata_json: None,
                },
            )
        })
        .expect("record_session_start");

        // Resume OFF → the arm returns the live session as a warning (NOT `[]`).
        let json = task(db.clone(), "agent_close_warnings", serde_json::json!({}))
            .compute()
            .expect("agent_close_warnings arm");
        let warnings: serde_json::Value = serde_json::from_str(&json).expect("parse warnings json");
        let arr = warnings.as_array().expect("warnings is a JSON array");
        // The whole point of the finding: NON-empty when there is a live, droppable session.
        assert_eq!(arr.len(), 1, "expected one real warning, got: {json}");
        assert_eq!(arr[0]["terminal_id"], serde_json::json!(term.id));
        assert_eq!(
            arr[0]["agent_kind"],
            serde_json::json!(db::AGENT_KIND_CLAUDE_CODE)
        );
        let message = arr[0]["message"].as_str().expect("message is a string");
        assert!(
            message.contains("Claude Code") && message.contains("build"),
            "message names the agent + terminal: {message}"
        );

        // Resume ON → the SAME session no longer warns (nyx will bring it back): back to `[]`.
        db.with_conn(|c| db::set_project_resume_agent_sessions(c, &project.id, true))
            .expect("set_project_resume_agent_sessions");
        let json_on = task(db.clone(), "agent_close_warnings", serde_json::json!({}))
            .compute()
            .expect("agent_close_warnings arm (resume on)");
        let warnings_on: serde_json::Value =
            serde_json::from_str(&json_on).expect("parse warnings json (on)");
        assert!(
            warnings_on.as_array().expect("array").is_empty(),
            "resume-ON project suppresses the warning, got: {json_on}"
        );

        // Raw evidence for the audit trail (captured by `cargo test -- --nocapture`).
        println!("[proof] agent_close_warnings (resume OFF) => {json}");
        println!("[proof] agent_close_warnings (resume ON)  => {json_on}");
    }
}
