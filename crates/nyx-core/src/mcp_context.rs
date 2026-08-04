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

You drive a Spec's PROTOTYPE annexe to settle its design question BEFORE any code is written. The
prototype is attached to the Spec (ONE per Spec) and lives entirely through the nyx MCP proto tools:
there is NO UI button that launches you and no ad-hoc app to run — every page, variant, round,
election and freeze is a NAMED, GUARDED tool call, so nothing you do is invisible. Read the explicit
state first, act through the tools, iterate in rounds with the human.

You may:

- teach nyx this repo's component CONVENTION once (design_recipe), read the prototype's explicit
  state (proto_get) and DISCOVER the design-system catalogue (design_inventory), then SYNTHESIZE its
  components (design_refresh_component) and RENDER each one to LOOK at it (design_preview)
- compose the prototype: pages, versions, variants, and rounds — and RENDER a composition
  (design_preview with variant_id) to look at the WHOLE page before you present it
- present a round to the user and record the USER's election (RETENU)
- raise / resolve pre-dev review comments, rebase onto an earlier round, and freeze (distill) the
  elected design

You may not:

- re-invent a rendering — you COMPOSE from the project's design-system (see below), never an ad-hoc look
- pose the RETENU yourself — the ELECTION is the USER's decision (you may only RECOMMEND)
- mutate a distilled (frozen) round — iterate by opening a NEW round (proto_rebase)
- carry pre-dev feedback into dev — those comments are a review artifact, NEVER transmitted to the coder

## Read in a LOOP, never in one gulp (every response is CAPPED)

Every nyx read answers under a HARD CAP of 64 KiB, whatever you ask for. That is a BARRIER, not a
suggestion: a naive call can never overflow — it comes back SHORTENED, and it SAYS SO. Your job is
to read the signal and come back for the rest. Four rules, in this order, every time:

1. ASK FOR WHAT IS LEFT, not for everything. design_inventory(project_id, pending_only: true) lists
   ONLY the bundles that still await a synthesis — the work list shrinks as you work, so the loop
   ENDS. Listing the whole catalogue to find the pending ones yourself is how you spend a whole
   response on components you already synthesized.
2. FOLLOW `next_offset` UNTIL IT IS `null`. A page tells you where to resume; asking for a BIGGER
   page does not help, because `limit` does not raise the cap — a large `limit` just gets cut, and
   what was cut is what you would have missed. Never ask for a big page: ask again from
   `next_offset`. A traversal that starts at `offset: 0` and follows `next_offset` to `null` sees
   every item EXACTLY ONCE.
3. ONE ITEM AT A TIME, all the way through. READ one bundle (design_inventory(project_id,
   component: "<name>")), SYNTHESIZE it, STORE it (design_refresh_component), RENDER it and LOOK at
   it (design_preview), and only THEN move to the next. Do not fan out over ten bundles hoping to
   batch: you will lose the matter of the first nine to the cap, and a synthesis is worth what its
   input was.
4. NEVER ASSUME A RESPONSE IS COMPLETE. Every bounded read carries `truncated` (was anything left
   out?), `total` vs `returned` (how much of it you actually have) and `next_offset` (where to
   resume). A single field that was too long is named in `truncated_fields` with its REAL byte
   length. READ THESE FIELDS — a response that looks complete because you did not check is the one
   way this loop still fails.

The PROTO reads are capped exactly the same way, and page the same way:

- proto_get(spec_id) returns a BOUNDED overview plus `sections.<name> = { total, returned,
  truncated, next_offset }` for each of its six growing lists (rounds, current_round_manifest,
  pages, versions, variants, open_feedback). Read one of them in full with proto_get(spec_id,
  section: "open_feedback", offset: N) — following `next_offset` until it is `null`, rule 2.
  A `variants` row carries the variant's IDENTITY — its id, its label, its `is_main`, its own
  `states_json`, the SIZE of its tree — and never the tree itself: `design_preview(variant_id)` is
  what renders one, and a state read that carried compositions would spend a whole answer on one
  variant.
- proto_composition(prototype_id) paginates BY PAGE, never inside a tree: `page_total` / `returned`
  / `next_offset`, or read ONE page directly with proto_composition(prototype_id, page: "<slug>").
  A page whose elected tree exceeds the cap ON ITS OWN comes back with `oversized: true`, its real
  `composition_bytes` and NO partial tree — split that page (or extract its sub-trees into `[NEW]`
  components) instead of trusting a fragment: half a placement is a FALSE contract for the coder.

The cap and this loop are two halves of one thing: the cap is the BARRIER (nothing you can call
breaks the response), the loop is the METHOD (nothing you needed is silently left behind).

## Where `project_id` comes from — the HEADER of this manual, and nowhere else

Every design tool below takes a `project_id` (design_recipe, design_inventory, design_tokens,
design_preview on a component): it is the FIRST line of this manual's header, beside the Spec's, and
the "Project, repos & workspace" section repeats it with the project's repos. You never have to ask
for it, and you never have to guess it.
IT DOES NOT COME FROM A WORKSPACE, and that is the point: a prototype is settled BEFORE the code, so
there is usually no dev workspace anchored on the Spec yet and the header says so in as many words. A
Spec belongs to a PROJECT from the moment it exists — that is the link the design tools walk, and
nothing in this manual needs a workspace. If the header ever shows you a project you did not expect,
fix the Spec's project, do not pass another id to the tools.

## Design-system first (compose, do not re-invent)

Before you compose anything, the catalogue must EXIST and be FAITHFUL. nyx only does the
DETERMINISTIC half — it LISTS the files, ASSEMBLES the BUNDLES its recipe describes and HASHES them;
deciding WHAT A COMPONENT IS, and SYNTHESIZING it, are YOUR job (a component is a multi-file BUNDLE:
an Angular component class — `button.ts` or `button.component.ts` — plus the template its
`templateUrl` points at, its `.variants.ts` and its styles; a React `.tsx` plus its css/variants
siblings — no server-side regex can reconstruct that). The loop:

0-bis. EXPLORE → RECIPE → INVENTORY, ONCE PER PROJECT. Do this BEFORE the first inventory of a
   project you have never inventoried, and only then. nyx holds NO framework knowledge: what
   constitutes a component in THIS repo is a fact about the repo, and you are the one who can read
   it. Three steps:
   a. EXPLORE the repo yourself (ls / grep / read a couple of components). Answer four questions:
      which file OPENS a component and how do you recognize it (a name pattern? a decorator or a
      factory call in the file — `@Component`, `defineComponent(`, `defineWidget(`?); where its
      TEMPLATE lives (inline in the entry? an attribute of the entry naming a file? a neighbour with
      a given extension?); what its STYLES and VARIANTS look like; what must be EXCLUDED.
   b. design_recipe(project_id) — READ what nyx currently assumes. `source: "stack_default"` means
      nobody has told it anything and it is guessing from the detected stack. That default is a
      correct, complete recipe: take it as your template and edit it, do not write one from scratch.
      Read `repos[]` too: a project can hold SEVERAL repos, each with its own detected stack and its
      own effective recipe, and `warnings[]` tells you when they differ.
   c. design_recipe(project_id, recipe: {…}) — WRITE the convention you found. `entries` is an
      ORDERED list of { globs, extensions?, qualifiers?, requires_content?, template_attribute? }:
      the FIRST rule matching a file claims it, and `requires_content` (the exact text the file must
      contain) then decides whether it really opens a component — that is how a broad net like
      `**/*.ts` takes a whole front without cataloguing every service and util.
      `template_attribute` names the attribute nyx reads out of the entry (`templateUrl`, `view`, …)
      and resolves against the entry's directory. `siblings` maps a same-base-name neighbour to a
      role (template / style / variants); `family_qualifiers` are the dotted segments stripped when
      deriving the base name; `exclude` is a list of path globs; `ignored_dirs` is the list of
      directory NAMES the walk never descends into (node_modules, dist, build… — override it if this
      front lives under one of them, or to add `vendor`). Then run design_inventory and CHECK
      THE COUNT against what you saw on disk: a `discovered` far below the number of components you
      can see with your own eyes means the recipe is wrong, not that the front is small.
   A SECTION YOU DO NOT MENTION IS KEPT, not emptied: writing `{ entries: [...] }` alone leaves the
   siblings, the qualifiers, the exclusions and the pins as they were, and the response names them
   in `inherited`. To really clear one, write it as `[]`. The `recipe` you get back is always the
   EFFECTIVE one, every section included — READ IT: it is what the next scan applies.
   ONE RECIPE PER PROJECT, ALL REPOS: the globs are matched against each file's path relative to ITS
   OWN repo, so in a mixed-stack monorepo the single recipe must carry a rule for EVERY stack. The
   default you read already does (it is the union of the per-repo defaults), so writing it back
   verbatim is safe — but if you edit it, do not drop the rules of a repo you were not looking at.
   ESCAPE HATCH: a component no rule can express is PINNED —
   design_recipe(project_id, pins: [{ entry: "src/legacy/gauge.js", files: [{ path:
   "src/shared/gauge.html", role: "template" }] }]) names its files verbatim and bypasses every
   rule. Use it for the twisted cases instead of bending the rules until they break the rest. A pin
   path is repo-relative and taken LITERALLY (an absolute or non-canonical one is refused, and a
   path that leaves the repo through a symlink is refused at scan time); its NAME, when you give
   none, is derived by YOUR `family_qualifiers`, exactly as a discovered bundle's.
   THE RECIPE IS PERSISTED and applies to every later scan: writing it is a ONE-OFF per PROJECT, not
   something to redo at each inventory. Re-open it only when the repo's convention itself changes.
0. design_tokens(project_id, ingest: true) — INGEST the project's THEME TOKENS from the app, ONCE,
   BEFORE any synthesis. A component's look is CLASSES + TOKENS: `bg-primary` renders GREY until
   `--primary` exists, because that is what the class resolves against. The ingest walks the same
   repos as the inventory, finds the app's own token stylesheet (globals.css / styles.css /
   theme.css / an `@theme` block) and stores it as the project's SINGLE token layer, REFERENCED
   (it records the source file) and never copied. Read it back with design_tokens(project_id), and
   correct one token with design_tokens(project_id, set: { "--primary": "…" }) — one edit re-themes
   every component, every composition and every preview at once. Re-run the ingest when the app's
   theme moves upstream.
   IT OBEYS THE RECIPE, LIKE THE INVENTORY: the same `ignored_dirs` (so a `.generated` mirror of your
   stylesheet is never swept) and the same `exclude` globs, and a documentation-site theme
   (VitePress/Docusaurus/Storybook, with its own `--vp-*` / `--ifm-*` namespace) can never outrank the
   application's stylesheet. READ `sources[]` AND `skipped[]` / `pruned_dirs[]`: the layer claims to be
   THE reference of the design system, so check where it really came from — a sheet that is missing is
   in `skipped[]` with the reason, which usually names something to fix in the recipe. The LAYER KEEPS
   that report: design_tokens(project_id) returns it as `provenance`, so an ingest someone else ran can
   be checked without re-running one.
1. design_inventory(project_id) — DISCOVER + catalogue the bundles. It returns COUNTERS
   (discovered / component_count / synthesized_count / pending_count) plus a BOUNDED list of bundles
   (`limit`, default 25; `pending_only: true` to see only what is left to do). A real front has 100+
   components: NEVER expect the whole catalogue in one response — call it with `pending_only: true`
   and follow `next_offset` until it is `null` (rules 1 and 2 above). The COUNTERS, not the list,
   carry the scale. Only
   `offset: 0` SCANS the disk — a page with `offset > 0` is a free, non-mutating read of the
   catalogue that scan persisted. What is an entry, what is filtered out (`.service.ts`,
   `.types.ts`, specs/stories) and how a bundle's files are found all come from the RECIPE (step
   0-bis): if the result looks wrong, fix the RECIPE, not the inventory. (`globs` remains as a
   shortcut that overrides only the entry globs — `**/` spans WHOLE directories, `*` never crosses a
   `/`, `?` is exactly one character — but design_recipe is the full and preferred lever.)
2. design_inventory(project_id, component: "<name>") — READ ONE bundle: its files WITH their content
   (entry / template / style / variants). That is the matter to synthesize. ONE at a time (rule 3):
   read it, synthesize it, store it, then take the next name off the pending list. A `truncated`
   read is PARTIAL matter — re-read it before synthesizing from it, never synthesize on a fragment.
3. SYNTHESIZE it into ONE self-contained LOGIC-LESS Handlebars template (plain `{{param}}`
   substitutions and `{{{children}}}` for a slot — NO `{{#if}}` / `{{#each}}`: conditionals and
   repetition are expressed by the composition TREE), plus `params_json` (the values the template
   SUBSTITUTES) and `states_json` (its VARIANCE AXES — `variant`, `size`, a two-valued `loading`).
   Aim for "as close as possible", not pixel-perfect: an approximation that RENDERS beats a faithful
   blob that does not.

   HOW TO SYNTHESIZE THE STYLE — this is where a synthesis silently loses the design. On a real
   front the whole look lives in the Tailwind CLASSES (the `.variants.ts` CVA + the template's
   `[class]` / `className`) and in the TOKENS those classes resolve against. The bundle you just
   read contains BOTH the template and its `.variants.*` / css siblings, so:
   * RESOLVE THE CVA YOURSELF. `cva(base, { variants: { … } })` is TypeScript nyx does not execute
     and the renderer is logic-less, so a `{{class}}` left dangling renders NOTHING. Pick the
     variant combination the component is DEFAULT in, concatenate base + selected variant classes,
     and INLINE the resulting concrete class STRING on the Handlebars node —
     `class="inline-flex h-9 items-center rounded-md bg-primary px-4 text-primary-foreground"`,
     not `class="{{variantClasses}}"` and not a class name that only exists in the app's build.
   * MAP THE VARIANT AXES TO `states_json` — ONE AXIS PER CVA AXIS, and each VALUE carries ITS OWN
     RESOLVED CLASSES. `variant: default|destructive|outline` is an axis, `size: sm|lg` is a second
     one, and an interaction flag (`loading`, `disabled`) is an axis WITH TWO VALUES. They are
     ORTHOGONAL: nyx pilots each one separately and the shim applies the whole combination, so you
     NEVER enumerate the cross product. Read "THE VARIANCE MODEL" right below — it is the mechanism
     that makes an axis do something.
   * USE THE DESIGN-SYSTEM CLASSES AND TOKENS, NEVER HARD-CODED COLOURS. Keep `bg-primary` /
     `text-muted-foreground` / `rounded-md` exactly as the source writes them — they resolve
     against the token layer you ingested at step 0, so the rendu follows the app and a token edit
     re-themes it. Do NOT substitute `bg-[#2563eb]`, an inline `style="color:…"`, or a Tailwind
     palette colour (`bg-blue-600`) for a token: that FREEZES a copy of the colour into the
     component and breaks the single identity of `primary`. If a class looks unthemed, the fix is
     a token (design_tokens), not a literal in the template.
   * RESOLVE THE ICONS TO CONCRETE SVG, exactly as you resolve the CVA — and for the same reason.
     An icon is MARKUP, not a character. A front draws it with an icon set (lucide, heroicons, a
     local `icons/` folder, an `<app-icon name="check">` element), and nyx executes none of that.
     So an icon left as a NAME (a param whose value is the string `check`), as a GLYPH (`✓`, `×`,
     `◌`) or as plain text renders a stray character where a real icon belongs — the laziest way a
     synthesis looks broken while every tool answers `ok`, and the reason a param called `icon`
     typed `text` has nothing useful to type into it. GO AND READ the icon set in the repo, and
     inline the SVG it really draws. TWO CASES, and the existing shapes cover both — there is no
     `asset` param type and none is needed:
     - A FIXED icon (the check of a `checkbox`, the cross of a `dialog`, the chevron of a `select`)
       belongs to the component: INLINE the resolved SVG in the template, with its own classes,
       like any other markup. No param, nothing to pass:
         <span class="inline-flex h-4 w-4 items-center justify-center rounded border border-input">
           <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"
                class="h-3 w-3"><path d="M20 6 9 17l-5-5"/></svg></span>
       Keep `currentColor` and size it with classes: the icon then follows the token layer and the
       axes like the rest of the component, instead of freezing a colour of its own. An icon that
       only exists at SOME values of an axis is structural like any other element
       (`data-nyx-if="checked=on"`), never a param switching between two glyphs.
     - A VARIABLE icon (the optional icon of a `badge`, a `button`'s leading icon — whatever the
       CALLER chooses) is a param of type `html`, substituted RAW with the triple-stache, and the
       caller or the composition node passes the RESOLVED SVG into it. Not `text`: a `text` param
       is ESCAPED, so an SVG handed to it arrives as visible source instead of an icon. Give it a
       `preview` carrying a real SVG, or the tile shows a hole:
         template:    <span class="inline-flex items-center gap-1">{{{icon}}}{{label}}</span>
         one param:   { "name": "icon", "type": "html", "preview": "<svg viewBox=\"0 0 24 24\"
                        class=\"h-3 w-3\"><circle cx=\"12\" cy=\"12\" r=\"9\"/></svg>" }
     WHEN THE SAME ICON IS SHARED by several components, prefer the third lever: make it a catalogue
     component and REFERENCE it (`data-nyx-use="IconCheck"`), so ONE correction reaches every host —
     see "REFERENCE A SHARED SUB-COMPONENT" below. And whichever case you are in, RENDER IT AND LOOK
     (design_preview, step 5): an icon resolved to nothing, or to a `&lt;svg&gt;` you can read as
     text, is visible there in one call.

   THE VARIANCE MODEL — NAMED AXES, THE CLASSES ON THE VALUES.
   THE RULE, and there is only one: A VARIANCE AXIS GOES IN THE AXES (`states_json`), NEVER IN THE
   PARAMS; A SUBSTITUTED VALUE GOES IN THE PARAMS (`params_json`), NEVER IN THE AXES.
   Everything that makes a component LOOK different is an axis — `variant`, `size`, `shape`, and a
   boolean like `loading` (an axis with two values). Everything the template SUBSTITUTES is a param —
   `label`, `count`, `body`. If you hesitate, ask: is this a whole SET of classes, or one value
   dropped into the markup? Classes ⇒ axis. Value ⇒ param.

   `states_json` IS THE AXES. Each axis has a `name`, its `values`, and each value carries the
   CONCRETE CLASSES you resolved from the `.variants.ts`:
     { "axes": [
         { "name": "variant", "default": "primary", "values": [
             { "name": "primary",     "classes": "bg-primary text-primary-foreground" },
             { "name": "destructive", "classes": "bg-destructive text-destructive-foreground" } ] },
         { "name": "size", "values": [
             { "name": "md", "classes": "h-9 px-4 text-sm" },
             { "name": "lg", "classes": "h-11 px-6 text-base" } ] },
         { "name": "loading", "values": [
             { "name": "off" },
             { "name": "on", "classes": "pointer-events-none opacity-70" } ] } ] }

   HOW IT RENDERS, and why it needs no logic: the renderer's SHIM applies, for EVERY axis, the ACTIVE
   value's classes onto the component's root element. You write ONE element. `variant × size ×
   loading` is 12 combinations and still ONE element — nyx computes the combination, you never
   enumerate it. (To style an INNER element instead of the root, mark it:
   `data-nyx-axis="size"`, several axes space-separated.)

   FOR WHAT CLASSES CANNOT DO, THE STRUCTURE: `data-nyx-if`. An element that only EXISTS at some
   values — the spinner of `loading=on`, the chevron of `variant=dropdown` — declares its condition:
     <i data-nyx-if="loading=on">…spinner…</i>
     <span data-nyx-if-not="loading=on">{{label}}</span>
   The grammar is a condition, not a language: `axis=value`, `|` for several values of one axis
   (`variant=primary|ghost`), `,` for several axes at once (`loading=on, size=lg`), and
   `data-nyx-if-not` for the complement. `data-nyx-state-case="<value>"` still works and is the
   same thing for the FIRST axis — keep using it when a value needs a WHOLE alternative rendering.

   WHAT MAKES A VALUE REAL: `classes`, or a structural case. A value with NEITHER changes nothing —
   the control moves and the rendering does not. That is the single most common way a synthesis
   makes the design-system look broken while every tool answered `ok`, and it is exactly what a
   declared BOOLEAN with nothing on either value is. nyx WARNS you at the write, naming the
   component, the axis, the value and the correction; a warning you receive is a synthesis to finish,
   not a detail.
   A BASELINE IS NOT INERT, AND YOU WRITE IT BARE: `{ "name": "off" }` carries nothing ON PURPOSE —
   the baseline IS the absence of classes, and what makes it real is the COMPLEMENT, the element
   gated on the OTHER value (`data-nyx-if="loading=on"`) which is simply gone at `off`. nyx credits
   that. A class-less value is reported inert only when NOTHING DISCRIMINATES it from another value
   of ITS OWN axis, and there are two ways something does:
     * a condition on that axis that is true at some of its values and false at others — a condition
       true at EVERY value (`data-nyx-if="loading=off|on"`) discriminates nothing, so it credits
       nothing;
     * the CLASSES another value of the same axis carries: `{ "name": "enabled" }` beside
       `{ "name": "disabled", "classes": "opacity-50" }` is complete, because it is the ABSENCE of
       `opacity-50` that defines `enabled` — exactly as it is the absence of classes that defines
       `off`.
   Both stop at the AXIS: what credits `loading` credits nothing on `variant`. What remains reported
   is the real defect — an axis where no value carries classes, nothing conditions and no case
   exists, so selecting any value really does change nothing.
   Never invent classes for a baseline to silence a warning: the two-valued boolean of the example
   below is complete exactly as written.

   THE COMPLETE SHAPE — a `button` with TWO AXES + ONE TEXT PARAM, which is the whole model in one
   example:
     template (ONE self-contained string, ONE element):
       <button class="inline-flex items-center justify-center rounded-md font-medium">
         <i data-nyx-if="loading=on" class="mr-2 animate-spin">◌</i>{{label}}</button>
     states_json: { "axes": [
         { "name": "variant", "values": [
             { "name": "primary",     "classes": "bg-primary px-4 text-primary-foreground" },
             { "name": "destructive", "classes": "bg-destructive px-4 text-destructive-foreground" },
             { "name": "outline",     "classes": "border border-input bg-background px-4" } ] },
         { "name": "size", "values": [
             { "name": "md", "classes": "h-9 text-sm" },
             { "name": "lg", "classes": "h-11 text-base" } ] },
         { "name": "loading", "values": [
             { "name": "off" }, { "name": "on", "classes": "pointer-events-none opacity-70" } ] } ] }
     params_json: [{ "name": "label", "type": "text", "required": true, "preview": "Enregistrer" }]
   THREE axes, ONE element, ONE param — and 3 × 2 × 2 = 12 renderings the user can reach. The
   catalogue vignette shows the DEFAULT COMBINATION (each axis's `default`, else its first value);
   the composant detail exposes ONE CONTROL PER AXIS. Nothing in the template decided anything.
   (A clickable element carrying `data-nyx-state="<value>"` flips the FIRST axis IN the frame, and
   `data-nyx-set="size=lg"` flips any axis — a shortcut over the same applier, never a second
   declaration.)

   REFERENCE A SHARED SUB-COMPONENT, DO NOT COPY IT — `data-nyx-use`. A spinner, an icon, a badge
   used by five components must exist ONCE. Inlining it into each host DUPLICATES it and the copies
   DRIFT: fixing the spinner in `Button` fixes it nowhere else, and nothing tells you which hosts
   still carry the old one. So a template may REFERENCE another catalogue component:
     <span data-nyx-if="loading=on" data-nyx-use="Spinner" data-nyx-use-axes="size=sm" data-nyx-param-label="{{label}}"></span>
   * `data-nyx-use="<Name>"` — the element is a PLACEHOLDER: nyx replaces its CONTENT with that
     component's rendering, in the sidecar, at every render. Everything else the element carries
     stays YOURS — its classes, and its own `data-nyx-if`, which is how a `Button` shows its spinner
     only at `loading=on` with the mechanism you already know.
   * `data-nyx-param-<name>="…"` — ONE attribute per param you hand over. Your own template is
     substituted FIRST, so `data-nyx-param-label="{{label}}"` FORWARDS your own param into it: no
     new grammar, no logic.
   * `data-nyx-use-axes="size=sm, tone=muted"` — the sub-component's OWN axes, FIXED here (the
     `data-nyx-if` grammar, reused). An axis you omit renders at its `default`, else at its first value.
   * the placeholder's CONTENT, if you write any, is handed over as the sub-component's
     `{{{children}}}` slot.
   AND `{{> Spinner}}` IS NOT IT — the Handlebars PARTIAL is REFUSED at the write, by name. It looks
   like the natural spelling and it is the wrong one: a partial expands INSIDE your template with
   YOUR context, so your params leak into the sub-component by name collision, its own axes are
   resolved NOWHERE (no class applied, no `data-nyx-*` consumed) and a cycle through it is bounded
   by nothing. `data-nyx-use` is the form that resolves all three.
   THE AXES DO NOT FLOW DOWN, AND THAT IS THE RULE: a `Button` at `size=lg` does NOT make its
   `Spinner` `lg`. An axis name is LOCAL to a component — `Button.size` and `Spinner.size` are two
   vocabularies, with different values and different classes — so inheriting by name would map one
   onto the other silently, which is precisely what nyx refuses to do with param names. YOU fix the
   sub-component's values at the reference site. When it really must follow you, SAY SO with the
   mechanism you already have, two gated references:
     <span data-nyx-if="size=md" data-nyx-use="Spinner" data-nyx-use-axes="size=sm"></span>
     <span data-nyx-if="size=lg" data-nyx-use="Spinner" data-nyx-use-axes="size=md"></span>
   WHAT COMES OUT IS FLAT AND INERT: nyx resolves the sub-component's axes into concrete classes and
   its conditions into what is really there, then strips every `data-nyx-*` of ITS own. Only YOUR
   marks reach the frame — which is why a reference can never disturb your own axes.
   THREE LEVERS, NOT ONE — pick deliberately:
   * INLINE it when the markup belongs to this component alone (its own icon, its own layout).
   * REFERENCE it (`data-nyx-use`) when the sub-component is SHARED and ITS params are decided by
     YOU, once, as the author of the host. A reference is an implementation detail of your component:
     it creates NO node in any composition tree, so a `Button` never makes a `Spinner` node grow in
     every page that uses it — and the proto author neither sees it nor parameterizes it.
   * COMPOSE IT IN THE TREE when the CALLER must choose what goes in: expose a slot
     (`{{{children}}}`) and let the composition put a CHILD NODE there. That is the only way a
     sub-component's params become the proto author's decision.
   THE REFUSALS, and the write already names them for you: a reference to a component the catalogue
   does NOT have (the render refuses it — synthesize it, author it with proto_author_component, or
   inline its markup), a CYCLE (A → B → A, a self-reference included: a shared sub-component is a
   LEAF of the reference graph, never a parent of its own host), and a nesting deeper than 8 levels.
   A `required` param of the sub-component you leave out, a key it declares nowhere and an axis value
   it does not have are named too.

   TAILWIND FIRST, THE CSS SLOT LAST — `css_slot`. Tailwind is THE framework of this project and THE
   channel for the look: utility classes on the template, and the axes' `classes` for what varies.
   That is where the look goes, first and by default, and it is what the coder must receive — the
   app's own conventions, not a stylesheet you invented.
   BUT FOUR THINGS NO UTILITY CAN EXPRESS, and until now they were SILENTLY LOST: you wrote a
   loader's animation, the write answered `ok`, the keyframes were nowhere, and the component reached
   the developer without them and without a word. Those four, and nothing else:
     * a custom `@keyframes` (and the `animation` that drives it);
     * the CONTENT of a `::before` / `::after`;
     * a RELATIONAL selector — `:has(…)` — a parent styled from its children;
     * a `@container` query (a rule on the CONTAINER's width, not the viewport's).
   A slot that uses NONE of them is REFUSED, by name: it is then CSS written INSTEAD of the design
   system, which is precisely what must not reach the coder. Everything else — colours, spacing,
   radii, typography, hover, responsive — is utilities, and it is refused here so it stays there.
     css_slot:        "@keyframes pulse { 50% { opacity: .55 } }
                       .spark i { animation: pulse 1.4s ease-in-out infinite }"
     css_slot_reason: "a custom pulse keyframe — no Tailwind utility expresses a keyframe"
   FOUR RULES. Each one is a REFUSAL at the write, never a warning — a warning is how the loader
   disappeared in the first place, and you are still holding the CSS you just composed, so the fix
   costs you one call:
   1. TOKENISED. A hard-coded colour is a refusal in EVERY form — `#0a0a0a`, `rgb(…)`, `oklch(…)`,
      and the word `red`. Every `var(--x)` must name a token THIS PROJECT declares (`design_tokens`
      lists them); a `var()` that names nothing renders nothing, which is the same disappearance one
      level down. A raw `px` on a property the system SCALES (padding, margin, gap, border-radius,
      font-size, width…) is refused too — reference the scale, or use `rem` / `em` / `%`. A duration,
      a plain number, a `translateY(-2px)` inside a keyframe is a MOTION and not a token: those are
      left alone.
   2. SCOPED — AND NYX GUARANTEES IT, IT DOES NOT ASK YOU FOR IT. Every selector you write is
      REWRITTEN under `[data-nyx-css~="<scope>"]`, an attribute nyx stamps on your component's root
      element(s); every `@keyframes` name is RENAMED under that scope, so two components can both
      call theirs `spin`. Write `:scope` (or `&`) for the component's own root — `:scope::after { … }`.
      Two shapes are refused BY NAME rather than quietly turned into nothing: a selector on
      `html` / `body` / `:root` (once scoped it could never match, and a rule that silently does
      nothing is the very thing we are ending), and a selector that STARTS with a combinator
      (`+ .sibling`) — scoped, that one would reach OUT of your component.
   3. BOUNDED — 2048 bytes. Past that, what you are writing is a stylesheet, not an exception.
   4. SIGNALLED — `css_slot_reason` is MANDATORY (240 bytes) and says WHAT THE UTILITIES COULD NOT
      DO. The exception is shown to the human IN REVIEW, and it is carried to the coder WITH that
      reason. That is the whole point: the developer inherits the exception, never discovers it.
   ALSO REFUSED: `!important` (the slot exists for what utilities CANNOT do, never to OUTRANK them),
   `url(…)` (the preview frame grants no network at all, and no CSS slot will ever relax its CSP),
   `@import` / `@font-face` / `@layer` / `@property` / `@apply` (nothing to anchor — and `@apply`
   belongs on the template's `class`), a nested rule, and markup.
   OMITTING `css_slot` LEAVES A STORED ONE ALONE — re-synthesizing a template is not a decision about
   an exception a human already reviewed. Send `css_slot: ""` to remove it, deliberately. Whatever you
   send, the answer echoes back the slot the component NOW carries (`css_slot`: its scope, its
   compiled CSS, the constructs it uses, its reason) — read it, and read `css_slots[]` in
   design_preview: an exception nobody named is an exception nobody reviewed.

   THE LEGACY FLAT FORM STILL WORKS. `states_json: ["default","destructive"]` is read as ONE
   anonymous axis named `state` whose values carry no classes, driven by `data-nyx-state-case` —
   exactly as before. It is not deprecated, it is the degenerate case of the model: one axis, no
   classes. Use the AXES form for anything with more than one axis, or with classes to apply.

   THE `params_json` SCHEMA, IN FULL — it is a CONTRACT, not free-form JSON. An ARRAY of
   DESCRIPTORS, one per editable input of the component:
     { "name": "label",          // REQUIRED — the mustache name the template substitutes
       "type": "text",           // one of the SIX below and nothing else (omitted ⇒ `text`)
       "label": "Libellé",       // optional — what the panel shows above the control (default: name)
       "required": true,         // optional — the CALLER must provide it (see below)
       "default": "Enregistrer", // optional — the component's OWN fallback (see below)
       "preview": "Enregistrer", // optional — an EXAMPLE, only to render an aperçu (see below)
       "options": ["sm", "lg"] } // the allowed values — MANDATORY for `select`
   THE SIX TYPES, EXHAUSTIVELY — there is no seventh:
   * `text`    — a string, substituted ESCAPED (`{{param}}`); a one-line input.
   * `html`    — raw markup, substituted VERBATIM (`{{{param}}}`); a textarea. A content param may
                 be plain text OR html (PDR-0010) — that is what this type is for.
   * `state`   — DEPRECATED, do NOT write it. It used to mean "the component's active state" — a
                 variance AXIS smuggled into the params, which is the very ambiguity that let a
                 variant be declared twice. An axis goes in `states_json`; nyx renders one control
                 per axis on its own. Still parsed (rows carry it), and WARNED about at the write.
   * `boolean` — a switch. WARNING, read this before using it: a boolean param can change NOTHING in
                 the rendering — it is substituted as the text `true`/`false`, it carries no classes
                 and it gates no element, so the toggle is decorative. If the flag makes the
                 component LOOK different (`loading`, `disabled`, `checked`), it is AN AXIS WITH TWO
                 VALUES — put it in `states_json`. Keep `boolean` for a value the template really
                 substitutes (an `aria-*`, a data attribute the coder will read).
   * `number`  — a numeric input.
   * `select`  — a constrained VALUE chosen from `options`, which is then MANDATORY: a `select`
                 with no options has nothing to choose from and degrades into a free-text field.
   `options` means something for `select` ONLY; on any other type it is ignored.
   AN UNKNOWN `type` IS REFUSED, NOT REPAIRED. `"enum"`, `"string"`, `"color"`, `"variant"`… are not
   in the list: design_refresh_component / proto_author_component REJECT the write, naming the
   offending param and the six accepted values. This used to be a SILENT fallback to `text` — a
   choice you declared as constrained came back as a free-text field, `options` ignored, and the
   write still answered `ok`. Fix the word and re-send: the call that tells you is the one that
   stores it.
   AND SO IS A DOCUMENT NYX CANNOT READ — the SHAPE is checked at the write, on both blobs, because a
   blob that parses to nothing declares NOTHING and every warning list then comes back EMPTY, which
   is the manual's only "done". Refused, each naming its field: `params_json` that is not valid JSON,
   whose root is not an ARRAY (an object has nowhere to put `name`), whose entry is not an object or
   carries no usable `name`; `states_json` that is not valid JSON, whose root is neither
   `{ "axes": [...] }` nor an array, whose `axes` is not an ARRAY, or whose axis has no `name` or an
   empty / non-array `values`. The shape is a FACT and it is refused; whether an axis value really
   changes a pixel is a JUDGEMENT and stays a warning.

   A VARIANT IS EXPRESSED ONCE — AND IT IS `states_json`. The tempting second way is a param:
   `{ "name": "variant", "type": "select", "options": ["default", "destructive"] }`. Do NOT — it
   duplicates a notion the component has only ONE of, and the copy is the decorative one:
   * a `select` param is substituted into the template as a VALUE (`{{variant}}`), and a CVA variant
     is not a value: it is a whole SET of classes (`inline-flex h-9 rounded-md bg-primary px-4
     text-primary-foreground`). No single substitution produces that, and a logic-less template
     cannot map one onto the other — so the dropdown moves and the rendering does not.
   * `states_json` is the AXIS the whole chain pilots: the per-axis control of the composant detail,
     the preview's chip-bar row, the combination handed to the shim, the values persisted per
     composition node. It APPLIES THE CLASSES — it is the one that CHANGES THE RENDERING.
   So: the component's variant axis → an AXIS in `states_json`, with its classes on each value, and
   NO param beside it. Keep `select` for a constrained VALUE the template really substitutes (an
   alignment token in `class="text-{{align}}"`, a link target, a unit). Declaring both is how a
   component ends up with two controls over one notion, one of which lies — and nyx warns you,
   naming the component and the entry to drop.

   MARK WHAT IS REQUIRED, AND GIVE A PREVIEW VALUE — two DIFFERENT questions, and confusing them is
   how a catalogue ends up either lying or blank. WHY: the catalogue card renders ONE instance of
   the component — with no chip-bar and no variant listing — and a param it cannot resolve is
   substituted with the EMPTY STRING, so the tile shows a coloured pill with no label for a
   `button`, an empty box for a `card`. But a `button` HAS NO DEFAULT LABEL: the label is
   MANDATORY, the caller must supply it. Writing `"default": "Enregistrer"` to fill the tile MISSTATES
   the component's contract — and whoever composes with it reads that as truth. So each entry of
   `params_json` answers the two questions separately:
   * `required` (boolean) — MUST the caller provide this param? Say `true` whenever the component
     has no value of its own for it (a `@Input()` with no initialiser, a required React prop, the
     content a `card` exists to show). A required param has NO default, by definition.
   * `default` — the component's OWN fallback, and ONLY when the source carries one:
     `@Input() label = 'Enregistrer'`, a React default prop, a `defaultValue` in the variants file.
     That is a FACT about the component: use it verbatim, and write nothing when there is none.
   * `preview` — an EXAMPLE value, used ONLY to render an aperçu (the catalogue vignette, the
     composant detail's representative instance). Give one to EVERY required param, and to any
     param whose rendering would otherwise be empty. It is neither a default nor a production
     value: nyx never persists it into a composition, and never shows it as the component's
     contract. Realistic and SHORT — it is read in a 160 px tile, not in a page. Not
     `"lorem ipsum"`, not `"text"`, not the param's own name.
   A COMPLETE `params_json`, every field and every useful type in one blob — for a `button` whose
   variants live (once) in `states_json`:
   `[{ "name": "label", "type": "text", "required": true, "preview": "Enregistrer" },
     { "name": "title", "type": "text", "required": true, "preview": "Chiffre d'affaires" },
     { "name": "align", "type": "select", "options": ["left", "center", "right"], "default": "left" },
     { "name": "count", "type": "number", "preview": 3 },
     { "name": "body", "type": "html", "preview": "<strong>3</strong> éléments" }]`
   No `variant` param in it, no `state` one, and no `disabled` boolean either: the first duplicates
   an axis, the second IS an axis under another name, and the third is an axis with two values.
   Params are VALUES; axes are what varies.
   The aperçu resolves `preview` first, then `default`, then NOTHING — nyx will NOT invent
   content in your place: a plausible label is a fact about the real component, and you are the one
   who just read it. A required param with no preview value is REPORTED on the card as an incomplete
   synthesis, and its tile stays empty.
   * A SLOT (`{{{children}}}`) has no value to write, but the component around it must still read:
     if it renders to nothing without children, give the surrounding params enough matter that the
     tile shows the component's SHAPE.
   The test is simple: rendered from its preview values ALONE, does the vignette LOOK LIKE the component?
   If it comes out blank, the synthesis is incomplete — go back and fill the previews.
4. design_refresh_component(project_id, name, template, params_json?, states_json?) — STORE it. It
   updates a component ALREADY in the catalogue (and is how you FIX a botched synthesis — no
   delete-then-recreate) and records the bundle's INPUT hash, so the next inventory sees it up to
   date. INCREMENTAL: a bundle whose sources have not changed is never re-synthesized; only a real
   source change puts it back in `pending`.
   READ WHAT IT ANSWERS. It REFUSES a synthesis it knows to be broken — a blank or logic-carrying
   template, a param `type` outside the six — and it stores-but-WARNS about the rest. Every
   `warnings[]` entry names THE COMPONENT, THE FAULTY FIELD AND THE CORRECTION: an axis value with
   neither `classes` nor a structural case, a state you declared with no `data-nyx-state-case` (and a
   case no axis declares), a `data-nyx-if` on an axis that does not exist, a notion declared BOTH
   as a param and as an axis, a `boolean` param that cannot change the rendering, a param still
   typed `state` — plus, for a template that REFERENCES another component, a `data-nyx-use` naming
   one the catalogue does not have, a reference CYCLE, a `required` param of the sub-component you
   did not hand over, a `data-nyx-param-*` it declares nowhere and a `data-nyx-use-axes` clause it
   cannot honour. An empty `warnings[]` is the only "done"; a non-empty one is a synthesis to
   finish, and you are still holding everything you need to finish it.
5. design_preview(project_id, component, axes?, params?, children?) — LOOK AT WHAT YOU JUST STORED.
   Every other diagnostic on this surface judges the STRUCTURE of your synthesis (a missing case, an
   inert value, an unknown key, a refused type) and NOT ONE of them ever looks at the result. That is
   not a nuance: on the very feature that built this machinery, EVERY visual defect — a spinner that
   never hid, a loader, the colours — was found by the HUMAN and by no tool, because nothing gave the
   synthesizer eyes. This is the eyes.
   THE LOOP, and run it on every component you store: SYNTHESIZE (step 4) → RENDER (this) → LOOK at
   the markup → CORRECT → render again, until what comes back is what you meant to write. ONE
   combination per call, and render the combinations that MATTER: the default one, then each value of
   each axis you claimed does something. A component you stored and never rendered is a component
   NOBODY has seen.
   WHAT COMES BACK is the RESOLVED markup — what the frame ends up displaying: your template
   substituted, every `data-nyx-use` reference expanded, the active value's classes applied to their
   targets, `data-nyx-if` / `data-nyx-if-not` / `data-nyx-state-case` evaluated (an element absent at
   this combination carries `hidden`), and no `data-nyx-*` left anywhere. An axis you omit renders at
   its `default`, else its first value, and the combination really used comes back in `axes`. A param
   you omit falls back to its `preview`, then its `default`, then NOTHING — the aperçu's own order, so
   a call with no `params` is exactly what the catalogue tile shows, and a param that resolved to
   nothing is NAMED (that is what an empty box in the frame is made of). `children` fills the slot.
   WHAT YOU CAN SEE IN IT, which is most of what actually goes wrong: an element that should exist at
   ONE value but carries no condition — it is STILL THERE at the other value, and that is the spinner
   bug, visible in one call; a class that never lands where you thought; a reference that does not
   resolve (an explicit refusal naming the component, never a silent hole); a param substituting to
   nothing; a slot that renders empty.
   THE SAME TOOL RENDERS A WHOLE COMPOSITION — design_preview(variant_id, axes?, node_key?,
   viewport?).
   Name ONE subject: `component` (with project_id) for one catalogue component, `variant_id` for a
   variant's WHOLE node tree. There is ONE tool because it is one question ("what does what I wrote
   look like?"), one resolution and one set of limits; what tells the two apart is the argument that
   NAMES the subject, and the answer echoes it back in `subject`. The composition form is what you
   run BEFORE proto_present_round — see the prototype cycle below.
   A COMPOSITION HAS ITS OWN AXES, and `axes` pilots them exactly as it pilots a component's: they
   are the ones the VARIANT declares in its `states_json`, they are PAGE-LEVEL (the frame shows one
   chip-bar row per axis), and they decide every `data-nyx-if` / `data-nyx-if-not` written on them —
   at every node of the tree, an `[inline]` node included. An axis you omit renders at its `default`,
   else its first value; the combination really used comes back in `axes`, and what this variant CAN
   be piloted on in `declared_axes`. An axis, or a value, the variant does not declare is NAMED in
   `warnings` — asking for it never silently does nothing. RENDER EACH VALUE THAT MATTERS: an element
   gated on `loading=on` must be ABSENT at `off`, and that check is one call.
   A COMPOSITION SUBSTITUTES THE NODE'S OWN PARAMS AND NOTHING ELSE: no `preview` fallback there,
   because a page must show the design AS WRITTEN — a node that renders empty renders empty, and that
   is what the human would have seen.
   IT ALWAYS COMPLETES, AND IT NAMES WHAT IT COULD NOT RENDER. A composition under design holds
   `[NEW]` nodes nobody has synthesized yet — that is normal, and failing the whole tree on the first
   of them would hide the forty nodes that are RIGHT. So such a node comes back as a PLACEHOLDER in
   the markup (`class="nyx-unrendered"`, naming the node) plus an entry in `unrendered[]`
   (`node_key`, `component`, `reason`, and the correction), and everything around it renders. READ
   `unrendered[]` BEFORE PRESENTING: each entry is a hole where the human expects a design.
   THE VIEWPORT, AND WHAT IT REALLY MEANS HERE — `viewport: 390` (CSS px) or `"mobile"` / `"tablet"`
   / `"desktop"`. Be exact about what it does, because it is easy to over-read: nyx LAYS NOTHING OUT.
   The markup is byte for byte the same at every width; no width reflows anything and nothing here
   measures a box. A viewport does exactly two real things: it is the width a FRAME must apply to
   display this markup, and it says which responsive CLASSES are in force at it — the breakpoints
   (from your project's own `--breakpoint-*` tokens when it declares them, else Tailwind's defaults)
   and every `md:` / `lg:` / `max-*` variant your markup carries, each with `active`. That is a real
   verdict, and it catches the real defect: markup carrying NO responsive class renders IDENTICALLY
   at every width, and the answer SAYS SO. Whether the result then LOOKS right at that width is a
   rendering, and you are not looking at one.
   WHAT YOU CANNOT SEE IN IT — READ THIS BEFORE YOU CALL ANYTHING CHECKED. It is MARKUP, NOT AN
   IMAGE, and it is NOT a visual validation. You see a CLASS; you never see the pixel that class
   produces. No colour (`bg-primary` says nothing about whether the token behind it resolves, or
   contrasts with the text on top of it), no spacing, no alignment, no size, no overflow, no
   wrapping, no font, no z-order, no hover/focus/disabled appearance — and no answer at all to "does
   it LOOK like the component it is supposed to be". A class you invented and a class the theme
   really defines read IDENTICALLY here. A WHOLE PAGE IS NO DIFFERENT — it is more markup, not more
   sight: nothing here tells you whether the page fits, whether two blocks collide, whether anything
   overflows, or what it becomes at a width. A composition you have only rendered here is a
   composition NOBODY HAS SEEN. So when you hand back: say what you CHECKED (the structure — which
   elements exist, which classes are applied, which params substituted, which nodes are still holes)
   and say what you did NOT (the rendering). NEVER report a component or a page as visually validated
   on the strength of this tool. The human is still the only eye on the visual, and letting them
   believe otherwise is the exact failure this tool exists to correct — so when the open question IS
   a visual one (a colour, a contrast, a spacing, a responsive layout, "is this the right look"),
   PRESENT the round and ASK.
   THE IMAGE: nyx produces none, on purpose and for now — for a whole COMPOSITION exactly as for a
   component, and the answer to "can I export a screenshot of the page?" is NO. The sidecar has no
   rendering engine, nothing in the product captures one, and an image does not fit a 64 KiB JSON
   response; the only thing that can turn this markup into pixels is the app's own preview frame — so
   a capture is a decision about the ARCHITECTURE (the sidecar driving a running app), not one more
   argument on this tool. Do not wait for it, and do not pretend you have it.
6. design_delete_component(project_id, name) — REMOVE an entry that does not belong in the catalogue
   (a `[NEW]` authored by mistake, a duplicate, an entry an earlier wrong glob brought in).
   A DELETE ALONE IS EPHEMERAL WHEN THE FILE IS STILL THERE: a component's identity IS its source, so
   as long as the file matches the recipe the next design_inventory discovers it again, pending. The
   response tells you (`ephemeral: true` + `warnings[]`); it is NOT a way to "keep only N components".
   To make it last: RESTRICT the recipe (step 0-bis) when a whole family does not belong — that is the
   real fix — or, for ONE bundle, design_delete_component(project_id, name, exclude_source: true),
   which appends its paths to the recipe's `exclude` list (and materializes the recipe as yours). A
   greenfield `[NEW]` has no source: deleting it is already definitive.
   IT ALSO NAMES WHO STILL REFERENCES IT: a component other templates point at with `data-nyx-use`
   is a shared sub-component, and removing it makes every one of those hosts FAIL to render. The
   response lists them — re-synthesize each host onto another component, or inline the markup it was
   borrowing, before you consider the removal done.

Two layers, two homes — never mix them. A COMPONENT owns its structure and its CLASSES; the PROJECT
owns the TOKENS those classes resolve against, once, for the whole catalogue. That is what makes a
theme editable in one place and applied everywhere; a component that carries its own colours is a
copy that will drift.

The catalogue tracks the app: a component is identified by its SOURCE, never by its name. A bundle
that MOVES on disk is re-anchored by CONTENT — same row, same synthesis (`moved`); a row whose
source is GONE is reported in `orphans` and pruned, unless the run could not judge it
(`kept_because`), which is exactly what design_delete_component is for. Nothing is ever re-pointed
at another file just because the name matches.

Then COMPOSE from that catalogue — the project's components + tokens — exactly as the coder does;
never re-invent a bespoke rendering. A variant's `composition_json` is a NAMED COMPONENT TREE (the
placement), not an HTML blob. When the design needs a component the catalogue does NOT have, AUTHOR
it greenfield with proto_author_component(prototype_id, name, …): it enters the catalogue as a
`[NEW]` component the coder will build — you do not hand-roll it inline. (That tool CREATES and
refuses a name already catalogued; to update an existing one, use design_refresh_component.) An
authored `[NEW]` STAYS `[NEW]` until the app really implements it: the nature is the component's
ORIGIN (does it exist in the app?), not its presence in the catalogue — authoring it never makes
the coder believe it is already built.

## The prototype cycle (all agent-driven, via MCP)

0. proto_get(spec_id) — READ where the prototype stands (rounds, current round + its manifest, pages,
   their VERSIONS and VARIANTS, the design LOCK, open comments) before every action. No blind
   navigation. The overview is BOUNDED: check `truncated` and each `sections.<name>.next_offset`, and
   pull the rest one list at a time with proto_get(spec_id, section: "<name>", offset: N) — do not
   act on a list you only half read.
   THIS IS WHERE THE IDS COME FROM, and it is what makes a RESUMED session possible: a version id, a
   variant id and a round id are all in this one answer, each row naming its parent (`page_id`,
   `page_version_id`), so you never have to remember what an earlier call returned. `locked` and
   `design_lock` say whether the design is FROZEN and on WHICH round — read them before you try to
   elect or compose, rather than discovering the freeze through a refusal. And `warnings` names what
   is legal but worth saying (a frozen round nobody ever presented).
   A CONFLICT ALSO HANDS YOU AN ID: creating a page / version / variant / round whose slug or label
   already exists is refused WITH the existing row's id, so a re-run of a step you already did is one
   call, not a search.
1. proto_create(spec_id, description?) — CREATE the prototype on the Spec (one per Spec); it seeds
   the first round (`v1`). Start here.
2. COMPOSE the pages:
   - proto_add_page(prototype_id, slug, title) — a logical screen (`slug` unique per prototype). It
     returns the page's ID: KEEP IT — that id is what a node LINKS to when you wire the parcours
     (below). Create every page of the parcours BEFORE composing the trees that link to them.
   - proto_add_version(page_id, label) — a frozen ITERATION of that page; you iterate by minting a
     NEW version, never mutating an existing one.
   - proto_add_variant(page_version_id, label, title, composition_json?, states_json?) — one concrete
     rendering (`composition_json` = the component tree). Add SEVERAL radically different variants
     when the question is "what does it look like?".

     A NODE'S KEYS ARE THE COMPONENT'S KEYS. A node is `{ "component": "<catalogue name>", "params":
     { … } }`, and every key of `params` must be one that component DECLARES — a name from its
     `params_json`, or one of its AXES from `states_json`. Anything else is substituted by NOTHING:
     the node renders without it, and it used to do so in total silence. proto_add_variant now
     ANSWERS with `warnings` naming, per node, the keys the component declares nowhere and the
     `required` params the node does not provide. READ THEM — they are the only thing between a typo
     and an empty rendering. nyx never guesses a mapping (`text` is not `buttonLabel`), so the
     correction is always yours: rename the key onto the declared one, or provide the missing value.

         { "component": "Button",
           "params": { "buttonLabel": "Reset filters", "variant": "ghost", "size": "sm" } }

     If you do not know what a component declares, READ it before composing with it — a component's
     `params_json` / `states_json` ARE the contract.

     THE TREE'S OWN SHAPE IS CHECKED AT THE WRITE, and refused rather than warned about: a
     `composition_json` that is not valid JSON, and one whose root is an ARRAY instead of ONE node
     object (several blocks are the CHILDREN of one root — `{ "key": "root", "children": [ … ] }`).
     Both used to store with `warnings: []`, which is the worst possible answer: the node-contract
     check ran over a tree that parsed to nothing, found no node, and reported nothing wrong.
     AN `[inline]` NODE IS A TEMPLATE LIKE ANY OTHER, and obeys the same LOGIC-LESS contract: a
     `{{#if}}` / `{{#each}}` block or a `{{> partial}}` in its markup is refused here exactly as it
     is in a component's template, naming the node. Express the condition with the composition TREE,
     or with `data-nyx-if` on a component's axes.

     A PROTOTYPE SHOWS A PARCOURS, NOT A PILE OF SCREENS — `navigate_to`. A node may LINK to another
     page of the prototype: clicking it navigates there in the frame. It is ONE field on the node,
     and it holds the destination page's ID:

         { "component": "Button", "params": { "label": "Voir les tarifs" },
           "navigate_to": "<the id proto_add_page returned>" }

     ON THE NODE, NEVER ON THE COMPONENT, and that is a decision, not an accident: a destination is a
     PLACEMENT choice. The same `Button` is a "Continue" on one page and a "Back" on another, so a
     link baked into the component would need one component per destination — the copy-instead-of-
     reference drift `data-nyx-use` exists to end. Two more reasons agree: the composition TREE is
     what crosses to the coder, so a parcours travels with the placement it belongs to; and a
     component row is RE-SYNTHESIZED from the app's real sources, so a link written into a template
     would be erased by the next design_refresh_component.
     BY ID, NEVER BY SLUG. A slug gets renamed; an id does not. nyx resolves the id to the current
     slug AT EVERY RENDER — so renaming a page costs you nothing, and the frame still navigates.
     What you and the human READ is resolved the same way: design_preview answers `navigation[]` with
     the destination's slug AND title per linking node.
     A DEAD LINK IS REFUSED AT THE WRITE. `navigate_to` naming a page this prototype does not have is
     an `invalid_id` listing the pages it DOES have — a page is never "not created yet on purpose",
     unlike a `[NEW]` component. A link on a STRUCTURAL node (no `component`, no `inline`) is refused
     too: such a node is only its children, so there would be nothing to click. If a page DISAPPEARS
     after the fact, the render says so in place (`navigation[].status = "unknown_page"`) and refuses
     to navigate — never a click that silently does nothing. The refusal is STRUCTURAL: a node that
     states a destination has the whole navigation vocabulary cleared off its root before the render
     writes what it decided, so a `data-nyx-page` a component's template happened to bake in can
     neither outrank the modelled link nor survive beside the invalidation marker.
     CYCLES ARE FINE. `A → B → A` is what a "Back" button IS. A navigation is a USER GESTURE at click
     time, not an expansion at render time: the render walks the tree and never follows a link, so a
     cycle costs nothing and is refused nowhere. (That is the whole difference with `data-nyx-use`,
     whose cycle would be an infinite expansion and IS refused.)
   - proto_recommend_variant(variant_id) — YOUR recommendation among a version's variants (`is_main`).
     Say which one you would keep, and why in your handback. ONE per version and REVERSIBLE (calling
     it on a sibling moves the reco). It is NOT the decision: it never poses the RETENU, which only
     the user's proto_elect_variant records.
3. proto_pin_page(round_id, page_id, page_version_id) — PIN a (page, version) into the current
   round's manifest, so the round "sees" the page and it becomes electable / renderable.
3-bis. RENDER WHAT YOU ARE ABOUT TO PRESENT — design_preview(variant_id) on every variant the round
   pins, BEFORE proto_present_round (the id is what proto_add_variant returned; proto_get's
   `variants` section lists every one of them, and for a round already elected it is
   `chosen_variant_id` in proto_get's manifest, `elected_variant_id` in
   proto_composition). The composition IS the deliverable: presenting a tree you never
   resolved is how a page reaches the human with a node nobody built, a param substituting to
   nothing, or a component that renders empty — and the human then finds, one by one, what a single
   call would have shown you. Read `unrendered[]` first (each entry is a HOLE where a design should
   be: a `[NEW]` to synthesize, a typo in a node's component name), then read the markup itself, then
   — for the responsive question — call it again with `viewport: "mobile"` and read which of your
   `md:` / `lg:` classes are in force there, and whether you wrote any at all. Use `node_key` to walk
   a big page branch by branch. If the variant declares AXES, render each value that matters —
   design_preview(variant_id, axes: { "loading": "on" }) — and check the element gated on it is
   really absent at the other value; `axes` in the answer says which combination you looked at, and
   `declared_axes` what else there is to look at. Read `navigation[]`: every node that LINKS to
   another page, with the
   destination resolved to its slug and title — and `navigation_broken_count`, which is how many of
   your links lead NOWHERE. A parcours you never walked is a parcours the human walks for you.
   Read `css_slots[]` too: every SCOPED CSS EXCEPTION this page carries,
   with the scope it is bounded to and the REASON it exists. An exception is a departure from the
   design system — NAME IT in your handback so the human reviews it, because it will be carried,
   verbatim and with its reason, to the coder. It is still NOT a visual validation and it is NOT an
   image: step 5 of the design loop lists exactly what you do not see, and your handback must say it.
4. proto_present_round(round_id) — PRESENT (freeze) the round to the user: a handback. A presented
   round is frozen; later feedback opens a new round.
5. PRE-DEV review, anchored to the tree — it comes BEFORE the election, because its whole purpose is
   to CORRECT the design while it can still be corrected (PDR-0009):
   - proto_feedback_create(prototype_id, body, node_key?) — raise a comment anchored to a TREE NODE
     (`node_key`), not a CSS selector. These are NEVER transmitted to the coder.
   - proto_feedback_resolve(feedback_id, resolution_note?, resolution_via_variant_id?) — resolve it
     once addressed. Addressing it means a NEW version/variant (step 2) in a NEW round (step 7) — you
     never mutate a presented round.
   Only once the open comments are worked does the design deserve a verdict.
6. The USER ELECTS: proto_elect_variant(round_id, page_id, variant_id, rationale?, decided_by?)
   records the USER's RETENU (chosen) variant for the page in the round. This is the USER's design
   decision — DISTINCT from `is_main`, the agent's per-version RECOMMENDATION you pose with
   proto_recommend_variant (a variant attribute surfaced by proto_get, reversible, NOT the verdict).
   You record what the user decided; you never pose the RETENU on your own authority. The page must
   be pinned in the round first.
   THE VARIANT MUST BE ONE OF THE PINNED VERSION'S OWN, and a variant from another page — or from
   another version of the right page — is REFUSED, naming the ones that ARE electable. The RETENU is
   the tree the coder receives UNDER THIS PAGE'S NAME: another page's variant would cross the seam as
   this page's design, which is a corruption of the contract and not a preference.
7. proto_rebase(prototype_id, source_round_id, label) — ROLL BACK onto an earlier round: opens a NEW
   current round whose manifest COPIES that round's pinned versions, so you iterate forward WITHOUT
   mutating the frozen source (rounds are kept as rollback points). Use proto_open_round(prototype_id,
   label) instead to start a fresh EMPTY round composed from scratch.
8. proto_distill(prototype_id, round_id, decided_by?) — FREEZE the elected round as THE design for
   the Spec. This LOCKS the election (the composition tree is already the contract — no separate
   placement to rewrite).
   IT REFUSES TO FREEZE WHAT IS NOT A DECISION, and each refusal names what to do: a round that was
   never PRESENTED (nobody was shown it, so nobody decided), a round that pins NO page (nothing to
   freeze), and a round where a pinned page still owes an arbitration — that page would cross the
   seam EMPTY, and the coder would read an empty page as the design. Elect it, or unpin it.
   AND THE LOCK DOES NOT MOVE ON ITS OWN. Distilling a SECOND round while one is already frozen is
   refused unless you pass `supersede: true`. The forward path stays open — a new iteration
   legitimately becomes THE design — but it is a deliberate gesture: what the coder consumes changes
   with it, and the answer says which round was superseded. Re-distilling the SAME round is
   idempotent and needs nothing.

## The lock (élection figée)

Once a round is distilled, its election is FROZEN: proto_elect_variant / proto_add_variant /
proto_pin_page on that round are REFUSED (invalid_state). To iterate again, OPEN A NEW ROUND
(proto_rebase / proto_open_round) — a frozen round is never mutated, so the design the coder will
consume can never drift under it.

## The composition seam (what dev consumes)

proto_composition(prototype_id) READS the FROZEN elected composition — the coder SEAM. It exposes
NOTHING until the design is locked (`{ composition: null }`). Once locked it gives, per page: the
effective RETENU variant + its NAMED TREE = the placement, each node classified by one of THREE
natures —
- `[DS]`     — a component that EXISTS in the app (it has an on-disk source): compose it, don't
  rebuild it;
- `[NEW]`    — a component to BUILD: it is not (or no longer) in the app, whether or not it is
  already in the catalogue — an authored greenfield `[NEW]` is one of these. These are the ANCHORS
  of the derived dev tasks;
- `[inline]` — bespoke markup.
The nature is the component's ORIGIN, recalculated at every read: the day the coder ships an authored
`[NEW]` and the inventory discovers it, the SAME node reads `[DS]` — with no duplicate row.
The design-system is carried as a REFERENCE (project + design + component ids, NOT injected content).
This frozen tree — not the illustrative rendering — is what feeds the Spec's dev.

It is READ PAGE BY PAGE, and a tree is never cut: follow `next_offset` to `null` to see every elected
page, or ask for one with `page: "<slug>"`. A page returned WHOLE is a whole placement; a page marked
`oversized` is one that could not cross the seam at all, and it is on you to split it. A page with NO
elected variant crosses EMPTY — it is counted in `unelected_pages` and NAMED in `warnings[]`, because
an empty page is an arbitration nobody made, never a design. proto_distill refuses to freeze such a
round, so seeing one here means a freeze posed before that guard, or a variant that disappeared under
it: do not build those pages.

THE PARCOURS CROSSES IT TOO — `navigates_to` on every node that LINKS to another page. "This button
opens the checkout" is a DESIGN decision, settled and frozen here like any other; if it did not
cross, the coder would get a set of pages with no idea which leads to which, and would re-invent the
routing. It carries the destination's `page_id` (what the tree stores) plus the `slug` and `title`
resolved at the read, and a `status`: `pinned` (the destination is part of the frozen design),
`unpinned` (the page exists but this round does not ship it — the parcours leaves what you handed
over) or `unknown` (the page is gone). Named in all three cases, never a blank.

THE CSS EXCEPTIONS CROSS THIS SEAM WHOLE — `css_exceptions[]`, per page. Everything else here is a
REFERENCE the harness resolves, because it exists in the app; a scoped CSS slot exists NOWHERE else,
so it travels entire: the component it belongs to, the SCOPE it is bounded to
(`[data-nyx-css~="…"]`), the COMPILED CSS (selectors already anchored, `@keyframes` already renamed),
the constructs it uses, and the REASON the agent wrote it. `reached_by` says whether a node PLACES the
component or another component merely REFERENCES it. Each node that lands on one points at it with
`css_scope` — one record per page, never a copy per placement. If it did not cross, the coder would
rebuild the component without it, which is the disappearance this exists to end, one seam later.

The pre-dev review crosses this seam as ANCHORS AND COUNTS ONLY — which NODES were corrected before
the election, how many comments were resolved, how many are still open. Never a comment BODY: the
pre-dev feedback is NEVER transmitted to the coder, and the seam is built so it cannot be. The
comments themselves stay on the prototype (proto_get), where you and the user work them.

## Rounds & the human loop

Iterate with the human in ROUNDS until the design matches their intent: compose → present → gather +
resolve the pre-dev feedback → the user elects the RETENU → rebase for the next round. The feedback
comes BEFORE the election on purpose: it is what corrects the design, and a design is elected once it
is right, not corrected once it is elected. When the design is settled and elected, distill it to lock
it. The settled verdict FEEDS the Spec (the /nspec author inlines the decisive decision); the
composition seam then carries the concrete placement to dev."#
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
                db::list_specs(c, &spec.project_id)?
            } else {
                Vec::new()
            };
            Ok(Some((
                spec,
                revision,
                tasks,
                workspace_context,
                project,
                repos,
                sibling_specs,
            )))
        })
        .map_err(internal_db)?;
    let Some((spec, revision, tasks, workspace_context, project, repos, sibling_specs)) = bundle
    else {
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
        None => out.push_str(
            "- workspace: (none anchored on this Spec — a dev workspace is the CODE link and is \
             created when the code is; the design tools above need only the project_id)\n",
        ),
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
            let context = render_agent_context(db, role, axis, &spec_id)?.ok_or_else(|| {
                RpcError::new(RpcCode::InvalidId, format!("unknown spec {spec_id}"))
            })?;
            Ok(ToolOutcome::read(json!({
                "role": role_name,
                "spec_id": spec_id,
                // The Spec's OWN project — echoed beside the manual, so a caller reads the id it
                // must pass to every design_* tool without parsing the header text.
                "project_id": spec_project,
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
        // Project + its repos + the workspace (one section — the project half is unconditional,
        // finding `01KZ3JQYRN95EETZ0JBQXED13T`).
        assert!(
            text.contains("Project, repos & workspace"),
            "project/workspace section"
        );
        assert!(text.contains("api → ."), "the project's repo");
        assert!(text.contains("- workspace: "), "the anchored workspace");
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

    /// Review finding 01KYA146WYNK80SVFSXX3KKMPJ: the prototype manual must reflect the REAL v2.7
    /// machinery (it was stuck on the stale "throwaway app / one command to launch" text that
    /// named none of the shipped tools). It must describe design_inventory + the create → pages →
    /// versions → variants → pin → present → feedback → election (is_main ≠ RETENU) → rebase →
    /// distill cycle, the proto_composition seam (3 natures), the lock, and the design-system
    /// consigne — so an agent can drive a proto end-to-end from it.
    ///
    /// Review #48 adds the ORDER to what is pinned: the manual used to list the pre-dev feedback
    /// AFTER the election, while the review PRECEDES it (the feedback corrects the design before the
    /// user retains a variant — PDR-0009). An agent following the manual top to bottom must execute a
    /// VALID sequence, so the assertions below check positions, not just presence.
    #[test]
    fn render_prototype_role_carries_the_v2_7_machinery() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Prototype, None, &spec_id)
            .unwrap()
            .unwrap();
        assert!(text.contains("Context: Prototype"), "prototype preamble");
        // Design-system FIRST: inventory the catalogue + the compose-don't-reinvent consigne
        // (mirror of the coder's).
        assert!(
            text.contains("design_inventory(project_id)")
                && text.contains("never re-invent a bespoke"),
            "design-system consigne (inventory + compose, don't re-invent)",
        );
        // Finding 01KYKXPHW103C0V5RBXQ29BBXQ — the RECIPE pivot: nyx holds NO framework knowledge,
        // so the manual must teach the sequence EXPLORE → RECIPE → INVENTORY, say it is done ONCE
        // per project, and name the escape hatch (pins).
        assert!(
            text.contains("EXPLORE → RECIPE → INVENTORY, ONCE PER PROJECT")
                && text.contains("design_recipe(project_id)")
                && text.contains("design_recipe(project_id, recipe:")
                && text.contains("nyx holds NO framework knowledge"),
            "the explore → write the recipe → inventory sequence, once per project",
        );
        assert!(
            text.contains("ESCAPE HATCH")
                && text.contains("design_recipe(project_id, pins:")
                && text.contains("bypasses every"),
            "the pin escape hatch for what no rule expresses",
        );
        assert!(
            text.contains("ONE-OFF per PROJECT, not")
                && text.contains("if the result looks wrong, fix the RECIPE"),
            "the recipe is written once and is the lever when the inventory looks wrong",
        );
        // Review #40 — the BUNDLE pivot: the manual must describe the REAL flow (nyx discovers the
        // bundles, the AGENT synthesizes each one into a logic-less Handlebars template + params +
        // states and stores it back), the BOUNDED response, and the INCREMENTAL rule.
        assert!(
            text.contains("BUNDLE")
                && text.contains("design_inventory(project_id, component:")
                && text.contains("design_refresh_component(project_id, name, template"),
            "the bundle flow: list bundles → read one → synthesize → store it",
        );
        assert!(
            text.contains("LOGIC-LESS") && text.contains("pending_only"),
            "the logic-less synthesis target + the bounded/pending work list",
        );
        assert!(
            text.contains("INCREMENTAL") && text.contains("never re-synthesized"),
            "an unchanged bundle is never re-synthesized",
        );
        // Review 01KYMF0KZJBSK064AH0X4ZDQ29 — the INCREMENTAL LOOP, made explicit and imperative.
        // The barrier is the hard response cap (its twin finding); THIS is the method, and the
        // manual has to spell it out in order, or a naive agent still tries to slurp the catalogue.
        assert!(
            text.contains("Read in a LOOP, never in one gulp") && text.contains("64 KiB"),
            "the manual names the incremental loop and the cap it lives under",
        );
        assert!(
            text.contains("ASK FOR WHAT IS LEFT")
                && text.contains("pending_only: true")
                && text.contains("the work list shrinks as you work"),
            "rule 1: list with pending_only, so the loop ends",
        );
        assert!(
            text.contains("FOLLOW `next_offset` UNTIL IT IS `null`")
                && text.contains("`limit` does not raise the cap"),
            "rule 2: follow next_offset rather than asking for a big page",
        );
        assert!(
            text.contains("ONE ITEM AT A TIME") && text.contains("SYNTHESIZE it, STORE it"),
            "rule 3: one bundle at a time, synthesized before the next",
        );
        assert!(
            text.contains("NEVER ASSUME A RESPONSE IS COMPLETE")
                && text.contains("`truncated`")
                && text.contains("`total` vs `returned`")
                && text.contains("`truncated_fields`"),
            "rule 4: the truncation signal is named, field by field",
        );
        // …and the SAME discipline is carried onto the (now capped) proto reads.
        assert!(
            text.contains("proto_get(spec_id, section:")
                && text.contains("sections.<name>")
                && text.contains("proto_composition(prototype_id, page:")
                && text.contains("`oversized: true`"),
            "the proto reads are paged the same way (sections / by page / oversized)",
        );
        // The two halves are named as complementary: barrier + method.
        assert!(
            text.contains("the cap is the BARRIER") && text.contains("the loop is the METHOD"),
            "the hard cap and the guidance are described as two halves of one thing",
        );
        // Review #42 — the CSS/tokens half of a synthesis. The manual used to say "synthesize the
        // bundle" and stop there, so an agent produced structure with no style: the `.variants.ts`
        // CVA was left as a dangling `{{class}}`, the variant axes never reached `states_json`, and
        // hard-coded colours crept in. It must now spell out the three rules.
        assert!(
            text.contains("design_tokens(project_id, ingest: true)")
                && text.contains("renders GREY until"),
            "the tokens are ingested from the app BEFORE any synthesis (the grey-preview cause)",
        );
        assert!(
            text.contains("RESOLVE THE CVA YOURSELF")
                && text.contains(".variants.ts")
                && text.contains("INLINE the resulting concrete class STRING"),
            "CVA → concrete class strings inlined on the Handlebars nodes",
        );
        assert!(
            text.contains("MAP THE VARIANT AXES TO `states_json` — ONE AXIS PER CVA AXIS")
                && text.contains("each VALUE carries ITS OWN\n     RESOLVED CLASSES")
                && text.contains("an axis WITH TWO VALUES"),
            "each CVA axis becomes a NAMED axis whose values carry their classes",
        );
        // Finding 01KYQ0R84KVF6QVQXQ9YVG9Z8T — THE RULE, in the manual, once. The model used to
        // expose four overlapping representations of the same notion (param / state / CVA axis /
        // boolean) with nothing saying which was which, and the four reported symptoms all came from
        // that flou. The manual must state the rule, and it must be one sentence.
        assert!(
            text.contains("A VARIANCE AXIS GOES IN THE AXES (`states_json`), NEVER IN THE")
                && text.contains(
                    "A SUBSTITUTED VALUE GOES IN THE PARAMS (`params_json`), NEVER IN THE AXES"
                )
                && text.contains("Classes ⇒ axis. Value ⇒ param."),
            "THE RULE is written down, in one line, both ways",
        );
        // HOW a variant is actually RENDERED: the shim applies the ACTIVE value's classes of every
        // axis, so ONE element expresses the whole product. `data-nyx-state-case` remains, and
        // `data-nyx-if` generalizes it to a condition on any axis value (the structural half).
        assert!(
            text.contains("THE VARIANCE MODEL — NAMED AXES, THE CLASSES ON THE VALUES")
                && text
                    .contains("the ACTIVE\n   value's classes onto the component's root element")
                && text.contains("You write ONE element")
                && text.contains("you never\n   enumerate it"),
            "the axes applier is documented as the normal way to express a variant",
        );
        assert!(
            text.contains("FOR WHAT CLASSES CANNOT DO, THE STRUCTURE: `data-nyx-if`")
                && text.contains("data-nyx-if=\"loading=on\"")
                && text.contains("data-nyx-if-not")
                && text.contains("`data-nyx-state-case=\"<value>\"` still works"),
            "the structural half is the generalized data-nyx-if, and the state case survives",
        );
        assert!(
            text.contains("WHAT MAKES A VALUE REAL: `classes`, or a structural case")
                && text
                    .contains("naming the\n   component, the axis, the value and the correction"),
            "an inert value is named as THE defect, and the write-time warning is announced",
        );
        assert!(
            text.contains("THE COMPLETE SHAPE — a `button` with TWO AXES + ONE TEXT PARAM")
                && text.contains("{ \"name\": \"variant\", \"values\": [")
                && text.contains("{ \"name\": \"size\", \"values\": [")
                && text.contains("{ \"name\": \"loading\", \"values\": [")
                && text.contains("\"name\": \"label\", \"type\": \"text\", \"required\": true")
                && text.contains("THREE axes, ONE element, ONE param"),
            "a COMPLETE example: two axes + a structural boolean + one text param, ONE element",
        );
        // RETRO-COMPATIBILITY, said to the agent: the flat list is not deprecated, it is the
        // degenerate case. 173 stored components are written that way.
        assert!(
            text.contains("THE LEGACY FLAT FORM STILL WORKS")
                && text.contains("ONE\n   anonymous axis named `state`"),
            "the flat states_json is documented as the one-axis degenerate case, not as dead",
        );
        assert!(
            text.contains("USE THE DESIGN-SYSTEM CLASSES AND TOKENS, NEVER HARD-CODED COLOURS")
                && text.contains("bg-[#2563eb]"),
            "design-system classes/tokens rather than a frozen copy of a colour",
        );
        // Finding 01KYPSG0HPG2D28CMJEXRKNEVK — the params CONTRACT itself. The manual asked for a
        // `params_json` without ever saying what may be IN it: the agent guessed `type: "enum"`,
        // the UI silently fell back to `text`, its `options` were dropped, and a constrained choice
        // came out as a free-text field. The schema must be written down, exhaustively.
        assert!(
            text.contains("THE `params_json` SCHEMA, IN FULL")
                && text.contains("THE SIX TYPES, EXHAUSTIVELY")
                && text.contains("`text`")
                && text.contains("`html`")
                && text.contains("`state`")
                && text.contains("`boolean`")
                && text.contains("`number`")
                && text.contains("`select`"),
            "the six accepted param types are listed exhaustively",
        );
        assert!(
            text.contains("MANDATORY for `select`")
                && text.contains("`options` means something for `select` ONLY"),
            "what `options` is for, and where it is required",
        );
        // The two param shapes that are really an AXIS in disguise, named as such in the type list
        // (finding 01KYQ0R84KVF6QVQXQ9YVG9Z8T): the DEPRECATED `state`, and the boolean whose toggle
        // is decorative until it becomes a two-valued axis.
        assert!(
            text.contains("`state`   — DEPRECATED, do NOT write it")
                && text.contains("a boolean param can change NOTHING in\n                 the rendering")
                && text.contains("it is AN AXIS WITH TWO\n                 VALUES"),
            "the deprecated `state` type and the inert boolean are named where an agent reads types",
        );
        assert!(
            text.contains("AN UNKNOWN `type` IS REFUSED, NOT REPAIRED")
                && text.contains("\"enum\"")
                && text.contains("SILENT fallback to `text`"),
            "an unknown type is refused at the write, and the manual says why",
        );
        assert!(
            text.contains("A COMPLETE `params_json`")
                && text.contains("no `disabled` boolean either")
                && text.contains("\"name\": \"align\", \"type\": \"select\", \"options\"")
                && text.contains("\"required\": true")
                && text.contains("\"preview\"")
                && text.contains("\"default\""),
            "a COMPLETE JSON example carrying every field and the useful types",
        );
        // The DUPLICATION, arbitrated: a variant is declared ONCE, and `states_json` is the one
        // that renders — a `select` param over the same names is decorative by construction.
        assert!(
            text.contains("A VARIANT IS EXPRESSED ONCE — AND IT IS `states_json`")
                && text.contains("is not a value: it is a whole SET of classes")
                && text.contains("the one that")
                && text.contains("CHANGES THE RENDERING"),
            "the variant is expressed once, and the manual says which one is authoritative and why",
        );
        // Finding 01KYPD8K75EC2AAY1KVAS2XZNX then 01KYPJ1RC6WJX1Z9CPPV23FG27 — the PARAMS half of a
        // synthesis. The catalogue tile renders ONE instance, so a param the agent leaves without a
        // usable value is substituted with the EMPTY STRING: a `button` becomes an unlabeled pill, a
        // `card` an empty box. But "give every param a realistic DEFAULT" was the WRONG cure: a
        // `button` has no default label — the label is REQUIRED — so a default written to fill the
        // tile MISSTATES the contract. The manual must ask for the two notions SEPARATELY.
        assert!(
            text.contains("MARK WHAT IS REQUIRED, AND GIVE A PREVIEW VALUE")
                && text.contains("nyx will NOT invent")
                && text.contains("content in your place"),
            "required + preview are asked for separately, and nyx fabricates neither",
        );
        assert!(
            text.contains("the catalogue card renders ONE instance")
                && text.contains("no chip-bar and no variant listing")
                && text.contains("substituted with the EMPTY STRING")
                && text.contains("HAS NO DEFAULT LABEL"),
            "the WHY: the vignette is one instance, and a required param has no default to render",
        );
        assert!(
            text.contains("`required` (boolean)")
                && text.contains("A required param has NO default, by definition")
                && text.contains("ONLY when the source carries one")
                && text.contains("@Input() label = 'Enregistrer'"),
            "required is the contract; a default exists only when the SOURCE declares one",
        );
        assert!(
            text.contains("used ONLY to render an aperçu")
                && text.contains("Give one to EVERY required param")
                && text
                    .contains("The aperçu resolves `preview` first, then `default`, then NOTHING")
                && text.contains("does the vignette LOOK LIKE the component?"),
            "the preview value: what it is for, who needs one, and how the aperçu resolves it",
        );
        // The full agent-driven cycle: create → pages/versions/variants → pin → present → elect.
        assert!(
            text.contains("proto_create(spec_id")
                && text.contains("proto_add_page(")
                && text.contains("proto_add_version(")
                && text.contains("proto_add_variant(")
                && text.contains("proto_pin_page(")
                && text.contains("proto_present_round("),
            "the create/compose/pin/present cycle is spelled out",
        );
        // Election: the USER poses the RETENU (proto_elect_variant), distinct from is_main.
        assert!(
            text.contains("proto_elect_variant(") && text.contains("RETENU"),
            "the user's RETENU election via proto_elect_variant",
        );
        assert!(
            text.contains("is_main") && text.contains("DISTINCT from"),
            "is_main (agent reco) is distinguished from the RETENU (user decision)",
        );
        // Review #46: the agent can actually POSE its reco — the manual names the tool that does it.
        assert!(
            text.contains("proto_recommend_variant(variant_id)")
                && text.contains("it never poses the RETENU"),
            "the agent's is_main reco is posable (proto_recommend_variant) and never poses the RETENU",
        );
        // Pre-dev feedback anchored to a node, NOT transmitted to the coder.
        assert!(
            text.contains("proto_feedback_create(")
                && text.contains("proto_feedback_resolve(")
                && text.contains("NEVER transmitted to the coder"),
            "node-anchored pre-dev feedback, never transmitted to the coder",
        );
        // DONE-CRITERION (review #48): the cycle is in USE ORDER — an agent that follows it top to
        // bottom runs a VALID sequence. The decisive pair: the pre-dev review comes BEFORE the
        // election (it corrects the design; the election settles a corrected design), and both come
        // after the presentation and before the distill freeze.
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("the manual names {needle}"))
        };
        let steps = [
            "proto_create(spec_id",
            "proto_add_page(",
            "proto_add_variant(",
            "proto_pin_page(",
            "proto_present_round(",
            "proto_feedback_create(",
            "proto_elect_variant(",
            "proto_rebase(",
            "proto_distill(",
        ];
        for pair in steps.windows(2) {
            assert!(
                at(pair[0]) < at(pair[1]),
                "the cycle is out of order: {} must be described before {}",
                pair[0],
                pair[1],
            );
        }
        // …and the closing summary of the human loop tells the SAME story (it used to invert it).
        assert!(
            text.contains(
                "compose → present → gather +\nresolve the pre-dev feedback → the user elects the \
                 RETENU → rebase for the next round"
            ),
            "the round-loop summary lists the feedback BEFORE the election too",
        );
        // Rebase (rollback) + distill (freeze/lock the election).
        assert!(
            text.contains("proto_rebase(") && text.contains("proto_distill("),
            "rebase (rollback) + distill (freeze) are described",
        );
        // The lock: once distilled, election mutations are refused → open a new round.
        assert!(
            text.contains("élection figée") && text.contains("REFUSED (invalid_state)"),
            "the lock refuses election mutations once distilled",
        );
        // The composition seam + its 3 natures + greenfield [NEW] authoring.
        assert!(
            text.contains("proto_composition(prototype_id)")
                && text.contains("`[DS]`")
                && text.contains("`[NEW]`")
                && text.contains("`[inline]`")
                && text.contains("proto_author_component("),
            "the coder seam (proto_composition) with the 3 natures + [NEW] authoring",
        );
        // The stale wording is GONE (recadré for the greenfield machinery).
        assert!(
            !text.contains("One command to launch") && !text.contains("throwaway branch"),
            "the stale 'one command to launch / throwaway branch' text is removed",
        );
        // Finding 01KYS14G4V1Q5ERRAHB0QVW362 — THE MANUAL TEACHES ONE VOCABULARY, AND IT IS `nyx`.
        // The machinery was ported from Depot and the interaction markers kept the OTHER TOOL'S
        // NAME, right down to mechanisms invented here (`data-depot-if`, `data-depot-axis`). The
        // manual is where that reached the agent: an agent who reads it writes what it reads. So
        // the whole rendered text is checked for the word — not just the attributes — because a
        // single leftover mention is a synthesis emitting the dead prefix all over again.
        assert!(
            !text.to_lowercase().contains("depot"),
            "the manual must not name the other tool anywhere — the vocabulary is data-nyx-*",
        );
        // Finding 01KYSNSBBT4N22ZTWQHFNQBDJQ — THE SAME SWEEP AS THE TOOL DESCRIPTORS, on the SAME
        // list ([`STALE_AGENT_FORMULATIONS`]). The descriptors got a stale-wording guard; the manual
        // did not, so it kept summarizing `states_json` as "its declared states/variants" — a
        // formulation forbidden in a descriptor, in the first sentence an agent reads about the
        // field. Same rule, both surfaces, one list: a wording chased off one can no longer survive
        // on the other.
        let lowered = text.to_lowercase();
        for (stale, why) in STALE_AGENT_FORMULATIONS {
            assert!(
                !lowered.contains(stale),
                "the manual still teaches the stale form {stale:?}: {why}",
            );
        }
        assert!(
            text.contains("data-nyx-if")
                && text.contains("data-nyx-if-not")
                && text.contains("data-nyx-state-case")
                && text.contains("data-nyx-axis")
                && text.contains("data-nyx-state")
                && text.contains("data-nyx-set")
                && text.contains("data-nyx-use")
                && text.contains("data-nyx-use-axes")
                && text.contains("data-nyx-param-"),
            "…and it teaches the whole nyx vocabulary explicitly",
        );
        // Finding 01KYT0Z3RYGF3A881SDQN0X9D7 (MAJOR) — THE AGENT MUST BE TOLD IT CAN SEE, AND TOLD
        // WHAT IT STILL CANNOT. The warnings validate the STRUCTURE and never the RENDERING, so on
        // this very feature every visual defect was found by the human and by no tool. The manual
        // now names the tool, teaches the LOOP, and — this is the half that matters most — states
        // where the agent's sight stops, so "I rendered it" never becomes "I validated it".
        assert!(
            text.contains("design_preview(project_id, component, axes?, params?, children?)")
                && text.contains("SYNTHESIZE (step 4) → RENDER (this) → LOOK at\n   the markup → CORRECT → render again"),
            "the manual names design_preview and teaches the synthesize → render → look → correct loop",
        );
        assert!(
            text.contains("WHAT YOU CANNOT SEE IN IT")
                && text.contains("MARKUP, NOT AN\n   IMAGE, and it is NOT a visual validation")
                && text
                    .contains("You see a CLASS; you never see the pixel that class\n   produces")
                && text.contains(
                    "NEVER report a component or a page as visually validated\n   on the strength \
                     of this tool"
                ),
            "…and it says explicitly what the resolved markup does NOT show, and forbids claiming \
             the visual was validated",
        );
        assert!(
            text.contains("THE IMAGE: nyx produces none")
                && text.contains("The sidecar has no\n   rendering engine")
                && text.contains("nothing in the product captures one"),
            "the image question is answered in the manual too — no capture, and why",
        );
        // Finding 01KYT45C6RC7K84V1CA3561SPF (MAJOR) — THE COMPOSITION IS THE DELIVERABLE, and the
        // agent had no way to render one. The manual must teach the SECOND subject of the same tool,
        // must run it BEFORE the round is presented, must say what an unrenderable node does, and
        // must state — franchement — what a viewport really decides here, so `viewport` is never
        // read as "nyx laid this out at 390px".
        assert!(
            text.contains("design_preview(variant_id, axes?, node_key?,\n   viewport?)")
                && text.contains("RENDER WHAT YOU ARE ABOUT TO PRESENT")
                && text.contains("BEFORE proto_present_round"),
            "the manual names the composition subject and puts it before the handback",
        );
        // Finding 01KZ3JS7PNF3F8GFB3GYRK6CKH — a VARIANT declares axes too, and they pilot the whole
        // page. The manual must say so where it teaches the composition subject, or the agent has no
        // reason to render the value that matters.
        assert!(
            text.contains("A COMPOSITION HAS ITS OWN AXES")
                && text.contains("`declared_axes`")
                && text.contains("must be ABSENT at `off`"),
            "…and it teaches that a variant's axes are pilotable and must be looked at",
        );
        assert!(
            text.contains("IT ALWAYS COMPLETES, AND IT NAMES WHAT IT COULD NOT RENDER")
                && text.contains("class=\"nyx-unrendered\"")
                && text.contains("READ\n   `unrendered[]` BEFORE PRESENTING"),
            "…and it teaches that a [NEW] node is NAMED, never a failed tree",
        );
        assert!(
            text.contains("nyx LAYS NOTHING OUT")
                && text.contains("byte for byte the same at every width")
                && text.contains("which responsive CLASSES are in force"),
            "…and it says what a viewport really decides, instead of pretending it renders one",
        );
        assert!(
            text.contains("A WHOLE PAGE IS NO DIFFERENT")
                && text.contains(
                    "A composition you have only rendered here is a\n   composition \
                                  NOBODY HAS SEEN"
                ),
            "…and the honesty paragraph covers the composition, so the new subject reopens nothing",
        );
        // Finding 01KYT2CM2BVNZYS7Z8NFK51RCN — THE ICONS, taught as a DISCIPLINE, exactly like the
        // CVA resolution beside it. Nothing was missing from the model (the user checked and said
        // so): what was missing was the instruction, so a lazy synthesis put a glyph or a name where
        // the real SVG belongs and the `icon` param had nothing useful to type into it. Both cases
        // must be named, because their answers are opposite — inline for a FIXED icon, an `html`
        // param (or a reference) for a VARIABLE one.
        assert!(
            text.contains("RESOLVE THE ICONS TO CONCRETE SVG")
                && text.contains("An icon is MARKUP, not a character"),
            "the manual teaches resolving icons to concrete SVG, like the CVA",
        );
        assert!(
            text.contains("A FIXED icon")
                && text.contains("INLINE the resolved SVG in the template")
                && text.contains("A VARIABLE icon")
                && text.contains("is a param of type `html`, substituted RAW")
                && text.contains("data-nyx-use=\"IconCheck\""),
            "…with the two cases named and the third lever (a shared icon is REFERENCED)",
        );
        assert!(
            text.contains("there is no\n     `asset` param type and none is needed"),
            "…and it says the existing shapes suffice — no primitive was added for this",
        );
        // Finding 01KYVMTWB71R0D0CT1SV88J8DX (MAJOR) — TAILWIND FIRST, THE CSS SLOT LAST. Everything
        // the utilities cannot express used to be SILENTLY LOST (the loader's animation), so the
        // manual must teach the ORDER (utilities are the channel), the four constructs that are the
        // only admitted reasons, and the four constraints — each as a REFUSAL, not a suggestion.
        assert!(
            text.contains("TAILWIND FIRST, THE CSS SLOT LAST")
                && text.contains("Tailwind is THE framework of this project and THE\n   channel")
                && text.contains("SILENTLY LOST"),
            "the manual teaches the order: utilities first, the slot as the last resort",
        );
        assert!(
            text.contains("a custom `@keyframes`")
                && text.contains("the CONTENT of a `::before` / `::after`")
                && text.contains(":has(…)")
                && text.contains("a `@container` query")
                && text.contains("A slot that uses NONE of them is REFUSED"),
            "…names the four constructs that are the only admitted reasons, and the refusal of \
             anything else",
        );
        assert!(
            text.contains("1. TOKENISED")
                && text.contains("A hard-coded colour is a refusal in EVERY form")
                && text.contains("2. SCOPED — AND NYX GUARANTEES IT, IT DOES NOT ASK YOU FOR IT")
                && text.contains("3. BOUNDED — 2048 bytes")
                && text.contains("4. SIGNALLED")
                && text.contains("`css_slot_reason` is MANDATORY"),
            "…and the four constraints, in order, each stated as a refusal",
        );
        assert!(
            text.contains("no CSS slot will ever relax its CSP"),
            "…and it says the CSP is never relaxed for a slot — the frame's contract is not \
             negotiable either",
        );
        assert!(
            text.contains("css_exceptions[]")
                && text.contains("the REASON the agent wrote it")
                && text.contains("`css_scope`"),
            "…and the seam section says the exception reaches the coder WITH its reason",
        );
        // Finding 01KYW34347CZSNXCYS35KDG3EY (MAJOR) — A PROTOTYPE SHOWS A PARCOURS. Nothing let a
        // node target ANOTHER PAGE, so a multi-page proto was a collection of screens that did not
        // answer each other. The manual must teach the field, WHERE the link lives and why, that it
        // is an ID and not a slug, what a dead link does, and that a cycle is fine — that last one
        // stated rather than left open, because the natural reflex is to assume it is refused like a
        // `data-nyx-use` cycle.
        assert!(
            text.contains("A PROTOTYPE SHOWS A PARCOURS, NOT A PILE OF SCREENS — `navigate_to`")
                && text.contains("\"navigate_to\": \"<the id proto_add_page returned>\""),
            "the manual teaches the inter-page link and its shape",
        );
        assert!(
            text.contains("ON THE NODE, NEVER ON THE COMPONENT")
                && text.contains("a destination is a\n     PLACEMENT choice")
                && text.contains("BY ID, NEVER BY SLUG"),
            "…and it argues WHERE the link lives, and why it is an id",
        );
        assert!(
            text.contains("A DEAD LINK IS REFUSED AT THE WRITE")
                && text.contains("navigation[].status = \"unknown_page\"")
                && text.contains("never a click that silently does nothing"),
            "…and it says what a link to a page that is gone does — refusal, then a named marker",
        );
        assert!(
            text.contains("CYCLES ARE FINE")
                && text.contains("A navigation is a USER GESTURE at click\n     time"),
            "…and the cycle question is CLOSED in the text, not left to the reader",
        );
        assert!(
            text.contains("THE PARCOURS CROSSES IT TOO — `navigates_to`")
                && text.contains("`pinned`")
                && text.contains("`unpinned`"),
            "…and the seam section says the parcours reaches the coder, with its status",
        );
    }

    /// DONE-CRITERION (finding `01KYSCY4J8JANZEJPYME86B1VH`): THE MANUAL AND THE VALIDATOR MUST NOT
    /// CONTRADICT EACH OTHER. The manual's canonical component was written with a BARE baseline
    /// (`{ "name": "off" }`) while the write path reported that value as one that "cannot change one
    /// pixel" — so the text taught exactly what the tool reproached, and an agent doing the right
    /// thing got a warning for it.
    ///
    /// The guard is not a COPY of the example (a copy drifts in silence): it EXTRACTS the canonical
    /// blocks out of the rendered manual and runs them through the very functions the write path
    /// calls. It fails the day the example changes shape — which is precisely when the pair has to
    /// be re-verified — and the day a check starts reproaching what the manual teaches.
    #[test]
    fn the_manuals_canonical_component_passes_the_checks_the_write_path_applies() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Prototype, None, &spec_id)
            .unwrap()
            .unwrap();
        let example_at = text
            .find("THE COMPLETE SHAPE — a `button` with TWO AXES + ONE TEXT PARAM")
            .expect("the manual carries the canonical example");
        let example = &text[example_at..];
        let between = |from: &str, to: &str| -> String {
            let start = example
                .find(from)
                .unwrap_or_else(|| panic!("the canonical example must carry '{from}'"));
            let rest = &example[start + from.len()..];
            let end = rest
                .find(to)
                .unwrap_or_else(|| panic!("'{to}' must follow '{from}' in the canonical example"));
            rest[..end].trim().to_string()
        };
        let template = between(
            "template (ONE self-contained string, ONE element):",
            "states_json:",
        );
        let states = between("states_json:", "params_json:");
        let params = between("params_json:", "\n");

        // The example really is the shape the finding is about: a boolean axis whose BASELINE is
        // bare, made visible by the COMPLEMENT alone (one `data-nyx-if`, no `data-nyx-if-not`).
        assert!(
            template.contains("data-nyx-if=\"loading=on\"")
                && !template.contains("data-nyx-if-not"),
            "the canonical template gates the spinner on `loading=on` and nothing else: {template}"
        );
        assert!(
            states.contains("{ \"name\": \"off\" }"),
            "the canonical axes declare the baseline BARE: {states}"
        );
        // Both blobs are valid JSON — the manual hands an agent something it can send verbatim.
        serde_json::from_str::<Value>(&states).expect("the canonical states_json is valid JSON");
        serde_json::from_str::<Value>(&params).expect("the canonical params_json is valid JSON");

        // …and every WRITE-TIME check accepts it, warnings included.
        crate::mcp_tools_design::ensure_params_json_accepted(Some(&params))
            .expect("the canonical params declare only accepted types");
        crate::mcp_tools_design::ensure_states_json_accepted(Some(&states))
            .expect("the canonical axes are shaped as the write path demands");
        let warnings = crate::mcp_tools_design::variance_warnings(
            "Button",
            &template,
            Some(&states),
            Some(&params),
        );
        assert!(
            warnings.is_empty(),
            "the manual's canonical example must not be warned about by the validator it teaches: \
             {warnings:?}"
        );

        // …AND IT IS THE CASE THE RENDERER'S SUITE READS (finding `01KYSNQWQD1324E0NA0172M66J`).
        // The TS twin used to RE-TYPE this example by hand, so it could not fail — it drifted. The
        // example now lives in `contract/design-fixtures.json`, which both suites read, and THIS is
        // where it stays moored to the manual: the day the manual's canonical component changes, the
        // fixture has to follow, and the renderer is judged on the new one.
        let collapse =
            |text: &str| -> String { text.split_whitespace().collect::<Vec<_>>().join(" ") };
        let (id, case) = crate::mcp_tools_design::variance_fixture_cases()
            .into_iter()
            .find(|(_, case)| case["from_manual"] == Value::Bool(true))
            .expect("one fixture case IS the manual's canonical component");
        assert_eq!(
            collapse(case["template"].as_str().unwrap_or_default()),
            collapse(&template),
            "[{id}] the shared fixture's template must BE the manual's, whitespace aside"
        );
        for (field, from_manual) in [("states_json", &states), ("params_json", &params)] {
            let fixture: Value = serde_json::from_str(case[field].as_str().unwrap_or_default())
                .unwrap_or_else(|_| panic!("[{id}] the fixture's {field} is valid JSON"));
            let manual: Value = serde_json::from_str(from_manual)
                .unwrap_or_else(|_| panic!("the manual's {field} is valid JSON"));
            assert_eq!(
                fixture, manual,
                "[{id}] the shared fixture's {field} must BE the manual's"
            );
        }
    }

    /// THE SAME DISCIPLINE, applied to the SIX PARAM TYPES (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`,
    /// family 1). The list of accepted `type` words lives at THREE places — the constant the write
    /// path refuses outside of, the renderer's constant, and this manual's prose — and the prose was
    /// moored to neither. A seventh type accepted by the write path alone would be taught nowhere
    /// and rendered as a free-text field with its `options` silently dropped; a type removed from
    /// the constant would go on being taught, and every agent believing the manual would have its
    /// synthesis refused.
    ///
    /// Like the canonical component above, this is not a COPY of the list: it EXTRACTS the bullets
    /// out of the RENDERED manual, in order, and confronts them with
    /// `contract/design-fixtures.json` — the same file the write path's constant
    /// (`the_shared_fixture_pins_the_six_param_types`) and the renderer's (`params.test.ts`) are
    /// held to. It fails the day any one of the three moves alone.
    #[test]
    fn the_manual_teaches_exactly_the_six_param_types_the_write_path_accepts() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Prototype, None, &spec_id)
            .unwrap()
            .unwrap();
        let section = crate::mcp_tools_design::design_fixture_section("param_types");
        let heading = section["manual_heading"]
            .as_str()
            .expect("the fixture names the manual's heading");
        let at = text
            .find(heading)
            .unwrap_or_else(|| panic!("the manual carries the `{heading}` section"));

        // The bullets, in order. A bullet opens with `* \`<type>\``; a line indented past the bullet
        // marker CONTINUES the current one; anything else ENDS the list.
        let mut taught: Vec<String> = Vec::new();
        let mut ended = false;
        for line in text[at..].lines().skip(1) {
            if let Some(rest) = line.trim().strip_prefix("* `") {
                taught.push(rest.split('`').next().unwrap_or_default().to_string());
                continue;
            }
            if !taught.is_empty() && !line.starts_with("        ") {
                ended = true;
                break;
            }
        }
        assert!(
            ended && !taught.is_empty(),
            "the `{heading}` section is a bullet list this guard can read: {taught:?}"
        );

        let pinned: Vec<&str> = section["types"]
            .as_array()
            .expect("the types")
            .iter()
            .map(|t| t.as_str().expect("a type"))
            .collect();
        assert_eq!(
            taught, pinned,
            "the manual bullets EXACTLY the accepted types, in the accepted order"
        );

        // …the one that is deprecated is TAUGHT as deprecated, never offered as a plain choice…
        let deprecated = section["deprecated"].as_str().expect("the deprecated type");
        let bullet_at = text[at..]
            .find(&format!("* `{deprecated}`"))
            .expect("the manual bullets the deprecated type");
        let bullet = &text[at + bullet_at..];
        let bullet_end = bullet[1..].find("* `").map_or(bullet.len(), |i| i + 1);
        assert!(
            bullet[..bullet_end].contains("DEPRECATED"),
            "`{deprecated}` is taught as DEPRECATED — it is parsed, never offered"
        );

        // …and an unknown type is taught as REFUSED, not repaired. The renderer coerces an unknown
        // type towards `text` because it only DISPLAYS a row already stored; that is not a second
        // opinion about the write, and the manual is where the difference is stated.
        assert_eq!(
            section["unknown_type_is_refused"],
            Value::Bool(true),
            "the fixture states the write-path rule"
        );
        assert!(
            text[at..].contains("AN UNKNOWN `type` IS REFUSED, NOT REPAIRED"),
            "the manual says an unknown type is refused, never silently turned into `text`"
        );
    }

    /// The SAME discipline, applied to the REFERENCE capability (finding
    /// `01KYSCZ822K4ZFAAYTG6GT6YWZ`): the manual's canonical `data-nyx-use` line is EXTRACTED from
    /// the rendered manual and handed to the very reader the render uses and the very validator the
    /// write path applies. It fails the day the taught attribute names drift from the parsed ones —
    /// a `data-nyx-uses` typo in the text would otherwise teach something nothing reads.
    #[test]
    fn the_manuals_canonical_reference_is_the_one_the_render_and_the_write_path_read() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Prototype, None, &spec_id)
            .unwrap()
            .unwrap();
        let section_at = text
            .find("REFERENCE A SHARED SUB-COMPONENT, DO NOT COPY IT")
            .expect("the manual teaches the reference capability");
        let example = text[section_at..]
            .lines()
            .find(|line| {
                line.contains("data-nyx-use=\"Spinner\"") && line.contains("data-nyx-param-")
            })
            .expect("the section carries the canonical reference line")
            .trim()
            .to_string();

        // 1. The READER the render uses finds it, with its param and its fixed axis.
        let sites = crate::render::reference_sites(&example);
        assert_eq!(sites.len(), 1, "one reference site: {example}");
        assert_eq!(sites[0].component, "Spinner");
        assert_eq!(
            sites[0].params,
            vec![("label".to_string(), "{{label}}".to_string())],
            "the taught attribute really names the param it claims to: {example}"
        );
        assert_eq!(sites[0].axes, vec![("size".to_string(), "sm".to_string())]);

        // 2. The VALIDATOR the write path applies accepts it against the component it describes.
        let catalogue = vec![crate::render::RenderComponent {
            name: "Spinner".into(),
            template: "<i class=\"animate-spin\">{{label}}</i>".into(),
            states_json: Some(
                r#"{"axes":[{"name":"size","values":[{"name":"sm","classes":"h-3 w-3"}]}]}"#.into(),
            ),
            params_json: Some(r#"[{"name":"label","type":"text","required":true}]"#.into()),
            css_slot: None,
        }];
        let warnings = crate::mcp_tools_design::reference_warnings("Button", &example, &catalogue);
        assert!(
            warnings.is_empty(),
            "the manual's canonical reference must not be warned about by the validator it \
             teaches: {warnings:?}"
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
        let text = render_agent_context(&db, AgentRole::Prototype, None, &spec_id)
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
            text.contains("Where `project_id` comes from"),
            "the manual SAYS where the id it keeps asking for comes from",
        );
        // …and it must not tell the agent to go and find a workspace it does not have.
        assert!(
            !text.contains("(not anchored yet)"),
            "the workspace-less state is stated as normal, not as a missing prerequisite",
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
