//! The GENERIC non-PTY backend-command dispatcher (G0) — the sidecar port of the v1
//! `nyx-napi` `DbCommandTask` the TS core-host owned.
//!
//! ## What this is
//!
//! The supervisor forwards every contract `BackendCommand` that has no dedicated typed
//! pipe kind (`pty_*`, `command_start/stop/relaunch`, `mcp_start`) as one
//! [`Request::DbCommand`](crate::pipe::wire::Request::DbCommand): a command NAME + its
//! args serialized to JSON. [`dispatch`] routes each to the right AUTHORITY the sidecar
//! owns and returns the result ALREADY serialized to a JSON string (`"null"` for the void
//! commands), which the supervisor `JSON.parse`s back to the contract shape — exactly the
//! wire shape the supervisor already spoke to the v1 core-host, so no renderer call-site
//! changes; only the transport underneath does.
//!
//! ## Authorities
//!
//!   * DB-backed CRUD (terminals / projects / workspaces / templates / instances / agent
//!     sessions) -> the SAME `nyx_core::db` functions the v1 dispatcher called, on the
//!     shared pool.
//!   * the managed-command RUNTIME's output/acknowledge -> the live runner.
//!   * the live-PTY introspection (`terminal_info`) -> the [`PtyManager`].
//!   * the runtime agent-activity store (`agent_activity_snapshot` / `agent_mark_ready_read`)
//!     -> the SHARED [`AgentActivityStore`] the MCP per-turn hooks write.
//!   * the auto-attach resolver (`auto_attach_terminal`) -> the shell-agnostic
//!     `nyx_core::resolve::auto_attach_terminal`.
//!   * the integrations install/remove (`integration_*`) -> the SHARED
//!     `nyx_core::integrations` cores against Claude's real config.
//!
//! After a successful template/tree MUTATION the dispatcher emits the matching
//! `commands://changed` / `workspaces://changed` invalidation (parity with the v1
//! core-command router) so every sidebar/command surface re-pulls — including one that did
//! not refresh locally.

use nyx_core::db;
use serde_json::Value;

use super::events::EventSink;
use super::Runtime;
use crate::pipe::wire::Event;

/// Route a `db_command` to its authority and return the result ALREADY serialized to a
/// JSON string (`"null"` for the void commands). An unknown command, a missing/ill-typed
/// arg, or a backend failure is an `Err(message)` the caller maps to a structured
/// `Response::Error` (a readable `command` BridgeError) — never a hang.
///
/// `args_json` MUST be a JSON object (`"{}"` for the no-arg commands); a malformed blob is
/// a readable parse error, never a panic.
pub fn dispatch(
    runtime: &Runtime,
    command: &str,
    args_json: &str,
    events: &EventSink,
) -> Result<String, String> {
    let args: Value =
        serde_json::from_str(args_json).map_err(|e| format!("{command}: bad args json: {e}"))?;
    if !args.is_object() {
        return Err(format!(
            "{command}: args must be a JSON object, got {args_json}"
        ));
    }
    // IDEMPOTENCY (Phase 4 — crash recovery): a mutating op may carry an `idempotencyKey`
    // so a retry after a crash is applied AT MOST ONCE. If the key was already applied, the
    // mutation is a no-op success (`"null"`) instead of double-applying (e.g. a duplicate
    // `delete_project` that would otherwise error "unknown", or a duplicate `create_*` that
    // would otherwise insert twice). The key is recorded only AFTER a successful apply below,
    // so a mutation that FAILED is retriable. A read or a key-less mutation skips the guard.
    let idempotency_key = args
        .get("idempotencyKey")
        .and_then(|v| v.as_str())
        .filter(|k| !k.is_empty())
        .map(|k| k.to_string());
    let guarded = idempotency_key.is_some()
        && (mutates_command_templates(command) || mutates_workspace_tree(command));
    if let (true, Some(key)) = (guarded, idempotency_key.as_deref()) {
        if runtime.idempotency_seen(key) {
            // Already applied on a prior attempt — return a no-op success WITHOUT re-running
            // the mutation. The renderer's invalidation already fired the first time.
            return Ok("null".to_string());
        }
    }

    let ctx = Ctx {
        runtime,
        command,
        args,
    };
    let result = ctx.run()?;

    // The mutation applied successfully: NOW record its idempotency key so a retry after a
    // crash is recognised as a duplicate. (A failed `ctx.run()?` short-circuited above with
    // the key unrecorded, keeping the op retriable.)
    if guarded {
        if let Some(key) = idempotency_key.as_deref() {
            runtime.idempotency_record(key);
        }
    }

    // Parity with the v1 `emit_commands_changed` / `emit_workspaces_changed`: a TEMPLATE
    // mutation broadcasts `commands://changed`; a project/workspace TREE mutation
    // broadcasts `workspaces://changed`, so every surface re-pulls. Emitted ONLY after a
    // successful mutation (an `Err` above skips it).
    if mutates_command_templates(command) {
        events.emit(Event::Changed {
            topic: "commands".to_string(),
        });
    } else if mutates_workspace_tree(command) {
        events.emit(Event::Changed {
            topic: "workspaces".to_string(),
        });
    } else if command == "agent_mark_ready_read" {
        // Nudge the renderer to re-pull the activity map so the cleared green dot
        // disappears (and stays gone after a re-deselect) — parity with the v1 router's
        // `events.changed("agent-sessions")`.
        events.emit(Event::Changed {
            topic: "agent-sessions".to_string(),
        });
    }
    Ok(result)
}

/// The per-call dispatch context: the runtime handle + the command name + its parsed args.
struct Ctx<'a> {
    runtime: &'a Runtime,
    command: &'a str,
    args: Value,
}

impl Ctx<'_> {
    /// Read a required string arg, or a readable error.
    fn arg_str(&self, name: &str) -> Result<String, String> {
        self.args
            .get(name)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("{}: missing string arg '{name}'", self.command))
    }

    /// Read an optional string arg (`null`/absent -> `None`).
    fn arg_opt_str(&self, name: &str) -> Option<String> {
        self.args
            .get(name)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Read a required bool arg, or a readable error.
    fn arg_bool(&self, name: &str) -> Result<bool, String> {
        self.args
            .get(name)
            .and_then(|v| v.as_bool())
            .ok_or_else(|| format!("{}: missing bool arg '{name}'", self.command))
    }

    /// Read an optional bool arg (`null`/absent -> `None`).
    fn arg_opt_bool(&self, name: &str) -> Option<bool> {
        self.args.get(name).and_then(|v| v.as_bool())
    }

    /// Read a required `string[]` arg, or a readable error.
    fn arg_str_vec(&self, name: &str) -> Result<Vec<String>, String> {
        self.args
            .get(name)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .ok_or_else(|| format!("{}: missing string[] arg '{name}'", self.command))
    }

    /// The shared DB pool.
    fn db(&self) -> &nyx_core::db::Db {
        self.runtime.db()
    }

    /// Serialize a value to the JSON-string result the supervisor parses back.
    fn json<T: serde::Serialize>(v: &T) -> Result<String, String> {
        serde_json::to_string(v).map_err(|e| format!("serialize result: {e}"))
    }

    /// Map a Diesel error to a readable string.
    fn db_err(e: diesel::result::Error) -> String {
        format!("db error: {e}")
    }

    /// Refuse a template mutation while any of its instances is running (parity with the v1
    /// `guard_template_not_running`). The live runner's map is authoritative; a `None`/
    /// un-built runner means nothing is running yet, so the guard passes.
    fn guard_template_not_running(&self, template_id: &str) -> Result<(), String> {
        let instance_ids = self
            .db()
            .with_conn(|c| db::instance_ids_for_template(c, template_id))
            .map_err(Self::db_err)?;
        let running = {
            let guard = self.runtime.runner_cell().lock().unwrap();
            guard
                .as_ref()
                .map(|r| r.any_running(&instance_ids))
                .unwrap_or(false)
        };
        if running {
            return Err("this command is running in at least one workspace — stop it before editing or deleting it".to_string());
        }
        Ok(())
    }

    /// Resolve the Claude plugin install descriptor + CLI driver from the runtime's
    /// resolved dirs (the v1 `claude_plugin`): the bundled plugin SOURCE (via
    /// `resource_dir`), the STABLE install dir (under `data_dir`), and the `claude plugin`
    /// CLI driver. Each is `None` when its paths cannot be resolved.
    fn claude_plugin(
        &self,
    ) -> (
        Option<nyx_core::plugin::PluginInstall>,
        Option<Box<dyn nyx_core::plugin::PluginCli>>,
    ) {
        use nyx_core::agent::AgentAdapter;
        let resource_dir = self.runtime.resource_dir();
        let app_data_dir = self.runtime.data_dir().to_path_buf();
        let install =
            nyx_core::agent::ClaudeCodeAdapter.plugin_install(resource_dir, Some(&app_data_dir));
        let cli = nyx_core::agent::ClaudeCodeAdapter.plugin_cli();
        (install, cli)
    }

    /// Install the integration for `provider` (parity with the v1 `integration_install`):
    /// resolve the Claude plugin descriptor + the legacy-MCP `OnboardingTarget`, then drive
    /// the SHARED `nyx_core::integrations::install` core. Returns the post-mutation status.
    fn integration_install(
        &self,
        provider: &str,
    ) -> Result<nyx_core::integrations::IntegrationStatus, String> {
        let state_path = nyx_core::integrations::state_path_in(self.runtime.data_dir());
        let target = nyx_core::onboarding::OnboardingTarget::claude_code()
            .ok_or_else(|| "Could not resolve Claude Code config path (no home dir)".to_string())?;
        let (install, cli) = self.claude_plugin();
        nyx_core::integrations::install(
            provider,
            &target,
            install.as_ref(),
            cli.as_deref(),
            &state_path,
        )
    }

    /// Uninstall the integration for `provider` (parity with the v1 `integration_remove`):
    /// the mirror of [`Self::integration_install`], driving `nyx_core::integrations::remove`.
    fn integration_remove(
        &self,
        provider: &str,
    ) -> Result<nyx_core::integrations::IntegrationStatus, String> {
        let state_path = nyx_core::integrations::state_path_in(self.runtime.data_dir());
        let target = nyx_core::onboarding::OnboardingTarget::claude_code()
            .ok_or_else(|| "Could not resolve Claude Code config path (no home dir)".to_string())?;
        let (install, cli) = self.claude_plugin();
        nyx_core::integrations::remove(
            provider,
            &target,
            install.as_ref(),
            cli.as_deref(),
            &state_path,
        )
    }

    /// The ONE allowlisted match on the contract command name. Each arm is a thin wrapper
    /// over the SAME `nyx_core` logic the v1 dispatcher called. `()` -> JSON `"null"`.
    ///
    /// TRANSPORT INVARIANT: every arm's `Ok(_)` payload MUST be a valid JSON string — produced
    /// by [`Self::json`] (any serializable value, INCLUDING a bare `String` scrollback) or the
    /// literal `"null"`. The relay (`apps/desktop/src/main/sidecar.ts`) `JSON.parse`s EVERY
    /// `db_result.resultJson` with no exceptions, so a raw, un-encoded String would throw there
    /// and be swallowed by the renderer (regression #43: the `command_output` backlog never
    /// replayed). Do NOT return a raw `String` from an arm — wrap it through [`Self::json`].
    fn run(&self) -> Result<String, String> {
        match self.command {
            // --- terminals -----------------------------------------------------
            "list_terminals" => self
                .db()
                .with_conn(db::list_terminals)
                .map_err(Self::db_err)
                .and_then(|rows| Self::json(&rows)),
            "create_terminal" => {
                let cwd = self.arg_str("cwd")?;
                let label = self.arg_opt_str("label");
                self.db()
                    .with_conn(|c| db::create_terminal(c, &cwd, label.clone()))
                    .map_err(Self::db_err)
                    .and_then(|row| Self::json(&row))
            }
            "close_terminal" => {
                let id = self.arg_str("id")?;
                self.db()
                    .with_conn(|c| db::close_terminal(c, &id))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "reorder" => {
                let ids = self.arg_str_vec("ids")?;
                self.db()
                    .with_conn(|c| db::reorder(c, &ids))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "projects_reorder" => {
                let ids = self.arg_str_vec("ids")?;
                self.db()
                    .with_conn(|c| db::reorder_projects(c, &ids))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "rename" => {
                let id = self.arg_str("id")?;
                let label = self.arg_opt_str("label");
                self.db()
                    .with_conn(|c| db::rename(c, &id, label.clone()))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "set_active" => {
                let id = self.arg_str("id")?;
                self.db()
                    .with_conn(|c| db::set_active(c, &id))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "persist_scrollback" => {
                let id = self.arg_str("id")?;
                let serialized = self.arg_str("serialized")?;
                self.db()
                    .with_conn(|c| db::persist_scrollback(c, &id, &serialized))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "set_terminal_cwd" => {
                let id = self.arg_str("id")?;
                let cwd = self.arg_str("cwd")?;
                self.db()
                    .with_conn(|c| db::set_terminal_cwd(c, &id, &cwd))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "terminal_exec_mark_read" => {
                let id = self.arg_str("id")?;
                self.db()
                    .with_conn(|c| db::mark_exec_state_read(c, &id))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "attach_terminal" => {
                let terminal_id = self.arg_str("terminalId")?;
                let workspace_id = self.arg_str("workspaceId")?;
                let mode = self.arg_str("mode")?;
                self.db()
                    .with_conn(|c| db::attach_terminal(c, &terminal_id, &workspace_id, &mode))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            // --- live-PTY introspection + auto-attach (host-owned, not the DB) --
            "terminal_info" => {
                // `{ id: ptyId }` -> live `{ cwd, foreground }` (Linux `/proc`). Unknown /
                // exited pty -> `{ null, null }` WITHOUT erroring (so the poll never spams).
                let pty_id = self
                    .args
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| format!("{}: missing number arg 'id'", self.command))?
                    as u32;
                let (cwd, foreground) = self.runtime.pty().terminal_info(pty_id);
                Self::json(&serde_json::json!({ "cwd": cwd, "foreground": foreground }))
            }
            "register_terminal_pty" => {
                // The sidecar OWNS the PTY and auto-registers the record -> live-pty join on
                // spawn (and retracts it on close), so the v1 explicit publish is a no-op
                // here — kept in the surface so the renderer's call succeeds. The liveness
                // registry the MCP dispatcher reads is already maintained by `PtyManager`.
                Ok("null".to_string())
            }
            "auto_attach_terminal" => {
                // `{ terminalId, cwd }` -> run the shared resolver + persist the decided
                // binding; `{ workspace_id, changed }` back (the snake_case the renderer reads).
                let terminal_id = self.arg_str("terminalId")?;
                let cwd = self.arg_opt_str("cwd");
                let res = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::resolve::auto_attach_terminal(c, &terminal_id, cwd.as_deref())
                    })
                    .map_err(Self::db_err)?;
                Self::json(&serde_json::json!({
                    "workspace_id": res.workspace_id,
                    "changed": res.changed,
                }))
            }
            // --- projects ------------------------------------------------------
            "list_projects" => self
                .db()
                .with_conn(db::list_projects)
                .map_err(Self::db_err)
                .and_then(|rows| Self::json(&rows)),
            "create_project" => {
                let name = self.arg_str("name")?;
                let root_path = self.arg_str("rootPath")?;
                let root_name = self.arg_opt_str("rootName");
                self.db()
                    .with_conn(|c| db::create_project(c, &name, &root_path, root_name.as_deref()))
                    .map_err(Self::db_err)
                    .and_then(|(project, root)| {
                        Self::json(&serde_json::json!({ "project": project, "root": root }))
                    })
            }
            "update_project" => {
                let id = self.arg_str("id")?;
                let name = self.arg_str("name")?;
                self.db()
                    .with_conn(|c| db::update_project(c, &id, &name))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "delete_project" => {
                let id = self.arg_str("id")?;
                self.db()
                    .with_conn(|c| db::delete_project(c, &id))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "set_project_collapsed" => {
                let id = self.arg_str("id")?;
                let collapsed = self.arg_bool("collapsed")?;
                self.db()
                    .with_conn(|c| db::set_project_collapsed(c, &id, collapsed))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "set_project_resume_agent_sessions" => {
                let id = self.arg_str("id")?;
                let resume = self.arg_bool("resume")?;
                self.db()
                    .with_conn(|c| db::set_project_resume_agent_sessions(c, &id, resume))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            // --- workspaces ----------------------------------------------------
            "list_workspaces" => {
                let project_id = self.arg_str("projectId")?;
                self.db()
                    .with_conn(|c| db::list_workspaces(c, &project_id))
                    .map_err(Self::db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            "create_workspace" => {
                let project_id = self.arg_str("projectId")?;
                let name = self.arg_str("name")?;
                let path = self.arg_str("path")?;
                self.db()
                    .with_conn(|c| db::create_workspace(c, &project_id, &name, &path))
                    .map_err(Self::db_err)
                    .and_then(|row| Self::json(&row))
            }
            "rename_workspace" => {
                let id = self.arg_str("id")?;
                let name = self.arg_str("name")?;
                self.db()
                    .with_conn(|c| db::rename_workspace(c, &id, &name))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "workspace_delete" => {
                let id = self.arg_str("id")?;
                let workspace = self
                    .db()
                    .with_conn(|c| db::get_workspace(c, &id))
                    .map_err(Self::db_err)?;
                match workspace {
                    None => return Err(format!("unknown workspace {id}")),
                    Some(ws) if ws.is_root => {
                        return Err("this is the project's root workspace — it cannot be removed on its own; delete the whole project instead".to_string())
                    }
                    Some(_) => {}
                }
                let instance_ids = self
                    .db()
                    .with_conn(|c| db::instance_ids_for_workspace(c, &id))
                    .map_err(Self::db_err)?;
                let running = {
                    let guard = self.runtime.runner_cell().lock().unwrap();
                    guard
                        .as_ref()
                        .map(|r| r.any_running(&instance_ids))
                        .unwrap_or(false)
                };
                if running {
                    return Err("this workspace has a running command — stop it before removing the workspace".to_string());
                }
                let deleted = self
                    .db()
                    .with_conn(|c| db::delete_workspace(c, &id))
                    .map_err(Self::db_err)?;
                if deleted == 0 {
                    return Err(format!("unknown workspace {id}"));
                }
                Ok("null".to_string())
            }
            "set_workspace_collapsed" => {
                let id = self.arg_str("id")?;
                let collapsed = self.arg_bool("collapsed")?;
                self.db()
                    .with_conn(|c| db::set_workspace_collapsed(c, &id, collapsed))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            // --- managed-command templates / instances (read + CRUD + source/import).
            //     The RUNTIME start/stop/relaunch ride the typed pipe kinds; output +
            //     acknowledge route below to the live runner. ----------------------
            "command_list" => {
                let project_id = self.arg_str("projectId")?;
                self.db()
                    .with_conn(|c| db::list_templates(c, &project_id))
                    .map_err(Self::db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            "command_create" => {
                let project_id = self.arg_str("projectId")?;
                let name = self.arg_str("name")?;
                let command = self.arg_str("command")?;
                let subfolder = self.arg_opt_str("subfolder");
                let restart_on_startup = self.arg_opt_bool("restartOnStartup");
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
                self.db()
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
                    .map_err(Self::db_err)
                    .and_then(|row| Self::json(&row))
            }
            "command_update" => {
                let id = self.arg_str("id")?;
                let name = self.arg_str("name")?;
                let command = self.arg_str("command")?;
                let subfolder = self.arg_opt_str("subfolder");
                let restart_on_startup = self.arg_opt_bool("restartOnStartup");
                self.guard_template_not_running(&id)?;
                self.db()
                    .with_conn(|c| {
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
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "command_delete" => {
                let id = self.arg_str("id")?;
                self.guard_template_not_running(&id)?;
                self.db()
                    .with_conn(|c| db::delete_template(c, &id))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "command_source_refresh" => {
                let id = self.arg_str("id")?;
                self.db()
                    .with_conn(|c| nyx_core::pkgjson::source_refresh(c, &id))
                    .map_err(|e| e.to_string())
                    .and_then(|r| Self::json(&r))
            }
            "command_resync_source" => {
                let id = self.arg_str("id")?;
                self.guard_template_not_running(&id)?;
                self.db()
                    .with_conn(|c| nyx_core::pkgjson::source_resync(c, &id))
                    .map_err(|e| e.to_string())
                    .and_then(|body| Self::json(&body))
            }
            "command_unlink_source" => {
                let id = self.arg_str("id")?;
                self.db()
                    .with_conn(|c| db::set_template_source(c, &id, db::CommandSource::default()))
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "command_import_scripts" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let path = self
                    .db()
                    .with_conn(|c| db::workspace_path(c, &workspace_id))
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown workspace {workspace_id}"))?;
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
                self.db()
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
                    .map_err(|e| e.to_string())
                    .and_then(|row| Self::json(&row))
            }
            "command_instance_list" => {
                let workspace_id = self.arg_str("workspaceId")?;
                self.db()
                    .with_conn(|c| db::list_instances_for_workspace(c, &workspace_id))
                    .map_err(Self::db_err)
                    .and_then(|mut rows| {
                        for row in &mut rows {
                            row.cwd = Some(nyx_core::subfolder::resolve_run_dir_lossy(
                                &row.workspace_path,
                                row.subfolder.as_deref(),
                            ));
                        }
                        Self::json(&rows)
                    })
            }
            // --- managed-command RUNTIME output / acknowledge -------------------
            "command_output" => {
                // The scrollback is a String, but it MUST still ride the transport JSON-encoded
                // like every other arm: the relay (`sidecar.ts`) `JSON.parse`s EVERY `db_result`,
                // so a raw String would fail to parse (regression #43). `Self::json` emits a JSON
                // string literal the relay decodes straight back to the original scrollback.
                let instance_id = self.arg_str("instanceId")?;
                let cmd = self
                    .runtime
                    .command()
                    .ok_or_else(|| "managed-command runtime not started".to_string())?;
                let output = cmd.get_output(&instance_id).map_err(|e| e.to_string())?;
                Self::json(&output)
            }
            "command_acknowledge" => {
                let instance_id = self.arg_str("instanceId")?;
                let cmd = self
                    .runtime
                    .command()
                    .ok_or_else(|| "managed-command runtime not started".to_string())?;
                Self::json(&cmd.acknowledge(&instance_id))
            }
            // --- agents --------------------------------------------------------
            "agent_active_sessions" => self
                .db()
                .with_conn(db::active_agent_sessions)
                .map_err(Self::db_err)
                .and_then(|rows| Self::json(&rows)),
            "agent_close_warnings" => {
                use nyx_core::agent_resume::{
                    close_warning_message, should_warn_on_close, SessionState,
                };
                #[derive(serde::Serialize)]
                struct CloseWarningEntry {
                    terminal_id: String,
                    agent_kind: String,
                    message: String,
                }
                self.db()
                    .with_conn(db::close_warning_candidates)
                    .map_err(Self::db_err)
                    .and_then(|rows| {
                        let entries: Vec<CloseWarningEntry> = rows
                            .into_iter()
                            .filter(|w| {
                                SessionState::from_db(&w.session_state)
                                    .is_some_and(|s| should_warn_on_close(s, w.project_resume_on))
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
            // The RUNTIME agent-activity map (the live dot) — a synchronous in-memory read
            // off the SAME store the MCP `agent_session_event` hooks write. NOT the DB
            // (activity is never persisted). Mapped to the snake_case contract shape.
            "agent_activity_snapshot" => {
                #[derive(serde::Serialize)]
                struct ActivityRow {
                    terminal_id: String,
                    activity: &'static str,
                    ready_unread: bool,
                    error_unread: bool,
                    plugin_outdated: bool,
                }
                let rows: Vec<ActivityRow> = self
                    .runtime
                    .activity()
                    .snapshot_all()
                    .into_iter()
                    .map(|(terminal_id, snap)| ActivityRow {
                        terminal_id,
                        activity: activity_str(snap.activity),
                        ready_unread: snap.ready_unread,
                        error_unread: snap.error_unread,
                        plugin_outdated: snap.plugin_outdated,
                    })
                    .collect();
                Self::json(&rows)
            }
            "agent_mark_ready_read" => {
                // Clear the focus-aware "response ready" notification for a viewed terminal
                // (the activity analogue of `terminal_exec_mark_read`). Idempotent; NEVER
                // persisted. Nudge the renderer to re-pull so the cleared dot disappears.
                let terminal_id = self.arg_str("terminalId")?;
                self.runtime.activity().mark_ready_read(&terminal_id);
                Ok("null".to_string())
            }
            // --- integrations (Settings -> Integrations: the ONE bundled Claude plugin =
            //     MCP + session-capture hooks). Routed onto the SHARED nyx_core::integrations
            //     cores so the Install/Uninstall button drives the EXACT same `claude` plugin
            //     install/uninstall as v1, at parity. install/remove SHELL OUT to `claude`. --
            // --- crash recovery (Phase 4) -------------------------------------
            // The boot crash reconcile flags command instances persisted as `running` (whose
            // liveness it could not re-verify) as `interrupted`. This read surfaces that set
            // so the renderer badges them and the user chooses what to resume — NEVER an auto-
            // relaunch (the PRD non-goal). `{ interrupted: string[] }`.
            "recovery_snapshot" => Self::json(&serde_json::json!({
                "interrupted": self.runtime.interrupted_instances(),
            })),
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
            other => Err(format!(
                "db command '{other}' is not available over the sidecar transport"
            )),
        }
    }
}

/// Flatten the runtime [`Activity`](nyx_core::agent_activity::Activity) to the lowercase
/// string the renderer reads (`working` | `waiting` | `idle`).
fn activity_str(activity: nyx_core::agent_activity::Activity) -> &'static str {
    use nyx_core::agent_activity::Activity;
    match activity {
        Activity::Idle => "idle",
        Activity::Working => "working",
        Activity::Waiting => "waiting",
    }
}

/// The template-MUTATING DB commands — each broadcasts `commands://changed` on success
/// (parity with the v1 commands that call `emit_commands_changed`).
fn mutates_command_templates(command: &str) -> bool {
    matches!(
        command,
        "command_create"
            | "command_update"
            | "command_delete"
            | "command_resync_source"
            | "command_unlink_source"
            | "command_import_create"
    )
}

/// The project/workspace TREE-MUTATING DB commands that broadcast `workspaces://changed`
/// on success (parity with the v1 `emit_workspaces_changed`).
fn mutates_workspace_tree(command: &str) -> bool {
    matches!(
        command,
        "create_project"
            | "delete_project"
            | "projects_reorder"
            | "create_workspace"
            | "workspace_delete"
    )
}
