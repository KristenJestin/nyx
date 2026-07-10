//! Shell-agnostic MCP **agent-context render** — the native equivalent of
//! `depot context dev|coder|auditor`, rendered LIVE by nyx-core from nyx state (task
//! `01KX1DH8DR369K0JA20PNF4H4C`, PDR-0020).
//!
//! # What it renders
//!
//! Given a role (`dev` | `coder` | `auditor`) and a Spec (by id, or resolved via the
//! current dev workspace / cwd), [`render_agent_context`] produces the manual TEXT from
//! LIVE state:
//!   * the ROLE preamble (what this agent may / may not do) — the fixed part;
//!   * the SPEC header + its structured sections (problem / solution / … from the active
//!     revision);
//!   * the CURRENT PHASE (the Spec's in-progress phase, derived from the phase plan +
//!     task statuses) and THAT phase's tasks;
//!   * the derived WORKSPACE + REPOS (via [`db::spec_workspace_context`] — the SAME
//!     workspace → project → repos chain the Phase-4 UI reads).
//!
//! It is a FAITHFUL native render, not a byte-for-byte copy of depot: the shape (role +
//! spec sections + current-phase tasks + workspace/repos) is reproduced from nyx state.
//! Zero depot CLI, zero generated file — the text is computed on demand.
//!
//! # Exposure
//!
//! [`dispatch_context_tool`] serves the `agent_context` MCP tool (routed from
//! [`crate::mcp_tools_core::dispatch_extension_tool`]) and advertised in `tools/list` via
//! the tool descriptors in `mcp.rs`. An agent session reaches the rendered manual by
//! CALLING that tool over MCP — no depot CLI required.
//!
//! The plugin hook / slash-command injection path — a `SessionStart` /
//! `UserPromptSubmit` relay that would push the manual as `additionalContext`, plus
//! `/ndev` `/ncoder` `/nauditor` stubs — is a FUTURE delivery mechanism (PDR-0020) and is
//! NOT wired yet: today's `nyx-session-hook.cjs` relays only the `agent_session_event`
//! SessionStart context (project / workspace / terminal), not this role-based Spec manual.

use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, optional_str, require_str};
use crate::mcp_tools_core::ToolOutcome;

/// `agent_context` — render a dev/coder/auditor manual from nyx state.
pub const AGENT_CONTEXT_TOOL: &str = "agent_context";

/// The three agent ROLES a context can be rendered for (the three "principaux" of
/// PDR-0020). A `dev` orchestrates, a `coder` implements, an `auditor` reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Dev,
    Coder,
    Auditor,
}

impl AgentRole {
    /// Parse the `role` argument. An unknown role → `None` (the caller maps it to
    /// `invalid_argument`).
    fn parse(name: &str) -> Option<Self> {
        match name {
            "dev" => Some(AgentRole::Dev),
            "coder" => Some(AgentRole::Coder),
            "auditor" => Some(AgentRole::Auditor),
            _ => None,
        }
    }

    /// The manual's header label (`DEV` / `CODER` / `AUDITOR`).
    fn label(self) -> &'static str {
        match self {
            AgentRole::Dev => "DEV",
            AgentRole::Coder => "CODER",
            AgentRole::Auditor => "AUDITOR",
        }
    }

    /// The fixed ROLE preamble — the "you may / you may not" contract of the role. Faithful
    /// to the depot manuals' Role sections, rewritten for the native nyx surface (the agent
    /// authors/transitions through the nyx MCP tools, not the depot CLI — PDR-0020).
    fn preamble(self) -> &'static str {
        match self {
            AgentRole::Dev => {
                "# Context: Dev Orchestrator\n\n## Role\n\nYou are an orchestrator only.\n\n\
                 You may:\n\n\
                 - read the Spec, its tasks, and the codebase\n\
                 - ask the user targeted questions\n\
                 - author and refine the Spec through the nyx MCP tools (spec_create, \
                 spec_update_section, spec_fork)\n\
                 - move the Spec through its lifecycle with spec_transition (the sensitive \
                 transitions ready/done/cancel need the user's explicit confirmation)\n\
                 - delegate implementation to the coder sub-agent and audit to the auditor \
                 sub-agent\n\n\
                 You may not:\n\n\
                 - implement code yourself or directly edit source files\n\
                 - bypass the coder or auditor sub-agents\n\
                 - mark the Spec done without the user's explicit validation"
            }
            AgentRole::Coder => {
                "# Context: Coder Agent\n\n## Role\n\nYou are the coder sub-agent. Your job is \
                 to implement the tasks in this context and nothing else.\n\n\
                 You may:\n\n\
                 - read code and tests\n\
                 - modify the code needed to satisfy the assigned tasks\n\
                 - run the validation needed to satisfy the done criteria\n\
                 - drive task status through the nyx MCP tools (task_transition: \
                 start/done/block/skip)\n\n\
                 You may not:\n\n\
                 - redefine the Spec\n\
                 - transition the Spec lifecycle (that is the dev orchestrator's job)\n\
                 - invent tasks outside the assigned context\n\n\
                 If you meet a kind=human task, STOP — it describes an action only the user \
                 can do; hand it back to the dev orchestrator (task_transition verify is the \
                 user-confirmed close). Continue with the other independent tasks."
            }
            AgentRole::Auditor => {
                "# Context: Auditor Agent\n\n## Role\n\nYou are the auditor sub-agent. Your job \
                 is to audit the completed work for the Spec and report findings.\n\n\
                 You may:\n\n\
                 - read the codebase, tests, and current Spec state\n\
                 - add findings progressively while auditing (a finding is a review task: \
                 task_add with kind=review + a severity)\n\
                 - log useful audit notes\n\n\
                 You may not:\n\n\
                 - modify code or fix issues directly\n\
                 - change the Spec scope\n\
                 - mark the Spec done"
            }
        }
    }
}

/// A Spec's CURRENT PHASE: the lowest phase number that still has un-done tasks, resolved
/// from the phase plan + the task statuses. `None` when the Spec has no phase plan (flat
/// tasks) or every task is terminal. The `name` is looked up in the phase plan.
struct CurrentPhase {
    number: i32,
    name: String,
}

/// A phase entry parsed from `specs.phases` (`[{number,name}]`, PDR-0022) — a local mirror
/// (the `db` one is private) used only to name the resolved current phase.
#[derive(serde::Deserialize)]
struct PhaseEntry {
    number: i64,
    #[serde(default)]
    name: String,
}

/// Resolve the CURRENT phase from the Spec's `phases` plan + its `tasks`: the lowest phase
/// number that still carries a non-terminal (not done, not cancelled) task. Returns `None`
/// for a flat plan (no phases) or an all-terminal Spec.
fn resolve_current_phase(phases_json: &str, tasks: &[db::Task]) -> Option<CurrentPhase> {
    let trimmed = phases_json.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return None;
    }
    let entries: Vec<PhaseEntry> = serde_json::from_str(trimmed).ok()?;
    // The phases (by number) that still have open work.
    let mut open: Vec<i64> = tasks
        .iter()
        .filter(|t| t.status != db::TASK_STATUS_DONE && t.status != db::TASK_STATUS_CANCELLED)
        .filter_map(|t| t.phase.map(|p| p as i64))
        .collect();
    open.sort_unstable();
    let current = open.first().copied()?;
    let name = entries
        .iter()
        .find(|e| e.number == current)
        .map(|e| e.name.clone())
        .unwrap_or_default();
    Some(CurrentPhase {
        number: current as i32,
        name,
    })
}

/// Render one structured section as a `## <Heading>\n\n<body>` block, or an empty string
/// when the body is blank (a blank section is omitted, not rendered as an empty heading).
fn section_block(heading: &str, body: &str) -> String {
    if body.trim().is_empty() {
        String::new()
    } else {
        format!("## {heading}\n\n{}\n\n", body.trim())
    }
}

/// Render one task as a compact bullet: status, kind, phase, title, and (when set) its
/// severity / blocked reason — the live execution signal a coder/auditor reads.
fn task_line(task: &db::Task) -> String {
    let mut extras: Vec<String> = Vec::new();
    if let Some(sev) = task.severity.as_deref() {
        extras.push(format!("severity={sev}"));
    }
    if let Some(reason) = task.blocked_reason.as_deref() {
        if !reason.trim().is_empty() {
            extras.push(format!("blocked: {reason}"));
        }
    }
    let suffix = if extras.is_empty() {
        String::new()
    } else {
        format!("  ({})", extras.join(", "))
    };
    format!(
        "- [{}] {} · kind={} · {}{}",
        task.status, task.id, task.kind, task.title, suffix
    )
}

/// RENDER the full agent context manual for `role` + `spec_id`, LIVE from nyx state. Returns
/// `Ok(None)` when the Spec is unknown (the caller maps it to `invalid_id`). The render is
/// deterministic and side-effect-free (a pure read over the shared `db` accessors).
pub fn render_agent_context(
    db: &Db,
    role: AgentRole,
    spec_id: &str,
) -> Result<Option<String>, RpcError> {
    let bundle = db
        .with_conn(|c| -> Result<Option<_>, diesel::result::Error> {
            let Some(spec) = db::get_spec(c, spec_id)? else {
                return Ok(None);
            };
            let Some(revision) = db::get_active_revision(c, spec_id)? else {
                return Ok(None);
            };
            let tasks = db::list_tasks_for_spec(c, spec_id)?;
            let workspace_context = db::spec_workspace_context(c, spec_id)?;
            Ok(Some((spec, revision, tasks, workspace_context)))
        })
        .map_err(internal_db)?;
    let Some((spec, revision, tasks, workspace_context)) = bundle else {
        return Ok(None);
    };

    let mut out = String::new();

    // Header line (native equivalent of depot's `=== DEPOT CONTEXT — <ROLE> ===`).
    out.push_str(&format!("=== NYX CONTEXT — {} ===\n", role.label()));
    out.push_str(&format!(
        "Spec    : {} (#{} · {})\n",
        spec.title, spec.number, spec.status
    ));
    if let Some(ctx) = &workspace_context {
        out.push_str(&format!(
            "Project : {} (project_id={})\n",
            ctx.project.name, ctx.project.id
        ));
        out.push_str(&format!(
            "Workspace: {} (workspace_id={})\n",
            ctx.workspace.name, ctx.workspace.id
        ));
    } else {
        out.push_str("Workspace: (not anchored yet)\n");
    }
    out.push('\n');

    // ROLE preamble (the fixed contract).
    out.push_str(role.preamble());
    out.push_str("\n\n");

    // SPEC sections (the six structured sections from the active revision).
    out.push_str(&format!(
        "## Spec — {} (revision {})\n\n",
        revision.title, revision.revision_number
    ));
    out.push_str(&section_block("Problem", &revision.problem));
    out.push_str(&section_block("Solution", &revision.solution));
    out.push_str(&section_block(
        "Implementation decisions",
        &revision.implementation_decisions,
    ));
    out.push_str(&section_block(
        "Testing decisions",
        &revision.testing_decisions,
    ));
    out.push_str(&section_block("User stories", &revision.user_stories));
    out.push_str(&section_block("Out of scope", &revision.out_of_scope));

    // CURRENT PHASE + its tasks (the live execution scope). A flat plan renders ALL tasks.
    let current = resolve_current_phase(&spec.phases, &tasks);
    match &current {
        Some(phase) => {
            out.push_str(&format!(
                "## Current phase — {} · {}\n\n",
                phase.number, phase.name
            ));
            let phase_tasks: Vec<&db::Task> = tasks
                .iter()
                .filter(|t| t.phase == Some(phase.number))
                .collect();
            if phase_tasks.is_empty() {
                out.push_str("_No tasks in the current phase._\n\n");
            } else {
                for task in phase_tasks {
                    out.push_str(&task_line(task));
                    out.push('\n');
                }
                out.push('\n');
            }
        }
        None => {
            out.push_str("## Tasks\n\n");
            if tasks.is_empty() {
                out.push_str("_No tasks yet._\n\n");
            } else {
                for task in &tasks {
                    out.push_str(&task_line(task));
                    out.push('\n');
                }
                out.push('\n');
            }
        }
    }

    // WORKSPACE + derived REPOS (the review/coder's context: repos + project via the anchor).
    if let Some(ctx) = &workspace_context {
        out.push_str("## Workspace & repos\n\n");
        out.push_str(&format!(
            "- workspace: {} ({})\n",
            ctx.workspace.name, ctx.workspace.root_path
        ));
        out.push_str(&format!(
            "- project: {} (key={})\n",
            ctx.project.name, ctx.project.key
        ));
        let live_repos: Vec<&db::Repository> = ctx
            .repos
            .iter()
            .filter(|r| r.archived_at.is_none())
            .collect();
        if live_repos.is_empty() {
            out.push_str("- repos: (none)\n");
        } else {
            for repo in live_repos {
                let optional = if repo.optional { " · optional" } else { "" };
                out.push_str(&format!(
                    "- repo: {} → {}{}\n",
                    repo.name, repo.relative_path, optional
                ));
            }
        }
        out.push('\n');
    }

    Ok(Some(out))
}

/// RESOLVE the Spec id for a context render from the tool args, in this order (PDR-0020):
///   1. an explicit `spec_id`;
///   2. the Spec anchored on the `workspace_id` (its dev workspace);
///   3. the Spec anchored on the workspace whose path matches `cwd` ("sans id → spec
///      résolue via cwd").
///
/// Returns `Ok(None)` when none resolves (the caller maps it to `invalid_id`).
fn resolve_spec_id(db: &Db, args: &Value) -> Result<Option<String>, RpcError> {
    if let Some(spec_id) = optional_str(args, "spec_id")? {
        return Ok(Some(spec_id.to_string()));
    }
    if let Some(workspace_id) = optional_str(args, "workspace_id")? {
        let spec = db
            .with_conn(|c| db::spec_for_workspace(c, workspace_id))
            .map_err(internal_db)?;
        return Ok(spec.map(|s| s.id));
    }
    if let Some(cwd) = optional_str(args, "cwd")? {
        let normalized = crate::pathnorm::normalize(cwd);
        let spec = db
            .with_conn(|c| -> Result<Option<db::Spec>, diesel::result::Error> {
                // Match the cwd against every workspace's root path (exact or a nested cwd),
                // then find the Spec anchored on the best match. Newest workspace wins ties.
                let workspaces = db::all_workspaces(c)?;
                let matched = workspaces
                    .into_iter()
                    .filter(|w| cwd_under_workspace(&normalized, &w.root_path))
                    .max_by(|a, b| a.root_path.len().cmp(&b.root_path.len()));
                match matched {
                    Some(ws) => db::spec_for_workspace(c, &ws.id),
                    None => Ok(None),
                }
            })
            .map_err(internal_db)?;
        return Ok(spec.map(|s| s.id));
    }
    Ok(None)
}

/// Whether a normalized `cwd` is the workspace's `root_path` or nested under it (so a cwd
/// deeper in the tree still resolves the workspace). A trailing separator is normalized so
/// `/a/ws` matches `/a/ws/sub` but not `/a/wsX`.
fn cwd_under_workspace(cwd: &str, workspace_root: &str) -> bool {
    if cwd == workspace_root {
        return true;
    }
    let prefix = if workspace_root.ends_with('/') {
        workspace_root.to_string()
    } else {
        format!("{workspace_root}/")
    };
    cwd.starts_with(&prefix)
}

/// `agent_context` — `{ role, spec_id? | workspace_id? | cwd? }` → `{ role, spec_id, context }`.
/// Render a role manual LIVE from nyx state. A pure READ (no `changed` effect).
pub fn agent_context(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let role_name = require_str(args, "role")?;
    let role = AgentRole::parse(role_name).ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown role '{role_name}' (accepted: dev, coder, auditor)"),
        )
    })?;
    let spec_id = resolve_spec_id(db, args)?.ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidId,
            "could not resolve a Spec: pass spec_id, or a workspace_id/cwd anchored on one",
        )
    })?;
    let context = render_agent_context(db, role, &spec_id)?
        .ok_or_else(|| RpcError::new(RpcCode::InvalidId, format!("unknown spec {spec_id}")))?;
    Ok(ToolOutcome::read(json!({
        "role": role_name,
        "spec_id": spec_id,
        "context": context,
    })))
}

/// Route the `agent_context` tool call, or `None` when `name` is not ours. Called from
/// [`crate::mcp_tools_core::dispatch_extension_tool`] (one dispatcher, one pool).
pub fn dispatch_context_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    match name {
        AGENT_CONTEXT_TOOL => Some(agent_context(db, args)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A memory DB seeded with a project + root workspace + a repo + an anchored Spec with
    /// two sections and a two-phase plan + one task per phase. Returns `(db, spec_id,
    /// workspace_id, cwd)`.
    fn seeded() -> (Db, String, String, String) {
        let db = Db::in_memory();
        let (spec_id, workspace_id, cwd) = db.with_conn(|c| {
            let (project, root) =
                db::create_project(c, "Proj", "/tmp/nyx-ctx", None).expect("project");
            db::create_repository(c, &project.id, "api", ".", false).expect("repo");
            let (spec, _rev) = db::create_spec_with_workspace(
                c,
                &project.id,
                &root.id,
                "My Feature",
                db::SpecSections {
                    problem: "the problem".into(),
                    solution: "the solution".into(),
                    ..Default::default()
                },
            )
            .expect("spec");
            db::set_spec_phases(
                c,
                &spec.id,
                r#"[{"number":1,"name":"Foundation"},{"number":2,"name":"UI"}]"#,
            )
            .expect("phases");
            let phase_task = |phase: i32| db::NewTaskInput {
                spec_id: Some(spec.id.clone()),
                phase: Some(phase),
                ..db::NewTaskInput::slice()
            };
            db::create_task(c, &project.id, "Phase 1 task", phase_task(1)).expect("t1");
            db::create_task(c, &project.id, "Phase 2 task", phase_task(2)).expect("t2");
            (spec.id, root.id, root.root_path)
        });
        (db, spec_id, workspace_id, cwd)
    }

    #[test]
    fn render_carries_role_spec_sections_current_phase_tasks_and_workspace_repos() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Coder, &spec_id)
            .unwrap()
            .unwrap();
        // Role preamble.
        assert!(
            text.contains("Context: Coder Agent"),
            "role preamble present"
        );
        // Spec sections.
        assert!(
            text.contains("the problem") && text.contains("the solution"),
            "sections"
        );
        // Current phase = the lowest phase with open work (phase 1), and its task.
        assert!(
            text.contains("Current phase — 1 · Foundation"),
            "current phase"
        );
        assert!(text.contains("Phase 1 task"), "current-phase task");
        // The phase-2 task is NOT in the current-phase list.
        assert!(!text.contains("Phase 2 task"), "later-phase task excluded");
        // Workspace + derived repos.
        assert!(text.contains("Workspace & repos"), "workspace section");
        assert!(text.contains("api → ."), "derived repo");
    }

    #[test]
    fn render_each_role_uses_its_own_preamble() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        assert!(render_agent_context(&db, AgentRole::Dev, &spec_id)
            .unwrap()
            .unwrap()
            .contains("Dev Orchestrator"));
        assert!(render_agent_context(&db, AgentRole::Auditor, &spec_id)
            .unwrap()
            .unwrap()
            .contains("Auditor Agent"));
    }

    #[test]
    fn agent_context_tool_resolves_by_spec_id_workspace_id_and_cwd() {
        let _g = db_guard();
        let (db, spec_id, workspace_id, cwd) = seeded();

        // By spec_id.
        let by_id = agent_context(&db, &json!({ "role": "coder", "spec_id": spec_id })).unwrap();
        assert_eq!(by_id.result["spec_id"], spec_id);
        assert!(by_id.effects.is_empty(), "context render is a pure read");

        // By workspace_id (the anchored dev workspace).
        let by_ws =
            agent_context(&db, &json!({ "role": "dev", "workspace_id": workspace_id })).unwrap();
        assert_eq!(by_ws.result["spec_id"], spec_id);

        // By cwd (a path nested under the workspace root).
        let nested = format!("{cwd}/crates/api");
        let by_cwd = agent_context(&db, &json!({ "role": "auditor", "cwd": nested })).unwrap();
        assert_eq!(by_cwd.result["spec_id"], spec_id);
    }

    #[test]
    fn agent_context_unknown_role_is_invalid_argument() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let err = agent_context(&db, &json!({ "role": "wizard", "spec_id": spec_id })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
    }

    #[test]
    fn agent_context_unresolvable_spec_is_invalid_id() {
        let _g = db_guard();
        let (db, _spec_id, _ws, _cwd) = seeded();
        // No spec_id/workspace_id/cwd → nothing to resolve.
        let err = agent_context(&db, &json!({ "role": "coder" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    #[test]
    fn dispatch_context_tool_returns_none_for_a_foreign_tool() {
        let _g = db_guard();
        let (db, _spec_id, _ws, _cwd) = seeded();
        assert!(dispatch_context_tool(&db, "list_projects", &json!({})).is_none());
    }
}
