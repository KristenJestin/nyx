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
use crate::mcp_runtime::{
    anchor_str, internal_db, map_content_write_err, optional_bool, optional_str, optional_usize,
    require_str,
};
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
/// `spec_set_category` — ASSIGN the Spec's one category (PDR-0017), or REMOVE it.
pub const SPEC_SET_CATEGORY_TOOL: &str = "spec_set_category";
/// `task_add` — add a task (slice/human/review) to a Spec.
pub const TASK_ADD_TOOL: &str = "task_add";
/// `task_transition` — a GUARDED task transition (start/done/block/skip) or human verify.
pub const TASK_TRANSITION_TOOL: &str = "task_transition";
/// `task_add_dependency` — add an acyclic `depends_on` edge between two tasks.
pub const TASK_ADD_DEPENDENCY_TOOL: &str = "task_add_dependency";
/// `task_delete` — DELETE a task outright (+ its dependency edges), GUARDED to a `draft` Spec.
pub const TASK_DELETE_TOOL: &str = "task_delete";
/// `spec_link` — add a Spec↔Spec dependency edge (blocks/after/related + note), cycle-checked.
pub const SPEC_LINK_TOOL: &str = "spec_link";
/// `spec_unlink` — remove a Spec↔Spec dependency edge.
pub const SPEC_UNLINK_TOOL: &str = "spec_unlink";
/// `spec_deps` — read a Spec's dependency view + the computable "bloquée par / débloque" badges.
pub const SPEC_DEPS_TOOL: &str = "spec_deps";

/// The Spec authoring tool names — advertised in `tools/list` and routed by [`dispatch_spec_tool`].
/// A closed slice so a new tool is added in exactly one place. `spec_transition` is DELIBERATELY
/// ABSENT from the agent surface (v2.8 review N17(b)): the four Spec lifecycle transitions are now
/// imposed by NYX at the graph node built for each (ndev/nspec cover start/review/done/cancel and
/// ready), so the effector never transitions a Spec — it only authors the draft. (`task_transition`
/// STAYS: no graph covers task-level granularity — N17(d).)
pub const SPEC_TOOLS: [&str; 12] = [
    SPEC_CREATE_TOOL,
    SPEC_UPDATE_SECTION_TOOL,
    SPEC_FORK_TOOL,
    SPEC_SET_WORKSPACE_TOOL,
    SPEC_SET_CATEGORY_TOOL,
    TASK_ADD_TOOL,
    TASK_TRANSITION_TOOL,
    TASK_ADD_DEPENDENCY_TOOL,
    TASK_DELETE_TOOL,
    SPEC_LINK_TOOL,
    SPEC_UNLINK_TOOL,
    SPEC_DEPS_TOOL,
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
/// `pub(crate)` so the `nyx_next` handler ([`crate::mcp_engine`]) — which imposes a graph
/// `transition` through the SAME guarded authority — surfaces the SAME refusal vocabulary.
pub(crate) fn map_spec_transition_err(e: db::SpecTransitionError) -> RpcError {
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
        db::SpecTransitionError::CompletionValidationFailed(failures) => {
            let detail = serde_json::to_value(&failures).unwrap_or(Value::Null);
            RpcError::new(
                RpcCode::InvalidState,
                "spec is not complete: the pre-done validations failed (see data.failures)",
            )
            .with_data(json!({ "failures": detail }))
        }
        db::SpecTransitionError::ConfirmationRequired => RpcError::new(
            RpcCode::InvalidState,
            "this is a sensitive transition (ready/start/done/cancel): pass user_confirmed=true \
             once the human has confirmed",
        ),
        db::SpecTransitionError::WorkspaceNotFound => {
            RpcError::new(RpcCode::InvalidId, "unknown workspace")
        }
        db::SpecTransitionError::Db(e) => internal_db(e),
    }
}

/// Map a [`db::TaskTransitionError`] onto the D8 RPC vocabulary.
pub(crate) fn map_task_transition_err(e: db::TaskTransitionError) -> RpcError {
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

/// Map a [`db::DeleteTaskError`] onto the D8 RPC vocabulary. The draft-only GUARD lives in
/// `db.rs`; this only translates the typed refusal. Unknown task → `invalid_id`; a parent
/// Spec that is not `draft` (or absent) → `invalid_state` (skip it instead).
fn map_delete_task_err(e: db::DeleteTaskError) -> RpcError {
    match e {
        db::DeleteTaskError::NotFound => RpcError::new(RpcCode::InvalidId, "unknown task"),
        // The refusal NAMES both legitimate moves, because the freeze covers only one of the two
        // things a caller may be trying to do. Deleting planned work mid-build is what it stops;
        // correcting a finding whose TEXT came out malformed is not the same authority, and the
        // caller that hits this wall with a mangled description would otherwise have nowhere to go.
        db::DeleteTaskError::NotDraft { status } => RpcError::new(
            RpcCode::InvalidState,
            format!(
                "task_delete is only legal while the parent Spec is 'draft' (this task's Spec \
                 is '{status}'). To DROP the work: skip the task (task_transition \
                 transition=skip), which cancels it and keeps the audit trail. To fix a review \
                 finding whose TITLE or DESCRIPTION is wrong — malformed text, leftover markup: \
                 rewrite it with review_task_structure(id, title, details), which edits the words \
                 and changes nothing about what the build owes"
            ),
        ),
        db::DeleteTaskError::Db(e) => internal_db(e),
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

/// Map a [`db::SpecDependencyError`] onto the D8 RPC vocabulary (mirrors [`map_dependency_err`]
/// for the Spec↔Spec graph).
fn map_spec_dependency_err(e: db::SpecDependencyError) -> RpcError {
    match e {
        db::SpecDependencyError::SpecNotFound { id } => {
            RpcError::new(RpcCode::InvalidId, format!("unknown spec {id}"))
        }
        db::SpecDependencyError::SelfDependency => {
            RpcError::new(RpcCode::InvalidArgument, "a Spec cannot depend on itself")
        }
        db::SpecDependencyError::WouldCreateCycle {
            from_spec_id,
            to_spec_id,
        } => RpcError::new(
            RpcCode::InvalidState,
            format!(
                "linking {from_spec_id} --depends_on--> {to_spec_id} would create a \
                 Spec dependency cycle; refused"
            ),
        ),
        db::SpecDependencyError::Db(e) => internal_db(e),
    }
}

/// Resolve the Spec-dependency `kind` arg to the vocabulary (`blocks` | `after` | `related`),
/// defaulting to `blocks` (a hard directional gate) when absent. An unknown value →
/// `invalid_argument`.
fn parse_spec_dep_kind(args: &Value) -> Result<&'static str, RpcError> {
    match optional_str(args, "kind")? {
        None => Ok(db::SPEC_DEP_KIND_BLOCKS),
        Some("blocks") => Ok(db::SPEC_DEP_KIND_BLOCKS),
        Some("after") => Ok(db::SPEC_DEP_KIND_AFTER),
        Some("related") => Ok(db::SPEC_DEP_KIND_RELATED),
        Some(other) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown kind '{other}' (accepted: blocks, after, related)"),
        )),
    }
}

/// Map a [`db::SetSpecWorkspaceError`] onto the D8 RPC vocabulary. `pub(crate)` so the
/// workspace tools (`workspace_add` / `create_workspace`, which anchor through the same
/// authority when given a `spec_id`) refuse with the SAME words as `spec_set_workspace`.
pub(crate) fn map_set_workspace_err(e: db::SetSpecWorkspaceError) -> RpcError {
    match e {
        db::SetSpecWorkspaceError::SpecNotFound => {
            RpcError::new(RpcCode::InvalidId, "unknown spec")
        }
        db::SetSpecWorkspaceError::WorkspaceNotFound => {
            RpcError::new(RpcCode::InvalidId, "unknown workspace")
        }
        db::SetSpecWorkspaceError::InvalidState { status } => RpcError::new(
            RpcCode::InvalidState,
            format!(
                "a Spec's workspace anchor can only be set while it is ready or in_progress \
                 (this Spec is '{status}'): a draft is still being authored, and a settled \
                 Spec's anchor is frozen. Linking a workspace never transitions the Spec"
            ),
        ),
        db::SetSpecWorkspaceError::ProjectMismatch {
            spec_project_id,
            workspace_project_id,
        } => RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "workspace belongs to project {workspace_project_id} but the Spec belongs to \
                 project {spec_project_id}: a Spec can only anchor on a workspace of its own \
                 project"
            ),
        ),
        // The PREVALIDATION twin: no workspace exists yet on this path (the folder is not even
        // created), so the refusal names the TARGET project instead of claiming a workspace.
        db::SetSpecWorkspaceError::SpecProjectMismatch {
            spec_project_id,
            target_project_id,
        } => RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "the Spec belongs to project {spec_project_id} but the workspace would be \
                 registered in project {target_project_id}: a Spec can only anchor on a \
                 workspace of its own project"
            ),
        ),
        db::SetSpecWorkspaceError::Db(e) => internal_db(e),
    }
}

/// Map a [`crate::category::CategoryError`] onto the RPC vocabulary — the STABLE codes the category
/// surface answers with, so a caller reacts on `error.data.code` instead of parsing prose:
///
///   * an unknown Spec or an unknown category ID is an `invalid_id` (a reference that names nothing
///     — the same code every other unknown id answers with);
///   * a CROSS-PROJECT category is an `invalid_argument` naming BOTH projects — the argument is
///     well-formed and the row exists, it just does not belong to this Spec's vocabulary (the twin
///     of `SetSpecWorkspaceError::SpecProjectMismatch` above, which answers the same way);
///   * an AMBIGUOUS name is an `invalid_argument` LISTING the slugs that matched, because the
///     caller's next move is to re-ask with one of them and it needs the exact strings;
///   * a BLANK name is an `invalid_argument`;
///   * the two DELETION refusals — a system category, a category N Specs still carry — are
///     `invalid_argument` carrying the service's sentence. They are UNREACHABLE from this surface
///     (no MCP tool deletes a category: curating the vocabulary is the human's, from the settings)
///     and mapped anyway, so the day one is added it answers like the others instead of falling
///     into a wildcard nobody chose.
///
/// An unknown LABEL is deliberately absent: it is not an error at all, it is a creation
/// ([`crate::category::resolve_or_create`] — PDR-0017's non-blocking mint).
pub fn map_category_err(e: crate::category::CategoryError) -> RpcError {
    use crate::category::CategoryError;
    match e {
        CategoryError::SpecNotFound => RpcError::new(RpcCode::InvalidId, "unknown spec"),
        CategoryError::NotFound => RpcError::new(
            RpcCode::InvalidId,
            "no category of this Spec's project answers that reference",
        ),
        e @ (CategoryError::ProjectMismatch { .. }
        | CategoryError::AmbiguousName { .. }
        | CategoryError::BlankName
        | CategoryError::SystemCategory { .. }
        | CategoryError::CategoryInUse { .. }) => {
            RpcError::new(RpcCode::InvalidArgument, e.to_string())
        }
        CategoryError::Db(e) => internal_db(e),
    }
}

/// The category as the MCP surface returns it: its ID, its STABLE `slug`, its human `name`, the
/// colour token a chip draws with, `created_by_type` — WHO minted the row — and `system`, whether
/// the slug is one of nyx's four.
///
/// **`system` IS A VERDICT, AND IT IS DERIVED HERE SO NOBODY RE-DERIVES IT.** It is
/// `db::SYSTEM_CATEGORY_SLUGS.contains(slug)` — the same list [`crate::category::delete`] refuses
/// on and [`crate::category::slugify_for`] reserves against. The settings screen hides its Delete
/// button on exactly these rows, and it must not do so by carrying a copy of the four literals in
/// TypeScript: a fifth system category would then be deletable on screen and refused by the
/// service, which is the drift this field exists to prevent.
///
/// **NO `state` FIELD, since the 2026-09-02 amendment of PDR-0017.** There is no proposed /
/// confirmed axis any more: the agent has full rights over the classification, so a mint is valid
/// as it stands and there is nothing for a consumer to branch on. `created_by_type` remains — the
/// provenance is a fact, and a useful one; the curation STATE was a verdict, and there is no longer
/// anyone to pronounce it. `confirmed_at` is still carried as the RAW column it is (always set,
/// read by nobody — see [`db::Category::confirmed_at`]), so the payload keeps mirroring the row
/// rather than hiding half of it.
///
/// `pub` since v2.9 phase 8: the SIDECAR's human curation commands (`category_list`,
/// `category_create`, `spec_category_assign`, `category_rename`) answer the app with the EXACT
/// same object. A second shape would mean the Spec-detail chip and the agent's `spec_category`
/// read disagreeing about the same row — one derivation, one payload, two surfaces.
pub fn category_payload(category: &db::Category) -> Value {
    json!({
        "id": category.id,
        "slug": category.slug,
        "name": category.name,
        "color_token": category.color_token,
        "created_by_type": category.created_by_type,
        "confirmed_at": category.confirmed_at,
        "system": db::SYSTEM_CATEGORY_SLUGS.contains(&category.slug.as_str()),
    })
}

// --- Tool handlers ---------------------------------------------------------

/// `spec_create` — `{ project_id, title, category?, problem?, solution?,
/// implementation_decisions?, testing_decisions?, user_stories?, out_of_scope? }` →
/// `{ spec, revision, category }`. Creates a `draft` Spec (+ its first `active` revision) via the
/// SHARED `db::create_spec` authority. A Spec is authored PROJECT-scoped and UNANCHORED (review
/// finding): there is NO `workspace_id` here — the dev workspace anchor is posed later, at the
/// ready→in_progress `start` transition (`spec_transition`), never at authoring.
///
/// `category` is the Spec's ONE genre-of-work axis (PDR-0017), named by slug (`bug`) or by human
/// name ("Dette technique"). It is OPTIONAL — an uncategorized Spec is a legitimate state — and an
/// unknown label MINTS a category rather than refusing. The classification rides the SAME
/// transaction as the creation ([`crate::category::create_spec_with_category`]), so a Spec is never
/// left half-classified and a minted category is never orphaned by a failure downstream of it.
///
/// ONLY THE ABSENCE OF THE ARGUMENT MEANS "UNCLASSIFIED", and it is read with the same
/// [`anchor_str`] the sibling verb `spec_set_category` uses. Read with `optional_str`, `""` folded
/// into `None` BEFORE the service — so the same uninitialised caller variable produced three
/// different outcomes on one argument: `""` created a silently UNCLASSIFIED Spec (precisely what
/// escapes the red-before-green discipline), `" "` reached the service's blank-label guard and took
/// the whole Spec down with it, and `""` on `spec_set_category` was an explicit refusal. One
/// spelling, one behaviour: an empty string is refused here too, with the sentence that says an
/// empty string is not a way to say "none".
///
/// The PROVENANCE filed is [`db::DomainEventSource::Mcp`]: this is the agent classifying its own
/// draft. The human's assignment, from the Spec detail screen, files `Ui` through the same service —
/// see [`crate::category::assign_spec_category`].
pub fn spec_create(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let title = require_str(args, "title")?;
    let sections = sections_from_args(args)?;
    let category = anchor_str(args, "category", "create the Spec UNCLASSIFIED")?;

    let (spec, revision, category) = db
        .with_conn(|c| {
            crate::category::create_spec_with_category(
                c,
                project_id,
                title,
                sections,
                category,
                db::DomainEventSource::Mcp,
            )
        })
        .map_err(map_category_err)?;
    // BIND the run that authored it (F5): a `/nspec` run starts with no Spec and CREATES one here, so
    // this is the moment — and the only one — where the run and its Spec can be tied together. Left
    // untied, the run stays inert (every Spec query answers null/0, every transition applies to
    // nothing) and the flow ends `done` on a Spec still `draft`, silently. Best-effort and strictly
    // guarded (see [`crate::mcp_engine::bind_session_run_to_spec`]): with no in-flight run to bind —
    // an author working outside any run — nothing happens and the created Spec is unaffected.
    let bound_run_id = crate::mcp_engine::bind_session_run_to_spec(
        db,
        crate::mcp_engine::shared_runtime(),
        &spec.id,
    );
    Ok(ToolOutcome::changed(
        json!({
            "spec": spec,
            "revision": revision,
            "bound_run_id": bound_run_id,
            "category": category.as_ref().map(category_payload),
        }),
        ChangedTopic::Specs,
    ))
}

/// `spec_set_category` — `{ spec_id, category? | category_id? }` → `{ spec, category, created }`.
/// ASSIGN the Spec's ONE category (PDR-0017), or REMOVE it by naming neither.
///
/// TWO WAYS TO NAME A CATEGORY, and they are separate arguments because they resolve by different
/// rules and fail in different ways ([`crate::category::CategoryRef`]):
///
///   * `category_id` — the EXACT row, what a caller sends having just read the list. An id that
///     names nothing is `invalid_id`; an id that names a category of ANOTHER project is
///     `invalid_argument` naming both projects (the same-project guard, which SQLite's FK cannot
///     express — it lives in [`crate::category::assign_spec_category`]).
///   * `category` — the ERGONOMIC reference an agent types: the slug it read in a workflow
///     (`bug`), or the human name it saw on a chip. Resolved EXACT SLUG FIRST, then by normalized
///     name; a name matching two categories is refused, LISTING their slugs, rather than guessed.
///     An UNKNOWN label is not a refusal — it MINTS a category (`created_by_type=agent`, valid as
///     it stands since the 2026-09-02 amendment), and the response says `created: true`.
///     Cross-project is unreachable this way: the label is resolved inside the SPEC'S OWN project.
///
/// Passing BOTH is `invalid_argument` — two references that can disagree, and picking one silently
/// would classify the Spec by an argument the caller did not intend.
///
/// NAMING NEITHER IS THE REMOVAL, and it is a first-class write with its own event, not a no-op.
/// An EMPTY STRING is refused on both arguments (through [`anchor_str`]): here `None` is not
/// "nothing was said", it IS the removal, so absorbing `""` into it would let an uninitialized
/// caller variable silently uncategorize a Spec.
///
/// THE CATEGORY IS NOT FROZEN BY THE LIFECYCLE. Unlike `spec_update_section`, this call is accepted
/// at ANY Spec status: a classification is metadata about the genre of work, not prose the build
/// contracted on, and mis-classifying a Spec must stay correctable after it left draft — which is
/// precisely when a coder preflight discovers it. (A recategorization does NOT replay gates already
/// crossed; it changes what the next ones read.)
pub fn spec_set_category(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
    let removes = "REMOVE the Spec's category";
    let category_id = anchor_str(args, "category_id", removes)?;
    let label = anchor_str(args, "category", removes)?;
    if category_id.is_some() && label.is_some() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "pass EITHER 'category_id' (an exact category) OR 'category' (a slug or a name), not \
             both — two references that can disagree, and nyx will not choose between them. Omit \
             both to REMOVE the Spec's category",
        ));
    }

    let (spec, category, created) = db
        .with_conn(|c| match label {
            // The ERGONOMIC path, and the ONLY one that may mint (PDR-0017).
            Some(label) => crate::category::set_spec_category_by_label(
                c,
                spec_id,
                label,
                db::DomainEventSource::Mcp,
            )
            .map(|(spec, category, created)| (spec, Some(category), created)),
            // The EXACT path, and the REMOVAL — one call, because `None` is the removal.
            None => crate::category::assign_spec_category(
                c,
                spec_id,
                category_id.map(crate::category::CategoryRef::Id),
                db::DomainEventSource::Mcp,
            )
            .and_then(|spec| {
                let category = match spec.category_id.as_deref() {
                    Some(id) => db::get_category(c, id)?,
                    None => None,
                };
                Ok((spec, category, false))
            }),
        })
        .map_err(map_category_err)?;

    Ok(ToolOutcome::changed(
        json!({
            "spec": spec,
            "category": category.as_ref().map(category_payload),
            "created": created,
        }),
        ChangedTopic::Specs,
    ))
}

/// `spec_update_section` — `{ spec_id, section, value, run_id? }` → `{ updated: true }`. Edits ONE
/// structured section of the Spec's ACTIVE revision in place (NOT a fork) via the SHARED
/// `db::update_active_revision_section`. An unknown Spec → `invalid_id`.
///
/// `run_id` is the HUMAN-GATED exception to the freeze (H12), and nothing else: a Spec that has left
/// its writable state refuses every section write, EXCEPT for the run named here when that run carries
/// the authorization nyx's engine wrote from a human's answer at the coder escalation gate ("the
/// contradiction is in the Spec's TEXT — fix the Spec"). The caller does not claim the right; it names
/// the run, and nyx reads whether the human granted it
/// ([`crate::mcp_engine::spec_section_write_is_authorized`]). A `run_id` on a WRITABLE Spec changes
/// nothing — that write needed no exception.
pub fn spec_update_section(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
    let section = parse_section(require_str(args, "section")?)?;
    let run_id = optional_str(args, "run_id")?;
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
    let human_gated = run_id
        .map(|run_id| crate::mcp_engine::spec_section_write_is_authorized(db, run_id, spec_id))
        .unwrap_or(false);
    let updated = if human_gated {
        db.with_conn(|c| db::update_active_revision_section_human_gated(c, spec_id, section, value))
    } else {
        db.with_conn(|c| db::update_active_revision_section(c, spec_id, section, value))
    }
    .map_err(map_content_write_err)?;
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
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
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

/// `spec_transition` — `{ spec_id, transition, user_confirmed?, workspace_id?, cwd? }` →
/// `{ spec }`. Performs a GUARDED + VALIDATED lifecycle transition via the SHARED
/// `db::transition_spec_with_workspace`. The SENSITIVE transitions (ready/start/done/cancel)
/// are refused unless `user_confirmed=true` — the guard-rail is enforced in `db.rs`, so it
/// holds through the tool exactly as through the UI (à la depot `--user-confirmed`).
///
/// On `start` (ready→in_progress) the transition ALSO POSES the Spec's workspace anchor in
/// the SAME operation: the dev workspace is resolved from an explicit `workspace_id`, else
/// from `cwd` (the same cwd→workspace matcher `resolve_spec_id` uses). For every other
/// transition no anchor is posed (`workspace_id`/`cwd` are ignored).
///
/// An EMPTY `workspace_id` is REFUSED on `start` — the FOURTH anchoring surface, hardened
/// 2026-08-19 (finding `01M080S6Y5CTWK53FYY9YJF1GY`, HORS PERIMETRE ASSUME of PRD
/// 01KYA475AX5JBGSS8WK5J8NM1S). The refusal lives in [`crate::mcp_context::resolve_workspace_for_start`],
/// with the reasoning; here it matters that it happens BEFORE the write, so a refused anchor
/// leaves the Spec's status exactly where it was.
pub fn spec_transition(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id = crate::mcp_context::require_spec_arg(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
    let transition = parse_spec_transition(require_str(args, "transition")?)?;
    let user_confirmed = optional_bool(args, "user_confirmed")?.unwrap_or(false);
    // Resolve the anchor ONLY for `start` — the one transition that binds the Spec to a dev
    // workspace. Reuses the shared cwd→workspace resolution (no duplicated matcher).
    let workspace_id = if transition == db::SpecTransition::Start {
        crate::mcp_context::resolve_workspace_for_start(db, args)?
    } else {
        None
    };
    let spec = db
        .with_conn(|c| {
            db::transition_spec_with_workspace(
                c,
                spec_id,
                transition,
                user_confirmed,
                workspace_id.as_deref(),
            )
        })
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
///
/// An EMPTY `workspace_id` is REFUSED (review F2), through [`anchor_str`]: here `None` is not
/// "nothing was said", it is the DETACH, so absorbing `""` into it turned an uninitialized caller
/// variable into a silent un-anchoring. Absent and null keep their meaning exactly — detach.
///
/// THE ONE PROJECT ANCHOR IS THE ONE THE DETACH OMITS, which is what makes this call's `#N` a
/// CONDITIONAL form rather than an unconditional one: with a `workspace_id` the number is counted in
/// that workspace's project, and without it there is no project at all — so the reference is refused
/// BY NAME at the entry point instead of travelling down to `db::set_spec_workspace` and coming back
/// as a bare `unknown spec` that did not even quote the token sent (finding
/// `01M0ZKWXS353TABS0WCT3XXB81`). The description states the same condition.
pub fn spec_set_workspace(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
    let workspace_id = anchor_str(
        args,
        "workspace_id",
        "DETACH the Spec from its dev workspace",
    )?;
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
///
/// `phase` is a NAME (a string): while the Spec is `draft` nyx MATERIALIZES its plan from the
/// names the tasks carry (get-or-create, 1-based, first-seen order — [`db::create_task`] →
/// `resolve_or_create_phase`), so a task's phase always exists in the plan (no orphan). Once
/// the Spec has LEFT draft that plan is frozen: an unknown name is REFUSED, naming the phases
/// that exist, instead of growing the plan of a Spec already being built (K2). A phase name
/// needs a `spec_id` (the plan lives on the Spec) — passing one without a Spec is refused.
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
    input.spec_id = crate::mcp_context::optional_spec_arg(db, args, "spec_id")?;
    input.parent_task_id = optional_str(args, "parent_task_id")?.map(str::to_string);
    // ROUND (v2.5.11): the rework sub-phase a finding is rattached to (PER-PHASE, so it reads "2.1",
    // "4.1"…). OMITTED IS THE NORMAL CASE for a `kind=review` finding: `db::create_task` then fills
    // it from the Spec's own review pass, because the auditor raising the finding cannot know which
    // round it is in — and a finding that does not say which pass produced it cannot be arbitrated
    // afterwards. An explicit value still wins, for the caller attributing a finding to another pass.
    input.round = optional_usize(args, "round")?.map(|r| r as i32);
    // `phase` is a phase NAME: nyx materializes the plan from the names in first-seen order while
    // the Spec is drafted, and refuses an unknown one after (`db::create_task`). It needs a Spec —
    // the plan lives on the Spec.
    input.phase_name = optional_str(args, "phase")?.map(str::to_string);
    if input.phase_name.is_some() && input.spec_id.is_none() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "argument 'phase' needs a 'spec_id' — the phase plan lives on the Spec",
        ));
    }

    let task = db
        .with_conn(|c| db::create_task(c, project_id, title, input.clone()))
        .map_err(map_content_write_err)?;
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

/// `task_delete` — `{ task_id }` → `{ deleted: true, task_id }`. DELETES the task outright
/// (+ its `task_dependencies` edges, in either direction) via the SHARED `db::delete_task`,
/// GUARDED to a parent Spec still in `draft`. This is the draft-authoring hard removal — a
/// mis-sliced task disappears cleanly while you are still shaping the plan — vs. `skip`
/// (`task_transition`), which only cancels and keeps the ghost and is the legitimate move
/// once the Spec is ready/in_progress. The draft GUARD lives in `db.rs`: an unknown task →
/// `invalid_id`, a non-draft (or Spec-less) task → `invalid_state`. The Spec's auto-built
/// phase plan is left intact (an emptied phase entry stays, keeping the plan a clean 1..N).
pub fn task_delete(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let task_id = require_str(args, "task_id")?;
    db.with_conn(|c| db::delete_task(c, task_id))
        .map_err(map_delete_task_err)?;
    Ok(ToolOutcome::changed(
        json!({ "deleted": true, "task_id": task_id }),
        ChangedTopic::Specs,
    ))
}

/// `spec_link` — `{ from_spec_id, to_spec_id, kind?, note? }` → `{ dependency }`. Adds a directed
/// Spec↔Spec dependency edge (from depends on / comes after to) via the SHARED
/// `db::add_spec_dependency`. The CYCLE CHECK (directional kinds only — `related` is context and
/// never cycles), the self-edge refusal and the endpoint resolution live in `db.rs`. `kind` is
/// `blocks` (default) / `after` (directional) or `related` (context only); `note` is an optional
/// rationale. Re-linking a pair UPSERTS the kind/note. A cycle → `invalid_state`.
pub fn spec_link(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let from_spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "from_spec_id")?;
    let to_spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "to_spec_id")?;
    let (from_spec_id, to_spec_id) = (from_spec_id.as_str(), to_spec_id.as_str());
    let kind = parse_spec_dep_kind(args)?;
    let note = optional_str(args, "note")?;
    let dependency = db
        .with_conn(|c| db::add_spec_dependency(c, from_spec_id, to_spec_id, kind, note))
        .map_err(map_spec_dependency_err)?;
    Ok(ToolOutcome::changed(
        json!({ "dependency": dependency }),
        ChangedTopic::Specs,
    ))
}

/// `spec_unlink` — `{ from_spec_id, to_spec_id }` → `{ unlinked }`. Removes a Spec↔Spec dependency
/// edge via the SHARED `db::remove_spec_dependency`. Idempotent (`unlinked` is `false` when the
/// pair was not linked).
pub fn spec_unlink(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let from_spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "from_spec_id")?;
    let to_spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "to_spec_id")?;
    let (from_spec_id, to_spec_id) = (from_spec_id.as_str(), to_spec_id.as_str());
    let removed = db
        .with_conn(|c| db::remove_spec_dependency(c, from_spec_id, to_spec_id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        json!({ "unlinked": removed > 0, "from_spec_id": from_spec_id, "to_spec_id": to_spec_id }),
        ChangedTopic::Specs,
    ))
}

/// `spec_deps` — `{ spec_id }` → the Spec's dependency VIEW: `blocked_by` / `unblocks` (the
/// directional edges, each with the other Spec's title/number/status) + `related` context links,
/// plus the COMPUTABLE badge counts `blocked_by_count` ("bloquée par N") and `unblocks_count`
/// ("débloque M"). The read behind the board / Spec-detail badges (the UI is Phase 4). Read-only.
pub fn spec_deps(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
    let view = db
        .with_conn(|c| db::spec_dependency_view(c, spec_id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(json!(view)))
}

// --- Auto-map: the Spec a spec-scoped MUTATION worked on ---------------------

/// The Spec id a successful spec-authoring tool call WORKED ON — the auto-map signal
/// (v2.5.10 Phase 1, task 01KXN0HTAXXMXF3GXF4ACM2970). Given the tool `name` and its
/// successful `result`, returns the Spec the call touched so the caller can ADD it to the
/// calling session's set (`db::link_terminal_active_session_spec`). Returns `None` for:
///   * a READ or non-Spec tool (never a work signal — a simple read must NOT map, so those
///     names fall through to `None`);
///   * a task tool whose task carries NO Spec (`spec_id` null — a project-scoped light task
///     is not Spec work);
///   * a result whose expected id is absent/malformed (defensive).
///
/// Pure + deterministic (reads ONLY the tool's own result JSON — the SAME `spec`/`task`
/// object the tool already returns), so it is unit-testable in isolation and adds no DB
/// round-trip. The ACCUMULATION is CIBLÉE: only the MUTATING Spec authoring/transition tools
/// (`spec_create`, `spec_update_section`, `spec_fork`, `spec_transition`, `spec_set_workspace`,
/// `task_add`, `task_transition`) are work signals — exactly the PRD's `spec_create /
/// spec_update_section / task_add / task_transition / dev` set (a `spec_transition
/// transition=start` is the "dev" launch). `task_add_dependency` / `task_delete` and the v2.6
/// Spec-dependency tools (`spec_link` / `spec_unlink` / `spec_deps`) are deliberately NOT work
/// signals (a structural edge tweak / a badge read / a draft cleanup is not "working the Spec",
/// and a deleted task's Spec is gone from the result) — they fall through to `None`.
pub fn worked_spec_id(name: &str, result: &Value) -> Option<String> {
    let str_at = |obj: &Value, path: &[&str]| -> Option<String> {
        let mut cur = obj;
        for key in path {
            cur = cur.get(key)?;
        }
        cur.as_str().map(str::to_string)
    };
    match name {
        // The Spec is returned directly as `result.spec`. (`spec_transition` is no longer an agent
        // verb — N17(b); nyx transitions at the node, and `run_start` below is the map signal.)
        SPEC_CREATE_TOOL | SPEC_SET_WORKSPACE_TOOL => str_at(result, &["spec", "id"]),
        // `spec_update_section` echoes the edited `result.spec_id`.
        SPEC_UPDATE_SECTION_TOOL => str_at(result, &["spec_id"]),
        // A fork returns the new `result.revision`, which carries its `spec_id`.
        SPEC_FORK_TOOL => str_at(result, &["revision", "spec_id"]),
        // A task tool returns `result.task`; its `spec_id` is the worked Spec (null = a
        // project-scoped task with no Spec → `None`, no mapping).
        TASK_ADD_TOOL | TASK_TRANSITION_TOOL => str_at(result, &["task", "spec_id"]),
        // RETOUR-CHAT (v2.5.11): relaying the human's chat feedback CREATES a finding on the Spec —
        // a work signal. It returns `result.finding`; its `spec_id` is the worked Spec. (This is the
        // "extend the auto-map when a finding promotion verb is added" the PRD calls for.)
        crate::mcp::REVIEW_SUBMIT_FEEDBACK_TOOL => str_at(result, &["finding", "spec_id"]),
        // ENGINE cutover (v2.8 review N15): with the thin dispatcher the agent no longer calls the
        // mutating `spec_*` verbs — nyx runs them INSIDE the `nyx_next` tick — so those signals never
        // reach this auto-map, and the session ↔ Spec link (the terminal overlay's substrate) went
        // unwritten. `run_start` RESOLVES + returns the bound `spec_id` (null for an unbound run), so
        // OPENING a run on a Spec is the work signal that re-establishes the link EXPLICITLY, from the
        // run, at the moment it opens — not inferred from a mutation that no longer happens agent-side.
        crate::mcp_engine::RUN_START_TOOL => str_at(result, &["spec_id"]),
        // …and its RECOVERY twin, for the same reason: a run that opened UNBOUND wrote no link at
        // all, so the session ↔ Spec link is established exactly when the run finally learns which
        // Spec it drives. Leaving it out would make the repaired session the ONE case where the
        // overlay stays empty — the very session that already went wrong once.
        crate::mcp_engine::RUN_BIND_SPEC_TOOL => str_at(result, &["spec_id"]),
        // Reads, non-Spec tools, and the non-work Spec tools never map.
        _ => None,
    }
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
        // `spec_transition` is intentionally NOT routed to the agent (N17(b)) — nyx transitions the
        // Spec at the graph node; the effector never does. The function stays for the UI/tests.
        SPEC_SET_WORKSPACE_TOOL => Some(spec_set_workspace(db, args)),
        SPEC_SET_CATEGORY_TOOL => Some(spec_set_category(db, args)),
        TASK_ADD_TOOL => Some(task_add(db, args)),
        TASK_TRANSITION_TOOL => Some(task_transition(db, args)),
        TASK_ADD_DEPENDENCY_TOOL => Some(task_add_dependency(db, args)),
        TASK_DELETE_TOOL => Some(task_delete(db, args)),
        SPEC_LINK_TOOL => Some(spec_link(db, args)),
        SPEC_UNLINK_TOOL => Some(spec_unlink(db, args)),
        SPEC_DEPS_TOOL => Some(spec_deps(db, args)),
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

    /// DONE-CRITERION (task 01KXN0HTAXXMXF3GXF4ACM2970): the auto-map's targeting is CIBLÉE —
    /// a MUTATING Spec tool yields the worked Spec id, a READ (or a project-scoped task with no
    /// Spec, or a non-work Spec tool) yields `None`. Pure over the tool result JSON.
    #[test]
    fn worked_spec_id_targets_mutations_and_ignores_reads() {
        // Mutating Spec authoring/transition tools → the worked Spec id.
        assert_eq!(
            worked_spec_id(SPEC_CREATE_TOOL, &json!({ "spec": { "id": "S1" } })).as_deref(),
            Some("S1")
        );
        assert_eq!(
            worked_spec_id(SPEC_UPDATE_SECTION_TOOL, &json!({ "spec_id": "S2" })).as_deref(),
            Some("S2")
        );
        assert_eq!(
            worked_spec_id(SPEC_FORK_TOOL, &json!({ "revision": { "spec_id": "S3" } })).as_deref(),
            Some("S3")
        );
        assert_eq!(
            worked_spec_id(TASK_ADD_TOOL, &json!({ "task": { "spec_id": "S5" } })).as_deref(),
            Some("S5")
        );
        assert_eq!(
            worked_spec_id(
                TASK_TRANSITION_TOOL,
                &json!({ "task": { "spec_id": "S6" } })
            )
            .as_deref(),
            Some("S6")
        );

        // A task with NO Spec (project-scoped light work) is NOT Spec work.
        assert_eq!(
            worked_spec_id(TASK_ADD_TOOL, &json!({ "task": { "spec_id": null } })),
            None
        );
        // RETOUR-CHAT (v2.5.11): relaying the human's feedback CREATES a finding on the Spec — it
        // maps the worked Spec via `result.finding.spec_id` (the extended auto-map).
        assert_eq!(
            worked_spec_id(
                crate::mcp::REVIEW_SUBMIT_FEEDBACK_TOOL,
                &json!({ "finding": { "spec_id": "S7" }, "comment": { "id": "c1" } })
            )
            .as_deref(),
            Some("S7")
        );
        // N15 — OPENING a run on a Spec is the work signal under the thin dispatcher (the agent no
        // longer calls the mutating spec_* verbs): `run_start` echoes the bound `spec_id`.
        assert_eq!(
            worked_spec_id(
                crate::mcp_engine::RUN_START_TOOL,
                &json!({ "run_id": "R1", "spec_id": "S8" })
            )
            .as_deref(),
            Some("S8")
        );
        // An UNBOUND run (spec_id null) maps nothing.
        assert_eq!(
            worked_spec_id(
                crate::mcp_engine::RUN_START_TOOL,
                &json!({ "run_id": "R2", "spec_id": null })
            ),
            None
        );
        // Reads + non-Spec tools never map.
        assert_eq!(
            worked_spec_id("list_projects", &json!({ "projects": [] })),
            None
        );
        assert_eq!(
            worked_spec_id("agent_context", &json!({ "context": "…" })),
            None
        );
        // The non-work Spec tools (an edge tweak / a draft cleanup) are deliberately no-ops.
        assert_eq!(
            worked_spec_id(TASK_ADD_DEPENDENCY_TOOL, &json!({ "added": true })),
            None
        );
        assert_eq!(
            worked_spec_id(TASK_DELETE_TOOL, &json!({ "deleted": true })),
            None
        );
        // The v2.6 Spec-dependency tools (a structural edge tweak / a badge read) never map.
        assert_eq!(
            worked_spec_id(
                SPEC_LINK_TOOL,
                &json!({ "dependency": { "from_spec_id": "S" } })
            ),
            None
        );
        assert_eq!(
            worked_spec_id(SPEC_UNLINK_TOOL, &json!({ "unlinked": true })),
            None
        );
        assert_eq!(
            worked_spec_id(SPEC_DEPS_TOOL, &json!({ "blocked_by": [] })),
            None
        );
        // THE TWO RUN-LIFECYCLE SIGNALS. `run_start` maps the Spec it opened on; `run_bind_spec`
        // maps the Spec a run that opened UNBOUND was finally attached to — the repaired session is
        // exactly the one whose link was never written, so leaving it out would keep the terminal's
        // Spec overlay empty for the session that already went wrong once.
        assert_eq!(
            worked_spec_id(
                crate::mcp_engine::RUN_START_TOOL,
                &json!({ "run_id": "R1", "spec_id": "S8" })
            )
            .as_deref(),
            Some("S8")
        );
        assert_eq!(
            worked_spec_id(
                crate::mcp_engine::RUN_BIND_SPEC_TOOL,
                &json!({ "run_id": "R2", "spec_id": "S9" })
            )
            .as_deref(),
            Some("S9")
        );
        // An UNBOUND `run_start` (a /nspec with no Spec yet) maps nothing — null is not a Spec.
        assert_eq!(
            worked_spec_id(
                crate::mcp_engine::RUN_START_TOOL,
                &json!({ "run_id": "R3", "spec_id": null })
            ),
            None
        );
    }

    /// THE `spec_*` TOOLS INHERIT THE SHORT-REFERENCE GRAMMAR, and not one of them implements it:
    /// each reads its argument through [`crate::mcp_context::require_spec_arg`], the SAME entry
    /// point `agent_context` and `run_start` go through. Driven on the HUMAN KEY (which names its
    /// own project, so it resolves on a call carrying no anchor at all) in either case, and on the
    /// `#N` / bare `N` where the call carries a project — the discipline the module states: a
    /// number counts in a project, so it needs one to count in.
    #[test]
    fn the_spec_tools_accept_a_short_spec_reference() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let (spec_id, other_id, key, number) = db.with_conn(|c| {
            let mut make = |title: &str| {
                db::create_spec(
                    c,
                    &project_id,
                    title,
                    db::SpecSections {
                        problem: "p".into(),
                        solution: "s".into(),
                        ..db::SpecSections::empty()
                    },
                )
                .expect("spec")
                .0
            };
            let spec = make("Referenced");
            let other = make("Dependent");
            let project = db::get_project(c, &project_id).unwrap().unwrap();
            (spec.id, other.id, project.key, spec.number)
        });

        // The TWO-ENDPOINT tool reads BOTH its arguments through the grammar…
        spec_link(
            &db,
            &json!({
                "from_spec_id": format!("{key}-{}", number + 1),
                "to_spec_id": format!("{key}-{number}"),
            }),
        )
        .expect("spec_link by human key");
        // …and the READ sees the edge those keys created.
        let deps = spec_deps(&db, &json!({ "spec_id": format!("{key}-{}", number + 1) }))
            .expect("spec_deps by human key");
        assert_eq!(
            deps.result["blocked_by"][0]["spec_id"],
            json!(spec_id),
            "the edge links the two Specs the keys named"
        );
        // Read from the OTHER end, by key too: the edge points back at the Spec `KEY-(N+1)` names.
        let back = spec_deps(&db, &json!({ "spec_id": format!("{key}-{number}") }))
            .expect("spec_deps by human key");
        assert_eq!(back.result["unblocks"][0]["spec_id"], json!(other_id));
        spec_update_section(
            &db,
            &json!({
                "spec_id": format!("{}-{number}", key.to_lowercase()),
                "section": "problem",
                "value": "rewritten through the grammar",
            }),
        )
        .expect("spec_update_section by lower-cased human key");
        spec_fork(&db, &json!({ "spec_id": format!("{key}-{number}") })).expect("spec_fork");

        // `#N` and the bare `N` on a call that DOES carry its project.
        for token in [format!("#{number}"), number.to_string()] {
            let out = task_add(
                &db,
                &json!({ "project_id": project_id, "title": "t", "spec_id": token }),
            )
            .unwrap_or_else(|e| panic!("form '{token}' was refused: {e:?}"));
            assert_eq!(
                out.result["task"]["spec_id"],
                json!(spec_id),
                "form '{token}' must attach the task to the Spec it names"
            );
        }
    }

    /// THE OTHER HALF OF "WHAT NYX ADVERTISES, NYX ACCEPTS": what a tool CANNOT resolve, it
    /// refuses BY NAME. `spec_fork`, `spec_deps`, `spec_link` and their siblings — more `spec_id`
    /// FIELDS than tools, since `spec_link` and `spec_unlink` take two each — take none of the four
    /// project anchors, so a `#N` / bare `N` has nothing to be counted in. It used
    /// to travel down as a literal and come back as `unknown spec 3` — the exact opaque sentence
    /// this PRD exists to retire, on a field that had just PROMISED the form (finding
    /// `01M0ZGCCSQ2F25HPBS6NX5JAFS`). Now the refusal is raised at the entry point, says WHY, and
    /// names what to send instead — and the forms that DO work on these tools still work, which is
    /// the half that makes the refusal honest rather than a wall.
    #[test]
    fn an_anchorless_spec_tool_refuses_a_project_relative_reference_by_name() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let (spec_id, key, number) = db.with_conn(|c| {
            let spec = db::create_spec(c, &project_id, "Referenced", db::SpecSections::empty())
                .expect("spec")
                .0;
            let project = db::get_project(c, &project_id).unwrap().unwrap();
            (spec.id, project.key, spec.number)
        });

        for token in [format!("#{number}"), number.to_string()] {
            for (tool, args) in [
                ("spec_fork", json!({ "spec_id": token })),
                ("spec_deps", json!({ "spec_id": token })),
                (
                    "spec_link",
                    json!({ "from_spec_id": token, "to_spec_id": spec_id }),
                ),
            ] {
                let err = match tool {
                    "spec_fork" => spec_fork(&db, &args),
                    "spec_deps" => spec_deps(&db, &args),
                    _ => spec_link(&db, &args),
                }
                .expect_err("a project-relative reference cannot resolve here");
                assert_eq!(
                    err.code,
                    RpcCode::InvalidArgument,
                    "{tool}: the Spec is not unknown, the FORM is unusable here — {}",
                    err.message
                );
                assert!(
                    err.message.contains(&token)
                        && err.message.contains(crate::spec_ref::ANCHORLESS_FORMS),
                    "{tool}: the refusal names what was sent AND what to send instead — {}",
                    err.message
                );
                assert!(
                    !err.message.starts_with("unknown spec"),
                    "{tool}: the opaque sentence is exactly what this replaces — {}",
                    err.message
                );
            }
        }
        // …and the self-anchored forms the description DOES advertise still resolve on the very
        // same tool: the refusal narrows nothing that ever worked.
        spec_deps(&db, &json!({ "spec_id": format!("{key}-{number}") }))
            .expect("the human key carries its own project");
        spec_deps(&db, &json!({ "spec_id": spec_id })).expect("the canonical id is untouched");
    }

    /// THE CONDITIONAL THIRTEENTH FIELD. `spec_update_section`'s only project anchor is its
    /// OPTIONAL `run_id`, so `#N` resolves with one and cannot without — and its description now
    /// says exactly that (`mcp::tests::spec_update_section_states_the_condition_its_short_forms_
    /// depend_on`). Both branches are pinned here, because a form that works or not depending on an
    /// unrelated argument is the kind of thing a test must fix in place (finding
    /// `01M0ZGHD137MDXFGS2PBG6GS68`): silence on the nominal path was what made it undiagnosable.
    #[test]
    fn spec_update_section_honours_a_short_reference_only_with_the_run_id_it_advertises() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let (spec_id, number) = db.with_conn(|c| {
            let spec = db::create_spec(c, &project_id, "Edited", db::SpecSections::empty())
                .expect("spec")
                .0;
            (spec.id, spec.number)
        });
        let edit = |token: &str, run_id: Option<&str>| {
            let mut args = json!({
                "spec_id": token,
                "section": "problem",
                "value": "rewritten",
            });
            if let Some(run_id) = run_id {
                args["run_id"] = json!(run_id);
            }
            spec_update_section(&db, &args)
        };

        // NOMINAL PATH — editing a draft, no run_id: there is no project, and the refusal says so.
        let err = edit(&format!("#{number}"), None).expect_err("no project to count #N in");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains(crate::spec_ref::ANCHORLESS_FORMS),
            "the refusal names the forms that work without a run_id: {}",
            err.message
        );

        // WITH THE RUN THE DESCRIPTION NAMES, the same reference resolves — the run carries the
        // Spec, the Spec carries the project, and the number finally has something to count in.
        let run_id = db.with_conn(|c| {
            let wf = db::create_workflow(
                c,
                Some(&project_id),
                "nspec",
                db::WORKFLOW_SCOPE_NSPEC,
                None,
            )
            .expect("workflow");
            db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}")
                .expect("run")
                .id
        });
        let out = edit(&format!("#{number}"), Some(&run_id)).expect("a run_id anchors the project");
        assert_eq!(
            out.result["spec_id"],
            json!(spec_id),
            "the short reference must have resolved to the Spec it names"
        );
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

    /// A Spec is authored PROJECT-scoped and UNANCHORED (review finding #25): `spec_create`
    /// does NOT anchor on a workspace — a `workspace_id` arg is IGNORED (never rejected, never
    /// anchored). The dev anchor is posed later, at the `start` transition. The created Spec's
    /// workspace_id is null.
    #[test]
    fn spec_create_is_project_scoped_and_unanchored() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();
        // Even when a workspace_id is passed, the Spec is created unanchored.
        let out = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Authored", "workspace_id": ws }),
        )
        .unwrap();
        assert!(
            out.result["spec"]["workspace_id"].is_null(),
            "an authored Spec is unanchored (workspace_id ignored at create)",
        );
        assert_eq!(out.result["spec"]["status"], "draft");
    }

    /// The STATE GUARD through the tool, AMENDED (v2.5): `spec_set_workspace` is REFUSED
    /// (invalid_state) on a DRAFT Spec — authoring never poses the anchor — but a READY Spec
    /// is now ANCHORABLE, because the dev workspace usually exists before the work starts.
    /// The link NEVER transitions the Spec: `ready` stays `ready`.
    ///
    /// RENAMED from `spec_set_workspace_refused_before_in_progress` by the phase-1 review: the
    /// old name asserted the contract this very amendment removed.
    #[test]
    fn spec_set_workspace_allows_ready_and_refuses_draft() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);

        // draft → refused.
        let err =
            spec_set_workspace(&db, &json!({ "spec_id": sid, "workspace_id": ws })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(
            err.message.contains("ready or in_progress"),
            "the refusal names the anchorable statuses, got: {}",
            err.message
        );

        // ready → NOW ALLOWED, and the status does not move.
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();
        let ok = spec_set_workspace(&db, &json!({ "spec_id": sid, "workspace_id": ws })).unwrap();
        assert_eq!(ok.result["spec"]["workspace_id"], ws);
        assert_eq!(
            ok.result["spec"]["status"], "ready",
            "anchoring a ready Spec does not transition it"
        );

        // in_progress (via start, which itself anchors) → the standalone re-anchor is allowed.
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": ws }),
        )
        .unwrap();
        let ok = spec_set_workspace(&db, &json!({ "spec_id": sid, "workspace_id": ws })).unwrap();
        assert_eq!(ok.result["spec"]["workspace_id"], ws);
    }

    /// THE TWO BRANCHES OF `spec_set_workspace`'s CONDITIONAL `#N`, pinned together because it is
    /// their DIFFERENCE that the description now promises (finding `01M0ZKWXS353TABS0WCT3XXB81`).
    ///
    /// This call's only project anchor is `workspace_id`, and it is optional BECAUSE omitting it IS
    /// the detach — so on the detach path a number can never be counted, and it used to travel down
    /// to `db::set_spec_workspace` and come back as a bare `unknown spec` that did not even quote
    /// the token that was sent. Now the refusal is raised at the entry point, says WHY, and names
    /// what to send instead; and the ANCHORING path, which does carry a project, resolves the same
    /// reference — which is the half that makes the refusal honest rather than a wall.
    #[test]
    fn spec_set_workspace_honours_a_short_reference_only_with_the_workspace_id_it_advertises() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        let number = db
            .with_conn(|c| db::get_spec(c, &sid))
            .unwrap()
            .expect("spec")
            .number;
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();

        // THE DETACH PATH — no workspace_id, so no project, so nothing to count `#N` in. The
        // refusal names the form AND the way out, instead of the opaque `unknown spec`.
        let err = spec_set_workspace(&db, &json!({ "spec_id": format!("#{number}") }))
            .expect_err("a detach anchors no project, so #N cannot resolve");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains(crate::spec_ref::ANCHORLESS_FORMS),
            "the refusal names the forms that work without a workspace_id: {}",
            err.message
        );
        assert!(
            err.message.contains(&format!("#{number}")),
            "and it quotes the token that was actually sent: {}",
            err.message
        );

        // THE ANCHORING PATH — the workspace names the project, and the same reference resolves.
        let ok = spec_set_workspace(
            &db,
            &json!({ "spec_id": format!("#{number}"), "workspace_id": ws }),
        )
        .expect("a workspace_id anchors the project");
        assert_eq!(
            ok.result["spec"]["id"],
            json!(sid),
            "the short reference must have resolved to the Spec it names"
        );
        assert_eq!(ok.result["spec"]["workspace_id"], ws);
    }

    /// REVIEW F2 — THE EMPTY STRING IS NOT A WAY TO SAY "NONE", and here saying "none" is an ACT.
    ///
    /// `optional_str` maps `""` to `None`, which is right for the 122 arguments where `None` means
    /// "the caller said nothing". On THIS argument `None` means DETACH, so the normalization turned
    /// an uninitialized caller variable into a silent un-anchoring: a caller who believed it was
    /// anchoring detached instead, and the tool answered `ok` (observed in the DB: a Spec left with
    /// `workspace_id` NULL). `""` is now REFUSED — while `null` and ABSENT keep their meaning down
    /// to the letter, which is the other half of the contract and is asserted here too, because a
    /// guard that also broke the detach would be a worse bug than the one it fixes.
    #[test]
    fn an_empty_workspace_id_is_refused_while_null_and_absent_still_detach() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();
        let anchor = || {
            spec_set_workspace(&db, &json!({ "spec_id": sid, "workspace_id": ws }))
                .expect("anchoring a ready Spec is allowed");
        };
        let stored_anchor = || {
            db.with_conn(|c| db::get_spec(c, &sid))
                .unwrap()
                .unwrap()
                .workspace_id
        };

        // THE BUG: an EMPTY workspace_id on an ANCHORED Spec is refused, and changes NOTHING.
        anchor();
        let err = spec_set_workspace(&db, &json!({ "spec_id": sid, "workspace_id": "" }))
            .expect_err("an empty string is a caller bug, not a detach order");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("workspace_id"),
            "the refusal NAMES the offending argument, got: {}",
            err.message
        );
        assert!(
            err.message.contains("EMPTY string") && err.message.contains("not a way to say 'none'"),
            "…and tells the empty string apart from the absence, got: {}",
            err.message
        );
        assert_eq!(
            stored_anchor().as_deref(),
            Some(ws.as_str()),
            "the refused call left the anchor exactly where it was"
        );

        // THE UNCHANGED HALF #1 — an explicit null still DETACHES.
        let detached =
            spec_set_workspace(&db, &json!({ "spec_id": sid, "workspace_id": Value::Null }))
                .expect("null is the documented detach");
        assert!(detached.result["spec"]["workspace_id"].is_null());
        assert!(stored_anchor().is_none(), "null detached, as it always did");

        // THE UNCHANGED HALF #2 — an ABSENT workspace_id detaches too.
        anchor();
        assert_eq!(stored_anchor().as_deref(), Some(ws.as_str()), "re-anchored");
        let detached = spec_set_workspace(&db, &json!({ "spec_id": sid }))
            .expect("omitting the argument is the other documented detach");
        assert!(detached.result["spec"]["workspace_id"].is_null());
        assert!(
            stored_anchor().is_none(),
            "absent detached, as it always did"
        );
    }

    /// FINDING `01M080S6Y5CTWK53FYY9YJF1GY` — THE FOURTH ANCHORING SURFACE, closed 2026-08-19
    /// (HORS PERIMETRE ASSUME of PRD 01KYA475AX5JBGSS8WK5J8NM1S, on the user's decision).
    ///
    /// F2 hardened three tools and left this one on the record as an observation: on
    /// `spec_transition(start)`, `workspace_id` still went through `optional_str`, so `""` read as
    /// "the caller said nothing", fell through to `cwd`, and then to nothing at all — and the Spec
    /// went `ready → in_progress` UNANCHORED while the tool answered `ok`. One notch worse than the
    /// F2 cases, because this one COMMITS the Spec on its way past: an engaged build with no dev
    /// workspace, and no sentence anywhere saying so.
    ///
    /// The three clauses of the finding's own verification are asserted here, and the last two are
    /// what make the first one a guard rather than a mere refusal: ABSENCE must still start the Spec
    /// unanchored, and a `cwd` that resolves a workspace must still anchor it. Only the empty STRING
    /// changed meaning.
    #[test]
    fn an_empty_workspace_id_is_refused_on_start_while_absence_and_cwd_keep_their_meaning() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, workspace_id, cwd) = db.with_conn(|c| {
            let (project, root) = db::create_project(c, "Proj", "/tmp/nyx-fourth", None).unwrap();
            (project.id, root.id, root.root_path)
        });
        let ready_spec = || {
            let created = spec_create(
                &db,
                &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
            )
            .unwrap();
            let sid = spec_id_of(&created);
            spec_transition(
                &db,
                &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
            )
            .unwrap();
            sid
        };
        let stored = |sid: &str| {
            let spec = db.with_conn(|c| db::get_spec(c, sid)).unwrap().unwrap();
            (spec.status, spec.workspace_id)
        };

        // (1) THE BUG — `""` is refused, and the Spec does not move.
        let sid = ready_spec();
        let err = spec_transition(
            &db,
            &json!({
                "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": ""
            }),
        )
        .expect_err("an empty anchor is a caller bug, not an order to start unanchored");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("workspace_id")
                && err.message.contains("EMPTY string")
                && err.message.contains("not a way to say 'none'"),
            "the refusal names the argument and tells `\"\"` apart from the absence, got: {}",
            err.message
        );
        assert_eq!(
            stored(&sid),
            (db::SPEC_STATUS_READY.to_string(), None),
            "the refused call left the Spec ready and unanchored — it did not half-commit it"
        );

        // (2) THE UNCHANGED HALF #1 — the SAME Spec, with the argument OMITTED, still starts
        //     unanchored. The guard refuses a spelling, not the behaviour behind it.
        let started = spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "start", "user_confirmed": true }),
        )
        .expect("omitting workspace_id is the documented unanchored start");
        assert_eq!(started.result["spec"]["status"], "in_progress");
        assert!(started.result["spec"]["workspace_id"].is_null());
        assert_eq!(
            stored(&sid),
            (db::SPEC_STATUS_IN_PROGRESS.to_string(), None),
            "absent still starts the Spec with no anchor, as it always did"
        );

        // (3) THE UNCHANGED HALF #2 — a `cwd` inside a workspace still anchors it.
        let other = ready_spec();
        let nested = format!("{cwd}/crates/api");
        let anchored = spec_transition(
            &db,
            &json!({
                "spec_id": other, "transition": "start", "user_confirmed": true, "cwd": nested
            }),
        )
        .expect("the cwd road is untouched");
        assert_eq!(anchored.result["spec"]["workspace_id"], workspace_id);
        assert_eq!(
            stored(&other),
            (
                db::SPEC_STATUS_IN_PROGRESS.to_string(),
                Some(workspace_id.clone())
            ),
            "a cwd that resolves a workspace still anchors the Spec it starts"
        );
    }

    /// The PROJECT GUARD through the tool (v2.5): anchoring on a workspace of ANOTHER project
    /// is `invalid_argument` and writes nothing.
    #[test]
    fn spec_set_workspace_refuses_a_workspace_of_another_project() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let (_other_project, other_root) = db
            .with_conn(|c| db::create_project(c, "Other", "/tmp/other-proj", None))
            .unwrap();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();

        let err = spec_set_workspace(
            &db,
            &json!({ "spec_id": sid, "workspace_id": other_root.id }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("its own project"),
            "the refusal explains the project mismatch, got: {}",
            err.message
        );
    }

    /// `spec_transition` start is SENSITIVE and POSES the anchor in one op (review finding #25):
    /// without user_confirmed it is refused; with confirmation + a workspace_id it moves the Spec
    /// to in_progress AND anchors it. The anchor also resolves from `cwd` when workspace_id is
    /// absent (the same cwd→workspace matcher the context render uses).
    #[test]
    fn spec_transition_start_requires_confirmation_and_poses_the_anchor() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();

        // Unconfirmed start → refused (sensitive), nothing anchored.
        let err = spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "start", "workspace_id": ws }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(err.message.contains("sensitive"));

        // Confirmed start with an explicit workspace_id → in_progress + anchored.
        let started = spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": ws }),
        )
        .unwrap();
        assert_eq!(started.result["spec"]["status"], "in_progress");
        assert_eq!(
            started.result["spec"]["workspace_id"], ws,
            "start posed the anchor in the same operation",
        );
    }

    /// The `start` anchor resolves from `cwd` when no workspace_id is given: the Spec anchors on
    /// the workspace whose root path matches the cwd (review finding #25). agent_context(role=dev)
    /// then resolves the Spec from that workspace — no regression of the project-scoped author.
    #[test]
    fn spec_transition_start_resolves_the_anchor_from_cwd_and_dev_context_follows() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, workspace_id, cwd) = db.with_conn(|c| {
            let (project, root) = db::create_project(c, "Proj", "/tmp/nyx-start", None).unwrap();
            (project.id, root.id, root.root_path)
        });
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();
        // No workspace_id — resolve it from the cwd (nested under the workspace root).
        let nested = format!("{cwd}/crates/api");
        let started = spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "cwd": nested }),
        )
        .unwrap();
        assert_eq!(
            started.result["spec"]["workspace_id"], workspace_id,
            "start resolved the anchor from the cwd → workspace",
        );

        // agent_context(role=dev) resolves the Spec from the workspace anchored at the transition.
        let dev = crate::mcp_context::agent_context(
            &db,
            &json!({ "role": "dev", "workspace_id": workspace_id }),
        )
        .unwrap();
        assert_eq!(
            dev.result["spec_id"], sid,
            "dev context resolves the anchored Spec"
        );
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

        // Create PROJECT-scoped (unanchored) — the anchor is posed at the start transition.
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Feature" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        assert!(
            created.result["spec"]["workspace_id"].is_null(),
            "authored unanchored",
        );

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

        // ready (confirmed) → start (confirmed, anchoring on the dev workspace in one op).
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();
        let started = spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": ws }),
        )
        .unwrap();
        assert_eq!(started.result["spec"]["status"], "in_progress");
        assert_eq!(
            started.result["spec"]["workspace_id"], ws,
            "start posed the workspace anchor",
        );

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

    /// v2.5.8 finding #31 THROUGH the tool: `task_delete` REALLY removes a task (+ its
    /// dependency edges) in a DRAFT Spec, is REFUSED (`invalid_state`) once the Spec is
    /// in_progress (skip is the legit move there), and an unknown task is `invalid_id`.
    #[test]
    fn task_delete_is_draft_only_through_the_tool() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);

        // Two tasks + an edge, authored while the Spec is a draft.
        let a = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "A", "spec_id": sid }),
        )
        .unwrap();
        let b = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "B", "spec_id": sid }),
        )
        .unwrap();
        let aid = a.result["task"]["id"].as_str().unwrap().to_string();
        let bid = b.result["task"]["id"].as_str().unwrap().to_string();
        task_add_dependency(&db, &json!({ "task_id": aid, "depends_on_task_id": bid })).unwrap();

        // Unknown task → invalid_id.
        let err = task_delete(&db, &json!({ "task_id": "nope" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);

        // DRAFT: delete A really removes it (+ its edge) and reports the change on Specs.
        let out = task_delete(&db, &json!({ "task_id": aid })).unwrap();
        assert_eq!(out.effects, vec![ChangedTopic::Specs]);
        assert_eq!(out.result["deleted"], true);
        assert!(
            db.with_conn(|c| db::get_task(c, &aid)).unwrap().is_none(),
            "the task row is gone"
        );
        assert!(
            db.with_conn(|c| db::list_task_dependency_edges(c, &aid))
                .unwrap()
                .is_empty(),
            "the deleted task's dependency edge is gone"
        );

        // Move the Spec to in_progress (ready → start) → a delete is now REFUSED.
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "ready", "user_confirmed": true }),
        )
        .unwrap();
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "transition": "start", "user_confirmed": true, "workspace_id": ws }),
        )
        .unwrap();
        let err = task_delete(&db, &json!({ "task_id": bid })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(
            db.with_conn(|c| db::get_task(c, &bid)).unwrap().is_some(),
            "a refused delete removed nothing"
        );
        // …and the wall NAMES both ways out, because the caller hitting it may be trying either of
        // two different things. Dropping the work is `skip`; a finding whose TEXT came out malformed
        // is a rewrite, and the caller that lands here with a mangled description had nowhere to go.
        assert!(
            err.message.contains("transition=skip"),
            "the refusal names how to DROP the work: {}",
            err.message
        );
        assert!(
            err.message.contains("review_task_structure"),
            "…and how to FIX a finding's wording, which the freeze was never about: {}",
            err.message
        );
    }

    /// DONE-CRITERION (v2.6 spec deps): `spec_link` refuses a cycle, `related` is context-only
    /// (never cycles), `spec_deps` returns correct "bloquée par / débloque" badges, and
    /// `spec_unlink` is idempotent — all THROUGH the real dispatch.
    #[test]
    fn spec_link_cycle_refused_badges_and_unlink_through_dispatch() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let mk = |title: &str| -> String {
            spec_id_of(
                &spec_create(&db, &json!({ "project_id": project_id, "title": title })).unwrap(),
            )
        };
        let a = mk("A");
        let b = mk("B");
        let c = mk("C");

        // A depends on B (blocks), B depends on C (after) — through the real dispatch.
        let linked = dispatch_spec_tool(
            &db,
            SPEC_LINK_TOOL,
            &json!({ "from_spec_id": a, "to_spec_id": b, "kind": "blocks", "note": "no merge" }),
        )
        .expect("spec_link routed")
        .expect("spec_link ok");
        assert_eq!(linked.effects, vec![ChangedTopic::Specs]);
        assert_eq!(linked.result["dependency"]["kind"], "blocks");
        spec_link(
            &db,
            &json!({ "from_spec_id": b, "to_spec_id": c, "kind": "after" }),
        )
        .unwrap();

        // C → A would close the cycle A→B→C→A: REFUSED (invalid_state).
        assert_eq!(
            spec_link(&db, &json!({ "from_spec_id": c, "to_spec_id": a }))
                .unwrap_err()
                .code,
            RpcCode::InvalidState
        );
        // Self-link → invalid_argument; unknown Spec → invalid_id; bad kind → invalid_argument.
        assert_eq!(
            spec_link(&db, &json!({ "from_spec_id": a, "to_spec_id": a }))
                .unwrap_err()
                .code,
            RpcCode::InvalidArgument
        );
        assert_eq!(
            spec_link(&db, &json!({ "from_spec_id": a, "to_spec_id": "nope" }))
                .unwrap_err()
                .code,
            RpcCode::InvalidId
        );
        assert_eq!(
            spec_link(
                &db,
                &json!({ "from_spec_id": a, "to_spec_id": b, "kind": "bogus" })
            )
            .unwrap_err()
            .code,
            RpcCode::InvalidArgument
        );

        // A `related` C→A link is context-only — it does NOT cycle even though A→B→C exists.
        spec_link(
            &db,
            &json!({ "from_spec_id": c, "to_spec_id": a, "kind": "related", "note": "ctx" }),
        )
        .unwrap();

        // Badges: B is blocked-by C (1) and unblocks A (1).
        let deps_b = dispatch_spec_tool(&db, SPEC_DEPS_TOOL, &json!({ "spec_id": b }))
            .expect("spec_deps routed")
            .expect("spec_deps ok");
        assert!(deps_b.effects.is_empty(), "spec_deps is a read");
        assert_eq!(deps_b.result["blocked_by_count"], 1);
        assert_eq!(deps_b.result["blocked_by"][0]["spec_id"], c);
        assert_eq!(deps_b.result["unblocks_count"], 1);
        assert_eq!(deps_b.result["unblocks"][0]["spec_id"], a);

        // A: blocked-by B (1), unblocks 0 (the C→A link is related, not counted), 1 related link.
        let deps_a = spec_deps(&db, &json!({ "spec_id": a })).unwrap();
        assert_eq!(deps_a.result["blocked_by_count"], 1);
        assert_eq!(deps_a.result["blocked_by"][0]["note"], "no merge");
        assert_eq!(deps_a.result["unblocks_count"], 0);
        assert_eq!(deps_a.result["related"].as_array().unwrap().len(), 1);

        // Unlink A→B is idempotent and clears the badge.
        let un = spec_unlink(&db, &json!({ "from_spec_id": a, "to_spec_id": b })).unwrap();
        assert_eq!(un.result["unlinked"], true);
        let un2 = spec_unlink(&db, &json!({ "from_spec_id": a, "to_spec_id": b })).unwrap();
        assert_eq!(un2.result["unlinked"], false, "already unlinked → no-op");
        assert_eq!(
            spec_deps(&db, &json!({ "spec_id": a })).unwrap().result["blocked_by_count"],
            0
        );
    }

    #[test]
    fn dispatch_spec_tool_returns_none_for_a_foreign_tool() {
        let _g = db_guard();
        let (db, _p, _w) = seeded_db();
        assert!(dispatch_spec_tool(&db, "list_projects", &json!({})).is_none());
    }

    /// The REDESIGN (finding 01KXG68DN5DSCFDFA6C87NJHGV): `task_add` groups by phase NAME and
    /// nyx MATERIALIZES the plan from the names, THROUGH the real dispatch. Adding tasks with
    /// phase names auto-builds `specs.phases` (1-based, first-seen order, get-or-create), each
    /// task's `phase` is the resolved number, a repeated name reuses its number, and no task
    /// is orphaned. There is NO separate set-phases call.
    #[test]
    fn task_add_by_name_materializes_the_plan_through_dispatch() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let sid = spec_id_of(&created);

        // A fresh Spec has no plan.
        let before = db.with_conn(|c| db::get_spec(c, &sid)).unwrap().unwrap();
        assert_eq!(before.phases, "[]", "no phase plan before any task");

        // Add tasks by NAME through the exact MCP routing.
        let add = |title: &str, phase: &str| {
            dispatch_spec_tool(
                &db,
                TASK_ADD_TOOL,
                &json!({ "project_id": project_id, "title": title, "spec_id": sid, "phase": phase }),
            )
            .expect("task_add is routed")
            .expect("task_add ok")
        };
        let t1 = add("wire it", "Foundation");
        assert_eq!(t1.effects, vec![ChangedTopic::Specs]);
        assert_eq!(t1.result["task"]["phase"], 1, "first name seen → number 1");
        let t2 = add("ship it", "Delivery");
        assert_eq!(
            t2.result["task"]["phase"], 2,
            "second distinct name → number 2"
        );
        let t3 = add("more foundation", "Foundation");
        assert_eq!(
            t3.result["task"]["phase"], 1,
            "a repeated name reuses its number"
        );

        // The plan self-built from the names (1-based, first-seen), coherent — no orphan.
        let after = db.with_conn(|c| db::get_spec(c, &sid)).unwrap().unwrap();
        assert_eq!(
            after.phases, r#"[{"number":1,"name":"Foundation"},{"number":2,"name":"Delivery"}]"#,
            "specs.phases materializes from the task phase names",
        );
        assert!(
            db::validate_phase_plan(&after.phases).is_empty(),
            "the auto-built plan is a coherent 1..N",
        );
    }

    /// K2 — THE LIVE MISTAKE, REPRODUCED. Spec #12 was written with two phases ("Module de purge",
    /// "Endpoint admin"). Mid-build, an auditor called `task_add` passing the phase's rendered LABEL
    /// — "2 · Endpoint admin", the heading its manual prints — instead of the NAME. nyx materialized
    /// it: a THIRD phase appeared in the plan of an `in_progress` Spec. The agent noticed and
    /// cancelled its finding; the phantom phase stayed, and everything computed on phases counted it.
    ///
    /// A Spec's sections freeze outside `draft`, and tasks are refused on a `completed` one — yet its
    /// PHASE PLAN could be extended by one misspelt free-text argument. So the unknown name is
    /// REFUSED, nothing is written, and the refusal names the phases that do exist, since the mistake
    /// is always a near-miss and the caller needs the exact string that works.
    #[test]
    fn a_phase_label_typed_on_a_spec_under_build_is_refused_and_the_plan_is_untouched() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({
                "project_id": project_id,
                "title": "S",
                "problem": "the purge is manual",
                "solution": "a purge module and an admin endpoint",
            }),
        )
        .unwrap();
        let sid = spec_id_of(&created);

        // THE PLAN, written while the Spec is drafted — the one place a name may mint a phase.
        for (title, phase) in [
            ("purge", "Module de purge"),
            ("admin endpoint", "Endpoint admin"),
        ] {
            task_add(
                &db,
                &json!({ "project_id": project_id, "title": title, "spec_id": sid, "phase": phase }),
            )
            .expect("the plan is authored in draft");
        }
        let plan = r#"[{"number":1,"name":"Module de purge"},{"number":2,"name":"Endpoint admin"}]"#;
        assert_eq!(
            db.with_conn(|c| db::get_spec(c, &sid)).unwrap().unwrap().phases,
            plan
        );

        // THE BUILD STARTS — from here the plan is the author's, and nobody else's.
        db.with_conn(|c| {
            db::transition_spec(c, &sid, db::SpecTransition::Ready, true).expect("ready");
            db::transition_spec(c, &sid, db::SpecTransition::Start, true).expect("start");
        });
        let tasks_before = db.with_conn(|c| db::list_tasks_for_spec(c, &sid)).unwrap().len();

        // THE MISTAKE: the auditor files its finding against the phase's LABEL.
        let err = dispatch_spec_tool(
            &db,
            TASK_ADD_TOOL,
            &json!({
                "project_id": project_id,
                "title": "Auditor: the purge drops rows it should keep",
                "spec_id": sid,
                "kind": "review",
                "severity": "high",
                "phase": "2 · Endpoint admin",
            }),
        )
        .expect("task_add is routed")
        .expect_err("a phase the plan does not hold must be refused");
        assert_eq!(err.code, "invalid_argument");
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some("unknown_phase"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("'Module de purge'") && err.message.contains("'Endpoint admin'"),
            "the refusal names the phases that DO exist, so the caller can retry: {}",
            err.message
        );

        // AND NOTHING HAPPENED: no phantom phase, no half-written finding.
        assert_eq!(
            db.with_conn(|c| db::get_spec(c, &sid)).unwrap().unwrap().phases,
            plan,
            "the plan of a Spec under build is what its author wrote"
        );
        assert_eq!(
            db.with_conn(|c| db::list_tasks_for_spec(c, &sid)).unwrap().len(),
            tasks_before,
            "the refused call wrote no task either"
        );

        // The two ways through are both open: name a REAL phase, or name none and let nyx anchor it.
        let named = task_add(
            &db,
            &json!({
                "project_id": project_id,
                "title": "Auditor: same finding, right name",
                "spec_id": sid,
                "kind": "review",
                "severity": "high",
                "phase": "Endpoint admin",
            }),
        )
        .expect("an EXISTING phase is not a mutation");
        assert_eq!(named.result["task"]["phase"], json!(2));
        task_add(
            &db,
            &json!({
                "project_id": project_id,
                "title": "Auditor: no phase named",
                "spec_id": sid,
                "kind": "review",
                "severity": "high",
            }),
        )
        .expect("naming no phase is the auditor's normal case");
        assert_eq!(
            db.with_conn(|c| db::get_spec(c, &sid)).unwrap().unwrap().phases,
            plan,
            "neither route touched the plan"
        );
    }

    /// THE AUDITOR'S FINDING CARRIES ITS ROUND, and nyx is the one that puts it there.
    ///
    /// Measured live: every finding of a Spec carried `round = NULL` — one task in a whole campaign
    /// had one — so which audit round produced what could not be reconstructed from the database,
    /// which is precisely the data needed to arbitrate what the audit cost. `task_add` has always
    /// ACCEPTED a `round`; the auditor never passed one, because a sub-agent cannot know which round
    /// it is in. So the engine fills it: the finding lands on the phase under review, in that phase's
    /// current pass — and the N findings of ONE pass share it, while a pass raised after the previous
    /// one is settled opens the next.
    #[test]
    fn task_add_fills_a_findings_phase_and_round_from_nyx() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let sid = spec_id_of(&created);

        // A plan with one phase, DELIVERED — so phase 1 is the phase under review.
        let slice = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "ship it", "spec_id": sid, "phase": "Delivery" }),
        )
        .unwrap();
        let slice_id = slice.result["task"]["id"].as_str().unwrap().to_string();
        assert_eq!(
            slice.result["task"]["round"],
            json!(null),
            "a PLAN slice is not a finding — it stays round-less (round 0)"
        );
        db.with_conn(|c| {
            db::transition_task(c, &slice_id, db::TaskTransition::Start, None).unwrap();
            db::transition_task(c, &slice_id, db::TaskTransition::Done, None).unwrap();
        });

        // THE AUDITOR RAISES A FINDING — exactly as it does, with no `round` and no `phase`.
        let finding = |title: &str| {
            dispatch_spec_tool(
                &db,
                TASK_ADD_TOOL,
                &json!({
                    "project_id": project_id,
                    "title": title,
                    "spec_id": sid,
                    "kind": "review",
                    "severity": db::TASK_SEVERITY_HIGH,
                }),
            )
            .expect("task_add is routed")
            .expect("task_add ok")
            .result
        };
        let first = finding("Auditor: missing timeout guard");
        assert_eq!(
            first["task"]["phase"],
            json!(1),
            "the finding hangs on the phase under review (1), not on nothing"
        );
        assert_eq!(
            first["task"]["round"],
            json!(1),
            "…in that phase's first pass — \"1.1\" — filled by nyx, not by the auditor"
        );

        // A SECOND finding of the SAME pass shares the round: one pass = one round holding N
        // findings, which is what makes "what did round 1.1 produce" answerable.
        let second = finding("Auditor: unbounded retry");
        assert_eq!(second["task"]["round"], json!(1), "same open pass, same round");

        // The pass is settled; the NEXT audit opens the next round.
        for id in [
            first["task"]["id"].as_str().unwrap(),
            second["task"]["id"].as_str().unwrap(),
        ] {
            db.with_conn(|c| {
                db::transition_task(c, id, db::TaskTransition::Start, None).unwrap();
                db::transition_task(c, id, db::TaskTransition::Done, None).unwrap();
            });
        }
        assert_eq!(
            finding("Auditor: second pass")["task"]["round"],
            json!(2),
            "a pass raised after the previous one closed is a NEW round — \"1.2\""
        );

        // An explicit round still wins: the caller attributing a finding to another pass is obeyed.
        let explicit = dispatch_spec_tool(
            &db,
            TASK_ADD_TOOL,
            &json!({
                "project_id": project_id, "title": "late", "spec_id": sid,
                "kind": "review", "severity": db::TASK_SEVERITY_LOW, "round": 1,
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(explicit.result["task"]["round"], json!(1));
    }

    /// …and a Spec with NO delivered phase has no pass to number a finding in. It is filed all the
    /// same — raising a finding outside any review pass is legitimate — and stays un-phased and
    /// round-less rather than being given an invented round.
    #[test]
    fn a_finding_on_a_spec_with_nothing_delivered_stays_round_less() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let sid = spec_id_of(&created);

        let finding = task_add(
            &db,
            &json!({
                "project_id": project_id, "title": "raised early", "spec_id": sid,
                "kind": "review", "severity": db::TASK_SEVERITY_MEDIUM,
            }),
        )
        .unwrap();
        assert_eq!(finding.result["task"]["phase"], json!(null));
        assert_eq!(
            finding.result["task"]["round"],
            json!(null),
            "no delivered phase = no pass to belong to; nyx does not invent one"
        );
    }

    /// A phase name without a `spec_id` is refused (`invalid_argument`) — the plan lives on
    /// the Spec, so a name has nowhere to materialize without one.
    #[test]
    fn task_add_phase_without_spec_is_invalid_argument() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let err = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "orphan", "phase": "Foundation" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("spec_id"), "the refusal names spec_id");
    }

    // --- PDR-0017: the CATEGORY surface ------------------------------------

    /// The category of `spec_id`, read back from the DB — the assertion every test below makes,
    /// since the point is always what was PERSISTED, never what the response claimed.
    fn category_of(db: &Db, spec_id: &str) -> Option<db::Category> {
        db.with_conn(|c| {
            let spec = db::get_spec(c, spec_id).expect("read").expect("spec");
            match spec.category_id.as_deref() {
                Some(id) => db::get_category(c, id).expect("read"),
                None => None,
            }
        })
    }

    /// DONE-CRITERION: `spec_create(category)` accepts a slug or a name, resolves (or creates) the
    /// category, and lands it on the Spec — and the response says which one, with its provenance.
    #[test]
    fn spec_create_classifies_the_draft_it_authors() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();

        // A SEEDED slug — the nominal case: `bug` is the project's own, minted by the seed.
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "A bug", "category": "bug" }),
        )
        .expect("create");
        let sid = spec_id_of(&created);
        assert_eq!(created.result["category"]["slug"], json!("bug"));
        assert_eq!(created.result["category"]["name"], json!("Bug"));
        assert_eq!(
            created.result["category"]["created_by_type"],
            json!(db::CATEGORY_CREATED_BY_SYSTEM)
        );
        assert!(created.result["category"]["id"].is_string());
        assert_eq!(category_of(&db, &sid).expect("classified").slug, "bug");

        // A human NAME resolves too — case and inner whitespace fold.
        let by_name = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Cleanup", "category": "  rEfActor " }),
        )
        .expect("create");
        assert_eq!(
            category_of(&db, &spec_id_of(&by_name)).expect("classified").slug,
            "refactor"
        );

        // An UNKNOWN name is a CREATION, not a refusal (PDR-0017): the category is minted, and —
        // since the 2026-09-02 amendment — it is VALID as it stands. The payload says who minted
        // it and carries no curation `state` at all.
        let minted = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Debt", "category": "Dette technique" }),
        )
        .expect("create");
        assert_eq!(minted.result["category"]["slug"], json!("dette-technique"));
        assert!(
            minted.result["category"].get("state").is_none(),
            "the proposed/confirmed axis was retired: {}",
            minted.result["category"]
        );
        assert_eq!(
            minted.result["category"]["created_by_type"],
            json!(db::CATEGORY_CREATED_BY_AGENT)
        );

        // OMITTING it leaves the Spec unclassified — a legitimate state, and the response says so.
        let plain =
            spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).expect("create");
        assert_eq!(plain.result["category"], json!(null));
        assert!(category_of(&db, &spec_id_of(&plain)).is_none());

        // …AND `null` READS AS THE OMISSION IT IS.
        let explicit_none = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "N", "category": Value::Null }),
        )
        .expect("create");
        assert_eq!(explicit_none.result["category"], json!(null));
    }

    /// **ONLY THE ABSENCE OF `category` MEANS "UNCLASSIFIED"** — the same rule the sibling verb
    /// `spec_set_category` states, on the same argument name.
    ///
    /// Read with `optional_str`, an EMPTY string folded into `None` before the service ever saw it,
    /// so one uninitialised caller variable produced three incompatible outcomes on one argument:
    /// `""` created a silently UNCLASSIFIED Spec — precisely the shape that escapes the
    /// red-before-green discipline, and the population the coder preflight exists for — `" "`
    /// reached `resolve_or_create`'s blank-label guard and took the whole Spec down with it, and
    /// `""` on `spec_set_category` was an explicit refusal. The guard `create_spec_with_category`
    /// documents ("A BLANK label … takes the Spec down with it") was unreachable for the one
    /// spelling a caller is most likely to send.
    #[test]
    fn spec_create_refuses_an_empty_category_exactly_as_its_sibling_verb_does() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();

        for blank in ["", "   "] {
            let err = spec_create(
                &db,
                &json!({ "project_id": project_id, "title": "S", "category": blank }),
            )
            .expect_err("a blank category is not a way to say 'none'");
            assert_eq!(err.code, RpcCode::InvalidArgument, "category {blank:?}: {err:?}");
            assert!(
                db.with_conn(|c| db::list_specs(c, Some(&project_id)))
                    .expect("read")
                    .is_empty(),
                "category {blank:?}: and no Spec is left behind either way",
            );
        }

        // The sibling verb answers the empty string the same way, in the same words — which is the
        // whole point: one argument name, one behaviour.
        let created =
            spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).expect("create");
        let err = spec_set_category(
            &db,
            &json!({ "spec_id": spec_id_of(&created), "category": "" }),
        )
        .expect_err("the sibling verb refuses it too");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(err.message.contains("EMPTY string"), "{}", err.message);
    }

    /// DONE-CRITERION: a refused category takes the Spec with it — the classification rides the
    /// creation's own transaction, so `spec_create` never leaves a half-classified draft behind.
    /// (The other half — an error injected AFTER both writes rolling back the MINTED category too —
    /// is pinned at the service, `category::tests`.)
    #[test]
    fn spec_create_leaves_no_spec_behind_when_its_category_is_refused() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        // Two categories answering the same NAME: `bug`, and a `chore` renamed onto it.
        db.with_conn(|c| {
            crate::category::rename(
                c,
                &project_id,
                crate::category::CategoryRef::Label("chore"),
                "Bug",
            )
        })
        .expect("rename");

        let err = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Doomed", "category": "Bug" }),
        )
        .expect_err("an ambiguous name is refused");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains("bug") && err.message.contains("chore"),
            "the refusal LISTS the slugs to re-ask with: {}",
            err.message
        );
        assert!(
            db.with_conn(|c| db::list_specs(c, Some(&project_id)))
                .expect("read")
                .is_empty(),
            "no Spec was created for a classification that was refused"
        );
    }

    /// DONE-CRITERION: `spec_set_category` ASSIGNS by explicit id, ASSIGNS by unambiguous
    /// reference, MINTS on the fly for an unknown one, and REMOVES when neither is named.
    #[test]
    fn spec_set_category_assigns_mints_and_removes() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let sid = spec_id_of(
            &spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).expect("create"),
        );

        // BY REFERENCE (a slug).
        let assigned = dispatch_spec_tool(
            &db,
            SPEC_SET_CATEGORY_TOOL,
            &json!({ "spec_id": sid, "category": "bug" }),
        )
        .expect("routed")
        .expect("assign");
        assert_eq!(assigned.result["category"]["slug"], json!("bug"));
        assert_eq!(assigned.result["created"], json!(false));
        assert_eq!(category_of(&db, &sid).expect("classified").slug, "bug");

        // BY EXPLICIT ID — what a caller sends having just read the list.
        let feature = db
            .with_conn(|c| db::find_category_by_slug(c, &project_id, "feature"))
            .expect("read")
            .expect("seeded");
        let by_id = spec_set_category(
            &db,
            &json!({ "spec_id": sid, "category_id": feature.id }),
        )
        .expect("assign");
        assert_eq!(by_id.result["category"]["id"], json!(feature.id));
        assert_eq!(category_of(&db, &sid).expect("classified").slug, "feature");

        // AN UNKNOWN LABEL MINTS — `created_by_type=agent`, NO error, and NO curation state to
        // wait on (PDR-0017 amended 2026-09-02: the agent's mint is valid as it stands).
        let minted = spec_set_category(
            &db,
            &json!({ "spec_id": sid, "category": "Flaky Infra" }),
        )
        .expect("mint + assign");
        assert_eq!(minted.result["created"], json!(true));
        assert_eq!(minted.result["category"]["slug"], json!("flaky-infra"));
        assert!(
            minted.result["category"].get("state").is_none(),
            "no proposed/confirmed axis on the wire: {}",
            minted.result["category"]
        );
        let now = category_of(&db, &sid).expect("classified");
        assert_eq!(now.slug, "flaky-infra");
        assert_eq!(now.created_by_type, db::CATEGORY_CREATED_BY_AGENT);
        assert!(now.confirmed_at.is_some());

        // NAMING NEITHER IS THE REMOVAL — a first-class write, not a no-op.
        let removed = spec_set_category(&db, &json!({ "spec_id": sid })).expect("remove");
        assert_eq!(removed.result["category"], json!(null));
        assert!(category_of(&db, &sid).is_none());
        // …and the minted category SURVIVES the removal: it is the project's vocabulary now, not
        // the Spec's property.
        assert!(db
            .with_conn(|c| db::find_category_by_slug(c, &project_id, "flaky-infra"))
            .expect("read")
            .is_some());

        // The mutation announces the topic an open Specs surface re-pulls on.
        assert!(matches!(removed.effects.as_slice(), [ChangedTopic::Specs]));
    }

    /// DONE-CRITERION: THE CATEGORY IS NOT FROZEN BY THE LIFECYCLE. `spec_update_section` refuses a
    /// Spec that has left draft; a classification is metadata about the GENRE of work, and staying
    /// correctable is the whole point — a `/ndev` preflight discovers a mis-classified Spec exactly
    /// when it is already under build.
    #[test]
    fn a_spec_out_of_draft_can_still_be_reclassified() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let sid = spec_id_of(
            &spec_create(
                &db,
                &json!({
                    "project_id": project_id, "title": "S",
                    "problem": "the purge is manual", "solution": "a purge module",
                    "category": "feature",
                }),
            )
            .expect("create"),
        );
        db.with_conn(|c| {
            db::transition_spec(c, &sid, db::SpecTransition::Ready, true).expect("ready");
            db::transition_spec(c, &sid, db::SpecTransition::Start, true).expect("start");
        });

        // The CONTRAST, asserted rather than assumed: prose IS frozen here…
        let frozen = spec_update_section(
            &db,
            &json!({ "spec_id": sid, "section": "problem", "value": "rewritten" }),
        )
        .expect_err("a section of a Spec under build is frozen");
        assert_eq!(frozen.code, RpcCode::InvalidState);

        // …and the classification is NOT. It was a feature; it is a bug.
        spec_set_category(&db, &json!({ "spec_id": sid, "category": "bug" }))
            .expect("reclassifying an in_progress Spec is allowed");
        assert_eq!(category_of(&db, &sid).expect("classified").slug, "bug");
        // Removing it is allowed just the same.
        spec_set_category(&db, &json!({ "spec_id": sid })).expect("un-classifying too");
        assert!(category_of(&db, &sid).is_none());
    }

    /// DONE-CRITERION: unknown / ambiguous / cross-project references answer STABLE codes, and
    /// none of them writes anything.
    #[test]
    fn a_category_reference_that_cannot_be_honoured_answers_a_stable_code() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let beta = db
            .with_conn(|c| db::create_project(c, "Beta", "/tmp/beta", None))
            .expect("project")
            .0
            .id;
        let beta_bug = db
            .with_conn(|c| db::find_category_by_slug(c, &beta, "bug"))
            .expect("read")
            .expect("seeded");
        let sid = spec_id_of(
            &spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).expect("create"),
        );

        // CROSS-PROJECT: the row exists, it just is not this project's vocabulary. The refusal
        // names BOTH projects, and `specs.category_id` is untouched.
        let cross = spec_set_category(
            &db,
            &json!({ "spec_id": sid, "category_id": beta_bug.id }),
        )
        .expect_err("a category of another project is refused");
        assert_eq!(cross.code, RpcCode::InvalidArgument);
        assert!(
            cross.message.contains(&project_id) && cross.message.contains(&beta),
            "the refusal names both projects: {}",
            cross.message
        );
        assert!(category_of(&db, &sid).is_none(), "nothing was written");

        // AN UNKNOWN ID is a mistake — `invalid_id`. (An unknown LABEL is not: it MINTS.)
        assert_eq!(
            spec_set_category(&db, &json!({ "spec_id": sid, "category_id": "nope" }))
                .expect_err("unknown id")
                .code,
            RpcCode::InvalidId
        );

        // AN AMBIGUOUS NAME is refused rather than guessed, and LISTS the slugs to re-ask with.
        db.with_conn(|c| {
            crate::category::rename(
                c,
                &project_id,
                crate::category::CategoryRef::Label("chore"),
                "Bug",
            )
        })
        .expect("rename");
        let ambiguous = spec_set_category(&db, &json!({ "spec_id": sid, "category": "Bug" }))
            .expect_err("two categories answer that name");
        assert_eq!(ambiguous.code, RpcCode::InvalidArgument);
        assert!(
            ambiguous.message.contains("bug") && ambiguous.message.contains("chore"),
            "the refusal lists the slugs: {}",
            ambiguous.message
        );
        // …and the SLUG still resolves, unambiguously.
        spec_set_category(&db, &json!({ "spec_id": sid, "category": "bug" })).expect("by slug");
        assert_eq!(category_of(&db, &sid).expect("classified").slug, "bug");

        // BOTH references at once is refused: two references that can disagree.
        let both = spec_set_category(
            &db,
            &json!({ "spec_id": sid, "category": "feature", "category_id": beta_bug.id }),
        )
        .expect_err("either, not both");
        assert_eq!(both.code, RpcCode::InvalidArgument);

        // An EMPTY STRING is never read as the removal — the `spec_set_workspace` lesson (F2):
        // `None` here is an EFFECT, so an uninitialized caller variable must not trigger it.
        for empty in ["category", "category_id"] {
            let err = spec_set_category(&db, &json!({ "spec_id": sid, empty: "" }))
                .expect_err("an empty reference is refused");
            assert_eq!(err.code, RpcCode::InvalidArgument);
        }
        assert_eq!(
            category_of(&db, &sid).expect("still classified").slug,
            "bug",
            "no refusal above un-classified the Spec"
        );

        // An unknown SPEC is `invalid_id`.
        assert_eq!(
            spec_set_category(&db, &json!({ "spec_id": "01JZZZZZZZZZZZZZZZZZZZZZZZ" }))
                .expect_err("unknown spec")
                .code,
            RpcCode::InvalidId
        );
    }

    /// DONE-CRITERION: the tool LIST carries the new verb. `SPEC_TOOLS` is the one closed slice the
    /// advertisement, the dispatch and the surface count all read (`mcp::tests`), so a twelfth entry
    /// that is not routed — or routed but not advertised, which makes it undispatchable (N17) —
    /// fails here.
    #[test]
    fn the_category_verb_is_the_twelfth_spec_tool_and_it_is_routed() {
        let _g = db_guard();
        assert_eq!(SPEC_TOOLS.len(), 12);
        assert!(SPEC_TOOLS.contains(&SPEC_SET_CATEGORY_TOOL));
        let (db, project_id, _ws) = seeded_db();
        let sid = spec_id_of(
            &spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).expect("create"),
        );
        assert!(
            dispatch_spec_tool(
                &db,
                SPEC_SET_CATEGORY_TOOL,
                &json!({ "spec_id": sid, "category": "bug" })
            )
            .is_some(),
            "every SPEC_TOOLS entry must be routed by the shared dispatcher"
        );
    }
}
