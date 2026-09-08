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
    anchor_str, bounded_row, internal_db, json_len, map_content_write_err, optional_bool,
    optional_str, optional_usize, require_str, under_response_budget_strict, MAX_RESPONSE_BYTES,
    RESPONSE_ENVELOPE_BYTES,
};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

// --- Tool name constants (advertised in `tools/list`, dispatched below) ------

/// `spec_create` — create a new Spec (+ its first revision) from nyx state.
pub const SPEC_CREATE_TOOL: &str = "spec_create";
/// `spec_update_section` — edit ONE structured section of a Spec's active revision.
pub const SPEC_UPDATE_SECTION_TOOL: &str = "spec_update_section";
/// `spec_fork` — fork a Spec into a NEW revision (the prior one superseded).
pub const SPEC_FORK_TOOL: &str = "spec_fork";
/// `spec_transition` — a GUARDED Spec lifecycle transition, nommée par sa CIBLE `to`
/// (`ready` / `in_progress` / `in_review` / `completed` / `cancelled` / `draft`).
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

// --- The READ half (v2.8) --------------------------------------------------
//
// THE SURFACE WAS WRITE-ONLY, and that is not an omission an agent can work around. It could
// `spec_create`, `spec_update_section`, `spec_fork`, `task_add`, `task_transition` — and then had no
// way to READ any of it back. The only door was `agent_context`, which mixes the ROLE MANUAL with
// the state, refuses outright for `coder`/`doc` when no Spec resolves, and costs a manual's worth of
// tokens to answer "what did I just write". An agent that lost a Spec id had nothing at all
// (measured 2026-09-01: `spec_get`, `spec_list`, `task_list`, `task_get` — all absent).
//
// Three reads close it, and no more: ONE Spec whole, the Specs of a project, the tasks of a Spec.
// They are plain reads — no manual, no role, no run — so re-reading one's own write costs one call.

/// `spec_get` — READ one Spec whole: the row, its ACTIVE revision's sections, its phase plan, its
/// tasks.
pub const SPEC_GET_TOOL: &str = "spec_get";
/// `spec_list` — LIST the live Specs of a project (or of every project), to FIND one by title.
pub const SPEC_LIST_TOOL: &str = "spec_list";
/// `task_list` — LIST the tasks of a Spec (its plan and its findings).
pub const TASK_LIST_TOOL: &str = "task_list";

/// The Spec authoring tool names — advertised in `tools/list` and routed by [`dispatch_spec_tool`].
/// A closed slice so a new tool is added in exactly one place.
pub const SPEC_TOOLS: [&str; 16] = [
    SPEC_CREATE_TOOL,
    SPEC_UPDATE_SECTION_TOOL,
    SPEC_FORK_TOOL,
    SPEC_TRANSITION_TOOL,
    SPEC_SET_WORKSPACE_TOOL,
    SPEC_SET_CATEGORY_TOOL,
    TASK_ADD_TOOL,
    TASK_TRANSITION_TOOL,
    TASK_ADD_DEPENDENCY_TOOL,
    TASK_DELETE_TOOL,
    SPEC_LINK_TOOL,
    SPEC_UNLINK_TOOL,
    SPEC_DEPS_TOOL,
    SPEC_GET_TOOL,
    SPEC_LIST_TOOL,
    TASK_LIST_TOOL,
];

// --- The WIRE projection of a Spec -----------------------------------------

/// A Spec as it leaves nyx ON THE MCP WIRE: the stored row, with `phases` DECODED into the ordered
/// plan it has always been.
///
/// `specs.phases` is a TEXT column holding JSON (PDR-0022) and the row used to be serialized as-is,
/// so `spec_create` answered `"phases": "[]"` — a STRING. Measured 2026-09-01: an agent testing the
/// plan's length read 2 for an EMPTY plan, and every consumer had to know to parse a field that
/// `structuredContent` declared as structured. The COLUMN is untouched (this is a projection, not a
/// schema change), and this is the ONE function every Spec-returning tool serializes through, so the
/// wire shape cannot drift from one tool to the next.
pub(crate) fn spec_json(spec: &db::Spec) -> Value {
    let mut value = serde_json::to_value(spec).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "phases".to_string(),
            json!(db::parse_phase_plan(&spec.phases)),
        );
    }
    value
}

/// A task as it leaves nyx ON THE MCP WIRE: the stored row, plus the `phase_name` its numeric
/// `phase` stands for.
///
/// The column holds a RANK and the write path takes a NAME, so a row read back alone said "phase 2"
/// to a caller that had written "Wiring" — the same asymmetry `task_add` now answers with
/// `phase_name` (task `01M1F8X5B1WJ40XY2QRJ5G4W17`). The reads carry it too, or re-reading a task
/// would put the ambiguity straight back. `plan` is the Spec's decoded plan, passed in so a list of
/// N tasks costs ONE plan read instead of N.
pub(crate) fn task_json(task: &db::Task, plan: &[db::SpecPhase]) -> Value {
    let mut value = serde_json::to_value(task).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        let name = task
            .phase
            .and_then(|number| plan.iter().find(|p| p.number == number))
            .map(|p| p.name.clone());
        object.insert("phase_name".to_string(), json!(name));
    }
    value
}

/// [`task_json`] for a path that holds a `Task` without its Spec's plan in hand — the transition
/// verbs, and the three review surfaces that hand back a finding. Resolves the plan from the task's
/// own `spec_id` best-effort — a task with no Spec, or a Spec read that fails, yields
/// `phase_name: null` rather than dropping the field, so the shape a caller re-reads after a
/// transition matches the one `task_add`/`task_list` return.
///
/// `pub(crate)` because [`crate::mcp_tools_core`]'s `review_task_structure` /
/// `review_submit_feedback` serialize a task too, and ONE form of a task on the wire is the whole
/// point (#K13): a `db::Task` serialized raw carries the phase RANK and not the NAME the caller
/// wrote, which is the ambiguity phase 3 closed.
pub(crate) fn task_json_with_plan(db: &Db, task: &db::Task) -> Value {
    let plan = match task.spec_id.as_deref() {
        Some(spec_id) => db
            .with_conn(|c| db::get_spec(c, spec_id))
            .ok()
            .flatten()
            .map(|s| db::parse_phase_plan(&s.phases))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    task_json(task, &plan)
}

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

/// An OPTIONAL string argument constrained to a CLOSED vocabulary — the Rust-side twin of the SQL
/// `CHECK (col IN (…))` the column carries.
///
/// WHY IT IS CHECKED HERE AND NOT LEFT TO SQLITE. Left to the database, `task_add(kind:"bogus")`
/// answered `internal: db error: CHECK constraint failed: kind IN ('slice','human','review')`
/// (measured 2026-09-01). Three things are wrong with that at once: the CODE says server fault about
/// a caller's mistake, so an agent abandons or retries identically; the message is SQL, which asks
/// the caller to reverse-engineer a schema it cannot see; and it names the accepted values only by
/// accident of the constraint's body. The refusal built here is the shape the good errors of this
/// surface already have — `unknown section 'bogus' (accepted: …)` — and it carries the accepted set
/// in `data` too, so an agent can react without parsing prose.
fn optional_enum_str<'a>(
    args: &'a Value,
    key: &str,
    accepted: &[&str],
) -> Result<Option<&'a str>, RpcError> {
    let Some(value) = optional_str(args, key)? else {
        return Ok(None);
    };
    if accepted.contains(&value) {
        return Ok(Some(value));
    }
    Err(RpcError::new(
        RpcCode::InvalidArgument,
        format!(
            "unknown {key} '{value}' (accepted: {})",
            accepted.join(", ")
        ),
    )
    .with_data(json!({
        "reason": "unknown_value",
        "argument": key,
        "value": value,
        "accepted": accepted,
    })))
}

/// Read the OPTIONAL `phase` argument of `task_add`. It is a phase NAME — a STRING — and this is
/// where the two traps that compound on it are stopped.
///
/// TRAP ONE: THE TYPE. Every manual, every heading, every conversation says "phase 1", "phase 2", so
/// an integer is the natural thing to send. It used to be accepted and DROPPED — the task was
/// created outside any phase while the caller believed it had filed it (measured 2026-09-01,
/// `task_add(phase: 3)` → `phase: (empty)`). The generic "must be a string" refusal would stop the
/// call but not the misunderstanding, so this one says what the field IS.
///
/// TRAP TWO: THE ASYMMETRY THE FIRST ONE HIDES. The input is a name and the OUTPUT is a rank, and
/// nothing said so: `task_add(phase:"3")` answered `phase: 2`, because "3" was the second NAME that
/// Spec had seen. A caller re-reading its own write sees a value it never sent. The refusal states
/// the mechanism — nyx numbers the names, in first-seen order — and points at the `phase_name` the
/// response now carries beside the rank, so learning it costs no second call.
///
/// Shared by the WRITE (`task_add`) and the READ FILTER (`task_list`), which is why the third trap
/// — a name made only of digits — is NOT closed here but in [`phase_name_to_write`]: refusing to
/// FILTER on `"2"` would make the already-corrupted plans this review deliberately leaves alone
/// unreadable.
fn optional_phase_name(args: &Value) -> Result<Option<String>, RpcError> {
    match args.get("phase") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.to_string())),
        Some(other) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "argument 'phase' must be a STRING — it is a phase NAME, not a rank. Received \
                 {other}. nyx NUMBERS the phases itself, in the order their names are first seen \
                 while the Spec is a draft, so a number names nothing here: 'phase 3' is simply \
                 whichever name arrived third. Send the name you gave the phase (e.g. \
                 \"Foundations\"). The response then carries BOTH: `task.phase` is the rank nyx \
                 assigned, `phase_name` is the name you sent"
            ),
        )
        .with_data(json!({
            "reason": "phase_must_be_a_name",
            "argument": "phase",
            "expected_type": "string",
            "received": other,
        }))),
    }
}

/// Whether a phase NAME is indistinguishable from a RANK — nothing but digits, once trimmed.
///
/// Deliberately narrow (review A5). It is not "contains a digit": "Phase 2" and "v2 socle" are real
/// names a human wrote, and refusing them would push callers into naming phases things they do not
/// mean. What cannot be allowed is a name that READS as a position in the very list where positions
/// are the other axis.
fn is_rank_shaped(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit())
}

/// The `phase` argument as `task_add` may WRITE it — [`optional_phase_name`] plus the third trap.
///
/// TRAP THREE: THE FIRST TWO COME BACK THROUGH THE QUOTES (review A5). Refusing the INTEGER `2`
/// while accepting the STRING `"2"` closes the type and leaves the misunderstanding wide open — a
/// caller told "it must be a string" quotes the number and gets a phase literally NAMED `2`, whose
/// rank is whatever position it happened to arrive in. That is the exact ambiguity `phase_name`
/// exists to close, re-entering by the write side, and it is already materialised in stored plans:
/// `["Socle", "2"]`, and Specs whose phases are named `1`, `3`, `2` in that order.
///
/// So a name that is nothing but digits is refused HERE, on the write. A name that CONTAINS digits
/// ("Phase 2", "v2 socle") is a real name and stays accepted — the rule is about names
/// indistinguishable from ranks, not about digits. Reading is untouched: `task_list(phase: "2")`
/// still filters the plans already corrupted, which this review does not repair.
fn phase_name_to_write(args: &Value) -> Result<Option<String>, RpcError> {
    let name = optional_phase_name(args)?;
    if let Some(n) = &name {
        if is_rank_shaped(n) {
            let n = n.trim();
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "argument 'phase' = \"{n}\" is a phase NAME made only of digits, which collides \
                     with the RANK. nyx assigns the rank itself, in the order the names are first \
                     seen, so a phase named \"{n}\" reads as position {n} to every human and every \
                     tool that sees the plan — while its real rank is whatever position it arrived \
                     in. Give the phase a name that says what it is (e.g. \"Socle\", \"Wiring\"); \
                     the response carries BOTH, `task.phase` for the rank nyx assigned and \
                     `phase_name` for your name"
                ),
            )
            .with_data(json!({
                "reason": "phase_name_collides_with_rank",
                "argument": "phase",
                "received": n,
            })));
        }
    }
    Ok(name)
}

/// The refusal a REFERENCE argument naming no row earns: `invalid_argument`, naming the ARGUMENT
/// and quoting what was sent, plus where an existing value comes from.
///
/// `invalid_argument` rather than `invalid_id` is deliberate here: these are OPTIONAL attributes a
/// caller composes into one write (link this task to that Spec, under that parent), and what the
/// caller must change is the argument it passed, not the tool it reached for. Left to the FOREIGN
/// KEY constraint, the same mistake came back as `internal: db error: FOREIGN KEY constraint
/// failed` — which names neither the argument at fault nor the fact that the fault is the caller's.
fn unknown_reference(key: &str, value: &str, where_from: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidArgument,
        format!("argument '{key}' names nothing: '{value}' does not exist ({where_from})"),
    )
    .with_data(json!({
        "reason": "unknown_reference",
        "argument": key,
        "value": value,
    }))
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

/// Resolve the `to` string arg to the typed [`db::SpecTransition`].
///
/// LA CIBLE EST LE NOM, ET LE VERBE RESTE ACCEPTÉ. Les manuels d'agent écrivent la transition par
/// le STATUT VISÉ — `to="ready"`, `to="in_progress"`, `to="in_review"`, `to="completed"`,
/// `to="cancelled"`, `to="draft"` (`docs/product/agent-manuals.md`) — et c'est ce vocabulaire-là
/// qui fait foi. Les verbes conceptuels d'origine (`start`, `review`, `done`, `complete`,
/// `cancel`, `revise`) restent acceptés à côté : l'arm `"start" | "in_progress"` en était déjà le
/// précédent, et un agent qui a lu un ancien texte n'a pas à échouer sur un synonyme. Un mot hors
/// des deux jeux → `invalid_argument`, avec l'ensemble accepté dans le message.
/// `pub` parce que le SIDECAR l'appelle : le db-command `spec_transition` (les boutons de l'écran
/// Spec) doit accepter exactement le vocabulaire que l'outil MCP accepte, et un second parseur
/// serait la façon dont les deux surfaces divergent.
pub fn parse_spec_transition(name: &str) -> Result<db::SpecTransition, RpcError> {
    match name {
        "ready" => Ok(db::SpecTransition::Ready),
        "start" | "in_progress" => Ok(db::SpecTransition::Start),
        "review" | "in_review" => Ok(db::SpecTransition::Review),
        "done" | "complete" | "completed" => Ok(db::SpecTransition::Done),
        "cancel" | "cancelled" => Ok(db::SpecTransition::Cancel),
        "draft" | "revise" => Ok(db::SpecTransition::Revise),
        other => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "unknown target '{other}' (accepted: {SPEC_TRANSITION_TARGETS} — the conceptual \
                 verbs start, review, done, cancel and revise are accepted too)"
            ),
        )),
    }
}

/// Les CIBLES de `spec_transition`, telles que les manuels les écrivent — une seule source pour le
/// message de refus ci-dessus et pour le schéma annoncé dans `tools/list`, afin que ce qui est
/// promis et ce qui est accepté restent un seul fait.
pub const SPEC_TRANSITION_TARGETS: &str =
    "ready | in_progress | in_review | completed | cancelled | draft";

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
/// `pub` so every caller of the guarded transition authority — the MCP verb and the UI bridge
/// alike — surfaces the SAME refusal vocabulary. The bridge lives in the sidecar crate, so
/// `pub(crate)` would have left it to write a second, drifting set of sentences.
pub fn map_spec_transition_err(e: db::SpecTransitionError) -> RpcError {
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
            "this is a sensitive transition (ready, completed, cancelled, draft): the human \
             must say so in the chat first, in their own words — then pass user_confirmed=true. \
             Asking yourself is not a confirmation. Reopening a Spec in draft is one of the four \
             because it unfreezes the contract the human validated: a review return is answered \
             with CODE, filed as a finding, not by rewriting the Spec",
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
    Ok(ToolOutcome::changed(
        json!({
            "spec": spec_json(&spec),
            "revision": revision,
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
            "spec": spec_json(&spec),
            "category": category.as_ref().map(category_payload),
            "created": created,
        }),
        ChangedTopic::Specs,
    ))
}

/// `spec_update_section` — `{ spec_id, section, value }` → `{ updated: true }`. Edits ONE
/// structured section of the Spec's ACTIVE revision in place (NOT a fork) via the SHARED
/// `db::update_active_revision_section`. An unknown Spec → `invalid_id`; a Spec that has left its
/// writable state refuses the write (the freeze).
pub fn spec_update_section(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
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
///
/// STATE-GUARDED in the authority, not here: `draft` and `ready` only. A fork on a Spec that is
/// `in_progress` / `in_review` / blocked / closed comes back as `invalid_state` through
/// [`map_content_write_err`], with the sentence that names the route a review return really takes
/// (a finding). Rien n'est re-jugé dans cet adaptateur.
pub fn spec_fork(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let spec_id = spec_id.as_str();
    let change_reason = optional_str(args, "change_reason")?;
    let revision = db
        .with_conn(|c| db::fork_spec(c, spec_id, change_reason))
        .map_err(|e| match e {
            db::ContentWriteError::Db(diesel::result::Error::NotFound) => {
                RpcError::new(RpcCode::InvalidId, format!("unknown spec {spec_id}"))
            }
            other => map_content_write_err(other),
        })?;
    Ok(ToolOutcome::changed(
        json!({ "revision": revision }),
        ChangedTopic::Specs,
    ))
}

/// `spec_transition` — `{ spec_id, to, user_confirmed?, workspace_id?, cwd? }` →
/// `{ spec }`. Performs a GUARDED + VALIDATED lifecycle transition via the SHARED
/// `db::transition_spec_with_workspace`. The SENSITIVE transitions — `ready`, `done`
/// (→ `completed`), `cancel` (→ `cancelled`) and `revise` (→ `draft`), and those four alone — are refused unless
/// `user_confirmed=true` — the guard-rail is enforced in `db.rs`, so it holds through the tool
/// exactly as through the UI (à la depot `--user-confirmed`), et RIEN n'est re-jugé ici : ce
/// module est un adaptateur, la règle vit dans l'autorité unique.
///
/// L'ARGUMENT EST LA CIBLE, pas le verbe : `to="ready"` et non `transition="ready"`. Le manuel
/// d'agent (`docs/product/agent-manuals.md`) est le contrat, et il écrit la cible ;
/// [`parse_spec_transition`] accepte les deux orthographes pour que rien ne casse au passage.
///
/// On `start` (ready→in_progress) — which is NOT sensitive, and which the `/ndev` manual calls
/// with no `user_confirmed` — the transition ALSO POSES the Spec's workspace anchor in
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
    let transition = parse_spec_transition(require_str(args, "to")?)?;
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
        json!({ "spec": spec_json(&spec) }),
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
        json!({ "spec": spec_json(&spec) }),
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
    // THE FOUR ENUMERATED ARGUMENTS, checked against the vocabulary the column's CHECK holds — ABOVE
    // the database, so the refusal names the argument and its accepted set instead of surfacing the
    // constraint body as an `internal` error.
    if let Some(kind) = optional_enum_str(args, "kind", &db::TASK_KINDS)? {
        input.kind = kind.to_string();
    }
    if let Some(type_) = optional_enum_str(args, "type", &db::TASK_TYPES)? {
        input.type_ = type_.to_string();
    }
    if let Some(priority) = optional_enum_str(args, "priority", &db::TASK_PRIORITIES)? {
        input.priority = priority.to_string();
    }
    input.severity = optional_enum_str(args, "severity", &db::TASK_SEVERITIES)?.map(str::to_string);
    input.description = optional_str(args, "description")?.map(str::to_string);
    input.acceptance_criteria = optional_str(args, "acceptance_criteria")?.map(str::to_string);
    input.verification_command = optional_str(args, "verification_command")?.map(str::to_string);
    input.spec_id = crate::mcp_context::optional_spec_arg(db, args, "spec_id")?;
    input.parent_task_id = optional_str(args, "parent_task_id")?.map(str::to_string);
    // THE THREE REFERENCES, resolved BEFORE the insert rather than left to the FOREIGN KEY. Each
    // one is an id the caller composed, and each one used to die as the same opaque
    // `FOREIGN KEY constraint failed` — one sentence for three different mistakes, naming none of
    // them. Read here, the refusal says which argument is wrong and where a good value comes from.
    if db
        .with_conn(|c| db::get_project(c, project_id))
        .map_err(internal_db)?
        .is_none()
    {
        return Err(unknown_reference(
            "project_id",
            project_id,
            "use a project_id from list_projects",
        ));
    }
    if let Some(spec_id) = input.spec_id.as_deref() {
        if db
            .with_conn(|c| db::get_spec(c, spec_id))
            .map_err(internal_db)?
            .is_none()
        {
            return Err(unknown_reference(
                "spec_id",
                spec_id,
                "use a spec_id from spec_list(project_id=…), or omit it for a task with no Spec",
            ));
        }
    }
    if let Some(parent_task_id) = input.parent_task_id.as_deref() {
        if db
            .with_conn(|c| db::get_task(c, parent_task_id))
            .map_err(internal_db)?
            .is_none()
        {
            return Err(unknown_reference(
                "parent_task_id",
                parent_task_id,
                "use a task id from task_list(spec_id=…), or omit it for a top-level task",
            ));
        }
    }
    // ROUND (v2.5.11): the rework sub-phase a finding is rattached to (PER-PHASE, so it reads "2.1",
    // "4.1"…). OMITTED IS THE NORMAL CASE for a `kind=review` finding: `db::create_task` then fills
    // it from the Spec's own review pass, because the auditor raising the finding cannot know which
    // round it is in — and a finding that does not say which pass produced it cannot be arbitrated
    // afterwards. An explicit value still wins, for the caller attributing a finding to another pass.
    input.round = optional_usize(args, "round")?.map(|r| r as i32);
    // `phase` is a phase NAME: nyx materializes the plan from the names in first-seen order while
    // the Spec is drafted, and refuses an unknown one after (`db::create_task`). It needs a Spec —
    // the plan lives on the Spec.
    input.phase_name = phase_name_to_write(args)?;
    if input.phase_name.is_some() && input.spec_id.is_none() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "argument 'phase' needs a 'spec_id' — the phase plan lives on the Spec",
        ));
    }

    let task = db
        .with_conn(|c| db::create_task(c, project_id, title, input.clone()))
        .map_err(map_content_write_err)?;
    // ECHO THE NAME BESIDE THE RANK. `task.phase` is a NUMBER — the rank nyx assigned — while the
    // argument was a NAME, so a caller that sent `phase:"3"` read `phase: 2` back and had every
    // reason to call that a corruption (measured 2026-09-01). It was not: "3" was the SECOND name
    // this Spec had seen. The projection carries `phase_name` next to the rank, on the write AND on
    // the reads, so the two are readable as what they are instead of one field contradicting itself.
    let plan = match task.spec_id.as_deref() {
        Some(spec_id) => db
            .with_conn(|c| db::get_spec(c, spec_id))
            .map_err(internal_db)?
            .map(|s| db::parse_phase_plan(&s.phases))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(ToolOutcome::changed(
        json!({ "task": task_json(&task, &plan) }),
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
            json!({ "task": task_json_with_plan(db, &task) }),
            ChangedTopic::Specs,
        ));
    }

    let transition = parse_task_transition(transition_name)?;
    let reason = optional_str(args, "reason")?;
    let task = db
        .with_conn(|c| db::transition_task(c, task_id, transition, reason))
        .map_err(map_task_transition_err)?;
    Ok(ToolOutcome::changed(
        json!({ "task": task_json_with_plan(db, &task) }),
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

// --- The READ half: spec_get / spec_list / task_list ------------------------

/// How many rows a paged read of this surface returns when the caller names no `limit`, and the
/// ceiling it clamps an explicit one to. A naive call must be SAFE and USEFUL at once: 50 Specs (or
/// tasks) is a whole board, and the byte budget below cuts in long before it hurts.
const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 200;

/// The `offset` / `limit` pair of a paged read, clamped. `limit` 0 (or absent) means
/// [`DEFAULT_PAGE`]; anything above [`MAX_PAGE`] is clamped rather than refused — a caller asking
/// for more than nyx will serve wants "as much as you can", not an error.
pub(crate) fn page_args(args: &Value) -> Result<(usize, usize), RpcError> {
    let offset = optional_usize(args, "offset")?.unwrap_or(0);
    let limit = match optional_usize(args, "limit")? {
        None | Some(0) => DEFAULT_PAGE,
        Some(n) => n.min(MAX_PAGE),
    };
    Ok((offset, limit))
}

/// Emit one page of already-projected `rows` under BOTH the count bound and the byte budget, as the
/// `{ items, total, returned, offset, next_offset, truncated }` shape every read here answers with.
///
/// TWO BOUNDS, because either alone leaves a hole: a count cannot stop 50 wide rows from blowing the
/// transport, and a byte budget alone makes "how much did I get" unanswerable. `next_offset` is
/// non-null exactly when rows remain, so a caller pages by following it and stops when it is null —
/// no arithmetic, and no way to build a loop that never advances.
pub(crate) fn page_of(
    key: &str,
    rows: Vec<Value>,
    offset: usize,
    limit: usize,
    head_bytes: usize,
) -> Value {
    let total = rows.len();
    let window: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();
    let budget = MAX_RESPONSE_BYTES.saturating_sub(RESPONSE_ENVELOPE_BYTES + head_bytes);
    let (kept, cut_by_bytes) = under_response_budget_strict(window, budget);
    let returned = kept.len();
    let next = offset + returned;
    let more = next < total;
    json!({
        key: kept,
        "total": total,
        "returned": returned,
        "offset": offset,
        "next_offset": if more { json!(next) } else { Value::Null },
        "truncated": more,
        // Says WHICH bound cut, because the two call for different reactions: a byte cut means ask
        // for fewer rows, a count cut means simply page on.
        "truncated_by_size": cut_by_bytes,
    })
}

/// `spec_get` — `{ spec_id, project_id? }` → `{ spec, revision, tasks, … }`. READ one Spec WHOLE:
/// the row (with its `phases` plan decoded), its ACTIVE revision's six sections, and its tasks (each
/// carrying `phase_name` beside the numeric `phase`).
///
/// THIS IS THE READ-BACK THE SURFACE DID NOT HAVE. An agent could author a Spec and had no way to
/// re-read it: the only door was `agent_context`, which renders a ROLE MANUAL around the state, and
/// refuses outright for `coder`/`doc` when no Spec resolves. Re-reading one's own write is not a
/// role question, so it does not go through a role.
///
/// The task list is PAGED (`offset`/`limit`) like [`task_list`], and for the same reason: a Spec
/// with a long plan must not make its own read unreadable. An unknown Spec is `invalid_id`, naming
/// the read that finds one.
pub fn spec_get(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let (offset, limit) = page_args(args)?;
    let spec = db
        .with_conn(|c| db::get_spec(c, &spec_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!(
                    "unknown spec {spec_id} (use spec_list(project_id=…) to find one, or the id \
                     spec_create returned)"
                ),
            )
        })?;
    let revision = db
        .with_conn(|c| db::get_active_revision(c, &spec.id))
        .map_err(internal_db)?;
    let tasks = db
        .with_conn(|c| db::list_tasks_for_spec(c, &spec.id))
        .map_err(internal_db)?;
    let plan = db::parse_phase_plan(&spec.phases);
    let head = json!({
        "spec": spec_json(&spec),
        "revision": revision.as_ref().map(|r| bounded_row(&json!(r))),
    });
    let rows: Vec<Value> = tasks
        .iter()
        .map(|t| bounded_row(&task_json(t, &plan)))
        .collect();
    let page = page_of("tasks", rows, offset, limit, json_len(&head));
    let mut result = head;
    if let (Some(result), Some(page)) = (result.as_object_mut(), page.as_object()) {
        for (key, value) in page {
            // The paging counters are about the TASKS here, and say so — `tasks_total` reads
            // unambiguously beside a `spec` and a `revision`, where a bare `total` would not.
            let key = if key == "tasks" {
                key.clone()
            } else {
                format!("tasks_{key}")
            };
            result.insert(key, value.clone());
        }
    }
    Ok(ToolOutcome::read(result))
}

/// `spec_list` — `{ project_id?, status?, offset?, limit? }` → the LIVE Specs in BOARD ORDER, each
/// with its human `key` (`NYX-3`) beside its canonical id.
///
/// THE READ THAT FINDS AN ID NOBODY KEPT. `spec_create` returns an id once; an agent that lost it
/// had no way back — no list, no search, nothing (measured 2026-09-01). `project_id` is OPTIONAL and
/// the fallbacks are deliberate: absent, nyx resolves the project from the call's own anchors
/// (`workspace_id` / `cwd`), and failing that lists EVERY project's Specs — because "I do not know
/// which project" is precisely the state an agent looking for a lost id is in. A project_id that is
/// PRESENT but names nothing is the opposite case and is refused by name: the agent named a project,
/// so an empty list would read as "this project has no Spec" (#N4).
///
/// # THE ORDER IS OLDEST FIRST, and this doc used to say the reverse (#N7)
///
/// [`db::list_specs`] orders `(position asc, number asc, id asc)` — the BOARD's order, the same one
/// the UI reads. `specs.position` is written exactly once, as `0` by `create_spec`, and there is no
/// reorder path in the codebase that ever writes it again: every live Spec therefore sits in the
/// same tier and the tie-break `number asc` decides, so the MOST RECENTLY CREATED Spec comes LAST.
/// The doc claimed "newest board order first", which on a project with more than `limit` live Specs
/// put the Spec an agent had just created on the last page — the exact case this tool advertises.
///
/// The prose is corrected rather than the order: this list IS the board, and reversing it here would
/// either desynchronise the two readings of the same query or reorder the user's board to suit a
/// lookup. TO FIND THE SPEC YOU JUST CREATED, do one of:
///   * read `total` from the answer and re-call with `offset: total - 1` — the newest is last;
///   * filter `status: "draft"`, which a freshly created Spec is;
///   * or reference it by its human key, whose number is the highest in the project (`crate::spec_ref`).
///
/// `key` is computed here rather than stored: it is `projects.key || '-' || specs.number`, and it is
/// what makes the `KEY-N` reference form usable from a list (`crate::spec_ref`).
pub fn spec_list(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = match optional_str(args, "project_id")? {
        // NAMED EXPLICITLY — so it must EXIST (#N4). Treated as a bare filter, a typo'd or stale id
        // answered `{ specs: [], total: 0 }`, which reads as "that project has no Spec" and sends an
        // agent looking for a lost id off in the wrong direction entirely. `task_add` already
        // refuses the same argument by name; this says the same thing in the same words.
        Some(id) => {
            if db
                .with_conn(|c| db::get_project(c, id))
                .map_err(internal_db)?
                .is_none()
            {
                return Err(unknown_reference(
                    "project_id",
                    id,
                    "use a project_id from list_projects, or omit it to search every project",
                ));
            }
            Some(id.to_string())
        }
        // The other anchors this call may carry — a workspace, a cwd — resolve a project the same
        // way every other tool resolves one. Nothing resolved = every project. DELIBERATE, and NOT
        // the case above: "I do not know which project" is precisely the state an agent looking for
        // a lost id is in, and widening the search is the answer to it.
        None => crate::mcp_context::resolve_project(db, args)?.map(|p| p.id),
    };
    let status = optional_str(args, "status")?.map(str::to_string);
    let (offset, limit) = page_args(args)?;
    let specs = db
        .with_conn(|c| db::list_specs(c, project_id.as_deref()))
        .map_err(internal_db)?;
    // The project KEY of each Spec, read once per project rather than once per Spec.
    let mut keys: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut rows: Vec<Value> = Vec::with_capacity(specs.len());
    for spec in &specs {
        if status.as_deref().is_some_and(|s| s != spec.status) {
            continue;
        }
        let key = match keys.get(&spec.project_id) {
            Some(key) => Some(key.clone()),
            None => {
                let key = db
                    .with_conn(|c| db::get_project(c, &spec.project_id))
                    .map_err(internal_db)?
                    .map(|p| p.key);
                if let Some(key) = key.clone() {
                    keys.insert(spec.project_id.clone(), key);
                }
                key
            }
        };
        let mut row = spec_json(spec);
        if let (Some(object), Some(key)) = (row.as_object_mut(), key) {
            object.insert("key".to_string(), json!(format!("{key}-{}", spec.number)));
        }
        rows.push(bounded_row(&row));
    }
    let head = json!({ "project_id": project_id });
    let page = page_of("specs", rows, offset, limit, json_len(&head));
    let mut result = page;
    if let Some(object) = result.as_object_mut() {
        object.insert("project_id".to_string(), json!(project_id));
    }
    Ok(ToolOutcome::read(result))
}

/// `task_list` — `{ spec_id, project_id?, kind?, status?, phase?, offset?, limit? }` → the Spec's
/// tasks, in board order, each carrying `phase_name` beside its numeric `phase`.
///
/// THE PLAN AND THE FINDINGS ARE THE SAME LIST (finding = task, PDR-0016), so `kind` is how a caller
/// narrows to one or the other rather than a second tool. `phase` filters by NAME here too — the
/// same word `task_add` takes — so an agent never has to convert a name into a rank to ask a
/// question about a phase.
pub fn task_list(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let spec_id =
        crate::mcp_context::require_spec_arg_project_relative_refused(db, args, "spec_id")?;
    let kind = optional_enum_str(args, "kind", &db::TASK_KINDS)?.map(str::to_string);
    let status = optional_str(args, "status")?.map(str::to_string);
    let phase = optional_phase_name(args)?;
    let (offset, limit) = page_args(args)?;
    let spec = db
        .with_conn(|c| db::get_spec(c, &spec_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("unknown spec {spec_id} (use spec_list(project_id=…) to find one)"),
            )
        })?;
    let plan = db::parse_phase_plan(&spec.phases);
    let tasks = db
        .with_conn(|c| db::list_tasks_for_spec(c, &spec.id))
        .map_err(internal_db)?;
    let wanted_phase = phase.as_deref().map(|name| {
        plan.iter()
            .find(|p| p.name.trim() == name.trim())
            .map(|p| p.number)
    });
    let rows: Vec<Value> = tasks
        .iter()
        .filter(|t| kind.as_deref().is_none_or(|k| k == t.kind))
        .filter(|t| status.as_deref().is_none_or(|s| s == t.status))
        // A `phase` naming nothing in the plan yields NO task rather than every task: the question
        // was about one phase, and answering it with the whole list would read as an answer.
        .filter(|t| wanted_phase.is_none_or(|number| number.is_some() && number == t.phase))
        .map(|t| bounded_row(&task_json(t, &plan)))
        .collect();
    let head = json!({ "spec_id": spec.id, "phases": plan });
    let page = page_of("tasks", rows, offset, limit, json_len(&head));
    let mut result = page;
    if let Some(object) = result.as_object_mut() {
        object.insert("spec_id".to_string(), json!(spec.id));
        // The PLAN travels with the list, because a `phase` rank is unreadable without it and a
        // caller that has the list should not need a second call to name what it is looking at.
        object.insert("phases".to_string(), json!(plan));
    }
    Ok(ToolOutcome::read(result))
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
/// spec_update_section / task_add / task_transition / dev` set (a `spec_transition` onto
/// `in_progress` is the "dev" launch). `task_add_dependency` / `task_delete` and the v2.6
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
        // The Spec is returned directly as `result.spec`.
        SPEC_CREATE_TOOL | SPEC_TRANSITION_TOOL | SPEC_SET_WORKSPACE_TOOL => {
            str_at(result, &["spec", "id"])
        }
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
        SPEC_TRANSITION_TOOL => Some(spec_transition(db, args)),
        SPEC_SET_WORKSPACE_TOOL => Some(spec_set_workspace(db, args)),
        SPEC_SET_CATEGORY_TOOL => Some(spec_set_category(db, args)),
        TASK_ADD_TOOL => Some(task_add(db, args)),
        TASK_TRANSITION_TOOL => Some(task_transition(db, args)),
        TASK_ADD_DEPENDENCY_TOOL => Some(task_add_dependency(db, args)),
        TASK_DELETE_TOOL => Some(task_delete(db, args)),
        SPEC_LINK_TOOL => Some(spec_link(db, args)),
        SPEC_UNLINK_TOOL => Some(spec_unlink(db, args)),
        SPEC_DEPS_TOOL => Some(spec_deps(db, args)),
        // The READ half (v2.8) — plain reads, no role, no run.
        SPEC_GET_TOOL => Some(spec_get(db, args)),
        SPEC_LIST_TOOL => Some(spec_list(db, args)),
        TASK_LIST_TOOL => Some(task_list(db, args)),
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
    }

    /// THE `spec_*` TOOLS INHERIT THE SHORT-REFERENCE GRAMMAR, and not one of them implements it:
    /// each reads its argument through [`crate::mcp_context::require_spec_arg`], the SAME entry
    /// point `agent_context` goes through. Driven on the HUMAN KEY (which names its
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

    /// `spec_update_section` CARRIES NO PROJECT ANCHOR, so a `#N` or a bare `N` has nothing to
    /// count in and is refused, naming the forms that DO work without one. (The engine's `run_id`
    /// anchor is gone with the engine — PDR-0031.)
    #[test]
    fn spec_update_section_refuses_a_number_it_has_no_project_to_count_in() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let number = db.with_conn(|c| {
            db::create_spec(c, &project_id, "Edited", db::SpecSections::empty())
                .expect("spec")
                .0
                .number
        });
        let err = spec_update_section(
            &db,
            &json!({ "spec_id": format!("#{number}"), "section": "problem", "value": "rewritten" }),
        )
        .expect_err("no project to count #N in");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains(crate::spec_ref::ANCHORLESS_FORMS),
            "the refusal names the forms that work without an anchor: {}",
            err.message
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
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
            &json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": ws }),
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
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
                &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
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
                "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": ""
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
            &json!({ "spec_id": sid, "to": "start", "user_confirmed": true }),
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
                "spec_id": other, "to": "start", "user_confirmed": true, "cwd": nested
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
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

    /// `spec_transition` start POSES the anchor in one op (review finding #25) and asks the human
    /// NOTHING (PDR-0031 : l'ensemble sensible est `ready`/`completed`/`cancelled`). Un `start`
    /// SANS `user_confirmed` — la ligne exacte du manuel `/ndev` — passe et ancre la Spec. L'ancre
    /// se résout aussi depuis `cwd` quand `workspace_id` est absent (le même matcher
    /// cwd→workspace que le rendu de contexte).
    #[test]
    fn spec_transition_start_needs_no_confirmation_and_poses_the_anchor() {
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();

        // UNCONFIRMED start with an explicit workspace_id → in_progress + anchored. C'est
        // littéralement l'appel du manuel `/ndev` §2 étape 0, qui ne porte pas `user_confirmed`.
        let started = spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "start", "workspace_id": ws }),
        )
        .expect("start ne demande aucune confirmation — le manuel l'appelle nu");
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();
        // No workspace_id — resolve it from the cwd (nested under the workspace root).
        let nested = format!("{cwd}/crates/api");
        let started = spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "cwd": nested }),
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

    /// DONE-CRITERION (gel de la Spec) : **`spec_fork` sur une Spec en review est refusé, et le
    /// refus NOMME LA ROUTE.** Un `invalid_state` sec renverrait l'agent chercher une autre porte —
    /// c'est précisément ce qu'il a fait. La phrase doit dire où va un retour de review : un
    /// FINDING, et le code qui change. `in_progress` est refusé de la même façon.
    #[test]
    fn spec_fork_is_refused_past_ready_and_the_refusal_names_the_finding_route() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Frozen", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();

        // Depuis `ready`, le fork reste la route de rework : il passe.
        spec_fork(&db, &json!({ "spec_id": sid })).expect("fork depuis ready");

        for status in ["in_progress", "in_review"] {
            spec_transition(&db, &json!({ "spec_id": sid, "to": status })).unwrap();
            let err = spec_fork(&db, &json!({ "spec_id": sid })).unwrap_err();
            assert_eq!(err.code, RpcCode::InvalidState, "fork depuis {status}");
            assert!(
                err.message.contains("finding"),
                "le refus doit nommer la route — un finding, et le code qui change : {}",
                err.message
            );
            assert!(
                err.message.contains(status),
                "le refus cite le statut qui l'a produit : {}",
                err.message
            );
        }
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
            spec_transition(&db, &json!({ "spec_id": sid, "to": "ready" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
        assert!(err.message.contains("sensitive"));

        // With confirmation + a substantive draft (problem + solution present) → ready.
        let out = spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();
        assert_eq!(out.result["spec"]["status"], "ready");
    }

    /// DONE-CRITERION (PDR-0031, `agent-manuals.md`) : **l'argument est la CIBLE, et le verbe
    /// historique reste accepté.** Les manuels écrivent `to="in_review"` ; le code d'origine
    /// n'acceptait que `transition="review"`. Les deux orthographes doivent désigner le même
    /// mouvement, sinon un agent qui a lu le manuel et un agent qui a lu un ancien texte ne
    /// pilotent pas la même machine.
    #[test]
    fn spec_transition_accepts_both_the_target_and_the_verb_spellings() {
        for (target, verb) in [
            ("ready", "ready"),
            ("in_progress", "start"),
            ("in_review", "review"),
            ("completed", "done"),
            ("cancelled", "cancel"),
            ("draft", "revise"),
        ] {
            let by_target = parse_spec_transition(target).expect("the manual's target spelling");
            let by_verb = parse_spec_transition(verb).expect("the conceptual verb");
            assert_eq!(
                by_target, by_verb,
                "`to={target}` and `to={verb}` must name the SAME move"
            );
            assert_eq!(
                by_target.target_status(),
                if target == "in_progress" {
                    "in_progress"
                } else {
                    target
                },
                "the target spelling IS the status it lands on"
            );
        }
        // `complete` reste accepté (l'alias d'origine), et un mot hors des deux jeux est refusé en
        // nommant l'ensemble.
        assert_eq!(
            parse_spec_transition("complete").unwrap(),
            db::SpecTransition::Done
        );
        let err = parse_spec_transition("finish").unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.contains(SPEC_TRANSITION_TARGETS),
            "the refusal names the accepted set: {}",
            err.message
        );
    }

    /// DONE-CRITERION : **les trois cibles sensibles sont refusées sans `user_confirmed`, et le
    /// refus dit que l'humain doit l'avoir dit dans le chat.** Le garde-fou vit dans `db.rs` — ce
    /// test prouve qu'il tient À TRAVERS l'outil, cible par cible, sans qu'aucune ne se glisse.
    #[test]
    fn spec_transition_refuses_every_sensitive_target_without_user_confirmed() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        // QUATRE, pas trois : `draft` a rejoint l'ensemble le jour où un agent a répondu à une
        // review en forkant la Spec et en la rouvrant pour qu'elle colle à son code.
        for target in ["ready", "completed", "cancelled", "draft"] {
            let created = spec_create(
                &db,
                &json!({ "project_id": project_id, "title": target, "problem": "p", "solution": "s" }),
            )
            .unwrap();
            let sid = spec_id_of(&created);
            let err = spec_transition(&db, &json!({ "spec_id": sid, "to": target })).unwrap_err();
            assert_eq!(err.code, RpcCode::InvalidState, "`to={target}` must refuse");
            assert!(
                err.message.contains("in the chat"),
                "the refusal must send the agent back to the human, not to a flag: {}",
                err.message
            );
            // …et le refus n'a RIEN écrit : la Spec est toujours au statut d'avant.
            let out = spec_get(&db, &json!({ "spec_id": sid })).unwrap();
            assert_eq!(out.result["spec"]["status"], "draft");
        }
    }

    /// DONE-CRITERION (`agent-manuals.md` §1 étape 5 / §5 ligne 2) : **`to="draft"` rouvre une Spec
    /// déjà `ready`.** C'est la seconde moitié du geste de rework — `spec_fork` ouvre la révision,
    /// celle-ci rend les sections éditables — et depuis le gel de la Spec elle est SENSIBLE :
    /// rouvrir dégèle le contrat validé, donc l'humain le dit et l'agent transmet
    /// `user_confirmed=true`.
    #[test]
    fn spec_transition_to_draft_reopens_a_spec_for_rework() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Rework", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();

        // Le geste du manuel, dans l'ordre du manuel : fork (légal depuis `ready`), puis retour en
        // draft — celui-ci confirmé par l'humain.
        spec_fork(&db, &json!({ "spec_id": sid, "change_reason": "rework" })).unwrap();
        let reopened = spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "draft", "user_confirmed": true }),
        )
        .unwrap();
        assert_eq!(reopened.result["spec"]["status"], "draft");

        // Une Spec DÉJÀ draft n'a rien à rouvrir : la garde de source la refuse.
        let err = spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "draft", "user_confirmed": true }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidState);
    }

    /// DONE-CRITERION (le trou #1 des manuels) : **`spec_transition` est ROUTÉ.** Déclaré,
    /// dispatché nulle part, il ne pouvait être appelé par aucune surface ; le dispatcher partagé
    /// doit désormais répondre `Some(..)` pour lui comme pour les autres entrées de `SPEC_TOOLS`.
    #[test]
    fn spec_transition_is_a_spec_tool_and_it_is_routed() {
        let _g = db_guard();
        assert!(SPEC_TOOLS.contains(&SPEC_TRANSITION_TOOL));
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Routed", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        let routed = dispatch_spec_tool(
            &db,
            SPEC_TRANSITION_TOOL,
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .expect("the shared dispatcher answers for spec_transition")
        .expect("the transition succeeds");
        assert_eq!(routed.result["spec"]["status"], "ready");
        assert_eq!(
            routed.effects,
            vec![ChangedTopic::Specs],
            "a Spec mutation re-pulls the Specs surfaces"
        );
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();
        let started = spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": ws }),
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
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "start", "user_confirmed": true, "workspace_id": ws }),
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
    /// advertisement, the dispatch and the surface count all read (`mcp::tests`), so an entry that
    /// is not routed — or routed but not advertised, which makes it undispatchable (N17) — fails
    /// here.
    #[test]
    fn the_category_verb_is_a_spec_tool_and_it_is_routed() {
        let _g = db_guard();
        assert_eq!(SPEC_TOOLS.len(), 16);
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

    /// The literal wordings SQLite writes. Nothing an agent reads may carry one — see
    /// [`crate::mcp::RpcError::new`], where the guarantee is enforced, and the reason it exists.
    fn assert_no_sql_text(what: &str, message: &str) {
        let lower = message.to_ascii_lowercase();
        for marker in [
            "constraint failed",
            "foreign key",
            "check (",
            "no such table",
            "no such column",
            "sql logic error",
        ] {
            assert!(
                !lower.contains(marker),
                "{what} leaks SQL text ({marker:?}): {message}"
            );
        }
    }

    /// DONE-CRITERION (task `01M1F8X44HSK742EVVG8F94H6H`): **a constraint the caller can fix is an
    /// `invalid_argument` naming the argument, never an `internal` carrying SQL.**
    ///
    /// The two shapes measured live on 2026-09-01, both `-32000 internal`:
    ///   * an unknown `spec_id` → `db error: FOREIGN KEY constraint failed`
    ///   * `kind:"bogus"` → `db error: CHECK constraint failed: kind IN ('slice','human','review')`
    ///
    /// In both the fault is in the ARGUMENTS, and `internal` told the caller the opposite — so an
    /// agent reading it concludes nyx is broken and either abandons or retries the identical call.
    /// Each is now refused ABOVE the database, by name, and — for a closed vocabulary — with the set
    /// it may pick from.
    #[test]
    fn task_add_refuses_a_bad_reference_and_a_bad_vocabulary_by_name() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();

        // 1. AN UNKNOWN SPEC — the argument is named, and no FOREIGN KEY sentence survives.
        let err = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "spec_id": "01NOPE" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);
        assert!(
            err.message.contains("spec_id") && err.message.contains("01NOPE"),
            "the refusal names the argument AND quotes what was sent: {}",
            err.message
        );
        assert_no_sql_text("task_add(unknown spec_id)", &err.message);

        // 2. AN UNKNOWN KIND — every accepted value is listed, in prose AND in `data`.
        let err = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "kind": "bogus" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);
        for accepted in db::TASK_KINDS {
            assert!(
                err.message.contains(accepted),
                "the refusal lists the accepted kind '{accepted}': {}",
                err.message
            );
        }
        assert_eq!(
            err.data.as_ref().and_then(|d| d.get("accepted")).cloned(),
            Some(json!(db::TASK_KINDS)),
            "…and carries the set structurally, so an agent need not parse the prose"
        );
        assert_no_sql_text("task_add(bad kind)", &err.message);

        // 3. THE OTHER THREE CLOSED VOCABULARIES answer the same way, from the same constants.
        for (key, accepted) in [
            ("type", &db::TASK_TYPES[..]),
            ("priority", &db::TASK_PRIORITIES[..]),
            ("severity", &db::TASK_SEVERITIES[..]),
        ] {
            let err = task_add(
                &db,
                &json!({ "project_id": project_id, "title": "t", key: "bogus" }),
            )
            .unwrap_err();
            assert_eq!(err.code, RpcCode::InvalidArgument, "{key}: {}", err.message);
            assert!(
                err.message.contains(key) && accepted.iter().all(|v| err.message.contains(v)),
                "{key} refusal names the argument and its accepted set: {}",
                err.message
            );
        }

        // 4. AN UNKNOWN PARENT + AN UNKNOWN PROJECT — the other two references of this call.
        let err = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "parent_task_id": "01NOPARENT" }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);
        assert!(
            err.message.contains("parent_task_id"),
            "the refusal names parent_task_id: {}",
            err.message
        );
        let err = task_add(&db, &json!({ "project_id": "01NOPROJECT", "title": "t" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);
        assert!(
            err.message.contains("project_id"),
            "the refusal names project_id: {}",
            err.message
        );

        // 5. THE ALREADY-ACTIONABLE ERRORS ARE UNCHANGED — the non-goal of the task, asserted
        // rather than assumed: the missing-argument refusal and the unknown-section refusal keep
        // their exact wording and code.
        let err = task_add(&db, &json!({})).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert_eq!(
            err.message,
            "missing or empty required argument 'project_id'"
        );
        let err = parse_section("bogus").unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            err.message.starts_with("unknown section 'bogus' (accepted: problem, solution,"),
            "{}",
            err.message
        );

        // …and a NOMINAL task still writes, so none of the pre-validation blocks the good path.
        let ok = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "kind": "human",
                     "verification_command": "check it", "priority": "high" }),
        )
        .expect("a well-formed task_add still writes");
        assert_eq!(ok.result["task"]["kind"], "human");
    }

    /// #K2 — A REFUSAL THAT QUOTES THE CALLER SURVIVES THE ANTI-SQL NET — ON THE REAL TOOL.
    ///
    /// The net cut on SQLite's literal wordings wherever they appeared, for EVERY error code. But a
    /// good refusal REPEATS the argument it is refusing, so `task_add(kind: "no such table")` built
    /// `unknown kind 'no such table' (accepted: …)` — a marker with no `": "` in front of it, hence
    /// an empty head, hence the WHOLE message replaced by a sentence about a storage layer nothing
    /// had consulted. The accepted-values list, the only actionable part of the refusal, went with
    /// it (it survived in `error.data`, which the client of the day did not render).
    ///
    /// [`crate::mcp`]'s own `an_argument_refusal_that_quotes_the_caller_survives_the_sql_net` pins
    /// the NET, on the wording copied from here. This drives the TOOL, so the guarantee cannot go
    /// on holding for a copy that has drifted from what `task_add` really builds — or for a code
    /// this refusal no longer carries.
    #[test]
    fn task_add_kind_that_reads_like_sql_still_lists_its_accepted_values() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();

        let err = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "kind": "no such table" }),
        )
        .unwrap_err();
        // `invalid_argument`, NOT `internal` — which is what keeps it out of the net entirely.
        assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);
        for accepted in db::TASK_KINDS {
            assert!(
                err.message.contains(accepted),
                "the accepted kind '{accepted}' must survive the net: {}",
                err.message
            );
        }
        assert!(
            err.message.contains("no such table"),
            "and the refusal still quotes what the caller actually sent: {}",
            err.message
        );
        assert!(
            !err.message.contains("storage layer"),
            "nothing about a database nyx never consulted: {}",
            err.message
        );
    }

    /// #N4 — A `project_id` THE CALLER NAMED MUST EXIST.
    ///
    /// Treated as a bare filter, `spec_list(project_id: "bogus")` answered `{ specs: [], total: 0 }`
    /// — "this project has no Spec" — for an id that names nothing, while `task_add` refuses the
    /// same argument by name. An agent hunting a lost Spec reads that empty list as an answer and
    /// stops looking.
    ///
    /// The OTHER half of the tool's fallback is deliberate and untouched: with NO `project_id` and
    /// nothing resolvable from the anchors, the search widens to every project, because "I do not
    /// know which project" is exactly the state that reading is for.
    #[test]
    fn spec_list_refuses_a_project_id_that_names_nothing_but_still_widens_when_given_none() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        spec_create(&db, &json!({ "project_id": project_id, "title": "Une Spec" })).unwrap();

        // NAMED and unknown → a named refusal, the same one `task_add` gives for the same argument.
        let err = spec_list(&db, &json!({ "project_id": "bogus" })).unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert_eq!(
            err.data.as_ref().and_then(|d| d.get("reason")),
            Some(&json!("unknown_reference")),
            "the refusal is machine-readable: {err:?}"
        );
        assert!(
            err.message.contains("bogus") && err.message.contains("project_id"),
            "and it quotes the argument at fault: {}",
            err.message
        );

        // ABSENT, with no resolvable anchor → the cross-project listing, unchanged.
        let all = spec_list(&db, &json!({})).unwrap();
        assert!(
            all.result["specs"]
                .as_array()
                .expect("specs")
                .iter()
                .any(|s| s["title"] == json!("Une Spec")),
            "an unscoped call still searches every project: {}",
            all.result
        );
    }

    /// #N7 — THE ORDER IS THE BOARD'S, AND THE PROSE NOW SAYS SO.
    ///
    /// TRACED FIRST, as the finding asked: `db::list_specs` orders `(position asc, number asc, id
    /// asc)`; `specs.position` is written ONLY by `create_spec`, always as `0`, and NOTHING in the
    /// codebase ever writes it again — there is no board reorder. So every live Spec shares one
    /// tier, `number asc` decides, and the newest is LAST. The doc claimed "newest board order
    /// first", which on a project past the default limit put the Spec an agent had just created on
    /// the last page — the very case the tool advertises.
    ///
    /// The order is left alone (this list IS the board the UI reads) and the prose corrected, so
    /// this test pins the REAL order and the documented way back to the newest Spec.
    #[test]
    fn spec_list_returns_the_board_order_oldest_first_and_the_newest_is_the_last_row() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let ids: Vec<String> = ["Premiere", "Deuxieme", "Troisieme"]
            .iter()
            .map(|title| {
                spec_id_of(
                    &spec_create(&db, &json!({ "project_id": project_id, "title": title })).unwrap(),
                )
            })
            .collect();

        let listed = spec_list(&db, &json!({ "project_id": project_id })).unwrap();
        let rows = listed.result["specs"].as_array().expect("specs");
        assert_eq!(
            rows.iter().map(|s| s["id"].clone()).collect::<Vec<_>>(),
            ids.iter().map(|id| json!(id)).collect::<Vec<_>>(),
            "creation order, oldest first — NOT newest first: {}",
            listed.result
        );

        // THE DOCUMENTED WAY BACK to the Spec you just created: `total - 1`.
        let total = listed.result["total"].as_u64().expect("a total") as usize;
        let newest = spec_list(
            &db,
            &json!({ "project_id": project_id, "offset": total - 1, "limit": 1 }),
        )
        .unwrap();
        assert_eq!(
            newest.result["specs"][0]["id"],
            json!(ids.last().unwrap()),
            "offset = total - 1 lands on the newest Spec: {}",
            newest.result
        );
    }

    /// DONE-CRITERION (task `01M1F8X7SE5B3BQTKSXCFQGTNY`): **an agent re-reads what it wrote, and
    /// finds a Spec whose id it never kept — without `agent_context`.**
    ///
    /// The surface was WRITE-ONLY (measured 2026-09-01: `spec_get`, `spec_list`, `task_list`,
    /// `task_get` all absent). The only door was `agent_context`, which renders a ROLE MANUAL around
    /// the state and refuses outright for `coder`/`doc` when no Spec resolves — so "what did I just
    /// write" had no answer at all. The whole journey is replayed here: author, slice, lose the id,
    /// find it again by title, and read the Spec whole.
    #[test]
    fn the_specs_surface_can_be_read_back_without_agent_context() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "Pilotage agent",
                     "problem": "an agent cannot re-read", "solution": "three reads" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        for (title, phase, kind) in [
            ("slice one", "Foundations", "slice"),
            ("slice two", "Foundations", "slice"),
            ("a finding", "Wiring", "review"),
        ] {
            task_add(
                &db,
                &json!({ "project_id": project_id, "title": title, "spec_id": sid,
                         "phase": phase, "kind": kind, "severity": "low" }),
            )
            .unwrap_or_else(|e| panic!("{title}: {}", e.message));
        }

        // 1. FIND AN ID NOBODY KEPT — by title, from the project alone. And with NO project either:
        // "I do not know which project" is the state an agent looking for a lost id is really in.
        for scope in [json!({ "project_id": project_id }), json!({})] {
            let listed = spec_list(&db, &scope).unwrap();
            let found = listed.result["specs"]
                .as_array()
                .expect("specs")
                .iter()
                .find(|s| s["title"] == json!("Pilotage agent"))
                .unwrap_or_else(|| panic!("the Spec is listed under {scope}"));
            assert_eq!(found["id"], json!(sid));
            // …and the HUMAN key is there too, which is the other way to name it afterwards.
            assert!(
                found["key"]
                    .as_str()
                    .is_some_and(|k| k.ends_with(&format!("-{}", found["number"]))),
                "the row carries the KEY-N form: {found}"
            );
        }

        // 2. RE-READ IT WHOLE — the sections, the plan, and the tasks, in ONE call and with no role.
        let read = spec_get(&db, &json!({ "spec_id": sid })).unwrap().result;
        assert_eq!(read["spec"]["id"], json!(sid));
        assert_eq!(read["revision"]["problem"], json!("an agent cannot re-read"));
        assert_eq!(read["revision"]["solution"], json!("three reads"));
        assert_eq!(
            read["spec"]["phases"],
            json!([
                { "number": 1, "name": "Foundations" },
                { "number": 2, "name": "Wiring" },
            ])
        );
        assert_eq!(read["tasks_total"], json!(3));
        assert_eq!(read["tasks"].as_array().map(Vec::len), Some(3));
        assert_eq!(read["tasks_next_offset"], Value::Null, "nothing left to page");

        // 3. THE TASKS ANSWER ON THEIR OWN TOO, narrowed by the words the write path takes.
        let plan_only = task_list(&db, &json!({ "spec_id": sid, "kind": "slice" }))
            .unwrap()
            .result;
        assert_eq!(plan_only["total"], json!(2));
        let by_phase = task_list(&db, &json!({ "spec_id": sid, "phase": "Wiring" }))
            .unwrap()
            .result;
        assert_eq!(by_phase["total"], json!(1));
        assert_eq!(by_phase["tasks"][0]["phase_name"], json!("Wiring"));
        // A phase NAME the plan does not hold answers NOTHING, never everything.
        let ghost = task_list(&db, &json!({ "spec_id": sid, "phase": "Nowhere" }))
            .unwrap()
            .result;
        assert_eq!(ghost["total"], json!(0));

        // 4. AN UNKNOWN SPEC IS `invalid_id`, and the refusal names the read that finds one.
        for read in [
            spec_get(&db, &json!({ "spec_id": "01NOPE" })),
            task_list(&db, &json!({ "spec_id": "01NOPE" })),
        ] {
            let err = read.unwrap_err();
            assert_eq!(err.code, RpcCode::InvalidId, "{}", err.message);
            assert!(
                err.message.contains("spec_list"),
                "the refusal points at the read that finds a Spec: {}",
                err.message
            );
        }
    }

    /// PAGING IS A CONTRACT, not a hint: `next_offset` advances and ends on `null`, so a caller that
    /// follows it reads every row exactly once and cannot build a loop that never terminates.
    #[test]
    fn the_specs_reads_page_by_following_next_offset_until_it_is_null() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let spec = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let sid = spec_id_of(&spec);
        for i in 0..5 {
            task_add(
                &db,
                &json!({ "project_id": project_id, "title": format!("t{i}"), "spec_id": sid }),
            )
            .unwrap();
        }

        let mut offset = json!(0);
        let mut seen: Vec<String> = Vec::new();
        let mut pages = 0usize;
        while let Some(at) = offset.as_u64() {
            let page = task_list(&db, &json!({ "spec_id": sid, "offset": at, "limit": 2 }))
                .unwrap()
                .result;
            for task in page["tasks"].as_array().expect("tasks") {
                seen.push(task["id"].as_str().expect("an id").to_string());
            }
            offset = page["next_offset"].clone();
            pages += 1;
            assert!(pages < 10, "the paging must terminate");
        }
        assert_eq!(pages, 3, "5 rows at 2 per page: two full pages plus the tail");
        assert_eq!(seen.len(), 5, "every row is read exactly once: {seen:?}");
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 5, "…and none twice: {seen:?}");
    }

    /// A STRING THAT IS REALLY ENCODED JSON — an array or an object smuggled through a text field.
    /// That is the shape `"phases": "[]"` had: a consumer measuring the field got the length of the
    /// SOURCE (2), and `structuredContent` declared a type the payload did not have.
    fn is_encapsulated_json(text: &str) -> bool {
        let trimmed = text.trim();
        let bracketed = (trimmed.starts_with('[') && trimmed.ends_with(']'))
            || (trimmed.starts_with('{') && trimmed.ends_with('}'));
        bracketed
            && serde_json::from_str::<Value>(trimmed)
                .map(|v| v.is_array() || v.is_object())
                .unwrap_or(false)
    }

    /// Walk a tool result and collect the paths whose STRING value is encoded JSON.
    fn encapsulated_json_paths(value: &Value, path: &str, found: &mut Vec<String>) {
        match value {
            Value::String(text) if is_encapsulated_json(text) => {
                found.push(format!("{path} = {text}"))
            }
            Value::Object(map) => {
                for (key, child) in map {
                    encapsulated_json_paths(child, &format!("{path}.{key}"), found);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    encapsulated_json_paths(child, &format!("{path}[{i}]"), found);
                }
            }
            _ => {}
        }
    }

    /// DONE-CRITERION (task `01M1F8X6HGMHPT0V0743VAW50W`): **the Specs / tasks surface returns
    /// structure as structure — no field carries JSON encapsulated in a string.**
    ///
    /// The measured case: `spec_create` answered `"phases": "[]"`, so an agent testing the plan's
    /// length got 2 for an empty plan (2026-09-01). The fix is a WIRE PROJECTION ([`spec_json`]) —
    /// the `specs.phases` column stays TEXT — and this test is the sweep that says the defect had no
    /// siblings: every tool of the surface is called for real and its whole result tree is walked.
    #[test]
    fn no_specs_or_tasks_tool_returns_json_encapsulated_in_a_string() {
        let _g = db_guard();
        let (db, project_id, ws) = seeded_db();

        let mut results: Vec<(&str, Value)> = Vec::new();
        let created = spec_create(
            &db,
            &json!({ "project_id": project_id, "title": "S", "problem": "p", "solution": "s" }),
        )
        .unwrap();
        let sid = spec_id_of(&created);
        results.push((SPEC_CREATE_TOOL, created.result.clone()));

        // THE PLAN IS NON-EMPTY TOO: an empty plan would pass a projection that simply dropped the
        // field, so the sweep runs on a Spec that really carries phases.
        let task = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "spec_id": sid,
                     "phase": "Foundations" }),
        )
        .unwrap();
        let task_id = task.result["task"]["id"].as_str().unwrap().to_string();
        results.push((TASK_ADD_TOOL, task.result.clone()));
        let second = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "u", "spec_id": sid, "phase": "Wiring" }),
        )
        .unwrap();
        let second_id = second.result["task"]["id"].as_str().unwrap().to_string();

        results.push((
            SPEC_UPDATE_SECTION_TOOL,
            spec_update_section(
                &db,
                &json!({ "spec_id": sid, "section": "solution", "value": "s2" }),
            )
            .unwrap()
            .result,
        ));
        results.push((
            TASK_ADD_DEPENDENCY_TOOL,
            task_add_dependency(
                &db,
                &json!({ "task_id": second_id, "depends_on_task_id": task_id }),
            )
            .unwrap()
            .result,
        ));
        results.push((
            TASK_TRANSITION_TOOL,
            task_transition(&db, &json!({ "task_id": task_id, "transition": "start" }))
                .unwrap()
                .result,
        ));
        results.push((
            SPEC_FORK_TOOL,
            spec_fork(&db, &json!({ "spec_id": sid, "change_reason": "why" }))
                .unwrap()
                .result,
        ));
        results.push((
            SPEC_DEPS_TOOL,
            spec_deps(&db, &json!({ "spec_id": sid })).unwrap().result,
        ));

        // The ANCHOR path returns a Spec too — through the same projection.
        spec_transition(
            &db,
            &json!({ "spec_id": sid, "to": "ready", "user_confirmed": true }),
        )
        .unwrap();
        results.push((
            SPEC_SET_WORKSPACE_TOOL,
            spec_set_workspace(&db, &json!({ "spec_id": sid, "workspace_id": ws }))
                .unwrap()
                .result,
        ));

        // The READ half answers with the same shapes and must not re-introduce the defect.
        results.push((
            SPEC_GET_TOOL,
            spec_get(&db, &json!({ "spec_id": sid })).unwrap().result,
        ));
        results.push((
            SPEC_LIST_TOOL,
            spec_list(&db, &json!({ "project_id": project_id }))
                .unwrap()
                .result,
        ));
        results.push((
            TASK_LIST_TOOL,
            task_list(&db, &json!({ "spec_id": sid })).unwrap().result,
        ));

        for (tool, result) in &results {
            let mut found = Vec::new();
            encapsulated_json_paths(result, "result", &mut found);
            assert!(
                found.is_empty(),
                "tool `{tool}` returns JSON encapsulated in a string: {found:?}"
            );
        }

        // …and the specific field the defect was measured on IS an array, carrying the plan.
        let phases = &results[0].1["spec"]["phases"];
        assert!(phases.is_array(), "spec_create returns a phases ARRAY: {phases}");
        assert_eq!(phases.as_array().unwrap().len(), 0, "a fresh Spec has no phase yet");
        let read_back = &results
            .iter()
            .find(|(tool, _)| *tool == SPEC_GET_TOOL)
            .expect("spec_get was swept")
            .1["spec"]["phases"];
        assert_eq!(
            read_back,
            &json!([
                { "number": 1, "name": "Foundations" },
                { "number": 2, "name": "Wiring" },
            ]),
            "…and a sliced Spec carries its plan as {{number, name}} entries"
        );
    }

    /// DONE-CRITERION (task `01M1F8X5B1WJ40XY2QRJ5G4W17`): **`phase` takes a NAME, refuses anything
    /// else, and the response lets a caller re-read its own write.**
    ///
    /// The two traps, measured 2026-09-01 and reproduced here in the order they compound:
    ///   * `task_add(phase: 3)` was ACCEPTED and the field DROPPED — the task landed outside every
    ///     phase while the caller believed it had filed it;
    ///   * `task_add(phase: "3")` answered `phase: 2`, because "3" was the second NAME the Spec had
    ///     seen. Nothing in the response said the number was a RANK, so re-reading one's own write
    ///     looked exactly like a corruption.
    #[test]
    fn task_add_phase_refuses_a_number_and_echoes_the_name_beside_the_rank() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let spec = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let spec_id = spec_id_of(&spec);

        // TRAP ONE — an integer is REFUSED, and the refusal teaches the field rather than just the
        // type: it says a NAME is expected and states how nyx numbers them.
        let err = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "spec_id": spec_id, "phase": 3 }),
        )
        .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidArgument, "{}", err.message);
        assert!(
            err.message.contains("NAME") && err.message.contains("first seen"),
            "the refusal names the expected type AND the first-appearance rule: {}",
            err.message
        );
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some("phase_must_be_a_name")
        );
        // …and nothing was written: the Spec's plan is still empty.
        let plan = db
            .with_conn(|c| db::get_spec(c, &spec_id))
            .unwrap()
            .expect("the spec");
        assert_eq!(db::parse_phase_plan(&plan.phases), Vec::new());

        // TRAP TWO — a NAME that CARRIES a number. It is the SECOND name this Spec sees, so the rank
        // is 2 while the name says 3: the response carries both, unambiguously. (A name that is
        // NOTHING BUT digits is a third trap, refused outright — see
        // `task_add_refuses_a_phase_name_that_is_indistinguishable_from_a_rank`.)
        for (name, expected_rank) in [("Foundations", 1), ("Phase 3", 2)] {
            let out = task_add(
                &db,
                &json!({ "project_id": project_id, "title": "t", "spec_id": spec_id,
                         "phase": name }),
            )
            .unwrap_or_else(|e| panic!("phase '{name}' must be accepted: {}", e.message));
            assert_eq!(
                out.result["task"]["phase"],
                json!(expected_rank),
                "`task.phase` is the RANK nyx assigned"
            );
            assert_eq!(
                out.result["task"]["phase_name"],
                json!(name),
                "…and `phase_name` echoes the NAME that was sent, so the two are readable apart"
            );
        }

        // A task with NO phase says so plainly on both fields.
        let flat = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "flat", "spec_id": spec_id }),
        )
        .unwrap();
        assert_eq!(flat.result["task"]["phase"], Value::Null);
        assert_eq!(flat.result["task"]["phase_name"], Value::Null);

        // THE SAME PAIR SURVIVES A RE-READ, which is what "re-read your own write" means: the read
        // half answers with the very fields the write did.
        let listed = task_list(&db, &json!({ "spec_id": spec_id })).unwrap();
        let phased: Vec<(&Value, &Value)> = listed.result["tasks"]
            .as_array()
            .expect("tasks")
            .iter()
            .map(|t| (&t["phase"], &t["phase_name"]))
            .collect();
        assert!(
            phased.contains(&(&json!(1), &json!("Foundations")))
                && phased.contains(&(&json!(2), &json!("Phase 3"))),
            "task_list carries the same rank↔name pairing task_add answered with: {phased:?}"
        );
    }

    /// A5 — A PHASE NAME MADE ONLY OF DIGITS IS REFUSED, AND ONE THAT MERELY CONTAINS DIGITS IS NOT.
    ///
    /// Refusing the INTEGER `2` while accepting the STRING `"2"` closed the type and left the
    /// misunderstanding intact: a caller told "it must be a string" quotes the number and gets a
    /// phase literally NAMED `2`, whose rank is whatever position it arrived in. Observed in stored
    /// plans — `["Socle", "2"]`, and Specs whose phases are named `1`, `3`, `2` in that order — which
    /// is exactly the rank/name ambiguity `phase_name` was introduced to close, coming back in
    /// through the write.
    ///
    /// The line is drawn at "indistinguishable from a rank", not at "contains a digit": "Phase 2"
    /// and "v2 socle" are names a human wrote and stay accepted. And READING is untouched — the
    /// already-corrupted plans this review leaves alone must still be filterable.
    #[test]
    fn task_add_refuses_a_phase_name_that_is_indistinguishable_from_a_rank() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let spec = spec_create(&db, &json!({ "project_id": project_id, "title": "S" })).unwrap();
        let spec_id = spec_id_of(&spec);

        // REFUSED: nothing but digits, whitespace included.
        for numeric in ["2", "1", "10", " 3 "] {
            let err = task_add(
                &db,
                &json!({ "project_id": project_id, "title": "t", "spec_id": spec_id,
                         "phase": numeric }),
            )
            .err()
            .unwrap_or_else(|| panic!("phase {numeric:?} must be refused"));
            assert_eq!(err.code, RpcCode::InvalidArgument);
            assert!(
                err.message.contains("collides with the RANK"),
                "the refusal explains the name/rank collision: {}",
                err.message
            );
            assert!(
                err.message.contains("nyx assigns the rank itself"),
                "…and recalls that the rank is nyx's to assign: {}",
                err.message
            );
            assert_eq!(
                err.data
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str),
                Some("phase_name_collides_with_rank")
            );
        }
        // Nothing was written: the plan is still empty.
        let plan = db
            .with_conn(|c| db::get_spec(c, &spec_id))
            .unwrap()
            .expect("the spec");
        assert_eq!(db::parse_phase_plan(&plan.phases), Vec::new());

        // ACCEPTED: a name that merely CONTAINS a digit is a name.
        for (name, rank) in [("Phase 2", 1), ("v2 socle", 2), ("2e passe", 3)] {
            let out = task_add(
                &db,
                &json!({ "project_id": project_id, "title": "t", "spec_id": spec_id,
                         "phase": name }),
            )
            .unwrap_or_else(|e| panic!("phase {name:?} must be accepted: {}", e.message));
            assert_eq!(out.result["task"]["phase"], json!(rank));
            assert_eq!(out.result["task"]["phase_name"], json!(name));
        }

        // The INTEGER refusal delivered in phase 3 of this PRD is untouched, with its own reason.
        let int_err = task_add(
            &db,
            &json!({ "project_id": project_id, "title": "t", "spec_id": spec_id, "phase": 2 }),
        )
        .expect_err("an integer is still refused");
        assert_eq!(
            int_err
                .data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some("phase_must_be_a_name")
        );

        // READING is untouched: a plan already holding a numeric name stays filterable. Seeded
        // through the DB, since the tool now refuses to CREATE one.
        db.with_conn(|c| db::set_spec_phases(c, &spec_id, r#"["Socle","2"]"#))
            .expect("seed a corrupted plan the way the audit found it");
        assert!(
            task_list(&db, &json!({ "spec_id": spec_id, "phase": "2" })).is_ok(),
            "filtering an already-corrupted plan must keep working — this review does not repair \
             stored plans, so it must not make them unreadable either"
        );
    }

    /// THE ADVERTISED SET IS THE ENFORCED SET, moored to the LIVE SCHEMA rather than to the
    /// migration's text: every value of each `db::TASK_*` slice is pushed through a real
    /// `task_add`, so a vocabulary that drifted from its column's `CHECK` goes red here instead of
    /// coming back as an `internal` on a caller that believed the description.
    #[test]
    fn every_advertised_task_vocabulary_value_is_one_the_schema_accepts() {
        let _g = db_guard();
        let (db, project_id, _ws) = seeded_db();
        let mut written = 0usize;
        for (key, accepted) in [
            ("kind", &db::TASK_KINDS[..]),
            ("type", &db::TASK_TYPES[..]),
            ("priority", &db::TASK_PRIORITIES[..]),
            ("severity", &db::TASK_SEVERITIES[..]),
        ] {
            for value in accepted {
                let mut args = json!({ "project_id": project_id, "title": "t", key: value });
                // A `kind=human` task is verifiable by construction — the only extra the write
                // path asks for, and not what this test is about.
                if key == "kind" && *value == db::TASK_KIND_HUMAN {
                    args["verification_command"] = json!("check it");
                }
                let out = task_add(&db, &args)
                    .unwrap_or_else(|e| panic!("{key}='{value}' must be accepted: {}", e.message));
                // `type` is `type_` in the serialized row (`type` is a Rust keyword).
                let read_back = if key == "type" { "type_" } else { key };
                assert_eq!(out.result["task"][read_back], json!(value));
                written += 1;
            }
        }
        assert_eq!(
            written,
            db::TASK_KINDS.len()
                + db::TASK_TYPES.len()
                + db::TASK_PRIORITIES.len()
                + db::TASK_SEVERITIES.len(),
            "every value of every vocabulary was exercised"
        );
    }
}
