//! The `nyx_next` MCP verb — the SINGLE-SESSION runtime TICK over the pure behavior-tree
//! interpreter (v2.8 — Harnais : moteur d'orchestration, Phase 3, tasks
//! `01KY71J0Q1DZX4MYQ6D6M1Y2VN` / `01KY71J1AKNE9SQQVW2DNVEQB5`).
//!
//! # What this module is
//!
//! [`crate::engine`] is the PURE graph walker: it advances a stored tree and mutates a blackboard,
//! but it runs NO real effect. This module is the EXPOSURE + EFFECTS layer on top: the `nyx_next`
//! verb the thin dispatcher calls in a loop. Each call
//!
//!   1. loads the run (its Spec binding + blackboard) and fetches the in-memory [`Engine`] for it
//!      (the single-session traversal stack lives in the [`EngineRuntime`] registry across calls),
//!      handing it the graph as it stands NOW ([`adopt_current_tree`]) so an edit made mid-run is
//!      executed from the next tick and not from the next session,
//!   2. decodes the dispatcher's reported `last_result` into a [`NodeResult`] for the leaf the run
//!      was parked on,
//!   3. TICKS the engine, and — this is the crux — runs nyx's OWN effects INLINE:
//!      - a `gate_check` is executed BY NYX over the PTY ([`crate::command::CommandPty`]); nyx has
//!        the PTY, so it runs the managed command itself and reads the exit code as the verdict,
//!      - a `transition` is IMPOSED BY NYX through the GUARDED Spec authority
//!        ([`crate::db::transition_spec`]), so the state rule (`source_allowed`) is enforced here,
//!   4. SUSPENDS on a dispatcher-facing leaf (`spawn_subagent` / `judge` / `ask_human` / `produce`
//!      / `wait`), persisting the run's cursor + blackboard, and returns that atomic action for the
//!      dispatcher to perform — carrying the `note`s the walk went past, which SPEAK but suspend
//!      nothing of their own ([`with_notes`]).
//!
//! # ONE tick at a time per run (v2.8 L1)
//!
//! Every step above is one READ-MODIFY-WRITE over a single run, and the MCP surface serves its
//! requests on a worker pool, so two `nyx_next` calls on the SAME run can reach this handler at
//! once. Two such ticks would each walk from the same persisted cursor, each run the gate and spawn
//! the sub-agent again, and the last to persist would silently overwrite the other. So a run is
//! ENTERED by one tick at a time ([`EngineRuntime::enter_tick`], taken before the run is even read),
//! and a second concurrent tick is REFUSED rather than queued ([`refuse_concurrent_tick`]).
//! Ticks on DIFFERENT runs — and every other tool — are untouched by it.
//!
//! # Enforcement lives HERE, in the handler — no provider hook (task #5)
//!
//! The two hard gates are anchored on the `nyx_next` verb itself, NOT on a `PreToolUse` /
//! `SubagentStop` provider hook — so the enforcement is identical on Claude / Codex / OpenCode, and
//! a provider without those hooks loses only the comfort signal, never the enforcement:
//!
//!   * GATE — when nyx runs a BLOCKING `gate_check` and it is RED, `nyx_next` REFUSES to advance
//!     past it: it never returns a `transition`/`done` while that gate is red (it re-spawns inside a
//!     well-authored loop, and hard-refuses with [`RpcCode::InvalidState`] on a graph that would
//!     otherwise advance). This is exactly the P1 spike invariant "un gate rouge ne renvoie jamais
//!     avance", lifted into the real verb. And a BLOCKING gate with NO commands is refused the same
//!     way, BEFORE it is run ([`refuse_vacuous_gate`]): a gate that verifies nothing cannot certify
//!     anything, so its "green" is not a verdict — it is the absence of one.
//!   * DECLARED EXPECTATION — a gate that says what its check must DO
//!     ([`crate::engine::GateExpectation`]) is graded on THREE values, not two: `passed`, `failed`
//!     (a real non-zero exit) and `indeterminate` (a timeout, a spawn failure, any infrastructure
//!     fault). `green` is answered only by `passed`, `red` only by `failed`, and `indeterminate`
//!     answers NEITHER. A blocking gate whose attempt does not answer its declaration is refused ON
//!     THE SPOT and stays refused until one does ([`refuse_gate_expectation`]) — never through the
//!     M5 red anchor, which re-runs what it holds and would therefore wedge a reproduction gate the
//!     moment the fix made it green. A gate that declares NOTHING keeps the anchor and is enforced
//!     exactly as it was before expectations existed.
//!   * STATE RULE — a `transition` node routes through [`crate::db::transition_spec`], whose
//!     `source_allowed` guard REFUSES an illegal lifecycle move; the refusal surfaces from the
//!     `nyx_next` handler, so a forbidden transition cannot be walked past either.
//!   * HUMAN CONSENT — the state rule is objective, but a human's APPROVAL reaches nyx as a string
//!     the dispatcher produced, and the engine cannot tell a human's decision from an agent's claim
//!     about one. Observed live: the user had said, ONE TURN EARLIER, "when nyx asks me to finalise,
//!     move it to ready"; nyx then asked, and the session answered `yes` on their behalf — and said
//!     so — and a Spec reached `ready` they never validated. The missing fact is exactly that: NO
//!     HUMAN TURN happened between the question and the answer. So an `ask_human` whose answer
//!     COMMITS something the session does not own ([`crate::engine::Tree::ask_human_commitment`],
//!     DERIVED from the graph, never declared) is asked, relayed and answered like every other
//!     question — but the tick that receives the answer REFUSES it unless the human-turn ledger of
//!     the terminal it was asked on ([`crate::db::HumanTurn`], fed by the `UserPromptSubmit` hook,
//!     which no tool call can produce) NAMES a different turn than it did when nyx posed the
//!     question ([`accept_consent_answer`]). The asking is recorded on
//!     `workflow_runs.pending_consent`; the accepted answer is journaled with the RAW human prompt
//!     behind it. Both sides of that comparison are PERSISTED, which is what lets the answer arrive
//!     in a later session than the question — every `claude -p` invocation is one.
//!
//!     AND ONE TURN AUTHORISES ONE COMMITMENT. A different turn is necessary and not sufficient:
//!     nothing SPENT it, so two runs parked on a commitment question from the SAME terminal — two
//!     `/ndev` on two Specs, the ordinary way to work — both read a last turn different from their
//!     own after ONE sentence from the human, and both accepted. So accepting MARKS the turn it
//!     spends ([`crate::db::HumanTurn::consumed_by`]), in the very transaction that closes the
//!     question, and the second run is refused IN ITS OWN TERMS: the human did speak, their words
//!     answered another question, and what moves this one is being asked it.
//!
//!     TWO commitments qualify, and they are one rule read twice — an answer that disposes of
//!     something the session does not own. The Spec's LIFECYCLE (an engaging `ready`/`done`/`cancel`
//!     the answer gates) was the first. The user's BUILD BUDGET is the second (J2): a question whose
//!     answer re-opens a loop whose body spawns buys another round of sub-agents. ndev's "rework or
//!     abandon" is exactly that question and was left out because it commits no transition — so one
//!     `/ndev` answered it alone, on a general authorisation given earlier, and ran 3 h 35 / 109
//!     ticks / $69.48 over rounds that after the first only produced second-order findings. Both
//!     failures are the same session being helpful about something that was not its call.
//!
//!     THE QUESTION BELONGS TO THE SESSION IT IS PUT TO, and that is what makes the check
//!     survivable. Its answer is weighed against ONE terminal's ledger, so the receipt is re-armed on
//!     whoever picks the run up ([`crate::db::open_run_consent`]) — while STAYING frozen for the
//!     session already being asked, whose dispatcher polls `nyx_next` and would otherwise walk its
//!     own reference forward past the human's turn. And a caller with NO terminal is refused AT the
//!     question ([`refuse_unattributable_consent`]): there is no ledger keyed on nothing, so such a
//!     session could only collect answers nyx must refuse.
//!
//! Both refusals are STICKY, and that is what makes them enforcement rather than a speed bump:
//!   * the red-gate anchor is carried on the PERSISTED blackboard ([`BLOCKING_GATE_RED_KEY`]), not in
//!     a call-local, because the guarded `transition`/`done` is normally reached on a LATER call of
//!     the dispatcher loop than the gate that must block it;
//!   * a refused leaf is marked for REPLAY ([`Engine::replay_current`]), so the next call
//!     RE-ATTEMPTS it — without that, simply calling `nyx_next` again (exactly what the dispatcher
//!     is told to do) would ack the parked leaf and walk straight past the refused effect.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use portable_pty::PtySize;
use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::engine::{
    Action, Blackboard, Engine, EngineError, GateExpectation, GateGrade, GateVerdict, NodeResult,
    NodeType, Step, Tree,
};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, optional_str, require_str};
use crate::mcp_tools_core::{ChangedTopic, ToolOutcome};

/// `nyx_next` — the runtime tick the thin dispatcher calls in a loop. Advertised in `tools/list`
/// and routed from [`crate::mcp_tools_core::dispatch_extension_tool`].
pub const NYX_NEXT_TOOL: &str = "nyx_next";

/// `run_start` — the run-lifecycle ENTRY the thin dispatcher calls ONCE before its `nyx_next` loop.
/// Advertised in `tools/list` and routed from [`crate::mcp_tools_core::dispatch_extension_tool`].
pub const RUN_START_TOOL: &str = "run_start";

/// The blackboard key carrying the SET (a JSON array) of BLOCKING `gate_check` node_ids nyx ran RED.
/// It lives on the board — not in a local — because the board is what gets PERSISTED: the dispatcher
/// loop returns to the caller between the gate and the `transition`/`done` it must guard, so a
/// call-local anchor would be dropped and the red gate would stop blocking anything. Each id is
/// cleared only when ITS OWN gate runs GREEN (a set, so one green gate never clears another's red).
///
/// `pub(crate)` because it is a FIXED key the engine writes without any node declaring it, and rule
/// (4)'s single-writer condition has to census it ([`crate::mcp_authoring`]'s
/// `node_written_board_keys`).
pub(crate) const BLOCKING_GATE_RED_KEY: &str = "blocking_gate_red";

/// Hard cap on ONE `gate_check` command. nyx runs the gate itself over the PTY, ON the `nyx_next`
/// dispatch path, so a managed command that never exits (a watcher, a dev server pointed at by
/// mistake) would hang the verb — and the whole session — forever. On expiry the PTY is dropped
/// (which kills the process tree) and the gate is RED: a gate that cannot prove itself green is not
/// green. Mirrors the bounded-join discipline of [`crate::command`].
const GATE_TIMEOUT: Duration = Duration::from_secs(600);

/// Hard cap on the WHOLE gate — every managed command TOGETHER — in ADDITION to the per-command
/// [`GATE_TIMEOUT`] (M6). The gate runs INLINE on the `nyx_next` dispatch path, so it must resolve
/// well within the MCP client's own request patience: several commands each finishing under the
/// 600s per-command cap can still, summed, outlast the client and leave the session wedged with no
/// verdict. 90s is a deliberate ceiling — comfortably under a typical MCP client timeout (Claude
/// Code's is minutes), yet ample for a real build+test gate — past which the gate is CUT SHORT with
/// a "total time budget" marker rather than a hang.
///
/// This is a PATIENCE budget, and only that. It used to be a freeze budget too — the gate ran on the
/// MCP server's single accept thread, so these 90 seconds were 90 seconds during which nyx answered
/// nobody (v2.8 L1). The surface is served by a worker pool now, so a gate holds ONE worker and its
/// own run; the number is unchanged because what it bounds — how long a caller waits for a verdict —
/// never was the problem.
pub(crate) const GATE_TOTAL_TIMEOUT: Duration = Duration::from_secs(90);

/// Bounded tail kept of EACH gate command's combined STDOUT/STDERR (M6): the last ~16 KB, dropped
/// from the FRONT so a chatty command cannot grow memory without bound. 16 KB comfortably holds a
/// failing test's summary and a stack trace or two — the END is exactly the part that explains a
/// failure — while staying small enough to ride in the tick payload and the persisted blackboard.
const GATE_OUTPUT_TAIL_BYTES: usize = 16 * 1024;

/// The blackboard key carrying the MOST RECENT non-empty gate's structured result (a bounded
/// [`GateOutcome`] JSON, tagged with its `node_id`) — M6. It is PERSISTED with the run (so the UI's
/// run card / M6 tab can read WHY a gate was red after the fact) and rides the tick's
/// `blackboard_delta` (so the dispatcher sees it live). Bounded by construction: each command keeps
/// only a [`GATE_OUTPUT_TAIL_BYTES`] tail.
///
/// `pub(crate)` for the same reason as [`BLOCKING_GATE_RED_KEY`]: a FIXED key no node declares, which
/// rule (4)'s single-writer condition must still census.
pub(crate) const LAST_GATE_RESULT_KEY: &str = "last_gate_result";

// ============================================================================
// The single-session engine registry
// ============================================================================

/// A run's engine SLOT: the resident engine behind a mutex, because the registry hands out `&mut
/// Engine` from a map several threads share. Mutual exclusion between ticks of the SAME run is NOT
/// this mutex's job — it is [`EngineRuntime::enter_tick`]'s, which is taken before the run is even
/// read from the DB and therefore covers the whole read-walk-persist, not just the walk.
type EngineSlot = Arc<Mutex<Engine>>;

/// The SINGLE-SESSION registry of live [`Engine`]s, keyed by `workflow_runs.id`. The engine holds
/// the traversal STACK (which loop iteration / sequence child the run is on) — state the persisted
/// `current_node_id` + `blackboard` alone cannot reconstruct — so it must live in memory across the
/// `nyx_next` calls of ONE session. Unlike the [`Db`] — the shared store on the dispatch path,
/// instantiated once by the shell and INJECTED as a parameter through dispatch — this registry
/// is NOT threaded as an argument: it is reached through a deliberate process-global singleton
/// ([`shared_runtime`]). The persisted state (`current_node_id` + `blackboard`) lives in the DB;
/// this global only caches the in-memory traversal stack the DB cannot reconstruct, and the
/// testable core stays injectable regardless — [`nyx_next`] takes `runtime` by parameter, so only
/// the single live dispatch call site touches the global.
///
/// What it does NOT cache is the TREE: a resident engine is handed the freshly-read graph before every
/// tick ([`adopt_current_tree`]), because a [`Tree`] carries the nodes' decoded config and a cached one
/// made a mid-run `node_update` invisible to the very run it was made for.
///
/// CROSS-SESSION resume IS supported (v2.8 review B1): the traversal stack is ALSO persisted to
/// `workflow_runs.traversal` (serialized by node id) on every tick, so a cold `nyx_next` on a run
/// absent from the registry does NOT replay the graph from the root (which would re-execute its
/// effects) — [`build_engine`] RESTORES the persisted stack instead ([`Engine::restore`]), or starts
/// FRESH for a run with no cursor. This registry stays the fast in-session cache; the DB is the
/// durable source. A PROGRESSED run whose traversal was never persisted (a pre-B1 run) is still
/// refused, as is one whose traversal no longer fits a structurally-edited tree.
#[derive(Default)]
pub struct EngineRuntime {
    runs: Mutex<HashMap<String, EngineSlot>>,
    /// The runs a tick is CURRENTLY inside — see [`EngineRuntime::enter_tick`].
    ticking: Mutex<HashSet<String>>,
}

impl EngineRuntime {
    /// A fresh, empty registry.
    pub fn new() -> Self {
        EngineRuntime {
            runs: Mutex::new(HashMap::new()),
            ticking: Mutex::new(HashSet::new()),
        }
    }

    /// ENTER `run_id` for the duration of one tick, or `None` when something is already inside it.
    ///
    /// The MCP surface serves its requests on a worker POOL ([`crate::mcp::REQUEST_WORKERS`]), so
    /// two `nyx_next` calls on the same run can now reach this handler at once — which the single
    /// accept thread it replaces made impossible by accident. And a tick is a READ-MODIFY-WRITE over
    /// one run: it reads the row (cursor, blackboard, traversal), walks, runs nyx's own effects, and
    /// persists the result. Two of them interleaved would each walk from the SAME cursor, each
    /// re-run the gate and re-spawn the sub-agent, and whichever persisted last would silently
    /// overwrite the other. So a run is entered by ONE tick at a time.
    ///
    /// `run_delete` takes it too ([`crate::mcp_authoring::run_delete`]): deleting a run is the other
    /// way to make a tick's read-modify-write land on something that is no longer there, and it is
    /// the very gesture nyx's own refusals tell a caller to make while a run is live.
    ///
    /// NON-BLOCKING by design: the caller that loses is refused rather than queued
    /// ([`refuse_concurrent_tick`] argues why).
    pub(crate) fn enter_tick(&self, run_id: &str) -> Option<TickGuard<'_>> {
        if !self.ticking().insert(run_id.to_string()) {
            return None;
        }
        Some(TickGuard {
            runtime: self,
            run_id: run_id.to_string(),
        })
    }

    /// The slot for `run_id`, or `None` when this session has never ticked it.
    fn slot(&self, run_id: &str) -> Option<EngineSlot> {
        self.lock().get(run_id).cloned()
    }

    /// Install `engine` as `run_id`'s slot and hand back the shared handle. Only ever called from
    /// INSIDE that run's [`TickGuard`], so no other tick can be installing the same run's slot
    /// concurrently — the entry is this tick's to write.
    fn install(&self, run_id: &str, engine: Engine) -> EngineSlot {
        let slot: EngineSlot = Arc::new(Mutex::new(engine));
        self.lock().insert(run_id.to_string(), Arc::clone(&slot));
        slot
    }

    /// Drop a finished (or DELETED) run's slot so the process-global registry does not accumulate the
    /// full tree + blackboard of every run the session ever drove.
    pub(crate) fn forget(&self, run_id: &str) {
        self.lock().remove(run_id);
    }

    /// The ids of the runs THIS session is driving (the registry's keys). A run enters on its first
    /// `nyx_next` and leaves when it finishes ([`EngineRuntime::forget`]), so this IS the session ↔
    /// run mapping — what [`bind_session_run_to_spec`] needs to know WHICH run a Spec created
    /// mid-flight belongs to (F5).
    fn session_run_ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, EngineSlot>> {
        self.runs.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn ticking(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.ticking.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Holds a run's tick slot for the whole of one `nyx_next`, releasing it on DROP — which covers
/// every early return (a refusal, a DB failure) and a panic in the walk alike, so a tick that ends
/// badly can never leave a run permanently un-tickable.
pub(crate) struct TickGuard<'a> {
    runtime: &'a EngineRuntime,
    run_id: String,
}

impl Drop for TickGuard<'_> {
    fn drop(&mut self) {
        self.runtime.ticking().remove(&self.run_id);
    }
}

/// Build the "this run is already being ticked" refusal (v2.8 L1).
///
/// REFUSED, not queued — worth arguing, because waiting looks like the kinder answer:
///
///   * a tick holds its run for as long as its gate takes (up to [`GATE_TOTAL_TIMEOUT`]), and a
///     waiting tick would hold an MCP WORKER for that whole time. Queueing here re-imports, one run
///     at a time, exactly the worker starvation the pool exists to remove;
///   * a queued tick would then fold a STALE answer. Its `last_result` describes the leaf the run
///     was parked on BEFORE the tick ahead of it moved the cursor; waiting does not make that answer
///     current, it only delays the moment it is applied to the wrong leaf. Refusing sends the caller
///     back to re-read the run, which is the only thing that can make its next answer true;
///   * and nothing is lost. `nyx_next` is a POLL the dispatcher already calls in a loop, so the
///     caller's own next iteration IS the retry — including for the caller most likely to land here,
///     one that wrongly believes its first call hung and fired a second.
///
/// The run is left exactly as it was: this refusal happens before the run is read, so the call
/// touches nothing.
fn refuse_concurrent_tick(run_id: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "run '{run_id}' is already being advanced by another nyx_next call — nyx ticks a run \
             ONE call at a time, so two ticks can never walk from the same cursor and overwrite \
             each other's work. NOTHING was changed by this call: let the in-flight tick return (it \
             may be running a gate, which nyx bounds at {}s) and call nyx_next again",
            GATE_TOTAL_TIMEOUT.as_secs()
        ),
    )
    .with_data(json!({ "reason": "run_busy", "run_id": run_id }))
}

/// Build the "the run I must REPLACE is being ticked right now" refusal ([`start_or_replace_run`]).
///
/// Sibling of [`refuse_concurrent_tick`] and of `run_delete`'s own refusal — same `run_busy` marker,
/// because it is the same fact about the same row: something is inside it. What differs is what the
/// caller was trying to do (open/resume a run, and the resolution landed on a corpse), so the
/// sentence says that instead of "you called nyx_next twice".
fn refuse_busy_replacement(run_id: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "run '{run_id}' can no longer be advanced and has to be REPLACED, but a nyx_next call is \
             inside it right now — dropping it under that tick would throw away what the tick has \
             already done (a gate it ran, a transition it imposed) with no way left to say so. \
             NOTHING was changed by this call: let the in-flight tick return (it may be running a \
             gate, which nyx bounds at {}s) and call again",
            GATE_TOTAL_TIMEOUT.as_secs()
        ),
    )
    .with_data(json!({ "reason": "run_busy", "run_id": run_id }))
}

/// The process-global [`EngineRuntime`] the live MCP dispatch uses — a DELIBERATE process singleton,
/// and the FIRST intentional process-global mutable state in `nyx-core/src` (every other shared
/// store on the dispatch path is injected, not held in a global). The engine runtime is a
/// session/process singleton by nature (single-session), so a global fits — and it keeps the
/// `dispatch_extension_tool` signature (and thus both shells' call sites) untouched. Tests build a
/// LOCAL [`EngineRuntime`] and call [`nyx_next`] directly, so they never share this instance.
pub fn shared_runtime() -> &'static EngineRuntime {
    static RUNTIME: OnceLock<EngineRuntime> = OnceLock::new();
    RUNTIME.get_or_init(EngineRuntime::new)
}

/// BIND the session's in-flight run to the Spec it just CREATED, returning the run bound (or `None`
/// when nothing was). Called by `spec_create` (F5).
///
/// A `/nspec` run legitimately starts with NO Spec — its own graph authors one — but nothing ever
/// tied the run to that Spec, and an unbound run is INERT: `query spec_status` answers `null`, so the
/// commit selector's `final_status == "draft"` never matches, the `[always]` note wins, the `ready`
/// transition NEVER runs, and the flow ends `done` on a Spec left `draft` — in total silence. The
/// same nothing disarms every other Spec fact of that run, and of any child it opens.
///
/// The CANDIDATE is deliberately narrow, because binding the wrong run is worse than binding none:
///   * the runs of THIS session ([`EngineRuntime::session_run_ids`] — the registry is the only
///     session ↔ run mapping there is), never some other session's;
///   * still LIVE, and carrying NO Spec — a bound run is never re-bound and never stolen (the
///     authority [`db::set_run_spec`] re-checks that in its own WHERE);
///   * TOP-LEVEL (no `parent_run_id`): a `sub_workflow` child INHERITS its Spec from its parent at
///     creation, it is not the flow that authored one;
///   * and EXACTLY ONE such candidate. Two in-flight authoring runs in one session make "the run
///     that created this Spec" a guess, and a guess here silently binds a Spec to the wrong flow.
pub(crate) fn bind_session_run_to_spec(
    db: &Db,
    runtime: &EngineRuntime,
    spec_id: &str,
) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    for run_id in runtime.session_run_ids() {
        let Ok(Some(run)) = db.with_conn(|c| db::get_workflow_run(c, &run_id)) else {
            continue;
        };
        if run.spec_id.is_none()
            && run.parent_run_id.is_none()
            && !db::run_status_is_terminal(&run.status)
        {
            candidates.push(run.id);
        }
    }
    let [run_id] = candidates.as_slice() else {
        return None;
    };
    match db.with_conn(|c| db::set_run_spec(c, run_id, spec_id)) {
        Ok(1..) => Some(run_id.clone()),
        _ => None,
    }
}

// ============================================================================
// The verb handlers
// ============================================================================

/// `run_start` — `{ spec_id? | workspace_id? | cwd?, workflow_id?, scope? }` → `{ run_id,
/// workflow_id, spec_id, scope }`. OPEN a new run so the thin dispatcher has a `run_id` to tick:
/// resolve the target workflow (an explicit `workflow_id`, else the resolved project's active
/// workflow of `scope` — default `ndev` — via [`db::resolve_active_workflow`]), optionally bind it
/// to the resolved Spec, and create the [`db::WorkflowRun`] parked at its START (cursor NULL, empty
/// blackboard) — the FIRST `nyx_next` drives it from the root. This is the run-creation entry the
/// v2.8 review found missing (B2): before it, no verb produced a `run_id`, so `/ndev`'s dispatcher
/// manual demanded a `run_id` nothing could give.
/// Whether a scope's flow OPERATES ON an existing Spec, and so may not start without one.
///
/// `ndev` / `ndoc` drive a Spec's lifecycle; `coder` / `prototyper` build a Spec's tasks and its
/// prototype (a `sub_workflow` node always binds them to the parent's Spec, so requiring it costs
/// them nothing and catches a hand-started orphan). `nspec` is EXCLUDED — authoring is exactly the
/// flow allowed to begin with no Spec and create one — and so is `custom`, which declares no Spec
/// contract at all.
fn scope_requires_spec(scope: &str) -> bool {
    matches!(
        scope,
        db::WORKFLOW_SCOPE_NDEV
            | db::WORKFLOW_SCOPE_NDOC
            | db::WORKFLOW_SCOPE_CODER
            | db::WORKFLOW_SCOPE_PROTOTYPER
    )
}

/// THE REFUSAL A `spec_id` NAMING NO SPEC EARNS — `invalid_id`, and ACTIONABLE.
///
/// `unknown spec '3'` said neither why nor what to send instead, and the agent that met it had no
/// way forward but to guess (observed: four MCP calls to recover an id nyx already knew). So when
/// the token IS a recognised reference carrying a number, the sentence names the accepted forms —
/// from [`crate::spec_ref::ACCEPTED_FORMS`], the same string the parser is written beside — and
/// the project's most probable candidate, id included, so the next call can be formed from the
/// answer.
///
/// TWO SHAPES ARE LEFT EXACTLY AS THEY WERE. Prose is not a reference and keeps the bare sentence
/// (nyx never claimed to read it), and so does a canonical id: it carries no number to be near,
/// and dressing its refusal up would change the behaviour of the calls that already pass one.
///
/// THE ANCHORLESS TRAILER IS THE CALLER'S, because what a call can do about having no project is
/// not the same on every verb. `run_start` takes a `cwd` / `workspace_id` / `project_id` and can
/// honestly offer them — honestly meaning ADVERTISED, all three named in its `inputSchema`, since a
/// caller reading `tools/list` cannot act on an argument the schema omits (`project_id` was
/// honoured there but unnamed until finding `01M1008NHM9RRA1ZAS4V7K1PM4`); `run_bind_spec` takes
/// NONE of the three, so offering them there sent an agent hunting for arguments the schema does
/// not have — half a refusal that cannot be acted on (finding `01M0ZWAH9EQDAZR66AFS1YJFPY`). Each
/// caller writes its own, punctuation included, and it is appended to the one thing that is always
/// true: send a form that carries its own project.
///
/// A `KEY-N` IS ANSWERED ABOUT ITS OWN PROJECT, never about the caller's. The key CARRIES a
/// project ([`crate::spec_ref`]), so counting its number inside the project the call happens to
/// anchor would answer a question nobody asked: an `ACME-3` refused under a `cwd` of `NYX` used to
/// come back "project nyx (NYX) has no Spec 3; its nearest is #2 …" — a suggestion pointing at
/// ANOTHER project's Spec, on the one refusal whose whole job is to stop a session substituting a
/// Spec of its own. And with no anchor at all it advised sending "the canonical id or the human
/// key", which is precisely the form that had just been sent. So the key is resolved to ITS project
/// first, and a key no project carries is told so by name instead of being counted anywhere.
///
/// The outer `Err` is a database failure, never a refusal — the refusal is the `Ok` value.
fn unknown_spec_refusal(
    db: &Db,
    project_id: Option<&str>,
    token: &str,
    anchorless_trailer: Option<&str>,
) -> Result<RpcError, RpcError> {
    let bare = || RpcError::new(RpcCode::InvalidId, format!("unknown spec '{token}'"));
    let forms = || {
        format!(
            "unknown spec '{token}' — nyx accepts {}",
            crate::spec_ref::ACCEPTED_FORMS
        )
    };
    // WHERE the number is counted, and WHICH number it is. A `KEY-N` names its own project; a
    // `#N` / `N` is counted in the caller's, when the caller has one.
    let (number, project_id) = match crate::spec_ref::parse(token) {
        Some(crate::spec_ref::SpecRef::Key { key, number }) => {
            let found = db
                .with_conn(|c| db::project_by_key(c, &key))
                .map_err(internal_db)?;
            let db::ProjectByKey::Found(project) = found else {
                // The key designates no ONE project — absent, or ambiguous once case is folded.
                // Nothing about the caller's project would answer that.
                return Ok(RpcError::new(
                    RpcCode::InvalidId,
                    format!(
                        "{}. No project of this nyx carries the key '{key}', so '{token}' names \
                         nothing — check the key against the project the Spec belongs to \
                         (list_projects), or send its canonical id",
                        forms()
                    ),
                ));
            };
            (number, Some(project.id))
        }
        Some(crate::spec_ref::SpecRef::Number(number)) => {
            (number, project_id.map(str::to_string))
        }
        // Not a reference at all, or a canonical id: unchanged.
        _ => return Ok(bare()),
    };
    let mut message = forms();
    match project_id.as_deref() {
        // A number is counted INSIDE a project, and there is none here. Naming the forms that
        // carry their own is the half of this refusal that is always actionable; the other half —
        // how to give the call a project — is only offered when this caller actually has one.
        None => {
            message.push_str(&format!(
                ". A '#N' or a bare number is counted inside a project and this call anchors none \
                 — send {}",
                crate::spec_ref::ANCHORLESS_FORMS
            ));
            if let Some(trailer) = anchorless_trailer {
                message.push_str(trailer);
            }
        }
        Some(project_id) => {
            let (project, candidate) = db
                .with_conn(|c| -> diesel::QueryResult<_> {
                    Ok((
                        db::get_project(c, project_id)?,
                        crate::spec_ref::likely_candidate(c, project_id, number)?,
                    ))
                })
                .map_err(internal_db)?;
            let named = project
                .as_ref()
                .map(|p| format!("project {} ({})", p.name, p.key))
                .unwrap_or_else(|| format!("project {project_id}"));
            match candidate {
                Some(spec) => message.push_str(&format!(
                    ". {named} has no Spec {number}; its nearest is #{} \"{}\" (spec_id {})",
                    spec.number, spec.title, spec.id
                )),
                None => message.push_str(&format!(
                    ". {named} holds no Spec yet — author one rather than guessing an id"
                )),
            }
        }
    }
    Ok(RpcError::new(RpcCode::InvalidId, message))
}

pub fn run_start(
    db: &Db,
    runtime: &EngineRuntime,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let scope = optional_str(args, "scope")?.unwrap_or("ndev").to_string();
    let mut warnings = Vec::new();
    // THE WORKSPACE ANCHOR IS RESOLVED HERE — BEFORE ANY WRITE — and only APPLIED further down.
    //
    // It is a pure READ (`db::resolvable_workspaces` + the shared cwd→workspace selector), so hoisting it
    // costs nothing; what it buys is that its REFUSAL costs nothing either. Since the empty-string
    // hardening ([`crate::mcp_context::resolve_workspace_for_start`] reads `workspace_id` through
    // `anchor_str`), this resolution can FAIL — and it used to fail AFTER `start_or_replace_run`,
    // which drops a run whose graph changed and inserts a fresh one. A `run_start(workspace_id: "")`
    // therefore destroyed a live run, opened another, and only then answered `invalid_argument`: the
    // caller reads a refusal while the run pool has moved under it, the exact half-committed shape
    // this PRD refuses everywhere else ("prevalidate, then mkdir"). Resolved here, refused here,
    // nothing written.
    let located = crate::mcp_context::resolve_workspace_for_start(db, args)?;
    // G2 — an AMBIGUOUS location must not cost an AUTHORING flow its `cwd`. Resolving the Spec from a
    // workspace holding SEVERAL active ones is refused (N14b: picking one silently attributes work to
    // an arbitrary Spec), and that refusal is right for a flow that works ON a Spec. For one that
    // CREATES its own it refuses the wrong thing entirely, and the caller's only way through is to drop
    // the `cwd` and retry — which is exactly what a live /nspec did, opening a run with no workspace
    // anchor because a Spec it never wanted was ambiguous (observed: `run_start(scope:"nspec",
    // cwd:"…")` → "has 3 active Specs", then `run_start(scope:"nspec")`). So for such a flow the
    // ambiguity resolves NO Spec — never an arbitrary one, N14b is untouched — the run starts unbound
    // as designed, the `cwd` keeps anchoring the workspace, and the refusal's own sentence rides the
    // `warnings` the dispatcher relays.
    //
    // Keyed on the `scope` ARGUMENT, the only thing known before the workflow is resolved (the project
    // comes out of this very call). It is the CONSERVATIVE reading: an explicit `workflow_id` leaves
    // `scope` at its "ndev" default, so such a caller keeps the refusal.
    let (project_id, spec_id) = match crate::mcp_context::resolve_run_target(db, args) {
        Ok(target) => target,
        Err(e) if !scope_requires_spec(&scope) && crate::mcp_context::is_ambiguous_spec(&e) => {
            warnings.push(json!({
                "code": SPEC_AMBIGUOUS_WARNING_CODE,
                "message": format!(
                    "{} — this '{scope}' run therefore starts bound to NO Spec (the flow authors its \
                     own). Re-run with a spec_id to work on one of them instead",
                    e.message
                ),
            }));
            (crate::mcp_context::resolve_run_project(db, args)?, None)
        }
        Err(e) => return Err(e),
    };
    // A bound Spec must EXIST: an explicit bad `spec_id` would otherwise surface as an opaque FK
    // error from create_workflow_run rather than a clean `invalid_id` (v2.8 review L-a).
    //
    // THIS IS THE LINK THAT PRODUCED THE DUPLICATE, and its POSITION is the fix. A designated
    // reference that resolves nothing (`3`, `NYX-99`) reaches here as the token the caller typed —
    // [`crate::mcp_context::resolve_spec_arg`] hands back what it could not resolve — and is
    // refused HERE, BEFORE `start_or_replace_run` writes anything. Opening a run anyway is what the
    // observed session did: unbound, `spec_status` resolved nothing, the entry gate fell through to
    // "No Spec yet — author a NEW one from scratch", and the flow ordered a duplicate of the very
    // Spec the human had named. A run that never opens cannot do that; and a refusal that left a
    // run behind would be worse than the bug, so nothing above this point writes.
    if let Some(sid) = spec_id.as_deref() {
        if db
            .with_conn(|c| db::get_spec(c, sid))
            .map_err(internal_db)?
            .is_none()
        {
            // `run_start` DOES take the three AND advertises the three, so it is honest here to
            // offer them: what this sentence names, `tools/list` names too.
            return Err(unknown_spec_refusal(
                db,
                project_id.as_deref(),
                sid,
                Some(RUN_START_ANCHOR_TRAILER),
            )?);
        }
    }
    let workflow = match optional_str(args, "workflow_id")? {
        Some(wf_id) => db
            .with_conn(|c| db::get_workflow(c, wf_id))
            .map_err(internal_db)?
            .ok_or_else(|| RpcError::new(RpcCode::InvalidId, format!("unknown workflow '{wf_id}'")))?,
        None => db
            .with_conn(|c| db::resolve_active_workflow(c, project_id.as_deref(), &scope))
            .map_err(internal_db)?
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidState,
                    format!(
                        "no '{scope}' workflow to run: neither the resolved project nor a built-in \
                         default has one — author one (workflow_create) or pass workflow_id"
                    ),
                )
            })?,
    };

    // A SPEC-SCOPED flow REFUSES to start unbound. `resolve_run_target` yields no Spec in two very
    // different situations: the workspace holds SEVERAL active Specs — already a loud refusal ("pass an
    // explicit spec_id") — and the SILENT one: no anchor was passed at all, or the workspace anchors
    // NONE. That silent case used to open an ORPHAN run, and an orphan run is inert: `spec_status`,
    // `spec_open_tasks` and `spec_can_transition` resolve against nothing and the Spec transitions
    // apply to nothing, so the graph walks a lifecycle that does not exist while the Spec it was meant
    // to drive never learns about it — and, carrying no Spec, the run cannot be found FROM one either
    // (the Spec's run modal legitimately reports "no run"). Both cases now fail the same way, loudly.
    //
    // Keyed on the RESOLVED workflow's own scope, never the `scope` ARGUMENT: an explicit `workflow_id`
    // picks the flow directly and leaves `scope` at its "ndev" default, so judging the argument would
    // refuse a perfectly valid custom workflow. `nspec` is the deliberate exception (authoring may
    // begin with no Spec and create one) and `custom` declares no Spec contract at all.
    if spec_id.is_none() && scope_requires_spec(&workflow.scope) {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "'{}' works ON a Spec and none could be resolved — pass an explicit `spec_id` (or a \
                 `cwd`/`workspace_id` anchoring exactly one active Spec). Starting unbound would open \
                 a run whose Spec queries and transitions apply to nothing",
                workflow.scope
            ),
        ));
    }

    // IDEMPOTENT (N20) and VERIFIED (M4): converge to AT MOST ONE live run per (workflow, spec) AND
    // hand back one that can actually be TICKED — [`start_or_replace_run`] owns both, and is the SAME
    // resolution the `sub_workflow` node opens its child through.
    //
    // A run bound to NO Spec never resumes: the convergence is keyed on the Spec because the Spec is
    // the identity, and an authoring `/nspec` has none yet — two invocations are two sessions on two
    // subjects, not one run opened twice.
    //
    // M4 — and the replacement SPEAKS. Dropping the dead run is the right recovery, but it used to
    // happen in total silence: the caller got a brand-new `run_id` with no hint that a run had just
    // been destroyed, so a session that had walked half the flow silently restarted from the top and
    // the human was told nothing (observed live). The dropped run is therefore reported as a
    // `warnings` entry — never an error, since replacing IS the correct move.
    //
    // AND SO DOES THE RESUME, which is the third of the three things this call can do. Created and
    // replaced each had their sentence; resumed had none, so it was told apart from a fresh start by
    // nothing at all — while it is the one outcome that lands the caller in the MIDDLE of a flow
    // somebody else (or they, days ago) left running. Announced on the same channel and, like the
    // replacement, as a WARNING rather than an error: resuming is the correct behaviour, N20 is
    // untouched, and nothing here creates or destroys a run.
    //
    // READ BEFORE THE WORKSPACE ANCHOR further down, for the reason the stranded report is: that
    // write bumps `updated_at`, and an age taken after it would report every resumed run as fresh.
    let (run, outcome) = start_or_replace_run(db, runtime, &workflow.id, spec_id.as_deref(), "{}")?;
    match &outcome {
        RunStartOutcome::Replaced(replaced) => {
            warnings.push(run_replaced_warning(&replaced.dropped, &replaced.why));
        }
        RunStartOutcome::Resumed => warnings.push(run_resumed_warning(db, &run)?),
        RunStartOutcome::Created => {}
    }

    // SIGNAL the runs left for dead. Nothing in nyx surfaces a `blocked` run, so they pile up in
    // silence — and they are not inert: a dead run bound to a Spec is EXACTLY what the (workflow,
    // spec) convergence above RESUMES the next time that Spec is driven, the same mechanism by which
    // one abandoned `/nspec` used to confiscate every later one.
    //
    // BEFORE the anchor below, which bumps `updated_at` on a run that had no workspace — that write
    // would rejuvenate the very corpse this call just resumed and hide it from its own report.
    warnings.extend(stranded_run_warnings(db, &run)?);

    // SIGNAL a run parked on a consent question NOBODY can answer as it stands (K1) — the other way
    // a resumed run hands itself back looking healthy.
    warnings.extend(unanswerable_consent_warning(db, &run)?);

    // ANCHOR the run on the dev workspace it was opened against (an explicit `workspace_id`, else the
    // `cwd`'s workspace): `apply_transition` reads it at the graph's entry `start` transition and poses
    // the Spec's workspace anchor in the same op — exactly what the manual `spec_transition start` did
    // (v2.8 review N24). No workspace resolves ⇒ the run stays unanchored (started by id alone).
    //
    // G2 — on a RESUMED run too, not just a fresh one. The anchor is a FACT about the run, not a
    // start-only effect: a run that opened without one (started by `spec_id` alone, or by a caller
    // whose first attempt was refused and who dropped its `cwd` to get through) could never acquire it
    // afterwards, and an unanchored run is an effector with no repos. Posing it later costs nothing and
    // re-runs no effect. What must NOT happen is MOVING an anchor the run already carries — the
    // authority's own `WHERE workspace_id IS NULL` guard refuses that, so a second `run_start` from a
    // different cwd resumes the run without re-homing it.
    // `located` was RESOLVED at the top of this function (a pure read, so its refusal precedes every
    // write — see the note there); only the WRITE lives here, after the stranded-run report that must
    // not be rejuvenated by this `updated_at` bump.
    if let Some(workspace_id) = &located {
        db.with_conn(|c| db::set_run_workspace(c, &run.id, workspace_id))
            .map_err(internal_db)?;
    }

    // PARITY WITH `agent_context`'s case 0 (review F1) — RULED, half implemented and half declined,
    // so the asymmetry between the two verbs is a decision on the record rather than an oversight.
    //
    // DECLINED — the "no Spec resolved" half. `run_start` already answers it in every branch, and
    // more loudly than a warning: a SPEC-SCOPED flow REFUSES to start unbound (the guard above),
    // which is precisely the answer `agent_context` may NOT give for role=dev/spec without breaking
    // /nspec. `nspec` is the one flow DESIGNED to begin with no Spec — its first node queries
    // `spec_status` and its second branches on the null to CREATE the Spec — so a systematic "no
    // Spec" warning would fire on 100 % of authoring starts, exactly what the exclusion just below
    // already declined for `query_without_spec_warnings`. And `custom` keeps that conditional
    // warning, which fires when the empty answer can actually mislead a branch. `agent_context` has
    // none of that structure to lean on: it RENDERS a manual that reads identically scoped or not,
    // which is why ITS case 0 must speak every time and this one must not.
    //
    // IMPLEMENTED — the "wrong folder" half, F1's sub-case (b), the one thing `run_start` was
    // genuinely silent about. A `cwd` matching no known workspace opens a run with NO workspace
    // anchor: an effector with no repos (see the anchor note above), from a caller who believed they
    // were running inside their dev workspace. Keyed on a `cwd` having been PASSED and the run
    // ending up unanchored all the same — `run_start(spec_id=…)` names no folder on purpose and is
    // never warned, and a RESUMED run that already carries an anchor has lost nothing.
    if located.is_none() && run.workspace_id.is_none() {
        if let Some(cwd) = optional_str(args, "cwd")? {
            warnings.push(json!({
                "code": RUN_LOCATION_UNRESOLVED_WARNING_CODE,
                "message": format!(
                    "cwd '{cwd}' matches no workspace nyx knows, so this run opens with NO workspace \
                     anchor and its effectors resolve no repos. Run it from inside the dev \
                     workspace's folder, or pass an explicit workspace_id"
                ),
            }));
        }
    }

    // SIGNAL what is wrong with THE GRAPHS this run will walk — this flow and every flow it opens
    // through a `sub_workflow` node. A gate that resolves to no command verifies nothing, and a
    // BLOCKING one is REFUSED at the tick, so saying it here turns "your build stops halfway with an
    // error" into "this flow cannot verify anything yet, fix it before you start"; a FORK left behind
    // by the built-in it came from is the same kind of fact — nothing fails, the project just quietly
    // stops receiving nyx's graph fixes. Both are found by following the sub_workflow hop, because
    // that is where the graph with the defect actually is.
    warnings.extend(flow_graph_warnings(db, &run, &workflow)?);

    // SIGNAL a graph that asks SPEC-SCOPED questions on a run bound to NO Spec. Every `query` fact
    // answers "nothing" rather than erroring on an unbound run (`null`, `0`, `false`) — deliberately,
    // since a fact is not an error. But the GRAPH cannot tell that "nothing" apart from a Spec with
    // genuinely nothing open, so a selector branching on it takes the wrong branch in silence. The
    // run is legitimate (`scope_requires_spec` let it start), so this is a warning and not a refusal.
    //
    // EXCEPT for `nspec`, whose FIRST node is `query spec_status` and whose second BRANCHES on the
    // null to create the Spec: there the empty answer is the designed signal, not a trap, and warning
    // on it would fire on every authoring start. That leaves `custom` — a graph that declares no Spec
    // contract, asks about one anyway, and has nobody who designed for the answer.
    if run.spec_id.is_none() && workflow.scope != db::WORKFLOW_SCOPE_NSPEC {
        warnings.extend(query_without_spec_warnings(db, &workflow.id)?);
    }

    Ok(ToolOutcome::changed(
        json!({
            "run_id": run.id,
            "workflow_id": workflow.id,
            // The NAME too (not just the id): with "most-recent wins" resolution, the dispatcher (and
            // the human) must SEE which workflow the run actually started on (v2.8 review N6).
            "workflow_name": workflow.name,
            "spec_id": run.spec_id,
            "scope": workflow.scope,
            // WHICH OF THE THREE THINGS THIS CALL DID — `created` | `resumed` | `replaced`. A
            // caller must not have to infer it from which warnings happen to be present: two of the
            // three used to publish one and the third published nothing, so "no warning" meant
            // either "brand new" or "you have just walked into somebody's half-finished session".
            "outcome": outcome.tag(),
            "warnings": warnings,
        }),
        ChangedTopic::Specs,
    ))
}

/// `run_bind_spec` — the tool name. Advertised in `tools/list` and routed from
/// [`crate::mcp_tools_core::dispatch_extension_tool`], like its two siblings above.
pub const RUN_BIND_SPEC_TOOL: &str = "run_bind_spec";

/// The MACHINE HANDLE of each `run_bind_spec` refusal, carried under `error.data.reason`. One
/// constant per precondition, because "the bind was refused" is not an answer a caller can act on:
/// the whole point of exposing this recovery is that its `no` says WHICH condition failed.
///
/// STABLE STRINGS — a caller (and the tests) key on them; the human sentence beside each is free to
/// be re-worded.
///
/// `pub(crate)` FOR THE SAME REASON AS [`RUN_LOCATION_UNRESOLVED_WARNING_CODE`]: the
/// `run_bind_spec` DESCRIPTOR names these codes to a caller, and it used to name them as re-typed
/// literals `mcp.rs` had no way to reach. Renaming a VALUE here would then have left the
/// advertisement lying in silence. The test
/// `mcp::tests::run_bind_spec_advertises_its_refusal_codes_by_constant` now confronts the
/// descriptor with THIS module: every code declared here is either named there, or is
/// `AUTHORITY_REFUSED`, whose omission is deliberate (a race is not something a caller can rule out
/// BEFORE calling) and therefore ASSERTED rather than assumed. The scan reads the module's own
/// declarations WHATEVER THEIR VISIBILITY — `pub const` and `pub(crate) const` reach equally far
/// inside a `pub(crate)` module, so neither may be the spelling that hides a code from the scan —
/// and it fails on a declaration it cannot parse rather than skipping it. A code added HERE mute
/// therefore fails instead of shipping unadvertised (findings `01M10063H5B65Q81V4HRW94W71`,
/// `01M10YD9QZYJJDAQQ1S8TGWSKH`). A `reason` that never becomes a constant of this module is
/// outside its reach: every call site passes one of these.
pub(crate) mod bind_refusal {
    /// The `run_id` is not one THIS session drives.
    pub const FOREIGN_RUN: &str = "run_not_in_this_session";
    /// The run already carries a Spec.
    pub const ALREADY_BOUND: &str = "run_already_bound";
    /// The run is a `sub_workflow` child; it inherits its Spec from its parent.
    pub const CHILD_RUN: &str = "run_is_a_child";
    /// The run is finished (`done` / `failed`).
    pub const TERMINAL_RUN: &str = "run_is_terminal";
    /// The target Spec already carries a live run of the same workflow (N20).
    pub const SPEC_HAS_A_LIVE_RUN: &str = "spec_already_has_a_live_run";
    /// The Spec belongs to another project than the run's workspace does.
    pub const PROJECT_DISAGREEMENT: &str = "spec_project_disagrees_with_run";
    /// The DB authority's own WHERE refused the write (a race, or a condition re-checked there).
    pub const AUTHORITY_REFUSED: &str = "bind_refused_by_the_authority";
}

/// The refusal shape every precondition of [`run_bind_spec`] answers with — `invalid_state` plus
/// the `reason` that NAMES which one failed. Written once so they cannot drift apart.
fn bind_refusal(reason: &str, run_id: &str, message: String) -> RpcError {
    RpcError::new(RpcCode::InvalidState, message)
        .with_data(json!({ "reason": reason, "run_id": run_id }))
}

/// `run_bind_spec` — `{ run_id, spec_id }` → `{ run_id, spec_id, workflow_id }`.
/// ATTACH an already-open run to the Spec it turned out to be about.
///
/// THIS IS A RECOVERY, NOT THE WAY TO ANCHOR A RUN. The normal way is to open the run ALREADY
/// anchored — `run_start` with the Spec named, which is what the command hook does when the human
/// typed a reference. This verb exists for the session that started WRONG: a run opened with no
/// Spec (the human named none, or their reference was not read), work already done inside it, and
/// no way out but to throw the session away. It is worth exposing precisely because throwing it
/// away is the alternative — so nothing here relaxes a guard to make the recovery easier.
///
/// THE PRECONDITIONS ARE [`bind_session_run_to_spec`]'s — NOT ONE IS RELAXED, and the ones that
/// read differently are said here rather than left for a reader to spot, because binding the wrong
/// run is worse than binding none. The callee states the same set (mcp_engine.rs, `The CANDIDATE is
/// deliberately narrow`); this verb states them as the guards it evaluates, in the order it
/// evaluates them:
///   * the run is NOT TERMINAL. This EXPLICITS the callee's "still LIVE" rather than adding to it,
///     and it is evaluated FIRST for a reason the callee never faced: the session registry evicts a
///     run when it finishes, so on a finished run "is it in the registry?" no longer answers "is it
///     yours?" — see the comment on the guard itself;
///   * the run is one THIS session drives ([`EngineRuntime::session_run_ids`]). Handing the verb an
///     explicit `run_id` does NOT retire that guard, it relocates the guess: `/nspec` runs on the
///     BUILT-IN workflow (`project_id IS NULL`, shared by the whole installation), `run_list` on it
///     enumerates every session's and every project's runs with no filter, and the tool descriptor
///     points an agent straight at that list. Two worktrees of one project — the ordinary case —
///     are enough for a session to anchor another's run. The registry is the only session ↔ run
///     mapping there is, so it is what says "yours";
///   * the run carries NO Spec — a bound run is never re-bound and never stolen (the authority
///     [`db::set_run_spec`] re-checks that in its own WHERE, and this call does not bypass it);
///   * the run is TOP-LEVEL (no `parent_run_id`): a `sub_workflow` child INHERITS its Spec from its
///     parent at creation, so binding one would contradict its parent rather than repair anything;
///   * and the callee's UNIQUENESS condition — EXACTLY ONE such candidate — is REPLACED, not
///     dropped: it exists because `spec_create` must GUESS which run authored the Spec, and two
///     live candidates make that a coin toss. Here the caller NAMES the run, so the guess it
///     guards against cannot happen; what replaces it is the session rule above, which is what
///     keeps the named run from being someone else's.
///
/// AND THE GUARDS ON THE SPEC, which [`bind_session_run_to_spec`] never needed because the Spec it
/// is handed was just CREATED and can carry nothing, while the Spec here is one the human names:
///   * the Spec must belong to the project the run is anchored in. A `KEY-N` carries its OWN
///     project, so the short grammar is precisely the form that can name a Spec of somewhere else;
///   * the Spec must not ALREADY carry a live run of this workflow. N20 ("at most one live run per
///     (workflow, Spec)") is upheld by `run_start` ALONE, inside
///     [`db::start_or_resume_workflow_run`], which converges the pair by DELETING every run but the
///     most recent. So a bind that looked only at its source run could compose the very state
///     `run_start` refuses to open — and the next `run_start` on that Spec would silently destroy
///     the other run, with everything it had walked. A recovery that recreates the damage it exists
///     to repair is not one.
///
/// AND THE WRITE ITSELF CAN STILL REFUSE: the authority [`db::set_run_spec`] re-checks
/// `spec_id IS NULL` in its own WHERE, so 0 rows means a condition held at read time no longer does
/// — a refusal (`bind_refused_by_the_authority`), never a silent success.
///
/// EVERY refusal above answers under its own [`bind_refusal`] code, and that module — not this list
/// — is the enumeration to read: it is where a code is added, so a summary that re-counted them
/// here would be a second statement free to fall behind (finding `01M0ZW802X47JFC8977EKD9YAN`).
///
/// The Spec reference accepts the four forms like every other `spec_id` field, through the one
/// entry point ([`crate::mcp_context::require_spec_arg`]). The `run_id` is the anchor a `#N` / `N`
/// is counted in — and it is an anchor that can END ON NOTHING: a run's project comes from its
/// WORKSPACE, and `run_start` deliberately opens a run without one when its `cwd` matched nothing
/// (warning [`RUN_LOCATION_UNRESOLVED_WARNING_CODE`]) — which is the very session this verb repairs.
/// So the short forms are advertised CONDITIONALLY here, and the refusal on that path names the
/// forms that carry their own project instead of arguments this verb does not take.
pub fn run_bind_spec(
    db: &Db,
    runtime: &EngineRuntime,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let run_id = require_str(args, "run_id")?.to_string();
    let run = db
        .with_conn(|c| db::get_workflow_run(c, &run_id))
        .map_err(internal_db)?
        .ok_or_else(|| RpcError::new(RpcCode::InvalidId, format!("unknown run '{run_id}'")))?;

    // THE RUN'S OWN PRECONDITIONS FIRST, before the Spec is even looked up: a run this call may not
    // touch is the answer, whatever Spec was named.
    //
    // TERMINALITY IS READ BEFORE OWNERSHIP, and that order is the whole point. The session registry
    // EVICTS a run the moment it finishes (`nyx_next` calls [`EngineRuntime::forget`]), so "absent
    // from the registry" answers two different questions at once, and only one of them is "not
    // yours": a run THIS session drove to its end is absent exactly like a stranger's. Asking the
    // status first is what stops the commonest case of all — bind the run I just finished — from
    // being told the one thing that is false about it. Nothing is relaxed by the order: a LIVE run
    // outside the registry still earns `FOREIGN_RUN`, and a terminal one is refused either way.
    if db::run_status_is_terminal(&run.status) {
        return Err(bind_refusal(
            bind_refusal::TERMINAL_RUN,
            &run.id,
            format!(
                "run '{run_id}' is '{}' — it is over, and anchoring a finished run rewrites history \
                 rather than repairing a session. Open a new run on the Spec",
                run.status
            ),
        ));
    }
    if !runtime.session_run_ids().iter().any(|id| *id == run.id) {
        return Err(bind_refusal(
            bind_refusal::FOREIGN_RUN,
            &run.id,
            format!(
                "run '{run_id}' is not one this session is driving, so this session cannot say what \
                 it is about. run_list enumerates the runs of EVERY session and project on a \
                 built-in workflow, so an id read there may well be someone else's. Bind a run you \
                 opened (its id rides in the NYX RUN block, or comes back from run_start)"
            ),
        ));
    }
    if let Some(bound) = run.spec_id.as_deref() {
        return Err(bind_refusal(
            bind_refusal::ALREADY_BOUND,
            &run.id,
            format!(
                "run '{run_id}' already drives spec {bound}: a run's Spec is never re-written, only \
                 ever posed on a run that carries none. Open a run on the other Spec instead"
            ),
        ));
    }
    if let Some(parent) = run.parent_run_id.as_deref() {
        return Err(bind_refusal(
            bind_refusal::CHILD_RUN,
            &run.id,
            format!(
                "run '{run_id}' is a child of run {parent} and INHERITS its Spec from it at \
                 creation — binding the child would contradict its parent. Bind the parent, if it \
                 is what carries none"
            ),
        ));
    }

    // THE PROJECT THE RUN IS ANCHORED IN — its workspace's, since a run reaching this point carries
    // no Spec to name one. It is what a `#N` counts inside, and what the Spec's own project is
    // confronted with below.
    let run_project = match run.workspace_id.as_deref() {
        Some(ws) => db
            .with_conn(|c| -> diesel::QueryResult<Option<String>> {
                Ok(db::get_workspace(c, ws)?.map(|w| w.project_id))
            })
            .map_err(internal_db)?,
        None => None,
    };
    // THE ANCHORS ARE WALKED ONCE. `require_spec_arg` would re-derive the project from `args` —
    // `require_run` + `get_workspace` + `get_project` on the very `run_id` this call already
    // loaded, for the same answer `run_project` above is holding. So the project is HANDED to the
    // grammar instead, exactly as `mcp_context::resolve_spec_arg_project_relative_refused` does for
    // the same reason (finding `01M0ZMA20NRB2WDSTSHE63YYX5`). Every form is unchanged: a canonical
    // id and prose come straight back, a `KEY-N` still resolves in the project ITS key names, and a
    // `#N` / `N` counts in the run's project — which is what the descriptor promises.
    let spec_id = crate::mcp_context::resolve_spec_ref_in(
        db,
        run_project.as_deref(),
        require_str(args, "spec_id")?,
    )?;
    let spec = db
        .with_conn(|c| db::get_spec(c, &spec_id))
        .map_err(internal_db)?;
    let Some(spec) = spec else {
        // The SAME actionable refusal `run_start` gives a reference that designates nothing — the
        // forms, and what the run's project actually holds.
        //
        // WITH ONE DIFFERENCE, and it is the point: when the run anchors NO project, this verb has
        // no `cwd` / `workspace_id` / `project_id` to offer (its schema carries neither), so it says
        // what IS true of this call — the anchor is the run, and the run carries no workspace.
        let trailer = format!(
            ". run_bind_spec takes no cwd / workspace_id / project_id: its only project anchor is \
             the run, and run '{run_id}' carries no workspace to name one"
        );
        return Err(unknown_spec_refusal(
            db,
            run_project.as_deref(),
            &spec_id,
            Some(&trailer),
        )?);
    };

    // THE CROSS-PROJECT GUARD, on the RESOLVED id exactly as `resolve_run_target` applies it to
    // `run_start`: a `KEY-N` carries its own project, so a key of another one must not become the
    // back door through which a foreign Spec is anchored on this run.
    if let Some(anchor) = run_project.as_deref() {
        if anchor != spec.project_id {
            return Err(bind_refusal(
                bind_refusal::PROJECT_DISAGREEMENT,
                &run.id,
                format!(
                    "spec {} belongs to project {}, but run '{run_id}' is anchored on a workspace \
                     of project {anchor} — they disagree. Name a Spec of project {anchor}",
                    spec.id, spec.project_id
                ),
            ));
        }
    }

    // N20 ON THE TARGET — see the note above: the pair must not end up carrying two live runs.
    let rival = db
        .with_conn(|c| db::find_runs_for_spec(c, &run.workflow_id, Some(&spec.id)))
        .map_err(internal_db)?
        .into_iter()
        .find(|r| !db::run_status_is_terminal(&r.status));
    if let Some(rival) = rival {
        // THE GESTURES ARE ORDERED, AND THE DELETABLE RUN IS NAMED. "Resume, or delete the one you
        // do not want" was true and dangerously readable the wrong way: from inside THIS run, "the
        // one you do not want" is the rival — the parked run that has already walked the flow, whose
        // loss this very refusal exists to prevent, and which the caller has not read. So the
        // recovery is said first, and the run that is SAFE to delete is the one the caller can see:
        // its own (finding `01M0ZWC64WXFCGM5H0P36PNSV6`).
        let rival_id = &rival.id;
        let target = &spec.id;
        return Err(bind_refusal(
            bind_refusal::SPEC_HAS_A_LIVE_RUN,
            &run.id,
            format!(
                "spec {target} already carries the live run {rival_id} on this workflow, and nyx \
                 holds AT MOST ONE live run per (workflow, Spec) — binding '{run_id}' too would \
                 make the next run_start on that Spec DELETE one of them, with everything it had \
                 walked. RESUME {rival_id} FIRST: it is the run that has already walked this flow, \
                 and picking it up loses nothing. If one of the two must then go, the safe one to \
                 run_delete is '{run_id}' — this call's own run, whose work you can see; \
                 {rival_id} is the one whose loss this refusal exists to prevent, so delete it \
                 only after reading it"
            ),
        ));
    }

    // The authority re-checks `spec_id IS NULL` in its own WHERE. It is not bypassed and not
    // trusted blindly either: 0 rows means a condition held at read time no longer does (a
    // concurrent bind), and that is a refusal, never a silent success.
    let written = db
        .with_conn(|c| db::set_run_spec(c, &run.id, &spec.id))
        .map_err(internal_db)?;
    if written == 0 {
        return Err(bind_refusal(
            bind_refusal::AUTHORITY_REFUSED,
            &run.id,
            format!(
                "run '{run_id}' was not bound: it no longer carries no Spec — something bound it \
                 between this call's checks and its write. Read it back (run_list) before deciding \
                 what to do"
            ),
        ));
    }
    Ok(ToolOutcome::changed(
        json!({
            "run_id": run.id,
            "spec_id": spec.id,
            "workflow_id": run.workflow_id,
        }),
        ChangedTopic::Specs,
    ))
}

/// THE ANCHORLESS TRAILER `run_start` APPENDS to [`unknown_spec_refusal`] — the arguments it tells
/// a caller to ADD when a `#N` found no project to be counted in. A refusal's advice is only advice
/// if the verb emitting it advertises what it names, and this sentence spent a release naming a
/// `project_id` absent from `run_start`'s own `inputSchema` (finding `01M1008NHM9RRA1ZAS4V7K1PM4`).
///
/// `pub(crate)` for the same reason as [`RUN_LOCATION_UNRESOLVED_WARNING_CODE`] below:
/// `mcp::tests::run_start_advises_only_anchors_its_own_schema_advertises` confronts THIS constant
/// with the descriptor's `properties`, rather than a re-typed literal — so re-wording the advice
/// without adding the argument turns that test red.
pub(crate) const RUN_START_ANCHOR_TRAILER: &str = ", or add a cwd / workspace_id / project_id";

/// The `run_start` WARNING code for a location that anchors SEVERAL active Specs on a flow that needs
/// none: the run starts UNBOUND rather than refusing (G2). A STABLE machine code, sibling of
/// `gate_unconfigured` and `run_replaced_graph_changed` in the same `warnings` array.
const SPEC_AMBIGUOUS_WARNING_CODE: &str = "spec_ambiguous_started_unbound";

/// The `run_start` WARNING code for a `cwd` that matched NO workspace, leaving the run with no
/// workspace anchor (review F1, sub-case (b) — the "wrong folder" half of the `agent_context`
/// parity). A STABLE machine code, sibling of `run_replaced_graph_changed` / `run_blocked_stranded`
/// in the same `warnings` array; it is deliberately NOT the same code as `agent_context`'s
/// `spec_location_unresolved`, because what is lost here is the run's WORKSPACE, not a Spec scope.
/// `pub(crate)` so `mcp::tests::the_spec_anchor_and_resolution_tools_document_the_contract_they_enforce`
/// pins the DESCRIPTOR against the constant itself rather than against a re-typed literal: renaming
/// the code without re-wording `run_start`'s description then turns that test red.
pub(crate) const RUN_LOCATION_UNRESOLVED_WARNING_CODE: &str = "run_location_unresolved";

/// The `run_start` WARNING code for a run DROPPED because its graph was replaced under it (M4). A
/// STABLE machine code — the dispatcher / UI keys on it, the human sentence beside it is free to
/// change. Sibling of `gate_unconfigured` (N26a) in the same `warnings` array.
const RUN_REPLACED_WARNING_CODE: &str = "run_replaced_graph_changed";

/// The `run_start` WARNING code for a run RESUMED rather than opened. A STABLE machine code, sibling
/// of [`RUN_REPLACED_WARNING_CODE`] in the same `warnings` array — and its counterpart: one says the
/// flow restarts from the top, the other that it does not.
///
/// `pub(crate)` for the same reason as [`RUN_LOCATION_UNRESOLVED_WARNING_CODE`]: `run_start`'s
/// DESCRIPTOR names it to a caller, and `mcp::tests` pins the descriptor against this constant
/// rather than a re-typed literal.
pub(crate) const RUN_RESUMED_WARNING_CODE: &str = "run_resumed";

/// Build the warning for the run `run_start` RESUMED — the third outcome, and the one that used to
/// say nothing at all.
///
/// WHY IT IS WORTH A SENTENCE. The replacement warning exists because a session that had walked half
/// the flow was silently restarted from the top and nobody was told (observed live). Resuming is the
/// same silence with the sign flipped: the human invokes a command believing they start at the
/// beginning, and the very first tick answers from wherever a run — theirs from last week, or a
/// parallel session's — happened to stop. Nothing is wrong; they simply have to KNOW, because it
/// decides whether they carry on or clear the run and start again.
///
/// TWO FACTS, and they are the two the task named: WHERE it lands (the node, by label and id) and
/// HOW LONG it has been sitting there (since `updated_at`, which the resume path leaves untouched —
/// this is read before the workspace anchor's write for exactly that reason). The duration is
/// spelled by [`crate::mcp_context::human_span`], the same function the context render's `Run` line
/// uses, so the two surfaces cannot describe one run's age in two different vocabularies.
///
/// THE WAY OUT IS NAMED FROM THE VERB'S OWN CONSTANT, never re-typed here — the rule
/// `mcp_context::unanchored_signal` states for its own remedy, and the one this repo already paid
/// for on a refusal naming an argument its schema did not carry (`01M1008NHM9RRA1ZAS4V7K1PM4`).
/// Advice naming a tool that does not exist is worse than no advice; renaming `run_delete` re-words
/// this sentence with it.
///
/// A WARNING, never an error: N20's convergence is the correct behaviour and this does not touch it.
fn run_resumed_warning(db: &Db, run: &db::WorkflowRun) -> Result<Value, RpcError> {
    let idle_ms = (db::now_millis() - run.updated_at).max(0);
    let landing = match run.current_node_id.as_deref() {
        Some(node_id) => db
            .with_conn(|c| db::get_node(c, node_id))
            .map_err(internal_db)?
            .map_or_else(
                // The node is gone from under the cursor: say the id, which is still true.
                || format!("parked on node {node_id}"),
                |n| format!("parked on \"{}\" (node_id={})", n.label, n.id),
            ),
        None => "still at the flow's START — it was opened but never ticked".to_string(),
    };
    Ok(json!({
        "code": RUN_RESUMED_WARNING_CODE,
        "run_id": run.id,
        "node_id": run.current_node_id,
        "status": run.status,
        "idle_ms": idle_ms,
        "message": format!(
            "run {} was RESUMED, not opened: it already existed for this (workflow, Spec) and is {landing}, \
             '{}', with nothing having touched it for {}. You are picking a session up MID-FLOW — the \
             next nyx_next answers from where it stopped, not from the top. Say so before you tick; if \
             that run is not the work you meant to continue, clear it with {} run_id='{}' rather \
             than driving it",
            run.id,
            run.status,
            crate::mcp_context::human_span(idle_ms),
            crate::mcp_authoring::RUN_DELETE_TOOL,
            run.id
        ),
    }))
}

/// The corpse [`start_or_replace_run`] DROPPED, and [`build_engine`]'s own refusal naming how it
/// died — everything a caller needs to SAY what it just did.
struct ReplacedRun {
    dropped: db::WorkflowRun,
    why: RpcError,
}

/// WHICH of the three things [`start_or_replace_run`] did — the answer `run_start` reports, and the
/// reason this is an enum rather than the `Option<ReplacedRun>` it used to hand back.
///
/// TWO OF THE THREE ALREADY SPOKE and the third did not, which made the SILENT one ambiguous:
/// "created" and "resumed" were both "no replacement", so a caller could not tell a run it had just
/// opened from one it had walked into halfway through. That asymmetry is the same defect the
/// replacement warning was written for, in the other direction — there, a session that had walked
/// half the flow silently restarted from the top; here, a human who believes they are starting from
/// the top lands in the middle.
enum RunStartOutcome {
    /// No live run of the pair: a fresh row, parked at the flow's start.
    Created,
    /// The pair's live run was handed back AS IS — same id, no new row, and parked wherever the last
    /// session left it.
    Resumed,
    /// The pair's live run could never tick again, so it was dropped and replaced by a fresh one.
    Replaced(ReplacedRun),
}

/// The THREE stable machine tags [`RunStartOutcome::tag`] publishes under `outcome`. A caller keys
/// on these strings, and `run_start`'s DESCRIPTOR spells all three out to say what `outcome` can
/// hold — so they are CONSTANTS, not literals typed once here and once in `tools/list`.
///
/// `pub(crate)` for that second surface. A tag renamed on the enum alone would leave the descriptor
/// announcing a value no run ever returns, which is the same defect as advice naming a verb that
/// does not exist (finding `01M1008NHM9RRA1ZAS4V7K1PM4`) — with nothing to catch it, since the
/// engine's own tests would simply follow the rename.
pub(crate) const RUN_START_OUTCOME_CREATED: &str = "created";
pub(crate) const RUN_START_OUTCOME_RESUMED: &str = "resumed";
pub(crate) const RUN_START_OUTCOME_REPLACED: &str = "replaced";

/// The three, as ONE list — what `run_start`'s description must name, walked by
/// `mcp::tests::run_start_advertises_every_listed_outcome_tag`.
///
/// WHAT THAT WALK HOLDS, EXACTLY: every tag ON THIS LIST is spelled out in `tools/list`. It is NOT
/// "every tag [`RunStartOutcome`] can publish", and this list cannot be made to hold that. Nothing
/// ties it to the enum: [`RunStartOutcome::tag`] is exhaustive, so the COMPILER does force a fourth
/// variant to be given a tag — but adding `Self::Cancelled => RUN_START_OUTCOME_CANCELLED` while
/// leaving this array alone compiles, passes, and ships the new tag undocumented.
///
/// WHAT CLOSES IT AS FAR AS IT CAN BE CLOSED IS A VISIT, NOT A DERIVATION. Rust has no way to
/// enumerate a variant set (`Replaced` carries a payload, so there is no value list to walk, and
/// `variant_count` is unstable), so DERIVING this list would take a declarative macro generating the
/// enum and the list from one declaration — machinery this crate has nowhere else, for a lock on
/// PROSE. A second exhaustive match costs nothing, though, and buys the next best thing:
/// `tests::every_run_start_outcome_carries_a_listed_tag` matches on [`RunStartOutcome`] and asserts
/// membership in THIS array, so a fourth variant stops that file compiling and its author reads the
/// rule before they can go green. The rule, in words, because a visit cannot state it: A FOURTH
/// OUTCOME IS ADDED TO THIS LIST TOO, and then the walk makes the descriptor state it.
pub(crate) const RUN_START_OUTCOME_TAGS: [&str; 3] = [
    RUN_START_OUTCOME_CREATED,
    RUN_START_OUTCOME_RESUMED,
    RUN_START_OUTCOME_REPLACED,
];

impl RunStartOutcome {
    /// The STABLE machine tag `run_start` publishes under `outcome` — read off the constants above,
    /// so the value a caller receives and the value `tools/list` advertises are ONE fact.
    fn tag(&self) -> &'static str {
        match self {
            Self::Created => RUN_START_OUTCOME_CREATED,
            Self::Resumed => RUN_START_OUTCOME_RESUMED,
            Self::Replaced(_) => RUN_START_OUTCOME_REPLACED,
        }
    }
}

/// Resolve the ONE live run of a `(workflow, spec)` pair — N20's idempotency — and VERIFY it can
/// still be advanced before handing it back. The ONE way a run is resolved-then-trusted, used by
/// `run_start` AND by the `sub_workflow` node that opens a child.
///
/// N20 alone: a RESUMABLE run (running / blocked — blocked re-runs its gate on the next tick, M5) is
/// RETURNED as-is (same run_id, no new row) and any stale duplicates / terminal runs of the pair are
/// deleted; only when no resumable run remains is a FRESH one created — all in one transaction.
///
/// …but N20 judges resumability on STATUS ALONE, and a status is not enough. When the workflow's graph
/// was edited under a live run, its persisted `traversal` points at nodes the tree no longer holds and
/// [`build_engine`] REFUSES to resume it (`run_traversal_stale`) so no effect is ever replayed. The row
/// still reads `running`/`blocked`, so the status finder hands that CORPSE back forever.
///
/// So the candidate is VERIFIED: its engine is rebuilt through the very path `nyx_next`/`drive` take.
/// A rebuild that fails means the run can never tick again ⇒ it is DEAD, so it is deleted and the SAME
/// create path the "no resumable run remains" branch takes is taken — the caller transparently gets a
/// working run, plus the corpse it dropped so it can say so.
///
/// M1 — WHY BOTH SITES, one function. The verification used to live inline in `run_start`, and the
/// `sub_workflow` node resolved its child through the RAW N20 finder. Observed live at the built-in
/// 18 → 19 bump: the parked parent was correctly replaced, its FRESH replacement reached its
/// prototyper node, and the pair `(child workflow, spec)` handed it the dead child from BEFORE the
/// bump — re-attached to the new parent, and unable to tick. The parent re-emitted its `sub_workflow`
/// action at every tick forever, and since the parent HAD been replaced correctly, nothing pointed at
/// the child as the cause; the only way out was `run_delete` on the child by hand. The idempotency was
/// never the defect — trusting what it resolved was.
///
/// This lives HERE and not in `db::start_or_resume_workflow_run` because the db layer cannot call the
/// engine (the tree/traversal restore is a nyx-core engine concern), and it does NOT weaken
/// build_engine's refusal: a stale traversal still refuses to REPLAY.
///
/// A genuinely resumable run rebuilds fine and is returned AS-IS (same id, no [`ReplacedRun`]) —
/// `blocked` included, since it re-runs its gate on the next tick (M5). The N20 contract (AT MOST ONE
/// live run per (workflow, spec)) is untouched: one row is replaced by one row.
fn start_or_replace_run(
    db: &Db,
    runtime: &EngineRuntime,
    workflow_id: &str,
    spec_id: Option<&str>,
    blackboard: &str,
) -> Result<(db::WorkflowRun, RunStartOutcome), RpcError> {
    // `created` marks WHICH path the resolution took — only a RESUMED run has a corpse to check.
    let (run, created) = db
        .with_conn(|c| db::start_or_resume_workflow_run(c, workflow_id, spec_id, None, blackboard))
        .map_err(internal_db)?;
    if created {
        return Ok((run, RunStartOutcome::Created));
    }
    let Err(why) = build_engine(db, &run) else {
        // The pair's live run rebuilt fine and is handed back untouched — including its
        // `updated_at`, which is what lets the caller say how long it had been sitting.
        return Ok((run, RunStartOutcome::Resumed));
    };
    // DROPPING a run is the SECOND way to delete one, and it takes the same tick guard as the first
    // ([`crate::mcp_authoring::run_delete`]) — L1 is about the row, not about which verb removes it.
    // A tick already inside this run reached it from its RESIDENT slot (so it never re-ran
    // `build_engine` and never saw what makes us judge the run dead), may have RUN ITS GATE and
    // imposed its transition, and would `persist` onto a row deleted underneath it: `run_vanished`,
    // with the walk's work unrecorded. `persist`'s own doc argues that cannot happen because
    // "run_delete holds the same tick guard" — this is the site that used to make that false.
    //
    // REFUSED rather than queued, exactly like the other two guard sites: the tick we would wait on
    // is itself bounded, and the caller's retry is what makes its answer current.
    let Some(_tick) = runtime.enter_tick(&run.id) else {
        return Err(refuse_busy_replacement(&run.id));
    };
    db.with_conn(|c| db::delete_workflow_run(c, &run.id))
        .map_err(internal_db)?;
    // The row is gone; drop its resident engine with it (`run_delete` does the same), or the
    // process-global registry keeps the dead run's whole tree + blackboard — and still counts it
    // among the runs this session is driving — for the rest of the session.
    runtime.forget(&run.id);
    let fresh = db
        .with_conn(|c| db::start_or_resume_workflow_run(c, workflow_id, spec_id, None, blackboard))
        .map_err(internal_db)?
        .0;
    Ok((
        fresh,
        RunStartOutcome::Replaced(ReplacedRun { dropped: run, why }),
    ))
}

/// The machine `reason` [`build_engine`] refused with — the STABLE code naming WHICH way a run died.
fn run_death_reason(why: &RpcError) -> &str {
    why.data
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("run_not_resumable")
}

/// The [`db::RUN_GRAPH_REPLACED_KEY`] marker a dead run carries when whoever replaced its graph posed
/// one — today the built-in upgrade at boot.
fn run_graph_replaced_marker(dead: &db::WorkflowRun) -> Option<Value> {
    serde_json::from_str::<Value>(&dead.blackboard)
        .ok()
        .and_then(|b| b.get(db::RUN_GRAPH_REPLACED_KEY).cloned())
}

/// WHY `dead` can never tick again, as ONE clause: the RECORDED cause when there is one — "nyx
/// upgraded the graph itself at boot" is a very different thing to tell a human than "the graph
/// changed" — and the shape of the wreckage from [`build_engine`]'s own refusal.
///
/// Shared by every site that drops such a run, so `run_start`'s warning and the `sub_workflow` node's
/// notes say the SAME thing about the same death.
fn run_death_clause(dead: &db::WorkflowRun, why: &RpcError) -> String {
    let detail = match run_death_reason(why) {
        "run_traversal_stale" => "the walk it had recorded points at nodes the graph no longer holds",
        "run_not_resumable" => "it had progressed with no recorded walk left to restore",
        _ => "its workflow's stored tree no longer builds",
    };
    let revision = |v: Option<&Value>| match v {
        Some(Value::Number(n)) => n.to_string(),
        // A pre-N11 built-in carries no stamp at all (NULL), which the seeder reads as 0.
        _ => "unstamped".to_string(),
    };
    let cause = match run_graph_replaced_marker(dead) {
        Some(m) if m.get("cause").and_then(Value::as_str) == Some("builtin_upgrade") => format!(
            "nyx UPGRADED this built-in workflow's graph at boot (revision {} → {})",
            revision(m.get("from_revision")),
            revision(m.get("to_revision")),
        ),
        _ => "its workflow's graph changed under it".to_string(),
    };
    format!("{cause} ({detail})")
}

/// Build the M4 warning for the run `run_start` just DROPPED as unresumable. `why` is
/// [`build_engine`]'s own refusal, whose `reason` marker names WHICH way the run died — so the
/// caller gets the machine code, the id of the run that was destroyed, and one sentence a human can
/// act on. Deliberately a WARNING and not an error: replacing the corpse is the correct recovery
/// (D3), it just must not be silent.
///
/// When the run carries the [`db::RUN_GRAPH_REPLACED_KEY`] marker — posed by whoever replaced the
/// graph, today the built-in upgrade at boot — it is echoed VERBATIM under `graph_replaced` and the
/// sentence names that cause instead of describing the wreckage.
fn run_replaced_warning(dropped: &db::WorkflowRun, why: &RpcError) -> Value {
    let mut warning = json!({
        "code": RUN_REPLACED_WARNING_CODE,
        // The DROPPED run, spelled out — the result's own `run_id` is the FRESH one, so naming this
        // field `run_id` would make the two indistinguishable at a glance.
        "dropped_run_id": dropped.id,
        "reason": run_death_reason(why),
        "message": format!(
            "run {} was DROPPED, not resumed: {}, so its progress could not be picked up and nothing \
             of it was recoverable. The run_id returned here is a FRESH run starting the flow from \
             the top — say so before you drive it",
            dropped.id,
            run_death_clause(dropped, why),
        ),
    });
    let marker = run_graph_replaced_marker(dropped);
    if let (Some(obj), Some(marker)) = (warning.as_object_mut(), marker) {
        obj.insert(db::RUN_GRAPH_REPLACED_KEY.to_string(), marker);
    }
    warning
}

/// Collect the `run_start` WARNINGS about THE GRAPHS this run will walk — the flow it opens AND every
/// flow that flow opens. Two findings, one walk, because they are found on the same edges:
///
///   * a `gate_check` that resolves to NO command (an empty `command_ids`, or a `slot` this project
///     never bound) — it verifies nothing, and a BLOCKING one will REFUSE the run at that node (H10).
///     Best-effort on the node config: a malformed gate config counts as unarmed → warned. RESTRICTED
///     to the gates THIS run can actually reach: see [`gate_is_owed_by_this_run`].
///   * a FORK left behind by its source ([`fork_stale_warning`]) — the project is running a copy taken
///     at revision N of a built-in that has since moved on, so nyx's graph fixes never reached it.
///
/// THE SCAN FOLLOWS `sub_workflow` NODES, and that is the whole point of it. Scanning only the graph
/// `run_start` opens looked exhaustive and was blind exactly where it mattered: the seeded gate lives
/// in the `coder` graph, which `run_start` NEVER opens — a `sub_workflow` node does, several ticks in.
/// Verified live, `run_start(scope:"ndev")` answered `"warnings": []` on a nyx whose only quality gate
/// verified nothing. The alarm has to walk the same edges the run will — which is equally true of the
/// stale fork, since `coder` is precisely the flow a project used to have to fork.
///
/// The child is resolved the way the ENGINE resolves it at run time ([`resolve_child_workflow`]: an
/// explicit `workflow_id`, else the scope's active workflow in the run's project) so the warning names
/// the graph the run will actually open, not a plausible one. A child that resolves to nothing is
/// SKIPPED rather than reported: the run will meet that refusal itself, in its own words, at the node.
///
/// Each warning names the WORKFLOW as well as the node, because "gate_check 'checks'" alone is
/// unactionable once the scan spans several graphs — the fix needs the node, and the human needs to
/// know which flow to look in.
fn flow_graph_warnings(
    db: &Db,
    run: &db::WorkflowRun,
    workflow: &db::Workflow,
) -> Result<Vec<Value>, RpcError> {
    let mut warnings = Vec::new();
    let project_id = run_project_id(db, run)?;
    // The run's GENRE OF WORK, resolved through the very fact the graph's own branch will read
    // ([`QUERY_SPEC_CATEGORY`]) rather than through a second reading of the same table: the scan's
    // answer to "will this run reach that gate" must be the engine's answer, or the warning and the
    // walk would be about two different Specs. `null` for an unclassified Spec and for an unbound
    // run alike — the fact spells both absences one way.
    let category = resolve_query(db, run, QUERY_SPEC_CATEGORY)?
        .as_str()
        .map(str::to_string);
    collect_flow_warnings(
        db,
        run,
        workflow,
        project_id.as_deref(),
        category.as_deref(),
        &mut HashSet::new(),
        &mut warnings,
    )?;
    Ok(warnings)
}

/// Whether an unarmed `gate_check` is a finding FOR THIS RUN — i.e. whether the branch it sits on is
/// one this run's Spec can reach at all.
///
/// A `run_start` warning is a promise about what this call will hit, and the seeded coder graph now
/// holds a gate that MOST runs never reach: the reproduction gate ([`crate::seed::REPRO_CHECK_SLOT`])
/// lives under `selector "bug discipline?"`, on the branch a `bug` Spec takes and no other genre
/// does. Reporting it unconditionally would tell every `/ndev` on every `feature` that its run "WILL
/// BE REFUSED" at a node it is not going to walk — a false alarm on the overwhelming majority of
/// runs, and the surest way to teach a dispatcher to skim past the array that also carries the
/// `build_check` warning, which is true every time.
///
/// The restriction is on the WARNING and on nothing else. The runtime defence is untouched: should a
/// run reach that gate unbound — a `bug` classified after the scan, a fork that moved the node, a
/// project that bound the slot to an empty list — the tick still refuses it as the vacuous gate it
/// is ([`refuse_vacuous_gate`]). The scan answers "what will you hit"; the gate answers "was this
/// verified", and only the second one is a guarantee.
///
/// Every OTHER gate is owed by every run: `build_check` sits on the path any task takes, and a
/// project's own graph is its own business.
fn gate_is_owed_by_this_run(node: &db::Node, category: Option<&str>) -> bool {
    match node_gate_slot(node).as_deref() {
        Some(crate::seed::REPRO_CHECK_SLOT) => {
            category == Some(crate::db::CATEGORY_SLUG_BUG)
        }
        _ => true,
    }
}

/// One graph's worth of [`flow_graph_warnings`], recursing into the children its `sub_workflow` nodes
/// open. `visited` holds the workflow ids already scanned: a graph can reach ITSELF (directly, or
/// through a chain — nothing forbids a `sub_workflow` whose scope resolves back to an ancestor), and
/// without the guard the walk would recurse until the stack blew on a graph that merely LOOKS odd. It
/// also collapses the diamond case (two nodes opening the same child) to ONE report per gate.
fn collect_flow_warnings(
    db: &Db,
    run: &db::WorkflowRun,
    workflow: &db::Workflow,
    project_id: Option<&str>,
    category: Option<&str>,
    visited: &mut HashSet<String>,
    warnings: &mut Vec<Value>,
) -> Result<(), RpcError> {
    if !visited.insert(workflow.id.clone()) {
        return Ok(());
    }
    // The GRAPH's own provenance, before its nodes: whether this flow is a fork that stopped tracking
    // the built-in it came from is a fact about the whole tree, not about any node in it.
    if let Some(stale) = fork_stale_warning(db, workflow)? {
        warnings.push(stale);
    }
    let nodes = db
        .with_conn(|c| db::list_nodes(c, &workflow.id))
        .map_err(internal_db)?;
    for n in &nodes {
        match n.type_.as_str() {
            db::NODE_TYPE_GATE_CHECK => {
                // A config nyx cannot even read arms no command and defaults to BLOCKING, so it is
                // reported as the refusal it will produce. The commands go through the SAME
                // resolution the tick uses ([`gate_commands`]) — an unbound slot is unarmed here for
                // exactly the reason it is unarmed there.
                let blocking = serde_json::from_str::<crate::engine::GateConfig>(&n.config)
                    .map(|c| c.blocking)
                    .unwrap_or(true);
                if gate_is_owed_by_this_run(n, category)
                    && node_gate_commands(db, project_id, n)?.is_empty()
                {
                    warnings.push(gate_unconfigured_warning(workflow, n, blocking, project_id));
                }
            }
            db::NODE_TYPE_SUB_WORKFLOW => {
                let cfg: crate::engine::SubWorkflowConfig =
                    serde_json::from_str(&n.config).unwrap_or_default();
                if let Ok(child) =
                    resolve_child_workflow(db, run, cfg.workflow_id.as_deref(), &cfg.scope)
                {
                    collect_flow_warnings(
                        db, run, &child, project_id, category, visited, warnings,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The warning for ONE unarmed `gate_check`. A BLOCKING one announces the REFUSAL the run will hit at
/// that node (H10) — the same finding the tick states, said early enough to fix before anything is
/// spawned; a NON-BLOCKING one stays what it always was, an advisory about a gate that misleads
/// whoever reads the graph without ever certifying anything.
fn gate_unconfigured_warning(
    workflow: &db::Workflow,
    node: &db::Node,
    blocking: bool,
    project_id: Option<&str>,
) -> Value {
    let hint = arm_gate_hint(workflow, node, project_id);
    let cause = unarmed_gate_cause(node);
    let message = if blocking {
        format!(
            "gate_check '{}' (workflow '{}') is BLOCKING and {cause} — it would pass without \
             verifying anything, so this run WILL BE REFUSED when it reaches that node. To fix it: \
             {hint}",
            node.label, workflow.name
        )
    } else {
        format!(
            "gate_check '{}' (workflow '{}') {cause} — it runs vacuously GREEN and verifies \
             nothing; being non-blocking it refuses nothing, but it gates nothing either. To make it \
             gate: {hint}",
            node.label, workflow.name
        )
    };
    let mut warning = json!({
        "code": GATE_UNCONFIGURED_CODE,
        "node_id": node.id,
        // The WORKFLOW too: the scan spans several graphs, so a node id with no graph beside it
        // cannot be found by the human it is addressed to.
        "workflow_id": workflow.id,
        "workflow_name": workflow.name,
        "blocking": blocking,
        "message": message,
    });
    // The SLOT, as a field and not only inside the sentence: it is the argument the fix takes, and a
    // dispatcher that means to bind it should not have to parse prose to find the name.
    if let (Some(obj), Some(slot)) = (warning.as_object_mut(), node_gate_slot(node)) {
        obj.insert("slot".to_string(), Value::from(slot));
    }
    warning
}

/// The `run_start` WARNING code for a FORK whose SOURCE has moved on — the project is running a graph
/// frozen at an older revision. A STABLE machine code, sibling of `gate_unconfigured` and
/// `run_replaced_graph_changed` in the same `warnings` array.
const FORK_STALE_WARNING_CODE: &str = "workflow_fork_stale";

/// The warning for a workflow that is a FORK of a REVISIONED source which has since advanced — `None`
/// when there is nothing to say (not a fork; a fork of a fork / of a `custom` template, whose source
/// carries no revision to be behind; a fork still level with its source).
///
/// # Why a fork must be able to say it is behind
///
/// The boot seeder upgrades only the rows IT seeded (`builtin_revision < BUILTIN_REVISION`), and a
/// fork is not one — by design: a project's edits must survive nyx's. But `resolve_active_workflow`
/// PREFERS the project's workflow, so from the moment a project forks, every later fix to that graph
/// stops reaching it, silently, forever. Observed: a `coder` fork sitting at revision 11 while the
/// built-in shipped 16, missing three corrections written since, with nothing anywhere saying so.
///
/// The command SLOT removes the reason most forks existed (arming a shared gate no longer needs one),
/// but forks remain legitimate for real customisation — so what they need is not to be prevented, it
/// is to be VISIBLE. Hence a warning and not a refusal: the fork may well be deliberate, and only its
/// owner can decide whether the drift matters.
///
/// A fork with NO recorded source revision (taken before migration v47) is reported too, in its own
/// terms: "an unrecorded revision" is honest, and inventing a number for it would not be. It is the
/// same finding — this graph is a copy nobody has compared to the original since.
fn fork_stale_warning(db: &Db, workflow: &db::Workflow) -> Result<Option<Value>, RpcError> {
    let Some(source_id) = workflow.forked_from.as_deref() else {
        return Ok(None);
    };
    let Some(source) = db
        .with_conn(|c| db::get_workflow(c, source_id))
        .map_err(internal_db)?
    else {
        // The source was deleted (`forked_from` SET NULL never fired because the row is gone in the
        // same breath) — there is nothing to compare against, so there is nothing to say.
        return Ok(None);
    };
    // Only a REVISIONED source can be ahead of anything: a fork of a fork, or of a custom template,
    // has no upstream that upgrades itself.
    let Some(current) = source.builtin_revision else {
        return Ok(None);
    };
    let forked_at = workflow.forked_from_revision;
    if forked_at.is_some_and(|at| at >= current) {
        return Ok(None);
    }
    let frozen_at = match forked_at {
        Some(at) => format!("revision {at}"),
        None => "an unrecorded revision (it predates the stamp)".to_string(),
    };
    Ok(Some(json!({
        "code": FORK_STALE_WARNING_CODE,
        "workflow_id": workflow.id,
        "workflow_name": workflow.name,
        "scope": workflow.scope,
        "source_workflow_id": source.id,
        "forked_from_revision": forked_at,
        "builtin_revision": current,
        "message": format!(
            "workflow '{}' is YOUR FORK of the built-in '{}', frozen at {frozen_at}, and that \
             built-in is now at revision {current}: every graph fix nyx has shipped since is missing \
             from this project, and nothing will ever backfill it — the boot upgrade only touches the \
             built-in rows. If you forked it only to arm its quality gate, you no longer need to: bind \
             the gate's command slot for this project (slot_bind) and workflow_delete workflow_id='{}' \
             so the built-in resolves again and keeps upgrading itself. If the fork carries real \
             customisation, re-fork the current built-in and re-apply it",
            workflow.name, source.name, workflow.id,
        ),
    })))
}

/// The `run_start` WARNING code for a SPEC-SCOPED question asked by a graph whose run is bound to no
/// Spec. A STABLE machine code, sibling of `gate_unconfigured` and `run_replaced_graph_changed` in
/// the same `warnings` array.
const QUERY_WITHOUT_SPEC_WARNING_CODE: &str = "query_without_spec";

/// Collect one warning per `query` node of the tree — called ONLY for a run bound to no Spec, and
/// never for `nspec` (which questions a Spec it has not created yet ON PURPOSE).
///
/// Every fact [`resolve_query`] answers is read THROUGH the run's Spec, and an unbound run gets the
/// empty answer rather than an error: `spec_status` → `null`, the counts → `0`, `design_locked` →
/// `false`. That is right for the FACT (there is no Spec, so nothing is open) and wrong for the
/// DECISION taken on it: `0 open tasks` reads exactly like a finished plan, so a selector routes to
/// its "everything is done" branch and the run walks a flow that was never meant for it — silently,
/// because nothing distinguishes "no Spec" from "nothing left". Naming the node and the fact at
/// `run_start` is what makes that visible BEFORE the first tick.
///
/// Best-effort on the node config, like [`unconfigured_gate_warnings`]: a malformed config yields no
/// fact name and the node is still named, since a query node that cannot even be read is not a
/// reassuring sight on an unbound run either.
fn query_without_spec_warnings(db: &Db, workflow_id: &str) -> Result<Vec<Value>, RpcError> {
    let nodes = db
        .with_conn(|c| db::list_nodes(c, workflow_id))
        .map_err(internal_db)?;
    let mut warnings = Vec::new();
    for n in &nodes {
        if n.type_ != db::NODE_TYPE_QUERY {
            continue;
        }
        let fact = serde_json::from_str::<Value>(&n.config)
            .ok()
            .and_then(|c| c.get("query").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| "?".to_string());
        warnings.push(json!({
            "code": QUERY_WITHOUT_SPEC_WARNING_CODE,
            "node_id": n.id,
            "query": fact,
            "message": format!(
                "query '{fact}' ('{}') is a question about this run's Spec, and the run is bound to \
                 none — it answers the EMPTY value (null / 0 / false), which a branch reads as \
                 'nothing left to do'. Start the run on a Spec (spec_id), or take a graph that asks \
                 nothing of one",
                n.label
            ),
        }));
    }
    Ok(warnings)
}

/// WHAT a run is ABOUT, as one clause a human reads: the Spec it drives — human key, title AND
/// `spec_id` — or the fact that it drives none.
///
/// THE `spec_id` IS IN THE SENTENCE, and that is a change of mind this PRD made explicit. The
/// warnings below used to name the Spec by its human key ALONE, on the argument that `#3` is what
/// the human reads and the id is what they would then have to resolve. Two things undo it. These
/// warnings are MACHINE-WIDE — [`stranded_run_warnings`] enumerates the blocked runs of every
/// project on the installation — so a `#N`, which is only ever counted INSIDE a project, does not
/// designate a Spec at all in a sentence that names no project. And the id is now the thing the
/// reader ACTS with: the remedy each of these warnings prescribes is a call taking a `spec_id`.
/// The `#N` and the title stay; nothing is removed, the id is added.
///
/// ONE function for both warnings, because they said the same thing in two copies.
fn run_subject(db: &Db, run: &db::WorkflowRun) -> Result<String, RpcError> {
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Ok("no Spec (an authoring run)".to_string());
    };
    Ok(db
        .with_conn(|c| db::get_spec(c, spec_id))
        .map_err(internal_db)?
        .map_or_else(
            // The row is gone under the run: the id is then the only true thing left to say.
            || format!("spec '{spec_id}'"),
            |s| format!("Spec #{} '{}' (spec_id {})", s.number, s.title, s.id),
        ))
}

/// The `run_start` WARNING code for a `blocked` run nobody has come back to. A STABLE machine code,
/// sibling of `gate_unconfigured` / `run_replaced_graph_changed` in the same `warnings` array.
const RUN_STRANDED_WARNING_CODE: &str = "run_blocked_stranded";

/// How long a `blocked` run must sit UNTOUCHED before `run_start` calls its Spec STRANDED.
///
/// `blocked` is a NOMINAL state, not a failure: a run parks there on a red gate and re-runs that gate
/// on its next tick (M5), so a run blocked a minute ago is a run someone is fixing something for. The
/// only thing that tells it apart from one nobody will ever drive again is how long it has sat, so
/// this threshold is the whole distinction — and it has to clear the longest legitimate pause: a run
/// left on a Friday evening and picked up on Monday morning has been still for ~63 h and is perfectly
/// alive. 3 days sits just past that weekend; the corpses that motivated this had been still for a
/// week (three `nspec` runs and one `coder` run, blocked since 5–6 August, surfaced by nothing).
/// `pub(crate)` because the CONTEXT RENDER marks a run STALE off THIS constant
/// ([`crate::mcp_context::run_state_line`]) and "how long is too long" must be ONE number: a second
/// copy of that answer would let a render call a run fresh while this warning reports it stranded,
/// the two-truths failure this repo has already paid for on the anchor order.
///
/// THE THRESHOLD IS SHARED; THE POPULATION IS NOT — and reading the first as the second is how a
/// doc-comment starts promising an equivalence the code never had. This warning strands `blocked`
/// runs ONLY ([`crate::db::blocked_runs_untouched_since`]): `blocked` is the state a gate parks a run
/// in, and how long it has been parked is the whole question. The render marks EVERY live run,
/// `running` included, because a header answering "is someone already on this?" must also say that a
/// `running` run nothing has touched for a week is not a session in progress. So a stale `running`
/// run shows up in the render and in NO `run_start` warning, by design; what the shared constant
/// rules out is the two surfaces disagreeing about a run they BOTH look at. The POLICY stays here,
/// with the warning that motivated it.
pub(crate) const STRANDED_BLOCKED_RUN_AFTER_MS: i64 = 3 * 24 * 60 * 60 * 1000;

/// One `run_start` warning per run that has been `blocked` longer than
/// [`STRANDED_BLOCKED_RUN_AFTER_MS`] — machine-wide, because what the human needs is the list of
/// their OWN abandoned work, and they are not going to go looking for it in the flow they happen to
/// be starting.
///
/// It names WHICH Spec is stranded and the verb that clears it (`run_delete`), and it DELETES
/// NOTHING: M4 drops a run it has PROVEN unresumable, which is a fact about the run; "blocked for a
/// week" is a fact about the human's attention, and destroying work on that basis is not nyx's call.
///
/// The run this very call returns is reported too, and first-class: when `run_start` converged onto a
/// corpse, the caller is holding it, and "you are about to drive a run that died a week ago" is the
/// one thing they cannot deduce from a `run_id`.
fn stranded_run_warnings(db: &Db, opened: &db::WorkflowRun) -> Result<Vec<Value>, RpcError> {
    let now = db::now_millis();
    let stranded = db
        .with_conn(|c| db::blocked_runs_untouched_since(c, now - STRANDED_BLOCKED_RUN_AFTER_MS))
        .map_err(internal_db)?;
    let mut warnings = Vec::new();
    for run in stranded {
        let days = (now - run.updated_at) / (24 * 60 * 60 * 1000);
        let flow = db
            .with_conn(|c| db::get_workflow(c, &run.workflow_id))
            .map_err(internal_db)?
            .map_or_else(|| run.workflow_id.clone(), |w| w.name);
        let subject = run_subject(db, &run)?;
        let resumed_here = run.id == opened.id;
        let fate = if resumed_here {
            "and it is the run THIS CALL just handed back: what you are about to drive is that run, \
             not a fresh one"
        } else if run.spec_id.is_some() {
            "and the next run opened on that Spec will RESUME it instead of starting fresh"
        } else {
            "and nothing will ever pick it up on its own"
        };
        warnings.push(json!({
            "code": RUN_STRANDED_WARNING_CODE,
            "run_id": run.id,
            "spec_id": run.spec_id,
            "blocked_days": days,
            // The caller must be able to tell "your other work is stuck" from "the run you are
            // holding is stuck" without parsing the sentence.
            "resumed_here": resumed_here,
            "message": format!(
                "run {} ('{flow}') on {subject} has been BLOCKED for {days} day(s) {fate}. Drive it \
                 (nyx_next) and it re-runs the check it stopped on; if it is dead, clear it with \
                 run_delete run_id='{}' — nyx deletes no run you have not asked it to",
                run.id, run.id
            ),
        }));
    }
    Ok(warnings)
}

/// The `run_start` WARNING code for a run parked on a consent question posed to NO session (K1). A
/// STABLE machine code, sibling of `run_blocked_stranded` / `gate_unconfigured` in the same
/// `warnings` array.
const CONSENT_UNANSWERABLE_WARNING_CODE: &str = "run_consent_unanswerable";

/// The warning for a run whose consent receipt names NO terminal — a question put to nobody, which no
/// human turn can satisfy ([`db::PendingConsent::terminal_id`]).
///
/// It exists because such a run is INVISIBLE otherwise: `run_start` converges onto it and hands it
/// back with an empty `warnings`, so every later `/nspec` on that Spec lands on a question it cannot
/// close and the Spec reads as merely slow rather than stuck. Observed live on a run parked on
/// "Mark the Spec ready for build?" whose receipt carried `terminal_id: null` — three authentic `yes`
/// were refused, an app restart changed nothing, and nothing anywhere said why.
///
/// It names the run, the Spec and the ONE gesture that clears it: nyx no longer opens such a receipt
/// ([`refuse_unattributable_consent`]) and re-arms a stored one onto the session that picks the run
/// up ([`db::open_run_consent`]), so a tick from a real terminal repairs it — but only a caller told
/// so knows that a run is waiting for exactly that.
fn unanswerable_consent_warning(db: &Db, run: &db::WorkflowRun) -> Result<Vec<Value>, RpcError> {
    let Some(pending) = db::PendingConsent::of(run).filter(|p| p.terminal_id.is_none()) else {
        return Ok(Vec::new());
    };
    let flow = db
        .with_conn(|c| db::get_workflow(c, &run.workflow_id))
        .map_err(internal_db)?
        .map_or_else(|| run.workflow_id.clone(), |w| w.name);
    let subject = run_subject(db, run)?;
    Ok(vec![json!({
        "code": CONSENT_UNANSWERABLE_WARNING_CODE,
        "run_id": run.id,
        "spec_id": run.spec_id,
        "prompt": pending.prompt,
        "message": format!(
            "run {} ('{flow}') on {subject} is parked on the question \"{}\", and that question was \
             put to NO session — nyx checks such an answer against the human-turn ledger of the \
             terminal it asked, and the receipt names none, so nothing can satisfy it as it stands. \
             Tick this run from a nyx terminal (nyx_next(run_id=\"{}\") with NO last_result): the \
             question is re-posed to your session, and the answer you then relay counts. Until that \
             happens the Spec cannot move past it",
            run.id, pending.prompt, run.id
        ),
    })])
}

/// The SESSION a tick comes from: the terminal the `nyx_next` call carried
/// ([`crate::mcp::TERMINAL_ID_HEADER`]).
///
/// Threaded through the tick because it is the only input on this path an agent cannot manufacture,
/// and the CONSENT check (H6) is built entirely on it: a question is posed TO a terminal, and its
/// answer is only an answer if the human at that terminal took a turn in between (the ledger itself
/// is read from the DB — [`db::last_human_turn`] — because it must outlive the session that was
/// asked). `terminal_id` is `None` for a caller nyx cannot attribute — a bare MCP client, a test —
/// and such a caller is never ASKED a consent question at all ([`refuse_unattributable_consent`]):
/// with no terminal there is no ledger, so no answer it relays could ever be checked, and a question
/// whose every answer is refused only costs the human the trouble of answering it.
#[derive(Clone, Copy)]
pub struct CallerSession<'a> {
    pub terminal_id: Option<&'a str>,
}

/// `nyx_next` — `{ run_id, last_result?, provider? }` → `{ run_id, action, current_node_id,
/// blackboard, status }`. Ticks the run's engine, runs nyx's own effects (gate/transition) inline,
/// and returns the next atomic action for the dispatcher (or `{ "type": "done" }`).
///
/// `last_result` is what the dispatcher extracted from the leaf it was told to perform last (a
/// sub-agent `diff`, a judge `verdict`, an `ask_human` `decision`, a `produce` `artifact`; absent
/// on the first tick). `provider` is carried for the spawn adapter (task #6); it does not affect
/// the enforcement, which is provider-independent. `caller` is the session behind the call — read
/// only by the consent check (H6).
pub fn nyx_next(
    db: &Db,
    runtime: &EngineRuntime,
    caller: CallerSession<'_>,
    args: &Value,
) -> Result<ToolOutcome, RpcError> {
    let run_id = require_str(args, "run_id")?;
    // ONE tick at a time on a run (v2.8 L1). Taken FIRST — before the run row is even read — so the
    // whole read-walk-persist below is atomic against another tick on this run. A guard taken later
    // (around the walk alone) would still let two ticks read the same cursor and blackboard, which
    // is the half that decides what the walk does.
    let _tick = runtime
        .enter_tick(run_id)
        .ok_or_else(|| refuse_concurrent_tick(run_id))?;
    let last_result = args.get("last_result");
    // The session's provider drives the sub-agent spawn MECHANISM (native Task vs inline fallback);
    // it never affects the enforcement. Defaults to Claude.
    let provider = optional_str(args, "provider")?
        .unwrap_or("claude")
        .to_string();

    // Load the run row: its Spec binding (for transitions) + workflow id + persisted blackboard.
    let run = db
        .with_conn(|c| db::get_workflow_run(c, run_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("unknown workflow run '{run_id}'"),
            )
        })?;

    // Fetch (or lazily BUILD from the DB) the run's engine SLOT — the single-session traversal
    // stack. The slot STAYS in the registry between ticks; the tick guard above is what makes this
    // read-or-build atomic, so no other tick can be building a second engine for the same run.
    let slot = match runtime.slot(run_id) {
        Some(slot) => slot,
        None => runtime.install(run_id, build_engine(db, &run)?),
    };

    let (outcome, finished) = {
        let mut engine = slot.lock().unwrap_or_else(|p| p.into_inner());
        // RE-READ the tree before ticking: a resident engine holds the tree — INCLUDING every node's
        // decoded config — as it stood at its first tick, so without this a mid-run `node_update`
        // reached the walk only once a LATER session rebuilt it. That is the very thing the authoring
        // layer promises when it lets that edit through under a live run (M4), and the thing an
        // unconfigured blocking gate's own refusal tells the caller to do.
        //
        // UNCONDITIONAL, including on the tick that just built the engine from that same tree: one
        // path, no "was it fresh?" branch to get wrong. One tree read per tick is the price of the
        // tick always executing the graph as it is authored NOW.
        let outcome = adopt_current_tree(db, &run, &mut engine)
            .and_then(|()| drive(db, runtime, &run, &mut engine, last_result, &provider, caller));
        // A run that reached the end (or died on a malformed tree) will never be ticked again, so its
        // slot is dropped. A REFUSAL is NOT terminal — that engine stays resident so the refusal is
        // STICKY (the refused leaf is re-attempted, or the end is re-refused behind the red gate);
        // note a gate refusal CAN come from `Step::Done`, hence the `is_ok()` and not just `is_done`.
        let finished = (outcome.is_ok() && engine.is_done()) || outcome_is_fatal(&outcome);
        (outcome, finished)
    };
    if finished {
        runtime.forget(run_id);
    }
    outcome
}

/// Whether a `drive` outcome is a TERMINAL engine failure — the [`map_engine_err`] marker (a
/// malformed tree / a mis-authored loop), which re-ticking can only reproduce.
fn outcome_is_fatal(outcome: &Result<ToolOutcome, RpcError>) -> bool {
    let Err(e) = outcome else { return false };
    e.data
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        == Some("engine_error")
}

/// Read `run`'s workflow tree from the DB — the LIVE authored graph, decoded configs and all.
fn load_tree(db: &Db, run: &db::WorkflowRun) -> Result<Tree, RpcError> {
    let workflow = db
        .with_conn(|c| db::get_workflow(c, &run.workflow_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("run '{}' references unknown workflow", run.id),
            )
        })?;
    let nodes = db
        .with_conn(|c| db::list_nodes(c, &run.workflow_id))
        .map_err(internal_db)?;
    Tree::from_rows(&nodes, workflow.root_node_id.as_deref()).map_err(map_engine_err)
}

/// Hand the RESIDENT engine the tree as it stands NOW, so an edit made mid-run reaches the walk on its
/// VERY NEXT tick ([`Engine::adopt_tree`]) — what `node_update` tells its caller will happen.
///
/// Only a `node_update` (label / config) may reach a walking run: every node mutation that MOVES an id
/// is refused while a run walks the workflow, and such an edit leaves the structure identical, so the
/// adoption is a swap and the walk is untouched. A tree that is NOT structurally identical means the
/// graph moved some other way, under a walk whose frames are indices into it: the tick REFUSES rather
/// than resume on a node the frames no longer point at. That refusal is deliberately NOT the
/// [`map_engine_err`] `engine_error` (which would drop the run's engine and let a later, luckier
/// resume replay a shifted walk) — the run cannot be ticked again and is told to be dropped.
fn adopt_current_tree(db: &Db, run: &db::WorkflowRun, engine: &mut Engine) -> Result<(), RpcError> {
    engine.adopt_tree(load_tree(db, run)?).map_err(|e| {
        RpcError::new(
            RpcCode::InvalidState,
            format!(
                "run '{}' cannot be advanced: {e} — its recorded walk no longer describes this \
                 graph, so nyx will not guess which node it was on; drop the run (run_delete \
                 run_id='{}') and start a new one",
                run.id, run.id
            ),
        )
        .with_data(json!({ "reason": "run_tree_reshaped" }))
    })
}

/// Rebuild a fresh [`Engine`] for `run` from its stored workflow tree, seeded with the run's
/// persisted blackboard.
fn build_engine(db: &Db, run: &db::WorkflowRun) -> Result<Engine, RpcError> {
    let tree = load_tree(db, run)?;
    let board = Blackboard::from_json_str(&run.blackboard);
    // A FINISHED run (done/failed) is never ticked again — a CLEAR terminal message, distinct from
    // the resume paths below (v2.8 review m3: a terminated run used to return the resume-refusal
    // wording, conflating "finished" with "lost mid-flight").
    if run.status == db::RUN_STATUS_DONE || run.status == db::RUN_STATUS_FAILED {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "run '{}' has already finished ({}) — it cannot be ticked; start a new run",
                run.id, run.status
            ),
        )
        .with_data(json!({ "reason": "run_finished" })));
    }
    // The PROGRESS signal is the persisted CURSOR, not blackboard emptiness: a run parked on its
    // FIRST leaf (e.g. ndev's opening `spawn_subagent`) has an EMPTY blackboard yet a
    // `current_node_id`. A FRESH run (cursor NULL) is the legitimate cold start from the root.
    match (run.current_node_id.is_some(), run.traversal.as_deref()) {
        // A TRULY fresh run (no cursor AND no traversal): start the walk at the root.
        (false, None) => Ok(Engine::new(tree, board)),
        // A PROGRESSED run WITH a persisted traversal: resume the exact walk cross-session — the
        // whole of B1. Restoring the stack (not replaying from the root) is what stops the resume
        // from re-spawning sub-agents / re-running gates / re-applying transitions. This also covers a
        // run whose cursor was SET-NULL by a built-in REPLACEMENT (N11): its stale traversal fails to
        // restore below, so it is REFUSED — never silently replayed from the root.
        (_, Some(traversal)) => Engine::restore(tree, board, traversal).map_err(|e| {
            // The traversal references a node the current tree no longer holds (the workflow was
            // edited structurally): refuse rather than resume a stale walk.
            RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "run '{}' cannot be resumed: its persisted traversal no longer fits the workflow \
                     tree ({e}) — the graph was edited; start a new run",
                    run.id
                ),
            )
            .with_data(json!({ "reason": "run_traversal_stale" }))
        }),
        // A progressed run with NO persisted traversal (a pre-B1 run, or one whose traversal was
        // never written): replaying from the root would re-execute effects, so refuse loudly.
        (true, None) => Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "run '{}' cannot be resumed: no persisted traversal state, and replaying the graph \
                 from the root would re-execute its effects — start a new run",
                run.id
            ),
        )
        .with_data(json!({ "reason": "run_not_resumable" }))),
    }
}

/// Drive the engine from its current cursor to the next dispatcher-facing suspension (or the end),
/// running nyx's own effects inline and enforcing the gate + state rules. Persists the run each
/// time it stops.
///
/// OWNS the tick's NOTES, and that is why the walk is a separate function. The notes are what the
/// walk SAYS to the human — that a child run was dropped, that a completion leaves findings behind —
/// and they rode the returned action only ([`with_notes`]). A tick that REFUSES hands back no action,
/// so every one of them died with the stack frame: the walk went past "the child run this step was
/// waiting on was DROPPED — SAY SO to the human", then hit a still-red gate, and the caller was told
/// about the gate and nothing else. Attaching them HERE, on the one exit every refusal funnels
/// through, is what makes "a note is never lost" a property of the tick rather than a habit to
/// repeat at a dozen `return Err` sites.
///
/// OWNS THE TICK'S INVALIDATIONS TOO, for the same reason and by the same means. A tick is the one
/// MCP call that mutates SEVERAL domains behind ONE return value: it walks the graph and runs nyx's
/// own effects inline, so what it touched is known only to the walk, one arm at a time. See
/// [`outcome`] for what the shell does with them and what it cost to hand it a constant instead.
fn drive(
    db: &Db,
    runtime: &EngineRuntime,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    last_result: Option<&Value>,
    provider: &str,
    caller: CallerSession<'_>,
) -> Result<ToolOutcome, RpcError> {
    // The notes WALKED PAST during this tick, in graph order — they ride the next action this tick
    // hands back (G4, see [`with_notes`]), or the refusal that replaces it ([`refusal_with_notes`]).
    let mut notes: Vec<String> = Vec::new();
    // The topics this tick INVALIDATED, in the order the walk touched them. Seeded with `Specs`
    // because EVERY tick advances the run itself (cursor + blackboard), which is what the Specs
    // board, the Spec detail and the engine visualiser all re-pull on; an arm that mutates another
    // domain adds its own through [`note_effect`].
    let mut effects: Vec<ChangedTopic> = vec![ChangedTopic::Specs];
    let walked = walk(
        db,
        runtime,
        run,
        engine,
        last_result,
        provider,
        caller,
        &mut notes,
        &mut effects,
    );
    match walked {
        // ONE assembly point, the same one the notes use. [`outcome`] builds the success value from
        // the action the tick STOPPED on and names the `Specs` floor every tick invalidates; it
        // CANNOT know the rest, because the node that mutated is one the walk already left behind.
        // So the accumulated set is attached here, where the walk is over and every arm has spoken.
        Ok(mut out) => {
            out.effects = effects;
            Ok(out)
        }
        Err(e) => Err(refusal_with_notes(e, &notes)),
    }
}

/// RECORD that this tick invalidated `topic`, once — the walk's half of the tick's `changed`
/// contract ([`outcome`]).
///
/// Idempotent because a graph LOOPS: a body holding an effect node is walked once per pass, and the
/// shell pushes one `*://changed` per entry ([`ToolOutcome::effects`]), so a naive `push` would ask
/// the renderer to re-pull the same list once per iteration. The topic set is tiny and the order is
/// the order the walk touched them, so a linear scan is the whole implementation.
fn note_effect(effects: &mut Vec<ChangedTopic>, topic: ChangedTopic) {
    if !effects.contains(&topic) {
        effects.push(topic);
    }
}

/// ATTACH the notes the walk went past to a REFUSAL, under `data.notes` — the same array
/// [`with_notes`] puts on an action, in the one place a refused tick can still carry them.
///
/// MERGED into the existing `data` (never replacing it): a refusal's markers — `reason`, `node_id`,
/// `gate_result` — are what the dispatcher branches on, and they must survive a note riding beside
/// them. A refusal that carried no `data` at all gets an object holding just the notes.
fn refusal_with_notes(mut err: RpcError, notes: &[String]) -> RpcError {
    if notes.is_empty() {
        return err;
    }
    let list = Value::Array(notes.iter().map(|n| Value::from(n.as_str())).collect());
    match err.data.as_mut().and_then(Value::as_object_mut) {
        Some(obj) => {
            obj.insert("notes".to_string(), list);
            err
        }
        None => err.with_data(json!({ "notes": list })),
    }
}

/// The walk itself — see [`drive`], which owns the notes and the effects it fills and is the only
/// caller.
#[allow(clippy::too_many_arguments)]
fn walk(
    db: &Db,
    runtime: &EngineRuntime,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    last_result: Option<&Value>,
    provider: &str,
    caller: CallerSession<'_>,
    notes: &mut Vec<String>,
    effects: &mut Vec<ChangedTopic>,
) -> Result<ToolOutcome, RpcError> {
    // Decode the dispatcher's reported result for the leaf we were parked on (None on the first
    // tick, or when the parked leaf carries no payload).
    let mut result: Option<NodeResult> = engine
        .awaiting_node_type()
        .map(|node_type| decode_result(node_type, last_result));

    // The three ways a parked `ask_human` can be ticked without a usable answer, and what nyx owes
    // the caller for each (F4, then H5):
    //
    //   * NO ANSWER AT ALL — RE-PRESENT the same question. A tick with no `last_result` is exactly
    //     what a dispatcher RESUMING after a crash produces, and resuming after a crash is a stated
    //     principle of this engine, so it is not an edge case. It used to fold `Human(null)`: the
    //     variable took `null` (observed live: `"blackboard_delta": {"election": null}`) and the run
    //     ADVANCED, losing in silence the very question a human was asked. A question nobody answered
    //     has no answer, and the only honest reply is to ask it again.
    //   * an unusable SHAPE (no `decision`, none of its synonyms, or an object/array under them) —
    //     REFUSE OUT LOUD. Re-presenting the identical node told the caller nothing, so a wrongly-keyed
    //     answer looped blind and unbounded; the run stays parked exactly where it is (nothing is
    //     persisted here) and the error names the field + the allowed values.
    //   * N19, a WELL-FORMED but OFF-VOCABULARY answer — re-ask the SAME question rather than silently
    //     folding a value a downstream selector would then miss (falling through to its default). The
    //     re-presented action now carries `answer_key` + `options`, so the re-ask is no longer mute.
    //
    // ONLY a parked `ask_human`. A `note` or an acked leaf legitimately reports nothing on its own
    // tick and must still advance — "no result" is an ANSWER there, and a missing one only here.
    //
    // And a FOURTH way, for a question whose answer COMMITS something the session does not own (H6,
    // then J2): the answer is well-formed, on-vocabulary, and still not the human's. That check comes
    // last, because it is about WHO answered and the three above are about WHAT was answered.
    let options = engine.awaiting_options();
    if engine.awaiting_node_type() == Some(NodeType::AskHuman) {
        match last_result.filter(|v| !v.is_null()) {
            None => {
                engine.replay_current();
                result = None;
            }
            Some(reported) => match human_answer(Some(reported)) {
                None => return Err(ask_human_answer_error(&options)),
                // Empty `options` = free-form (no contract, pre-N19): any well-formed answer folds.
                Some(answer) => {
                    let known = options.is_empty()
                        || answer
                            .as_str()
                            .map(|a| options.iter().any(|o| o == a))
                            .unwrap_or(false);
                    if !known {
                        // Discard the off-vocabulary answer and re-yield the ask_human (N19).
                        engine.replay_current();
                        result = None;
                    } else if let Some(commitment) = awaiting_commitment(engine) {
                        // H6 + J2 — this answer disposes of something the session does not own (the
                        // Spec's lifecycle, or another round of build spend), so it is only taken
                        // from a human turn nyx itself witnessed. Accepting it closes the question
                        // and journals it; refusing leaves the run parked, nothing persisted, and
                        // the leaf still asking.
                        accept_consent_answer(db, run, caller, &answer, commitment)?;
                    }
                }
            },
        }
    }

    // A `sub_workflow` leaf PARKS the parent across ticks (it suspends on a CHILD run). On resume, do
    // NOT fold the dispatcher's reported result and advance — RE-YIELD the leaf so the SubWorkflow arm
    // re-checks the CHILD run STATUS (the source of truth) rather than trusting the sub-agent's claim.
    if engine.awaiting_node_type() == Some(NodeType::SubWorkflow) {
        engine.replay_current();
        result = None;
    }

    // The hard-gate anchor: the SET of BLOCKING gate node_ids nyx ran RED. While it is non-empty,
    // `nyx_next` must not advance past it (never a transition / done). A SET, not one anchor, so a
    // GREEN gate only clears ITS OWN red — never a DIFFERENT gate's, which would let a multi-gate
    // `sequence[gate_A(red), gate_B(green), transition]` slip past the still-red A (v2.8 review H1).
    // Seeded from the PERSISTED blackboard (a JSON array), because the guarded `transition`/`done` is
    // typically reached on a LATER call of the dispatcher loop than the gate that must block it.
    //
    // A **B-TREE** set and not a hash one, because the two block points below name the refusing gate
    // with `.iter().next()`. Over a `HashSet` that is a draw: the set is rebuilt from the board on
    // every `nyx_next`, so a fresh `RandomState` each tick meant two consecutive calls on the SAME
    // state could name A then B — while the board persisted `[A, B]` sorted, and while
    // `reeval_red_gates` re-ran them in that same arbitrary order, each pass overwriting
    // [`LAST_GATE_RESULT_KEY`], so the gate named and the gate whose evidence rode with the refusal
    // were not reliably the same one. Ordered, the refusal names the FIRST id of the array the human
    // can read on the board, every time.
    let mut blocking_gates_red: BTreeSet<String> = engine
        .blackboard()
        .get(BLOCKING_GATE_RED_KEY)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // The gates this tick has ALREADY run over the PTY. The M5 re-eval at a block point re-runs
    // every still-anchored red gate, and a `sequence[gate, …, transition]` reaches that block point
    // in the SAME walk as the gate itself — so without this the project's test suite ran TWICE per
    // tick, and a tick could hold its run (and an MCP worker) for two whole
    // [`GATE_TOTAL_TIMEOUT`] budgets. A verdict taken a few milliseconds ago is the verdict.
    let mut evaluated_this_tick: HashSet<String> = HashSet::new();

    // Every stop below journals the SAME input — `last_result` is what the whole tick decided on —
    // and its own action type (`None` when the stop hands the dispatcher nothing: a refusal, a
    // failure, which is itself what it decided).
    loop {
        let step = match engine.tick(result.take()) {
            Ok(step) => step,
            Err(e) => {
                let trace = tick_trace(None, last_result);
                let _ = persist(db, run, engine, None, db::RUN_STATUS_FAILED, &trace);
                return Err(map_engine_err(e));
            }
        };

        match step {
            Step::Done => {
                // M5 — RE-RUN each still-anchored red gate at the block point before refusing: a run
                // parked past a red gate now has an EXIT (fix the cause + call again ⇒ the gate re-runs
                // green ⇒ the block clears ⇒ the run advances). Only a gate STILL red after re-eval blocks.
                reeval_red_gates(
                    db,
                    run,
                    engine,
                    &mut blocking_gates_red,
                    &evaluated_this_tick,
                    last_result,
                )?;
                if let Some(node_id) = blocking_gates_red.iter().next().cloned() {
                    return Err(refuse_gate(db, run, engine, &node_id, last_result));
                }
                // The notes still pending ride the `done` — the run's last word carries what the walk
                // had to say (G4).
                let done = with_notes(json!({ "type": "done" }), notes.as_slice());
                let trace = tick_trace(action_type(&done), last_result);
                persist(db, run, engine, None, db::RUN_STATUS_DONE, &trace)?;
                return Ok(outcome(run, engine, done, db::RUN_STATUS_DONE));
            }
            Step::Action(action) => match action {
                // nyx runs the gate ITSELF over the PTY, and reads the exit code as the verdict.
                Action::GateCheck {
                    node_id,
                    command_ids,
                    slot,
                    blocking,
                    expect,
                } => {
                    // WHICH commands this gate runs is resolved HERE, through the one path both ways
                    // of arming a gate go through ([`gate_commands`]): a `slot` gate asks the run's
                    // project what it binds that slot to, a plain one uses its own ids. Everything
                    // below — the vacuity refusal, the PTY run, the red anchor — then reads a single
                    // "the commands to run" and never has to know which shape produced it.
                    // A resolution that FAILS parks the run on this gate instead of erroring bare —
                    // an un-replayed gate is a gate the next tick walks past (see
                    // [`block_on_effect_error`]).
                    let resolved = run_project_id(db, run).and_then(|project_id| {
                        gate_commands(db, project_id.as_deref(), &command_ids, slot.as_deref())
                    });
                    let command_ids = match resolved {
                        Ok(ids) => ids,
                        Err(e) => {
                            return Err(block_on_effect_error(
                                db,
                                run,
                                engine,
                                &node_id,
                                last_result,
                                e,
                            ))
                        }
                    };
                    // A BLOCKING gate with NO commands REFUSES to advance — it is an `invalid_state`
                    // at the tick, never a green light. Such a gate cannot fail, so its GREEN carries
                    // no information: the loop it closes exits on its first round and the `done` that
                    // follows certifies work nobody checked. Observed live on the seeded coder's
                    // "checks" node: `gate_green = true`, `last_diff = "no files changed — no code
                    // written"`, one round, task marked `done` — and, one Spec up, a `completed` Spec
                    // whose central feature was never built. The verdict a gate is asked for is not
                    // "did anything fail" but "was this verified"; with no command the honest answer
                    // is "no", and "no" does not advance a run.
                    //
                    // A NON-BLOCKING gate with no commands stays a WARNING (`gate_unconfigured` at
                    // run_start), and that is not an inconsistency: a non-blocking gate is ADVISORY by
                    // construction — nothing downstream is entitled to read its verdict as proof — so
                    // an empty one misleads a human reading the graph without ever certifying a thing.
                    // Only the blocking gate makes a promise, and only a promise can be broken.
                    //
                    // An UNBOUND SLOT lands here identically, and deliberately: a gate declaring
                    // "the project's build check" that the project never answered verifies exactly as
                    // much as one with no commands — nothing. The rule is untouched; only the route
                    // out differs (bind the slot, not fork the graph — [`arm_gate_hint`]).
                    if blocking && command_ids.is_empty() {
                        return Err(refuse_vacuous_gate(db, run, engine, &node_id, last_result));
                    }
                    // The expectation this attempt is GRADED against — the declared one, else
                    // `green`. `expect` (the Option) is kept beside it because WHICH ENFORCEMENT
                    // applies below turns on "did the node declare one", not on the value.
                    let expectation = expect.unwrap_or(GateExpectation::Green);
                    let gate = match run_gate(db, &command_ids) {
                        Ok(gate) => gate,
                        // The gate could not be RESOLVED at all (an unknown command instance, a run
                        // directory that no longer exists). That is not a green light and not a
                        // skip: record the attempt as INDETERMINATE — it proves nothing, and a
                        // silent hole in the journal is exactly what an auditor cannot detect —
                        // then park on the node and replay.
                        //
                        // THE JOURNAL NAMES THE COMMAND THAT ACTUALLY FAILED, and keeps the ones
                        // that already ran ([`GateRunError`]). It used to name `command_ids.first()`
                        // with empty cmdline/cwd whatever went wrong, so a two-command gate whose
                        // SECOND command was unresolvable accused the first — the one that had just
                        // run fine — and threw its result away. Those three fields are the identity
                        // the auditor confronts between a red and a green; an attempt that misnames
                        // them is worse than one that is missing.
                        Err(e) => {
                            let mut commands = e.done;
                            commands.push(GateCommandResult::could_not_run(
                                &e.failing_id,
                                "",
                                "",
                                &e.error.message,
                            ));
                            record_gate_attempt(
                                db,
                                &GateAttempt {
                                    run,
                                    node_id: &node_id,
                                    slot: slot.as_deref(),
                                    expect: expectation,
                                    declared: expect.is_some(),
                                    blocking,
                                    grade: GateGrade::Indeterminate,
                                    satisfied: false,
                                    commands: &commands,
                                },
                            );
                            return Err(block_on_effect_error(
                                db,
                                run,
                                engine,
                                &node_id,
                                last_result,
                                e.error,
                            ));
                        }
                    };
                    let green = gate.passed;
                    let grade = gate.outcome;
                    // A gate whose command could not be LAUNCHED — see [`gate_spawn_failure`]. Read
                    // before `gate` is consumed by the surfaced result below, because the `None` arm
                    // has to park on it.
                    let spawn_failure = gate_spawn_failure(&gate);
                    // The SECOND fact, and the one a boolean could not carry: did what the command
                    // did ANSWER what this node declared. An `indeterminate` answers neither
                    // expectation, so this is false for it under `green` and under `red` alike.
                    let satisfied = expectation.is_satisfied_by(grade);
                    // This gate has just been judged on this very tick — the M5 re-eval at the
                    // block point below must not run the project's whole suite a SECOND time for it.
                    evaluated_this_tick.insert(node_id.clone());
                    // M6 — SURFACE the gate's structured result (the command(s), their cwd, exit code
                    // or timeout marker, and a bounded output tail) on the blackboard: it PERSISTS with
                    // the run and rides the tick's `blackboard_delta`, so both the dispatcher and the UI
                    // can see WHY a gate is the colour it is. Only a gate with REAL commands writes it —
                    // an empty (vacuously green) gate has nothing to explain, so it leaves no residue.
                    //
                    // The board key is the LATEST attempt and only that — the next gate overwrites it.
                    // The DURABLE, append-only record of the same attempt is the journal below, which
                    // is what lets a red and the green that follows it be read as one sequence.
                    if !gate.commands.is_empty() {
                        record_gate_attempt(
                            db,
                            &GateAttempt {
                                run,
                                node_id: &node_id,
                                slot: slot.as_deref(),
                                expect: expectation,
                                declared: expect.is_some(),
                                blocking,
                                grade,
                                satisfied,
                                commands: &gate.commands,
                            },
                        );
                        engine.set_board_as(
                            &node_id,
                            LAST_GATE_RESULT_KEY,
                            gate_result_json(&node_id, expectation, satisfied, gate),
                        );
                    }
                    match expect {
                        // NO DECLARED EXPECTATION — the pre-expectation enforcement, unchanged, to
                        // the letter. A GREEN gate clears ONLY ITS OWN red anchor; a RED BLOCKING
                        // gate raises its own. A red NON-blocking gate, and a green NON-blocking gate
                        // that was never anchored, change nothing — neither must touch another gate's
                        // refusal (H1). The refusal itself lands later, at the `transition`/`done`
                        // this anchor guards.
                        None => {
                            // …WITH ONE EXCEPTION THE ANCHOR CANNOT EXPRESS: a command that could
                            // not be LAUNCHED. Before the tri-valued grade, a failed spawn came out
                            // of the runner as an `Err` and parked the run on this very node in ONE
                            // tick, with the spawn's own message. Graded, it became an ordinary
                            // "not green" and the walk CONTINUED to the next block point — which, on
                            // the seeded coder, means the `until gate_green` loop re-entering its
                            // body and re-spawning the coder up to 25 times to repair code that is
                            // not broken, before dying on a loop limit that names `gate_green`
                            // instead of the missing binary. "The binary is not there" is not a
                            // verdict about the work, and the enforcement `run_one_gate`'s doc
                            // promises was "unchanged" only for a DECLARED expectation. So it parks
                            // here too — after the attempt above was journaled as `indeterminate`,
                            // which is the record an auditor reads.
                            //
                            // A TIMEOUT is deliberately NOT this case: it is also `indeterminate`,
                            // but it is a check that RAN and did not finish, the anchor's
                            // "cut short" refusal already says so in its own words, and parking on
                            // it would change an enforcement nobody found wanting.
                            if blocking {
                                if let Some(why) = spawn_failure {
                                    return Err(block_on_effect_error(
                                        db,
                                        run,
                                        engine,
                                        &node_id,
                                        last_result,
                                        why,
                                    ));
                                }
                            }
                            if green {
                                blocking_gates_red.remove(&node_id);
                            } else if blocking {
                                blocking_gates_red.insert(node_id.clone());
                            }
                            // Mirror the set onto the persisted board so it carries across the loop.
                            mirror_blocking_gates(engine, &blocking_gates_red);
                        }
                        // A DECLARED EXPECTATION is enforced HERE and NEVER through the M5 anchor,
                        // and the reason is the whole point of the separation:
                        //
                        // M5 RE-RUNS every anchored gate at each block point. A reproduction gate
                        // (`expect: red`) that anchored itself would therefore be re-run AFTER the
                        // fix — where it passes, where passing no longer answers `red`, and where it
                        // would refuse the transition FOREVER. The run would be wedged by the very
                        // proof it just produced. So a declared gate raises no anchor, clears none
                        // (another gate's red is not its business — H1 again), and is never a
                        // candidate for re-evaluation.
                        //
                        // What replaces the anchor is a refusal ON THE SPOT, which is strictly
                        // stronger: the mismatch does not wait for a `transition` to exist
                        // downstream, so a graph that ends on a plain `done` — or on nothing at all —
                        // is guarded too. It is STICKY by the same mechanism every gate refusal uses:
                        // the leaf is marked for REPLAY, so the next tick re-runs THIS gate and hits
                        // the same refusal until an attempt satisfies the expectation.
                        Some(_) => {
                            if blocking && !satisfied {
                                // The tick stops HERE, so the ordinary fold never runs — write the
                                // three verdict keys first, or the one run somebody is about to
                                // inspect (the blocked one) would be the one with an empty board.
                                engine.set_gate_verdict(
                                    &node_id,
                                    GateVerdict {
                                        grade,
                                        expect: expectation,
                                    },
                                );
                                return Err(refuse_gate_expectation(
                                    db,
                                    run,
                                    engine,
                                    &node_id,
                                    expectation,
                                    grade,
                                    last_result,
                                ));
                            }
                            // A DECLARED GATE THAT ANSWERED ITS DECLARATION CARRIES NO ANCHOR — and
                            // that has to be enforced, not merely intended. "A declared gate raises
                            // no anchor" was true of the gate as it stands now; it says nothing
                            // about an anchor it INHERITED, which is exactly what a `node_update`
                            // adding `expect: red` to a gate that already went red without one
                            // leaves behind — the route out this module's own
                            // [`refuse_gate_expectation`] recommends. The anchor then outlives the
                            // declaration: the gate satisfies `red`, the journal records it
                            // satisfied, and the M5 guard refuses the transition forever for a red
                            // the node no longer treats as one. Dropping its own id here (and in the
                            // re-eval, which lifts on `satisfied` for a declared gate) is what makes
                            // the two enforcements mutually exclusive in fact.
                            if satisfied && blocking_gates_red.remove(&node_id) {
                                mirror_blocking_gates(engine, &blocking_gates_red);
                            }
                        }
                    }
                    // Feed the verdict back so a surrounding loop/selector branches on it, and keep
                    // ticking (the gate is never surfaced to the dispatcher).
                    result = Some(NodeResult::Gate(GateVerdict {
                        grade,
                        expect: expectation,
                    }));
                }
                // nyx IMPOSES the transition through the guarded Spec authority (state rule).
                Action::Transition {
                    node_id,
                    target_state,
                    gate_on,
                } => {
                    // M5 — RE-RUN each still-anchored red gate before refusing the transition: the run
                    // parked here on red now has an EXIT (fix the cause + call `nyx_next` again ⇒ the
                    // gate re-runs green ⇒ the block clears ⇒ the transition fires). See `reeval_red_gates`.
                    reeval_red_gates(
                        db,
                        run,
                        engine,
                        &mut blocking_gates_red,
                        &evaluated_this_tick,
                        last_result,
                    )?;
                    if let Some(gate_node) = blocking_gates_red.iter().next().cloned() {
                        return Err(refuse_gate(db, run, engine, &gate_node, last_result));
                    }
                    // An ENGAGING transition (ready/done/cancel) fires only when its preceding
                    // ask_human's approval key is affirmative on the blackboard (B3).
                    let gate_satisfied = gate_on
                        .as_deref()
                        .map(|k| board_value_is_affirmative(engine.blackboard().get(k)))
                        .unwrap_or(false);
                    // AND the gate's affirmative must be a HUMAN's. The static gate answers "is this
                    // key saying yes"; the provenance answers "who put that yes there", which is the
                    // question a `gate_on` pointing at a `query` fact never had to face. Asked only
                    // when the transition is ENGAGING and about to fire, so a non-sensitive move
                    // (`start`, `in_review`) and a gate already refusing are untouched.
                    //
                    // AND ONLY ON A RUN THAT HAS A SPEC. `apply_transition` returns early for a
                    // Spec-less run: there is no lifecycle to move, so the node commits nothing, and
                    // refusing it would park a run over an effect that never happens (M2).
                    if gate_satisfied
                        && run.spec_id.is_some()
                        && transition_is_engaging_target(&target_state)
                    {
                        if let Err(e) = crossing_is_authorized(
                            engine,
                            &node_id,
                            crate::engine::SensitiveEffect::SpecTransition,
                            notes,
                        ) {
                            return Err(block_on_effect_error(
                                db,
                                run,
                                engine,
                                &node_id,
                                last_result,
                                e,
                            ));
                        }
                    }
                    if let Err(e) = apply_transition(db, run, &target_state, gate_satisfied) {
                        // The transition was REFUSED (illegal source / a confirmation-gated target),
                        // so it must be RE-ATTEMPTED on the next call, never skipped: without the
                        // replay the next tick would ack the parked leaf and walk straight past it.
                        engine.replay_current();
                        let trace = tick_trace(None, last_result);
                        let _ = persist(
                            db,
                            run,
                            engine,
                            Some(&node_id),
                            db::RUN_STATUS_BLOCKED,
                            &trace,
                        );
                        return Err(e);
                    }
                    // A COMPLETION that leaves deferred findings behind SAYS SO — through the notes
                    // channel, which is precisely the "the walk has something to tell you and nothing
                    // to ask" seam (G4). The note rides the run's own `done`, so the session's last
                    // word to the human names what this Spec is shipping with.
                    if let Some(text) = deferred_findings_note(db, run, &target_state)? {
                        notes.push(text);
                    }
                    result = Some(NodeResult::Ack);
                }
                // nyx reads its OWN state and folds the verified fact onto the blackboard, then keeps
                // ticking so a following selector/loop branches on it (never surfaced to the dispatcher).
                Action::Query {
                    node_id,
                    query,
                    write,
                    target,
                    task_key,
                    root_key,
                    name_key,
                } => {
                    // The fact is resolved into a PAIR of board writes first, so a fact nyx cannot
                    // read parks the run on this node rather than erroring bare — an un-replayed
                    // query is a query the next tick walks past, leaving the branch below it to
                    // route on the `null` nobody wrote (see [`block_on_effect_error`]).
                    let resolved: Result<Vec<(String, Value)>, RpcError> = (|| {
                        if query == QUERY_SPEC_CAN_TRANSITION {
                            // A PARAMETERIZED fact: fold the bool AND the reason (`<write>_reason`)
                            // so the graph can both route AND explain a refusal.
                            let (can, reason) = resolve_can_transition(db, run, target.as_deref())?;
                            Ok(vec![
                                (write.clone(), Value::from(can)),
                                (format!("{write}_reason"), Value::from(reason)),
                            ])
                        } else if query == QUERY_TASK_CLOSE_ROUTE {
                            // A TASK-GRAINED fact, read for the task the named board key holds: fold
                            // the route AND the sentence explaining it (`<write>_reason`), which is
                            // what the escalation prompt shows the human — the task's title and the
                            // coder's own blocked reason, not a node id.
                            let task_id = engine
                                .blackboard()
                                .get(task_key.as_deref().unwrap_or(crate::engine::DEFAULT_TASK_KEY))
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            let (route, reason) = resolve_task_close_route(db, task_id.as_deref())?;
                            Ok(vec![
                                (write.clone(), route),
                                (format!("{write}_reason"), Value::from(reason)),
                            ])
                        } else if query == QUERY_DESIGN_ROUND_PAGES {
                            // Fold the COUNT the branch tests AND the listing (`<write>_reason`) the
                            // election prompt shows: a human asked to elect must be told what is on
                            // the table, and the pages are only knowable at the moment of asking.
                            let (count, listing) = resolve_design_round_pages(db, run)?;
                            Ok(vec![
                                (write.clone(), count),
                                (format!("{write}_reason"), Value::from(listing)),
                            ])
                        } else if query == QUERY_PROJECT_WORKTREES_ROOT {
                            // TWO writes for ONE fact, and the second one exists so a QUESTION can
                            // fill the first. The graph must branch on "is a root configured?" AND
                            // hand the root itself to the `create_workspace` below — but when nothing
                            // is configured the ANSWER to the question takes that key over, and J1
                            // forbids an `ask_human` writing a key a selector branches on (a folder
                            // path has no closed answer set to declare). So the BRANCHABLE half is
                            // folded out as `<write>_configured`, leaving `<write>` free to be
                            // answered into. One key holds the root, whoever filled it.
                            let root = resolve_query(db, run, &query)?;
                            let configured =
                                root.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false);
                            Ok(vec![
                                (write.clone(), root),
                                (format!("{write}_configured"), Value::from(configured)),
                            ])
                        } else if query == QUERY_WORKSPACE_TARGET_FREE {
                            // The WORKSPACE prevalidation, read over the SAME board keys the
                            // `create_workspace` it guards reads — so the folder checked here and the
                            // folder created there are one folder, not two derivations that can drift.
                            //
                            // SAME KEYS IS NOT ENOUGH: the RENDERING has to match too. The effect
                            // resolves its keys through `engine::interpolate`, which renders a
                            // NON-string board value as its JSON form (`123`, `true`), while an
                            // `as_str()` read answers `None` for exactly those values. Reading them
                            // differently is the drift this comment claims does not exist: the check
                            // would answer about `""` (fail-closed "no root") while the effect built a
                            // real path out of `123`. So the two spellings are ONE, here.
                            let board = |key: Option<&str>, default: &str| -> String {
                                match engine.blackboard().get(key.unwrap_or(default)) {
                                    Some(Value::String(s)) => s.clone(),
                                    Some(v) => v.to_string(),
                                    None => String::new(),
                                }
                            };
                            let root = board(
                                root_key.as_deref(),
                                crate::engine::DEFAULT_WORKTREES_ROOT_KEY,
                            );
                            let name = board(
                                name_key.as_deref(),
                                crate::engine::DEFAULT_WORKSPACE_NAME_KEY,
                            );
                            let (free, reason) =
                                resolve_workspace_target_free(db, run, &root, &name)?;
                            Ok(vec![
                                (write.clone(), Value::from(free)),
                                (format!("{write}_reason"), Value::from(reason)),
                            ])
                        } else {
                            Ok(vec![(write.clone(), resolve_query(db, run, &query)?)])
                        }
                    })();
                    let folded = match resolved {
                        Ok(folded) => folded,
                        Err(e) => {
                            return Err(block_on_effect_error(
                                db,
                                run,
                                engine,
                                &node_id,
                                last_result,
                                e,
                            ))
                        }
                    };
                    for (key, value) in folded {
                        // ATTRIBUTED to the `query` node: this is a FACT nyx read, not an answer a
                        // human gave, and the board must say so — a consent gate that fires on this
                        // key is gating on machinery, whatever the graph's shape suggests.
                        engine.set_board_as(&node_id, &key, value);
                    }
                    // H3 — a fact this run can never change is a CONSTANT, and a loop gated on it can
                    // never exit. Say so on the board NOW, so the engine kills such a loop on its
                    // FIRST round instead of the twentieth. Observed live: a `prototyper` child on a
                    // Spec-less parent looped `until design_locked == true` — hard `false` for an
                    // unbound run — through 20 rounds and 22 sub-agents before the backstop caught it.
                    if spec_binding_is_settled_absent(run) {
                        engine.freeze_key(&write, UNBOUND_SPEC_FACT_FROZEN);
                    }
                    result = Some(NodeResult::Ack);
                }
                // nyx MARKS the task through the guarded task lifecycle (start/done/block/skip) — a
                // nyx-own inline effect, the task-level twin of the Spec `transition` arm. UNGATED by
                // default; when `gate_on` is set the effect fires only on an affirmative board key. A
                // REFUSAL (illegal source, or the human-guard `done` on a kind=human task) is
                // RE-ATTEMPTED on the next call (replay + BLOCKED), never skipped. Never surfaced.
                Action::TransitionTask {
                    node_id,
                    task_id,
                    transition,
                    reason,
                    gate_on,
                } => {
                    let gate_satisfied = gate_on
                        .as_deref()
                        .map(|k| board_value_is_affirmative(engine.blackboard().get(k)))
                        .unwrap_or(true);
                    if gate_satisfied && transition == TASK_TRANSITION_VERIFY {
                        // The VERIFIED close of a `kind=human` task — a SEPARATE authority
                        // ([`db::verify_human_task`]), not a `TaskTransition`: the plain `done` is
                        // refused for a human action, on purpose, and this is the path that refusal
                        // names. Its own refusals (not human / not verifiable / not started) are
                        // replayed + BLOCKED like any other, so nothing is skipped silently.
                        if let Err(e) = db.with_conn(|c| db::verify_human_task(c, &task_id)) {
                            engine.replay_current();
                            let trace = tick_trace(None, last_result);
                            let _ = persist(
                                db,
                                run,
                                engine,
                                Some(&node_id),
                                db::RUN_STATUS_BLOCKED,
                                &trace,
                            );
                            return Err(crate::mcp_tools_spec::map_task_transition_err(e));
                        }
                    } else if gate_satisfied {
                        let tt = match parse_task_transition(&transition) {
                            Some(tt) => tt,
                            None => {
                                // An unknown transition verb (authoring rejects it, but a hand-written
                                // config could carry one): refuse loudly rather than silently ack.
                                engine.replay_current();
                                let trace = tick_trace(None, last_result);
                                let _ = persist(
                                    db,
                                    run,
                                    engine,
                                    Some(&node_id),
                                    db::RUN_STATUS_BLOCKED,
                                    &trace,
                                );
                                return Err(RpcError::new(
                                    RpcCode::InvalidArgument,
                                    format!(
                                        "transition_task '{transition}' is not a task transition \
                                         (start/done/block/skip/verify)"
                                    ),
                                ));
                            }
                        };
                        if let Err(e) =
                            db.with_conn(|c| db::transition_task(c, &task_id, tt, reason.as_deref()))
                        {
                            engine.replay_current();
                            let trace = tick_trace(None, last_result);
                            let _ = persist(
                                db,
                                run,
                                engine,
                                Some(&node_id),
                                db::RUN_STATUS_BLOCKED,
                                &trace,
                            );
                            return Err(crate::mcp_tools_spec::map_task_transition_err(e));
                        }
                    }
                    result = Some(NodeResult::Ack);
                }
                // nyx FORKS the bound Spec into a new editable draft revision — but ONLY when the
                // preceding ask_human approved (N13). A declined fork (or an unbound run) is a no-op:
                // the graph continues either way. Never surfaced to the dispatcher.
                Action::ForkSpec { node_id, gate_on } => {
                    let approved = gate_on
                        .as_deref()
                        .map(|k| board_value_is_affirmative(engine.blackboard().get(k)))
                        .unwrap_or(false);
                    // The fork is a member of the sensitive census, so the affirmative that opens it
                    // must be a HUMAN's — see [`crossing_is_authorized`]. Asked INSIDE the Spec
                    // binding, like the fork itself: an unbound run forks nothing, so there is
                    // nothing to demand a human for.
                    if approved && run.spec_id.is_some() {
                        if let Err(e) = crossing_is_authorized(
                            engine,
                            &node_id,
                            crate::engine::SensitiveEffect::CreateRevision,
                            notes,
                        ) {
                            return Err(block_on_effect_error(
                                db,
                                run,
                                engine,
                                &node_id,
                                last_result,
                                e,
                            ));
                        }
                    }
                    if approved {
                        if let Some(spec_id) = run.spec_id.as_deref() {
                            // A fork the authority refused must be RE-ATTEMPTED, never walked past:
                            // the human approved a new draft revision and the flow below this node
                            // is authored to edit one (see [`block_on_effect_error`]).
                            if let Err(e) = db.with_conn(|c| db::revise_spec(c, spec_id)) {
                                return Err(block_on_effect_error(
                                    db,
                                    run,
                                    engine,
                                    &node_id,
                                    last_result,
                                    RpcError::new(
                                        RpcCode::Internal,
                                        format!("fork_spec: {e}"),
                                    ),
                                ));
                            }
                        }
                    }
                    result = Some(NodeResult::Ack);
                }
                // nyx OPENS THE DEV WORKSPACE of the run's bound Spec — folder, workspace row and Spec
                // ANCHOR in one guarded write, through the SAME `mcp_runtime::create_workspace` the
                // MCP tool calls. UNGATED by default; with `gate_on` set it fires only on an
                // affirmative board key, which is how the human answer that ASKED for a workspace is
                // the thing that opens one. Never surfaced to the dispatcher.
                //
                // A REFUSAL (no Spec, no path, a folder that is a file, a duplicate path) BLOCKS and
                // is re-attempted on the next call rather than walked past: the flow below is
                // authored on the workspace existing, and a silent skip would leave the human
                // believing they got one.
                Action::CreateWorkspace {
                    node_id,
                    root,
                    name,
                    gate_on,
                } => {
                    let approved = gate_on
                        .as_deref()
                        .map(|k| board_value_is_affirmative(engine.blackboard().get(k)))
                        .unwrap_or(true);
                    if approved {
                        // The MOST PHYSICAL of the three sensitive effects, and the one whose seeded
                        // gate reads a `query` fact: the human's consent for `/nspec`'s `go` arm is
                        // the selector above, which is exactly what `consent_keys` climbs to.
                        if let Err(e) = crossing_is_authorized(
                            engine,
                            &node_id,
                            crate::engine::SensitiveEffect::CreateWorkspace,
                            notes,
                        ) {
                            return Err(block_on_effect_error(
                                db,
                                run,
                                engine,
                                &node_id,
                                last_result,
                                e,
                            ));
                        }
                        if let Err(e) = create_run_workspace(db, run, &root, &name) {
                            let blocked =
                                block_on_effect_error(db, run, engine, &node_id, last_result, e);
                            return Err(blocked);
                        }
                        // THE WORKSPACE POOL CHANGED — say so, or the row exists and nobody knows.
                        // This is the ONE inline effect whose write lands OUTSIDE the Spec domain
                        // the tick names by default, and the sidebar is the only view of it. See
                        // [`outcome`] for the incident this line closes. Only on the APPROVED,
                        // SUCCEEDED path: a gated-off node created nothing, and a refused one
                        // compensated its own folder.
                        note_effect(effects, ChangedTopic::Workspaces);
                    }
                    result = Some(NodeResult::Ack);
                }
                // nyx CLASSIFIES the run's bound Spec — the genre of work it carries (PDR-0017),
                // written through the SAME [`crate::category::set_spec_category_by_label`] the
                // `spec_set_category` tool calls. THE SERVICE AND NEVER A SECOND IMPLEMENTATION: the
                // same-project refusal, the slug-first resolution, the on-the-fly MINT of an unknown
                // label and the `spec.category_set` event all live there, and a node that
                // re-derived any of them would be a second authority over `specs.category_id` — the
                // one thing that module exists to prevent. In particular the CROSS-PROJECT refusal is
                // PRONOUNCED BY THE SERVICE (`CategoryError::ProjectMismatch`), never re-checked here.
                //
                // UNGATED by default; with `gate_on` set it fires only on an affirmative board key.
                // Never surfaced to the dispatcher.
                //
                // A RUN WITH NO SPEC COMMITS NOTHING AND IS NOT PARKED — the same arbitrage as the
                // `transition` arm (M2, `apply_transition`'s early return): there is no Spec to
                // classify, so the node commits nothing, and blocking on it would park a run over an
                // effect that never happens. A `/nspec` run reaches this node before its Spec exists;
                // that is a legitimate state, not a failure. A key NOTHING EVER WROTE reads the same
                // way: the node names a board slot that does not exist, so there is nothing to
                // classify WITH, and the graph walks on.
                //
                // WHAT IS NOT THE SAME IS A KEY THAT HOLDS SOMETHING UNUSABLE, and that distinction
                // is the whole of [`refuse_unusable_category`]: only a NON-EMPTY STRING is a label.
                // `Value::Null` in particular is what a `judge` leaves on the board when the
                // dispatcher resumes a parked run WITHOUT a `last_result` — a stated principle of
                // this engine, not an edge case — and the label travels here through
                // `{{key}}` INTERPOLATION, which renders that null as the four letters `null`. A
                // non-empty string is exactly what the category service MINTS on, so the run used to
                // come out with a category named `null` in the project's vocabulary, the Spec
                // classified under it, and the `spec_category` fact answering a truthy slug —
                // permanently exempting that Spec from the very discipline the preflight exists to
                // impose. Reading the RAW board value instead of the rendered string is what tells
                // "the judge answered nothing" apart from "the judge answered a word".
                //
                // A REFUSAL the service DOES pronounce (an ambiguous name, a blank label, a DB error)
                // BLOCKS and is re-attempted on the next call rather than walked past: the discipline
                // below this node is authored on the classification having landed, and a silent skip
                // would run a `bug` flow over a Spec nyx never classified.
                //
                // The tick's default `ChangedTopic::Specs` already covers this write — unlike
                // `create_workspace`, it lands INSIDE the Spec domain (see [`outcome`]).
                Action::SetCategory {
                    node_id,
                    key,
                    category: _,
                    gate_on,
                } => {
                    let approved = gate_on
                        .as_deref()
                        .map(|k| board_value_is_affirmative(engine.blackboard().get(k)))
                        .unwrap_or(true);
                    if approved {
                        if let Some(spec_id) = run.spec_id.as_deref() {
                            let read = board_category_label(engine.blackboard().get(&key));
                            match read {
                                BoardLabel::Absent => {}
                                BoardLabel::Unusable(seen) => {
                                    let e = refuse_unusable_category(&key, &seen);
                                    return Err(block_on_effect_error(
                                        db,
                                        run,
                                        engine,
                                        &node_id,
                                        last_result,
                                        e,
                                    ));
                                }
                                BoardLabel::Label(label) => {
                                    if let Err(e) = db
                                        .with_conn(|c| {
                                            crate::category::set_spec_category_by_label(
                                                c,
                                                spec_id,
                                                &label,
                                                db::DomainEventSource::Mcp,
                                            )
                                        })
                                        .map_err(crate::mcp_tools_spec::map_category_err)
                                    {
                                        return Err(block_on_effect_error(
                                            db,
                                            run,
                                            engine,
                                            &node_id,
                                            last_result,
                                            e,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    result = Some(NodeResult::Ack);
                }
                // HYBRID: open (or re-check) a CHILD run of another flow. This is the one arm that runs
                // a nyx-inline effect (create/read the child run) AND suspends the parent while a FRESH
                // sub-agent drives the child. The child result is read from the child run's VERIFIED
                // blackboard, never trusted from the sub-agent's claim (v2.8 lot 2).
                Action::SubWorkflow {
                    node_id,
                    scope,
                    workflow_id,
                    inject,
                    retrieve,
                    output_key,
                } => {
                    let child_key = subworkflow_child_key(&node_id);
                    match engine
                        .blackboard()
                        .get(&child_key)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                    {
                        // RESUME: a child run already exists — branch on its STATUS (the source of
                        // truth). Only `running` waits; done/blocked/failed return a SubResult the parent
                        // selector branches on (the parent is the level with human agency).
                        Some(child_run_id) => {
                            let child = db
                                .with_conn(|c| db::get_workflow_run(c, &child_run_id))
                                .map_err(internal_db)?;
                            match child {
                                // The child row vanished (deleted out from under the parent): surface it
                                // as a failure so the graph can branch, rather than waiting forever.
                                None => {
                                    result = Some(NodeResult::SubResult(
                                        json!({ "status": db::RUN_STATUS_FAILED }),
                                    ));
                                }
                                Some(child) if child.status == db::RUN_STATUS_DONE => {
                                    // SUCCESS: fold the `retrieve` keys from the child's VERIFIED
                                    // blackboard onto the parent board, AND summarize them in the result.
                                    let child_board = Blackboard::from_json_str(&child.blackboard);
                                    let mut retrieved = serde_json::Map::new();
                                    for copy in &retrieve {
                                        if let Some(v) = child_board.get(&copy.from) {
                                            // The widest write in the vocabulary, and it is attributed
                                            // to THIS `sub_workflow` node — NOT to whoever wrote the
                                            // key on the CHILD board. A human answered the child's
                                            // question, on the child's run, to the child's graph; what
                                            // lands here is a value a node copied. Carrying the
                                            // child's `ask_human` provenance across would let a parent
                                            // gate cross on a consent given to another question, which
                                            // is the H6 failure with one extra hop.
                                            engine.set_board_as(&node_id, &copy.to, v.clone());
                                            retrieved.insert(copy.to.clone(), v.clone());
                                        }
                                    }
                                    retrieved.insert(
                                        "status".to_string(),
                                        Value::from(db::RUN_STATUS_DONE),
                                    );
                                    result =
                                        Some(NodeResult::SubResult(Value::Object(retrieved)));
                                }
                                Some(child)
                                    if child.status == db::RUN_STATUS_BLOCKED
                                        || child.status == db::RUN_STATUS_FAILED =>
                                {
                                    // FAILURE: surface the child's terminal-ish status as the result so
                                    // the parent selector can retry / abandon / escalate — do NOT wait
                                    // (a blocked child has no human in ITS loop to unblock it).
                                    result = Some(NodeResult::SubResult(
                                        json!({ "status": child.status }),
                                    ));
                                }
                                // Still `running`: three shapes. (0) M1 — the child can no longer BE
                                // ADVANCED (its own graph moved under it), so it is DEAD whatever its
                                // row says: nothing will ever tick it to a terminal state, and waiting
                                // on it is waiting forever. A child that died is exactly the fact the
                                // parent graph is already built to test (H4), so it FOLDS as `failed`
                                // like the vanished row above and the parent selector decides (retry /
                                // abandon / escalate) — the level with human agency. (1) the child is
                                // PARKED on an `ask_human` — BUBBLE it UP through THIS sub_workflow node
                                // (Design A): the parent re-emits the child's question (its prompt
                                // interpolated against the CHILD board), and the human's answer routes
                                // to `nyx_next(child_run_id)`, where the child's own ask_human + N19
                                // machinery handle it unchanged. The parent cursor stays on this node.
                                // (2) not on an ask_human ⇒ the driving sub-agent may have DIED:
                                // RE-DISPATCH a fresh sub-agent with the SAME child run_id (idempotent —
                                // the child resumes on its own persisted traversal) and re-suspend.
                                Some(child) => {
                                    if let Err(why) = build_engine(db, &child) {
                                        notes.push(child_died_note(&child, &why));
                                        result = Some(NodeResult::SubResult(
                                            json!({ "status": db::RUN_STATUS_FAILED }),
                                        ));
                                    } else if let Some(ask) = child_ask_human_bubble(db, &child)? {
                                        // Parent cursor UNCHANGED — persist RUNNING on this node so the
                                        // next parent tick re-checks the child (bubbles again / folds).
                                        let ask = with_notes(ask, notes.as_slice());
                                        let trace = tick_trace(action_type(&ask), last_result);
                                        persist(
                                            db,
                                            run,
                                            engine,
                                            Some(&node_id),
                                            db::RUN_STATUS_RUNNING,
                                            &trace,
                                        )?;
                                        return Ok(outcome(run, engine, ask, db::RUN_STATUS_RUNNING));
                                    } else {
                                        // The action is BUILT first: the journal records the `type` this
                                        // stop actually hands back, never a guess made before it exists.
                                        let json =
                                            with_notes(subworkflow_wait_json(&child_run_id), notes.as_slice());
                                        let trace = tick_trace(action_type(&json), last_result);
                                        persist(
                                            db,
                                            run,
                                            engine,
                                            Some(&node_id),
                                            db::RUN_STATUS_RUNNING,
                                            &trace,
                                        )?;
                                        return Ok(outcome(run, engine, json, db::RUN_STATUS_RUNNING));
                                    }
                                }
                            }
                            // The child reached a TERMINAL state (done / blocked / failed / vanished) and
                            // its result is now folded — CLEAR the per-node child key so a LOOP re-entry
                            // (ndev's inner audit-rework / outer phases loop) opens a FRESH child rather
                            // than resuming this finished one. The running arm returned early above, so a
                            // still-running child keeps its key (the sub-agent-death re-dispatch is
                            // unaffected) — this line is reached ONLY for a folded terminal child (v2.8
                            // lot 2, the looped sub_workflow).
                            engine.clear_board(&child_key);
                        }
                        // FIRST VISIT: resolve the child workflow, seed the ISOLATED child board from the
                        // parent's `inject` keys, open the child run bound to the parent's spec + workspace
                        // with `parent_run_id` set, record the child id on the parent board, and SUSPEND.
                        //
                        // M1 — the child is resolved through the SAME verified path `run_start` uses
                        // ([`start_or_replace_run`]): the N20 idempotency on (child workflow, spec) hands
                        // back the pair's LIVE run, which may be a corpse from before a graph change, and
                        // adopting one leaves the parent re-emitting this action forever. The replacement
                        // is announced on the notes riding the very action that carries the new child id —
                        // a session that had been driving the old child must learn that the one it is
                        // handed now starts from the top.
                        None => {
                            let child_wf =
                                resolve_child_workflow(db, run, workflow_id.as_deref(), &scope)?;
                            let mut child_board = Blackboard::new();
                            // INJECTED keys are attributed, on the CHILD board, to the PARENT's
                            // `sub_workflow` node — the node that put them there. The id names a node
                            // of the parent's graph, which is exactly what happened; what matters to
                            // the child's consent checks is the TYPE, and `sub_workflow` is not
                            // `ask_human`, so an injected value can never authorize a sensitive node
                            // down there however affirmative it looks.
                            let injected_by = crate::engine::NodeOrigin::new(
                                node_id.clone(),
                                NodeType::SubWorkflow,
                            );
                            for copy in &inject {
                                if let Some(v) = engine.blackboard().get(&copy.from) {
                                    child_board
                                        .written_by(&injected_by)
                                        .set(&copy.to, v.clone());
                                }
                            }
                            let (child_run, child_outcome) = start_or_replace_run(
                                db,
                                runtime,
                                &child_wf.id,
                                run.spec_id.as_deref(),
                                &child_board.to_json_string(),
                            )?;
                            // ONLY the REPLACEMENT is noted here. `run_start` also announces a
                            // RESUME, because there the caller is a human who believes they are
                            // starting a flow; a `sub_workflow` node is the graph opening its own
                            // child and its `Created`/`Resumed` are the SAME event from the
                            // parent's side — the child key was absent, so this node is handing the
                            // parent a child either way. A replacement is not: the child the parent
                            // gets starts from the top of ITS graph.
                            if let RunStartOutcome::Replaced(replaced) = child_outcome {
                                notes.push(child_replaced_note(&replaced, &child_run.id));
                            }
                            db.with_conn(|c| db::set_run_parent(c, &child_run.id, &run.id))
                                .map_err(internal_db)?;
                            if let Some(workspace_id) = run.workspace_id.as_deref() {
                                db.with_conn(|c| db::set_run_workspace(c, &child_run.id, workspace_id))
                                    .map_err(internal_db)?;
                            }
                            engine.set_board_as(
                                &node_id,
                                &child_key,
                                Value::from(child_run.id.clone()),
                            );
                            let _ = &output_key; // routing honored via write_result on SubResult.
                            let json = with_notes(subworkflow_wait_json(&child_run.id), notes.as_slice());
                            let trace = tick_trace(action_type(&json), last_result);
                            persist(
                                db,
                                run,
                                engine,
                                Some(&node_id),
                                db::RUN_STATUS_RUNNING,
                                &trace,
                            )?;
                            return Ok(outcome(run, engine, json, db::RUN_STATUS_RUNNING));
                        }
                    }
                }
                // A note SPEAKS, it does not ask: it is COLLECTED and the walk goes on (G4). What it
                // still EXECUTES happens right here, unchanged — only the suspension is gone.
                Action::Note {
                    text,
                    clear_gate_block,
                    revoke_spec_write,
                    ..
                } => {
                    // M5 — a `note { clear_gate_block: true }` reached in the graph is the
                    // graph-expressible ABANDON: CLEAR the run's red-gate anchor (every anchored blocking
                    // gate) so the downstream `done` no longer refuses on the stale block. ONLY an
                    // EXPLICIT route through such a node clears it — nothing implicit does, so the anchor
                    // + H1 multi-gate backstop is otherwise untouched.
                    if clear_gate_block {
                        blocking_gates_red.clear();
                        mirror_blocking_gates(engine, &blocking_gates_red);
                    }
                    // H12 — and a `note { revoke_spec_write: true }` CLOSES the Spec-write window the
                    // human opened at the escalation gate. The grant is dropped as the walk goes past,
                    // so the authorization lasts exactly the branch it was given for.
                    if revoke_spec_write {
                        engine.clear_board(crate::engine::SPEC_WRITE_GRANT_KEY);
                    }
                    notes.push(text);
                    // ACK the leaf so the cursor advances — the note is walked PAST, exactly as before.
                    result = Some(NodeResult::Ack);
                }
                // An `ask_human` is a dispatcher-facing leaf like any other. When its answer is what
                // lets an engaging Spec transition fire (H6), it travels the SAME way — the agent
                // relays the question, the human answers in the chat, the agent relays that back —
                // but nyx RECORDS the asking first: the instant, and the asking session's human-turn
                // count, which is what the tick receiving the answer will check it against.
                Action::AskHuman {
                    node_id,
                    prompt,
                    options,
                } => {
                    // J1 — a question a BRANCH answers must declare that branch's answers. Refused
                    // BEFORE it is asked, like the vacuous gate: an unconstrained answer is not a
                    // verdict the graph can honour, it is the absence of one.
                    if let Some(reader) = engine.tree().ask_human_unconstrained_branch(&node_id) {
                        return Err(refuse_unconstrained_ask(
                            db,
                            run,
                            engine,
                            &node_id,
                            &reader,
                            last_result,
                        ));
                    }
                    let commitment = engine.tree().ask_human_commitment(&node_id);
                    if let Some(commitment) = commitment {
                        // K1 — a question whose answer must be checked against a HUMAN TURN can only
                        // be put to a session nyx can name: the ledger is keyed by terminal, so a
                        // caller carrying none has no ledger to read and every answer it relays is
                        // refused. Refuse HERE, where the question would be written, rather than
                        // asking something whose every answer is already doomed.
                        let Some(terminal_id) = caller.terminal_id else {
                            return Err(refuse_unattributable_consent(
                                db,
                                run,
                                engine,
                                &node_id,
                                &prompt,
                                commitment,
                                last_result,
                            ));
                        };
                        // NAME the human turn that stands at the instant of the asking — the answer
                        // must come after a DIFFERENT one. `None` (nyx knows of no turn on this
                        // terminal yet) is a legitimate reference: the first turn it does learn of
                        // will differ from it.
                        let at_ask = db
                            .with_conn(|c| db::last_human_turn(c, terminal_id))
                            .map_err(internal_db)?
                            .map(|turn| turn.turn_id);
                        db.with_conn(|c| {
                            db::open_run_consent(
                                c,
                                &run.id,
                                &node_id,
                                &prompt,
                                &options,
                                terminal_id,
                                at_ask.as_deref(),
                            )
                        })
                        .map_err(internal_db)?;
                    }
                    let json =
                        with_notes(ask_human_json(&prompt, &options, None, commitment), notes.as_slice());
                    let trace = tick_trace(action_type(&json), last_result);
                    persist(db, run, engine, Some(&node_id), db::RUN_STATUS_RUNNING, &trace)?;
                    return Ok(outcome(run, engine, json, db::RUN_STATUS_RUNNING));
                }
                // A dispatcher-facing leaf: persist the cursor + blackboard and hand it back.
                other => {
                    let node_id = other.node_id().to_string();
                    let json = with_notes(action_json(&other, provider), notes.as_slice());
                    let trace = tick_trace(action_type(&json), last_result);
                    persist(db, run, engine, Some(&node_id), db::RUN_STATUS_RUNNING, &trace)?;
                    return Ok(outcome(run, engine, json, db::RUN_STATUS_RUNNING));
                }
            },
        }
    }
}

/// A nyx-own INLINE EFFECT failed — a gate command that cannot be resolved or spawned, a fact nyx
/// cannot read, a Spec fork the authority refused. Park the run `blocked` ON that node and mark the
/// leaf for REPLAY, then hand the error back unchanged.
///
/// THE REPLAY IS THE WHOLE POINT, and without it the failure was worse than the error it reported.
/// An inline leaf carries no dispatcher payload, so the next tick decodes its `last_result` as an
/// `Ack` ([`decode_result`]'s catch-all), folds it, and the walk advances PAST the leaf — the effect
/// never ran and nothing says so. For a BLOCKING `gate_check` that is a gate skipped in silence: a
/// command whose workspace subfolder no longer exists fails `resolve_command_and_cwd`, the tick
/// errors once, and the caller's very next `nyx_next` walks past the check and on to the `done` it
/// was there to guard. Same shape for a `query` (the branch then reads the `null` nobody wrote) and
/// for a `fork_spec`. Every OTHER refusing arm already replays ([`refuse_gate`],
/// [`refuse_vacuous_gate`], the two `transition` arms); these were the ones that did not.
fn block_on_effect_error(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    node_id: &str,
    last_result: Option<&Value>,
    e: RpcError,
) -> RpcError {
    engine.replay_current();
    let trace = tick_trace(None, last_result);
    let _ = persist(db, run, engine, Some(node_id), db::RUN_STATUS_BLOCKED, &trace);
    e
}

/// ATTACH the notes the walk went past to the action the tick hands back, under `notes` (G4).
///
/// WHY. A `note` asks the dispatcher NOTHING — `/nspec` opens on "No Spec yet — author a NEW one from
/// scratch.", `/ndev` on "The Spec is not 'ready' — skipping the start." — yet suspending on one cost
/// a full round-trip: one MCP call, one response, one conversation turn, for a sentence the dispatcher
/// could only read and re-tick past. So a note no longer STOPS anything: it accumulates and TRAVELS
/// with the next real action, alongside what the dispatcher must actually perform. Nothing is lost —
/// a note that does ask something of the dispatcher still reaches it — and the round-trip is gone.
///
/// An ARRAY, because one tick can walk past several notes; ABSENT when there are none, so every
/// existing action payload stays byte-identical.
fn with_notes(mut action: Value, notes: &[String]) -> Value {
    if notes.is_empty() {
        return action;
    }
    if let Some(obj) = action.as_object_mut() {
        obj.insert(
            "notes".to_string(),
            Value::Array(notes.iter().map(|n| Value::from(n.as_str())).collect()),
        );
    }
    action
}

/// The PARENT-board key a `sub_workflow` node records its CHILD run id under — keyed PER NODE so a
/// tree may hold several sub_workflow leaves. Deliberately dot-free (a UUID node id carries no `.`),
/// so [`Blackboard::get`]'s dot-path split reads it as one flat key (v2.8 lot 2).
///
/// `pub(crate)` because rule (4)'s single-writer condition must census it: it is a write the engine
/// performs that the node's config does not name ([`crate::mcp_authoring`]'s
/// `node_written_board_keys`).
pub(crate) fn subworkflow_child_key(node_id: &str) -> String {
    format!("__child_run_id__{node_id}")
}

/// The dispatcher-facing WAIT action a `sub_workflow` leaf hands back: it carries the CHILD `run_id`
/// plus a STATIC generic-dispatcher `context` telling the session to drive that child to done. The
/// same session then loops `nyx_next(child_run_id)`; the PARENT resumes (re-checking the child status)
/// on the next `nyx_next(parent_run_id)` (v2.8 lot 2). The parent node it suspends on is the tick's
/// `current_node_id`, so the action does not repeat it (G5).
fn subworkflow_wait_json(child_run_id: &str) -> Value {
    json!({
        "type": "sub_workflow",
        "child_run_id": child_run_id,
        "context": format!(
            "You are a DISPATCHER for a sub-workflow. Call nyx_next(run_id=\"{child_run_id}\") in a \
             loop, performing exactly the action it returns each tick, until it returns \
             {{\"type\":\"done\"}} — then report back. Do nothing else."
        ),
    })
}

/// M1 — the NOTE a `sub_workflow` node rides when the child it resolved was a CORPSE and
/// [`start_or_replace_run`] opened a fresh one in its place. `fresh_id` is the child the action itself
/// carries.
///
/// WHY A NOTE. `run_start` says the same thing in its `warnings`, but a `sub_workflow` node hands back
/// an ACTION, and the notes are the channel that rides one (G4) — the dispatcher is told to read them
/// and act on what they ask (`mcp_context`'s dispatcher manual). Nothing else on this path reaches the
/// session, and a silent swap here is its own trap: a session that had been driving a child would
/// suddenly be driving another one from the top with no way to know.
fn child_replaced_note(replaced: &ReplacedRun, fresh_id: &str) -> String {
    format!(
        "The child run this step resolved ({}) was DROPPED, not resumed: {}, so nothing of its \
         progress could be picked up. The child_run_id in this action ({fresh_id}) is a FRESH child \
         starting that flow from the top — SAY SO to the human before you drive it",
        replaced.dropped.id,
        run_death_clause(&replaced.dropped, &replaced.why),
    )
}

/// M1 — the NOTE a `sub_workflow` node rides when the child it was WAITING ON can no longer be
/// advanced: it is folded as `failed` rather than waited on forever, and the parent graph's own
/// "child result?" selector takes it from there (H4). Same channel and same sentence-source as
/// [`child_replaced_note`] — one death, said one way.
fn child_died_note(child: &db::WorkflowRun, why: &RpcError) -> String {
    format!(
        "The child run this step was waiting on ({}) can no longer be advanced: {}. It is reported \
         FAILED rather than waited on forever — SAY SO to the human, whatever the flow decides next",
        child.id,
        run_death_clause(child, why),
    )
}

/// BUBBLE-UP (Design A): if the still-running CHILD run is PARKED on an `ask_human` leaf, build the
/// dispatcher-facing `ask_human` action that re-emits it THROUGH the parent's sub_workflow node,
/// keeping `child_node_id` — which G5's drop of `node_id` SPARES, since the tick's `current_node_id`
/// is the PARENT's node and this is the only thing saying where the CHILD is parked. The prompt is re-interpolated against the CHILD board (the question moves with
/// the child's state, like any looped ask), and the action carries the THREAD — `child_run_id` +
/// `child_node_id` + `via:"sub_workflow"` — so the top-level routes the human's answer to
/// `nyx_next(child_run_id, {decision})`, where the child's own ask_human resume + N19 handle it
/// unchanged. Returns `None` when the child is NOT on an ask_human (a genuine sub-agent death — the
/// caller re-dispatches) (v2.8 lot 2).
///
/// A child parked on a CONSENT question bubbles up carrying that fact (H6) — the bubble is where the
/// top-level session LEARNS the question, so it is where it must also learn that answering it takes a
/// real human turn. The check itself stays on the child's own tick, which is where the answer lands.
fn child_ask_human_bubble(db: &Db, child: &db::WorkflowRun) -> Result<Option<Value>, RpcError> {
    let Some(cursor) = child.current_node_id.as_deref() else {
        return Ok(None);
    };
    let Some(node) = db.with_conn(|c| db::get_node(c, cursor)).map_err(internal_db)? else {
        return Ok(None);
    };
    if node.type_ != db::NODE_TYPE_ASK_HUMAN {
        return Ok(None);
    }
    let cfg: crate::engine::AskHumanConfig = serde_json::from_str(&node.config).unwrap_or_default();
    let child_board = Blackboard::from_json_str(&child.blackboard);
    let prompt = crate::engine::interpolate(&cfg.prompt, &child_board);
    let commitment = load_tree(db, child)?.ask_human_commitment(cursor);
    Ok(Some(ask_human_json(
        &prompt,
        &cfg.options,
        Some((&child.id, cursor)),
        commitment,
    )))
}

/// WHO actually authorized a SENSITIVE node the run is about to cross — read from the board's
/// PROVENANCE ([`crate::engine::Blackboard::origin_of`]), which is a record of the run that happened
/// and not an analysis of the graph that might.
///
/// This is the answer the six static guards each approximate. They ask "does this graph LOOK like a
/// human decides here?" before anything runs; a live run holds the value AND the name of whoever wrote
/// it. The keys consulted are the ones the graph puts between the run and the effect
/// ([`crate::engine::Tree::consent_keys`]) — the node's own `gate_on`, and the unanimous routing key of
/// each `selector` above it — and only those currently holding an AFFIRMATIVE, since a key saying "no"
/// authorizes nothing whoever wrote it.
enum ConsentOrigin {
    /// An `ask_human` wrote the value that opens this node. The one verdict that crosses.
    Human,
    /// Every key that could open it has a KNOWN author and NONE of them is a question put to a human:
    /// a `query`'s fact, a `judge`'s verdict, a `spawn_subagent`'s report. Carries the nearest such
    /// key and its author, because a refusal that names neither cannot be acted on.
    NotHuman {
        key: String,
        origin: crate::engine::NodeOrigin,
    },
    /// The board says nothing about who wrote what could open this node — a run already in flight when
    /// provenance shipped, a key filled through a path that could not name its author, or a node
    /// nothing gates at all. NOT a refusal: see [`crossing_is_authorized`].
    Unrecorded,
}

/// The runtime consent verdict for the sensitive node `node_id`, given the live board.
///
/// The ORDER is [`crate::engine::Tree::consent_keys`]'s: the node's own gate is asked first, the
/// selectors above it next. A single human answer anywhere on that list is enough — the `/nspec` `go`
/// arm gates its `create_workspace` on the `workspace_free` PREVALIDATION (a `query` fact) and puts the
/// human's consent in the selector one level up, so a check that stopped at the gate would refuse the
/// shipped flow.
fn consent_origin(engine: &Engine, node_id: &str) -> ConsentOrigin {
    let board = engine.blackboard();
    let mut known: Option<(String, crate::engine::NodeOrigin)> = None;
    for key in engine.tree().consent_keys(node_id) {
        if !board_value_is_affirmative(board.get(&key)) {
            continue;
        }
        match board.origin_of(&key) {
            Some(origin) if origin.is_human_answer() => return ConsentOrigin::Human,
            Some(origin) => {
                if known.is_none() {
                    known = Some((key, origin));
                }
            }
            None => {}
        }
    }
    match known {
        Some((key, origin)) => ConsentOrigin::NotHuman { key, origin },
        None => ConsentOrigin::Unrecorded,
    }
}

/// Whether a SENSITIVE node whose static gate is already satisfied may actually be CROSSED — `Ok(())`
/// to cross, `Err` to refuse. Consulted ONLY where the node was about to fire anyway, so it can turn a
/// crossing into a refusal and never the reverse: nothing here can make a run advance further than it
/// would have without it.
///
/// # The three verdicts, and why the third is not a refusal
///
///   * a HUMAN's answer authorizes it — cross, exactly as before.
///   * a KNOWN, non-human author — REFUSE, naming the key, what actually wrote it, and what was
///     expected. This is the class the six static guards each closed one member of: a `query`'s fact,
///     a `judge`'s verdict or a `spawn_subagent`'s report standing in for a human's decision. The
///     graph does not have to be re-authored for this to hold — the check reads the run, so a graph
///     written before any of this gains it untouched.
///   * NOTHING RECORDED — cross, and WARN. A run already in flight when provenance shipped carries a
///     board without it, and so does a node nothing gates. Refusing there would freeze every live run
///     at deployment, for a guarantee that fills itself in as runs renew; the warning is what keeps
///     the degradation visible instead of silent. The static guards are still standing underneath,
///     which is what makes this a safe place to be generous.
fn crossing_is_authorized(
    engine: &Engine,
    node_id: &str,
    effect: crate::engine::SensitiveEffect,
    notes: &mut Vec<String>,
) -> Result<(), RpcError> {
    match consent_origin(engine, node_id) {
        ConsentOrigin::Human => Ok(()),
        ConsentOrigin::Unrecorded => {
            notes.push(format!(
                "nyx cannot tell WHO authorized this step ({}): its blackboard holds no record of \
                 which node wrote the key it fires on, so it is being allowed on the graph's shape \
                 alone, as before. A run opened before nyx began recording that carries no such \
                 record — the next run of this flow will. Tell the human, and check that one of them \
                 really approved it.",
                crate::engine::HumanCommitment::from(effect).what_it_commits(),
            ));
            Ok(())
        }
        ConsentOrigin::NotHuman { key, origin } => Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "this step {}, and nyx refuses it: the value that opens it — blackboard key '{}' — \
                 was written by the '{}' node '{}', which is a fact nyx read and not an answer a \
                 human gave. Only an 'ask_human' answer opens a step like this one. nyx knows it \
                 because it records WHO writes each blackboard key as the run goes, so the shape of \
                 the graph cannot stand in for what actually happened. What makes progress is a \
                 question put to the human whose answer lands on '{}' — not a retry, and not another \
                 value under the same key.",
                crate::engine::HumanCommitment::from(effect).what_it_commits(),
                key,
                origin.node_type.as_db(),
                origin.node_id,
                key,
            ),
        )
        .with_data(json!({
            "reason": "sensitive_step_without_human_provenance",
            "node_id": node_id,
            "gate_key": key,
            "written_by_node": origin.node_id,
            "written_by_type": origin.node_type.as_db(),
            "expected": db::NODE_TYPE_ASK_HUMAN,
        }))),
    }
}

/// WHAT the leaf the run is PARKED on commits, when it is an `ask_human` whose answer disposes of
/// something the session does not own (H6, then J2). DERIVED from the LIVE tree
/// ([`crate::engine::Tree::ask_human_commitment`]), so it follows an edited graph.
///
/// STILL STATIC, and deliberately so: it answers "what do I WARN the human about, and whose turn do I
/// demand, BEFORE this question is answered?", and at that instant no value exists to have a
/// provenance. The provenance decides the OTHER question — "may this effect fire, given what was
/// actually written?" ([`crossing_is_authorized`]) — one step later, where the value exists.
fn awaiting_commitment(engine: &Engine) -> Option<crate::engine::HumanCommitment> {
    if engine.awaiting_node_type() != Some(NodeType::AskHuman) {
        return None;
    }
    engine
        .current_node_id()
        .and_then(|id| engine.tree().ask_human_commitment(id))
}

/// H6 (then J2) — ACCEPT (or refuse) a relayed answer to a question whose answer COMMITS something
/// the session does not own: the Spec's lifecycle, or another round of build spend.
///
/// # What is actually being checked, and why it is this
///
/// The live failure was not an agent inventing an answer from nothing. The human had said, ONE TURN
/// EARLIER, "when nyx asks me to finalise, I want it moved to ready". nyx asked; the session answered
/// `yes` on their behalf — and reported that it had. Every field of that call was right. The one
/// thing that was missing is that NO HUMAN TURN happened between the question and the answer.
///
/// So that is what is verified, and nothing else: the asking NAMED the last human turn nyx knew of on
/// the asking terminal ([`db::PendingConsent::human_turn_at_ask`], from the `UserPromptSubmit` hook —
/// a signal that fires when the person types and that no tool call can produce), and the answer is
/// accepted only if the last known turn is a DIFFERENT one. Two identities, not two numbers: both
/// sides are now rows that outlive the session, which is what the counter this replaces could not do
/// — a `claude -p` invocation is a whole session, so a per-session counter compared against a
/// persisted receipt was comparing two lifetimes, refusing authentic answers and, the other way
/// round, able to accept one nobody gave.
///
/// # What this signal is, exactly — and the limit that follows
///
/// `UserPromptSubmit` means "a prompt entered the session from OUTSIDE the model". It is the best
/// available proxy for "a person spoke", and it is not a proof of one: nyx cannot tell a human typing
/// `claude -p "yes"` from a cron doing the same thing, and it does not pretend to. What it does close
/// is the observed failure — the session answering out of its own context, with no external input at
/// all — and that is the whole claim being made here.
///
/// # The residual weakness, stated rather than solved
///
/// A session could ask the human something UNRELATED, take their reply as cover, and relay an answer
/// they never gave. That is far-fetched next to the observed failure — a good-faith session applying
/// an earlier instruction — and the only way to close it would be to match the answer against the
/// human's raw text, which breaks the moment they type "go ahead" instead of "yes". So the hole is
/// left open, deliberately, and the raw prompt is JOURNALED instead: the check is cheap and never
/// wrong about the ordinary case, and the record makes any dispute settleable after the fact.
///
/// # ONE TURN, ONE COMMITMENT
///
/// Naming the turn is not enough on its own, because until this nothing SPENT it. Two runs parked on
/// a commitment question asked from the SAME terminal — two `/ndev` on two Specs, which is the
/// ordinary way to work — each named the turn standing at their own asking; the human typed ONCE; both
/// then read a last turn different from theirs, and both accepted. One sentence carried two Spec
/// commitments, the second of them precisely the failure above: an answer nobody gave to THAT
/// question. So accepting MARKS the turn it spent, and a turn already spent authorises nothing more.
///
/// # Where the verdict is taken, and why not here
///
/// The check reads the ledger and the acceptance writes to it — one fact, so one transaction
/// ([`db::close_run_consent`]). Read here and written there, two runs would both read "unspent" and
/// both write, which is the same defect through a smaller window. What stays here is the WORDING: the
/// four outcomes are four different situations, and each gets the sentence that is true of it.
///
/// On acceptance the question is closed and journaled with the raw prompt that paid for it; on refusal
/// NOTHING is written and the run stays parked exactly where it is, still asking. `commitment` is only
/// carried so the refusal can NAME what the answer would have disposed of.
fn accept_consent_answer(
    db: &Db,
    run: &db::WorkflowRun,
    _caller: CallerSession<'_>,
    answer: &Value,
    commitment: crate::engine::HumanCommitment,
) -> Result<(), RpcError> {
    // The receipt as the tick holds it — read for the REFUSAL's wording only (the question and the
    // options it was put with). The verdict re-reads it inside its own transaction.
    let pending = db::PendingConsent::of(run);
    let closed = db
        .with_conn(|c| db::close_run_consent(c, &run.id, answer.as_str().unwrap_or_default()))
        .map_err(internal_db)?;
    match closed {
        db::ConsentClose::Accepted(_) => Ok(()),
        db::ConsentClose::TurnAlreadySpent { turn, on_run } => Err(
            consent_turn_already_spent_error(run, pending.as_ref(), &turn, &on_run, commitment),
        ),
        db::ConsentClose::NoTurnSince(last) => Err(consent_without_human_turn_error(
            run,
            pending.as_ref(),
            last.as_ref(),
            commitment,
        )),
        // No receipt = nyx has no record of having asked this question (a run resumed after a
        // restart, a graph edited under it). It cannot then tell whether a human answered, and
        // "cannot tell" is refused: the next tick re-asks and re-opens the receipt, one round-trip.
        db::ConsentClose::NoReceipt => {
            Err(consent_without_human_turn_error(run, None, None, commitment))
        }
    }
}

/// The REFUSAL a session gets for an answer no human turn stands behind (H6, then J2).
///
/// It has one job beyond stopping the move: making a WELL-MEANING session understand what to do. The
/// session this exists to catch was helping — it had been told the answer in advance and passed it
/// on — so a refusal that reads like "wrong field" or "wrong value" sends it looking for a different
/// key and retrying forever. The wording therefore says the call was well-formed, says the missing
/// thing is the human's turn, and says the sequence that produces one.
///
/// AND IT SAYS WHAT IS ACTUALLY THE CASE. The three situations are genuinely different, and one
/// sentence for all three lied to the very person it was talking to: telling someone who had just
/// taken a turn that nyx had "seen no human turn" reads as a bug, and a session told a falsehood
/// about the state of the world cannot reason its way out of it. So the middle clause is built from
/// what nyx actually holds — no turn at all on that terminal / the same turn as at the asking / no
/// record of the asking — and nothing else in the message changes. A FOURTH situation, a turn that
/// really did happen and was already spent on another commitment, is the opposite of all three and
/// has its own refusal ([`consent_turn_already_spent_error`]).
///
/// It also NAMES what the answer commits ([`crate::engine::HumanCommitment::what_it_commits`]),
/// because "you may not answer this" without "this buys another sub-agent round" reads as an
/// arbitrary wall — and the session's next move should be to put the cost to the human, not to
/// look for a way round the check.
fn consent_without_human_turn_error(
    run: &db::WorkflowRun,
    pending: Option<&db::PendingConsent>,
    last_turn: Option<&db::HumanTurn>,
    commitment: crate::engine::HumanCommitment,
) -> RpcError {
    let prompt = pending.map(|p| p.prompt.clone()).unwrap_or_default();
    let state = match (pending, last_turn) {
        // The receipt is gone (a graph edited under a parked run, a question nyx never recorded
        // asking). nyx cannot say what the answer would be an answer TO.
        (None, _) => "nyx holds no record of having asked this question, so it cannot tell what \
                      this answer answers"
            .to_string(),
        // The ledger has moved to nothing: no prompt has ever reached nyx on the terminal the
        // question was posed to.
        (Some(_), None) => "nyx has recorded no human prompt at all on the session it asked — so \
                            nothing stands behind this answer"
            .to_string(),
        // The honest one: a human HAS typed on that terminal, but not since the question was posed.
        (Some(_), Some(turn)) => format!(
            "the last human prompt nyx has recorded on that session is still the one that stood \
             when it asked ({}) — nobody has typed since, so the answer is not theirs to have given",
            quote_prompt(&turn.prompt)
        ),
    };
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "this answer {}, and {}. Nothing is malformed: the field, the value and the run are all \
             correct; what is missing is the human. ASK THEM the question now, in the conversation, \
             WAIT for their reply, and report it on the tick that follows it. An instruction they \
             gave you earlier — including 'answer yes when nyx asks' — is not an answer to this \
             question, and nyx will refuse it again. Until then, call nyx_next(run_id=\"{}\") with \
             NO last_result: the same question comes back.",
            commitment.what_it_commits(),
            state,
            run.id
        ),
    )
    .with_data(json!({
        "reason": "human_consent_without_human_turn",
        "run_id": run.id,
        "prompt": prompt,
        "options": pending.map(|p| p.options.clone()).unwrap_or_default(),
    }))
}

/// The REFUSAL for an answer whose human turn was ALREADY SPENT on another commitment.
///
/// ITS OWN CASE, and not a variant of "no human turn since it asked", because the situation is the
/// opposite one: the human DID speak. A session told "nobody has typed since" when somebody just did
/// reads that as a bug in nyx and argues with it — or, worse, believes it and tells the human nyx
/// lost their answer. What actually happened is that those words settled ANOTHER question, and only
/// saying so makes the next move obvious.
///
/// So it NAMES the other run — the words themselves are in the outbox (`run.consent_answered`), the
/// run id is what identifies the commitment they paid for — and states the rule it broke in one line:
/// one human turn authorises one commitment. What makes progress is not a retry, not another field
/// and not a different value: it is putting THIS question to the human, and a NEW turn of theirs.
fn consent_turn_already_spent_error(
    run: &db::WorkflowRun,
    pending: Option<&db::PendingConsent>,
    turn: &db::HumanTurn,
    on_run: &str,
    commitment: crate::engine::HumanCommitment,
) -> RpcError {
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "this answer {}, and the human HAS spoken since nyx asked — but that turn ({}) has \
             already been accepted as the answer to ANOTHER consent question, on run '{}'. One human \
             turn authorises ONE commitment: those words settled that question, and there is nothing \
             of them left to settle this one. Nothing is malformed, and nyx has lost nothing. ASK \
             THEM THIS question now, in the conversation — quote it, so they answer THIS one and not \
             the other — WAIT for their reply, and report it on the tick that follows: that new turn \
             is what settles this question. Until then, call nyx_next(run_id=\"{}\") with NO \
             last_result: the same question comes back.",
            commitment.what_it_commits(),
            quote_prompt(&turn.prompt),
            on_run,
            run.id
        ),
    )
    .with_data(json!({
        "reason": "human_consent_turn_already_spent",
        "run_id": run.id,
        "prompt": pending.map(|p| p.prompt.clone()).unwrap_or_default(),
        "options": pending.map(|p| p.options.clone()).unwrap_or_default(),
        // The commitment those words DID authorise, so a session driving several runs can see at
        // once which of its own questions took the answer it was relaying here.
        "spent_on_run": on_run,
    }))
}

/// A human prompt QUOTED inside the refusal, bounded: the point is that the person recognises the
/// turn nyx is talking about, and the first line of it does that — a whole pasted stack trace in an
/// error message does not. Single-line (a newline would break the sentence it sits in).
fn quote_prompt(prompt: &str) -> String {
    const MAX: usize = 60;
    let first_line = prompt.lines().next().unwrap_or("").trim();
    let head: String = first_line.chars().take(MAX).collect();
    if head.is_empty() {
        "an empty prompt".to_string()
    } else if head.chars().count() < first_line.chars().count() {
        format!("\"{head}…\"")
    } else {
        format!("\"{head}\"")
    }
}

/// The WIRE form of an `ask_human` — built HERE for both the top-level ask ([`action_json`]) and the
/// one BUBBLED UP from a child run ([`child_ask_human_bubble`]), so the two can never state different
/// contracts for the same question (they each had their own literal, and `answer_key` had to be added
/// to both).
///
/// `thread` = the child run and the node it is parked on, when the question comes from a sub-workflow:
/// the answer must then be routed to `nyx_next(child_run_id)`, where the CHILD's own ask_human resume
/// + the N19 `options` check handle it unchanged.
///
/// `commitment` marks the question whose answer disposes of something the session does not own (H6,
/// then J2). The contract is otherwise IDENTICAL — same type, same `answer_key`, same route — because
/// the answer really does travel the ordinary way; what the flag adds is the warning that nyx will
/// check a human turn stands behind it, and WHAT that answer commits. Said up front so the normal
/// path costs no refusal: a session told in advance asks first.
fn ask_human_json(
    prompt: &str,
    options: &[String],
    thread: Option<(&str, &str)>,
    commitment: Option<crate::engine::HumanCommitment>,
) -> Value {
    let mut json = json!({
        "type": "ask_human",
        "prompt": prompt,
        // N19 — the closed answer set nyx expects; the dispatcher OFFERS these (it does not invent)
        // and nyx refuses anything else. Empty = free-form.
        "options": options,
        // F4 — and the FIELD to send it under: the action states the whole contract, so a caller never
        // has to guess the key (`{"answer": …}`, `{"choice": …}` … were tried blind).
        "answer_key": ASK_HUMAN_ANSWER_KEY,
    });
    if let (Some((child_run_id, child_node_id)), Some(obj)) = (thread, json.as_object_mut()) {
        // The THREAD the top level answers along. `child_node_id` is the one node id G5's drop SPARES:
        // the tick's `current_node_id` is the PARENT's sub_workflow node, so nothing else says where
        // the child is parked.
        obj.insert("child_run_id".to_string(), Value::from(child_run_id));
        obj.insert("child_node_id".to_string(), Value::from(child_node_id));
        obj.insert("via".to_string(), Value::from("sub_workflow"));
    }
    if let (Some(commitment), Some(obj)) = (commitment, json.as_object_mut()) {
        obj.insert("human_turn_required".to_string(), Value::from(true));
        obj.insert(
            "context".to_string(),
            Value::from(format!(
                "This answer {}. Put the question to the human in the conversation, wait for THEIR \
                 reply, and report it on the tick after it: nyx checks its own record of this \
                 session's prompts and refuses an answer with no human turn behind it — including \
                 one they told you in advance.",
                commitment.what_it_commits()
            )),
        );
    }
    json
}

/// The PROJECT a run belongs to — read THROUGH its bound Spec, which is the only project anchor a run
/// carries (a child run inherits the parent's `spec_id`, so the whole tree of runs answers the same
/// project). `None` for a run bound to no Spec.
///
/// Two things resolve on it and must never disagree: which workflow a `sub_workflow` node opens, and
/// which command a gate's SLOT is bound to. Both are "what does THIS project do here".
///
/// IT IS DELIBERATELY NOT WIDENED TO `run.workspace_id`, even though a workspace resolves a project
/// perfectly well ([`run_workspace_project_id`] does exactly that, for one caller). The two consumers
/// above are WHY: widening here would change WHICH GRAPH a `sub_workflow` opens on a Spec-less run (a
/// project-owned `coder` instead of the seeded built-in) and WHICH COMMANDS a slot gate runs on one (a
/// blocking gate that today refuses to advance for want of commands would suddenly arm itself). Both
/// are decisions about what a run is ALLOWED to do, taken from a weaker anchor than the one they were
/// authored against — a behaviour change that owes its own consent discussion and its own tests, not a
/// side effect of fixing a path lookup. A FACT that merely READS a project setting carries no such
/// weight, so that one resolves the wider way, at its own site and nowhere else.
fn run_project_id(db: &Db, run: &db::WorkflowRun) -> Result<Option<String>, RpcError> {
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Ok(None);
    };
    Ok(db
        .with_conn(|c| db::get_spec(c, spec_id))
        .map_err(internal_db)?
        .map(|s| s.project_id))
}

/// The PROJECT a run belongs to, read through its bound Spec AND — failing that — through the dev
/// WORKSPACE it was opened against ([`db::WorkflowRun::workspace_id`]). `None` only when the run has
/// neither.
///
/// A workspace belongs to exactly one project, so a run parked in one KNOWS its project whether or not
/// a Spec exists yet. Reading the Spec alone made `project_worktrees_root` answer `""` for a `custom`
/// run, and for a `/nspec` run before its Spec is created — so `worktrees_root_configured` came back
/// false and the seeded selector routed to the ask_human that demands a root the project had ALREADY
/// frozen in `projects.worktrees_root`. Asking again for a setting the user has set is the failure
/// this closes.
///
/// SPEC FIRST, workspace second: the Spec is the run's declared anchor ([`run_project_id`]) and a
/// workspace is where it happens to be running, so the two are ordered rather than merged — and they
/// cannot contradict each other in practice, since a workspace anchored to a Spec is anchored to that
/// Spec's project (`workspaces.spec_id`, this PRD's own invariant).
fn run_workspace_project_id(db: &Db, run: &db::WorkflowRun) -> Result<Option<String>, RpcError> {
    if let Some(project_id) = run_project_id(db, run)? {
        return Ok(Some(project_id));
    }
    let Some(workspace_id) = run.workspace_id.as_deref() else {
        return Ok(None);
    };
    Ok(db
        .with_conn(|c| db::get_workspace(c, workspace_id))
        .map_err(internal_db)?
        .map(|w| w.project_id))
}

/// Resolve a `sub_workflow` node's CHILD workflow: an explicit `workflow_id` (must exist), else the
/// `scope`'s active workflow in the PARENT's project (from its bound Spec), falling back to the seeded
/// built-in — the SAME resolution `run_start` uses (v2.8 lot 2).
fn resolve_child_workflow(
    db: &Db,
    run: &db::WorkflowRun,
    workflow_id: Option<&str>,
    scope: &str,
) -> Result<db::Workflow, RpcError> {
    if let Some(wf_id) = workflow_id {
        return db
            .with_conn(|c| db::get_workflow(c, wf_id))
            .map_err(internal_db)?
            .ok_or_else(|| {
                RpcError::new(
                    RpcCode::InvalidId,
                    format!("sub_workflow: unknown workflow '{wf_id}'"),
                )
            });
    }
    let project_id = run_project_id(db, run)?;
    db.with_conn(|c| db::resolve_active_workflow(c, project_id.as_deref(), scope))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidState,
                format!(
                    "sub_workflow: no '{scope}' workflow resolves — neither the parent's project nor a \
                     built-in default has one (author one, or set the node's workflow_id)"
                ),
            )
        })
}

// ============================================================================
// nyx's own effects: the PTY gate + the guarded transition
// ============================================================================

/// ONE managed command's result within a gate run (M6) — enough to explain WHY the gate is the
/// colour it is. `output_tail` is the last [`GATE_OUTPUT_TAIL_BYTES`] of the command's combined
/// STDOUT/STDERR; that stream is what the user already sees in a terminal, so surfacing it is safe —
/// and it is only the command's OUTPUT, never an env VALUE (the "secret values never logged"
/// invariant is untouched: this path captures output, it does not touch env).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GateCommandResult {
    /// The managed-command instance id the `gate_check` node referenced.
    command_id: String,
    /// The RESOLVED command line nyx actually ran.
    cmdline: String,
    /// The RESOLVED run directory (a wrong subfolder is one of the four indistinguishable red causes
    /// M6 exists to disambiguate).
    cwd: String,
    /// The process exit code, or `None` when it did not exit on its own (a timeout — see `timeout`).
    exit_code: Option<i32>,
    /// Whether this command passed (exited 0).
    passed: bool,
    /// The TRI-VALUED grade of this command — `passed` / `failed` (a non-zero EXIT) /
    /// `indeterminate` (no exit at all: a timeout, a spawn that failed, a command that could not be
    /// resolved). `passed` above is kept beside it because every existing reader (the UI, the
    /// refusal payload, the M6 tests) is written on it; what it cannot express is the third value.
    outcome: GateGrade,
    /// The bounded tail of the command's combined output (the END — the part that explains a failure).
    output_tail: String,
    /// A human-readable marker when the command did NOT exit: its own [`GATE_TIMEOUT`], or the gate's
    /// global [`GATE_TOTAL_TIMEOUT`] budget. `None` on a clean exit.
    timeout: Option<String>,
    /// Wall time the command took, in ms — part of what makes two attempts on "the same check"
    /// comparable, and the number that tells a genuinely slow suite from one that hangs.
    duration_ms: u64,
}

impl GateCommandResult {
    /// The result recorded when the gate's GLOBAL [`GATE_TOTAL_TIMEOUT`] budget is spent on this
    /// command (whatever partial `output_tail` was captured travels with it). Never green — and
    /// INDETERMINATE, not failed: the budget ran out, so nothing was judged.
    fn budget_exceeded(
        command_id: &str,
        cmdline: &str,
        cwd: &str,
        output_tail: String,
        duration_ms: u64,
    ) -> Self {
        GateCommandResult {
            command_id: command_id.to_string(),
            cmdline: cmdline.to_string(),
            cwd: cwd.to_string(),
            exit_code: None,
            passed: false,
            outcome: GateGrade::Indeterminate,
            output_tail,
            timeout: Some(gate_total_budget_marker()),
            duration_ms,
        }
    }

    /// The result recorded when the command could not be RUN AT ALL — a spawn failure, a binary that
    /// is not there, a run directory that no longer exists. `error` is the harness fault, kept in the
    /// output tail because that is where a reader looks for "what happened"; the grade is
    /// INDETERMINATE, which satisfies no expectation whatsoever.
    fn could_not_run(command_id: &str, cmdline: &str, cwd: &str, error: &str) -> Self {
        GateCommandResult {
            command_id: command_id.to_string(),
            cmdline: cmdline.to_string(),
            cwd: cwd.to_string(),
            exit_code: None,
            passed: false,
            outcome: GateGrade::Indeterminate,
            output_tail: error.to_string(),
            timeout: None,
            duration_ms: 0,
        }
    }
}

/// The structured verdict of a whole `gate_check` (M6): `passed` (every command green), the
/// TRI-VALUED `outcome`, plus the per-command detail. Serialized onto the blackboard
/// ([`LAST_GATE_RESULT_KEY`]) and attached to the gate-red refusal, so a RED gate explains itself
/// instead of being an opaque exit code.
#[derive(Debug, Clone, serde::Serialize)]
struct GateOutcome {
    passed: bool,
    /// The gate's grade, folded from its commands: INDETERMINATE as soon as one command produced no
    /// verdict, else FAILED as soon as one exited non-zero, else PASSED. The indeterminate wins
    /// because it is the absence of an answer, and an absent answer cannot be outvoted by the
    /// commands that did answer.
    outcome: GateGrade,
    commands: Vec<GateCommandResult>,
}

// ============================================================================
// The evidence PORT — every gate attempt, on its way to the durable journal
// ============================================================================

/// ONE gate attempt, as the tick that ran it saw it — the payload of the evidence PORT.
///
/// A "port" and not just a function call: the tick's job is to grade and enforce, and it must not
/// also decide what a record of the attempt is worth keeping. Everything an auditor was told to
/// check — WHICH check (node + slot), WHAT was asked of it (`expect`), WHAT it did (`grade`),
/// whether that ANSWERED (`satisfied`), and the identity of the control itself (each command's
/// resolved cmdline, its cwd, its exit status, how long it took and a bounded extract of its
/// output) — travels here, once, from the one place that holds all of it.
///
/// The identity fields are not decoration. The auditor's `bug` discipline tells them to confront the
/// red attempt and the green one on SAME SLOT, SAME RESOLVED COMMANDS, SAME CWD — "a red obtained on
/// one check and a green obtained on another demonstrate nothing together". That comparison is only
/// possible if each attempt carries those three, resolved, as they were at the moment it ran.
pub(crate) struct GateAttempt<'a> {
    /// The run the attempt belongs to — and, through it, the Spec and the workspace.
    pub run: &'a db::WorkflowRun,
    /// The `gate_check` node. Two gates of one flow are two nodes, and this is what keeps their
    /// attempts apart.
    pub node_id: &'a str,
    /// The command SLOT the node declared, when it declared one (`None` = it names its own ids).
    pub slot: Option<&'a str>,
    /// The expectation the attempt was graded against (the declared one, else `green`).
    pub expect: GateExpectation,
    /// Whether that expectation was DECLARED by the node or defaulted. Recorded because a graph that
    /// declared nothing and one that declared `green` are graded identically but enforced
    /// differently, and a journal that flattened the two would misdescribe why a run was refused.
    pub declared: bool,
    /// Whether the node is blocking — the difference between a refusal and an advisory reading.
    pub blocking: bool,
    /// What the check DID.
    pub grade: GateGrade,
    /// Whether that answered [`GateAttempt::expect`].
    pub satisfied: bool,
    /// The per-command detail, already BOUNDED by [`GATE_OUTPUT_TAIL_BYTES`] at capture time.
    pub commands: &'a [GateCommandResult],
}

/// **THE PORT** every gate attempt is emitted through, on its way to the durable append-only
/// journal — the one the auditor's `bug` discipline reads the red→green sequence out of.
///
/// Called from EVERY attempt site, including the ones that produced no verdict: a journal an
/// auditor is told to treat as complete ("what you cannot see recorded, treat as absent") must not
/// be able to omit the attempts that failed to run, or the absence of a record would read as the
/// absence of an attempt.
///
/// It NEVER fails the tick. A gate whose verdict is sound must not be turned into a refusal because
/// the audit trail could not be written — the enforcement is the tick's promise, the journal is the
/// record of it, and losing the second must not corrupt the first.
fn record_gate_attempt(db: &Db, attempt: &GateAttempt<'_>) {
    // The DECIDING command is the LAST one: `run_gate_bounded` stops at the first command that does
    // not pass, so the tail of the list is the one that produced the gate's grade. Its exit status is
    // the attempt's; the identity fields join EVERY command, because "the same check" means the whole
    // list and not just the one that happened to speak.
    let deciding = attempt.commands.last();
    let join = |f: fn(&GateCommandResult) -> &str| -> String {
        attempt
            .commands
            .iter()
            .map(f)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let output = attempt
        .commands
        .iter()
        .map(|c| c.output_tail.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    // The SAME bounding + sanitisation policy on both surfaces — the flat excerpt and the
    // per-command detail — because a policy applied to one of two copies of the same bytes is not a
    // policy ([`db::gate_evidence_excerpt`]).
    let commands_json = Value::Array(
        attempt
            .commands
            .iter()
            .map(|c| {
                json!({
                    "command_id": c.command_id,
                    "cmdline": c.cmdline,
                    "cwd": c.cwd,
                    "exit_code": c.exit_code,
                    "outcome": c.outcome.as_str(),
                    "duration_ms": c.duration_ms,
                    "timeout": c.timeout,
                    "output_excerpt": db::gate_evidence_excerpt(&c.output_tail),
                })
            })
            .collect(),
    )
    .to_string();
    let row = db::NewGateAttempt {
        run_id: attempt.run.id.clone(),
        spec_id: attempt.run.spec_id.clone(),
        node_id: attempt.node_id.to_string(),
        slot: attempt.slot.map(str::to_string),
        expect: attempt.expect.as_str().to_string(),
        declared: attempt.declared,
        blocking: attempt.blocking,
        outcome: attempt.grade.as_str().to_string(),
        satisfied: attempt.satisfied,
        command_line: join(|c| c.cmdline.as_str()),
        cwd: join(|c| c.cwd.as_str()),
        exit_code: deciding.and_then(|c| c.exit_code),
        duration_ms: attempt
            .commands
            .iter()
            .map(|c| i64::try_from(c.duration_ms).unwrap_or(i64::MAX))
            .sum(),
        output_excerpt: db::gate_evidence_excerpt(&output),
        commands_json,
        created_at: 0,
    };
    // A journal write that fails must not fail the tick — the enforcement already happened and is
    // the promise; losing the record of it must not corrupt it. It is not swallowed in silence
    // either: the run's own `run.tick` row still describes the stop, and the missing attempt is
    // exactly what the auditor's "treat what you cannot see recorded as absent" is written for.
    let _ = db.with_conn(|c| db::append_gate_attempt(c, row));
}

/// The marker recorded when a gate blows its GLOBAL [`GATE_TOTAL_TIMEOUT`] budget (all commands
/// together) — distinct from a single command's own timeout. A pure helper so the global-bound path
/// is unit-testable without sleeping the whole budget.
fn gate_total_budget_marker() -> String {
    format!(
        "gate exceeded its total time budget of {}s (all commands combined)",
        GATE_TOTAL_TIMEOUT.as_secs()
    )
}

/// Tag a gate's [`GateOutcome`] with the `node_id` it describes, the EXPECTATION it was graded
/// against and whether it answered it, then serialize it to the JSON stored on the blackboard /
/// attached to the refusal — so a reader can tell WHICH gate it belongs to and WHAT was asked of it.
///
/// `expect` + `satisfied` ride HERE rather than only in the two board booleans because the surfaced
/// object is what the UI's run card and the refusal payload read, and "the command exited 1" is not
/// a finding on its own: on a reproduction gate it is the awaited proof, on a build gate it is the
/// failure. The object has to say which.
fn gate_result_json(
    node_id: &str,
    expect: GateExpectation,
    satisfied: bool,
    gate: GateOutcome,
) -> Value {
    let mut value = serde_json::to_value(gate).unwrap_or(Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("node_id".to_string(), Value::from(node_id));
        obj.insert("expect".to_string(), Value::from(expect.as_str()));
        obj.insert("satisfied".to_string(), Value::from(satisfied));
    }
    value
}

/// A bounded, drop-from-the-FRONT byte buffer keeping only the last `cap` bytes of a command's
/// output — so a chatty gate command cannot grow memory without bound. The kept slice is the END
/// (the tail), exactly the part that explains a failure. The reader sends ≤8 KB chunks, so the
/// buffer never exceeds `cap + one chunk` before it is trimmed.
struct GateTail {
    cap: usize,
    buf: Vec<u8>,
}

impl GateTail {
    fn new(cap: usize) -> Self {
        GateTail {
            cap,
            buf: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > self.cap {
            let drop = self.buf.len() - self.cap;
            self.buf.drain(..drop);
        }
    }

    /// The captured tail as a String (lossy: a UTF-8 sequence sliced by the tail cut / a chunk
    /// boundary becomes the replacement char rather than failing).
    fn into_string(self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }
}

/// Run a `gate_check`'s managed commands as NYX runs them: each `command_id` is resolved to its
/// command line + run directory + PERSISTED environment ([`crate::command::resolve_command_and_cwd`])
/// and executed over a real PTY ([`crate::command::CommandPty`]); the exit code IS the verdict
/// (0 = green). The gate is GREEN only when EVERY command is green (an empty gate is vacuously green,
/// and only a NON-BLOCKING one ever gets here — a blocking empty gate is refused before this runs,
/// see [`refuse_vacuous_gate`]). This is the whole of "nyx a le PTY, exécute le gate_check LUI-MÊME" —
/// now RETURNING each command's output tail so a red gate says WHY (M6).
///
/// # The environment a gate command gets (v2.8 review N31)
///
/// A gate runs the command WITH the environment stored on the command itself
/// (`managed_commands.env`), merged onto nyx's inherited one — the SAME base an agent-initiated
/// `start_command` applies, because the env belongs to the COMMAND, not to whoever launches it.
/// What a gate does NOT get is the per-run `env` argument of `start_command`/`relaunch_command`:
/// that override is transient and agent-initiated by definition, so a gate verdict never depends on
/// what an agent happened to pass on its last manual start. A malformed stored env yields an EMPTY
/// one, never an unresolvable command.
///
/// Each command still runs NON-interactively, WITHOUT sourcing the user's rc (see
/// [`crate::command::CommandPty::spawn`]) — so mise/nvm/bun/direnv are NOT activated. That half of
/// N31 stays DEFERRED by an explicit user decision (an interactive shell would break the pgroup
/// tree-kill), so a gate command needing a version-manager-provided tool must declare what it needs
/// in its persisted env, use absolute tool paths, or wrap itself in a script that sources the
/// environment.
fn run_gate(db: &Db, command_ids: &[String]) -> Result<GateOutcome, GateRunError> {
    run_gate_bounded(db, command_ids, GATE_TOTAL_TIMEOUT)
}

/// A gate that could not be RESOLVED — the one failure [`run_gate_bounded`] cannot grade, carrying
/// everything the journal needs to describe it honestly.
///
/// A bare [`RpcError`] could not: it says WHAT went wrong and nothing about WHERE. A gate arms a
/// LIST of commands and the runner walks it in order, so the failure can land on the third one after
/// the first two have already run over the PTY. Recorded from the bare error, the attempt named
/// `command_ids.first()` — the wrong command, with an empty cmdline and an empty cwd — and dropped
/// the results of the commands that did run. Those three fields are the identity of the control the
/// auditor's discipline tells them to confront between a red and a green ([`GateAttempt`]), so an
/// attempt that misattributes them is not a smaller record, it is a false one.
#[derive(Debug)]
struct GateRunError {
    /// The command instance id whose resolution failed — the one the journal must name.
    failing_id: String,
    /// The commands that ALREADY RAN, in order, with their real cmdline, cwd, exit code and output.
    done: Vec<GateCommandResult>,
    /// The refusal itself, as the caller propagates it.
    error: RpcError,
}

/// The refusal a BLOCKING gate earns when one of its commands could not be LAUNCHED at all — a
/// binary that is not there, a run directory that vanished, an exhausted PTY. `None` for every other
/// shape of "not green", including a TIMEOUT: a check that ran and did not finish is a different
/// fact with a different route out, and the M5 anchor already reports it in its own words.
///
/// A spawn failure is told apart from a timeout by the two fields [`GateCommandResult::could_not_run`]
/// sets and [`GateCommandResult::budget_exceeded`] does not: `indeterminate` with NO timeout marker.
fn gate_spawn_failure(gate: &GateOutcome) -> Option<RpcError> {
    let failed = gate
        .commands
        .iter()
        .find(|c| c.outcome == GateGrade::Indeterminate && c.timeout.is_none())?;
    Some(
        RpcError::new(
            RpcCode::InvalidState,
            format!(
                "a blocking gate_check could NOT BE RUN — nyx never obtained a verdict from it, so \
                 nothing was checked and nyx_next refuses to advance past it. This is not a failing \
                 check and not a red: `{cmdline}` (cwd `{cwd}`) could not be launched — {why}. Fix \
                 what stops the command from starting; re-running it changes nothing",
                cmdline = failed.cmdline,
                cwd = failed.cwd,
                why = failed.output_tail.trim(),
            ),
        )
        .with_data(json!({
            "reason": "gate_could_not_run",
            "command_id": failed.command_id,
            "cmdline": failed.cmdline,
            "cwd": failed.cwd,
        })),
    )
}

/// [`run_gate`] with an INJECTABLE global budget so the total-timeout path is testable without
/// sleeping [`GATE_TOTAL_TIMEOUT`] (a test lowers it). Runs each command in order, keeping a bounded
/// output tail, and STOPS at the first failing command (the gate is already red) or when the global
/// `total_budget` is exhausted (every command shares it, in addition to the per-command
/// [`GATE_TIMEOUT`]). An empty gate yields `{ passed: true, commands: [] }` (vacuously green).
fn run_gate_bounded(
    db: &Db,
    command_ids: &[String],
    total_budget: Duration,
) -> Result<GateOutcome, GateRunError> {
    let gate_deadline = Instant::now() + total_budget;
    let mut commands = Vec::new();
    for command_id in command_ids {
        // A RESOLUTION FAILURE NAMES **THIS** COMMAND and hands back what already ran — see
        // [`GateRunError`]. The loop holds the id; the error is where it used to be lost.
        let resolved = crate::command::resolve_command_and_cwd(db, command_id).map_err(|e| {
            GateRunError {
                failing_id: command_id.clone(),
                done: std::mem::take(&mut commands),
                error: RpcError::new(RpcCode::InvalidId, format!("gate command: {e}")),
            }
        });
        let (cmdline, cwd, env) = resolved?;
        let global_left = gate_deadline.saturating_duration_since(Instant::now());
        if global_left.is_zero() {
            // The global budget is spent before this command could even start — nothing was judged.
            commands.push(GateCommandResult::budget_exceeded(
                command_id,
                &cmdline,
                &cwd,
                String::new(),
                0,
            ));
            break;
        }
        // Bound EACH command by the smaller of its own cap and the remaining GLOBAL budget.
        let budget = global_left.min(GATE_TIMEOUT);
        let mut result = run_one_gate(command_id, &cmdline, &cwd, &env, budget);
        let cmd_passed = result.passed;
        // A timeout the GLOBAL budget bound (not the command's own 600s) is the total-budget breach —
        // relabel it so the operator sees WHICH limit tripped, keeping the partial output captured.
        if result.timeout.is_some() && global_left <= GATE_TIMEOUT {
            result = GateCommandResult::budget_exceeded(
                command_id,
                &cmdline,
                &cwd,
                result.output_tail,
                result.duration_ms,
            );
        }
        commands.push(result);
        if !cmd_passed {
            break;
        }
    }
    Ok(GateOutcome {
        passed: commands.iter().all(|c| c.passed),
        outcome: fold_gate_grade(&commands),
        commands,
    })
}

/// The gate's grade from its commands' — INDETERMINATE as soon as one produced no verdict, else
/// FAILED as soon as one exited non-zero, else PASSED (an empty gate is vacuously passed, which is
/// the vacuous-green rule the blocking case is refused before ever reaching).
///
/// The precedence is not arbitrary. [`run_gate_bounded`] stops at the FIRST non-passing command, so
/// at most one command is ever non-passing — but the fold is written for the general shape anyway,
/// and in it an ABSENT answer must dominate: a gate whose second command timed out has not been
/// verified, whatever its first command said.
fn fold_gate_grade(commands: &[GateCommandResult]) -> GateGrade {
    if commands
        .iter()
        .any(|c| c.outcome == GateGrade::Indeterminate)
    {
        GateGrade::Indeterminate
    } else if commands.iter().any(|c| c.outcome == GateGrade::Failed) {
        GateGrade::Failed
    } else {
        GateGrade::Passed
    }
}

/// Spawn ONE managed command over a PTY, CAPTURE a bounded tail of its output (M6), and return its
/// result. BOUNDED by `budget` (the smaller of [`GATE_TIMEOUT`] and the gate's remaining global
/// budget): a command that never exits yields RED with a timeout marker rather than hanging the verb
/// (dropping the `CommandPty` on the way out kills its process tree).
///
/// `env` is the command's PERSISTED environment (N31), merged onto nyx's inherited one — the same
/// base every other spawn path applies. Its VALUES are secrets: they reach `cmd.env` and nothing
/// else (the captured output tail is the COMMAND's, not the environment's).
/// A SPAWN that fails is NOT an error out of this function any more: it is an attempt graded
/// INDETERMINATE ([`GateCommandResult::could_not_run`]). "The binary is not there" and "the tests
/// failed" are opposite facts, and the second one is the only one that may ever answer a
/// reproduction's `expect: red` — so the failure to launch has to be a graded, recorded attempt
/// rather than a bare `Err` the grading never sees. The tick still parks the run on such a gate
/// (nothing satisfies an indeterminate), so the ENFORCEMENT is unchanged.
fn run_one_gate(
    command_id: &str,
    cmdline: &str,
    cwd: &str,
    env: &[(String, String)],
    budget: Duration,
) -> GateCommandResult {
    let started = Instant::now();
    let spawned = crate::command::CommandPty::spawn(
        cmdline,
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        Some(cwd),
        env,
    );
    let (mut pty, rx) = match spawned {
        Ok(pair) => pair,
        Err(e) => {
            return GateCommandResult::could_not_run(
                command_id,
                cmdline,
                cwd,
                &format!("gate spawn failed: {e}"),
            )
        }
    };
    // Drain output onto the bounded tail so the reader thread runs to EOF (the channel is unbounded —
    // no backpressure — so we must keep receiving; the TAIL, not the whole stream, is what we retain).
    let mut tail = GateTail::new(GATE_OUTPUT_TAIL_BYTES);
    let deadline = Instant::now() + budget;
    let timed_out = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break true;
        }
        match rx.recv_timeout(left) {
            Ok(chunk) => tail.push(&chunk),
            Err(RecvTimeoutError::Timeout) => break true,
            // EOF: the command closed the PTY, so its exit code is (about to be) available.
            Err(RecvTimeoutError::Disconnected) => break false,
        }
    };
    let output_tail = tail.into_string();
    if timed_out {
        // Dropping `pty` on return kills the process tree.
        return GateCommandResult {
            command_id: command_id.to_string(),
            cmdline: cmdline.to_string(),
            cwd: cwd.to_string(),
            exit_code: None,
            passed: false,
            outcome: GateGrade::Indeterminate,
            output_tail,
            timeout: Some(format!(
                "gate command exceeded its {}s time limit",
                budget.as_secs()
            )),
            duration_ms: elapsed_ms(started),
        };
    }
    let exit_code = pty.wait();
    GateCommandResult {
        command_id: command_id.to_string(),
        cmdline: cmdline.to_string(),
        cwd: cwd.to_string(),
        exit_code,
        passed: exit_code == Some(0),
        // An exit code nyx could not READ is not a non-zero exit: the process is gone and its status
        // with it, so nothing was judged.
        outcome: match exit_code {
            Some(0) => GateGrade::Passed,
            Some(_) => GateGrade::Failed,
            None => GateGrade::Indeterminate,
        },
        output_tail,
        timeout: None,
        duration_ms: elapsed_ms(started),
    }
}

/// Milliseconds since `started`, saturated into a `u64` (a duration that overflows one is a clock
/// anomaly, not a measurement — record the ceiling rather than panic inside a gate).
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// An ENTRY transition — the graph's opening move (`start`, ready→in_progress), whose consent came
/// from LAUNCHING the command (/ndev with `user_confirmed`). A graph may impose it WITHOUT a per-node
/// human gate (v2.8 review B3).
fn transition_is_entry(t: db::SpecTransition) -> bool {
    matches!(t, db::SpecTransition::Start)
}

/// An ENGAGING transition — a commitment (`ready` / `done` / `cancel`) a graph may impose ONLY behind
/// a preceding `ask_human` approval: validated in the tree (a preceding-sibling ask_human) AND gated
/// at runtime on the blackboard key that ask_human wrote (B3). `in_review` is NEITHER entry nor
/// engaging — it is non-sensitive and always imposable.
fn transition_is_engaging(t: db::SpecTransition) -> bool {
    matches!(
        t,
        db::SpecTransition::Ready | db::SpecTransition::Done | db::SpecTransition::Cancel
    )
}

/// Whether a blackboard value counts as an AFFIRMATIVE human approval for an engaging transition's
/// gate: `true`, or an affirmative word (yes/y/ok/approve(d)/ready/go, case-insensitive). Absent /
/// `false` / "no" / a structured value is NOT approval (B3).
///
/// `pub(crate)` because the AUTHORING guard reads it too — but ONLY for the arm where the runtime
/// really calls this function, and that is worth stating precisely, because the two arms of rule (4)'s
/// condition (ii) do NOT share a comparator:
///
///   * the `gate_on` ARM — the runtime gate of a sensitive node calls THIS on the live board value
///     (`drive`'s transition / transition_task / fork_spec / create_workspace arms). One vocabulary of
///     consent for the two layers there, so a word the authoring rule accepts as an approval and a
///     word the gate honours at runtime cannot drift apart.
///   * the SELECTOR ARM — the runtime does NOT call this at all. A branch is chosen by
///     [`crate::engine::Condition::matches`], which compares by EXACT JSON equality: no `trim`, no
///     case folding. So a literal this function alone would credit (`eq: "YES"`, `eq: " yes "`) is a
///     branch that can never fire, and when no branch fires the selector kills the run with
///     [`crate::engine::EngineError::SelectorNoBranch`]. The authoring polarity test therefore goes
///     through [`eq_literal_is_affirmative`], which is this vocabulary NARROWED to the literals
///     `matches` can actually match (finding `01M0C3FGCACXCZA1ZESRV73N0R`).
pub(crate) fn board_value_is_affirmative(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "yes" | "y" | "ok" | "approve" | "approved" | "ready" | "go" | "true"
        ),
        _ => false,
    }
}

/// What the board holds under a `set_category` node's key, read as a LABEL — the three states the
/// effect arm branches on, and they are three and not two on purpose.
enum BoardLabel {
    /// The key is not on the board at all: nothing was ever written there. Nothing to classify
    /// WITH, and nothing went wrong — the node is a no-op and the walk continues (the long-standing
    /// arbitrage, kept: a graph may name a key a branch above it did not take).
    Absent,
    /// The key EXISTS and holds something that is not a label: `null` (a judge that answered
    /// nothing — what a resume without `last_result` leaves), a number, a bool, an object, an array,
    /// or a string that is blank once trimmed. A value was written and it says nothing. Carries the
    /// value as the refusal names it, so the message can quote what was found.
    Unusable(String),
    /// A real label — a non-empty, trimmed string.
    Label(String),
}

/// Read a `set_category` node's board key as a [`BoardLabel`].
///
/// THE RAW VALUE, NEVER THE INTERPOLATED STRING. `{{key}}` interpolation flattens every JSON value
/// to its `to_string()`, so `Value::Null` arrives as the four letters `null` — a perfectly ordinary
/// category NAME, which `crate::category::resolve_or_create` would happily mint. Distinguishing "no
/// answer" from "the answer is the word null" is only possible before that flattening.
fn board_category_label(value: Option<&Value>) -> BoardLabel {
    match value {
        None => BoardLabel::Absent,
        Some(Value::String(s)) if !s.trim().is_empty() => BoardLabel::Label(s.trim().to_string()),
        Some(Value::Null) => BoardLabel::Unusable("null".to_string()),
        Some(Value::String(_)) => BoardLabel::Unusable("an empty string".to_string()),
        Some(other) => BoardLabel::Unusable(format!("`{other}`")),
    }
}

/// The refusal a `set_category` node earns when its key holds something that is not a label — a
/// `null`, a number, an object, a blank string. NAMES THE KEY, because the fix is to write that key
/// (re-answer the judge, re-run the query) and a message that only said "invalid category" would
/// send a reader looking at the vocabulary instead of at the board.
///
/// It is an `invalid_state` and it PARKS the run (the caller hands it to
/// [`block_on_effect_error`]), for the same reason the service's own refusals do: the discipline
/// downstream of this node is authored on the classification HAVING LANDED. Walking past it would
/// run a `bug` flow over a Spec nyx never classified — and minting a category out of the rendered
/// value would be worse still, since a truthy `spec_category` fact then exempts that Spec from the
/// preflight forever.
fn refuse_unusable_category(key: &str, seen: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "the `set_category` node reads its label from the blackboard key '{key}', and that key \
             holds {seen} — not a category label. nyx will NOT classify a Spec with it: a rendered \
             `null` is a perfectly good category NAME and would be minted as one, which then makes \
             the Spec look classified to every branch that reads it back. Write '{key}' (answer the \
             judge that fills it, or re-run the query that does) and call nyx_next again — this \
             node re-runs until it holds a real label"
        ),
    )
    .with_data(json!({ "reason": "category_label_unusable", "key": key }))
}

/// Whether a selector branch's `eq` LITERAL is an approval the RUNTIME can actually match — the
/// polarity test of rule (4)'s SELECTOR arm ([`crate::engine::Condition::affirmative_key`]).
///
/// [`board_value_is_affirmative`] answers "does this VALUE count as a yes?", and it is generous on
/// purpose: it trims and case-folds, because a human's typed answer may arrive as `" Yes "`. A branch
/// LITERAL is the other side of that comparison, and [`crate::engine::Condition::matches`] compares it
/// to the board value by EXACT JSON equality. A literal that only becomes affirmative AFTER trimming
/// or lowering therefore describes a branch that never fires: authoring credited it as the consent,
/// the run reached the selector, no branch matched, and `SelectorNoBranch` failed the run. So the
/// literal must ALREADY be in the canonical form the vocabulary lists — `"yes"`, `"go"`, `true` pass;
/// `"YES"` and `" yes "` do not.
pub(crate) fn eq_literal_is_affirmative(v: &Value) -> bool {
    match v {
        // Canonical form or nothing: `s` must be the very string the vocabulary holds — trimmed and
        // lowered ALREADY — since that is the only string `Condition::matches` can compare a board
        // value to and win.
        Value::String(s) => {
            board_value_is_affirmative(Some(v)) && *s == s.trim().to_ascii_lowercase()
        }
        other => board_value_is_affirmative(Some(other)),
    }
}

/// Apply a `transition` node through the GUARDED Spec authority. `gate_satisfied` = whether the
/// preceding `ask_human` approved (read from the blackboard by `drive` via the node's `gate_on`),
/// which authorizes an ENGAGING transition — that ask_human IS the human's confirmation, routed
/// through the graph, so nyx does NOT bypass the guard-rail, it satisfies it (B3). An ENTRY
/// transition (`start`) is authorized by the launch; `in_review` is non-sensitive. A run not bound
/// to a Spec advances after the target is validated (M2). An engaging move without a satisfied gate
/// is refused (sticky — the caller replays the leaf).
fn apply_transition(
    db: &Db,
    run: &db::WorkflowRun,
    target_state: &str,
    gate_satisfied: bool,
) -> Result<(), RpcError> {
    let transition = parse_target_transition(target_state)?;
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Ok(());
    };
    if transition_is_engaging(transition) && !gate_satisfied {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            format!(
                "engaging transition '{target_state}' requires a preceding ask_human approval — its \
                 gate is not satisfied (no affirmative human answer on the gate_on key)"
            ),
        )
        .with_data(json!({ "reason": "engaging_transition_ungated" })));
    }
    // The human's confirmation reaches the guarded authority: an ENTRY (launch consent) or an
    // ENGAGING move whose gate is satisfied confirms; `in_review` is not sensitive (confirm = false).
    let confirmed =
        transition_is_entry(transition) || (transition_is_engaging(transition) && gate_satisfied);
    // Pass the run's workspace through: `transition_spec_with_workspace` poses it as the Spec's anchor
    // on the ENTRY `start` transition (ready→in_progress) and IGNORES it for every other transition, so
    // a graph-started Spec keeps the anchor the manual `spec_transition start` used to pose (N24).
    db.with_conn(|c| {
        db::transition_spec_with_workspace(
            c,
            spec_id,
            transition,
            confirmed,
            run.workspace_id.as_deref(),
        )
    })
    .map(|_| ())
    .map_err(crate::mcp_tools_spec::map_spec_transition_err)
}

/// The sentence a `done` transition owes the human when the Spec it just COMPLETED still carries
/// DEFERRED findings — `None` for any other transition, and for a completion that carries none.
///
/// COMPLETING WITH OPEN LOWS IS THE DECIDED OUTCOME, BUT THE COMPLETION MUST NOT PRETEND THE SPEC IS
/// CLEAN. [`db::validate_done`] is where that fact LIVES — a standing property any surface can read
/// at any time, still true tomorrow when someone opens the Spec — and this is where it SPEAKS, in the
/// one flow that just took the decision. Read AFTER the transition committed, so it reports what was
/// actually shipped rather than what was about to be.
///
/// Not `run_start`'s `warnings`: that channel opens a run, and a completion happens many ticks later
/// at its END — a Spec also completes straight from the UI, which never calls `run_start` at all.
fn deferred_findings_note(
    db: &Db,
    run: &db::WorkflowRun,
    target_state: &str,
) -> Result<Option<String>, RpcError> {
    if parse_target_transition(target_state)? != db::SpecTransition::Done {
        return Ok(None);
    }
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Ok(None);
    };
    let deferred = db
        .with_conn(|c| db::validate_done(c, spec_id))
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("transition done: {e}")))?
        .into_iter()
        .find_map(|f| match f {
            db::DoneValidation::DeferredFindings { count } => Some(count),
            _ => None,
        });
    Ok(deferred.map(|count| {
        format!(
            "The Spec is completed, carrying {count} open low-severity finding(s). They were NOT \
             worked in this cycle by design — only a high/medium finding re-opens a build — and they \
             stay on the Spec as open review tasks with their severity, findable on the next one."
        )
    }))
}

/// The dev workspace a `create_workspace` node WOULD open for `run` — the bound Spec, the NAME the
/// folder carries and the FOLDER ITSELF. It is THE ONE derivation: the effect
/// ([`create_run_workspace`]) creates exactly this, and the PREVALIDATION
/// ([`resolve_workspace_target_free`]) checks exactly this, so a check and the write it guards can
/// never be told about two different folders.
///
/// WHERE the folder goes: `root` JOINED with `name`, always — `root` is a ROOT, the folder workspaces
/// land UNDER, whoever filled the board key it came from (a project's configured
/// [`db::Project::worktrees_root`] read by a `query`, or a root a human answered). An EMPTY root is
/// `invalid_argument`: a node that cannot say WHERE must not guess a folder on a user's disk.
///
/// THE DERIVATION IS NOT OPTIONAL, and that is the whole lesson of 2026-08-18. This function used to
/// take a second source — an "explicit path" — that WON over the root and was registered AS the
/// workspace, with no name joined. The graph's two branches therefore disagreed about what the same
/// kind of value meant: the configured root got a subfolder, the ANSWERED one was taken whole. The
/// question asked for a root, the human gave one (`d:\projects\_worktrees`), and nyx registered the
/// common parent of every worktree as a workspace — silently, because the folder already existed, so
/// the `mkdir` had nothing to report. Every later run then collided with that row. Merging the sources
/// BEFORE the join is what makes the asymmetry unrepresentable: there is one base, and one `join`.
///
/// `name` empty → DERIVED as the Spec's human key (`<project key>-<number>`, e.g. `NYX-42`): the name
/// the human already knows the Spec by, and one that is filesystem-safe by construction, unlike a
/// title. The derivation lives here rather than in the engine because the engine is pure and the key
/// is two DB rows away.
fn resolve_workspace_target(
    db: &Db,
    run: &db::WorkflowRun,
    root: &str,
    name: &str,
) -> Result<(db::Spec, String, String), RpcError> {
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Err(RpcError::new(
            RpcCode::InvalidState,
            "create_workspace: this run carries no Spec, and the workspace this node opens is the \
             Spec's — there is nothing to anchor it to",
        ));
    };
    let spec = db
        .with_conn(|c| db::get_spec(c, spec_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("create_workspace: unknown Spec {spec_id}"),
            )
        })?;
    let project = db
        .with_conn(|c| db::get_project(c, &spec.project_id))
        .map_err(internal_db)?
        .ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidId,
                format!("create_workspace: unknown project {}", spec.project_id),
            )
        })?;

    let name = match name.trim() {
        "" => format!("{}-{}", project.key, spec.number),
        given => given.to_string(),
    };
    // THE NAME MUST STAY A NAME — one path SEGMENT, joined UNDER the root. `Path::join` silently
    // REPLACES the base when the right-hand side is absolute (`join("D:/elsewhere")` → `D:/elsewhere`)
    // and walks out of it on `..`, so a `name_key` fed by a human answer or a sub-workflow result
    // could put the workspace anywhere on the disk — the SAME defect as the retired `path_key`
    // (finding `01M0B5AG0XR4BG6DSGEFGKW6C4`), just entering by the other argument. The derived
    // `<project key>-<number>` is safe by construction; a board-supplied one is not, so it is checked
    // rather than trusted.
    // ONE `Component::Normal` and nothing else: the count alone is not enough, since `..` and `.`
    // are each a SINGLE component carrying no separator, and `C:` is a single Prefix — all three
    // would pass a naive length check and still leave the root.
    let mut segments = std::path::Path::new(&name).components();
    let single_plain_segment = matches!(segments.next(), Some(std::path::Component::Normal(_)))
        && segments.next().is_none()
        && !name.contains('/')
        && !name.contains('\\');
    if !single_plain_segment {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "create_workspace: the workspace name '{name}' is not a single folder name — it \
                 carries a path separator, a `..`/`.` or a drive prefix, which would place the \
                 workspace OUTSIDE the worktrees root instead of under it. The name is joined under \
                 the root; give a plain name, or leave it empty to derive the Spec's own key"
            ),
        ));
    }
    let root = root.trim();
    if root.is_empty() {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "create_workspace: no folder to create — the node got no worktrees root at all. Set the \
             project's `worktrees_root` (set_project_settings), or author the graph to ask for one \
             first",
        ));
    }
    let path = std::path::Path::new(root)
        .join(&name)
        .to_string_lossy()
        .into_owned();
    Ok((spec, name, path))
}

/// Open the DEV WORKSPACE of `run`'s bound Spec — the effect behind a `create_workspace` node.
///
/// IT REUSES THE TOOL, IT DOES NOT REIMPLEMENT IT. Everything past "which folder, which name" —
/// resolved by [`resolve_workspace_target`] — is [`crate::mcp_runtime::create_workspace`]: the
/// `mkdir`, the Spec-anchor PREVALIDATION that happens before anything touches the disk, the shared
/// transaction that makes registration + anchor atomic, and the leaf-only compensation of a folder
/// this call created. The Spec ANCHOR is that tool's `spec_id` argument (the one this PRD delivered) —
/// there is no second anchoring path here, and the anchor never moves the Spec's status.
///
/// THE SINK IS NULL HERE, ON PURPOSE, AND IT IS NOT WHERE THE REFRESH COMES FROM. The tool's
/// `events` parameter is the POST-COMMIT [`crate::usecase::EventSink`] a SHELL injects: the sidecar
/// hands `dispatch_workspace_tool` a `PushEventSink` (`nyx-sidecar/src/runtime/mcp.rs`), which turns
/// the `workspace.created` domain event into the unsolicited `workspaces://changed` the sidebar
/// re-pulls on. No shell sink reaches a `nyx_next` tick — `dispatch_extension_tool` takes none — so
/// this call passes a [`crate::usecase::NullEventSink`] and the graph path carries the SAME
/// invalidation on the tick's own `changed` topics instead ([`outcome`]). Nothing is lost by the
/// null sink itself: the `domain_events` row is written INSIDE the use case's transaction, not by
/// the sink, so only the real-time push ever depended on it.
fn create_run_workspace(
    db: &Db,
    run: &db::WorkflowRun,
    root: &str,
    name: &str,
) -> Result<Value, RpcError> {
    let (spec, name, path) = resolve_workspace_target(db, run, root, name)?;
    let spec_id = spec.id.clone();

    // THE tool, unchanged: prevalidated anchor → mkdir → one transaction → compensation. The engine
    // is not a second entry point into the workspace model, it is a caller of the first one.
    crate::mcp_runtime::create_workspace(
        db,
        &crate::usecase::NullEventSink,
        &json!({
            "project_id": spec.project_id,
            "name": name,
            "path": path,
            "spec_id": spec_id,
        }),
    )
}

/// Resolve `workspace_target_free`: whether the folder a `create_workspace` is ABOUT to open is still
/// free of a workspace of this project — plus the sentence that says which folder, and why it is not
/// (folded to `<write>_reason` by `drive`).
///
/// IT IS A PREVALIDATION, IN THE PRD'S OWN SENSE, MOVED ONE STEP EARLIER. The tool already prevalidates
/// the Spec anchor before it touches the disk, so a refused link creates no folder. This fact carries
/// the same idea up to the GRAPH, where the thing to protect is not the disk but the TRANSITION: the
/// `go` answer means "mark it ready AND open a workspace", and the sequence used to commit the
/// `ready` first, so a `create_workspace` that then failed left the Spec ready with NO workspace —
/// half of what the human asked for, silently. A graph that reads this fact BEFORE its transition can
/// route both or neither.
///
/// What it checks is exactly what the DB then refuses: `UNIQUE(project_id, root_path)`, compared over
/// the SAME [`crate::pathnorm::normalize`]d spelling [`db::create_workspace`] stores. It does not
/// pretend to cover every way the effect can fail (a permission error, a file where the folder goes,
/// a racing writer between the check and the write): the collision is the one that BIT, it is the one
/// a second `go` on a project hits by construction, and a check that cannot be complete is still worth
/// having when the alternative is a Spec left half-committed.
///
/// FAIL-CLOSED: anything that stops the folder being NAMED at all (no Spec, no root) answers `false`
/// carrying its own sentence, never an error — an erroring `query` parks the run, while `false` routes
/// it to the branch that says so and leaves the Spec exactly as it was.
fn resolve_workspace_target_free(
    db: &Db,
    run: &db::WorkflowRun,
    root: &str,
    name: &str,
) -> Result<(bool, String), RpcError> {
    let (spec, _name, path) = match resolve_workspace_target(db, run, root, name) {
        Ok(target) => target,
        Err(e) => return Ok((false, e.message)),
    };
    let normalized = crate::pathnorm::normalize(&path);
    let taken = db
        .with_conn(|c| db::list_workspaces(c, &spec.project_id))
        .map_err(|e| {
            RpcError::new(
                RpcCode::Internal,
                format!("query workspace_target_free: {e}"),
            )
        })?
        .into_iter()
        .find(|w| w.root_path == normalized);
    Ok(match taken {
        Some(w) => (
            false,
            format!(
                "the folder '{normalized}' is ALREADY the workspace '{}' of this project — one path \
                 holds one workspace, so opening a second one there is refused",
                w.name
            ),
        ),
        None => (
            true,
            format!("'{normalized}' is free — no workspace of this project occupies it"),
        ),
    })
}

/// The nyx fact a `query` node reads: the bound Spec's STATUS (`draft` / `in_review` / …). Extend
/// this set as graphs need more verifiable state to branch on (v2.8 review B2).
const QUERY_SPEC_STATUS: &str = "spec_status";
/// The bound Spec's CATEGORY — its genre of work (PDR-0017), as the **SLUG** (`"bug"`,
/// `"feature"`, a project's own), or `null`.
///
/// THE SLUG AND NEVER THE NAME. A category carries both, and only one of them is an identity: the
/// name is a human label a curator may rewrite at will (`Bug` → `Défaut`), while the slug is minted
/// once and immutable ([`crate::category`]). A graph branching on the name would silently leave the
/// discipline the day somebody renamed the chip — which is exactly the failure this whole PRD exists
/// to prevent, arriving through the back door.
///
/// `null` IS WRITTEN, NOT OMITTED, and for both of the ways there is no category: the Spec is
/// UNCLASSIFIED, or the run carries NO SPEC at all. A selector must be able to TEST the key — an
/// absent key and a key holding `null` read the same to a human and differently to a matcher, so the
/// fact spells absence one way. A run with no Spec is a legitimate state (a `custom` run, a `/nspec`
/// run before its Spec exists), so it answers `null` and keeps walking: never an error, never a park.
const QUERY_SPEC_CATEGORY: &str = "spec_category";
/// A PARAMETERIZED fact: whether the bound Spec CAN take the transition named by the node's `target`
/// — the legality from the current status ([`db::SpecTransition::source_allowed`]) PLUS the quality
/// gate (`ready` runs the pre-ready validations). Lets a graph CHECK before it proposes, routing away
/// instead of hitting a transition that refuses and freezes the run (v2.8 review, prevents N16).
const QUERY_SPEC_CAN_TRANSITION: &str = "spec_can_transition";
/// The bound Spec's COUNT of the work that REOPENS THE BUILD ([`db::reopens_the_build`] — the ONE
/// shared predicate, so this fact, `spec_open_findings`, the `phase_*` pair and [`db::validate_done`]
/// all answer over the same set). It INCLUDES the review findings the auditor files mid-build (the
/// count GROWS as findings are raised) but not the DEFERRED ones. The ndev build's outer "phases" loop
/// reads it to keep building until every task that reopens the build is resolved, keeping COMPLETION
/// (open work) distinct from QUALITY (the audit verdict) — v2.8 review N30.
const QUERY_SPEC_OPEN_TASKS: &str = "spec_open_tasks";
/// The bound Spec's COUNT of the OPEN REVIEW FINDINGS THAT REOPEN THE BUILD — the same
/// [`db::reopens_the_build`] predicate as `spec_open_tasks`, RESTRICTED to the tasks of kind `review`
/// (the findings an auditor files). Where `spec_open_tasks` measures COMPLETION (all the work, build
/// tasks included), this measures the AUDIT: it is what lets a graph DERIVE its audit verdict from the
/// findings actually filed instead of recording an agent's self-declared verdict (the NAMED-FACT rule
/// applied to the verdict itself — v2.8). A DEFERRED finding does not count, so a build whose audit
/// only leaves lows sees the derived `0` and STOPS. Unbound run → 0 (no Spec, no finding).
const QUERY_SPEC_OPEN_FINDINGS: &str = "spec_open_findings";
/// The bound Spec's COUNT of the OPEN findings the build DEFERRED — [`db::is_deferred_finding`], the
/// exact complement of [`db::reopens_the_build`], so this fact and `spec_open_findings` partition the
/// open findings between them and cannot double-count or lose one. It is what the Spec would SHIP
/// with, and it exists so the closing question can SAY so: a graph asking "mark the Spec completed?"
/// while six findings stay open is asking for a consent the human cannot give informed, and
/// [`deferred_findings_note`] only spoke AFTER the transition had fired. Unbound run → 0.
const QUERY_SPEC_DEFERRED_FINDINGS: &str = "spec_deferred_findings";
/// The bound Spec's COUNT of the OPEN tasks the coder child can WORK — the same
/// [`db::reopens_the_build`] predicate as `spec_open_tasks` (INCLUDING non-deferred review findings),
/// SCOPED to the coder's active phase
/// ([`crate::mcp_context::current_build_phase_number`], the lowest phase with open work) and falling
/// back to the phase-less work when the plan holds none ([`selectable_open_tasks`]). An INNER, per-phase
/// loop reads it to keep building until there is nothing left to build, while `spec_open_tasks` governs
/// the OUTER whole-Spec loop. Unbound run → 0 (v2.8 lot 2).
const QUERY_PHASE_OPEN_TASKS: &str = "phase_open_tasks";
/// The bound Spec's NEXT open task for the coder child — the id of the first non-terminal task
/// (`status ∉ {done, cancelled}`, in `list_tasks_for_spec` order) of the set `phase_open_tasks` counts
/// ([`selectable_open_tasks`]), or `null` when there is nothing left / the run is unbound. The twin of
/// `phase_open_tasks` at the TASK grain: the coder loop reads it to pick the NEXT task to drive (start →
/// gate → done) as a NODE, not prose (v2.8 lot 2).
const QUERY_PHASE_NEXT_TASK: &str = "phase_next_task";
/// Whether the bound Spec's prototype carries a whole-design FREEZE (a `design_lock` exists on its
/// prototype). Lets a graph branch on "the design is locked" — the prototyper hands off to the coder
/// only once the composition is frozen. Unbound run / no prototype / no lock → false (v2.8 lot 2).
const QUERY_DESIGN_LOCKED: &str = "design_locked";
/// The COUNT of pages the bound Spec's prototype puts to the ELECTION in its CURRENT round — the
/// pinned manifest (the `prototype_round_pages` of [`db::current_round`]). The SAME authority the UI
/// reads: [`db::proto_tree`] resolves its `manifest_entry` / `effective_variant_id` over exactly this
/// set, and [`crate::mcp_tools_proto::proto_distill`] refuses to freeze a round when it is empty.
/// Unbound run / no prototype / no round → 0. Its companion `<write>_reason` NAMES the pages and the
/// variants on offer, so the question put to the human can SHOW the choices.
///
/// It exists because a round could be EMPTY and nothing checked (L2). Observed on the prototyper's
/// first NOMINAL run: the effector laid down 2 pages and a throwaway probe variant, pinned NOTHING,
/// and the graph asked the human to "elect the RETAINED variant per page" over a round holding no
/// page at all. `design_locked` cannot see that — it answers "is the design frozen", which is equally
/// false of an empty round and of one still being argued over — so the graph had no fact for "is
/// there anything to elect". This is it.
const QUERY_DESIGN_ROUND_PAGES: &str = "design_round_pages";
/// A TASK-GRAINED fact (parameterized by the node's `task_key`, default `task_id`): HOW the task that
/// key names can be closed RIGHT NOW — `"done"`, `"verify"`, `"escalate"`, or `null`. It is the task
/// lifecycle's own rules, READ instead of guessed: the coder graph used to close every task with an
/// unconditional `done`, which the guard-rails refuse for a `kind=human` task and for a task the coder
/// BLOCKED — and a refused `transition_task` replays, so the run re-raised the same `invalid_state` on
/// every later tick, forever (H9). Its companion `<write>_reason` names the task and, when it is
/// blocked, the coder's own reason. See [`resolve_task_close_route`] for the four answers.
const QUERY_TASK_CLOSE_ROUTE: &str = "task_close_route";
/// The PROJECT-level root folder this run's dev workspaces land under
/// ([`db::Project::worktrees_root`]) — the WHERE of a workspace, read as a FACT so the graph can
/// branch on "is it configured?" instead of asking a question it may not need. The project is
/// resolved through the run's Spec OR its WORKSPACE ([`run_workspace_project_id`]), so a run that
/// carries a workspace but no Spec yet still reads the root its project configured. Unset / no
/// project at all → the EMPTY STRING, which is falsy for a `truthy: false` branch AND interpolates to
/// nothing in a path (a `null` would interpolate to the literal text `null` and build a folder named
/// after it — the one way this fact could do damage).
const QUERY_PROJECT_WORKTREES_ROOT: &str = "project_worktrees_root";
/// Whether the folder a `create_workspace` is ABOUT to open is still FREE — the prevalidation a graph
/// reads BEFORE the transition that commits the Spec, so "mark it ready AND open a workspace" is not
/// left half-done when the folder is taken ([`resolve_workspace_target_free`]). Parameterized by the
/// board keys the guarded node itself reads (`root_key` / `name_key`), and its companion
/// `<write>_reason` NAMES the folder and, when it is taken, the workspace already holding it.
const QUERY_WORKSPACE_TARGET_FREE: &str = "workspace_target_free";

/// WHY a `query` fact read on such a run can never move again — the sentence a dead loop's failure
/// carries to the human ([`Engine::freeze_key`]).
const UNBOUND_SPEC_FACT_FROZEN: &str =
    "the run carries no Spec and can no longer acquire one (a sub-workflow child inherits its \
     parent's Spec at creation), so every Spec fact it reads is a constant";

/// Whether this run's Spec binding is SETTLED at "none" — so every `query` fact it reads is the
/// unbound constant (`null` / `0` / `false`) rather than a value that may still turn (H3).
///
/// Two conditions, and both are needed. It carries no Spec — every fact in the `query` vocabulary
/// reads the BOUND Spec, so the binding is the whole question. And it can never acquire one: a
/// TOP-LEVEL run still can, since F5 binds an authoring run to the Spec its own graph creates, but a
/// `sub_workflow` CHILD inherits its parent's Spec AT CREATION and is deliberately excluded from that
/// late binding ([`bind_session_run_to_spec`]) — for a child, unbound is final.
fn spec_binding_is_settled_absent(run: &db::WorkflowRun) -> bool {
    run.spec_id.is_none() && run.parent_run_id.is_some()
}

/// Resolve a `query` node's fact NAME against real nyx state, returning the JSON value `drive` folds
/// onto the blackboard. `spec_status` yields the bound Spec's status (or `null` when the run is not
/// bound to a Spec — an unbound run has no status to read, not an error); `spec_category` its
/// category SLUG, on the same terms. An UNKNOWN fact name is a mis-authored node, refused so it
/// surfaces rather than silently branching on `null` (B2).
fn resolve_query(db: &Db, run: &db::WorkflowRun, query: &str) -> Result<Value, RpcError> {
    match query {
        QUERY_SPEC_STATUS => {
            let Some(spec_id) = run.spec_id.as_deref() else {
                return Ok(Value::Null);
            };
            let status = db
                .with_conn(|c| db::get_spec(c, spec_id))
                .map_err(|e| RpcError::new(RpcCode::Internal, format!("query spec_status: {e}")))?
                .map(|s| Value::from(s.status))
                .unwrap_or(Value::Null);
            Ok(status)
        }
        QUERY_SPEC_CATEGORY => {
            // The bound Spec's category SLUG, or `null` — written either way (see the const's doc:
            // a selector must be able to test the key, and the two ways to have no category are one
            // spelling of absence). An unbound run answers `null` and the walk continues: a run with
            // no Spec has no genre of work, which is a fact and not a failure.
            let Some(spec_id) = run.spec_id.as_deref() else {
                return Ok(Value::Null);
            };
            let slug = db
                .with_conn(|c| -> Result<Option<String>, diesel::result::Error> {
                    let Some(spec) = db::get_spec(c, spec_id)? else {
                        return Ok(None);
                    };
                    let Some(category_id) = spec.category_id else {
                        return Ok(None);
                    };
                    Ok(db::get_category(c, &category_id)?.map(|category| category.slug))
                })
                .map_err(|e| {
                    RpcError::new(RpcCode::Internal, format!("query spec_category: {e}"))
                })?;
            Ok(slug.map(Value::from).unwrap_or(Value::Null))
        }
        QUERY_SPEC_OPEN_TASKS => {
            // COUNT the bound Spec's work that REOPENS THE BUILD ([`db::reopens_the_build`] — build
            // tasks AND the findings that buy another round). An unbound run has no tasks — 0
            // (nothing to keep building for), never an error.
            let Some(spec_id) = run.spec_id.as_deref() else {
                return Ok(Value::from(0));
            };
            let open = db
                .with_conn(|c| db::list_tasks_for_spec(c, spec_id))
                .map_err(|e| {
                    RpcError::new(RpcCode::Internal, format!("query spec_open_tasks: {e}"))
                })?
                .iter()
                .filter(|t| db::reopens_the_build(t))
                .count();
            Ok(Value::from(open as i64))
        }
        QUERY_SPEC_OPEN_FINDINGS => {
            // COUNT the bound Spec's REOPENING tasks of kind `review` — the audit findings ALONE (a
            // build task never counts). The SAME [`db::reopens_the_build`] predicate as
            // `spec_open_tasks`, so the two facts can never disagree on what "open" means — nor on
            // which finding is DEFERRED, which is what keeps a low from re-arming a verdict that
            // costs a coder sub-workflow plus four audit passes. An unbound run — 0.
            let Some(spec_id) = run.spec_id.as_deref() else {
                return Ok(Value::from(0));
            };
            let open = db
                .with_conn(|c| db::list_tasks_for_spec(c, spec_id))
                .map_err(|e| {
                    RpcError::new(RpcCode::Internal, format!("query spec_open_findings: {e}"))
                })?
                .iter()
                .filter(|t| t.kind == db::TASK_KIND_REVIEW && db::reopens_the_build(t))
                .count();
            Ok(Value::from(open as i64))
        }
        QUERY_SPEC_DEFERRED_FINDINGS => {
            // COUNT what the Spec would SHIP with: the open findings the build deliberately LEFT
            // (`low`/`info`), read through the SAME predicate `validate_done` reports them by — so the
            // number the closing question shows and the one the completion note states are one fact,
            // not two counts that can drift. An unbound run — 0.
            let Some(spec_id) = run.spec_id.as_deref() else {
                return Ok(Value::from(0));
            };
            let deferred = db
                .with_conn(|c| db::list_tasks_for_spec(c, spec_id))
                .map_err(|e| {
                    RpcError::new(
                        RpcCode::Internal,
                        format!("query spec_deferred_findings: {e}"),
                    )
                })?
                .iter()
                .filter(|t| db::is_deferred_finding(t))
                .count();
            Ok(Value::from(deferred as i64))
        }
        QUERY_PHASE_OPEN_TASKS => {
            // COUNT the open work the coder child can actually WORK. An unbound run has nothing to
            // keep building for → 0, never an error.
            let open = selectable_open_tasks(db, run, QUERY_PHASE_OPEN_TASKS)?;
            Ok(Value::from(open.len() as i64))
        }
        QUERY_PHASE_NEXT_TASK => {
            // The id of the NEXT open task the child can work (the first in `list_tasks_for_spec`
            // order), or `null` when there is none. The twin of `phase_open_tasks` at the TASK grain —
            // over the very same set, so counting and selecting can never disagree.
            let next = selectable_open_tasks(db, run, QUERY_PHASE_NEXT_TASK)?
                .into_iter()
                .next()
                .map(|t| Value::from(t.id))
                .unwrap_or(Value::Null);
            Ok(next)
        }
        QUERY_DESIGN_LOCKED => {
            // Whether the bound Spec's prototype carries a WHOLE-DESIGN freeze. Unbound run / no
            // prototype / no lock → false (nothing frozen), never an error.
            let Some(spec_id) = run.spec_id.as_deref() else {
                return Ok(Value::from(false));
            };
            let locked = db
                .with_conn(|c| {
                    let Some(proto) = db::get_prototype_for_spec(c, spec_id)? else {
                        return Ok::<bool, diesel::result::Error>(false);
                    };
                    Ok(db::get_design_lock(c, &proto.id)?.is_some())
                })
                .map_err(|e| {
                    RpcError::new(RpcCode::Internal, format!("query design_locked: {e}"))
                })?;
            Ok(Value::from(locked))
        }
        QUERY_PROJECT_WORKTREES_ROOT => {
            // The project's configured worktrees root, or "" when unset / unbound. EMPTY STRING, not
            // `null`, on purpose: this fact is read for TWO things at once — a `truthy: false` branch
            // ("nothing configured — ask the human") and a `{{key}}` interpolation into the folder
            // path — and `null` is falsy for the first while rendering as the literal `null` for the
            // second. One spelling of "unset" that is safe on both readers (the F2 lesson: an absent
            // value must have ONE spelling).
            //
            // THE PROJECT IS READ THE WIDE WAY HERE, and only here: the Spec if there is one, else the
            // WORKSPACE the run was opened against ([`run_workspace_project_id`]). A `custom` run, and
            // a `/nspec` run before its Spec exists, carry a workspace but no Spec — and answering ""
            // for them made the seeded selector ask the human for a root the project had already
            // frozen. The widening stays on this fact and does NOT move into `run_project_id`, whose
            // two other consumers decide what a run may DO (see its doc).
            let Some(project_id) = run_workspace_project_id(db, run)? else {
                return Ok(Value::from(""));
            };
            let root = db
                .with_conn(|c| db::get_project(c, &project_id))
                .map_err(|e| {
                    RpcError::new(
                        RpcCode::Internal,
                        format!("query project_worktrees_root: {e}"),
                    )
                })?
                .and_then(|p| p.worktrees_root)
                .unwrap_or_default();
            Ok(Value::from(root))
        }
        // The RUNTIME's own refusal, and it reads [`QUERY_FACTS`] like the authoring one: a listing
        // recopied by hand is a listing that drifts, and this sentence is reached precisely when
        // somebody needs it to be complete.
        other => Err(RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "unknown query '{other}' (expected one of: {})",
                QUERY_FACTS.join(", ")
            ),
        )),
    }
}

/// What the bound Spec's prototype puts to the ELECTION right now — the COUNT of pages pinned into
/// its CURRENT round, plus the sentence NAMING them and the variants each offers (folded to
/// `<write>_reason` by `drive`) — the fact behind [`QUERY_DESIGN_ROUND_PAGES`] (L2).
///
/// It reads the round through [`db::current_round`] + [`db::list_round_pages`], which is the manifest
/// [`db::proto_tree`] serves the UI from and the one [`crate::mcp_tools_proto::proto_distill`] refuses
/// to freeze when it is empty — so "what the human is shown", "what a graph may elect over" and "what
/// can be frozen" are ONE set, and cannot disagree about whether a round holds anything.
///
/// The listing is the other half of the fix. Asking a human to elect while naming nothing is asking
/// them to answer from a surface nyx never showed them; the pages and their variants are only knowable
/// at the moment of asking, so they are read HERE, one node before the question, exactly as ndev's
/// closing confirm reads `spec_deferred_findings`.
///
/// Unbound run / no prototype / no round → `0`, and a sentence saying which of those it is. Never an
/// error: the absence of a prototype is a legitimate state, not a failure.
fn resolve_design_round_pages(db: &Db, run: &db::WorkflowRun) -> Result<(Value, String), RpcError> {
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Ok((
            Value::from(0),
            "the run carries no Spec, so there is no prototype to elect from".to_string(),
        ));
    };
    let (count, listing) = db
        .with_conn(|c| -> Result<(usize, String), diesel::result::Error> {
            let Some(proto) = db::get_prototype_for_spec(c, spec_id)? else {
                return Ok((0, "the Spec has no prototype yet".to_string()));
            };
            let Some(round) = db::current_round(c, &proto.id)? else {
                return Ok((0, "the prototype has no round yet".to_string()));
            };
            let manifest = db::list_round_pages(c, &round.id)?;
            if manifest.is_empty() {
                return Ok((
                    0,
                    format!(
                        "round '{}' pins NO page — nothing was composed to elect on",
                        round.label
                    ),
                ));
            }
            let mut pages = Vec::new();
            for entry in &manifest {
                let title = db::get_page(c, &entry.page_id)?
                    .map(|p| format!("'{}' ({})", p.title, p.slug))
                    .unwrap_or_else(|| entry.page_id.clone());
                let variants = db::list_variants(c, &entry.page_version_id)?;
                let retained = db::effective_retained_variant(c, &round.id, &entry.page_id)?;
                let offered = variants
                    .iter()
                    .map(|v| {
                        if retained.as_deref() == Some(v.id.as_str()) {
                            format!("{} [retained]", v.label)
                        } else {
                            v.label.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                pages.push(match offered.is_empty() {
                    true => format!("{title} — NO variant"),
                    false => format!("{title} — {offered}"),
                });
            }
            Ok((
                manifest.len(),
                format!(
                    "round '{}' puts {} page(s) to the election: {}",
                    round.label,
                    manifest.len(),
                    pages.join(" ; ")
                ),
            ))
        })
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("query design_round_pages: {e}")))?;
    Ok((Value::from(count as i64), listing))
}

/// The CLOSE route the task lifecycle allows for `task_id` right now, plus the sentence explaining it
/// (folded to `<write>_reason` by `drive`) — the fact behind [`QUERY_TASK_CLOSE_ROUTE`] (H9).
///
/// FOUR answers, and each names a move that is LEGAL at the moment it is read — that is the whole
/// point of the fact. The coder graph closed every task with an unconditional `done`, and the two
/// tasks `done` does not close both came up in ONE live session: a `kind=human` task the coder rightly
/// refused ("a kind=human task closes only through the verified path") and a review finding the coder
/// itself blocked ("task is 'blocked', which cannot transition to 'done'"). Both left the child run
/// re-raising the same `invalid_state` at every tick, with no way out from inside the graph.
///
///   * `"done"` — an agent task IN PROGRESS: the plain close.
///   * `"verify"` — a `kind=human` task IN PROGRESS carrying a `verification_command`: it closes ONLY
///     through the verified path. Read BEFORE the work, this is also how the graph knows the task is
///     the HUMAN's to do and must never be handed to the coder.
///   * `"escalate"` — no close is legal: the task is BLOCKED (the coder refused it — its `why` rides
///     the reason), or it is a `kind=human` task nothing can certify (no `verification_command`).
///     Only a human can resolve it, which is what the escalation gate is for.
///   * `null` — nothing to close: no task named, unknown id, or already terminal (done/cancelled).
fn resolve_task_close_route(db: &Db, task_id: Option<&str>) -> Result<(Value, String), RpcError> {
    let Some(task_id) = task_id.filter(|id| !id.is_empty()) else {
        return Ok((Value::Null, "no task is named on the board".to_string()));
    };
    let Some(task) = db
        .with_conn(|c| db::get_task(c, task_id))
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("query task_close_route: {e}")))?
    else {
        return Ok((Value::Null, format!("unknown task {task_id}")));
    };
    let titled = format!("'{}'", task.title);
    if task.status == db::TASK_STATUS_DONE || task.status == db::TASK_STATUS_CANCELLED {
        return Ok((
            Value::Null,
            format!("{titled} is already {} — nothing to close", task.status),
        ));
    }
    if task.status == db::TASK_STATUS_BLOCKED {
        let why = task
            .blocked_reason
            .as_deref()
            .unwrap_or("no reason was recorded");
        return Ok((
            Value::from("escalate"),
            format!("{titled} is BLOCKED — the reason given: {why}"),
        ));
    }
    if task.status != db::TASK_STATUS_IN_PROGRESS {
        return Ok((
            Value::Null,
            format!(
                "{titled} is '{}' — it was never started, so no close applies yet",
                task.status
            ),
        ));
    }
    if task.kind == db::TASK_KIND_HUMAN {
        return Ok(match task.verification_command.as_deref() {
            Some(cmd) => (
                Value::from("verify"),
                format!("{titled} is a HUMAN action, verified by: {cmd}"),
            ),
            // A human task nobody can verify closes through neither path — the guard-rail refuses the
            // plain `done` AND the verified one. It is a mis-created task, and only a human can rule.
            None => (
                Value::from("escalate"),
                format!("{titled} is a HUMAN action carrying NO verification_command — nothing can certify it"),
            ),
        });
    }
    Ok((
        Value::from("done"),
        format!("{titled} is an agent task in progress"),
    ))
}

/// The bound Spec's OPEN tasks the coder child can SELECT — the ONE set `phase_open_tasks` counts and
/// `phase_next_task` picks from, in `list_tasks_for_spec` order. Empty for an unbound run / unknown
/// Spec (nothing to build, never an error).
///
/// F7 — THE COUNTING END AND THE SELECTING END MUST ANSWER OVER THE SAME SET. The ndev build gate reads
/// `spec_open_tasks` while the coder child reads these two, which filtered on the CURRENT BUILD phase
/// alone. A review finding legitimately carries NO phase (it is not part of the plan — the auditor's
/// `task_add` files it un-phased), so the parent COUNTED work its child could not SEE: it opened a
/// coder child that immediately read `task_id: null` and exited without building, the outer `phases`
/// loop recounted the same findings and re-opened another, until `max_iterations`. A spin, on work
/// nobody could reach.
///
/// So when the plan has no current phase left — every phased task terminal, or a flat Spec with no plan
/// at all — the child falls back to the work that carries no phase, and WORKS the findings instead of
/// exiting on them. `current_build_phase_number` only ever names a phase that still HAS open work, so
/// this set is empty exactly when `spec_open_tasks` is 0: the two ends now agree on "there is nothing
/// left", which is what makes the spin impossible rather than merely rarer.
///
/// That agreement is what makes [`db::reopens_the_build`] load-bearing HERE and not merely tidy: the
/// narrowing is applied BEFORE `current_build_phase_number` reads the list, so the phase the child
/// drains is resolved over the very set the parent counted. Filter the phase first and a Spec whose
/// only remaining work in the lowest phase is a DEFERRED finding would name that phase current, hand
/// the child an empty selection, and re-open the F7 spin against a count that had already dropped to
/// zero.
fn selectable_open_tasks(
    db: &Db,
    run: &db::WorkflowRun,
    query: &str,
) -> Result<Vec<db::Task>, RpcError> {
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Ok(Vec::new());
    };
    let db_err = |e| RpcError::new(RpcCode::Internal, format!("query {query}: {e}"));
    let Some(spec) = db
        .with_conn(|c| db::get_spec(c, spec_id))
        .map_err(db_err)?
    else {
        return Ok(Vec::new());
    };
    let open: Vec<db::Task> = db
        .with_conn(|c| db::list_tasks_for_spec(c, spec_id))
        .map_err(db_err)?
        .into_iter()
        .filter(db::reopens_the_build)
        .collect();
    match crate::mcp_context::current_build_phase_number(&spec.phases, &open) {
        // A phase is still open: the child drains THAT phase, findings of that phase included —
        // unchanged, and the nominal case.
        Some(phase) => Ok(open.into_iter().filter(|t| t.phase == Some(phase)).collect()),
        // No phase left to build: what remains carries no phase (that is WHY no phase resolved), and
        // it is the parent's `spec_open_tasks` count. The child works it.
        None => Ok(open),
    }
}

/// Resolve `spec_can_transition`: whether the bound Spec CAN take `target` NOW, plus a human-readable
/// reason (written to `<write>_reason` by `drive`). Legality comes from the current status
/// ([`db::SpecTransition::source_allowed`]); for `ready` the QUALITY gate ([`db::validate_ready`]) and
/// for `done` the COMPLETION gate ([`db::validate_done`] — open work blocks closure, N32; a DEFERRED
/// finding does not, and is named in the reason instead) run too. An
/// unbound run / unknown Spec cannot transition (a reason says so). A missing `target` is a
/// mis-authored node (v2.8 review).
fn resolve_can_transition(
    db: &Db,
    run: &db::WorkflowRun,
    target: Option<&str>,
) -> Result<(bool, String), RpcError> {
    let Some(target) = target else {
        return Err(RpcError::new(
            RpcCode::InvalidArgument,
            "spec_can_transition needs a 'target' (the transition to check)",
        ));
    };
    let transition = parse_target_transition(target)?;
    let Some(spec_id) = run.spec_id.as_deref() else {
        return Ok((false, "the run is not bound to a Spec".to_string()));
    };
    let Some(spec) = db
        .with_conn(|c| db::get_spec(c, spec_id))
        .map_err(|e| RpcError::new(RpcCode::Internal, format!("spec_can_transition: {e}")))?
    else {
        return Ok((false, format!("unknown spec {spec_id}")));
    };
    if !transition.source_allowed(&spec.status) {
        return Ok((
            false,
            format!("'{target}' is not legal from status '{}'", spec.status),
        ));
    }
    if transition == db::SpecTransition::Ready {
        let failures = db
            .with_conn(|c| db::validate_ready(c, spec_id))
            .map_err(|e| RpcError::new(RpcCode::Internal, format!("spec_can_transition: {e}")))?;
        if !failures.is_empty() {
            let reasons = failures
                .iter()
                .map(|f| format!("{f:?}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Ok((false, format!("not ready: {reasons}")));
        }
    }
    if transition == db::SpecTransition::Done {
        let failures = db
            .with_conn(|c| db::validate_done(c, spec_id))
            .map_err(|e| RpcError::new(RpcCode::Internal, format!("spec_can_transition: {e}")))?;
        let reasons = || {
            failures
                .iter()
                .map(|f| format!("{f:?}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        // Only a BLOCKING finding answers "no" — gating on `!failures.is_empty()` would route the
        // graph away from the very completion the deferral was decided to allow. A REPORTED one
        // answers YES and rides the reason, which `drive` folds to `<write>_reason`: the board the
        // visualiser shows then carries, in the run's own record, that this Spec completed carrying
        // findings.
        if failures.iter().any(db::DoneValidation::blocks_completion) {
            return Ok((false, format!("not complete: {}", reasons())));
        }
        if !failures.is_empty() {
            return Ok((true, format!("complete, carrying: {}", reasons())));
        }
    }
    Ok((true, "ok".to_string()))
}

/// EVERY nyx fact a `query` node may name — THE accepted set, held ONCE, and the ORDER every listing
/// prints (this array's own; the facts are NOT all resolved in one place, so no resolver can be their
/// order). THREE readers, no fourth: the acceptance test ([`valid_query`]), the AUTHORING refusal
/// ([`crate::mcp_authoring::validate_node_config`]) and the RUNTIME refusal ([`resolve_query`]'s
/// `other` arm) — because a listing recopied by hand drifts from the set it describes, and both
/// refusals are read exactly when somebody needs the list to be right.
///
/// It is written down because it happened INSIDE this PRD: `project_worktrees_root` reached the
/// resolver while both refusal sentences still enumerated ten facts by hand, so an author who
/// mistyped the eleventh was handed a list missing the very fact they wanted. Nothing shipped that
/// way — the drift lived in a working tree — and the NEXT fact costs one line here and none anywhere
/// else (deliberately un-numbered: a count in this comment is one more thing that goes stale, and
/// `workspace_target_free` landed the day after and made the first one wrong).
pub(crate) const QUERY_FACTS: &[&str] = &[
    QUERY_SPEC_STATUS,
    QUERY_SPEC_CATEGORY,
    QUERY_SPEC_CAN_TRANSITION,
    QUERY_SPEC_OPEN_TASKS,
    QUERY_SPEC_OPEN_FINDINGS,
    QUERY_SPEC_DEFERRED_FINDINGS,
    QUERY_PHASE_OPEN_TASKS,
    QUERY_PHASE_NEXT_TASK,
    QUERY_TASK_CLOSE_ROUTE,
    QUERY_DESIGN_ROUND_PAGES,
    QUERY_PROJECT_WORKTREES_ROOT,
    QUERY_WORKSPACE_TARGET_FREE,
    QUERY_DESIGN_LOCKED,
];

/// Whether `query` names a fact a `query` node can read — the AUTHORING-side check (validate_node_config)
/// behind the "query 'banane'" refusal. Shares the ONE accepted set ([`QUERY_FACTS`]) with the runtime,
/// so the two never drift (v2.8 review B2 + spec_can_transition).
pub(crate) fn valid_query(query: &str) -> bool {
    QUERY_FACTS.contains(&query)
}

/// Whether `run_id` carries the HUMAN's authorization to write a section of `spec_id` while the Spec
/// is out of its writable state — the check `spec_update_section` runs before it takes the ONE
/// exception to the freeze (H12).
///
/// THE FREEZE STAYS THE RULE; THE HUMAN IS ITS ONLY EXCEPTION. So this asks for three things at once,
/// and none of them is the caller's word:
///   * the run EXISTS and is still LIVE — a permission does not outlive the flow that was given it;
///   * it drives THIS Spec (`runs.spec_id`, set by nyx when the run — or its parent — was opened), so
///     a grant on one build can never authorize a write on another Spec;
///   * its blackboard carries [`crate::engine::SPEC_WRITE_GRANT_KEY`], which ONLY the engine writes,
///     and only from a human's answer to an `ask_human` declaring `grant_spec_write_on` (the coder
///     graph's escalation gate: "the contradiction is in the Spec's TEXT — fix the Spec").
///
/// That last point is the whole design. The authority is the HUMAN's answer; the engine merely records
/// it. So the day sensitive answers arrive from the nyx UI instead of an agent's message, this
/// authorization becomes unforgeable with them — nothing here has to change for that to be true.
pub(crate) fn spec_section_write_is_authorized(db: &Db, run_id: &str, spec_id: &str) -> bool {
    let Ok(Some(run)) = db.with_conn(|c| db::get_workflow_run(c, run_id)) else {
        return false;
    };
    if db::run_status_is_terminal(&run.status) || run.spec_id.as_deref() != Some(spec_id) {
        return false;
    }
    Blackboard::from_json_str(&run.blackboard)
        .get(crate::engine::SPEC_WRITE_GRANT_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Resolve a `transition` node's `target_state` (a Spec STATUS, or its conceptual verb) to the
/// typed [`db::SpecTransition`]. Accepts both the canonical status (`in_review`) and the verb
/// (`review`), so an authored node reads naturally either way.
fn parse_target_transition(target: &str) -> Result<db::SpecTransition, RpcError> {
    Ok(match target {
        "ready" => db::SpecTransition::Ready,
        "in_progress" | "start" => db::SpecTransition::Start,
        "in_review" | "review" => db::SpecTransition::Review,
        "completed" | "done" | "complete" => db::SpecTransition::Done,
        "cancelled" | "cancel" | "canceled" => db::SpecTransition::Cancel,
        other => {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "unknown transition target_state '{other}' (expected a Spec status or its verb)"
                ),
            ))
        }
    })
}

/// Whether `target` names a VALID Spec transition (a status or its verb). The AUTHORING-side check
/// behind M1/M2's "transition vers 'banane'" refusal — it shares the ONE accepted set with
/// [`parse_target_transition`] (the runtime check), so the two never drift.
pub(crate) fn valid_transition_target(target: &str) -> bool {
    parse_target_transition(target).is_ok()
}

/// The `transition_task` verb that closes a `kind=human` task through the VERIFIED path
/// ([`db::verify_human_task`]) rather than the plain `done` the human guard-rail refuses. It is NOT a
/// [`db::TaskTransition`] — it is the separate authority — so it is resolved by name in the drive arm
/// (H9). The `task_transition` MCP tool has always accepted it; the graph vocabulary was the one place
/// missing it, which is why a `kind=human` task had no closing node at all.
const TASK_TRANSITION_VERIFY: &str = "verify";

/// Resolve a `transition_task` node's `transition` verb to the typed [`db::TaskTransition`], or `None`
/// for an unknown verb (`verify` included — see [`TASK_TRANSITION_VERIFY`]). The task-level twin of
/// [`parse_target_transition`] — the ONE accepted set the runtime drive and the authoring
/// [`valid_task_transition`] share so they never drift (v2.8 lot 2).
fn parse_task_transition(transition: &str) -> Option<db::TaskTransition> {
    Some(match transition {
        "start" => db::TaskTransition::Start,
        "done" => db::TaskTransition::Done,
        "block" => db::TaskTransition::Block,
        "skip" => db::TaskTransition::Skip,
        _ => return None,
    })
}

/// Whether `transition` names a VALID task lifecycle move (start/done/block/skip, or the human
/// `verify`) — the AUTHORING-side check behind the "transition_task 'banane'" refusal, sharing the
/// accepted set with the runtime [`parse_task_transition`] so the two never drift (v2.8 lot 2, H9).
pub(crate) fn valid_task_transition(transition: &str) -> bool {
    transition == TASK_TRANSITION_VERIFY || parse_task_transition(transition).is_some()
}

/// Whether `target` names an ENGAGING transition (ready/done/cancel) — the AUTHORING layer uses this
/// to require a `gate_on` field AND a human gate REACHABLE on the branch that leads to the node
/// ([`crate::mcp_authoring::sensitive_node_gate`]), sharing the classification with the runtime so the
/// two never drift (v2.8 review B3, superseding M-a).
pub(crate) fn transition_is_engaging_target(target: &str) -> bool {
    parse_target_transition(target)
        .map(transition_is_engaging)
        .unwrap_or(false)
}

// ============================================================================
// Result decode + action / outcome serialization
// ============================================================================

/// Decode the dispatcher's reported `last_result` JSON into the [`NodeResult`] the parked leaf
/// expects. Lenient: a named field (`diff` / `verdict` / `decision` / `artifact`) is preferred, the
/// whole value is the fallback, and the effect-only leaves ack.
fn decode_result(node_type: NodeType, last: Option<&Value>) -> NodeResult {
    match node_type {
        NodeType::SpawnSubagent => {
            let diff = last
                .and_then(|v| v.get("diff"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| last.and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            NodeResult::Diff(diff)
        }
        NodeType::Judge => NodeResult::Verdict(pick(last, "verdict").unwrap_or(Value::Null)),
        // An ask_human answer has its OWN reader: the whole-object fallback `pick` applies is never a
        // valid human answer (F4 — see [`human_answer`]).
        NodeType::AskHuman => NodeResult::Human(human_answer(last).unwrap_or(Value::Null)),
        NodeType::Produce => NodeResult::Artifact(pick(last, "artifact").unwrap_or(Value::Null)),
        // wait / note (and, defensively, any nyx-own leaf that never actually parks) just ack.
        _ => NodeResult::Ack,
    }
}

/// A named field of `last`, or the whole value when the field is absent.
fn pick(last: Option<&Value>, field: &str) -> Option<Value> {
    last.and_then(|v| v.get(field).cloned().or_else(|| Some(v.clone())))
}

/// The FIELD an `ask_human` answer is read from — the contract nyx STATES in the action it hands
/// back (`answer_key`), so a caller can see what to send instead of guessing (F4).
pub(crate) const ASK_HUMAN_ANSWER_KEY: &str = "decision";

/// The field names accepted BESIDE [`ASK_HUMAN_ANSWER_KEY`]. A LLM dispatcher naturally produces
/// `{"answer": …}` / `{"choice": …}` / `{"value": …}` / `{"selected": …}`; refusing those cost a live
/// run three blind round-trips (observed: `answer`, `choice`, `value`, then giving up). `decision` is
/// read FIRST — a payload carrying both is never ambiguous (F4).
const ASK_HUMAN_ANSWER_SYNONYMS: [&str; 4] = ["answer", "choice", "value", "selected"];

/// Read the human's answer out of the dispatcher's reported `last_result`, or `None` when the payload
/// has NO usable shape. Accepts a BARE scalar (`"yes"`), then [`ASK_HUMAN_ANSWER_KEY`], then its
/// [`ASK_HUMAN_ANSWER_SYNONYMS`], and takes the first key PRESENT with a scalar value.
///
/// Deliberately NOT [`pick`]: folding the WHOLE object when the key is missing is what made a
/// wrongly-keyed answer fail silently — the object then failed the N19 `options` check and the engine
/// re-presented the identical question forever. An object/array is never an answer to a question with
/// a closed answer set, so it is `None` here and the caller REFUSES OUT LOUD (F4).
fn human_answer(last: Option<&Value>) -> Option<Value> {
    let scalar = |v: &Value| (!v.is_object() && !v.is_array() && !v.is_null()).then(|| v.clone());
    let reported = last?;
    if !reported.is_object() {
        return scalar(reported);
    }
    std::iter::once(ASK_HUMAN_ANSWER_KEY)
        .chain(ASK_HUMAN_ANSWER_SYNONYMS)
        .find_map(|field| reported.get(field).and_then(scalar))
}

/// The EXPLICIT refusal a wrongly-shaped `ask_human` answer gets: it names the expected FIELD and,
/// when the node declares a closed set, the allowed VALUES — `ask_human expects {"decision": "<one
/// of: yes, no>"}`. The alternative (what shipped) was to discard the answer and re-present the same
/// node with no error and no message, unbounded: three dispatcher attempts saw nothing at all (F4).
fn ask_human_answer_error(options: &[String]) -> RpcError {
    let expected = if options.is_empty() {
        format!("{{\"{ASK_HUMAN_ANSWER_KEY}\": \"<your answer>\"}}")
    } else {
        format!(
            "{{\"{ASK_HUMAN_ANSWER_KEY}\": \"<one of: {}>\"}}",
            options.join(", ")
        )
    };
    RpcError::new(
        RpcCode::InvalidArgument,
        format!(
            "ask_human expects {expected} — the answer field is '{ASK_HUMAN_ANSWER_KEY}' (the \
             synonyms {} are accepted too); an object or an array is not an answer",
            ASK_HUMAN_ANSWER_SYNONYMS.join(" / ")
        ),
    )
    .with_data(json!({
        "reason": "ask_human_answer_shape",
        "answer_key": ASK_HUMAN_ANSWER_KEY,
        "options": options,
    }))
}

/// Serialize a dispatcher-facing [`Action`] to the JSON the verb returns. A `spawn_subagent` action
/// is enriched by the provider ADAPTER ([`crate::engine::spawn`]) with the resolved spawn MODE
/// (native `Task` vs inline fallback) so the dispatcher knows HOW to run it. `gate_check` /
/// `transition` are nyx's own inline effects and never reach here in the normal flow (kept
/// exhaustive).
///
/// A SPAWN SHIPS IDENTIFIERS, NEVER A MANUAL. It names WHO (`agent_id`), WHAT FOR (the node's
/// `label`), on WHICH OBJECT (`subject`, when the node declares one) and HOW (`mode`) — and the
/// sub-agent then calls `agent_context(run_id, role, subject?)` to pull its own role and this run's
/// state. That is the same model the `sub_workflow` action always used (a `run_id` plus three lines
/// telling the child to loop `nyx_next`), and it is what a spawn had been the exception to.
///
/// NO action carries a `node_id` (G5): the tick's top-level `current_node_id` is the SINGLE answer to
/// "where is the run", and the two were the same value at every stop by construction. The one node id
/// that survives is a sub-run's `child_node_id` — it names the CHILD's node while the cursor sits on
/// the parent's `sub_workflow` node, so it says something `current_node_id` cannot.
fn action_json(action: &Action, provider: &str) -> Value {
    match action {
        Action::SpawnSubagent {
            agent_id,
            label,
            inline,
            subject,
            ..
        } => {
            // A node that forces `inline` overrides the provider — an interactive effector runs
            // in-session (mode `inline`), never as a blind Task (v2.8 review N2).
            let spawner = crate::engine::spawn::spawner_for(provider, *inline);
            let mut json =
                crate::engine::spawn::SpawnInstruction::resolve(spawner.as_ref(), agent_id, label)
                    .to_json();
            // THE PER-NODE SUBJECT: the same role spawned over several objects is told WHICH one by
            // its node (the four ndev audit passes name a Spec commitment section each). Carried
            // ONLY when the node declares one — the sub-agent passes it straight back to
            // `agent_context`, which narrows the Spec block to that section.
            if let Some(subject) = subject {
                if let Some(obj) = json.as_object_mut() {
                    obj.insert("subject".to_string(), Value::from(subject.clone()));
                }
            }
            json
        }
        Action::Judge {
            prompt,
            output_schema,
            ..
        } => json!({
            "type": "judge", "prompt": prompt, "output_schema": output_schema,
        }),
        // `ask_human` builds its OWN dispatcher-facing action in `drive` (it must first record the
        // asking when the answer COMMITS something, H6/J2); this arm only keeps the match total.
        Action::AskHuman { prompt, options, .. } => ask_human_json(prompt, options, None, None),
        Action::Produce { .. } => json!({ "type": "produce" }),
        Action::Wait { .. } => json!({ "type": "wait" }),
        // A `note` never reaches the dispatcher as an action of its own: `drive` collects it and it
        // rides the next one under `notes` (G4, [`with_notes`]). This arm only keeps the match total.
        Action::Note { text, .. } => json!({ "type": "note", "text": text }),
        Action::GateCheck { .. } => json!({ "type": "gate_check" }),
        Action::Transition { target_state, .. } => {
            json!({ "type": "transition", "target_state": target_state })
        }
        // `query` and `fork_spec` are nyx-own inline effects resolved in `drive` — they never reach
        // the dispatcher; these arms only keep the match total.
        Action::Query {
            query,
            write,
            target,
            ..
        } => json!({
            "type": "query", "query": query, "write": write, "target": target,
        }),
        Action::ForkSpec { .. } => json!({ "type": "fork_spec" }),
        // `transition_task` is a nyx-own inline effect resolved in `drive` — it never reaches the
        // dispatcher; this arm only keeps the match total.
        Action::TransitionTask { .. } => json!({ "type": "transition_task" }),
        // `create_workspace` is a nyx-own inline effect resolved in `drive` — it never reaches the
        // dispatcher; this arm only keeps the match total.
        Action::CreateWorkspace { .. } => json!({ "type": "create_workspace" }),
        // `set_category` is a nyx-own inline effect resolved in `drive` — it never reaches the
        // dispatcher; the arm exists so the match stays total.
        Action::SetCategory { .. } => json!({ "type": "set_category" }),
        // `sub_workflow` builds its OWN dispatcher-facing action in `drive` ([`subworkflow_wait_json`],
        // which carries the child run_id + context); this arm only keeps the match total.
        Action::SubWorkflow { .. } => json!({ "type": "sub_workflow" }),
    }
}

/// Build the verb's success [`ToolOutcome`] from the run's live state. The run drives a Spec, so
/// this names [`ChangedTopic::Specs`] — the FLOOR every tick invalidates (any open Specs board, Spec
/// detail or engine visualiser re-pulls the run's live cursor).
///
/// IT IS A FLOOR, NOT THE WHOLE SET, AND TREATING IT AS THE WHOLE SET COST A WORKSPACE. `nyx_next`
/// is the one MCP verb that reaches SEVERAL domains behind one return value: it runs nyx's own
/// effects INLINE (`transition`, `transition_task`, `fork_spec`, `create_workspace`) and surfaces
/// only the action it STOPPED on, so the node that mutated and the action returned here are not the
/// same node — this function cannot see the difference, which is why [`drive`] accumulates the topics
/// during the walk ([`note_effect`]) and attaches them at its single exit. Three of the four inline
/// effects write inside the Spec domain, so the constant was right by luck; `create_workspace`
/// writes the WORKSPACE POOL, whose only view is the sidebar, and the sidebar re-pulls on
/// `workspaces://changed` alone. Observed on 2026-08-18: an `nspec` answered `go` created DEPOT-6,
/// anchored it, wrote the row and made the folder — and the sidebar stayed empty until the app was
/// relaunched, which reads exactly like a flow that failed.
///
/// THE SHELL'S HALF, for the record: the sidecar drains `effects` into one unsolicited
/// `Event::Changed` per topic (`emit_effects`, `nyx-sidecar/src/runtime/mcp.rs`), which the renderer
/// receives as `<topic>://changed`. It is the SAME wire the workspace TOOLS reach by another road —
/// they return a bare `Value`, so their push rides the post-commit `usecase::EventSink` the
/// dispatcher injects (a `PushEventSink`) instead. The graph calls those tools with a
/// [`crate::usecase::NullEventSink`] because no shell sink reaches that far down; this vec is the
/// seam that carries the same invalidation for it. See [`create_run_workspace`].
///
/// WHAT IT STILL DOES NOT COVER: a tick that mutates and THEN refuses hands back an `RpcError`,
/// which has no effects channel — the walk's invalidations die with it. Reachable only by authoring
/// a refusing node BELOW an effect node (in the seeded graphs `create_workspace` is the last node of
/// its sequence), and closing it would mean giving the error type a channel the shell reads.
///
/// N22 — the tick returns a `blackboard_delta` (only the keys that CHANGED this tick), NOT the whole
/// board every time. The full board is unbounded (a `last_diff` holds an effector's ENTIRE report),
/// and re-sending it on every tick was the dominant driver of the dispatcher's context growth (~30-40k
/// per nspec run). The delta sends each value ONCE, when it changes. The FULL board stays PERSISTED
/// (`persist` writes it), so the visualiser's read model — which reads it from the DB — is unaffected.
fn outcome(run: &db::WorkflowRun, engine: &Engine, action: Value, status: &str) -> ToolOutcome {
    let pre = Blackboard::from_json_str(&run.blackboard);
    let post = engine.blackboard();
    ToolOutcome::changed(
        json!({
            "run_id": run.id,
            "action": action,
            "current_node_id": engine.current_node_id(),
            "blackboard_delta": blackboard_delta(&pre, post),
            "blackboard_removed": blackboard_removed(&pre, post),
            "status": status,
        }),
        ChangedTopic::Specs,
    )
}

/// The keys of `post` whose value DIFFERS from `pre` (added or changed) — the per-tick blackboard
/// delta the `nyx_next` outcome returns (N22). A key that was REMOVED is reported BESIDE it by
/// [`blackboard_removed`] and never in here: a `null` in the delta could not be told apart from a key
/// deliberately set to `null`.
///
/// [`crate::engine::PROVENANCE_KEY`] is EXCLUDED from both directions, and it is the one key that is.
/// It is nyx's own record of who wrote what, read by nyx's own consent check and by nothing the
/// dispatcher does; it changes on nearly every write, and it holds one entry per board key, so
/// shipping it in the delta would re-send a growing object on almost every tick — the exact cost N22
/// exists to remove, paid for a value no caller acts on. It IS persisted in full with the board, and a
/// refusal that turns on it NAMES the origin in its message, so nothing about it is hidden from anyone
/// who needs it.
fn blackboard_delta(pre: &Blackboard, post: &Blackboard) -> Value {
    let mut delta = serde_json::Map::new();
    for (k, v) in post.as_map() {
        if k != crate::engine::PROVENANCE_KEY && pre.get(k) != Some(v) {
            delta.insert(k.clone(), v.clone());
        }
    }
    Value::Object(delta)
}

/// The keys `pre` held and `post` no longer does — this tick's REMOVALS, sorted so the payload is
/// deterministic. EMPTY on almost every tick, which is what keeps it cheap.
///
/// The forward diff alone rested on "the graphs only ever add / overwrite blackboard keys", and the
/// walk itself makes that false: it CLEARS [`crate::engine::SPEC_WRITE_GRANT_KEY`] (a `note` carrying
/// `revoke_spec_write`), [`BLOCKING_GATE_RED_KEY`] (the anchor of a gate that went green, or was
/// abandoned) and `__child_run_id__<node>` (a child run that reached a terminal state). A dispatcher
/// tracking the board from the deltas alone therefore went on reading an OPEN Spec-write window after
/// it had closed, a red blocking gate after it had cleared, and a child in flight after it had ended
/// — only the persisted full board was ever right, and it is not what the tick hands back.
fn blackboard_removed(pre: &Blackboard, post: &Blackboard) -> Value {
    let mut gone: Vec<&str> = pre
        .as_map()
        .keys()
        .map(String::as_str)
        .filter(|k| *k != crate::engine::PROVENANCE_KEY && !post.as_map().contains_key(*k))
        .collect();
    gone.sort_unstable();
    Value::Array(gone.into_iter().map(Value::from).collect())
}

/// Persist the run's live state (cursor + blackboard + status + traversal stack) AND journal the
/// stop — the single write each stop makes. The traversal is what lets a LATER session resume the
/// exact walk (B1).
///
/// `trace` is what the journal keeps beyond that state: the `type` of the action this stop hands the
/// dispatcher, and the `last_result` it decided on (F3). The two writes share ONE transaction
/// ([`db::persist_run_tick`]), so the audit trail can neither claim a step the run never took nor
/// miss one it did.
///
/// A run that is GONE by the time we write ([`db::persist_run_tick`] matching no row) is an ERROR, not
/// a quiet no-op: the walk really happened — nyx ran the gate, imposed the transition — and returning
/// the resulting action while nothing recorded it would hand the dispatcher a step to perform on a run
/// that does not exist. `run_delete` holds the same tick guard so it cannot be the cause; a cascade
/// from elsewhere still can, and this is where it stops being silent.
fn persist(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &Engine,
    cursor: Option<&str>,
    status: &str,
    trace: &db::TickTrace<'_>,
) -> Result<(), RpcError> {
    let blackboard = engine.blackboard_json();
    // The walk is kept for as long as the run can still be TICKED, and dropped exactly when it
    // cannot. The status being RECORDED is what says which of the two this is — not whether the
    // engine happened to reach the end of the tree, since a `Step::Done` that a red blocking gate
    // REFUSES (M5) is recorded `blocked` and must resume in the next session on that very refusal.
    let traversal = if db::run_status_is_terminal(status) {
        None
    } else {
        engine.export_traversal()
    };
    let persisted = db
        .with_conn(|c| {
            db::persist_run_tick(
                c,
                &run.id,
                cursor,
                &blackboard,
                status,
                traversal.as_deref(),
                trace,
            )
        })
        .map_err(internal_db)?;
    if persisted.is_none() {
        return Err(RpcError::new(
            RpcCode::InvalidId,
            format!(
                "run '{}' no longer exists — it was deleted while this tick was walking it, so \
                 nothing of what the tick did could be recorded; start a new run",
                run.id
            ),
        )
        .with_data(json!({ "reason": "run_vanished", "run_id": run.id })));
    }
    Ok(())
}

/// The action's `type` string — the ONLY part of a dispatcher-facing action the journal keeps. Never
/// its `context`: a spawn's rendered role manual runs to ~70k chars, and the outbox payload is small
/// by design (F3).
fn action_type(action: &Value) -> Option<&str> {
    action.get("type").and_then(Value::as_str)
}

/// Pair a stop's two journal facts for [`persist`]. A pair rather than two more positional arguments
/// so an action `type` can never land where the cursor goes.
fn tick_trace<'a>(
    action_type: Option<&'a str>,
    last_result: Option<&'a Value>,
) -> db::TickTrace<'a> {
    db::TickTrace {
        action_type,
        last_result,
    }
}

/// Mirror the in-memory red-gate anchor SET onto the PERSISTED blackboard ([`BLOCKING_GATE_RED_KEY`]),
/// sorted for a deterministic board, so it rides `persist` across the dispatcher loop. An EMPTY set
/// CLEARS the key entirely (no residue). Shared by the `gate_check` arm, the M5 re-eval, and the
/// abandon note, so the board and the in-memory set never drift.
///
/// The set is a [`BTreeSet`], so the array is written in the set's own order — the same order the
/// block points pick the refusing gate in, and the same order the re-eval re-runs them in.
fn mirror_blocking_gates(engine: &mut Engine, blocking: &BTreeSet<String>) {
    if blocking.is_empty() {
        engine.clear_board(BLOCKING_GATE_RED_KEY);
    } else {
        engine.set_board(
            BLOCKING_GATE_RED_KEY,
            Value::Array(blocking.iter().map(|id| Value::from(id.as_str())).collect()),
        );
    }
}

/// M5 — RE-RUN each still-anchored red BLOCKING gate at a block point (a `transition`/`done` about to
/// refuse): THE exit out of a run parked on a red gate. For every gate id in `blocking`, look its
/// `gate_check` node up in the workflow (the same `nodes` rows the tree is built from), re-resolve the
/// commands it arms ([`gate_commands`] — its own ids, or its slot's binding, which the human may have
/// just made), and re-run it over the PTY via [`run_gate`] (reusing M6's path — this REFRESHES
/// [`LAST_GATE_RESULT_KEY`], so the UI/dispatcher see the latest verdict). A gate that now passes GREEN
/// is DROPPED from the anchor; the mirror is updated. Net effect: fixing the cause (env / subfolder /
/// failing test) + calling `nyx_next` again re-runs the gate and, when green, clears the block and the
/// run advances — no cursor surgery, the re-eval is localised to the guard points. A gate STILL red
/// keeps its anchor, so the caller refuses as before. H1 is preserved: each gate clears only its own id.
///
/// `already_run` names the gates THIS TICK has just judged over the PTY, and they are SKIPPED. A
/// `sequence[gate_check, …, transition]` reaches this block point in the same walk as the gate it
/// holds, so re-running them here executed the project's whole test suite TWICE per tick: twice the
/// wait for the caller, twice the [`GATE_TOTAL_TIMEOUT`] a tick can hold its run (and an MCP worker)
/// for, and two runs of a check the graph asked for once. A verdict taken milliseconds ago IS the
/// current verdict. The re-eval exists for a gate that went red on an EARLIER call — the only kind
/// whose cause the caller can have fixed in between — and that is exactly what this leaves it doing.
fn reeval_red_gates(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    blocking: &mut BTreeSet<String>,
    already_run: &HashSet<String>,
    last_result: Option<&Value>,
) -> Result<(), RpcError> {
    if blocking.is_empty() || blocking.iter().all(|id| already_run.contains(id)) {
        return Ok(());
    }
    // The gate config is STATIC — read it from the same `nodes` rows the tree was reconstructed from.
    let nodes = db
        .with_conn(|c| db::list_nodes(c, &run.workflow_id))
        .map_err(internal_db)?;
    let project_id = run_project_id(db, run)?;
    // Snapshot the ids so the set can be mutated while iterating. ORDERED, because `blocking` is a
    // `BTreeSet`: the gate whose result lands last on [`LAST_GATE_RESULT_KEY`] is the same one from
    // one tick to the next, and it is the same one the caller then names in its refusal.
    for node_id in blocking.iter().cloned().collect::<Vec<_>>() {
        if already_run.contains(&node_id) {
            continue;
        }
        let Some(node) = nodes.iter().find(|n| n.id == node_id) else {
            // The gate node is gone from the workflow (a structural edit): it cannot be re-run, so it
            // stays anchored — an honest kept block, never a silent lift.
            continue;
        };
        let command_ids = node_gate_commands(db, project_id.as_deref(), node)?;
        // A RESOLUTION FAILURE IS AN ATTEMPT HERE TOO. The tick's own gate arm records it as
        // `indeterminate` and parks the run, with the reason "a silent hole in the journal is
        // exactly what an auditor cannot detect" — and this path used to propagate the bare `Err`
        // instead: no `gate_attempts` row, no `run.gate_attempt`, and no `persist` either, so the
        // run's status and cursor stayed on the previous state while the verb failed. The auditor
        // was then counting one attempt fewer than really happened, on a journal they are told to
        // read as complete.
        let outcome = match run_gate(db, &command_ids) {
            Ok(outcome) => outcome,
            Err(e) => {
                let mut commands = e.done;
                commands.push(GateCommandResult::could_not_run(
                    &e.failing_id,
                    "",
                    "",
                    &e.error.message,
                ));
                record_gate_attempt(
                    db,
                    &GateAttempt {
                        run,
                        node_id: &node_id,
                        slot: node_gate_slot(node).as_deref(),
                        expect: node_gate_expect(node).unwrap_or(GateExpectation::Green),
                        declared: node_gate_expect(node).is_some(),
                        blocking: true,
                        grade: GateGrade::Indeterminate,
                        satisfied: false,
                        commands: &commands,
                    },
                );
                // The anchor is KEPT (the set is mirrored on the way out of the tick by the park
                // itself), and the run is parked on the gate rather than erroring bare.
                return Err(block_on_effect_error(
                    db,
                    run,
                    engine,
                    &node_id,
                    last_result,
                    e.error,
                ));
            }
        };
        let green = outcome.passed;
        // WHAT THIS GATE DECLARES, read off the node rather than assumed. The anchor is raised only
        // by a gate that declared NOTHING — but it can be INHERITED by one that declares something
        // now, because `node_update` may add an `expect` to a gate a previous tick already anchored,
        // and this module's own expectation refusal names that edit as the route out.
        let declared = node_gate_expect(node);
        let expect = declared.unwrap_or(GateExpectation::Green);
        let satisfied = expect.is_satisfied_by(outcome.outcome);
        // A RE-RUN is an ATTEMPT, and it is journaled like any other: the M5 exit route works by
        // running the check again, so a red that lifts on the third re-eval is a three-attempt
        // sequence, and an auditor reading two of them would read a different story.
        if !outcome.commands.is_empty() {
            record_gate_attempt(
                db,
                &GateAttempt {
                    run,
                    node_id: &node_id,
                    slot: node_gate_slot(node).as_deref(),
                    expect,
                    declared: declared.is_some(),
                    blocking: true,
                    grade: outcome.outcome,
                    satisfied,
                    commands: &outcome.commands,
                },
            );
            // Keep M6's surfaced result fresh on every re-run (only a gate with REAL commands
            // writes it).
            engine.set_board_as(
                &node_id,
                LAST_GATE_RESULT_KEY,
                gate_result_json(&node_id, expect, satisfied, outcome),
            );
        }
        // WHAT LIFTS THE ANCHOR is what the node ASKED FOR — `satisfied` when the gate declares an
        // expectation, `green` when it declares none (and for an undeclared gate the two are the
        // same thing, since the default expectation IS green). Reading `green` unconditionally is
        // what wedged an inherited anchor on an `expect: red` gate: the reproduction went red, was
        // satisfied, was journaled satisfied — and the anchor held anyway, because a reproduction
        // does not pass. The run then refused its transition forever for a red the node had asked
        // for. See the `Some(_)` arm of the gate leaf, which drops the same anchor on the way in.
        if if declared.is_some() { satisfied } else { green } {
            blocking.remove(&node_id);
        }
    }
    mirror_blocking_gates(engine, blocking);
    Ok(())
}

/// THE commands a `gate_check` runs for THIS run — the ONE resolution every gate path goes through
/// (the tick that runs the gate, the M5 re-eval at a block point, and the `run_start` scan that warns
/// about a gate verifying nothing). A gate arms one way or the other and the callers must never hold
/// two notions of it, which is why this is a single function and not a match repeated three times.
///
///   * a `slot` gate asks the PROJECT what answers that slot ([`db::command_slot_ids`]) — the shape a
///     SHARED graph uses, since no graph every project runs can name `bun test`;
///   * otherwise the node's own `command_ids` stand — a graph that owns its commands names them.
///
/// EMPTY is the unarmed answer in both cases (an unbound slot, an empty list, a config nyx cannot even
/// decode), and every caller treats it identically: a BLOCKING gate on it refuses to advance. What
/// differs is only the ROUTE out, which [`arm_gate_hint`] reads off the same config.
fn gate_commands(
    db: &Db,
    project_id: Option<&str>,
    command_ids: &[String],
    slot: Option<&str>,
) -> Result<Vec<String>, RpcError> {
    let Some(slot) = slot else {
        return Ok(command_ids.to_vec());
    };
    let Some(project_id) = project_id else {
        // No project = nobody to hold the binding. The gate is unarmed, and the refusal says so.
        return Ok(Vec::new());
    };
    db.with_conn(|c| db::command_slot_ids(c, project_id, slot))
        .map_err(internal_db)
}

/// [`gate_commands`] straight off a stored NODE — the form the M5 re-eval and the `run_start` scan
/// need, where the config was never decoded into an [`Action`]. A config nyx cannot decode arms
/// nothing, exactly as it did before slots existed.
fn node_gate_commands(
    db: &Db,
    project_id: Option<&str>,
    node: &db::Node,
) -> Result<Vec<String>, RpcError> {
    match serde_json::from_str::<crate::engine::GateConfig>(&node.config) {
        Ok(cfg) => gate_commands(db, project_id, &cfg.command_ids, cfg.slot.as_deref()),
        Err(_) => Ok(Vec::new()),
    }
}

/// The SLOT a `gate_check` node declares, or `None` when it names its own commands (or its config
/// cannot be decoded, which arms nothing either way). What [`arm_gate_hint`] reads to pick the route
/// out it names, so the early warning and the hard refusal can never send a caller two different ways.
fn node_gate_slot(node: &db::Node) -> Option<String> {
    serde_json::from_str::<crate::engine::GateConfig>(&node.config)
        .ok()
        .and_then(|c| c.slot)
}

/// The expectation a `gate_check` node DECLARES, straight off the stored row — `None` when it
/// declares none (or its config cannot be decoded, which declares nothing either way). The read the
/// M5 re-eval and the `run_start` scan need, where the config was never decoded into an [`Action`].
fn node_gate_expect(node: &db::Node) -> Option<GateExpectation> {
    serde_json::from_str::<crate::engine::GateConfig>(&node.config)
        .ok()
        .and_then(|c| c.expect)
}

/// Whether a gate's structured result says it was CUT SHORT rather than FAILED: its failing command
/// did not exit at all (a [`GateCommandResult::timeout`] marker — its own [`GATE_TIMEOUT`] or the
/// gate's [`GATE_TOTAL_TIMEOUT`] budget) instead of exiting non-zero.
///
/// The two are opposite facts and take opposite fixes. A non-zero exit is a VERDICT: the check ran
/// and the work did not pass, so fix the work. A timeout is the ABSENCE of a verdict: nothing was
/// judged, the clock ran out — because the command does not terminate (a `--watch`, a dev server),
/// or because the suite genuinely needs more room. Telling a session "your gate is red" for the
/// second sends it looking for a defect in code the gate never finished reading.
///
/// [`run_gate_bounded`] stops at the FIRST failing command, so that command is the one to read.
fn gate_was_cut_short(gate_result: &Value) -> bool {
    gate_result
        .get("commands")
        .and_then(Value::as_array)
        .and_then(|commands| {
            commands
                .iter()
                .find(|c| c.get("passed") == Some(&Value::Bool(false)))
        })
        .is_some_and(|failing| !failing.get("timeout").unwrap_or(&Value::Null).is_null())
}

/// Build the "blocking gate did not go green" refusal: park the run `blocked` on the gate node and
/// return the [`RpcCode::InvalidState`] error that REFUSES the progression verb in its handler. The
/// leaf the run was parked on is marked for REPLAY, so the refusal is STICKY — the next call of the
/// dispatcher loop re-attempts that leaf and hits the same refusal instead of walking past it.
///
/// The ENFORCEMENT is identical either way — a gate that cannot prove itself green never advances a
/// run — but the SENTENCE is not: a gate cut short by its time budget is reported as cut short, never
/// as a failing check ([`gate_was_cut_short`]).
fn refuse_gate(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    gate_node: &str,
    last_result: Option<&Value>,
) -> RpcError {
    engine.replay_current();
    // M6 — carry the RED gate's own detail (its failing command(s) + output tail + exit code) in the
    // refusal so the dispatcher can report WHY immediately, not just "a gate is red". The surfaced
    // result lives on the blackboard; attach it only when it describes THIS gate (in a multi-gate
    // sequence a later green gate may have overwritten the key — then the reason alone is honest).
    let gate_result = engine
        .blackboard()
        .get(LAST_GATE_RESULT_KEY)
        .filter(|v| v.get("node_id").and_then(Value::as_str) == Some(gate_node))
        .cloned();
    let trace = tick_trace(None, last_result);
    let _ = persist(db, run, engine, Some(gate_node), db::RUN_STATUS_BLOCKED, &trace);
    let cut_short = gate_result.as_ref().is_some_and(gate_was_cut_short);
    let mut data = json!({ "reason": "gate_red", "node_id": gate_node, "cut_short": cut_short });
    if let Some(gate_result) = gate_result {
        if let Some(obj) = data.as_object_mut() {
            obj.insert("gate_result".to_string(), gate_result);
        }
    }
    let message = if cut_short {
        "a blocking gate_check was CUT SHORT by its time budget — its command never exited, so the \
         gate produced NO verdict and nyx_next refuses to advance past it. This is not a failing \
         check: nothing was judged. Either the bound command does not terminate (a --watch, a dev \
         server — bind its one-shot form instead), or the check needs more room than the budget \
         gives it"
    } else {
        "a blocking gate_check is RED — nyx_next refuses to advance past it"
    };
    RpcError::new(RpcCode::InvalidState, message).with_data(data)
}

/// The machine code of an EXPECTATION MISMATCH — a `gate_check` that DECLARED what its check must do
/// and got something else. Distinct from [`refuse_gate`]'s `gate_red`, and it has to be: `gate_red`
/// means "the check did not pass", which on a reproduction gate is the AWAITED result. A reader
/// keying on the reason must not have to guess which of the two it is looking at.
pub(crate) const GATE_EXPECTATION_MISMATCH_REASON: &str = "gate_expectation_mismatch";

/// Build the "this gate did not do what it declared it would" refusal: park the run `blocked` on the
/// gate node and return the [`RpcCode::InvalidState`] error that REFUSES the tick.
///
/// IMMEDIATE, not deferred to a block point. The M5 anchor refuses at the next `transition`/`done`,
/// which presumes one exists downstream and presumes it is reached; a mismatch is refused where it
/// happens, so a gate at the end of a `sequence`, inside a `loop` body, or in a graph with no
/// transition at all is guarded identically.
///
/// STICKY by [`Engine::replay_current`], the same mechanism every other gate refusal uses: the leaf
/// stays the run's cursor, so the next `nyx_next` RE-RUNS this gate rather than walking past it. The
/// block lifts when — and only when — an attempt satisfies the declared expectation. Nothing else
/// clears it: there is no anchor to clear, no note that lifts it, no other gate whose green could be
/// mistaken for this one's.
///
/// THE ROUTE OUT IS THE CHECK ITSELF, and there is no second one by design — a declared expectation
/// that a `note` could wave away would declare nothing. What a HUMAN can still do, because a tick
/// re-reads the graph before it walks ([`adopt_current_tree`]), is `node_update` the gate: changing
/// its `expect`, its `slot` or its `command_ids` reaches the VERY NEXT tick of this run. That is a
/// deliberate, recorded authoring gesture on the graph — not a runtime escape hatch — and it leaves
/// the journal saying exactly what was attempted before it. Dropping the run (`run_delete`) remains
/// the other honest end.
///
/// The message names both halves (what was declared, what happened) because the two mismatches read
/// nothing alike: a `expect: red` gate that PASSED means the defect did not reproduce, while any
/// `indeterminate` means nothing ran and the fix is to the harness, not to the code.
fn refuse_gate_expectation(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    gate_node: &str,
    expect: GateExpectation,
    grade: GateGrade,
    last_result: Option<&Value>,
) -> RpcError {
    engine.replay_current();
    let gate_result = engine
        .blackboard()
        .get(LAST_GATE_RESULT_KEY)
        .filter(|v| v.get("node_id").and_then(Value::as_str) == Some(gate_node))
        .cloned();
    let trace = tick_trace(None, last_result);
    let _ = persist(db, run, engine, Some(gate_node), db::RUN_STATUS_BLOCKED, &trace);
    let mut data = json!({
        "reason": GATE_EXPECTATION_MISMATCH_REASON,
        "node_id": gate_node,
        "expect": expect.as_str(),
        "outcome": grade.as_str(),
        "satisfied": false,
    });
    if let Some(gate_result) = gate_result {
        if let Some(obj) = data.as_object_mut() {
            obj.insert("gate_result".to_string(), gate_result);
        }
    }
    let message = match grade {
        GateGrade::Indeterminate => format!(
            "a blocking gate_check declaring 'expect: {}' produced NO verdict — it was cut short, \
             could not be spawned, or never terminated, so nothing was checked. An indeterminate \
             attempt satisfies NEITHER a red nor a green expectation, and nyx_next refuses to \
             advance past it. Fix what stopped the check from running; do not re-run it hoping it \
             turns",
            expect.as_str()
        ),
        _ => format!(
            "a blocking gate_check declared 'expect: {}' and the check {} — the declared \
             expectation is NOT satisfied and nyx_next refuses to advance past it. This gate re-runs \
             on the next call and keeps refusing until it does what it declared",
            expect.as_str(),
            match grade {
                GateGrade::Passed => "PASSED (exit 0)",
                _ => "FAILED (non-zero exit)",
            }
        ),
    };
    RpcError::new(RpcCode::InvalidState, message).with_data(data)
}

/// The machine code of an UNARMED gate — a `gate_check` that resolves to NO command: an empty
/// `command_ids`, or a `slot` this project never bound. ONE code for the two ends that speak about it:
/// the `run_start` warning (which names it before anything runs) and the tick refusal (which stops the
/// run at it). A reader keying on the code sees the same finding twice, once early and once fatally;
/// the `blocking` field beside it says which of the two it will be.
const GATE_UNCONFIGURED_CODE: &str = "gate_unconfigured";

/// WHY a gate verifies nothing, in the terms of how it names its commands — the clause both the
/// `run_start` warning and the tick refusal open with, so the two ends describe ONE defect one way.
/// A slot gate's cause names the slot, because that string is what the fix takes.
fn unarmed_gate_cause(node: &db::Node) -> String {
    match node_gate_slot(node) {
        Some(slot) => {
            format!("declares the '{slot}' command slot, which this project has bound nothing to")
        }
        None => "has no command_ids".to_string(),
    }
}

/// The ONE sentence saying how to ARM an unarmed gate — shared by the `run_start` warning and the tick
/// refusal, so the early signal and the hard stop can never tell a caller two different stories.
///
/// The route is read off HOW THE GATE NAMES ITS COMMANDS, because that is what says whose data is
/// missing:
///
///   * a gate declaring a SLOT is missing PROJECT data — it says WHAT it needs verified and this
///     project never said with what. The fix is `slot_bind`, one row, and the graph is not touched:
///     the built-in stays shared and keeps receiving nyx's fixes. This is also the only route that
///     unblocks THE RUN THAT IS REFUSING: the binding is resolved at every tick from the project, so
///     calling `nyx_next` again re-resolves this very node and runs the check.
///   * a gate naming its own `command_ids` is missing NODE data, and the fix is `node_update` on that
///     node — which the authoring layer accepts, because a graph that names its own commands is one
///     the project owns.
///
/// The route that is deliberately NOT offered any more is "fork the built-in": it was the only way to
/// arm a shared gate that named its own commands, and it cost the project every later fix — the boot
/// seeder upgrades only the rows it seeded, so the fork froze at that day's revision. That defect is
/// what the slot removes, and a message still naming the fork would keep re-creating it.
fn arm_gate_hint(workflow: &db::Workflow, node: &db::Node, project_id: Option<&str>) -> String {
    match node_gate_slot(node) {
        Some(slot) => format!(
            "register the check as a managed command (add_command), take its instance_id \
             (list_commands), then BIND it to this project's '{slot}' slot: slot_bind \
             project_id='{}' slot='{slot}' command_ids=[\"<instance_id>\"]. The '{}' gate of '{}' \
             says WHAT must be verified; your project says WITH WHAT — so do NOT fork the flow to arm \
             it (a fork stops receiving nyx's graph fixes). The binding is re-read at every tick: call \
             nyx_next again and THIS run picks it up",
            project_id.unwrap_or("<project_id>"),
            node.label,
            workflow.name,
        ),
        None => format!(
            "register the check as a managed command (add_command), take its instance_id \
             (list_commands), then set it on this node: node_update node_id='{}' \
             config={{\"command_ids\": [\"<instance_id>\"], \"blocking\": true}}",
            node.id
        ),
    }
}

/// Build the "blocking gate verifies NOTHING" refusal — the twin of [`refuse_gate`] for a gate that
/// was never armed. Park the run `blocked` ON the gate node and mark the current leaf for REPLAY, so
/// the refusal is STICKY exactly like the red-gate one: the next call of the dispatcher loop hits it
/// again instead of walking past.
///
/// The sentence is written for the person who must ACT: it names the node, states that advancing would
/// certify unverified work, and carries [`arm_gate_hint`]'s route out. A refusal a caller cannot
/// resolve is just a wall.
fn refuse_vacuous_gate(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    gate_node: &str,
    last_result: Option<&Value>,
) -> RpcError {
    engine.replay_current();
    // The node's LABEL is what the human recognises in the graph ("checks"); the WORKFLOW decides
    // which route out is real. Both come from the same read, so a lookup failure degrades the
    // sentence rather than losing the refusal.
    let node = db.with_conn(|c| db::get_node(c, gate_node)).ok().flatten();
    let workflow = node.as_ref().and_then(|n| {
        db.with_conn(|c| db::get_workflow(c, &n.workflow_id))
            .ok()
            .flatten()
    });
    let project_id = run_project_id(db, run).ok().flatten();
    let (label, cause, hint) = match (node.as_ref(), workflow.as_ref()) {
        (Some(node), Some(workflow)) => (
            node.label.clone(),
            unarmed_gate_cause(node),
            arm_gate_hint(workflow, node, project_id.as_deref()),
        ),
        _ => (
            gate_node.to_string(),
            "has no command_ids".to_string(),
            "register the check as a managed command (add_command) and arm the node — bind its \
             command slot (slot_bind) when it declares one, else set its command_ids (node_update)"
                .to_string(),
        ),
    };
    let trace = tick_trace(None, last_result);
    let _ = persist(db, run, engine, Some(gate_node), db::RUN_STATUS_BLOCKED, &trace);
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "gate_check '{label}' is BLOCKING and {cause} — it would pass without verifying \
             anything, and nyx_next refuses to advance past a gate that certifies nothing. \
             To fix it: {hint}"
        ),
    )
    .with_data(json!({ "reason": GATE_UNCONFIGURED_CODE, "node_id": gate_node }))
}

/// Build the "this question is answered BY A BRANCH and declares no `options`" refusal (J1) — the
/// twin of [`refuse_vacuous_gate`] for a question that cannot produce a verdict. Park the run
/// `blocked` ON the ask and mark the leaf for REPLAY, so the refusal is STICKY: fixing the node
/// (`node_update … options`) and calling again resumes on the very same question, now answerable.
///
/// # Why the RUNTIME refuses, and not merely a `run_start` warning
///
/// Authoring already refuses to commit such a node, so what reaches here is a graph authored BEFORE
/// the check existed — the built-ins upgrade themselves at boot, but a project's FORK does not, and a
/// fork is exactly where a hand-edited question lives. A `run_start` warning cannot cover it: it fires
/// once, at the start, and is advisory by construction, while the failure it describes happens LATER,
/// mid-run, and its whole signature is that the run keeps going and ends GREEN. The observed ndoc run
/// dropped 160 lines of un-reviewed README and reported done. So the refusal sits where the damage is
/// done — at the tick that would ask the question — for the same reason `nyx_next` refuses a blocking
/// gate with no commands rather than warning about it: an answer nothing constrains is not a decision,
/// and a run must not advance on the absence of one.
///
/// The sentence is written for whoever must ACT: it names the question, the branch that decides on it,
/// and the edit that fixes it.
fn refuse_unconstrained_ask(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    ask_node: &str,
    reader: &crate::engine::BranchReader,
    last_result: Option<&Value>,
) -> RpcError {
    engine.replay_current();
    let label = db
        .with_conn(|c| db::get_node(c, ask_node))
        .ok()
        .flatten()
        .map(|n| n.label)
        .unwrap_or_else(|| ask_node.to_string());
    let trace = tick_trace(None, last_result);
    let _ = persist(db, run, engine, Some(ask_node), db::RUN_STATUS_BLOCKED, &trace);
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "ask_human '{label}' declares no `options`, yet '{}' BRANCHES on the answer it writes \
             ({}) — the graph compares that answer by equality while accepting any text, so a reply \
             it does not recognise would take a branch nobody authored for it, and the run would \
             carry on as if it had been answered. nyx_next refuses to ask a question it cannot \
             honour. To fix it: node_update(node_id=\"{ask_node}\", config with the closed answer set \
             the branch already assumes, e.g. \"options\": [\"approve\", \"changes\", \"cancel\"]).",
            reader.label, reader.condition
        ),
    )
    .with_data(json!({
        "reason": ASK_UNCONSTRAINED_CODE,
        "node_id": ask_node,
        "branch_node_id": reader.node_id,
        "condition": reader.condition,
    }))
}

/// The machine code for the J1 refusal — a STABLE marker the dispatcher / UI keys on, sibling of
/// [`GATE_UNCONFIGURED_CODE`]. The sentence beside it is free to change.
const ASK_UNCONSTRAINED_CODE: &str = "ask_human_unconstrained_branch";

/// The machine code for the K1 refusal — a caller with no session reaching a CONSENT question.
/// Sibling of [`ASK_UNCONSTRAINED_CODE`]; the sentence beside it is free to change.
const CONSENT_CALLER_UNATTRIBUTABLE_CODE: &str = "consent_caller_unattributable";

/// Build the "this question cannot be PUT to you" refusal (K1) — the twin of
/// [`refuse_unconstrained_ask`] for a question nyx could never accept an answer to.
///
/// # Why the asking is refused, and not merely the answer
///
/// A consent answer is checked against the human-turn ledger of the terminal the question was posed
/// to ([`accept_consent_answer`]), and that ledger is KEYED BY TERMINAL. A tick carrying no terminal
/// — the plugin sends `X-Nyx-Terminal-Id: ${NYX_TERMINAL_ID:-}`, which is empty for a session started
/// outside a nyx terminal — therefore has no ledger at all, so every answer it relays is refused
/// whatever the human does. Asking anyway produced the observed loop: the session relayed the
/// question, the person answered it three times from a real `claude -p --resume`, and all three were
/// refused. A question whose every answer is doomed is not a question, so it is not asked.
///
/// The run is left RUNNING, not `blocked`: nothing is wrong with the graph or the run, only with the
/// session that reached it. The leaf is marked for REPLAY so the refusal is STICKY and so the very
/// same question is put — for the first time — to the session that comes back with a terminal.
fn refuse_unattributable_consent(
    db: &Db,
    run: &db::WorkflowRun,
    engine: &mut Engine,
    ask_node: &str,
    prompt: &str,
    commitment: crate::engine::HumanCommitment,
    last_result: Option<&Value>,
) -> RpcError {
    engine.replay_current();
    let trace = tick_trace(None, last_result);
    let _ = persist(db, run, engine, Some(ask_node), db::RUN_STATUS_RUNNING, &trace);
    RpcError::new(
        RpcCode::InvalidState,
        format!(
            "the answer to '{prompt}' {}, so nyx accepts it only from a human turn it witnessed on \
             the session it asked — and this call carries NO session: its {} header is empty, which \
             is what a Claude started outside a nyx terminal sends. nyx keeps that ledger per \
             terminal, so there is nothing here to check an answer against and every reply relayed \
             from this session would be refused, a genuine one included. The question is therefore \
             not asked at all. Run this flow from a nyx terminal — its sessions carry the header — \
             and call nyx_next(run_id=\"{}\") again: the question is then posed to you, and your \
             reply counts.",
            commitment.what_it_commits(),
            crate::mcp::TERMINAL_ID_HEADER,
            run.id
        ),
    )
    .with_data(json!({
        "reason": CONSENT_CALLER_UNATTRIBUTABLE_CODE,
        "run_id": run.id,
        "node_id": ask_node,
        "prompt": prompt,
    }))
}

/// Map an [`EngineError`] (a malformed tree / mis-authored loop) onto the RPC vocabulary. The
/// `engine_error` reason marks it TERMINAL for the run (see [`outcome_is_fatal`]).
fn map_engine_err(e: EngineError) -> RpcError {
    RpcError::new(RpcCode::InvalidState, format!("workflow engine: {e}"))
        .with_data(json!({ "reason": "engine_error" }))
}

#[cfg(test)]
mod tests;
