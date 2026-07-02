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

use super::events::{EventSink, PushEventSink};
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
        events,
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
    // mutation broadcasts `commands://changed`; a project/workspace TREE mutation that runs
    // OUTSIDE a use case (so no domain event drives its push) broadcasts `workspaces://changed`
    // here, so every surface re-pulls. Emitted ONLY after a successful mutation (an `Err`
    // above skips it).
    //
    // The IDENTITY mutations (`create_project`, `create_workspace`, `rename_workspace`,
    // `workspace_delete`) are NOT broadcast here: each now runs through the shared
    // `usecase::*` layer and pushes `workspaces://changed` via the domain-event-driven
    // [`PushEventSink`] (task #10, same as the MCP path). Broadcasting them here too would
    // DOUBLE-push. So the legacy list is trimmed to the genuine tree mutations that have no
    // use case emitting a domain event (`projects_reorder`, `delete_project`).
    if mutates_command_templates(command) {
        events.emit(Event::Changed {
            topic: "commands".to_string(),
        });
    } else if pushes_workspace_tree_legacy(command) {
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

/// The per-call dispatch context: the runtime handle + the command name + its parsed args +
/// the pipe event sink (so an IDENTITY mutation can build a domain-event-driven
/// [`PushEventSink`], the same real-time push the MCP path uses).
struct Ctx<'a> {
    runtime: &'a Runtime,
    command: &'a str,
    args: Value,
    events: &'a EventSink,
}

impl Ctx<'_> {
    /// The domain-event-driven push sink for an IDENTITY mutation of the UI path, built from
    /// the sidecar's pipe event sink — the SAME real-time push the MCP path wires (`mcp.rs`).
    /// A use case emits its `DomainEvent` AFTER commit; this sink turns it into the coarse
    /// `workspaces://changed` invalidation, so the push is driven by the committed DOMAIN
    /// event (not by a string-matched command name in the legacy broadcast below).
    fn push_sink(&self) -> PushEventSink {
        PushEventSink::new(self.events.clone())
    }

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
            // Every identity op below delegates to the SHARED `nyx_core::usecase` layer
            // (PDR-0005, v2.1 task #7) — the SAME application service the MCP surface calls
            // — so NO business logic lives in this UI bridge. The use cases emit domain
            // events through the injected sink; in Phase 3 the UI bridge passes a
            // `NullEventSink` (no consumer yet — the persistent `domain_events` table and
            // the push pipeline are Phase 4), while still broadcasting the coarse
            // `workspaces://changed` UI-refresh invalidation below as before.
            "list_projects" => self
                .db()
                .with_conn(nyx_core::usecase::list_projects)
                .map_err(Self::db_err)
                .and_then(|rows| Self::json(&rows)),
            "create_project" => {
                let name = self.arg_str("name")?;
                let root_path = self.arg_str("rootPath")?;
                let root_name = self.arg_opt_str("rootName");
                // Canonical registration flow via the SHARED use case: create the project +
                // its root workspace, auto-detect the Git repos under the root
                // (done_criterion #5), and emit `project.created` — the SAME use case the
                // MCP surface reaches (no duplicated orchestration). The push is driven by
                // that committed DOMAIN event through the [`PushEventSink`] (task #10), so
                // this command is NOT in the legacy `pushes_workspace_tree_legacy` broadcast.
                let push = self.push_sink();
                self.db()
                    .with_conn(|c| {
                        nyx_core::usecase::create_project(
                            c,
                            &push,
                            &name,
                            &root_path,
                            root_name.as_deref(),
                            nyx_core::usecase::DomainEventSource::Ui,
                        )
                    })
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
                    .with_conn(|c| nyx_core::usecase::list_workspaces(c, &project_id))
                    .map_err(Self::db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            "create_workspace" => {
                let project_id = self.arg_str("projectId")?;
                let name = self.arg_str("name")?;
                let path = self.arg_str("path")?;
                // Canonical registration flow via the SHARED use case: insert the workspace,
                // auto-detect + persist the Git repos under its root_path (done_criterion
                // #5), and emit `workspace.created` — the SAME `usecase::register_workspace`
                // the MCP `workspace_add`/`create_workspace` tools reach. The push rides the
                // committed DOMAIN event through the [`PushEventSink`] (task #10), so this
                // command is NOT in the legacy broadcast.
                let push = self.push_sink();
                self.db()
                    .with_conn(|c| {
                        nyx_core::usecase::register_workspace(
                            c,
                            &push,
                            &project_id,
                            &name,
                            &path,
                            nyx_core::usecase::DomainEventSource::Ui,
                        )
                    })
                    .map_err(Self::db_err)
                    .and_then(|row| Self::json(&row))
            }
            "rename_workspace" => {
                let id = self.arg_str("id")?;
                let name = self.arg_str("name")?;
                // Route through the SHARED use case (mirror of `rename_repository`): rename +
                // emit a correlated `workspace.renamed` domain_event IN the same transaction
                // (#9), then push via the committed DOMAIN event through the [`PushEventSink`]
                // (#10) — the SAME real-time push the MCP path uses. This closes the prior gap
                // where the UI rename went straight to `db::rename_workspace` (no event, no
                // push). NOT in the legacy broadcast (the domain event drives the push).
                let push = self.push_sink();
                self.db()
                    .with_conn(|c| {
                        nyx_core::usecase::rename_workspace(
                            c,
                            &push,
                            &id,
                            &name,
                            nyx_core::usecase::DomainEventSource::Ui,
                        )
                    })
                    .map_err(Self::db_err)?;
                Ok("null".to_string())
            }
            "workspace_delete" => {
                let id = self.arg_str("id")?;
                // RUNTIME kill owned by the bridge: STOP every running command instance +
                // CLOSE every interactive terminal PTY of the workspace BEFORE delegating.
                // The live-process authorities (the runner + the PTY manager) are the
                // sidecar's, not reachable from the nyx-core use case's `&mut SqliteConnection`,
                // so the kill runs HERE — the same pattern as `create_workspace`'s FS
                // precondition. This REPLACES the former "refuse while a command runs" guard
                // (review 01KWEX11 — "Tout supprimer + tuer les vivants": the user wants the
                // workspace's runtime cleared, not to be blocked). `stop`/`close_record` are
                // idempotent, so passing the full membership lists (no live-filter) is safe.
                let instance_ids = self
                    .db()
                    .with_conn(|c| db::instance_ids_for_workspace(c, &id))
                    .map_err(Self::db_err)?;
                let terminal_ids = self
                    .db()
                    .with_conn(|c| db::terminal_ids_for_workspace(c, &id))
                    .map_err(Self::db_err)?;
                {
                    let guard = self.runtime.runner_cell().lock().unwrap();
                    if let Some(runner) = guard.as_ref() {
                        for instance_id in &instance_ids {
                            let _ = runner.stop(instance_id);
                        }
                    }
                }
                for terminal_id in &terminal_ids {
                    self.runtime.pty().close_record(terminal_id);
                }
                // The DB-expressible business decision (root invariant + runtime-row delete +
                // the `workspace.removed` domain event) is the SINGLE `usecase::remove_workspace`,
                // shared with the MCP route — no invariant is duplicated in this bridge. The
                // push rides the committed DOMAIN event through the [`PushEventSink`] (#10),
                // so this command is NOT in the legacy broadcast.
                let push = self.push_sink();
                self.db()
                    .with_conn(|c| {
                        nyx_core::usecase::remove_workspace(
                            c,
                            &push,
                            &id,
                            nyx_core::usecase::DomainEventSource::Ui,
                        )
                    })
                    .map_err(|e| match e {
                        nyx_core::usecase::RemoveWorkspaceError::NotFound => {
                            format!("unknown workspace {id}")
                        }
                        nyx_core::usecase::RemoveWorkspaceError::IsRoot => {
                            "this is the project's root workspace — it cannot be removed on its own; delete the whole project instead".to_string()
                        }
                        nyx_core::usecase::RemoveWorkspaceError::Db(e) => Self::db_err(e),
                    })?;
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

/// The project/workspace TREE-MUTATING DB commands, for the IDEMPOTENCY guard (`dispatch`,
/// the `idempotencyKey` at-most-once crash-recovery guard). This is the FULL set of tree
/// mutations that must be recognised as duplicates on a post-crash retry — INCLUDING the
/// identity mutations now routed through the shared use cases (`rename_workspace` is here so
/// a retried rename carrying an `idempotencyKey` is a no-op success, not a second rename).
///
/// This is DELIBERATELY a SEPARATE predicate from [`pushes_workspace_tree_legacy`]: the
/// idempotency question ("is this a tree mutation to guard?") is NOT the push question ("does
/// the legacy broadcast own this command's push, or does a domain-event-driven `PushEventSink`
/// own it?"). Trimming the legacy broadcast for the domain-event-driven commands must NOT
/// weaken the idempotency guard, so the two concerns are two predicates.
fn mutates_workspace_tree(command: &str) -> bool {
    matches!(
        command,
        "create_project"
            | "delete_project"
            | "projects_reorder"
            | "create_workspace"
            | "rename_workspace"
            | "workspace_delete"
    )
}

/// The project/workspace TREE-MUTATING DB commands whose `workspaces://changed` push is still
/// owned by the LEGACY string-matched broadcast in [`dispatch`] — i.e. the genuine tree
/// mutations that run OUTSIDE the shared use-case layer, so no committed DOMAIN event drives
/// their push. The identity mutations (`create_project`, `create_workspace`,
/// `rename_workspace`, `workspace_delete`) are ABSENT: each pushes exactly once via the
/// domain-event-driven [`PushEventSink`] (task #10), so listing them here too would double-push.
///
/// `projects_reorder` is a pure ordering write (no use case, no domain event). `delete_project`
/// likewise has NO use case emitting a domain event (it calls `db::delete_project` directly),
/// so its push stays on this legacy broadcast until it is migrated to a use case.
fn pushes_workspace_tree_legacy(command: &str) -> bool {
    matches!(command, "delete_project" | "projects_reorder")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::codec::FrameDecoder;
    use crate::pipe::wire::{decode_message, Message};
    use nyx_core::db::Db;
    use std::sync::{Arc, Mutex};

    /// A fresh on-disk migrated `Db` under a unique temp dir (the sidecar cannot use
    /// nyx-core's `#[cfg(test)]` in-memory db, which is not compiled into this crate). Returns
    /// the temp DIR alongside the `Db` so the caller can pin it as the runtime's `data_dir` —
    /// the sidecar persists `sidecar-recovery.json` (the idempotency journal) NEXT to `nyx.db`,
    /// so a data dir of `PathBuf::new()` would write that file relative to the test binary's
    /// CWD (the crate root), polluting the source tree. Pinning the temp dir keeps every
    /// runtime artefact inside it.
    fn temp_db() -> (std::path::PathBuf, Arc<Db>) {
        let dir = std::env::temp_dir().join(format!(
            "nyx-dbcmd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("nyx.db")).expect("open db"));
        (dir, db)
    }

    /// A `Write` that appends to a shared buffer so a test can read back the frames the pipe
    /// sink pushed.
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

    /// Every `workspaces://changed` invalidation captured on the pipe.
    fn workspaces_changed(buf: &Arc<Mutex<Vec<u8>>>) -> usize {
        let bytes = buf.lock().unwrap().clone();
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        let mut count = 0;
        while let Some(payload) = dec.next_frame().expect("decode frame") {
            if let Ok(Message::Event(Event::Changed { topic })) = decode_message(&payload) {
                if topic == "workspaces" {
                    count += 1;
                }
            }
        }
        count
    }

    /// A dispatch harness: a `Runtime` over a temp DB + a capturing pipe sink. `run` drives the
    /// REAL [`dispatch`] (idempotency guard + legacy broadcast reconciliation included), exactly
    /// as the pipe run loop does, and returns the raw JSON result string.
    struct Harness {
        runtime: Runtime,
        events: EventSink,
        buf: Arc<Mutex<Vec<u8>>>,
    }
    impl Harness {
        fn new() -> Self {
            let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
            let events = EventSink::new(Box::new(SharedBuf(buf.clone())));
            // Pin the runtime's `data_dir` to the DB's temp dir so any runtime artefact the
            // dispatch persists (notably `sidecar-recovery.json`, the idempotency journal
            // written by `idempotency_record`) lands INSIDE the temp dir — never relative to the
            // test binary's CWD (the crate root), which would pollute the source tree.
            let (data_dir, db) = temp_db();
            Harness {
                runtime: Runtime::with_db_and_paths(db, data_dir, None),
                events,
                buf,
            }
        }
        fn run(&self, command: &str, args: serde_json::Value) -> Result<String, String> {
            dispatch(&self.runtime, command, &args.to_string(), &self.events)
        }
        fn pushes(&self) -> usize {
            workspaces_changed(&self.buf)
        }
    }

    /// #10 — every IDENTITY mutation of the UI path (create_project, create_workspace,
    /// rename_workspace, workspace_delete) pushes `workspaces://changed` EXACTLY ONCE, via the
    /// domain-event-driven `PushEventSink` — NOT double-pushed by the trimmed legacy broadcast.
    #[test]
    fn ui_identity_mutations_each_push_workspaces_changed_exactly_once() {
        let h = Harness::new();

        // create_project → 1 push (project.created domain event).
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-p" }),
            )
            .unwrap();
        assert_eq!(h.pushes(), 1, "create_project pushed exactly once");
        let created: serde_json::Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        // create_workspace → +1 push (workspace.created).
        let ws = h
            .run(
                "create_workspace",
                serde_json::json!({ "projectId": project_id, "name": "wt", "path": "/tmp/dbcmd-wt" }),
            )
            .unwrap();
        assert_eq!(h.pushes(), 2, "create_workspace pushed exactly once more");
        let ws: serde_json::Value = serde_json::from_str(&ws).unwrap();
        let ws_id = ws["id"].as_str().unwrap().to_string();

        // rename_workspace (the closed gap) → +1 push (workspace.renamed).
        h.run(
            "rename_workspace",
            serde_json::json!({ "id": ws_id, "name": "renamed" }),
        )
        .unwrap();
        assert_eq!(h.pushes(), 3, "rename_workspace pushed exactly once");

        // workspace_delete → +1 push (workspace.removed).
        h.run("workspace_delete", serde_json::json!({ "id": ws_id }))
            .unwrap();
        assert_eq!(h.pushes(), 4, "workspace_delete pushed exactly once");

        // Four identity mutations, four pushes — no double-push from the legacy broadcast.
        assert_eq!(h.pushes(), 4);

        // #9 side: the UI rename persisted a correlated `workspace.renamed` domain_event.
        let events = h
            .runtime
            .db()
            .with_conn(nyx_core::db::list_domain_events)
            .unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event_type == "workspace.renamed" && e.source == "ui"),
            "the UI rename emitted a workspace.renamed domain_event (source=ui)"
        );
    }

    /// The IDEMPOTENCY guard is NOT weakened by trimming the legacy broadcast: a retried
    /// mutation carrying the SAME `idempotencyKey` is a no-op success, and does NOT push a
    /// second time. Covers a domain-event-driven command (`create_workspace`) whose push moved
    /// off the legacy broadcast — the guard still recognises the duplicate via
    /// `mutates_workspace_tree`.
    #[test]
    fn idempotency_guard_holds_for_domain_event_driven_commands() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-idem-p" }),
            )
            .unwrap();
        let project_id = serde_json::from_str::<serde_json::Value>(&created).unwrap()["project"]
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(h.pushes(), 1);

        // First create_workspace with an idempotencyKey: applies, pushes once.
        h.run(
            "create_workspace",
            serde_json::json!({
                "projectId": project_id,
                "name": "wt",
                "path": "/tmp/dbcmd-idem-wt",
                "idempotencyKey": "k-1",
            }),
        )
        .unwrap();
        assert_eq!(h.pushes(), 2, "first apply pushed once");
        let workspaces = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::usecase::list_workspaces(c, &project_id))
            .unwrap();
        let ws_count = workspaces.len();

        // Retry with the SAME key: recognised as a duplicate → no-op success, NO second push,
        // NO second workspace inserted.
        let retry = h
            .run(
                "create_workspace",
                serde_json::json!({
                    "projectId": project_id,
                    "name": "wt",
                    "path": "/tmp/dbcmd-idem-wt",
                    "idempotencyKey": "k-1",
                }),
            )
            .unwrap();
        assert_eq!(retry, "null", "the retry is a no-op success");
        assert_eq!(h.pushes(), 2, "the guarded retry did NOT push again");
        let after = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::usecase::list_workspaces(c, &project_id))
            .unwrap();
        assert_eq!(
            after.len(),
            ws_count,
            "the guarded retry inserted no second workspace"
        );
    }
}
