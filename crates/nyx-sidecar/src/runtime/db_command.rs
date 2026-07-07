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
    } else if mutates_terminals(command) {
        // A terminal RECORD mutation driven by the UI path (create/close/attach/auto-attach)
        // broadcasts `terminals://changed`, SYMMETRIC to the MCP terminal tools (which emit
        // the same topic via `emit_effects`, `mcp_tools_core.rs:1447/1664`). Without this the
        // v2.2 cockpit read model (`useWorkspaceRuntime`, which re-pulls on `terminals://changed`)
        // never refreshes for a UI-initiated terminal mutation — the case the MCP path already
        // covered but the UI path did not (review 01KWKH7TE86AXS41Q4VS2CEXCW).
        //
        // NO reconciliation loop: the ONLY subscribers to `terminals://changed` are pure
        // READERS — `useWorkspaceRuntime` re-pulls `workspace_runtime_snapshot` and
        // `useTerminals` re-lists `list_terminals` (`use-terminals.ts:391`); neither issues a
        // terminal-MUTATING db-command in response, so the emission cannot re-trigger itself.
        events.emit(Event::Changed {
            topic: "terminals".to_string(),
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

    /// Snapshot the live-runner run-state outcomes for `instance_ids` into a plain owned map,
    /// taken UNDER the runner lock so the DB read that consumes it is not held under the lock.
    /// The map backs the `RunStateOverlay` closure the workspace-runtime read model applies
    /// (the SAME `runner.outcome` overlay as the MCP `list_commands` path). An un-built runner
    /// yields an empty map (every service then reports its PERSISTED `last_state`).
    fn run_state_outcomes(
        &self,
        instance_ids: &[String],
    ) -> std::collections::HashMap<String, (nyx_core::command::RunState, Option<i32>, bool)> {
        let guard = self.runtime.runner_cell().lock().unwrap();
        match guard.as_ref() {
            Some(runner) => instance_ids
                .iter()
                .filter_map(|id| runner.outcome(id).map(|o| (id.clone(), o)))
                .collect(),
            None => std::collections::HashMap::new(),
        }
    }

    /// Build the WORKSPACE CONTEXT-RESTORE plan (task 01KWHPV33M4H5YSMGYNW9TG6J0) by REUSING
    /// the existing resume policies — the resume decision (`agent_resume::workspace_resume_plan`
    /// over `decide_resume`), the `restart_on_startup` restore rows, and the crash-reconcile
    /// interrupted set. No new policy, no blind relaunch: the plan is what the cockpit offers so
    /// the USER chooses what resumes. Pure read; idempotent.
    fn workspace_restore_plan(&self, workspace_id: &str) -> Result<String, String> {
        // The workspace must exist (a readable error, not an empty plan, for an unknown id).
        if self
            .db()
            .with_conn(|c| nyx_core::db::get_workspace(c, workspace_id))
            .map_err(Self::db_err)?
            .is_none()
        {
            return Err(format!("unknown workspace {workspace_id}"));
        }

        // (1) Agent-session resume plan — REUSE `decide_resume` per candidate. Gather the
        // workspace's `active`/`unknown` candidates, probe each transcript on disk (the pure
        // policy needs the boolean), classify the target off the resolved shell, and resolve
        // each kind's adapter via the production registry.
        let candidates = self
            .db()
            .with_conn(|c| nyx_core::db::resume_candidates_for_workspace(c, workspace_id))
            .map_err(Self::db_err)?;
        let target =
            nyx_core::agent_resume::ResumeTarget::classify_shell(&nyx_core::pty::resolve_shell());
        // Resolve each kind to its adapter through the `AgentRegistry` — the SINGLE source of
        // truth for kind→adapter (no inline `claude_code` branch duplicated here). Today only
        // `claude_code` builds a resume command (the other kinds' `GenericAdapter` returns
        // `None`), which `decide_resume` maps to `NoResumeCommand`; if a future resumable
        // adapter is added to the registry, this restore plan reflects it with no edit here.
        let registry = nyx_core::agent::AgentRegistry::default();
        let resolve = |kind: &str| registry.get(kind);
        let owned: Vec<nyx_core::agent_resume::WorkspaceResumeCandidate> = candidates
            .iter()
            .filter_map(|c| {
                let session_state =
                    nyx_core::agent_resume::SessionState::from_db(&c.session_state)?;
                // `stat` the captured transcript path (finding #53): a missing file → skip.
                let transcript_exists = c
                    .transcript_path
                    .as_deref()
                    .map(|p| std::path::Path::new(p).exists())
                    .unwrap_or(false);
                Some(nyx_core::agent_resume::WorkspaceResumeCandidate {
                    terminal_id: &c.terminal_id,
                    session_id: &c.session_id,
                    agent_kind: &c.agent_kind,
                    external_session_id: &c.external_session_id,
                    session_state,
                    project_resume_on: c.project_resume_on,
                    transcript_exists,
                    target,
                })
            })
            .collect();
        let resume_sessions = nyx_core::agent_resume::workspace_resume_plan(&owned, &resolve);

        // (2) restart_on_startup services of this workspace — the SAME restore rows the boot
        // flow reads, filtered to this workspace's instances. Each carries whether it was
        // running at the last shutdown (so the cockpit shows what WOULD relaunch).
        let ws_instance_ids: std::collections::HashSet<String> = self
            .db()
            .with_conn(|c| nyx_core::db::instance_ids_for_workspace(c, workspace_id))
            .map_err(Self::db_err)?
            .into_iter()
            .collect();
        let restore_rows = self
            .db()
            .with_conn(nyx_core::db::all_instances_for_restore)
            .map_err(Self::db_err)?;
        #[derive(serde::Serialize)]
        struct RestartService {
            instance_id: String,
            command: String,
            was_running_on_shutdown: bool,
        }
        let restart_on_startup: Vec<RestartService> = restore_rows
            .iter()
            .filter(|r| r.restart_on_startup && ws_instance_ids.contains(&r.instance_id))
            .map(|r| RestartService {
                instance_id: r.instance_id.clone(),
                command: r.command.clone(),
                was_running_on_shutdown: r.was_running_on_shutdown,
            })
            .collect();

        // (3) interrupted instances of this workspace (the crash reconcile's doubtful set).
        let interrupted: Vec<String> = self
            .runtime
            .interrupted_instances()
            .into_iter()
            .filter(|id| ws_instance_ids.contains(id))
            .collect();

        Self::json(&serde_json::json!({
            "workspace_id": workspace_id,
            "resume_sessions": resume_sessions,
            "restart_on_startup": restart_on_startup,
            "interrupted": interrupted,
        }))
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
            "terminal_screen_rehydration" => {
                // `{ recordId }` -> `{ bytes: number[] }`: the ANSI re-hydration byte stream that
                // repaints this terminal's screen (main + alt-screen) into a FRESH wterm/ghostty
                // terminal at restart (v2.2.5 task #7). Prefers a still-live terminal's current
                // screen, else the snapshot persisted at the last shutdown; `{ bytes: [] }` when
                // there is nothing to restore. The renderer writes the bytes verbatim (ghostty is
                // the VT authority) — no grid model crosses the wire, so the payload is
                // front-lib independent. Reading is non-destructive and the stream self-resets, so
                // repeated calls are idempotent.
                let record_id = self.arg_str("recordId")?;
                let bytes = self
                    .runtime
                    .terminal_rehydration_bytes(&record_id)
                    .unwrap_or_default();
                Self::json(&serde_json::json!({ "bytes": bytes }))
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
                // Broadcast `terminals://changed` ONLY when the resolver actually re-bound the
                // terminal (its workspace grouping changed) — symmetric to the other UI terminal
                // mutations, but conditional here because a no-op resolve leaves the aggregate
                // untouched. Handled at the call-site (not the name-matched `mutates_terminals`
                // list) because the emission depends on `res.changed`, which the outer dispatch
                // block cannot see (review 01KWKH7TE86AXS41Q4VS2CEXCW).
                if res.changed {
                    self.events.emit(Event::Changed {
                        topic: "terminals".to_string(),
                    });
                }
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
            // The v2.2 cockpit read model (task 01KWHPV32YVCRBNY1JW5CDY4AY): a workspace's
            // LIVE runtime aggregate — terminals + their agent sessions, the cross-terminal
            // agent sessions, and the managed-command services with their current run state.
            // Routes through the SHARED `nyx_core::workspace_runtime::snapshot` — the EXACT
            // same read model the MCP `workspace_runtime` tool serves (PDR-0005: one
            // authority for UI + MCP) — overlaying each service's FACTUAL run state from the
            // live runner (`runner.outcome`), exactly like the MCP `list_commands` path. A
            // pure read: no mutation, no `changed` broadcast, no polling.
            "workspace_runtime_snapshot" => {
                let workspace_id = self.arg_str("workspaceId")?;
                // Snapshot the live-runner outcomes UNDER the lock into a plain map, then
                // build the overlay off that map — so the DB read below is not held under
                // the runner lock (and the overlay closure is a simple owned lookup).
                let instance_ids = self
                    .db()
                    .with_conn(|c| db::instance_ids_for_workspace(c, &workspace_id))
                    .map_err(Self::db_err)?;
                let outcomes = self.run_state_outcomes(&instance_ids);
                let overlay = move |instance_id: &str| outcomes.get(instance_id).copied();
                let snapshot = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::workspace_runtime::snapshot(c, &workspace_id, &overlay)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown workspace {workspace_id}"))?;
                Self::json(&snapshot)
            }
            // FREE-WORK entry point (task 01KWHPV33H0FX8NA4C94T5W46J): open the cockpit on a
            // project's PRIMARY workspace WITHOUT requiring a Spec ("le primary workspace est
            // l'espace de travail libre par défaut"). Resolves the primary/free-work workspace
            // (`nyx_core::usecase::resolve_primary_workspace`) and serves its runtime through
            // the SAME shared `snapshot` read model as `workspace_runtime_snapshot` — no Spec
            // seam, no separate authority (there is no `specs` table in v2.2). Pure read.
            "workspace_runtime_primary" => {
                let project_id = self.arg_str("projectId")?;
                // Resolve the primary workspace first so the overlay is scoped to ITS instances.
                let primary = self
                    .db()
                    .with_conn(|c| nyx_core::usecase::resolve_primary_workspace(c, &project_id))
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("project {project_id} has no primary workspace"))?;
                let instance_ids = self
                    .db()
                    .with_conn(|c| db::instance_ids_for_workspace(c, &primary.id))
                    .map_err(Self::db_err)?;
                let outcomes = self.run_state_outcomes(&instance_ids);
                let overlay = move |instance_id: &str| outcomes.get(instance_id).copied();
                let snapshot = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::workspace_runtime::snapshot_for_project(c, &project_id, &overlay)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("project {project_id} has no primary workspace"))?;
                Self::json(&snapshot)
            }
            // v2.2 workspace CONTEXT-RESTORE plan (task 01KWHPV33M4H5YSMGYNW9TG6J0): after a
            // restart, WHAT this workspace offers to resume — computed by REUSING the existing
            // resume policies (NO third system, Q-011). Three parts, all from live authorities:
            //   * `resume_sessions` — the per-session `decide_resume` verdict for the
            //     workspace's `active`/`unknown` sessions (via `resume_candidates_for_workspace`
            //     + `agent_resume::workspace_resume_plan`), keyed on `resume_agent_sessions`,
            //     the transcript-on-disk probe, and the target shell — the SAME policy the
            //     close-warning uses. The USER chooses what to inject (nyx never blind-resumes).
            //   * `restart_on_startup` — the workspace's services whose template opts into
            //     `restart_on_startup` (from `all_instances_for_restore`, the SAME rows the boot
            //     restore reads), each with whether it was running at the last shutdown.
            //   * `interrupted` — this workspace's instances the crash reconcile flagged
            //     `interrupted` (doubtful `running` it could not re-verify → NOT relaunched).
            // A pure read: no mutation, no `changed` broadcast, no blind relaunch. Idempotent.
            "workspace_restore_plan" => {
                let workspace_id = self.arg_str("workspaceId")?;
                self.workspace_restore_plan(&workspace_id)
            }
            // The v2.2 Git CHANGES read model (task 01KWHPV33EGTB3Z41J0RQR1BAK, PDR-0013): per
            // non-optional repo of the workspace, its branch/dirty/changed-files RE-READ from
            // Git — the EXACT same `nyx_core::workspace_git::summary` the MCP `workspace_git`
            // tool serves (PDR-0005: one authority for UI + MCP). Git state is NEVER stored as
            // truth in the DB: this arm issues only reads (no mutation, no `changed` broadcast).
            "workspace_git_summary" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let summary = self
                    .db()
                    .with_conn(|c| nyx_core::workspace_git::summary(c, &workspace_id))
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown workspace {workspace_id}"))?;
                Self::json(&summary)
            }
            // The v2.2 workspace TIMELINE read model (task 01KWHPV33SAV9NV1STSY403CF0): a
            // BOUNDED, oldest-first fil of the workspace's most recent `domain_events` (v2.1) —
            // the business "changements notables" (workspace/repository/project lifecycle),
            // NEVER a terminal byte (the outbox carries none). The EXACT same
            // `nyx_core::workspace_timeline::timeline` the MCP `workspace_timeline` tool serves
            // (PDR-0005: one authority for UI + MCP). `limit` bounds the read (omitted / 0 → the
            // read model's default ceiling). A pure read: no mutation, no `changed` broadcast,
            // no polling — the live refresh rides the existing unsolicited `*://changed` channel.
            "workspace_timeline" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let limit = self.args.get("limit").and_then(|v| v.as_i64()).unwrap_or(0);
                let timeline = self
                    .db()
                    .with_conn(|c| nyx_core::workspace_timeline::timeline(c, &workspace_id, limit))
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown workspace {workspace_id}"))?;
                Self::json(&timeline)
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

/// The terminal RECORD-MUTATING DB commands whose UI path must broadcast `terminals://changed`
/// — SYMMETRIC to the MCP terminal tools (`create_terminal`/`close_terminal`/`attach_terminal`
/// emit `ChangedTopic::Terminals` via `emit_effects`). These flip a terminal record's existence
/// or workspace binding, which the v2.2 cockpit runtime aggregate contains, so every surface
/// that re-pulls on `terminals://changed` (`useWorkspaceRuntime`, `useTerminals`) reflects the
/// mutation live — WITHOUT polling (review 01KWKH7TE86AXS41Q4VS2CEXCW).
///
/// EXCLUDED on purpose: the pure sidebar-local ops (`reorder`, `rename`, `set_active`,
/// `persist_scrollback`, `set_terminal_cwd`, `terminal_exec_mark_read`) — they do not change the
/// runtime aggregate's shape (which terminals exist / where they are bound), and the sidebar
/// already folds them in optimistically. `auto_attach_terminal` is INCLUDED but emits only when
/// it actually re-bound (`changed == true`); it is handled at its call-site, not by this list,
/// because the emission is conditional on the resolver's decision.
fn mutates_terminals(command: &str) -> bool {
    matches!(
        command,
        "create_terminal" | "close_terminal" | "attach_terminal"
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

    /// Count the `Event::Changed { topic }` invalidations for `want_topic` captured on the pipe.
    fn changed_count(buf: &Arc<Mutex<Vec<u8>>>, want_topic: &str) -> usize {
        let bytes = buf.lock().unwrap().clone();
        let mut dec = FrameDecoder::new();
        dec.push(&bytes);
        let mut count = 0;
        while let Some(payload) = dec.next_frame().expect("decode frame") {
            if let Ok(Message::Event(Event::Changed { topic })) = decode_message(&payload) {
                if topic == want_topic {
                    count += 1;
                }
            }
        }
        count
    }

    /// Every `workspaces://changed` invalidation captured on the pipe.
    fn workspaces_changed(buf: &Arc<Mutex<Vec<u8>>>) -> usize {
        changed_count(buf, "workspaces")
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
        fn terminals_changed(&self) -> usize {
            changed_count(&self.buf, "terminals")
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

    /// DONE-CRITERION (task 01KWHPV32YVCRBNY1JW5CDY4AY, criterion #2): the UI bridge's
    /// `workspace_runtime_snapshot` db-command serves the SAME shared read model the MCP
    /// `workspace_runtime` tool serves — a workspace's terminals + agent sessions +
    /// services. Proof: seed a workspace + a terminal with a live session + a command
    /// instance via the shared authorities, dispatch the db-command, and assert the
    /// aggregate shape. The read does NOT push a `changed` invalidation (it is a pure read).
    #[test]
    fn workspace_runtime_snapshot_serves_the_shared_read_model_over_the_ui_bridge() {
        let h = Harness::new();

        // Seed via the SAME db-commands the renderer uses (create_project/create_workspace).
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/wr-ui-p" }),
            )
            .unwrap();
        let project_id = serde_json::from_str::<serde_json::Value>(&created).unwrap()["project"]
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        let ws = h
            .run(
                "create_workspace",
                serde_json::json!({ "projectId": project_id, "name": "wt", "path": "/tmp/wr-ui-wt" }),
            )
            .unwrap();
        let ws_id = serde_json::from_str::<serde_json::Value>(&ws).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let pushes_before = h.pushes();

        // A command template → one instance in the workspace, plus a terminal hosting a
        // live agent session anchored to the workspace (seed directly on the shared DB).
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::create_template(
                    c,
                    &project_id,
                    "dev",
                    "npm run dev",
                    None,
                    nyx_core::db::CommandSource::default(),
                )
            })
            .unwrap();
        let term = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::db::create_terminal(c, "/tmp/wr-ui-wt", None))
            .unwrap();
        h.runtime
            .db()
            .with_conn(|c| nyx_core::db::attach_terminal(c, &term.id, &ws_id, "auto"))
            .unwrap();
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::record_session_start(
                    c,
                    &term.id,
                    "claude_code",
                    nyx_core::db::SessionCapture {
                        workspace_id: Some(ws_id.clone()),
                        external_session_id: "sess-ui".to_string(),
                        cwd: "/tmp/wr-ui-wt".to_string(),
                        transcript_path: None,
                        metadata_json: None,
                    },
                )
            })
            .unwrap();

        // READ via the UI bridge db-command.
        let snap = h
            .run(
                "workspace_runtime_snapshot",
                serde_json::json!({ "workspaceId": ws_id }),
            )
            .unwrap();
        let runtime: serde_json::Value = serde_json::from_str(&snap).unwrap();
        assert_eq!(runtime["workspace"]["id"], serde_json::json!(ws_id));
        let terminals = runtime["terminals"].as_array().unwrap();
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["id"], serde_json::json!(term.id));
        assert_eq!(
            terminals[0]["sessions"][0]["external_session_id"],
            serde_json::json!("sess-ui")
        );
        assert_eq!(runtime["agent_sessions"].as_array().unwrap().len(), 1);
        let services = runtime["services"].as_array().unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0]["name"], serde_json::json!("dev"));
        assert_eq!(services[0]["run_state"], serde_json::json!("idle"));

        // The read pushed NO `workspaces://changed` invalidation (a pure read).
        assert_eq!(
            h.pushes(),
            pushes_before,
            "workspace_runtime_snapshot is a pure read — no changed broadcast"
        );

        // An unknown workspace id is a readable error, not a panic.
        let err = h
            .run(
                "workspace_runtime_snapshot",
                serde_json::json!({ "workspaceId": "nope" }),
            )
            .unwrap_err();
        assert!(err.contains("unknown workspace"), "got: {err}");
    }

    /// FREE-WORK entry point (task 01KWHPV33H0FX8NA4C94T5W46J): `workspace_runtime_primary`
    /// opens the cockpit on a project's PRIMARY workspace WITHOUT a Spec, keyed on the PROJECT
    /// id, and returns the SAME aggregate as `workspace_runtime_snapshot` for that primary
    /// workspace. Proof: a fresh project (only its root/primary workspace — no worktree, no
    /// Spec) resolves through `workspace_runtime_primary` to its primary workspace's runtime,
    /// byte-identical to the by-workspace read. An unknown project is a readable error.
    #[test]
    fn workspace_runtime_primary_opens_the_free_work_workspace_over_the_ui_bridge() {
        let h = Harness::new();

        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/wr-free-p" }),
            )
            .unwrap();
        let created_json: serde_json::Value = serde_json::from_str(&created).unwrap();
        let project_id = created_json["project"]["id"].as_str().unwrap().to_string();
        // The project's primary (root) workspace id — the free-work default.
        let primary_id = created_json["root"]["id"].as_str().unwrap().to_string();
        let pushes_before = h.pushes();

        // Open free work by PROJECT (no Spec anywhere).
        let by_project = h
            .run(
                "workspace_runtime_primary",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap();
        let primary_runtime: serde_json::Value = serde_json::from_str(&by_project).unwrap();
        assert_eq!(
            primary_runtime["workspace"]["id"],
            serde_json::json!(primary_id),
            "free-work opens the project's primary workspace"
        );

        // It is the SAME aggregate as the by-workspace read for that primary workspace.
        let by_workspace = h
            .run(
                "workspace_runtime_snapshot",
                serde_json::json!({ "workspaceId": primary_id }),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&by_workspace).unwrap(),
            primary_runtime,
            "free-work (by project) == the primary workspace's runtime (by id): no Spec seam"
        );

        // Pure read: no `changed` broadcast.
        assert_eq!(
            h.pushes(),
            pushes_before,
            "workspace_runtime_primary is a pure read — no changed broadcast"
        );

        // An unknown project is a readable error, not a panic.
        let err = h
            .run(
                "workspace_runtime_primary",
                serde_json::json!({ "projectId": "nope" }),
            )
            .unwrap_err();
        assert!(err.contains("no primary workspace"), "got: {err}");
    }

    /// WORKSPACE CONTEXT-RESTORE plan (task 01KWHPV33M4H5YSMGYNW9TG6J0): after a restart, the
    /// `workspace_restore_plan` db-command REUSES the existing resume policies to report, for
    /// ONE workspace: (a) the per-session `decide_resume` verdicts (resume the `resume_agent_
    /// sessions`-ON candidate with a transcript, exact id); (b) the `restart_on_startup`
    /// services (with their was-running snapshot); (c) the interrupted set. Proves "sessions
    /// reprenables + restart_on_startup restaurés", scoped to the workspace, via the SAME
    /// policies (no third system). Pure read — idempotent, no `changed` broadcast.
    #[test]
    fn workspace_restore_plan_reuses_existing_policies_scoped_to_workspace() {
        let h = Harness::new();

        // A project whose resume_agent_sessions is ON (so an active session is a resume candidate).
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/wr-restore-p" }),
            )
            .unwrap();
        let created_json: serde_json::Value = serde_json::from_str(&created).unwrap();
        let project_id = created_json["project"]["id"].as_str().unwrap().to_string();
        let ws_id = created_json["root"]["id"].as_str().unwrap().to_string();
        h.run(
            "set_project_resume_agent_sessions",
            serde_json::json!({ "id": project_id, "resume": true }),
        )
        .unwrap();

        // A template with restart_on_startup ON → its instance in the workspace, snapshotted
        // running-at-shutdown so the plan shows it WOULD relaunch.
        let restart_instance = h
            .runtime
            .db()
            .with_conn(|c| {
                let tpl = nyx_core::db::create_template(
                    c,
                    &project_id,
                    "dev",
                    "npm run dev",
                    None,
                    nyx_core::db::CommandSource::default(),
                )?;
                nyx_core::db::set_restart_on_startup(c, &tpl.id, true)?;
                let ids = nyx_core::db::instance_ids_for_template(c, &tpl.id)?;
                let id = ids[0].clone();
                nyx_core::db::set_was_running_on_shutdown(c, &id, true)?;
                Ok::<_, diesel::result::Error>(id)
            })
            .unwrap();

        // A terminal in the workspace hosting an ACTIVE Claude session WITH a real transcript
        // on disk (so `decide_resume` resumes it — finding #53's present→resume half).
        let transcript = h.runtime.data_dir().join("sess.jsonl");
        std::fs::write(&transcript, b"{}\n").unwrap();
        let term = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::db::create_terminal(c, "/tmp/wr-restore-p", None))
            .unwrap();
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::attach_terminal(c, &term.id, &ws_id, "auto")?;
                nyx_core::db::record_session_start(
                    c,
                    &term.id,
                    nyx_core::db::AGENT_KIND_CLAUDE_CODE,
                    nyx_core::db::SessionCapture {
                        workspace_id: Some(ws_id.clone()),
                        external_session_id: "ext-restore".to_string(),
                        cwd: "/tmp/wr-restore-p".to_string(),
                        transcript_path: Some(transcript.to_string_lossy().to_string()),
                        metadata_json: None,
                    },
                )?;
                Ok::<_, diesel::result::Error>(())
            })
            .unwrap();
        let pushes_before = h.pushes();

        let plan_json = h
            .run(
                "workspace_restore_plan",
                serde_json::json!({ "workspaceId": ws_id }),
            )
            .unwrap();
        let plan: serde_json::Value = serde_json::from_str(&plan_json).unwrap();

        // (a) The active session resumes with the EXACT id (policy reused verbatim).
        let sessions = plan["resume_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "one resume candidate in the workspace");
        assert_eq!(sessions[0]["resume"], serde_json::json!(true));
        assert_eq!(
            sessions[0]["resume_command"],
            serde_json::json!("claude --resume ext-restore")
        );
        assert_eq!(sessions[0]["uncertain"], serde_json::json!(false));

        // (b) The restart_on_startup service is present with its was-running snapshot.
        let services = plan["restart_on_startup"].as_array().unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(
            services[0]["instance_id"],
            serde_json::json!(restart_instance)
        );
        assert_eq!(
            services[0]["was_running_on_shutdown"],
            serde_json::json!(true)
        );

        // (c) No crash this run → interrupted is empty (nothing doubtful, no blind relaunch).
        assert!(plan["interrupted"].as_array().unwrap().is_empty());

        // Pure read + idempotent: no `changed` broadcast, and a second call is byte-identical.
        assert_eq!(
            h.pushes(),
            pushes_before,
            "workspace_restore_plan is a pure read — no changed broadcast"
        );
        let plan_again = h
            .run(
                "workspace_restore_plan",
                serde_json::json!({ "workspaceId": ws_id }),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&plan_again).unwrap(),
            plan,
            "the restore plan is idempotent — a repeat call yields the same plan"
        );

        // An unknown workspace is a readable error.
        let err = h
            .run(
                "workspace_restore_plan",
                serde_json::json!({ "workspaceId": "nope" }),
            )
            .unwrap_err();
        assert!(err.contains("unknown workspace"), "got: {err}");
    }

    /// WORKSPACE RESTORE — DOUBTFUL STATE: with `resume_agent_sessions` OFF, an `unknown`
    /// (stale/probable-kill) session is reported as a NON-resume entry (`option_off`), never
    /// blind-resumed; the plan surfaces the doubtful state for the user to choose. Covers the
    /// done-criterion "états douteux marqués unknown/interrupted".
    #[test]
    fn workspace_restore_plan_surfaces_doubtful_state_without_blind_resume() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/wr-doubt-p" }),
            )
            .unwrap();
        let created_json: serde_json::Value = serde_json::from_str(&created).unwrap();
        let ws_id = created_json["root"]["id"].as_str().unwrap().to_string();
        // resume_agent_sessions stays OFF (the default) → no auto-resume offered.

        // A terminal in the workspace hosting an UNKNOWN (doubtful) session.
        let term = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::db::create_terminal(c, "/tmp/wr-doubt-p", None))
            .unwrap();
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::attach_terminal(c, &term.id, &ws_id, "auto")?;
                nyx_core::db::record_session_start(
                    c,
                    &term.id,
                    nyx_core::db::AGENT_KIND_CLAUDE_CODE,
                    nyx_core::db::SessionCapture {
                        workspace_id: Some(ws_id.clone()),
                        external_session_id: "ext-doubt".to_string(),
                        cwd: "/tmp/wr-doubt-p".to_string(),
                        transcript_path: None,
                        metadata_json: None,
                    },
                )?;
                // Flip every still-`active` session to `unknown` (a stale active after a probable
                // kill) via the SAME sweep the crash reconcile uses — the doubtful-state path.
                nyx_core::db::sweep_stale_active_sessions(c, -1)?;
                Ok::<_, diesel::result::Error>(())
            })
            .unwrap();

        let plan_json = h
            .run(
                "workspace_restore_plan",
                serde_json::json!({ "workspaceId": ws_id }),
            )
            .unwrap();
        let plan: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
        let sessions = plan["resume_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "the doubtful session is surfaced");
        assert_eq!(
            sessions[0]["resume"],
            serde_json::json!(false),
            "option OFF → not auto-resumed (no blind resume)"
        );
        assert_eq!(sessions[0]["skip_reason"], serde_json::json!("option_off"));
    }

    /// REVIEW 01KWKH7TE86AXS41Q4VS2CEXCW (+ 01KWKHC0FW9D2K69ATK4266H32): a terminal RECORD
    /// mutation driven by the UI db-command path (create/close/attach/auto-attach) broadcasts
    /// `terminals://changed`, SYMMETRIC to the MCP terminal tools — so the v2.2 cockpit
    /// (`useWorkspaceRuntime`, which re-pulls on `terminals://changed`) refreshes live for a
    /// UI-initiated terminal mutation. Before the fix these arms emitted NO invalidation, so a
    /// UI create/close/attach left the cockpit aggregate stale.
    #[test]
    fn ui_terminal_mutations_broadcast_terminals_changed() {
        let h = Harness::new();

        // create_terminal → 1 `terminals://changed` (a new record enters the aggregate).
        let created = h
            .run(
                "create_terminal",
                serde_json::json!({ "cwd": "/tmp/tc-ui" }),
            )
            .unwrap();
        assert_eq!(
            h.terminals_changed(),
            1,
            "create_terminal broadcast terminals://changed once"
        );
        let term_id = serde_json::from_str::<serde_json::Value>(&created).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        // A workspace to attach the terminal to (its own identity pushes are on `workspaces`,
        // NOT counted here).
        let project = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/tc-ui-p" }),
            )
            .unwrap();
        let project_id = serde_json::from_str::<serde_json::Value>(&project).unwrap()["project"]
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        let ws = h
            .run(
                "create_workspace",
                serde_json::json!({ "projectId": project_id, "name": "wt", "path": "/tmp/tc-ui-wt" }),
            )
            .unwrap();
        let ws_id = serde_json::from_str::<serde_json::Value>(&ws).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        // attach_terminal → +1 `terminals://changed` (the terminal's workspace grouping changed).
        h.run(
            "attach_terminal",
            serde_json::json!({ "terminalId": term_id, "workspaceId": ws_id, "mode": "manual" }),
        )
        .unwrap();
        assert_eq!(
            h.terminals_changed(),
            2,
            "attach_terminal broadcast terminals://changed once more"
        );

        // close_terminal → +1 `terminals://changed` (the record leaves the aggregate).
        h.run("close_terminal", serde_json::json!({ "id": term_id }))
            .unwrap();
        assert_eq!(
            h.terminals_changed(),
            3,
            "close_terminal broadcast terminals://changed once more"
        );

        // A NON-shape terminal op (rename) does NOT broadcast — it does not change which
        // terminals exist / where they are bound, so the aggregate shape is untouched.
        let before = h.terminals_changed();
        let t2 = h
            .run(
                "create_terminal",
                serde_json::json!({ "cwd": "/tmp/tc-ui-2" }),
            )
            .unwrap();
        let t2_id = serde_json::from_str::<serde_json::Value>(&t2).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let after_create = h.terminals_changed();
        assert_eq!(after_create, before + 1, "the second create broadcast once");
        h.run("rename", serde_json::json!({ "id": t2_id, "label": "x" }))
            .unwrap();
        assert_eq!(
            h.terminals_changed(),
            after_create,
            "rename does NOT broadcast terminals://changed (no aggregate-shape change)"
        );
    }

    /// REVIEW 01KWKH7TE86AXS41Q4VS2CEXCW: `auto_attach_terminal` broadcasts `terminals://changed`
    /// ONLY when it actually re-bound the terminal (`changed == true`) — a no-op resolve leaves
    /// the aggregate untouched and emits nothing.
    #[test]
    fn ui_auto_attach_terminal_broadcasts_only_when_it_rebinds() {
        let h = Harness::new();
        // A terminal + a workspace whose root is the terminal's cwd, so the resolver binds it.
        let created = h
            .run(
                "create_terminal",
                serde_json::json!({ "cwd": "/tmp/tc-aa-wt" }),
            )
            .unwrap();
        let term_id = serde_json::from_str::<serde_json::Value>(&created).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let project = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/tc-aa-p" }),
            )
            .unwrap();
        let project_id = serde_json::from_str::<serde_json::Value>(&project).unwrap()["project"]
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        h.run(
            "create_workspace",
            serde_json::json!({ "projectId": project_id, "name": "wt", "path": "/tmp/tc-aa-wt" }),
        )
        .unwrap();

        let before = h.terminals_changed();
        // First auto-attach: the cwd matches the workspace root → the resolver re-binds → emits.
        let res = h
            .run(
                "auto_attach_terminal",
                serde_json::json!({ "terminalId": term_id, "cwd": "/tmp/tc-aa-wt" }),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&res).unwrap()["changed"],
            serde_json::json!(true)
        );
        assert_eq!(
            h.terminals_changed(),
            before + 1,
            "a re-binding auto-attach broadcast terminals://changed once"
        );

        // Second auto-attach with the SAME cwd: already bound → `changed == false` → NO emit.
        let res2 = h
            .run(
                "auto_attach_terminal",
                serde_json::json!({ "terminalId": term_id, "cwd": "/tmp/tc-aa-wt" }),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&res2).unwrap()["changed"],
            serde_json::json!(false)
        );
        assert_eq!(
            h.terminals_changed(),
            before + 1,
            "a no-op auto-attach broadcast NOTHING"
        );
    }

    /// DONE-CRITERION (task 01KWHPV33EGTB3Z41J0RQR1BAK): the UI bridge's
    /// `workspace_git_summary` db-command serves the SAME Git CHANGES read model the MCP
    /// `workspace_git` tool serves — branch/dirty/changed-files per non-optional repo,
    /// RE-READ from Git. Proof: register a real git repo as a workspace, dirty it, read via
    /// the db-command, and assert the change surfaces. The read pushes NO `changed`
    /// invalidation and stores NOTHING (Git is never authoritative in the DB). Git-guarded.
    #[test]
    fn workspace_git_summary_serves_the_shared_git_read_model_over_the_ui_bridge() {
        if nyx_core::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping ui workspace_git_summary test: git unavailable");
            return;
        }
        let h = Harness::new();

        // A real git repo as the workspace's mono-repo tree. Uniqueness from pid + nanos
        // (no uuid dep in this crate) so parallel test threads never collide on the path.
        let root = std::env::temp_dir().join(format!(
            "nyx-ui-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let ok = nyx_core::proc_util::command("git")
                .args([
                    "-c",
                    "user.email=t@nyx",
                    "-c",
                    "user.name=nyx",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        std::fs::write(root.join("README.md"), b"nyx").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "-q", "-m", "init"]);

        // Register via the shared registration flow (create_project auto-detects the repo).
        let (_project, root_ws) = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::repository::register_project(c, "Nyx", root.to_str().unwrap(), None)
            })
            .unwrap();
        let pushes_before = h.pushes();

        // Dirty the repo with an untracked file, then read the summary via the db-command.
        std::fs::write(root.join("new.txt"), b"new").unwrap();
        let out = h
            .run(
                "workspace_git_summary",
                serde_json::json!({ "workspaceId": root_ws.id }),
            )
            .unwrap();
        let git_summary: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(git_summary["workspace_id"], serde_json::json!(root_ws.id));
        let repos = git_summary["repos"].as_array().unwrap();
        assert_eq!(repos.len(), 1, "one mono-repo summarized");
        assert_eq!(repos[0]["branch"], serde_json::json!("main"));
        assert_eq!(repos[0]["dirty"], serde_json::json!(true));
        let files: Vec<&str> = repos[0]["changed_files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        assert!(files.contains(&"new.txt"), "changed files: {files:?}");

        // A pure read: no `workspaces://changed` push.
        assert_eq!(
            h.pushes(),
            pushes_before,
            "workspace_git_summary is a pure read — no changed broadcast"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task 01KWHPV33SAV9NV1STSY403CF0): the UI bridge's `workspace_timeline`
    /// db-command serves the SAME bounded read model the MCP `workspace_timeline` tool serves —
    /// the workspace's recent `domain_events` as an oldest-first, capped fil, and NO terminal
    /// byte. Proof: seeding a project + workspace via the SAME db-commands the renderer uses
    /// lands correlated `domain_events`; the timeline db-command returns them bounded, and every
    /// entry is a business event payload (never terminal output). The read pushes NO `changed`.
    #[test]
    fn workspace_timeline_serves_the_bounded_domain_events_read_model_over_the_ui_bridge() {
        let h = Harness::new();

        // Seed via the SAME db-commands the renderer uses — each mutation persists a
        // correlated `domain_events` row (the outbox the timeline reads).
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/tl-ui-p" }),
            )
            .unwrap();
        let project_id = serde_json::from_str::<serde_json::Value>(&created).unwrap()["project"]
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
        let ws = h
            .run(
                "create_workspace",
                serde_json::json!({ "projectId": project_id, "name": "wt", "path": "/tmp/tl-ui-wt" }),
            )
            .unwrap();
        let ws_id = serde_json::from_str::<serde_json::Value>(&ws).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let pushes_before = h.pushes();

        // READ the timeline via the UI bridge db-command.
        let out = h
            .run(
                "workspace_timeline",
                serde_json::json!({ "workspaceId": ws_id }),
            )
            .unwrap();
        let timeline: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(timeline["workspace_id"], serde_json::json!(ws_id));
        let entries = timeline["entries"].as_array().unwrap();
        // The workspace-scoped `workspace.created` event surfaces, oldest-first among a small
        // chronology, and every entry is a business event payload — NEVER a terminal byte.
        let types: Vec<&str> = entries
            .iter()
            .map(|e| e["event_type"].as_str().unwrap())
            .collect();
        assert!(
            types.contains(&"workspace.created"),
            "the workspace.created event is in the timeline: {types:?}"
        );
        assert!(
            entries
                .iter()
                .all(|e| e["payload"].as_str().unwrap().contains("identity_id")),
            "every timeline entry is a business event payload, no terminal bytes"
        );
        let seqs: Vec<i64> = entries
            .iter()
            .map(|e| e["sequence"].as_i64().unwrap())
            .collect();
        assert!(
            seqs.windows(2).all(|w| w[0] < w[1]),
            "timeline is oldest-first (bounded chronology)"
        );

        // The `limit` bounds the read.
        let bounded = h
            .run(
                "workspace_timeline",
                serde_json::json!({ "workspaceId": ws_id, "limit": 1 }),
            )
            .unwrap();
        let bounded: serde_json::Value = serde_json::from_str(&bounded).unwrap();
        assert_eq!(bounded["limit"], serde_json::json!(1));
        assert_eq!(bounded["entries"].as_array().unwrap().len(), 1);

        // A pure read: no `changed` broadcast.
        assert_eq!(
            h.pushes(),
            pushes_before,
            "workspace_timeline is a pure read — no changed broadcast"
        );
    }

    // --- terminal re-hydration at restart (v2.2.5 task #7) -----------------------

    use nyx_core::vt_screen::VtScreen;

    /// Decode the `{ bytes: number[] }` result of `terminal_screen_rehydration` into raw bytes.
    fn rehydration_bytes_from_result(json: &str) -> Vec<u8> {
        let v: serde_json::Value = serde_json::from_str(json).expect("json result");
        v["bytes"]
            .as_array()
            .expect("bytes array")
            .iter()
            .map(|n| n.as_u64().expect("byte") as u8)
            .collect()
    }

    /// The `terminal_screen_rehydration` db-command returns re-hydration bytes for a terminal
    /// whose VT screen the sidecar persisted — and feeding those bytes into a fresh VT screen
    /// restores a TUI on the ALT-SCREEN (not just the scrollback). This is the end-to-end
    /// restart protocol at the sidecar seam: persist at shutdown → serve at restart.
    #[test]
    fn terminal_screen_rehydration_restores_a_persisted_tui() {
        let h = Harness::new();
        let data_dir = h.runtime.data_dir().to_path_buf();

        // Simulate the pre-restart run: a terminal ended a session in a TUI on the alt-screen.
        // We persist that screen through the sidecar's snapshot store keyed by the record id —
        // exactly what `Runtime::shutdown` does with the live PtyManager snapshots.
        let mut vt = VtScreen::new(6, 40);
        vt.feed(b"$ vim notes.txt\r\n");
        vt.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
        vt.feed(b"\x1b[1;1H  1 the quick brown fox");
        vt.feed(b"\x1b[6;1H\x1b[7m-- INSERT --\x1b[0m");
        let snap = vt.snapshot();
        {
            let mut store = super::super::snapshot_store::SnapshotStore::default();
            store.save_live(&data_dir, [("rec-tui".to_string(), snap)]);
        }

        // RESTART: a fresh runtime over the SAME data dir loads the persisted store (exactly as
        // `attach_events` does at boot).
        let db2 = Arc::clone(h.runtime.db());
        let runtime2 = Runtime::with_db_and_paths(db2, data_dir.clone(), None);
        runtime2.load_snapshots_for_test(&data_dir);
        let events = EventSink::new(Box::new(SharedBuf(Arc::new(Mutex::new(Vec::new())))));
        let result = dispatch(
            &runtime2,
            "terminal_screen_rehydration",
            &serde_json::json!({ "recordId": "rec-tui" }).to_string(),
            &events,
        )
        .expect("rehydration command");
        let bytes = rehydration_bytes_from_result(&result);
        assert!(
            !bytes.is_empty(),
            "a persisted terminal yields re-hydration bytes"
        );

        // The renderer would write these into a fresh wterm/ghostty; we prove fidelity by
        // feeding them into a fresh headless VT and asserting the alt-screen TUI is restored.
        let mut fresh = VtScreen::new(6, 40);
        fresh.feed(&bytes);
        let restored = fresh.snapshot();
        assert!(
            restored.alt_active,
            "restored terminal is back on the alt-screen (a TUI, not history)"
        );
        let alt = restored.alt.expect("alt buffer restored");
        assert!(
            (0..alt.rows).any(|r| alt.row_text(r).contains("the quick brown fox")),
            "the TUI's alt-screen content is restored"
        );
        assert!(
            (0..alt.rows).any(|r| alt.row_text(r).contains("-- INSERT --")),
            "the TUI status line is restored"
        );

        std::fs::remove_dir_all(&data_dir).ok();
    }

    /// The command is IDEMPOTENT: reading it twice returns the same bytes (a non-destructive
    /// read of a self-resetting stream), and both restore the same screen.
    #[test]
    fn terminal_screen_rehydration_is_idempotent() {
        let h = Harness::new();
        let data_dir = h.runtime.data_dir().to_path_buf();
        let mut vt = VtScreen::new(4, 30);
        vt.feed(b"\x1b[?1049h\x1b[2J\x1b[HTUI STATE\r\nrow two");
        {
            let mut store = super::super::snapshot_store::SnapshotStore::default();
            store.save_live(&data_dir, [("rec-1".to_string(), vt.snapshot())]);
        }
        h.runtime.load_snapshots_for_test(&data_dir);

        let call = || {
            rehydration_bytes_from_result(
                &h.run(
                    "terminal_screen_rehydration",
                    serde_json::json!({ "recordId": "rec-1" }),
                )
                .unwrap(),
            )
        };
        let first = call();
        let second = call();
        assert_eq!(
            first, second,
            "re-reading returns identical bytes (non-destructive)"
        );
        assert!(!first.is_empty());
        std::fs::remove_dir_all(&data_dir).ok();
    }

    /// An unknown record id (never persisted, not live) yields empty bytes — a benign
    /// "nothing to restore", not an error.
    #[test]
    fn terminal_screen_rehydration_unknown_record_is_empty() {
        let h = Harness::new();
        let result = h
            .run(
                "terminal_screen_rehydration",
                serde_json::json!({ "recordId": "does-not-exist" }),
            )
            .unwrap();
        assert!(rehydration_bytes_from_result(&result).is_empty());
    }
}
