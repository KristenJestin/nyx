//! Shell-agnostic MCP **Spec authoring + transition tools** — the DB-backed dispatch
//! logic for the v2.4 Phase-5 tools that let an agent AUTHOR and TRANSITION a Spec from
//! nyx state, with ZERO depot CLI (task `01KX1DH7XFT3X0WY221M3F2JFT`, PDR-0020).
//!
//! # Single mutation authority (no double path)
//!
//! Every tool here is a THIN adapter: it parses the JSON args, then delegates to the
//! phase-1/2/3 `nyx_core::db` authority (`create_spec` / `update_active_revision_section` /
//! `fork_spec` / `transition_spec` / `set_spec_workspace` / `create_task` /
//! `transition_task` / `verify_human_task` / `add_task_dependency`). The lifecycle GUARDS,
//! the pre-ready VALIDATIONS, the human-verifiability guard-rail, the sensitive-transition
//! user-confirmation guard-rail and the acyclic-graph cycle check ALL live in `db.rs`, the
//! SAME code path the native Specs UI (Phase 4 `spec_list`/`spec_detail`) reads from. The
//! MCP layer NEVER re-implements a rule — so the MCP mutation is the single authority, with
//! no parallel implementation to drift.
//!
//! # Effects
//!
//! Each mutating tool returns [`ChangedTopic::Specs`] so an open Specs surface re-pulls
//! (the front's re-pull wiring is phase 6; the topic is named here so the seam is complete).

use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, optional_bool, optional_str, require_str};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

// --- Tool name constants (advertised in `tools/list`, dispatched below) ------

/// `spec_create` — create a new Spec (+ its first revision) from nyx state.
pub const SPEC_CREATE_TOOL: &str = "spec_create";
/// `spec_update_section` — edit ONE structured section of a Spec's active revision.
pub const SPEC_UPDATE_SECTION_TOOL: &str = "spec_update_section";
/// `spec_fork` — fork a Spec into a NEW revision (the prior one superseded).
pub const SPEC_FORK_TOOL: &str = "spec_fork";
/// `spec_transition` — a GUARDED Spec lifecycle transition (ready/start/review/done/cancel).
pub const SPEC_TRANSITION_TOOL: &str = "spec_transition";
/// `spec_set_workspace` — anchor (or detach) a Spec on a dev workspace.
pub const SPEC_SET_WORKSPACE_TOOL: &str = "spec_set_workspace";
/// `task_add` — add a task (slice/human/review) to a Spec.
pub const TASK_ADD_TOOL: &str = "task_add";
/// `task_transition` — a GUARDED task transition (start/done/block/skip) or human verify.
pub const TASK_TRANSITION_TOOL: &str = "task_transition";
/// `task_add_dependency` — add an acyclic `depends_on` edge between two tasks.
pub const TASK_ADD_DEPENDENCY_TOOL: &str = "task_add_dependency";

/// The Spec authoring + transition tool names — advertised in `tools/list` and routed by
/// [`dispatch_spec_tool`]. A closed slice so a new tool is added in exactly one place.
pub const SPEC_TOOLS: [&str; 8] = [
    SPEC_CREATE_TOOL,
    SPEC_UPDATE_SECTION_TOOL,
    SPEC_FORK_TOOL,
    SPEC_TRANSITION_TOOL,
    SPEC_SET_WORKSPACE_TOOL,
    TASK_ADD_TOOL,
    TASK_TRANSITION_TOOL,
    TASK_ADD_DEPENDENCY_TOOL,
];

// --- Argument parsing helpers (the D8 vocabulary) --------------------------

/// Resolve the six-section vocabulary from a `section` string arg to the typed
/// [`db::SpecSection`]. An unknown name → `invalid_argument` naming the accepted set.
fn parse_section(name: &str) -> Result<db::SpecSection, RpcError> {
    match name {
        "problem" => Ok(db::SpecSection::Problem),
        "solution" => Ok(db::SpecSection::Solution),
        "implementation_decisions" | "implementationDecisions" => {
            Ok(db::SpecSection::ImplementationDecisions)
        }
        "testing_decisions" | "testingDecisions" => Ok(db::SpecSection::TestingDecisions),
        "user_stories" | "userStories" => Ok(db::SpecSection::UserStories),
        "out_of_scope" | "outOfScope" => Ok(db::SpecSection::OutOfScope),
        other => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "unknown section '{other}' (accepted: problem, solution, \
                 implementation_decisions, testing_decisions, user_stories, out_of_scope)"
            ),
        )),
    }
}

/// The six structured sections from OPTIONAL `problem` / `solution` / … string args (each
/// absent → `""`). Used by `spec_create` so a caller can seed the design in one call.
fn sections_from_args(args: &Value) -> Result<db::SpecSections, RpcError> {
    let get = |k: &str| -> Result<String, RpcError> {
        Ok(optional_str(args, k)?.unwrap_or("").to_string())
    };
    Ok(db::SpecSections {
        problem: get("problem")?,
        solution: get("solution")?,
        implementation_decisions: get("implementation_decisions")?,
        testing_decisions: get("testing_decisions")?,
        user_stories: get("user_stories")?,
        out_of_scope: get("out_of_scope")?,
    })
}

/// Resolve the `transition` string arg to the typed [`db::SpecTransition`]. Accepts the
/// conceptual verbs (`ready`/`start`/`review`/`done`/`cancel`) — the SAME vocabulary
/// depot's `spec` lifecycle used. An unknown verb → `invalid_argument`.
fn parse_spec_transition(name: &str) -> Result<db::SpecTransition, RpcError> {
    match name {
        "ready" => Ok(db::SpecTransition::Ready),
        "start" | "in_progress" => Ok(db::SpecTransition::Start),
        "review" => Ok(db::SpecTransition::Review),
        "done" | "complete" => Ok(db::SpecTransition::Done),
        "cancel" => Ok(db::SpecTransition::Cancel),
        other => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown transition '{other}' (accepted: ready, start, review, done, cancel)"),
        )),
    }
}

/// Resolve the `transition` string arg to the typed [`db::TaskTransition`]. `verify` is
/// handled separately (the human-verified path), so it is NOT parsed here.
fn parse_task_transition(name: &str) -> Result<db::TaskTransition, RpcError> {
    match name {
        "start" => Ok(db::TaskTransition::Start),
        "done" => Ok(db::TaskTransition::Done),
        "block" => Ok(db::TaskTransition::Block),
        "skip" => Ok(db::TaskTransition::Skip),
        other => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown transition '{other}' (accepted: start, done, block, skip, verify)"),
        )),
    }
}

// --- Error mappers (the D8 vocabulary, per delegated authority) ------------

/// Map a [`db::SpecTransitionError`] onto the D8 RPC vocabulary. The lifecycle rules live
/// in `db.rs`; this only translates the typed refusal into the agent-facing code/message.
fn map_spec_transition_err(e: db::SpecTransitionError) -> RpcError {
    match e {
        db::SpecTransitionError::NotFound => RpcError::new(RpcCode::InvalidId, "unknown spec"),
        db::SpecTransitionError::InvalidSource { from, to } => RpcError::new(
            RpcCode::InvalidState,
            format!("spec is '{from}', which cannot transition to '{to}'"),
        ),
        db::SpecTransitionError::ValidationFailed(failures) => {
            let detail = serde_json::to_value(&failures).unwrap_or(Value::Null);
            RpcError::new(
                RpcCode::InvalidState,
                "spec is not ready: the pre-ready validations failed (see data.failures)",
            )
            .with_data(json!({ "failures": detail }))
        }
        db::SpecTransitionError::ConfirmationRequired => RpcError::new(
            RpcCode::InvalidState,
            "this is a sensitive transition (ready/done/cancel): pass user_confirmed=true \
             once the human has confirmed",
        ),
        db::SpecTransitionError::Db(e) => internal_db(e),
    }
}

/// Map a [`db::TaskTransitionError`] onto the D8 RPC vocabulary.
fn map_task_transition_err(e: db::TaskTransitionError) -> RpcError {
    match e {
        db::TaskTransitionError::NotFound => RpcError::new(RpcCode::InvalidId, "unknown task"),
        db::TaskTransitionError::InvalidSource { from, to } => RpcError::new(
            RpcCode::InvalidState,
            format!("task is '{from}', which cannot transition to '{to}'"),
        ),
        db::TaskTransitionError::HumanRequiresVerification => RpcError::new(
            RpcCode::InvalidState,
            "a kind=human task closes only through the verified path: use \
             transition='verify' on a task that carries a verification_command",
        ),
        db::TaskTransitionError::CannotVerify { from } => RpcError::new(
            RpcCode::InvalidState,
            format!("cannot verify a task in '{from}': it must be in progress (start it first)"),
        ),
        db::TaskTransitionError::Db(e) => internal_db(e),
    }
}

/// Map a [`db::DependencyError`] onto the D8 RPC vocabulary.
fn map_dependency_err(e: db::DependencyError) -> RpcError {
    match e {
        db::DependencyError::TaskNotFound { id } => {
            RpcError::new(RpcCode::InvalidId, format!("unknown task {id}"))
        }
        db::DependencyError::SelfDependency => {
            RpcError::new(RpcCode::InvalidArgument, "a task cannot depend on itself")
        }
        db::DependencyError::WouldCreateCycle {
            task_id,
            depends_on_task_id,
        } => RpcError::new(
            RpcCode::InvalidState,
            format!(
                "adding {task_id} → depends_on → {depends_on_task_id} would create a \
                 dependency cycle; refused"
            ),
        ),
        db::DependencyError::Db(e) => internal_db(e),
    }
}

/// Map a [`db::SetSpecWorkspaceError`] onto the D8 RPC vocabulary.
fn map_set_workspace_err(e: db::SetSpecWorkspaceError) -> RpcError {
    match e {
        db::SetSpecWorkspaceError::SpecNotFound => {
            RpcError::new(RpcCode::InvalidId, "unknown spec")
        }
        db::SetSpecWorkspaceError::WorkspaceNotFound => {
            RpcError::new(RpcCode::InvalidId, "unknown workspace")
        }
        db::SetSpecWorkspaceError::Db(e) => internal_db(e),
    }
}

// --- Tool handlers ---------------------------------------------------------

/// `spec_create` — `{ project_id, title, workspace_id?, problem?, solution?,
/// implementation_decisions?, testing_decisions?, user_stories?, out_of_scope? }` →
/// `{ spec, revision }`. Creates a `draft` Spec (+ its first `active` revision) via the
/// SHARED `db::create_spec` / `db::create_spec_with_workspace` authority. When
/// `workspace_id` is given the Spec is anchored on that dev workspace at birth (the GUARD:
/// the workspace must exist).
pub fn spec_create(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let title = require_str(args, "title")?;
    let sections = sections_from_args(args)?;
    let workspace_id = optional_str(args, "workspace_id")?;

    let (spec, revision) = match workspace_id {
        Some(ws) => db
            .with_conn(|c| db::create_spec_with_workspace(c, project_id, ws, title, sections))
            .map_err(map_set_workspace_err)?,
        None => db
            .with_conn(|c| db::create_spec(c, project_id, title, sections))
            .map_err(internal_db)?,
    };
    Ok(ToolOutcome::changed(
        json!({ "spec": spec, "revision": revision }),
        ChangedTopic::Specs,
    ))
}

/// `spec_update_section` — `{ spec_id, section, value }` → `{ updated: true }`. Edits ONE
/// structured section of the Spec's ACTIVE revision in place (NOT a fork) via the SHARED
/// `db::update_active_revision_section`. An unknown Spec → `invalid_id`.
pub fn spec_update_section(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let section = parse_section(require_str(args, "section")?)?;
    // `value` is REQUIRED but may legitimately be an empty string (clearing a section);
    // read it directly so an explicit "" is honored rather than rejected by `require_str`.
    let value =
        match args.get("value") {
            Some(Value::String(s)) => s.as_str(),
            Some(Value::Null) | None => return Err(RpcError::new(
                RpcCode::InvalidArgument,
                "missing required argument 'value' (may be an empty string to clear the section)",
            )),
            Some(_) => {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    "argument 'value' must be a string",
                ))
            }
        };
    let updated = db
        .with_conn(|c| db::update_active_revision_section(c, spec_id, section, value))
        .map_err(internal_db)?;
    if !updated {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!("unknown spec {spec_id}"),
        ));
    }
    Ok(ToolOutcome::changed(
        json!({ "updated": true, "spec_id": spec_id }),
        ChangedTopic::Specs,
    ))
}

/// `spec_fork` — `{ spec_id, change_reason? }` → `{ revision }`. Forks the Spec into a NEW
/// active revision (the prior one superseded), snapshotting the current sections, via the
/// SHARED `db::fork_spec`. An unknown Spec → `invalid_id`.
pub fn spec_fork(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let change_reason = optional_str(args, "change_reason")?;
    let revision = db
        .with_conn(|c| db::fork_spec(c, spec_id, change_reason))
        .map_err(|e| match e {
            diesel::result::Error::NotFound => {
                RpcError::new(RpcCode::InvalidId, format!("unknown spec {spec_id}"))
            }
            other => internal_db(other),
        })?;
    Ok(ToolOutcome::changed(
        json!({ "revision": revision }),
        ChangedTopic::Specs,
    ))
}

/// `spec_transition` — `{ spec_id, transition, user_confirmed? }` → `{ spec }`. Performs a
/// GUARDED + VALIDATED lifecycle transition via the SHARED `db::transition_spec`. The
/// SENSITIVE transitions (ready/done/cancel) are refused unless `user_confirmed=true` — the
/// guard-rail is enforced in `db.rs`, so it holds through the tool exactly as through the
/// UI (à la depot `--user-confirmed`).
pub fn spec_transition(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let transition = parse_spec_transition(require_str(args, "transition")?)?;
    let user_confirmed = optional_bool(args, "user_confirmed")?.unwrap_or(false);
    let spec = db
        .with_conn(|c| db::transition_spec(c, spec_id, transition, user_confirmed))
        .map_err(map_spec_transition_err)?;
    Ok(ToolOutcome::changed(
        json!({ "spec": spec }),
        ChangedTopic::Specs,
    ))
}

/// `spec_set_workspace` — `{ spec_id, workspace_id? }` → `{ spec }`. Anchors the Spec on a
/// dev workspace (repos + project DERIVED through it), or DETACHES it when `workspace_id`
/// is absent/null. Delegates to the SHARED `db::set_spec_workspace` (the GUARD: a non-null
/// workspace must exist).
pub fn spec_set_workspace(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = require_str(args, "spec_id")?;
    let workspace_id = optional_str(args, "workspace_id")?;
    let spec = db
        .with_conn(|c| db::set_spec_workspace(c, spec_id, workspace_id))
        .map_err(map_set_workspace_err)?;
    Ok(ToolOutcome::changed(
        json!({ "spec": spec }),
        ChangedTopic::Specs,
    ))
}

/// `task_add` — `{ project_id, title, spec_id?, kind?, type?, priority?, severity?,
/// description?, acceptance_criteria?, verification_command?, parent_task_id?, phase? }` →
/// `{ task }`. Creates a task (slice/human/review) via the SHARED `db::create_task` (which
/// DERIVES `executor` from `kind`, keeping the pairing coherent). A `kind=human` task
/// SHOULD carry a `verification_command` so it is verifiable at close time.
pub fn task_add(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let title = require_str(args, "title")?;

    let mut input = db::NewTaskInput::default();
    if let Some(kind) = optional_str(args, "kind")? {
        input.kind = kind.to_string();
    }
    if let Some(type_) = optional_str(args, "type")? {
        input.type_ = type_.to_string();
    }
    if let Some(priority) = optional_str(args, "priority")? {
        input.priority = priority.to_string();
    }
    input.severity = optional_str(args, "severity")?.map(str::to_string);
    input.description = optional_str(args, "description")?.map(str::to_string);
    input.acceptance_criteria = optional_str(args, "acceptance_criteria")?.map(str::to_string);
    input.verification_command = optional_str(args, "verification_command")?.map(str::to_string);
    input.spec_id = optional_str(args, "spec_id")?.map(str::to_string);
    input.parent_task_id = optional_str(args, "parent_task_id")?.map(str::to_string);
    input.phase = match args.get("phase") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                "argument 'phase' must be an integer",
            )
        })? as i32),
    };

    let task = db
        .with_conn(|c| db::create_task(c, project_id, title, input.clone()))
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "task": task }),
        ChangedTopic::Specs,
    ))
}

/// `task_transition` — `{ task_id, transition, reason? }` → `{ task }`. Performs a GUARDED
/// task transition via the SHARED `db::transition_task` (start/done/block/skip), OR the
/// human-verified close via `db::verify_human_task` when `transition='verify'`. The
/// human-verifiability guard-rail (a `kind=human` task cannot be plain-`done`d) is enforced
/// in `db.rs`, so it holds through the tool. `reason` is stamped on block/skip.
pub fn task_transition(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let task_id = require_str(args, "task_id")?;
    let transition_name = require_str(args, "transition")?;

    // `verify` is the human-action verified close (the separate authority), NOT one of the
    // four plain transitions.
    if transition_name == "verify" {
        let task = db
            .with_conn(|c| db::verify_human_task(c, task_id))
            .map_err(map_task_transition_err)?;
        return Ok(ToolOutcome::changed(
            json!({ "task": task }),
            ChangedTopic::Specs,
        ));
    }

    let transition = parse_task_transition(transition_name)?;
    let reason = optional_str(args, "reason")?;
    let task = db
        .with_conn(|c| db::transition_task(c, task_id, transition, reason))
        .map_err(map_task_transition_err)?;
    Ok(ToolOutcome::changed(
        json!({ "task": task }),
        ChangedTopic::Specs,
    ))
}

/// `task_add_dependency` — `{ task_id, depends_on_task_id, type? }` → `{ added: true }`.
/// Adds an acyclic `task_id --depends_on--> depends_on_task_id` edge via the SHARED
/// `db::add_task_dependency`. The CYCLE CHECK + self-edge refusal live in `db.rs`; `type`
/// is `blocks` (ordering, default) or `relates_to` (soft link). A duplicate is idempotent.
pub fn task_add_dependency(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let task_id = require_str(args, "task_id")?;
    let depends_on_task_id = require_str(args, "depends_on_task_id")?;
    let dep_type = optional_str(args, "type")?.unwrap_or(db::DEPENDENCY_TYPE_BLOCKS);
    db.with_conn(|c| db::add_task_dependency(c, task_id, depends_on_task_id, dep_type))
        .map_err(map_dependency_err)?;
    Ok(ToolOutcome::changed(
        json!({
            "added": true,
            "task_id": task_id,
            "depends_on_task_id": depends_on_task_id,
            "type": dep_type,
        }),
        ChangedTopic::Specs,
    ))
}

// --- Dispatch --------------------------------------------------------------

/// Route a Spec authoring/transition tool call, or `None` when `name` is not one of ours.
/// Called from [`crate::mcp_tools_core::dispatch_extension_tool`] so the Spec tools share
/// the SAME dispatch entry point as every other extension tool (one dispatcher, one pool).
pub fn dispatch_spec_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    match name {
        SPEC_CREATE_TOOL => Some(spec_create(db, args)),
        SPEC_UPDATE_SECTION_TOOL => Some(spec_update_section(db, args)),
        SPEC_FORK_TOOL => Some(spec_fork(db, args)),
        SPEC_TRANSITION_TOOL => Some(spec_transition(db, args)),
        SPEC_SET_WORKSPACE_TOOL => Some(spec_set_workspace(db, args)),
        TASK_ADD_TOOL => Some(task_add(db, args)),
        TASK_TRANSITION_TOOL => Some(task_transition(db, args)),
        TASK_ADD_DEPENDENCY_TOOL => Some(task_add_dependency(db, args)),
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

    /// A memory DB seeded with one project + its root workspace (for the anchor tests).
    /// Returns `(db, project_id, workspace_id)`.
    fn seeded_db() -> (Db, String, String) {
        let db = Db::in_memory();
        let (project_id, workspace_id) = db.with_conn(|c| {
            let (project, root) = db::create_project(c, "Proj", "/tmp/ws", None).expect("project");
            (project.id, root.id)
        });
        (db, project_id, workspace_id)
    }

    fn spec_id_of(out: &ToolOutcome) -> String {
        out.result["spec"]["id"].as_str().unwrap().to_string()
    }

    #[test]
    fn spec_create_persists_a_draft_with_sections_and_changes_specs() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let out = spec_create(
            &db,
            &json!({
                "project_id": project_id,
                "title": "My Spec",
                "problem": "the problem",
                "solution": "the solution",
            }),
        )
        .unwrap();
        assert_eq!(out.effects, vec![ChangedTopic::Specs]);
        assert_eq!(out.result["spec"]["status"], "draft");
        assert_eq!(out.result["revision"]["problem"], "the problem");
        assert_eq!(out.result["revision"]["solution"], "the solution");
    }

    #[test]
    fn spec_create_with_workspace_anchors_and_unknown_workspace_is_invalid_id() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();
        let out = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Anchored", "workspace_id": ws }),
        )
        .unwrap();
        assert_eq!(out.result["spec"]["workspace_id"], ws);

        let err = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Bad", "workspace_id": "nope" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    #[test]
    fn spec_update_section_edits_the_active_revision_and_unknown_spec_is_invalid_id() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let sid = spec_id_of(&created);

        let out = spec_update_section(
            &db,
            &json!({ "spec_id": sid, "section": "problem", "value": "edited problem" }),
        )
        .unwrap();
        assert_eq!(out.result["updated"], true);
        // The edit is visible through the DB authority.
        let rev = db
            .with_conn(|c| db::get_active_revision(c, &sid))
            .unwrap()
            .unwrap();
        assert_eq!(rev.problem, "edited problem");

        // An empty value clears the section (honored, not rejected).
        spec_update_section(
            &db,
            &json!({ "spec_id": sid, "section": "problem", "value": "" }),
        )
        .unwrap();

        let err = spec_update_section(
            &db,
            &json!({ "spec_id": "nope", "section": "problem", "value": "x" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        let bad = spec_update_section(
            &db,
            &json!({ "spec_id": sid, "section": "not_a_section", "value": "x" }),
        )
        .unwrap_err();
        assert_eq!(bad.code, RpcCode::InvalidArgument);
    }

    #[test]
    fn spec_fork_makes_a_new_active_revision() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let sid = spec_id_of(&created);
        let out = spec_fork(&db, &json!({ "spec_id": sid, "change_reason": "rethink" })).unwrap();
        assert_eq!(out.result["revision"]["revision_number"], 2);
        assert_eq!(out.result["revision"]["change_reason"], "rethink");
    }

    /// The SENSITIVE-transition confirmation guard-rail must be ENFORCED THROUGH the tool:
    /// a `ready` without `user_confirmed` is refused, and even with confirmation it still
    /// runs the pre-ready validations (an empty draft fails).
    #[test]
    fn spec_transition_ready_honors_the_confirmation_guard_and_validations() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);

        // Without confirmation → refused by the guard-rail.
        let err =
            spec_transition(&db, &json!({ "spec_id": sid, "transition": "ready" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(err.message.contains("sensitive"));

        // With confirmation + a substantive draft (problem + solution present) → ready.
        let out = spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();
        assert_eq!(out.result["spec"]["status"], "ready");
    }

    #[test]
    fn spec_transition_ready_validation_failure_surfaces_failures() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        // A blank draft (no problem/solution) confirmed for ready still fails validation.
        let created =
            spec_create(&db, &json!({ "project_id": project_id, "title": "Blank" })).unwrap();
        let sid = spec_id_of(&created);
        let err = spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        // The failures are carried in data.failures.
        let failures = err.data.as_ref().unwrap()["failures"].as_array().unwrap();
        assert!(!failures.is_empty());
    }

    /// End-to-end authoring + transition through the tool surface: create → author →
    /// ready → start → task → task start/done. Proves an agent can drive a Spec end to
    /// end through the MCP dispatch (via the SHARED db authority).
    #[test]
    fn author_and_transition_a_spec_end_to_end_through_the_tools() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();

        // Create anchored on the workspace.
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Feature", "workspace_id": ws }),
        )
        .unwrap();
        let sid = spec_id_of(&created);

        // Author the two required sections.
        spec_update_section(
            &db,
            &json!({ "spec_id": sid, "section": "problem", "value": "why" }),
        )
        .unwrap();
        spec_update_section(
            &db,
            &json!({ "spec_id": sid, "section": "solution", "value": "how" }),
        )
        .unwrap();

        // ready (confirmed) → start.
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();
        let started =
            spec_transition(&db, &json!({ "spec_id": sid, "transition": "start" })).unwrap();
        assert_eq!(started.result["spec"]["status"], "in_progress");

        // Add a slice task and drive it start → done.
        let task_out = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "Do the thing", "spec_id": sid }),
        )
        .unwrap();
        let tid = task_out.result["task"]["id"].as_str().unwrap().to_string();
        task_transition(&db, &json!({ "task_id": tid, "transition": "start" })).unwrap();
        let done = task_transition(&db, &json!({ "task_id": tid, "transition": "done" })).unwrap();
        assert_eq!(done.result["task"]["status"], "done");
    }

    /// The human-verifiability guard-rail holds through the tool: a `kind=human` task
    /// cannot be plain-`done`d, but the `verify` path closes it when it carries a
    /// verification_command.
    #[test]
    fn task_transition_human_verify_guard_rail_holds_through_the_tool() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let task_out = task_add(
            &db,
            &json!({
                "project_id": project_id,
                "title": "Rotate the secret",
                "kind": "human",
                "verification_command": "manual: confirmed by user",
            }),
        )
        .unwrap();
        let tid = task_out.result["task"]["id"].as_str().unwrap().to_string();
        task_transition(&db, &json!({ "task_id": tid, "transition": "start" })).unwrap();

        // Plain done is refused for a human task.
        let err =
            task_transition(&db, &json!({ "task_id": tid, "transition": "done" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(err.message.contains("verified path"));

        // The verify path closes it.
        let verified =
            task_transition(&db, &json!({ "task_id": tid, "transition": "verify" })).unwrap();
        assert_eq!(verified.result["task"]["status"], "done");
    }

    /// Verifying a human task that was never STARTED speaks of the `verify` verb (not the
    /// internal `done` target) and guides the caller to start it first — the H8 fix for the
    /// misleading "cannot transition to 'done'" message.
    #[test]
    fn task_transition_verify_on_todo_speaks_of_verify_not_done() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let task_out = task_add(
            &db,
            &json!({
                "project_id": project_id,
                "title": "Rotate the secret",
                "kind": "human",
                "verification_command": "manual: confirmed by user",
            }),
        )
        .unwrap();
        let tid = task_out.result["task"]["id"].as_str().unwrap().to_string();

        // The task is still `todo` (never started) — verifying it is refused.
        let err =
            task_transition(&db, &json!({ "task_id": tid, "transition": "verify" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(
            err.message.contains("verify") && err.message.contains("todo"),
            "the error speaks of the verify verb + the source state, got: {}",
            err.message
        );
        assert!(
            !err.message.contains("done"),
            "the error must NOT leak the internal 'done' target, got: {}",
            err.message
        );
        assert!(
            err.message.contains("start"),
            "the error guides the caller to start first, got: {}",
            err.message
        );
    }

    #[test]
    fn task_add_dependency_refuses_a_cycle_through_the_tool() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let a = task_add(&db, &json!({ "project_id": project_id, "title": "A" })).unwrap();
        let b = task_add(&db, &json!({ "project_id": project_id, "title": "B" })).unwrap();
        let aid = a.result["task"]["id"].as_str().unwrap().to_string();
        let bid = b.result["task"]["id"].as_str().unwrap().to_string();

        // A depends on B — fine.
        task_add_dependency(&db, &json!({ "task_id": aid, "depends_on_task_id": bid })).unwrap();
        // B depends on A would close a cycle — refused.
        let err = task_add_dependency(&db, &json!({ "task_id": bid, "depends_on_task_id": aid }))
            .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
    }

    #[test]
    fn dispatch_spec_tool_returns_none_for_a_foreign_tool() {
        let _g = db_guard();
        let (db, _p, _w) = seeded_db();
        assert!(dispatch_spec_tool(&db, "list_projects", &json!({})).is_none());
    }
}
