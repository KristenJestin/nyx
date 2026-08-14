//! The BEHAVIOR-TREE INTERPRETER — the deterministic engine that walks a stored workflow tree,
//! executes each typed node, advances the graph and mutates the run's blackboard (v2.8 — Harnais :
//! moteur d'orchestration, Phase 2, task 01KY71J03W9GST9050G96HCM53).
//!
//! ## What this module is (and is not)
//!
//! The graph is DATA — the [`crate::db`] `workflows` / `nodes` / `workflow_runs` rows. This module
//! is the CODE that gives those rows behaviour: it reconstructs the PURE tree from the rows
//! ([`Tree::from_rows`]) and TICKS it ([`Engine::tick`]). Each tick either hands the thin dispatcher
//! ONE atomic [`Action`] to perform (spawn a sub-agent, run a gate, judge a question, …) and then
//! SUSPENDS until the dispatcher reports the [`NodeResult`] back, or reports the run is [`Step::Done`].
//! This is the exact shape the P1 spike (`tests/nyx_next_loop_spike.rs`) proved end-to-end, generalised
//! to the full node vocabulary.
//!
//! It is DELIBERATELY pure: it does NOT run a real PTY gate, spawn a real sub-agent, or transition a
//! Spec — those effects belong to the `nyx_next` verb handler (task T4), which wires the real
//! primitives (`command::CommandPty`, the native `Task` sub-agent, `spec_transition`) into the loop
//! AROUND this interpreter and persists each tick's cursor + blackboard through the run CRUD. Keeping
//! the graph walk pure is what makes it exhaustively unit-testable here.
//!
//! ## The node model (PRD behavior tree)
//!
//!   * COMPOSITES drive the flow, internally, without a round-trip:
//!     - `sequence` — run the children in `order_index` order.
//!     - `loop` — run the body (the children, as a sequence) repeatedly UNTIL its `until` condition
//!       (a verdict read from the blackboard) holds; the body always runs at least once (repeat-until).
//!     - `selector` — check each branch's `cond` against the blackboard and descend into the first
//!       matching branch's child (a `cond: null` branch is the default/else).
//!   * LEAVES need the dispatcher: each yields an [`Action`] and the engine suspends until the reported
//!     [`NodeResult`] comes back on the next tick (which updates the blackboard):
//!     - fully executed here (task T3 scope): `spawn_subagent`, `gate_check`, `judge`, `transition`,
//!       `wait`, `note`.
//!     - recognised, DEFINED advance behaviour, minimal semantics (ARBITRAGE — see below): `ask_human`,
//!       `produce`. They yield an action and advance on the reported result like any other leaf, but
//!       their deeper semantics (blocking on a real human decision / persisting a structured artefact
//!       through a nyx verb) are out of Phase-2 scope. They are recognised so a tree containing them
//!       still walks; the effective execution is a later phase.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::db;

/// The multi-provider `spawn_subagent` ADAPTER (task 01KY71J1Y5BXR8K8HV3KRV9PHT): native `Task`
/// on Claude, inline fallback where a provider has no sub-agents, plus git-diff result extraction.
pub mod spawn;

/// Safety bound on a `loop` node: how many times its body may run before the engine gives up with
/// [`EngineError::LoopLimit`]. A real `until` verdict exits long before this; the cap only stops a
/// mis-authored never-true loop from spinning forever (the P1 spike used the same idea with a
/// `max_spawns` guard). Overridable per node via `LoopConfig.max_iterations`.
///
/// A backstop, not a budget: a `loop` whose `until` never becomes true (e.g. a judge that never
/// returns the awaited verdict — `last_result` absent ⇒ `verdict: null` ⇒ the condition never holds)
/// would otherwise spawn a sub-agent EVERY iteration up to this cap. 100 was far too high for a loop
/// that spawns real sub-agents; 25 is a generous phase count while still cutting a runaway short
/// (v2.8 review m6). A workflow that genuinely needs more sets `LoopConfig.max_iterations` per node.
///
/// The LAST resort, not the first: a loop whose exit reads a value the run can no longer change dies
/// on its FIRST round instead ([`Engine::freeze_key`]). Paying the cap's worth of body passes — a
/// sub-agent each — to learn what one pass already settled is exactly what the cap cannot prevent.
pub const DEFAULT_MAX_LOOP_ITERATIONS: u32 = 25;

/// The blackboard key a `judge` verdict lands under by default (a `loop`'s `until` / a `selector`'s
/// `cond` reads it). Overridable per node via `JudgeConfig.output_key`.
pub const DEFAULT_JUDGE_OUTPUT_KEY: &str = "verdict";

/// The blackboard key an `ask_human` decision lands under by default (a following `selector`'s `cond`
/// / an engaging `transition`'s `gate_on` reads it). Overridable per node via `output_key`.
pub const DEFAULT_ASK_HUMAN_OUTPUT_KEY: &str = "human";

/// The blackboard key a `transition_task` reads its target task id from by default. Overridable per
/// node via `TransitionTaskConfig.task_key` (v2.8 lot 2).
pub const DEFAULT_TASK_KEY: &str = "task_id";

/// The RUN-SCOPED fact that authorizes ONE Spec-section write on a Spec whose sections are otherwise
/// FROZEN (H12). The freeze stays the rule — the human is its only exception — so this key is written
/// by the ENGINE ALONE, from a human's answer to an `ask_human` that declares
/// [`AskHumanConfig::grant_spec_write_on`], and is dropped again by a `note {revoke_spec_write}`.
/// `spec_update_section` requires it (plus the `run_id` naming this very run) instead of trusting a
/// caller's word, so the authority to write is the HUMAN's answer and never an agent's claim about it.
///
/// The double-underscore name is deliberate: dot-free (so [`Blackboard::get`]'s dot-path split reads
/// it as one flat key, like `__child_run_id__`) and RESERVED — authoring REFUSES a node that writes it
/// ([`crate::mcp_authoring`]), so no graph can hand itself the grant by naming this key as an
/// `output_key`.
pub const SPEC_WRITE_GRANT_KEY: &str = "__spec_write_granted__";

/// The blackboard key a `loop` records its completed-pass count under — as an OBJECT keyed by the
/// loop's NODE ID (`loop_iter.<node id>`, read back by dot-path), never one number for the whole run:
/// a graph nests loops (ndev runs an audit/rework loop inside its phases loop) and siblings run one
/// after another, so a flat counter described whichever loop finished a pass last.
pub const LOOP_ITER_KEY: &str = "loop_iter";

// ============================================================================
// Node types
// ============================================================================

/// A behavior-tree node type — the FULL PRD vocabulary (3 composites + 8 leaves). Mirrors the DB
/// `nodes.type` CHECK ([`db::NODE_TYPES`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    // Composites.
    Sequence,
    Loop,
    Selector,
    // Leaves (fully executed in T3 scope).
    SpawnSubagent,
    GateCheck,
    Judge,
    Transition,
    Wait,
    Note,
    /// Reads a verifiable nyx fact (a Spec status, …) to the blackboard — a nyx-own inline effect,
    /// like GateCheck / Transition (v2.8 review B2).
    Query,
    /// Forks the run's bound Spec into a new editable draft revision — a nyx-own inline effect gated
    /// on a preceding ask_human (v2.8 review N13).
    ForkSpec,
    /// Marks a task through the guarded task lifecycle (start/done/block/skip) — a nyx-own inline
    /// effect, the task-level twin of [`NodeType::Transition`]. UNGATED by default (v2.8 lot 2).
    TransitionTask,
    /// Opens a CHILD run of another flow (a fresh sub-agent drives it) and folds the child's VERIFIED
    /// blackboard result back onto the parent board — the one HYBRID leaf (a nyx-own inline effect
    /// that ALSO suspends the parent while the child runs) (v2.8 lot 2).
    SubWorkflow,
    // Leaves (recognised, minimal — ARBITRAGE, see module docs).
    AskHuman,
    Produce,
}

impl NodeType {
    /// Parse a DB `nodes.type` string, or `None` for an unknown value.
    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            db::NODE_TYPE_SEQUENCE => NodeType::Sequence,
            db::NODE_TYPE_LOOP => NodeType::Loop,
            db::NODE_TYPE_SELECTOR => NodeType::Selector,
            db::NODE_TYPE_SPAWN_SUBAGENT => NodeType::SpawnSubagent,
            db::NODE_TYPE_GATE_CHECK => NodeType::GateCheck,
            db::NODE_TYPE_JUDGE => NodeType::Judge,
            db::NODE_TYPE_TRANSITION => NodeType::Transition,
            db::NODE_TYPE_WAIT => NodeType::Wait,
            db::NODE_TYPE_NOTE => NodeType::Note,
            db::NODE_TYPE_QUERY => NodeType::Query,
            db::NODE_TYPE_FORK_SPEC => NodeType::ForkSpec,
            db::NODE_TYPE_TRANSITION_TASK => NodeType::TransitionTask,
            db::NODE_TYPE_SUB_WORKFLOW => NodeType::SubWorkflow,
            db::NODE_TYPE_ASK_HUMAN => NodeType::AskHuman,
            db::NODE_TYPE_PRODUCE => NodeType::Produce,
            _ => return None,
        })
    }

    /// The canonical DB `nodes.type` string.
    pub fn as_db(&self) -> &'static str {
        match self {
            NodeType::Sequence => db::NODE_TYPE_SEQUENCE,
            NodeType::Loop => db::NODE_TYPE_LOOP,
            NodeType::Selector => db::NODE_TYPE_SELECTOR,
            NodeType::SpawnSubagent => db::NODE_TYPE_SPAWN_SUBAGENT,
            NodeType::GateCheck => db::NODE_TYPE_GATE_CHECK,
            NodeType::Judge => db::NODE_TYPE_JUDGE,
            NodeType::Transition => db::NODE_TYPE_TRANSITION,
            NodeType::Wait => db::NODE_TYPE_WAIT,
            NodeType::Note => db::NODE_TYPE_NOTE,
            NodeType::Query => db::NODE_TYPE_QUERY,
            NodeType::ForkSpec => db::NODE_TYPE_FORK_SPEC,
            NodeType::TransitionTask => db::NODE_TYPE_TRANSITION_TASK,
            NodeType::SubWorkflow => db::NODE_TYPE_SUB_WORKFLOW,
            NodeType::AskHuman => db::NODE_TYPE_ASK_HUMAN,
            NodeType::Produce => db::NODE_TYPE_PRODUCE,
        }
    }

    /// Whether this node is a COMPOSITE (drives flow via its children) vs a LEAF (yields an action).
    pub fn is_composite(&self) -> bool {
        matches!(
            self,
            NodeType::Sequence | NodeType::Loop | NodeType::Selector
        )
    }
}

// ============================================================================
// Typed configs (decoded per node kind)
// ============================================================================

/// `spawn_subagent` config — `{agent_id}` (PRD Impl Decisions). Lenient: a missing field defaults so
/// a partially-authored node still walks.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SpawnConfig {
    #[serde(default)]
    pub agent_id: String,
    /// FORCE the inline spawn mode (the effector runs IN the dispatcher's session, keeping user
    /// access) regardless of provider — for an INTERACTIVE effector (e.g. the spec-author grill) that
    /// a blind Task sub-agent cannot serve (v2.8 review N2). Default false = the provider-resolved mode.
    #[serde(default)]
    pub inline: bool,
    /// The per-node SUBJECT handed to the sub-agent: WHAT this particular spawn is about, when the
    /// SAME role is spawned several times over different objects. The graph names the focus at the
    /// NODE (e.g. four `auditor` spawns, one per Spec commitment section — `user_stories`,
    /// `testing_decisions`, …), instead of multiplying near-identical roles or letting the agent pick
    /// its own angle. OPTIONAL: a spawn without one is unchanged (the role's manual stands alone).
    #[serde(default)]
    pub subject: Option<String>,
}

/// `gate_check` config — `{command_ids | slot, blocking}`. `blocking` defaults TRUE (a gate is
/// blocking by nature; the T4 handler refuses to advance while a blocking gate is red).
///
/// A gate names its commands ONE of two ways, and which one is a property of the GRAPH it lives in:
///
///   * `command_ids` — the managed-command instance ids, named by the node itself. Legitimate in a
///     graph that OWNS them: a project's own flow, a custom one.
///   * `slot` — the NAME of what must be verified ("build_check"), bound per project
///     ([`crate::db::bind_command_slot`]). This is what a SHARED graph uses: no graph every project
///     runs can name `bun test`, and a built-in that tried had to be FORKED to be armed — which
///     froze it at that day's revision and cut the project off from every later fix.
///
/// The engine carries both to the effects layer, which resolves them through ONE path
/// (`crate::mcp_engine`'s gate resolution) so the gate has a single notion of "the commands to run".
/// Authoring refuses a node that sets both.
#[derive(Debug, Clone, Deserialize)]
pub struct GateConfig {
    #[serde(default)]
    pub command_ids: Vec<String>,
    /// The SLOT this gate declares, resolved against the run's project (`None` = the node names its
    /// own `command_ids`).
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default = "default_true")]
    pub blocking: bool,
}

/// `judge` config — `{prompt, output_schema}` + an optional `output_key` (the blackboard key the
/// verdict lands under; defaults to [`DEFAULT_JUDGE_OUTPUT_KEY`]).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct JudgeConfig {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub output_key: Option<String>,
}

/// The OUTPUT-ROUTING fields shared by the RESULT-producing leaves (judge / ask_human / produce): a
/// `output_key` (the blackboard key the result lands under; each leaf keeps its own default when
/// omitted) and `append` (PUSH onto a JSON array at that key instead of overwriting — lets a loop
/// ACCUMULATE results rather than clobber them). Parsed as a lenient subset of any leaf's config, so
/// omitting both is the exact pre-N9 behaviour (fixed key, overwrite) — fully backward-compatible.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutputConfig {
    #[serde(default)]
    pub output_key: Option<String>,
    #[serde(default)]
    pub append: bool,
}

/// `transition` config — `{target_state}` (the Spec state the T4 handler transitions to).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TransitionConfig {
    #[serde(default)]
    pub target_state: String,
    /// For an ENGAGING transition (ready / done / cancel), the blackboard key a PRECEDING `ask_human`
    /// wrote its approval to — the transition fires only when that key is truthy (v2.8 review B3).
    /// Ignored for an ENTRY transition (`start`, consent from the launch) and `in_review`.
    #[serde(default)]
    pub gate_on: Option<String>,
}

/// `note` config — `{text, clear_gate_block?}` (free text injected into the dispatcher = trappe).
/// `clear_gate_block` (M5) marks the note as a graph-expressible ABANDON: reaching it makes the
/// `nyx_next` handler CLEAR the run's red-gate anchor ([`crate::mcp_engine`]'s `BLOCKING_GATE_RED_KEY`)
/// — every anchored blocking gate of the run — so a downstream `done` no longer refuses on the stale
/// block. The block clears ONLY on an EXPLICIT route through such a node, never implicitly.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NoteConfig {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub clear_gate_block: bool,
    /// CLOSES the Spec-write window a preceding `ask_human` opened ([`SPEC_WRITE_GRANT_KEY`], H12):
    /// reaching this note drops the grant from the run's board. The graph therefore SHOWS the window
    /// — it opens at the human's answer and closes on this note, right after the spawn that was
    /// authorized — instead of leaving a run-long permission behind on the board.
    #[serde(default)]
    pub revoke_spec_write: bool,
}

/// `query` config — `{query, write}`: the NAME of the nyx fact to read (`spec_status`, …) and the
/// blackboard key to write it under. The T4 handler resolves `query` against real nyx state and folds
/// the result onto the blackboard, so a following `selector`/`loop` branches on a VERIFIED fact rather
/// than an LLM's claim (v2.8 review B2).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QueryConfig {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub write: String,
    /// For a PARAMETERIZED fact (`spec_can_transition`), the transition target it asks about
    /// (`ready` / `done` / …) — the T4 handler answers whether the bound Spec CAN take it, and writes
    /// the reason to `<write>_reason` (v2.8 review `spec_can_transition`).
    #[serde(default)]
    pub target: Option<String>,
    /// For a TASK-GRAINED fact (`task_close_route`), the blackboard key holding the task id it reads
    /// (default `task_id`) — the same naming a `transition_task` node uses, so the fact and the
    /// transition that acts on it can never be told about two different tasks (H9).
    #[serde(default)]
    pub task_key: Option<String>,
}

/// `ask_human` config — `{prompt, output_key?, append?, options?}`. `options` is the CLOSED set of
/// answers the human may give (v2.8 review N19): nyx returns them WITH the action (the dispatcher
/// proposes them, it does not invent) and REFUSES an answer outside the set (re-asking), so a
/// downstream `selector` never silently falls through on an off-vocabulary reply. Empty = free-form
/// (the pre-N19 behaviour). The `output_key`/`append` routing is parsed separately via [`OutputConfig`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AskHumanConfig {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// THE ONE EXCEPTION TO THE SPEC FREEZE, and it is spelled here because it must come from a HUMAN
    /// and from nothing else (H12). When the human's answer to THIS question equals this value, the
    /// engine writes [`SPEC_WRITE_GRANT_KEY`] on the run's board — the fact `spec_update_section`
    /// requires to write a section of a Spec that has left its writable state. The graph never claims
    /// the grant, the effector never asks for it: it is DERIVED from the answer, so the day sensitive
    /// answers come from the nyx UI instead of an agent's message, this becomes unforgeable with them.
    #[serde(default)]
    pub grant_spec_write_on: Option<String>,
}

/// `fork_spec` config — `{gate_on}`: the blackboard key a PRECEDING `ask_human` wrote its approval to.
/// The fork (a sensitive Spec mutation) fires ONLY when that key is truthy — the human confirmation
/// routed through the graph, never implicit (v2.8 review N13). Same gating shape as an engaging
/// transition (B3).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForkSpecConfig {
    #[serde(default)]
    pub gate_on: Option<String>,
}

/// `transition_task` config — `{transition, task_key?, reason?, gate_on?}`. `transition` is the task
/// lifecycle move (`start`/`done`/`block`/`skip`); `task_key` NAMES the blackboard key holding the
/// task id (default `task_id`); `reason` rides a block/skip; `gate_on` is an OPTIONAL affirmative
/// blackboard key (UNGATED when absent — the effect fires unconditionally). The task-level twin of
/// [`TransitionConfig`] (v2.8 lot 2).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TransitionTaskConfig {
    #[serde(default)]
    pub transition: String,
    #[serde(default)]
    pub task_key: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub gate_on: Option<String>,
}

/// ONE declared blackboard COPY `{from, to}` — read the parent board's `from` key, write it under `to`
/// on the other board. The explicit inject/retrieve seam of a `sub_workflow` node (v2.8 lot 2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BoardCopy {
    pub from: String,
    pub to: String,
}

/// `sub_workflow` config — `{scope, workflow_id?, inject, retrieve, output_key?}`. The child target is
/// resolved BY SCOPE (`scope`) unless an explicit `workflow_id` overrides it. `inject` seeds the
/// ISOLATED child board from the parent board (the ONLY keys the child sees); `retrieve` folds the
/// child's VERIFIED result keys back onto the parent board when the child finishes. `output_key`
/// routes the summary [`NodeResult::SubResult`] (defaults to `sub_result`) (v2.8 lot 2).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubWorkflowConfig {
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub inject: Vec<BoardCopy>,
    #[serde(default)]
    pub retrieve: Vec<BoardCopy>,
    #[serde(default)]
    pub output_key: Option<String>,
}

/// `loop` config — `{until}` (PRD: the condition is a VERDICT read from the blackboard) + a safety
/// `max_iterations`. An absent `until` is treated as "exit after one body pass" (a safe non-looping
/// default rather than an infinite loop).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LoopConfig {
    #[serde(default)]
    pub until: Option<Condition>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
}

/// `selector` config — `{branches:[{cond, child}]}`. Each branch guards a child; the FIRST branch
/// whose `cond` matches is taken (`cond: null` = default/else).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SelectorConfig {
    #[serde(default)]
    pub branches: Vec<SelectorBranch>,
}

/// One `selector` branch — a guard `cond` (`None` = default match) and the `child` it selects
/// (`child` = the index into the selector's ordered children; when absent it defaults to the branch's
/// own position, so a straight `branches`↔`children` alignment needs no explicit index).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SelectorBranch {
    #[serde(default)]
    pub cond: Option<Condition>,
    #[serde(default)]
    pub child: Option<usize>,
}

fn default_true() -> bool {
    true
}

// ============================================================================
// Conditions (loop `until` / selector `cond`), checked over the blackboard
// ============================================================================

/// A boolean test over the run's blackboard — the `until` of a `loop` and the `cond` of a `selector`
/// branch. Small on purpose: name a blackboard `var` (a dot-path) and compare it. The tests are
/// combined as AND when several are present; `always: true` short-circuits to true (the default/else
/// branch of a selector). A bare `var` with no comparator means "the value is truthy/present".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Condition {
    /// The blackboard key (dot-path, e.g. `"verdict"` or `"gate.green"`) the test reads.
    #[serde(default)]
    pub var: Option<String>,
    /// Matches when `var`'s value equals this.
    #[serde(default)]
    pub eq: Option<Value>,
    /// Matches when `var`'s value does NOT equal this.
    #[serde(default)]
    pub ne: Option<Value>,
    /// `true` matches when `var`'s value is TRUTHY (`true`, a non-empty string/array, a non-zero
    /// number); `false` matches when it is FALSY (absent / `null` / `false` / empty / zero).
    #[serde(default)]
    pub truthy: Option<bool>,
    /// Unconditional match (the default/else branch of a selector). Wins over every other field.
    #[serde(default)]
    pub always: Option<bool>,
}

impl Condition {
    /// The condition in ONE human sentence — `design_locked == true`, `phase_open_tasks is falsy`.
    /// A loop that fails to exit reports itself with this: the error message is the ONLY thing the
    /// user gets, and a node id alone says neither what was awaited nor why it never came.
    pub fn describe(&self) -> String {
        if self.always == Some(true) {
            return "always".to_string();
        }
        let Some(var) = self.var.as_deref() else {
            return "nothing (neither a var nor 'always')".to_string();
        };
        let mut tests = Vec::new();
        if let Some(expected) = &self.eq {
            tests.push(format!("{var} == {expected}"));
        }
        if let Some(unexpected) = &self.ne {
            tests.push(format!("{var} != {unexpected}"));
        }
        if let Some(want_truthy) = self.truthy {
            tests.push(format!(
                "{var} is {}",
                if want_truthy { "truthy" } else { "falsy" }
            ));
        }
        if tests.is_empty() {
            tests.push(format!("{var} is truthy"));
        }
        tests.join(" and ")
    }

    /// Test the condition against `board`. `always: true` ⇒ true. Otherwise every present test
    /// (`eq`/`ne`/`truthy`, and a bare `var`) must pass (AND). A condition with no `var` and no
    /// `always` is `false` (nothing to test).
    pub fn matches(&self, board: &Blackboard) -> bool {
        if self.always == Some(true) {
            return true;
        }
        let Some(var) = self.var.as_deref() else {
            return false;
        };
        let value = board.get(var);
        let mut any_test = false;
        if let Some(expected) = &self.eq {
            any_test = true;
            if value != Some(expected) {
                return false;
            }
        }
        if let Some(unexpected) = &self.ne {
            any_test = true;
            if value == Some(unexpected) {
                return false;
            }
        }
        // `truthy` is a two-way test: `true` demands a truthy value, `false` demands a falsy one
        // (an authored `{"var": "gate_green", "truthy": false}` means "the gate is NOT green").
        if let Some(want_truthy) = self.truthy {
            any_test = true;
            if is_truthy(value) != want_truthy {
                return false;
            }
        }
        // A bare `var` (no comparator) tests truthiness/presence.
        if !any_test {
            return is_truthy(value);
        }
        true
    }
}

/// JSON truthiness: `true`, a non-empty string, a non-zero number, a non-empty array/object. `null`
/// and absent are falsy.
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// ============================================================================
// Blackboard
// ============================================================================

/// The run's blackboard — a JSON object scratchpad the interpreter reads/writes (last gate verdict,
/// phase/loop index, spawn count, last diff, …). Mirrors `workflow_runs.blackboard`; the T4 handler
/// persists [`Blackboard::to_json_string`] each tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Blackboard {
    map: Map<String, Value>,
}

impl Blackboard {
    /// A fresh, empty blackboard.
    pub fn new() -> Self {
        Blackboard { map: Map::new() }
    }

    /// Parse a blackboard from a `workflow_runs.blackboard` JSON string. A non-object (or empty
    /// string) yields an empty board.
    pub fn from_json_str(s: &str) -> Self {
        match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(map)) => Blackboard { map },
            _ => Blackboard::new(),
        }
    }

    /// Serialize to the JSON object string stored in `workflow_runs.blackboard`.
    pub fn to_json_string(&self) -> String {
        Value::Object(self.map.clone()).to_string()
    }

    /// Read a value by dot-path (`"a.b.c"`), or `None` when absent.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut cur: Option<&Value> = None;
        for (i, key) in path.split('.').enumerate() {
            cur = if i == 0 {
                self.map.get(key)
            } else {
                cur.and_then(|v| v.get(key))
            };
            cur?;
        }
        cur
    }

    /// Set a TOP-LEVEL key (the interpreter's convenience keys are all top-level).
    pub fn set(&mut self, key: &str, value: Value) {
        self.map.insert(key.to_string(), value);
    }

    /// Remove a TOP-LEVEL key (the mirror of [`Blackboard::set`]), so a cleared flag leaves no
    /// residue on the persisted board.
    pub fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    /// Write `value` under `scope` INSIDE the object at a top-level `key`, creating that object when
    /// the key is absent (or holds something else) — so a per-node counter reads back by dot-path
    /// (`key.scope`, [`Blackboard::get`]) with no new resolution rule.
    ///
    /// This is what a value belonging to ONE node needs: a flat key holds a single value for the whole
    /// run, so nested and sibling loops writing `loop_iter` clobbered each other's count — ndev's
    /// inner audit/rework loop overwrote it on every pass, the outer phases loop overwrote it again
    /// when its body finished, and the number a condition (or a human reading the board) saw was right
    /// for none of them but the last to complete a pass.
    pub fn set_scoped(&mut self, key: &str, scope: &str, value: Value) {
        match self.map.get_mut(key) {
            Some(Value::Object(obj)) => {
                obj.insert(scope.to_string(), value);
            }
            _ => {
                let mut obj = Map::new();
                obj.insert(scope.to_string(), value);
                self.map.insert(key.to_string(), Value::Object(obj));
            }
        }
    }

    /// Increment a top-level integer counter (0 when absent), returning the new value.
    pub fn increment(&mut self, key: &str) -> i64 {
        let next = self.get(key).and_then(|v| v.as_i64()).unwrap_or(0) + 1;
        self.set(key, Value::from(next));
        next
    }

    /// APPEND `value` to the JSON array at a TOP-LEVEL `key`, creating `[value]` when the key is
    /// absent (or holds a non-array). This is what lets a LOOP ACCUMULATE its results — a grill's Q&A
    /// history, successive artifacts — instead of each turn overwriting the last (v2.8 review N9).
    pub fn append(&mut self, key: &str, value: Value) {
        match self.map.get_mut(key) {
            Some(Value::Array(arr)) => arr.push(value),
            _ => {
                self.map.insert(key.to_string(), Value::Array(vec![value]));
            }
        }
    }

    /// The underlying JSON object (read-only).
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.map
    }
}

/// Interpolate `{{path}}` placeholders in a leaf's prompt/text from the blackboard (dot-paths, same
/// resolution as [`Blackboard::get`]). A string value inserts RAW; a non-string inserts its compact
/// JSON; an ABSENT path inserts nothing. A template with no `{{` is returned unchanged, so this is
/// backward-compatible for every existing prompt. This is what lets a LOOPED `ask_human`/`judge` ask
/// a QUESTION THAT MOVES with the run's state instead of a single frozen prompt (v2.8 review N9).
/// `pub(crate)` so the `sub_workflow` bubble-up can re-interpolate a CHILD's parked `ask_human` prompt
/// against the CHILD board when it re-emits it through the parent (Design A, v2.8 lot 2).
pub(crate) fn interpolate(template: &str, board: &Blackboard) -> String {
    if !template.contains("{{") {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let path = after[..end].trim();
                match board.get(path) {
                    Some(Value::String(s)) => out.push_str(s),
                    Some(v) => out.push_str(&v.to_string()),
                    None => {}
                }
                rest = &after[end + 2..];
            }
            // An unclosed `{{` is emitted verbatim (not a placeholder).
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ============================================================================
// Tree reconstruction (from DB rows)
// ============================================================================

/// A node in the reconstructed tree: its identity, type, label, decoded `config`, and its ORDERED
/// children (as arena indices). Built by [`Tree::from_rows`] from the flat `nodes` rows.
#[derive(Debug, Clone)]
pub struct EngineNode {
    pub id: String,
    pub node_type: NodeType,
    pub label: String,
    pub config: Value,
    pub children: Vec<usize>,
}

/// The reconstructed behavior tree — an arena of [`EngineNode`]s + the root index. Built PURELY from
/// the `nodes` rows (parent_id + order_index) — the done-criterion "arbre reconstructible".
#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<EngineNode>,
    root: usize,
}

impl Tree {
    /// Reconstruct the tree of a workflow from its `nodes` rows. `root_id` is the workflow's
    /// `root_node_id`; when `None`, the single row with `parent_id IS NULL` is taken as the root.
    ///
    /// Errors: an unknown node type, a config that is not a JSON object, a child pointing at a
    /// missing/foreign parent, no root (or an ambiguous root when `root_id` is `None`).
    pub fn from_rows(rows: &[db::Node], root_id: Option<&str>) -> Result<Tree, EngineError> {
        // Arena + id → index.
        let mut nodes: Vec<EngineNode> = Vec::with_capacity(rows.len());
        let mut index_of: HashMap<&str, usize> = HashMap::with_capacity(rows.len());
        for row in rows {
            let node_type = NodeType::from_db(&row.type_)
                .ok_or_else(|| EngineError::UnknownNodeType(row.type_.clone()))?;
            let config: Value =
                serde_json::from_str(&row.config).map_err(|e| EngineError::BadConfig {
                    node_id: row.id.clone(),
                    message: e.to_string(),
                })?;
            if !config.is_object() {
                return Err(EngineError::BadConfig {
                    node_id: row.id.clone(),
                    message: "config must be a JSON object".to_string(),
                });
            }
            index_of.insert(row.id.as_str(), nodes.len());
            nodes.push(EngineNode {
                id: row.id.clone(),
                node_type,
                label: row.label.clone(),
                config,
                children: Vec::new(),
            });
        }

        // Wire children onto their parent. Re-sort by (parent, order_index, child) defensively so the
        // tree is correct regardless of the caller's row ordering.
        let mut roots: Vec<usize> = Vec::new();
        let mut pending: Vec<(usize, i32, usize)> = Vec::new(); // (parent_idx, order_index, child_idx)
        for (child_idx, row) in rows.iter().enumerate() {
            match &row.parent_id {
                None => roots.push(child_idx),
                Some(parent_id) => {
                    let parent_idx = *index_of.get(parent_id.as_str()).ok_or_else(|| {
                        EngineError::DanglingChild {
                            node_id: row.id.clone(),
                            parent_id: parent_id.clone(),
                        }
                    })?;
                    pending.push((parent_idx, row.order_index, child_idx));
                }
            }
        }
        pending.sort_by_key(|&(parent, order, child)| (parent, order, child));
        for (parent_idx, _order, child_idx) in pending {
            nodes[parent_idx].children.push(child_idx);
        }

        // Resolve the root.
        let root = match root_id {
            Some(id) => *index_of
                .get(id)
                .ok_or_else(|| EngineError::MissingRoot(Some(id.to_string())))?,
            None => match roots.as_slice() {
                [single] => *single,
                [] => return Err(EngineError::MissingRoot(None)),
                _ => return Err(EngineError::MultipleRoots(roots.len())),
            },
        };

        // A CYCLE reachable from the root would make `Engine::tick` descend forever (it pushes a
        // frame per composite entry), so close it here: walk the tree from the root and refuse a
        // node that is entered twice. O(n) — a cycle NOT reachable from the root is never walked,
        // so it is left to the authoring layer's stricter `validate_tree`.
        let mut seen = vec![false; nodes.len()];
        let mut stack = vec![root];
        while let Some(idx) = stack.pop() {
            if seen[idx] {
                return Err(EngineError::Cycle {
                    node_id: nodes[idx].id.clone(),
                });
            }
            seen[idx] = true;
            stack.extend(nodes[idx].children.iter().copied());
        }

        Ok(Tree { nodes, root })
    }

    /// The root node's id.
    pub fn root_id(&self) -> &str {
        &self.nodes[self.root].id
    }

    /// The arena index of the node with this id, or `None` when the tree has no such node — the
    /// lookup [`Engine::restore`] uses to rebuild a persisted traversal (keyed by node id) and to
    /// REFUSE a resume whose traversal references a node the edited tree no longer holds (B1).
    fn index_of(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// The number of nodes in the tree.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree has no nodes (it never does once built — a tree always has a root — but the
    /// accessor keeps clippy's `len_without_is_empty` happy).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// WHAT the `ask_human` node `node_id` COMMITS when it is answered — `None` when the answer
    /// commits nothing a human alone may commit, and the question is an ordinary one (H6, then J2).
    ///
    /// DERIVED, never declared, and derived FROM WHAT THE ANSWER COMMITS rather than from which node
    /// it is. A list of sensitive labels would drift from the graphs it describes and would protect no
    /// fork; the structure already states the commitment, in two shapes:
    ///
    ///   * [`HumanCommitment::SpecTransition`] — an engaging `transition` fires only on its `gate_on`
    ///     blackboard key ([`TransitionConfig::gate_on`]), and authoring REFUSES such a transition
    ///     without a preceding `ask_human`, so the ask that WRITES a gate_on key is the one the Spec's
    ///     move hangs on. The engaging CLASSIFICATION comes from
    ///     [`crate::mcp_engine::transition_is_engaging_target`] — the reading the runtime gate and the
    ///     authoring guard already share, so a third one cannot appear here.
    ///   * [`HumanCommitment::AnotherRound`] — a `loop` exits on its `until` key, so the ask that
    ///     WRITES that key is what decides whether the body runs AGAIN; and when the body SPENDS (a
    ///     `spawn_subagent` or a `sub_workflow` sits under it), running it again is a real commitment
    ///     of time and money. The case that forced this: ndev's "rework or abandon" governs the inner
    ///     build loop, and one live `/ndev` answered it alone, from a general authorisation given
    ///     earlier — 3 h 35, 109 ticks, $69.48, over rounds that only produced second-order findings.
    ///     It committed no transition, so H6 left it out; it commits a build round, which is why it is
    ///     in now.
    ///
    /// A Spec move outranks a round: both can hold at once (an answer that gates a transition AND
    /// re-opens a loop), and the graver commitment is the one the human is told about.
    pub fn ask_human_commitment(&self, node_id: &str) -> Option<HumanCommitment> {
        let idx = self.index_of(node_id)?;
        if self.nodes[idx].node_type != NodeType::AskHuman {
            return None;
        }
        let written = self.ask_human_written_key(idx);
        // A `gate_on` is read through [`Blackboard::get`], which splits on dots, so the key it
        // actually depends on is the path's FIRST segment — the same reading the loop test below
        // already applies. Comparing the whole path let a `gate_on: "answer.decision"` slip the
        // classification, and with it the human-turn check the Spec's move hangs on.
        let gates_an_engaging_transition = self.nodes.iter().any(|n| {
            if n.node_type != NodeType::Transition {
                return false;
            }
            let Ok(cfg) = serde_json::from_value::<TransitionConfig>(n.config.clone()) else {
                return false;
            };
            let gated_on_root = cfg
                .gate_on
                .as_deref()
                .and_then(|k| k.split('.').next())
                .map(str::to_string);
            gated_on_root.as_deref() == Some(written.as_str())
                && crate::mcp_engine::transition_is_engaging_target(&cfg.target_state)
        });
        if gates_an_engaging_transition {
            return Some(HumanCommitment::SpecTransition);
        }
        let reopens_a_spending_loop = self.nodes.iter().enumerate().any(|(i, n)| {
            if n.node_type != NodeType::Loop {
                return false;
            }
            let exits_on_the_answer = serde_json::from_value::<LoopConfig>(n.config.clone())
                .ok()
                .and_then(|cfg| cfg.until)
                .and_then(|until| until.var)
                .and_then(|var| var.split('.').next().map(str::to_string))
                .as_deref()
                == Some(written.as_str());
            exits_on_the_answer && self.subtree_spends(i)
        });
        reopens_a_spending_loop.then_some(HumanCommitment::AnotherRound)
    }

    /// Whether anything under `idx` (itself included) SPENDS — opens a sub-agent or a child run. It is
    /// what turns "the answer re-opens a loop" into "the answer buys another round": a loop body of
    /// notes and queries costs a tick, one that spawns costs a session.
    fn subtree_spends(&self, idx: usize) -> bool {
        let mut stack = vec![idx];
        while let Some(i) = stack.pop() {
            if matches!(
                self.nodes[i].node_type,
                NodeType::SpawnSubagent | NodeType::SubWorkflow
            ) {
                return true;
            }
            stack.extend(self.nodes[i].children.iter().copied());
        }
        false
    }

    /// The blackboard key an `ask_human` node writes its answer to — its `output_key`, else
    /// [`DEFAULT_ASK_HUMAN_OUTPUT_KEY`]. The one reading of "which key does this question fill", so the
    /// derivations here cannot disagree about it.
    fn ask_human_written_key(&self, idx: usize) -> String {
        serde_json::from_value::<OutputConfig>(self.nodes[idx].config.clone())
            .ok()
            .and_then(|oc| oc.output_key)
            .unwrap_or_else(|| DEFAULT_ASK_HUMAN_OUTPUT_KEY.to_string())
    }

    /// The BRANCH that reads an `ask_human`'s answer as a routing decision, when that question
    /// declares NO closed answer set — the mis-authoring [`Tree::ask_human_unconstrained_branch`]
    /// reports. Carries what a refusal must name: WHICH node branches, and on WHAT test.
    fn branch_reading(&self, key: &str) -> Option<BranchReader> {
        // A condition's `var` is a dot-path ([`Blackboard::get`] splits it), so the key it READS is
        // the path's first segment: a branch on `answer.status` still branches on `answer`.
        let reads = |cond: &Condition| -> bool {
            cond.var
                .as_deref()
                .and_then(|v| v.split('.').next())
                .map(|root| root == key)
                .unwrap_or(false)
        };
        self.nodes.iter().find_map(|n| {
            let cond = match n.node_type {
                NodeType::Selector => serde_json::from_value::<SelectorConfig>(n.config.clone())
                    .ok()?
                    .branches
                    .into_iter()
                    .find_map(|b| b.cond.filter(&reads))?,
                NodeType::Loop => serde_json::from_value::<LoopConfig>(n.config.clone())
                    .ok()?
                    .until
                    .filter(&reads)?,
                _ => return None,
            };
            Some(BranchReader {
                node_id: n.id.clone(),
                label: n.label.clone(),
                condition: cond.describe(),
            })
        })
    }

    /// Whether the `ask_human` node `node_id` asks a question the graph will answer BY EQUALITY while
    /// leaving the answer free — the authoring error J1 is about — and, if so, the branch that reads it.
    ///
    /// DERIVED from the graph, exactly like [`Tree::ask_human_commitment`], and for the same reason: a
    /// hand-maintained list of "questions that need options" would drift from the graphs it describes,
    /// and would protect no fork. The structure already states it. An `ask_human` whose written key is
    /// READ BY A BRANCH — a `selector` branch `cond`, or a `loop`'s `until` — is a question with a
    /// CLOSED answer set BY CONSTRUCTION: the branch enumerates the answers it honours and compares
    /// them by equality. Declaring no `options` therefore promises the human something the graph cannot
    /// keep, and the run continues on a value nothing constrains.
    ///
    /// Observed live: ndoc's `review` took free text and the graph compared it to `"changes"` /
    /// `"approve"`. The human answered "changes — the DEL fix is right but it draws the parser's line
    /// in the wrong place"; every comparison missed, the sync loop EXITED instead of looping, the
    /// correction was dropped, and the run reported done.
    pub fn ask_human_unconstrained_branch(&self, node_id: &str) -> Option<BranchReader> {
        let idx = self.index_of(node_id)?;
        if self.nodes[idx].node_type != NodeType::AskHuman {
            return None;
        }
        let cfg = serde_json::from_value::<AskHumanConfig>(self.nodes[idx].config.clone()).ok()?;
        if !cfg.options.is_empty() {
            return None;
        }
        self.branch_reading(&self.ask_human_written_key(idx))
    }
}

/// What a human's answer to an `ask_human` COMMITS — the fact that makes it a question only a human
/// may answer, and so the fact that makes nyx demand a witnessed human turn behind the reply (H6,
/// then J2). DERIVED from the graph by [`Tree::ask_human_commitment`], never declared on a node.
///
/// The two are ONE rule read twice: an answer that DISPOSES of something the session does not own.
/// A Spec's lifecycle is not the session's to move; the user's build budget is not the session's to
/// spend. Both used to be crossed by a well-meaning session applying an instruction given earlier —
/// which is precisely what a human-turn ledger, and nothing else, can tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HumanCommitment {
    /// The answer, and nothing else, lets an ENGAGING Spec transition (`ready` / `done` / `cancel`)
    /// fire — it moves the Spec's lifecycle.
    SpecTransition,
    /// The answer re-opens a `loop` whose body SPENDS sub-agents — it buys another build round.
    AnotherRound,
}

impl HumanCommitment {
    /// What the answer commits, as ONE clause both the refusal and the up-front warning drop into a
    /// sentence — so the session is told what it is being stopped from disposing of, not merely that
    /// it was stopped.
    pub fn what_it_commits(&self) -> &'static str {
        match self {
            HumanCommitment::SpecTransition => {
                "COMMITS the Spec (an engaging transition: ready / done / cancel)"
            }
            HumanCommitment::AnotherRound => {
                "COMMITS another build round — a fresh sub-agent pass, with the time and the money it \
                 spends"
            }
        }
    }
}

/// The BRANCH that routes on a blackboard key — the `selector` branch or `loop` `until` a question's
/// answer is compared in. Named so a refusal can say WHICH node decides on the answer and on WHAT
/// test, instead of pointing at the question alone (see [`Tree::ask_human_unconstrained_branch`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchReader {
    pub node_id: String,
    pub label: String,
    /// The condition in one human sentence ([`Condition::describe`]) — `decision != "changes"`.
    pub condition: String,
}

// ============================================================================
// Actions, results, steps
// ============================================================================

/// The atomic instruction the engine hands the thin dispatcher for a LEAF node. The dispatcher
/// performs it, then reports the outcome back as a [`NodeResult`] on the next [`Engine::tick`]. Every
/// variant carries the `node_id` it came from (the run's cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run `agent_id` as a sub-agent (native `Task` / inline fallback) — report its diff.
    SpawnSubagent {
        node_id: String,
        agent_id: String,
        /// The NODE's own label — WHAT this spawn is for, in the graph author's words ("implement",
        /// "audit user stories", "distill"). It TRAVELS with the action because nothing else says
        /// what the sub-agent must DO: the role says who it is and the state says what it is about,
        /// so without this the dispatcher would launch a coder without saying what for.
        label: String,
        /// The node forces the INLINE mode (run in-session, keep user access) — see [`SpawnConfig`].
        inline: bool,
        /// WHICH OBJECT this spawn is about — the per-node focus (see [`SpawnConfig::subject`]).
        /// `None` for a spawn that needs no narrowing. The sub-agent passes it back to
        /// `agent_context` to get the matching, narrowed state.
        subject: Option<String>,
    },
    /// Run the node's managed commands as a gate (nyx runs them itself over the PTY in T4) — report
    /// green/red. `blocking` = a red gate must refuse to advance.
    ///
    /// The commands are named EITHER by `command_ids` (a graph that owns them) OR by `slot` (a
    /// SHARED graph, bound per project) — see [`GateConfig`]. The engine is pure, so it hands both
    /// to the effects layer, which resolves them against the run's project.
    GateCheck {
        node_id: String,
        command_ids: Vec<String>,
        slot: Option<String>,
        blocking: bool,
    },
    /// Ask the LLM the structured `prompt` — report the verdict.
    Judge {
        node_id: String,
        prompt: String,
        output_schema: Option<Value>,
    },
    /// Transition the run's Spec to `target_state` (a nyx state verb in T4) — report the ack.
    Transition {
        node_id: String,
        target_state: String,
        /// The blackboard key a preceding `ask_human` approval must satisfy for an ENGAGING
        /// transition (ready/done/cancel); `None` for entry (`start`) / `in_review` (B3).
        gate_on: Option<String>,
    },
    /// Read a nyx fact `query` (e.g. `spec_status`) and write it to the blackboard key `write` — a
    /// nyx-own inline effect the T4 handler resolves itself (never surfaced to the dispatcher).
    /// `target` parameterizes `spec_can_transition` (the transition it asks about); `task_key` names
    /// the board key holding the task a TASK-GRAINED fact reads (`task_close_route`).
    Query {
        node_id: String,
        query: String,
        write: String,
        target: Option<String>,
        task_key: Option<String>,
    },
    /// Fork the run's bound Spec into a new editable draft revision — a nyx-own inline effect the T4
    /// handler resolves itself. `gate_on` = the blackboard key a preceding ask_human approved (N13).
    ForkSpec {
        node_id: String,
        gate_on: Option<String>,
    },
    /// Mark `task_id` (resolved from the board) through the guarded task lifecycle `transition`
    /// (start/done/block/skip) — a nyx-own inline effect the handler resolves itself (never surfaced).
    /// `reason` rides a block/skip; `gate_on` is an OPTIONAL affirmative board key (UNGATED when None).
    TransitionTask {
        node_id: String,
        task_id: String,
        transition: String,
        reason: Option<String>,
        gate_on: Option<String>,
    },
    /// Pause — report the ack to resume.
    Wait { node_id: String },
    /// Inject free `text` into the dispatcher (trappe) — report the ack. `clear_gate_block` (M5) marks
    /// a graph-expressible ABANDON: the handler clears the run's red-gate anchor when this note is
    /// reached, giving a run parked past a red gate a clean exit (never implicit — only this explicit
    /// route through the node clears the block).
    Note {
        node_id: String,
        text: String,
        clear_gate_block: bool,
        /// Drop the Spec-write grant this run carries — see [`NoteConfig::revoke_spec_write`] (H12).
        revoke_spec_write: bool,
    },
    /// ARBITRAGE (minimal): propose/ask the human a `prompt` — report their decision. `options` is the
    /// closed answer set the dispatcher must offer (empty = free-form); nyx REFUSES an answer outside
    /// it (N19). The BLOCKING human interaction itself is out of Phase-2 scope.
    AskHuman {
        node_id: String,
        prompt: String,
        options: Vec<String>,
    },
    /// ARBITRAGE (minimal): produce a structured artefact — report its reference. Persisting the
    /// artefact through a nyx verb is out of Phase-2 scope; this variant makes a tree with the node walk.
    Produce { node_id: String },
    /// HYBRID: open (or re-check) a CHILD run of another flow. The T4 handler resolves the child target
    /// (`workflow_id` else `scope`), seeds the ISOLATED child board from the parent's `inject` keys,
    /// suspends the parent while a fresh sub-agent drives the child, and on the child finishing folds
    /// its `retrieve` keys back — feeding a [`NodeResult::SubResult`]. Never trusts the sub-agent's
    /// claim: the result is read from the child run's VERIFIED blackboard (v2.8 lot 2).
    SubWorkflow {
        node_id: String,
        scope: String,
        workflow_id: Option<String>,
        inject: Vec<BoardCopy>,
        retrieve: Vec<BoardCopy>,
        output_key: Option<String>,
    },
}

impl Action {
    /// The id of the node this action came from — the run's cursor while it is pending.
    pub fn node_id(&self) -> &str {
        match self {
            Action::SpawnSubagent { node_id, .. }
            | Action::GateCheck { node_id, .. }
            | Action::Judge { node_id, .. }
            | Action::Transition { node_id, .. }
            | Action::Query { node_id, .. }
            | Action::ForkSpec { node_id, .. }
            | Action::TransitionTask { node_id, .. }
            | Action::Wait { node_id }
            | Action::Note { node_id, .. }
            | Action::AskHuman { node_id, .. }
            | Action::Produce { node_id }
            | Action::SubWorkflow { node_id, .. } => node_id,
        }
    }
}

/// What the dispatcher reports back for a pending [`Action`]. The engine folds it into the blackboard
/// before advancing. `Ack` is the trivial completion for the effect-only leaves (wait/note/transition).
#[derive(Debug, Clone, PartialEq)]
pub enum NodeResult {
    /// Trivial completion (wait / note / transition, or "nothing to report").
    Ack,
    /// A `spawn_subagent`'s extracted diff.
    Diff(String),
    /// A `gate_check` verdict (`true` = green).
    Gate(bool),
    /// A `judge` verdict (any JSON — a string like `"clair"`, or a structured object).
    Verdict(Value),
    /// An `ask_human` decision.
    Human(Value),
    /// A `produce` artefact reference.
    Artifact(Value),
    /// A `sub_workflow` child's VERIFIED result — a summary object `{ status, ...retrieved }` the T4
    /// handler builds from the CHILD run's blackboard (never the sub-agent's claim). A parent selector
    /// branches on it (`status` = done/blocked/failed) (v2.8 lot 2).
    SubResult(Value),
}

/// The outcome of one [`Engine::tick`]: either an [`Action`] for the dispatcher to perform, or the
/// run has reached the end of the tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Perform this action, then call `tick` again with its [`NodeResult`].
    Action(Action),
    /// The tree is fully walked — the run is done.
    Done,
}

// ============================================================================
// Errors
// ============================================================================

/// An engine failure — a malformed tree or a mis-authored loop. The T4 handler maps these to a
/// `failed`/`blocked` run status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// A node's `type` is not one of [`db::NODE_TYPES`].
    UnknownNodeType(String),
    /// A node's `config` is not a JSON object (or does not parse).
    BadConfig { node_id: String, message: String },
    /// A child references a `parent_id` that is not in the workflow's rows.
    DanglingChild { node_id: String, parent_id: String },
    /// No root: the given `root_id` is absent (`Some`), or no `parent_id IS NULL` row exists (`None`).
    MissingRoot(Option<String>),
    /// The tree has several `parent_id IS NULL` rows and no explicit `root_id` to disambiguate.
    MultipleRoots(usize),
    /// A node is reachable from the root TWICE — the parent links form a cycle (or a diamond), which
    /// would make the graph walk descend forever.
    Cycle { node_id: String },
    /// A `loop` ran its body `max` times without its `until` ever holding. Carries WHAT was awaited
    /// (`condition`) and what the board last held for it (`observed`) — see [`EngineError::Display`].
    LoopLimit {
        node_id: String,
        max: u32,
        condition: String,
        observed: String,
    },
    /// A `loop` whose `until` reads a blackboard key FROZEN for this run ([`Engine::freeze_key`]):
    /// the exit is unreachable BY CONSTRUCTION, so the loop fails on its FIRST failed exit check
    /// rather than burning its whole iteration budget (and, with it, a sub-agent per round).
    LoopUnsatisfiable {
        node_id: String,
        condition: String,
        observed: String,
        why: String,
    },
    /// A `selector` branch selected a child index that is out of range.
    BadBranchChild { node_id: String, child: usize },
    /// A `selector` matched NO branch. It used to complete as a silent no-op, which is how a run
    /// REPORTED SUCCESS for a flow that took no branch anybody authored for the value it read (J1).
    SelectorNoBranch {
        node_id: String,
        label: String,
        /// Each branch's condition and what the board actually held for it, so the failure says why
        /// every one of them missed — the same "awaited vs observed" pairing [`EngineError::LoopLimit`]
        /// gives a loop that never exits.
        tried: String,
    },
    /// The tree RE-READ mid-walk ([`Engine::adopt_tree`]) is not the one the live traversal was built
    /// on: a node moved, changed type, or came and went. The frames point at arena positions that no
    /// longer mean what they meant, so the walk stops instead of resuming on a different node.
    ReshapedTree { detail: String },
    /// `tick` was called with a result but nothing was awaiting one (dispatcher protocol misuse).
    UnexpectedResult,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::UnknownNodeType(t) => write!(f, "unknown node type: {t}"),
            EngineError::BadConfig { node_id, message } => {
                write!(f, "bad config on node {node_id}: {message}")
            }
            EngineError::DanglingChild { node_id, parent_id } => {
                write!(f, "node {node_id} references missing parent {parent_id}")
            }
            EngineError::MissingRoot(Some(id)) => write!(f, "root node {id} not found"),
            EngineError::MissingRoot(None) => write!(f, "no root node (no parent_id IS NULL row)"),
            EngineError::MultipleRoots(n) => {
                write!(f, "{n} root nodes and no explicit root_id to disambiguate")
            }
            EngineError::Cycle { node_id } => {
                write!(f, "node {node_id} is reachable twice from the root (cycle)")
            }
            EngineError::LoopLimit {
                node_id,
                max,
                condition,
                observed,
            } => {
                write!(
                    f,
                    "loop {node_id} exceeded {max} iterations without exiting: its exit condition \
                     ({condition}) never held — last observed {observed}"
                )
            }
            EngineError::LoopUnsatisfiable {
                node_id,
                condition,
                observed,
                why,
            } => {
                write!(
                    f,
                    "loop {node_id} can never exit: its exit condition ({condition}) reads a value \
                     this run can no longer change — {observed}, and {why}"
                )
            }
            EngineError::BadBranchChild { node_id, child } => {
                write!(
                    f,
                    "selector {node_id} branch selects out-of-range child {child}"
                )
            }
            EngineError::SelectorNoBranch {
                node_id,
                label,
                tried,
            } => {
                write!(
                    f,
                    "selector {node_id} ('{label}') matched NO branch, so nothing it routes to was \
                     authored for the value it read: {tried}. Give the value a branch, or give the \
                     selector a default (an `always` branch) that is SAFE to fall through to"
                )
            }
            EngineError::UnexpectedResult => {
                write!(f, "tick received a result while nothing was awaiting one")
            }
            EngineError::ReshapedTree { detail } => {
                write!(f, "the workflow tree was reshaped under the live walk: {detail}")
            }
        }
    }
}

impl std::error::Error for EngineError {}

// ============================================================================
// The engine (the graph walker)
// ============================================================================

/// A composite frame on the execution stack. Leaves are NOT framed — a composite that descends into a
/// leaf child yields the leaf's [`Action`] and records it as `awaiting`.
#[derive(Debug, Clone, Copy)]
enum Frame {
    /// A `sequence`: `next` is the index of the child to run next.
    Sequence { node: usize, next: usize },
    /// A `loop`: `iter` is the 0-based body pass, `cursor` walks the body's children (0..`body_len`).
    Loop {
        node: usize,
        iter: u32,
        cursor: usize,
        body_len: usize,
    },
    /// A `selector`: `descended` = whether the chosen branch's child has been entered yet.
    Selector { node: usize, descended: bool },
}

/// The interpreter's traversal STATE serialized for cross-session resume (v2.8 review B1): the
/// composite stack + the parked-leaf cursor + the sticky-replay flag, keyed by NODE ID (the arena
/// index is not stable across a rebuild; the id is). Persisted to `workflow_runs.traversal` by
/// [`Engine::export_traversal`] each tick, restored by [`Engine::restore`] on the first tick of a
/// later session. `body_len` is omitted (recomputed from the tree on restore, so it cannot drift).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TraversalState {
    stack: Vec<TraversalFrame>,
    #[serde(default)]
    awaiting: Option<String>,
    #[serde(default)]
    replay: bool,
}

/// One serialized composite [`Frame`], its `node` an id rather than an arena index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TraversalFrame {
    Sequence { node: String, next: usize },
    Loop { node: String, iter: u32, cursor: usize },
    Selector { node: String, descended: bool },
}

/// Resolve a traversal frame's node id to its arena index in `tree`, or fail with
/// [`EngineError::UnknownNodeType`] carrying the missing id — the structural guard that REFUSES a
/// resume whose traversal references a node the (edited) tree no longer holds (B1).
fn traversal_index(tree: &Tree, id: &str) -> Result<usize, EngineError> {
    tree.index_of(id)
        .ok_or_else(|| EngineError::UnknownNodeType(id.to_string()))
}

/// The interpreter over ONE run of a [`Tree`]: it holds the traversal stack, the blackboard, and the
/// pending-leaf cursor. Drive it by calling [`Engine::tick`] repeatedly, feeding back each
/// [`Action`]'s [`NodeResult`], until it returns [`Step::Done`].
#[derive(Debug, Clone)]
pub struct Engine {
    tree: Tree,
    board: Blackboard,
    stack: Vec<Frame>,
    /// The arena index of the leaf whose action was yielded and whose result is awaited.
    awaiting: Option<usize>,
    /// Set by [`Engine::replay_current`]: the next [`Engine::tick`] RE-YIELDS the parked leaf's
    /// action instead of folding a result and advancing (the handler refused to perform it).
    replay: bool,
    started: bool,
    done: bool,
    /// The ordered ids of the leaf nodes executed (for observability + tests).
    trace: Vec<String>,
    /// The blackboard keys whose value this run can NO LONGER change, and WHY — declared by the
    /// effects layer through [`Engine::freeze_key`]. Read only when a `loop`'s `until` fails to hold:
    /// a condition on a frozen key can never hold either, so the loop is dead on the first round.
    /// Call-scoped like the board itself (rebuilt each session, re-declared by the effect that writes
    /// the constant), so it is never persisted and can never go stale.
    frozen: HashMap<String, String>,
}

impl Engine {
    /// Start a run of `tree` with the given initial `board`.
    pub fn new(tree: Tree, board: Blackboard) -> Self {
        Engine {
            tree,
            board,
            stack: Vec::new(),
            awaiting: None,
            replay: false,
            started: false,
            done: false,
            trace: Vec::new(),
            frozen: HashMap::new(),
        }
    }

    /// Start a run with an empty blackboard.
    pub fn start(tree: Tree) -> Self {
        Engine::new(tree, Blackboard::new())
    }

    /// The run's blackboard (read-only).
    pub fn blackboard(&self) -> &Blackboard {
        &self.board
    }

    /// The tree this engine is walking (read-only) — the LIVE one, re-adopted each tick
    /// ([`Engine::adopt_tree`]). The `nyx_next` handler reads it to classify the leaf it is about to
    /// hand back ([`Tree::ask_human_commitment`], [`Tree::ask_human_unconstrained_branch`]).
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The blackboard serialized for `workflow_runs.blackboard`.
    pub fn blackboard_json(&self) -> String {
        self.board.to_json_string()
    }

    /// Write a TOP-LEVEL blackboard key from OUTSIDE the graph walk. The `nyx_next` handler uses it
    /// to record its own inline-effect state (the id of the blocking gate it just ran RED) on the
    /// board that gets PERSISTED, so the refusal survives across calls of the dispatcher loop.
    pub fn set_board(&mut self, key: &str, value: Value) {
        self.board.set(key, value);
    }

    /// Drop a top-level blackboard key written by [`Engine::set_board`] (the flag no longer holds).
    pub fn clear_board(&mut self, key: &str) {
        self.board.remove(key);
    }

    /// DECLARE that `key`'s value can no longer change for this run, and `why` — the effects layer
    /// knows it just wrote a CONSTANT (a Spec fact on a run that will never carry a Spec); the
    /// interpreter cannot know that. A `loop` whose `until` reads a frozen key is then failed at its
    /// FIRST failed exit check ([`EngineError::LoopUnsatisfiable`]) instead of running its whole
    /// budget — and, for a body that spawns, a sub-agent per round.
    ///
    /// DELIBERATELY not a general unreachability analyser. Proving that a condition can never hold
    /// means proving that nothing the body still does can move it — a judge's next verdict, a
    /// human's next answer, a sub-agent's next diff — over a graph whose values come from outside the
    /// engine entirely. The engine therefore decides nothing: it only believes what the layer that
    /// OWNS a value declares about it, and every other never-exiting loop remains the backstop's
    /// business ([`DEFAULT_MAX_LOOP_ITERATIONS`]).
    pub fn freeze_key(&mut self, key: &str, why: &str) {
        self.frozen.insert(key.to_string(), why.to_string());
    }

    /// ADOPT a freshly-read `tree` in place of the one this engine was built with, keeping the WALK
    /// exactly where it is (stack, parked leaf, blackboard, replay flag, frozen keys).
    ///
    /// WHY. A [`Tree`] carries each node's DECODED `config`, so an engine that lives across the calls
    /// of a dispatcher loop executes the config as it was at its FIRST tick. That silently broke the
    /// one graph edit the authoring layer allows under a live run: `node_update` (label / config moves
    /// no node id, so the traversal restores intact) tells the caller the run "will execute the edited
    /// node from its very next tick" — and the resident tree meant the edit reached nothing until a
    /// later session rebuilt from the persisted traversal. That is load-bearing, not cosmetic: arming
    /// an unconfigured blocking `gate_check` is exactly a `node_update`, and it is the fix nyx itself
    /// prescribes when it refuses to advance past a gate that verifies nothing.
    ///
    /// REFUSED unless the new tree is structurally IDENTICAL — same nodes, in the same arena
    /// positions, with the same types and the same children. The live stack holds arena INDICES (and
    /// a `sequence`/`loop` frame a sibling CURSOR), so a reshaped tree would resume the walk on a
    /// different node than the frame means. A structural edit under a walking run is already refused
    /// by the authoring layer; reaching this means the graph moved some other way, and resuming on the
    /// wrong child is worse than stopping.
    pub fn adopt_tree(&mut self, tree: Tree) -> Result<(), EngineError> {
        if tree.nodes.len() != self.tree.nodes.len() {
            return Err(EngineError::ReshapedTree {
                detail: format!(
                    "{} nodes now, {} when the walk started",
                    tree.nodes.len(),
                    self.tree.nodes.len()
                ),
            });
        }
        for (fresh, live) in tree.nodes.iter().zip(&self.tree.nodes) {
            if fresh.id != live.id || fresh.node_type != live.node_type || fresh.children != live.children
            {
                return Err(EngineError::ReshapedTree {
                    detail: format!("node {} is no longer where (or what) it was", live.id),
                });
            }
        }
        self.tree = tree;
        Ok(())
    }

    /// RE-YIELD the currently-parked leaf's action on the next [`Engine::tick`] instead of folding a
    /// result and advancing. The handler calls this when it REFUSED to perform that leaf's effect (a
    /// red blocking gate, an illegal Spec transition): the refused leaf must be RE-ATTEMPTED on the
    /// next call, never silently skipped. A no-op when nothing is parked.
    pub fn replay_current(&mut self) {
        if self.awaiting.is_some() {
            self.replay = true;
        }
    }

    /// The run's CURSOR — the id of the node it is parked on (the pending leaf), for
    /// `workflow_runs.current_node_id`. `None` before the first tick and after completion.
    pub fn current_node_id(&self) -> Option<&str> {
        self.awaiting.map(|idx| self.tree.nodes[idx].id.as_str())
    }

    /// The [`NodeType`] of the leaf the run is currently parked on (the awaited cursor), or `None`
    /// before the first tick / after completion. The `nyx_next` verb handler reads this to decode
    /// the dispatcher's reported `last_result` into the right [`NodeResult`] variant for the leaf.
    pub fn awaiting_node_type(&self) -> Option<NodeType> {
        self.awaiting.map(|idx| self.tree.nodes[idx].node_type)
    }

    /// The declared `options` of the awaited `ask_human` leaf (empty when free-form or not an
    /// ask_human) — the closed answer set the T4 handler validates a reported answer against (N19).
    pub fn awaiting_options(&self) -> Vec<String> {
        let Some(idx) = self.awaiting else {
            return Vec::new();
        };
        if self.tree.nodes[idx].node_type != NodeType::AskHuman {
            return Vec::new();
        }
        self.parse_config::<AskHumanConfig>(idx)
            .map(|cfg| cfg.options)
            .unwrap_or_default()
    }

    /// Whether the run has reached [`Step::Done`].
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The ordered ids of the leaf nodes executed so far.
    pub fn trace(&self) -> &[String] {
        &self.trace
    }

    /// Serialize the traversal STACK + parked cursor (by NODE ID) for `workflow_runs.traversal`, so a
    /// later session can [`restore`](Engine::restore) the EXACT walk instead of replaying from the
    /// root (which would re-execute every effect). `None` before the first tick — there is nothing to
    /// resume (v2.8 review B1).
    ///
    /// A run that reached the END is NOT excluded here, and that is load-bearing. Reaching
    /// [`Step::Done`] is not the same thing as the run being RECORDED done: the effects layer re-runs
    /// every still-anchored blocking gate at that point and REFUSES the completion while one is red
    /// (M5), parking the run `blocked` with its cursor on the gate. Bailing on `self.done` made that
    /// stop persist a NULL traversal, so the next SESSION read a progressed run with no walk to
    /// restore, refused it as unresumable, and `start_or_replace_run` destroyed it and started the
    /// flow from the top — the exact opposite of the exit M5 promises. The walk is therefore always
    /// exported once started; it is [`crate::mcp_engine`]'s `persist` that clears it, keyed on the
    /// status it is RECORDING (a terminal one, and nothing else).
    pub fn export_traversal(&self) -> Option<String> {
        if !self.started {
            return None;
        }
        let stack = self
            .stack
            .iter()
            .map(|frame| match *frame {
                Frame::Sequence { node, next } => TraversalFrame::Sequence {
                    node: self.tree.nodes[node].id.clone(),
                    next,
                },
                Frame::Loop {
                    node, iter, cursor, ..
                } => TraversalFrame::Loop {
                    node: self.tree.nodes[node].id.clone(),
                    iter,
                    cursor,
                },
                Frame::Selector { node, descended } => TraversalFrame::Selector {
                    node: self.tree.nodes[node].id.clone(),
                    descended,
                },
            })
            .collect();
        let state = TraversalState {
            stack,
            awaiting: self.awaiting.map(|idx| self.tree.nodes[idx].id.clone()),
            replay: self.replay,
        };
        serde_json::to_string(&state).ok()
    }

    /// Rebuild an [`Engine`] RESUMING the persisted `traversal` (a [`TraversalState`] JSON) over
    /// `tree`, seeded with `board` — the cross-session resume (v2.8 review B1). Every node id in the
    /// traversal MUST still exist in `tree`: a structural edit (a deleted/renamed node) fails the
    /// resume with [`EngineError::UnknownNodeType`] rather than resuming a stale walk. A loop frame's
    /// `body_len` is recomputed from the tree (so it cannot drift from an edited body).
    pub fn restore(tree: Tree, board: Blackboard, traversal: &str) -> Result<Self, EngineError> {
        let state: TraversalState =
            serde_json::from_str(traversal).map_err(|e| EngineError::BadConfig {
                node_id: "<traversal>".to_string(),
                message: format!("malformed traversal: {e}"),
            })?;
        let mut stack = Vec::with_capacity(state.stack.len());
        for frame in &state.stack {
            stack.push(match frame {
                TraversalFrame::Sequence { node, next } => Frame::Sequence {
                    node: traversal_index(&tree, node)?,
                    next: *next,
                },
                TraversalFrame::Loop { node, iter, cursor } => {
                    let node = traversal_index(&tree, node)?;
                    Frame::Loop {
                        node,
                        iter: *iter,
                        cursor: *cursor,
                        body_len: tree.nodes[node].children.len(),
                    }
                }
                TraversalFrame::Selector { node, descended } => Frame::Selector {
                    node: traversal_index(&tree, node)?,
                    descended: *descended,
                },
            });
        }
        let awaiting = match &state.awaiting {
            Some(id) => Some(traversal_index(&tree, id)?),
            None => None,
        };
        Ok(Engine {
            tree,
            board,
            stack,
            awaiting,
            replay: state.replay,
            started: true,
            done: false,
            trace: Vec::new(),
            frozen: HashMap::new(),
        })
    }

    /// THE TICK. Fold `result` (the outcome of the previously-yielded [`Action`], `None` on the very
    /// first tick / for the effect-only leaves) into the blackboard, advance the graph, and return the
    /// next [`Action`] to perform — or [`Step::Done`].
    ///
    /// Contract: the caller performs each returned [`Action`] and reports its [`NodeResult`] on the
    /// next call. The composites (sequence/loop/selector) advance INTERNALLY with no round-trip; only
    /// leaves suspend.
    pub fn tick(&mut self, result: Option<NodeResult>) -> Result<Step, EngineError> {
        // 0. REPLAY: the handler REFUSED to perform the parked leaf's effect, so re-yield the SAME
        //    action (the refusal is sticky) and discard whatever the caller reported for it.
        if self.replay {
            self.replay = false;
            if let Some(leaf_idx) = self.awaiting {
                drop(result);
                return Ok(Step::Action(self.leaf_action(leaf_idx)?));
            }
        }

        // 1. Incorporate the result of the leaf we were parked on.
        if let Some(leaf_idx) = self.awaiting.take() {
            self.apply_result(leaf_idx, result.unwrap_or(NodeResult::Ack));
        } else if result.is_some() {
            return Err(EngineError::UnexpectedResult);
        }

        // 2. Enter the root on the first tick.
        if !self.started {
            self.started = true;
            if let Some(action) = self.enter(self.tree.root)? {
                return Ok(Step::Action(action));
            }
        }

        // 3. Drive the stack until a leaf yields an action or the stack empties.
        loop {
            let Some(&frame) = self.stack.last() else {
                self.done = true;
                return Ok(Step::Done);
            };
            match frame {
                Frame::Sequence { node, next } => {
                    let children_len = self.tree.nodes[node].children.len();
                    if next < children_len {
                        let child = self.tree.nodes[node].children[next];
                        *self.stack.last_mut().unwrap() = Frame::Sequence {
                            node,
                            next: next + 1,
                        };
                        if let Some(action) = self.enter(child)? {
                            return Ok(Step::Action(action));
                        }
                    } else {
                        self.stack.pop();
                    }
                }
                Frame::Loop {
                    node,
                    iter,
                    cursor,
                    body_len,
                } => {
                    if cursor < body_len {
                        let child = self.tree.nodes[node].children[cursor];
                        *self.stack.last_mut().unwrap() = Frame::Loop {
                            node,
                            iter,
                            cursor: cursor + 1,
                            body_len,
                        };
                        if let Some(action) = self.enter(child)? {
                            return Ok(Step::Action(action));
                        }
                    } else {
                        // The body finished this pass → check `until` (repeat-UNTIL: after the body).
                        let cfg: LoopConfig = self.parse_config(node)?;
                        let exit = cfg
                            .until
                            .as_ref()
                            .map(|c| c.matches(&self.board))
                            .unwrap_or(true);
                        // PER NODE (`loop_iter.<node id>`), never one flat `loop_iter`: ndev nests an
                        // audit/rework loop inside a phases loop, and a single key made the two
                        // overwrite each other — whoever completed a pass last owned the number, and
                        // nothing reading it could tell WHICH loop it described.
                        let loop_id = self.tree.nodes[node].id.clone();
                        self.board
                            .set_scoped(LOOP_ITER_KEY, &loop_id, Value::from(iter));
                        if exit {
                            self.stack.pop();
                        } else {
                            // The `until` is necessarily present here — an absent one exits above —
                            // so the loop can name what it awaited and what it actually read. That
                            // sentence is all the user gets when the loop dies.
                            let until = cfg.until.as_ref().expect("an absent `until` exits");
                            let condition = until.describe();
                            let observed = self.observed(until);
                            // UNSATISFIABLE BY CONSTRUCTION: the exit reads a value this run can no
                            // longer change, so round 2 would read the same thing as round 1 — and
                            // every round costs what the body costs. Fail HERE, on the first miss.
                            if let Some(why) = until.var.as_deref().and_then(|v| self.frozen.get(v))
                            {
                                return Err(EngineError::LoopUnsatisfiable {
                                    node_id: self.tree.nodes[node].id.clone(),
                                    condition,
                                    observed,
                                    why: why.clone(),
                                });
                            }
                            let max = cfg.max_iterations.unwrap_or(DEFAULT_MAX_LOOP_ITERATIONS);
                            if iter + 1 >= max {
                                return Err(EngineError::LoopLimit {
                                    node_id: self.tree.nodes[node].id.clone(),
                                    max,
                                    condition,
                                    observed,
                                });
                            }
                            *self.stack.last_mut().unwrap() = Frame::Loop {
                                node,
                                iter: iter + 1,
                                cursor: 0,
                                body_len,
                            };
                        }
                    }
                }
                Frame::Selector { node, descended } => {
                    if descended {
                        self.stack.pop();
                    } else {
                        let cfg: SelectorConfig = self.parse_config(node)?;
                        match self.pick_branch(node, &cfg)? {
                            Some(child) => {
                                *self.stack.last_mut().unwrap() = Frame::Selector {
                                    node,
                                    descended: true,
                                };
                                if let Some(action) = self.enter(child)? {
                                    return Ok(Step::Action(action));
                                }
                            }
                            // NO branch matched. This used to complete as a silent no-op — the walk
                            // simply carried on, and the run reported DONE having routed nothing.
                            // That is the second half of the J1 failure: a decision that governs the
                            // flow taken on a value nothing constrains, and a GREEN run behind it.
                            // A selector exists to CHOOSE; when it can choose nothing, the honest
                            // outcome is a failure that names what it read, not a success.
                            None => {
                                return Err(EngineError::SelectorNoBranch {
                                    node_id: self.tree.nodes[node].id.clone(),
                                    label: self.tree.nodes[node].label.clone(),
                                    tried: self.tried_branches(&cfg),
                                })
                            }
                        }
                    }
                }
            }
        }
    }

    /// ENTER a node: push a composite's frame (returning `None` to keep driving), or yield a leaf's
    /// action (recording it as the awaited cursor).
    fn enter(&mut self, idx: usize) -> Result<Option<Action>, EngineError> {
        let node_type = self.tree.nodes[idx].node_type;
        match node_type {
            NodeType::Sequence => {
                self.stack.push(Frame::Sequence { node: idx, next: 0 });
                Ok(None)
            }
            NodeType::Loop => {
                let body_len = self.tree.nodes[idx].children.len();
                self.stack.push(Frame::Loop {
                    node: idx,
                    iter: 0,
                    cursor: 0,
                    body_len,
                });
                Ok(None)
            }
            NodeType::Selector => {
                self.stack.push(Frame::Selector {
                    node: idx,
                    descended: false,
                });
                Ok(None)
            }
            // Leaves: yield the action and park on it.
            _ => {
                let action = self.leaf_action(idx)?;
                self.awaiting = Some(idx);
                Ok(Some(action))
            }
        }
    }

    /// Build the [`Action`] for a leaf node from its decoded config.
    fn leaf_action(&self, idx: usize) -> Result<Action, EngineError> {
        let node = &self.tree.nodes[idx];
        let node_id = node.id.clone();
        Ok(match node.node_type {
            NodeType::SpawnSubagent => {
                let cfg: SpawnConfig = self.parse_config(idx)?;
                Action::SpawnSubagent {
                    node_id,
                    agent_id: cfg.agent_id,
                    label: node.label.clone(),
                    inline: cfg.inline,
                    subject: cfg.subject,
                }
            }
            NodeType::GateCheck => {
                let cfg: GateConfig = self.parse_config(idx)?;
                Action::GateCheck {
                    node_id,
                    command_ids: cfg.command_ids,
                    slot: cfg.slot,
                    blocking: cfg.blocking,
                }
            }
            NodeType::Judge => {
                let cfg: JudgeConfig = self.parse_config(idx)?;
                Action::Judge {
                    node_id,
                    prompt: interpolate(&cfg.prompt, &self.board),
                    output_schema: cfg.output_schema,
                }
            }
            NodeType::Transition => {
                let cfg: TransitionConfig = self.parse_config(idx)?;
                Action::Transition {
                    node_id,
                    target_state: cfg.target_state,
                    gate_on: cfg.gate_on,
                }
            }
            NodeType::Query => {
                let cfg: QueryConfig = self.parse_config(idx)?;
                Action::Query {
                    node_id,
                    query: cfg.query,
                    write: cfg.write,
                    target: cfg.target,
                    task_key: cfg.task_key,
                }
            }
            NodeType::ForkSpec => {
                let cfg: ForkSpecConfig = self.parse_config(idx)?;
                Action::ForkSpec {
                    node_id,
                    gate_on: cfg.gate_on,
                }
            }
            NodeType::TransitionTask => {
                let cfg: TransitionTaskConfig = self.parse_config(idx)?;
                // RESOLVE the task id from the board via the named key (default `task_id`), the same
                // `{{path}}` interpolation the prompt leaves use — so a graph names its task by a
                // blackboard key the injecting parent seeded (e.g. an injected `task_id`).
                let task_key = cfg.task_key.as_deref().unwrap_or(DEFAULT_TASK_KEY);
                let task_id = interpolate(&format!("{{{{{task_key}}}}}"), &self.board);
                Action::TransitionTask {
                    node_id,
                    task_id,
                    transition: cfg.transition,
                    reason: cfg.reason,
                    gate_on: cfg.gate_on,
                }
            }
            NodeType::Wait => Action::Wait { node_id },
            NodeType::Note => {
                let cfg: NoteConfig = self.parse_config(idx)?;
                Action::Note {
                    node_id,
                    text: interpolate(&cfg.text, &self.board),
                    clear_gate_block: cfg.clear_gate_block,
                    revoke_spec_write: cfg.revoke_spec_write,
                }
            }
            NodeType::AskHuman => {
                let cfg: AskHumanConfig = self.parse_config(idx)?;
                Action::AskHuman {
                    node_id,
                    prompt: interpolate(&cfg.prompt, &self.board),
                    options: cfg.options,
                }
            }
            NodeType::Produce => Action::Produce { node_id },
            NodeType::SubWorkflow => {
                let cfg: SubWorkflowConfig = self.parse_config(idx)?;
                Action::SubWorkflow {
                    node_id,
                    scope: cfg.scope,
                    workflow_id: cfg.workflow_id,
                    inject: cfg.inject,
                    retrieve: cfg.retrieve,
                    output_key: cfg.output_key,
                }
            }
            // The composites never reach here (handled in `enter`).
            NodeType::Sequence | NodeType::Loop | NodeType::Selector => {
                unreachable!("composite routed to leaf_action")
            }
        })
    }

    /// Fold a leaf's reported [`NodeResult`] into the blackboard (and the trace). The type-specific
    /// convenience keys are what a downstream `loop.until` / `selector.cond` reads.
    fn apply_result(&mut self, leaf_idx: usize, result: NodeResult) {
        let node_type = self.tree.nodes[leaf_idx].node_type;
        let node_id = self.tree.nodes[leaf_idx].id.clone();
        match (node_type, result) {
            (NodeType::SpawnSubagent, NodeResult::Diff(diff)) => {
                self.board.set("last_diff", Value::from(diff));
                self.board.increment("spawns");
            }
            (NodeType::GateCheck, NodeResult::Gate(green)) => {
                self.board.set("gate_green", Value::from(green));
            }
            (NodeType::Judge, NodeResult::Verdict(verdict)) => {
                self.write_result(leaf_idx, DEFAULT_JUDGE_OUTPUT_KEY, verdict);
            }
            (NodeType::AskHuman, NodeResult::Human(decision)) => {
                // H12 — the human's answer is ALSO where a Spec-write grant can come from, and the
                // only place it can: a node declaring `grant_spec_write_on` opens the window when the
                // answer it names is the one given. Written here, at the FOLD, so the grant is exactly
                // as trustworthy as the answer it is derived from — no more, and no less.
                let cfg: AskHumanConfig = self.parse_config(leaf_idx).unwrap_or_default();
                if let (Some(granting), Some(answer)) =
                    (cfg.grant_spec_write_on.as_deref(), decision.as_str())
                {
                    if granting == answer {
                        self.board.set(SPEC_WRITE_GRANT_KEY, Value::Bool(true));
                    }
                }
                self.write_result(leaf_idx, DEFAULT_ASK_HUMAN_OUTPUT_KEY, decision);
            }
            (NodeType::SubWorkflow, NodeResult::SubResult(sub)) => {
                self.write_result(leaf_idx, "sub_result", sub);
            }
            (NodeType::Produce, NodeResult::Artifact(artifact)) => {
                self.write_result(leaf_idx, "artifact", artifact);
            }
            // Effect-only leaves (wait/note/transition) and Ack results carry no blackboard payload.
            _ => {}
        }
        self.trace.push(node_id);
    }

    /// Fold a RESULT-producing leaf's value onto the blackboard, honoring its [`OutputConfig`]: the
    /// `output_key` (defaulting to `default_key` — `verdict` / `human` / `artifact`) and the `append`
    /// flag (push onto a JSON array vs overwrite). A leaf with neither field behaves exactly as before
    /// N9 (its fixed key, overwrite).
    fn write_result(&mut self, leaf_idx: usize, default_key: &str, value: Value) {
        let oc: OutputConfig = self.parse_config(leaf_idx).unwrap_or_default();
        let key = oc.output_key.unwrap_or_else(|| default_key.to_string());
        if oc.append {
            self.board.append(&key, value);
        } else {
            self.board.set(&key, value);
        }
    }

    /// Choose a selector's branch: the first whose `cond` matches (a `None`/`always` cond is the
    /// default). Returns the arena index of the selected child, or `None` when nothing matches.
    fn pick_branch(&self, node: usize, cfg: &SelectorConfig) -> Result<Option<usize>, EngineError> {
        let children = &self.tree.nodes[node].children;
        for (position, branch) in cfg.branches.iter().enumerate() {
            let matched = match &branch.cond {
                None => true, // a branch with no cond is the default/else.
                Some(cond) => cond.matches(&self.board),
            };
            if matched {
                let child_pos = branch.child.unwrap_or(position);
                let child = *children.get(child_pos).ok_or(EngineError::BadBranchChild {
                    node_id: self.tree.nodes[node].id.clone(),
                    child: child_pos,
                })?;
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// Every branch a selector tried, each with what the board actually held for it — the whole of
    /// what a [`EngineError::SelectorNoBranch`] must explain. A branch with no `cond` matches, so it
    /// can never appear here; it is rendered anyway rather than silently skipped, since a config the
    /// author believed was a default and that did not act as one is the very thing they need to see.
    fn tried_branches(&self, cfg: &SelectorConfig) -> String {
        cfg.branches
            .iter()
            .map(|b| match &b.cond {
                Some(cond) => format!("{} (board holds {})", cond.describe(), self.observed(cond)),
                None => "a default branch".to_string(),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// What the board ACTUALLY holds for `cond`'s variable, rendered for a failure message — the
    /// other half of [`Condition::describe`]: the condition says what was awaited, this says what
    /// came instead. An absent key reads `null`, which is exactly how the condition saw it.
    fn observed(&self, cond: &Condition) -> String {
        match cond.var.as_deref() {
            Some(var) => format!("{var} = {}", self.board.get(var).unwrap_or(&Value::Null)),
            None => "nothing (the condition reads no blackboard key)".to_string(),
        }
    }

    /// Decode a node's `config` into the typed shape `T`.
    fn parse_config<T: for<'de> Deserialize<'de>>(&self, idx: usize) -> Result<T, EngineError> {
        let node = &self.tree.nodes[idx];
        serde_json::from_value(node.config.clone()).map_err(|e| EngineError::BadConfig {
            node_id: node.id.clone(),
            message: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests;
