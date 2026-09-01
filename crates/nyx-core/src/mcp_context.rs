//! Shell-agnostic MCP **agent-context render** — the native equivalent of
//! `depot context spec|dev|coder|auditor|doc|prototype`, rendered LIVE by nyx-core from nyx state (task
//! `01KX1DH8DR369K0JA20PNF4H4C`, PDR-0020).
//!
//! # What it renders
//!
//! Given a role (`spec` | `dev` | `coder` | `auditor` | `doc` | `prototype`) plus a Spec (by id, or
//! resolved via the current dev workspace / cwd), [`render_agent_context`] produces the manual TEXT
//! from LIVE state:
//!   * the ROLE preamble — the fixed part, and a DELIBERATELY SMALL one: what this agent is, and
//!     the HARD PROHIBITIONS the graph cannot express. Not the process (the behavior tree executes
//!     it, and the spawn node's `label` names the step), not the project's conventions (the ambient
//!     `CLAUDE.md` carries them — N29), not a tool's contract (its descriptor does);
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
//! # Exposure — and the DELIVERY, which is a PULL
//!
//! [`dispatch_context_tool`] serves the `agent_context` MCP tool (routed from
//! [`crate::mcp_tools_core::dispatch_extension_tool`]) and advertised in `tools/list` via
//! the tool descriptors in `mcp.rs`. An agent reaches its manual by CALLING that tool.
//!
//! THAT CALL IS THE WHOLE MECHANISM, and the `run_id` is what makes it possible: a run carries the
//! Spec it drives (F5) and the workspace it opened against (G2), so a sub-agent spawned by a
//! `nyx_next` action — which hands it an `agent_id`, a `label`, a `subject` and that `run_id`, and
//! NOTHING else — resolves everything from it. nyx used to SHIP the rendered manual inside the
//! spawn action instead; that made a tick weigh what the manual weighed (73 547 chars on a
//! `prototype` spawn) for a text nothing in the tick changed, while the `sub_workflow` action
//! (a child `run_id` plus three lines) had been proving the pull model next to it all along.

use serde_json::{json, Value};

use crate::command::RunState;
use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{anchor_str, internal_db, optional_str, require_str};
use crate::mcp_tools_core::ToolOutcome;

/// `agent_context` — render an agent-role manual (spec | dev | coder | auditor | doc | prototype)
/// from nyx state.
pub const AGENT_CONTEXT_TOOL: &str = "agent_context";

/// How many OPEN backlog ideas the /nspec authoring manual surfaces up front (a bounded preview —
/// the author then `idea_search`es the concrete topic; this is only the "there is a backlog, look
/// at it" nudge, never a full dump).
const SPEC_AUTHORING_IDEA_PREVIEW: i64 = 20;

/// THE HIERARCHY OF SOURCES, stated ONCE and rendered at the top of EVERY manual (v2.8 review N29).
///
/// It corrects a claim the manuals carried and settles the one they never made. A spawned sub-agent
/// does NOT start context-less: whatever launches it, the provider loads the AMBIENT `CLAUDE.md`
/// files of the project it runs in — what it does not inherit is the SESSION (the run, the Spec, the
/// history), which is exactly what THIS render carries, and what the sub-agent pulls by calling
/// `agent_context(run_id=…)` itself. And since both texts reach the agent, the manual must say which
/// one WINS on the project's own ground: the project's, because this manual is BUILT-IN and knows
/// nothing of it. So a role manual carries the ROLE and stops there — a commit style, a branch
/// policy, a language, a tool choice are the project's, and prescribing one here is how a built-in
/// text overrides a convention it cannot see.
const SOURCES_NOTE: &str = "\
> SOURCES — this manual carries your ROLE (what you do here, and what you may not); it does not\n\
> carry the project's conventions. You also inherit the project's ambient `CLAUDE.md` files — you\n\
> never start context-less — and on the project's own ground (commits, branches, language, tooling,\n\
> house style) THEY take precedence over this text. What you do NOT inherit is this run: it is\n\
> below.\n\n";

/// The agent ROLES a context can be rendered for (v2.5.8 agent harness, PDR-0020). The
/// "principaux" (directly triggerable) are `spec` (authors a Spec), `dev` (orchestrates its
/// implementation) and `doc` (syncs the doc); `coder` and `auditor` are the sub-agents the dev
/// delegates to, and `prototype` is the throwaway design proto the spec author proposes for a
/// graphical subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Spec,
    Dev,
    Coder,
    Auditor,
    Doc,
    Prototype,
}

impl AgentRole {
    /// Parse the `role` argument. An unknown role → `None` (the caller maps it to
    /// `invalid_argument`).
    fn parse(name: &str) -> Option<Self> {
        match name {
            "spec" => Some(AgentRole::Spec),
            "dev" => Some(AgentRole::Dev),
            "coder" => Some(AgentRole::Coder),
            "auditor" => Some(AgentRole::Auditor),
            "doc" => Some(AgentRole::Doc),
            "prototype" => Some(AgentRole::Prototype),
            _ => None,
        }
    }

    /// The manual's header label (`SPEC` / `DEV` / `CODER` / `AUDITOR` / `DOC` / `PROTOTYPE`).
    fn label(self) -> &'static str {
        match self {
            AgentRole::Spec => "SPEC",
            AgentRole::Dev => "DEV",
            AgentRole::Coder => "CODER",
            AgentRole::Auditor => "AUDITOR",
            AgentRole::Doc => "DOC",
            AgentRole::Prototype => "PROTOTYPE",
        }
    }

    /// The lowercase role SLUG, used to name a role-scoped rule (`role:coder`) in the resolved
    /// `{{rules}}` block and its lineage note. Mirrors [`AgentRole::parse`].
    fn slug(self) -> &'static str {
        match self {
            AgentRole::Spec => "spec",
            AgentRole::Dev => "dev",
            AgentRole::Coder => "coder",
            AgentRole::Auditor => "auditor",
            AgentRole::Doc => "doc",
            AgentRole::Prototype => "prototype",
        }
    }

    /// The fixed ROLE preamble — WHAT this agent is, and the HARD PROHIBITIONS the graph cannot
    /// express.
    ///
    /// It carries neither the process nor the conventions, and that is the whole shape of it:
    ///   * the PROCESS is the GRAPH. A node's `label` says what to do at this step, the state below
    ///     says what it is about, and the `gate_check` that follows says whether it held — a manual
    ///     re-narrating "Step 0 → Step 3" was re-encoding, in prose, the tree nyx already walks.
    ///   * the CONVENTIONS (commit style, branch policy, language, tooling, house style) are the
    ///     PROJECT's, carried by the ambient `CLAUDE.md` every sub-agent inherits (N29). A built-in
    ///     text that prescribed them would be overriding a convention it cannot see.
    ///   * the TOOL contracts are the tool DESCRIPTORS (`mcp.rs`), which every session already gets
    ///     from `tools/list` — repeating one here paid for it twice.
    /// What is left is what NOTHING else can say: a node label states what to do, and no graph
    /// structure states what is FORBIDDEN. Hence the shape — three lines of identity, then
    /// "you never …".
    fn preamble(self) -> &'static str {
        match self {
            AgentRole::Spec => {
                // THE ONE CONVENTION THIS MANUAL DOES STATE, and it states a RESOLUTION ORDER, not a
                // language. Spec #7 came out in English and Spec #8 in French — same project, same
                // effector, both asked for in French — because nothing here says how to decide, so
                // each session decided again. A Spec is a durable artifact a team re-reads and
                // builds against; its language drifting per session is a defect of this render.
                //
                // It does not contradict the CONVENTIONS doctrine ([`SOURCES_NOTE`]): the project
                // still wins, and step 1 of the order IS the project. What the ambient `CLAUDE.md`
                // cannot settle is the Spec itself — the standing convention on a machine like this
                // one ("prose to the human in French, code and technical artifacts in English")
                // classifies neither, a Spec being a commitment document written for humans in a
                // technical tool. So the fallback is the only other thing the run actually knows:
                // the language the human asked in. And the first step is CHECKABLE from where the
                // author stands — `render_spec_authoring` prints the project's existing Specs right
                // under this text.
                r#"# Context: Spec Author (/nspec)

You are the spec author: you turn a raw human intent into a coherent nyx Spec and slice it into
phases and dev-ready tasks, through the nyx MCP tools. The human DUMPS raw, you GRILL, the human
VALIDATES. What to do at THIS step is the node label nyx handed the dispatcher; the run's state is
below.

You never:

- implement code;
- transition the Spec's lifecycle (ready / start / review / done / cancel) — nyx imposes EACH
  transition at the graph node built for it: you never call spec_transition, and never ask nyx to
  (v2.8 review N17);
- launch a prototype — you FLAG a graphical subject in your handback, with the design question;
  nyx asks the human and runs the prototyper itself;
- validate in the human's place: the human validates the SPEC, you decide the TASKS;
- rewrite a Spec in place — one modification to an existing Spec is a new revision (fork rule). The
  ONE exception is this spawn: when nyx sends you to a task BLOCKED because the Spec's TEXT
  contradicts code that is right, the human ruled at the escalation gate that the prose is what must
  change. Correct THAT section, in place, with spec_update_section(run_id="<this run>") — the run_id
  is what carries their authorization, the window is this spawn alone, and it is a CORRECTION of a
  sentence the code already contradicts, never a redesign;
- open a Spec for a need the project's backlog already tracks: idea_search the subject FIRST, and
  link what this Spec resolves with idea_link(kind="addresses");
- choose the Spec's LANGUAGE yourself. A Spec is read long after this session, so its language is
  RESOLVED, in this order: (1) what the project prescribes — its own `CLAUDE.md`, and the language
  its existing Specs are written in (they are listed below; open one if the answer is not obvious);
  (2) failing that, the language the human asked in. One language for the whole Spec — every
  section, every phase name, every task title — and the same one when you revise it later."#
            }
            AgentRole::Dev => {
                // v2.8 — the DISPATCHER (thin, built-in). The ex-orchestrator's ~117 lines of
                // prose (state rule, per-phase cadence, backlog capture, the review loop, …) are
                // GONE from the session: that whole process is now the DETERMINISTIC behavior tree
                // nyx interprets itself (the `graphe engine`). What remains here is a dispatcher
                // that owns NO flow — it calls `nyx_next` and performs EXACTLY the one atomic
                // action nyx returns, nothing else. Cf. annex `context-dev-dispatcher`.
                r#"# Context: Dev — dispatcher

## Role

You do NOT orchestrate. You do NOT know the workflow — nyx does. Your ONLY job: call
`nyx_next`, do EXACTLY the one atomic action it returns, report the result, and call it again.
You never decide the flow — the orchestration lives in the graph, not in your head.

## Your run is already open

You tick a RUN, and nyx opened this one for you before the prompt reached you: its `run_id` is in
the `NYX RUN` block that precedes this manual. That id is what every `nyx_next` below takes — you
make NO call to obtain it, and you never invent one.
- WARNINGS in that block are written for the human: RELAY each one, in one line, BEFORE your first
  tick. `run_replaced_graph_changed` above all — a previous run was DESTROYED (its graph changed
  under it) and yours restarts the flow from the top. Losing a run's progress in silence is not an
  option.
- No such block (you pulled this manual by hand, outside a nyx command) ⇒ open one yourself:
  `run_start(scope: "{{scope}}", cwd: …)`. It is IDEMPOTENT — at most ONE live run per (workflow,
  Spec) — so a call made when you already hold a `run_id` RESUMES that very run instead of opening
  a second; it simply buys you nothing.
- The human's command may OPEN WITH A SPEC REFERENCE — the canonical id, the human key (e.g. KEY-3),
  '#3', or the bare number 3 — and that reference is the Spec they want worked on. Pass it to
  `run_start` as `spec_id`, VERBATIM: nyx owns the grammar, so you neither translate the token nor
  drop it. If nyx REFUSES it, STOP and ASK the human which Spec they meant, quoting the refusal (it
  names what the project actually holds). Never substitute a Spec of your own choosing, and never
  author a new one to get past a refused reference — that is how a duplicate of the very Spec they
  named gets ordered.

## The loop (all you do)

1. Call `nyx_next(run_id, last_result?)`.
2. It returns ONE atomic action — do only that, nothing more:
   - `spawn <sub_agent>`  -> it names WHO (`agent_id`), WHAT (`label` — the node's own words for
                             this step) and, when the node has one, on WHICH OBJECT (`subject`).
                             You forward those; you do NOT carry a role manual, because the
                             sub-agent PULLS its own: `agent_context(run_id, role, subject?)`
                             renders its role and this run's live state. The action's `mode` says
                             HOW to run it:
       • mode `native_task` -> launch it via your provider's NATIVE sub-agent (Task tool). Its
                               prompt is the `label`, the `subject` and this run's `run_id`, plus
                               the instruction to call `agent_context(run_id="<run_id>",
                               role="<agent_id>", subject="<subject>")` FIRST and follow what it
                               returns. Report its result (its diff).
       • mode `inline`      -> do NOT spawn a Task. YOU are the effector for this one step, so
                               CALL `agent_context(run_id="<run_id>", role="<agent_id>",
                               subject="<subject>")` YOURSELF, follow it, and do the step IN this
                               session — which is what lets an INTERACTIVE effector (e.g. the
                               spec-author grill) reach the user. Report what it produced.
   - `sub_workflow`       -> nyx hands you a CHILD run to drive to a stopping point: it carries
                             `child_run_id` + a generic-dispatcher `context`. Spawn a native Task
                             (like `spawn`) handed that `context` + `child_run_id`; it loops
                             `nyx_next(child_run_id)`, handling each child action EXACTLY as this
                             top-level loop does (spawn -> Task, nyx-inline -> nyx handles, …) until
                             the child returns `done` (or stops). When it returns, call
                             `nyx_next(run_id)` AGAIN on the PARENT with NO `last_result` — nyx
                             itself reads the child's verified state and advances the parent (folds
                             the child result / branches). You NEVER report the child's result
                             through `last_result`; you just re-tick the parent.
   - `judge <question>`   -> answer it (structured); report the verdict.
   - `ask_human <prompt>` -> relay it to the user, wait, then report their answer (its `decision`).
                             WHERE you report it depends on the action's shape:
       • PLAIN (no `via`/`child_run_id`) -> answer the CURRENT run, as today:
         `nyx_next(run_id, last_result={decision})`.
       • BUBBLED (carries `via: "sub_workflow"` + `child_run_id` + `child_node_id`) -> the question
         comes from a sub-workflow CHILD (its own dispatcher can't answer a human). Present the
         prompt as usual, but report the answer to `nyx_next(child_run_id, last_result={decision})`
         — the CHILD, NOT the parent. Then KEEP DRIVING THAT CHILD DIRECTLY (loop
         `nyx_next(child_run_id)`, performing each returned action and reporting its REAL result,
         answering any further bubbled child ask the same way) until it returns done/blocked/failed
         — and ONLY THEN re-tick the PARENT (`nyx_next(run_id)`, no `last_result`). Do NOT re-spawn a
         fresh child Task right after the answer: that folds an EMPTY result and skips the child's
         next step (e.g. the prototyper's apply-election spawn).
   - `produce <node>`     -> produce the STRUCTURED artifact nyx asks for (a spec section, a task, a
                             review finding); report it as `{ "artifact": … }`.
   - `wait` / `done`      -> wait, or stop (the run is complete).
   Any action (`done` included) may carry a `notes` array: free text the graph injected on the way
   to it. Read it, act on it if it asks something of you, and perform the action it rides with —
   `notes` is never an action of its own and reports no result.
3. Report the outcome and go back to 1.

## You may not

- orchestrate, loop, branch, or transition on your own initiative — nyx owns the flow.
- skip a step, reorder, or invent an action nyx did not return.
- run gates or state transitions yourself — nyx does that INSIDE `nyx_next`.
- launch a sub-agent WITHOUT the `run_id` and the `label` — without the first it cannot read its
  role or this run's state, without the second it does not know which step it is performing.

{{rules}}"#
            }
            AgentRole::Coder => {
                // v2.8 — the coder does not PILOT its tasks: the seeded `coder` sub-workflow owns
                // the task lifecycle (`transition_task` start/done around the spawn) and the
                // validation (`gate_check`, run by nyx over the PTY). So the prohibitions are all
                // that is left — including the two transitions no node covers (block / skip).
                r#"# Context: Coder Agent

You are the coder sub-agent: you implement the step the node label names, over the current phase's
tasks, through the nyx tools. Your hand-back is a REPORT — the diff you produced, the commands you
ran and what they said, what is still open.

You never:

- redefine the Spec, or transition its lifecycle;
- move a task to started / done — nyx does that around your spawn. The one lifecycle move that is
  yours is a REFUSAL: task_transition block / skip on a task you cannot take;
- run the acceptance gate, or rule on its verdict — nyx runs the checks itself, so "it is done" is
  a REPORT, never a state change;
- do a `kind=human` task — that action is reserved for the user: task_transition block, say why,
  and hand back;
- run a command outside the nyx tools (an unobserved launch). The workspace's own commands — its dev
  server, its build, its test run — are managed: `list_commands` says which exist and which are
  already up, `start_command` runs one THROUGH nyx (supervised, output captured, visible to the user
  as to you), and it is a NO-OP on one already running, so there is never a reason to relaunch "just
  in case" nor to spawn the same server a second time in a raw shell;
- re-invent a rendering — for any UI you COMPOSE from the project's design-system
  (design_inventory), never an ad-hoc look;
- capture a follow-up yourself (no idea_add): tech debt, an edge case, an out-of-task improvement
  is SURFACED upward in your report, and the orchestrator alone proposes it to the user;
- invent a task outside your context, widen your task, or resolve a human comment.

{{rules}}"#
            }
            AgentRole::Auditor => {
                r#"# Context: Auditor Agent

You audit ONE COMMITMENT of the Spec — the section named by the `subject` of this spawn, reproduced
below — against the code and the nyx-derived diff, and you FILE what it does not honour (a finding
is a review task: task_add kind=review + severity). A section the build honours costs you nothing:
filing NOTHING is the clean report.

SEVERITY IS LOAD-BEARING — nyx reads it, it is not a label. A `critical`/`high`/`medium` finding
RE-OPENS the build (another coder pass, another full audit); a `low`/`info` one does not — it is
recorded on the Spec, keeps its severity, and is picked up on the next cycle. File it either way:
deferred is TRACED, not dropped. Omitting the severity re-opens the build, since nothing proves an
unclassified finding is minor — so classify every finding you file, and reserve high/medium for what
must be fixed before this Spec ships.

You never:

- modify the code, change the scope, mark the Spec done, or resolve a human comment;
- audit another section — another commitment is another pass's business, and for `out_of_scope` the
  question is whether the exclusion was RESPECTED;
- pronounce a verdict on the work ("ready", "good enough", "one more pass") — nyx DERIVES it from
  the findings you filed, and from nothing else. An unmet point you did not file does not exist; a
  concern you cannot anchor is not a finding;
- bundle several unmet points into ONE finding — the verdict is a COUNT of findings, so a bundle
  under-reports it. One point, one finding, anchored file:line, citing the commitment it breaks.

{{rules}}"#
            }
            AgentRole::Doc => {
                r#"# Context: Doc-sync (/ndoc)

You sync the project's documentation to what the Spec actually shipped, reading its Git range
(since..until) and writing into the working tree.

You never:

- commit on your own authority;
- validate your own output — it is a REVIEW, and the human validates before it ships;
- impose a doc convention of your own: what to read, where to write, for whom, in which language
  and with which vocabulary (glossary / ADR) is the PROJECT's call, carried by its own `CLAUDE.md`
  and docs guide.

{{rules}}"#
            }
            AgentRole::Prototype => {
                // v2.8 — the ROUND LOOP and the DESIGN PROCEDURE both left this manual. The seeded
                // `prototyper` sub-workflow loops until `design_locked`, ASKS the human (election,
                // then lock/changes) and spawns the distill itself; and the ~48k catalogue procedure
                // it narrated is the `design_*` tool DESCRIPTORS, which every session already holds.
                r#"# Context: Prototype (sub-agent)

You drive a Spec's PROTOTYPE annexe to settle its design question BEFORE any code is written. It is
attached to the Spec (ONE per Spec) and lives entirely through the nyx `proto_*` / `design_*` MCP
tools — every page, variant, round, election and freeze is a NAMED, GUARDED call, and each tool's
own description carries its contract. Read the explicit state first (proto_get), then act. Every
`design_*` call takes the `project_id` in this manual's header: a prototype is settled before the
code, so there is usually no dev workspace yet, and nothing here needs one.

You never:

- re-invent a rendering — you COMPOSE from the project's design-system catalogue, and a component
  the catalogue does NOT have is AUTHORED as a `[NEW]` (proto_author_component) for the coder to
  build, never hand-rolled inline;
- pose the RETENU — the ELECTION is the USER's decision (proto_elect_variant records it); you may
  RECOMMEND (proto_recommend_variant) and nothing more;
- mutate a distilled (frozen) round — you iterate by opening a NEW one (proto_rebase);
- carry pre-dev feedback into dev — those comments are a review artifact, NEVER transmitted to the
  coder;
- decide to iterate again, or to freeze: ONE spawn = ONE round. nyx asks the human, then re-spawns
  you for the next round or hands you the freeze;
- report a design as visually VALIDATED on the strength of design_preview — it renders MARKUP, not
  an image. Say what you CHECKED (the structure) and what you did NOT (the rendering), and when the
  open question IS visual (a colour, a spacing, a layout), PRESENT the round and ASK."#
            }
        }
    }

}

// ===========================================================================
// `{{rules}}` — customisable effector rules, injected at a template marker.
//
// v2.8 keeps the EFFECTORS (coder / auditor / doc) customisable: their role template carries a
// `{{rules}}` marker that nyx RESOLVES by scope (always / role:<role> / fork) + lineage and
// REPLACES with the project's directives. The DISPATCHER (dev) carries the marker too, but a thin
// one: it resolves to the ALWAYS scope only (it does not code → no role directives; it is built-in
// and NOT forked → no fork rules). Cf. annexes `context-effecteur` + `context-dev-dispatcher`.
//
// SCOPE OF THIS SLICE: the RESOLUTION + INJECTION MECHANISM (this module) is what Phase 5 owns. A
// persistent, per-project rules STORE (a `rules` table + authoring CRUD) does NOT exist yet — so
// [`resolve_rule_set`] returns an EMPTY set today and a stock render injects an explicit "no rules
// configured" block under the resolved-scope header (the marker is still resolved by scope). Once
// the store lands, only [`resolve_rule_set`] changes; the marker, the scoping, the lineage and the
// injection are already in place (and unit-tested with a populated [`RuleSet`]).
// ===========================================================================

/// The literal marker a role TEMPLATE carries at its rules injection point. [`render_agent_context`]
/// RESOLVES it (scope + lineage) and REPLACES it with the rendered rules block. A template WITHOUT
/// the marker (`spec` / `prototype`, outside the v2.8 effector/dispatcher scope) is passed through
/// untouched.
const RULES_MARKER: &str = "{{rules}}";

/// The SCOPE that gates whether a project directive ("rule") is injected into a given render, and
/// which group it lands in. Resolution order at the marker is ALWAYS → ROLE(role) → FORK (the annex
/// `context-effecteur` ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleScope {
    /// Applies to EVERY agent of the project (e.g. "pnpm, never npm", the commit convention).
    Always,
    /// Applies only to a specific ROLE (e.g. `role:coder` = "compose from the design-system").
    Role(AgentRole),
    /// Added by a project FORK of a built-in template — carried ONLY when the render has a fork
    /// lineage (a forked effector), never on a built-in, non-forked render.
    Fork,
}

/// One project directive injected at the `{{rules}}` marker. `curated` mirrors the annex's
/// curated|proposed chip: a human-vetted rule vs an agent-proposed one (surfaced but not yet
/// blessed).
#[derive(Debug, Clone)]
struct Rule {
    scope: RuleScope,
    text: String,
    curated: bool,
}

impl Rule {
    /// A convenience constructor for a curated rule of a given scope. Test-only until the
    /// per-project rules store lands (production builds resolve to an empty [`RuleSet`]).
    #[cfg(test)]
    fn curated(scope: RuleScope, text: &str) -> Self {
        Rule {
            scope,
            text: text.to_string(),
            curated: true,
        }
    }
}

/// The LINEAGE of a forked effector's template — the `forked_from` provenance the annex renders
/// ("forked_from coder (built-in)"). Present ⇒ the render is a project FORK (its FORK-scoped rules
/// apply + the lineage note is emitted); absent ⇒ a built-in, non-forked render.
#[derive(Debug, Clone)]
struct Lineage {
    /// The built-in role this effector was forked from (its slug, e.g. "coder").
    forked_from: String,
}

/// A project's configurable directives + the render's lineage — everything the `{{rules}}` marker
/// resolves against. v2.8 ships the RESOLUTION + INJECTION; the persistent per-project STORE is a
/// follow-up (see the module note above), so [`RuleSet::empty`] is what a stock render injects
/// today (fully resolved by scope, just with nothing configured).
#[derive(Debug, Clone, Default)]
struct RuleSet {
    rules: Vec<Rule>,
    lineage: Option<Lineage>,
}

impl RuleSet {
    /// The default source used by [`render_agent_context`] until a per-project rules store exists:
    /// no rules, no lineage (a built-in, non-forked render).
    fn empty() -> Self {
        RuleSet::default()
    }

    /// The SCOPES considered when resolving the marker for `role`, in injection order — the SINGLE
    /// SOURCE OF TRUTH for the always → role:<role> → fork policy (both the rule resolution and the
    /// label/heading rendering derive from this list, so a new scope is added in exactly one place).
    /// ALWAYS for every role; plus the render's own `role` scope and (when this render carries a fork
    /// lineage) the `fork` scope for the EFFECTORS. The DISPATCHER (`dev`) considers ALWAYS only (no
    /// role directives — it does not code; no fork — it is built-in, not forked).
    fn applicable_scopes(&self, role: AgentRole) -> Vec<RuleScope> {
        let mut scopes = vec![RuleScope::Always];
        if role != AgentRole::Dev {
            scopes.push(RuleScope::Role(role));
            if self.lineage.is_some() {
                scopes.push(RuleScope::Fork);
            }
        }
        scopes
    }

    /// The group LABELS of the applicable scopes for `role`, in injection order — derived from
    /// [`RuleSet::applicable_scopes`] via [`scope_label`], so labels stay in lockstep with the rule
    /// resolution.
    fn resolved_scope_labels(&self, role: AgentRole) -> Vec<String> {
        self.applicable_scopes(role)
            .into_iter()
            .map(scope_label)
            .collect()
    }

    /// Resolve the rules injected for `role`, in annex order (always → role → fork), by collecting
    /// the rules of each applicable scope from [`RuleSet::applicable_scopes`] — the shared policy.
    fn resolve_for(&self, role: AgentRole) -> Vec<&Rule> {
        let mut out: Vec<&Rule> = Vec::new();
        for scope in self.applicable_scopes(role) {
            out.extend(self.rules.iter().filter(|r| r.scope == scope));
        }
        out
    }

    /// Render the resolved-rules block that REPLACES the `{{rules}}` marker for `role`. The header
    /// names the RESOLVED scopes + the lineage, so the resolution is observable even when the
    /// project has configured no rules yet; an empty set renders an explicit "no rules" note under
    /// that same header (the marker is still resolved by scope).
    fn render_block(&self, role: AgentRole) -> String {
        let scopes = self.resolved_scope_labels(role).join(" + ");
        let lineage = match &self.lineage {
            Some(l) => format!("forked_from {}", l.forked_from),
            None => "no lineage (built-in, not forked)".to_string(),
        };
        let mut out = format!(
            "## Rules (project directives)\n\n\
             Resolved at the rules marker by scope ({scopes}) — {lineage}. These are project \
             directives injected by nyx; follow them as part of your context.\n\n"
        );
        let resolved = self.resolve_for(role);
        if resolved.is_empty() {
            out.push_str(
                "_No project rules configured yet (the per-project rules store is not wired \
                 yet — the marker still resolves by scope)._",
            );
            return out;
        }
        // Group the resolved rules by their scope, in the same always → role → fork order.
        for label in self.resolved_scope_labels(role) {
            let group: Vec<&&Rule> = resolved
                .iter()
                .filter(|r| scope_label(r.scope) == label)
                .collect();
            if group.is_empty() {
                continue;
            }
            out.push_str(&format!("### {label}\n"));
            for rule in group {
                let tag = if rule.curated { "curated" } else { "proposed" };
                out.push_str(&format!("- {} [{}]\n", rule.text, tag));
            }
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    /// Resolve + inject the `{{rules}}` marker into a role `section`. A section without the marker
    /// (spec / prototype) is returned unchanged.
    fn inject(&self, section: &str, role: AgentRole) -> String {
        if section.contains(RULES_MARKER) {
            section.replace(RULES_MARKER, &self.render_block(role))
        } else {
            section.to_string()
        }
    }
}

/// The group LABEL of a scope (`always` / `role:<role>` / `fork`) — the key that lines a rule up
/// under its heading in [`RuleSet::render_block`].
fn scope_label(scope: RuleScope) -> String {
    match scope {
        RuleScope::Always => "always".to_string(),
        RuleScope::Role(role) => format!("role:{}", role.slug()),
        RuleScope::Fork => "fork".to_string(),
    }
}

/// Render the `{{rules}}` REPLACEMENT for the STATIC role template (review finding #35): with NO
/// run, the project rules are RESOLVED AT RUN, so the marker names the scopes it WILL resolve
/// (always / role:<role> / fork) but DEFERS the directives themselves. Reuses the live block's
/// scope naming ([`RuleSet::resolved_scope_labels`]) so the reader sees the same shape cold as
/// bound.
fn deferred_rules_block(role: AgentRole) -> String {
    let scopes = RuleSet::empty().resolved_scope_labels(role).join(" + ");
    format!(
        "## Rules (project directives)\n\n\
         _Resolved at run by scope ({scopes}) — bind a run to a Spec to see the project's \
         directives injected here._"
    )
}

/// Replace the `{{rules}}` marker in a role `section` with the [`deferred_rules_block`] placeholder
/// (the static-template path). A section WITHOUT the marker (spec / prototype) is returned
/// unchanged — mirroring [`RuleSet::inject`] for the live path.
fn inject_deferred_rules(section: &str, role: AgentRole) -> String {
    if section.contains(RULES_MARKER) {
        section.replace(RULES_MARKER, &deferred_rules_block(role))
    } else {
        section.to_string()
    }
}

/// Resolve the [`RuleSet`] for a render. TODAY this is always EMPTY: the persistent per-project
/// rules STORE (a `rules` table + authoring CRUD, plus the effector-fork lineage that feeds the
/// FORK scope) does not exist yet, so there is nothing to load. The MECHANISM around it — the
/// marker, the scope resolution, the lineage-aware fork scope and the injection — is fully in
/// place, so populating this (once the store lands) is the ONLY change needed for live rules.
fn resolve_rule_set(_db: &Db, _role: AgentRole, _spec_id: &str) -> RuleSet {
    RuleSet::empty()
}

/// A Spec's CURRENT PHASE: the lowest phase number that still has un-done tasks, resolved
/// from the phase plan + the task statuses. `None` when the Spec has no phase plan (flat
/// tasks) or every task is terminal. The `name` is looked up in the phase plan.
struct CurrentPhase {
    number: i32,
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
    // The phase plan is parsed with `db::PhaseEntry` — the one declaration of what a
    // `specs.phases` entry IS. This file used to carry a byte-identical local mirror, justified
    // by "the db one is private"; it is `pub(crate)` since the home read model started reading it
    // (`home::spec_progress`), so the mirror had lost its reason to exist.
    let entries: Vec<db::PhaseEntry> = serde_json::from_str(trimmed).ok()?;
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

/// The NUMBER of a Spec's CURRENT BUILD phase (the lowest phase still carrying open work),
/// resolved from its `phases` plan + `tasks` — the coder's active phase. `None` for a flat plan
/// (no phases) or an all-terminal Spec. Shares the [`resolve_current_phase`] logic (the render's
/// current-phase seam) so the `phase_open_tasks` query and the effector manual never drift, without
/// exposing the private [`CurrentPhase`] struct across modules (v2.8 lot 2).
pub(crate) fn current_build_phase_number(phases_json: &str, tasks: &[db::Task]) -> Option<i32> {
    resolve_current_phase(phases_json, tasks).map(|p| p.number)
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

/// The human HEADING a Spec section is rendered under — the same wording the full render uses, so a
/// narrowed context reads identically to the block it replaces.
fn section_heading(section: db::SpecSection) -> &'static str {
    match section {
        db::SpecSection::Problem => "Problem",
        db::SpecSection::Solution => "Solution",
        db::SpecSection::ImplementationDecisions => "Implementation decisions",
        db::SpecSection::TestingDecisions => "Testing decisions",
        db::SpecSection::UserStories => "User stories",
        db::SpecSection::OutOfScope => "Out of scope",
    }
}

/// The BODY of one section of an active revision.
fn revision_section(revision: &db::SpecRevision, section: db::SpecSection) -> &str {
    match section {
        db::SpecSection::Problem => &revision.problem,
        db::SpecSection::Solution => &revision.solution,
        db::SpecSection::ImplementationDecisions => &revision.implementation_decisions,
        db::SpecSection::TestingDecisions => &revision.testing_decisions,
        db::SpecSection::UserStories => &revision.user_stories,
        db::SpecSection::OutOfScope => &revision.out_of_scope,
    }
}

/// Render one SIBLING Spec of the project as a compact bullet — the anti-duplication lists of BOTH
/// context renders (the anchored one and the /nspec authoring one) go through here, so the two
/// cannot say the Spec's identity differently.
///
/// IT CARRIES THE `spec_id`, beside the `#N` and on the model of the idea list fifty lines below
/// (`- <id> · <title> · theme=…`). `#N` is the only name these lists used to give, and it is the one
/// form nyx displayed everywhere while accepting none of it — the agent that met it had to spend
/// four MCP calls recovering an id nyx already held. The `#N` is KEPT: it is what the human reads
/// and now also what they may type.
fn spec_line(spec: &db::Spec) -> String {
    format!(
        "- #{} · {} · {} · spec_id={}\n",
        spec.number, spec.title, spec.status, spec.id
    )
}

/// The live state of ONE run driving a Spec, as the context header states it.
///
/// It is a snapshot, resolved inside the render's own `with_conn`, because the render must not hold
/// a connection while it formats.
struct SpecRunState {
    run_id: String,
    /// The flow's NAME and SCOPE — `#3 has a live run` is not actionable until you know whether it
    /// is the `nspec` run that authored it or the `ndev` run building it.
    flow: String,
    scope: String,
    /// `running` | `blocked` (a terminal run is not live and never reaches here).
    status: String,
    /// The node the run is PARKED on — its label and id — or `None` before the first tick.
    node: Option<(String, String)>,
    /// Milliseconds since `updated_at`: the run's last activity.
    idle_ms: i64,
    /// A `sub_workflow` child (it inherits this Spec from its parent), not a run someone opened on
    /// the Spec directly.
    child: bool,
}

/// The live runs of a Spec as the header needs them: the ones it will LIST, and how many there are
/// IN ALL. Two fields because the block says both things and they are not the same number —
/// [`SPEC_RUN_LINES_MAX`] bounds the first, nothing bounds the second.
struct SpecLiveRuns {
    /// The runs that get a line, most recently active first — at most [`SPEC_RUN_LINES_MAX`].
    listed: Vec<SpecRunState>,
    /// How many live runs the Spec carries, listed or not: what the "… and N more" line counts.
    total: usize,
}

/// THE LIVE RUNS OF A SPEC — non-terminal runs, MOST RECENTLY ACTIVE FIRST, each resolved to what a
/// reader needs.
///
/// ONLY THE ONES THAT GET A LINE ARE RESOLVED. The cap is applied to the RUN ROWS, before the
/// per-run `get_workflow` / `get_node` reads, so the accumulation case this header exists to
/// surface — a Spec carrying a dozen live runs — no longer pays two queries per run to print three
/// of them. The COUNT still covers them all: it is taken from the filtered rows, which is the one
/// thing the cap must not truncate.
///
/// THE QUERY IS [`db::list_runs_for_spec`], the read the Spec-side runtime view (`/work/$specId`'s
/// run modal, the dev overlay) already uses. A third "runs of this Spec" query is exactly what the
/// task forbade, and rightly: `run_start`'s stranded report and the (workflow, spec) convergence
/// each read these rows for their own purpose, and a fourth reader with its own filter is how two
/// surfaces start disagreeing about whether a Spec is being driven.
///
/// LIVE = NOT TERMINAL, through [`db::run_status_is_terminal`] — the same predicate the convergence
/// judges resumability with, so "the render says there is a live run" and "`run_start` will resume
/// a run" cannot come apart. `blocked` IS live: a blocked run re-runs its gate on the next tick.
///
/// THE ORDER IS RE-DECIDED HERE, and not left to the query's, because the two lists answer two
/// questions. The runtime view lists a Spec's runs INCLUDING the finished ones, where `started_at`
/// desc is the stable chronology a human scrolls; this list is LIVE runs under a CAP
/// ([`SPEC_RUN_LINES_MAX`]), where the question is "who is driving this?" and the answer is the run
/// somebody touched last. Sorting on `started_at` there drops precisely the wrong row: a top-level
/// ndev flow STARTS BEFORE every `sub_workflow` child it opens and before every later run on the
/// Spec, so at four live runs it is the ndev line — the one this header exists to state — that
/// falls off the end and is merely counted. So the sort is `updated_at` desc, spelled as `idle_ms`
/// ascending: the SAME field the STALE mark reads, so "listed first" and "not abandoned" cannot
/// disagree. The sort is STABLE, so runs touched in the same millisecond keep the query's
/// `started_at` desc order and the render stays deterministic.
fn spec_live_runs(
    conn: &mut diesel::SqliteConnection,
    spec_id: &str,
    now: i64,
) -> diesel::QueryResult<SpecLiveRuns> {
    let mut live: Vec<db::WorkflowRun> = db::list_runs_for_spec(conn, spec_id)?
        .into_iter()
        .filter(|run| !db::run_status_is_terminal(&run.status))
        .collect();
    let total = live.len();
    live.sort_by_key(|run| (now - run.updated_at).max(0));
    live.truncate(SPEC_RUN_LINES_MAX);

    let mut listed = Vec::with_capacity(live.len());
    for run in live {
        let (flow, scope) = db::get_workflow(conn, &run.workflow_id)?.map_or_else(
            || (run.workflow_id.clone(), "?".to_string()),
            |w| (w.name, w.scope),
        );
        let node = match run.current_node_id.as_deref() {
            Some(node_id) => db::get_node(conn, node_id)?.map(|n| (n.label, n.id)),
            None => None,
        };
        listed.push(SpecRunState {
            run_id: run.id,
            flow,
            scope,
            status: run.status,
            node,
            idle_ms: (now - run.updated_at).max(0),
            child: run.parent_run_id.is_some(),
        });
    }
    Ok(SpecLiveRuns { listed, total })
}

/// A millisecond span as a coarse human duration — the "last activity" of a run.
///
/// COARSE ON PURPOSE: the reader's question is "is anyone still on this?", which days and hours
/// answer and milliseconds do not. The largest unit that is ≥ 1 wins, so nothing ever reads
/// "0 day(s)".
///
/// `pub(crate)` because [`crate::mcp_engine`]'s resume announcement states the SAME fact about the
/// SAME field (`workflow_runs.updated_at`) on the other surface — the run a `run_start` just picked
/// up. One vocabulary for one quantity: "3 day(s)" in the render and "3 days ago" in the warning
/// would be two spellings a reader has to reconcile for nothing.
pub(crate) fn human_span(ms: i64) -> String {
    const MINUTE: i64 = 60 * 1000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    match ms {
        d if d >= DAY => format!("{} day(s)", d / DAY),
        h if h >= HOUR => format!("{} h", h / HOUR),
        m if m >= MINUTE => format!("{} min", m / MINUTE),
        _ => "less than a minute".to_string(),
    }
}

/// How many live runs the `Run     :` block LISTS before it stops listing and starts COUNTING.
///
/// THE BLOCK IS ONE LINE PER LIVE RUN, so its size is a function of state — and of exactly the
/// state [`crate::mcp_engine::stranded_run_warnings`] describes as accumulating in silence:
/// non-terminal runs nobody ever came back to. Left unbounded, a Spec that has collected a dozen of
/// them puts a dozen lines at the top of EVERY render of EVERY pass — the header eating the manual
/// it exists to introduce, on the one input the reader cannot bound.
///
/// THREE, because a Spec holds at most ONE live run per (workflow, Spec) pair (N20), and the set
/// that is actually driving it is the flow itself, the `sub_workflow` child that flow has open, and
/// one more beside them (an authoring run left live). Past that, a fourth line tells the reader
/// nothing a COUNT does not — and the count says "this is accumulation, not a session" out loud,
/// which is the answer they came for.
///
/// WHAT THE CAP BOUNDS IS THE LINE COUNT, NOT THE BYTES. A line's width follows what it has to say
/// — the label of the node its run is parked on, the lineage clause, the STALE sentence — so three
/// lines plus a count is not one number of characters. That is why the render's size is asserted
/// TWICE in [`crate::mcp_context::tests`]: tightly on a one-run Spec, and again at THIS cap, which
/// is the assertion that makes the number above a bound rather than a decoration.
const SPEC_RUN_LINES_MAX: usize = 3;

/// One `Run     :` header line — the state of one live run, and what it means for the reader.
///
/// A run PARKED for longer than [`crate::mcp_engine::STRANDED_BLOCKED_RUN_AFTER_MS`] is marked
/// STALE, with the same threshold `run_start` strands one by: the observed corpses had sat for a
/// week while nothing anywhere surfaced them, and "running, last activity 8 day(s)" read exactly
/// like a colleague's session in progress.
fn run_state_line(state: &SpecRunState) -> String {
    let node = match &state.node {
        Some((label, id)) => format!("parked on \"{label}\" (node_id={id})"),
        None => "not ticked yet (parked at the flow's start)".to_string(),
    };
    let lineage = if state.child {
        " · a sub_workflow CHILD (it inherits this Spec from its parent)"
    } else {
        ""
    };
    let stale = if state.idle_ms >= crate::mcp_engine::STRANDED_BLOCKED_RUN_AFTER_MS {
        " — STALE: nobody has touched it since, so treat it as abandoned work rather than a \
         session in progress"
    } else {
        ""
    };
    format!(
        "Run     : {} ({} · {}) · {} · {} · last activity {} ago{lineage}{stale}\n",
        state.run_id,
        state.flow,
        state.scope,
        state.status,
        node,
        human_span(state.idle_ms),
    )
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
///
/// `subject` is the spawn's per-node FOCUS ([`crate::engine::SpawnConfig::subject`]). When it names
/// one of the Spec's own sections, the Spec block is NARROWED to THAT section — see
/// [`spec_section_focus`]. `None` (or a subject that names no section) renders every section, as
/// before.
pub fn render_agent_context(
    db: &Db,
    role: AgentRole,
    spec_id: &str,
    subject: Option<&str>,
) -> Result<Option<String>, RpcError> {
    let rules = resolve_rule_set(db, role, spec_id);
    render_agent_context_with_rules(db, role, spec_id, &rules, subject)
}

/// The Spec SECTION a spawn's `subject` names, or `None` when it names none.
///
/// F6 — an audit pass is spawned with `subject = "user_stories"` (one of the four commitment
/// sections the ndev graph walks) and audits THAT commitment only; its own manual says "that section
/// is reproduced below". Copying the OTHER five in was the bulk of the pass's context, re-paid four
/// times per audit cycle. So the render carries the named section and drops the rest.
///
/// Matching is on the section's canonical slug (the [`db::SpecSections`] field name), trimmed and
/// case-insensitive, with `-`/space accepted for `_` — the graph writes the slug, a hand-authored
/// node may not. A subject that names NO section (a free-form focus: a file, a component, a
/// question) leaves the render untouched: narrowing on a guess would hide the Spec from a spawn that
/// never asked for a section.
fn spec_section_focus(subject: &str) -> Option<db::SpecSection> {
    let key = subject.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match key.as_str() {
        "problem" => Some(db::SpecSection::Problem),
        "solution" => Some(db::SpecSection::Solution),
        "implementation_decisions" => Some(db::SpecSection::ImplementationDecisions),
        "testing_decisions" => Some(db::SpecSection::TestingDecisions),
        "user_stories" => Some(db::SpecSection::UserStories),
        "out_of_scope" => Some(db::SpecSection::OutOfScope),
        _ => None,
    }
}

/// UI SEAM for the engine graph's NODE INSPECTOR (v2.8 Phase 6): render the EFFECTOR context a
/// `spawn_subagent` node's sub-agent will READ, keyed by the spawn node's `agent_id` (its role slug,
/// e.g. `coder` / `auditor`). The sub-agent itself reaches the same text through the `agent_context`
/// tool; this seam lets the inspector show it without a run ticking.
///
///   * `spec_id = Some(id)` — a RUN is bound: the SAME LIVE [`render_agent_context`] the MCP
///     `agent_context` tool serves (one authority for UI + MCP), with the `{{rules}}` marker
///     RESOLVED by scope (always / role:<role> / fork) + lineage — the annex `context-effecteur`
///     view. `Ok(None)` when the Spec is unknown (the caller maps it to an empty inspector).
///   * `spec_id = None` — NO run bound (review finding #35): the STATIC role TEMPLATE (the
///     sub-agent's brief), with the run-resolved sections shown as explicit "resolved at run"
///     placeholders (see [`render_role_template`]). Always `Ok(Some(_))` for a valid role, so a
///     built-in with no run shows its brief instead of an empty inspector.
///
/// `subject` is the spawn node's own `subject`: when it names a Spec section, the bound render is
/// NARROWED to it (see [`spec_section_focus`]) — the inspector then shows exactly what the pass
/// spawned on that section will read. It has no effect on the run-less template.
///
/// `Err(invalid_argument)` when `agent_id` is not a known role. A pure, side-effect-free read.
pub fn render_effector_context(
    db: &Db,
    agent_id: &str,
    spec_id: Option<&str>,
    subject: Option<&str>,
) -> Result<Option<String>, RpcError> {
    let role = AgentRole::parse(agent_id).ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "'{agent_id}' is not a renderable effector role (accepted: spec, dev, coder, \
                 auditor, doc, prototype)"
            ),
        )
    })?;
    match spec_id {
        Some(spec_id) => render_agent_context(db, role, spec_id, subject),
        None => Ok(Some(render_role_template(role))),
    }
}

/// RENDER the STATIC role TEMPLATE of an effector — its role MANUAL (the sub-agent's brief) —
/// WITHOUT a Spec/run (review finding #35). The node inspector shows this the moment a
/// `spawn_subagent` node is clicked with NO run bound: the brief is STATIC, so it is visible cold.
///
/// The parts that DEPEND on a run — the Spec/Project/Workspace header, the current phase + its
/// tasks, the workspace & repos, and the resolved `{{rules}}` — are rendered as EXPLICIT
/// "resolved at run" placeholders (parity with the live render's section order), never requiring a
/// Spec; a bound run swaps them for the live values via [`render_agent_context`]. A pure function,
/// no DB read (the role manual is a compile-time constant + the deferred-scope naming).
pub fn render_role_template(role: AgentRole) -> String {
    let mut out = String::new();

    // Header — the STATIC template banner. No run, so the Spec/Project/Workspace coordinates are
    // DEFERRED: they resolve when a run binds this workflow to a Spec.
    out.push_str(&format!(
        "=== NYX CONTEXT — {} (role template) ===\n",
        role.label()
    ));
    out.push_str("Spec    : (resolved at run — no run bound yet)\n");
    out.push_str("Workspace: (resolved at run — no run bound yet)\n\n");

    // WHERE THE INSTRUCTIONS COME FROM, before any of them (N29).
    out.push_str(SOURCES_NOTE);

    // A one-line note: this IS the static brief; the run-specific parts fill in when a run binds.
    out.push_str(
        "> This is the sub-agent's STATIC role template (its brief). The Spec / Project / \
         Workspace header, the current phase & tasks, the workspace & repos, and the project rules \
         are RESOLVED AT RUN — bind this workflow to a run on a Spec to see them.\n\n",
    );

    // The ROLE preamble (the manual) — the STATIC part. The `{{rules}}` marker is replaced by the
    // DEFERRED placeholder (rules resolve per run).
    out.push_str(&inject_deferred_rules(role.preamble(), role));
    out.push_str("\n\n");

    // The run-resolved sections, as EXPLICIT placeholders (same shape/order as the live render).
    out.push_str("## Current phase — (resolved at run)\n\n");
    out.push_str("_The Spec's current phase and its tasks appear here once a run is bound._\n\n");
    out.push_str("## Workspace & repos — (resolved at run)\n\n");
    out.push_str("_The dev workspace and its repos appear here once a run is bound._\n");

    out
}

/// Like [`render_agent_context`], but with an EXPLICIT [`RuleSet`] injected at the `{{rules}}`
/// marker. The public entry resolves the set from state (empty today — no store); this seam lets a
/// test drive a POPULATED set through the real render to prove the marker resolves + injects by
/// scope (always / role:<role> / fork) + lineage.
fn render_agent_context_with_rules(
    db: &Db,
    role: AgentRole,
    spec_id: &str,
    rules: &RuleSet,
    subject: Option<&str>,
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
            // The Spec's OWN project, read from `specs.project_id` and NOT through the workspace
            // anchor (finding `01KZ3JQYRN95EETZ0JBQXED13T`). A Spec belongs to a project from the
            // moment it is created; a workspace is the DEV link, added later. Deriving the project
            // from the anchor left every manual project-LESS until somebody anchored a dev
            // workspace — and the PROTOTYPE role, whose whole design half is written
            // `design_recipe(project_id)` / `design_inventory(project_id)` / `design_tokens(
            // project_id)`, runs BEFORE any code and therefore before any dev workspace: that is
            // its NOMINAL case, not its edge case. So the project is resolved here, always, and its
            // repos with it (a `repositories` row belongs to the PROJECT, never to the workspace).
            let project = db::get_project(c, &spec.project_id)?;
            let repos = db::list_repositories(c, &spec.project_id)?;
            // The Spec AUTHOR weaves the project's OTHER live Specs (anti-duplication); the other
            // roles never need them, so we only pay the query for the `spec` role.
            let sibling_specs = if role == AgentRole::Spec {
                db::list_specs(c, Some(&spec.project_id))?
            } else {
                Vec::new()
            };
            // WHAT IS ALREADY DRIVING THIS SPEC — for EVERY role, not just the author. "Does this
            // Spec have a run in progress?" cost the observed session four MCP calls for a fact nyx
            // holds in one join, and the answer it never got is what let a second run open beside a
            // live one.
            let live_runs = spec_live_runs(c, spec_id, db::now_millis())?;
            Ok(Some((
                spec,
                revision,
                tasks,
                workspace_context,
                project,
                repos,
                sibling_specs,
                live_runs,
            )))
        })
        .map_err(internal_db)?;
    let Some((spec, revision, tasks, workspace_context, project, repos, sibling_specs, live_runs)) =
        bundle
    else {
        return Ok(None);
    };

    let mut out = String::new();

    // Header line (native equivalent of depot's `=== DEPOT CONTEXT — <ROLE> ===`).
    out.push_str(&format!("=== NYX CONTEXT — {} ===\n", role.label()));
    // THE `spec_id` IS ON THIS LINE, beside the `#N` — the same reason the project line below carries
    // `project_id=…`: it is the value the tools take, and a header that names the Spec only by the key
    // the human reads leaves the agent to go and find it. It cost four MCP calls in the observed
    // session. The `#N` STAYS: it is what the human reads back.
    out.push_str(&format!(
        "Spec    : {} (#{} · {} · spec_id={})\n",
        spec.title, spec.number, spec.status, spec.id
    ));
    // THE PROJECT LINE IS UNCONDITIONAL (finding `01KZ3JQYRN95EETZ0JBQXED13T`): it is read off the
    // Spec's own `project_id`, so it is there whether or not a dev workspace is anchored. Every
    // `design_*` tool takes a `project_id` and this is where an agent reads it.
    match &project {
        Some(p) => out.push_str(&format!("Project : {} (project_id={})\n", p.name, p.id)),
        // The row is gone (a project deleted under a Spec that survived it): say the ID anyway —
        // it is still what the design tools take, and a blank line would be the silence this
        // header exists to break.
        None => out.push_str(&format!(
            "Project : (unknown project row) (project_id={})\n",
            spec.project_id
        )),
    }
    if let Some(ctx) = &workspace_context {
        out.push_str(&format!(
            "Workspace: {} (workspace_id={})\n",
            ctx.workspace.name, ctx.workspace.id
        ));
    } else {
        out.push_str(
            "Workspace: (no dev workspace anchored — normal before the code exists; nothing below \
             needs one, and every design_* call takes the project_id above)\n",
        );
    }
    // WHAT IS ALREADY DRIVING THIS SPEC — in the header, beside the Spec it is about, because the
    // question it answers ("is someone already on this?") is asked BEFORE any of the manual below.
    //
    // AND THE ABSENCE IS WRITTEN OUT. "No line" and "no run" must not look alike: that confusion IS
    // the defect this whole PRD is about, and here it is the one that decides between resuming a
    // session and opening a second one beside it.
    //
    // EVERY live run, not just the first: a Spec is driven by its top-level flow AND by the
    // `sub_workflow` children that flow opens, and hiding the children would make "one run" a claim
    // the data does not support.
    if live_runs.total == 0 {
        out.push_str(
            "Run     : (none live — no flow is currently driving this Spec; a finished or deleted \
             run is not a live one)\n",
        );
    } else {
        // …AND THE BLOCK IS BOUNDED ([`SPEC_RUN_LINES_MAX`]), then a COUNT. [`spec_live_runs`] has
        // already applied the cap, ordered by LAST ACTIVITY, so what it dropped is what nobody has
        // touched in longest — never the run that is driving the Spec, which is by definition the
        // one moving.
        for state in &live_runs.listed {
            out.push_str(&run_state_line(state));
        }
        let hidden = live_runs.total.saturating_sub(live_runs.listed.len());
        if hidden > 0 {
            // WHAT THIS LINE MAY ADVISE IS BOUNDED BY WHAT IT LEAVES THE READER HOLDING. The hidden
            // runs' `run_id`s are exactly what the cap withheld, and they sit on as many DIFFERENT
            // workflows as there are runs (N20: one live run per (workflow, Spec) pair) — so
            // `run_list`, which takes a `workflow_id` and answers for ONE flow, cannot be pointed at
            // this set at all, and `run_delete` needs an id this line does not carry. Saying
            // otherwise would send the reader on the very `workflow_list` + N × `run_list` + filter
            // detour this header exists to spare them, which is the "advice naming a verb that does
            // not exist" defect (`01M1008NHM9RRA1ZAS4V7K1PM4`) in its subtler form: a verb that
            // exists and does not do this. So the line ADVISES NOTHING about the hidden set, states
            // that no verb is Spec-scoped, and points `run_delete` at the ids that ARE above it.
            out.push_str(&format!(
                "Run     : … and {hidden} more live run(s) not listed — the ones nothing has \
                 touched in longest ({} live on this Spec in all). That many at once is accumulated \
                 work, not sessions in progress. No verb lists a Spec's runs across flows ({} takes \
                 one workflow_id, and these sit on that many flows); {} clears one by the run_id \
                 the lines above carry.\n",
                live_runs.total,
                crate::mcp_authoring::RUN_LIST_TOOL,
                crate::mcp_authoring::RUN_DELETE_TOOL,
            ));
        }
    }
    out.push('\n');

    // WHERE THE INSTRUCTIONS COME FROM, before any of them (N29): the project's ambient `CLAUDE.md`
    // outranks this manual on the project's own ground; this manual carries the ROLE.
    out.push_str(SOURCES_NOTE);

    // ROLE preamble (the fixed contract). Its `{{rules}}` marker is RESOLVED + INJECTED by scope; a
    // preamble with no marker (spec / prototype) passes through unchanged.
    out.push_str(&rules.inject(role.preamble(), role));
    out.push_str("\n\n");

    // SPEC AUTHOR live state: the project's OTHER Specs, so the author does not duplicate an
    // existing one (anti-duplication). Rendered only for the `spec` role.
    if role == AgentRole::Spec {
        out.push_str("## Existing Specs in the project (anti-duplication)\n\n");
        let siblings: Vec<&db::Spec> = sibling_specs.iter().filter(|s| s.id != spec.id).collect();
        if siblings.is_empty() {
            out.push_str("_No other Specs in this project yet._\n\n");
        } else {
            for s in siblings {
                out.push_str(&spec_line(s));
            }
            out.push('\n');
        }
    }

    // SPEC sections (the six structured sections from the active revision) — under the SAME
    // identifying header whether all six are carried or only the one this spawn is about (F6).
    out.push_str(&format!(
        "## Spec — {} (revision {})\n\n",
        revision.title, revision.revision_number
    ));
    match subject.and_then(spec_section_focus) {
        // NARROWED: the spawn names ONE section — carry that one, and SAY it is the only one, so a
        // sub-agent never mistakes the absence of the others for an empty Spec (F6).
        Some(section) => {
            out.push_str(&format!(
                "_Your subject is `{}` — only that section is reproduced here, on purpose. The \
                 Spec's other sections are another pass's business; their absence is not an empty \
                 Spec._\n\n",
                section.as_str()
            ));
            out.push_str(&section_block(
                section_heading(section),
                revision_section(&revision, section),
            ));
        }
        // No subject, or a subject naming no section: the whole Spec, unchanged.
        None => {
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
        }
    }

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

    // PROJECT + its REPOS, and the WORKSPACE when one is anchored. The project half is
    // UNCONDITIONAL (finding `01KZ3JQYRN95EETZ0JBQXED13T`): repos belong to the project, and the
    // design tools walk them by `project_id` alone — a prototype agent must be able to run the whole
    // recipe → inventory → tokens → preview chain with nothing but this section.
    out.push_str("## Project, repos & workspace\n\n");
    match &project {
        Some(p) => out.push_str(&format!(
            "- project: {} (key={}, project_id={})\n",
            p.name, p.key, p.id
        )),
        None => out.push_str(&format!(
            "- project: (unknown project row) (project_id={})\n",
            spec.project_id
        )),
    }
    let live_repos: Vec<&db::Repository> =
        repos.iter().filter(|r| r.archived_at.is_none()).collect();
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
    match &workspace_context {
        Some(ctx) => out.push_str(&format!(
            "- workspace: {} ({}, workspace_id={})\n",
            ctx.workspace.name, ctx.workspace.root_path, ctx.workspace.id
        )),
        // The WHY is on the header's Workspace line, once (v2.8): repeating it here said the same
        // thing twice to every role.
        None => out.push_str("- workspace: (none anchored — see the header)\n"),
    }
    out.push('\n');

    Ok(Some(out))
}

/// RENDER the SPEC AUTHOR's project-scoped AUTHORING manual — the /nspec context when NO Spec
/// is anchored yet (the author is about to CREATE one). This is the fix for review finding
/// 01KXENA8R6NC2F2P8SSNTZ6YXJ: /nspec must NEVER error "no Spec anchored on this workspace",
/// because a Spec is a PROJECT artifact, not a workspace anchor.
///
/// The manual carries: the SPEC-author role preamble (the full grill → write → decompose →
/// spec_create contract), the PROJECT header, the project's EXISTING Specs (anti-duplication —
/// even zero), and an explicit "author a new Spec via spec_create with this project_id" step.
/// `project = None` (cwd matched no workspace/project) renders a GRACEFUL manual that asks the
/// human to select the project — still the authoring contract, never a hard error.
///
/// `notice` is the case-0 line ([`UnanchoredSignal::notice`]): the SAME sentence the payload's
/// `warnings` carries, rendered into the header so the author reads WHY no Spec is in scope
/// (a location with none / no location at all) instead of inferring it from a `null` field. Empty
/// for a caller that has nothing to signal.
fn render_spec_authoring(
    db: &Db,
    project: Option<&db::Project>,
    notice: &str,
) -> Result<String, RpcError> {
    let mut out = String::new();

    // Header — the authoring variant of the context banner. No Spec yet; the SCOPE is the
    // project (or, unresolved, an invitation to pick one). Deliberately NO "anchored on a
    // workspace" wording — /nspec is project-scoped.
    out.push_str("=== NYX CONTEXT — SPEC (authoring) ===\n");
    match project {
        Some(p) => out.push_str(&format!("Project : {} (project_id={})\n", p.name, p.id)),
        None => out.push_str("Project : (not resolved — select the project to author in)\n"),
    }
    out.push_str("Spec    : (none yet — you are authoring a new one)\n\n");

    // WHY there is none — beside the header that states it, never further down.
    out.push_str(notice);

    // WHERE THE INSTRUCTIONS COME FROM, before any of them (N29).
    out.push_str(SOURCES_NOTE);

    // The fixed SPEC-author contract (grill → write → decompose → spec_create + guard-rails).
    out.push_str(AgentRole::Spec.preamble());
    out.push_str("\n\n");

    // The project's EXISTING Specs — anti-duplication scoped by project_id, rendered even when
    // there are none (the /nspec author must see "is there already a Spec for this?" up front).
    out.push_str("## Existing Specs in the project (anti-duplication)\n\n");
    match project {
        Some(p) => {
            let specs = db
                .with_conn(|c| db::list_specs(c, Some(&p.id)))
                .map_err(internal_db)?;
            if specs.is_empty() {
                out.push_str(
                    "_No Specs in this project yet — you are authoring the first one._\n\n",
                );
            } else {
                for s in &specs {
                    out.push_str(&spec_line(s));
                }
                out.push('\n');
            }
        }
        None => {
            out.push_str(
                "_Select the project first (pass project_id, or run from a workspace folder) to \
                 list its Specs._\n\n",
            );
        }
    }

    // The project's OPEN idea backlog — the maintained follow-ups the author must SEARCH before
    // creating a duplicate Spec (PRD Impl Decisions: "INTÉGRÉ AU /nspec … au démarrage d'une Spec,
    // l'agent cherche d'office les idées du sujet … et les auto-linker (kind=addresses)"). Surfaced
    // up front as a bounded preview so the backlog is visible.
    //
    // AND IT IS THE ONLY HOME THE CONTRACT HAS. The nspec GRAPH walks no idea node, so the
    // "no duplicate Spec" rule has no structural equivalent — deleting the preamble's Step 0
    // therefore had to land it somewhere, and this is the one place that can state it with THIS
    // project's id already in the call. Two verbs, not a procedure: SEARCH before you author, and
    // LINK what the Spec resolves (`idea_link` then drives the idea's lifecycle by itself).
    out.push_str("## Existing ideas — search the backlog before you spec (anti-duplication)\n\n");
    match project {
        Some(p) => {
            let ideas = db
                .with_conn(|c| {
                    db::list_ideas(
                        c,
                        Some(&p.id),
                        db::IdeaStatusFilter::One(db::IDEA_STATUS_OPEN),
                        None,
                        SPEC_AUTHORING_IDEA_PREVIEW,
                        0,
                    )
                })
                .map_err(internal_db)?;
            if ideas.is_empty() {
                out.push_str(
                    "_No open ideas in this project yet — still, run \
                     idea_search(project_id=…, query=…) on the subject before you author, in case \
                     one is tracked._\n\n",
                );
            } else {
                for i in &ideas {
                    let theme = i.theme.as_deref().unwrap_or("—");
                    out.push_str(&format!("- {} · {} · theme={}\n", i.id, i.title, theme));
                }
                out.push('\n');
            }
            out.push_str(&format!(
                "Run idea_search(project_id=\"{}\", query=\"<the subject>\") on THIS topic before \
                 you author: a match means the need is already tracked, so SURFACE it and build the \
                 Spec around it rather than opening a duplicate Spec. Then link every idea this \
                 Spec resolves with idea_link(idea_id, spec_id, kind=\"addresses\") — that link \
                 drives the idea's lifecycle on its own.\n\n",
                p.id
            ));
        }
        None => {
            out.push_str(
                "_Select the project first (pass project_id, or run from a workspace folder), then \
                 idea_search its backlog before creating a Spec._\n\n",
            );
        }
    }

    // The concrete "create the Spec" step: the author CREATES a Spec in THIS project.
    out.push_str("## Author a new Spec (there is none yet)\n\n");
    match project {
        Some(p) => {
            out.push_str(&format!(
                "Once you have grilled the intent and written the design, CREATE the Spec with \
                 spec_create(project_id=\"{}\", title=…) — a Spec belongs to the PROJECT. Fill \
                 its sections with spec_update_section, then slice it into tasks with task_add, \
                 grouping each by a phase NAME (task_add(phase=\"Phase name\")) — nyx builds the \
                 phase plan from those names, in first-seen order. The Spec stays PROJECT-scoped \
                 while authored; nyx poses the dev workspace anchor itself, at the transition node \
                 that opens the implementation.\n",
                p.id
            ));
        }
        None => {
            out.push_str(
                "First SELECT the project this Spec belongs to (pass project_id, or run /nspec \
                 from inside a nyx workspace folder so the project resolves from the cwd). Then \
                 CREATE the Spec with spec_create(project_id=…, title=…) — a Spec belongs to the \
                 PROJECT, not to a workspace.\n",
            );
        }
    }

    Ok(out)
}

/// THE ANCHOR a context call actually used — and the ONE place the anchoring ORDER of a context
/// render lives (PDR-0020): `spec_id` → `run_id` → `workspace_id` → `cwd`.
///
/// WHY THIS TYPE EXISTS (standards finding `01M0813X53D1NRPAXYJGG6RJAV`). Two functions ask
/// questions about that order: [`resolve_spec_id`], which RESOLVES the Spec, and
/// [`unanchored_signal`], which DIAGNOSES the case where nothing resolved. They used to walk two
/// hand-written copies of the same list, and two lists drift. This repo has already paid for that
/// exact class of bug inside this very PRD: `best_workspace_for_cwd` had silently diverged from
/// `crate::resolve::resolve_workspace` (it compared on a hard-coded `/`), so no nested cwd resolved
/// on Windows at all — see the note on [`best_workspace_for_cwd`], which states the rule that two
/// selectors answering the same question must not drift apart.
///
/// THE ARBITRATION, written down because the finding asked for it. The alternatives offered were a
/// cross-comment or a test that reddens on divergence; FACTORING was chosen over both because it
/// makes the failure IMPOSSIBLE rather than merely detectable, and because the mechanical part
/// carries less risk than the duplication does: the order is read once, here, and each caller only
/// says what it does with the anchor it is handed. The two `match` blocks below are also written
/// WITHOUT a catch-all arm on purpose — adding a fifth anchor forces the compiler to stop at
/// [`unanchored_signal`] too, so a new anchor cannot be added resolvable-but-undiagnosed, which is
/// precisely the silent failure the finding described (a diagnosis naming the argument the caller
/// did not use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecAnchor<'a> {
    /// An explicit `spec_id`. It always RESOLVES — an id naming no Spec is refused later, at the
    /// render — so it is the one anchor [`unanchored_signal`] can never be asked about.
    Explicit(&'a str),
    /// A `run_id`: the anchor a SPAWNED sub-agent has, and the only one it needs. The walk STOPS
    /// here — bound ⇒ that run's Spec, unbound ⇒ none.
    Run(&'a str),
    /// A `workspace_id`: the Spec anchored on that dev workspace.
    Workspace(&'a str),
    /// A `cwd`: the Spec anchored on the workspace whose path matches it ("sans id → spec résolue
    /// via cwd").
    Cwd(&'a str),
}

/// The FIRST anchor the caller passed, in [`SpecAnchor`]'s order; `None` when none was passed.
/// THE ONLY WALK of that order in this module — both [`resolve_spec_id`] and [`unanchored_signal`]
/// go through it.
fn spec_anchor(args: &Value) -> Result<Option<SpecAnchor<'_>>, RpcError> {
    if let Some(spec_id) = optional_str(args, "spec_id")? {
        return Ok(Some(SpecAnchor::Explicit(spec_id)));
    }
    if let Some(run_id) = optional_str(args, "run_id")? {
        return Ok(Some(SpecAnchor::Run(run_id)));
    }
    if let Some(workspace_id) = optional_str(args, "workspace_id")? {
        return Ok(Some(SpecAnchor::Workspace(workspace_id)));
    }
    if let Some(cwd) = optional_str(args, "cwd")? {
        return Ok(Some(SpecAnchor::Cwd(cwd)));
    }
    Ok(None)
}

/// WHO REFUSES a project-relative `spec_id` (`#N` / bare `N`) that the call anchors no project for.
/// Both callers of [`resolve_spec_id`] expose ONLY optional anchors, so both can be reached without
/// a project — they differ in WHERE the refusal that says so is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectRelative {
    /// REFUSED HERE, by name, before anything is read — `agent_context`, which otherwise answered
    /// the literal `unknown spec 3` this grammar exists to retire, on a field whose description had
    /// just promised the form (finding `01M0ZKWXS353TABS0WCT3XXB81`).
    RefusedHere,
    /// LEFT TO TRAVEL, because the caller owns a refusal that says MORE — `run_start`, whose
    /// [`crate::mcp_engine`] refusal always names the accepted forms, then says what THIS call makes
    /// actionable — the two are exclusive: anchoring no project, the anchors to add and the forms
    /// that carry their own; anchoring one, what that project actually holds (its nearest Spec, or
    /// that it holds none yet). Raising a poorer refusal one layer earlier would be a downgrade,
    /// not a fix.
    RefusedByTheCaller,
}

/// RESOLVE the Spec id for a context render from the tool args, at the anchor [`spec_anchor`]
/// stopped on — this function no longer decides the ORDER, it only says what each anchor resolves.
///
/// Returns `Ok(None)` when none resolves (the caller maps it to `invalid_id`, or — for the two
/// roles allowed to work without a Spec — to [`unanchored_signal`]).
fn resolve_spec_id(
    db: &Db,
    args: &Value,
    project_relative: ProjectRelative,
) -> Result<Option<String>, RpcError> {
    match spec_anchor(args)? {
        // THE ONE PLACE THE SHORT-REFERENCE GRAMMAR ENTERS a context render. Widening it HERE is
        // what makes `#3` / `KEY-3` / `3` work for `agent_context`, `run_start` and every `spec_*`
        // tool at once, with no caller re-implementing the forms — see [`resolve_spec_arg`].
        Some(SpecAnchor::Explicit(spec_id)) => Ok(Some(match project_relative {
            ProjectRelative::RefusedHere => {
                resolve_spec_arg_project_relative_refused(db, args, "spec_id", spec_id)?
            }
            ProjectRelative::RefusedByTheCaller => resolve_spec_arg(db, args, spec_id)?,
        })),
        Some(SpecAnchor::Run(run_id)) => {
            // AN UNBOUND RUN RESOLVES NO SPEC, and that is an ANSWER — not a reason to look further.
            //
            // Reaching past the run to its workspace would raise the ambiguity a real project always
            // carries (several active Specs, so no location names one), which is exactly what
            // `run_start` declines to raise for a flow that AUTHORS its own Spec (G2). The hook pulls
            // this render to inject it (G6), so the refusal would land there and inject nothing — the
            // two would cancel out in the very case that motivated them.
            //
            // It would also be wrong on its own merits: a run that drives no Spec would be handed the
            // state of one it has nothing to do with.
            Ok(require_run(db, run_id)?.spec_id)
        }
        Some(SpecAnchor::Workspace(workspace_id)) => unique_spec_for_workspace(db, workspace_id),
        Some(SpecAnchor::Cwd(cwd)) => {
            let normalized = crate::pathnorm::normalize(cwd);
            let ws = db
                .with_conn(|c| best_workspace_for_cwd(c, &normalized))
                .map_err(internal_db)?;
            match ws {
                Some(ws) => unique_spec_for_workspace(db, &ws.id),
                None => Ok(None),
            }
        }
        None => Ok(None),
    }
}

/// TRANSLATE a `spec_id` ARGUMENT through the shared grammar — THE ONE ENTRY POINT, and the only
/// place in the repo where a typed token becomes a Spec id ([`crate::spec_ref`] owns the forms).
///
/// A reference that RESOLVES comes back as the canonical id; ANYTHING ELSE comes back exactly as it
/// was sent — a canonical id (untouched, no lookup, so a caller passing one is affected by nothing
/// here), prose, or a reference designating no Spec. The caller's own refusal therefore still sees
/// the token the caller was given, and still names it: widening the grammar adds resolution
/// WITHOUT taking any existing refusal away.
///
/// THE PROJECT IS THE CALL'S OWN, never one this function goes looking for. `#N` and the bare `N`
/// carry no project, so they resolve in the one [`resolve_project`] already derives from the
/// anchors every tool passes (`project_id` | `run_id` | `workspace_id` | `cwd`) — the SAME
/// resolution `run_start` uses, not a second one. No anchor ⇒ nothing to count in ⇒ the token
/// stands (and the caller refuses it, naming [`crate::spec_ref::ANCHORLESS_FORMS`]). The canonical
/// id and `KEY-N` describe themselves, so they never pay for that resolution.
///
/// THE CROSS-PROJECT GUARD IS UNTOUCHED, and that is the point of resolving to an id and stopping:
/// a `NYX-3` under a `cwd` of another project resolves to the Spec `NYX-3` names, then meets
/// [`resolve_run_target`]'s existing disagreement refusal exactly as a hand-typed id would. A short
/// reference must not become the back door through which another project's Spec enters.
pub(crate) fn resolve_spec_arg(db: &Db, args: &Value, token: &str) -> Result<String, RpcError> {
    let project_id = match crate::spec_ref::parse(token) {
        // Not a reference, or a canonical id: no project, no query, no change.
        None | Some(crate::spec_ref::SpecRef::Uuid(_)) => return Ok(token.to_string()),
        // `KEY-N` names its own project; asking for the call's would only decide whether to report a
        // disagreement, and the callers confront that on the RESOLVED id anyway.
        Some(crate::spec_ref::SpecRef::Key { .. }) => None,
        Some(crate::spec_ref::SpecRef::Number(_)) => resolve_project(db, args)?.map(|p| p.id),
    };
    resolve_spec_ref_in(db, project_id.as_deref(), token)
}

/// [`resolve_spec_arg`] for a caller that ALREADY knows the project (it took one as an argument),
/// so the anchors need not be walked a second time.
pub(crate) fn resolve_spec_ref_in(
    db: &Db,
    project_id: Option<&str>,
    token: &str,
) -> Result<String, RpcError> {
    let resolution = db
        .with_conn(|c| crate::spec_ref::resolve(c, project_id, token))
        .map_err(internal_db)?;
    match resolution {
        crate::spec_ref::SpecRefResolution::Resolved(r) => Ok(r.spec_id),
        // Canonical / NotAReference / Unresolved — the token stands as sent (see the note above).
        _ => Ok(token.to_string()),
    }
}

/// A REQUIRED Spec-reference argument: [`crate::mcp_runtime::require_str`] then the grammar. The
/// shape every tool taking a mandatory `spec_id` reads its argument through, so none of them ever
/// learns the forms.
pub(crate) fn require_spec_arg(db: &Db, args: &Value, key: &str) -> Result<String, RpcError> {
    let token = require_str(args, key)?;
    resolve_spec_arg(db, args, token)
}

/// [`require_spec_arg`] PLUS THE REFUSAL A PROJECT-RELATIVE FORM EARNS on a call that resolves no
/// project — what nyx advertises, nyx accepts, in BOTH directions.
///
/// `#N` and the bare `N` are counted INSIDE a project ([`crate::spec_ref`] never looks past the one
/// it is given). The advertised `spec_id` FIELDS of `spec_fork`, `spec_link`, `spec_unlink`,
/// `spec_deps`, `idea_link`, `idea_unlink`, `annexe_add`, `annexe_list`, `proto_get` and
/// `proto_create` — `spec_link` and `spec_unlink` carry two each — take NONE of the four anchors
/// [`resolve_project`] reads, so on them a number can never be counted. Left to
/// [`resolve_spec_arg`] alone the token simply stood, travelled down to the row read, and came back
/// as `unknown spec 3`: the exact sentence this grammar exists to retire, this time on a field whose
/// description had just PROMISED the form (finding `01M0ZGCCSQ2F25HPBS6NX5JAFS`, and the arbitrage
/// recorded out-of-scope `01M0D95TFHNMGZ9ES20Z68R395` — those fields stop announcing the four forms
/// AND refuse explicitly).
///
/// The refusal is raised HERE, before any read or write, and it is ACTIONABLE: it says why a number
/// cannot resolve on this call and names [`crate::spec_ref::ANCHORLESS_FORMS`] — what to send
/// instead. `invalid_argument`, not `invalid_id`: the Spec is not unknown, the FORM is unusable here.
///
/// THE CONDITION IS THE CALL'S, NOT THE TOOL'S, which is what makes one function serve two families.
/// A field whose anchor is OPTIONAL can reach this very guard too, and it then decides per CALL:
/// `spec_update_section` (an optional `run_id`, the human-gated correction path) and
/// `spec_set_workspace` (an optional `workspace_id` whose ABSENCE is the detach, so a number can
/// never resolve while detaching) are refused by name when the anchor is missing and resolve when it
/// is there (findings `01M0ZGHD137MDXFGS2PBG6GS68`, `01M0ZKWXS353TABS0WCT3XXB81`). Their
/// descriptions state that condition, so the two halves still say the same thing. `agent_context`
/// meets the same guard body through the other entry point below, on the token [`SpecAnchor`] handed
/// it ([`ProjectRelative::RefusedHere`]).
///
/// AN OPTIONAL ANCHOR IS NOT A PROMISE OF THIS GUARD: `run_start` is routed AROUND it, on to
/// [`resolve_spec_arg`], because its own refusal says more — see
/// [`ProjectRelative::RefusedByTheCaller`], which owns that arbitrage.
///
/// Everything else is unchanged: a canonical id, a `KEY-N` and prose never reach the guard (only
/// [`crate::spec_ref::SpecRef::Number`] does), and the resolution that follows is the SAME one every
/// other `spec_id` argument goes through.
pub(crate) fn require_spec_arg_project_relative_refused(
    db: &Db,
    args: &Value,
    key: &str,
) -> Result<String, RpcError> {
    let token = require_str(args, key)?.to_string();
    resolve_spec_arg_project_relative_refused(db, args, key, &token)
}

/// [`require_spec_arg_project_relative_refused`] on a token the caller ALREADY holds — what
/// `require_…` itself delegates to once it has read the key, and what a caller that reached the
/// token through [`SpecAnchor`] rather than by reading the key calls directly (`agent_context`).
/// Those two are its whole readership.
///
/// THE ANCHORS ARE WALKED AT MOST ONCE. The guard needs the project to decide whether to refuse, and
/// the resolution needs the same project to count the number in — so the project the guard resolved
/// is HANDED to [`resolve_spec_ref_in`] instead of being recomputed by [`resolve_spec_arg`], which
/// would call [`resolve_project`] a second time on the same arguments. On `spec_update_section` with
/// a `run_id` that second walk was a whole `require_run` + `get_spec` + `get_project` replayed for
/// nothing (finding `01M0ZMA20NRB2WDSTSHE63YYX5`). Behaviour is unchanged in every branch: the same
/// refusal, the same codes, the same resolution.
pub(crate) fn resolve_spec_arg_project_relative_refused(
    db: &Db,
    args: &Value,
    key: &str,
    token: &str,
) -> Result<String, RpcError> {
    if matches!(
        crate::spec_ref::parse(token),
        Some(crate::spec_ref::SpecRef::Number(_))
    ) {
        let Some(project) = resolve_project(db, args)? else {
            return Err(project_relative_refusal(token, key));
        };
        return resolve_spec_ref_in(db, Some(&project.id), token);
    }
    resolve_spec_arg(db, args, token)
}

/// THE REFUSAL A PROJECT-RELATIVE FORM EARNS when the call resolves no project — written once, so
/// the two entry points that raise it cannot word it differently.
fn project_relative_refusal(token: &str, key: &str) -> RpcError {
    RpcError::new(
        RpcCode::InvalidArgument,
        format!(
            "'{token}' is a PROJECT-RELATIVE reference: a '#N' or a bare number is counted inside \
             a project, and this call resolves none — it carries no project_id / workspace_id / \
             cwd / run_id to read one from. Send {} in '{key}' instead; both carry their own \
             project",
            crate::spec_ref::ANCHORLESS_FORMS
        ),
    )
}

/// The OPTIONAL twin of [`require_spec_arg`] — absent / null / empty stays `None`, exactly as
/// [`crate::mcp_runtime::optional_str`] decides it.
pub(crate) fn optional_spec_arg(
    db: &Db,
    args: &Value,
    key: &str,
) -> Result<Option<String>, RpcError> {
    match optional_str(args, key)? {
        Some(token) => Ok(Some(resolve_spec_arg(db, args, token)?)),
        None => Ok(None),
    }
}

/// THE RUN ANCHOR — load the run `run_id` names, or refuse with `invalid_id`.
///
/// A run is the ONE identifier that resolves everything a spawned sub-agent needs: it carries the
/// Spec it drives (F5) and the workspace it opened against (G2). That is what lets a `spawn_subagent`
/// action ship an id instead of a manual — the sub-agent calls `agent_context(run_id=…)` and pulls
/// its own state. An unknown id is REFUSED rather than silently resolving nothing: a sub-agent that
/// got a bad id must learn it here, not discover an empty render.
fn require_run(db: &Db, run_id: &str) -> Result<db::WorkflowRun, RpcError> {
    db.with_conn(|c| db::get_workflow_run(c, run_id))
        .map_err(internal_db)?
        .ok_or_else(|| RpcError::new(RpcCode::InvalidId, format!("unknown run {run_id}")))
}

/// The `reason` marker on the AMBIGUOUS-location refusal — the machine handle a caller uses to tell
/// "this location anchors several Specs" apart from every other `invalid_argument`. `run_start` reads
/// it to start an authoring flow UNBOUND instead of losing the call (G2).
const AMBIGUOUS_SPEC_REASON: &str = "spec_ambiguous_for_workspace";

/// Whether `e` is the AMBIGUOUS-location refusal [`unique_spec_for_workspace`] raises (a workspace
/// with several active Specs), as opposed to any other refusal a Spec resolution can produce.
pub(crate) fn is_ambiguous_spec(e: &RpcError) -> bool {
    e.data
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        == Some(AMBIGUOUS_SPEC_REASON)
}

/// Resolve the ONE ACTIVE Spec anchored on `workspace_id`, REFUSING when several are — a workspace
/// with more than one started Spec cannot resolve one by location alone, so picking the newest
/// silently would attribute work to an ARBITRARY Spec (v2.8 review N14(b)). The refusal LISTS the
/// candidates so the caller can pass an explicit `spec_id`.
///
/// THE 0/1/N CONTRACT, and the only one: 0 candidates ⇒ `Ok(None)` (the caller decides whether an
/// unbound flow is legitimate), 1 ⇒ `Ok(Some(id))`, N ⇒ `invalid_argument` carrying
/// `reason = spec_ambiguous_for_workspace` and the candidate list. BOTH location paths of
/// [`resolve_spec_id`] — an explicit `workspace_id` and a `cwd` (through
/// [`best_workspace_for_cwd`]) — land HERE, so there is exactly one place where a location becomes
/// a Spec and NO caller anywhere selects the most recent candidate. The db-side companion that
/// returned "the newest anchored Spec" was DELETED for that reason (see the note where it used to
/// live in `db.rs`).
///
/// The candidate set is [`db::active_specs_for_workspace`]: the TERMINAL Specs a workspace
/// accumulates as it ships (`completed` / `cancelled`) are NOT candidates, so a workspace whose only
/// live Spec is X resolves to X however many finished ones share it — and the message below, which
/// says "active Specs", is true.
fn unique_spec_for_workspace(db: &Db, workspace_id: &str) -> Result<Option<String>, RpcError> {
    let specs = db
        .with_conn(|c| db::active_specs_for_workspace(c, workspace_id))
        .map_err(internal_db)?;
    match specs.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.id.clone())),
        many => {
            let candidates = many
                .iter()
                .map(|s| format!("{} (\"{}\")", s.id, s.title))
                .collect::<Vec<_>>()
                .join(", ");
            Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "workspace {workspace_id} has {} active Specs — resolving one by location alone \
                     is ambiguous; pass an explicit spec_id. Candidates: {candidates}",
                    many.len()
                ),
            )
            .with_data(json!({ "reason": AMBIGUOUS_SPEC_REASON })))
        }
    }
}

/// The `agent_context` WARNING code for the case 0 where a location RESOLVED a workspace and that
/// workspace anchors NO active Spec. A LEGITIMATE state — a project's first `/nspec` starts exactly
/// there — so it is SIGNALLED, never refused (refusing would break the one entry point authoring
/// has). A STABLE machine code, sibling of `run_start`'s `spec_ambiguous_started_unbound` /
/// `gate_unconfigured` in the same `warnings` array; the human sentence beside it is free to change.
const NO_SPEC_ON_LOCATION_WARNING_CODE: &str = "spec_none_on_location";

/// The `agent_context` WARNING code for the OTHER case 0: NO workspace resolved at all — no
/// anchoring argument was passed, or the `cwd` matches no workspace nyx knows. That one is an
/// INVOCATION mistake (the observed symptom: an agent calling from the wrong folder and receiving a
/// perfectly normal-looking context), which is exactly why it must not share a code with the
/// legitimate case above.
const NO_LOCATION_WARNING_CODE: &str = "spec_location_unresolved";

/// The `agent_context` WARNING code for a `run_id` naming a run bound to NO Spec — the third shape
/// of case 0, kept apart because the location resolved perfectly well: it is the RUN that carries no
/// Spec (an authoring run has none until it creates one), and telling such a caller to "pass a cwd"
/// would name the wrong fix.
const RUN_UNBOUND_WARNING_CODE: &str = "spec_run_unbound";

/// THE CASE-0 SIGNAL — the machine CODE and the human SENTENCE, emitted TOGETHER on the two
/// surfaces that must agree: the `warnings` array of the payload (the dispatcher relays each entry
/// to the human, see the `WARNINGS in that block` rule of the dispatcher preamble) and one explicit
/// LINE inside the rendered manual (an agent reading the text must not have to notice a null field).
/// ONE sentence, built once, so the two can never drift.
struct UnanchoredSignal {
    code: &'static str,
    message: String,
}

impl UnanchoredSignal {
    /// The `warnings` entry — `{ code, message }`, the shape `run_start` already publishes.
    fn warning(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }

    /// The LINE rendered into the manual itself. A blockquote, like [`SOURCES_NOTE`], so it reads as
    /// a note ABOUT the manual rather than as one of its instructions.
    fn notice(&self) -> String {
        format!("> NO SPEC ANCHORED — {}\n\n", self.message)
    }
}

/// DIAGNOSE a case 0 of the 0/1/N contract: [`resolve_spec_id`] resolved NO Spec, and this says
/// WHICH of the three very different situations that is.
///
/// It reads the anchor through [`spec_anchor`] — the SAME single walk the resolution used, not a
/// second copy of the order — so the sentence can only ever name the argument the resolution
/// actually stopped on. That shared walk is the whole point: a diagnosis with its own copy of the
/// order keeps compiling after the order changes and starts naming an argument the caller never
/// passed, which would make this signal WORSE than the silence it replaced.
///
/// It re-reads the workspace instead of threading the resolution's own findings back out, because
/// the three code paths that produce `Ok(None)` (an unbound run, a workspace with zero active Specs,
/// a cwd matching nothing) share no return channel — and a diagnosis is a READ, taken on the case
/// that already lost. An UNKNOWN `workspace_id` lands in [`NO_LOCATION_WARNING_CODE`] on purpose: a
/// location that names nothing is an invocation mistake, whatever argument carried it.
fn unanchored_signal(db: &Db, args: &Value) -> Result<UnanchoredSignal, RpcError> {
    let located = match spec_anchor(args)? {
        Some(SpecAnchor::Run(run_id)) => {
            // The run RESOLVED (a bad id was already refused upstream) and carries no Spec: the
            // anchor is fine, the run is simply unbound — `resolve_spec_id` STOPS at the run.
            // IT NAMES THE REMEDY, not only the state. The observed session read this warning,
            // relayed it verbatim to the human — the only thing it could do with it — and went on
            // to be told "No Spec yet, author a NEW one from scratch" about a Spec the human had
            // just named. What was missing was never the diagnosis; it was that the way OUT went
            // unsaid while the way DEEPER IN was written in the very manual this line rides on.
            // `run_bind_spec` (the recovery this PRD exposed) repairs the run in place; it is
            // pointed at BY NAME, with this run's own id already in the call.
            //
            // THE NAME COMES FROM THE VERB'S OWN CONSTANT, never re-typed here: advice naming a
            // tool that does not exist is worse than no advice, and this repo has already paid for
            // a refusal that named an argument its schema did not carry (finding
            // `01M1008NHM9RRA1ZAS4V7K1PM4`). Renaming the verb re-words this sentence with it.
            return Ok(UnanchoredSignal {
                code: RUN_UNBOUND_WARNING_CODE,
                message: format!(
                    "run {run_id} drives NO Spec — an authoring run has none until it creates one. \
                     Nothing below is scoped to a Spec, and every Spec-scoped fact of this run \
                     answers the empty value. IF THE HUMAN NAMED AN EXISTING SPEC, this run missed \
                     it: do NOT author a second one — anchor THIS run on theirs with \
                     {}(run_id=\"{run_id}\", spec_id=…), which keeps the session and everything it \
                     has already walked, then re-read this manual. Ask the human which Spec they \
                     meant rather than guessing",
                    crate::mcp_engine::RUN_BIND_SPEC_TOOL
                ),
            });
        }
        Some(SpecAnchor::Workspace(workspace_id)) => db
            .with_conn(|c| db::get_workspace(c, workspace_id))
            .map_err(internal_db)?,
        Some(SpecAnchor::Cwd(cwd)) => {
            let normalized = crate::pathnorm::normalize(cwd);
            db.with_conn(|c| best_workspace_for_cwd(c, &normalized))
                .map_err(internal_db)?
        }
        // An explicit `spec_id` ALWAYS resolves, so it never reaches a case-0 diagnosis; no anchor
        // at all is the same "located nothing" as a cwd matching nothing. Both are spelled out
        // rather than folded into a `_` arm: a fifth anchor added to [`SpecAnchor`] must then break
        // the build HERE, not silently fall through to the wrong sentence.
        Some(SpecAnchor::Explicit(_)) | None => None,
    };
    Ok(match located {
        Some(ws) => UnanchoredSignal {
            code: NO_SPEC_ON_LOCATION_WARNING_CODE,
            message: format!(
                "workspace {} (\"{}\") carries no ACTIVE Spec — the location is a real workspace, it \
                 just has none yet, which is the normal state of a project about to author its \
                 first. Nothing below is scoped to a Spec: pass an explicit spec_id to work on one",
                ws.id, ws.name
            ),
        },
        None => UnanchoredSignal {
            code: NO_LOCATION_WARNING_CODE,
            message:
                "this call resolved NO workspace at all: no workspace_id/cwd was passed, or the cwd \
                 names a folder nyx tracks no workspace for. If you expected a Spec here, you are \
                 calling from the wrong folder — pass a spec_id, or a cwd inside the Spec's dev \
                 workspace"
                    .to_string(),
        },
    })
}

/// The best workspace whose `root_path` matches a normalized `cwd`: the workspace that IS the
/// cwd or an ancestor of it, the DEEPEST `root_path` winning. The single cwd→workspace selector
/// shared by the three cwd resolvers ([`resolve_spec_id`], [`resolve_workspace_for_start`],
/// [`resolve_project`]) — each then keeps the workspace itself, its Spec, or its project.
///
/// THE MATCHING AND THE TIE-BREAK ARE NOT RE-DERIVED HERE: this row-level wrapper delegates to
/// [`crate::resolve::resolve_workspace`], the repo's canonical cwd→workspace selector (also driving
/// terminal auto-attach), which matches through [`crate::pathnorm::is_ancestor_or_equal`] and ranks
/// on [`crate::pathnorm::depth`]. `resolve.rs` is the SOURCE of the convention — "deepest ancestor
/// wins, ties broken on the LOWEST id" is stated and tested there; this function only maps the
/// winning id back to its `db::Workspace` row. Two selectors that answer the same question must not
/// drift apart: this one used to compare with a hard-coded `/` while the canonical form uses `\` on
/// Windows, so NO nested cwd resolved there at all — only an exact `cwd == root_path` hit.
///
/// WHY LOWEST-ID (the convention, restated once because this path is the one that anchors Specs):
/// two matching workspaces can only tie on depth by having the very same `root_path` (both are
/// ancestors of one cwd, so equal depth ⇒ equal string), and that is reachable — uniqueness is
/// `UNIQUE(project_id, root_path)`, so two PROJECTS may register one folder. Ids are UUIDv7, hence
/// time-ordered, so lowest-id is the INCUMBENT and the deliberate OPPOSITE of newest-wins: the
/// ranking used to be `max_by` on path length, which returns the LAST maximum of a list ordered
/// `(root_path, id)` and therefore handed the folder to whichever workspace was registered most
/// recently — silently, and differently after every new registration. Lowest-id is STABLE:
/// registering a second workspace on a folder never re-points the cwd of the first, and the answer
/// for a given database never depends on when it is asked.
///
/// LIVENESS IS NOT PART OF THE RANKING — said here rather than left to be discovered. The candidate
/// set is [`db::all_workspaces`], which filters NOTHING: an ARCHIVED workspace (`archived_at`), or
/// one whose worktree folder was cleaned up at merge (`folder_removed_at` — the row is PERMANENT by
/// PDR-0008), still competes. So on a shared `root_path` the incumbent wins even when it is archived
/// and its folder is gone, ahead of a live registration. That is a deliberate LIMIT, not an
/// oversight: [`crate::resolve::resolve_workspace`] ranks the very same unfiltered set for terminal
/// auto-attach, and one cwd must not resolve to one workspace for a terminal and another for a Spec.
/// Filtering the dead rows is therefore a change to make in BOTH selectors at once, never here
/// alone. Pinned by `an_archived_workspace_still_wins_the_cwd_because_liveness_is_not_ranked`.
fn best_workspace_for_cwd(
    conn: &mut diesel::SqliteConnection,
    normalized: &str,
) -> Result<Option<db::Workspace>, diesel::result::Error> {
    let workspaces = db::all_workspaces(conn)?;
    let candidates: Vec<crate::resolve::WorkspaceMatch> = workspaces
        .iter()
        .map(|w| crate::resolve::WorkspaceMatch {
            id: w.id.clone(),
            path: w.root_path.clone(),
        })
        .collect();
    let Some(winner) = crate::resolve::resolve_workspace(normalized, &candidates) else {
        return Ok(None);
    };
    Ok(workspaces.into_iter().find(|w| w.id == winner))
}

/// Resolve the dev WORKSPACE to ANCHOR a Spec on at the `start` transition (review finding:
/// the anchor is posed at ready→in_progress, never at authoring): an explicit `workspace_id`,
/// else the workspace whose `root_path` matches `cwd` (best/longest match — via the shared
/// [`best_workspace_for_cwd`] selector [`resolve_spec_id`] and [`resolve_project`] also use, so
/// the cwd→workspace selection lives in a single place). `Ok(None)` when neither is given or
/// resolves; the transition then poses no anchor. A pure READ — the caller
/// ([`crate::mcp_tools_spec::spec_transition`]) does the atomic write.
///
/// HORS PERIMETRE ASSUME (2026-08-19) of PRD 01KYA475AX5JBGSS8WK5J8NM1S, on the user's decision —
/// the FOURTH ANCHORING SURFACE, closed here. Review finding `01M080S6Y5CTWK53FYY9YJF1GY` had it
/// out of scope: F2 bounded the empty-string hardening to THREE tools
/// (`spec_set_workspace.workspace_id`, `workspace_add.spec_id`, `create_workspace.spec_id`) and
/// recorded this one as an observation to arbitrate later. The arbitration came back "fix it", and
/// it is the same defect on a WORSE tick: `""` here read as `None` fell through to the `cwd`, and
/// then to no anchor at all, so a `spec_transition(start, workspace_id: "")` moved the Spec
/// `ready → in_progress` UNANCHORED and said nothing — a Spec silently engaged in a build with no
/// dev workspace, which the F2 cases at least left in a state nobody had committed.
///
/// It is [`anchor_str`] and not a local check, because that is where the other three read their
/// anchor from, and a fourth spelling of the same rule is how the rule drifts. `optional_str`
/// itself stays UNTOUCHED (122 call sites — its own subject, exactly as F2 said).
///
/// IT COVERS BOTH DOORS TO THIS ANCHOR, and deliberately so: `run_start` reaches this same resolver
/// ([`crate::mcp_engine::run_start`], which poses the run's workspace and through it the Spec's at
/// the graph's entry `start`). The argument, the anchor and the silence are the same on both roads,
/// so hardening the resolver rather than one caller is what keeps them one rule. What is NOT changed
/// is the meaning of ABSENCE: no `workspace_id` still falls through to `cwd`, and a `cwd` that
/// resolves nothing still starts the Spec unanchored — only the empty STRING is refused.
pub(crate) fn resolve_workspace_for_start(
    db: &Db,
    args: &Value,
) -> Result<Option<String>, RpcError> {
    if let Some(workspace_id) = anchor_str(
        args,
        "workspace_id",
        "fall through to `cwd` — and, when that resolves nothing either, START THE SPEC WITH NO DEV \
         WORKSPACE ANCHORED",
    )? {
        return Ok(Some(workspace_id.to_string()));
    }
    if let Some(cwd) = optional_str(args, "cwd")? {
        let normalized = crate::pathnorm::normalize(cwd);
        let ws = db
            .with_conn(|c| best_workspace_for_cwd(c, &normalized))
            .map_err(internal_db)?;
        return Ok(ws.map(|w| w.id));
    }
    Ok(None)
}

/// RESOLVE the PROJECT for a `role=spec` authoring render when no Spec is anchored — a Spec is
/// a PROJECT artifact the author CREATES, so the project (not a Spec) is the scope. Same arg
/// order as [`resolve_spec_id`], but each anchor resolves the PROJECT instead of a Spec:
///   1. an explicit `project_id`;
///   2. the `workspace_id`'s project (`workspace.project_id`);
///   3. the project of the workspace whose `root_path` matches `cwd` (best/longest match).
///
/// Returns `Ok(None)` when none resolves — the caller renders a graceful "select a project"
/// authoring manual, NEVER an error (that is the whole point of the /nspec project-scoping).
fn resolve_project(db: &Db, args: &Value) -> Result<Option<db::Project>, RpcError> {
    if let Some(project_id) = optional_str(args, "project_id")? {
        let project = db
            .with_conn(|c| db::get_project(c, project_id))
            .map_err(internal_db)?;
        return Ok(project);
    }
    if let Some(run_id) = optional_str(args, "run_id")? {
        // Same anchor as the Spec resolution: the run's Spec names its project, and an UNBOUND run
        // (a /nspec that has not created one yet — the authoring render's whole case) still names
        // the workspace it opened against (G2).
        let run = require_run(db, run_id)?;
        let project = db
            .with_conn(|c| -> Result<Option<db::Project>, diesel::result::Error> {
                if let Some(spec_id) = run.spec_id.as_deref() {
                    if let Some(spec) = db::get_spec(c, spec_id)? {
                        return db::get_project(c, &spec.project_id);
                    }
                }
                match run.workspace_id.as_deref() {
                    Some(ws) => match db::get_workspace(c, ws)? {
                        Some(ws) => db::get_project(c, &ws.project_id),
                        None => Ok(None),
                    },
                    None => Ok(None),
                }
            })
            .map_err(internal_db)?;
        return Ok(project);
    }
    if let Some(workspace_id) = optional_str(args, "workspace_id")? {
        let project = db
            .with_conn(|c| -> Result<Option<db::Project>, diesel::result::Error> {
                match db::get_workspace(c, workspace_id)? {
                    Some(ws) => db::get_project(c, &ws.project_id),
                    None => Ok(None),
                }
            })
            .map_err(internal_db)?;
        return Ok(project);
    }
    if let Some(cwd) = optional_str(args, "cwd")? {
        let normalized = crate::pathnorm::normalize(cwd);
        let project = db
            .with_conn(|c| -> Result<Option<db::Project>, diesel::result::Error> {
                // Same longest-match resolution as the cwd → Spec path, but keep the
                // workspace's PROJECT (a project can be authored in even with zero Specs).
                match best_workspace_for_cwd(c, &normalized)? {
                    Some(ws) => db::get_project(c, &ws.project_id),
                    None => Ok(None),
                }
            })
            .map_err(internal_db)?;
        return Ok(project);
    }
    Ok(None)
}

/// The PROJECT alone a `run_start` resolves from its anchors (`project_id` | `workspace_id` | `cwd`),
/// with no Spec involved — what an authoring flow still needs to pick its active workflow when the
/// location anchors several Specs and [`resolve_run_target`] therefore resolves none (G2).
pub(crate) fn resolve_run_project(db: &Db, args: &Value) -> Result<Option<String>, RpcError> {
    Ok(resolve_project(db, args)?.map(|p| p.id))
}

/// RESOLVE the `(project_id, spec_id)` a `run_start` binds a run to, from the SAME anchors a
/// context render uses (`spec_id` | `workspace_id` | `cwd`). The Spec is OPTIONAL — an unbound run
/// simply has no lifecycle to transition — while the project drives [`db::resolve_active_workflow`]
/// (which workflow /ndev runs). When only a `spec_id` is given, the project falls back to that
/// Spec's own project, so a run started by id still resolves the project's workflow (v2.8 review B2).
pub(crate) fn resolve_run_target(
    db: &Db,
    args: &Value,
) -> Result<(Option<String>, Option<String>), RpcError> {
    let spec_id = resolve_spec_id(db, args, ProjectRelative::RefusedByTheCaller)?;
    // The project resolved from the RUN's OWN anchors (project_id | workspace_id | cwd), independent
    // of the Spec.
    let anchor_project = resolve_project(db, args)?.map(|p| p.id);
    // The Spec's own project (a fact of the Spec).
    let spec_project = match spec_id.as_deref() {
        Some(sid) => db
            .with_conn(|c| db::get_spec(c, sid))
            .map_err(internal_db)?
            .map(|s| s.project_id),
        None => None,
    };
    // CONFRONT a disagreement rather than silently picking one (v2.8 review N14(a), the same guard
    // agent_context already applies): a spec_id whose project ≠ the project the cwd / workspace_id /
    // project_id resolves is refused, never quietly overridden either way.
    if let (Some(anchor), Some(owner)) = (&anchor_project, &spec_project) {
        if anchor != owner {
            return Err(RpcError::new(
                RpcCode::InvalidArgument,
                format!(
                    "spec {} belongs to project {owner}, but the cwd / workspace_id / project_id you \
                     passed resolves project {anchor} — they disagree. Drop the mismatched anchor, or \
                     pass a spec_id of project {anchor}",
                    spec_id.as_deref().unwrap_or_default()
                ),
            ));
        }
    }
    Ok((anchor_project.or(spec_project), spec_id))
}

/// `agent_context` — `{ role, spec_id? | run_id? | workspace_id? | cwd?, scope?, subject? }` →
/// `{ role, spec_id, context }`. Render a role manual LIVE from nyx state. A pure READ (no `changed`
/// effect).
///
/// THIS IS THE DELIVERY MECHANISM. A `spawn_subagent` action ships `agent_id` + `label` + `mode`
/// (+ `subject`) and NO manual: the sub-agent it launches calls THIS, with the `run_id` it was
/// handed, and pulls its own role + state. `subject` mirrors the spawn node's own subject, so a pass
/// spawned on ONE Spec section reads THAT section (see [`spec_section_focus`]).
pub fn agent_context(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let role_name = require_str(args, "role")?;
    let role = AgentRole::parse(role_name).ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!(
                "unknown role '{role_name}' (accepted: spec, dev, coder, auditor, doc, prototype)"
            ),
        )
    })?;
    // The dispatcher's run SCOPE (ndev | nspec | ndoc) — substituted into the `{{scope}}` marker of
    // the dispatcher manual so its `run_start` opens the RIGHT flow. Defaults to ndev; absent from
    // (and a no-op for) the effector manuals (v2.8 review N1).
    let scope = optional_str(args, "scope")?.unwrap_or("ndev").to_string();
    // The spawn's per-node FOCUS, when the caller is re-fetching an effector manual it was handed
    // narrowed (F6). Absent = the whole Spec, as before.
    let subject = optional_str(args, "subject")?.map(str::to_string);
    match resolve_spec_id(db, args, ProjectRelative::RefusedHere)? {
        // An anchored Spec resolves (a dev in progress, or an explicit spec_id): render its
        // LIVE manual — UNCHANGED for every role, including a spec author refining an existing
        // Spec.
        Some(spec_id) => {
            // A `project_id` passed BESIDE a resolved Spec is CONFRONTED, never silently dropped
            // (finding `01KZ3JQYRN95EETZ0JBQXED13T`). A Spec carries its own project; the render
            // reads THAT one, so an argument naming another project decided nothing and used to
            // leave a byte-identical manual behind — the caller then believed it had scoped the
            // render. Naming the disagreement is the only honest answer; agreeing is a no-op.
            let spec_project = db
                .with_conn(|c| db::get_spec(c, &spec_id))
                .map_err(internal_db)?
                .map(|s| s.project_id);
            if let (Some(asked), Some(owner)) = (optional_str(args, "project_id")?, &spec_project) {
                if asked != owner {
                    return Err(RpcError::new(
                        RpcCode::InvalidArgument,
                        format!(
                            "spec {spec_id} belongs to project {owner}, not to the project_id \
                             {asked} you passed — a Spec's project is a fact of the Spec, and this \
                             manual is always rendered for the OWNER. Drop the project_id (the \
                             header of the answer carries the right one), or ask for a Spec of \
                             project {asked}"
                        ),
                    ));
                }
            }
            let context = render_agent_context(db, role, &spec_id, subject.as_deref())?
                .ok_or_else(|| RpcError::new(RpcCode::InvalidId, format!("unknown spec {spec_id}")))?
                .replace("{{scope}}", &scope);
            Ok(ToolOutcome::read(json!({
                "role": role_name,
                "spec_id": spec_id,
                // The Spec's OWN project — echoed beside the manual, so a caller reads the id it
                // must pass to every design_* tool without parsing the header text.
                "project_id": spec_project,
                "context": context,
                // ALWAYS present, empty here: a caller reads `warnings` unconditionally instead of
                // branching on whether the key exists (same contract as `run_start`'s payload).
                "warnings": [],
            })))
        }
        // No Spec resolves. For the SPEC AUTHOR (/nspec) this is the NORMAL entry point — a Spec
        // is a PROJECT artifact the author CREATES, not a pre-existing workspace anchor (a
        // workspace is the DEV link). So resolve the PROJECT (explicit project_id, or
        // cwd/workspace_id → project) and render the project-scoped AUTHORING manual (grill →
        // write → decompose → spec_create), listing the project's existing Specs for
        // anti-duplication — NEVER an error, and NEVER "anchored on a workspace", even with zero
        // Specs or an unresolved project (a graceful "select a project" manual then). Every
        // OTHER role keeps the invalid_id contract unchanged.
        None => {
            // THE CASE 0 SPEAKS (review F1). It used to be the one branch of the 0/1/N contract that
            // said nothing: 1 resolves, N refuses by name (`spec_ambiguous_for_workspace`), and 0
            // handed back ~6 600 characters of perfectly normal-looking manual with `spec_id: null`
            // and not one line about it — so an agent calling from the WRONG FOLDER could not tell
            // its context apart from a scoped one. The two roles below are not refused (that is the
            // arbitration: refusing role=dev/spec would break /nspec, whose whole point is that no
            // Spec exists yet), they are made to SAY it — the same sentence, on both channels.
            //
            // DIAGNOSED INSIDE THE TWO BRANCHES THAT SPEAK IT, never before the role is read: the
            // diagnosis is a second pass over `db::all_workspaces` (the cwd anchor re-runs
            // [`best_workspace_for_cwd`], which loads and clones every workspace row), and the four
            // Spec-scoped roles below throw it away to return `invalid_id`. A signal nobody reads is
            // a table scan nobody asked for.
            if role == AgentRole::Dev {
                let signal = unanchored_signal(db, args)?;
                // The DISPATCHER owns no flow and binds no Spec itself — `run_start` resolves (or,
                // for a fresh nspec, CREATES) the run. So it renders STANDALONE, with NO Spec
                // required: this is what lets /nspec (which starts a brand-new Spec — none yet) and
                // /ndoc drive the engine the SAME way /ndev does (v2.8 review N1). The `{{scope}}`
                // marker carries the invoked flow into its `run_start` call.
                let context = format!(
                    "{SOURCES_NOTE}{}{}",
                    signal.notice(),
                    inject_deferred_rules(AgentRole::Dev.preamble(), AgentRole::Dev)
                )
                .replace("{{scope}}", &scope);
                Ok(ToolOutcome::read(json!({
                    "role": role_name,
                    "spec_id": Value::Null,
                    "context": context,
                    "warnings": [signal.warning()],
                })))
            } else if role == AgentRole::Spec {
                let signal = unanchored_signal(db, args)?;
                let project = resolve_project(db, args)?;
                let context = render_spec_authoring(db, project.as_ref(), &signal.notice())?;
                Ok(ToolOutcome::read(json!({
                    "role": role_name,
                    "spec_id": Value::Null,
                    "project_id": project.as_ref().map(|p| p.id.as_str()),
                    "context": context,
                    "warnings": [signal.warning()],
                })))
            } else {
                Err(RpcError::new(
                    RpcCode::InvalidId,
                    "could not resolve a Spec: pass spec_id, or a workspace_id/cwd anchored on one",
                ))
            }
        }
    }
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

// ---------------------------------------------------------------------------
// The WORKSPACE COMMANDS block of the AMBIENT session context.
// ---------------------------------------------------------------------------
//
// It renders HERE, next to the role manuals, and not in `mcp_tools_core` — this is the module that
// renders agent-facing TEXT, and the one place the wording of what an agent reads is reviewable in
// one pass. `mcp_tools_core::session_context_line` calls it with the workspace's instances and an
// overlay closure over its runner.

/// How many command lines the ambient block renders before it stops counting them one by one. Past
/// it the block says HOW MANY are left and names the tool that lists them all — the point of the
/// block is orientation, not an exhaustive dump into every session's first tokens.
const MAX_COMMAND_LINES: usize = 12;

/// The widest a rendered command line is, ellipsis INCLUDED. A managed command can be a whole shell
/// pipeline; the agent needs to RECOGNIZE it (and it has `list_commands` for the exact text), not to
/// re-type it from the block.
const MAX_COMMAND_CHARS: usize = 160;

/// WHAT TO DO WITH THE LIST — the two directives that hold for every workspace that has one.
///
/// The FIRST is the whole point of the block: these commands exist, and a run started through nyx is
/// supervised and visible to the user AND to the agent, where the same run in a raw shell is an
/// invisible process nobody can read back. The SECOND is what stops the duplicate launch: an agent
/// that cannot tell whether something is up relaunches it "just in case", and `start_command` is
/// already a no-op on a running command — so the block states the ack it gets back rather than
/// leaving it to be discovered.
///
/// ADVISORY, not a lock (PDR-0011): nothing here prevents a shell, and nyx does not try to.
const DIRECTIVE_LINES: &[&str] = &[
    "Prefer start_command over launching these in a raw shell: nyx then supervises the run and \
     captures its output, and the user sees it as you do.",
    "start_command is a NO-OP on a command already running (it answers was_running:true, \
     restarted:false) — never relaunch one \"just in case\"; relaunch_command is the real restart.",
];

/// The GROUP directive, rendered ONLY when a rendered line actually carries a `group_id` — a
/// workspace with no multi-pane group would be told about a call it cannot make.
const GROUP_DIRECTIVE_LINE: &str =
    "A command carrying a group_id belongs to a multi-pane group: start_command {group_id, \
     workspace_id} starts every member that is not already up, in ONE call — not one call per \
     instance.";

/// The FRESHNESS directive. The FACT is load-bearing: the plugin's `hooks.json` matches
/// `startup|resume|clear|compact`, so this block IS re-taken after a `/clear` and after a
/// `/compact` — but never between two, which is the failure mode an agent must plan for. Saying "at
/// session open" alone would be false on both counts.
///
/// AND SO IS THE SHAPE OF THE SENTENCE (finding `01M1A4J8W87R23ZRJ7QVVS6B3S`). Naming `/clear` and
/// `/compact` is NOT enough. The first wording opened on "re-taken every time a session opens" and
/// carried them in an em-dash aside; a real session, asked what it had in context, read the whole
/// thing back as "a snapshot taken at session open". An aside is the first thing a model drops when
/// it compresses, and the tests — which assert the substrings are PRESENT — could not catch it: the
/// letter held, the intent did not.
///
/// So the RULE for anyone rewriting this line: the carrying fact never sits in an aside, `/clear`
/// and `/compact` hold the main clause, and nothing frames the snapshot as taken "at session open"
/// (the block is being read at one — that half needs no words). A reader who keeps ONE thing out of
/// this sentence must keep that those two commands are what re-takes it.
const SNAPSHOT_DIRECTIVE_LINE: &str =
    "This list is a SNAPSHOT. A /clear or a /compact re-takes it; nothing in between does, so it \
     goes stale as commands start and stop. For the current state call list_commands, for what a \
     command printed call get_command_output.";

/// The command text as the block shows it: newlines flattened (one command, one line) and the whole
/// thing capped at [`MAX_COMMAND_CHARS`] — the ellipsis counts INSIDE the cap, so a rendered command
/// is never longer than the bound. Counts CHARS, not bytes, so a truncation never splits a
/// multi-byte character.
fn render_command_text(command: &str) -> String {
    let flattened = command.replace(['\r', '\n'], " ");
    if flattened.chars().count() <= MAX_COMMAND_CHARS {
        return flattened;
    }
    let mut out: String = flattened.chars().take(MAX_COMMAND_CHARS - 1).collect();
    out.push('…');
    out
}

/// Render the workspace's MANAGED COMMANDS as the block the ambient session context carries after
/// the `You're in nyx — …` orientation line, so an agent knows what this workspace already defines
/// and WHAT IS ALREADY UP without a single discovery call.
///
/// One line per instance — `name · command · state · instance_id=…`, plus `· group_id=…` on a
/// command that belongs to a multi-pane group and NOTHING more on a solo one. `command_id` is
/// deliberately absent: no tool takes it, and it would cost an ULID per line in every session's
/// first tokens.
///
/// `instances` is rendered in the order it arrives — [`db::list_command_lines_for_workspace`]
/// already orders by the template's `order_index` then `id`, which is the order of the project's
/// sidebar, so re-sorting here would make the block disagree with what the user sees.
///
/// THE STATE IS FACTUAL, and `overlay` is why: it is the runner's live outcome for an instance id
/// (`None` when no live entry backs it), so a command running THIS session reads `running` even
/// when its persisted `last_state` is stale — the same fallback `list_commands` applies. Taking it
/// as a CLOSURE rather than a `&CommandRunner<S>` is what makes this renderer testable without
/// spawning a single process. The vocabulary is the domain's own ([`RunState::as_db_str`]):
/// `idle | running | success | error`.
///
/// The list is followed by the DIRECTIVE — [`DIRECTIVE_LINES`], the group line when a rendered
/// command carries a `group_id`, then [`SNAPSHOT_DIRECTIVE_LINE`] — because a list with no
/// instruction is what nyx already had: an agent that knows the commands exist and launches them in
/// a raw shell anyway.
///
/// An EMPTY `instances` renders an EMPTY string — a workspace with no managed command, or a loose
/// terminal, gets no block at all (and, therefore, no directive either).
///
/// No environment value can leak here: [`db::WorkspaceCommandLine`] carries none (`env` is a column
/// of `managed_commands`, which its reader does not select), and the block renders nothing it is
/// not handed.
///
/// THE ROW IS THE LEAN ONE, ON PURPOSE (review finding `01M152NMXPWKD59QFK95CMSZWQ`). This takes
/// [`db::WorkspaceCommandLine`], not [`db::InstanceWithTemplate`]: the latter carries each
/// instance's `scrollback` (up to 256 KiB apiece, so up to 3 MiB on a full workspace) which this
/// renderer never reads, and it renders on the SessionStart hook's hard 1000 ms budget — where a
/// timeout costs the whole injected context.
pub(crate) fn workspace_commands_block<F>(
    instances: &[db::WorkspaceCommandLine],
    overlay: F,
) -> String
where
    F: Fn(&str) -> Option<RunState>,
{
    if instances.is_empty() {
        return String::new();
    }
    let mut out = String::from("Managed commands of this workspace:");
    let mut rendered_a_group = false;
    for row in instances.iter().take(MAX_COMMAND_LINES) {
        let state = overlay(&row.id).unwrap_or_else(|| RunState::from_db_str(&row.last_state));
        out.push_str(&format!(
            "\n- {} · {} · {} · instance_id={}",
            row.name,
            render_command_text(&row.command),
            state.as_db_str(),
            row.id,
        ));
        if let Some(group_id) = row.group_id.as_deref().filter(|g| !g.is_empty()) {
            out.push_str(&format!(" · group_id={group_id}"));
            rendered_a_group = true;
        }
    }
    let overflow = instances.len().saturating_sub(MAX_COMMAND_LINES);
    if overflow > 0 {
        out.push_str(&format!("\n+{overflow} more commands (list_commands)"));
    }
    for line in DIRECTIVE_LINES {
        out.push('\n');
        out.push_str(line);
    }
    if rendered_a_group {
        out.push('\n');
        out.push_str(GROUP_DIRECTIVE_LINE);
    }
    out.push('\n');
    out.push_str(SNAPSHOT_DIRECTIVE_LINE);
    out
}

/// THE STALE FORMULATIONS — wordings the variance model stopped accepting, forbidden on EVERY
/// agent-facing surface, with the reason each one is dead.
///
/// WHY IT LIVES HERE, SHARED (findings `01KYSCY526C9N9B2BBH68A3N2X` then `01KYSNSBBT4N22ZTWQHFNQBDJQ`).
/// An agent meets this model through TWO surfaces: the tool DESCRIPTORS it gets from `tools/list`,
/// and the ROLE MANUAL `agent_context` renders. The first got a guard; the second did not, and it
/// still summarized `states_json` as "its declared states/variants" — one of the four strings the
/// descriptor guard forbids, in the very text an agent reads FIRST. Two guards over two surfaces
/// with two copies of the list would have drifted apart the same way; ONE list, read by both
/// (`mcp.rs::tool_descriptors_describe_the_variance_model_the_tools_actually_accept` and
/// `tests::prototype_role_teaches_the_axes_model`), means a wording chased off one surface can no
/// longer survive on the other.
#[cfg(test)]
pub(crate) const STALE_AGENT_FORMULATIONS: &[(&str, &str)] = &[
    (
        "array of names",
        "`states_json` is the AXES (named axes whose VALUES carry their classes), not a flat list \
         of state names",
    ),
    (
        "declared states",
        "the field declares AXES OF VARIANCE, not \"states\" — the wording predates the model",
    ),
    (
        "options?",
        "`options` is not one more optional field: it is MANDATORY for `select` and ignored on \
         every other type",
    ),
    ("data-depot-", "the interaction vocabulary is data-nyx-*"),
];

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
            // The plan MATERIALIZES from the task phase names (get-or-create, first-seen):
            // "Foundation" → phase 1, "UI" → phase 2. No separate set-phases step.
            let phase_task = |name: &str| db::NewTaskInput {
                spec_id: Some(spec.id.clone()),
                phase_name: Some(name.to_string()),
                ..db::NewTaskInput::slice()
            };
            db::create_task(c, &project.id, "Phase 1 task", phase_task("Foundation")).expect("t1");
            db::create_task(c, &project.id, "Phase 2 task", phase_task("UI")).expect("t2");
            (spec.id, root.id, root.root_path)
        });
        (db, spec_id, workspace_id, cwd)
    }

    /// A memory DB seeded with a project + its root workspace + a repo but NO Spec — the
    /// /nspec ENTRY state (the author is about to CREATE the first Spec of the project).
    /// Returns `(db, project_id, workspace_id, cwd)`.
    fn seeded_project_without_spec() -> (Db, String, String, String) {
        let db = Db::in_memory();
        let (project_id, workspace_id, cwd) = db.with_conn(|c| {
            let (project, root) =
                db::create_project(c, "Proj", "/tmp/nyx-nospec", None).expect("project");
            db::create_repository(c, &project.id, "api", ".", false).expect("repo");
            (project.id, root.id.clone(), root.root_path.clone())
        });
        (db, project_id, workspace_id, cwd)
    }

    #[test]
    fn render_carries_role_spec_sections_current_phase_tasks_and_workspace_repos() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Coder, &spec_id, None)
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
        // Project + its repos + the workspace (one section — the project half is unconditional,
        // finding `01KZ3JQYRN95EETZ0JBQXED13T`).
        assert!(
            text.contains("Project, repos & workspace"),
            "project/workspace section"
        );
        assert!(text.contains("api → ."), "the project's repo");
        assert!(text.contains("- workspace: "), "the anchored workspace");
    }

    /// N29 — THE HIERARCHY OF SOURCES, on every manual surface. A sub-agent inherits the project's
    /// ambient `CLAUDE.md` (the "starts blank" claim was false), and that ambient text OUTRANKS the
    /// built-in manual on the project's own ground — so no manual prescribes a commit style, a
    /// branch policy, a language or a tooling choice.
    #[test]
    fn every_manual_states_the_project_claude_md_outranks_it() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();

        let mut manuals: Vec<String> = Vec::new();
        for role in [
            AgentRole::Spec,
            AgentRole::Dev,
            AgentRole::Coder,
            AgentRole::Auditor,
            AgentRole::Doc,
            AgentRole::Prototype,
        ] {
            manuals.push(render_agent_context(&db, role, &spec_id, None).unwrap().unwrap());
            // …and the STATIC template (no run bound) says it too.
            manuals.push(render_role_template(role));
        }
        // The two run-less entry points: the /nspec authoring manual and the standalone dispatcher.
        manuals.push(render_spec_authoring(&db, None, "").unwrap());
        let dispatcher = agent_context(&db, &json!({ "role": "dev", "scope": "ndev" })).unwrap();
        manuals.push(dispatcher.result["context"].as_str().unwrap().to_string());

        for text in &manuals {
            assert!(
                text.contains("SOURCES — this manual carries your ROLE"),
                "every manual states where its instructions come from",
            );
            assert!(
                text.contains("ambient `CLAUDE.md` files")
                    && text.contains("never start context-less")
                    && text.contains("THEY take precedence over this text"),
                "the ambient CLAUDE.md is inherited AND outranks the manual",
            );
        }

        // The FALSE claim is gone from the dispatcher: a spawned sub-agent inherits the project's
        // ambient CLAUDE.md — what it does not inherit is the SESSION.
        let dev = render_agent_context(&db, AgentRole::Dev, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(
            !dev.contains("starts blank"),
            "the 'starts blank' claim must not survive — it is false",
        );
        // …and so is the FORWARDING that claim justified: the dispatcher carries no manual at all
        // now, it hands a `run_id` and the sub-agent pulls its own role + state.
        assert!(
            !dev.contains("PASTING") && !dev.contains("paste"),
            "the dispatcher no longer pastes a forwarded manual anywhere:\n{dev}",
        );
        assert!(
            dev.contains("agent_context(run_id=") && dev.contains("PULLS its own"),
            "it names the call the sub-agent makes to read its own role + state",
        );

        // No built-in manual prescribes a project convention (N29): the branch policy and the doc
        // profile's style/audience prescriptions are the project's business.
        for (role, text) in [
            (AgentRole::Coder, &manuals[4]),
            (AgentRole::Doc, &manuals[8]),
        ] {
            for gone in ["Commit on the current branch", "the profile's sources"] {
                assert!(
                    !text.contains(gone),
                    "{}: the project-convention prescription `{gone}` must not survive",
                    role.slug(),
                );
            }
        }
    }

    /// THE SPEC'S LANGUAGE IS RESOLVED, NEVER GUESSED. Spec #7 was authored in English and Spec #8
    /// in French — same project, same effector, both requested in French — because the role said
    /// nothing and each session decided for itself. The manual now carries the ORDER (the project's
    /// convention and its existing Specs first; the language the human asked in only when nothing
    /// else decides), on every surface the author can be reading it from: the run-bound render, the
    /// static template, and the run-less authoring manual.
    #[test]
    fn the_spec_author_is_told_how_to_resolve_the_specs_language() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();

        for (surface, text) in [
            (
                "the run-bound render",
                render_agent_context(&db, AgentRole::Spec, &spec_id, None)
                    .unwrap()
                    .unwrap(),
            ),
            ("the static template", render_role_template(AgentRole::Spec)),
            (
                "the authoring manual",
                render_spec_authoring(&db, None, "").unwrap(),
            ),
        ] {
            assert!(
                text.contains("choose the Spec's LANGUAGE yourself"),
                "{surface}: picking the language per session is what the rule forbids:\n{text}",
            );
            // The ORDER is the rule — a list of sources with no precedence resolves nothing.
            let project = text
                .find("what the project prescribes")
                .unwrap_or_else(|| panic!("{surface}: the project decides first:\n{text}"));
            let existing = text
                .find("its existing Specs are written in")
                .unwrap_or_else(|| panic!("{surface}: the Specs already written count too:\n{text}"));
            let human = text
                .find("the language the human asked in")
                .unwrap_or_else(|| panic!("{surface}: the fallback is the human's own:\n{text}"));
            assert!(
                project < existing && existing < human,
                "{surface}: the human's language is the FALLBACK, after what the project says:\n{text}",
            );
            assert!(
                text.contains("One language for the whole Spec"),
                "{surface}: the resolved language holds across the sections and the revisions:\n{text}",
            );
        }
    }

    #[test]
    fn render_each_role_uses_its_own_preamble() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        assert!(render_agent_context(&db, AgentRole::Dev, &spec_id, None)
            .unwrap()
            .unwrap()
            .contains("Dev — dispatcher"));
        assert!(
            render_agent_context(&db, AgentRole::Auditor, &spec_id, None)
                .unwrap()
                .unwrap()
                .contains("Auditor Agent")
        );
    }

    /// v2.8 (task 01KY71J3MVTQ1V86J92GNC9FKH, annex `context-dev-dispatcher`): the DEV context is
    /// now a THIN DISPATCHER. It calls `nyx_next` and performs EXACTLY the atomic action nyx
    /// returns — it orients NOTHING itself: it never loops / branches / transitions on its own
    /// initiative (the whole orchestration moved into the graph). This test pins both the
    /// dispatcher mandate AND the ABSENCE of the ex-orchestrator's prose.
    #[test]
    fn render_dev_is_the_thin_dispatcher_not_the_orchestrator() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Dev, &spec_id, None)
            .unwrap()
            .unwrap();

        // The dispatcher mandate: call nyx_next, do exactly the one action it returns, nothing else.
        assert!(text.contains("Dev — dispatcher"), "dispatcher header");
        assert!(
            text.contains("You do NOT orchestrate")
                && text.contains("You do NOT know the workflow — nyx does"),
            "it does not orchestrate and does not know the flow",
        );
        assert!(
            text.contains("call\n`nyx_next`") || text.contains("call `nyx_next`"),
            "its only job is to call nyx_next",
        );
        assert!(
            text.contains("EXACTLY the one atomic action it returns"),
            "it does exactly the returned atomic action",
        );
        assert!(
            text.contains("You never decide the flow"),
            "it never decides the flow",
        );
        // It NEVER loops / branches / transitions of its own initiative, and never runs gates.
        assert!(
            text.contains("orchestrate, loop, branch, or transition on your own initiative"),
            "no self-initiated loop / branch / transition",
        );
        assert!(
            text.contains("run gates or state transitions yourself — nyx does that INSIDE"),
            "gates + transitions happen inside nyx_next, not in the session",
        );

        // Sub-workflow doctrine (v2.8 lot 2): driving a `sub_workflow` action drives the CHILD run,
        // then re-ticks the PARENT with NO last_result (nyx folds the child's verified state itself).
        assert!(
            text.contains("`sub_workflow`")
                && text.contains("nyx_next(child_run_id)")
                && text.contains("re-tick the parent"),
            "the sub_workflow case: drive the child, re-tick the parent with no last_result",
        );
        // A BUBBLED ask_human (`via: \"sub_workflow\"` + child_run_id) is answered against the CHILD,
        // then the dispatcher keeps driving THAT child before it re-ticks the parent.
        assert!(
            text.contains(r#"`via: "sub_workflow"`"#)
                && text.contains("nyx_next(child_run_id, last_result={decision})")
                && text.contains("KEEP DRIVING THAT CHILD DIRECTLY"),
            "the bubbled ask_human case: answer the child run, keep driving it, then re-tick the parent",
        );

        // The ex-orchestrator's prose is GONE (it is now the behavior-tree graph, not the context):
        // no state rule, no per-phase cadence, no 2-axis auditor drive, no review loop, no backlog
        // capture in the dispatcher.
        for gone in [
            "Dev Orchestrator",
            "The state rule",
            "2-axis AUDITOR ONCE",
            "review_comment_list",
            "review_task_structure",
            "recreate a PRD",
            "idea_add(",
        ] {
            assert!(
                !text.contains(gone),
                "the ex-orchestrator prose `{gone}` must NOT survive in the thin dispatcher",
            );
        }
    }

    /// G6 — the dispatcher makes NO call before its first tick. The nyx command hook opens the run
    /// and injects its `run_id` with this very manual, so the "call `run_start` ONCE, before the
    /// loop" step is gone and the session's first act is `nyx_next(run_id)`.
    ///
    /// What replaces it is not silence: the id has to be pointed at (nothing else hands the
    /// dispatcher one), the `warnings` that rode with it have to reach the human (the agent no
    /// longer reads them off its own call — see `hook_push_injection_e2e`), and `run_start` STAYS
    /// dispatchable, so the manual says plainly why calling it is unnecessary rather than leaving
    /// an advertised tool looking like a required step.
    #[test]
    fn the_dispatcher_is_handed_its_run_and_opens_none() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Dev, &spec_id, None)
            .unwrap()
            .unwrap();

        // The run is a GIVEN, named where the injection puts it.
        assert!(
            text.contains("Your run is already open") && text.contains("`NYX RUN` block"),
            "the manual says the run_id is already in hand, and where: {text}",
        );
        assert!(
            text.contains("make NO call to obtain it"),
            "…and that no call is needed to obtain it",
        );
        // The warnings the hook injected beside it are the human's, and they get relayed.
        assert!(
            text.contains("RELAY each one") && text.contains("run_replaced_graph_changed"),
            "the injected warnings are relayed to the human before the first tick",
        );
        // `run_start` is still dispatchable (N17: an un-advertised tool is undispatchable, and the
        // hook reaches the sidecar through the same surface), so the manual states what a second
        // call does — resume — instead of implying a required step.
        assert!(
            text.contains("IDEMPOTENT") && text.contains("RESUMES that very run"),
            "a stray run_start resumes the same run, and the manual says so",
        );
        // The pre-tick step itself is GONE.
        for gone in [
            "call `run_start` EXACTLY ONCE",
            "Before the loop",
            "First: open your run",
            "it returns the `run_id` you pass to every `nyx_next`",
        ] {
            assert!(
                !text.contains(gone),
                "the pre-tick `run_start` step `{gone}` must NOT survive: the hook opened the run",
            );
        }
    }

    /// THE DELIVERY MECHANISM, stated where the spawn happens. The action carries identifiers, not a
    /// manual, so the dispatcher must know the two halves of the pull — or the sub-agent is spawned
    /// with no role at all. NEITHER MODE MAY BE MUTE: a `native_task` sub-agent is TOLD to call
    /// `agent_context` first, and an `inline` step — where the dispatcher IS the effector — makes
    /// that call ITSELF.
    #[test]
    fn the_dispatcher_hands_the_run_id_and_lets_the_sub_agent_pull_its_own_context() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Dev, &spec_id, None)
            .unwrap()
            .unwrap();

        // What the action carries: WHO, WHAT FOR, on WHICH object — and no manual.
        assert!(
            text.contains("`agent_id`") && text.contains("`label`") && text.contains("`subject`"),
            "the spawn action names the agent, the step and the object",
        );
        assert!(
            text.contains("you do NOT carry a role manual"),
            "and states, in as many words, that no manual travels with it",
        );

        // The NATIVE branch: the sub-agent is told to make the call, with the run_id it was handed.
        assert!(
            text.contains(r#"agent_context(run_id="<run_id>","#)
                && text.contains("PULLS its own"),
            "the native_task branch hands the run_id and names the call to make FIRST",
        );

        // The INLINE branch: the dispatcher IS the effector, so IT calls agent_context.
        let inline = text
            .split("mode `inline`")
            .nth(1)
            .expect("the manual has an inline branch");
        assert!(
            inline.contains("YOURSELF"),
            "in inline mode the dispatcher makes the agent_context call itself: {inline}",
        );

        // The F6 reuse plumbing is GONE — it existed only to avoid re-sending a manual that no
        // longer travels.
        for gone in ["context_already_sent", "context_recall", "PASTE THE ONE YOU KEPT"] {
            assert!(
                !text.contains(gone),
                "the forwarding-era rule `{gone}` must not survive",
            );
        }
    }

    /// N1 — the DISPATCHER renders STANDALONE (no Spec required), with the invoked SCOPE substituted
    /// into the manual — so /nspec (which starts a FRESH Spec — none yet) and /ndoc drive the engine
    /// the same way /ndev does, instead of erroring "could not resolve a Spec".
    #[test]
    fn dispatcher_renders_standalone_with_the_invoked_scope() {
        let _g = db_guard();
        let (db, _spec_id, _ws, _cwd) = seeded();
        // role=dev + a scope + NO spec/cwd → the standalone dispatcher (no Spec resolves).
        let out = agent_context(&db, &json!({ "role": "dev", "scope": "nspec" }))
            .expect("agent_context ok")
            .result;
        let ctx = out["context"].as_str().expect("a rendered context");
        assert!(
            ctx.contains("Dev — dispatcher"),
            "the dispatcher manual renders with no Spec bound"
        );
        assert!(
            ctx.contains(r#"scope: "nspec""#),
            "the invoked scope is substituted into run_start: {ctx}"
        );
        assert!(!ctx.contains("{{scope}}"), "the scope marker is resolved");
        assert_eq!(out["spec_id"], serde_json::Value::Null);
    }

    /// THE STUB AND THE MANUAL SAY THE SAME THING ABOUT THE NAMED SPEC — the divergence that cost a
    /// session. `nspec.md` advertised "an optional Spec id selects the Spec to work on" three lines
    /// under a fallback `run_start` that passed no `spec_id` at all, so an agent handed `/nspec 3`
    /// saw the `3`, found no instruction for it, opened an unanchored run and let the flow order a
    /// DUPLICATE of the Spec the human had named. Two texts an agent reads in the same breath —
    /// the command stub Claude Code expands, and the dispatcher manual `agent_context` renders —
    /// must therefore each carry the WHOLE instruction about the named Spec, and this guard holds
    /// every one of them to that.
    ///
    /// THE CLAUSES ARE THE TABLE THE LOOP READS, held on EVERY text at once — the manual and every
    /// stub `USER_COMMAND_STUBS` carries, however many that is — each the half of an instruction
    /// that is inert without the others: the accepted FORMS (from
    /// [`crate::spec_ref::ACCEPTED_FORMS`], so the advertisement and the parser stay one fact), the
    /// ARGUMENT the reference rides in, that it rides VERBATIM (nyx translates, the agent does not),
    /// what to do when nyx refuses it — ASK — and that asking is never to be papered over with a
    /// Spec of one's own. Their NUMBER is the table's and is not restated here: a count in a comment
    /// is a second statement of the list, and it fell behind the first clause added (finding
    /// `01M0ZW8MT6C62JGD5Q0QN2J41Y`).
    ///
    /// AN OMISSION IS WHAT GOES RED, NOT A CONTRADICTION (finding `01M1103CJXFN794TJ9BK1NCV5Q`).
    /// Each clause is looked for by PRESENCE, so a text that drops one fails — and that IS the
    /// shape the incident took: the stub went SILENT on the reference its own hint advertised.
    /// This note used to say the two texts "cannot disagree on this point"; they can. A text that
    /// carries every clause and then says the opposite of one, two lines below, passes here, and
    /// no presence check can do better — judging prose the table does not name would take a second
    /// table, of forbidden phrasings, hand-kept and stale on the first wording nobody foresaw. The
    /// one divergence held as an EQUIVALENCE, both halves or neither and either alone red, is the
    /// hint against the body, next door in
    /// [`tests::the_command_stubs_announce_the_named_spec_in_their_argument_hint`](mod@tests).
    #[test]
    fn the_command_stubs_and_the_dispatcher_manual_each_carry_the_named_spec_instruction() {
        let _g = db_guard();
        let (db, _spec_id, _ws, _cwd) = seeded();
        let manual = agent_context(&db, &json!({ "role": "dev", "scope": "nspec" }))
            .expect("agent_context ok")
            .result["context"]
            .as_str()
            .expect("a rendered context")
            .to_string();
        // Both sides are HARD-WRAPPED prose, so a clause routinely straddles a line break: the
        // comparison is on whitespace-collapsed text, which is the same sentence as read.
        let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut texts: Vec<(&str, &str)> = vec![("the dispatcher manual", manual.as_str())];
        texts.extend(
            crate::plugin::USER_COMMAND_STUBS
                .iter()
                .map(|(name, body)| (*name, *body)),
        );
        for (name, text) in texts {
            let text = flat(text);
            for clause in [
                crate::spec_ref::ACCEPTED_FORMS,
                "spec_id",
                "VERBATIM",
                "STOP and ASK the human which Spec they meant",
                "Never substitute a Spec of your own choosing, and never author a new one to get \
                 past a refused reference",
            ] {
                assert!(
                    text.contains(&flat(clause)),
                    "{name} must carry `{clause}` — the stub and the manual are read together, and \
                     a clause only one of them carries is the silence that let a typed reference \
                     go unused",
                );
            }
        }
    }

    /// …AND THE HINT AGREES WITH THE BODY. The contradiction this PRD closes was between the two
    /// halves of ONE stub: `argument-hint: "[spec-id] <free instruction>"` announced a Spec
    /// reference in first position while the procedure below it did nothing with one, so `/nspec 3`
    /// reached an agent that saw the `3`, found no instruction for it, and opened an unanchored run.
    /// The guard above closes that in one direction (the body now names the grammar); this one
    /// closes it in the other — a hint that stopped announcing the reference while the body still
    /// instructed to forward one would recreate the same disagreement, wearing the other hat, with
    /// nothing turning red (PRD testing decision, finding `01M0ZWAH9TMFHX534TXMAPZRK3`).
    ///
    /// THE TWO SIDES ARE READ, NEVER RESTATED: the hint from the stub's own frontmatter, the body's
    /// promise from [`crate::spec_ref::ACCEPTED_FORMS`] — the same constant the parser is written
    /// beside. So the assertion is an EQUIVALENCE, and either half going missing alone is what
    /// fails.
    #[test]
    fn the_command_stubs_announce_the_named_spec_in_their_argument_hint() {
        // The token the hint announces the reference with. It is what Claude Code shows the human
        // as they type the command, which is why it must lead: the reference is the FIRST argument
        // or it is not one at all — `specIdFromPrompt` reads no other position.
        const SPEC_REF_HINT: &str = "[spec-id]";
        let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        for (name, body) in crate::plugin::USER_COMMAND_STUBS {
            let hint = frontmatter_value(body, "argument-hint")
                .unwrap_or_else(|| panic!("{name} declares an argument-hint in its frontmatter"));
            let hint = hint.trim().trim_matches('"');
            let body_names_the_grammar =
                flat(body).contains(&flat(crate::spec_ref::ACCEPTED_FORMS));
            assert_eq!(
                hint.contains(SPEC_REF_HINT),
                body_names_the_grammar,
                "{name}: the hint says `{hint}` while its body {} the accepted forms — the hint and \
                 the procedure are read in the same breath, and this is the exact disagreement that \
                 let a typed reference go unused",
                if body_names_the_grammar {
                    "DOES name"
                } else {
                    "does NOT name"
                }
            );
            assert!(
                body_names_the_grammar,
                "{name} must still instruct on the reference — this guard holds the two halves \
                 together, it is not a licence to drop both"
            );
            assert!(
                hint.starts_with(SPEC_REF_HINT),
                "{name}: the reference is the FIRST argument or none — the recogniser reads no \
                 other position, but the hint says `{hint}`"
            );
        }
    }

    /// One frontmatter scalar of a command stub, read from the `---`-delimited block the file opens
    /// with. Deliberately minimal, and the shape is what licenses it: the stubs open on a `---`
    /// block of FLAT `key: value` scalars — no nesting, no lists, no block strings — so a YAML
    /// dependency to read them would be a second parser to keep true. Nothing here counts those
    /// lines: a number in this sentence would be one nothing checks, and a stub gaining a key is a
    /// change this parser absorbs without a word (finding `01M1005MPE7GAD9FN1CE0AS2X7`).
    fn frontmatter_value(body: &str, key: &str) -> Option<String> {
        let mut lines = body.lines();
        if lines.next()?.trim() != "---" {
            return None;
        }
        for line in lines {
            if line.trim() == "---" {
                return None;
            }
            if let Some(value) = line.trim().strip_prefix(&format!("{key}:")) {
                return Some(value.trim().to_string());
            }
        }
        None
    }

    /// The dispatcher's `{{rules}}` marker resolves to the ALWAYS scope ONLY — no `role:dev` (it
    /// does not code) and no `fork` (built-in, not forked) — per the annex `context-dev-dispatcher`.
    #[test]
    fn render_dev_dispatcher_rules_resolve_to_always_only() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        // A populated set carrying every scope + a fork lineage — the dispatcher must still resolve
        // to ALWAYS only (it ignores role:dev + fork), proving the thin resolution.
        let rules = RuleSet {
            rules: vec![
                Rule::curated(RuleScope::Always, "pnpm, never npm"),
                Rule::curated(RuleScope::Role(AgentRole::Dev), "should be ignored for dev"),
                Rule {
                    scope: RuleScope::Fork,
                    text: "should be ignored for a built-in dispatcher".into(),
                    curated: false,
                },
            ],
            lineage: Some(Lineage {
                forked_from: "dev".into(),
            }),
        };
        let text = render_agent_context_with_rules(&db, AgentRole::Dev, &spec_id, &rules, None)
            .unwrap()
            .unwrap();
        // The marker itself is consumed (resolved), and the block names the ALWAYS-only resolution.
        assert!(
            !text.contains(RULES_MARKER),
            "the {{rules}} marker is resolved"
        );
        assert!(
            text.contains("Resolved at the rules marker by scope (always)"),
            "dispatcher resolves the always scope only",
        );
        assert!(
            text.contains("pnpm, never npm"),
            "the always rule is injected"
        );
        assert!(
            !text.contains("should be ignored"),
            "role:dev + fork rules are NOT injected into the dispatcher",
        );
    }

    /// The coder manual is PROHIBITIONS and nothing else. The seeded `coder` sub-workflow owns the
    /// task lifecycle (`transition_task` start/done) and the validation (`gate_check`, run by nyx
    /// over the PTY), and the node's label says what to do — so what is left is the set of things
    /// NO graph structure can state: the transitions that are not the coder's, the gate that is not
    /// its verdict, the `kind=human` task it must refuse, the `block`/`skip` no node covers, and
    /// the follow-up it may not capture itself.
    #[test]
    fn render_coder_carries_only_the_prohibitions_the_graph_cannot_express() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Coder, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(text.contains("design-system"), "compose from design-system");
        assert!(
            text.contains("do a `kind=human` task"),
            "kind=human is reserved for the user",
        );
        // The two transitions NO node covers are the only ones left.
        assert!(
            text.contains("task_transition block / skip"),
            "block/skip stay with the coder (no node covers them)",
        );
        assert!(
            text.contains("move a task to started / done — nyx does that around your spawn"),
            "start/done are the graph's transition_task nodes, not the coder's",
        );
        assert!(
            text.contains("run the acceptance gate, or rule on its verdict"),
            "the gate is nyx's (gate_check), not the coder's",
        );
        // The hand-back contract replaces the self-declared "done".
        assert!(
            text.contains("a REPORT") && text.contains("never a state change"),
            "the coder reports; it does not move the state",
        );
        // The state-driving prose is GONE.
        for gone in [
            "task_transition start/done",
            "the full suite\n  once at the end",
            "Task done -> task_transition done",
            "continue with the other independent tasks",
        ] {
            assert!(
                !text.contains(gone),
                "the state-driving instruction `{gone}` must not survive in the coder manual",
            );
        }
        // …and so is the PROCESS the graph executes: the node label says what to do at this step.
        for gone in ["## How you work", "TDD at the pre-agreed seams", "Typecheck + targeted"] {
            assert!(
                !text.contains(gone),
                "the narrated process `{gone}` belongs to the graph, not to the manual",
            );
        }
    }

    /// The unobserved-launch prohibition NAMES the tools (task 01M14TJ97MW647Q4PV6V5N80D7). The
    /// generic rule was already there and was not enough: an agent told not to launch outside nyx,
    /// without the two tool names and without the no-op-if-running semantics, still reaches for a raw
    /// shell — it has no reason to believe nyx has an answer.
    ///
    /// The preamble stays STATIC: it renders NO command list. It has neither a workspace nor a runner
    /// in hand; the LIVE list is the ambient session context's job
    /// ([`workspace_commands_block`]), and only that one has the state to render.
    #[test]
    fn coder_preamble_names_the_command_tools_without_rendering_any_list() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Coder, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(
            text.contains("run a command outside the nyx tools (an unobserved launch)"),
            "the existing prohibition is still the anchor of the bullet: {text}",
        );
        assert!(
            text.contains("`list_commands`") && text.contains("`start_command`"),
            "…and it now names the two tools that make it actionable",
        );
        assert!(
            text.contains("NO-OP on one already running"),
            "…and the reason there is never a need to relaunch blind",
        );
        assert!(
            !text.contains("Managed commands of this workspace:"),
            "the preamble is STATIC: it renders no command list (that is the ambient context's job)",
        );
        // Not propagated: the other roles keep the manuals they had.
        for role in [
            AgentRole::Dev,
            AgentRole::Auditor,
            AgentRole::Doc,
            AgentRole::Prototype,
        ] {
            assert!(
                !role.preamble().contains("`start_command`"),
                "{:?} is deliberately untouched — the coder is the one that executes, and the \
                 ambient context covers everyone else",
                role.label(),
            );
        }
    }

    /// v2.8 (task 01KY71J3MVTQ1V86J92GNC9FKH, annex `context-effecteur`): an EFFECTOR (coder)
    /// renders its ROLE context PLUS `{{rules}}` injected at the marker, resolved by scope
    /// (always / role:coder / fork) + lineage. This drives a POPULATED [`RuleSet`] through the real
    /// render seam to prove the marker resolves + injects (grouped, ordered, role-isolated).
    #[test]
    fn render_effector_injects_rules_resolved_by_scope_and_lineage() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let rules = RuleSet {
            rules: vec![
                Rule::curated(RuleScope::Always, "pnpm, never npm"),
                Rule::curated(RuleScope::Always, "sandbox-* libs auto-include"),
                Rule::curated(
                    RuleScope::Role(AgentRole::Coder),
                    "compose from the design-system",
                ),
                // A DIFFERENT role's rule must NOT leak into the coder render.
                Rule::curated(
                    RuleScope::Role(AgentRole::Auditor),
                    "auditor-only directive",
                ),
                Rule {
                    scope: RuleScope::Fork,
                    text: "amounts in cents, never a float".into(),
                    curated: false,
                },
            ],
            lineage: Some(Lineage {
                forked_from: "coder".into(),
            }),
        };
        let text = render_agent_context_with_rules(&db, AgentRole::Coder, &spec_id, &rules, None)
            .unwrap()
            .unwrap();

        // The role context is still there, and the marker is consumed (resolved).
        assert!(
            text.contains("Context: Coder Agent"),
            "role context preserved"
        );
        assert!(!text.contains(RULES_MARKER), "the marker is resolved");
        // The injected block header names the RESOLVED scopes + the lineage.
        assert!(
            text.contains("## Rules (project directives)")
                && text
                    .contains("Resolved at the rules marker by scope (always + role:coder + fork)")
                && text.contains("forked_from coder"),
            "rules block header names the resolved scopes + lineage",
        );
        // Each scope group carries its rule, tagged curated|proposed.
        assert!(
            text.contains("### always")
                && text.contains("- pnpm, never npm [curated]")
                && text.contains("- sandbox-* libs auto-include [curated]"),
            "always-scoped rules injected",
        );
        assert!(
            text.contains("### role:coder")
                && text.contains("- compose from the design-system [curated]"),
            "role:coder rule injected",
        );
        assert!(
            text.contains("### fork")
                && text.contains("- amounts in cents, never a float [proposed]"),
            "fork rule injected (proposed tag)",
        );
        // Role isolation: another role's rule is NOT injected here.
        assert!(
            !text.contains("auditor-only directive"),
            "a role:auditor rule does not leak into the coder render",
        );
        // The rules land AFTER the role contract (injected at the marker at the end of the template).
        let contract_at = text.find("Context: Coder Agent").unwrap();
        let rules_at = text.find("## Rules (project directives)").unwrap();
        assert!(
            rules_at > contract_at,
            "rules injected after the role contract"
        );
    }

    /// coder / auditor / doc each render their role context WITH the `{{rules}}` marker RESOLVED,
    /// even against the empty default source (no store yet): the marker is consumed and an explicit
    /// scope-named "no rules configured" block is injected. The auditor's block also lands after its
    /// axis instruction, and the resolved scopes name each effector's own role scope.
    #[test]
    fn render_effectors_resolve_the_rules_marker_with_the_empty_default_source() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        for (role, scope_label) in [
            (AgentRole::Coder, "role:coder"),
            (AgentRole::Doc, "role:doc"),
        ] {
            let text = render_agent_context(&db, role, &spec_id, None)
                .unwrap()
                .unwrap();
            assert!(
                !text.contains(RULES_MARKER),
                "{scope_label}: marker resolved"
            );
            assert!(
                text.contains("## Rules (project directives)"),
                "{scope_label}: rules block injected",
            );
            assert!(
                text.contains(&format!("scope (always + {scope_label})")),
                "{scope_label}: resolved scopes name the effector's own role scope (no fork, unforked)",
            );
            assert!(
                text.contains("No project rules configured yet"),
                "{scope_label}: empty store renders the explicit no-rules note",
            );
        }
        // The auditor: the rules block resolves at the END of its own manual.
        let auditor = render_agent_context(&db, AgentRole::Auditor, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(!auditor.contains(RULES_MARKER), "auditor marker resolved");
        assert!(
            auditor.contains("scope (always + role:auditor)"),
            "auditor resolves its own role scope",
        );
        let how_at = auditor.find("One point, one finding").unwrap();
        let rules_at = auditor.find("## Rules (project directives)").unwrap();
        assert!(rules_at > how_at, "auditor rules land after its manual");
    }

    /// The out-of-scope roles (spec / prototype) carry NO `{{rules}}` marker, so their render is
    /// passed through untouched — no rules block, and certainly no stray unresolved marker.
    #[test]
    fn non_effector_roles_carry_no_rules_marker() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        for role in [AgentRole::Spec, AgentRole::Prototype] {
            let text = render_agent_context(&db, role, &spec_id, None)
                .unwrap()
                .unwrap();
            assert!(!text.contains(RULES_MARKER), "no stray marker");
            assert!(
                !text.contains("## Rules (project directives)"),
                "no rules block for a non-effector role",
            );
        }
    }

    /// The pure resolution semantics: always for every role; `role:<role>` scoped to that role;
    /// `fork` only with a lineage; the DISPATCHER (dev) is always-only.
    #[test]
    fn resolve_for_orders_scopes_isolates_roles_and_gates_fork_on_lineage() {
        let set = RuleSet {
            rules: vec![
                Rule::curated(RuleScope::Always, "A"),
                Rule::curated(RuleScope::Role(AgentRole::Coder), "C"),
                Rule::curated(RuleScope::Role(AgentRole::Auditor), "U"),
                Rule::curated(RuleScope::Fork, "F"),
            ],
            lineage: Some(Lineage {
                forked_from: "coder".into(),
            }),
        };
        // Coder (forked): always → role:coder → fork, in that order; auditor's rule excluded.
        let coder: Vec<&str> = set
            .resolve_for(AgentRole::Coder)
            .iter()
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            coder,
            vec!["A", "C", "F"],
            "coder: always + role:coder + fork, ordered"
        );
        // Auditor (same set): its own role rule, not the coder's.
        let auditor: Vec<&str> = set
            .resolve_for(AgentRole::Auditor)
            .iter()
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            auditor,
            vec!["A", "U", "F"],
            "auditor: always + role:auditor + fork"
        );
        // Dispatcher: always only (no role, no fork), even with the populated set + lineage.
        let dev: Vec<&str> = set
            .resolve_for(AgentRole::Dev)
            .iter()
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(dev, vec!["A"], "dev dispatcher: always only");
        // No lineage → fork is dropped for the effectors too.
        let unforked = RuleSet {
            rules: set.rules.clone(),
            lineage: None,
        };
        let coder_unforked: Vec<&str> = unforked
            .resolve_for(AgentRole::Coder)
            .iter()
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            coder_unforked,
            vec!["A", "C"],
            "unforked coder: no fork scope"
        );
    }

    /// Review finding 01KY4ZJQ6BCXHQZ8268P47ZGNH: the Coder is a non-interactive sub-agent — it
    /// SURFACES a discovered follow-up UPWARD to the dev orchestrator (final report / note, like a
    /// scope-gap) and never calls `idea_add` itself.
    #[test]
    fn render_coder_surfaces_followups_upward_not_idea_add() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Coder, &spec_id, None)
            .unwrap()
            .unwrap();
        // SURFACE upward to the orchestrator, in the report — never captured on its own authority.
        assert!(
            text.contains("SURFACED upward in your report"),
            "coder surfaces follow-ups upward instead of capturing them",
        );
        // It is explicitly told NOT to capture directly (no autonomous idea_add).
        assert!(
            text.contains("capture a follow-up yourself (no idea_add)")
                && text.contains("the orchestrator alone proposes it to the user"),
            "coder is told NOT to idea_add itself",
        );
        // The direct-capture form (with source=session) is the DEV's tool, never the coder's.
        assert!(
            !text.contains("source=\"session\""),
            "coder carries no direct idea_add capture instruction",
        );
    }

    /// ANTI-DUPLICATION SURVIVES THE DISSOLUTION, and it had to: the nspec GRAPH walks no idea node,
    /// so "no duplicate Spec for a need the backlog already tracks" is a prohibition with NO
    /// structural equivalent. It is kept as a prohibition (the Spec preamble) and as a CALL with
    /// this project's id in it (the authoring render's live backlog block). What went is the
    /// PROCEDURE around it — the narrated Step 0, and the backlog housekeeping (close / merge /
    /// regroup), which prohibits nothing and which the idea tools' own descriptors carry.
    #[test]
    fn render_spec_forbids_a_duplicate_spec_and_wires_the_backlog_call() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, &spec_id, None)
            .unwrap()
            .unwrap();
        // The PROHIBITION, self-contained: search first, link what the Spec resolves.
        assert!(
            text.contains("open a Spec for a need the project's backlog already tracks")
                && text.contains("idea_search")
                && text.contains("kind=\"addresses\""),
            "the anti-duplication prohibition names both verbs:\n{text}",
        );
        // The narrated procedure is gone.
        for gone in [
            "## Step 0",
            "RECONCILE the backlog while you author",
            "do NOT call idea_implement by hand",
        ] {
            assert!(
                !text.contains(gone),
                "the narrated backlog procedure `{gone}` must not survive",
            );
        }

        // And the AUTHORING render (no Spec yet — where a duplicate is actually created) carries the
        // call with the project's own id already in it.
        let (db2, project_id, _ws2, _cwd2) = seeded_project_without_spec();
        let project = db2
            .with_conn(|c| db::get_project(c, &project_id))
            .unwrap()
            .unwrap();
        let authoring = render_spec_authoring(&db2, Some(&project), "").unwrap();
        assert!(
            authoring.contains(&format!("idea_search(project_id=\"{project_id}\"")),
            "the authoring render wires the call with THIS project's id:\n{authoring}",
        );
        assert!(
            authoring.contains("rather than opening a duplicate Spec")
                && authoring.contains("idea_link(idea_id, spec_id, kind=\"addresses\")"),
            "and states what a match means + the link that follows",
        );
    }

    /// The AUDITOR audits ONE COMMITMENT of the Spec — the section the spawn's `subject` names —
    /// and files findings; it never rules on the work. Its manual must SAY both, name the four
    /// commitment sections it can be pointed at, and carry NOTHING of the invented "axes" it used to
    /// pick from (v2.8: the questions come from the Spec, the verdict from the findings).
    #[test]
    fn render_auditor_audits_one_named_commitment_and_pronounces_no_verdict() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let auditor = render_agent_context(&db, AgentRole::Auditor, &spec_id, None)
            .unwrap()
            .unwrap();

        assert!(auditor.contains("=== NYX CONTEXT — AUDITOR ==="), "header");
        assert!(
            auditor.contains("ONE COMMITMENT") && auditor.contains("`subject`"),
            "the mandate: one commitment, named by the spawn's subject",
        );
        // WHICH commitment is the graph's business, not the manual's: the spawn node names it and
        // the render carries THAT section, saying so.
        for section in [
            "user_stories",
            "testing_decisions",
            "implementation_decisions",
            "out_of_scope",
        ] {
            let pass = render_agent_context(&db, AgentRole::Auditor, &spec_id, Some(section))
                .unwrap()
                .unwrap();
            assert!(
                pass.contains(&format!("Your subject is `{section}`")),
                "a pass spawned on `{section}` is told so, and gets that section alone",
            );
        }
        assert!(
            auditor.contains("task_add kind=review") && auditor.contains("file:line"),
            "the finding rules survive: a finding is a review task, anchored",
        );
        assert!(
            auditor.contains("pronounce a verdict") && auditor.contains("DERIVES"),
            "it is forbidden a verdict — nyx derives one from the findings",
        );
        // The invented axes are GONE — no vocabulary left for the auditor to pick an angle.
        for gone in [
            "Your axis",
            "standards | spec (pick one)",
            "CODING_STANDARDS",
            "scope creep",
        ] {
            assert!(
                !auditor.contains(gone),
                "the axis prose `{gone}` must not survive in the auditor manual",
            );
        }
    }

    /// THE PROTOTYPE MANUAL IS PROHIBITIONS, AND THE PROCEDURE IS THE TOOLS'. It used to carry
    /// ~71 800 characters — 48 000 of them a "Design-system first" procedure its own text called
    /// "ONCE PER PROJECT", plus the round loop the `prototyper` graph drives. Every contract it
    /// narrated is the `design_*` / `proto_*` DESCRIPTORS an agent already receives in `tools/list`
    /// (guarded by `mcp::tests::tool_descriptors_describe_the_variance_model_the_tools_actually_accept`),
    /// so narrating it here billed the same text twice. What no descriptor and no node label can
    /// state is what the role may NOT do — this test pins exactly that, and pins the absence of the
    /// rest.
    #[test]
    fn render_prototype_carries_the_prohibitions_and_no_procedure() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Prototype, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(text.contains("Context: Prototype"), "prototype preamble");

        // THE PROHIBITIONS — each one is a rule the graph's structure cannot express.
        for (needle, why) in [
            (
                "re-invent a rendering",
                "compose from the catalogue; a missing component is AUTHORED as [NEW]",
            ),
            (
                "pose the RETENU",
                "the election is the USER's decision — the agent may only recommend",
            ),
            (
                "mutate a distilled (frozen) round",
                "a frozen round is never mutated; iterate by opening a new one",
            ),
            (
                "carry pre-dev feedback into dev",
                "the pre-dev comments are never transmitted to the coder",
            ),
            (
                "decide to iterate again, or to freeze",
                "ONE spawn = ONE round; nyx drives the rounds",
            ),
            (
                "visually VALIDATED on the strength of design_preview",
                "a markup render is not an eye — say what you checked and what you did not",
            ),
        ] {
            assert!(
                text.contains(needle),
                "the prohibition `{needle}` ({why}) must survive the dissolution",
            );
        }

        // THE PROCEDURE IS GONE — every one of these headings opened a block that re-narrated a
        // tool's own description.
        for gone in [
            "## Design-system first",
            "EXPLORE → RECIPE → INVENTORY, ONCE PER PROJECT",
            "Read in a LOOP, never in one gulp",
            "THE VARIANCE MODEL",
            "THE SIX TYPES, EXHAUSTIVELY",
            "REFERENCE A SHARED SUB-COMPONENT",
            "TAILWIND FIRST, THE CSS SLOT LAST",
            "## The prototype cycle (all agent-driven, via MCP)",
            "## The composition seam (what dev consumes)",
        ] {
            assert!(
                !text.contains(gone),
                "the narrated procedure `{gone}` belongs to the tool descriptors, not here",
            );
        }

        // AND THE SIZE IS THE POINT: the whole render, live Spec included, is now a fraction of the
        // preamble it used to open with.
        assert!(
            text.len() < 6_000,
            "the prototype render is {} chars — the dissolution is what this number is for",
            text.len(),
        );
    }
    /// A Spec with NO dev workspace anchored — the PROTOTYPE's nominal state, because a prototype is
    /// settled BEFORE the code exists (finding `01KZ3JQYRN95EETZ0JBQXED13T`). The manual used to
    /// render `Workspace: (not anchored yet)` and NOTHING else: no project line, no `project_id`
    /// anywhere in 64 KiB of text, no repos section — while its whole design half is written
    /// `design_recipe(project_id)`, `design_inventory(project_id)`, `design_tokens(project_id)`.
    /// A Spec carries `specs.project_id` from creation, so the project is resolvable there and the
    /// agent has to be handed it.
    #[test]
    fn prototype_manual_carries_the_project_id_without_any_anchored_workspace() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, spec_id) = db.with_conn(|c| {
            let (project, _root) =
                db::create_project(c, "Proj", "/tmp/nyx-ctx-noanchor", None).expect("project");
            db::create_repository(c, &project.id, "front", "apps/front", false).expect("repo");
            // A Spec with NO workspace anchor — the state a /nspec author leaves behind before any
            // code exists, and the state the prototype role runs in.
            let (spec, _rev) = db::create_spec(
                c,
                &project.id,
                "Design the pricing page",
                db::SpecSections {
                    problem: "the problem".into(),
                    solution: "the solution".into(),
                    ..Default::default()
                },
            )
            .expect("spec");
            (project.id, spec.id)
        });
        let text = render_agent_context(&db, AgentRole::Prototype, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(
            text.contains(&format!("project_id={project_id}")),
            "the header must hand the agent the project_id every design_* tool takes: {}",
            &text[..text.find("\n\n").unwrap_or(200)]
        );
        assert!(
            text.contains("## Project, repos & workspace"),
            "the project section is rendered with no anchor",
        );
        assert!(
            text.contains("- repo: front → apps/front"),
            "the project's repos are the ones the inventory walks — they belong to the project",
        );
        assert!(
            text.contains("`design_*` call takes the `project_id` in this manual's header"),
            "the manual SAYS where the id it keeps asking for comes from",
        );
        // …and it must not tell the agent to go and find a workspace it does not have.
        assert!(
            !text.contains("(not anchored yet)"),
            "the workspace-less state is stated as normal, not as a missing prerequisite",
        );
        // It is stated ONCE, on the header's Workspace line — the section below points at it
        // instead of repeating the same paragraph to every role (v2.8 render slim).
        assert!(
            text.contains("Workspace: (no dev workspace anchored — normal before the code exists"),
            "the header carries the WHY",
        );
        assert_eq!(
            text.matches("normal before the code exists").count(),
            1,
            "…and says it exactly once",
        );
        assert!(
            text.contains("- workspace: (none anchored — see the header)"),
            "the section points at the header rather than repeating it",
        );

        // The tool ECHOES the project id beside the manual, so a caller never parses the header.
        let out = agent_context(
            &db,
            &json!({ "role": "prototype", "spec_id": spec_id.clone() }),
        )
        .expect("prototype context");
        assert_eq!(out.result["project_id"], json!(project_id));

        // A `project_id` that DISAGREES with the Spec's own is refused BY NAME — it used to be
        // swallowed, leaving a byte-identical manual and a caller believing it had scoped it.
        let refused = agent_context(
            &db,
            &json!({ "role": "prototype", "spec_id": spec_id, "project_id": "some-other-project" }),
        )
        .expect_err("a foreign project_id is refused");
        assert_eq!(refused.code, RpcCode::InvalidArgument);
        assert!(
            refused.message.contains(&project_id) && refused.message.contains("some-other-project"),
            "the refusal names BOTH the Spec's project and the one that was asked for: {}",
            refused.message
        );
    }

    /// v2.8: the proto DECISION is nspec's graph (`judge "graphic render?"` → `ask_human "go through
    /// a proto?"` → `sub_workflow prototyper`). The author FLAGS a graphical subject and keeps what
    /// it does with the verdict — it no longer proposes, waits, or launches the prototype role.
    #[test]
    fn spec_role_flags_a_graphical_subject_and_never_launches_the_proto() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(
            text.contains("launch a prototype") && text.contains("you FLAG a graphical subject"),
            "the author flags; it never launches",
        );
        assert!(
            text.contains("nyx asks the human and runs the prototyper itself"),
            "the asking and the launching are nyx's",
        );
        // The delegation call is gone — an effector never spawns another effector.
        assert!(
            !text.contains("agent_context(role=prototype"),
            "the spec author no longer delegates to the prototype role itself",
        );
    }

    #[test]
    fn render_doc_role_carries_doc_sync_context() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Doc, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(text.contains("Context: Doc-sync"), "doc-sync preamble");
        assert!(
            text.contains("Git range\n(since..until)"),
            "reads the Spec's git range",
        );
        assert!(
            text.contains("validate your own output — it is a REVIEW"),
            "output is a review the human validates",
        );
    }

    #[test]
    fn render_spec_role_carries_authoring_context_and_sibling_specs() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, &spec_id, None)
            .unwrap()
            .unwrap();
        // The /nspec authoring context: the role, its prohibitions, and the project's live state.
        assert!(text.contains("Spec Author"), "spec author preamble");
        assert!(
            text.contains("the human validates the SPEC, you decide the TASKS"),
            "who validates what stays a prohibition, not a procedure",
        );
        assert!(
            text.contains("transition the Spec's lifecycle"),
            "the absolute no-transition prohibition is present (N17)",
        );
        // Anti-duplication live weaving: the project's other Specs (here: none besides this one).
        assert!(
            text.contains("Existing Specs in the project (anti-duplication)"),
            "sibling-specs weaving section",
        );
        // The idea-backlog prohibition lives in the shared spec-author preamble, so an anchored
        // render carries it too.
        assert!(
            text.contains("backlog already tracks") && text.contains("idea_search"),
            "the anti-duplication prohibition is in the spec-author preamble",
        );
    }

    /// WHAT NYX DISPLAYS, NYX ALSO ACCEPTS — and the reverse: the `spec_id` rides beside the `#N` on
    /// every surface that NAMES a Spec, so an agent never has to go and find an id nyx already holds.
    /// The observed session spent four MCP calls doing exactly that, on a render that listed the
    /// Specs `- #3 · titre · statut` while the idea list fifty lines below carried full ids.
    ///
    /// Three surfaces, one test, because they are one decision: the anchored render's HEADER line,
    /// its sibling-Specs list, and the /nspec AUTHORING render's list of the project's Specs.
    ///
    /// IT ASSERTS PRESENCE, AND NOTHING OF THE LAYOUT *BETWEEN THE TWO NAMES* (the task says so):
    /// each check looks for the id and for the `#N` inside the SAME line, and pins no separator, no
    /// order and no punctuation between them — that is what a render is free to re-format. It is
    /// NOT layout-free beyond that, and claiming so would be a promise the needles do not keep, and
    /// they do not all pin the same thing. The header needle `Spec    : My Feature (#N` freezes the
    /// column padding, the opening parenthesis and title-BEFORE-`#N`. The two list needles are the
    /// other way round: `#N · A Sibling Feature` freezes the separator AND the title after it, in
    /// the order `#N`-before-title, while the authoring one, `#N ·`, freezes the separator alone.
    /// What is UNDER TEST is that the two names travel together; the little that surrounds them is
    /// incidentally pinned, and a render that re-shapes it re-words these needles with it.
    ///
    /// The `#N` half is not decoration — it is the guard against "fixing" this by swapping the human
    /// key for the id, which would take away the only name the human reads.
    #[test]
    fn every_render_that_names_a_spec_carries_its_id_beside_the_human_key() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let (project_id, number, sibling_id, sibling_number) = db.with_conn(|c| {
            let spec = db::get_spec(c, &spec_id).expect("read").expect("the Spec");
            let (sibling, _rev) = db::create_spec(
                c,
                &spec.project_id,
                "A Sibling Feature",
                db::SpecSections::default(),
            )
            .expect("sibling");
            (spec.project_id, spec.number, sibling.id, sibling.number)
        });

        // Every line of a render that mentions `#N` must mention the id too — asserted per LINE, so
        // an id landing somewhere else in the manual cannot stand in for one that is missing here.
        let names_both = |text: &str, needle: &str, id: &str| {
            text.lines()
                .any(|line| line.contains(needle) && line.contains(id))
        };

        // 1 + 2 — the ANCHORED render: the `Spec    :` header, and the sibling list the /nspec author
        // reads for anti-duplication (rendered for the `spec` role).
        let anchored = render_agent_context(&db, AgentRole::Spec, &spec_id, None)
            .unwrap()
            .unwrap();
        assert!(
            names_both(
                &anchored,
                &format!("Spec    : My Feature (#{number}"),
                &spec_id
            ),
            "the header line names the Spec by #N AND by spec_id, like the project line \
             below it names project_id:\n{anchored}",
        );
        assert!(
            names_both(
                &anchored,
                &format!("#{sibling_number} · A Sibling Feature"),
                &sibling_id
            ),
            "the sibling-Specs list carries the id beside the #N:\n{anchored}",
        );

        // 3 — the AUTHORING render (/nspec with no Spec resolved): the SAME list, of the SAME
        // project, and it must not have been left behind.
        let authoring = agent_context(&db, &json!({ "role": "spec", "project_id": project_id }))
            .expect("authoring render")
            .result["context"]
            .as_str()
            .expect("a manual")
            .to_string();
        for (n, id) in [(number, &spec_id), (sibling_number, &sibling_id)] {
            assert!(
                names_both(&authoring, &format!("#{n} ·"), id),
                "the authoring render lists Spec #{n} with its id:\n{authoring}",
            );
        }
    }

    /// "DOES THIS SPEC ALREADY HAVE A RUN?" — ANSWERED IN THE RENDER, NEVER ASKED AGAIN.
    ///
    /// It cost the observed session four MCP calls, and the answer it never got is what let a
    /// SECOND run open beside a live one: run `01a011d0` was `running`, parked on a node, and
    /// nothing anywhere connected it to the Spec the human had named.
    ///
    /// FOUR STATES, and the point of the test is that they READ DIFFERENTLY:
    ///   * NOTHING live — written out, because "no line" and "no run" looking alike is the exact
    ///     confusion this PRD exists to end;
    ///   * a RUNNING run — id, flow, status, the node it is parked on, and how long since it moved;
    ///   * the SAME run left BLOCKED for a week — still live, and marked STALE, so abandoned work
    ///     cannot be mistaken for a colleague's session in progress;
    ///   * a TERMINAL run — not live, and back to the explicit "none".
    #[test]
    fn the_render_says_what_drives_the_spec_and_says_so_when_nothing_does() {
        use diesel::prelude::*;
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let render = || {
            render_agent_context(&db, AgentRole::Coder, &spec_id, None)
                .expect("render")
                .expect("a manual")
        };
        // WHAT IS ASSERTED IS THE RUN LINE ITSELF, not "somewhere in the manual" — the same reason
        // the sibling test above checks its two names per LINE. `running`, `last activity` and a
        // flow's name are ordinary words: a `contains` over the whole render would go green on a
        // needle the run line had dropped, as long as the manual said it anywhere at all.
        let run_line = |text: &str| -> String {
            text.lines()
                .find(|l| l.starts_with("Run     : "))
                .unwrap_or_default()
                .to_string()
        };

        // 1 — NOTHING LIVE. The absence is a sentence, not a missing line.
        let idle = render();
        assert!(
            idle.contains("Run     : (none live"),
            "an undriven Spec SAYS it is undriven:\n{idle}",
        );

        // 2 — A RUNNING RUN parked on a node.
        let (node_id, run_id) = db.with_conn(|c| {
            let project_id = db::get_spec(c, &spec_id).unwrap().unwrap().project_id;
            let wf = db::create_workflow(
                c,
                Some(&project_id),
                "authoring",
                db::WORKFLOW_SCOPE_NSPEC,
                None,
            )
            .expect("workflow");
            let node = db::create_node(
                c,
                &wf.id,
                None,
                0,
                db::NODE_TYPE_NOTE,
                "Grill the intent",
                "{}",
            )
            .expect("node");
            let run = db::create_workflow_run(c, &wf.id, Some(&spec_id), Some(&node.id), "{}")
                .expect("run");
            (node.id, run.id)
        });
        let live = render();
        assert!(
            !live.contains("(none live"),
            "a Spec with a live run must not read as undriven:\n{live}",
        );
        let live_line = run_line(&live);
        for needle in [
            run_id.as_str(),
            "authoring",
            db::RUN_STATUS_RUNNING,
            "Grill the intent",
            node_id.as_str(),
            "last activity",
        ] {
            assert!(
                live_line.contains(needle),
                "the run line carries '{needle}' — its id, its flow, its status, the node it landed \
                 on (label AND id) and how long since it moved are what the caller would otherwise \
                 go and fetch:\n{live}",
            );
        }

        // 3 — THE SAME RUN, BLOCKED and untouched for a week. Still live (a blocked run re-runs its
        // gate on the next tick), and no longer presentable as work in progress.
        let a_week_ago = db::now_millis() - 7 * 24 * 60 * 60 * 1000;
        db.with_conn(|c| {
            db::update_workflow_run(
                c,
                &run_id,
                Some(&node_id),
                "{}",
                db::RUN_STATUS_BLOCKED,
                None,
            )
            .expect("block it");
            diesel::update(crate::schema::workflow_runs::table.find(&run_id))
                .set(crate::schema::workflow_runs::updated_at.eq(a_week_ago))
                .execute(c)
        })
        .expect("age the run");
        let stranded = render();
        let stranded_line = run_line(&stranded);
        assert!(
            stranded_line.contains(db::RUN_STATUS_BLOCKED) && stranded_line.contains("7 day(s)"),
            "a blocked run says so, and says how long it has sat:\n{stranded}",
        );
        assert!(
            stranded_line.contains("STALE"),
            "…and past the threshold run_start strands one by, it is marked as abandoned work \
             rather than a session in progress:\n{stranded}",
        );

        // 4 — TERMINAL is not live: the render goes back to the explicit absence.
        db.with_conn(|c| {
            db::update_workflow_run(c, &run_id, None, "{}", db::RUN_STATUS_DONE, None)
        })
        .expect("finish it");
        assert!(
            render().contains("Run     : (none live"),
            "a finished run is not a live one",
        );
    }

    /// THE STALENESS THRESHOLD IS ONE NUMBER, shared with the warning that motivated it.
    ///
    /// `run_start` calls a run STRANDED past [`crate::mcp_engine::STRANDED_BLOCKED_RUN_AFTER_MS`]
    /// while this render calls it STALE; two copies of "how long is too long" would let one surface
    /// report abandoned work while the other still showed a healthy session — the two-truths failure
    /// this module has already paid for on the anchor order. The line is built FROM the constant, so
    /// there is nothing to keep in step; this pins the boundary either side of it.
    #[test]
    fn the_render_marks_a_run_stale_on_the_same_threshold_run_start_strands_one() {
        let threshold = crate::mcp_engine::STRANDED_BLOCKED_RUN_AFTER_MS;
        let state = |idle_ms: i64| SpecRunState {
            run_id: "01run".to_string(),
            flow: "ndev".to_string(),
            scope: db::WORKFLOW_SCOPE_NDEV.to_string(),
            status: db::RUN_STATUS_BLOCKED.to_string(),
            node: None,
            idle_ms,
            child: false,
        };
        assert!(
            !run_state_line(&state(threshold - 1)).contains("STALE"),
            "one millisecond short of the threshold is not stale",
        );
        assert!(
            run_state_line(&state(threshold)).contains("STALE"),
            "the threshold itself is",
        );
    }

    /// THE RUN BLOCK IS THE ONE PIECE OF THIS HEADER WHOSE SIZE IS A FUNCTION OF STATE — one line
    /// per live run — and the state it reads is the very one `run_start`'s stranded warning
    /// describes as accumulating in silence. Unbounded, a Spec that collected non-terminal runs
    /// would put a line per corpse at the top of every render of every pass, and the measured budget
    /// of [`a_role_render_stays_inside_its_measured_budget`] would be a bound on nothing.
    ///
    /// SO IT IS CAPPED, AND THE CAP STILL ANSWERS THE QUESTION: [`SPEC_RUN_LINES_MAX`] lines MOST
    /// RECENTLY ACTIVE FIRST, then ONE line counting the rest — the reader still learns how many are
    /// live and which verb clears one. What a cap must NOT become is a way to stop saying "this is
    /// abandoned work", so a STALE run among the listed ones is still marked as such.
    ///
    /// AND WHAT IT DROPS IS THE POINT OF THE ORDER. The fixture below makes the run that STARTED
    /// FIRST the one touched LAST — the shape of every ndev cycle, whose top-level flow opens before
    /// each `sub_workflow` child it spawns and keeps ticking after them — and asserts that this run
    /// is LISTED. Ordered by `started_at`, it is the first line a cap of three drops at four live
    /// runs: the header would count the run it exists to name.
    #[test]
    fn the_run_block_lists_the_moving_runs_and_counts_the_rest() {
        use diesel::prelude::*;
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();

        // Two more live runs than the header will list. One workflow each: nyx holds at most ONE
        // live run per (workflow, Spec), so this is what accumulation actually looks like.
        let total = SPEC_RUN_LINES_MAX + 2;
        let mut ids: Vec<String> = Vec::new();
        for i in 0..total {
            ids.push(db.with_conn(|c| {
                let project_id = db::get_spec(c, &spec_id).unwrap().unwrap().project_id;
                let wf = db::create_workflow(
                    c,
                    Some(&project_id),
                    &format!("flow-{i}"),
                    db::WORKFLOW_SCOPE_CUSTOM,
                    None,
                )
                .expect("workflow");
                db::create_workflow_run(c, &wf.id, Some(&spec_id), None, "{}")
                    .expect("run")
                    .id
            }));
        }
        // LAST ACTIVITY RUNS AGAINST START ORDER — the ndev shape. `ids[0]` opened first and is the
        // one still moving; every run opened after it has been sitting longer, `ids[1]` onwards past
        // the staleness threshold. So the block's three lines are `ids[0..3]`, two of them STALE,
        // and `started_at` desc would have listed `ids[2..5]` and merely counted the driver.
        let week = 7 * 24 * 60 * 60 * 1000;
        for (i, id) in ids.iter().enumerate() {
            db.with_conn(|c| {
                diesel::update(crate::schema::workflow_runs::table.find(id))
                    .set(
                        crate::schema::workflow_runs::updated_at
                            .eq(db::now_millis() - (i as i64) * week),
                    )
                    .execute(c)
            })
            .expect("age it");
        }

        let text = render_agent_context(&db, AgentRole::Coder, &spec_id, None)
            .expect("render")
            .expect("a manual");
        let block: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("Run     : "))
            .collect();
        assert_eq!(
            block.len(),
            SPEC_RUN_LINES_MAX + 1,
            "{total} live runs render as {SPEC_RUN_LINES_MAX} lines plus ONE counting the rest, \
             whatever the state has accumulated:\n{text}",
        );

        // MOST RECENTLY ACTIVE FIRST is what a cap drops the least recently active of.
        for id in ids.iter().take(SPEC_RUN_LINES_MAX) {
            assert!(
                block.iter().any(|l| l.contains(id.as_str())),
                "the {SPEC_RUN_LINES_MAX} most recently touched live runs are the ones listed; \
                 {id} is not:\n{text}",
            );
        }
        for id in ids.iter().skip(SPEC_RUN_LINES_MAX) {
            assert!(
                !text.contains(id.as_str()),
                "the ones nothing has touched in longest are counted, not listed; {id} is still \
                 there:\n{text}",
            );
        }

        // THE DRIVER SURVIVES THE CAP. `ids[0]` is the OLDEST run by `started_at` and the one still
        // moving. What that STANDS IN FOR is an ndev flow still ticking among the `sub_workflow`
        // children it opened — but the property is about the ORDER, not about lineage, so the
        // fixture holds only what the property needs: five top-level runs on five flows, none of
        // them anyone's child, aged so that start order runs against last activity. Under the
        // previous `started_at` order `ids[0]` was the first line dropped; here it is the first
        // line printed.
        assert!(
            block[0].contains(ids[0].as_str()),
            "the run that started first but is still moving heads the block — that is the one the \
             header exists to name:\n{text}",
        );

        // The COUNT says how many are hidden, how many are live in all, and — because the hidden
        // ids are exactly what it withheld — what a reader can and cannot do about them: `run_list`
        // is named ONLY beside the `workflow_id` it takes, `run_delete` only beside the ids above.
        let counted = block.last().expect("the counting line");
        for needle in [
            format!("{}", total - SPEC_RUN_LINES_MAX),
            format!("{total} live"),
            crate::mcp_authoring::RUN_DELETE_TOOL.to_string(),
            "workflow_id".to_string(),
            "run_id".to_string(),
        ] {
            assert!(
                counted.contains(&needle),
                "the counting line must carry '{needle}', got: {counted}",
            );
        }
        assert!(
            counted.contains(crate::mcp_authoring::RUN_LIST_TOOL)
                && counted.contains("No verb lists a Spec's runs across flows"),
            "the counting line may name {} only to say it does NOT answer for this set — the \
             hidden runs are on as many flows as there are runs (N20), got: {counted}",
            crate::mcp_authoring::RUN_LIST_TOOL,
        );

        assert!(
            block
                .iter()
                .any(|l| l.contains(ids[1].as_str()) && l.contains("STALE")),
            "a listed run past the staleness threshold is still marked abandoned:\n{text}",
        );
        assert!(
            !block[0].contains("STALE"),
            "…and the one still moving is not:\n{text}",
        );
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

    /// THE TWO BRANCHES OF `agent_context`'s CONDITIONAL `#N` (finding `01M0ZKWXS353TABS0WCT3XXB81`).
    ///
    /// `role` is this call's ONLY required argument, so every project anchor it could read is
    /// optional — and `agent_context(role="dev", spec_id="#3")` alone used to answer the literal
    /// `unknown spec 3` this PRD exists to retire, on a field whose description had just promised
    /// the form. The refusal is now raised at the entry point, before the render, and it is
    /// actionable; and the same reference resolves as soon as ONE anchor is passed, which is what
    /// makes the condition a condition rather than a wall.
    #[test]
    fn agent_context_honours_a_short_reference_only_with_the_anchor_it_advertises() {
        let _g = db_guard();
        let (db, spec_id, workspace_id, cwd) = seeded();
        let (project_id, number) = db.with_conn(|c| {
            let spec = db::get_spec(c, &spec_id).unwrap().expect("spec");
            (spec.project_id, spec.number)
        });
        let token = format!("#{number}");

        // NO ANCHOR — the number has no project to be counted in, and the refusal says so by name.
        let err = agent_context(&db, &json!({ "role": "dev", "spec_id": token }))
            .expect_err("a bare #N anchors no project here");
        assert_eq!(err.code, RpcCode::InvalidArgument);
        assert!(
            !err.message.contains("unknown spec"),
            "the opaque sentence this PRD retires must not be what an agent gets: {}",
            err.message
        );
        assert!(
            err.message.contains(crate::spec_ref::ANCHORLESS_FORMS) && err.message.contains(&token),
            "the refusal quotes the token and names what to send instead: {}",
            err.message
        );

        // ANY ONE OF THE FOUR ANCHORS — the same reference resolves to the Spec it names.
        for anchor in [
            json!({ "project_id": project_id }),
            json!({ "workspace_id": workspace_id }),
            json!({ "cwd": format!("{cwd}/crates/api") }),
        ] {
            let mut args = json!({ "role": "dev", "spec_id": token });
            for (k, v) in anchor.as_object().expect("anchor object") {
                args[k] = v.clone();
            }
            let out = agent_context(&db, &args)
                .unwrap_or_else(|e| panic!("anchor {anchor} should let '{token}' resolve: {e:?}"));
            assert_eq!(
                out.result["spec_id"],
                json!(spec_id),
                "anchor {anchor}: the short reference must resolve to the Spec it names"
            );
        }
    }

    /// THE 0/1/N LOCATION CONTRACT, and that the `workspace_id` anchor and the `cwd` anchor answer
    /// IDENTICALLY — they funnel through the SAME [`unique_spec_for_workspace`], so there is one
    /// place where a location becomes a Spec:
    ///   * 0 active Specs anchored ⇒ `Ok(None)` — no Spec, never an arbitrary one;
    ///   * 1 ⇒ its id;
    ///   * N ⇒ `invalid_argument` carrying `reason = spec_ambiguous_for_workspace` (the machine
    ///     handle) AND every candidate id, so the caller can pass an explicit `spec_id`.
    ///
    /// The N case is the one that matters: the most recently created Spec is emphatically NOT the
    /// answer. (`cwd` is a path NESTED under the workspace root — the everyday shape of an agent's
    /// cwd — so the ancestor match is exercised on every platform, not just the exact-root hit.)
    #[test]
    fn resolving_a_spec_by_location_answers_zero_one_or_a_listed_ambiguity() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (project_id, workspace_id, cwd) = db.with_conn(|c| {
            let (project, root) =
                db::create_project(c, "Proj", "/tmp/nyx-zero-one-n", None).expect("project");
            (project.id, root.id.clone(), root.root_path.clone())
        });
        let by_ws = json!({ "workspace_id": workspace_id });
        let by_cwd = json!({ "cwd": format!("{cwd}/crates/api") });
        let anchor = |title: &str| {
            db.with_conn(|c| {
                db::create_spec_with_workspace(
                    c,
                    &project_id,
                    &workspace_id,
                    title,
                    db::SpecSections::empty(),
                )
                .expect("spec")
                .0
                .id
            })
        };

        // These anchors are LOCATIONS, never an explicit `spec_id`, so the project-relative policy
        // is never consulted on this path — either value would drive the same assertions.
        const BY_LOCATION: ProjectRelative = ProjectRelative::RefusedHere;

        // ZERO — both anchors resolve NOTHING, and that is an answer rather than an error.
        assert_eq!(resolve_spec_id(&db, &by_ws, BY_LOCATION).unwrap(), None);
        assert_eq!(resolve_spec_id(&db, &by_cwd, BY_LOCATION).unwrap(), None);

        // ONE — both anchors resolve THE one.
        let first = anchor("First");
        assert_eq!(
            resolve_spec_id(&db, &by_ws, BY_LOCATION)
                .unwrap()
                .as_deref(),
            Some(first.as_str())
        );
        assert_eq!(
            resolve_spec_id(&db, &by_cwd, BY_LOCATION)
                .unwrap()
                .as_deref(),
            Some(first.as_str())
        );

        // N — both anchors REFUSE, with the machine-readable reason and the full candidate list.
        let second = anchor("Second");
        for args in [&by_ws, &by_cwd] {
            let err = resolve_spec_id(&db, args, BY_LOCATION)
                .expect_err("N candidates cannot resolve one");
            assert_eq!(err.code, RpcCode::InvalidArgument);
            assert!(
                is_ambiguous_spec(&err),
                "the refusal carries the machine handle: {err:?}"
            );
            assert_eq!(
                err.data
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str),
                Some("spec_ambiguous_for_workspace"),
                "the reason marker is the documented constant",
            );
            assert!(
                err.message.contains(&first) && err.message.contains(&second),
                "both candidates are listed: {}",
                err.message
            );
        }
    }

    /// THE FOUR FORMS REACH THE RENDER, through the ONE entry point. `agent_context` is asked for
    /// the same Spec four ways — the canonical id no human ever sees, the human key, the `#N` the
    /// renders print, and the bare `N` a human actually types — and answers the same Spec every
    /// time. Nothing in `agent_context` knows the grammar: widening [`resolve_spec_arg`] is what
    /// serves it, and with it `run_start` and every `spec_*` tool.
    #[test]
    fn agent_context_accepts_the_four_forms_of_a_spec_reference() {
        let _g = db_guard();
        let (db, spec_id, _ws, cwd) = seeded();
        let (key, number) = db.with_conn(|c| {
            let spec = db::get_spec(c, &spec_id).expect("read").expect("seeded");
            let project = db::get_project(c, &spec.project_id)
                .expect("read")
                .expect("seeded");
            (project.key, spec.number)
        });

        for token in [
            spec_id.clone(),
            format!("{key}-{number}"),
            format!("#{number}"),
            number.to_string(),
        ] {
            // The `cwd` is what a `#N` / `N` counts in — the SAME anchor the call already carried.
            let out = agent_context(
                &db,
                &json!({ "role": "coder", "spec_id": token, "cwd": cwd }),
            )
            .unwrap_or_else(|e| panic!("form '{token}' was refused: {e:?}"));
            assert_eq!(
                out.result["spec_id"], spec_id,
                "form '{token}' must render the SAME Spec"
            );
        }
    }

    /// A CALL PASSING A CANONICAL ID IS UNTOUCHED — the property that makes widening the grammar
    /// safe for the 30-odd `spec_id` fields that inherit it. A well-formed id naming NO Spec is
    /// never re-read as a key or a number: it keeps its own refusal, at the render, where it
    /// always was.
    #[test]
    fn a_canonical_id_argument_keeps_its_own_refusal() {
        let _g = db_guard();
        let (db, _spec_id, _ws, cwd) = seeded();
        let unknown = "019ff517-0000-7000-8000-0000000000ab";

        // The grammar hands the id straight back — no lookup, no project, no reinterpretation.
        assert_eq!(
            resolve_spec_arg(&db, &json!({ "cwd": cwd }), unknown).unwrap(),
            unknown
        );
        let err = agent_context(&db, &json!({ "role": "coder", "spec_id": unknown }))
            .expect_err("an id naming no Spec is still refused");
        assert_eq!(err.code, RpcCode::InvalidId);
        assert!(
            err.message.contains(unknown),
            "the refusal names the id the caller sent: {}",
            err.message
        );
    }

    /// A REFERENCE THAT DESIGNATES NOTHING COMES BACK AS SENT, so the caller's own refusal still
    /// names what the human typed (`unknown spec '99'`, not `unknown spec ''`) and can still tell
    /// it apart from prose. Resolution is ADDED here; no refusal is taken away.
    #[test]
    fn an_unresolvable_reference_is_handed_back_verbatim() {
        let _g = db_guard();
        let (db, _spec_id, _ws, cwd) = seeded();
        let anchored = json!({ "cwd": cwd });
        for token in ["99", "#99", "NOSUCH-1", "some prose"] {
            assert_eq!(
                resolve_spec_arg(&db, &anchored, token).unwrap(),
                token,
                "'{token}' designates nothing, so it must reach the caller unchanged"
            );
        }
    }

    /// WITHOUT AN ANCHOR THERE IS NO PROJECT TO COUNT IN, and `#N` / `N` resolve nothing rather
    /// than being hunted across the database — the cross-project leak the grammar exists to keep
    /// shut. The two self-describing forms still work, anchor or no anchor.
    #[test]
    fn a_bare_number_needs_an_anchor_while_the_key_carries_its_own() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let (key, number) = db.with_conn(|c| {
            let spec = db::get_spec(c, &spec_id).expect("read").expect("seeded");
            let project = db::get_project(c, &spec.project_id)
                .expect("read")
                .expect("seeded");
            (project.key, spec.number)
        });
        let no_anchor = json!({});

        assert_eq!(
            resolve_spec_arg(&db, &no_anchor, &number.to_string()).unwrap(),
            number.to_string(),
            "a bare number with nothing to count in resolves nothing"
        );
        assert_eq!(
            resolve_spec_arg(&db, &no_anchor, &format!("{key}-{number}")).unwrap(),
            spec_id,
            "the human key names its own project, so it needs no anchor"
        );
    }

    /// THE cwd→workspace TIE-BREAK, pinned. Two matching workspaces can only tie on `root_path`
    /// LENGTH by sharing the very same `root_path` (both are prefixes of one cwd, so equal length
    /// means equal string), and uniqueness ALLOWS that: it is `UNIQUE(project_id, root_path)`, so
    /// two PROJECTS may be registered on one folder. The winner is the INCUMBENT — the lowest id,
    /// hence (ids being UUIDv7) the OLDEST registration — never the newest: the ranking used to be
    /// `max_by` on length alone, which returns the LAST maximum of a `(root_path, id)`-ordered list
    /// and therefore silently re-pointed the folder at whatever workspace was created last.
    #[test]
    fn a_root_path_tie_is_broken_on_the_incumbent_workspace_never_the_newest() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (incumbent, latecomer, deeper, shared_path, deep_path) = db.with_conn(|c| {
            let (_p1, w1) = db::create_project(c, "First", "/tmp/nyx-tie", None).expect("p1");
            // A SECOND project on the SAME folder — allowed by UNIQUE(project_id, root_path).
            let (_p2, w2) = db::create_project(c, "Second", "/tmp/nyx-tie", None).expect("p2");
            let (_p3, w3) =
                db::create_project(c, "Deeper", "/tmp/nyx-tie/inner", None).expect("p3");
            let shared = w1.root_path.clone();
            let deep = w3.root_path.clone();
            (w1.id, w2.id, w3.id, shared, deep)
        });
        assert_eq!(
            db.with_conn(|c| db::get_workspace(c, &latecomer))
                .unwrap()
                .unwrap()
                .root_path,
            shared_path,
            "the two workspaces really do share one root_path — the tie is reachable"
        );
        assert!(
            incumbent < latecomer,
            "UUIDv7 ids are time-ordered, so the second registration sorts HIGHER: \
             {incumbent} vs {latecomer}"
        );

        // THE TIE: the incumbent wins, deterministically — the newest never takes the folder.
        let winner = db
            .with_conn(|c| best_workspace_for_cwd(c, &shared_path))
            .unwrap()
            .expect("the folder resolves a workspace");
        assert_eq!(
            winner.id, incumbent,
            "the incumbent (lowest id) wins the tie"
        );
        assert_ne!(
            winner.id, latecomer,
            "the most recent registration never wins silently"
        );

        // …and the tie-break NEVER overrides the depth rule: with a cwd nested under BOTH roots,
        // the deeper one wins. The guard below keeps this assertion from going vacuous: the
        // shallow root must really be a candidate for that cwd (it was not on Windows while the
        // matcher compared with a hard-coded `/`, leaving a single workspace in the race).
        let nested_cwd = crate::pathnorm::normalize(&format!("{deep_path}/src"));
        assert!(
            crate::pathnorm::is_ancestor_or_equal(&shared_path, &nested_cwd)
                && crate::pathnorm::is_ancestor_or_equal(&deep_path, &nested_cwd),
            "both roots match the nested cwd, so depth is what decides: \
             {shared_path} / {deep_path} vs {nested_cwd}"
        );
        let deep = db
            .with_conn(|c| best_workspace_for_cwd(c, &nested_cwd))
            .unwrap()
            .expect("the nested folder resolves a workspace");
        assert_eq!(deep.id, deeper, "the deepest matching root_path still wins");
    }

    /// LIVENESS IS NOT IN THE RANKING, pinned so the limit is a DECISION and not an accident. The
    /// candidate set of [`best_workspace_for_cwd`] is `db::all_workspaces`, which filters neither
    /// `archived_at` nor `folder_removed_at`, so on a shared `root_path` the incumbent takes the cwd
    /// even archived and folder-cleaned, ahead of a live registration. It is the same unfiltered set
    /// [`crate::resolve::resolve_workspace`] ranks for terminal auto-attach: filtering dead rows must
    /// happen in BOTH selectors (and change this test), or a cwd would answer one workspace for a
    /// terminal and another for a Spec.
    #[test]
    fn an_archived_workspace_still_wins_the_cwd_because_liveness_is_not_ranked() {
        let _g = db_guard();
        let db = Db::in_memory();
        let (dead, live, shared_path) = db.with_conn(|c| {
            let (_p1, w1) =
                db::create_project(c, "Archived", "/tmp/nyx-liveness", None).expect("p1");
            // A SECOND project on the SAME folder — allowed by UNIQUE(project_id, root_path).
            let (_p2, w2) = db::create_project(c, "Live", "/tmp/nyx-liveness", None).expect("p2");
            // The incumbent is retired both ways: archived AND its worktree folder cleaned up.
            db::archive_workspace(c, &w1.id).expect("archive");
            db::mark_workspace_folder_removed(c, &w1.id).expect("folder cleanup");
            (w1.id.clone(), w2.id, w1.root_path)
        });
        assert!(dead < live, "the archived row is the incumbent (lowest id)");

        let winner = db
            .with_conn(|c| best_workspace_for_cwd(c, &shared_path))
            .unwrap()
            .expect("the folder resolves a workspace");
        assert_eq!(
            winner.id, dead,
            "the incumbent wins the cwd even archived — liveness is not a ranking key",
        );
        assert!(
            winner.archived_at.is_some() && winner.folder_removed_at.is_some(),
            "and the winner really is the retired row: {winner:?}",
        );
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

    /// Review finding 01KXENA8R6NC2F2P8SSNTZ6YXJ: /nspec in a project workspace with NO Spec
    /// anchored must inject the PROJECT-scoped AUTHORING manual (grill → write → decompose →
    /// spec_create) listing the PROJECT's Specs (here: zero) for anti-duplication — with NO
    /// error and NO "anchored on a workspace" language. The project resolves from project_id,
    /// workspace_id, OR cwd (the three anchors a stub-less provider supplies).
    #[test]
    fn spec_role_without_anchored_spec_renders_project_scoped_authoring() {
        let _g = db_guard();
        let (db, project_id, workspace_id, cwd) = seeded_project_without_spec();

        // Resolve the PROJECT via project_id — no Spec anchored anywhere → authoring, not error.
        let by_project =
            agent_context(&db, &json!({ "role": "spec", "project_id": project_id })).unwrap();
        // A pure read carrying the project (not a Spec).
        assert!(
            by_project.effects.is_empty(),
            "authoring render is a pure read"
        );
        assert!(
            by_project.result["spec_id"].is_null(),
            "no Spec resolved yet"
        );
        assert_eq!(by_project.result["project_id"], project_id);

        let text = by_project.result["context"].as_str().unwrap();
        // The AUTHORING contract (grill / decompose / create the Spec).
        assert!(text.contains("SPEC (authoring)"), "authoring header");
        assert!(text.contains("Spec Author"), "spec-author preamble");
        assert!(
            text.contains("The human DUMPS raw, you GRILL, the human\nVALIDATES"),
            "the grill contract",
        );
        assert!(
            text.contains("the human validates the SPEC, you decide the TASKS"),
            "autonomous decompose rule",
        );
        assert!(
            text.contains("spec_create") && text.contains("Author a new Spec"),
            "the concrete create-the-Spec step, project-scoped",
        );
        // Anti-duplication scoped to the PROJECT, even with ZERO Specs.
        assert!(
            text.contains("Existing Specs in the project (anti-duplication)"),
            "project-scoped anti-duplication section",
        );
        assert!(
            text.contains("No Specs in this project yet"),
            "zero-Spec case is handled gracefully",
        );
        // The forbidden failure modes: no error, no workspace-anchor language.
        assert!(
            !text.to_lowercase().contains("anchored on"),
            "must NOT tell the user a Spec must be anchored on the workspace",
        );
        assert!(
            !text.to_lowercase().contains("could not resolve a spec"),
            "must NOT surface the resolve error for /nspec",
        );

        // The SAME project-scoped authoring resolves via workspace_id and via a nested cwd.
        let by_ws = agent_context(
            &db,
            &json!({ "role": "spec", "workspace_id": workspace_id }),
        )
        .unwrap();
        assert_eq!(by_ws.result["project_id"], project_id);
        assert!(by_ws.result["context"]
            .as_str()
            .unwrap()
            .contains("SPEC (authoring)"));

        let nested = format!("{cwd}/crates/api");
        let by_cwd = agent_context(&db, &json!({ "role": "spec", "cwd": nested })).unwrap();
        assert_eq!(by_cwd.result["project_id"], project_id);
        assert!(by_cwd.result["context"]
            .as_str()
            .unwrap()
            .contains("SPEC (authoring)"));
    }

    /// DONE-CRITERION (task 01KXZ9K3HZC5C1YF4HXVE1M77J: "/nspec déclenche idea_search au démarrage ;
    /// auto-link kind=addresses ; pas de doublon Spec"): the /nspec authoring context WIRES the idea
    /// backlog — it instructs idea_search at Spec start, auto-linking the resolved ideas as
    /// kind=addresses to avoid a duplicate Spec, AND surfaces the project's OPEN ideas up front.
    #[test]
    fn spec_authoring_wires_idea_search_and_surfaces_the_open_backlog() {
        let _g = db_guard();
        let (db, project_id, _ws, _cwd) = seeded_project_without_spec();
        // Seed an OPEN idea in the project's backlog + a dropped one (must not be surfaced).
        let open_idea = db
            .with_conn(|c| {
                db::create_idea(
                    c,
                    &project_id,
                    "Rework the auth token refresh",
                    "",
                    Some("auth"),
                    db::IDEA_SOURCE_SESSION,
                )
            })
            .unwrap();
        let dropped = db
            .with_conn(|c| {
                db::create_idea(
                    c,
                    &project_id,
                    "Abandoned thing",
                    "",
                    None,
                    db::IDEA_SOURCE_SESSION,
                )
            })
            .unwrap();
        db.with_conn(|c| db::close_idea(c, &dropped.id, db::IDEA_STATUS_DROPPED, None))
            .unwrap();

        let out = agent_context(&db, &json!({ "role": "spec", "project_id": project_id })).unwrap();
        assert!(out.effects.is_empty(), "authoring render is a pure read");
        let text = out.result["context"].as_str().unwrap();

        // The PROHIBITION (the role) — search first, link what the Spec resolves, no duplicate.
        assert!(
            text.contains("open a Spec for a need the project's backlog already tracks"),
            "the anti-duplication prohibition is in the preamble",
        );
        assert!(text.contains("idea_search"), "instructs idea_search");
        assert!(
            text.contains("kind=\"addresses\""),
            "link the resolved ideas as addresses",
        );
        assert!(
            text.to_lowercase().contains("duplicate spec"),
            "the anti-duplication goal is explicit",
        );

        // The concrete backlog surface: the OPEN idea is listed up front; the dropped one is not.
        assert!(
            text.contains("Existing ideas — search the backlog before you spec"),
            "the backlog section header",
        );
        // The render carries the one thing it alone can give — the call with THIS project's id.
        assert!(
            text.contains(&format!("idea_search(project_id=\"{project_id}\"")),
            "the project-scoped call is spelled out with the real id",
        );
        assert!(
            text.contains(&open_idea.id) && text.contains("Rework the auth token refresh"),
            "the open idea is surfaced up front",
        );
        assert!(
            !text.contains("Abandoned thing"),
            "a dropped idea is not surfaced in the open backlog",
        );
    }

    /// When the cwd matches no nyx workspace/project (or no anchor is given at all), /nspec
    /// still renders a GRACEFUL authoring manual that asks the human to select the project —
    /// NEVER a hard error.
    #[test]
    fn spec_role_without_a_resolvable_project_renders_graceful_manual_not_an_error() {
        let _g = db_guard();
        let db = Db::in_memory(); // empty: no project, no workspace.

        // A cwd matching nothing → graceful authoring manual, no project.
        let by_cwd =
            agent_context(&db, &json!({ "role": "spec", "cwd": "/nowhere/unknown" })).unwrap();
        assert!(by_cwd.result["project_id"].is_null(), "no project resolved");
        let text = by_cwd.result["context"].as_str().unwrap();
        assert!(
            text.contains("SPEC (authoring)"),
            "still the authoring manual"
        );
        assert!(text.contains("Spec Author"), "preamble present");
        assert!(
            text.to_lowercase().contains("select the project"),
            "asks the human to select the project",
        );

        // No anchor at all → still graceful, never invalid_id.
        let bare = agent_context(&db, &json!({ "role": "spec" })).unwrap();
        assert!(bare.result["context"]
            .as_str()
            .unwrap()
            .contains("SPEC (authoring)"));
    }

    /// The no-Spec fallbacks are TARGETED: the authoring manual is SPEC-only, and the standalone
    /// DISPATCHER render is DEV-only (N1). Any OTHER role (coder/auditor/doc) with no resolvable Spec
    /// keeps its unchanged `invalid_id` contract.
    #[test]
    fn other_roles_without_a_spec_still_error_invalid_id() {
        let _g = db_guard();
        let (db, _project_id, workspace_id, _cwd) = seeded_project_without_spec();
        let err = agent_context(&db, &json!({ "role": "coder", "workspace_id": workspace_id }))
            .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// REVIEW F1 — THE CASE 0 SPEAKS, on BOTH channels and through BOTH location anchors.
    ///
    /// The bug this pins: `agent_context(role=dev|spec)` on a location anchoring no Spec used to
    /// return `ok`, a full manual and `spec_id: null` with NOT ONE WORD about it — so a call made
    /// from the wrong folder was indistinguishable from a properly scoped one. The contract now: the
    /// render still succeeds (refusing would break /nspec, whose entry state is exactly this), and it
    /// SAYS SO twice — a `warnings` entry carrying the STABLE machine code, and one explicit line in
    /// the rendered text. The two carry the SAME sentence, asserted here, so a future edit cannot
    /// silence one and leave the other.
    ///
    /// Both `resolve_spec_id` location paths are exercised — an explicit `workspace_id` and a NESTED
    /// `cwd` — because the finding was reproduced through both.
    #[test]
    fn the_case_zero_speaks_on_both_channels_for_the_dispatcher_and_the_author() {
        let _g = db_guard();
        let (db, _project_id, workspace_id, cwd) = seeded_project_without_spec();
        let nested = format!("{cwd}/crates/api");
        for role in ["dev", "spec"] {
            for anchor in [
                json!({ "role": role, "workspace_id": workspace_id }),
                json!({ "role": role, "cwd": nested }),
            ] {
                let out = agent_context(&db, &anchor).unwrap_or_else(|e| {
                    panic!("case 0 is SIGNALLED, never refused ({anchor}): {e:?}")
                });
                assert!(out.effects.is_empty(), "still a pure read ({anchor})");
                assert!(
                    out.result["spec_id"].is_null(),
                    "no Spec resolved ({anchor})"
                );

                // CHANNEL 1 — the machine one. A non-empty array, with the stable code.
                let warnings = out.result["warnings"]
                    .as_array()
                    .unwrap_or_else(|| panic!("a warnings array ({anchor})"));
                assert_eq!(warnings.len(), 1, "exactly one diagnosis ({anchor})");
                assert_eq!(
                    warnings[0]["code"], NO_SPEC_ON_LOCATION_WARNING_CODE,
                    "the location RESOLVED a workspace; it simply anchors no active Spec ({anchor})",
                );
                let message = warnings[0]["message"].as_str().expect("a human sentence");
                assert!(
                    message.contains(&workspace_id),
                    "the sentence NAMES the workspace it looked at ({anchor}): {message}",
                );

                // CHANNEL 2 — the human one, INSIDE the manual an agent actually reads.
                let text = out.result["context"].as_str().expect("a rendered manual");
                assert!(
                    text.contains("NO SPEC ANCHORED"),
                    "the rendered context carries an explicit line ({anchor})",
                );
                assert!(
                    text.contains(message),
                    "…and it is the SAME sentence as the warning, never a second wording ({anchor})",
                );
            }
        }
    }

    /// THE UNBOUND-RUN DIAGNOSIS NAMES THE WAY OUT, and the way out EXISTS.
    ///
    /// This is the warning the observed session read and could do nothing with. It was exact and
    /// inert: it described a state ("this run drives no Spec") to an agent whose next instruction,
    /// in the very manual this line rides on, was to author a Spec from scratch — so the only
    /// documented move was the wrong one, and a duplicate of the Spec the human had named was
    /// ordered. What it lacked was the sentence saying the session is repairable.
    ///
    /// FOUR THINGS ARE PINNED, and the last is the one that keeps the advice honest:
    ///   * it stays a WARNING — `agent_context` still answers `Ok`, still renders the manual;
    ///   * it names the run, so the remedy can be pasted;
    ///   * it names the REMEDY verb, and by the verb's OWN constant — a re-typed literal would let
    ///     the verb be renamed while this sentence kept pointing at a tool that no longer exists;
    ///   * that verb is ADVERTISED in `tools/list`. An agent cannot call what `tools/list` omits, so
    ///     advice naming an unadvertised verb is exactly the half-remedy this task exists to end.
    #[test]
    fn the_unbound_run_warning_names_the_verb_that_repairs_the_session() {
        let _g = db_guard();
        let (db, project_id, _ws, _cwd) = seeded_project_without_spec();
        let run_id = db.with_conn(|c| {
            let wf =
                db::create_workflow(c, Some(&project_id), "authoring", "nspec", None).expect("wf");
            db::create_workflow_run(c, &wf.id, None, None, "{}")
                .expect("run")
                .id
        });

        let out = agent_context(&db, &json!({ "role": "dev", "run_id": run_id }))
            .expect("an unbound run is DIAGNOSED, never refused");
        let warning = &out.result["warnings"][0];
        assert_eq!(warning["code"], RUN_UNBOUND_WARNING_CODE);
        let message = warning["message"].as_str().expect("a human sentence");
        assert!(
            message.contains(&run_id),
            "the remedy carries the run's own id, ready to paste: {message}",
        );
        assert!(
            message.contains(crate::mcp_engine::RUN_BIND_SPEC_TOOL),
            "it names the verb that anchors this very run instead of only describing the state: \
             {message}",
        );
        // The manual an agent READS says the same thing — the two channels never drift.
        assert!(
            out.result["context"]
                .as_str()
                .expect("a manual")
                .contains(message),
            "the notice inside the manual is the SAME sentence as the warning",
        );
        // …and the verb it sends the caller to is one `tools/list` actually offers.
        assert!(
            crate::mcp::tool_descriptors()
                .iter()
                .any(|d| d["name"] == crate::mcp_engine::RUN_BIND_SPEC_TOOL),
            "the advice points at an ADVERTISED tool, not one an agent has no way to call",
        );
    }

    /// REVIEW F1, criterion 2 — THE CASE 0s DO NOT SAY THE SAME THING, so they do not share a code.
    ///
    ///   * a location that RESOLVED a workspace carrying no active Spec is a LEGITIMATE state (the
    ///     project about to author its first Spec);
    ///   * a location that resolved NOTHING — a cwd matching no workspace, or no location argument at
    ///     all — is an INVOCATION mistake, and the one the finding was filed on;
    ///   * a `run_id` naming an UNBOUND run is a third thing again: the location is perfect, the RUN
    ///     drives no Spec, and telling that caller to "pass a cwd" would name the wrong fix.
    ///
    /// A machine consumer (and the human the dispatcher relays to) must be able to tell them apart
    /// without parsing prose, so the assertion is on the CODES.
    #[test]
    fn the_three_case_zeros_are_told_apart_by_their_warning_code() {
        let _g = db_guard();
        let (db, project_id, workspace_id, _cwd) = seeded_project_without_spec();
        let code = |args: Value| -> String {
            agent_context(&db, &args)
                .unwrap_or_else(|e| panic!("case 0 is never refused for dev/spec: {e:?}"))
                .result["warnings"][0]["code"]
                .as_str()
                .expect("a machine code")
                .to_string()
        };

        let located = code(json!({ "role": "dev", "workspace_id": workspace_id }));
        let unlocated = code(json!({ "role": "dev", "cwd": "/nowhere/nyx-knows-nothing" }));
        assert_eq!(located, NO_SPEC_ON_LOCATION_WARNING_CODE);
        assert_eq!(unlocated, NO_LOCATION_WARNING_CODE);
        assert_ne!(
            located, unlocated,
            "a workspace with no Spec and a folder nyx never heard of are NOT the same diagnosis",
        );
        // No location argument at all is the SAME diagnosis as a cwd matching nothing…
        assert_eq!(code(json!({ "role": "spec" })), NO_LOCATION_WARNING_CODE);
        // …and an UNKNOWN workspace_id too: a location naming nothing is a location naming nothing,
        // whichever argument carried it.
        assert_eq!(
            code(json!({ "role": "dev", "workspace_id": "01no-such-workspace" })),
            NO_LOCATION_WARNING_CODE,
        );

        // THE RUN — resolved, and bound to no Spec (a /nspec that has not created one yet).
        let run_id = db.with_conn(|c| {
            let wf = db::create_workflow(c, Some(&project_id), "authoring", "nspec", None)
                .expect("workflow");
            db::create_workflow_run(c, &wf.id, None, None, "{}")
                .expect("run")
                .id
        });
        let unbound = code(json!({ "role": "dev", "run_id": run_id }));
        assert_eq!(unbound, RUN_UNBOUND_WARNING_CODE);
        assert_ne!(
            unbound, unlocated,
            "the run's location resolved fine — it is the RUN that drives no Spec",
        );
    }

    /// STANDARDS FINDING `01M0813X53D1NRPAXYJGG6RJAV` — THE DIAGNOSIS AND THE RESOLUTION READ ONE
    /// ORDER, AND IT IS OBSERVABLE FROM OUTSIDE.
    ///
    /// [`unanchored_signal`] used to re-walk its OWN copy of [`resolve_spec_id`]'s anchor order. Two
    /// independent ordered lists drift: insert an anchor or permute two on one side only, and the
    /// diagnosis keeps compiling while it names THE ARGUMENT THE CALLER DID NOT USE — a signal worse
    /// than the silence review F1 replaced. The order now lives once, in [`spec_anchor`].
    ///
    /// The structural half of that guarantee is the compiler's: both `match` blocks are exhaustive
    /// over [`SpecAnchor`], so a fifth anchor cannot be made resolvable without the build stopping
    /// at the diagnosis too. THIS test pins the other half — the order's actual VALUE — from the
    /// only place it is observable: every call below passes TWO anchors at once, chosen so that each
    /// would yield a DIFFERENT code alone, and asserts the code of the EARLIER one. Permuting
    /// `spec_anchor` reddens here.
    #[test]
    fn the_case_zero_diagnosis_names_the_anchor_the_resolution_stopped_on() {
        let _g = db_guard();
        let (db, project_id, workspace_id, cwd) = seeded_project_without_spec();
        let code = |args: Value| -> String {
            agent_context(&db, &args)
                .unwrap_or_else(|e| panic!("case 0 is never refused for dev/spec: {e:?}"))
                .result["warnings"][0]["code"]
                .as_str()
                .expect("a machine code")
                .to_string()
        };
        let unbound_run = db.with_conn(|c| {
            let wf = db::create_workflow(c, Some(&project_id), "authoring", "nspec", None)
                .expect("workflow");
            db::create_workflow_run(c, &wf.id, None, None, "{}")
                .expect("run")
                .id
        });

        // run_id BEFORE workspace_id, and BEFORE cwd. Both locations resolve a real workspace here,
        // so a diagnosis reading them first would answer `spec_none_on_location` — the caller would
        // be told to "pass an explicit spec_id" when what it actually holds is an authoring run.
        assert_eq!(
            code(json!({ "role": "dev", "run_id": unbound_run, "workspace_id": workspace_id })),
            RUN_UNBOUND_WARNING_CODE,
            "the run is read BEFORE the workspace_id, as the resolution reads it",
        );
        assert_eq!(
            code(json!({ "role": "dev", "run_id": unbound_run, "cwd": cwd })),
            RUN_UNBOUND_WARNING_CODE,
            "the run is read BEFORE the cwd, as the resolution reads it",
        );

        // workspace_id BEFORE cwd — asserted in BOTH directions, because a single direction cannot
        // tell a permutation from a coincidence. Real workspace + nowhere-cwd must answer on the
        // workspace; unknown workspace + real cwd must answer on the (unknown) workspace.
        assert_eq!(
            code(json!({
                "role": "dev",
                "workspace_id": workspace_id,
                "cwd": "/nowhere/nyx-knows-nothing",
            })),
            NO_SPEC_ON_LOCATION_WARNING_CODE,
            "the workspace_id answers; the cwd behind it is never consulted",
        );
        assert_eq!(
            code(json!({ "role": "dev", "workspace_id": "01no-such-workspace", "cwd": cwd })),
            NO_LOCATION_WARNING_CODE,
            "the workspace_id STILL answers even when it names nothing — the cwd does not rescue it",
        );

        // …and the head of the order: an explicit spec_id stops the walk before any location, so a
        // case-0 diagnosis is never reached at all — an unknown id is `invalid_id` at the render.
        let err = agent_context(
            &db,
            &json!({ "role": "dev", "spec_id": "01no-such-spec", "cwd": cwd }),
        )
        .expect_err("an explicit spec_id is the first anchor, and an unknown one is refused");
        assert_eq!(
            err.code,
            RpcCode::InvalidId,
            "spec_id precedes every location: no location gets to answer in its place",
        );
    }

    /// REVIEW F1, criterion 3 — SIGNALLING THE 0 CHANGED NOTHING ABOUT THE 1 AND THE N.
    ///
    /// The finding asked for a WARNING on the case 0, explicitly NOT for a symmetric refusal, and
    /// explicitly not at the cost of the branches that already behave. So this pins the whole
    /// contract around the change: the four Spec-scoped roles still REFUSE the case 0 with
    /// `invalid_id`, the N case still REFUSES with the machine reason `spec_ambiguous_for_workspace`
    /// (a warning there would be a regression, not an improvement), and a resolved Spec — the case 1
    /// — carries an EMPTY `warnings` array rather than no key at all.
    #[test]
    fn signalling_the_case_zero_left_the_other_branches_untouched() {
        let _g = db_guard();
        let (db, project_id, workspace_id, cwd) = seeded_project_without_spec();

        // 0 — the four Spec-scoped roles are NOT part of the arbitration: they still refuse.
        for role in ["coder", "auditor", "doc", "prototype"] {
            for anchor in [
                json!({ "role": role, "workspace_id": workspace_id }),
                json!({ "role": role, "cwd": cwd }),
            ] {
                let err = agent_context(&db, &anchor)
                    .expect_err("a Spec-scoped role without a Spec is refused, not warned");
                assert_eq!(err.code, RpcCode::InvalidId, "role {role} ({anchor})");
            }
        }

        // N — two active Specs on that same workspace: still a REFUSAL carrying the machine reason.
        let anchor_spec = |title: &str| {
            db.with_conn(|c| {
                db::create_spec_with_workspace(
                    c,
                    &project_id,
                    &workspace_id,
                    title,
                    db::SpecSections::empty(),
                )
                .expect("spec")
                .0
                .id
            })
        };
        let _first = anchor_spec("First");
        let _second = anchor_spec("Second");
        let err = agent_context(&db, &json!({ "role": "dev", "workspace_id": workspace_id }))
            .expect_err("N candidates are refused, never warned-and-rendered");
        assert!(
            is_ambiguous_spec(&err),
            "the N refusal keeps its machine handle: {err:?}",
        );

        // 1 — a resolved Spec answers with an EMPTY warnings array (the key is unconditional, so a
        // consumer never has to branch on its presence).
        let (db1, spec_id, ws1, _cwd1) = seeded();
        let ok = agent_context(&db1, &json!({ "role": "coder", "workspace_id": ws1 })).unwrap();
        assert_eq!(ok.result["spec_id"], spec_id);
        assert_eq!(
            ok.result["warnings"],
            json!([]),
            "a scoped render warns about nothing — and still publishes the key",
        );
    }

    /// THE PHASE-PLAN CONTRACT MOVED TO THE TOOL THAT TAKES IT. `task_add(phase="<name>")` and the
    /// first-seen ordering are the `task_add` DESCRIPTOR's business — an agent gets it from
    /// `tools/list` whether or not it ever renders a manual — so the spec preamble must not teach it
    /// a second time. The positive half is
    /// `mcp::tests::the_task_add_descriptor_teaches_the_phase_plan_emerges_from_the_names`.
    #[test]
    fn spec_preamble_does_not_re_teach_the_task_add_phase_contract() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, &spec_id, None)
            .unwrap()
            .unwrap();
        for gone in ["task_add(phase=", "first-seen", "spec_set_phases", "1..N"] {
            assert!(
                !text.contains(gone),
                "`{gone}` is the task_add descriptor's contract, not the manual's",
            );
        }
    }

    /// Review finding #35: the node inspector must show the STATIC role template even with NO run.
    /// `render_effector_context(agent_id, None)` returns the role MANUAL (the sub-agent's brief) for
    /// a valid role — needing no Spec — with the run-specific sections marked "resolved at run" and
    /// the `{{rules}}` marker DEFERRED (its scopes named, its directives bound per run).
    #[test]
    fn effector_context_without_a_spec_renders_the_static_role_template() {
        let _g = db_guard();
        let db = Db::in_memory(); // NO Spec anywhere — the static template needs none.

        let text = render_effector_context(&db, "coder", None, None)
            .unwrap()
            .unwrap();

        // The STATIC role manual (the sub-agent's brief) is present cold.
        assert!(
            text.contains("Context: Coder Agent"),
            "the role manual is rendered without a run",
        );
        assert!(
            text.contains("(role template)"),
            "the header marks it a static template",
        );
        // The run-specific sections are EXPLICIT "resolved at run" placeholders, never required.
        assert!(
            text.contains("Spec    : (resolved at run"),
            "the Spec header is deferred",
        );
        assert!(
            text.contains("Current phase — (resolved at run)"),
            "the current phase is deferred",
        );
        assert!(
            text.contains("Workspace & repos — (resolved at run)"),
            "the workspace/repos are deferred",
        );
        // The `{{rules}}` marker is CONSUMED into a deferred block that names its scopes — no stray
        // marker, no live directives (those bind per run).
        assert!(
            !text.contains(RULES_MARKER),
            "the rules marker is resolved (deferred)",
        );
        assert!(
            text.contains("Resolved at run by scope (always + role:coder)"),
            "the deferred rules block names the scopes it will resolve",
        );
    }

    /// The static template is available for EVERY renderable role; the auditor's states the
    /// commitment mandate cold (the SECTION it examines is named per spawn, not in the manual), and
    /// an unknown role is still `invalid_argument`.
    #[test]
    fn effector_template_covers_each_role_and_rejects_an_unknown_one() {
        let _g = db_guard();
        let db = Db::in_memory();
        for (agent_id, needle) in [
            ("spec", "Context: Spec Author"),
            ("auditor", "Context: Auditor Agent"),
            ("doc", "Context: Doc-sync"),
        ] {
            let text = render_effector_context(&db, agent_id, None, None)
                .unwrap()
                .unwrap();
            assert!(
                text.contains(needle),
                "{agent_id}: role manual present cold"
            );
            assert!(
                text.contains("(role template)"),
                "{agent_id}: static-template header",
            );
        }
        // The auditor template states its mandate cold: ONE commitment, named by the spawn's subject.
        let auditor = render_effector_context(&db, "auditor", None, None)
            .unwrap()
            .unwrap();
        assert!(
            auditor.contains("ONE COMMITMENT") && auditor.contains("`subject`"),
            "the auditor template carries the commitment mandate without a run",
        );
        // An unknown role is rejected (the inspector falls back to the node config).
        assert_eq!(
            render_effector_context(&db, "bogus", None, None)
                .unwrap_err()
                .code,
            RpcCode::InvalidArgument,
        );
    }

    /// When a run IS bound (a Spec resolves), the effector context is the LIVE enriched manual, not
    /// the deferred template — the run-specific sections carry REAL values (finding #35: "enrichi si
    /// un run est lié"). A pure passthrough to [`render_agent_context`].
    #[test]
    fn effector_context_with_a_spec_renders_the_live_enriched_manual() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_effector_context(&db, "coder", Some(&spec_id), None)
            .unwrap()
            .unwrap();
        assert!(text.contains("Context: Coder Agent"), "role manual present");
        assert!(
            !text.contains("(role template)"),
            "not the static-template header when a run is bound",
        );
        assert!(
            text.contains("Current phase — 1 · Foundation"),
            "the LIVE current phase, not a placeholder",
        );
        assert!(
            !text.contains("Current phase — (resolved at run)"),
            "no deferred placeholder when a run is bound",
        );
    }

    // ========================================================================
    // F6 — the Spec is not copied WHOLE into a pass that audits ONE section
    // ========================================================================

    /// A Spec whose SIX sections carry a distinct, recognisable body — so a render can be checked
    /// section by section. Returns `(db, spec_id)`.
    fn seeded_six_sections() -> (Db, String) {
        let db = Db::in_memory();
        let spec_id = db.with_conn(|c| {
            let (project, root) =
                db::create_project(c, "Proj", "/tmp/nyx-sections", None).expect("project");
            let (spec, _rev) = db::create_spec_with_workspace(
                c,
                &project.id,
                &root.id,
                "My Feature",
                db::SpecSections {
                    problem: "BODY-PROBLEM".into(),
                    solution: "BODY-SOLUTION".into(),
                    implementation_decisions: "BODY-IMPL".into(),
                    testing_decisions: "BODY-TESTING".into(),
                    user_stories: "BODY-STORIES".into(),
                    out_of_scope: "BODY-SCOPE".into(),
                },
            )
            .expect("spec");
            spec.id
        });
        (db, spec_id)
    }

    /// F6 — an audit PASS is spawned with a `subject` naming ONE Spec commitment and audits that
    /// one only ("that section is reproduced below", says its own manual). The render carries THAT
    /// section and NOT the other five: copying the whole Spec into each of the four passes of an
    /// audit cycle paid for the same text four times, and three of those copies were for sections
    /// the pass is explicitly told are not its business.
    #[test]
    fn a_subject_naming_a_spec_section_narrows_the_render_to_it() {
        let _g = db_guard();
        let (db, spec_id) = seeded_six_sections();
        let text = render_effector_context(&db, "auditor", Some(&spec_id), Some("user_stories"))
            .unwrap()
            .unwrap();
        assert!(text.contains("BODY-STORIES"), "the subject's own section");
        for other in [
            "BODY-PROBLEM",
            "BODY-SOLUTION",
            "BODY-IMPL",
            "BODY-TESTING",
            "BODY-SCOPE",
        ] {
            assert!(
                !text.contains(other),
                "{other} is another pass's business — it must not be copied in",
            );
        }
        // The identifying header stays (the Spec is still named), and the narrowing is STATED, so
        // the sub-agent never reads the missing sections as an empty Spec.
        assert!(text.contains("## Spec — My Feature"), "Spec header kept");
        assert!(
            text.contains("only that section is reproduced here, on purpose"),
            "the render says it is narrowed, and how to read the rest",
        );
        // The ROLE manual is untouched by the narrowing.
        assert!(text.contains("Context: Auditor Agent"), "role manual kept");
    }

    /// Every one of the six section slugs narrows to its OWN body — the mapping is not a special
    /// case for the four commitments the ndev audit walks.
    #[test]
    fn each_spec_section_slug_narrows_to_its_own_body() {
        let _g = db_guard();
        let (db, spec_id) = seeded_six_sections();
        for (subject, body) in [
            ("problem", "BODY-PROBLEM"),
            ("solution", "BODY-SOLUTION"),
            ("implementation_decisions", "BODY-IMPL"),
            ("testing_decisions", "BODY-TESTING"),
            ("user_stories", "BODY-STORIES"),
            ("out_of_scope", "BODY-SCOPE"),
            // Tolerated spellings of the same slug (a hand-authored node, not the seed).
            ("  Out-Of-Scope ", "BODY-SCOPE"),
        ] {
            let text = render_effector_context(&db, "auditor", Some(&spec_id), Some(subject))
                .unwrap()
                .unwrap();
            assert!(
                text.contains(body),
                "subject '{subject}' must carry {body}",
            );
        }
    }

    /// A spawn with NO subject is UNCHANGED: the whole Spec, as before. And so is a subject that
    /// names no section (a free-form focus — a file, a component, a question): narrowing on a guess
    /// would hide the Spec from a spawn that never asked for a section.
    #[test]
    fn no_subject_or_a_non_section_subject_keeps_the_whole_spec() {
        let _g = db_guard();
        let (db, spec_id) = seeded_six_sections();
        for subject in [None, Some("the login page"), Some("")] {
            let text = render_effector_context(&db, "coder", Some(&spec_id), subject)
                .unwrap()
                .unwrap();
            for body in [
                "BODY-PROBLEM",
                "BODY-SOLUTION",
                "BODY-IMPL",
                "BODY-TESTING",
                "BODY-STORIES",
                "BODY-SCOPE",
            ] {
                assert!(text.contains(body), "subject {subject:?} keeps {body}");
            }
            assert!(
                !text.contains("only that section is reproduced here, on purpose"),
                "subject {subject:?} narrows nothing, so it announces nothing",
            );
        }
    }

    /// The `agent_context` verb takes the SAME `subject` — this is the escape hatch a dispatcher
    /// uses to RE-FETCH the manual a spawn action handed it (F6 part A's `context_already_sent`).
    /// What it returns must be the very text the spawn carried.
    #[test]
    fn agent_context_subject_reproduces_the_manual_the_spawn_carried() {
        let _g = db_guard();
        let (db, spec_id) = seeded_six_sections();
        let forwarded =
            render_effector_context(&db, "auditor", Some(&spec_id), Some("out_of_scope"))
                .unwrap()
                .unwrap();
        let refetched = agent_context(
            &db,
            &json!({ "role": "auditor", "spec_id": spec_id, "subject": "out_of_scope" }),
        )
        .unwrap();
        assert_eq!(
            refetched.result["context"].as_str().unwrap(),
            forwarded,
            "the re-fetch returns the manual the spawn forwarded, narrowing included",
        );
    }

    // ========================================================================
    // The SIZE of a role render is a measured budget, not a guess
    // ========================================================================

    /// The UPPER BOUNDS, in characters, of what a role render costs. Taken from the numbers a REAL
    /// render produces on [`seeded_realistic`] (a Spec with six written sections and ten tasks —
    /// the shape of the run that ran out of window), with room to breathe: this is a regression
    /// guard, not a straitjacket.
    ///
    ///   * ROLE MANUAL — the STATIC preamble, the only part a manual edit can inflate. Measured
    ///     after the dissolution: prototype 1 638, spec 2 019, auditor 1 603, coder 1 230, doc 530.
    ///     The bound is per role and deliberately tight: the whole point of the item is that a role
    ///     manual is a preamble and a list of prohibitions, so a manual that grows past 2 500 has
    ///     started narrating a process again.
    ///   * PROTOTYPE RENDER, measured 17 317 — the design procedure it used to carry now lives in
    ///     the `design_*` tool descriptors. What is LEFT is almost entirely the Spec's own six
    ///     sections, which is what a render is supposed to be.
    ///   * AUDITOR PASS, measured 5 478 – 6 987 depending on the section (whole-Spec: 17 607) — the
    ///     `subject` narrowing, which survives as a parameter of the `agent_context` call the
    ///     sub-agent now makes itself.
    ///
    /// THE CYCLE BOUND WAS RAISED, DELIBERATELY, and this is the record of it. The identity header
    /// gained the Spec's own `spec_id` (` · spec_id=<uuid>`, 48 bytes on the `Spec    :` line) and a
    /// `Run     :` BLOCK saying what drives the Spec — the whole subject of the PRD that added it
    /// (the agent that lacked it spent four MCP calls fetching what the render already knew).
    ///
    /// THAT BLOCK IS NOT A FIXED-SIZE LINE, and treating it as one is how a bound gets set on a
    /// render nobody performs. It is ONE LINE PER LIVE RUN — `render_agent_context` emits it for
    /// EVERY live run, top-level and `sub_workflow` child alike — so it grows with the run COUNT,
    /// and each line grows with what it has to say: the label of the node it is parked on, the
    /// ` · a sub_workflow CHILD …` lineage (+64 bytes), the ` — STALE: …` sentence (+104). Its
    /// CHEAPEST shape is the one an audit cycle never has — no live run at all, the 110 bytes of
    /// "(none live …)".
    ///
    /// SO THERE ARE TWO BOUNDS, BECAUSE ONE NUMBER CANNOT BE BOTH. A single figure asked to be a
    /// tight regression guard AND a capacity bound on a render whose size follows STATE is neither:
    /// set tight it fails on the second live run, set wide it stops catching the inflation it exists
    /// for. They are separated here.
    ///
    /// EVERY LINE FIGURE BELOW INCLUDES ITS TRAILING NEWLINE — that is what the render pays for a
    /// line, and one convention throughout is what lets the two budgets be compared at all.
    ///
    ///   * TIGHT, ON ONE LIVE RUN — [`AUDIT_CYCLE_CONTEXT_MAX`] / [`AUDIT_PASS_CONTEXT_MAX`].
    ///     [`seeded_realistic`] renders the shape the cycle ACTUALLY has: a live `ndev` run parked
    ///     on an audit node, which the test asserts BY ITS ID in every pass, so these numbers cannot
    ///     quietly go back to pricing the empty case. MEASURED THERE: the run line is 199 bytes
    ///     against the 110 of "(none live …)", and the four-pass cycle is 24 481. The SAME cycle on
    ///     an undriven Spec is 24 481 − 4 × (199 − 110) = 24 125, so a bound sized on THAT shape and
    ///     given a few hundred bytes of slack sits UNDER the shape the cycle actually has. The bound
    ///     is 25 000 — 519 over the driven number, about 130 per pass. What that catches is a manual
    ///     or a section growing under it; what it does NOT do is bound the block, and it never
    ///     could: a SECOND live run alone carries the cycle past 25 000.
    ///   * AT THE CAP — [`AUDIT_CYCLE_AT_RUN_CAP_MAX`] / [`AUDIT_PASS_AT_RUN_CAP_MAX`], measured on
    ///     [`seeded_realistic_at_run_cap`]: the block at [`SPEC_RUN_LINES_MAX`] lines plus its
    ///     counting line, EVERY listed line carrying EVERY optional clause the block has (STALE,
    ///     +104; the `sub_workflow` lineage, +64) and the dearest node label nyx itself ships
    ///     ([`LONGEST_SEEDED_NODE_LABEL`], 61). MEASURED THERE: a driver line of 395 bytes, two
    ///     child lines of 405, a counting line of 355, a pass of 6 839 – 8 348 — over
    ///     [`AUDIT_PASS_CONTEXT_MAX`] on its own, which is the whole reason these are two budgets —
    ///     and a cycle of 29 925. The bounds are 8 500 (152 of slack) and 30 500 (575, about 144 per
    ///     pass — deliberately no tighter per pass than the bound above, so the wider fixture is not
    ///     the more sensitive of the two).
    ///
    /// WHAT 30 500 IS, EXACTLY, SO IT IS NOT READ AS MORE. It is what THIS render costs on THAT
    /// fixture, and nothing else: a lock against a clause added to a run line, against
    /// [`SPEC_RUN_LINES_MAX`] being lifted, against a manual or a section inflating underneath. A
    /// new clause on the listed lines costs 12 bytes per character of it across the cycle (3 lines ×
    /// 4 passes), so anything longer than ~48 characters goes red here, and the clauses that exist
    /// are 64 and 104; a cap of 4 adds a whole ~405-byte line to every pass.
    ///
    /// IT IS NOT A CAPACITY BOUND ON THE STATE, AND NO NUMBER HERE COULD BE. What the cap bounds is
    /// the LINE COUNT, not the bytes, and two of the things a line prints are FREE-FORM USER DATA
    /// that this fixture can only pick a plausible value for:
    ///
    ///   * THE FLOW NAME — `workflow_create` takes an arbitrary `name`. The fixture's are `ndev` (4)
    ///     and `child-flow-N` (12), and each extra character costs 12 across the cycle, as above.
    ///     MEASURED: the same fixture with a 38-character name on all three flows renders a 30 269
    ///     cycle; at 58 it passes 30 500.
    ///   * THE NODE LABEL past 61 — `node_create` takes an arbitrary label too, and 61 is only where
    ///     nyx's OWN seeded graphs top out. Same rate.
    ///
    /// So a red here means the CODE grew, not that a user did something unusual — and a Spec whose
    /// flows are named at length can exceed 30 500 with nothing wrong. That is the honest reading,
    /// and it is why the block is capped in LINES: the bytes have no bound to assert.
    ///
    /// Every figure in THIS comment is one of three things: a size a render in this file produces
    /// TODAY, a bound declared just below, or an arithmetic step shown with its terms. None of them
    /// is a remembered "before".
    ///
    /// WHAT THE GUARD STILL GUARDS IS INTACT: [`ROLE_MANUAL_MAX`] and [`AUDIT_PASS_CONTEXT_MAX`]
    /// were NOT re-widened and still pass, so no manual re-inflated and no pass was re-widened —
    /// which is what this test was written to catch. A header that grows again shows up here.
    const ROLE_MANUAL_MAX: usize = 2_500;
    const PROTOTYPE_CONTEXT_MAX: usize = 25_000;
    const AUDIT_PASS_CONTEXT_MAX: usize = 8_000;
    const AUDIT_CYCLE_CONTEXT_MAX: usize = 25_000;
    const AUDIT_PASS_AT_RUN_CAP_MAX: usize = 8_500;
    const AUDIT_CYCLE_AT_RUN_CAP_MAX: usize = 30_500;

    /// A Spec of REALISTIC weight: six written sections, ten tasks — and A LIVE `ndev` RUN PARKED ON
    /// THE AUDIT NODE, which is what the F6 measurements are taken on. Returns
    /// `(db, spec_id, run_id)` — the run's id because the assertions below check that the render
    /// states THIS run, not merely that some `Run     :` line is non-empty.
    ///
    /// THE LIVE RUN IS NOT DECORATION. An ndev audit cycle is BY CONSTRUCTION driven by a live run
    /// bound to the Spec — the four passes below are the four `spawn_subagent` children of that
    /// run's own `audit` sequence, and the `Run     :` block states it in every one of them. A
    /// fixture with no run measures the ONE shape the cycle never has, and prices the header at its
    /// cheapest line ("(none live)") instead of the line the cycle actually pays for.
    fn seeded_realistic() -> (Db, String, String) {
        let db = Db::in_memory();
        let body = |label: &str, lines: usize| {
            (0..lines)
                .map(|i| {
                    format!(
                        "- {label} commitment #{i}: the build owes this, stated in a sentence long \
                         enough to be realistic prose.\n"
                    )
                })
                .collect::<String>()
        };
        let (spec_id, run_id) = db.with_conn(|c| {
            let (project, root) =
                db::create_project(c, "Proj", "/tmp/nyx-ctx-size", None).expect("project");
            db::create_repository(c, &project.id, "api", ".", false).expect("repo");
            let (spec, _rev) = db::create_spec_with_workspace(
                c,
                &project.id,
                &root.id,
                "My Feature",
                db::SpecSections {
                    problem: body("problem", 20),
                    solution: body("solution", 30),
                    implementation_decisions: body("impl", 30),
                    testing_decisions: body("testing", 20),
                    user_stories: body("story", 20),
                    out_of_scope: body("scope", 15),
                },
            )
            .expect("spec");
            for i in 0..10 {
                db::create_task(
                    c,
                    &project.id,
                    &format!("Task {i}"),
                    db::NewTaskInput {
                        spec_id: Some(spec.id.clone()),
                        phase_name: Some("Foundation".to_string()),
                        ..db::NewTaskInput::slice()
                    },
                )
                .expect("task");
            }
            // The run that is driving it, parked where an audit cycle parks: the `audit` sequence's
            // own spawn node, whose labels the built-in ndev tree writes as "audit <section>".
            let wf =
                db::create_workflow(c, Some(&project.id), "ndev", db::WORKFLOW_SCOPE_NDEV, None)
                    .expect("workflow");
            let node = db::create_node(
                c,
                &wf.id,
                None,
                0,
                db::NODE_TYPE_SPAWN_SUBAGENT,
                "audit testing decisions",
                "{}",
            )
            .expect("node");
            let run = db::create_workflow_run(c, &wf.id, Some(&spec.id), Some(&node.id), "{}")
                .expect("run");
            (spec.id, run.id)
        });
        (db, spec_id, run_id)
    }

    /// The LONGEST node label any of nyx's OWN seeded graphs carries (`seed.rs`, the `defer` branch
    /// of `escalate_blocked_task`, 61 chars) — copied here because a line's width follows the label
    /// of the node its run is parked on, and a worst case priced on a short label is not one.
    ///
    /// A COPY, and knowingly so: it is not read from the seeder, so a LONGER built-in label added
    /// later would not travel here. That is one of the un-priced components the budget's own doc
    /// names, and the slack it carries is sized for them.
    const LONGEST_SEEDED_NODE_LABEL: &str = "record this blocked task as a backlog idea linked to \
                                             the Spec";

    /// THE SAME SPEC WITH ITS `Run     :` BLOCK AT THE CAP — the fixture the capacity bound is
    /// priced on. `the_run_block_lists_the_moving_runs_and_counts_the_rest` renders that shape too
    /// and asserts its FORM (which runs are listed, which are counted); this one is where its SIZE
    /// is asserted, which is why it is built to be dear rather than merely to be at the cap.
    ///
    /// IT IS BUILT TO BE EXPENSIVE, not to be typical: a capacity bound prices what the render CAN
    /// print, so EVERY listed line here carries EVERY optional clause the block has and the dearest
    /// label nyx itself ships. All the runs are STALE (+104 each — a stranded ndev flow is exactly
    /// what `stranded_run_warnings` reports); ALL of them, the driver included, are `sub_workflow`
    /// CHILDREN (+64 each); all are parked on a node wearing [`LONGEST_SEEDED_NODE_LABEL`]; and
    /// there are two more than the cap so the counting line is rendered too.
    ///
    /// THE DRIVER IS A CHILD TOO, which is the correction that made this fixture a worst case: line
    /// 0 was assumed to be a top-level flow, and a top-level flow cannot have the lineage clause —
    /// but nothing makes line 0 top-level. The block is ordered by LAST ACTIVITY, so its head is
    /// whichever live run moved most recently, and a `sub_workflow` child whose parent has finished
    /// is exactly that: the parent is terminal, so [`spec_live_runs`] skips it and it costs no line,
    /// while the child still carries `parent_run_id` and pays the +64.
    ///
    /// THE DRIVER IS STILL FIRST. It is the least stale of them, so the cap keeps it: the fixture
    /// prices the worst case WITHOUT quietly dropping the run the header exists to name.
    fn seeded_realistic_at_run_cap() -> (Db, String, String) {
        use diesel::prelude::*;
        let (db, spec_id, run_id) = seeded_realistic();
        let day = 24 * 60 * 60 * 1000;
        let stale_at = |days: i64| {
            db::now_millis() - crate::mcp_engine::STRANDED_BLOCKED_RUN_AFTER_MS - days * day
        };
        db.with_conn(|c| {
            let project_id = db::get_spec(c, &spec_id).unwrap().unwrap().project_id;
            // The driver, stranded — listed first because it is the least stale of the set — and a
            // CHILD like the rest, so no listed line is cheaper than any other. Its parent is a run
            // that has FINISHED: `spec_live_runs` skips terminal runs, so the parent costs no line
            // while the driver keeps its `parent_run_id` and pays the lineage clause. (A DELETED
            // parent would not do: the self-FK is ON DELETE SET NULL, v43 — an orphan of a deleted
            // parent renders as top-level.)
            let outer = db::create_workflow(
                c,
                Some(&project_id),
                "outer",
                db::WORKFLOW_SCOPE_CUSTOM,
                None,
            )
            .expect("outer workflow");
            let parent = db::create_workflow_run(c, &outer.id, Some(&spec_id), None, "{}")
                .expect("parent run");
            diesel::update(crate::schema::workflow_runs::table.find(&parent.id))
                .set(crate::schema::workflow_runs::status.eq(db::RUN_STATUS_DONE))
                .execute(c)
                .expect("finish the parent");
            diesel::update(crate::schema::workflow_runs::table.find(&run_id))
                .set((
                    crate::schema::workflow_runs::parent_run_id.eq(&parent.id),
                    crate::schema::workflow_runs::updated_at.eq(stale_at(0)),
                ))
                .execute(c)
                .expect("strand the driver");
            // …and the driver's own node wears the dearest label a SEEDED graph carries, like the
            // children below.
            let driver_node = db::get_workflow_run(c, &run_id)
                .unwrap()
                .unwrap()
                .current_node_id
                .expect("the driver is parked");
            diesel::update(crate::schema::nodes::table.find(&driver_node))
                .set(crate::schema::nodes::label.eq(LONGEST_SEEDED_NODE_LABEL))
                .execute(c)
                .expect("relabel the driver's node");
            for i in 0..(SPEC_RUN_LINES_MAX + 1) {
                let wf = db::create_workflow(
                    c,
                    Some(&project_id),
                    &format!("child-flow-{i}"),
                    db::WORKFLOW_SCOPE_CUSTOM,
                    None,
                )
                .expect("workflow");
                let node = db::create_node(
                    c,
                    &wf.id,
                    None,
                    0,
                    db::NODE_TYPE_SPAWN_SUBAGENT,
                    LONGEST_SEEDED_NODE_LABEL,
                    "{}",
                )
                .expect("node");
                let child =
                    db::create_workflow_run(c, &wf.id, Some(&spec_id), Some(&node.id), "{}")
                        .expect("run");
                diesel::update(crate::schema::workflow_runs::table.find(&child.id))
                    .set((
                        crate::schema::workflow_runs::parent_run_id.eq(&run_id),
                        crate::schema::workflow_runs::updated_at.eq(stale_at(i as i64 + 1)),
                    ))
                    .execute(c)
                    .expect("age the child");
            }
        });
        (db, spec_id, run_id)
    }

    /// THE RENDER HAS A MEASURED BUDGET. A single tick returning a spawn used to ship 73 547
    /// characters, 71 798 of them the forwarded manual (~18k tokens); one ndev audit cycle paid that
    /// four times over and the dispatcher's window gave out. The manual no longer travels at all —
    /// but it is still RENDERED, on demand, by the sub-agent's own `agent_context` call, so the
    /// sizes are asserted rather than assumed: an edit that re-inflates a role manual, or re-widens
    /// what a pass is given, fails here rather than in a live run.
    #[test]
    fn a_role_render_stays_inside_its_measured_budget() {
        let _g = db_guard();
        let (db, spec_id, run_id) = seeded_realistic();

        // EVERY role manual is a preamble + prohibitions. This is the guard against a process
        // creeping back into one of them.
        for role in [
            AgentRole::Spec,
            AgentRole::Coder,
            AgentRole::Auditor,
            AgentRole::Doc,
            AgentRole::Prototype,
        ] {
            let manual = role.preamble();
            assert!(
                manual.len() <= ROLE_MANUAL_MAX,
                "the {} manual is {} chars, over the {ROLE_MANUAL_MAX} budget — a role manual says \
                 what the agent is and what it may NOT do; the process is the graph's",
                role.slug(),
                manual.len(),
            );
        }

        let prototype = render_effector_context(&db, "prototype", Some(&spec_id), None)
            .unwrap()
            .unwrap();
        assert!(
            prototype.len() <= PROTOTYPE_CONTEXT_MAX,
            "the prototype context is {} chars, over the {PROTOTYPE_CONTEXT_MAX} budget",
            prototype.len(),
        );

        // The FOUR ndev audit passes — each is ONE Spec commitment, and each is bounded on its own.
        let mut cycle = 0usize;
        for subject in [
            "user_stories",
            "testing_decisions",
            "implementation_decisions",
            "out_of_scope",
        ] {
            let pass = render_effector_context(&db, "auditor", Some(&spec_id), Some(subject))
                .unwrap()
                .unwrap();
            assert!(
                pass.len() <= AUDIT_PASS_CONTEXT_MAX,
                "the '{subject}' audit pass is {} chars, over the {AUDIT_PASS_CONTEXT_MAX} budget",
                pass.len(),
            );
            // THE BUDGET IS PRICED ON THE DRIVEN SHAPE. An audit cycle is run BY a live run bound to
            // the Spec, so every pass carries THAT run's `Run     :` line — asserted by the run's
            // own id, not by "some line that is not the empty phrase", which the COUNTING line
            // satisfies too. A fixture that lost the run would otherwise silently re-measure the
            // whole budget on the one shape the cycle never has, at the cheapest line the header
            // can print.
            assert!(
                pass.lines()
                    .any(|l| l.starts_with("Run     : ") && l.contains(run_id.as_str())),
                "the '{subject}' pass must state the live run {run_id} driving the Spec — that \
                 line is part of what this cycle costs:\n{pass}",
            );
            cycle += pass.len();
        }
        assert!(
            cycle <= AUDIT_CYCLE_CONTEXT_MAX,
            "a 4-pass audit cycle is {cycle} chars, over the {AUDIT_CYCLE_CONTEXT_MAX} budget",
        );

        // And the narrowing is what buys it: the same cycle unnarrowed is several times bigger.
        let whole = render_effector_context(&db, "auditor", Some(&spec_id), None)
            .unwrap()
            .unwrap();
        assert!(
            cycle < whole.len() * 2,
            "four narrowed passes ({cycle}) must cost less than two whole-Spec ones ({})",
            whole.len() * 2,
        );
    }

    /// WHAT MAKES [`SPEC_RUN_LINES_MAX`] A BOUND AND NOT A DECORATION. The budget above is priced on
    /// ONE live run, which is what a healthy cycle has and what makes that number tight enough to
    /// catch a manual or a section inflating under it — but a tight number on a render whose size
    /// follows STATE bounds nothing: a SECOND live run alone carries the same cycle past it. The cap
    /// is what turns "one line per live run" back into a finite quantity, and a cap nothing renders
    /// is a claim, so here it is rendered.
    ///
    /// AT THE CAP, IN THE DEAREST SHAPE THE BLOCK HAS ([`seeded_realistic_at_run_cap`]): the three
    /// listed lines all STALE, all three `sub_workflow` children, all three on the longest label a
    /// seeded graph carries, and the counting line under them. Whatever a Spec accumulates past
    /// that, the block does not grow by a LINE — which is the property this asserts.
    ///
    /// THE NUMBER IS NOT "WHAT AN AUDIT CYCLE CAN COST". It is what this render costs on THIS
    /// fixture: two of the things a line prints (the flow name, the node label) are free-form user
    /// data with no ceiling, so no byte figure bounds the state — see the budget's own doc for the
    /// rate and the measurements. What a red here means is that the CODE grew: the cap was lifted, a
    /// line was given a new clause, or a manual or a section inflated underneath.
    #[test]
    fn the_run_block_at_its_cap_stays_inside_its_own_budget() {
        let _g = db_guard();
        let (db, spec_id, run_id) = seeded_realistic_at_run_cap();

        let mut cycle = 0usize;
        for subject in [
            "user_stories",
            "testing_decisions",
            "implementation_decisions",
            "out_of_scope",
        ] {
            let pass = render_effector_context(&db, "auditor", Some(&spec_id), Some(subject))
                .unwrap()
                .unwrap();
            // THE FIXTURE IS THE ONE CLAIMED, checked on the render rather than on the seeding: the
            // block is AT the cap (its lines plus the counting one), it still names the driver, and
            // the expensive sentences are actually printed. A fixture that quietly stopped being
            // the worst case would leave this budget priced on something cheaper.
            let block: Vec<&str> = pass
                .lines()
                .filter(|l| l.starts_with("Run     : "))
                .collect();
            assert_eq!(
                block.len(),
                SPEC_RUN_LINES_MAX + 1,
                "the '{subject}' pass must render the block AT its cap — that is the shape this \
                 budget prices:\n{pass}",
            );
            assert!(
                block[0].contains(run_id.as_str()),
                "…still headed by the driver:\n{pass}",
            );
            assert!(
                block[..SPEC_RUN_LINES_MAX]
                    .iter()
                    .all(|l| l.contains("STALE")
                        && l.contains("sub_workflow CHILD")
                        && l.contains(LONGEST_SEEDED_NODE_LABEL)),
                "…with EVERY listed line paying for EVERY clause that makes it expensive, on the \
                 dearest label a seeded graph carries:\n{pass}",
            );
            assert!(
                pass.len() <= AUDIT_PASS_AT_RUN_CAP_MAX,
                "the '{subject}' audit pass at the run cap is {} chars, over the \
                 {AUDIT_PASS_AT_RUN_CAP_MAX} budget",
                pass.len(),
            );
            cycle += pass.len();
        }
        assert!(
            cycle <= AUDIT_CYCLE_AT_RUN_CAP_MAX,
            "a 4-pass audit cycle at the run cap is {cycle} chars, over the \
             {AUDIT_CYCLE_AT_RUN_CAP_MAX} budget",
        );
    }

    /// THE AMBIENT WORKSPACE-COMMANDS BLOCK (task 01M14TJ97MW647Q4PV6V5N80D8) — a NESTED submodule
    /// of `tests`, the house idiom for a thematic batch (see `db::tests::schema_drift`), so a
    /// `cargo test mcp_context::tests::` filter reaches these and a reader opening `mod tests` finds
    /// them.
    ///
    /// The renderer takes its run-state overlay as a CLOSURE, so EVERY case below is exercised
    /// without a `CommandRunner` and without spawning one process — which matters twice over: there
    /// is no API to mark an instance `running` in a runner short of launching a real process, and
    /// such a test would land squarely in the Windows PTY/kill failure family `CLAUDE.md` documents.
    mod workspace_commands_block_tests {
        use super::*;

        /// One `WorkspaceCommandLine` row, with the fields the block reads spelled out. Built by hand
        /// (not read back from SQLite) precisely because the renderer's contract is
        /// `(rows, overlay) -> String`: the DB reader has its own tests.
        fn row(id: &str, name: &str, command: &str, last_state: &str) -> db::WorkspaceCommandLine {
            db::WorkspaceCommandLine {
                id: id.to_string(),
                name: name.to_string(),
                command: command.to_string(),
                last_state: last_state.to_string(),
                group_id: None,
                order_index: 0,
            }
        }

        /// The overlay that knows nothing: every row falls back to its persisted `last_state`.
        fn no_overlay(_: &str) -> Option<RunState> {
            None
        }

        /// A workspace WITH managed commands renders the block: a header, then one line per command
        /// carrying `name · command · state · instance_id=…` — the id being the whole point (it is what
        /// `start_command` takes, so the agent needs no discovery call at all).
        #[test]
        fn a_workspace_with_commands_renders_one_line_per_command() {
            let rows = vec![
                row("inst-dev", "dev", "bun run dev", db::STATE_RUNNING),
                row("inst-build", "build", "bun run build", db::STATE_IDLE),
            ];
            let out = workspace_commands_block(&rows, no_overlay);
            assert!(
                out.starts_with("Managed commands of this workspace:"),
                "the block opens with its header: {out}"
            );
            assert!(
                out.contains("\n- dev · bun run dev · running · instance_id=inst-dev\n"),
                "the running command's line is rendered verbatim: {out}"
            );
            assert!(
                out.contains("\n- build · bun run build · idle · instance_id=inst-build\n"),
                "…and so is the idle one: {out}"
            );
        }

        /// A workspace with NO managed command renders NOTHING — no header, no directive. The ambient
        /// context of such a session is exactly the orientation line it always was.
        #[test]
        fn no_command_renders_no_block_and_no_directive() {
            let out = workspace_commands_block(&[], no_overlay);
            assert!(out.is_empty(), "an empty workspace renders nothing: {out:?}");
        }

        /// The FOUR domain states are rendered distinctly, in the domain's OWN vocabulary
        /// (`RunState::as_db_str`) — no invented label an agent would have to map back.
        #[test]
        fn the_four_run_states_are_rendered_with_the_domain_vocabulary() {
            let rows = vec![
                row("i-idle", "a", "c", db::STATE_IDLE),
                row("i-run", "b", "c", db::STATE_RUNNING),
                row("i-ok", "c", "c", db::STATE_SUCCESS),
                row("i-ko", "d", "c", db::STATE_ERROR),
            ];
            let out = workspace_commands_block(&rows, no_overlay);
            for (id, state) in [
                ("i-idle", "idle"),
                ("i-run", "running"),
                ("i-ok", "success"),
                ("i-ko", "error"),
            ] {
                assert!(
                    out.contains(&format!("· {state} · instance_id={id}")),
                    "{id} renders as `{state}`: {out}"
                );
            }
        }

        /// THE OVERLAY WINS. A row persisted `idle` whose instance the runner backs as `Running` renders
        /// `running` — this is the whole reason the block reads the runner instead of the row, and the
        /// same precedence `list_commands` applies. A row the overlay does not know keeps its persisted
        /// state, in the same render.
        #[test]
        fn the_runner_overlay_takes_precedence_over_a_stale_persisted_state() {
            let rows = vec![
                row("i-live", "dev", "bun run dev", db::STATE_IDLE),
                row("i-cold", "build", "bun run build", db::STATE_SUCCESS),
            ];
            let out = workspace_commands_block(&rows, |id: &str| {
                (id == "i-live").then_some(RunState::Running)
            });
            assert!(
                out.contains("· running · instance_id=i-live"),
                "the live instance reads `running` despite its stale `idle` row: {out}"
            );
            assert!(
                out.contains("· success · instance_id=i-cold"),
                "the instance the runner does not back keeps its persisted state: {out}"
            );
        }

        /// The block renders rows in the order it RECEIVES them —
        /// `db::list_command_lines_for_workspace` already orders by `order_index` then `id`, which is
        /// the order of the project's sidebar, so any re-sort here would make the block disagree with
        /// what the user is looking at.
        #[test]
        fn the_native_order_is_preserved_and_never_re_sorted() {
            let rows = vec![
                row("i-3", "zulu", "c", db::STATE_IDLE),
                row("i-1", "alpha", "c", db::STATE_IDLE),
                row("i-2", "mike", "c", db::STATE_IDLE),
            ];
            let out = workspace_commands_block(&rows, no_overlay);
            let order: Vec<&str> = out
                .lines()
                .filter(|l| l.starts_with("- "))
                .map(|l| l.split(" · ").next().unwrap_or_default())
                .collect();
            assert_eq!(
                order,
                vec!["- zulu", "- alpha", "- mike"],
                "the block is the reader's order, not an alphabetical one: {out}"
            );
        }

        /// Past twelve commands the block STOPS listing and COUNTS: twelve lines, then one
        /// `+N more commands (list_commands)` line naming the tool that has the rest. The block is
        /// orientation, not a dump into every session's first tokens.
        #[test]
        fn beyond_twelve_commands_the_surplus_is_counted_not_listed() {
            let rows: Vec<db::WorkspaceCommandLine> = (0..15)
                .map(|i| row(&format!("i-{i}"), &format!("cmd-{i}"), "c", db::STATE_IDLE))
                .collect();
            let out = workspace_commands_block(&rows, no_overlay);
            assert_eq!(
                out.lines().filter(|l| l.starts_with("- ")).count(),
                MAX_COMMAND_LINES,
                "at most twelve command lines: {out}"
            );
            assert!(
                out.contains("\n+3 more commands (list_commands)"),
                "the surplus is counted, and names where to read it: {out}"
            );
            assert!(
                !out.contains("instance_id=i-12"),
                "the 13th command is NOT listed: {out}"
            );
            // Exactly twelve is the boundary: no surplus line at all.
            let out12 = workspace_commands_block(&rows[..12], no_overlay);
            assert!(
                !out12.contains("more commands (list_commands)"),
                "twelve commands fit: no surplus line: {out12}"
            );
        }

        /// A long command is TRUNCATED — the rendered text never exceeds the bound, ellipsis included,
        /// and the ellipsis says so. The agent has `list_commands` for the exact text; the block is there
        /// for it to RECOGNIZE the command.
        #[test]
        fn a_command_longer_than_the_bound_is_truncated() {
            let long = "x".repeat(400);
            let rows = vec![row("i-long", "big", &long, db::STATE_IDLE)];
            let out = workspace_commands_block(&rows, no_overlay);
            let line = out
                .lines()
                .find(|l| l.starts_with("- big "))
                .expect("the long command has a line");
            let rendered = line
                .split(" · ")
                .nth(1)
                .expect("the line carries a command field");
            assert_eq!(
                rendered.chars().count(),
                MAX_COMMAND_CHARS,
                "the rendered command is capped at the bound, ellipsis included: {rendered}"
            );
            assert!(
                rendered.ends_with('…'),
                "the truncation is visible: {rendered}"
            );
            assert!(
                !out.contains(&long),
                "the raw command never reaches the block: {out}"
            );
            // A command AT the bound is left strictly alone.
            let exact = "y".repeat(MAX_COMMAND_CHARS);
            let fit = [row("i-fit", "fit", &exact, db::STATE_IDLE)];
            let kept = workspace_commands_block(&fit, no_overlay);
            assert!(
                kept.contains(&exact),
                "a command exactly at the bound is not touched"
            );
        }

        /// `group_id=…` is rendered on a GROUPED command and on nothing else — and the group DIRECTIVE
        /// follows the same rule, because telling an agent about `start_command {group_id}` in a
        /// workspace that has no group is telling it about a call it cannot make.
        #[test]
        fn only_a_grouped_command_carries_a_group_id_and_the_group_directive() {
            let mut grouped = row("i-api", "api", "bun run api", db::STATE_IDLE);
            grouped.group_id = Some("grp-1".to_string());
            let solo = row("i-solo", "solo", "bun run solo", db::STATE_IDLE);

            let with_group = workspace_commands_block(&[grouped, solo.clone()], no_overlay);
            assert!(
                with_group.contains("· instance_id=i-api · group_id=grp-1"),
                "the grouped command names its group, last on the line: {with_group}"
            );
            assert!(
                with_group.contains("· instance_id=i-solo\n"),
                "the solo command carries nothing more: {with_group}"
            );
            assert!(
                with_group.contains(GROUP_DIRECTIVE_LINE),
                "the group directive is rendered when a group is on screen: {with_group}"
            );

            let solo_only = workspace_commands_block(&[solo], no_overlay);
            assert!(
                !solo_only.contains("group_id="),
                "no group anywhere, so not a word about groups: {solo_only}"
            );
            assert!(
                !solo_only.contains(GROUP_DIRECTIVE_LINE),
                "…the group directive included: {solo_only}"
            );
        }

        /// THE DIRECTIVE, and what it must NAME. The tool names are the contract: an agent told to
        /// "prefer nyx" without them has to go discover the surface it was supposed to save.
        #[test]
        fn the_directive_names_the_tools_and_states_the_no_op_and_the_snapshot() {
            let out = workspace_commands_block(&[row("i", "dev", "c", db::STATE_IDLE)], no_overlay);
            for tool in [
                "start_command",
                "relaunch_command",
                "list_commands",
                "get_command_output",
            ] {
                assert!(out.contains(tool), "the directive names {tool}: {out}");
            }
            assert!(
                out.contains("was_running:true") && out.contains("restarted:false"),
                "it states the ack a no-op start returns, so nothing is relaunched blind: {out}"
            );
            assert!(
                out.contains("/clear") && out.contains("/compact"),
                "the snapshot sentence says WHEN the block is re-taken — /clear and /compact included, \
                 which is what `hooks.json` actually matches: {out}"
            );
            let directive: Vec<&str> = out
                .lines()
                .filter(|l| !l.starts_with("- ") && !l.starts_with("Managed commands"))
                .collect();
            assert!(
                directive.len() <= 4,
                "the directive stays lean — 4 lines at most, got {}: {directive:?}",
                directive.len()
            );
        }

        /// WHERE the snapshot sentence puts `/clear` and `/compact`, not merely THAT it names them
        /// (finding `01M1A4J8W87R23ZRJ7QVVS6B3S`). The first wording named both — and a real session
        /// still read the line back as "a snapshot taken at session open", because they sat in an
        /// em-dash aside behind that framing, and an aside is what a model drops first when it
        /// compresses. Presence was already asserted; it is the SHAPE that failed, so the shape is
        /// what this locks.
        #[test]
        fn the_snapshot_sentence_carries_clear_and_compact_in_its_main_clause() {
            let out = workspace_commands_block(&[row("i", "dev", "c", db::STATE_IDLE)], no_overlay);
            let line = out
                .lines()
                .find(|l| l.contains("SNAPSHOT"))
                .unwrap_or_else(|| panic!("the block ends on the snapshot directive: {out}"));

            assert!(
                !line.contains('—'),
                "no em-dash aside holds a fact here — an aside is the first thing a model drops \
                 when it compresses: {line}"
            );

            let clear_at = line.find("/clear").expect("names /clear");
            let compact_at = line.find("/compact").expect("names /compact");
            let fact_at = clear_at.max(compact_at);

            if let Some(session_at) = line.find("session") {
                assert!(
                    fact_at < session_at,
                    "nothing frames the snapshot at session open BEFORE naming what re-takes it: \
                     {line}"
                );
            }

            for remedy in ["list_commands", "get_command_output"] {
                let remedy_at = line.find(remedy).unwrap();
                assert!(
                    fact_at < remedy_at,
                    "the fact leads, the remedy follows — {remedy} comes after /clear and /compact: \
                     {line}"
                );
            }
        }

        /// NO ENVIRONMENT VALUE can reach the block — and this proves it through the REAL reader, not by
        /// re-asserting the struct's shape. `env` is a column of `managed_commands` (the TEMPLATE), so a
        /// template CAN carry a secret; `db::list_command_lines_for_workspace` never selects it and
        /// `WorkspaceCommandLine` has no field to hold it, so the render cannot leak what it was never
        /// handed.
        #[test]
        fn no_environment_value_reaches_the_render() {
            let db = Db::in_memory();
            let rows = db
                .with_conn(|c| {
                    let (project, root) = db::create_project(c, "p", "/tmp/env-leak", None)?;
                    let tpl = db::create_template(
                        c,
                        &project.id,
                        "dev",
                        "bun run dev",
                        None,
                        db::CommandSource::default(),
                    )?;
                    db::set_template_env(
                        c,
                        &tpl.id,
                        Some(r#"{"VAULT_TOKEN":"s3cr3t-do-not-inject"}"#),
                    )?;
                    db::list_command_lines_for_workspace(c, &root.id)
                })
                .expect("seeded workspace");
            assert_eq!(rows.len(), 1, "the template materialized one instance");
            let out = workspace_commands_block(&rows, no_overlay);
            assert!(
                out.contains("- dev · bun run dev · idle · instance_id="),
                "the command IS rendered: {out}"
            );
            assert!(
                !out.contains("s3cr3t-do-not-inject") && !out.contains("VAULT_TOKEN"),
                "…and its environment is nowhere near it: {out}"
            );
        }
    }
}
