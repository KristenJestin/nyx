//! Shell-agnostic MCP **graph-authoring tools** — the agent builds an orchestration behavior tree
//! from raw intent; the human validates it (via the Phase-6 visualiser). v2.8 — Harnais : moteur
//! d'orchestration, Phase 4, task `01KY71J2GF83Z2ATBRA5125FPV`.
//!
//! # What this layer IS (and is not)
//!
//! The LOW-LEVEL row CRUD already exists in [`crate::db`] (Phase 2: `create_workflow` /
//! `create_node` / `list_nodes` / `update_node` / `set_node_position` / …). This module is the MCP
//! COUCHE on top: the authoring verbs plus — the crux — a TREE-COHERENCE guard that refuses to
//! commit a mutation that would leave an incoherent graph. It never re-implements the CRUD.
//!
//!   * `workflow_create` — a fresh (empty) workflow draft, OWNED by a project (unowned is allowed
//!     ONLY for a `custom` template; an unowned seeded scope ndev/nspec/ndoc is refused — S2).
//!   * `workflow_fork` — a DEEP COPY of an existing workflow's whole tree into a new PROJECT-OWNED
//!     workflow (`forked_from` = the source; `project_id` REQUIRED). The customisation path: fork a
//!     built-in default, then edit the copy — the shared built-in is never touched. A project holds
//!     ONE workflow per scoped family, so a second ndev/nspec/ndoc fork is refused (S3).
//!   * `workflow_assign` — (re)BIND a workflow to a project (`workflows.project_id`).
//!   * `slot_bind` — bind a graph's command SLOT to this project's managed commands. The gesture that
//!     REPLACES forking a built-in to arm its gate: a shared graph's `gate_check` declares WHAT it
//!     needs verified (`config.slot`), the project says WITH WHAT, and the built-in — untouched —
//!     keeps receiving its upgrades. A fork froze the graph at that day's revision forever.
//!   * `node_add` / `node_update` / `node_remove` / `node_reorder` — the node CRUD that RE-BUILDS
//!     the tree, each validated before it commits (config + coherence).
//!   * `workflow_list` / `workflow_get` / `run_list` — the READ surface (S1): discover workflows,
//!     read a graph's node ids, list a workflow's runs.
//!   * `workflow_delete` — delete a project-owned workflow + its nodes/runs (S4).
//!
//! A SEEDED built-in default (`project_id IS NULL` + a seeded scope) is IMMUTABLE to every mutation
//! here ([`require_mutable_workflow`]) — it is customised by forking, never edited in place (S2).
//!
//! # A graph is not edited under a run that is walking it (M4)
//!
//! The tree is DATA a run walks by node id and sibling index, so a STRUCTURAL edit — `node_add`,
//! `node_remove`, `node_reorder`, and `workflow_delete`'s cascade — either strands the run's
//! persisted traversal (its progress is lost the next time `run_start` looks at it) or slides it onto
//! the wrong child. Both used to happen without a word, so those four are REFUSED while a run is
//! walking the workflow ([`require_no_walking_run`]), naming the run and the two ways out: let it
//! finish, or `run_delete` it. `node_update` is the exception — it moves no node id, so the walk
//! survives — and merely WARNS ([`walking_run_warnings`]); refusing it would contradict the N26(a)
//! `gate_unconfigured` warning, which asks for exactly that edit while a run is live.
//!
//! ## The `assign` / `projects.workflow_id` decision
//!
//! The task criterion says create/fork/assign "posent `projects.workflow_id`". Phase 2 modelled the
//! project↔workflow link as its INVERSE — `workflows.project_id` (NULL = a built-in nyx default,
//! non-NULL = a project's own copy) — because a project uses SEVERAL scoped flows (ndev + nspec +
//! ndoc), so ownership cannot be one scalar pointer on the project; it lives on each workflow, and
//! "which workflow does project P use for scope S" resolves as `owned(P, S)` preferring the built-in
//! `NULL` default (exactly what [`crate::db::list_workflows`] already does). So `assign` sets
//! `workflows.project_id` ([`crate::db::set_workflow_project`]); no `projects.workflow_id` column is
//! added — it would be redundant AND unable to express the per-scope reality.
//!
//! # Coherence (the "refus d'un arbre incohérent")
//!
//! Every mutating verb runs inside ONE transaction: it performs the row write, then re-reads the
//! whole workflow and [`validate_tree`]s it; a violation returns an error that ROLLS THE MUTATION
//! BACK (nothing is persisted). The rules: the interpreter's own [`crate::engine::Tree::from_rows`]
//! (unknown node type, non-object config, dangling parent, missing/duplicate root) PLUS a leaf may
//! not have children PLUS no cycle / orphan in the parent chain (a disconnected cycle that coexists
//! with a valid root slips past `from_rows`, so the parent-chain walk closes it).
//!
//! # Effects
//!
//! Authoring mutates the orchestration graph, so each verb reports [`ChangedTopic::Workflows`]. The
//! front's re-pull on it is wired in Phase 6 (the React Flow visualiser); the topic is NAMED here so
//! the seam is single-authority end to end (the same forward-declared pattern as `Specs` / `Ideas`).

use std::collections::{HashMap, HashSet};

use diesel::Connection;
use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::engine::{board_key_root, NodeType, SensitiveEffect, Tree};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, optional_str, require_str};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

// --- Tool name constants (advertised in `tools/list`, dispatched below) ------

/// `workflow_create` — create a fresh (empty) workflow draft.
pub const WORKFLOW_CREATE_TOOL: &str = "workflow_create";
/// `workflow_fork` — deep-copy an existing workflow's tree into a new workflow.
pub const WORKFLOW_FORK_TOOL: &str = "workflow_fork";
/// `workflow_assign` — (re)bind a workflow to a project.
pub const WORKFLOW_ASSIGN_TOOL: &str = "workflow_assign";
/// `slot_bind` — bind a graph's command SLOT to this project's managed commands.
pub const SLOT_BIND_TOOL: &str = "slot_bind";
/// `node_add` — add a typed node to a workflow's tree.
pub const NODE_ADD_TOOL: &str = "node_add";
/// `node_update` — edit a node's `label` / `config`.
pub const NODE_UPDATE_TOOL: &str = "node_update";
/// `node_remove` — delete a node (and its subtree).
pub const NODE_REMOVE_TOOL: &str = "node_remove";
/// `node_reorder` — move a node (sibling order and/or reparent).
pub const NODE_REORDER_TOOL: &str = "node_reorder";
/// `workflow_list` — list workflows (built-ins + a project's own). READ (review S1).
pub const WORKFLOW_LIST_TOOL: &str = "workflow_list";
/// `workflow_get` — a workflow's metadata + its whole node tree. READ (review S1).
pub const WORKFLOW_GET_TOOL: &str = "workflow_get";
/// `run_list` — a workflow's runs (cursor + blackboard + status). READ (review S1).
pub const RUN_LIST_TOOL: &str = "run_list";
/// `workflow_delete` — delete a PROJECT-OWNED workflow (+ its nodes/runs). MUTATION (review S4).
pub const WORKFLOW_DELETE_TOOL: &str = "workflow_delete";
/// `run_delete` — delete ONE workflow run by id. MUTATION (review N5).
pub const RUN_DELETE_TOOL: &str = "run_delete";

/// The authoring tool names — advertised in `tools/list` and routed by [`dispatch_authoring_tool`].
/// A closed slice so a new tool is added in exactly one place. Seven MUTATIONS + the v2.8-review
/// additions (three READS `workflow_list`/`workflow_get`/`run_list` + the `workflow_delete` /
/// `run_delete` mutations) + `slot_bind`, the per-project half of a SHARED graph's gate.
pub const AUTHORING_TOOLS: [&str; 13] = [
    WORKFLOW_CREATE_TOOL,
    WORKFLOW_FORK_TOOL,
    WORKFLOW_ASSIGN_TOOL,
    SLOT_BIND_TOOL,
    NODE_ADD_TOOL,
    NODE_UPDATE_TOOL,
    NODE_REMOVE_TOOL,
    NODE_REORDER_TOOL,
    WORKFLOW_LIST_TOOL,
    WORKFLOW_GET_TOOL,
    RUN_LIST_TOOL,
    WORKFLOW_DELETE_TOOL,
    RUN_DELETE_TOOL,
];

// --- Transaction error plumbing --------------------------------------------

/// The error a mutating verb's transaction can fail with: a raw DB error (mapped to `internal`) or
/// a caller-facing [`RpcError`] (an unknown id, or a coherence violation). Implementing
/// `From<diesel::result::Error>` lets `?` on a `QueryResult` roll the transaction back.
enum TxErr {
    Db(diesel::result::Error),
    Rpc(RpcError),
}

impl From<diesel::result::Error> for TxErr {
    fn from(e: diesel::result::Error) -> Self {
        TxErr::Db(e)
    }
}

fn map_tx_err(e: TxErr) -> RpcError {
    match e {
        TxErr::Db(e) => internal_db(e),
        TxErr::Rpc(r) => r,
    }
}

fn unknown_workflow(id: &str) -> RpcError {
    RpcError::new(RpcCode::InvalidId, format!("unknown workflow {id}"))
}

fn unknown_node(id: &str) -> RpcError {
    RpcError::new(RpcCode::InvalidId, format!("unknown node {id}"))
}

/// The `invalid_state` refusal an incoherent-tree violation maps to (the "refus d'un arbre
/// incohérent" criterion).
fn incoherent(msg: impl Into<String>) -> RpcError {
    RpcError::new(
        RpcCode::InvalidState,
        format!("incoherent workflow tree: {}", msg.into()),
    )
    .with_data(json!({ "reason": "incoherent_tree" }))
}

/// Serialize a node for the AGENT-facing MCP contract with `type` — NOT the Diesel column name
/// `type_`. The write verbs take `type` as input, so the read/response must echo `type`; the
/// persistence field name must not leak into the tool contract (v2.8 review m1). The UI bridge keeps
/// its own `type_` serialization of [`db::Node`] (the renderer reads that), so this is scoped to the
/// agent surface only.
fn node_json(node: &db::Node) -> Value {
    let mut v = serde_json::to_value(node).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        if let Some(t) = obj.remove("type_") {
            obj.insert("type".to_string(), t);
        }
    }
    v
}

/// The same `type_`→`type` mapping over a list of nodes (a workflow's tree in a read response).
fn nodes_json(nodes: &[db::Node]) -> Value {
    Value::Array(nodes.iter().map(node_json).collect())
}

/// Whether a workflow is a PROTECTED BUILT-IN default: UNOWNED (`project_id IS NULL`) AND of a
/// SEEDED scope (ndev/nspec/ndoc). These are the shared nyx defaults `ensure_builtin` maintains and
/// every project falls back to. An unowned `custom` workflow is a user TEMPLATE/draft, not a
/// protected default, so it stays mutable.
///
/// Shared with [`crate::mcp_engine`]: an unconfigured-gate refusal must tell the caller the route that
/// actually exists, and for a gate living in a built-in that route goes through `workflow_fork` — the
/// `node_update` it would otherwise name is refused right here.
pub(crate) fn is_builtin_default(wf: &db::Workflow) -> bool {
    // `forked_from IS NULL` is what tells a SEEDED default apart from a stray unowned FORK of a
    // seeded scope (a `project_id NULL` fork left over from before the fork guard): the latter is
    // NOT a protected built-in, so it stays mutable/deletable and can be cleaned up (v2.8 review N4).
    wf.project_id.is_none() && wf.scope != db::WORKFLOW_SCOPE_CUSTOM && wf.forked_from.is_none()
}

/// Load a workflow and REFUSE the call if it is a PROTECTED BUILT-IN default ([`is_builtin_default`]):
/// mutating one would corrupt the shared default for EVERY project and survive every reboot
/// (`ensure_builtin` is idempotent by scope, it does not self-heal a mutated tree). Point the caller
/// at `workflow_fork` instead — fork the default, edit the project-owned copy (v2.8 review S2).
/// Returns the workflow when it IS mutable.
fn require_mutable_workflow(
    tc: &mut diesel::SqliteConnection,
    workflow_id: &str,
) -> Result<db::Workflow, TxErr> {
    let wf =
        db::get_workflow(tc, workflow_id)?.ok_or_else(|| TxErr::Rpc(unknown_workflow(workflow_id)))?;
    if is_builtin_default(&wf) {
        return Err(TxErr::Rpc(
            RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "workflow {workflow_id} is a SEEDED built-in default ('{}', project_id is null) \
                     and is immutable — fork it with workflow_fork and edit the project-owned copy",
                    wf.scope
                ),
            )
            .with_data(json!({ "reason": "builtin_immutable" })),
        ));
    }
    Ok(wf)
}

/// The machine marker of the M4 live-run guard — the `reason` of the REFUSAL and the `code` of the
/// node_update WARNING, deliberately the same string: both say "a run is walking this workflow".
const LIVE_RUN_MARKER: &str = "workflow_has_live_run";

/// REFUSE a STRUCTURAL edit of a workflow whose tree a run is currently WALKING (M4) — the same
/// "confront rather than silently proceed" posture [`crate::mcp_context::resolve_run_target`] takes
/// on a disagreeing anchor.
///
/// A run's persisted `traversal` is keyed by node ID and its frames carry SIBLING INDICES, so adding,
/// removing, reordering or reparenting a node under a live run does one of two things, both silent:
/// the walk no longer restores at all (the run is dead — `run_start` drops it and its progress is
/// gone), or it restores against shifted siblings and the run resumes on the WRONG child. Neither is
/// something to discover afterwards, so the mutation is refused and the caller is told the two ways
/// out: finish the run, or `run_delete` it. Only a run that has actually walked counts
/// ([`db::walking_runs_for_workflow`]): a live run that has never ticked simply starts from the root
/// of the new tree, losing nothing. `gesture` names the refused move, in the message.
fn require_no_walking_run(
    tc: &mut diesel::SqliteConnection,
    workflow_id: &str,
    gesture: &str,
) -> Result<(), TxErr> {
    let Some(run) = db::walking_runs_for_workflow(tc, workflow_id)?.into_iter().next() else {
        return Ok(());
    };
    Err(TxErr::Rpc(
        RpcError::new(
            RpcCode::InvalidState,
            format!(
                "workflow {workflow_id} has a LIVE run ({}, {}) already walking its tree — {gesture} \
                 would reshape the graph under it and destroy its progress (its recorded walk would \
                 point at nodes that no longer exist, or resume on the wrong child). Let that run \
                 finish, or drop it with run_delete run_id={}, then edit the graph",
                run.id, run.status, run.id
            ),
        )
        .with_data(json!({ "reason": LIVE_RUN_MARKER, "run_id": run.id })),
    ))
}

/// The M4 WARNINGS for an edit that keeps every node ID — today only `node_update` (label / config).
/// Such an edit is NOT refused: the traversal restores exactly as before, so the run keeps its
/// progress, and refusing would contradict nyx's own advice — the N26(a) `gate_unconfigured` warning
/// tells the caller to `node_update` a gate's `command_ids`, and it is raised by `run_start`, i.e.
/// precisely when a run is live. But the run WILL execute the new config from its next tick, mid
/// flight, so it is said out loud. Same `warnings` shape as `run_start` (`{code, …, message}`).
fn walking_run_warnings(
    tc: &mut diesel::SqliteConnection,
    workflow_id: &str,
) -> Result<Vec<Value>, TxErr> {
    Ok(db::walking_runs_for_workflow(tc, workflow_id)?
        .into_iter()
        .map(|run| {
            json!({
                "code": LIVE_RUN_MARKER,
                "run_id": run.id,
                "message": format!(
                    "run {} ({}) is walking this workflow right now: it keeps its progress (no node \
                     id moved), but it will execute the edited node from its very next tick — check \
                     that is what you want",
                    run.id, run.status
                ),
            })
        })
        .collect())
}

/// Refuse a SECOND workflow of a SCOPED family (ndev/nspec/ndoc) in a project — the invariant that
/// keeps `run_start`'s resolution unambiguous (ONE workflow per scope per project). `custom` is
/// freeform (many allowed). `exclude` skips a workflow id (e.g. a re-home of a workflow already in
/// the project). Enforced at EVERY entry that can add a scoped workflow to a project — create, fork,
/// assign — not just fork, so the invariant is real, not theatre (v2.8 review S3/L-d). Runs inside
/// the caller's transaction.
fn require_scope_available(
    tc: &mut diesel::SqliteConnection,
    project_id: &str,
    scope: &str,
    exclude: Option<&str>,
) -> Result<(), TxErr> {
    if scope == db::WORKFLOW_SCOPE_CUSTOM {
        return Ok(());
    }
    if let Some(existing) = db::list_workflows(tc, Some(project_id))?
        .into_iter()
        .find(|w| w.scope == scope && Some(w.id.as_str()) != exclude)
    {
        return Err(TxErr::Rpc(
            RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "project already owns a '{scope}' workflow ({}) — a project has ONE workflow per \
                     scope; edit it, or workflow_delete it, before adding another",
                    existing.id
                ),
            )
            .with_data(json!({ "reason": "scope_already_owned" })),
        ));
    }
    Ok(())
}

/// The KNOWN config fields of each node type — the UNION of every struct that decodes it, so a
/// judge/ask_human (decoded as `JudgeConfig` AND `OutputConfig`) lists both its prompt fields and
/// the `output_key`/`append` output-routing. The single source for the "no unknown field" check
/// (v2.8 review N10a). Serde `deny_unknown_fields` cannot be used: the multi-decode makes every
/// field "unknown" to the OTHER struct, so it would reject valid N9 configs.
pub(crate) fn known_config_fields(node_type: &str) -> &'static [&'static str] {
    match node_type {
        db::NODE_TYPE_SPAWN_SUBAGENT => &["agent_id", "inline", "subject"],
        db::NODE_TYPE_GATE_CHECK => &["command_ids", "slot", "blocking", "expect"],
        db::NODE_TYPE_JUDGE => &["prompt", "output_schema", "output_key", "append"],
        db::NODE_TYPE_ASK_HUMAN => &[
            "prompt",
            "output_key",
            "append",
            "options",
            "grant_spec_write_on",
        ],
        db::NODE_TYPE_PRODUCE => &["output_key", "append"],
        db::NODE_TYPE_TRANSITION => &["target_state", "gate_on"],
        db::NODE_TYPE_QUERY => &[
            "query", "write", "target", "task_key", "root_key", "name_key",
        ],
        db::NODE_TYPE_FORK_SPEC => &["gate_on"],
        db::NODE_TYPE_TRANSITION_TASK => &["transition", "task_key", "reason", "gate_on"],
        db::NODE_TYPE_SUB_WORKFLOW => &["scope", "workflow_id", "inject", "retrieve", "output_key"],
        // create_workspace — exactly the fields [`crate::engine::CreateWorkspaceConfig`] decodes, so a fork of
        // /nspec can be re-authored through node_add/node_update like every other type. Without
        // this arm the type fell to the catch-all below, which MEANS "takes no config" — and the
        // node is inexecutable without one.
        db::NODE_TYPE_CREATE_WORKSPACE => &["root_key", "name_key", "gate_on"],
        // set_category — exactly the fields [`crate::engine::SetCategoryConfig`] decodes, for the
        // same reason `create_workspace` has its own arm: the catch-all below MEANS "takes no
        // config", and this node is inexecutable without one.
        db::NODE_TYPE_SET_CATEGORY => &["category_key", "gate_on"],
        db::NODE_TYPE_NOTE => &["text", "clear_gate_block", "revoke_spec_write"],
        db::NODE_TYPE_LOOP => &["until", "max_iterations"],
        db::NODE_TYPE_SELECTOR => &["branches"],
        // sequence / wait take no config.
        _ => &[],
    }
}

/// Validate a node's `config` against its TYPE at AUTHORING time. The runtime configs are LENIENT by
/// design (a missing field defaults so a partial tree still walks) — which is exactly how an
/// inexecutable action shipped silently (`spawn` with `agent_id:""`, `transition` to `"banane"`,
/// `judge` with no prompt — v2.8 review M1/M2). So the WRITE side enforces the required fields the
/// runtime leaves optional. Deliberately narrow: only the fields whose absence makes the action
/// inexecutable — an empty `gate_check` (vacuously green) and an empty `note` (a no-op trap) stay
/// valid authored states.
fn validate_node_config(node_type: &str, config: &Value) -> Result<(), RpcError> {
    // Reject UNKNOWN fields — the complement to the required-field checks below: a typo (the review's
    // `accumulate` instead of `append`) used to be accepted and SILENTLY ignored at runtime, so the
    // graph behaved differently with no signal. A typo must be an authoring ERROR (v2.8 review N10a).
    if let Some(obj) = config.as_object() {
        let known = known_config_fields(node_type);
        if let Some(bad) = obj.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "unknown config field '{bad}' for a '{node_type}' node (accepted: {})",
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                ),
            ));
        }
    }
    // The board keys this node WRITES must be keys the board can actually hold, and must not be the
    // engine's own (H12) — see [`validate_board_write_key`].
    if let Some(obj) = config.as_object() {
        for field in ["write", "output_key"] {
            if let Some(key) = obj.get(field).and_then(Value::as_str) {
                validate_board_write_key(&format!("'{field}'"), key)?;
            }
        }
    }
    let required = |field: &str| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("a '{node_type}' node needs a non-empty '{field}' in its config"),
        )
    };
    let non_empty = |field: &str| {
        config
            .get(field)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    };
    match node_type {
        db::NODE_TYPE_SPAWN_SUBAGENT if !non_empty("agent_id") => Err(required("agent_id")),
        db::NODE_TYPE_JUDGE if !non_empty("prompt") => Err(required("prompt")),
        // A gate names its commands ONE way: its own `command_ids`, or a `slot` the project binds.
        // Setting both is not a merge, it is a QUESTION with two answers — the runtime resolves the
        // slot and the ids beside it would sit there looking authoritative while running nothing. An
        // empty `command_ids` alongside a slot is the ordinary shape (the field simply defaults), so
        // only a NON-EMPTY one conflicts.
        db::NODE_TYPE_GATE_CHECK
            if non_empty("slot")
                && config
                    .get("command_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| !ids.is_empty()) =>
        {
            Err(RpcError::new(
                RpcCode::InvalidArgument,
                "a 'gate_check' node arms itself EITHER with its own 'command_ids' (a graph that owns \
                 its commands) OR with a 'slot' each project binds (a graph SHARED by every project) \
                 — never both: only the slot would be run, and the ids beside it would read as the \
                 truth while doing nothing",
            ))
        }
        // A DECLARED `expect` must be one of the two words the runtime grades against. The runtime
        // is lenient by design and would fail to DECODE the config, which parks the run on the node
        // with a decode error at the moment the gate should have run — the authoring side is where
        // "expect: raed" has to be a typo and not a wedged flow. Absent is valid and means `green`,
        // so only a PRESENT value is checked.
        db::NODE_TYPE_GATE_CHECK
            if config.get("expect").is_some_and(|v| {
                serde_json::from_value::<crate::engine::GateExpectation>(v.clone()).is_err()
            }) =>
        {
            Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "a 'gate_check' node's 'expect' must be \"{}\" (the check must PASS — the \
                     default when the field is absent) or \"{}\" (the check must FAIL, which is how \
                     a reproduction is declared), got {}",
                    db::GATE_EXPECT_GREEN,
                    db::GATE_EXPECT_RED,
                    config.get("expect").unwrap_or(&Value::Null)
                ),
            ))
        }
        db::NODE_TYPE_TRANSITION => {
            let target = config.get("target_state").and_then(Value::as_str).unwrap_or("");
            if target.trim().is_empty() {
                Err(required("target_state"))
            } else if !crate::mcp_engine::valid_transition_target(target) {
                Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!(
                        "transition target_state '{target}' is not a valid Spec state or verb \
                         (in_review/review, in_progress/start, ready, completed/done, cancelled)"
                    ),
                ))
            } else if crate::mcp_engine::transition_is_engaging_target(target)
                && config
                    .get("gate_on")
                    .and_then(Value::as_str)
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true)
            {
                // An ENGAGING transition (ready/done/cancel) is a COMMITMENT — a graph may impose it
                // only behind a human approval. It needs a `gate_on` naming the blackboard key an
                // ask_human approves (B3); WHICH ask_human is allowed to approve it — one that
                // necessarily runs before it, on the branch that reaches it — is the SHAPE
                // [`sensitive_node_gate`] enforces. (Entry `start` and `in_review` need no gate.)
                Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!(
                        "transition target_state '{target}' is ENGAGING (ready/done/cancel): it needs \
                         a 'gate_on' naming the blackboard key a preceding ask_human approves"
                    ),
                ))
            } else {
                Ok(())
            }
        }
        db::NODE_TYPE_FORK_SPEC if !non_empty("gate_on") => {
            // A fork is a sensitive Spec mutation: it needs a `gate_on` naming the blackboard key an
            // ask_human approves (WHICH ask_human may approve it is the SHAPE [`sensitive_node_gate`]
            // checks), so nyx never forks implicitly (v2.8 review N13).
            Err(RpcError::new(
                RpcCode::InvalidArgument,
                "a 'fork_spec' node needs a 'gate_on' naming the blackboard key a preceding ask_human \
                 approves — a fork is never implicit",
            ))
        }
        db::NODE_TYPE_TRANSITION_TASK => {
            // The runtime leaves `transition` optional (a partial tree still walks); the WRITE side
            // requires a VALID task lifecycle move, so a mis-typed verb is an authoring error rather
            // than a runtime BLOCKED (v2.8 lot 2). `task_key`/`reason`/`gate_on` stay optional.
            let transition = config.get("transition").and_then(Value::as_str).unwrap_or("");
            if transition.trim().is_empty() {
                Err(required("transition"))
            } else if !crate::mcp_engine::valid_task_transition(transition) {
                Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    format!(
                        "transition_task transition '{transition}' is not a task transition \
                         (start, done, block, skip, verify)"
                    ),
                ))
            } else {
                Ok(())
            }
        }
        db::NODE_TYPE_CREATE_WORKSPACE if !non_empty("root_key") => {
            // create_workspace — the folder must RESOLVE from the node's ONE declared source, NAMED.
            //
            // The RUNTIME would not strictly force it: `CreateWorkspaceConfig` defaults `root_key` to
            // `None` and the engine then reads [`crate::engine::DEFAULT_WORKTREES_ROOT_KEY`], so a
            // config-less node runs whenever the board happens to carry `worktrees_root`. THAT is the
            // reason the WRITE side is stricter, not laxer: "wherever the board happens to hold a root"
            // is not a decision a graph should take implicitly — the one thing this node does is say
            // WHERE, and a node that says it by omission says it to whatever a sibling last wrote.
            // `name_key`/`gate_on` stay optional (the name falls back to the Spec's, and an absent gate
            // means UNGATED). There is no second "explicit path" source to fall back on any more, and
            // that removal is the point: a node that took a whole path took it AS the workspace, so the
            // same value meant "the folder to create" here and "the folder to create it UNDER" there.
            Err(RpcError::new(
                RpcCode::InvalidArgument,
                "a 'create_workspace' node needs a 'root_key' — the blackboard key holding the ROOT \
                 its folder is created under (a `query project_worktrees_root`, or an ask_human that \
                 asked for one). Without it there is no folder to derive",
            ))
        }
        db::NODE_TYPE_SET_CATEGORY if !non_empty("category_key") => {
            // set_category — the category must RESOLVE from the node's ONE declared source, NAMED.
            //
            // The RUNTIME would not strictly force it: `SetCategoryConfig` defaults `category_key` to
            // `None` and the engine then reads [`crate::engine::DEFAULT_CATEGORY_KEY`], so a
            // config-less node classifies whenever the board happens to carry `category`. THAT is why
            // the WRITE side is stricter, not laxer — the same reading as `create_workspace`'s
            // `root_key`: the one thing this node does is say WHICH category, and a node that says it
            // by omission says it to whatever a sibling last wrote. Under this PRD that is not
            // cosmetic: the `bug` discipline is read off the slug this node lands, so a Spec
            // classified from a stray board key is a Spec put under (or out of) a red-before-green
            // gate by accident. `gate_on` stays optional (an absent gate means UNGATED).
            Err(RpcError::new(
                RpcCode::InvalidArgument,
                "a 'set_category' node needs a 'category_key' — the blackboard key holding the \
                 category LABEL it assigns (a slug like `bug`, or a human name; a `judge` verdict or \
                 a `query spec_category` reading fills it). Without it there is no category to assign",
            ))
        }
        db::NODE_TYPE_SUB_WORKFLOW => {
            // The child target must RESOLVE: a non-empty `scope`, or an explicit `workflow_id` override
            // (else the runtime has nothing to open). `inject`/`retrieve`, when present, must be arrays
            // of well-formed `{from, to}` copies — a mis-shaped map would silently copy nothing at
            // runtime (v2.8 lot 2).
            let has_scope = non_empty("scope");
            let has_workflow_id = non_empty("workflow_id");
            if !has_scope && !has_workflow_id {
                return Err(RpcError::new(
                    RpcCode::InvalidArgument,
                    "a 'sub_workflow' node needs a non-empty 'scope' (the child flow to resolve) or an \
                     explicit 'workflow_id'",
                ));
            }
            for field in ["inject", "retrieve"] {
                if let Some(value) = config.get(field) {
                    validate_board_copies(field, value)?;
                }
            }
            Ok(())
        }
        db::NODE_TYPE_QUERY => {
            if !non_empty("query") {
                Err(required("query"))
            } else if !non_empty("write") {
                Err(required("write"))
            } else {
                // The fact NAME must be one nyx actually resolves — a mis-typed `query` would branch
                // the graph on a permanent `null` at runtime, so it is refused at authoring (B2). The
                // accepted set is shared with the runtime resolver — the TEST and BOTH listings (this
                // one and the runtime's) read `mcp_engine::QUERY_FACTS`, so none of them can drift
                // from it (this one did, by one fact, inside this PRD's working tree).
                let query = config.get("query").and_then(Value::as_str).unwrap_or("");
                if !crate::mcp_engine::valid_query(query) {
                    Err(RpcError::new(
                        RpcCode::InvalidArgument,
                        format!(
                            "query '{query}' is not a known nyx fact (accepted: {})",
                            crate::mcp_engine::QUERY_FACTS.join(", ")
                        ),
                    ))
                } else if query == "spec_can_transition" {
                    // The parameterized fact needs a VALID transition `target` (else it always answers
                    // false with a "missing target" reason at runtime).
                    let target = config.get("target").and_then(Value::as_str).unwrap_or("");
                    if target.trim().is_empty() {
                        Err(required("target"))
                    } else if !crate::mcp_engine::valid_transition_target(target) {
                        Err(RpcError::new(
                            RpcCode::InvalidArgument,
                            format!(
                                "spec_can_transition target '{target}' is not a valid Spec transition \
                                 (ready, in_progress/start, in_review/review, done, cancel)"
                            ),
                        ))
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
        }
        _ => Ok(()),
    }
}

/// Validate a `sub_workflow` node's `inject` / `retrieve` field: it must be an ARRAY of objects, each
/// carrying a non-empty string `from` AND `to` (the declared board copy). A non-array, or an entry
/// missing/blanking either key, is refused at authoring so a mis-shaped map does not silently copy
/// nothing at runtime (v2.8 lot 2).
///
/// A copy's `to` is a BLACKBOARD WRITE like any other, so it goes through the SAME key validation a
/// `write` / `output_key` does ([`validate_board_write_key`]). Checking only those two left the H12
/// grant reachable through the widest write in the vocabulary: `retrieve: [{from: <any truthy child
/// key>, to: "__spec_write_granted__"}]` lands that key on the PARENT board straight from a child
/// run's blackboard, and `spec_section_write_is_authorized` reads exactly that — so a graph could hand
/// itself the authorization to rewrite a Spec that has left draft, with no human answer anywhere.
/// "Only the engine writes it" has to hold over EVERY write a node can express, and so does "a key
/// the board can read back".
fn validate_board_copies(field: &str, value: &Value) -> Result<(), RpcError> {
    let bad = |msg: String| RpcError::new(RpcCode::InvalidArgument, msg);
    let Some(arr) = value.as_array() else {
        return Err(bad(format!(
            "a 'sub_workflow' node's '{field}' must be an array of {{from, to}} copies"
        )));
    };
    for (i, entry) in arr.iter().enumerate() {
        let non_empty_str = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        };
        if !non_empty_str("from") || !non_empty_str("to") {
            return Err(bad(format!(
                "'{field}'[{i}] must be a {{from, to}} object with non-empty string keys"
            )));
        }
        let to = entry.get("to").and_then(Value::as_str).unwrap_or("");
        validate_board_write_key(&format!("'{field}'[{i}]'s 'to'"), to)?;
    }
    Ok(())
}

/// Validate ONE blackboard key a node declares it WRITES — a `write`, an `output_key`, a copy's `to`.
/// `what` names the field for the message (the caller knows whether it is holding a config field or a
/// copy entry).
///
/// Two ways such a key is a trap, both silent at runtime:
///
///   * the RESERVED [`crate::engine::SPEC_WRITE_GRANT_KEY`] (H12) — a node that writes it hands
///     ITSELF the authorization to correct a Spec that has left draft, which is the human's answer to
///     give and nobody else's. Refusing it here is what makes "only the engine writes it" a property.
///   * the RESERVED [`crate::engine::PROVENANCE_KEY`] — the board's record of WHICH NODE wrote each
///     key, which the runtime consent check reads to decide whether a value came from a human. A node
///     that can name it as an `output_key` (or a `write`, or a `retrieve.to`) writes its own answer to
///     that question: `output_key: "__written_by__"` with the right object under it makes every gate
///     in the graph read "an ask_human filled this". The refusal is what keeps provenance a FACT the
///     engine observes rather than a claim the graph makes about itself — the same reasoning, for the
///     same reason, as the grant key above.
///   * a DOTTED key. The board WRITES flat ([`crate::engine::Blackboard::set`] inserts one top-level
///     key, `append` likewise) and READS by dot-path ([`crate::engine::Blackboard::get`] splits on
///     '.'), so `write: "spec.status"` inserts a literal `"spec.status"` entry that every later read
///     resolves as `map["spec"]["status"]` — absent, forever. A selector branching on it misses every
///     branch and a loop reading it burns its whole iteration budget (a round, and a sub-agent, per
///     pass) on a value that can never appear. The key was accepted, the write happened, and only the
///     read was impossible: nothing anywhere said so.
fn validate_board_write_key(what: &str, key: &str) -> Result<(), RpcError> {
    if key == crate::engine::SPEC_WRITE_GRANT_KEY {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "{what} writes '{}', which is RESERVED: nyx writes it itself from a human's answer \
                 (an ask_human's 'grant_spec_write_on'), and no node may write it — it is what \
                 authorizes a Spec-section correction on a Spec that has left draft",
                crate::engine::SPEC_WRITE_GRANT_KEY
            ),
        ));
    }
    if key == crate::engine::PROVENANCE_KEY {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "{what} writes '{}', which is RESERVED: it is the board's own record of WHICH NODE \
                 wrote each key, kept by the engine at every write and read by nyx to tell a human's \
                 answer from a fact a query produced. A node that writes it would be declaring its \
                 own provenance — name a top-level key of your own",
                crate::engine::PROVENANCE_KEY
            ),
        ));
    }
    if key.contains('.') {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "{what} writes '{key}', and a blackboard key may not contain a '.': the board is \
                 written FLAT and read by dot-path, so '{key}' would be stored whole and every later \
                 read of it would look for a nested value that is never there — name a top-level key"
            ),
        ));
    }
    Ok(())
}

// --- Argument helpers ------------------------------------------------------

/// Resolve the workflow `scope` arg (default `custom`), validated against the vocabulary.
fn parse_scope(args: &Value) -> Result<&'static str, RpcError> {
    Ok(match optional_str(args, "scope")? {
        None => db::WORKFLOW_SCOPE_CUSTOM,
        Some(db::WORKFLOW_SCOPE_NDEV) => db::WORKFLOW_SCOPE_NDEV,
        Some(db::WORKFLOW_SCOPE_NSPEC) => db::WORKFLOW_SCOPE_NSPEC,
        Some(db::WORKFLOW_SCOPE_NDOC) => db::WORKFLOW_SCOPE_NDOC,
        Some(db::WORKFLOW_SCOPE_CUSTOM) => db::WORKFLOW_SCOPE_CUSTOM,
        // The SUB-WORKFLOW-ONLY scopes (v2.8 lot 2): authorable so a `sub_workflow` node has a
        // coder/prototyper target to resolve, but reached only as a sub-workflow child (no
        // `/coder` `/prototyper` command, no default run_start wiring).
        Some(db::WORKFLOW_SCOPE_CODER) => db::WORKFLOW_SCOPE_CODER,
        Some(db::WORKFLOW_SCOPE_PROTOTYPER) => db::WORKFLOW_SCOPE_PROTOTYPER,
        Some(other) => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "unknown scope '{other}' (accepted: ndev, nspec, ndoc, custom, coder, prototyper)"
                ),
            ))
        }
    })
}

/// Resolve the REQUIRED node `type` arg, validated against the behavior-tree vocabulary
/// ([`NodeType::from_db`]).
fn parse_node_type(args: &Value) -> Result<&str, RpcError> {
    let t = require_str(args, "type")?;
    if NodeType::from_db(t).is_some() {
        Ok(t)
    } else {
        // The listing is READ from [`db::NODE_TYPES`], never re-typed beside it — the same rule
        // `mcp_engine::QUERY_FACTS` now enforces for the `query` facts, and for the same reason: a
        // hand-kept copy drifts from the set it describes, and this sentence is reached exactly when
        // somebody needs it to be complete. It had already fallen a type behind twice (`sub_workflow`,
        // then `create_workspace`), each time telling an author their node type did not exist.
        Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "unknown node type '{t}' (accepted: {})",
                db::NODE_TYPES.join(", ")
            ),
        ))
    }
}

/// A node `config` arg: absent/null → `None`; a JSON OBJECT → its serialized string; anything else
/// is refused (a node config must be a JSON object).
fn config_string(args: &Value, key: &str) -> Result<Option<String>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v @ Value::Object(_)) => Ok(Some(v.to_string())),
        Some(_) => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!("'{key}' must be a JSON object"),
        )),
    }
}

/// An optional 32-bit integer arg (e.g. `order_index`).
fn optional_i32(args: &Value, key: &str) -> Result<Option<i32>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidArgument,
                    format!("'{key}' must be a 32-bit integer"),
                )
            }),
    }
}

/// A REQUIRED 32-bit integer arg.
fn require_i32(args: &Value, key: &str) -> Result<i32, RpcError> {
    optional_i32(args, key)?.ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("missing required '{key}'"),
        )
    })
}

/// Validate a project id EXISTS (a bad id → `invalid_id`). Used before binding a workflow to it.
fn validate_project(db: &Db, project_id: &str) -> Result<(), RpcError> {
    if db
        .with_conn(|c| db::get_project(c, project_id))
        .map_err(internal_db)?
        .is_none()
    {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!("unknown project {project_id}"),
        ));
    }
    Ok(())
}

// --- Tree coherence (the single validation authority) ----------------------

/// Whether a node COMMITS something a human alone may commit — the set the shape rule (4) of
/// [`validate_tree`] demands a gate for.
///
/// A THIN READ of [`crate::engine::SensitiveEffect`], which is where the set itself lives, together
/// with what each member commits and why. It is deliberately not a second list: this rule and the
/// RUNTIME's human-turn demand ([`crate::engine::Tree::ask_human_commitment`]) each used to carry
/// their own, and the runtime's held ONE of the three — so a `fork_spec` or a `create_workspace` this
/// function certified HUMAN-GATED asked for no human turn at all, and the session driving the run
/// answered for the human (finding `01M0C3BZPWV832D802C0GZFTMC`).
fn is_sensitive_node(n: &db::Node) -> bool {
    let Some(node_type) = NodeType::from_db(&n.type_) else {
        return false;
    };
    let config: Value = serde_json::from_str(&n.config).unwrap_or_else(|_| json!({}));
    SensitiveEffect::of(node_type, &config).is_some()
}

/// Every blackboard key a node WRITES, reduced to the flat root the board actually holds — the reading
/// behind rule (4)'s SINGLE-WRITER condition (iii) in [`sensitive_node_gate`].
///
/// Derived from the config, node type by node type, and it covers every write the interpreter
/// performs ([`crate::engine::Executor::apply_result`] and `mcp_engine::drive`):
///
///   * `ask_human` / `judge` / `produce` / `sub_workflow` — the `output_key`, else the type's DEFAULT
///     key (`human` / `verdict` / `artifact` / `sub_result`). A leaf with no `output_key` still
///     writes, and it writes a key another node is free to name.
///   * `sub_workflow` ALSO folds each `retrieve[].to` FLAT onto the PARENT board (`engine.set_board`),
///     so those count — it is the widest write in the vocabulary, which is why
///     [`validate_board_copies`] already validates them as writes.
///   * `query` — its `write`, plus the companion `<write>_reason` `drive` folds beside it.
///   * `spawn_subagent` (`last_diff`, `spawns`), `gate_check` (`gate_green`,
///     [`crate::mcp_engine::LAST_GATE_RESULT_KEY`], [`crate::mcp_engine::BLOCKING_GATE_RED_KEY`]),
///     `loop` ([`crate::engine::LOOP_ITER_KEY`]), `sub_workflow`
///     ([`crate::mcp_engine::subworkflow_child_key`]) — FIXED keys the engine writes WITHOUT the
///     node's config naming them, but any `output_key` may name them, so they are listed rather than
///     assumed unreachable.
///
/// The last three of those were MISSING (finding `01M0C3F7V89HJFDGWJEBP12P0N`): `last_gate_result`,
/// `blocking_gate_red` and `__child_run_id__<node>` were written by `drive` and censused by nobody, so
/// a graph could satisfy condition (iii) on a key a `gate_check` or a `sub_workflow` also writes. It
/// was never a bypass — those three carry an object, an array and a UUID, and
/// [`crate::mcp_engine::board_value_is_affirmative`] credits none of them, so they can REVOKE a
/// consent but never fabricate one — but "the list is the contract" has to mean the list, and
/// [`crate::mcp_authoring::tests`]'s census test now fails when a fixed key is added without one.
///
/// What it does NOT model: the VALUE a write carries, nor whether the writer is on the path actually
/// taken. It answers "which nodes may touch this key at all", never "what will be in it" — deliberately,
/// because rule (4) wants the conservative half of that question.
fn node_written_board_keys(n: &db::Node) -> Vec<String> {
    let cfg: Value = serde_json::from_str(&n.config).unwrap_or_else(|_| json!({}));
    let str_field = |name: &str| -> Option<String> {
        cfg.get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| board_key_root(s).to_string())
    };
    let with_default = |default: &str| -> Vec<String> {
        vec![str_field("output_key").unwrap_or_else(|| default.to_string())]
    };
    match n.type_.as_str() {
        db::NODE_TYPE_ASK_HUMAN => with_default(crate::engine::DEFAULT_ASK_HUMAN_OUTPUT_KEY),
        db::NODE_TYPE_JUDGE => with_default(crate::engine::DEFAULT_JUDGE_OUTPUT_KEY),
        // The two remaining result-producing leaves' fixed defaults, spelled as `apply_result` spells
        // them (`write_result(leaf, "artifact"/"sub_result", …)`).
        db::NODE_TYPE_PRODUCE => with_default("artifact"),
        db::NODE_TYPE_SUB_WORKFLOW => {
            let mut keys = with_default("sub_result");
            // The CHILD-run handle `drive` parks the leaf on — keyed per node, written and cleared by
            // the `sub_workflow` arm, and named by no config field.
            keys.push(crate::mcp_engine::subworkflow_child_key(&n.id));
            for copy in cfg
                .get("retrieve")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(to) = copy.get("to").and_then(Value::as_str) {
                    let to = to.trim();
                    if !to.is_empty() {
                        keys.push(board_key_root(to).to_string());
                    }
                }
            }
            keys
        }
        db::NODE_TYPE_QUERY => match str_field("write") {
            Some(write) => vec![format!("{write}_reason"), write],
            None => Vec::new(),
        },
        db::NODE_TYPE_SPAWN_SUBAGENT => vec!["last_diff".to_string(), "spawns".to_string()],
        // A `gate_check` writes FIVE keys, and none is a config-visible one. Three come from the
        // fold — WHAT the command did ([`crate::engine::GATE_PASSED_KEY`]), its historical alias
        // ([`crate::engine::GATE_GREEN_KEY`]) and whether that ANSWERED the declared expectation
        // ([`crate::engine::GATE_SATISFIED_KEY`]) — and `drive` also folds the full verdict object
        // and (for a BLOCKING gate with no declared expectation) the red-gate anchor array.
        db::NODE_TYPE_GATE_CHECK => vec![
            crate::engine::GATE_PASSED_KEY.to_string(),
            crate::engine::GATE_GREEN_KEY.to_string(),
            crate::engine::GATE_SATISFIED_KEY.to_string(),
            crate::mcp_engine::LAST_GATE_RESULT_KEY.to_string(),
            crate::mcp_engine::BLOCKING_GATE_RED_KEY.to_string(),
        ],
        db::NODE_TYPE_LOOP => vec![crate::engine::LOOP_ITER_KEY.to_string()],
        // `set_category` writes NOTHING on the board, and that is a decision rather than an
        // omission: it READS a label (`category_key`) and commits it to `specs.category_id`, so the
        // classification lives in the DOMAIN and not on the run's board. A graph that has to ROUTE on
        // it reads it BACK through the `spec_category` query fact, which answers the persisted slug —
        // one source of truth for the genre of work, never a board copy that can go stale the moment
        // a human recategorizes the Spec. The arm is spelled out so the census names the type
        // deliberately instead of letting it fall to the catch-all below.
        db::NODE_TYPE_SET_CATEGORY => Vec::new(),
        _ => Vec::new(),
    }
}

/// A config field read RAW — the string as authored, dots and all. [`board_key_root`] is the right
/// reading for "which key does this DEPEND on"; this is the reading for "what will the engine
/// actually look up", which is what the two dead-gate closures below need to tell apart.
fn raw_field(n: &db::Node, name: &str) -> Option<String> {
    serde_json::from_str::<Value>(&n.config)
        .ok()?
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The board path `raw` names, when an `ask_human`'s ANSWER could ever satisfy a read of it — `None`
/// for a DOTTED path, which no answer can (finding `01M0C3FGCACXCZA1ZESRV73N0R`(a)).
///
/// An answer is always a SCALAR: `mcp_engine::human_answer` accepts a string / number / bool and
/// refuses an object or an array outright. `Blackboard::set` then puts that scalar under ONE flat
/// key. A read of `ok.deep` goes through [`crate::engine::Blackboard::get`], which splits on dots and
/// descends — into a scalar, so it resolves to nothing, forever. The gate cannot open and the branch
/// cannot match, whatever the human answers.
///
/// This is the READ side of the pair [`validate_board_write_key`] already refuses on the WRITE side,
/// and for the same reason it gives there: "the key was accepted, the write happened, and only the
/// read was impossible: nothing anywhere said so". It is NOT a blanket ban on dotted reads — a
/// `selector` may perfectly well branch on `proto_result.status`, because a `sub_workflow` writes an
/// OBJECT there, and `/nspec` ships exactly that. What is refused is a dotted read counting as a
/// CONSENT, since the value under it would have to be a human's answer and a human's answer is flat.
fn answer_readable_key(raw: &str) -> Option<&str> {
    (!raw.is_empty() && !raw.contains('.')).then_some(raw)
}

/// Whether an `ask_human`'s answer lands on its key in a shape a consent read can ever accept —
/// `false` for `append: true` (finding `01M0C3FGCACXCZA1ZESRV73N0R`(b)).
///
/// `Executor::write_result` honours `append` by calling `Blackboard::append`, so the key holds an
/// ARRAY (`["yes"]`), not the answer. [`crate::mcp_engine::board_value_is_affirmative`] knows only
/// `Bool` and `String`, so an array is never an approval; and a selector `eq` compares against the
/// array, which no affirmative literal equals. Both readings of condition (ii) are therefore dead,
/// and crediting such a question would certify a gate that can never open. An appending question is
/// a TRANSCRIPT (`/nspec`'s "steer" accumulates the grill's answers); it is not a decision.
fn answer_can_fill(ask: &db::Node) -> bool {
    !serde_json::from_str::<Value>(&ask.config)
        .ok()
        .and_then(|c| c.get("append").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// The blackboard key a selector branch tests for an AFFIRMATIVE value — rule (4)'s POLARITY
/// condition — or `None` when the branch carries no consent at all.
///
/// A THIN READ of [`crate::engine::Condition::affirmative_key`], which is where the shape and every
/// refusal are written down, and which the RUNTIME classification
/// ([`crate::engine::Tree::ask_human_commitment`]) reads too — the selector arm of rule (4) and the
/// human-turn demand behind it must route on the same branches or neither claim means anything.
fn branch_affirmative_key(cond: &crate::engine::Condition) -> Option<String> {
    cond.affirmative_key().map(str::to_string)
}

/// The `ask_human` node that GATES a sensitive node, or `None` when nothing does — the whole of rule
/// (4). THREE conditions, and ALL THREE are needed; any one alone is a guard that only looks like one.
///
/// **(i) THE QUESTION MUST HAVE BEEN ASKED.** The `ask_human` has to be a node that NECESSARILY ran
/// before this one: a preceding sibling — of the sensitive node itself, or of any of its ANCESTORS —
/// inside a `sequence` or a `loop` body, the two composites that walk their children in order (a
/// `loop` re-enters at cursor 0, so a left sibling ran in every pass). A `selector` gives no such
/// guarantee: it PICKS one child, so nothing to a branch's left is known to have run, and a sibling
/// branch's `ask_human` is a question this run may never have been asked at all.
///
/// **WHY IT CLIMBS.** The rule used to look at the node's own siblings only, and that is what left
/// `create_workspace` ungatable: the seeded `/nspec` asks "mark it ready?" ONE selector higher, and
/// the branch that answer selects is a whole sequence — read where the folder goes, prevalidate it,
/// commit, create. A consent given higher up the chain is still a consent; refusing to look at it
/// would have meant either a false gate or an un-authorable graph. What must NOT follow is that the
/// whole tree counts: the climb walks the ANCESTOR CHAIN, which is exactly the branch the run took to
/// reach the node, and never leaves it.
///
/// **(ii) THAT ANSWER MUST DECIDE THIS NODE, AND DECIDE IT AFFIRMATIVELY.** "A human was asked
/// something first" is not consent — it is chronology. So the key the question WRITES has to be one
/// the path to this node actually reads, in a way that only an APPROVAL satisfies. Exactly two
/// readings qualify:
///
///   * the sensitive node's own `gate_on` — the RUNTIME gate fires the node only when that key holds
///     an affirmative ([`crate::mcp_engine::board_value_is_affirmative`]), so the polarity is enforced
///     where the effect happens;
///   * the `cond` of a `selector` branch the chain descends THROUGH, when that cond is an AFFIRMATIVE
///     test on the key ([`branch_affirmative_key`]) — the routing IS the decision, the N16 pattern:
///     approve → the node, refuse → a safe note that never freezes the run.
///
/// AND THE READING MUST BE ONE AN ANSWER CAN SATISFY. Both readings above are refused when they are
/// DOTTED ([`answer_readable_key`]) and the crediting question is refused when it APPENDS
/// ([`answer_can_fill`]): in each case the value the gate reads is provably not the answer — nothing
/// (a dot-path descending into a scalar) or an array. Such a graph used to VALIDATE and then never
/// fire its sensitive node, or die on `SelectorNoBranch`; a gate the author believes is posed and that
/// cannot open is a wrong answer to a question about consent, even though it is the safe wrong answer.
///
/// Any other branch contributes NOTHING to the decision — an `always`/default branch, a cond with no
/// `var`, a `truthy`, a `ne`, and (the reproduced bypass) an `eq` on a REFUSAL: `{var: "ready_ok",
/// eq: "no"}` routing a `create_workspace` used to validate, and answering "no" then made the folder.
/// A branch that contributes nothing leaves the child reachable with no approval behind it, so no
/// approval is accountable for it. A child named by MORE THAN ONE branch counts only when EVERY one of
/// them tests the SAME key affirmatively — otherwise the child is reachable by a route that key never
/// decided. Which branch selects which child, and how a cond reads, both come from
/// [`crate::engine::SelectorConfig`] / [`crate::engine::Condition`], the interpreter's own decoding.
///
/// **(iii) NOTHING ELSE MAY WRITE THAT KEY.** The question's key must have exactly ONE writer in the
/// whole workflow: the crediting `ask_human` itself ([`node_written_board_keys`]). Without (iii),
/// (i) and (ii) together still only proved that a key with the right NAME was filled by a question
/// once — and the reproduced bypass is three nodes long: `sequence[ ask_human{output_key:
/// "approved"}, query{write: "approved"}, transition{ready, gate_on: "approved"} ]` validated, the
/// human answered "no", the `query` overwrote it with `true`, and the Spec moved to `ready`. A single
/// writer is what makes "the value the gate reads is the answer the human gave" a property of the
/// GRAPH rather than a hope about execution order, and it is checked over the whole workflow rather
/// than over the path, because "the path" is exactly the thing static analysis cannot pin down.
///
/// # What this rule does NOT guarantee
///
/// Stated flatly, because four widenings of this guard each shipped a comment that promised more than
/// the code held:
///
///   * **Not that the human APPROVED.** It proves that an approval is the only thing that can reach
///     the node, given the graph. What actually lands on the key is a runtime fact; the `gate_on` arm
///     leans on the runtime gate for it, the selector arm on the branch's `eq`.
///   * **Not that the answer is a HUMAN's.** `ask_human` is a node type here, not an observed turn.
///     Whether a real human answered is `mcp_engine`'s `human_turn_required` / `accept_consent_answer`
///     business, and this rule neither sees nor replaces it. That delegation is only worth anything
///     because the two layers now read THE SAME SET ([`crate::engine::SensitiveEffect`]): while they
///     each kept their own, this rule certified `fork_spec` and `create_workspace` as human-gated and
///     the runtime demanded no human turn for either, so the delegation went nowhere.
///   * **Not that the key is unwritable.** (iii) covers the writes a node DECLARES in its config PLUS
///     the fixed keys the engine writes on a node's behalf, per [`node_written_board_keys`]. A node
///     type added later that writes a key that list does not name is invisible here; the list is the
///     contract, and `mcp_authoring::tests`'s census test is what keeps it one.
///   * **Not that the branch is REACHED.** (i) proves the question necessarily ran; nothing proves the
///     run gets as far as the sensitive node, and nothing needs to.
///   * **Not that the gate is LIVE beyond the two shapes named above.** A dotted read and an appending
///     question are refused because no answer can satisfy them, but this rule does not simulate the
///     run: it cannot tell an author that the answer they will actually get is off-vocabulary, nor
///     that a `gate_on` whose key a `query` fills will be `false` on the day.
///   * **Not that the graph is safe.** It answers ONE question — is this commitment behind a human
///     approval — for the three members of [`crate::engine::SensitiveEffect`], and nothing about the
///     rest.
///
/// # The alternative this does NOT take (for the user to arbitrate)
///
/// All of the above is AUTHORING-time analysis: it reads the graph and reasons about what execution
/// could do. The engine, at the instant it is about to run a sensitive node, does not have to reason
/// — it holds the actual value on the actual board, and it already tells a real human turn from a
/// replayed one (`mcp_engine`'s `human_turn_required` / `accept_consent_answer`). The one thing it
/// does not keep today is WHICH node last wrote a board key: the blackboard is a flat JSON object, so
/// that provenance would have to be recorded on the write side — an addition to
/// [`crate::engine::Blackboard`], not an inference. With it, a RUNTIME check — "refuse the effect
/// unless the gate key's current value was written by an `ask_human` on a human turn" — needs no
/// single-writer approximation, no polarity read of `cond` literals, and no ancestor climb, and it
/// holds for graphs authored before it existed. Its cost is the mirror image: the refusal lands
/// mid-run instead of at `node_add`, so an author meets it as a parked run rather than as a rejected
/// edit. The two are complementary, not exclusive. Whether to add it — and whether this static rule
/// then relaxes — is a design decision for the user; it is written down here, not built.
fn sensitive_node_gate<'a>(nodes: &'a [db::Node], node: &db::Node) -> Option<&'a db::Node> {
    let by_id: HashMap<&str, &db::Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    // Children per parent in EXECUTION order — the same `(order_index, id)` ordering
    // [`Tree::from_rows`] rebuilds, so "the branch that selects this child" is read off the very
    // sequence the interpreter walks.
    let mut children_of: HashMap<&str, Vec<&db::Node>> = HashMap::new();
    for n in nodes {
        if let Some(p) = n.parent_id.as_deref() {
            children_of.entry(p).or_default().push(n);
        }
    }
    for kids in children_of.values_mut() {
        kids.sort_by(|a, b| {
            a.order_index
                .cmp(&b.order_index)
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    let field = |n: &db::Node, name: &str| -> Option<String> {
        serde_json::from_str::<Value>(&n.config)
            .ok()?
            .get(name)
            .and_then(Value::as_str)
            .map(|s| board_key_root(s).to_string())
    };
    // The key an `ask_human` fills: its `output_key`, else the shared default the engine writes to.
    let written_key = |n: &db::Node| -> String {
        field(n, "output_key")
            .unwrap_or_else(|| crate::engine::DEFAULT_ASK_HUMAN_OUTPUT_KEY.to_string())
    };
    // The key the branches that route `child` test AFFIRMATIVELY — at most one, and only when EVERY
    // branch naming that child tests THE SAME key that way. One branch that routes there
    // unconditionally, on a refusal, or on a non-discriminating test (a `truthy` on a closed answer
    // set is an `always` in disguise) makes the child reachable without an approval, so the whole
    // selector then decides NOTHING and contributes nothing (ii).
    let routing_keys = |selector: &db::Node, child: &str| -> Vec<String> {
        let Ok(cfg) = serde_json::from_str::<crate::engine::SelectorConfig>(&selector.config)
        else {
            return Vec::new();
        };
        let Some(position) = children_of
            .get(selector.id.as_str())
            .and_then(|kids| kids.iter().position(|k| k.id == child))
        else {
            return Vec::new();
        };
        let mut approved: Option<String> = None;
        for (i, branch) in cfg.branches.iter().enumerate() {
            if branch.child.unwrap_or(i) != position {
                continue;
            }
            let Some(cond) = branch.cond.as_ref() else {
                return Vec::new();
            };
            // A DOTTED read is a read no answer can satisfy ([`answer_can_fill`]), so the branch
            // decides nothing — and, unlike a plain non-affirmative branch, it also makes the child
            // reachable on the `always` fall-through. Treated exactly like one: the whole selector
            // contributes nothing.
            let Some(key) = answer_readable_key(cond.var.as_deref().unwrap_or(""))
                .and(branch_affirmative_key(cond))
            else {
                return Vec::new();
            };
            match &approved {
                Some(previous) if *previous != key => return Vec::new(),
                _ => approved = Some(key),
            }
        }
        approved.into_iter().collect()
    };

    // (iii) SINGLE WRITER — who, in the WHOLE workflow, can put a value under each board key.
    let mut writers_of: HashMap<String, Vec<&str>> = HashMap::new();
    for n in nodes {
        for key in node_written_board_keys(n) {
            writers_of.entry(key).or_default().push(n.id.as_str());
        }
    }
    let is_sole_writer = |ask: &db::Node, key: &str| -> bool {
        writers_of
            .get(key)
            .map(|w| w.len() == 1 && w[0] == ask.id)
            .unwrap_or(false)
    };

    // The keys whose AFFIRMATIVE value DECIDES whether this node fires, accumulated as the climb goes
    // up: its own runtime gate first (the gate itself tests the polarity), then every selector branch
    // the path descends through that tests a key for an approval. A question found at level N sits
    // ABOVE everything collected below it, so its answer is available to all of them.
    let mut decides: HashSet<String> = HashSet::new();
    decides.extend(
        raw_field(node, SensitiveEffect::GATE_FIELD)
            .as_deref()
            .and_then(answer_readable_key)
            .map(str::to_string),
    );
    let mut cursor = node;
    for _ in 0..nodes.len() {
        let Some(parent) = cursor
            .parent_id
            .as_deref()
            .and_then(|p| by_id.get(p).copied())
        else {
            return None;
        };
        match parent.type_.as_str() {
            // Ordered composites: every left sibling has necessarily run.
            db::NODE_TYPE_SEQUENCE | db::NODE_TYPE_LOOP => {
                let asked = children_of
                    .get(parent.id.as_str())
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|s| {
                        s.type_ == db::NODE_TYPE_ASK_HUMAN && s.order_index < cursor.order_index
                    })
                    .filter(|s| answer_can_fill(s))
                    .find(|s| {
                        let key = written_key(s);
                        decides.contains(&key) && is_sole_writer(s, &key)
                    });
                if asked.is_some() {
                    return asked;
                }
            }
            // A selector guarantees no ordering, but the edge carries the test that chose this branch.
            db::NODE_TYPE_SELECTOR => decides.extend(routing_keys(parent, &cursor.id)),
            _ => {}
        }
        cursor = parent;
    }
    None
}

/// Validate a workflow's node rows form a COHERENT tree, or return the reason it does not. An empty
/// (node-less) draft is a valid intermediate state. The rules, layered:
///
///   1. [`Tree::from_rows`] — unknown node type, non-object `config`, a child referencing a parent
///      not in the workflow (dangling), and a missing / ambiguous root.
///   2. A LEAF node may not have children (`type`/enfants invalides).
///   3. No cycle / orphan: every node's `parent_id` chain must reach THE root without looping. A
///      disconnected cycle that coexists with a valid root passes (1), so this closes it.
///   4. A SENSITIVE node ([`is_sensitive_node`]) is HUMAN-GATED — an `ask_human` that necessarily runs
///      before it, whose answer decides it AFFIRMATIVELY, and whose key nothing else writes
///      ([`sensitive_node_gate`], which also states what that does NOT guarantee).
///   5. A question a BRANCH answers declares that branch's answers ([`Tree::ask_human_unconstrained_branch`], J1).
pub(crate) fn validate_tree(nodes: &[db::Node], root_id: Option<&str>) -> Result<(), String> {
    if nodes.is_empty() {
        return Ok(());
    }
    // (1) The interpreter's own reconstruction is the single source of the structural rules.
    let tree = Tree::from_rows(nodes, root_id).map_err(|e| e.to_string())?;

    let type_of: HashMap<&str, &str> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.type_.as_str()))
        .collect();
    let parent_of: HashMap<&str, Option<&str>> = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.parent_id.as_deref()))
        .collect();

    // (2) A leaf has no children.
    for n in nodes {
        let Some(parent) = n.parent_id.as_deref() else {
            continue;
        };
        if let Some(ptype) = type_of.get(parent) {
            let is_composite = NodeType::from_db(ptype)
                .map(|t| t.is_composite())
                .unwrap_or(false);
            if !is_composite {
                return Err(format!(
                    "node {} is a child of leaf node {parent} (type '{ptype}'): a leaf has no children",
                    n.id
                ));
            }
        }
    }

    // (3) Cycle / orphan: walk each node's parent chain to the root.
    let root = match root_id {
        Some(id) => id,
        // `from_rows` already proved exactly one parent-less node when `root_id` is None.
        None => nodes
            .iter()
            .find(|n| n.parent_id.is_none())
            .map(|n| n.id.as_str())
            .ok_or_else(|| "no root node".to_string())?,
    };
    for start in nodes {
        let mut cur = start.id.as_str();
        let mut steps = 0usize;
        while cur != root {
            match parent_of.get(cur).copied().flatten() {
                Some(p) => {
                    cur = p;
                    steps += 1;
                    if steps > nodes.len() {
                        return Err(format!(
                            "cycle detected in the parent chain of node {}",
                            start.id
                        ));
                    }
                }
                None => {
                    return Err(format!(
                        "node {} is not connected to root {root} (orphan / second root)",
                        start.id
                    ));
                }
            }
        }
    }

    // (4) A SENSITIVE commitment — an ENGAGING transition (ready/done/cancel), a `fork_spec`, or a
    //     `create_workspace` — must be HUMAN-GATED (v2.8 review B3 / N13 / N16). See
    //     [`sensitive_node_gate`] for the rule and for why it climbs to the ANCESTORS.
    for n in nodes {
        if !is_sensitive_node(n) {
            continue;
        }
        if n.parent_id.is_none() {
            return Err(format!(
                "sensitive node {} ('{}') has no parent — a sensitive node cannot BE the graph: it \
                 must sit under an ask_human that approves it (in its sequence or in an ancestor's), \
                 with that answer read by its own `gate_on` or by the selector branch that routes to \
                 it",
                n.id, n.type_
            ));
        }
        if sensitive_node_gate(nodes, n).is_none() {
            return Err(format!(
                "sensitive node {} ('{}') is not HUMAN-GATED. It needs ONE ask_human meeting all \
                 three conditions: (i) it NECESSARILY runs before — a preceding sibling of the node \
                 itself, or of one of its ancestors, in a sequence or a loop body (an ask_human in a \
                 selector's sibling branch is a question this run may never be asked), and it must \
                 NOT be an `append: true` question — its key would hold an array, never an approval; \
                 (ii) its key decides this node AFFIRMATIVELY — read by the node's own `gate_on`, or \
                 tested by every selector branch on the way down to it as `{{\"var\": <that key>, \
                 \"eq\": <yes/y/ok/approve(d)/ready/go/true>}}`, the literal spelled EXACTLY like \
                 that (the runtime compares by exact equality, so \"YES\" or \" yes \" never match) \
                 and the key un-dotted (a dot-path read of a human's answer resolves to nothing); an \
                 `always` branch, a `truthy`, a `ne`, or an `eq` on a REFUSAL such as \"no\" decides \
                 nothing — answering no would fire the node; (iii) NOTHING ELSE in this workflow \
                 writes that key — no query `write`, no judge/produce/sub_workflow `output_key`, no \
                 sub_workflow `retrieve` `to`, and none of the fixed keys the engine writes itself \
                 (`gate_green`, `last_gate_result`, `blocking_gate_red`, `last_diff`, `spawns`, \
                 `loop_iter`) — since a second writer can replace the human's answer before the gate \
                 reads it. B3 / N13 / N16",
                n.id, n.type_
            ));
        }
    }

    // (5) A question a BRANCH answers must declare the answers that branch honours (J1). An
    //     `ask_human` whose written key is read by a `selector` cond or a `loop` `until` is compared
    //     BY EQUALITY; leaving it free-form promises the human something the graph cannot keep, and
    //     the run then routes on a value nothing constrains. DERIVED from the graph
    //     ([`Tree::ask_human_unconstrained_branch`]) exactly like the sensitive-node rule above, so no
    //     list of "questions that need options" can drift from the graphs it describes.
    for n in nodes {
        if n.type_ != db::NODE_TYPE_ASK_HUMAN {
            continue;
        }
        if let Some(reader) = tree.ask_human_unconstrained_branch(&n.id) {
            return Err(format!(
                "ask_human {} ('{}') declares no `options`, but {} ('{}') BRANCHES on the key it \
                 writes ({}) — so the graph compares that answer by equality while accepting any \
                 text, and a reply it does not recognise takes a branch nobody authored for it. \
                 Declare the closed answer set the branch already assumes (J1)",
                n.id, n.label, reader.node_id, reader.label, reader.condition
            ));
        }
    }
    Ok(())
}

// --- workflow_* verbs ------------------------------------------------------

/// `workflow_create` — `{ name, scope?, project_id? }` → `{ workflow }`. Creates an empty draft (root
/// pointer NULL until the first `node_add`). `project_id` (validated) BINDS it to a project; omitted
/// → a global `custom` TEMPLATE. An UNOWNED workflow of a SEEDED scope (ndev/nspec/ndoc) is REFUSED:
/// those come from seeding, and an unowned one would be an immutable rogue default
/// ([`require_mutable_workflow`], v2.8 review S2) — own it, or keep scope `custom` for a template.
pub fn workflow_create(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let name = require_str(args, "name")?;
    let scope = parse_scope(args)?;
    let project_id = optional_str(args, "project_id")?;
    if let Some(p) = project_id {
        validate_project(db, p)?;
    }
    if project_id.is_none() && scope != db::WORKFLOW_SCOPE_CUSTOM {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "an unowned '{scope}' workflow is a SEEDED built-in, not something to author — pass \
                 project_id to own one, or use scope 'custom' for a global template"
            ),
        ));
    }
    let result: Result<db::Workflow, TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            // One scoped workflow per project (L-d): a second ndev/nspec/ndoc is refused here too,
            // not only in fork, so the invariant is real across every entry point.
            if let Some(p) = project_id {
                require_scope_available(tc, p, scope, None)?;
            }
            Ok(db::create_workflow(tc, project_id, name, scope, None)?)
        })
    });
    result
        .map(|wf| ToolOutcome::changed(json!({ "workflow": wf }), ChangedTopic::Workflows))
        .map_err(map_tx_err)
}

/// `workflow_assign` — `{ workflow_id, project_id }` → `{ workflow }`. (Re)binds the workflow to a
/// project (`workflows.project_id`) — the project↔workflow link. Both ids are validated.
pub fn workflow_assign(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workflow_id = require_str(args, "workflow_id")?;
    let project_id = require_str(args, "project_id")?;
    validate_project(db, project_id)?;
    let result: Result<db::Workflow, TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            // A SEEDED built-in cannot be re-homed into a project — that would remove the shared
            // default (review S2). Re-binding an already project-owned workflow is fine.
            let wf = require_mutable_workflow(tc, workflow_id)?;
            // One scoped workflow per project (L-d): refuse re-homing a 2nd ndev/nspec/ndoc into a
            // project that already owns that scope (excluding this workflow itself).
            require_scope_available(tc, project_id, &wf.scope, Some(workflow_id))?;
            db::set_workflow_project(tc, workflow_id, Some(project_id))?
                .ok_or_else(|| TxErr::Rpc(unknown_workflow(workflow_id)))
        })
    });
    result
        .map(|wf| ToolOutcome::changed(json!({ "workflow": wf }), ChangedTopic::Workflows))
        .map_err(map_tx_err)
}

/// `workflow_fork` — `{ workflow_id, name?, project_id }` → `{ workflow }`. DEEP-COPIES the source
/// workflow's whole tree into a new PROJECT-OWNED workflow (`forked_from` = source), remapping every
/// node id and re-wiring the copied parents, then re-pointing the root. `project_id` is REQUIRED: a
/// fork is a project's CUSTOMISATION, and a `project_id IS NULL` fork of a built-in would create a
/// SECOND rogue built-in (`ndev` etc.) that `ensure_builtin` then trips over (v2.8 review S3). For a
/// SCOPED family (ndev/nspec/ndoc) a project holds ONE workflow, so a second fork of that scope is
/// refused; `custom` is freeform (many allowed). The source is never mutated; the copy is validated
/// before it commits.
pub fn workflow_fork(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let source_id = require_str(args, "workflow_id")?;
    let name_arg = optional_str(args, "name")?;
    let project_id = require_str(args, "project_id")?;
    validate_project(db, project_id)?;

    let result: Result<db::Workflow, TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            let source = db::get_workflow(tc, source_id)?
                .ok_or_else(|| TxErr::Rpc(unknown_workflow(source_id)))?;
            // ONE workflow per SCOPED family per project (S3) — `custom` is freeform.
            require_scope_available(tc, project_id, &source.scope, None)?;
            let src_nodes = db::list_nodes(tc, source_id)?;
            let name = name_arg
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} (fork)", source.name));
            let new_wf =
                db::create_workflow(tc, Some(project_id), &name, &source.scope, Some(source_id))?;
            // STAMP the source's revision AT THE FORK. Without it a fork is a copy of unknown age:
            // the boot seeder upgrades only the rows it seeded, so from this instant every later fix
            // to the source stops reaching this project — and until this stamp existed, nothing could
            // even tell. `run_start` compares it to the source's revision TODAY and says so.
            db::set_forked_from_revision(tc, &new_wf.id, source.builtin_revision)?;

            // Copy nodes parent-before-child (an FK needs the parent row first): repeatedly copy
            // every node whose parent is either a root or already copied, until none remain. Sibling
            // ORDER is preserved intrinsically via each node's `order_index`.
            let mut id_map: HashMap<String, String> = HashMap::new();
            let mut remaining: Vec<&db::Node> = src_nodes.iter().collect();
            while !remaining.is_empty() {
                let mut progressed = false;
                let mut still: Vec<&db::Node> = Vec::new();
                for n in remaining {
                    let ready = match &n.parent_id {
                        None => true,
                        Some(p) => id_map.contains_key(p),
                    };
                    if !ready {
                        still.push(n);
                        continue;
                    }
                    let new_parent = n.parent_id.as_ref().map(|p| id_map[p].clone());
                    let created = db::create_node(
                        tc,
                        &new_wf.id,
                        new_parent.as_deref(),
                        n.order_index,
                        &n.type_,
                        &n.label,
                        &n.config,
                    )?;
                    id_map.insert(n.id.clone(), created.id);
                    progressed = true;
                }
                remaining = still;
                if !progressed {
                    // A malformed source (a dangling/cyclic parent) — refuse rather than spin.
                    return Err(TxErr::Rpc(incoherent(
                        "source workflow has a dangling or cyclic parent chain",
                    )));
                }
            }

            let new_root = source
                .root_node_id
                .as_ref()
                .and_then(|r| id_map.get(r))
                .cloned();
            if new_root.is_some() {
                db::set_workflow_root(tc, &new_wf.id, new_root.as_deref())?;
            }

            let nodes = db::list_nodes(tc, &new_wf.id)?;
            validate_tree(&nodes, new_root.as_deref()).map_err(|m| TxErr::Rpc(incoherent(m)))?;

            db::get_workflow(tc, &new_wf.id)?
                .ok_or_else(|| TxErr::Rpc(unknown_workflow(&new_wf.id)))
        })
    });

    result
        .map(|wf| ToolOutcome::changed(json!({ "workflow": wf }), ChangedTopic::Workflows))
        .map_err(map_tx_err)
}

/// `slot_bind` — `{ project_id, slot, command_ids }` → `{ slot, slots }`. BIND (or re-bind) a graph's
/// command SLOT to the managed commands THIS project answers it with.
///
/// # Why a project binds a slot instead of forking a graph
///
/// A `gate_check` used to name its commands directly, and a managed-command instance id is PROJECT
/// data — no graph shared by every project can name `bun test`. So arming the seeded `coder` gate
/// required editing a SEEDED built-in, which the authoring layer refuses (rightly: the tree is shared
/// and the seeder would overwrite the edit), leaving exactly one route — FORK the built-in and arm the
/// copy. And a fork is never upgraded: `ensure_builtin` touches only the rows it seeded, while
/// `resolve_active_workflow` prefers the project's own workflow. The first gesture any real project
/// had to make therefore froze its `coder` graph forever.
///
/// The slot splits the two halves along the line that always separated them: the SHARED graph declares
/// WHAT must be verified (`gate_check.config.slot`), this row says WITH WHAT. The built-in needs no
/// fork, so it keeps upgrading, and the binding is re-read at every tick — binding it unblocks the run
/// that is refusing, on its very next `nyx_next`.
///
/// Each id must be a KNOWN command instance (`add_command` materialises them, `list_commands` shows
/// them): a mistyped id would otherwise sit in the binding looking armed and surface much later, as a
/// gate that cannot run. An EMPTY `command_ids` is accepted and means UNBOUND — the honest way to undo
/// a binding, and the gate goes back to refusing.
///
/// And a command that OBVIOUSLY never returns is refused here too ([`never_returns`]) — see there for
/// why the binding is the right place to catch it.
pub fn slot_bind(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = require_str(args, "project_id")?;
    let slot = require_str(args, "slot")?.trim().to_string();
    if slot.is_empty() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "argument 'slot' must be a non-empty slot name (the one the gate_check node declares — \
             run_start's gate warning carries it under `slot`)",
        ));
    }
    let command_ids = crate::mcp_tools_core::require_str_vec(args, "command_ids")?;
    validate_project(db, project_id)?;
    for id in &command_ids {
        // ONE query answers both questions: does the instance exist, and is what it runs something
        // that can ever produce a verdict.
        let ctx = db
            .with_conn(|c| db::instance_run_context(c, id))
            .map_err(internal_db)?
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidId,
                    format!(
                        "unknown command instance '{id}' — bind the instance_id a command has in \
                         THIS workspace (list_commands), not a template id"
                    ),
                )
            })?;
        if let Some(reason) = never_returns(&ctx.command, ctx.restart_on_startup) {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "command instance '{id}' ({}) {reason}, so it can never produce a gate verdict. \
                     Bound to slot '{slot}', EVERY tick of every run reaching that gate would hold \
                     the command for {}s and then go red on the clock, never on the code — the run \
                     would stall there for good. Bind the one-shot form of the check instead (`bun \
                     test`, not `bun test --watch`); add_command registers it if the project has \
                     none",
                    ctx.command,
                    crate::mcp_engine::GATE_TOTAL_TIMEOUT.as_secs(),
                ),
            )
            .with_data(json!({ "reason": "command_never_returns", "command_id": id })));
        }
    }
    let (bound, slots) = db
        .with_conn(|c| {
            let bound = db::bind_command_slot(c, project_id, &slot, &command_ids)?;
            let slots = db::list_command_slots(c, project_id)?;
            Ok::<_, diesel::result::Error>((bound, slots))
        })
        .map_err(internal_db)?;
    Ok(ToolOutcome::changed(
        // The whole set beside the one just written: a project's slots are few and reading them back
        // is how the caller sees what is STILL unbound without a second round-trip.
        json!({ "slot": bound, "slots": slots }),
        ChangedTopic::Workflows,
    ))
}

/// WHY a managed command obviously never returns, or `None` when nothing says it does not (v2.8 L1).
///
/// A gate's verdict IS its exit code, so a command that never exits yields no verdict at all: it
/// holds an MCP worker until [`crate::mcp_engine::GATE_TOTAL_TIMEOUT`] cuts it off, the gate goes red
/// for the clock rather than for the code, and the run stalls on that node at EVERY tick — for as
/// long as anyone keeps calling `nyx_next`. Catching it at BINDING time costs one query the caller
/// already pays for; discovering it costs 90 seconds per tick, forever. And the mistake is right
/// there to be made: the demo workspace alone ships `bun test --watch`, `bun run typecheck --watch`
/// and `bun run dev`, and `list_commands` offers them to a gate like any other.
///
/// nyx only refuses what it can state as a FACT about the command, never a guess about a command
/// line it does not understand — a wrongly-refused binding is a wall in front of a legitimate gate:
///
///   * a `--watch` flag: the tool is being told to stay resident and re-run on every change, which
///     is the opposite of exiting with a verdict;
///   * `restart_on_startup`: the user DECLARED this command a service nyx relaunches at every app
///     start. A command worth restoring at boot is one meant to keep running.
///
/// Everything else is let through. A dev server named in a way nyx cannot read still reaches the
/// gate and is still cut short there — this narrows the ways in, it does not close them.
fn never_returns(cmdline: &str, restart_on_startup: bool) -> Option<String> {
    if cmdline
        .split_whitespace()
        .any(|tok| tok == "--watch" || tok.starts_with("--watch="))
    {
        return Some("runs in WATCH mode, so it stays resident instead of exiting".to_string());
    }
    if restart_on_startup {
        return Some(
            "is declared a long-running service (restart_on_startup: nyx relaunches it at every app \
             start), so it is not meant to exit"
                .to_string(),
        );
    }
    None
}

// --- node_* verbs (the CRUD that re-builds the tree, each validated) --------

/// `node_add` — `{ workflow_id, type, label?, config?, parent_id?, order_index? }` → `{ node,
/// workflow }`. Adds a typed node; a parent-less node in a still-rootless workflow becomes its root.
/// The resulting tree is validated (a leaf parent, a second root, a foreign parent … → refused), and
/// so is the workflow's runtime state: a run WALKING this tree refuses the edit (M4).
pub fn node_add(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workflow_id = require_str(args, "workflow_id")?;
    let node_type = parse_node_type(args)?;
    let label = optional_str(args, "label")?.unwrap_or("");
    let config = config_string(args, "config")?.unwrap_or_else(|| "{}".to_string());
    let parent_id = optional_str(args, "parent_id")?;
    let order_index = optional_i32(args, "order_index")?.unwrap_or(0);
    // Reject an INEXECUTABLE config at WRITE time — the runtime configs are lenient, so this is where
    // a `spawn` with no agent_id / a `transition` to a bogus state is caught (v2.8 review M1).
    let config_value: Value = serde_json::from_str(&config).unwrap_or_else(|_| json!({}));
    validate_node_config(node_type, &config_value)?;

    let result: Result<(db::Node, db::Workflow), TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            // S2: a BUILT-IN default's tree is immutable — fork it and edit the copy.
            let wf = require_mutable_workflow(tc, workflow_id)?;
            // M4: a new node shifts its siblings' indices under a walking run — refuse.
            require_no_walking_run(tc, workflow_id, "adding a node")?;
            // A given parent must live in THIS workflow (a clean error before an FK surprise).
            if let Some(p) = parent_id {
                let pnode = db::get_node(tc, p)?.ok_or_else(|| {
                    TxErr::Rpc(RpcError::new(
                        RpcCode::InvalidId,
                        format!("unknown parent node {p}"),
                    ))
                })?;
                if pnode.workflow_id != workflow_id {
                    return Err(TxErr::Rpc(incoherent(format!(
                        "parent node {p} belongs to another workflow"
                    ))));
                }
            }
            let node = db::create_node(
                tc,
                workflow_id,
                parent_id,
                order_index,
                node_type,
                label,
                &config,
            )?;
            // First root: wire the workflow's root pointer.
            if parent_id.is_none() && wf.root_node_id.is_none() {
                db::set_workflow_root(tc, workflow_id, Some(&node.id))?;
            }
            let nodes = db::list_nodes(tc, workflow_id)?;
            let wf2 = db::get_workflow(tc, workflow_id)?
                .ok_or_else(|| TxErr::Rpc(unknown_workflow(workflow_id)))?;
            validate_tree(&nodes, wf2.root_node_id.as_deref())
                .map_err(|m| TxErr::Rpc(incoherent(m)))?;
            Ok((node, wf2))
        })
    });

    result
        .map(|(node, wf)| {
            ToolOutcome::changed(
                json!({ "node": node_json(&node), "workflow": wf }),
                ChangedTopic::Workflows,
            )
        })
        .map_err(map_tx_err)
}

/// `node_update` — `{ node_id, label?, config? }` → `{ node, warnings }`. Edits a node's authoring
/// fields (type is immutable here). At least one field is required; the tree is re-validated (a
/// config that is not a JSON object is refused). The ONE node mutation that is NOT refused under a
/// live run — it moves no node id — but it WARNS when one is walking the workflow (M4).
pub fn node_update(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let node_id = require_str(args, "node_id")?;
    let label = optional_str(args, "label")?;
    let config = config_string(args, "config")?;
    if label.is_none() && config.is_none() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "node_update needs at least one of 'label' / 'config'",
        ));
    }

    let result: Result<(db::Node, Vec<Value>), TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            let existing =
                db::get_node(tc, node_id)?.ok_or_else(|| TxErr::Rpc(unknown_node(node_id)))?;
            // S2: the node's workflow must be mutable (not a built-in default).
            require_mutable_workflow(tc, &existing.workflow_id)?;
            // M1: a new config must be executable for the node's (immutable) type.
            if let Some(cfg) = config.as_deref() {
                let cfg_value: Value = serde_json::from_str(cfg).unwrap_or_else(|_| json!({}));
                validate_node_config(&existing.type_, &cfg_value).map_err(TxErr::Rpc)?;
            }
            // M4: NOT refused — this edit moves no node id, so a live walk restores intact — but a
            // run walking the workflow will execute the new config from its next tick, and that is
            // said out loud rather than discovered.
            let warnings = walking_run_warnings(tc, &existing.workflow_id)?;
            let updated = db::update_node(tc, node_id, label, config.as_deref())?
                .ok_or_else(|| TxErr::Rpc(unknown_node(node_id)))?;
            let nodes = db::list_nodes(tc, &existing.workflow_id)?;
            let wf = db::get_workflow(tc, &existing.workflow_id)?
                .ok_or_else(|| TxErr::Rpc(unknown_workflow(&existing.workflow_id)))?;
            validate_tree(&nodes, wf.root_node_id.as_deref())
                .map_err(|m| TxErr::Rpc(incoherent(m)))?;
            Ok((updated, warnings))
        })
    });

    result
        .map(|(node, warnings)| {
            ToolOutcome::changed(
                json!({ "node": node_json(&node), "warnings": warnings }),
                ChangedTopic::Workflows,
            )
        })
        .map_err(map_tx_err)
}

/// `node_remove` — `{ node_id }` → `{ removed, workflow_id }`. Deletes the node and (via the FK
/// cascade) its whole subtree; removing the root nulls the workflow's root pointer. The remaining
/// tree is re-validated. Refused while a run is WALKING the workflow (M4).
pub fn node_remove(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let node_id = require_str(args, "node_id")?;

    let result: Result<String, TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            let existing =
                db::get_node(tc, node_id)?.ok_or_else(|| TxErr::Rpc(unknown_node(node_id)))?;
            // S2: refuse deleting a node of a built-in default's tree.
            require_mutable_workflow(tc, &existing.workflow_id)?;
            // M4: the node (or its subtree) may be exactly where a live run stands — refuse.
            require_no_walking_run(tc, &existing.workflow_id, "removing a node")?;
            let wf_id = existing.workflow_id.clone();
            db::delete_node(tc, node_id)?;
            let nodes = db::list_nodes(tc, &wf_id)?;
            let wf = db::get_workflow(tc, &wf_id)?
                .ok_or_else(|| TxErr::Rpc(unknown_workflow(&wf_id)))?;
            validate_tree(&nodes, wf.root_node_id.as_deref())
                .map_err(|m| TxErr::Rpc(incoherent(m)))?;
            Ok(wf_id)
        })
    });

    result
        .map(|wf_id| {
            ToolOutcome::changed(
                json!({ "removed": true, "workflow_id": wf_id }),
                ChangedTopic::Workflows,
            )
        })
        .map_err(map_tx_err)
}

/// `node_reorder` — `{ node_id, order_index, parent_id? }` → `{ node }`. Sets the node's sibling
/// `order_index`; a given `parent_id` REPARENTS it (kept otherwise). The tree is re-validated — a
/// reparent that would create a CYCLE (moving a node under its own descendant) is refused, and so is
/// any move while a run is WALKING the workflow (M4).
pub fn node_reorder(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let node_id = require_str(args, "node_id")?;
    let order_index = require_i32(args, "order_index")?;
    let new_parent_arg = optional_str(args, "parent_id")?;

    let result: Result<db::Node, TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            let existing =
                db::get_node(tc, node_id)?.ok_or_else(|| TxErr::Rpc(unknown_node(node_id)))?;
            // S2: refuse reordering/reparenting within a built-in default's tree.
            require_mutable_workflow(tc, &existing.workflow_id)?;
            // M4: moving a node re-numbers the siblings a live walk is counting on — refuse.
            require_no_walking_run(tc, &existing.workflow_id, "moving a node")?;
            let new_parent: Option<String> = match new_parent_arg {
                Some(p) => {
                    let pnode = db::get_node(tc, p)?.ok_or_else(|| {
                        TxErr::Rpc(RpcError::new(
                            RpcCode::InvalidId,
                            format!("unknown parent node {p}"),
                        ))
                    })?;
                    if pnode.workflow_id != existing.workflow_id {
                        return Err(TxErr::Rpc(incoherent(format!(
                            "parent node {p} belongs to another workflow"
                        ))));
                    }
                    Some(p.to_string())
                }
                None => existing.parent_id.clone(),
            };
            let updated = db::set_node_position(tc, node_id, new_parent.as_deref(), order_index)?
                .ok_or_else(|| TxErr::Rpc(unknown_node(node_id)))?;
            let nodes = db::list_nodes(tc, &existing.workflow_id)?;
            let wf = db::get_workflow(tc, &existing.workflow_id)?
                .ok_or_else(|| TxErr::Rpc(unknown_workflow(&existing.workflow_id)))?;
            validate_tree(&nodes, wf.root_node_id.as_deref())
                .map_err(|m| TxErr::Rpc(incoherent(m)))?;
            Ok(updated)
        })
    });

    result
        .map(|node| ToolOutcome::changed(json!({ "node": node_json(&node) }), ChangedTopic::Workflows))
        .map_err(map_tx_err)
}

// --- Read verbs (S1) + delete (S4) -----------------------------------------

/// `workflow_list` — `{ project_id? }` → `{ workflows }`. The workflows an agent can run / fork /
/// edit: WITH `project_id`, the project's OWN workflows PLUS the built-in defaults it falls back to;
/// WITHOUT, just the built-ins. A pure READ — the authoring surface used to be write-only, so an
/// agent could not even discover what workflows exist (v2.8 review S1).
pub fn workflow_list(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let project_id = optional_str(args, "project_id")?;
    let workflows = db
        .with_conn(|c| -> Result<Vec<db::Workflow>, diesel::result::Error> {
            let mut list = Vec::new();
            if let Some(p) = project_id {
                list.extend(db::list_workflows(c, Some(p))?);
            }
            list.extend(db::list_workflows(c, None)?);
            Ok(list)
        })
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(json!({ "workflows": workflows })))
}

/// `workflow_get` — `{ workflow_id }` → `{ workflow, nodes }`. The workflow's metadata AND its whole
/// node tree — the read that hands an agent the `node_id`s that `node_update` / `node_remove` /
/// `node_reorder` require (there is no separate `node_list`: it would be a strict subset of this).
/// A pure READ (v2.8 review S1).
pub fn workflow_get(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workflow_id = require_str(args, "workflow_id")?;
    let (wf, nodes) = db
        .with_conn(|c| -> Result<_, diesel::result::Error> {
            let wf = db::get_workflow(c, workflow_id)?;
            let nodes = db::list_nodes(c, workflow_id)?;
            Ok((wf, nodes))
        })
        .map_err(internal_db)?;
    let wf = wf.ok_or_else(|| unknown_workflow(workflow_id))?;
    Ok(ToolOutcome::read(
        json!({ "workflow": wf, "nodes": nodes_json(&nodes) }),
    ))
}

/// `run_list` — `{ workflow_id }` → `{ runs }`. A workflow's runs, newest first (each with its
/// cursor, blackboard and status) — the read behind "which runs exist / what state are they in".
/// A pure READ (v2.8 review S1).
pub fn run_list(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workflow_id = require_str(args, "workflow_id")?;
    let runs = db
        .with_conn(|c| db::list_workflow_runs(c, workflow_id))
        .map_err(internal_db)?;
    Ok(ToolOutcome::read(json!({ "runs": runs })))
}

/// `workflow_delete` — `{ workflow_id }` → `{ deleted, workflow_id }`. Deletes a PROJECT-OWNED
/// workflow and (via the FK cascade) its nodes + runs — the way to clean up drafts and failed forks.
/// A BUILT-IN default (`project_id IS NULL`) is REFUSED ([`require_mutable_workflow`]): deleting the
/// shared default would break every project (v2.8 review S4, guarded by S2). So is a workflow a run
/// is still WALKING — the cascade would take that run down with it, silently (M4).
pub fn workflow_delete(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let workflow_id = require_str(args, "workflow_id")?;
    let result: Result<(), TxErr> = db.with_conn(|c| {
        c.transaction(|tc| {
            require_mutable_workflow(tc, workflow_id)?;
            // M4: the cascade would take a live run down with the workflow, without a word.
            require_no_walking_run(tc, workflow_id, "deleting the workflow")?;
            db::delete_workflow(tc, workflow_id)?;
            Ok(())
        })
    });
    result
        .map(|()| {
            ToolOutcome::changed(
                json!({ "deleted": true, "workflow_id": workflow_id }),
                ChangedTopic::Workflows,
            )
        })
        .map_err(map_tx_err)
}

/// `run_delete` — `{ run_id }` → `{ deleted, run_id }`. Delete ONE run by id — the way to clean up a
/// run opened by mistake, INCLUDING a run on a BUILT-IN flow (which `workflow_delete` cannot reach,
/// since the built-in itself is immutable). A run is a leaf, so nothing cascades. An unknown id is
/// `invalid_id` (v2.8 review N5).
///
/// # Not while a tick is walking it (v2.8 L1)
///
/// `nyx_next` holds a run for the whole of its read-modify-write, which is as long as its `gate_check`
/// takes — up to [`crate::mcp_engine::GATE_TOTAL_TIMEOUT`]. The MCP surface serves its requests on a
/// worker pool now, so this verb and that tick genuinely overlap; one accept thread used to keep them
/// apart by accident. Deleting the row underneath a walking tick makes the tick's own `persist` match
/// nothing: nyx has already run the gate and imposed the transition, and it would hand the dispatcher
/// the resulting action for a run that no longer exists.
///
/// So this takes the SAME tick guard and REFUSES while a tick holds it. The collision is not
/// hypothetical: `run_delete` is exactly what nyx's own refusals tell a caller to do about a live run
/// ("let it finish, or drop it with run_delete"), so both gestures are aimed at the same moment by
/// design. The refusal is bounded — the tick it waits on is itself bounded — and the caller simply
/// calls again.
pub fn run_delete(
    db: &Db,
    runtime: &crate::mcp_engine::EngineRuntime,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let run_id = require_str(args, "run_id")?;
    let Some(_tick) = runtime.enter_tick(run_id) else {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "run '{run_id}' is being advanced by a nyx_next call right now, and deleting it \
                 under that tick would throw away what the tick already did — a gate it ran, a \
                 transition it imposed — with no way left to say so. NOTHING was deleted: let the \
                 tick return (it is bounded) and call run_delete again"
            ),
        )
        .with_data(json!({ "reason": "run_busy", "run_id": run_id })));
    };
    let deleted = db
        .with_conn(|c| db::delete_workflow_run(c, run_id))
        .map_err(internal_db)?;
    if !deleted {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!("unknown workflow run {run_id}"),
        ));
    }
    // The row is gone; drop its resident engine with it, or the registry keeps the deleted run's
    // whole tree + blackboard and still counts it among the runs this session is driving.
    runtime.forget(run_id);
    Ok(ToolOutcome::changed(
        json!({ "deleted": true, "run_id": run_id }),
        ChangedTopic::Workflows,
    ))
}

// --- Sub-dispatcher --------------------------------------------------------

/// Route an authoring tool by name; `None` when `name` is not one of ours (the caller falls through
/// to the rest of the surface). A new tool is added in exactly this one place.
pub fn dispatch_authoring_tool(
    db: &Db,
    name: &str,
    args: &Value,
) -> Option<Result<ToolOutcome, RpcError>> {
    match name {
        WORKFLOW_CREATE_TOOL => Some(workflow_create(db, args)),
        WORKFLOW_FORK_TOOL => Some(workflow_fork(db, args)),
        WORKFLOW_ASSIGN_TOOL => Some(workflow_assign(db, args)),
        SLOT_BIND_TOOL => Some(slot_bind(db, args)),
        NODE_ADD_TOOL => Some(node_add(db, args)),
        NODE_UPDATE_TOOL => Some(node_update(db, args)),
        NODE_REMOVE_TOOL => Some(node_remove(db, args)),
        NODE_REORDER_TOOL => Some(node_reorder(db, args)),
        WORKFLOW_LIST_TOOL => Some(workflow_list(db, args)),
        WORKFLOW_GET_TOOL => Some(workflow_get(db, args)),
        RUN_LIST_TOOL => Some(run_list(db, args)),
        WORKFLOW_DELETE_TOOL => Some(workflow_delete(db, args)),
        // The one call site that reaches the process-global engine registry, exactly as the live
        // `nyx_next` dispatch does — so both gestures contend on the SAME registry in production,
        // while `run_delete` itself stays injectable for a test.
        RUN_DELETE_TOOL => Some(run_delete(db, crate::mcp_engine::shared_runtime(), args)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
