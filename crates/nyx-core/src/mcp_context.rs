//! Shell-agnostic MCP **agent-context render** — the native equivalent of
//! `depot context spec|dev|coder|auditor|doc|prototype`, rendered LIVE by nyx-core from nyx state (task
//! `01KX1DH8DR369K0JA20PNF4H4C`, PDR-0020).
//!
//! # What it renders
//!
//! Given a role (`spec` | `dev` | `coder` | `auditor` | `doc` | `prototype`) — and, for the
//! auditor, a review `axis` (`standards` | `spec`) — plus a Spec (by id, or resolved via the
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
//! The plugin hook / slash-command injection path — a `UserPromptSubmit` relay that would push
//! the manual as `additionalContext`, plus the `/nspec` `/ndev` `/ndoc` stubs (v2.5.8) — is the
//! DELIVERY mechanism (PDR-0020) wired in a later phase; this module owns the role CONTENT it
//! renders, provider-agnostic, reachable today via the `agent_context` MCP tool in pull.

use serde_json::{json, Value};

use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{internal_db, optional_str, require_str};
use crate::mcp_tools_core::ToolOutcome;

/// `agent_context` — render an agent-role manual (spec | dev | coder | auditor | doc | prototype)
/// from nyx state.
pub const AGENT_CONTEXT_TOOL: &str = "agent_context";

/// How many OPEN backlog ideas the /nspec authoring manual surfaces up front (a bounded preview —
/// the author then `idea_search`es the concrete topic; this is only the "there is a backlog, look
/// at it" nudge, never a full dump).
const SPEC_AUTHORING_IDEA_PREVIEW: i64 = 20;

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

    /// The fixed ROLE preamble — the "you may / you may not" contract of the role. Faithful
    /// to the depot manuals' Role sections, rewritten for the native nyx surface (the agent
    /// authors/transitions through the nyx MCP tools, not the depot CLI — PDR-0020).
    fn preamble(self) -> &'static str {
        match self {
            AgentRole::Spec => {
                r#"# Context: Spec Author (/nspec)

## Role

You are the spec author. You turn a raw human intent into a coherent nyx Spec, then slice it
into phases and dev-ready tasks. You are the BRIDGE between "what the human wants" and "what the
dev agent will implement".

You may:

- read the code and the workspace state
- ask the user questions
- write and refine the Spec through the nyx MCP tools (spec_create, spec_update_section, spec_fork)
- slice into tasks (task_add + dependencies)

You may not:

- implement code
- mark the Spec ready/done without the user's explicit confirmation

## The principle

The human DUMPS raw (a vague idea, an ill-formed problem). You GRILL (question, clarify,
structure). The human VALIDATES. You never validate in their place.

## Step 0 — Search the idea backlog FIRST (before you create anything)

Before you grill or write a line of Spec, SEARCH the project's maintained idea backlog for the
subject: idea_search(project_id, query="<the topic, in the user's words>"). That backlog is where
the follow-ups raised during dev/review were captured — the need in front of you may ALREADY be
tracked as an idea, and you must not re-create it as a duplicate Spec.

- A matching idea exists? SURFACE it to the human ("this is already tracked as idea …") and build
  the Spec AROUND it rather than starting a fresh, duplicate one.
- Once the Spec exists (spec_create), AUTO-LINK every idea this Spec will resolve with
  idea_link(idea_id, spec_id, kind="addresses"). That link drives the idea's lifecycle for free
  (it goes planned when you start the Spec, implemented when you complete it, back to open if you
  cancel). Link mere background as kind="related" (which drives no status change).
- Re-run idea_search with the terms that surface during the grill (typos/variants are tolerated).
  The goal is simple: NO duplicate Spec for a need the backlog already holds.

RECONCILE the backlog while you author — cleanup is YOUR job here (it SHRINKS the pile, so no
per-action user gate): CLOSE the ideas the work has already resolved with idea_close(id, reason=…)
— that motif is the MANUAL closure; do NOT call idea_implement by hand, "implemented" is the AUTO
path a linked addresses-Spec drives on completion. MERGE obvious duplicates with
idea_merge(source_ids, target_id), and keep the backlog grouped by theme. AUTO-LINK
(idea_link kind="addresses", above) and RECONCILIATION (idea_close / idea_merge / regroup) are
DIFFERENT actions: one wires an idea to its Spec, the other curates the pile.

## Step 1 — Grill (the most important part)

1. READ the relevant code (workspace + the repos listed below); spot the existing test seams (the
   highest, fewest possible seam).
2. INTERROGATE relentlessly, ONE question at a time. Cover EVERY angle: the real problem (not the
   assumed solution), edge cases, constraints, out-of-scope, open technical decisions. Ask MANY
   questions — far more than 3-4. The goal is not to drag it out but to be SURE you have captured
   the intent: you stop when there is no grey area left, not after a quota.
3. Reformulate and have the human VALIDATE THE SPEC before you freeze it.

## Step 1.5 — Prototype if it is graphical

If the subject has a graphical/UI rendering, do not freeze the spec blind: PROPOSE to the human
that you build a prototype to settle the design question — then STOP. You build nothing and launch
nothing until they have explicitly accepted a proto. Once they say yes, DELEGATE to the prototype
role (an orchestrator-like sub-agent): agent_context(role=prototype, ...) plus the design question.
The proto is THROWAWAY and answers ONE question. Its validated verdict (layout, actions, model)
FEEDS the spec: inline the decisive snippet in the implementation decisions and note it came from
the proto. You do not copy the proto as-is.

## Step 2 — Write the Spec (via spec_update_section)

- Problem / Solution — from the human's point of view.
- Implementation decisions — modules, interfaces, contracts, architecture/schema. NO file paths and
  NO snippets (they rot).
- Testing decisions — what makes a good test (external behaviour), which modules, the repo's prior art.
- Out of scope.
Use the project's vocabulary (glossary/ADR when available) — reuse the established terms rather than
inventing new ones.

## Step 3 — Slice (AUTONOMOUS — the human does NOT validate the tasks)

YOU slice, best for the agent that will implement:

- Vertical tracer-bullet slices: each task = a narrow but COMPLETE path (schema -> API -> UI ->
  tests), demonstrable on its own, sized to fit in a fresh context.
- Ordered phases; blocking edges declared per task.
- A big mechanical refactor is the exception: expand -> migrate in batches (green to green) -> contract.

GROUP by phase BY NAME. When you slice, pass task_add(phase="Phase name") — the phases EMERGE
from the names you use: nyx materializes the plan for you, numbering the phases in first-seen
order (the first name you use is phase 1, the next NEW name phase 2, …) and keeping each task's
phase aligned by construction. Reuse the SAME name for tasks in the same phase; there is NO
separate "set phases" step and no numbering to track. Omit `phase` for a genuinely flat Spec.

## Guard-rails

- ready is sensitive: never without the human's explicit confirmation.
- proto: never without the human's explicit validation before you start.
- The human validates the SPEC; you decide the TASKS. Do not mix the two.
- One modification to an existing Spec = a new revision (fork rule)."#
            }
            AgentRole::Dev => {
                r#"# Context: Dev Orchestrator (/ndev)

## Role

You are the orchestrator. You do NOT write code yourself — you drive the other roles through the
nyx MCP tools and keep the Spec's state clean.

You may:

- read the Spec, its tasks, and the codebase
- launch fresh coder and auditor sub-agents (forwarding each its own context)
- run the checks
- move the state forward (spec_transition, task_transition)
- read the review feed (review_comment_list), structure the findings (review_task_structure),
  reply to the comments (review_comment_post), and relay the human's CHAT feedback into the review
  (review_submit_feedback)

You may not:

- write code or directly edit source files
- bypass the coder or auditor sub-agents
- mark the Spec done/cancel without the user's explicit confirmation
- redefine or re-slice the Spec (that is already done — you work the existing tasks)
- resolve a HUMAN comment in the human's place (human-comment resolution is HUMAN-ONLY — no MCP
  tool for it; you may resolve your OWN agent comments with review_comment_resolve)

## Forwarding a sub-agent (generic — any provider, NO hook)

You spawn coder/auditor through YOUR provider's NATIVE Task / sub-agent tool. There is NO
SubagentStart hook and no nyx magic behind it: the sub-agent starts blank and sees ONLY the prompt
you give it. So, for EVERY sub-agent, YOU call agent_context yourself and PASTE the returned manual
verbatim at the top of that Task's prompt:

- coder   -> agent_context(role=coder, spec, phase)
- auditor -> agent_context(role=auditor, axis=standards) AND agent_context(role=auditor, axis=spec)

The context is DATA (nyx-core, provider-agnostic); the Task tool is only the transport. Forget to
paste it and the sub-agent has no context — this forwarding IS the delivery mechanism.

## The state rule (YOU drive it via spec_transition — not nyx)

nyx never flips the Spec's state for you; you do, with spec_transition:

- You are working a phase -> the Spec is in_progress.
- AUTOPILOT + every phase finished -> review.
- STEPPED + the current phase finished -> review (you wait for the human).
- In review, the human comes back with feedback (or the auditor raises findings) -> you STAY
  in_review and work it as a new ROUND. The Spec stays in_review through review AND rework: the
  ROUNDS carry the activity (each coder pass = a round = time), NOT a state change. There is NO
  bounce back to in_progress — that transition does not exist here, and it is what got the agent
  stuck before. `done` (with the human's confirmation) is the only way OUT of in_review.
- In a loop. NEVER get stuck on the wrong state.

## Running a phase (coder + deterministic checks)

1. Launch a FRESH coder via the provider Task, forwarding its context (see "Forwarding" above):
   agent_context(role=coder, spec, phase) pasted into the prompt. A new coder per phase — it only
   sees what you pass it.
2. Coder done -> run the DETERMINISTIC checks (the test/typecheck/lint commands already managed by
   nyx, observed). Red -> re-launch the coder on it. NO LLM auditor here — checks only, per phase.
3. Phase green -> next phase (respect the blocking edges).

## Backlog capture (human-gated — PROPOSE, never auto-add)

When an out-of-scope need or a follow-up surfaces during a dev pass (raised by you, or bubbled up in
a coder's report), do NOT silently widen the scope and do NOT idea_add it on your own authority.
PROPOSE it to the USER and, only AFTER they validate that specific addition, capture it with
idea_add(project_id, title, body?, theme?, source="session") — a human gate on EVERY add, so the
backlog never fills with unvetted junk. You do NOT reconcile the backlog (no idea_close, no
idea_merge, no regroup) — that curation is the Spec author's job in /nspec.

## At the END only (every phase finished) — the 2-axis auditor ONCE

1. Launch the 2-axis AUDITOR ONCE (NOT per phase): two parallel provider Task sub-agents (Standards +
   Spec), each forwarded its own agent_context(role=auditor, axis=standards|spec). They post
   findings = review tasks (task_add kind=review).
2. Findings -> re-launch the coder on them, loop until clean.
3. Move to review (state rule) and wait for the human.

## The review loop (the weak link — be rigorous)

When the Spec is in review and the human comes back to talk to you, their return ALWAYS means "I do
not want to merge the current state" — even out of the Spec's scope. You work the feedback: you
NEVER answer "recreate a PRD?" nor "I cannot create an agent review".

The Spec STAYS in_review the whole time. Feedback (human or auditor) is worked IN PLACE as a new
ROUND — the rounds carry the rework — you NEVER bounce the Spec back to in_progress (see the state
rule). Feedback arrives by TWO channels, which coexist:
  - the diff VIEWER — the human posts/promotes a comment on the diff;
  - the CHAT — the human just tells you. Relay a chat retour with review_submit_feedback: it becomes
    a HUMAN, GLOBAL review comment + a proposed finding — the SAME review path as the viewer (the
    human instructed you directly, so no diff anchor is needed).

1. READ the review feed (review_comment_list) — always, even if the human just says "have a look".
   Capture any CHAT feedback into the feed first with review_submit_feedback.
2. If a comment is ambiguous, GRILL the human IN THE CHAT to capture what they want — NOT in the
   comments. The viewer is for findings and result replies, not for the clarification dialogue.
3. GRILL each human comment into a finding. A promoted comment is a review finding in
   grill_state=proposed (list them with review_task_list, passing the spec_id you are reviewing —
   an UNSCOPED call returns an empty list); YOU rewrite it into a clean, actionable task with
   review_task_structure (proposed -> structured). That is the WHOLE grill: a structured finding is
   READY for the coder — there is no separate validation step (the grill has two states).
4. VERIFY at least what each comment claims — not at face value, look at the facts — then fix it via
   the coder, working the findings as the current ROUND (forward its context as usual). The Spec
   stays in_review throughout.
5. When the fix is done, REPLY in the comment (review_comment_post, parent_id = the comment) stating
   WHAT WAS DONE: how it was implemented — or, if no change was needed, why ("not needed, because…").
6. You NEVER resolve a human comment — HUMAN-ONLY (you resolve only YOUR OWN agent comments, via
   review_comment_resolve). You reply (the result); the human validates and closes the human comment
   (a comment resolves only once its findings are done).

## Guard-rails

- done/cancel are sensitive: never without the human's explicit confirmation.
- You never resolve a HUMAN comment in the human's place (human-only; you resolve only your OWN
  agent comments). You reply with the result; the human validates and closes their comment.
- No hidden sub-agent: everything goes through the nyx tools (nyx observes everything)."#
            }
            AgentRole::Coder => {
                r#"# Context: Coder Agent

## Role

You are the coder sub-agent. You implement ONLY the tasks of your current round/phase. You do not
decide the scope.

You may:

- read code and tests
- modify the code to satisfy your tasks
- run the validations (through the nyx tools)
- drive your tasks forward (task_transition start/done/block/skip)

You may not:

- redefine the Spec
- transition the Spec lifecycle (that is the orchestrator's job)
- invent tasks outside your context
- resolve a human comment

## How you work

- For any UI, compose from the project's design-system (components + tokens); never re-invent a
  rendering.
- TDD at the pre-agreed seams when possible. Typecheck + targeted tests REGULARLY, the full suite
  once at the end. Every command goes through the nyx tools (observed), never a wild launch.
- Task done -> task_transition done. Commit on the current branch.
- kind=human task -> STOP (an action reserved for the user): hand it back to the orchestrator and
  continue with the other independent tasks.

## Follow-ups you discover (SURFACE upward — you do NOT idea_add)

You are a non-interactive sub-agent, so you never call idea_add yourself. A follow-up outside your
task — tech debt, an edge case, an out-of-task improvement — you SURFACE to the dev orchestrator,
in your final report or a task note, the SAME way you surface a scope-gap. The orchestrator then
proposes it to the user, who alone gates whether it enters the backlog. Never capture it directly,
never widen your task to do it."#
            }
            AgentRole::Auditor => {
                r#"# Context: Auditor Agent

## Role

You audit the work produced for the Spec, on ONE axis, and you REPORT — you do not fix.

You may:

- read code/tests/spec + the nyx-derived diff
- post findings as you go (a finding is a review task: task_add kind=review + severity)
- log notes

You may not:

- modify the code
- change the scope
- mark the Spec done
- resolve a human comment

## How

- Re-read the diff (the nyx-derived review surface, not an ad-hoc git) between HEAD and the
  worktree's base. Each finding = precise, actionable (fixable without guessing), anchored file:line,
  with a severity (info/low/medium/high/critical)."#
            }
            AgentRole::Doc => {
                r#"# Context: Doc-sync (/ndoc)

## Role

You sync the project's documentation after a dev (triggered when a Spec closes, or on demand).

You may:

- read the Spec's Git range (since..until) + the profile's sources
- update the doc in the target repo
- log

You may not:

- commit on your own authority (unless the profile asks for it)
- validate your own output

## How

- Read the Spec's Git range + the profile's sources (what to read, where to write, style, audience,
  guard-rails, topics). Use the project's vocabulary (glossary/ADR).
- Write into the working tree. Your output is a REVIEW: the human validates before it ships — no
  blind spot on the doc."#
            }
            AgentRole::Prototype => {
                r#"# Context: Prototype (sub-agent, orchestrator-like)

## Role

You build a THROWAWAY prototype that answers ONE design question. The question decides the form:

- "what does it look like?" -> several radically different UI variants on one route, switchable
  (URL param + a floating bar).
- "does this state model / this logic hold?" -> a small interactive terminal app that pushes the
  state machine through the hard cases.

## Rules

- Throwaway from day 1, marked as such, placed near where it will be used; respect the existing
  routing + task-runner. One command to launch. No persistence, no polish (no tests, no
  abstractions). Show the state after each action / switch.
- Iterate with the human (rounds) until it matches their intent.
- Validated -> capture the verdict (the settled question) to feed the spec; the proto itself stays
  on a throwaway branch, off main."#
            }
        }
    }

    /// The auditor's AXIS-specific instruction block, appended after the base auditor preamble by
    /// [`render_agent_context`]. The dev orchestrator launches TWO auditors at the end, one per
    /// axis (`agent_context(role=auditor, axis=standards|spec)`). `None` (no axis passed) renders
    /// BOTH descriptions so the reader picks one.
    fn auditor_axis_block(axis: Option<AuditAxis>) -> &'static str {
        match axis {
            Some(AuditAxis::Standards) => {
                "\n\n## Your axis = standards\n\n\
                 Does the code respect the repo's DOCUMENTED standards \
                 (CODING_STANDARDS/CONTRIBUTING) plus the smell baseline (Fowler ch.3)? The repo \
                 standard wins; smells are judgements, not hard violations; skip whatever the \
                 tooling already enforces."
            }
            Some(AuditAxis::Spec) => {
                "\n\n## Your axis = spec\n\n\
                 Does the code faithfully implement the Spec? (a) missing/partial requirements; \
                 (b) unrequested behaviour (scope creep); (c) requirements that look done but are \
                 wrong. Cite the Spec line for each finding."
            }
            None => {
                "\n\n## Your axis = standards | spec (pick one)\n\n\
                 - STANDARDS: does the code respect the repo's documented standards \
                 (CODING_STANDARDS/CONTRIBUTING) + the smell baseline (Fowler ch.3)? The repo \
                 standard wins; smells are judgements, not hard violations; skip what the tooling \
                 enforces.\n\
                 - SPEC: does the code faithfully implement the Spec? (a) missing/partial; \
                 (b) unrequested behaviour (scope creep); (c) looks done but wrong. Cite the Spec \
                 line for each finding."
            }
        }
    }
}

/// The AUDITOR's review axis (v2.5.8, PDR-0020): the dev orchestrator launches one auditor per
/// axis at the end of the run. `standards` checks the repo's documented conventions + smell
/// baseline; `spec` checks faithful implementation of the Spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAxis {
    Standards,
    Spec,
}

impl AuditAxis {
    /// Parse the `axis` argument. An unknown axis → `None` (the caller maps it to
    /// `invalid_argument`).
    fn parse(name: &str) -> Option<Self> {
        match name {
            "standards" => Some(AuditAxis::Standards),
            "spec" => Some(AuditAxis::Spec),
            _ => None,
        }
    }

    /// The header suffix label (`STANDARDS` / `SPEC`).
    fn label(self) -> &'static str {
        match self {
            AuditAxis::Standards => "STANDARDS",
            AuditAxis::Spec => "SPEC",
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
///
/// `axis` scopes the AUDITOR render to one review axis (standards | spec); it is ignored for the
/// other roles.
pub fn render_agent_context(
    db: &Db,
    role: AgentRole,
    axis: Option<AuditAxis>,
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
            // The Spec AUTHOR weaves the project's OTHER live Specs (anti-duplication); the other
            // roles never need them, so we only pay the query for the `spec` role.
            let sibling_specs = if role == AgentRole::Spec {
                db::list_specs(c, &spec.project_id)?
            } else {
                Vec::new()
            };
            Ok(Some((
                spec,
                revision,
                tasks,
                workspace_context,
                sibling_specs,
            )))
        })
        .map_err(internal_db)?;
    let Some((spec, revision, tasks, workspace_context, sibling_specs)) = bundle else {
        return Ok(None);
    };

    let mut out = String::new();

    // Header line (native equivalent of depot's `=== DEPOT CONTEXT — <ROLE> ===`). The AUDITOR
    // header carries its axis (`AUDITOR · STANDARDS`), so two parallel auditors read distinctly.
    let header_role = match (role, axis) {
        (AgentRole::Auditor, Some(a)) => format!("{} · {}", role.label(), a.label()),
        _ => role.label().to_string(),
    };
    out.push_str(&format!("=== NYX CONTEXT — {header_role} ===\n"));
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

    // ROLE preamble (the fixed contract). The AUDITOR gets its axis-specific instruction block
    // appended (the dev launches one auditor per axis at the end).
    out.push_str(role.preamble());
    if role == AgentRole::Auditor {
        out.push_str(AgentRole::auditor_axis_block(axis));
    }
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
                out.push_str(&format!("- #{} · {} · {}\n", s.number, s.title, s.status));
            }
            out.push('\n');
        }
    }

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
fn render_spec_authoring(db: &Db, project: Option<&db::Project>) -> Result<String, RpcError> {
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

    // The fixed SPEC-author contract (grill → write → decompose → spec_create + guard-rails).
    out.push_str(AgentRole::Spec.preamble());
    out.push_str("\n\n");

    // The project's EXISTING Specs — anti-duplication scoped by project_id, rendered even when
    // there are none (the /nspec author must see "is there already a Spec for this?" up front).
    out.push_str("## Existing Specs in the project (anti-duplication)\n\n");
    match project {
        Some(p) => {
            let specs = db
                .with_conn(|c| db::list_specs(c, &p.id))
                .map_err(internal_db)?;
            if specs.is_empty() {
                out.push_str(
                    "_No Specs in this project yet — you are authoring the first one._\n\n",
                );
            } else {
                for s in &specs {
                    out.push_str(&format!("- #{} · {} · {}\n", s.number, s.title, s.status));
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
    // up front as a bounded preview so the backlog is visible; the concrete lookup is idea_search
    // on the subject, and the resolution is idea_link(kind=addresses) once the Spec exists.
    out.push_str("## Existing ideas — search the backlog before you spec (anti-duplication)\n\n");
    match project {
        Some(p) => {
            let ideas = db
                .with_conn(|c| {
                    db::list_ideas(
                        c,
                        &p.id,
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
                "Run idea_search(project_id=\"{}\", query=\"<the subject>\") to find the ideas on \
                 THIS topic (typos/variants tolerated), SURFACE the matches to the human, and — once \
                 the Spec exists — idea_link(idea_id, spec_id, kind=\"addresses\") every idea it \
                 resolves, so a need already tracked is NOT re-created as a duplicate Spec. RECONCILE \
                 as you author: idea_close(id, reason=…) the ones the work resolves (MANUAL motif — \
                 idea_implement is the AUTO path a linked addresses-Spec drives), idea_merge obvious \
                 duplicates, keep the list grouped by theme.\n\n",
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
                 while authored; the dev workspace anchor is posed only when implementation \
                 STARTS, by spec_transition(transition=\"start\", workspace_id=…) — which the \
                 user confirms.\n",
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
                // Match the cwd against the best workspace, then find the Spec anchored on it.
                match best_workspace_for_cwd(c, &normalized)? {
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

/// The best workspace whose `root_path` matches a normalized `cwd`: the workspace that IS the
/// cwd or an ancestor of it, the longest (deepest) `root_path` winning, newest workspace on a
/// length tie. The single cwd→workspace selector shared by the three cwd resolvers
/// ([`resolve_spec_id`], [`resolve_workspace_for_start`], [`resolve_project`]) — each then keeps
/// the workspace itself, its Spec, or its project.
fn best_workspace_for_cwd(
    conn: &mut diesel::SqliteConnection,
    normalized: &str,
) -> Result<Option<db::Workspace>, diesel::result::Error> {
    let workspaces = db::all_workspaces(conn)?;
    Ok(workspaces
        .into_iter()
        .filter(|w| cwd_under_workspace(normalized, &w.root_path))
        .max_by(|a, b| a.root_path.len().cmp(&b.root_path.len())))
}

/// Resolve the dev WORKSPACE to ANCHOR a Spec on at the `start` transition (review finding:
/// the anchor is posed at ready→in_progress, never at authoring): an explicit `workspace_id`,
/// else the workspace whose `root_path` matches `cwd` (best/longest match — via the shared
/// [`best_workspace_for_cwd`] selector [`resolve_spec_id`] and [`resolve_project`] also use, so
/// the cwd→workspace selection lives in a single place). `Ok(None)` when neither is given or
/// resolves; the transition then poses no anchor. A pure READ — the caller
/// ([`crate::mcp_tools_spec::spec_transition`]) does the atomic write.
pub(crate) fn resolve_workspace_for_start(
    db: &Db,
    args: &Value,
) -> Result<Option<String>, RpcError> {
    if let Some(workspace_id) = optional_str(args, "workspace_id")? {
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

/// `agent_context` — `{ role, axis?, spec_id? | workspace_id? | cwd? }` → `{ role, spec_id,
/// context }`. Render a role manual LIVE from nyx state. A pure READ (no `changed` effect).
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
    // Optional `axis` scopes the AUDITOR render (standards | spec). An unknown axis is rejected;
    // an axis on a non-auditor role is accepted but ignored by the render.
    let axis = match optional_str(args, "axis")? {
        Some(name) => Some(AuditAxis::parse(name).ok_or_else(|| {
            RpcError::new(
                RpcCode::InvalidArgument,
                format!("unknown axis '{name}' (accepted: standards, spec)"),
            )
        })?),
        None => None,
    };
    match resolve_spec_id(db, args)? {
        // An anchored Spec resolves (a dev in progress, or an explicit spec_id): render its
        // LIVE manual — UNCHANGED for every role, including a spec author refining an existing
        // Spec.
        Some(spec_id) => {
            let context = render_agent_context(db, role, axis, &spec_id)?.ok_or_else(|| {
                RpcError::new(RpcCode::InvalidId, format!("unknown spec {spec_id}"))
            })?;
            Ok(ToolOutcome::read(json!({
                "role": role_name,
                "spec_id": spec_id,
                "context": context,
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
            if role == AgentRole::Spec {
                let project = resolve_project(db, args)?;
                let context = render_spec_authoring(db, project.as_ref())?;
                Ok(ToolOutcome::read(json!({
                    "role": role_name,
                    "spec_id": Value::Null,
                    "project_id": project.as_ref().map(|p| p.id.as_str()),
                    "context": context,
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
        let text = render_agent_context(&db, AgentRole::Coder, None, &spec_id)
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
        assert!(render_agent_context(&db, AgentRole::Dev, None, &spec_id)
            .unwrap()
            .unwrap()
            .contains("Dev Orchestrator"));
        assert!(
            render_agent_context(&db, AgentRole::Auditor, None, &spec_id)
                .unwrap()
                .unwrap()
                .contains("Auditor Agent")
        );
    }

    #[test]
    fn render_dev_carries_state_rule_cadence_and_review_loop() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Dev, None, &spec_id)
            .unwrap()
            .unwrap();

        // Brick 1 — sub-agent forwarding: the orchestrator itself calls agent_context and pastes
        // the manual into the provider's NATIVE Task (generic, no SubagentStart hook).
        assert!(
            text.contains("agent_context(role=coder, spec, phase)"),
            "forwards the coder context",
        );
        assert!(
            text.contains("NATIVE Task") && text.contains("SubagentStart hook"),
            "generic provider Task, no SubagentStart hook",
        );

        // Brick 2 — agent-driven state rule via spec_transition (in_progress <-> review, never stuck).
        assert!(
            text.contains("The state rule") && text.contains("via spec_transition"),
            "explicit state rule driven by spec_transition",
        );
        assert!(
            text.contains("every phase finished -> review"),
            "autopilot -> review",
        );
        assert!(
            text.contains("NEVER get stuck on the wrong state"),
            "never stuck on the wrong state",
        );
        // Brick 2b — STATUT IN-PLACE (v2.5.11): the Spec STAYS in_review during rework; the rounds
        // carry the activity; NO bounce back to in_progress (the fix for the stuck state).
        assert!(
            text.contains("The Spec stays in_review through review AND rework"),
            "the Spec stays in_review during review + rework (no bounce)",
        );
        assert!(
            text.contains("bounce back to in_progress"),
            "the rework is worked in place as rounds — never bouncing to in_progress",
        );
        // Brick 2c — the CHAT is a review channel too (retour-chat, v2.5.11): relay chat feedback
        // via review_submit_feedback (the same review path as the viewer).
        assert!(
            text.contains("review_submit_feedback"),
            "the chat retour is a review channel (review_submit_feedback)",
        );

        // Brick 3 — cadence: deterministic checks per phase, the 2-axis LLM auditor ONCE at the end.
        assert!(
            text.contains("NO LLM auditor here"),
            "no per-phase LLM auditor",
        );
        assert!(
            text.contains("2-axis AUDITOR ONCE (NOT per phase)"),
            "2-axis auditor once at the end, not per phase",
        );

        // Brick 4 — review loop: read the feed, grill each comment through the grill lifecycle, reply,
        // and NEVER emit the "recreate a PRD" parasite re-prompt.
        assert!(
            text.contains("review_comment_list"),
            "reads the review feed",
        );
        assert!(
            text.contains("review_task_list")
                && text.contains("review_task_structure")
                && text.contains("proposed -> structured"),
            "grills each comment through the grill lifecycle (proposed -> structured)",
        );
        assert!(
            text.contains("no separate validation step"),
            "the grill has two states — a structured finding is coder-ready (no validate step, #31)",
        );
        assert!(
            text.contains("review_comment_post"),
            "replies in the comment with review_comment_post",
        );
        assert!(
            text.contains("NEVER resolve a human comment") || text.contains("never resolve"),
            "human-only resolution",
        );
        assert!(
            text.contains("recreate a PRD"),
            "explicit anti-pattern captured",
        );
    }

    #[test]
    fn render_coder_carries_work_rules() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Coder, None, &spec_id)
            .unwrap()
            .unwrap();
        assert!(text.contains("design-system"), "compose from design-system");
        assert!(text.contains("full suite"), "full suite once at the end");
        assert!(
            text.contains("kind=human task -> STOP"),
            "kind=human STOP rule",
        );
    }

    /// Review finding 01KY4ZJPNBJMYJDGWTMXWNYJZV: the Dev orchestrator's backlog capture is
    /// HUMAN-GATED — an out-of-scope follow-up is PROPOSED to the user and only `idea_add`ed AFTER
    /// they validate that specific add (never autonomous), and the Dev does NOT reconcile.
    #[test]
    fn render_dev_carries_human_gated_backlog_capture() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Dev, None, &spec_id)
            .unwrap()
            .unwrap();
        // PROPOSE to the user, then idea_add ONLY after they validate that specific addition.
        assert!(
            text.contains("PROPOSE it to the USER")
                && text.contains("only AFTER they validate")
                && text.contains("idea_add("),
            "Dev proposes the follow-up and captures only after the user validates it",
        );
        // A human gate on EVERY add — the anti-autonomous wording (no on-its-own-authority add).
        assert!(
            text.contains("human gate on EVERY add"),
            "every add is human-gated (backlog never fills with unvetted junk)",
        );
        assert!(
            text.contains("do NOT idea_add it on your own authority"),
            "Dev must NOT add autonomously",
        );
        // Dev does NOT reconcile — that curation belongs to /nspec.
        assert!(
            text.contains("You do NOT reconcile the backlog") && text.contains("/nspec"),
            "reconciliation (close/merge/regroup) is the Spec author's job, not the Dev's",
        );
    }

    /// Review finding 01KY4ZJQ6BCXHQZ8268P47ZGNH: the Coder is a non-interactive sub-agent — it
    /// SURFACES a discovered follow-up UPWARD to the dev orchestrator (final report / note, like a
    /// scope-gap) and never calls `idea_add` itself.
    #[test]
    fn render_coder_surfaces_followups_upward_not_idea_add() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Coder, None, &spec_id)
            .unwrap()
            .unwrap();
        // SURFACE upward to the orchestrator, in the final report or a note (same as a scope-gap).
        assert!(
            text.contains("SURFACE to the dev orchestrator"),
            "coder surfaces follow-ups upward to the orchestrator",
        );
        assert!(
            text.contains("final report") && text.contains("task note"),
            "surfaced via the final report / a task note",
        );
        // It is explicitly told NOT to capture directly (no autonomous idea_add).
        assert!(
            text.contains("you never call idea_add yourself")
                && text.contains("Never capture it directly"),
            "coder is told NOT to idea_add itself",
        );
        // The direct-capture form (with source=session) is the DEV's tool, never the coder's.
        assert!(
            !text.contains("source=\"session\""),
            "coder carries no direct idea_add capture instruction",
        );
    }

    /// Review finding 01KY4ZJQPZZPE21Y3QDSDAACP4: the Spec author keeps its existing SEARCH +
    /// auto-link AND now also RECONCILES the backlog (close resolved with the MANUAL motif, merge
    /// duplicates, regroup by theme) — auto-link and reconciliation being DIFFERENT actions.
    #[test]
    fn render_spec_carries_backlog_search_and_reconciliation() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, None, &spec_id)
            .unwrap()
            .unwrap();
        // The EXISTING search + auto-link wiring is preserved.
        assert!(
            text.contains("Search the idea backlog FIRST")
                && text.contains("idea_search(")
                && text.contains("kind=\"addresses\""),
            "search + auto-link (addresses) is kept",
        );
        // The NEW reconciliation: close (manual) / merge duplicates / regroup by theme.
        assert!(
            text.contains("RECONCILE the backlog while you author"),
            "reconciliation instruction added",
        );
        assert!(
            text.contains("idea_close(") && text.contains("idea_merge("),
            "close the resolved + merge duplicates",
        );
        assert!(text.contains("grouped by theme"), "regroup by theme");
        // close = MANUAL motif; implemented = AUTO (do NOT call idea_implement by hand).
        assert!(
            text.contains("do NOT call idea_implement by hand"),
            "manual close vs auto implemented is spelled out",
        );
        // auto-link and reconciliation are DISTINGUISHED as different actions.
        assert!(
            text.contains("DIFFERENT actions"),
            "auto-link vs reconciliation are clearly distinguished",
        );
    }

    #[test]
    fn render_auditor_is_rendered_per_axis() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let standards = render_agent_context(
            &db,
            AgentRole::Auditor,
            Some(AuditAxis::Standards),
            &spec_id,
        )
        .unwrap()
        .unwrap();
        assert!(
            standards.contains("AUDITOR · STANDARDS"),
            "axis in the header",
        );
        assert!(
            standards.contains("Your axis = standards") && standards.contains("CODING_STANDARDS"),
            "standards axis block",
        );
        assert!(
            !standards.contains("Your axis = spec\n"),
            "the spec axis block is not mixed in",
        );

        let spec_axis =
            render_agent_context(&db, AgentRole::Auditor, Some(AuditAxis::Spec), &spec_id)
                .unwrap()
                .unwrap();
        assert!(spec_axis.contains("AUDITOR · SPEC"), "axis in the header");
        assert!(
            spec_axis.contains("Your axis = spec") && spec_axis.contains("scope creep"),
            "spec axis block",
        );

        // No axis -> both descriptions offered so the reader picks one.
        let no_axis = render_agent_context(&db, AgentRole::Auditor, None, &spec_id)
            .unwrap()
            .unwrap();
        assert!(
            no_axis.contains("standards | spec (pick one)"),
            "axis-less auditor offers both",
        );
    }

    #[test]
    fn render_prototype_role_carries_light_throwaway_context() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Prototype, None, &spec_id)
            .unwrap()
            .unwrap();
        assert!(text.contains("Context: Prototype"), "prototype preamble");
        assert!(
            text.contains("THROWAWAY") && text.contains("ONE design question"),
            "throwaway proto answering one question",
        );
        // The two forms: UI variants OR a state-machine mini-app.
        assert!(
            text.contains("UI variants") && text.contains("state machine"),
            "variants / state mini-app",
        );
        assert!(text.contains("One command to launch"), "one launch command");
        // Light role: no heavy machinery (design-system rendering / distill / persisted rounds =
        // v2.7). The proto stays deliberately un-polished and un-persisted.
        assert!(
            text.contains("No persistence, no polish"),
            "light: no persistence/polish (heavy machinery stays out of scope)",
        );
    }

    #[test]
    fn spec_role_proposes_the_proto_and_waits_for_validation() {
        // T8 entry point: /nspec PROPOSES a proto when the subject is graphical and WAITS for the
        // human's explicit go before delegating to the prototype role.
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, None, &spec_id)
            .unwrap()
            .unwrap();
        assert!(
            text.contains("Prototype if it is graphical"),
            "proto proposed for a graphical subject",
        );
        assert!(
            text.contains("PROPOSE to the human") && text.contains("then STOP"),
            "proposes and stops until explicit acceptance",
        );
        assert!(
            text.contains("agent_context(role=prototype"),
            "delegates to the prototype role once accepted",
        );
    }

    #[test]
    fn render_doc_role_carries_doc_sync_context() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Doc, None, &spec_id)
            .unwrap()
            .unwrap();
        assert!(text.contains("Context: Doc-sync"), "doc-sync preamble");
        assert!(
            text.contains("Git range (since..until)"),
            "reads the Spec's git range",
        );
        assert!(
            text.contains("Your output is a REVIEW"),
            "output is a review the human validates",
        );
    }

    #[test]
    fn render_spec_role_carries_authoring_context_and_sibling_specs() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, None, &spec_id)
            .unwrap()
            .unwrap();
        // The written /nspec authoring context: preamble + exhaustive grill + autonomous slicing +
        // guard-rails + the proto proposal.
        assert!(text.contains("Spec Author"), "spec author preamble");
        assert!(
            text.contains("ONE question at a time"),
            "exhaustive grill rule"
        );
        assert!(
            text.contains("AUTONOMOUS") && text.contains("does NOT validate the tasks"),
            "autonomous decomposition rule",
        );
        assert!(
            text.contains("ready is sensitive"),
            "ready guard-rail present",
        );
        // Anti-duplication live weaving: the project's other Specs (here: none besides this one).
        assert!(
            text.contains("Existing Specs in the project (anti-duplication)"),
            "sibling-specs weaving section",
        );
        // The idea-backlog wiring lives in the shared spec-author preamble, so an anchored render
        // carries it too: search the backlog at Spec start, auto-link the resolved ideas.
        assert!(
            text.contains("Search the idea backlog FIRST") && text.contains("idea_search("),
            "the /nspec idea_search wiring (Step 0) is in the spec-author preamble",
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
        assert!(text.contains("ONE question at a time"), "grill rule");
        assert!(
            text.contains("AUTONOMOUS") && text.contains("does NOT validate the tasks"),
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

        // The instruction (Step 0): idea_search at the start, auto-link addresses, no duplicate Spec.
        assert!(
            text.contains("Search the idea backlog FIRST"),
            "Step 0 present in the preamble",
        );
        assert!(text.contains("idea_search("), "instructs idea_search");
        assert!(
            text.contains("kind=\"addresses\""),
            "auto-link the resolved ideas as addresses",
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

    /// The authoring fallback is SPEC-only: a NON-spec role with no resolvable Spec keeps its
    /// unchanged `invalid_id` contract (the other roles are untouched by finding #19).
    #[test]
    fn non_spec_roles_without_a_spec_still_error_invalid_id() {
        let _g = db_guard();
        let (db, _project_id, workspace_id, _cwd) = seeded_project_without_spec();
        let err = agent_context(&db, &json!({ "role": "dev", "workspace_id": workspace_id }))
            .unwrap_err();
        assert_eq!(err.code, RpcCode::InvalidId);
    }

    /// The REDESIGN (finding 01KXG68DN5DSCFDFA6C87NJHGV, preamble half): the spec-author
    /// preamble instructs grouping BY NAME via `task_add(phase="…")` and says the plan is
    /// materialized from those names — with NO separate `spec_set_phases` call and NO 1..N
    /// numbering to track.
    #[test]
    fn spec_preamble_instructs_task_add_by_phase_name() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Spec, None, &spec_id)
            .unwrap()
            .unwrap();
        assert!(
            text.contains("task_add(phase="),
            "preamble instructs grouping by phase NAME via task_add(phase=…)",
        );
        assert!(
            text.contains("first-seen"),
            "preamble explains the plan emerges from the names in first-seen order",
        );
        assert!(
            !text.contains("spec_set_phases"),
            "the removed spec_set_phases tool is NOT mentioned",
        );
        assert!(
            !text.contains("1..N"),
            "there is no 1..N numbering to track anymore",
        );
    }
}
