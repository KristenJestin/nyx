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
        // A terminal RECORD mutation driven by the UI path (create/close/attach/auto-attach, and
        // the v2.5.6 label `rename`) broadcasts `terminals://changed`, SYMMETRIC to the MCP terminal
        // tools (which emit the same topic via `emit_effects`, `mcp_tools_core.rs:1447/1664`).
        // Without this the cockpit read model (`useWorkspaceRuntime`, which re-pulls on
        // `terminals://changed`) never refreshes for a UI-initiated terminal mutation — the case the
        // MCP path already covered but the UI path did not (review 01KWKH7TE86AXS41Q4VS2CEXCW). See
        // `mutates_terminals` for why `rename` now belongs here (the tree row reads `label` from the
        // snapshot).
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
    } else if mutates_review_comments(command) {
        // A review COMMENT mutation (post/reply/resolve/reopen) broadcasts `repos://changed`
        // — the SAME topic the git-watch pushes and the MCP `review_comment_post` tool emits
        // via its `Repos` effect — so an open review panel re-pulls `review_comment_list`
        // live (a human post appears without a reload, symmetric to the agent's MCP reply).
        // The only subscribers are pure readers (the comment list), so no reconcile loop.
        events.emit(Event::Changed {
            topic: "repos".to_string(),
        });
        // A FINDING mutation (promote/validate/resolve) ALSO changes the Spec's task list — a
        // finding is a `tasks kind=review` row — so it ALSO broadcasts `specs://changed` for the
        // terminal-spec OVERLAY / Spec detail (`useSpecDetail` re-pulls on `specs`, not `repos`).
        // Without it the overlay only reflected the mutation after a manual reload (review F3).
        // Subscribers are pure readers (spec detail / board), so no reconcile loop.
        if mutates_review_findings(command) {
            events.emit(Event::Changed {
                topic: "specs".to_string(),
            });
        }
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

    /// Read the v2.5.5 resume-park batch (`[{ terminalId, sessionId, command }]`) as the
    /// `(record_id, session_id, command)` tuples [`Runtime::park_resumes`](super::Runtime::park_resumes)
    /// consumes. A missing array or a malformed entry is a readable error (never a silent drop —
    /// a lost park is a lost resume). Mirrors the `ResumePark` contract shape.
    fn arg_resume_parks(&self, name: &str) -> Result<Vec<(String, String, String)>, String> {
        let arr = self
            .args
            .get(name)
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("{}: missing object[] arg '{name}'", self.command))?;
        arr.iter()
            .map(|entry| {
                let field = |key: &str| {
                    entry
                        .get(key)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| {
                            format!("{}: park entry missing string field '{key}'", self.command)
                        })
                };
                Ok((field("terminalId")?, field("sessionId")?, field("command")?))
            })
            .collect()
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

    /// Surface a state-guarded content-write refusal (v2.8 review B4): a `Forbidden` becomes a clear
    /// message (the Spec's lifecycle froze this write), an `UnknownPhase` names the plan the caller
    /// missed (K2), a raw DB error stays as [`Self::db_err`].
    fn content_write_err(e: nyx_core::db::ContentWriteError) -> String {
        match e {
            nyx_core::db::ContentWriteError::Forbidden { status, what } => {
                format!("cannot write {what} while the Spec is '{status}' — frozen by its lifecycle")
            }
            nyx_core::db::ContentWriteError::UnknownPhase {
                name,
                status,
                phases,
            } => format!(
                "phase '{name}' is not a phase of this Spec, and its plan is frozen while it is \
                 '{status}' — its phases are [{}]",
                phases.join(", ")
            ),
            nyx_core::db::ContentWriteError::Db(d) => Self::db_err(d),
        }
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

    /// Overlay the LIVE per-terminal PTY signals onto a freshly-built runtime snapshot from the
    /// sidecar's live [`PtyManager`] (v2.5.5 review):
    ///   * T1 — each terminal's OS `busy` bit (`is_busy_record` — a foreground command running);
    ///   * T3 — the PASSIVELY-DETECTED agent kind: the terminal's foreground `comm`
    ///     (`foreground_comm_record`) resolved through the PROVIDER-AGNOSTIC
    ///     [`nyx_core::agent::AgentRegistry::detect`] (no hardcoded `claude` — every adapter is
    ///     eligible), so nyx can adopt a hand-launched agent's icon WITHOUT the hook. This never
    ///     creates an `agent_sessions` row; it is display-only, distinct from `sessions[]`.
    ///
    /// Keeps `nyx_core::workspace_runtime`'s pure-DB `snapshot` free of any live-PTY dependency;
    /// only this UI-bridge path, which HOLDS the manager, applies the overlay (the MCP tool serves
    /// the DB-only snapshot).
    fn apply_terminal_live(
        &self,
        snapshot: &mut nyx_core::workspace_runtime::WorkspaceRuntimeSnapshot,
    ) {
        let pty = self.runtime.pty();
        // The provider-agnostic adapter registry, resolved ONCE for the whole overlay pass.
        let registry = nyx_core::agent::AgentRegistry::default();
        let overlay = |record_id: &str| nyx_core::workspace_runtime::TerminalLive {
            busy: pty.is_busy_record(record_id),
            // Resolve the foreground program to an agent kind via the registry — `None` for a
            // plain shell/command, so a stopped agent's passive icon disappears on the next pull.
            detected_agent_kind: pty
                .foreground_comm_record(record_id)
                .and_then(|comm| registry.detect(&comm).map(|a| a.kind().to_string())),
        };
        nyx_core::workspace_runtime::apply_terminal_live_overlay(snapshot, &overlay);
    }

    /// Build the WORKSPACE CONTEXT-RESTORE plan (task 01KWHPV33M4H5YSMGYNW9TG6J0) by REUSING
    /// the existing resume policies — the resume decision (`agent_resume::workspace_resume_plan`
    /// over `decide_resume`) and the `restart_on_startup` restore rows. No new policy, no blind
    /// relaunch. The renderer's `use-lazy-resume` drains the resumable sessions SILENTLY (gated
    /// by the project toggle + the process-liveness gate); a crash carries no special surface (the
    /// v2.5.5 interrupted/restore card was reverted in v2.5.8). Pure read; idempotent.
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

        // (crash-UI reverted in v2.5.8: no `interrupted` set — a crash is treated as a clean
        // shutdown, so the resume plan is exactly the normal toggle-gated resume verdicts +
        // restart_on_startup services, with no doubtful/interrupted surface.)
        Self::json(&serde_json::json!({
            "workspace_id": workspace_id,
            "resume_sessions": resume_sessions,
            "restart_on_startup": restart_on_startup,
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
                // The saved buffer is level with the shell again, so `read_terminal` stops
                // flagging it stale (F2). This is the ONLY place a scrollback is saved, which is
                // what makes the freshness bit exact rather than a timestamp guess.
                self.runtime.pty().mark_scrollback_persisted(&id);
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
            // v2.5.5 LAZY agent-session resume: PARK the `claude --resume <id>` commands the
            // renderer read from `workspace_restore_plan` (`{ parks: [{ terminalId, sessionId,
            // command }] }`) into the runtime's `ResumeParks`, keyed by terminal RECORD id. Each is
            // injected ONCE at that terminal's PTY spawn — or IMMEDIATELY if the PTY is already live
            // (`Runtime::park_resumes` checks liveness). This is the sidecar half the `NyxBridge`
            // `set_resume_parks` invocation drives; without it the whole lazy-resume path is inert
            // over the transport (the in-process `Runtime::park_resumes` was never reachable from the
            // wire dispatch). Void result.
            "set_resume_parks" => {
                let parks = self.arg_resume_parks("parks")?;
                self.runtime.park_resumes(&parks, self.events);
                Ok("null".to_string())
            }
            // v2.5.5 IDLE-SESSION UNLOAD: the sidecar half of the renderer's timer sweep
            // (`use-idle-unload.ts`). Reclaim the resources of DORMANT agent sessions by closing the
            // PTY of every agent-hosting terminal that is Idle (NEVER Working/Waiting — an ACTIVE
            // session is never unloaded), NOT currently `visited` (focused), idle beyond
            // `thresholdSecs`, AND resume-eligible. The authoritative safety + eligibility decision
            // lives in [`Runtime::unload_idle_sessions`](super::Runtime::unload_idle_sessions)
            // (`decide_unload` gate + the `decide_resume` master switch); this arm ONLY maps the
            // renderer's args to it. This is the transport half the `NyxBridge` `unload_idle_sessions`
            // invocation drives — WITHOUT it the sweep fell into the fallback Error and NEVER ran over
            // the wire (the in-process method was unreachable from the dispatch, exactly the class of
            // bug fixed for `set_resume_parks`). `{ thresholdSecs?, visited: string[] }` → the unloaded
            // terminal record ids (parity with the contract's documented return). The DB sessions stay
            // `active`, so an in-run revisit RESUMES each via the phase-2 lazy park+inject.
            "unload_idle_sessions" => {
                // `thresholdSecs` is OPTIONAL (contract default = `DEFAULT_UNLOAD_THRESHOLD_SECS`,
                // 15 min); `visited` is the REQUIRED list of focused terminal record ids (a readable
                // error if missing — never a silent drop, as with `arg_resume_parks`).
                let threshold = std::time::Duration::from_secs(
                    self.args
                        .get("thresholdSecs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(nyx_core::agent_unload::DEFAULT_UNLOAD_THRESHOLD_SECS),
                );
                let visited: std::collections::HashSet<String> =
                    self.arg_str_vec("visited")?.into_iter().collect();
                let unloaded = self
                    .runtime
                    .unload_idle_sessions(threshold, &visited, self.events);
                Self::json(&unloaded)
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
                let mut snapshot = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::workspace_runtime::snapshot(c, &workspace_id, &overlay)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown workspace {workspace_id}"))?;
                // Overlay the LIVE per-terminal PTY signals (the OS `busy` bit, T1) onto the
                // pure-DB snapshot so the runtime tree LINE lights a `running` dot for a
                // foreground command. This is the ONE place with the live `PtyManager`; the
                // MCP tool / tests read the DB-only snapshot unchanged.
                self.apply_terminal_live(&mut snapshot);
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
                let mut snapshot = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::workspace_runtime::snapshot_for_project(c, &project_id, &overlay)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("project {project_id} has no primary workspace"))?;
                // Overlay the LIVE per-terminal PTY signals (the OS `busy` bit, T1) — same as
                // `workspace_runtime_snapshot` above (one authority, PDR-0005).
                self.apply_terminal_live(&mut snapshot);
                Self::json(&snapshot)
            }
            // v2.2 workspace CONTEXT-RESTORE plan (task 01KWHPV33M4H5YSMGYNW9TG6J0): after a
            // restart, WHAT this workspace offers to resume — computed by REUSING the existing
            // resume policies (NO third system, Q-011). Two parts, both from live authorities:
            //   * `resume_sessions` — the per-session `decide_resume` verdict for the
            //     workspace's `active`/`unknown` sessions (via `resume_candidates_for_workspace`
            //     + `agent_resume::workspace_resume_plan`), keyed on `resume_agent_sessions`,
            //     the transcript-on-disk probe, and the target shell — the SAME policy the
            //     close-warning uses. The renderer drains these SILENTLY (lazy resume).
            //   * `restart_on_startup` — the workspace's services whose template opts into
            //     `restart_on_startup` (from `all_instances_for_restore`, the SAME rows the boot
            //     restore reads), each with whether it was running at the last shutdown.
            // (The v2.5.5 crash `interrupted` set was reverted in v2.5.8 — a crash is a clean
            // shutdown, so there is no doubtful/interrupted surface here.)
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
            // --- Spec domain READ models (v2.4 Phase 4 — the native Specs UI) ----
            // The board (`spec_list`) and the PRD detail (`spec_detail`) consume the Spec /
            // revision / task / workspace-context model built in phases 1-3 (`nyx_core::db`).
            // Pure reads over the SAME `nyx_core::db` accessors the MCP authoring tools of
            // Phase 5 will reuse (one authority for UI + MCP, PDR-0005): no mutation, no
            // `changed` broadcast. Mirrors the `workspace_runtime_snapshot` read pattern.
            //
            // `spec_list` — the board read model: every LIVE Spec of a project (via
            // `db::list_specs`, archived excluded) with its per-Spec task tally (done/total
            // over `db::list_tasks_for_spec`) so the card renders its progress bar, and the
            // project's Linear `key` so the card shows the human key `KEY-number`. `{ projectId }`.
            "spec_list" => {
                let project_id = self.arg_str("projectId")?;
                #[derive(serde::Serialize)]
                struct SpecListEntry {
                    #[serde(flatten)]
                    spec: db::Spec,
                    /// The project's Linear prefix (the card's human key is `key-number`).
                    project_key: String,
                    /// Tasks reaching `done` (the progress bar's numerator).
                    tasks_done: i64,
                    /// Total tasks of the Spec (the denominator).
                    tasks_total: i64,
                    /// v2.6 — the "bloquée par N" badge count (directional prerequisites of this
                    /// Spec). Surfaced on the board card (PRD "dépendances surfacées sur le board").
                    blocked_by_count: i64,
                    /// v2.6 — the "débloque M" badge count (directional dependents of this Spec).
                    unblocks_count: i64,
                }
                self.db()
                    .with_conn(|c| {
                        let project = db::get_project(c, &project_id)?;
                        let project_key = project.map(|p| p.key).unwrap_or_default();
                        let specs = db::list_specs(c, &project_id)?;
                        let mut entries = Vec::with_capacity(specs.len());
                        for spec in specs {
                            let tasks = db::list_tasks_for_spec(c, &spec.id)?;
                            let tasks_total = tasks.len() as i64;
                            let tasks_done = tasks
                                .iter()
                                .filter(|t| t.status == db::TASK_STATUS_DONE)
                                .count() as i64;
                            // The Spec↔Spec dependency badge counts (SAME authority as the detail's
                            // `spec_dependency_view`, so board + detail agree). One extra bounded
                            // read per Spec — parity with the per-Spec task tally above.
                            let deps = db::spec_dependency_view(c, &spec.id)?;
                            entries.push(SpecListEntry {
                                spec,
                                project_key: project_key.clone(),
                                tasks_done,
                                tasks_total,
                                blocked_by_count: deps.blocked_by_count,
                                unblocks_count: deps.unblocks_count,
                            });
                        }
                        Ok::<_, diesel::result::Error>(entries)
                    })
                    .map_err(Self::db_err)
                    .and_then(|entries| Self::json(&entries))
            }
            // `spec_detail` — the PRD-detail read model: the Spec, its single ACTIVE revision
            // (the six structured sections), its tasks + the dependency edges, and the DERIVED
            // workspace context (workspace → project → repos, via
            // `db::spec_workspace_context`, `None` until the Spec is anchored). This is what
            // the detail's spec / tasks / activity / runtime tabs consume. `{ specId }`.
            "spec_detail" => {
                let spec_id = self.arg_str("specId")?;
                #[derive(serde::Serialize)]
                struct SpecDetail {
                    spec: db::Spec,
                    /// The single active revision (the editable one). Always present for a
                    /// known Spec (the `create_spec` invariant); `None` only for an unknown id
                    /// (handled by the `ok_or_else` below).
                    revision: db::SpecRevision,
                    tasks: Vec<db::Task>,
                    dependencies: Vec<db::TaskDependency>,
                    /// Workspace → project → repos, DERIVED from the anchor. `None` until the
                    /// Spec is posed on a dev workspace (a sane state, not an error).
                    workspace_context: Option<db::SpecWorkspaceContext>,
                }
                self.db()
                    .with_conn(|c| {
                        let Some(spec) = db::get_spec(c, &spec_id)? else {
                            return Ok(None);
                        };
                        let Some(revision) = db::get_active_revision(c, &spec_id)? else {
                            return Ok(None);
                        };
                        let tasks = db::list_tasks_for_spec(c, &spec_id)?;
                        // The dependency EDGES of this Spec's tasks (forward direction), so
                        // the tasks tab can render "blocked by" without N per-task round-trips.
                        let mut dependencies = Vec::new();
                        for task in &tasks {
                            dependencies.extend(db::list_task_dependency_edges(c, &task.id)?);
                        }
                        let workspace_context = db::spec_workspace_context(c, &spec_id)?;
                        Ok::<_, diesel::result::Error>(Some(SpecDetail {
                            spec,
                            revision,
                            tasks,
                            dependencies,
                            workspace_context,
                        }))
                    })
                    .map_err(Self::db_err)?
                    .map_or_else(
                        || Err(format!("unknown spec {spec_id}")),
                        |detail| Self::json(&detail),
                    )
            }
            // `spec_activity` — the PRD-detail ACTIVITY read model: a BOUNDED, oldest-first
            // fil of the Spec's most recent `spec`/`task` `domain_events` (its own lifecycle:
            // creation, section edits, transitions; plus its tasks' add/transition/dependency
            // events). The SAME `nyx_core::spec_activity::activity` an MCP tool serves (PDR-0005:
            // one authority for UI + MCP), mirroring `workspace_timeline`. `limit` bounds the
            // read (omitted / 0 → the read model's default ceiling). A pure read: no mutation,
            // no `changed` broadcast; the live refresh rides the existing unsolicited channel.
            "spec_activity" => {
                let spec_id = self.arg_str("specId")?;
                let limit = self.args.get("limit").and_then(|v| v.as_i64()).unwrap_or(0);
                let activity = self
                    .db()
                    .with_conn(|c| nyx_core::spec_activity::activity(c, &spec_id, limit))
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown spec {spec_id}"))?;
                Self::json(&activity)
            }
            // The v2.4 Git DIFF read model (task 01KX0F6FD836YASVYT17YV3FTW, PDR-0007/0015): the
            // line-level diff of ONE repository within a workspace for a review surface
            // (working_tree/staged/unstaged/untracked/commit/range/branches), LIVING or PINNED —
            // the EXACT same `nyx_core::git_diff` engine the MCP `repo_diff` tool serves (PDR-0005:
            // one authority for UI + MCP). gix primary, git CLI fallback. The (workspace,repository)
            // pair resolves to ONE repo's absolute path so the diff never mixes Git identities. A
            // pure read: no mutation, no `changed` broadcast, nothing persisted (PDR-0013).
            "repo_diff" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let kind = self.arg_str("kind")?;
                let rev = self.arg_opt_str("rev");
                let base = self.arg_opt_str("base");
                let target = self.arg_opt_str("target");
                let mode = self.arg_opt_str("mode");

                let diff_target = nyx_core::git_diff::target_from_params(
                    &kind,
                    rev.as_deref(),
                    base.as_deref(),
                    target.as_deref(),
                )?;
                let mode_override = nyx_core::git_diff::mode_from_str(mode.as_deref())?;

                let absolute_path = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::repository::member_absolute_path(c, &workspace_id, &repository_id)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| {
                        format!("no repository {repository_id} in workspace {workspace_id}")
                    })?;

                let effective_mode = mode_override.unwrap_or_else(|| diff_target.default_mode());
                let diff = nyx_core::git_diff::diff_repo_with_mode(
                    &absolute_path,
                    &diff_target,
                    effective_mode,
                )
                .map_err(|e| e.to_string())?;
                Self::json(&diff)
            }
            // The v2.4 DERIVED GIT VIEWS read model (task 01KX0F6FXE6AJ9BR4BYHRX6B1R, Q-015 /
            // PDR-0013): the per-repo dirty state (staged/modified/untracked), current branch +
            // base/target, ahead/behind, and change summary — RE-READ from Git on demand — the
            // EXACT same `nyx_core::git_views::repo_view_with_target` the MCP `repo_git_view` tool
            // serves (PDR-0005). The (workspace,repository) pair resolves to ONE repo's absolute
            // path (no identity mixing). A pure read: no mutation, no broadcast, nothing persisted.
            "repo_git_view" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let base = self.arg_opt_str("base");
                let target = self.arg_opt_str("target");

                let absolute_path = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::repository::member_absolute_path(c, &workspace_id, &repository_id)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| {
                        format!("no repository {repository_id} in workspace {workspace_id}")
                    })?;

                let view = nyx_core::git_views::repo_view_with_target(
                    &absolute_path,
                    base.as_deref(),
                    target.as_deref(),
                );
                Self::json(&view)
            }
            // The v2.4 REAL-TIME REFRESH registration (task 01KX0F6GDZEH3YN6QJDB7FC0WC, Q-015):
            // begin watching a `(workspace, repository)` so a work-tree/ref change (edit /
            // commit / checkout) recomputes the derived view and PUSHES `repos://changed` over
            // the SAME v2.1 pipe the domain events ride — no polling, debounced. A review surface
            // calls this as it opens; `repo_unwatch` releases it as it closes. The (workspace,
            // repository) pair resolves to ONE repo's absolute path (no identity mixing). Returns
            // null; the ongoing effect is the pushed refreshes, not this call's result. A missing
            // git-watch runtime (a bare pre-`attach_events` state) or an unresolved repo is a
            // readable error.
            "repo_watch" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let base = self.arg_opt_str("base");
                let target = self.arg_opt_str("target");

                let absolute_path = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::repository::member_absolute_path(c, &workspace_id, &repository_id)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| {
                        format!("no repository {repository_id} in workspace {workspace_id}")
                    })?;

                let git_watch = self
                    .runtime
                    .git_watch()
                    .ok_or_else(|| "git-watch runtime not started".to_string())?;
                git_watch.watch(&workspace_id, &repository_id, &absolute_path, base, target);
                Ok("null".to_string())
            }
            // Stop the real-time watch of a `(workspace, repository)` (releases its OS watches).
            // A pair that was never watched is a no-op success. Returns null.
            "repo_unwatch" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let git_watch = self
                    .runtime
                    .git_watch()
                    .ok_or_else(|| "git-watch runtime not started".to_string())?;
                git_watch.unwatch(&workspace_id, &repository_id);
                Ok("null".to_string())
            }
            // --- v2.4 review COMMENTS (task 01KX0F6HEBPK34CV49EREHK3Y9, PDR-0012/0015) ------
            // The ONLY persisted review artifact: a single auto-threaded `review_comments`
            // table (review = vue dérivée, no thread/message/finding entity). The UI façade
            // owns list/post/reply/resolve; the MCP agent façade owns list + post/reply but
            // NOT resolve (resolution is human-only). Both share `nyx_core::db` (PDR-0005). A
            // mutation broadcasts `repos://changed` so an open review re-pulls the feed live.
            //
            // LIST the auto-threaded feed of a `(workspace, repository)` — roots + replies,
            // anchored + global, with each comment's outdated/status. Read-only.
            "review_comment_list" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                // SPEC SCOPING (Finding 41): the review is opened FROM a Spec, so the UI passes
                // that Spec's id and the feed returns ONLY its comments — comments no longer leak
                // onto other Specs sharing the `(workspace, repository)`. A missing `specId`
                // (unscoped read) returns the whole repo feed.
                let spec_id = self.arg_opt_str("specId");
                let comments = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::db::list_review_comments(
                            c,
                            &workspace_id,
                            &repository_id,
                            spec_id.as_deref(),
                        )
                    })
                    .map_err(Self::db_err)?;
                Self::json(&comments)
            }
            // POST a HUMAN comment (a new thread root, an anchored comment, OR a reply). The
            // UI is the human façade, so `authorType` defaults to `human` (an explicit
            // `authorType` is honored for parity with the shared model). An anchor is captured
            // when a `filePath` is given; `lineEnd` defaults to `lineStart`. Returns the row.
            "review_comment_post" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let body = self.arg_str("body")?;
                let spec_id = self.arg_opt_str("specId");
                let parent_id = self.arg_opt_str("parentId");
                let author_type = self
                    .arg_opt_str("authorType")
                    .unwrap_or_else(|| nyx_core::db::AUTHOR_HUMAN.to_string());
                let author_name = self
                    .arg_opt_str("authorName")
                    .unwrap_or_else(|| "You".to_string());
                let file_path = self.arg_opt_str("filePath");
                let anchor = file_path.map(|fp| {
                    let start = self
                        .args
                        .get("lineStart")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(1) as i32;
                    let end = self
                        .args
                        .get("lineEnd")
                        .and_then(|v| v.as_i64())
                        .map(|e| e as i32)
                        .unwrap_or(start)
                        .max(start);
                    nyx_core::db::CommentAnchor {
                        file_path: fp,
                        side: self
                            .arg_opt_str("side")
                            .unwrap_or_else(|| nyx_core::db::SIDE_NEW.to_string()),
                        line_start: start,
                        line_end: end,
                        blob: self.arg_opt_str("blob"),
                        snippet: self.arg_opt_str("snippet"),
                        context: self.arg_opt_str("context"),
                    }
                });
                let comment = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::db::insert_review_comment(
                            c,
                            nyx_core::db::NewComment {
                                workspace_id: workspace_id.clone(),
                                repository_id: repository_id.clone(),
                                spec_id: spec_id.clone(),
                                parent_id: parent_id.clone(),
                                author_type: author_type.clone(),
                                author_name: author_name.clone(),
                                anchor: anchor.clone(),
                                body: body.clone(),
                            },
                        )
                    })
                    .map_err(Self::content_write_err)?;
                Self::json(&comment)
            }
            // RESOLVE a comment — the HUMAN-ONLY transition (stamps resolved_by_type=human),
            // GUARDED to a comment whose linked findings are all settled (v2.5.11 Phase 2: you
            // resolve in knowledge of the fix). A comment with pending `tasks kind=review` findings
            // is refused; one with none resolves freely. Reachable only from this UI façade; the MCP
            // agent façade advertises no resolve tool. Returns the updated row.
            "review_comment_resolve" => {
                let id = self.arg_str("id")?;
                let comment = self
                    .db()
                    .with_conn(|c| nyx_core::db::resolve_review_comment_when_findings_done(c, &id))
                    .map_err(|e| match e {
                        nyx_core::db::ResolveCommentError::NotFound => {
                            format!("unknown review comment {id}")
                        }
                        nyx_core::db::ResolveCommentError::FindingsNotDone { pending } => format!(
                            "cannot resolve comment {id}: {pending} linked finding(s) still open"
                        ),
                        nyx_core::db::ResolveCommentError::Db(e) => Self::db_err(e),
                    })?;
                Self::json(&comment)
            }
            // RE-OPEN a resolved comment (the human's undo). Human-only, same façade.
            "review_comment_reopen" => {
                let id = self.arg_str("id")?;
                let comment = self
                    .db()
                    .with_conn(|c| nyx_core::db::reopen_review_comment(c, &id))
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown review comment {id}"))?;
                Self::json(&comment)
            }
            // --- v2.4 review TASKS (= task) + unread (task 01KX0F6JEKCEV2JCTMEBMM3P4Z) --------
            // The promotion "grill" (collapsed to two states, review follow-up #31): the UI façade
            // owns the human's PROMOTE step and the read (LIST); the MCP agent façade owns
            // STRUCTURE + LIST — a structured finding is coder-ready, there is no `validated` step.
            // A mutation broadcasts `repos://changed` so an open review re-pulls the task list +
            // unread badge live.
            //
            // PROMOTE a comment into a FINDING — the grill's FIRST step (the human lâche brut).
            // Creates a `proposed` `tasks kind=review` finding linked to the comment (v2.5.11
            // cutover — NOT a `review_tasks` row); severity OPTIONAL. Returns it in the viewer wire
            // shape. The finding lives on the comment's Spec (a comment with no Spec cannot promote).
            "review_task_promote" => {
                let comment_id = self.arg_str("commentId")?;
                let title = self.arg_opt_str("title");
                let severity = self.arg_opt_str("severity");
                let finding = self
                    .db()
                    .with_conn(|c| -> diesel::QueryResult<nyx_core::db::ReviewFinding> {
                        let comment = nyx_core::db::get_review_comment(c, &comment_id)?
                            .ok_or(diesel::result::Error::NotFound)?;
                        // Seed the title from the comment body when the human gave none.
                        let seed = title.clone().unwrap_or_else(|| comment.body.clone());
                        let task = nyx_core::db::promote_comment_to_finding(
                            c,
                            &comment,
                            &seed,
                            severity.as_deref(),
                        )?;
                        Ok(nyx_core::db::ReviewFinding::from_task(
                            &task,
                            &comment.workspace_id,
                            &comment.repository_id,
                        ))
                    })
                    .map_err(Self::db_err)?;
                Self::json(&finding)
            }
            // LIST the GRILLED review findings of a Spec — the `tasks kind=review` rows carrying a
            // `grill_state` (v2.5.11 cutover — NOT `review_tasks`). Findings are Spec-scoped, so the
            // UI passes the Spec the review is opened from; each is projected into the viewer wire
            // shape, injecting the read's `(workspace, repository)`. Omitting `specId` returns an
            // empty list (there is no repo-wide finding bucket once findings are tasks). Read-only.
            "review_task_list" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let spec_id = self.arg_opt_str("specId");
                let findings: Vec<nyx_core::db::ReviewFinding> = match spec_id {
                    Some(sid) => self
                        .db()
                        .with_conn(|c| nyx_core::db::list_grilled_findings_for_spec(c, &sid))
                        .map_err(Self::db_err)?
                        .iter()
                        .map(|t| {
                            nyx_core::db::ReviewFinding::from_task(t, &workspace_id, &repository_id)
                        })
                        .collect(),
                    None => Vec::new(),
                };
                Self::json(&findings)
            }
            // The UNREAD count of a `(workspace, repository)` review feed (comments created after
            // the read watermark). Read-only. Backs the live non-lus badge (re-pulled on
            // `repos://changed`).
            "review_unread" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let unread = self
                    .db()
                    .with_conn(|c| nyx_core::db::review_unread(c, &workspace_id, &repository_id))
                    .map_err(Self::db_err)?;
                Self::json(&unread)
            }
            // MARK-READ the review feed of a `(workspace, repository)` — stamp the read watermark
            // to now (unread → 0). Mutating → broadcasts `repos://changed`. Returns the zeroed
            // unread state.
            "review_mark_read" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let unread = self
                    .db()
                    .with_conn(|c| nyx_core::db::mark_review_read(c, &workspace_id, &repository_id))
                    .map_err(Self::db_err)?;
                Self::json(&unread)
            }
            // RE-MAP the anchored comments of a `(workspace, repository)` against the repo's
            // CURRENT HEAD — the phase-4 `nyx_core::review_comment::remap_repo_comments` engine
            // (task 01KX0F6HEBPK34CV49EREHK3Y9, PDR-0012). Each open anchored ROOT is re-anchored:
            // one whose snippet MOVED follows the code (its `line_start`/`blob` refresh), one whose
            // snippet VANISHED flips `outdated`. This is the LIVING review's "l'ancre suit après un
            // nouveau commit" step: the review page invokes it after a `repos://changed` push (the
            // repo advanced), so the moving diff and its anchors stay coherent. A mutation → the
            // legacy broadcast below emits `repos://changed`, so the open feed re-pulls the
            // re-mapped/outdated anchors live. The (workspace,repository) resolves to ONE repo's
            // absolute path (no identity mixing). Returns `{ written }` (rows re-anchored/outdated).
            "review_remap" => {
                let workspace_id = self.arg_str("workspaceId")?;
                let repository_id = self.arg_str("repositoryId")?;
                let absolute_path = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::repository::member_absolute_path(c, &workspace_id, &repository_id)
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| {
                        format!("no repository {repository_id} in workspace {workspace_id}")
                    })?;
                let written = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::review_comment::remap_repo_comments(
                            c,
                            &absolute_path,
                            &workspace_id,
                            &repository_id,
                        )
                    })
                    .map_err(Self::db_err)?;
                Self::json(&serde_json::json!({ "written": written }))
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
            // The v2.5.10 terminal↔Spec OVERLAY resolution (task 01KXN0HVF8T0CKB6C575RECTXG):
            // `{ terminalId }` → the Specs a terminal's ACTIVE session is working (its
            // `agent_session_specs` set) + a summary of that session for the overlay's liveness
            // axis. NO active session → `{ session: null, specs: [] }` (no session ⇒ no overlay).
            // Pure read over the SHARED `nyx_core::db` resolvers (`specs_for_terminal` = terminal
            // → active session → its set). The live refresh rides the EXISTING unsolicited
            // channels with NO polling: the auto-map/un-map + any spec/task mutation push
            // `specs://changed`, and the session lifecycle pushes `agent-sessions://changed`, so
            // an open overlay re-pulls this command on either topic. `{ terminalId }`.
            "terminal_specs" => {
                let terminal_id = self.arg_str("terminalId")?;
                /// The overlay resolution result: the ACTIVE session's Spec set + a session
                /// summary (its `state` is the liveness axis; `agent_kind` the provider).
                #[derive(serde::Serialize)]
                struct TerminalSpecs {
                    /// The terminal's ACTIVE session, or `None` (no session ⇒ no overlay).
                    session: Option<TerminalSpecsSession>,
                    /// The Specs of that session's set (empty when no session / empty set).
                    specs: Vec<db::Spec>,
                }
                #[derive(serde::Serialize)]
                struct TerminalSpecsSession {
                    id: String,
                    agent_kind: String,
                    state: String,
                }
                self.db()
                    .with_conn(|c| {
                        let Some(session) = db::active_session_for_terminal(c, &terminal_id)?
                        else {
                            return Ok::<_, diesel::result::Error>(TerminalSpecs {
                                session: None,
                                specs: Vec::new(),
                            });
                        };
                        let specs = db::specs_for_session(c, &session.id)?;
                        Ok(TerminalSpecs {
                            session: Some(TerminalSpecsSession {
                                id: session.id,
                                agent_kind: session.agent_kind,
                                state: session.state,
                            }),
                            specs,
                        })
                    })
                    .map_err(Self::db_err)
                    .and_then(|out| Self::json(&out))
            }
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
            // (crash-UI reverted in v2.5.8: the `recovery_snapshot` db-command is gone — a crash
            // is treated as a clean shutdown, so there is no interrupted set to surface.)
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
            // --- v2.6 KNOWLEDGE READ models (Phase 4 — the Idées view + the Spec-detail
            //     annexes/dependency badges). Pure reads over the SAME `nyx_core::db` / `nyx_core::idea`
            //     accessors the Phase-1/3 MCP tools call (one authority for UI + MCP, PDR-0005): no
            //     mutation, no `changed` broadcast. Mirror the `spec_list`/`spec_detail` read pattern.
            //
            // `idea_themes` — the by-THEME grouping of a project's backlog: one row per theme
            // (ungrouped ideas fold into a single `theme = null` group), each with the total count
            // and the per-status breakdown (`open`/`planned`/`implemented`/`dropped`), biggest group
            // first. Backs the Idées group headers (counter + status mini-dots) + the theme facet.
            // `{ projectId }`.
            "idea_themes" => {
                let project_id = self.arg_str("projectId")?;
                // The renderer paints the FULL per-status breakdown (mini-dots) + filters
                // client-side, so the UI read spans EVERY status (`include_terminal = true`). The
                // active-only default is the MCP `idea_themes` tool's, not this UI read.
                self.db()
                    .with_conn(|c| db::list_idea_themes(c, &project_id, true))
                    .map_err(Self::db_err)
                    .and_then(|themes| Self::json(&themes))
            }
            // `idea_list` — the BOUNDED backlog rows for the Idées table, newest-first, each ENRICHED
            // with its linked Specs (so a row shows its Spec badges AND the detail drawer renders
            // "corps + Specs liées + motif de fermeture" from the same item, no second round-trip).
            // Composes `db::list_ideas` + `db::linked_specs_for_idea` per row — the SAME composition
            // pattern as `spec_list` (list + per-item tally). Always capped (never Depot's unbounded
            // dump). `{ projectId, status?, theme? }`.
            "idea_list" => {
                let project_id = self.arg_str("projectId")?;
                let status = self.arg_opt_str("status");
                let theme = self.arg_opt_str("theme");
                /// The BOUNDED cap on the browse list — generous for a real backlog while it can
                /// never re-create the flat unbounded list Depot drowned in (parity with the MCP
                /// `idea_list` LIST_MAX_LIMIT).
                const IDEA_LIST_CAP: i64 = 200;
                #[derive(serde::Serialize)]
                struct IdeaListItem {
                    #[serde(flatten)]
                    idea: db::Idea,
                    /// The idée↔Spec links, each with the Spec's title/status/number.
                    linked_specs: Vec<db::LinkedSpec>,
                }
                // The renderer paints its own status filter client-side (default "Active" =
                // open+planned), so the UI list fetches EVERY status by default (`All`) and narrows
                // only when a specific `status` arg is passed. The active-only default lives in the
                // MCP `idea_list` tool, not this UI read.
                let status_filter = match status.as_deref() {
                    Some(s) => db::IdeaStatusFilter::One(s),
                    None => db::IdeaStatusFilter::All,
                };
                self.db()
                    .with_conn(|c| {
                        let ideas = db::list_ideas(
                            c,
                            &project_id,
                            status_filter,
                            theme.as_deref(),
                            IDEA_LIST_CAP,
                            0,
                        )?;
                        let mut items = Vec::with_capacity(ideas.len());
                        for idea in ideas {
                            let linked_specs = db::linked_specs_for_idea(c, &idea.id)?;
                            items.push(IdeaListItem { idea, linked_specs });
                        }
                        Ok::<_, diesel::result::Error>(items)
                    })
                    .map_err(Self::db_err)
                    .and_then(|items| Self::json(&items))
            }
            // `idea_search` — the FTS5 + fuzzy scored search over a project's ideas (the SAME
            // `nyx_core::idea::search_ideas` the MCP `idea_search` tool serves). Each hit carries a
            // match extract, the blended score, and the idea's linked Specs. `theme`/`status` narrow
            // the candidate set; `limit` is clamped by the engine. `{ projectId, query, theme?,
            // status?, limit? }`.
            "idea_search" => {
                let project_id = self.arg_str("projectId")?;
                let query = self.arg_str("query")?;
                let theme = self.arg_opt_str("theme");
                let status = self.arg_opt_str("status");
                let limit = self
                    .args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(nyx_core::idea::DEFAULT_SEARCH_LIMIT);
                self.db()
                    .with_conn(|c| {
                        nyx_core::idea::search_ideas(
                            c,
                            &project_id,
                            &query,
                            theme.as_deref(),
                            status.as_deref(),
                            limit,
                        )
                    })
                    .map_err(Self::db_err)
                    .and_then(|results| Self::json(&results))
            }
            // `annexe_list` — the annexes attached to a Spec (the Spec-detail "Annexes" block),
            // newest-first, each with its blob `byte_len` (no content shipped in the list). The SAME
            // `db::list_annexes` the MCP `annexe_list` tool serves. `{ specId }`.
            "annexe_list" => {
                let spec_id = self.arg_str("specId")?;
                self.db()
                    .with_conn(|c| db::list_annexes(c, &spec_id))
                    .map_err(Self::db_err)
                    .and_then(|annexes| Self::json(&annexes))
            }
            // `spec_dependency_view` — a Spec's full dependency view: the directional `blocked_by` /
            // `unblocks` edges (with their computable badge counts) + the `related` context links.
            // Backs the Spec-detail dependency badges ("bloquée par N" / "débloque M"). The SAME
            // `db::spec_dependency_view` the MCP `spec_link`/state read serves. `{ specId }`.
            "spec_dependency_view" => {
                let spec_id = self.arg_str("specId")?;
                self.db()
                    .with_conn(|c| db::spec_dependency_view(c, &spec_id))
                    .map_err(Self::db_err)
                    .and_then(|view| Self::json(&view))
            }
            // ================================================================
            // v2.8 ORCHESTRATION ENGINE — the read models the React Flow graph view consumes
            // (Phase 6, task 01KY71J481RV2P108YP933RMV0). All pure reads over the SAME
            // `nyx_core::db` engine accessors the Phase-2 interpreter + Phase-4 authoring use
            // (one authority for UI + MCP, PDR-0005) — no second data path, nothing persisted.
            // ================================================================

            // `engine_workflow_list` — the workflows the graph picker offers: the BUILT-IN defaults
            // (`project_id` IS NULL — the seeded ndev/nspec/ndoc) always, PLUS the active project's
            // OWN (forked/custom) workflows when `projectId` is given. Newest-first within each scope,
            // project workflows first. `{ projectId? }` → `Vec<db::Workflow>` (each row carries its
            // `project_id`, so the UI groups built-in vs project).
            "engine_workflow_list" => {
                let project_id = self.arg_opt_str("projectId");
                self.db()
                    .with_conn(|c| {
                        let mut rows = Vec::new();
                        if let Some(p) = project_id.as_deref() {
                            rows.extend(db::list_workflows(c, Some(p))?);
                        }
                        rows.extend(db::list_workflows(c, None)?);
                        Ok::<_, diesel::result::Error>(rows)
                    })
                    .map_err(Self::db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            // `engine_workflow_graph` — ONE workflow + ALL its nodes (the DETERMINISTIC full dump the
            // renderer reconstructs the PURE tree from — `parent_id` + `order_index`, mirroring
            // `engine::Tree::from_rows`). `{ workflowId }` → `{ workflow, nodes }`, or an error for an
            // unknown id (the UI shows an empty-state).
            "engine_workflow_graph" => {
                let workflow_id = self.arg_str("workflowId")?;
                #[derive(serde::Serialize)]
                struct WorkflowGraph {
                    workflow: db::Workflow,
                    nodes: Vec<db::Node>,
                }
                self.db()
                    .with_conn(|c| {
                        let Some(workflow) = db::get_workflow(c, &workflow_id)? else {
                            return Ok(None);
                        };
                        let nodes = db::list_nodes(c, &workflow_id)?;
                        Ok::<_, diesel::result::Error>(Some(WorkflowGraph { workflow, nodes }))
                    })
                    .map_err(Self::db_err)?
                    .map_or_else(
                        || {
                            Err(format!(
                                "engine_workflow_graph: unknown workflow {workflow_id}"
                            ))
                        },
                        |graph| Self::json(&graph),
                    )
            }
            // `engine_run_list` — the runs of a workflow, newest-first (the RUNTIME view reads the
            // latest to surface `current_node_id` + the `blackboard` JSON). `{ workflowId }` →
            // `Vec<db::WorkflowRun>`.
            "engine_run_list" => {
                let workflow_id = self.arg_str("workflowId")?;
                self.db()
                    .with_conn(|c| db::list_workflow_runs(c, &workflow_id))
                    .map_err(Self::db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            // `engine_spec_runs` — the runs bound to ONE SPEC, across EVERY workflow, newest-first.
            // The Spec-side RUNTIME view (the run modal opened from the Spec detail / the dev
            // overlay) reads it: it starts from a Spec, not from a workflow, and a Spec is driven by
            // its ndev run PLUS the child-workflow runs ndev spawns. `{ specId }` →
            // `Vec<db::WorkflowRun>` (empty for an unknown Spec — the modal shows its empty state).
            "engine_spec_runs" => {
                let spec_id = self.arg_str("specId")?;
                self.db()
                    .with_conn(|c| db::list_runs_for_spec(c, &spec_id))
                    .map_err(Self::db_err)
                    .and_then(|rows| Self::json(&rows))
            }
            // `engine_effector_context` — the CONTEXT the node inspector shows when a `spawn_subagent`
            // node is clicked, keyed by the spawn node's `role` (its `agent_id` slug). `specId` is
            // OPTIONAL (review finding #35): with a run bound, `{ role, specId }` renders the LIVE
            // `render_agent_context` (role template + `{{rules}}` resolved by scope + lineage); WITHOUT
            // a run, `{ role }` renders the STATIC role template (the sub-agent's brief) whose
            // run-specific sections are marked "resolved at run" — so a built-in with no run shows its
            // brief instead of an empty inspector. `{ context }` (null only when a bound Spec is
            // unknown). An unknown role IS an error (the UI falls back to the node config).
            //
            // `subject` is the spawn node's own `subject` (F6): passing it shows the inspector the
            // SAME narrowed manual the run forwards — the Spec section that spawn is about, not all
            // six. Omitted = the whole Spec, as before.
            "engine_effector_context" => {
                let role = self.arg_str("role")?;
                let spec_id = self.arg_opt_str("specId");
                let subject = self.arg_opt_str("subject");
                let context = nyx_core::mcp_context::render_effector_context(
                    self.db(),
                    &role,
                    spec_id.as_deref(),
                    subject.as_deref(),
                )
                .map_err(|e| format!("{}: {}", self.command, e.message))?;
                Self::json(&serde_json::json!({ "context": context }))
            }
            // --- DESIGN-SYSTEM read/render seam (v2.7 Phase 3, PDR-0010) -------------
            // The native design-system UI (iframe renderer / catalogue / composant detail) reads
            // the `design` + `design_components` authority (Phase 1) and the composition renderer
            // (`nyx_core::render`, Phase 3 task #6). These are PURE reads/renders + a per-node param
            // persistence: no agent ACTION lives here (the agent drives the proto via the MCP
            // `proto_*` tools, never a UI button — the "actions via MCP" seam of PDR-0009).
            //
            // `design_get` — a project's design-system row: the derived `theme_tokens_json` (the
            // iframe's `@theme`) + the DESIGN.md `content`, or `null` when the project has none yet.
            // `{ projectId }`. A pure read (no `changed` broadcast).
            "design_get" => {
                let project_id = self.arg_str("projectId")?;
                self.db()
                    .with_conn(|c| db::get_design(c, &project_id))
                    .map_err(Self::db_err)
                    .and_then(|design| Self::json(&design))
            }
            // --- THEME TOKENS: the layer that stops the preview rendering grey (review #42) ------
            //
            // A synthesized component carries Tailwind CLASSES (`bg-primary`); those classes are
            // GREY until the project's tokens exist, because that is what they resolve against.
            // Nothing ever wrote `theme_tokens_json`, so the iframe's `@theme` was always empty.
            // These two commands are the UI twins of the `design_tokens` MCP tool, over the SAME
            // `nyx_core::design_tokens` authority (PDR-0005: one authority for UI + MCP).
            //
            // `design_tokens_ingest` — DERIVE the layer from the app: walk the project's repos (the
            // same resolution `design_inventory` uses), rank the stylesheets that look like the
            // design system, merge their GLOBAL custom properties. The layer is REFERENCED, not
            // copied: it records the source sheets and lives once per project, so `primary` keeps
            // ONE identity across every component and preview. An ingest that detects nothing
            // leaves a hand-authored layer untouched — it records only WHAT IT REFUSED.
            // `{ projectId }` -> the run summary. Emits `prototypes://changed` only when it really
            // moved.
            "design_tokens_ingest" => {
                let project_id = self.arg_str("projectId")?;
                let run = self
                    .db()
                    .with_conn(|c| nyx_core::design_tokens::ingest_project_tokens(c, &project_id))
                    .map_err(Self::db_err)?;
                // EITHER half is news for the panel: `changed` says the TOKENS moved,
                // `provenance_changed` says the run recorded what it REFUSED — which is all a run
                // that retained no sheet writes, and the only diagnostic a grey catalogue has
                // (finding `01KYSNVW31C0E8YQPET0505HW2`). The panel cannot re-run an ingest itself,
                // so it has to be woken for the second case too.
                if run.changed || run.provenance_changed {
                    self.events.emit(Event::Changed {
                        topic: "prototypes".to_string(),
                    });
                }
                Self::json(&run)
            }
            // `design_tokens_set` — EDIT the layer in place: `variables` upserts `--name -> value`
            // pairs, `remove` drops names, `dark: true` targets the dark map. Editing ONE token
            // re-themes every component at once (that is the point of a referenced layer). The
            // `sources[]` reference back to the app's stylesheet is preserved, so a later ingest can
            // re-derive from upstream. `{ projectId, variables?, remove?, dark? }` -> the stored
            // layer. Emits `prototypes://changed`.
            "design_tokens_set" => {
                let project_id = self.arg_str("projectId")?;
                let dark = self.arg_opt_bool("dark").unwrap_or(false);
                let mut set: std::collections::BTreeMap<String, String> =
                    std::collections::BTreeMap::new();
                if let Some(map) = self.args.get("variables").and_then(|v| v.as_object()) {
                    for (name, value) in map {
                        let Some(text) = value.as_str() else {
                            return Err(format!(
                                "{}: token '{name}' must be a CSS value string",
                                self.command
                            ));
                        };
                        set.insert(
                            nyx_core::design_tokens::normalize_token_name(name),
                            text.to_string(),
                        );
                    }
                }
                let remove: Vec<String> = self
                    .args
                    .get("remove")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(nyx_core::design_tokens::normalize_token_name)
                            .collect()
                    })
                    .unwrap_or_default();
                let layer = self
                    .db()
                    .with_conn(|c| {
                        nyx_core::design_tokens::edit_project_tokens(
                            c,
                            &project_id,
                            &set,
                            &remove,
                            dark,
                        )
                    })
                    .map_err(Self::db_err)?;
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&serde_json::json!({
                    "tokens_json": layer.to_json(),
                    "variable_count": layer.variables.len(),
                    "dark_count": layer.dark.len(),
                }))
            }
            // `design_component_list` — the CATALOGUE read model: every `design_components` row of
            // a project (DS + NEW, name-ordered) PLUS the set of component names the project's proto
            // compositions USE (the "liés-page" filter). Portée PROJET (global), never scoped to one
            // page. `{ projectId }` -> `{ components, linked_names }`. A pure read.
            //
            // "Uses" is TWO things since `data-nyx-use` exists (finding
            // `01KYSZPG9Y8QGGVED208SG5BTB`): the components a composition TREE places, and the ones
            // their TEMPLATES reference, transitively. A shared `Spinner` is typically reached only
            // the second way — exactly the case the reference capability was asked for — and reading
            // the trees alone showed it with no `linked` badge, i.e. as a legitimate deletion
            // candidate while it is loaded on every page. The graph is built ONCE per call
            // (`ReferenceIndex`: one scan per template, then pure edge-walking), never per card.
            "design_component_list" => {
                let project_id = self.arg_str("projectId")?;
                #[derive(serde::Serialize)]
                struct ComponentCatalog {
                    components: Vec<db::DesignComponent>,
                    /// Names used by at least one proto composition of the project (liés-page) —
                    /// placed in a tree, or reached from one through `data-nyx-use`.
                    linked_names: Vec<String>,
                }
                self.db()
                    .with_conn(|c| {
                        let components = db::list_components(c, &project_id)?;
                        // Union the component names referenced across every proto composition of the
                        // project (spec -> prototype -> page -> version -> variant.composition_json).
                        let mut linked: Vec<String> = Vec::new();
                        for spec in db::list_specs(c, &project_id)? {
                            let Some(proto) = db::get_prototype_for_spec(c, &spec.id)? else {
                                continue;
                            };
                            for page in db::list_pages(c, &proto.id)? {
                                // `true` = include archived versions: a component that appears in
                                // ANY iteration counts as linked-page (stable across rollbacks).
                                for version in db::list_page_versions(c, &page.id, true)? {
                                    for variant in db::list_variants(c, &version.id)? {
                                        for name in nyx_core::render::referenced_component_names(
                                            &variant.composition_json,
                                        ) {
                                            if !linked.contains(&name) {
                                                linked.push(name);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // …then everything those components' TEMPLATES reference, transitively.
                        nyx_core::render::ReferenceIndex::build(
                            components
                                .iter()
                                .map(|c| (c.name.as_str(), c.template.as_str())),
                        )
                        .extend_with_referenced(&mut linked);
                        Ok::<_, diesel::result::Error>(ComponentCatalog {
                            components,
                            linked_names: linked,
                        })
                    })
                    .map_err(Self::db_err)
                    .and_then(|catalog| Self::json(&catalog))
            }
            // `design_component_get` — ONE `design_components` row by id (the composant-detail read
            // model, task #11), or `null` when unknown. `{ componentId }`. A pure read.
            "design_component_get" => {
                let component_id = self.arg_str("componentId")?;
                self.db()
                    .with_conn(|c| db::get_component(c, &component_id))
                    .map_err(Self::db_err)
                    .and_then(|component| Self::json(&component))
            }
            // `design_component_nodes` — every COMPOSITION NODE across the project's proto
            // compositions that instantiates a given component (task #11: "clic sur un nœud → détail
            // pré-paramétré ... atteignable depuis l'arbre"). Each entry carries the variant it lives
            // in (id + title + page title) and the node's `node_key` PATH + `params_json` so the
            // detail opens ALREADY parameterized on that node. `{ projectId, componentName }` ->
            // `{ nodes }`. A pure read.
            "design_component_nodes" => {
                let project_id = self.arg_str("projectId")?;
                let component_name = self.arg_str("componentName")?;
                #[derive(serde::Serialize)]
                struct ComponentNode {
                    variant_id: String,
                    variant_title: String,
                    page_title: String,
                    node_key: String,
                    /// The node's params serialized (the pre-parameterization payload).
                    params_json: String,
                }
                self.db()
                    .with_conn(|c| {
                        let mut nodes: Vec<ComponentNode> = Vec::new();
                        for spec in db::list_specs(c, &project_id)? {
                            let Some(proto) = db::get_prototype_for_spec(c, &spec.id)? else {
                                continue;
                            };
                            for page in db::list_pages(c, &proto.id)? {
                                for version in db::list_page_versions(c, &page.id, true)? {
                                    for variant in db::list_variants(c, &version.id)? {
                                        for node in nyx_core::render::composition_nodes(
                                            &variant.composition_json,
                                        ) {
                                            if node.component.as_deref() == Some(&component_name) {
                                                nodes.push(ComponentNode {
                                                    variant_id: variant.id.clone(),
                                                    variant_title: variant.title.clone(),
                                                    page_title: page.title.clone(),
                                                    node_key: node.node_key,
                                                    params_json: serde_json::Value::Object(
                                                        node.params,
                                                    )
                                                    .to_string(),
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Ok::<_, diesel::result::Error>(nodes)
                    })
                    .map_err(Self::db_err)
                    .and_then(|nodes| Self::json(&serde_json::json!({ "nodes": nodes })))
            }
            // `proto_variant_set_node_params` — PERSIST a node's params PER NODE (task #11): locate
            // the node at `nodeKey` in the variant's composition tree, replace its params, and write
            // the whole tree back (the proto stays a COMPOSITION, not an HTML blob). Unknown
            // variant/node or a bad params blob is a readable error. Emits `prototypes://changed` so
            // an open design surface re-pulls.
            // `{ variantId, nodeKey, paramsJson }` -> `{ variant, warnings }`.
            //
            // `warnings` is THE NODE CONTRACT checked at the WRITE (finding
            // `01KYQ0R9414Q5K63TBV9CCMC9J`): the keys just persisted are CONFRONTED with the params
            // AND axes the component this node references declares. A key declared nowhere (a typo,
            // the wrong name) and a `required` param left unprovided are reported DISTINCTLY, each
            // naming the component, the node, the key and the correction. It is a WARNING, never a
            // refusal — the user's edit is saved either way, and the surface SAYS what will render
            // empty instead of letting the frame go blank in silence.
            "proto_variant_set_node_params" => {
                let variant_id = self.arg_str("variantId")?;
                let node_key = self.arg_str("nodeKey")?;
                let params_json = self.arg_str("paramsJson")?;
                let params: serde_json::Map<String, Value> =
                    match serde_json::from_str(&params_json) {
                        Ok(serde_json::Value::Object(m)) => m,
                        _ => {
                            return Err(format!(
                                "{}: paramsJson must be a JSON object",
                                self.command
                            ))
                        }
                    };
                type SetNodeParamsOutcome = Result<(db::PrototypeVariant, Vec<String>), String>;
                let (variant, warnings) = self
                    .db()
                    .with_conn(|c| -> Result<SetNodeParamsOutcome, diesel::result::Error> {
                        let Some(variant) = db::get_variant(c, &variant_id)? else {
                            return Ok(Err(format!("unknown variant {variant_id}")));
                        };
                        let root = match serde_json::from_str::<nyx_core::render::CompositionNode>(
                            &variant.composition_json,
                        ) {
                            Ok(r) => r,
                            Err(e) => return Ok(Err(format!("bad composition json: {e}"))),
                        };
                        let Some(updated) =
                            nyx_core::render::set_node_params(&root, &node_key, params.clone())
                        else {
                            return Ok(Err(format!(
                                "node '{node_key}' not found in variant {variant_id}"
                            )));
                        };
                        let json = serde_json::to_string(&updated)
                            .unwrap_or_else(|_| variant.composition_json.clone());
                        // THE NODE CONTRACT, on the node that was just edited — computed BEFORE
                        // the write guard so the diagnostic is about what the user typed, not
                        // about whether the round happened to be locked.
                        let warnings = node_contract_warnings(c, &variant, &json, &node_key)?;
                        // GUARDED: editing a variant that is the FROZEN elected composition
                        // of a design-locked round is refused (the "élection figée"
                        // invariant) — iterate by opening a NEW round, never mutate a frozen
                        // one (PDR-0009).
                        match db::update_variant_composition(c, &variant_id, &json) {
                            Ok(Some(v)) => Ok(Ok((v, warnings))),
                            Ok(None) => Ok(Err(format!("unknown variant {variant_id}"))),
                            Err(db::RoundMutationError::RoundLocked { locked_round_id }) => {
                                Ok(Err(format!(
                                    "variant {variant_id} is part of the FROZEN design \
                                         (round {locked_round_id} is locked): its composition is \
                                         immutable — open a NEW round to iterate (PDR-0009)"
                                )))
                            }
                            Err(db::RoundMutationError::Db(e)) => Err(e),
                        }
                    })
                    .map_err(Self::db_err)??;
                // An open design surface (detail / renderer / catalogue) re-pulls after the edit.
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&serde_json::json!({ "variant": variant, "warnings": warnings }))
            }
            // `design_render` — COMPILE a composition (component tree + params) to MARKUP entirely
            // in the sidecar (Phase 3 task #6, `nyx_core::render`), the templates read data-driven
            // from the project's `design_components`. The Electron renderer compiles NOTHING — it
            // only displays this markup in the iframe (task #7).
            //
            // It runs the LENIENT render (`render_composition_lenient`, finding
            // `01KYT78699XG6Q6P3VZC64W8H8`): the frame ALWAYS gets markup, and a node it cannot
            // render comes back as a NAMED placeholder in place plus an entry in `unrendered[]`
            // (`node_key`, `component`, `reason`, `detail`) instead of blanking the whole page. A
            // composition under design always holds `[NEW]` nodes, so "mostly renderable, with
            // holes" is the NORMAL state of a page in flight, not an edge case — and the renderer no
            // longer has to compute its own notion of a hole to work around an all-or-nothing
            // compile. The vocabulary is PRESERVED here (`Vocabulary::Preserve`): the shim pilots the
            // `data-nyx-*` marks live, which is what the chip-bar, the in-place state change and the
            // `data-nyx-page` navigation are made of.
            //
            // NAVIGATION (finding `01KYW34347CZSNXCYS35KDG3EY`): a node may state the PAGE it opens
            // (`navigate_to`, a `prototype_pages.id`), and the render resolves that id to the slug
            // the shim navigates with — `data-nyx-page` + `data-nyx-page-id` on the node's root. The
            // resolution needs the prototype's page DIRECTORY, so `prototypeId` is what supplies it:
            // WITHOUT it this render knows of no page, and every link comes back NAMED as unknown
            // (`navigation[].status = "unknown_page"`, a `data-nyx-page-missing` marker in place)
            // rather than silently dropped. Nothing is required for a composition with no link —
            // which is every composition that predates this — so the argument is optional.
            //
            // THE AXES THE SHIM WILL PILOT (`subjectStatesJson`, finding
            // `01KZ41YK39SGNKF6XV87A35GPN`): the very `states_json` the caller hands the frame as
            // `window.__nyxAxes`. It is NOT resolved here — the shim applies it live, which is what
            // the chip-bar and the in-place flip are — it is DECLARED, so each axis has exactly one
            // applier. Every axis a NODE declares and the shim does not is baked into that node's
            // markup instead, which is the only way a node's own `variant` / `size` / `loading` can
            // act at all: nothing in the frame pilots them. Optional — an absent one means the frame
            // gets no axes, so every node's own are baked (a page thumbnail, a variant that declares
            // none).
            //
            // The only refusal left is a composition that does not PARSE — a READABLE error, never a
            // silent empty frame. `{ projectId, compositionJson, prototypeId?, subjectStatesJson? }`
            // -> `{ markup, node_count, unrendered, unrendered_count, css_slots, navigation }`. A
            // pure render.
            "design_render" => {
                let project_id = self.arg_str("projectId")?;
                let composition_json = self.arg_str("compositionJson")?;
                let prototype_id = self.arg_opt_str("prototypeId");
                let subject_states_json = self.arg_opt_str("subjectStatesJson");
                let pages = match &prototype_id {
                    Some(prototype_id) => self
                        .db()
                        .with_conn(|c| db::render_pages(c, prototype_id))
                        .map_err(Self::db_err)?,
                    None => Vec::new(),
                };
                let components = self
                    .db()
                    .with_conn(|c| db::list_components(c, &project_id))
                    .map_err(Self::db_err)?;
                let render_components: Vec<nyx_core::render::RenderComponent> = components
                    .into_iter()
                    .map(|c| nyx_core::render::RenderComponent {
                        name: c.name,
                        template: c.template,
                        // A component REFERENCED by another one (`data-nyx-use`) has its OWN axes
                        // resolved to concrete classes HERE, in the sidecar — so the whole catalogue
                        // row crosses, states and params included, not just the template.
                        states_json: c.states_json,
                        params_json: c.params_json,
                        // The component's SCOPED CSS SLOT crosses too: the render stamps the scope
                        // on its root element(s) and appends the compiled stylesheet to the markup
                        // (finding 01KYVMTWB71R0D0CT1SV88J8DX). The frame's CSP is untouched — it
                        // already carries `style-src 'unsafe-inline'` for the theme layer.
                        css_slot: nyx_core::css_slot::CssSlot::parse(c.css_slot_json.as_deref()),
                    })
                    .collect();
                let resolved = nyx_core::render::render_composition_lenient(
                    &render_components,
                    &pages,
                    &composition_json,
                    subject_states_json.as_deref(),
                )
                .map_err(|e| format!("{}: {e}", self.command))?;
                #[derive(serde::Serialize)]
                struct UnrenderedNodeResult {
                    node_key: String,
                    component: Option<String>,
                    /// The stable machine code — pinned by `contract/design-fixtures.json`.
                    reason: &'static str,
                    detail: String,
                }
                /// ONE scoped CSS slot the render emitted — what the review surface BADGES on the
                /// node and counts in the chrome. `scope` is the literal selector the CSS is bounded
                /// to, `features` names which infeasible-in-utilities constructs it uses, `reason`
                /// says why the exception exists at all.
                #[derive(serde::Serialize)]
                struct CssSlotResult {
                    scope_id: String,
                    component: String,
                    scope: String,
                    reason: String,
                    features: Vec<String>,
                    bytes: usize,
                    css: String,
                    /// The refusal CODE when the stored slot no longer compiles (nothing was
                    /// emitted) — `null` for a slot that applies.
                    refused: Option<String>,
                    detail: Option<String>,
                }
                /// ONE navigation link the render resolved — the PARCOURS, as the frame reads it.
                /// `page` is the SLUG the shim posts (it is also what `data-nyx-page` carries on the
                /// node's root) and `page_title` is what a human reads; both are resolved from the
                /// `page_id` the tree stores, at every render. `status` is the machine code pinned
                /// by `contract/design-fixtures.json` — `linked` for a link that works, and a NAMED
                /// code for one that does not, never an absence.
                #[derive(serde::Serialize)]
                struct NavigationResult {
                    node_key: String,
                    page_id: String,
                    page: Option<String>,
                    page_title: Option<String>,
                    status: &'static str,
                    detail: String,
                }
                #[derive(serde::Serialize)]
                struct RenderResult {
                    markup: String,
                    node_count: usize,
                    unrendered: Vec<UnrenderedNodeResult>,
                    unrendered_count: usize,
                    css_slots: Vec<CssSlotResult>,
                    css_slot_count: usize,
                    navigation: Vec<NavigationResult>,
                    navigation_count: usize,
                    navigation_broken_count: usize,
                }
                let navigation_broken_count = resolved
                    .navigation
                    .iter()
                    .filter(|l| l.status != nyx_core::render::NavigationStatus::Linked)
                    .count();
                Self::json(&RenderResult {
                    markup: resolved.markup,
                    node_count: resolved.node_count,
                    unrendered_count: resolved.unrendered.len(),
                    unrendered: resolved
                        .unrendered
                        .into_iter()
                        .map(|u| UnrenderedNodeResult {
                            node_key: u.node_key,
                            component: u.component,
                            reason: u.reason.code(),
                            detail: u.detail,
                        })
                        .collect(),
                    css_slot_count: resolved.css_slots.len(),
                    css_slots: resolved
                        .css_slots
                        .into_iter()
                        .map(|s| CssSlotResult {
                            scope_id: s.scope_id,
                            component: s.component,
                            scope: s.selector,
                            reason: s.reason,
                            features: s.features,
                            bytes: s.bytes,
                            css: s.css,
                            refused: s.refused,
                            detail: s.detail,
                        })
                        .collect(),
                    navigation_count: resolved.navigation.len(),
                    navigation_broken_count,
                    navigation: resolved
                        .navigation
                        .into_iter()
                        .map(|l| NavigationResult {
                            node_key: l.node_key,
                            page_id: l.page_id,
                            page: l.slug,
                            page_title: l.title,
                            status: l.status.code(),
                            detail: l.detail,
                        })
                        .collect(),
                })
            }
            // `design_inventory` — the RUNTIME TRIGGER (reviews #23, #40): DISCOVER the project's
            // component BUNDLES and catalogue them. Resolves the project's repos the SAME way the dev
            // context does (root workspace → derived members), scans them by the detected-stack ENTRY
            // globs, groups each component's files into a BUNDLE (templateUrl resolved, non-components
            // filtered out) and UPSERTs its discovery — never a template: the SYNTHESIS is an agent's
            // job (`design_refresh_component`), so `pending_count` reports how many bundles await one.
            // The UI twin of the `design_inventory` MCP tool, sharing the SAME
            // `nyx_core::design::inventory_project` authority (PDR-0005). It also RECONCILES the
            // catalogue with reality: a bundle MOVED on disk is re-anchored by CONTENT (same row,
            // same synthesis), a greenfield [NEW] the front now implements is MATERIALIZED in place,
            // and a row whose source is GONE is pruned under guards — all counted in the result.
            // GREENFIELD-safe (no front → nothing, no error). Emits `prototypes://changed` so the
            // `/design` catalogue re-pulls — only when the catalogue actually moved.
            //
            // The counters this returns are what the `/design` header REPORTS after a sync (review
            // 01KYHPFDE21W0EMVX4J87RHGCV, CRITICAL): the button used to throw the whole result away,
            // so an élagage — including one caused by a scan that could not read part of the tree —
            // happened without a single signal, no confirmation and no undo. `pruned_count` says
            // what was removed, `orphan_count` what is now flagged, `scan_error_count` whether the
            // walk could read everything it looked at (non-zero ⇒ nothing was pruned in the repos
            // concerned, and `discovered` under-reports).
            //
            // `{ projectId }` -> `{ discovered, repos_scanned, pending_count, synthesized_count,
            // component_count, moved_count, materialized_count, orphan_count, pruned_count,
            // flagged_orphan_count, scan_error_count }`. Called by the AGENT (its `design_inventory`
            // MCP tool shares this exact `nyx_core::design::inventory_project` authority) — NO UI
            // gesture triggers it since finding 01KYKX7HBP4ZSSS6ZDM31T6A5E; `/design` is a reader
            // that repaints on the `prototypes://changed` this emits.
            "design_inventory" => {
                let project_id = self.arg_str("projectId")?;
                let outcome = self
                    .db()
                    .with_conn(|c| nyx_core::design::inventory_project(c, &project_id))
                    .map_err(Self::db_err)?;
                // An open design surface (catalogue / renderer / detail) re-pulls after the
                // inventory — but ONLY when the catalogue actually moved: a no-op sync has nothing
                // to re-pull, and one broadcast per idle run is noise every listener pays for.
                if outcome.catalogue_changed {
                    self.events.emit(Event::Changed {
                        topic: "prototypes".to_string(),
                    });
                }
                #[derive(serde::Serialize)]
                struct InventoryResult {
                    discovered: usize,
                    repos_scanned: usize,
                    pending_count: usize,
                    synthesized_count: usize,
                    component_count: usize,
                    /// Rows RE-ANCHORED onto a bundle that MOVED on disk (matched by CONTENT).
                    moved_count: usize,
                    /// Greenfield `[NEW]` rows a discovered bundle MATERIALIZED this run (#41).
                    materialized_count: usize,
                    /// Rows whose recorded source is GONE — pruned or spared (see `pruned_count`).
                    orphan_count: usize,
                    /// How many of those orphans this run actually REMOVED from the catalogue —
                    /// the DELETEs that really affected a row, never the ones merely intended
                    /// (the conditional delete spares a row a concurrent write gave content to).
                    /// Only rows carrying NO authored content can land here — removing them loses
                    /// nothing, a re-scan rebuilds them.
                    pruned_count: usize,
                    /// How many carry the PERSISTENT orphan flag: their source is gone and they
                    /// hold authored content, so they were kept for an explicit human decision.
                    /// Includes rows a run that COULD NOT JUDGE (repo not scanned / incomplete walk
                    /// / empty scan) left flagged — such a run no longer erases the verdict of the
                    /// last competent one, so the badge and the "Orphelines" filter stop blinking
                    /// off on a transient I/O failure.
                    flagged_orphan_count: usize,
                    /// I/O failures the WALK met. Non-zero ⇒ the discovery is PARTIAL and NOTHING
                    /// was pruned in the repos concerned.
                    scan_error_count: usize,
                }
                Self::json(&InventoryResult {
                    discovered: outcome.discovered,
                    repos_scanned: outcome.repos_scanned,
                    pending_count: outcome.pending.len(),
                    synthesized_count: outcome.synthesized_count,
                    component_count: outcome.component_count,
                    moved_count: outcome.moved.len(),
                    materialized_count: outcome.materialized.len(),
                    orphan_count: outcome.orphans.len(),
                    pruned_count: outcome.orphans.iter().filter(|o| o.pruned).count(),
                    flagged_orphan_count: outcome
                        .orphans
                        .iter()
                        .filter(|o| o.flagged_orphan)
                        .count(),
                    scan_error_count: outcome.scan_errors.count,
                })
            }
            // `design_component_delete` — REMOVE a component from the project's catalogue (review
            // #44: `db::delete_component` existed but was exposed NOWHERE, so an entry a wrong glob
            // had catalogued could never be taken out). The UI twin of the `design_delete_component`
            // MCP tool, over the SAME authority (`nyx_core::design::remove_component`, PDR-0005).
            // Deleting touches the CATALOGUE only: a composition references components BY NAME, so a
            // node pointing at a deleted component simply reads `[NEW]` (to be built) at the next
            // read — nothing frozen is rewritten. An unknown id is a readable error, never a silent
            // no-op. Emits `prototypes://changed`.
            //
            // IT SAYS WHAT IT REALLY ACHIEVES (finding 01KYSCX4PNKTMX17YVG9XD8KBZ). A component's
            // identity IS its source: deleting the row of a component whose file still matches the
            // project's recipe is UNDONE by the next inventory, which re-discovers it as pending. The
            // gesture used to answer a bare `{ deleted: true }`. It now returns `ephemeral` + the
            // discovery's `verdict` on that path + the `warnings[]` that name the correction
            // (restrict the recipe, or exclude the source), so the surface can stop promising a
            // removal that will not last. `excludeSource: true` writes the PERSISTENT exclusion (the
            // recipe's own `exclude` list) exactly as the MCP tool does.
            // `{ componentId, excludeSource? }` -> the removal (`deleted`, `effect`, `excluded`,
            // `recipe_materialized`, `warnings`).
            "design_component_delete" => {
                let component_id = self.arg_str("componentId")?;
                let exclude_source = self.arg_opt_bool("excludeSource").unwrap_or(false);
                let removal = self
                    .db()
                    .with_conn(|c| {
                        let Some(component) = db::get_component(c, &component_id)? else {
                            return Ok(None);
                        };
                        let project_id = component.project_id.clone();
                        nyx_core::design::remove_component(
                            c,
                            &project_id,
                            &component,
                            exclude_source,
                        )
                        .map(Some)
                    })
                    .map_err(Self::db_err)?;
                // An unknown id, and a row that vanished between the read and the DELETE, are the
                // same readable refusal they always were.
                let deleted = removal.as_ref().is_some_and(|r| r.deleted);
                let Some(removal) = removal else {
                    return Err(format!(
                        "{}: unknown component {component_id}",
                        self.command
                    ));
                };
                if let Some(refused) = &removal.refused {
                    return Err(format!("{}: {refused}", self.command));
                }
                if !deleted {
                    return Err(format!(
                        "{}: unknown component {component_id}",
                        self.command
                    ));
                }
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&removal)
            }
            // --- PROTO read-model + UI-driven writes (review #24) --------------------
            // The DATA layer the future proto UI (onglet + viewer / rounds / élection / commentaires
            // / composition — T25-T29) consumes. Reads share `nyx_core::db` with the MCP `proto_*`
            // tools (PDR-0005: one authority for UI + MCP); the WRITES are the USER's decisions
            // (RETENU is the user's choice, distinct from the agent's is_main — PDR-0009) and reuse
            // the SAME guarded `db` authority, so the "élection figée" lock still refuses a mutation
            // on a design-locked round. Every write emits `prototypes://changed` so an open surface
            // re-pulls (symmetric to the `proto_*` tools' `ChangedTopic::Prototypes`).
            //
            // `proto_tree` — the FULL tree of a Spec's prototype: pages/versions/variants + rounds +
            // the both-tier election (is_main = agent reco on each variant vs RETENU = user choice on
            // the current round) + anchored comments (open + resolved). `null` when the Spec has no
            // prototype (the UI's empty cue). `{ specId }`. A pure read.
            "proto_tree" => {
                let spec_id = self.arg_str("specId")?;
                self.db()
                    .with_conn(|c| db::proto_tree(c, &spec_id))
                    .map_err(Self::db_err)
                    .and_then(|tree| Self::json(&tree))
            }
            // `proto_composition` — the FROZEN elected composition of a Spec's prototype (T29): the
            // coder SEAM `db::elected_composition` exposes off the design LOCK — per page pinned in
            // the frozen round, the arbre nommé = PLACEMENT with each node classified by its NATURE
            // ([DS] catalogue / [NEW] to-build / [inline] bespoke) carrying a per-node design-system
            // `component_ref`, the `[NEW]` candidates, and the pre-dev review as ANCHORS + COUNTS
            // ONLY — never the comment BODIES (PDR-0009: the review is never transmitted to the
            // coder, and this is the seam a coder harness consumes — review #47).
            // `null` until the design is LOCKED (the "élection figée" empty cue: no lock → nothing to
            // expose, so the seam never drifts). SAME `nyx_core::db` authority as the MCP
            // `proto_composition` tool (PDR-0005: one authority for UI + MCP). `{ prototypeId }`. A
            // pure read — no mutation, no `changed` broadcast; the live refresh rides the
            // `prototypes://changed` channel the election/lock writes already push.
            "proto_composition" => {
                let prototype_id = self.arg_str("prototypeId")?;
                self.db()
                    .with_conn(|c| db::elected_composition(c, &prototype_id))
                    .map_err(Self::db_err)
                    .and_then(|composition| Self::json(&composition))
            }
            // `proto_elect` — POSE the USER's RETENU variant for a page in a round (T27). The page
            // must be pinned in the round and the variant must exist (readable errors otherwise).
            // GUARDED: electing on a design-LOCKED round is refused (élection figée) — orient toward a
            // rollback. `{ roundId, pageId, variantId, rationale?, decidedBy? }` -> `{ round_page }`.
            "proto_elect" => {
                let round_id = self.arg_str("roundId")?;
                let page_id = self.arg_str("pageId")?;
                let variant_id = self.arg_str("variantId")?;
                let rationale = self.arg_opt_str("rationale");
                let decided_by = self.arg_opt_str("decidedBy");
                let round_page = self
                    .db()
                    .with_conn(
                        |c| -> Result<Result<db::PrototypeRoundPage, String>, diesel::result::Error> {
                            if db::get_round_page(c, &round_id, &page_id)?.is_none() {
                                return Ok(Err(format!(
                                    "page {page_id} is not pinned in round {round_id}"
                                )));
                            }
                            if db::get_variant(c, &variant_id)?.is_none() {
                                return Ok(Err(format!("unknown variant {variant_id}")));
                            }
                            match db::elect_round_page_variant(
                                c,
                                &round_id,
                                &page_id,
                                &variant_id,
                                rationale.as_deref(),
                                decided_by.as_deref(),
                            ) {
                                Ok(Some(rp)) => Ok(Ok(rp)),
                                Ok(None) => Ok(Err(format!(
                                    "page {page_id} is not pinned in round {round_id}"
                                ))),
                                Err(db::RoundMutationError::RoundLocked { locked_round_id }) => {
                                    Ok(Err(format!(
                                        "round {locked_round_id} is FROZEN (its design is locked): \
                                         its election is immutable — open a NEW round (rollback) to \
                                         iterate (PDR-0009)"
                                    )))
                                }
                                Err(db::RoundMutationError::Db(e)) => Err(e),
                            }
                        },
                    )
                    .map_err(Self::db_err)??;
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&serde_json::json!({ "round_page": round_page }))
            }
            // `proto_election_clear` — RETIRE the USER's RETENU for a page in a round (RETENU is
            // reversible, PDR-0009). Same lock guard as `proto_elect`. `{ roundId, pageId }` ->
            // `{ round_page }`.
            "proto_election_clear" => {
                let round_id = self.arg_str("roundId")?;
                let page_id = self.arg_str("pageId")?;
                let round_page = self
                    .db()
                    .with_conn(
                        |c| -> Result<Result<db::PrototypeRoundPage, String>, diesel::result::Error> {
                            match db::clear_round_page_election(c, &round_id, &page_id) {
                                Ok(Some(rp)) => Ok(Ok(rp)),
                                Ok(None) => Ok(Err(format!(
                                    "page {page_id} is not pinned in round {round_id}"
                                ))),
                                Err(db::RoundMutationError::RoundLocked { locked_round_id }) => {
                                    Ok(Err(format!(
                                        "round {locked_round_id} is FROZEN (its design is locked): \
                                         its election is immutable — open a NEW round (rollback) to \
                                         iterate (PDR-0009)"
                                    )))
                                }
                                Err(db::RoundMutationError::Db(e)) => Err(e),
                            }
                        },
                    )
                    .map_err(Self::db_err)??;
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&serde_json::json!({ "round_page": round_page }))
            }
            // `proto_comment_add` — ADD a pre-dev review comment anchored to a tree NODE (T28). The UI
            // is the HUMAN façade, so `authorKind` defaults to `user` (an explicit value is honoured).
            // Unknown prototype → readable error. Never transmitted to the coder (PDR-0009). `{
            // prototypeId, body, nodeKey?, originVariantId?, authorKind?, authorRef? }` -> `{ feedback }`.
            "proto_comment_add" => {
                let prototype_id = self.arg_str("prototypeId")?;
                let body = self.arg_str("body")?;
                let node_key = self.arg_opt_str("nodeKey");
                let origin_variant_id = self.arg_opt_str("originVariantId");
                let author_kind = self
                    .arg_opt_str("authorKind")
                    .unwrap_or_else(|| db::FEEDBACK_AUTHOR_USER.to_string());
                let author_ref = self.arg_opt_str("authorRef");
                let input = db::FeedbackInput {
                    origin_variant_id,
                    node_key,
                    body,
                    author_kind,
                    author_ref,
                };
                let feedback = self
                    .db()
                    .with_conn(
                        |c| -> Result<
                            Result<db::PrototypeFeedback, String>,
                            diesel::result::Error,
                        > {
                            if db::get_prototype(c, &prototype_id)?.is_none() {
                                return Ok(Err(format!("unknown prototype {prototype_id}")));
                            }
                            Ok(Ok(db::create_feedback(c, &prototype_id, &input)?))
                        },
                    )
                    .map_err(Self::db_err)??;
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&serde_json::json!({ "feedback": feedback }))
            }
            // `proto_comment_resolve` — RESOLVE a pre-dev review comment (T28), recording an optional
            // note / the variant that addressed it. Unknown comment → readable error. `{ feedbackId,
            // resolutionNote?, resolutionViaVariantId? }` -> `{ feedback }`.
            "proto_comment_resolve" => {
                let feedback_id = self.arg_str("feedbackId")?;
                let resolution_note = self.arg_opt_str("resolutionNote");
                let resolution_via_variant_id = self.arg_opt_str("resolutionViaVariantId");
                let feedback = self
                    .db()
                    .with_conn(|c| {
                        db::resolve_feedback(
                            c,
                            &feedback_id,
                            resolution_note.as_deref(),
                            resolution_via_variant_id.as_deref(),
                        )
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| format!("unknown feedback {feedback_id}"))?;
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&serde_json::json!({ "feedback": feedback }))
            }
            // `proto_round_manifest` — the MANIFEST of ONE round, resolved (finding
            // `01KZ3AP34K1PKS6W0E75RP7DF6`): per page pinned in it, the entry (which page VERSION
            // that round froze + the USER's RETENU) and the EFFECTIVE retained variant — the same
            // pair `proto_tree` resolves for the CURRENT round, for ANY round. It is what makes a
            // PAST round CONSULTABLE: `proto_tree` already carries every version and variant
            // (archived included), so a surface holding both can show a round exactly as it was.
            // `{ roundId }` -> `RoundManifestPage[]`. An unknown round pins nothing, so it answers
            // the empty manifest — a list, never an error.
            //
            // A pure READ: no mutation, no `changed` broadcast. CONSULTING IS NOT ROLLING BACK —
            // it opens no round, copies no manifest and moves no election. `proto_round_rebase`
            // below stays the only thing that creates a round, and the UI does not call it
            // (PDR-0009: the agent opens a round to apply a correction; the human reads).
            "proto_round_manifest" => {
                let round_id = self.arg_str("roundId")?;
                self.db()
                    .with_conn(|c| db::round_manifest(c, &round_id))
                    .map_err(Self::db_err)
                    .and_then(|manifest| Self::json(&manifest))
            }
            // `proto_round_rebase` — ROLLBACK onto an earlier round (T26): open a NEW current round
            // whose manifest COPIES the source round's pinned versions, so the design returns to that
            // snapshot WITHOUT mutating the frozen source (PDR-0009: rounds gardés comme rollback).
            // Unknown / cross-prototype source → readable error. `{ prototypeId, sourceRoundId, label,
            // summary? }` -> `{ round }`.
            "proto_round_rebase" => {
                let prototype_id = self.arg_str("prototypeId")?;
                let source_round_id = self.arg_str("sourceRoundId")?;
                let label = self.arg_str("label")?;
                let summary = self.arg_opt_str("summary");
                let round = self
                    .db()
                    .with_conn(|c| {
                        db::rebase_round(
                            c,
                            &prototype_id,
                            &source_round_id,
                            &label,
                            summary.as_deref(),
                        )
                    })
                    .map_err(Self::db_err)?
                    .ok_or_else(|| {
                        format!("unknown or cross-prototype source round {source_round_id}")
                    })?;
                self.events.emit(Event::Changed {
                    topic: "prototypes".to_string(),
                });
                Self::json(&serde_json::json!({ "round": round }))
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

/// The review-MUTATING DB commands (v2.4) — each broadcasts `repos://changed` on success so an
/// open review panel re-pulls its `review_comment_list` feed + `review_task_list` + `review_unread`
/// badge live, symmetric to the MCP `review_comment_post` / `review_task_structure` tools'
/// `Repos` effect. The COMMENT mutations (post/resolve/reopen) change the feed and the unread
/// count; the TASK mutation (promote) changes the promoted-task list; `review_mark_read`
/// zeroes the unread badge; `review_remap` re-anchors/outdates the anchored comments after the
/// repo advanced — all push `repos://changed`. The pure reads (`review_comment_list`,
/// `review_task_list`, `review_unread`) are deliberately absent.
fn mutates_review_comments(command: &str) -> bool {
    matches!(
        command,
        "review_comment_post"
            | "review_comment_resolve"
            | "review_comment_reopen"
            | "review_task_promote"
            | "review_mark_read"
            | "review_remap"
    )
}

/// The review FINDING-mutating subset of [`mutates_review_comments`] — the command that creates a
/// `tasks kind=review` row (`promote` a comment into a finding). Besides the `repos://changed` all
/// review mutations push (the viewer), this ALSO pushes `specs://changed` because a finding IS a Spec
/// task: the terminal-spec OVERLAY (and the Spec detail page) render findings from `useSpecDetail`,
/// which re-pulls on `specs://changed`, NOT `repos://changed`. Without this the overlay only reflected
/// a promote after a manual reload (review F3, symmetric to the MCP `review_submit_feedback` /
/// `review_task_structure` tools now naming `ChangedTopic::Specs`). The pure COMMENT mutations
/// (post/resolve/reopen/mark_read/remap) do NOT touch the task list, so they stay `repos`-only.
fn mutates_review_findings(command: &str) -> bool {
    command == "review_task_promote"
}

/// The terminal RECORD-MUTATING DB commands whose UI path must broadcast `terminals://changed`
/// — SYMMETRIC to the MCP terminal tools (`create_terminal`/`close_terminal`/`attach_terminal`
/// emit `ChangedTopic::Terminals` via `emit_effects`). These flip a terminal record's existence
/// or workspace binding, which the runtime aggregate contains, so every surface that re-pulls on
/// `terminals://changed` (`useWorkspaceRuntime`, `useTerminals`) reflects the mutation live —
/// WITHOUT polling (review 01KWKH7TE86AXS41Q4VS2CEXCW).
///
/// `rename` is INCLUDED (v2.5.6 cockpit rebranch): the runtime tree ROW now renders the terminal's
/// display name from `label` in the `workspace_runtime_snapshot` (`WorkspaceTerminal` flattens the
/// persisted `Terminal`), NOT from the sidebar's optimistic `useTerminals` store. So a UI rename
/// that only updated `terminals.label` (no push) left the tree row showing the OLD name until an
/// unrelated `terminals://changed` or a relaunch — the inline rename looked broken. Broadcasting
/// here re-pulls the snapshot so the row repaints with the new label at once. The label is aggregate
/// CONTENT (what the row shows), even though it is not aggregate SHAPE — the old exclusion conflated
/// the two. Still safe: the only `terminals://changed` subscribers are pure READERS (no re-mutation,
/// no loop — see the emission note above).
///
/// EXCLUDED on purpose: the pure sidebar-local ops (`reorder`, `set_active`, `persist_scrollback`,
/// `set_terminal_cwd`, `terminal_exec_mark_read`) — they carry their own optimistic renderer state
/// (`reorder` drives the tree's optimistic order; a re-pull would clobber it). `auto_attach_terminal`
/// is INCLUDED but emits only when it actually re-bound (`changed == true`); it is handled at its
/// call-site, not by this list, because the emission is conditional on the resolver's decision.
fn mutates_terminals(command: &str) -> bool {
    matches!(
        command,
        "create_terminal" | "close_terminal" | "attach_terminal" | "rename"
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

/// THE NODE CONTRACT for the ONE node `proto_variant_set_node_params` just edited (finding
/// `01KYQ0R9414Q5K63TBV9CCMC9J`). It resolves the node's component through the project catalogue
/// (variant → page version → page → prototype → Spec → project) and confronts the keys the node now
/// carries with what that component DECLARES — its params and its axes.
///
/// Scoped to the EDITED node on purpose: this is the per-node authoring gesture, and reporting the
/// whole tree here would drown the one thing the user just changed. The whole-tree read is the
/// surface's job (the composition it displays) and the agent's is
/// [`nyx_core::mcp_tools_proto`]'s `proto_add_variant`.
///
/// It never fails the write: an unresolvable project, an unknown component or a node that vanished
/// yields NO warning rather than an error — a diagnostic must not be able to block a save.
fn node_contract_warnings(
    conn: &mut diesel::sqlite::SqliteConnection,
    variant: &db::PrototypeVariant,
    composition_json: &str,
    node_key: &str,
) -> Result<Vec<String>, diesel::result::Error> {
    let Some(node) = nyx_core::render::composition_nodes(composition_json)
        .into_iter()
        .find(|n| n.node_key == node_key)
    else {
        return Ok(Vec::new());
    };
    let Some(component_name) = node.component.clone() else {
        return Ok(Vec::new());
    };
    let Some(version) = db::get_page_version(conn, &variant.page_version_id)? else {
        return Ok(Vec::new());
    };
    let Some(page) = db::get_page(conn, &version.page_id)? else {
        return Ok(Vec::new());
    };
    let Some(prototype) = db::get_prototype(conn, &page.prototype_id)? else {
        return Ok(Vec::new());
    };
    let Some(spec) = db::get_spec(conn, &prototype.spec_id)? else {
        return Ok(Vec::new());
    };
    let Some(component) = db::get_component_by_name(conn, &spec.project_id, &component_name)?
    else {
        return Ok(Vec::new());
    };
    Ok(nyx_core::mcp_tools_design::node_contract_warnings(
        &component_name,
        node_key,
        &node.params,
        component.params_json.as_deref(),
        component.states_json.as_deref(),
    ))
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
    /// services (with their was-running snapshot). Proves "sessions reprenables + restart_on_
    /// startup restaurés", scoped to the workspace, via the SAME policies (no third system).
    /// Pure read — idempotent, no `changed` broadcast. (No `interrupted` set — reverted v2.5.8.)
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

        // (crash-UI reverted in v2.5.8: the plan carries no `interrupted` field anymore.)
        assert!(plan.get("interrupted").is_none());

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

    /// WORKSPACE RESTORE — OPTION OFF: with `resume_agent_sessions` OFF, an `unknown`
    /// (stale/probable-kill) session is reported as a NON-resume entry (`option_off`), never
    /// blind-resumed. (The `unknown` state can still exist from `sweep_stale_active_sessions`
    /// staleness; a crash no longer produces it — v2.5.8 revert. This pins the toggle gate.)
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
                // kill) via `sweep_stale_active_sessions` — a legit way to reach the `unknown`
                // read path (the crash reconcile no longer calls it — v2.5.8 revert).
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

        // A label `rename` DOES broadcast (v2.5.6 cockpit): the runtime tree row renders the
        // terminal's display name from `label` in `workspace_runtime_snapshot`, so the snapshot must
        // re-pull for the row to repaint with the new name. It is aggregate CONTENT (not shape), but
        // the row reads it, so the surface must refresh live.
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
            after_create + 1,
            "rename broadcasts terminals://changed so the tree row (which reads label from the \
             snapshot) repaints with the new name"
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

    /// DONE-CRITERION (task 01KX0F6GDZEH3YN6QJDB7FC0WC, Q-015): a repo modification refreshes
    /// the derived view and PUSHES the update to the UI + MCP over the v2.1 real-time bus,
    /// without polling, with debounce. This drives the FULL sidecar chain end to end: the UI
    /// `repo_watch` db-command registers a real repo on the git-watch runtime built by
    /// `attach_events`, then an EDIT to the work tree causes a debounced `repos://changed`
    /// invalidation to land on the SAME pipe the domain events ride (the channel the renderer
    /// AND — via the shared authority — MCP re-pull on). NO poll loop: the pushes come from the
    /// fs watch, and an idle repo stays silent.
    #[test]
    fn repo_watch_pushes_repos_changed_on_a_real_edit_over_the_v21_pipe() {
        if nyx_core::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping ui repo_watch real-time test: git unavailable");
            return;
        }
        let h = Harness::new();
        // Build the git-watch runtime (+ command/MCP runtimes) as the boot does, so the
        // `repo_watch` db-command resolves a live runtime rather than erroring.
        h.runtime.attach_events(&h.events);

        let root = std::env::temp_dir().join(format!(
            "nyx-ui-repowatch-{}-{}",
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

        let (_project, root_ws) = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::repository::register_project(c, "Nyx", root.to_str().unwrap(), None)
            })
            .unwrap();
        // Resolve the repository id the way the renderer would (from the Git summary).
        let repos = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::workspace_git::summary(c, &root_ws.id))
            .unwrap()
            .unwrap()
            .repos;
        let repository_id = repos[0].repository_id.clone();

        // Register the real-time watch via the UI db-command. The seed refresh pushes one
        // `repos://changed` immediately (the surface starts from truth).
        h.run(
            "repo_watch",
            serde_json::json!({ "workspaceId": root_ws.id, "repositoryId": repository_id }),
        )
        .unwrap();
        let seeded = wait_until_repos_changed(&h.buf, 1, std::time::Duration::from_secs(3));
        assert!(seeded, "repo_watch seeded one repos://changed");
        let after_seed = changed_count(&h.buf, "repos");

        // EDIT the work tree — the watcher must ring (no polling) and push a DEBOUNCED refresh.
        std::fs::write(root.join("README.md"), b"changed by the user").unwrap();
        let pushed =
            wait_until_repos_changed(&h.buf, after_seed + 1, std::time::Duration::from_secs(5));
        assert!(pushed, "the edit pushed a repos://changed over the pipe");

        // NO POLLING: with the repo idle, the count stops climbing.
        std::thread::sleep(std::time::Duration::from_millis(900));
        let quiescent = changed_count(&h.buf, "repos");
        std::thread::sleep(std::time::Duration::from_millis(900));
        assert_eq!(
            changed_count(&h.buf, "repos"),
            quiescent,
            "an idle watched repo pushes nothing further — event-driven, not polled"
        );

        // unwatch releases the watch: no further pushes after a subsequent edit.
        h.run(
            "repo_unwatch",
            serde_json::json!({ "workspaceId": root_ws.id, "repositoryId": repository_id }),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let baseline = changed_count(&h.buf, "repos");
        std::fs::write(root.join("README.md"), b"after unwatch").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert_eq!(
            changed_count(&h.buf, "repos"),
            baseline,
            "no repos://changed after repo_unwatch — the OS watch was released"
        );

        h.runtime.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Poll the captured pipe buffer (a TEST aid, not the production mechanism) until at least
    /// `want` `repos://changed` events have landed or `timeout` elapses.
    fn wait_until_repos_changed(
        buf: &Arc<Mutex<Vec<u8>>>,
        want: usize,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if changed_count(buf, "repos") >= want {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        changed_count(buf, "repos") >= want
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

    // --- v2.4 Phase 4: the Spec-domain READ commands (`spec_list` / `spec_detail`) -------

    /// Create a project through the dispatch and return its id (a helper for the Spec reads).
    fn make_project(h: &Harness, name: &str, root: &str) -> String {
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": name, "rootPath": root }),
            )
            .unwrap();
        let created: serde_json::Value = serde_json::from_str(&created).unwrap();
        created["project"]["id"].as_str().unwrap().to_string()
    }

    /// `spec_list` returns the project's LIVE Specs with the per-Spec task tally + project key.
    /// A pure READ — no `workspaces://changed` push.
    #[test]
    fn spec_list_returns_specs_with_task_tally() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-spec-list");
        let pushes_before = h.pushes();

        // Seed a Spec + two tasks (one done) directly through the shared `nyx_core::db`
        // pool — the Spec authoring path is the Phase-5 MCP tools, out of THIS phase's scope.
        let (spec_id, t1_id) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) =
                    nyx_core::db::create_spec(c, &project_id, "First spec", Default::default())?;
                let slice_of = |sid: &str| nyx_core::db::NewTaskInput {
                    spec_id: Some(sid.to_string()),
                    ..nyx_core::db::NewTaskInput::slice()
                };
                let t1 = nyx_core::db::create_task(c, &project_id, "Task one", slice_of(&spec.id))?;
                nyx_core::db::create_task(c, &project_id, "Task two", slice_of(&spec.id))?;
                Ok::<_, nyx_core::db::ContentWriteError>((spec.id, t1.id))
            })
            .unwrap();
        // Mark task one done (start → done) so the tally is 1/2. The guarded transition owns
        // its own transaction + error type, so it runs outside the seed closure above.
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::transition_task(c, &t1_id, nyx_core::db::TaskTransition::Start, None)
            })
            .unwrap();
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::transition_task(c, &t1_id, nyx_core::db::TaskTransition::Done, None)
            })
            .unwrap();

        let result = h
            .run("spec_list", serde_json::json!({ "projectId": project_id }))
            .unwrap();
        let entries: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = entries.as_array().unwrap();
        assert_eq!(arr.len(), 1, "one Spec listed");
        let entry = &arr[0];
        assert_eq!(entry["id"].as_str().unwrap(), spec_id);
        assert_eq!(entry["title"].as_str().unwrap(), "First spec");
        assert_eq!(entry["tasks_total"].as_i64().unwrap(), 2);
        assert_eq!(entry["tasks_done"].as_i64().unwrap(), 1);
        assert!(
            !entry["project_key"].as_str().unwrap().is_empty(),
            "the project key is derived for the human key"
        );
        assert_eq!(
            h.pushes(),
            pushes_before,
            "spec_list is a pure read (no push)"
        );
    }

    /// DONE-CRITERION (task 01KXN0HVF8T0CKB6C575RECTXG): "Le renderer obtient le set de Specs de
    /// la session active d'un terminal." The `terminal_specs` command resolves a terminal → its
    /// ACTIVE session → the session's Spec set (a pure read, no push). A terminal with NO active
    /// session resolves to `{ session: null, specs: [] }` (no session ⇒ no overlay).
    #[test]
    fn terminal_specs_resolves_the_active_session_spec_set() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-terminal-specs");
        let pushes_before = h.pushes();

        // A terminal hosting a LIVE session that works ONE Spec.
        let (terminal_id, spec_id) = h
            .runtime
            .db()
            .with_conn(|c| {
                let terminal = nyx_core::db::create_terminal(c, "/work", None)?;
                let session = nyx_core::db::record_session_start(
                    c,
                    &terminal.id,
                    nyx_core::db::AGENT_KIND_CLAUDE_CODE,
                    nyx_core::db::SessionCapture {
                        external_session_id: "ext-1".into(),
                        cwd: "/work".into(),
                        ..Default::default()
                    },
                )?;
                let (spec, _rev) =
                    nyx_core::db::create_spec(c, &project_id, "Worked spec", Default::default())?;
                nyx_core::db::link_session_spec(c, &session.id, &spec.id)?;
                Ok::<_, diesel::result::Error>((terminal.id, spec.id))
            })
            .unwrap();

        let result = h
            .run(
                "terminal_specs",
                serde_json::json!({ "terminalId": terminal_id }),
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            value["session"]["state"].as_str().unwrap(),
            "active",
            "the terminal's active session is surfaced (the overlay's liveness axis)"
        );
        let specs = value["specs"].as_array().unwrap();
        assert_eq!(specs.len(), 1, "the active session's Spec set is returned");
        assert_eq!(specs[0]["id"].as_str().unwrap(), spec_id);
        assert_eq!(h.pushes(), pushes_before, "terminal_specs is a pure read");

        // A terminal with NO active session → an EMPTY resolution (no session ⇒ no overlay).
        let bare = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::db::create_terminal(c, "/bare", None))
            .unwrap();
        let empty = h
            .run(
                "terminal_specs",
                serde_json::json!({ "terminalId": bare.id }),
            )
            .unwrap();
        let empty: serde_json::Value = serde_json::from_str(&empty).unwrap();
        assert!(
            empty["session"].is_null(),
            "no active session ⇒ session is null"
        );
        assert_eq!(
            empty["specs"].as_array().unwrap().len(),
            0,
            "no session ⇒ an empty Spec set (no overlay)"
        );
    }

    /// `spec_detail` returns the Spec + its active revision + tasks + dependency edges. An
    /// unknown spec id is a readable error, not a panic.
    #[test]
    fn spec_detail_returns_spec_revision_and_tasks() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-spec-detail");

        let (spec_id, t2_id) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Detailed spec",
                    nyx_core::db::SpecSections {
                        problem: "the problem".to_string(),
                        ..Default::default()
                    },
                )?;
                let slice_of = |sid: &str| nyx_core::db::NewTaskInput {
                    spec_id: Some(sid.to_string()),
                    ..nyx_core::db::NewTaskInput::slice()
                };
                let t1 = nyx_core::db::create_task(c, &project_id, "First", slice_of(&spec.id))?;
                let t2 = nyx_core::db::create_task(c, &project_id, "Second", slice_of(&spec.id))?;
                // t2 depends on t1 (a `blocks` edge) — the detail must surface the edge.
                nyx_core::db::add_task_dependency(
                    c,
                    &t2.id,
                    &t1.id,
                    nyx_core::db::DEPENDENCY_TYPE_BLOCKS,
                )
                .map_err(|_| diesel::result::Error::RollbackTransaction)?;
                Ok::<_, nyx_core::db::ContentWriteError>((spec.id, t2.id))
            })
            .unwrap();

        let result = h
            .run("spec_detail", serde_json::json!({ "specId": spec_id }))
            .unwrap();
        let detail: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(detail["spec"]["id"].as_str().unwrap(), spec_id);
        assert_eq!(
            detail["revision"]["problem"].as_str().unwrap(),
            "the problem"
        );
        assert_eq!(detail["tasks"].as_array().unwrap().len(), 2);
        let deps = detail["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 1, "the one dependency edge is surfaced");
        assert_eq!(deps[0]["task_id"].as_str().unwrap(), t2_id);
        // No anchor posed → the derived workspace context is null (a sane state).
        assert!(detail["workspace_context"].is_null());

        // Unknown spec id → a readable error.
        let err = h
            .run("spec_detail", serde_json::json!({ "specId": "nope" }))
            .unwrap_err();
        assert!(err.contains("unknown spec"), "readable error: {err}");
    }

    /// `spec_activity` serves the SPEC-scoped, bounded, oldest-first `domain_events` fil over
    /// the UI bridge: the Spec's own lifecycle (creation, section edit, transition) + its
    /// tasks' events (add/transition), NEVER a terminal byte. A pure read (no push). An unknown
    /// spec id is a readable error.
    #[test]
    fn spec_activity_serves_the_spec_scoped_domain_events_read_model_over_the_ui_bridge() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-spec-activity");
        let pushes_before = h.pushes();

        let spec_id = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Spec",
                    nyx_core::db::SpecSections {
                        problem: "p".to_string(),
                        solution: "s".to_string(),
                        ..Default::default()
                    },
                )?;
                nyx_core::db::update_active_revision_section(
                    c,
                    &spec.id,
                    nyx_core::db::SpecSection::Problem,
                    "edited",
                )?;
                let task = nyx_core::db::create_task(
                    c,
                    &project_id,
                    "T1",
                    nyx_core::db::NewTaskInput {
                        spec_id: Some(spec.id.clone()),
                        ..nyx_core::db::NewTaskInput::slice()
                    },
                )?;
                nyx_core::db::transition_task(
                    c,
                    &task.id,
                    nyx_core::db::TaskTransition::Start,
                    None,
                )
                .map_err(|_| diesel::result::Error::RollbackTransaction)?;
                nyx_core::db::transition_spec(
                    c,
                    &spec.id,
                    nyx_core::db::SpecTransition::Ready,
                    true,
                )
                .map_err(|_| diesel::result::Error::RollbackTransaction)?;
                Ok::<_, nyx_core::db::ContentWriteError>(spec.id)
            })
            .unwrap();

        let result = h
            .run("spec_activity", serde_json::json!({ "specId": spec_id }))
            .unwrap();
        let activity: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(activity["spec_id"].as_str().unwrap(), spec_id);
        let types: Vec<&str> = activity["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["event_type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            vec![
                "spec.created",
                "spec.section_updated",
                "task.added",
                "task.transitioned",
                "spec.transitioned",
            ],
            "the Spec's own + its task's events, oldest-first — not just project.created"
        );
        // Every entry carries the concerned spec_id in its small business payload.
        assert!(activity["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["payload"].as_str().unwrap().contains(&spec_id)));
        assert_eq!(
            h.pushes(),
            pushes_before,
            "spec_activity is a pure read (no push)"
        );

        // The `limit` bounds the read.
        let bounded = h
            .run(
                "spec_activity",
                serde_json::json!({ "specId": spec_id, "limit": 2 }),
            )
            .unwrap();
        let bounded: serde_json::Value = serde_json::from_str(&bounded).unwrap();
        assert_eq!(bounded["limit"], serde_json::json!(2));
        assert_eq!(bounded["entries"].as_array().unwrap().len(), 2);

        // Unknown spec id → a readable error.
        let err = h
            .run("spec_activity", serde_json::json!({ "specId": "nope" }))
            .unwrap_err();
        assert!(err.contains("unknown spec"), "readable error: {err}");
    }

    /// DONE-CRITERION (task 01KX0F6HEBPK34CV49EREHK3Y9): the UI façade's review-comment
    /// db-commands post/thread/list, RESOLUTION is human-only (a resolve stamps
    /// resolved_by_type=human), and each mutation broadcasts `repos://changed` so an open
    /// review re-pulls the feed live. Drives the REAL `dispatch` end-to-end.
    #[test]
    fn review_comment_db_commands_post_thread_list_resolve_human_only() {
        let h = Harness::new();

        // Post a HUMAN anchored root.
        let root = h
            .run(
                "review_comment_post",
                serde_json::json!({
                    "workspaceId": "ws",
                    "repositoryId": "repo",
                    "body": "this is wrong",
                    "filePath": "src/a.rs",
                    "side": "new",
                    "lineStart": 5,
                    "lineEnd": 6,
                }),
            )
            .unwrap();
        let root: serde_json::Value = serde_json::from_str(&root).unwrap();
        assert_eq!(
            root["author_type"],
            serde_json::json!("human"),
            "UI posts as human"
        );
        assert_eq!(root["file_path"], serde_json::json!("src/a.rs"));
        assert_eq!(root["line_start"], serde_json::json!(5));
        let root_id = root["id"].as_str().unwrap().to_string();

        // A comment mutation broadcasts repos://changed (the UI re-pulls the feed).
        assert_eq!(
            changed_count(&h.buf, "repos"),
            1,
            "post broadcasts repos://changed"
        );

        // Reply (thread under the root).
        h.run(
            "review_comment_post",
            serde_json::json!({
                "workspaceId": "ws",
                "repositoryId": "repo",
                "parentId": root_id,
                "body": "agreed",
            }),
        )
        .unwrap();
        assert_eq!(changed_count(&h.buf, "repos"), 2, "reply broadcasts too");

        // List: the auto-threaded feed (root + reply), oldest-first.
        let listed = h
            .run(
                "review_comment_list",
                serde_json::json!({ "workspaceId": "ws", "repositoryId": "repo" }),
            )
            .unwrap();
        let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
        let arr = listed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], serde_json::json!(root_id));
        assert_eq!(arr[1]["parent_id"], serde_json::json!(root_id));

        // RESOLVE — human-only: stamps resolved_by_type=human.
        let resolved = h
            .run(
                "review_comment_resolve",
                serde_json::json!({ "id": root_id }),
            )
            .unwrap();
        let resolved: serde_json::Value = serde_json::from_str(&resolved).unwrap();
        assert_eq!(resolved["status"], serde_json::json!("resolved"));
        assert_eq!(resolved["resolved_by_type"], serde_json::json!("human"));
        assert_eq!(changed_count(&h.buf, "repos"), 3, "resolve broadcasts too");

        // Re-open clears it.
        let reopened = h
            .run(
                "review_comment_reopen",
                serde_json::json!({ "id": root_id }),
            )
            .unwrap();
        let reopened: serde_json::Value = serde_json::from_str(&reopened).unwrap();
        assert_eq!(reopened["status"], serde_json::json!("open"));
        assert!(reopened["resolved_by_type"].is_null());
    }

    /// FINDING 41 (comments leak between Specs): `review_comment_post` carries the `specId` and
    /// `review_comment_list` FILTERS by it, so a comment posted while reviewing Spec A appears on
    /// Spec A ONLY — never on Spec B, even though both share the same `(workspace, repository)`.
    /// A reply INHERITS its root's Spec, and an unscoped list (no `specId`) still sees everything.
    #[test]
    fn review_comment_db_commands_scope_by_spec() {
        let h = Harness::new();

        // A comment posted while reviewing Spec A.
        let a = h
            .run(
                "review_comment_post",
                serde_json::json!({
                    "workspaceId": "ws",
                    "repositoryId": "repo",
                    "specId": "specA",
                    "body": "A-scoped remark",
                }),
            )
            .unwrap();
        let a: serde_json::Value = serde_json::from_str(&a).unwrap();
        assert_eq!(a["spec_id"], serde_json::json!("specA"));
        let a_id = a["id"].as_str().unwrap().to_string();

        // A reply WITHOUT an explicit specId inherits specA from its root.
        let reply = h
            .run(
                "review_comment_post",
                serde_json::json!({
                    "workspaceId": "ws",
                    "repositoryId": "repo",
                    "parentId": a_id,
                    "body": "inherits A",
                }),
            )
            .unwrap();
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(
            reply["spec_id"],
            serde_json::json!("specA"),
            "reply inherits root spec"
        );

        // A comment posted while reviewing Spec B (same workspace+repo).
        h.run(
            "review_comment_post",
            serde_json::json!({
                "workspaceId": "ws",
                "repositoryId": "repo",
                "specId": "specB",
                "body": "B-scoped remark",
            }),
        )
        .unwrap();

        // Spec A's scoped feed = its root + its inherited reply (NOT B's comment).
        let list_a = h
            .run(
                "review_comment_list",
                serde_json::json!({ "workspaceId": "ws", "repositoryId": "repo", "specId": "specA" }),
            )
            .unwrap();
        let list_a: serde_json::Value = serde_json::from_str(&list_a).unwrap();
        let arr_a = list_a.as_array().unwrap();
        assert_eq!(
            arr_a.len(),
            2,
            "Spec A sees only its own thread (no leak from B)"
        );
        assert!(arr_a
            .iter()
            .all(|c| c["spec_id"] == serde_json::json!("specA")));

        // Spec B's scoped feed = only B's comment.
        let list_b = h
            .run(
                "review_comment_list",
                serde_json::json!({ "workspaceId": "ws", "repositoryId": "repo", "specId": "specB" }),
            )
            .unwrap();
        let list_b: serde_json::Value = serde_json::from_str(&list_b).unwrap();
        assert_eq!(
            list_b.as_array().unwrap().len(),
            1,
            "Spec B sees only its own comment"
        );

        // The UNSCOPED read (no specId) still returns the whole repo feed (agent façade / aggregate).
        let list_all = h
            .run(
                "review_comment_list",
                serde_json::json!({ "workspaceId": "ws", "repositoryId": "repo" }),
            )
            .unwrap();
        let list_all: serde_json::Value = serde_json::from_str(&list_all).unwrap();
        assert_eq!(
            list_all.as_array().unwrap().len(),
            3,
            "unscoped read sees all three"
        );
    }

    /// v2.5.11 review follow-up (findings 01KXZYWEAMCVY9ASHRHTFX4FZA + 01KXZYWFARPQXA9FVBPEZ9QV3J +
    /// #31 grill collapse): a finding is AGENT-OWNED and AGENT-CLOSABLE at the bridge. A human posts
    /// an ANCHORED comment (`review_comment_post`, author=human), the agent grills it into a finding
    /// (`review_task_promote` — the finding carries NO `origin`), STRUCTURES it (coder-ready — the
    /// grill has no human-only validate step), and CLOSES it through the plain `db::transition_task`
    /// done path WITHOUT any human-only refusal. With the finding done, its source comment resolves
    /// (human-only, guarded on findings). Drives the REAL `dispatch` end-to-end.
    #[test]
    fn human_comment_promotes_finding_agent_closes_then_comment_resolves() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-finding-resolve");
        // A Spec to hang the finding on — promotion needs the comment's Spec (a comment with no
        // Spec cannot promote). Seed it through the shared pool (authoring is out of scope here).
        let spec_id = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::create_spec(c, &project_id, "Spec", Default::default())
                    .map(|(s, _)| s.id)
            })
            .unwrap();

        // 1. VIEWER channel: a HUMAN ANCHORED comment (file/line) through the UI façade.
        let comment = h
            .run(
                "review_comment_post",
                serde_json::json!({
                    "workspaceId": "ws",
                    "repositoryId": "repo",
                    "specId": spec_id,
                    "body": "this branch is wrong",
                    "filePath": "src/loop.rs",
                    "side": "new",
                    "lineStart": 12,
                    "lineEnd": 14,
                }),
            )
            .unwrap();
        let comment: serde_json::Value = serde_json::from_str(&comment).unwrap();
        assert_eq!(comment["author_type"], serde_json::json!("human"));
        assert_eq!(comment["file_path"], serde_json::json!("src/loop.rs"));
        let comment_id = comment["id"].as_str().unwrap().to_string();

        // 2. GRILL: the agent promotes the comment into a finding (agent-owned — NO origin; the human
        //    provenance stays on the comment's author_type).
        let finding = h
            .run(
                "review_task_promote",
                serde_json::json!({ "commentId": comment_id, "severity": "high" }),
            )
            .unwrap();
        let finding: serde_json::Value = serde_json::from_str(&finding).unwrap();
        assert!(
            finding.get("origin").is_none(),
            "a finding carries NO origin (v24 — always agent-created)"
        );
        assert_eq!(
            finding["from_comment_id"],
            serde_json::json!(comment_id),
            "finding trails its comment"
        );
        let finding_id = finding["id"].as_str().unwrap().to_string();
        // The promotion CREATED a `tasks kind=review` finding, so besides `repos` it ALSO pushed
        // `specs://changed` for the overlay / Spec detail (review F3). The prior comment_post was a
        // pure comment (repos only), so this is the FIRST `specs` push.
        assert_eq!(
            changed_count(&h.buf, "specs"),
            1,
            "review_task_promote broadcasts specs://changed (overlay live)"
        );

        // 3. STRUCTURE (agent): the grill's final step — a structured finding is coder-ready (#31;
        //    no human-only validate step). Driven through the shared engine the MCP tool wraps.
        let structured = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::structure_finding(c, &finding_id, "Fix the wrong branch", None, None)
            })
            .expect("structure ok")
            .expect("a proposed finding is structurable");
        assert!(
            structured.is_finding_ready_for_coder(),
            "a structured finding is ready for the coder (the grill's final state, #31)"
        );

        // 4. The AGENT coder loop CLOSES the finding through the plain `done` path — no human-only
        //    refusal anymore (v24 dropped the guard). Start then Done, both succeed.
        let closed = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::transition_task(
                    c,
                    &finding_id,
                    nyx_core::db::TaskTransition::Start,
                    None,
                )?;
                nyx_core::db::transition_task(
                    c,
                    &finding_id,
                    nyx_core::db::TaskTransition::Done,
                    None,
                )
            })
            .expect("the agent closes the finding via the plain done path");
        assert_eq!(
            closed.status,
            nyx_core::db::TASK_STATUS_DONE,
            "the finding is agent-closable (no human-only gate)"
        );
        assert!(closed.completed_at.is_some(), "completion is stamped");

        // 5. With its finding done, the source comment now resolves (comment resolution is
        //    human-only, guarded on findings).
        let comment_resolved = h
            .run(
                "review_comment_resolve",
                serde_json::json!({ "id": comment_id }),
            )
            .unwrap();
        let comment_resolved: serde_json::Value = serde_json::from_str(&comment_resolved).unwrap();
        assert_eq!(comment_resolved["status"], serde_json::json!("resolved"));
        assert_eq!(
            comment_resolved["resolved_by_type"],
            serde_json::json!("human")
        );
    }

    // --- v2.6 Phase 4: the KNOWLEDGE READ commands (Idées view + Spec-detail annexes/deps) ----

    /// `idea_themes` groups a project's backlog by theme with the total + per-status breakdown
    /// (biggest group first, ungrouped folded into a `theme = null` row). Backs the Idées group
    /// headers (counter + status mini-dots) + the theme facet. A pure READ — no push.
    #[test]
    fn idea_themes_groups_backlog_by_theme_with_status_breakdown() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-idea-themes");
        let pushes_before = h.pushes();

        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::create_idea(c, &project_id, "RBAC one", "b", Some("RBAC"), "manual")?;
                nyx_core::db::create_idea(c, &project_id, "RBAC two", "b", Some("RBAC"), "manual")?;
                let ot =
                    nyx_core::db::create_idea(c, &project_id, "OT", "b", Some("OT"), "manual")?;
                // Close the OT idea so its theme's breakdown shows a non-open bucket.
                nyx_core::db::close_idea(
                    c,
                    &ot.id,
                    nyx_core::db::IDEA_STATUS_IMPLEMENTED,
                    Some("done"),
                )?;
                Ok::<_, diesel::result::Error>(())
            })
            .unwrap();

        let result = h
            .run(
                "idea_themes",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap();
        let themes: serde_json::Value = serde_json::from_str(&result).unwrap();
        let themes = themes.as_array().unwrap();
        // Biggest group first: RBAC (2) before OT (1).
        assert_eq!(themes[0]["theme"].as_str().unwrap(), "RBAC");
        assert_eq!(themes[0]["total"].as_i64().unwrap(), 2);
        assert_eq!(themes[0]["open"].as_i64().unwrap(), 2);
        assert_eq!(themes[1]["theme"].as_str().unwrap(), "OT");
        assert_eq!(themes[1]["implemented"].as_i64().unwrap(), 1);
        // A pure read pushed nothing.
        assert_eq!(h.pushes(), pushes_before);
    }

    /// `idea_list` returns the bounded backlog rows ENRICHED with each idea's linked Specs (so a
    /// row shows its Spec badges and the drawer renders detail from the same item), and honours the
    /// `status`/`theme` filters. A pure READ — no push.
    #[test]
    fn idea_list_returns_bounded_rows_with_linked_specs() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-idea-list");

        let (idea_id, spec_id) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) =
                    nyx_core::db::create_spec(c, &project_id, "Linked spec", Default::default())?;
                let idea = nyx_core::db::create_idea(
                    c,
                    &project_id,
                    "Idea A",
                    "body",
                    Some("RBAC"),
                    "manual",
                )?;
                nyx_core::db::create_idea(c, &project_id, "Idea B", "b", Some("OT"), "manual")?;
                nyx_core::db::link_idea_spec(
                    c,
                    &idea.id,
                    &spec.id,
                    nyx_core::db::IDEA_KIND_ADDRESSES,
                )?;
                Ok::<_, diesel::result::Error>((idea.id, spec.id))
            })
            .unwrap();

        // Unfiltered — both ideas.
        let all = h
            .run("idea_list", serde_json::json!({ "projectId": project_id }))
            .unwrap();
        let all: serde_json::Value = serde_json::from_str(&all).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);

        // Filter by theme RBAC — one idea, ENRICHED with its linked Spec.
        let rbac = h
            .run(
                "idea_list",
                serde_json::json!({ "projectId": project_id, "theme": "RBAC" }),
            )
            .unwrap();
        let rbac: serde_json::Value = serde_json::from_str(&rbac).unwrap();
        let rbac = rbac.as_array().unwrap();
        assert_eq!(rbac.len(), 1);
        assert_eq!(rbac[0]["id"].as_str().unwrap(), idea_id);
        let linked = rbac[0]["linked_specs"].as_array().unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0]["spec_id"].as_str().unwrap(), spec_id);
        assert_eq!(linked[0]["kind"].as_str().unwrap(), "addresses");
    }

    /// `idea_search` serves the FTS5 + fuzzy scored search (the SAME engine the MCP tool serves):
    /// a query surfaces the matching idea with a score + its linked Specs. A pure READ — no push.
    #[test]
    fn idea_search_returns_scored_hits() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-idea-search");

        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::create_idea(
                    c,
                    &project_id,
                    "Cache permission resolution",
                    "the guard re-resolves permissions each request",
                    Some("Perf"),
                    "manual",
                )?;
                nyx_core::db::create_idea(
                    c,
                    &project_id,
                    "Unify opening days",
                    "luxon 1-7",
                    Some("Sites"),
                    "manual",
                )?;
                Ok::<_, diesel::result::Error>(())
            })
            .unwrap();

        let result = h
            .run(
                "idea_search",
                serde_json::json!({ "projectId": project_id, "query": "permission cache" }),
            )
            .unwrap();
        let hits: serde_json::Value = serde_json::from_str(&result).unwrap();
        let hits = hits.as_array().unwrap();
        assert!(!hits.is_empty(), "the permission-cache idea is surfaced");
        assert_eq!(
            hits[0]["title"].as_str().unwrap(),
            "Cache permission resolution"
        );
        assert!(hits[0]["score"].as_f64().unwrap() > 0.0);
    }

    /// `annexe_list` returns a Spec's annexes newest-first, each with its blob `byte_len` (no
    /// content in the list). Backs the Spec-detail "Annexes" block. A pure READ — no push.
    #[test]
    fn annexe_list_returns_spec_annexes() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-annexe-list");

        let spec_id = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Spec with annexes",
                    Default::default(),
                )?;
                nyx_core::db::add_annexe(
                    c,
                    &spec.id,
                    nyx_core::db::ANNEX_KIND_PROTOTYPE,
                    "Mockup L1",
                    "<html>mockup</html>",
                )?;
                nyx_core::db::add_annexe(
                    c,
                    &spec.id,
                    nyx_core::db::ANNEX_KIND_URL,
                    "Design ref",
                    "https://example.test/design",
                )?;
                Ok::<_, diesel::result::Error>(spec.id)
            })
            .unwrap();

        let result = h
            .run("annexe_list", serde_json::json!({ "specId": spec_id }))
            .unwrap();
        let annexes: serde_json::Value = serde_json::from_str(&result).unwrap();
        let annexes = annexes.as_array().unwrap();
        assert_eq!(annexes.len(), 2);
        // Newest-first: the URL annexe was added last.
        assert_eq!(annexes[0]["kind"].as_str().unwrap(), "url");
        assert_eq!(annexes[0]["title"].as_str().unwrap(), "Design ref");
        assert!(annexes[0]["byte_len"].as_i64().unwrap() > 0);
        assert!(
            annexes[0].get("content").is_none(),
            "the list never ships the blob content"
        );
    }

    /// `spec_dependency_view` returns the directional `blocked_by`/`unblocks` edges + the badge
    /// counts; `spec_list` surfaces the SAME counts per card. Backs the Spec-detail + board badges.
    /// A pure READ — no push.
    #[test]
    fn spec_dependency_view_and_list_surface_badge_counts() {
        let h = Harness::new();
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-spec-deps");

        // B depends on A (A blocks B): A "débloque" B, B "bloquée par" A.
        let (a_id, b_id) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (a, _) =
                    nyx_core::db::create_spec(c, &project_id, "Spec A", Default::default())?;
                let (b, _) =
                    nyx_core::db::create_spec(c, &project_id, "Spec B", Default::default())?;
                Ok::<_, diesel::result::Error>((a.id, b.id))
            })
            .unwrap();
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::add_spec_dependency(
                    c,
                    &b_id,
                    &a_id,
                    nyx_core::db::SPEC_DEP_KIND_BLOCKS,
                    None,
                )
                .map_err(|_| diesel::result::Error::RollbackTransaction)
            })
            .unwrap();

        // B's view: bloquée par 1 (A), débloque 0.
        let view_b = h
            .run(
                "spec_dependency_view",
                serde_json::json!({ "specId": b_id }),
            )
            .unwrap();
        let view_b: serde_json::Value = serde_json::from_str(&view_b).unwrap();
        assert_eq!(view_b["blocked_by_count"].as_i64().unwrap(), 1);
        assert_eq!(view_b["unblocks_count"].as_i64().unwrap(), 0);
        assert_eq!(view_b["blocked_by"][0]["spec_id"].as_str().unwrap(), a_id);

        // The board `spec_list` carries the same counts per card.
        let list = h
            .run("spec_list", serde_json::json!({ "projectId": project_id }))
            .unwrap();
        let list: serde_json::Value = serde_json::from_str(&list).unwrap();
        let entries = list.as_array().unwrap();
        let by_id = |id: &str| {
            entries
                .iter()
                .find(|e| e["id"].as_str() == Some(id))
                .unwrap()
                .clone()
        };
        assert_eq!(by_id(&b_id)["blocked_by_count"].as_i64().unwrap(), 1);
        assert_eq!(by_id(&a_id)["unblocks_count"].as_i64().unwrap(), 1);
    }

    /// v2.8 Phase 6 — the ENGINE GRAPH read models the React Flow view consumes.
    /// `engine_workflow_list` surfaces the seeded built-in flows; `engine_workflow_graph` returns a
    /// workflow + its node dump (the tree the renderer reconstructs); `engine_run_list` surfaces a
    /// run's cursor + blackboard; `engine_effector_context` renders the effector manual for a spawn
    /// node's role. All PURE READS — none pushes an invalidation.
    #[test]
    fn engine_read_commands_serve_the_graph_view() {
        let h = Harness::new();

        // The built-in defaults (ndev/nspec/ndoc) are seeded at `Db::open`.
        let list = h
            .run("engine_workflow_list", serde_json::json!({}))
            .unwrap();
        let workflows: serde_json::Value = serde_json::from_str(&list).unwrap();
        let ndev = workflows
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["scope"] == "ndev")
            .expect("the ndev built-in is listed");
        let wf_id = ndev["id"].as_str().unwrap().to_string();

        // The graph: the workflow + its nodes (the reconstructible tree — spawn leaf + phase loop).
        let graph = h
            .run(
                "engine_workflow_graph",
                serde_json::json!({ "workflowId": wf_id }),
            )
            .unwrap();
        let graph: serde_json::Value = serde_json::from_str(&graph).unwrap();
        let nodes = graph["nodes"].as_array().unwrap();
        assert!(
            nodes.iter().any(|n| n["type_"] == "spawn_subagent"),
            "the ndev graph carries a spawn node"
        );
        assert!(
            nodes.iter().any(|n| n["type_"] == "loop"),
            "and its phase loop composite"
        );
        // An unknown workflow id is a readable error (the UI shows an empty-state).
        assert!(h
            .run(
                "engine_workflow_graph",
                serde_json::json!({ "workflowId": "nope" })
            )
            .is_err());

        // A run bound to a Spec, parked on the spawn node with a blackboard.
        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-engine");
        let (spec_id, run_current) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) =
                    nyx_core::db::create_spec(c, &project_id, "Checkout", Default::default())?;
                let cursor = nyx_core::db::list_nodes(c, &wf_id)?
                    .into_iter()
                    .find(|n| n.type_ == "spawn_subagent")
                    .map(|n| n.id);
                let run = nyx_core::db::create_workflow_run(
                    c,
                    &wf_id,
                    Some(&spec.id),
                    cursor.as_deref(),
                    "{\"phase\":1}",
                )?;
                Ok::<_, diesel::result::Error>((spec.id, run.current_node_id))
            })
            .unwrap();

        let reads_before = h.pushes();

        // engine_run_list → the run's cursor + blackboard (the runtime overlay reads them).
        let runs = h
            .run(
                "engine_run_list",
                serde_json::json!({ "workflowId": wf_id }),
            )
            .unwrap();
        let runs: serde_json::Value = serde_json::from_str(&runs).unwrap();
        let runs = runs.as_array().unwrap();
        assert_eq!(runs.len(), 1, "the workflow's run is listed");
        assert_eq!(runs[0]["current_node_id"], serde_json::json!(run_current));
        assert_eq!(runs[0]["blackboard"].as_str().unwrap(), "{\"phase\":1}");

        // engine_effector_context → the coder manual rendered for that Spec (the inspector).
        let ctx = h
            .run(
                "engine_effector_context",
                serde_json::json!({ "role": "coder", "specId": spec_id }),
            )
            .unwrap();
        let ctx: serde_json::Value = serde_json::from_str(&ctx).unwrap();
        assert!(
            ctx["context"].as_str().unwrap().contains("Coder"),
            "the coder effector manual is rendered"
        );
        // finding #35: WITHOUT a specId (no run bound), the inspector still gets the STATIC role
        // template — the sub-agent's brief — with the run-specific parts marked "resolved at run".
        let template = h
            .run(
                "engine_effector_context",
                serde_json::json!({ "role": "coder" }),
            )
            .unwrap();
        let template: serde_json::Value = serde_json::from_str(&template).unwrap();
        let body = template["context"].as_str().unwrap();
        assert!(
            body.contains("Context: Coder Agent") && body.contains("(role template)"),
            "the static role template renders with no run bound",
        );
        assert!(
            body.contains("resolved at run"),
            "its run-specific parts are marked resolved at run",
        );
        // An unknown role is a readable error (the inspector falls back to the node config).
        assert!(h
            .run(
                "engine_effector_context",
                serde_json::json!({ "role": "bogus", "specId": spec_id }),
            )
            .is_err());

        assert_eq!(
            h.pushes(),
            reads_before,
            "engine reads never push an invalidation"
        );
    }

    /// `engine_spec_runs` — the SPEC-side half of the runtime read model, mirroring the
    /// `engine_run_list` leg above from the other end. The Spec run modal (opened from
    /// `/work/$specId` and from the dev overlay) starts from a SPEC, and a Spec is driven by its
    /// ndev run PLUS the child-workflow runs ndev spawns — so the command lists the Spec's runs
    /// ACROSS workflows, newest-first. Another Spec's run stays out, an unknown Spec is an EMPTY
    /// list (the modal's empty state), and — like every engine read — it pushes no invalidation.
    #[test]
    fn engine_spec_runs_lists_a_specs_runs_across_workflows() {
        let h = Harness::new();

        // The seeded ndev built-in + a project-owned child workflow, both running the SAME Spec.
        let list = h
            .run("engine_workflow_list", serde_json::json!({}))
            .unwrap();
        let workflows: serde_json::Value = serde_json::from_str(&list).unwrap();
        let ndev_id = workflows
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["scope"] == "ndev")
            .expect("the ndev built-in is listed")["id"]
            .as_str()
            .unwrap()
            .to_string();

        let project_id = make_project(&h, "Nyx", "/tmp/dbcmd-spec-runs");
        let (spec_id, other_spec_id, parent_run, child_run, foreign_run) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) =
                    nyx_core::db::create_spec(c, &project_id, "Checkout", Default::default())?;
                let (other, _rev2) =
                    nyx_core::db::create_spec(c, &project_id, "Autre", Default::default())?;
                let child_wf = nyx_core::db::create_workflow(
                    c,
                    Some(&project_id),
                    "coder",
                    nyx_core::db::WORKFLOW_SCOPE_CUSTOM,
                    None,
                )?;
                let parent = nyx_core::db::create_workflow_run(
                    c,
                    &ndev_id,
                    Some(&spec.id),
                    None,
                    "{\"phase\":1}",
                )?;
                let child =
                    nyx_core::db::create_workflow_run(c, &child_wf.id, Some(&spec.id), None, "{}")?;
                let foreign =
                    nyx_core::db::create_workflow_run(c, &ndev_id, Some(&other.id), None, "{}")?;
                // An UNBOUND run on ndev — it belongs to no Spec and must never surface.
                nyx_core::db::create_workflow_run(c, &ndev_id, None, None, "{}")?;
                Ok::<_, diesel::result::Error>((spec.id, other.id, parent.id, child.id, foreign.id))
            })
            .unwrap();

        let reads_before = h.pushes();

        let runs = h
            .run("engine_spec_runs", serde_json::json!({ "specId": spec_id }))
            .unwrap();
        let runs: serde_json::Value = serde_json::from_str(&runs).unwrap();
        let runs = runs.as_array().unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 2, "both of the Spec's runs are listed");
        assert!(
            ids.contains(&parent_run.as_str()) && ids.contains(&child_run.as_str()),
            "the ndev run AND the child-workflow run both surface for the Spec"
        );
        assert!(
            !ids.contains(&foreign_run.as_str()),
            "another Spec's run — and the unbound run — stay out"
        );
        // Newest-first: the child run was created last (same ms ⇒ the `id DESC` tiebreak still
        // orders the UUIDv7s), and its blackboard rides along for the modal's panel.
        assert!(runs
            .iter()
            .any(|r| r["blackboard"].as_str() == Some("{\"phase\":1}")));

        // The other Spec sees only ITS run; an unknown Spec is an empty list, not an error.
        let other = h
            .run(
                "engine_spec_runs",
                serde_json::json!({ "specId": other_spec_id }),
            )
            .unwrap();
        let other: serde_json::Value = serde_json::from_str(&other).unwrap();
        assert_eq!(other.as_array().unwrap().len(), 1);
        let unknown = h
            .run("engine_spec_runs", serde_json::json!({ "specId": "nope" }))
            .unwrap();
        let unknown: serde_json::Value = serde_json::from_str(&unknown).unwrap();
        assert!(unknown.as_array().unwrap().is_empty());

        assert_eq!(
            h.pushes(),
            reads_before,
            "engine_spec_runs never pushes an invalidation"
        );
    }

    /// v2.8 Phase 7 GATE (clause 4 — "visualiser runtime fidèle", the IPC-read-command half): the
    /// `engine_run_list` / `engine_workflow_graph` read-commands the React Flow view consumes
    /// reflect a LIVE `nyx_next` run FAITHFULLY. We drive the SEEDED ndev through the REAL Phase-3
    /// `nyx_next` verb over the same `Db` the read-commands read, then read back over the sidecar
    /// transport: at each tick `engine_run_list` exposes the EXACT `current_node_id` + `blackboard`
    /// nyx_next persisted, and `engine_workflow_graph` carries that cursor node (so the renderer's
    /// `build-graph` can locate + highlight it). Composes with `build-graph.test.ts` (the highlight)
    /// into end-to-end runtime fidelity.
    #[test]
    fn engine_run_list_reflects_a_live_nyx_next_run() {
        let h = Harness::new();

        // The seeded ndev built-in (seeded at `Db::open`).
        let list = h
            .run("engine_workflow_list", serde_json::json!({}))
            .unwrap();
        let workflows: serde_json::Value = serde_json::from_str(&list).unwrap();
        let wf_id = workflows
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["scope"] == "ndev")
            .expect("the ndev built-in is listed")["id"]
            .as_str()
            .unwrap()
            .to_string();

        // A run on ndev bound to a READY Spec carrying one open build task, so the entry `start` fires
        // inline (ready → in_progress), the build gate (`spec_open_tasks` — there IS work) routes into
        // the build, and the first SURFACED action is the coder SUB-WORKFLOW (lot 2 — the build is
        // delegated to a coder child; ndev keeps the audit/review/Spec-transitions).
        let (project_id, spec_id) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (project, _root) =
                    nyx_core::db::create_project(c, "P", "/tmp/sidecar-ndev", None).unwrap();
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project.id,
                    "S",
                    nyx_core::db::SpecSections {
                        problem: "p".into(),
                        solution: "s".into(),
                        ..Default::default()
                    },
                )
                .unwrap();
                nyx_core::db::create_task(
                    c,
                    &project.id,
                    "t0",
                    nyx_core::db::NewTaskInput {
                        spec_id: Some(spec.id.clone()),
                        phase_name: Some("Build".to_string()),
                        ..nyx_core::db::NewTaskInput::slice()
                    },
                )
                .unwrap();
                nyx_core::db::transition_spec(c, &spec.id, nyx_core::db::SpecTransition::Ready, true)
                    .unwrap();
                (project.id, spec.id)
            });

        // ARM the coder child's per-task gate. The seeded coder ships it BLOCKING and declaring the
        // `build_check` SLOT, and `nyx_next` REFUSES to advance past a gate nothing answers (it would
        // certify a build nothing checked) — so take the route the refusal names: register the check
        // and BIND it to this project's slot. Nothing is forked, so this project keeps running (and
        // keeps receiving upgrades of) nyx's own coder graph.
        std::fs::create_dir_all("/tmp/sidecar-ndev").expect("the gate needs a real cwd");
        let gate_instance = h.runtime.db().with_conn(|c| {
            let tpl = nyx_core::db::create_template(
                c,
                &project_id,
                "gate",
                "true",
                None,
                Default::default(),
            )
            .unwrap();
            nyx_core::db::instance_ids_for_template(c, &tpl.id)
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
        });
        let authoring = |name: &str, args: serde_json::Value| -> serde_json::Value {
            nyx_core::mcp_authoring::dispatch_authoring_tool(h.runtime.db(), name, &args)
                .unwrap_or_else(|| panic!("`{name}` was not routed"))
                .unwrap_or_else(|e| panic!("`{name}` failed: {}", e.message))
                .result
        };
        authoring(
            "slot_bind",
            serde_json::json!({
                "project_id": project_id,
                "slot": nyx_core::seed::BUILD_CHECK_SLOT,
                "command_ids": [gate_instance],
            }),
        );

        let run_id = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::create_workflow_run(c, &wf_id, Some(&spec_id), None, "{}")
            })
            .unwrap()
            .id;

        // Drive the REAL `nyx_next` verb (Phase 3) over the SAME `Db` the read-commands serve. The
        // caller session is empty — this deroulé reaches no consent question, so nyx has no human
        // turn to look for.
        let rt = nyx_core::mcp_engine::EngineRuntime::new();
        let tick = |rid: &str, last: Option<serde_json::Value>| -> serde_json::Value {
            let mut args = serde_json::json!({ "run_id": rid });
            if let Some(l) = last {
                args.as_object_mut()
                    .unwrap()
                    .insert("last_result".into(), l);
            }
            let caller = nyx_core::mcp_engine::CallerSession { terminal_id: None };
            nyx_core::mcp_engine::nyx_next(h.runtime.db(), &rt, caller, &args)
                .expect("nyx_next")
                .result
        };

        // The IPC read-commands mirror the live cursor + the PERSISTED (full) blackboard — what the
        // React Flow view reads from the DB. N22 made the tick OUTPUT a per-tick delta, but `persist`
        // still writes the full board, so this read model is unaffected. Returns the persisted board.
        let read_reflects = |cursor: &str| -> serde_json::Value {
            let runs = h
                .run("engine_run_list", serde_json::json!({ "workflowId": wf_id }))
                .unwrap();
            let runs: serde_json::Value = serde_json::from_str(&runs).unwrap();
            let runs = runs.as_array().unwrap();
            assert_eq!(runs.len(), 1, "the workflow's single run is listed");
            assert_eq!(
                runs[0]["current_node_id"],
                serde_json::json!(cursor),
                "engine_run_list reflects the live cursor nyx_next persisted"
            );
            let graph = h
                .run("engine_workflow_graph", serde_json::json!({ "workflowId": wf_id }))
                .unwrap();
            let graph: serde_json::Value = serde_json::from_str(&graph).unwrap();
            assert!(
                graph["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|n| n["id"] == serde_json::json!(cursor)),
                "the graph dump carries the live cursor node — the renderer can highlight it"
            );
            serde_json::from_str(runs[0]["blackboard"].as_str().unwrap()).unwrap()
        };

        // Tick 1 → the entry query + start fold INLINE, then the run OPENS the coder child and parks on
        // the sub_workflow node.
        let out1 = tick(&run_id, None);
        assert_eq!(out1["action"]["type"], "sub_workflow");
        let child_run_id = out1["action"]["child_run_id"].as_str().unwrap().to_string();
        let cursor1 = out1["current_node_id"].as_str().unwrap().to_string();
        let _board1 = read_reflects(&cursor1);

        // Drive the coder child to done (it drives the phase's single task), like the real dispatcher.
        let mut steps = 0;
        let mut clast: Option<serde_json::Value> = None;
        loop {
            let cout = tick(&child_run_id, clast.take());
            match cout["action"]["type"].as_str().unwrap() {
                "done" => break,
                "spawn_subagent" => clast = Some(serde_json::json!({ "diff": "d" })),
                "note" => {}
                other => panic!("unexpected coder child action '{other}'"),
            }
            steps += 1;
            assert!(steps < 20, "runaway coder child");
        }

        // Tick the PARENT: it folds the child's VERIFIED result and parks on the AUDITOR spawn; the
        // PERSISTED board now carries the folded sub_result.
        let out2 = tick(&run_id, None);
        assert_eq!(out2["action"]["type"], "spawn_subagent");
        assert_eq!(out2["action"]["agent_id"], "auditor");
        let cursor2 = out2["current_node_id"].as_str().unwrap().to_string();
        assert_ne!(
            cursor2, cursor1,
            "the cursor MOVED — the read model tracks the live run, not a stale value"
        );
        let board2 = read_reflects(&cursor2);
        assert_eq!(
            board2["sub_result"]["status"],
            serde_json::json!("done"),
            "the persisted read model exposes the true folded state (the child's verified result)"
        );
    }

    /// v2.7 Phase 3 — the DESIGN-SYSTEM read/render seam over the real dispatch: `design_get`
    /// surfaces the theme tokens, `design_component_list` lists the components (DS + NEW) name-
    /// ordered, and `design_render` compiles a composition to markup in the sidecar (an unknown
    /// component being a READABLE error, never a silent empty frame).
    #[test]
    fn design_read_and_render_seam() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-design" }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        // Seed a design + two components (one DS with a source, one greenfield NEW) directly.
        h.runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::upsert_design(
                    c,
                    &project_id,
                    "# DESIGN",
                    Some("{\"colors\":{\"primary\":\"#111\"}}"),
                )?;
                nyx_core::db::upsert_component(
                    c,
                    &project_id,
                    &nyx_core::db::ComponentInput {
                        name: "Button".into(),
                        template: "<button>{{label}}</button>".into(),
                        source_path: Some("src/Button.tsx".into()),
                        ..Default::default()
                    },
                )?;
                nyx_core::db::upsert_component(
                    c,
                    &project_id,
                    &nyx_core::db::ComponentInput {
                        name: "New".into(),
                        template: "<div>new</div>".into(),
                        ..Default::default()
                    },
                )?;
                Ok::<_, diesel::result::Error>(())
            })
            .unwrap();

        // design_get → the derived theme tokens.
        let design = h
            .run("design_get", serde_json::json!({ "projectId": project_id }))
            .unwrap();
        let design: Value = serde_json::from_str(&design).unwrap();
        assert!(design["theme_tokens_json"]
            .as_str()
            .unwrap()
            .contains("primary"));

        // design_component_list → both components (name-ordered), no liés-page yet.
        let list = h
            .run(
                "design_component_list",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap();
        let list: Value = serde_json::from_str(&list).unwrap();
        let names: Vec<&str> = list["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Button", "New"]);
        assert!(list["linked_names"].as_array().unwrap().is_empty());

        // design_render → the sidecar compiles the composition to markup.
        let rendered = h
            .run(
                "design_render",
                serde_json::json!({
                    "projectId": project_id,
                    "compositionJson": "{\"component\":\"Button\",\"params\":{\"label\":\"OK\"}}"
                }),
            )
            .unwrap();
        let rendered: Value = serde_json::from_str(&rendered).unwrap();
        // …and the node NAMES ITSELF on its own root (`data-nyx-node`, finding
        // `01KYW77Y65A9HYNDZ69M67S6B8`): that mark is what lets the frame's shim answer "which node
        // of the named tree is this element?" without a second, marked-up render of the same
        // composition — and `node_count` is the count of the TREE, nothing the render added.
        assert_eq!(
            rendered["markup"].as_str().unwrap(),
            "<button data-nyx-node=\"Button\">OK</button>"
        );
        assert_eq!(rendered["node_count"], serde_json::json!(1));

        // An unknown component is a NAMED HOLE, not a blanked page (finding
        // `01KYT78699XG6Q6P3VZC64W8H8`): the render COMPLETES, the node names itself in place, and
        // `unrendered[]` carries the correction. A composition under design always holds `[NEW]`
        // nodes, so refusing the whole tree over one of them was refusing the normal case.
        let holed = h
            .run(
                "design_render",
                serde_json::json!({
                    "projectId": project_id,
                    "compositionJson": "{\"key\":\"Root\",\"children\":[{\"component\":\"Button\",\"params\":{\"label\":\"OK\"}},{\"component\":\"Nope\"}]}"
                }),
            )
            .unwrap();
        let holed: Value = serde_json::from_str(&holed).unwrap();
        let markup = holed["markup"].as_str().unwrap();
        assert!(
            markup.contains("<button data-nyx-node=\"Root/Button\">OK</button>"),
            "the renderable node still draws: {markup}"
        );
        assert!(
            markup.contains("data-unrendered-reason=\"not_in_catalogue\"")
                && markup.contains("Nope"),
            "the hole is named IN PLACE: {markup}"
        );
        // …and the HOLE designates itself like any other node: a `[NEW]` node is precisely the one a
        // reviewer clicks to open its params, so its placeholder carries the node mark too.
        assert!(
            markup.contains("data-nyx-node=\"Root/Nope\""),
            "the hole is a node one can click: {markup}"
        );
        assert_eq!(holed["unrendered_count"], serde_json::json!(1));
        assert_eq!(holed["unrendered"][0]["node_key"], "Root/Nope");
        assert_eq!(holed["unrendered"][0]["reason"], "not_in_catalogue");
        assert!(holed["unrendered"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("proto_author_component"));

        // A component carrying a SCOPED CSS SLOT hands the frame the mark AND the stylesheet, and
        // the IPC answer NAMES the exception (finding `01KYVMTWB71R0D0CT1SV88J8DX`) — the review
        // surface badges the node and counts the chip off THIS, never off a scan of the markup.
        let button_id = h
            .runtime
            .db()
            .with_conn(|c| db::get_component_by_name(c, &project_id, "Button"))
            .unwrap()
            .unwrap()
            .id;
        h.runtime
            .db()
            .with_conn(|c| {
                db::set_component_css_slot(
                    c,
                    &button_id,
                    &db::CssSlotWrite::Set(
                        nyx_core::css_slot::CssSlot {
                            css: "@keyframes pulse { 50% { opacity: .5 } } \
                                  .spin { animation: pulse 1s linear infinite }"
                                .into(),
                            reason: "a custom pulse keyframe".into(),
                        }
                        .to_json(),
                    ),
                )
            })
            .unwrap();
        let slotted = h
            .run(
                "design_render",
                serde_json::json!({
                    "projectId": project_id,
                    "compositionJson": "{\"component\":\"Button\",\"params\":{\"label\":\"OK\"}}"
                }),
            )
            .unwrap();
        let slotted: Value = serde_json::from_str(&slotted).unwrap();
        let scope = nyx_core::css_slot::component_scope_id("Button");
        let markup = slotted["markup"].as_str().unwrap();
        assert!(
            markup.starts_with(&format!(
                "<button data-nyx-node=\"Button\" data-nyx-css=\"{scope}\">"
            )) && markup.contains(&format!("<style data-nyx-css-slot=\"{scope}\">")),
            "the frame receives the mark AND the scoped stylesheet: {markup}"
        );
        assert_eq!(slotted["css_slot_count"], serde_json::json!(1));
        assert_eq!(slotted["css_slots"][0]["component"], "Button");
        assert_eq!(slotted["css_slots"][0]["reason"], "a custom pulse keyframe");
        assert_eq!(
            slotted["css_slots"][0]["scope"],
            serde_json::json!(format!("[data-nyx-css~=\"{scope}\"]"))
        );
        assert_eq!(
            slotted["css_slots"][0]["features"],
            serde_json::json!(["keyframes"])
        );
        assert!(slotted["css_slots"][0]["refused"].is_null());

        // …and the ONE refusal left is a composition that does not PARSE — a readable error, never
        // a silent empty frame.
        let err = h
            .run(
                "design_render",
                serde_json::json!({ "projectId": project_id, "compositionJson": "not json" }),
            )
            .unwrap_err();
        assert!(
            err.contains("bad composition json"),
            "readable render error: {err}"
        );
    }

    /// DONE-CRITERION (finding `01KYW34347CZSNXCYS35KDG3EY`): `design_render` carries the PARCOURS —
    /// a node linking to another PAGE of the prototype — and it carries it as the shim's own
    /// vocabulary, so the frame navigates with no change to the shim at all.
    ///
    /// The page DIRECTORY is what an id resolves against, and the command is TOLD which prototype to
    /// read it from (`prototypeId`). Without it the render knows of no page — and says so, by name,
    /// instead of dropping the link: a click that silently does nothing is exactly the defect this
    /// finding is about.
    #[test]
    fn design_render_carries_the_parcours_when_it_is_told_which_prototype() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-nav" }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        let (prototype_id, pricing_id) = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::upsert_component(
                    c,
                    &project_id,
                    &nyx_core::db::ComponentInput {
                        name: "Button".into(),
                        template: "<button>{{label}}</button>".into(),
                        ..Default::default()
                    },
                )?;
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Feature",
                    nyx_core::db::SpecSections::empty(),
                )?;
                let proto = nyx_core::db::create_prototype(c, &spec.id, None)?;
                nyx_core::db::create_page(c, &proto.id, "home", "Accueil", 0)?;
                let pricing = nyx_core::db::create_page(c, &proto.id, "pricing", "Tarifs", 1)?;
                Ok::<_, diesel::result::Error>((proto.id, pricing.id))
            })
            .unwrap();

        let composition = serde_json::json!({
            "component": "Button", "params": { "label": "Voir les tarifs" },
            "navigate_to": pricing_id
        })
        .to_string();

        // TOLD which prototype: the id resolves to the slug the shim reads, and the readable half
        // (`page_title`) comes back beside it for the screen.
        let linked = h
            .run(
                "design_render",
                serde_json::json!({
                    "projectId": project_id,
                    "prototypeId": prototype_id,
                    "compositionJson": composition,
                }),
            )
            .unwrap();
        let linked: Value = serde_json::from_str(&linked).unwrap();
        assert_eq!(
            linked["markup"].as_str().unwrap(),
            format!(
                "<button data-nyx-node=\"Button\" data-nyx-page=\"pricing\" \
                 data-nyx-page-id=\"{pricing_id}\">Voir les tarifs</button>"
            ),
            "the shim's own attribute, unchanged, plus the durable id"
        );
        assert_eq!(linked["navigation_count"], serde_json::json!(1));
        assert_eq!(linked["navigation_broken_count"], serde_json::json!(0));
        assert_eq!(linked["navigation"][0]["status"], "linked");
        assert_eq!(linked["navigation"][0]["page"], "pricing");
        assert_eq!(linked["navigation"][0]["page_title"], "Tarifs");

        // NOT told: the render completes, nothing navigates, and the link is NAMED in place —
        // never dropped.
        let blind = h
            .run(
                "design_render",
                serde_json::json!({
                    "projectId": project_id,
                    "compositionJson": composition,
                }),
            )
            .unwrap();
        let blind: Value = serde_json::from_str(&blind).unwrap();
        let markup = blind["markup"].as_str().unwrap();
        assert!(
            markup.contains(&format!("data-nyx-page-missing=\"{pricing_id}\""))
                && !markup.contains("data-nyx-page=\""),
            "the marker is there and nothing can navigate: {markup}"
        );
        assert_eq!(blind["navigation_broken_count"], serde_json::json!(1));
        assert_eq!(blind["navigation"][0]["status"], "unknown_page");
        assert!(blind["navigation"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("NO page directory"));
    }

    /// DONE-CRITERION (finding `01KYSZPG9Y8QGGVED208SG5BTB`): the catalogue's "liés-page" set counts
    /// a component reached only from another component's TEMPLATE — transitively.
    ///
    /// `linked_names` was the union of the composition TREES alone. A shared `Spinner` — exactly the
    /// case `data-nyx-use` was asked for — is placed in no tree, so its card came back with no
    /// `linked` badge: a legitimate-looking deletion candidate, while it is expanded into every page
    /// that draws a Button. (Deletion itself was already guarded — `still_referenced_warning` names
    /// the hosts — so it was the LINK calculation that was behind, not the safety net.)
    #[test]
    fn a_component_reached_only_by_reference_counts_as_linked() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-linked" }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        h.runtime
            .db()
            .with_conn(|c| {
                // Button REFERENCES Spinner, which REFERENCES Icon. `Orphan` is reached by nothing.
                for (name, template) in [
                    (
                        "Button",
                        "<button><i data-nyx-use=\"Spinner\"></i>{{label}}</button>",
                    ),
                    ("Spinner", "<span data-nyx-use=\"Icon\"></span>"),
                    ("Icon", "<svg/>"),
                    ("Orphan", "<hr/>"),
                ] {
                    nyx_core::db::upsert_component(
                        c,
                        &project_id,
                        &nyx_core::db::ComponentInput {
                            name: name.into(),
                            template: template.into(),
                            ..Default::default()
                        },
                    )?;
                }
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Feature",
                    nyx_core::db::SpecSections::empty(),
                )?;
                let proto = nyx_core::db::create_prototype(c, &spec.id, None)?;
                let page = nyx_core::db::create_page(c, &proto.id, "home", "Home", 0)?;
                let version = nyx_core::db::create_page_version(c, &page.id, "v1", None)?;
                // The TREE places ONE component; the other two are reached only by reference.
                nyx_core::db::create_variant(
                    c,
                    &version.id,
                    &nyx_core::db::VariantInput {
                        label: "a".into(),
                        title: "A".into(),
                        composition_json: "{\"component\":\"Button\"}".into(),
                        ..Default::default()
                    },
                )
                .expect("create variant");
                Ok::<_, diesel::result::Error>(())
            })
            .unwrap();

        let list = h
            .run(
                "design_component_list",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap();
        let list: Value = serde_json::from_str(&list).unwrap();
        let linked: Vec<&str> = list["linked_names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(
            linked,
            vec!["Button", "Spinner", "Icon"],
            "the tree's placement, then everything its templates reach — TRANSITIVELY. `Orphan` is \
             reached by nothing and stays out, so the badge still means something"
        );
    }

    /// v2.7 Phase 3 — the composant-detail "params ENREGISTRÉS PAR NŒUD" seam: `design_component_nodes`
    /// lists the composition nodes that instantiate a component (each pre-parameterized), and
    /// `proto_variant_set_node_params` persists an edited node's params back into the variant's
    /// composition tree (which stays a COMPOSITION, not an HTML blob) + pushes `prototypes://changed`.
    #[test]
    fn design_component_nodes_and_per_node_param_persistence() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-node" }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        // Seed a Spec + prototype + page/version/variant whose composition uses a `Button`.
        let variant_id: String = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Feature",
                    nyx_core::db::SpecSections::empty(),
                )?;
                let proto = nyx_core::db::create_prototype(c, &spec.id, None)?;
                let page = nyx_core::db::create_page(c, &proto.id, "home", "Home", 0)?;
                let version = nyx_core::db::create_page_version(c, &page.id, "v1", None)?;
                let variant = nyx_core::db::create_variant(
                    c,
                    &version.id,
                    &nyx_core::db::VariantInput {
                        label: "a".into(),
                        title: "A".into(),
                        composition_json: "{\"component\":\"Root\",\"children\":[{\"component\":\"Button\",\"params\":{\"label\":\"Old\"}}]}".into(),
                        ..Default::default()
                    },
                )
                .expect("create variant on unlocked round");
                Ok::<_, diesel::result::Error>(variant.id)
            })
            .unwrap();

        // design_component_nodes → the Button node, pre-parameterized (node_key + params).
        let nodes = h
            .run(
                "design_component_nodes",
                serde_json::json!({ "projectId": project_id, "componentName": "Button" }),
            )
            .unwrap();
        let nodes: Value = serde_json::from_str(&nodes).unwrap();
        let node = &nodes["nodes"][0];
        assert_eq!(node["variant_id"].as_str().unwrap(), variant_id);
        assert_eq!(node["node_key"].as_str().unwrap(), "Root/Button");
        assert!(node["params_json"].as_str().unwrap().contains("Old"));

        // proto_variant_set_node_params → persist the edited node params PER NODE.
        h.run(
            "proto_variant_set_node_params",
            serde_json::json!({
                "variantId": variant_id,
                "nodeKey": "Root/Button",
                "paramsJson": "{\"label\":\"New\",\"state\":\"loading\"}"
            }),
        )
        .unwrap();
        assert_eq!(
            changed_count(&h.buf, "prototypes"),
            1,
            "pushed prototypes://changed once"
        );

        // The composition still holds a TREE (a composition, not an HTML blob) with the new params.
        let stored: String = h
            .runtime
            .db()
            .with_conn(|c| {
                Ok::<_, diesel::result::Error>(
                    nyx_core::db::get_variant(c, &variant_id)?
                        .unwrap()
                        .composition_json,
                )
            })
            .unwrap();
        assert!(stored.contains("New"), "new label persisted: {stored}");
        assert!(
            stored.contains("loading"),
            "state param persisted: {stored}"
        );
        assert!(!stored.contains("Old"), "old label replaced: {stored}");

        // An unknown node is a readable error, not a silent no-op.
        let err = h
            .run(
                "proto_variant_set_node_params",
                serde_json::json!({
                    "variantId": variant_id,
                    "nodeKey": "Root/Missing",
                    "paramsJson": "{}"
                }),
            )
            .unwrap_err();
        assert!(err.contains("not found"), "readable node error: {err}");
    }

    /// DONE-CRITERION (finding `01KYQ0R9414Q5K63TBV9CCMC9J`): the per-node WRITE confronts the keys
    /// it just persisted with what the referenced component DECLARES, and ANSWERS with the mismatch.
    /// The reported shape — a node carrying `text` for a `Button` declaring `buttonLabel` + a
    /// `variant` axis — used to save, render EMPTY and say nothing. Two DISTINCT messages (unknown
    /// key / unprovided `required`), each naming component + node + key + the correction, and the
    /// save still SUCCEEDS: nyx signals, it never refuses nor guesses a mapping.
    #[test]
    fn set_node_params_reports_what_the_node_owes_its_component() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-node-contract" }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        let variant_id: String = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Feature",
                    nyx_core::db::SpecSections::empty(),
                )?;
                let proto = nyx_core::db::create_prototype(c, &spec.id, None)?;
                let page = nyx_core::db::create_page(c, &proto.id, "home", "Home", 0)?;
                let version = nyx_core::db::create_page_version(c, &page.id, "v1", None)?;
                // The CONTRACT: `buttonLabel` is required, `variant` is an AXIS. Neither is `text`.
                nyx_core::db::upsert_component(
                    c,
                    &project_id,
                    &nyx_core::db::ComponentInput {
                        name: "Button".into(),
                        template: "<button>{{buttonLabel}}</button>".into(),
                        params_json: Some(
                            r#"[{"name":"buttonLabel","type":"text","required":true}]"#.into(),
                        ),
                        states_json: Some(
                            r#"{"axes":[{"name":"variant","values":[{"name":"ghost","classes":"bg-transparent"}]}]}"#
                                .into(),
                        ),
                        ..Default::default()
                    },
                )?;
                let variant = nyx_core::db::create_variant(
                    c,
                    &version.id,
                    &nyx_core::db::VariantInput {
                        label: "a".into(),
                        title: "A".into(),
                        composition_json:
                            "{\"key\":\"Root\",\"children\":[{\"component\":\"Button\",\"key\":\"Reset\"}]}"
                                .into(),
                        ..Default::default()
                    },
                )
                .expect("create variant on unlocked round");
                Ok::<_, diesel::result::Error>(variant.id)
            })
            .unwrap();

        let saved = h
            .run(
                "proto_variant_set_node_params",
                serde_json::json!({
                    "variantId": variant_id,
                    "nodeKey": "Root/Reset",
                    "paramsJson": "{\"text\":\"Reset filters\"}"
                }),
            )
            .expect("the save SUCCEEDS — the contract is a warning, never a refusal");
        let saved: Value = serde_json::from_str(&saved).unwrap();
        assert!(saved["variant"]["id"].is_string(), "the edit is persisted");
        let warnings: Vec<String> = saved["warnings"]
            .as_array()
            .expect("the write answers with the node contract")
            .iter()
            .map(|w| w.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(warnings.len(), 2, "two DISTINCT defects: {warnings:?}");
        assert!(
            warnings[0].contains("Root/Reset")
                && warnings[0].contains("'Button'")
                && warnings[0].contains("'text'")
                && warnings[0].contains("'buttonLabel'")
                && warnings[0].contains("RENAME"),
            "the unknown key names component + node + key + the correction: {}",
            warnings[0]
        );
        assert!(
            !warnings[0].to_lowercase().contains("did you mean"),
            "no mapping between neighbouring names is ever guessed: {}",
            warnings[0]
        );
        assert!(
            warnings[1].contains("Root/Reset")
                && warnings[1].contains("REQUIRED")
                && warnings[1].contains("'buttonLabel'"),
            "the unprovided REQUIRED param is its own message: {}",
            warnings[1]
        );

        // Corrected — every key declared, the required one provided — the write is SILENT.
        let fixed = h
            .run(
                "proto_variant_set_node_params",
                serde_json::json!({
                    "variantId": variant_id,
                    "nodeKey": "Root/Reset",
                    "paramsJson": "{\"buttonLabel\":\"Reset filters\",\"variant\":\"ghost\"}"
                }),
            )
            .unwrap();
        let fixed: Value = serde_json::from_str(&fixed).unwrap();
        assert_eq!(
            fixed["warnings"],
            serde_json::json!([]),
            "a node honouring the contract says nothing"
        );
    }

    /// review #24 — the proto UI data layer over IPC: `proto_tree` exposes the tree + both-tier
    /// election + comments; the UI-driven WRITES (elect / clear RETENU, comment add / resolve,
    /// rollback) go through the SHARED guarded `db` authority, each pushing `prototypes://changed`,
    /// and a mutation on a design-LOCKED round is REFUSED with a readable "FROZEN" error (élection figée).
    #[test]
    fn proto_tree_read_model_and_ui_writes_respect_the_lock() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-proto24" }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        // Seed: Spec + prototype + one page/version + two variants, page pinned into the current round.
        #[allow(clippy::type_complexity)]
        let (spec_id, proto_id, round_id, page_id, variant_a, variant_b): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = h
            .runtime
            .db()
            .with_conn(|c| {
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Feature",
                    nyx_core::db::SpecSections::empty(),
                )?;
                let proto = nyx_core::db::create_prototype(c, &spec.id, None)?;
                let round = nyx_core::db::current_round(c, &proto.id)?.unwrap();
                let page = nyx_core::db::create_page(c, &proto.id, "home", "Home", 0)?;
                let version = nyx_core::db::create_page_version(c, &page.id, "v1", None)?;
                let a = nyx_core::db::create_variant(
                    c,
                    &version.id,
                    &nyx_core::db::VariantInput {
                        label: "a".into(),
                        title: "A".into(),
                        ..Default::default()
                    },
                )
                .expect("create variant a on unlocked round");
                let b = nyx_core::db::create_variant(
                    c,
                    &version.id,
                    &nyx_core::db::VariantInput {
                        label: "b".into(),
                        title: "B".into(),
                        ..Default::default()
                    },
                )
                .expect("create variant b on unlocked round");
                nyx_core::db::set_round_page(c, &round.id, &page.id, &version.id, 0).unwrap();
                Ok::<_, diesel::result::Error>((spec.id, proto.id, round.id, page.id, a.id, b.id))
            })
            .unwrap();

        // proto_tree read-model: the tree + rounds + variants are exposed; no RETENU / lock yet.
        let tree: Value = serde_json::from_str(
            &h.run("proto_tree", serde_json::json!({ "specId": spec_id }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(tree["prototype"]["id"].as_str().unwrap(), proto_id);
        assert_eq!(tree["rounds"].as_array().unwrap().len(), 1);
        assert_eq!(tree["pages"].as_array().unwrap().len(), 1);
        assert_eq!(
            tree["pages"][0]["versions"][0]["variants"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(tree["design_lock"].is_null(), "not frozen yet");

        // proto_elect: POSE the USER's RETENU (b) → pushed once, and the tree reflects it.
        h.run(
            "proto_elect",
            serde_json::json!({ "roundId": round_id, "pageId": page_id, "variantId": variant_b, "decidedBy": "user:kris" }),
        )
        .unwrap();
        assert_eq!(changed_count(&h.buf, "prototypes"), 1, "elect pushed once");
        let tree: Value = serde_json::from_str(
            &h.run("proto_tree", serde_json::json!({ "specId": spec_id }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            tree["pages"][0]["manifest_entry"]["chosen_variant_id"]
                .as_str()
                .unwrap(),
            variant_b,
            "the RETENU is exposed on the manifest entry"
        );
        assert_eq!(
            tree["pages"][0]["effective_variant_id"].as_str().unwrap(),
            variant_b
        );

        // proto_election_clear: RETIRE the RETENU (reversible).
        h.run(
            "proto_election_clear",
            serde_json::json!({ "roundId": round_id, "pageId": page_id }),
        )
        .unwrap();
        let tree: Value = serde_json::from_str(
            &h.run("proto_tree", serde_json::json!({ "specId": spec_id }))
                .unwrap(),
        )
        .unwrap();
        assert!(
            tree["pages"][0]["manifest_entry"]["chosen_variant_id"].is_null(),
            "the RETENU is cleared"
        );

        // proto_comment_add (author defaults to `user`) then proto_comment_resolve.
        let added: Value = serde_json::from_str(
            &h.run(
                "proto_comment_add",
                serde_json::json!({ "prototypeId": proto_id, "body": "tighten spacing", "nodeKey": "Root/Header" }),
            )
            .unwrap(),
        )
        .unwrap();
        let feedback_id = added["feedback"]["id"].as_str().unwrap().to_string();
        assert_eq!(added["feedback"]["author_kind"].as_str().unwrap(), "user");
        let tree: Value = serde_json::from_str(
            &h.run("proto_tree", serde_json::json!({ "specId": spec_id }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(tree["open_feedback"].as_array().unwrap().len(), 1);

        h.run(
            "proto_comment_resolve",
            serde_json::json!({ "feedbackId": feedback_id, "resolutionNote": "done" }),
        )
        .unwrap();
        let tree: Value = serde_json::from_str(
            &h.run("proto_tree", serde_json::json!({ "specId": spec_id }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(tree["open_feedback"].as_array().unwrap().len(), 0);
        assert_eq!(tree["resolved_feedback"].as_array().unwrap().len(), 1);

        // proto_round_rebase: rollback opens a NEW round (the history grows).
        h.run(
            "proto_round_rebase",
            serde_json::json!({ "prototypeId": proto_id, "sourceRoundId": round_id, "label": "v2" }),
        )
        .unwrap();
        let tree: Value = serde_json::from_str(
            &h.run("proto_tree", serde_json::json!({ "specId": spec_id }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            tree["rounds"].as_array().unwrap().len(),
            2,
            "rollback added a round"
        );

        // `proto_round_manifest` — the round that is NO LONGER current is READABLE (finding
        // `01KZ3AP34K1PKS6W0E75RP7DF6`): what it pinned per page, and what was retained in it.
        // `proto_tree` above resolves the election against the CURRENT round only, so without this
        // an earlier round was a label with no reachable content.
        h.run(
            "proto_elect",
            serde_json::json!({ "roundId": round_id, "pageId": page_id, "variantId": variant_b }),
        )
        .unwrap();
        let manifest: Value = serde_json::from_str(
            &h.run(
                "proto_round_manifest",
                serde_json::json!({ "roundId": round_id }),
            )
            .unwrap(),
        )
        .unwrap();
        let manifest = manifest.as_array().unwrap();
        assert_eq!(manifest.len(), 1, "the past round pinned one page");
        assert_eq!(manifest[0]["entry"]["page_id"], serde_json::json!(page_id));
        assert_eq!(
            manifest[0]["effective_variant_id"],
            serde_json::json!(variant_b),
            "the variant RETAINED in that round, resolved"
        );
        // …and it is a pure READ: it opens no round, so the history did not grow.
        let tree: Value = serde_json::from_str(
            &h.run("proto_tree", serde_json::json!({ "specId": spec_id }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            tree["rounds"].as_array().unwrap().len(),
            2,
            "consulting a round creates none"
        );
        // An unknown round pins nothing — a manifest is a list, never an error.
        assert_eq!(
            h.run(
                "proto_round_manifest",
                serde_json::json!({ "roundId": "nope" })
            )
            .unwrap(),
            "[]"
        );

        // LOCK the original round, then an election on it is REFUSED (élection figée) with a readable error.
        h.runtime
            .db()
            .with_conn(|c| nyx_core::db::lock_design(c, &proto_id, &round_id, Some("user:kris")))
            .unwrap();
        let err = h
            .run(
                "proto_elect",
                serde_json::json!({ "roundId": round_id, "pageId": page_id, "variantId": variant_a }),
            )
            .unwrap_err();
        assert!(
            err.contains("FROZEN"),
            "electing on a locked round is refused: {err}"
        );
    }

    /// T29 — the elected-composition read-model over IPC: `proto_composition` exposes NOTHING
    /// (`null`) until the design is LOCKED (the "élection figée" empty cue = "pas encore figée"),
    /// then exposes the FROZEN composition — the arbre nommé (placement) per page pinned in the
    /// locked round, each node classified by its NATURE ([DS] catalogue with a design-system
    /// `component_ref` / [NEW] to-build / [inline] bespoke), plus the [NEW] candidates. It reads off
    /// the LOCK (`db::elected_composition`), the SAME authority the MCP tool serves (PDR-0005).
    #[test]
    fn proto_composition_exposes_the_frozen_election_over_ipc() {
        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": "/tmp/dbcmd-proto-comp" }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        // Seed: a design-system + one catalogue component (the [DS] target), a Spec + prototype, a
        // page whose lone variant mixes the 3 natures (Card [DS] / NewWidget [NEW] / [inline]),
        // pinned into the current round.
        let (proto_id, round_id, variant_id): (String, String, String) = h
            .runtime
            .db()
            .with_conn(|c| {
                nyx_core::db::upsert_design(c, &project_id, "# DESIGN", None).unwrap();
                nyx_core::db::upsert_component(
                    c,
                    &project_id,
                    &nyx_core::db::ComponentInput {
                        name: "Card".into(),
                        // [DS] = MATERIALIZED in the app: the nature is the component's ORIGIN (an
                        // on-disk source), not the mere presence of a catalogue row (review #41).
                        source_path: Some("src/ui/Card.tsx".into()),
                        template: "<div>{{{children}}}</div>".into(),
                        ..Default::default()
                    },
                )
                .unwrap();
                let (spec, _rev) = nyx_core::db::create_spec(
                    c,
                    &project_id,
                    "Feature",
                    nyx_core::db::SpecSections::empty(),
                )?;
                let proto = nyx_core::db::create_prototype(c, &spec.id, None)?;
                let round = nyx_core::db::current_round(c, &proto.id)?.unwrap();
                let page = nyx_core::db::create_page(c, &proto.id, "home", "Home", 0)?;
                let version = nyx_core::db::create_page_version(c, &page.id, "v1", None)?;
                let variant = nyx_core::db::create_variant(
                    c,
                    &version.id,
                    &nyx_core::db::VariantInput {
                        label: "a".into(),
                        title: "A".into(),
                        composition_json: "{\"component\":\"Card\",\"children\":[{\"component\":\"NewWidget\"},{\"inline\":\"<hr/>\"}]}".into(),
                        ..Default::default()
                    },
                )
                .expect("create variant on unlocked round");
                nyx_core::db::set_round_page(c, &round.id, &page.id, &version.id, 0).unwrap();
                Ok::<_, diesel::result::Error>((proto.id, round.id, variant.id))
            })
            .unwrap();

        // Before the LOCK: the seam exposes NOTHING (`null`) — the "pas encore figée" empty cue.
        let before = h
            .run(
                "proto_composition",
                serde_json::json!({ "prototypeId": proto_id }),
            )
            .unwrap();
        assert_eq!(before, "null", "no lock → no composition");

        // FREEZE the round, then the composition is exposed off the lock.
        h.runtime
            .db()
            .with_conn(|c| nyx_core::db::lock_design(c, &proto_id, &round_id, Some("user:kris")))
            .unwrap();
        let exposed: Value = serde_json::from_str(
            &h.run(
                "proto_composition",
                serde_json::json!({ "prototypeId": proto_id }),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(exposed["prototype_id"].as_str().unwrap(), proto_id);
        assert_eq!(
            exposed["locked"]["locked_round_id"].as_str().unwrap(),
            round_id,
            "reads off the LOCK"
        );
        // One page: its effective RETENU (the lone variant, retenue d'office) + the arbre nommé.
        let page = &exposed["pages"][0];
        assert_eq!(page["elected_variant_id"].as_str().unwrap(), variant_id);
        assert!(page["composition_json"].as_str().unwrap().contains("Card"));
        // The 3 natures made explicit, each carried on its node.
        let nodes = page["nodes"].as_array().unwrap();
        let nature = |key: &str| {
            nodes
                .iter()
                .find(|n| n["node_key"] == key)
                .unwrap_or_else(|| panic!("node {key} present"))["nature"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(nature("Card"), "ds");
        assert_eq!(nature("Card/NewWidget"), "new");
        assert_eq!(nature("Card/inline"), "inline");
        // [DS] carries a design-system reference; [NEW] does not.
        let ds = nodes.iter().find(|n| n["node_key"] == "Card").unwrap();
        assert!(
            ds["component_ref"].is_string(),
            "[DS] node carries a design-system component_ref"
        );
        let new = nodes
            .iter()
            .find(|n| n["node_key"] == "Card/NewWidget")
            .unwrap();
        assert!(
            new["component_ref"].is_null(),
            "[NEW] has no catalogue reference"
        );
        // The [NEW] candidates — the derived-task anchor.
        assert_eq!(page["new_components"][0].as_str().unwrap(), "NewWidget");
    }

    /// review #23 — the `design_inventory` IPC path (the CAPABILITY, kept intact when finding
    /// 01KYKX7HBP4ZSSS6ZDM31T6A5E removed the `/design` button that used to call it): it POPULATES
    /// `design_components` from the project's front and pushes `prototypes://changed` so the
    /// catalogue re-pulls by itself; `design_component_list` then lists the real components.
    #[test]
    fn design_inventory_command_populates_catalogue_and_pushes() {
        // A scratch front with two components under the project root.
        let root = std::env::temp_dir().join(format!(
            "nyx-dbcmd-inv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src/ui")).unwrap();
        std::fs::write(root.join("src/Button.tsx"), "<button>{{label}}</button>").unwrap();
        std::fs::write(root.join("src/ui/Card.tsx"), "<div class=\"card\"/>").unwrap();

        let h = Harness::new();
        let created = h
            .run(
                "create_project",
                serde_json::json!({ "name": "Nyx", "rootPath": root.to_str().unwrap() }),
            )
            .unwrap();
        let created: Value = serde_json::from_str(&created).unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        // The trigger populates the catalogue and reports the counts.
        let out: Value = serde_json::from_str(
            &h.run(
                "design_inventory",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out["component_count"].as_u64().unwrap(), 2);
        assert_eq!(out["discovered"].as_u64().unwrap(), 2);
        assert_eq!(
            changed_count(&h.buf, "prototypes"),
            1,
            "design_inventory pushed prototypes://changed once"
        );

        // The catalogue read model now lists the real components (what `/design` shows).
        let catalog: Value = serde_json::from_str(
            &h.run(
                "design_component_list",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = catalog["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Button", "Card"]);

        // review — a run that changes NOTHING notifies nothing (it used to push per run, so an
        // idle sync made every open surface re-pull for no reason).
        let again: Value = serde_json::from_str(
            &h.run(
                "design_inventory",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(again["component_count"].as_u64().unwrap(), 2);
        assert_eq!(again["orphan_count"].as_u64().unwrap(), 0);
        assert_eq!(
            changed_count(&h.buf, "prototypes"),
            1,
            "an idle inventory pushes NO second change"
        );

        // review #44 — the `/design` card's DELETE: `db::delete_component` was exposed nowhere, so
        // an entry a wrong glob had catalogued could be removed from no surface at all.
        let card_id = catalog["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "Card")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let deleted: Value = serde_json::from_str(
            &h.run(
                "design_component_delete",
                serde_json::json!({ "componentId": card_id }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(deleted["deleted"], serde_json::json!(true));
        assert_eq!(
            changed_count(&h.buf, "prototypes"),
            2,
            "the removal pushes prototypes://changed so /design re-pulls"
        );
        let after: Value = serde_json::from_str(
            &h.run(
                "design_component_list",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            after["components"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["Button"]
        );
        // An unknown id is a readable error, never a silent no-op.
        assert!(h
            .run(
                "design_component_delete",
                serde_json::json!({ "componentId": card_id }),
            )
            .is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// review #42 — the THEME TOKENS seam: nothing ever wrote `theme_tokens_json`, so the preview
    /// iframe's `@theme` was empty and every `bg-primary` rendered GREY. `design_tokens_ingest`
    /// DERIVES the layer from the app's own stylesheet (REFERENCED — it names the source sheet, and
    /// stores ONE layer per project, never a copy per component) and `design_tokens_set` EDITS it;
    /// both push `prototypes://changed` so `/design` and the frame re-pull.
    #[test]
    fn design_tokens_commands_ingest_from_the_app_and_edit_the_layer() {
        let root = std::env::temp_dir().join(format!(
            "nyx-dbcmd-tokens-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/styles.css"),
            ":root{--primary:oklch(0.55 0.21 264);--background:oklch(1 0 0);\
             --foreground:oklch(0.14 0 0);--radius:0.625rem}\
             .dark{--primary:oklch(0.72 0.16 264)}",
        )
        .unwrap();
        std::fs::write(
            root.join("src/Button.tsx"),
            "<button class=\"bg-primary\"/>",
        )
        .unwrap();

        let h = Harness::new();
        let created: Value = serde_json::from_str(
            &h.run(
                "create_project",
                serde_json::json!({ "name": "sandbox", "rootPath": root.to_str().unwrap() }),
            )
            .unwrap(),
        )
        .unwrap();
        let project_id = created["project"]["id"].as_str().unwrap().to_string();

        let run: Value = serde_json::from_str(
            &h.run(
                "design_tokens_ingest",
                serde_json::json!({ "projectId": project_id }),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(run["changed"], serde_json::json!(true));
        assert_eq!(run["variable_count"].as_u64().unwrap(), 4);
        assert_eq!(run["dark_count"].as_u64().unwrap(), 1);
        // REFERENCED: the layer says which app stylesheet `primary` comes from.
        assert_eq!(
            run["sources"][0]["path"],
            serde_json::json!("src/styles.css")
        );
        assert_eq!(
            changed_count(&h.buf, "prototypes"),
            1,
            "the ingest pushed prototypes://changed once"
        );

        // `design_get` — what the iframe reads — now carries the tokens `bg-primary` resolves
        // against, instead of the NULL that made every preview grey.
        let design: Value = serde_json::from_str(
            &h.run("design_get", serde_json::json!({ "projectId": project_id }))
                .unwrap(),
        )
        .unwrap();
        let tokens = design["theme_tokens_json"].as_str().unwrap();
        assert!(tokens.contains("--primary"), "tokens: {tokens}");
        assert!(tokens.contains("oklch(0.55 0.21 264)"));

        // An idle re-ingest pushes nothing.
        h.run(
            "design_tokens_ingest",
            serde_json::json!({ "projectId": project_id }),
        )
        .unwrap();
        assert_eq!(changed_count(&h.buf, "prototypes"), 1, "idle ⇒ no push");

        // EDIT once, apply everywhere — and the source reference survives the edit.
        let edited: Value = serde_json::from_str(
            &h.run(
                "design_tokens_set",
                serde_json::json!({
                    "projectId": project_id,
                    "variables": { "primary": "#ff0055" },
                    "remove": ["--radius"],
                }),
            )
            .unwrap(),
        )
        .unwrap();
        let stored = edited["tokens_json"].as_str().unwrap();
        assert!(stored.contains("#ff0055"));
        assert!(!stored.contains("0.625rem"), "--radius was removed");
        assert!(stored.contains("src/styles.css"), "reference preserved");
        assert_eq!(edited["variable_count"].as_u64().unwrap(), 3);
        assert_eq!(
            changed_count(&h.buf, "prototypes"),
            2,
            "the edit pushed once"
        );

        // A malformed token value is a readable error, never a silently mangled theme.
        assert!(h
            .run(
                "design_tokens_set",
                serde_json::json!({ "projectId": project_id, "variables": { "primary": 12 } }),
            )
            .is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// F2 wiring — `persist_scrollback` is the ONE place a terminal's scrollback is saved, so it
    /// is what marks the saved buffer level with the shell again. Without this the freshness bit
    /// `read_terminal` reports would latch `stale` forever after the terminal's first byte, and a
    /// caller would learn to ignore it.
    #[cfg(unix)]
    #[test]
    fn persist_scrollback_marks_the_saved_buffer_level_with_the_shell() {
        let h = Harness::new();
        let record_id = h
            .runtime
            .db()
            .with_conn(|c| nyx_core::db::create_terminal(c, "/tmp", None))
            .expect("terminal record")
            .id;
        let pty_id = h
            .runtime
            .pty()
            .spawn(80, 24, "", &record_id, 0, h.events.clone())
            .expect("spawn the record's pty");

        // The shell prints its prompt: the saved (empty) scrollback is now behind it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline
            && !h.runtime.pty().scrollback_is_behind(&record_id)
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            h.runtime.pty().scrollback_is_behind(&record_id),
            "output the front has not saved leaves the buffer behind the shell"
        );

        h.run(
            "persist_scrollback",
            serde_json::json!({ "id": record_id, "serialized": "$ prompt" }),
        )
        .expect("persist_scrollback succeeds");
        assert!(
            !h.runtime.pty().scrollback_is_behind(&record_id),
            "the save brought the buffer level with the shell"
        );

        h.runtime.pty().close(pty_id);
    }
}
