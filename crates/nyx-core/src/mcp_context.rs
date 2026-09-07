//! MCP **agent-manual render** — the text nyx injects into a `/nspec`, `/ndev` or `/ndoc` session,
//! and the CHARTER it injects into every sub-agent (PDR-0031).
//!
//! # What it renders
//!
//! The behaviour-tree engine is gone. What replaces it is prose: ONE manual per command, and one
//! charter for every sub-agent, whose text is `docs/product/agent-manuals.md` — the delivered
//! source, WORD FOR WORD. This module does not author that text; it *reproduces* it, substituting
//! only the `{{…}}` blocks nyx resolves live (the Spec, its tasks, the workspace, the managed
//! commands, the project, the category discipline).
//!
//! That is why the four fixed texts are RESOURCES ([`NSPEC_MANUAL`], [`NDEV_MANUAL`],
//! [`NDOC_MANUAL`], [`SUBAGENT_CHARTER`]) rather than string literals scattered through the render:
//! a `const` a renderer builds by concatenation is a text somebody edits a sentence of, and the doc
//! and the binary then say different things. The resources ARE the fenced blocks of the doc, and a
//! test in this module replays the doc against them.
//!
//! # Three roles, and an anchor that is never a run
//!
//! `spec` | `dev` | `doc`. `coder`, `auditor` and `prototype` are NOT nyx roles any more — they are
//! sub-agents the orchestrator briefs itself, framed by the charter.
//!
//! A render anchors on `spec_id` → `workspace_id` → `cwd`, in that order ([`SpecAnchor`]). The
//! `spec_id` is the RAW reference token the human typed (canonical id, `KEY-3`, `#3`, `3`); nyx
//! translates it through the shared grammar ([`crate::spec_ref`]). And the REFUSAL of a reference
//! that designates no Spec — or of a location that designates several — comes from HERE: it used to
//! be `run_start`'s, and the session hook already relays this one's sentence to the model.

use serde_json::{json, Value};

use crate::command::RunState;
use crate::db::{self, Db};
use crate::mcp::{RpcCode, RpcError};
use crate::mcp_runtime::{anchor_str, internal_db, optional_str, require_str};
use crate::mcp_tools_core::ToolOutcome;

/// `agent_context` — render a command's manual (spec | dev | doc) from nyx state.
pub const AGENT_CONTEXT_TOOL: &str = "agent_context";

/// How many OPEN backlog ideas the /nspec manual surfaces up front (a bounded preview — the author
/// then `idea_search`es the concrete topic; this is only the "there is a backlog, look at it" nudge,
/// never a full dump).
const SPEC_AUTHORING_IDEA_PREVIEW: i64 = 20;

/// How many sibling Specs a refusal lists back at the caller before it stops naming them one by one.
const REFUSAL_CANDIDATE_PREVIEW: usize = 20;

// ===========================================================================
// The four FIXED texts — `docs/product/agent-manuals.md`, byte for byte.
//
// Each file is the content of ONE fenced block of that document, extracted unchanged.
// `the_manual_resources_are_the_fenced_blocks_of_the_canonical_doc` replays the doc against them,
// so a sentence edited on one side and not the other is a red test, not a silent divergence.
// ===========================================================================

/// §1 — the `/nspec` manual (role `spec`).
const NSPEC_MANUAL: &str = include_str!("../resources/agent-manuals/nspec.md");
/// §2 — the `/ndev` manual (role `dev`).
const NDEV_MANUAL: &str = include_str!("../resources/agent-manuals/ndev.md");
/// §3 — the `/ndoc` manual (role `doc`).
const NDOC_MANUAL: &str = include_str!("../resources/agent-manuals/ndoc.md");
/// §4 — the charter injected into EVERY sub-agent, whatever mission the orchestrator wrote it.
const SUBAGENT_CHARTER: &str = include_str!("../resources/agent-manuals/subagent-charter.md");

/// The agent ROLES a manual can be rendered for (PDR-0031). THREE, and they are the three nyx
/// commands: `/nspec` authors the Spec, `/ndev` builds it, `/ndoc` reconciles the docs with what it
/// shipped. There is no `coder`, no `auditor`, no `prototype` — those are sub-agents the
/// orchestrator briefs itself, and the charter is what nyx gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Spec,
    Dev,
    Doc,
}

impl AgentRole {
    /// EVERY role, in the order the `role` argument advertises them. The ONE list a sweep can walk.
    pub(crate) const ALL: [AgentRole; 3] = [AgentRole::Spec, AgentRole::Dev, AgentRole::Doc];

    /// Parse the `role` argument. An unknown role → `None` (the caller maps it to
    /// `invalid_argument`).
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "spec" => Some(AgentRole::Spec),
            "dev" => Some(AgentRole::Dev),
            "doc" => Some(AgentRole::Doc),
            _ => None,
        }
    }

    /// The lowercase role SLUG — the value the `role` argument takes.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            AgentRole::Spec => "spec",
            AgentRole::Dev => "dev",
            AgentRole::Doc => "doc",
        }
    }

    /// The manual this role reads — the FIXED text, blocks unsubstituted.
    fn manual(self) -> &'static str {
        match self {
            AgentRole::Spec => NSPEC_MANUAL,
            AgentRole::Dev => NDEV_MANUAL,
            AgentRole::Doc => NDOC_MANUAL,
        }
    }

    /// Whether this role can render WITHOUT a Spec. Only `/nspec` can: a Spec is a PROJECT artifact
    /// the author is about to CREATE, so "no Spec here" is its nominal entry state. `/ndev` and
    /// `/ndoc` work ON a Spec, so they are refused rather than started on a guess — the contract the
    /// three command stubs state.
    fn renders_without_spec(self) -> bool {
        matches!(self, AgentRole::Spec)
    }
}

/// RENDER a role's manual with its `{{…}}` blocks unsubstituted — the FIXED text, as delivered. The
/// one surface that reads a manual without a Spec behind it (the agent-facing prose sweeps in
/// `mcp.rs`, which check what nyx SAYS rather than what a given call resolves).
pub fn render_role_template(role: AgentRole) -> String {
    role.manual().to_string()
}

// ===========================================================================
// `{{category_discipline}}` — the discipline nyx attaches to a Spec's CATEGORY.
//
// PDR-0017 gives a Spec ONE category, and ADR-0006 hangs a real constraint on it: a Spec classified
// `bug` owes a red-before-green proof, the other genres owe nothing. With the engine withdrawn
// nothing EXECUTES that sequence any more — the orchestrator does, through the managed commands —
// so the text says what each reader owes and names no node, no gate slot and no run.
// ===========================================================================

// THE SLUG THIS BLOCK IS KEYED ON is `db::CATEGORY_SLUG_BUG`, named at its SOURCE rather than
// restated here, so the literal `"bug"` is written once in the crate.

/// The HEADING every category-discipline block opens with — and therefore the MARKER a render
/// asserts on ("this manual carries the discipline", "that one does not").
pub const CATEGORY_DISCIPLINE_HEADING: &str = "## Category discipline";

/// The `bug` discipline for the SPEC AUTHOR — what the Spec must carry for a reproduction to be
/// POSSIBLE at all.
///
/// What no process can produce is a defect described precisely enough that a check CAN be pointed at
/// it. That is written here, once, by the author — and every later step spends it: the sub-agent
/// turns it into the red, the orchestrator re-reads it to decide whether the red was about this
/// defect. Hence the five elements are NAMED: a text that said "be precise" gets prose.
const BUG_DISCIPLINE_SPEC: &str = "\
This Spec is classified `bug`, and that classification hangs a red-before-green proof on it: an \
identified check must be OBSERVED failing BEFORE the fix and passing after. What that check can even \
BE is decided here, by what this Spec says about the defect — so a bug report is not a Spec until it \
is a REPRODUCTION CONTRACT.\n\
\n\
It owes five things, and each one has to be checkable by somebody who was not there:\n\
\n\
- the OBSERVED behaviour — what the system actually does, from someone who saw it: the exact input, \
  the state it was in, the version / platform, and what appeared. Not \"it is broken\".\n\
- the EXPECTED result — what it should have done instead, stated so that the gap between the two is \
  a difference of fact and not of taste.\n\
- a COUNTER-EXAMPLE — the neighbouring case that behaves CORRECTLY. It is what separates a defect \
  from a design somebody dislikes, and it is what tells the fix where to stop.\n\
- the SCOPE — which inputs, versions, platforms, entry points and data shapes are affected, and \
  which are explicitly NOT. An unbounded report cannot be closed, because nothing says which cases \
  the fix owes.\n\
- a REPRODUCTION STRATEGY that is VERIFIABLE — a sequence a stranger can run from a stated starting \
  state, whose failure is OBSERVABLE without judgement: a command that exits non-zero, an assertion \
  that fails, a named screen state. \"It feels slow\" is not observable; \"the call takes over 5 s \
  where the same call on an empty cache takes 200 ms\" is. Name what will carry it — which test, \
  which command, or which manual protocol.\n\
\n\
Write all of it for the person who will have to make the defect FAIL ON PURPOSE before touching the \
code, and for whoever will later re-read that failure and ask whether it was about THIS defect. \
Neither of them has anything but what this Spec says.\n\
\n\
IF THE REPRODUCTION CANNOT BE AUTOMATED on this project — it needs a device, a third-party account, \
a human gesture, a state no command can set up — SAY SO here, and write the manual protocol \
instead: the steps, and what the observer will see while the defect is present. That is a declared \
route, observed once before the fix and once after, never a way out of the proof.";

/// The `bug` discipline for the DEV ORCHESTRATOR — the sequence it owes, and what does not count as
/// a red.
///
/// The manual already says "the reproducing test task comes first and must be seen RED by you before
/// any fix task starts, and GREEN after". This says what "seen RED" means, because the failure mode
/// is an honest one: a check that could not RUN also comes back "not green", and accepting that as
/// the reproduction lets a run which verified nothing stand as the proof.
const BUG_DISCIPLINE_DEV: &str = "\
This Spec is classified `bug`, so it owes a proof the other categories do not, and YOU are the one \
who observes it: an identified check FAILING before the fix and PASSING after. You run both through \
nyx (start_command → wait_for_command → get_command_output), and you read the output yourself — a \
sub-agent's account of a red is not a red.\n\
\n\
- REPRODUCE BEFORE ANY REPAIR. The reproducing test task is the FIRST task, and no fix task starts \
  before you have seen its check fail on the code as it stands. A fix written before anything went \
  red proves nothing: the symptom may have been a stale cache, a restart or an unrelated commit.\n\
- THE SAME CHECK, AFTER. The green that closes the bug is the SAME command in the SAME cwd as the \
  red. A red obtained on one command and a green obtained on another demonstrate nothing together.\n\
- A CHECK THAT COULD NOT RUN IS NOT A RED. Separate what the command DID from what it ANSWERED: a \
  timeout, a cancellation, a spawn that failed, any fault that left no exit status to read proves \
  nothing either way. Never present one as the reproduction. And know what an exit status alone \
  cannot tell you — a missing binary or a test file that does not compile terminates non-zero just \
  as convincingly as a real reproduction. Only the PAIR catches it: the same command must exit 0 \
  after the fix, and a broken invocation never will.\n\
- THE RED IS ABOUT THE RIGHT DEFECT. Read the reproducing test itself against the Spec's OBSERVED \
  behaviour, its EXPECTED result and its COUNTER-EXAMPLE, and ask whether THIS failure is THAT \
  defect. Nothing executes that question for you.\n\
\n\
IF THE REPRODUCTION CANNOT BE AUTOMATED — the defect needs a device, a third-party account, a human \
gesture, a state no command can set up — say so to the human and run the Spec's manual protocol \
instead: observed once before the fix, observed again after, the same steps both times, both \
recorded. Do NOT weaken the check until something goes red, and do NOT proceed on a run that could \
not execute.";

/// The `bug` discipline injected into a SUB-AGENT's charter — the half that belongs to whoever
/// actually writes the reproduction.
const BUG_DISCIPLINE_SUBAGENT: &str = "\
## Category discipline — `bug`\n\
\n\
This Spec is classified `bug`. You do not get to assert that the fix works: the defect is made to \
FAIL first, on purpose, and only then repaired.\n\
\n\
- Write the reproducing test FIRST, under the project's convention, and run it through nyx before \
  you touch production code. It stays as this defect's anti-regression case.\n\
- The green that closes it is the SAME command in the SAME cwd as the red.\n\
- A run that could not execute (a timeout, a cancellation, a spawn that failed) is NOT a red — it \
  proves nothing either way, and presenting one as the reproduction is the failure this discipline \
  exists to prevent.\n\
- Report BOTH attempts: the command, the cwd, and what each one returned.\n\
\n\
If the reproduction cannot be automated — it needs a device, a third-party account, a human gesture, \
a state no command can set up — STOP and say so, with what you tried. Do not weaken the check until \
something goes red, and do not proceed on a run that verified nothing.";

/// The DISCIPLINE nyx injects for a `role` working on a Spec whose persisted category is `slug` —
/// `None` when the pair owes none (every role on a category with no discipline, and `doc` on any
/// category at all: it syncs documentation after the fact and decides nothing about a proof).
///
/// PURE — no DB: the (role, slug) pair is all it takes, which is what lets a test snapshot it
/// without a fixture.
pub fn category_discipline(role: AgentRole, slug: &str) -> Option<String> {
    let body = match (slug, role) {
        (db::CATEGORY_SLUG_BUG, AgentRole::Spec) => BUG_DISCIPLINE_SPEC,
        (db::CATEGORY_SLUG_BUG, AgentRole::Dev) => BUG_DISCIPLINE_DEV,
        _ => return None,
    };
    Some(format!(
        "{CATEGORY_DISCIPLINE_HEADING} — `{slug}`\n\n{body}"
    ))
}

/// The category discipline a SUB-AGENT's charter carries for a Spec of category `slug`.
fn subagent_category_discipline(slug: &str) -> Option<String> {
    match slug {
        db::CATEGORY_SLUG_BUG => Some(BUG_DISCIPLINE_SUBAGENT.to_string()),
        _ => None,
    }
}

// ===========================================================================
// The `{{…}}` BLOCK renderers.
//
// Each one answers ONE marker of the manuals. They are small, pure over what they are handed, and
// they never re-word the manual around them: the fixed text is the resource's, these fill the holes.
// ===========================================================================

/// Render one structured section as a `### <Heading>\n\n<body>` block, or an empty string when the
/// body is blank (a blank section is omitted, not rendered as an empty heading).
fn section_block(heading: &str, body: &str) -> String {
    if body.trim().is_empty() {
        String::new()
    } else {
        format!("### {heading}\n\n{}\n\n", body.trim())
    }
}

/// The Spec's HUMAN KEY (`NYX-3`) — the form every nyx surface displays and the human types.
fn spec_key(project: Option<&db::Project>, spec: &db::Spec) -> String {
    match project {
        Some(p) => format!("{}-{}", p.key, spec.number),
        None => format!("#{}", spec.number),
    }
}

/// Render one SIBLING Spec of the project as a compact bullet — every anti-duplication list goes
/// through here, so no two of them say a Spec's identity differently.
fn spec_line(spec: &db::Spec) -> String {
    format!(
        "- #{} · {} · {} · spec_id={}\n",
        spec.number, spec.title, spec.status, spec.id
    )
}

/// `{{spec}}` — the Spec's identity plus its six structured sections, from the ACTIVE revision.
fn spec_block(
    spec: &db::Spec,
    revision: &db::SpecRevision,
    project: Option<&db::Project>,
    category_slug: Option<&str>,
) -> String {
    let mut out = format!("## Spec — {}\n\n", revision.title);
    out.push_str(&format!(
        "- key      : {} (#{})\n",
        spec_key(project, spec),
        spec.number
    ));
    out.push_str(&format!("- spec_id  : {}\n", spec.id));
    out.push_str(&format!("- status   : {}\n", spec.status));
    out.push_str(&format!(
        "- category : {}\n",
        category_slug.unwrap_or("(none — spec_set_category assigns one)")
    ));
    out.push_str(&format!("- revision : {}\n\n", revision.revision_number));
    out.push_str(&section_block("Problem", &revision.problem));
    out.push_str(&section_block("Solution", &revision.solution));
    out.push_str(&section_block("User stories", &revision.user_stories));
    out.push_str(&section_block(
        "Implementation decisions",
        &revision.implementation_decisions,
    ));
    out.push_str(&section_block(
        "Testing decisions",
        &revision.testing_decisions,
    ));
    out.push_str(&section_block("Out of scope", &revision.out_of_scope));
    out.trim_end().to_string()
}

/// `{{spec}}` for the /nspec AUTHORING case — there is no Spec yet, and saying so is the point.
/// `notice` is [`UnanchoredSignal::notice`]: WHY none is in scope (a location with none, or no
/// location at all), so the author reads it instead of inferring it from a null field.
fn no_spec_block(notice: &str) -> String {
    format!(
        "## Spec — none yet\n\n{notice}You are authoring a NEW Spec in this project. Create it with \
         spec_create(project_id=…, title=…) once the interview is over — a Spec belongs to the \
         PROJECT, and nyx poses the dev workspace anchor later."
    )
}

/// `{{project}}` — the project the Spec belongs to and its logical repositories. The `project_id` is
/// on the line because it is the value every `design_*` tool takes.
fn project_block(
    project: Option<&db::Project>,
    repos: &[db::Repository],
    project_id: &str,
) -> String {
    let mut out = String::from("## Project\n\n");
    match project {
        Some(p) => out.push_str(&format!(
            "- project: {} (key={}, project_id={})\n",
            p.name, p.key, p.id
        )),
        None => out.push_str(&format!(
            "- project: (unknown project row) (project_id={project_id})\n"
        )),
    }
    let live: Vec<&db::Repository> = repos.iter().filter(|r| r.archived_at.is_none()).collect();
    if live.is_empty() {
        out.push_str("- repos: (none)\n");
    } else {
        for repo in live {
            let optional = if repo.optional { " · optional" } else { "" };
            out.push_str(&format!(
                "- repo: {} → {}{}\n",
                repo.name, repo.relative_path, optional
            ));
        }
    }
    out.trim_end().to_string()
}

/// `{{existing_specs}}` — the project's other Specs and its open idea backlog, the two lists the
/// /nspec manual's "Before creating: idea_search + spec_list" step is about. Rendered even when both
/// are empty: "there is nothing tracked" is an answer, an absent section is not.
fn existing_specs_block(
    project: Option<&db::Project>,
    siblings: &[db::Spec],
    ideas: &[db::Idea],
) -> String {
    let mut out = String::from("## Existing Specs in the project (anti-duplication)\n\n");
    let Some(project) = project else {
        out.push_str(
            "_No project resolved from this call — pass a project_id, or run from inside a nyx \
             workspace folder, then spec_list and idea_search before you author._",
        );
        return out;
    };
    if siblings.is_empty() {
        out.push_str("_No other Spec in this project yet._\n\n");
    } else {
        for s in siblings {
            out.push_str(&spec_line(s));
        }
        out.push('\n');
    }
    out.push_str("## Open ideas — the backlog to search before you spec\n\n");
    if ideas.is_empty() {
        out.push_str(
            "_No open idea in this project yet — still, idea_search the concrete subject before you \
             author, in case one is tracked._\n\n",
        );
    } else {
        for i in ideas {
            let theme = i.theme.as_deref().unwrap_or("—");
            out.push_str(&format!("- {} · {} · theme={}\n", i.id, i.title, theme));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "idea_search(project_id=\"{}\", query=\"<the subject>\") is the call; idea_implement links \
         the idea this Spec resolves.",
        project.id
    ));
    out.trim_end().to_string()
}

/// One task of the `{{tasks}}` block: its STATUS, its identity, its KIND, its PHASE as a LABEL, and
/// what it is BLOCKED BY.
///
/// The phase is a label and nothing else (PDR-0031, and the manuals' own words): nyx numbers phases
/// in first-seen order and no reader here branches on one. The blocking edges are the plan's real
/// order, so they get their own indented line rather than being crushed into the first — an
/// orchestrator reads them to build its frontier, and that is the one thing it must not misread.
fn task_line(task: &db::Task, phases: &[db::SpecPhase], blockers: &[&db::Task]) -> String {
    let mut out = format!(
        "- [{}] {} · task_id={} · kind={}",
        task.status, task.title, task.id, task.kind
    );
    if let Some(number) = task.phase {
        let name = phases
            .iter()
            .find(|p| p.number == number)
            .map(|p| p.name.as_str())
            .unwrap_or("");
        if name.is_empty() {
            out.push_str(&format!(" · phase={number}"));
        } else {
            out.push_str(&format!(" · phase=\"{name}\""));
        }
    }
    if let Some(sev) = task.severity.as_deref() {
        out.push_str(&format!(" · severity={sev}"));
    }
    if let Some(cmd) = task
        .verification_command
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        out.push_str(&format!(" · verification_command: {cmd}"));
    }
    out.push('\n');
    if blockers.is_empty() {
        out.push_str("    blocked by: nothing — this task can start now\n");
    } else {
        let list = blockers
            .iter()
            .map(|b| format!("[{}] {} (task_id={})", b.status, b.title, b.id))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("    blocked by: {list}\n"));
    }
    if let Some(reason) = task
        .blocked_reason
        .as_deref()
        .filter(|r| !r.trim().is_empty())
    {
        out.push_str(&format!("    blocked because: {reason}\n"));
    }
    out
}

/// `{{tasks}}` — the plan, in its own order, with the statuses, the blocking dependencies and the
/// phase labels. This is the /ndev orchestrator's frontier: everything it needs to decide what can
/// start NOW is on these lines, so it never has to walk the graph task by task.
fn tasks_block(tasks: &[db::Task], edges: &[db::TaskDependency], phases_json: &str) -> String {
    let mut out = String::from("## Tasks — the plan, in order\n\n");
    if tasks.is_empty() {
        out.push_str("_No task yet — the Spec has not been decomposed._");
        return out;
    }
    out.push_str(
        "`blocked by` names the tasks that must be done first; a task blocked by nothing can start \
         now, and independent tasks run in PARALLEL. A `phase=` is a grouping LABEL only — nothing \
         depends on it.\n\n",
    );
    let phases = db::parse_phase_plan(phases_json);
    for task in tasks {
        // Only `blocks` edges gate ordering; `relates_to` is a soft link, and counting it would make
        // a task look blocked by something it merely mentions.
        let blockers: Vec<&db::Task> = edges
            .iter()
            .filter(|e| e.task_id == task.id && e.type_ == db::DEPENDENCY_TYPE_BLOCKS)
            .filter_map(|e| tasks.iter().find(|t| t.id == e.depends_on_task_id))
            .collect();
        out.push_str(&task_line(task, &phases, &blockers));
    }
    out.trim_end().to_string()
}

/// `{{workspace}}` — the dev workspace the Spec is anchored on, its path, its branch and its repos.
/// `None` renders what is missing and the call that fixes it, never a blank.
fn workspace_block(context: Option<&db::SpecWorkspaceContext>) -> String {
    let mut out = String::from("## Workspace\n\n");
    let Some(ctx) = context else {
        out.push_str(
            "_No dev workspace anchored on this Spec yet — create_workspace(spec_id=…) opens one on \
             the project's convention._",
        );
        return out;
    };
    out.push_str(&format!(
        "- workspace: {} (workspace_id={})\n",
        ctx.workspace.name, ctx.workspace.id
    ));
    out.push_str(&format!("- path: {}\n", ctx.workspace.root_path));
    out.push_str(&format!(
        "- branch: {}\n",
        ctx.workspace.branch.as_deref().unwrap_or("(none)")
    ));
    out.push_str(&format!(
        "- project: {} (key={}, project_id={})\n",
        ctx.project.name, ctx.project.key, ctx.project.id
    ));
    let live: Vec<&db::Repository> = ctx
        .repos
        .iter()
        .filter(|r| r.archived_at.is_none())
        .collect();
    if live.is_empty() {
        out.push_str("- repos: (none)\n");
    } else {
        for repo in live {
            let optional = if repo.optional { " · optional" } else { "" };
            out.push_str(&format!(
                "- repo: {} → {}{}\n",
                repo.name, repo.relative_path, optional
            ));
        }
    }
    out.trim_end().to_string()
}

/// The command lines of a workspace — one per instance, `name · command · state ·
/// instance_id=…` (plus `· group_id=…` when it belongs to a multi-pane group), and under each
/// line its NOTE FOR THE AGENT when the human wrote one: the thing that says WHEN to run the
/// command, PDR-0031's replacement for the withdrawn gate slots. Same rendering as the ambient
/// SessionStart block ([`push_command_lines`]), so a command reads the same everywhere.
fn command_lines(rows: &[db::WorkspaceCommandLine]) -> String {
    rows.iter()
        .map(|row| {
            let state = RunState::from_db_str(&row.last_state);
            let mut line = format!(
                "- {} · {} · {} · instance_id={}",
                row.name,
                render_command_text(&row.command),
                state.as_db_str(),
                row.id
            );
            if let Some(group_id) = row.group_id.as_deref().filter(|g| !g.is_empty()) {
                line.push_str(&format!(" · group_id={group_id}"));
            }
            if let Some(note) = render_note_text(row.description.as_deref()) {
                line.push_str(NOTE_PREFIX);
                line.push_str(&note);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `{{commands}}` — the workspace's managed commands, as a section of its own.
fn commands_block(rows: &[db::WorkspaceCommandLine]) -> String {
    let mut out = String::from("## Managed commands of this workspace\n\n");
    if rows.is_empty() {
        out.push_str(
            "_None registered on this workspace — add_command registers one, and until then any \
             check runs through create_terminal / send_to_terminal._",
        );
        return out;
    }
    out.push_str(&command_lines(rows));
    out.push_str(
        "\n\nThe state is the last PERSISTED one: list_commands answers live, start_command is a \
         no-op on one already running, and get_command_output is what a command printed.",
    );
    out
}

/// `{{commands_with_notes}}` — the same lines, notes included, under the header the charter
/// already carries (see [`command_lines`]).
fn commands_with_notes_block(rows: &[db::WorkspaceCommandLine]) -> String {
    if rows.is_empty() {
        return "- (none registered on this workspace)".to_string();
    }
    command_lines(rows)
}

/// `{{spec_language}}` — not a fact nyx stores, but a RESOLUTION ORDER, and stating it is the whole
/// point: two Specs of one project came out in two languages because nothing said how to decide.
const SPEC_LANGUAGE_RULE: &str = "resolved in this order: what the project's own CLAUDE.md \
                                  prescribes, then the language the project's existing Specs listed \
                                  below are written in; failing both, the language the human asked \
                                  in — one language for the whole Spec, and the same one when you \
                                  revise it";

/// Substitute the `{{…}}` blocks of a manual. The fixed text is passed through UNTOUCHED — this is
/// the only transformation a manual ever undergoes.
fn substitute(manual: &str, blocks: &[(&str, &str)]) -> String {
    // The resources are checked out through `core.autocrlf` on Windows, so `include_str!` may
    // carry CRLF there: the agent must read one manual whatever the checkout, and the parity
    // test below compares against the doc's `lines()`, which strip the CR.
    let mut out = manual.replace("\r\n", "\n");
    for (marker, value) in blocks {
        out = out.replace(marker, value);
    }
    out
}

/// Everything one render needs, read in ONE `with_conn` — the Spec, its revision, its tasks and
/// their edges, its workspace context, its project, its repos, its category, its workspace's managed
/// commands, and (for /nspec) the project's other Specs and open ideas.
struct RenderBundle {
    spec: db::Spec,
    revision: db::SpecRevision,
    tasks: Vec<db::Task>,
    edges: Vec<db::TaskDependency>,
    workspace_context: Option<db::SpecWorkspaceContext>,
    project: Option<db::Project>,
    repos: Vec<db::Repository>,
    siblings: Vec<db::Spec>,
    ideas: Vec<db::Idea>,
    category_slug: Option<String>,
    commands: Vec<db::WorkspaceCommandLine>,
}

/// Load a [`RenderBundle`] for `spec_id`, or `Ok(None)` when the Spec (or its active revision) does
/// not exist. `role` decides only what is worth QUERYING — the /nspec lists cost two reads the other
/// two roles would throw away.
fn load_bundle(db: &Db, role: AgentRole, spec_id: &str) -> Result<Option<RenderBundle>, RpcError> {
    db.with_conn(|c| -> Result<Option<RenderBundle>, diesel::result::Error> {
        let Some(spec) = db::get_spec(c, spec_id)? else {
            return Ok(None);
        };
        let Some(revision) = db::get_active_revision(c, spec_id)? else {
            return Ok(None);
        };
        let tasks = db::list_tasks_for_spec(c, spec_id)?;
        let mut edges = Vec::new();
        for task in &tasks {
            edges.extend(db::list_task_dependency_edges(c, &task.id)?);
        }
        let workspace_context = db::spec_workspace_context(c, spec_id)?;
        // The Spec's OWN project, read off `specs.project_id` and NOT through the workspace anchor:
        // a Spec belongs to a project from the moment it is created, while the workspace is the DEV
        // link, added later.
        let project = db::get_project(c, &spec.project_id)?;
        let repos = db::list_repositories(c, &spec.project_id)?;
        let (siblings, ideas) = if role == AgentRole::Spec {
            let siblings: Vec<db::Spec> = db::list_specs(c, Some(&spec.project_id))?
                .into_iter()
                .filter(|s| s.id != spec.id)
                .collect();
            let ideas = db::list_ideas(
                c,
                Some(&spec.project_id),
                db::IdeaStatusFilter::One(db::IDEA_STATUS_OPEN),
                None,
                SPEC_AUTHORING_IDEA_PREVIEW,
                0,
            )?;
            (siblings, ideas)
        } else {
            (Vec::new(), Vec::new())
        };
        // The PERSISTED category slug (PDR-0017) — `None` for an unclassified Spec. The SLUG, never
        // the name: the name is a human label and may be renamed into a homonym at any time.
        let category_slug = match spec.category_id.as_deref() {
            Some(id) => db::get_category(c, id)?.map(|category| category.slug),
            None => None,
        };
        let commands = match workspace_context.as_ref() {
            Some(ctx) => db::list_command_lines_for_workspace(c, &ctx.workspace.id)?,
            None => Vec::new(),
        };
        Ok(Some(RenderBundle {
            spec,
            revision,
            tasks,
            edges,
            workspace_context,
            project,
            repos,
            siblings,
            ideas,
            category_slug,
            commands,
        }))
    })
    .map_err(internal_db)
}

/// RENDER `role`'s manual for `spec_id`, LIVE from nyx state — the fixed text of
/// `agent-manuals.md` with its `{{…}}` blocks substituted, and nothing else. `Ok(None)` when the
/// Spec is unknown (the caller maps it to a refusal). Deterministic and side-effect-free.
pub fn render_agent_context(
    db: &Db,
    role: AgentRole,
    spec_id: &str,
) -> Result<Option<String>, RpcError> {
    let Some(b) = load_bundle(db, role, spec_id)? else {
        return Ok(None);
    };
    let project = b.project.as_ref();
    let discipline = category_discipline(role, b.category_slug.as_deref().unwrap_or_default())
        .unwrap_or_default();
    let spec = spec_block(&b.spec, &b.revision, project, b.category_slug.as_deref());
    let text = match role {
        AgentRole::Spec => substitute(
            NSPEC_MANUAL,
            &[
                ("{{spec_language}}", SPEC_LANGUAGE_RULE),
                ("{{category_discipline}}", &discipline),
                ("{{spec}}", &spec),
                (
                    "{{project}}",
                    &project_block(project, &b.repos, &b.spec.project_id),
                ),
                (
                    "{{existing_specs}}",
                    &existing_specs_block(project, &b.siblings, &b.ideas),
                ),
            ],
        ),
        AgentRole::Dev => substitute(
            NDEV_MANUAL,
            &[
                ("{{spec_key}}", &spec_key(project, &b.spec)),
                ("{{category_discipline}}", &discipline),
                ("{{spec}}", &spec),
                ("{{tasks}}", &tasks_block(&b.tasks, &b.edges, &b.spec.phases)),
                (
                    "{{workspace}}",
                    &workspace_block(b.workspace_context.as_ref()),
                ),
                ("{{commands}}", &commands_block(&b.commands)),
            ],
        ),
        AgentRole::Doc => substitute(
            NDOC_MANUAL,
            &[
                ("{{spec_key}}", &spec_key(project, &b.spec)),
                ("{{spec}}", &spec),
                (
                    "{{workspace}}",
                    &workspace_block(b.workspace_context.as_ref()),
                ),
            ],
        ),
    };
    Ok(Some(text))
}

/// RENDER the /nspec manual with NO Spec anchored — the AUTHORING entry, and the nominal state of a
/// project about to open its first Spec. Same fixed text, same blocks; `{{spec}}` says there is none
/// yet and WHY (`notice`), and the project half still carries the two anti-duplication lists.
fn render_spec_authoring(
    db: &Db,
    project: Option<&db::Project>,
    notice: &str,
) -> Result<String, RpcError> {
    type AuthoringLists = (Vec<db::Spec>, Vec<db::Idea>, Vec<db::Repository>);
    let (siblings, ideas, repos): AuthoringLists = match project {
        Some(p) => db
            .with_conn(|c| -> Result<AuthoringLists, diesel::result::Error> {
                let siblings = db::list_specs(c, Some(&p.id))?;
                let ideas = db::list_ideas(
                    c,
                    Some(&p.id),
                    db::IdeaStatusFilter::One(db::IDEA_STATUS_OPEN),
                    None,
                    SPEC_AUTHORING_IDEA_PREVIEW,
                    0,
                )?;
                let repos = db::list_repositories(c, &p.id)?;
                Ok((siblings, ideas, repos))
            })
            .map_err(internal_db)?,
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    let project_id = project.map(|p| p.id.as_str()).unwrap_or("(unresolved)");
    Ok(substitute(
        NSPEC_MANUAL,
        &[
            ("{{spec_language}}", SPEC_LANGUAGE_RULE),
            // No Spec ⇒ no category ⇒ no discipline. A "resolved later" placeholder would promise a
            // block this render can never produce.
            ("{{category_discipline}}", ""),
            ("{{spec}}", &no_spec_block(notice)),
            ("{{project}}", &project_block(project, &repos, project_id)),
            (
                "{{existing_specs}}",
                &existing_specs_block(project, &siblings, &ideas),
            ),
        ],
    ))
}

// ===========================================================================
// The SUB-AGENT CHARTER — §4, resolved from the PARENT TERMINAL.
// ===========================================================================

/// RENDER the sub-agent charter (`agent-manuals.md` §4) for the terminal `terminal_id`
/// (`NYX_TERMINAL_ID`), resolving terminal → workspace → anchored Spec → managed commands.
///
/// A sub-agent inherits its MISSION from the orchestrator and nothing else: not the workspace, not
/// the Spec, not the commands. nyx knows all three from the parent terminal, which is why the
/// `SubagentStart` hook relays this text as `additionalContext`.
///
/// IT NEVER REFUSES ON A MISSING ANCHOR, and that is the contract: a terminal attached to no
/// workspace, or a workspace anchoring no Spec, still gets the charter — with the unknown parts
/// NAMED. A sub-agent that reads "no Spec anchored on this workspace" can ask its orchestrator; one
/// that reads a raw `{{spec_id}}` can do nothing at all. Only a DB failure is an `Err`.
pub fn render_subagent_charter(db: &Db, terminal_id: &str) -> Result<String, RpcError> {
    let workspace = db
        .with_conn(|c| -> Result<Option<db::Workspace>, diesel::result::Error> {
            let Some(terminal) = db::get_terminal(c, terminal_id)? else {
                return Ok(None);
            };
            match terminal.workspace_id.as_deref() {
                Some(ws_id) => db::get_workspace(c, ws_id),
                None => Ok(None),
            }
        })
        .map_err(internal_db)?;

    let Some(workspace) = workspace else {
        return Ok(substitute(
            SUBAGENT_CHARTER,
            &[
                (
                    "{{workspace}}",
                    "unknown — this terminal is attached to no nyx workspace",
                ),
                ("{{path}}", "path unknown"),
                ("{{spec_key}}", "unknown"),
                (
                    "{{spec_id}}",
                    "no Spec anchored — ask your orchestrator for the spec_id",
                ),
                (
                    "{{commands_with_notes}}",
                    "- (unknown — no workspace resolved from this terminal)",
                ),
                ("{{category_discipline}}", ""),
            ],
        ));
    };

    // The workspace's ONE active Spec. The N case is NOT a refusal here (a charter is best-effort,
    // and the sub-agent's mission already names its subject): several anchored Specs simply leaves
    // the Spec unnamed, which is what the degraded wording below is for.
    let anchored = unique_spec_for_workspace(db, &workspace.id).unwrap_or(None);
    let commands = db
        .with_conn(|c| db::list_command_lines_for_workspace(c, &workspace.id))
        .map_err(internal_db)?;
    type AnchoredSpec = (db::Spec, Option<db::Project>, Option<String>);
    let resolved: Option<AnchoredSpec> = match anchored {
        Some(spec_id) => db
            .with_conn(|c| -> Result<Option<AnchoredSpec>, diesel::result::Error> {
                let Some(spec) = db::get_spec(c, &spec_id)? else {
                    return Ok(None);
                };
                let project = db::get_project(c, &spec.project_id)?;
                let slug = match spec.category_id.as_deref() {
                    Some(id) => db::get_category(c, id)?.map(|cat| cat.slug),
                    None => None,
                };
                Ok(Some((spec, project, slug)))
            })
            .map_err(internal_db)?,
        None => None,
    };

    let (key, id, discipline) = match &resolved {
        Some((spec, project, slug)) => (
            spec_key(project.as_ref(), spec),
            spec.id.clone(),
            slug.as_deref()
                .and_then(subagent_category_discipline)
                .unwrap_or_default(),
        ),
        None => (
            "unknown — no Spec is anchored on this workspace".to_string(),
            "no Spec anchored — ask your orchestrator for the spec_id".to_string(),
            String::new(),
        ),
    };

    Ok(substitute(
        SUBAGENT_CHARTER,
        &[
            ("{{workspace}}", &workspace.name),
            ("{{path}}", &workspace.root_path),
            ("{{spec_key}}", &key),
            ("{{spec_id}}", &id),
            (
                "{{commands_with_notes}}",
                &commands_with_notes_block(&commands),
            ),
            ("{{category_discipline}}", &discipline),
        ],
    ))
}

// ===========================================================================
// ANCHORING — `spec_id` → `workspace_id` → `cwd`, and the refusals it earns.
// ===========================================================================

/// THE ANCHOR a context call actually used — and the ONE place the anchoring ORDER of a manual
/// render lives (PDR-0031): `spec_id` → `workspace_id` → `cwd`. There is no `run_id` any more.
///
/// WHY THIS TYPE EXISTS. Two functions ask questions about that order: [`resolve_spec_id`], which
/// RESOLVES the Spec, and [`unanchored_signal`], which DIAGNOSES the case where nothing resolved.
/// They used to walk two hand-written copies of the same list, and two lists drift. The two `match`
/// blocks below are written WITHOUT a catch-all arm on purpose — adding a fourth anchor forces the
/// compiler to stop at [`unanchored_signal`] too, so a new anchor cannot be added
/// resolvable-but-undiagnosed, which is a diagnosis naming the argument the caller did not use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecAnchor<'a> {
    /// An explicit `spec_id` — the RAW reference token the human typed, in any of the four forms.
    Explicit(&'a str),
    /// A `workspace_id`: the ONE active Spec anchored on that dev workspace.
    Workspace(&'a str),
    /// A `cwd`: the Spec anchored on the workspace whose path matches it.
    Cwd(&'a str),
}

/// The FIRST anchor the caller passed, in [`SpecAnchor`]'s order; `None` when none was passed.
/// THE ONLY WALK of that order in this module.
fn spec_anchor(args: &Value) -> Result<Option<SpecAnchor<'_>>, RpcError> {
    if let Some(spec_id) = optional_str(args, "spec_id")? {
        return Ok(Some(SpecAnchor::Explicit(spec_id)));
    }
    if let Some(workspace_id) = optional_str(args, "workspace_id")? {
        return Ok(Some(SpecAnchor::Workspace(workspace_id)));
    }
    if let Some(cwd) = optional_str(args, "cwd")? {
        return Ok(Some(SpecAnchor::Cwd(cwd)));
    }
    Ok(None)
}

/// RESOLVE the Spec id for a manual render at the anchor [`spec_anchor`] stopped on. Returns
/// `Ok(None)` when none resolves — the caller's role decides what that means.
fn resolve_spec_id(db: &Db, args: &Value) -> Result<Option<String>, RpcError> {
    match spec_anchor(args)? {
        // THE ONE PLACE THE SHORT-REFERENCE GRAMMAR ENTERS a manual render: `#3` / `KEY-3` / `3`
        // work here and on every `spec_*` tool through the SAME entry, with no caller
        // re-implementing the forms. A project-relative `#N` / bare `N` on a call that anchors NO
        // project is refused BY NAME, before anything is read.
        Some(SpecAnchor::Explicit(spec_id)) => Ok(Some(
            resolve_spec_arg_project_relative_refused(db, args, "spec_id", spec_id)?,
        )),
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
/// was sent — a canonical id (untouched, no lookup), prose, or a reference designating no Spec. The
/// caller's own refusal therefore still sees the token the caller was given, and still names it:
/// widening the grammar adds resolution WITHOUT taking any existing refusal away.
///
/// THE PROJECT IS THE CALL'S OWN, never one this function goes looking for. `#N` and the bare `N`
/// carry no project, so they resolve in the one [`resolve_project`] derives from the anchors every
/// tool passes (`project_id` | `workspace_id` | `cwd`). No anchor ⇒ nothing to count in ⇒ the token
/// stands, and the caller refuses it naming [`crate::spec_ref::ANCHORLESS_FORMS`]. The canonical id
/// and `KEY-N` describe themselves, so they never pay for that resolution.
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
/// `proto_create` take NONE of the anchors [`resolve_project`] reads, so on them a number can never
/// be counted. Left to [`resolve_spec_arg`] alone the token simply stood, travelled down to the row
/// read, and came back as `unknown spec 3`: the exact sentence this grammar exists to retire, on a
/// field whose description had just PROMISED the form.
///
/// The refusal is raised HERE, before any read or write, and it is ACTIONABLE: it says why a number
/// cannot resolve on this call and names [`crate::spec_ref::ANCHORLESS_FORMS`] — what to send
/// instead. `invalid_argument`, not `invalid_id`: the Spec is not unknown, the FORM is unusable here.
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
///
/// THE ANCHORS ARE WALKED AT MOST ONCE. The guard needs the project to decide whether to refuse, and
/// the resolution needs the same project to count the number in — so the project the guard resolved
/// is HANDED to [`resolve_spec_ref_in`] instead of being recomputed by [`resolve_spec_arg`].
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
             cwd to read one from. Send {} in '{key}' instead; both carry their own project",
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

/// The `reason` marker on the AMBIGUOUS-location refusal — the machine handle a caller uses to tell
/// "this location anchors several Specs" apart from every other `invalid_argument`.
const AMBIGUOUS_SPEC_REASON: &str = "spec_ambiguous_for_workspace";

/// Resolve the ONE ACTIVE Spec anchored on `workspace_id`, REFUSING when several are — a workspace
/// with more than one started Spec cannot resolve one by location alone, so picking the newest
/// silently would attribute work to an ARBITRARY Spec. The refusal LISTS the candidates so the
/// caller can pass an explicit `spec_id`.
///
/// THE 0/1/N CONTRACT, and the only one: 0 candidates ⇒ `Ok(None)` (the caller's role decides
/// whether that is legitimate), 1 ⇒ `Ok(Some(id))`, N ⇒ `invalid_argument` carrying
/// `reason = spec_ambiguous_for_workspace` and the candidate list. BOTH location paths of
/// [`resolve_spec_id`] land HERE, so there is exactly one place where a location becomes a Spec and
/// NO caller anywhere selects the most recent candidate.
///
/// The candidate set is [`db::active_specs_for_workspace`]: the TERMINAL Specs a workspace
/// accumulates as it ships (`completed` / `cancelled`) are NOT candidates, so a workspace whose only
/// live Spec is X resolves to X however many finished ones share it.
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
/// workspace anchors NO active Spec. A LEGITIMATE state for /nspec — a project's first Spec starts
/// exactly there — so it is SIGNALLED for that role, and it is the refusal's reason for the other
/// two.
const NO_SPEC_ON_LOCATION_WARNING_CODE: &str = "spec_none_on_location";

/// The `agent_context` WARNING code for the OTHER case 0: NO workspace resolved at all — no
/// anchoring argument was passed, or the `cwd` matches no workspace nyx knows. That one is an
/// INVOCATION mistake (the observed symptom: an agent calling from the wrong folder and receiving a
/// perfectly normal-looking manual), which is exactly why it must not share a code with the
/// legitimate case above.
const NO_LOCATION_WARNING_CODE: &str = "spec_location_unresolved";

/// THE CASE-0 SIGNAL — the machine CODE and the human SENTENCE, emitted TOGETHER on the two surfaces
/// that must agree: the `warnings` array of the payload, and one explicit LINE inside the rendered
/// manual (an agent reading the text must not have to notice a null field). ONE sentence, built
/// once, so the two can never drift.
struct UnanchoredSignal {
    code: &'static str,
    message: String,
}

impl UnanchoredSignal {
    /// The `warnings` entry — `{ code, message }`.
    fn warning(&self) -> Value {
        json!({ "code": self.code, "message": self.message })
    }

    /// The LINE rendered into the manual itself. A blockquote, so it reads as a note ABOUT the
    /// manual rather than as one of its instructions.
    fn notice(&self) -> String {
        format!("> NO SPEC ANCHORED — {}\n\n", self.message)
    }
}

/// DIAGNOSE a case 0 of the 0/1/N contract: [`resolve_spec_id`] resolved NO Spec, and this says
/// WHICH of the two very different situations that is.
///
/// It reads the anchor through [`spec_anchor`] — the SAME single walk the resolution used, not a
/// second copy of the order — so the sentence can only ever name the argument the resolution
/// actually stopped on. An UNKNOWN `workspace_id` lands in [`NO_LOCATION_WARNING_CODE`] on purpose:
/// a location that names nothing is an invocation mistake, whatever argument carried it.
fn unanchored_signal(db: &Db, args: &Value) -> Result<UnanchoredSignal, RpcError> {
    let located = match spec_anchor(args)? {
        Some(SpecAnchor::Workspace(workspace_id)) => db
            .with_conn(|c| db::get_workspace(c, workspace_id))
            .map_err(internal_db)?,
        Some(SpecAnchor::Cwd(cwd)) => {
            let normalized = crate::pathnorm::normalize(cwd);
            db.with_conn(|c| best_workspace_for_cwd(c, &normalized))
                .map_err(internal_db)?
        }
        // An explicit `spec_id` either resolves or is refused by name, so it never reaches a case-0
        // diagnosis; no anchor at all is the same "located nothing" as a cwd matching nothing. Both
        // are spelled out rather than folded into a `_` arm: a fourth anchor added to [`SpecAnchor`]
        // must then break the build HERE, not silently fall through to the wrong sentence.
        Some(SpecAnchor::Explicit(_)) | None => None,
    };
    Ok(match located {
        Some(ws) => UnanchoredSignal {
            code: NO_SPEC_ON_LOCATION_WARNING_CODE,
            message: format!(
                "workspace {} (\"{}\") carries no ACTIVE Spec — the location is a real workspace, it \
                 just has none yet, which is the normal state of a project about to author its \
                 first",
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

/// WHAT THE PROJECT ACTUALLY HOLDS — the tail every refusal carries, so the human reading it back
/// can pick the reference to pass next. Empty-handed rather than silent: "this project holds no
/// Spec" is an answer, and it is the one that stops an agent authoring a duplicate.
fn project_holdings(db: &Db, project: Option<&db::Project>) -> String {
    let Some(project) = project else {
        return "No project resolved from this call either — pass a cwd inside the project's \
                workspace, or a project_id, so nyx can say what it holds."
            .to_string();
    };
    let specs = db
        .with_conn(|c| db::list_specs(c, Some(&project.id)))
        .unwrap_or_default();
    if specs.is_empty() {
        return format!(
            "Project {} (key={}) holds no Spec at all.",
            project.name, project.key
        );
    }
    let listed = specs
        .iter()
        .take(REFUSAL_CANDIDATE_PREVIEW)
        .map(|s| {
            format!(
                "{}-{} (\"{}\" · {} · spec_id={})",
                project.key, s.number, s.title, s.status, s.id
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let overflow = specs.len().saturating_sub(REFUSAL_CANDIDATE_PREVIEW);
    let more = if overflow > 0 {
        format!(" (+{overflow} more — spec_list)")
    } else {
        String::new()
    };
    format!(
        "Project {} (key={}) holds: {listed}{more}.",
        project.name, project.key
    )
}

/// THE REFUSAL OF A REFERENCE THE HUMAN TYPED — the one that MOVED here from `run_start` (PDR-0031).
///
/// It is the sentence the session hook relays into the model when no manual could be rendered
/// (`refusedReferenceBlock` reads it out of the JSON-RPC error), so it has to carry three things and
/// nothing else: the token AS TYPED, the four ACCEPTED FORMS, and what the project actually holds.
/// That is what lets an agent stop and ask the human WHICH Spec they meant instead of authoring a
/// duplicate of the very one they named.
fn unknown_reference_refusal(db: &Db, args: &Value, token: &str) -> RpcError {
    let project = resolve_project(db, args).unwrap_or(None);
    RpcError::new(
        RpcCode::InvalidId,
        format!(
            "'{token}' designates no Spec nyx can resolve here. A Spec reference is the canonical \
             id, the human key (e.g. KEY-3), '#3', or the bare number 3. {}",
            project_holdings(db, project.as_ref())
        ),
    )
}

/// THE REFUSAL OF A COMMAND THAT NAMED NO REFERENCE and whose location anchors no Spec — `/ndev` and
/// `/ndoc` only. They work ON a Spec, so a guess is worse than a refusal, and the refusal names the
/// candidates to pass as the reference (the contract the two command stubs state).
fn no_anchored_spec_refusal(
    db: &Db,
    args: &Value,
    role: AgentRole,
    signal: &UnanchoredSignal,
) -> RpcError {
    let project = resolve_project(db, args).unwrap_or(None);
    RpcError::new(
        RpcCode::InvalidId,
        format!(
            "/{} works ON a Spec and none is anchored here: {}. Pass the reference the human typed \
             as spec_id — the canonical id, the human key (e.g. KEY-3), '#3', or the bare number 3. \
             {}",
            role.slug(),
            signal.message,
            project_holdings(db, project.as_ref())
        ),
    )
    .with_data(json!({ "reason": signal.code }))
}

/// The best workspace whose `root_path` matches a normalized `cwd`: the workspace that IS the cwd or
/// an ancestor of it, the DEEPEST `root_path` winning. The single cwd→workspace selector shared by
/// the three cwd resolvers ([`resolve_spec_id`], [`resolve_workspace_for_start`],
/// [`resolve_project`]) — each then keeps the workspace itself, its Spec, or its project.
///
/// THE MATCHING AND THE TIE-BREAK ARE NOT RE-DERIVED HERE: this row-level wrapper delegates to
/// [`crate::resolve::resolve_workspace`], the repo's canonical cwd→workspace selector (also driving
/// terminal auto-attach), which matches through [`crate::pathnorm::is_ancestor_or_equal`] and ranks
/// on [`crate::pathnorm::depth`]. Two selectors that answer the same question must not drift apart:
/// this one used to compare with a hard-coded `/` while the canonical form uses `\` on Windows, so
/// NO nested cwd resolved there at all — only an exact `cwd == root_path` hit.
///
/// WHY LOWEST-ID (the convention, restated once because this path is the one that anchors Specs):
/// two matching workspaces can only tie on depth by having the very same `root_path`, and that is
/// reachable — uniqueness is `UNIQUE(project_id, root_path)`, so two PROJECTS may register one
/// folder. Ids are UUIDv7, hence time-ordered, so lowest-id is the INCUMBENT and the deliberate
/// OPPOSITE of newest-wins: registering a second workspace on a folder never re-points the cwd of
/// the first, and the answer for a given database never depends on when it is asked.
///
/// THE SYSTÈME WORKSPACE IS NOT A CANDIDATE. [`db::resolvable_workspaces`] excludes it by join on
/// `projects.kind`, and it must: its `root_path` is the HOME directory, an ancestor of nearly every
/// folder on the machine, so it would win the cwd of anything outside a registered project.
///
/// LIVENESS IS NOT PART OF THE RANKING — said here rather than left to be discovered. An ARCHIVED
/// workspace, or one whose worktree folder was cleaned up at merge, still competes. That is a
/// deliberate LIMIT: [`crate::resolve::resolve_workspace`] ranks the very same set for terminal
/// auto-attach, and one cwd must not resolve to one workspace for a terminal and another for a Spec.
fn best_workspace_for_cwd(
    conn: &mut diesel::SqliteConnection,
    normalized: &str,
) -> Result<Option<db::Workspace>, diesel::result::Error> {
    let workspaces = db::resolvable_workspaces(conn)?;
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

/// Resolve the dev WORKSPACE to ANCHOR a Spec on at the `start` transition (the anchor is posed at
/// ready→in_progress, never at authoring): an explicit `workspace_id`, else the workspace whose
/// `root_path` matches `cwd` (best/longest match — via the shared [`best_workspace_for_cwd`]
/// selector). `Ok(None)` when neither is given or resolves; the transition then poses no anchor. A
/// pure READ — the caller ([`crate::mcp_tools_spec::spec_transition`]) does the atomic write.
///
/// It is [`anchor_str`] and not a local check, because that is where the other anchoring arguments
/// read theirs from, and a fourth spelling of the same rule is how the rule drifts: `""` read as
/// `None` fell through to the `cwd`, then to no anchor at all, so a
/// `spec_transition(start, workspace_id: "")` moved the Spec `ready → in_progress` UNANCHORED and
/// said nothing. What is NOT changed is the meaning of ABSENCE: no `workspace_id` still falls
/// through to `cwd`, and a `cwd` that resolves nothing still starts the Spec unanchored — only the
/// empty STRING is refused.
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

/// RESOLVE the PROJECT of a call — the scope a `/nspec` render falls back on when no Spec is
/// anchored (a Spec is a PROJECT artifact the author CREATES), and the project a `#N` is counted in.
/// Same anchor order as [`resolve_spec_id`], but each anchor resolves the PROJECT instead of a Spec:
///   1. an explicit `project_id`;
///   2. the `workspace_id`'s project (`workspace.project_id`);
///   3. the project of the workspace whose `root_path` matches `cwd` (best/longest match).
///
/// Returns `Ok(None)` when none resolves — the caller renders a graceful manual, or names the
/// missing project in its refusal.
pub(crate) fn resolve_project(db: &Db, args: &Value) -> Result<Option<db::Project>, RpcError> {
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
                // Same longest-match resolution as the cwd → Spec path, but keep the workspace's
                // PROJECT (a project can be authored in even with zero Specs).
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

/// `agent_context` — `{ role, spec_id?, workspace_id?, cwd?, project_id? }` →
/// `{ role, spec_id, project_id, context, warnings }`. Render a command's manual LIVE from nyx
/// state. A pure READ (no `changed` effect).
///
/// `role` selects the manual (`spec` | `dev` | `doc`). `spec_id` is the RAW reference token the
/// human typed — nyx translates it, the caller never does. `cwd` resolves the project and the
/// workspace, and is what the `UserPromptSubmit` hook always sends; `workspace_id` is the same
/// anchor by id.
///
/// THE REFUSALS ARE THIS TOOL'S, since PDR-0031: a reference designating no Spec, a location
/// anchoring several, and (for `/ndev` / `/ndoc`) a location anchoring none. `/nspec` meets none of
/// the last two — a project with zero or several Specs is exactly where a new one gets authored.
pub fn agent_context(db: &Db, args: &Value) -> Result<ToolOutcome, RpcError> {
    let role_name = require_str(args, "role")?;
    let role = AgentRole::parse(role_name).ok_or_else(|| {
        RpcError::new(
            RpcCode::InvalidArgument,
            format!("unknown role '{role_name}' (accepted: spec, dev, doc)"),
        )
    })?;
    // The token AS TYPED, kept beside the resolution: a refusal has to name what the human wrote,
    // not the canonical id nyx failed to find.
    let typed = optional_str(args, "spec_id")?.map(str::to_string);
    match resolve_spec_id(db, args)? {
        Some(spec_id) => {
            // A `project_id` passed BESIDE a resolved Spec is CONFRONTED, never silently dropped: a
            // Spec carries its own project, the render reads THAT one, so an argument naming another
            // project decided nothing and used to leave a byte-identical manual behind — the caller
            // then believed it had scoped the render.
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
                             manual is always rendered for the OWNER. Drop the project_id, or ask \
                             for a Spec of project {asked}"
                        ),
                    ));
                }
            }
            let Some(context) = render_agent_context(db, role, &spec_id)? else {
                // The anchor produced an id no Spec answers to. That is only reachable through an
                // explicit reference (a location resolves against real rows), so the refusal names
                // the token the human typed and what the project holds.
                return Err(unknown_reference_refusal(
                    db,
                    args,
                    typed.as_deref().unwrap_or(&spec_id),
                ));
            };
            Ok(ToolOutcome::read(json!({
                "role": role_name,
                "spec_id": spec_id,
                // The Spec's OWN project — echoed beside the manual, so a caller reads the id it
                // must pass to every design_* tool without parsing the text.
                "project_id": spec_project,
                "context": context,
                // ALWAYS present, empty here: a caller reads `warnings` unconditionally instead of
                // branching on whether the key exists.
                "warnings": [],
            })))
        }
        None => {
            // An explicit reference that resolved nothing is a REFUSAL for every role, /nspec
            // included: they named an existing Spec, and authoring a second is how a duplicate of
            // the very one they meant gets ordered.
            if let Some(token) = typed.as_deref() {
                return Err(unknown_reference_refusal(db, args, token));
            }
            let signal = unanchored_signal(db, args)?;
            if !role.renders_without_spec() {
                return Err(no_anchored_spec_refusal(db, args, role, &signal));
            }
            let project = resolve_project(db, args)?;
            let context = render_spec_authoring(db, project.as_ref(), &signal.notice())?;
            Ok(ToolOutcome::read(json!({
                "role": role_name,
                "spec_id": Value::Null,
                "project_id": project.as_ref().map(|p| p.id.as_str()),
                "context": context,
                "warnings": [signal.warning()],
            })))
        }
    }
}

/// THE MCP HALF OF THE SYSTÈME DOMAIN BOUNDARY: `Some(RpcError)` when calling `tool` with these
/// `args` would aim a capability the Système scope does not have at the Système scope, `None`
/// otherwise.
///
/// One function, called from the TOP of each MCP dispatcher, over the allow-list — so a tool cannot
/// be added to the surface and silently gain power over the singleton: it is refused until it is
/// written down.
///
/// IT READS BOTH HALVES OF THE ALLOW-LIST, on both surfaces. The table is split in two NAMESPACES —
/// [`crate::system_scope::SYSTEM_SCOPE_MCP_CAPABILITIES`] (the agent's advertised tool names) and
/// [`crate::system_scope::SYSTEM_SCOPE_IPC_CAPABILITIES`] (the UI bridge's db-command names) — and
/// `system_scope::is_system_capability` reads both, so this guard does too. The consequence, stated
/// rather than hidden: an IPC-ONLY name (`command_list`, `attach_terminal`,
/// `workspace_runtime_snapshot`…) also traverses this guard UNREFUSED when it arrives on the agent
/// surface. It is harmless only because no such name is an advertised MCP tool, and the only thing
/// keeping it that way is
/// `system_scope::tests::the_two_halves_of_the_allow_list_do_not_share_a_name_with_the_live_mcp_surface`,
/// which fails the day one of them is advertised.
///
/// The refusal carries [`crate::mcp::RpcCode::SystemScopeForbidden`], never `invalid_id`: the
/// singleton EXISTS, and saying so is deliberate. Its id is not a secret and hiding it was never the
/// defense; an agent that holds the id gets the same refusal as one that guessed it.
///
/// A DB error while reading the scope is reported rather than swallowed — failing OPEN here would
/// turn a transient SQL hiccup into a hole in the boundary.
pub fn refuse_system_scope(db: &Db, tool: &str, args: &Value) -> Option<RpcError> {
    if !crate::system_scope::needs_scope_lookup(tool, args) {
        return None;
    }
    match db.with_conn(|c| crate::system_scope::guard_payload(c, tool, args)) {
        Ok(()) => None,
        Err(e) => Some(e.into()),
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
// The COMMANDS block of the AMBIENT session context — the workspace's, and the SHARED ones.
// ---------------------------------------------------------------------------
//
// It renders HERE, next to the manuals, and not in `mcp_tools_core` — this is the module that
// renders agent-facing TEXT, and the one place the wording of what an agent reads is reviewable in
// one pass. `mcp_tools_core::session_context_line` calls it with the workspace's instances, the
// Système scope's instances, and an overlay closure over its runner.

/// How many command lines the ambient block renders before it stops counting them one by one. Past
/// it the block says HOW MANY are left and names the tool that lists them all — the point of the
/// block is orientation, not an exhaustive dump into every session's first tokens.
const MAX_COMMAND_LINES: usize = 12;

/// The same kind of cap for the SHARED half — and a SEPARATE budget on purpose.
///
/// THE ARBITRATION, spelled out because it is a decision and not an accident. The two halves could
/// have shared [`MAX_COMMAND_LINES`]; they do not, because one budget makes them COMPETE, and both
/// directions of that competition are wrong:
///
/// * a workspace defining twelve commands would push every machine-wide service out of the block —
///   the agent would be told about its own dev servers and never about the SHARED ones it is the
///   most likely to break without meaning to;
/// * and a machine carrying a dozen services would do the reverse, hiding the workspace the session
///   was actually started in behind other projects' infrastructure.
///
/// So each half is bounded on its own and neither can starve the other. EIGHT rather than twelve
/// because the two are not symmetric: the workspace list is the agent's WORKING SET, the shared one
/// is context it must not ignore, and cross-project services are few by nature.
const MAX_SYSTEM_COMMAND_LINES: usize = 8;

/// The widest a rendered command line is, ellipsis INCLUDED. A managed command can be a whole shell
/// pipeline; the agent needs to RECOGNIZE it (and it has `list_commands` for the exact text), not to
/// re-type it from the block.
const MAX_COMMAND_CHARS: usize = 160;

/// The widest a rendered NOTE is, ellipsis INCLUDED — the same rule as [`MAX_COMMAND_CHARS`], one
/// notch wider because the two are not the same object. A command line is RECOGNISED (the agent has
/// `list_commands` for the exact text); a note is READ, and it is the sentence that decides whether
/// the agent starts the command at all, so cutting it at 160 would routinely cut it mid-reason.
///
/// Bounded all the same, and the bound is the block's whole economy: the column is unconstrained, a
/// human can paste a paragraph into it, and twelve pasted paragraphs would blow the `SessionStart`
/// hook's 1000 ms budget — where a timeout drops the WHOLE injected context, orientation line
/// included. Past the cut the agent still has `list_commands`, which returns the note untruncated.
const MAX_NOTE_CHARS: usize = 200;

/// The indent a note is rendered under, and the ONE thing that says a note BELONGS to the command
/// above it rather than being a thirteenth command. Two spaces + a middle dot: the same separator
/// the command line itself uses between its fields, so the block keeps one punctuation vocabulary.
const NOTE_PREFIX: &str = "\n  · ";

/// The header of the WORKSPACE half — the commands of the workspace this session was started in.
const WORKSPACE_BLOCK_HEADER: &str = "Managed commands of this workspace:";

/// The header of the SHARED half. It says the two things that make these lines different from the
/// ones above — they are NOT this workspace's, and they are machine-wide — before a single command
/// is read, so a reader that keeps only the header still knows what it is looking at.
const SYSTEM_BLOCK_HEADER: &str = "Shared System commands — machine-wide, NOT this workspace's:";

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
/// AND SO IS THE SHAPE OF THE SENTENCE. Naming `/clear` and `/compact` is NOT enough. The first
/// wording opened on "re-taken every time a session opens" and carried them in an em-dash aside; a
/// real session, asked what it had in context, read the whole thing back as "a snapshot taken at
/// session open". An aside is the first thing a model drops when it compresses, and the tests —
/// which assert the substrings are PRESENT — could not catch it: the letter held, the intent did not.
///
/// So the RULE for anyone rewriting this line: the carrying fact never sits in an aside, `/clear`
/// and `/compact` hold the main clause, and nothing frames the snapshot as taken "at session open".
const SNAPSHOT_DIRECTIVE_LINE: &str =
    "This list is a SNAPSHOT. A /clear or a /compact re-takes it; nothing in between does, so it \
     goes stale as commands start and stop. For the current state call list_commands, for what a \
     command printed call get_command_output.";

/// THE SHARED DIRECTIVE — the reason the System half is in this block at all, and the one fact an
/// agent that keeps a single sentence of it must keep.
///
/// The lines above it are a LIST; a list of services reads exactly like a list of dev servers, and
/// an agent that treats a machine-wide service as its own will stop it to "free the port" the way it
/// stops its own watcher. So the sentence leads on the SHARING and states its consequence in the
/// main clause — same rule as [`SNAPSHOT_DIRECTIVE_LINE`], for the same reason.
const SYSTEM_SHARED_DIRECTIVE_LINE: &str =
    "Those System commands are SHARED: machine-wide services, the same ones for every project and \
     every agent on this machine. Stopping one stops it for ALL of them at once — it is not your \
     own dev server.";

/// THE AUTHORIZATION DIRECTIVE — what the backend already does with each verb, said rather than
/// discovered.
///
/// It is not a new right and it grants none: `crate::system_command_guard` classifies these calls
/// the same way whether the agent read this line or not — a start on a stopped instance is FREE, a
/// stop / relaunch / edit / removal is PROTECTED and answered by a human approval the call WAITS
/// for. Saying so is what stops an agent from reading a blocked call as a hung tool, and from
/// treating the free half as licence over the rest.
const SYSTEM_AUTHORIZATION_DIRECTIVE_LINE: &str =
    "Starting a stopped System command is free, exactly like a project one. Stopping, relaunching, \
     editing or deleting one needs the USER's approval: that call BLOCKS until they answer in the \
     nyx app, and nyx performs it itself. list_system_commands has the full inventory and the ids \
     to address the scope.";

/// The command text as the block shows it: newlines flattened (one command, one line) and the whole
/// thing capped at [`MAX_COMMAND_CHARS`] — the ellipsis counts INSIDE the cap, so a rendered command
/// is never longer than the bound. Counts CHARS, not bytes, so a truncation never splits a
/// multi-byte character.
fn render_command_text(command: &str) -> String {
    render_flat(command, MAX_COMMAND_CHARS)
}

/// The NOTE as the block shows it, or `None` when there is nothing to show. Same flattening and
/// same char-counted cap as the command text ([`MAX_NOTE_CHARS`] rather than
/// [`MAX_COMMAND_CHARS`] — see the constant for why the two differ).
///
/// A note that is absent AND a note that is blank both answer `None`: `set_template_description`
/// already stores a blank one as NULL, and this second guard is what keeps a row that predates it —
/// or one written straight into SQLite — from rendering an empty bullet under its command.
fn render_note_text(description: Option<&str>) -> Option<String> {
    let text = description?.trim();
    if text.is_empty() {
        return None;
    }
    Some(render_flat(text, MAX_NOTE_CHARS))
}

/// Flatten `text` onto ONE line and cap it at `max` CHARS, ellipsis INCLUDED (so a rendered value is
/// never longer than the bound). Counts chars, not bytes, so a truncation never splits a multi-byte
/// character. Shared by the command text and the note so the two can never drift into two
/// truncation rules.
fn render_flat(text: &str, max: usize) -> String {
    let flattened = text.replace(['\r', '\n'], " ");
    if flattened.chars().count() <= max {
        return flattened;
    }
    let mut out: String = flattened.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// ONE HALF of the block: `rows` rendered as command lines, capped at `cap`, appended to `out`.
/// Answers whether at least one rendered line carried a `group_id` (the group directive's
/// condition), for the caller to fold over both halves.
///
/// The `nature` marker is written on the line ONLY for the shared half. It is the SAME value
/// [`db::PROJECT_KIND_SYSTEM`] that `list_commands` / `list_system_commands` put on the wire — the
/// vocabulary an agent already meets there, not a second label invented for the text — and it is
/// per-line on purpose: a section header can be summarized away, a marker sitting on the line an
/// agent is about to act on cannot.
///
/// THE NOTE GETS ITS OWN LINE, indented under the command's ([`NOTE_PREFIX`]), and only when the
/// command carries one. Not appended to the command line: that line is already `name · command ·
/// state · instance_id` and, on the shared half, a `nature` and a `group_id` — a fifth free-prose
/// field on the end would push the `instance_id` an agent must copy past where it can be found at a
/// glance. A command with no note renders exactly what it rendered before this: ONE line.
fn push_command_lines<F>(
    out: &mut String,
    rows: &[db::WorkspaceCommandLine],
    overlay: &F,
    cap: usize,
    system: bool,
) -> bool
where
    F: Fn(&str) -> Option<RunState>,
{
    let mut rendered_a_group = false;
    for row in rows.iter().take(cap) {
        let state = overlay(&row.id).unwrap_or_else(|| RunState::from_db_str(&row.last_state));
        out.push_str(&format!(
            "\n- {} · {} · {} · instance_id={}",
            row.name,
            render_command_text(&row.command),
            state.as_db_str(),
            row.id,
        ));
        if system {
            out.push_str(&format!(" · nature={}", db::PROJECT_KIND_SYSTEM));
        }
        if let Some(group_id) = row.group_id.as_deref().filter(|g| !g.is_empty()) {
            out.push_str(&format!(" · group_id={group_id}"));
            rendered_a_group = true;
        }
        // The note LAST, under the whole line, so every machine-readable field of the command stays
        // where it was and the prose never sits between two of them.
        if let Some(note) = render_note_text(row.description.as_deref()) {
            out.push_str(NOTE_PREFIX);
            out.push_str(&note);
        }
    }
    rendered_a_group
}

/// Render the MANAGED COMMANDS this session can act on as the block the ambient context carries
/// after the `You're in nyx — …` orientation line, so an agent knows what already exists and WHAT IS
/// ALREADY UP without a single discovery call.
///
/// TWO HALVES, ONE BLOCK. `workspace` is the session's own workspace; `system` is the Système
/// scope's — the user's machine-wide services, which every agent on this machine shares. They arrive
/// together because they are available together: a session that is told about its dev server and not
/// about the shared database it is about to restart has been told half of its situation. They stay
/// TOLD APART because acting on them does not cost the same.
///
/// One line per instance — `name · command · state · instance_id=…`, plus `· nature=system` on a
/// shared one and `· group_id=…` on a command that belongs to a multi-pane group. `command_id` is
/// deliberately absent: no tool takes it, and it would cost an ULID per line in every session's
/// first tokens.
///
/// A command carrying a NOTE FOR THE AGENT gets a SECOND, indented line under its own — the human's
/// sentence about WHEN and WHY to run it, the one thing the four fields above cannot say. Only when
/// there is one, and capped at [`MAX_NOTE_CHARS`]; a command with no note renders exactly the single
/// line it rendered before the note existed.
///
/// Each half is rendered in the order it arrives — [`db::list_command_lines_for_workspace`] already
/// orders by the template's `order_index` then `id`, which is the order of the sidebar, so
/// re-sorting here would make the block disagree with what the user sees.
///
/// THE STATE IS FACTUAL, and `overlay` is why: it is the runner's live outcome for an instance id
/// (`None` when no live entry backs it), so a command running THIS session reads `running` even when
/// its persisted `last_state` is stale — the same fallback `list_commands` applies. Taking it as a
/// CLOSURE rather than a `&CommandRunner<S>` is what makes this renderer testable without spawning a
/// single process.
///
/// TWO EMPTY HALVES render an EMPTY string — a loose terminal on a machine with no Système command
/// gets no block at all (and, therefore, no directive either).
///
/// No environment value can leak here: [`db::WorkspaceCommandLine`] carries none (`env` is a column
/// of `managed_commands`, which its reader does not select), and the block renders nothing it is not
/// handed.
///
/// THE ROW IS THE LEAN ONE, ON PURPOSE. This takes [`db::WorkspaceCommandLine`], not
/// [`db::InstanceWithTemplate`]: the latter carries each instance's `scrollback` (up to 256 KiB
/// apiece) which this renderer never reads, and it renders on the SessionStart hook's hard 1000 ms
/// budget — where a timeout costs the whole injected context.
///
/// ## THE ASSUMED DEVIATION — the shared half short-circuits the dedicated discovery door
///
/// The Système container is deliberately ABSENT from `list_projects` and `list_workspaces`, and its
/// commands are reached through ONE door, `mcp_tools_core::list_system_commands`. Listing them here
/// means an agent meets them WITHOUT walking through that door. That is a deviation from the
/// principle the PRD posed, it is the user's call, and it is written here rather than left to be
/// discovered as a side effect.
///
/// What it costs: nothing in RIGHTS. Reading the inventory and starting a stopped instance are
/// already FREE in `crate::system_command_guard`'s matrix, whichever way the ids were learned, and
/// every other verb stays behind the same human approval.
///
/// What it deliberately does NOT carry, so the door keeps a job: the scope's `project_id` and
/// `workspace_id`.
pub(crate) fn session_commands_block<F>(
    workspace: &[db::WorkspaceCommandLine],
    system: &[db::WorkspaceCommandLine],
    overlay: F,
) -> String
where
    F: Fn(&str) -> Option<RunState>,
{
    if workspace.is_empty() && system.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut rendered_a_group = false;
    if !workspace.is_empty() {
        out.push_str(WORKSPACE_BLOCK_HEADER);
        rendered_a_group |=
            push_command_lines(&mut out, workspace, &overlay, MAX_COMMAND_LINES, false);
        let overflow = workspace.len().saturating_sub(MAX_COMMAND_LINES);
        if overflow > 0 {
            out.push_str(&format!("\n+{overflow} more commands (list_commands)"));
        }
    }
    if !system.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(SYSTEM_BLOCK_HEADER);
        rendered_a_group |=
            push_command_lines(&mut out, system, &overlay, MAX_SYSTEM_COMMAND_LINES, true);
        // Its OWN surplus, counted against its OWN cap and pointing at its OWN door — the two halves
        // never borrow lines from each other, so neither can hide the other's overflow either.
        let overflow = system.len().saturating_sub(MAX_SYSTEM_COMMAND_LINES);
        if overflow > 0 {
            out.push_str(&format!(
                "\n+{overflow} more System commands (list_system_commands)"
            ));
        }
    }
    for line in DIRECTIVE_LINES {
        out.push('\n');
        out.push_str(line);
    }
    if rendered_a_group {
        out.push('\n');
        out.push_str(GROUP_DIRECTIVE_LINE);
    }
    if !system.is_empty() {
        out.push('\n');
        out.push_str(SYSTEM_SHARED_DIRECTIVE_LINE);
        out.push('\n');
        out.push_str(SYSTEM_AUTHORIZATION_DIRECTIVE_LINE);
    }
    out.push('\n');
    out.push_str(SNAPSHOT_DIRECTIVE_LINE);
    out
}

/// THE STALE FORMULATIONS — wordings the variance model stopped accepting, forbidden on EVERY
/// agent-facing surface, with the reason each one is dead.
///
/// WHY IT LIVES HERE, SHARED. An agent meets this model through TWO surfaces: the tool DESCRIPTORS
/// it gets from `tools/list`, and the MANUAL `agent_context` renders. The first got a guard; the
/// second did not, and it still summarized `states_json` as "its declared states/variants" — one of
/// the strings the descriptor guard forbids, in the very text an agent reads FIRST. Two guards over
/// two surfaces with two copies of the list would have drifted apart the same way; ONE list, read by
/// both, means a wording chased off one surface can no longer survive on the other.
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

    /// The CANONICAL doc, compiled into the test binary so the resources can be replayed against it.
    const AGENT_MANUALS_DOC: &str = include_str!("../../../docs/product/agent-manuals.md");

    fn db_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A memory DB seeded with a project + root workspace + a repo + an anchored Spec with two
    /// sections and two phase-labelled tasks, the second BLOCKED by the first. Returns
    /// `(db, spec_id, workspace_id, cwd)`.
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
            let first =
                db::create_task(c, &project.id, "Extract the lexer", phase_task("Foundation"))
                    .expect("t1");
            let second =
                db::create_task(c, &project.id, "Wire the parser", phase_task("UI")).expect("t2");
            db::add_task_dependency(c, &second.id, &first.id, db::DEPENDENCY_TYPE_BLOCKS)
                .expect("edge");
            (spec.id, root.id, root.root_path)
        });
        (db, spec_id, workspace_id, cwd)
    }

    /// A memory DB seeded with a project + its root workspace + a repo but NO Spec — the /nspec
    /// ENTRY state. Returns `(db, project_id, workspace_id, cwd)`.
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

    /// The project a seeded Spec belongs to — its `key` is minted by `create_project`, so a test
    /// asserting on the human key reads it rather than guessing at it.
    fn project_of(db: &Db, spec_id: &str) -> db::Project {
        db.with_conn(|c| {
            let spec = db::get_spec(c, spec_id).unwrap().expect("spec");
            db::get_project(c, &spec.project_id).unwrap().expect("row")
        })
    }

    fn command_row(id: &str, name: &str, command: &str, state: &str) -> db::WorkspaceCommandLine {
        db::WorkspaceCommandLine {
            id: id.to_string(),
            name: name.to_string(),
            command: command.to_string(),
            last_state: state.to_string(),
            group_id: None,
            order_index: 0,
            description: None,
        }
    }

    /// The FIXED segments of a template — the text between its `{{…}}` markers. What a render must
    /// reproduce byte for byte.
    fn fixed_segments(template: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = template;
        while let Some(open) = rest.find("{{") {
            let (before, after_open) = rest.split_at(open);
            out.push(before);
            let close = after_open
                .find("}}")
                .unwrap_or_else(|| panic!("unterminated marker in the template: {after_open}"));
            rest = &after_open[close + 2..];
        }
        out.push(rest);
        out.into_iter().filter(|s| !s.is_empty()).collect()
    }

    /// THE WORD-FOR-WORD ASSERTION. Every fixed segment of `template` occurs in `rendered`, IN
    /// ORDER, and no marker survives — so the manual was reproduced exactly and only the `{{…}}`
    /// blocks were substituted. A paraphrase, a reordering or an "improved" sentence fails here.
    fn assert_reproduced_word_for_word(label: &str, template: &str, rendered: &str) {
        // Same CRLF normalisation as `substitute`: the resource may be checked out with CRLF.
        let template = template.replace("\r\n", "\n");
        let mut cursor = 0usize;
        for segment in fixed_segments(&template) {
            let found = rendered[cursor..].find(segment).unwrap_or_else(|| {
                panic!(
                    "{label}: the manual's fixed text is not reproduced verbatim. Missing (or out \
                     of order) from offset {cursor}:\n---\n{segment}\n---\nrendered:\n{rendered}"
                )
            });
            cursor += found + segment.len();
        }
        assert!(
            !rendered.contains("{{"),
            "{label}: an unsubstituted marker survived the render:\n{rendered}"
        );
    }

    /// The content of every top-level fenced block of the canonical doc, in order.
    fn fenced_blocks(doc: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut current: Option<Vec<&str>> = None;
        for line in doc.lines() {
            if line.starts_with("```") {
                match current.take() {
                    Some(body) => {
                        let mut text = body.join("\n");
                        text.push('\n');
                        blocks.push(text);
                    }
                    None => current = Some(Vec::new()),
                }
                continue;
            }
            if let Some(body) = current.as_mut() {
                body.push(line);
            }
        }
        assert!(current.is_none(), "an unterminated fence in the doc");
        blocks
    }

    /// THE SINGLE SOURCE. The four resources this module compiles in ARE the four fenced blocks of
    /// `docs/product/agent-manuals.md` — so "word for word" is not a promise a reviewer has to
    /// re-check by eye: a sentence edited on either side turns this red.
    #[test]
    fn the_manual_resources_are_the_fenced_blocks_of_the_canonical_doc() {
        let blocks = fenced_blocks(AGENT_MANUALS_DOC);
        assert_eq!(
            blocks.len(),
            4,
            "the doc carries exactly the /nspec, /ndev, /ndoc manuals and the charter"
        );
        for (label, resource, block) in [
            ("/nspec", NSPEC_MANUAL, &blocks[0]),
            ("/ndev", NDEV_MANUAL, &blocks[1]),
            ("/ndoc", NDOC_MANUAL, &blocks[2]),
            ("charter", SUBAGENT_CHARTER, &blocks[3]),
        ] {
            assert_eq!(
                &resource.replace("\r\n", "\n"),
                block,
                "{label}: the compiled resource and the doc's fenced block must be byte-identical"
            );
        }
    }

    /// The three roles, and only the three. `coder`, `auditor` and `prototype` are not nyx roles any
    /// more — an orchestrator briefs those sub-agents itself, and the charter frames them.
    #[test]
    fn only_three_roles_exist_and_the_withdrawn_ones_are_refused() {
        assert_eq!(
            AgentRole::ALL.map(AgentRole::slug),
            ["spec", "dev", "doc"],
            "exactly the three commands"
        );
        for gone in ["coder", "auditor", "prototype", "prototyper", "reviewer"] {
            assert!(
                AgentRole::parse(gone).is_none(),
                "'{gone}' must not parse as a role"
            );
        }
    }

    #[test]
    fn an_unknown_role_is_refused_by_name() {
        let _g = db_guard();
        let (db, _spec, _ws, _cwd) = seeded();
        let err = agent_context(&db, &json!({ "role": "coder" })).expect_err("refused");
        assert!(matches!(err.code, RpcCode::InvalidArgument));
        assert!(
            err.message.contains("spec, dev, doc"),
            "the refusal names the accepted roles: {}",
            err.message
        );
    }

    /// EACH MANUAL IS REPRODUCED WORD FOR WORD, with only its `{{…}}` blocks substituted.
    #[test]
    fn each_manual_reproduces_its_fixed_text_exactly() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        for role in AgentRole::ALL {
            let rendered = render_agent_context(&db, role, &spec_id)
                .expect("render")
                .expect("a Spec");
            assert_reproduced_word_for_word(role.slug(), role.manual(), &rendered);
        }
    }

    /// …and so is the /nspec manual on its OTHER path, the one with no Spec at all.
    #[test]
    fn the_authoring_manual_reproduces_the_nspec_text_exactly() {
        let _g = db_guard();
        let (db, project_id, _ws, cwd) = seeded_project_without_spec();
        let out = agent_context(&db, &json!({ "role": "spec", "cwd": cwd })).expect("rendered");
        let text = out.result["context"].as_str().expect("a manual");
        assert_reproduced_word_for_word("/nspec (authoring)", NSPEC_MANUAL, text);
        assert_eq!(out.result["spec_id"], Value::Null);
        assert_eq!(out.result["project_id"], json!(project_id));
        assert_eq!(
            out.result["warnings"][0]["code"],
            NO_SPEC_ON_LOCATION_WARNING_CODE,
            "the case-0 signal names the legitimate case, not an invocation mistake"
        );
        assert!(
            text.contains("NO SPEC ANCHORED"),
            "the reason is IN the manual too, not only in the warnings array:\n{text}"
        );
    }

    /// …and so is the charter.
    #[test]
    fn the_subagent_charter_reproduces_its_fixed_text_exactly() {
        let _g = db_guard();
        let (db, spec_id, workspace_id, _cwd) = seeded();
        let terminal_id = db.with_conn(|c| {
            let t = db::create_terminal(c, "/tmp/nyx-ctx", None).expect("terminal");
            db::attach_terminal(c, &t.id, &workspace_id, db::BINDING_MANUAL).expect("attach");
            t.id
        });
        let text = render_subagent_charter(&db, &terminal_id).expect("charter");
        assert_reproduced_word_for_word("charter", SUBAGENT_CHARTER, &text);
        assert!(
            text.contains("Managed commands of this workspace:"),
            "the charter's own header survives:\n{text}"
        );
        assert!(
            text.contains(&spec_id),
            "the anchored Spec's id reaches spec_get():\n{text}"
        );
    }

    /// A terminal that anchors NEITHER a workspace NOR a Spec still gets a charter — with the
    /// unknown parts NAMED. A sub-agent that reads "no Spec anchored" can ask; one that reads a raw
    /// `{{spec_id}}` can do nothing at all.
    #[test]
    fn the_charter_degrades_by_naming_what_is_unknown() {
        let _g = db_guard();
        let (db, _spec_id, _ws, _cwd) = seeded();
        let loose = db.with_conn(|c| {
            db::create_terminal(c, "/tmp/elsewhere", None)
                .expect("terminal")
                .id
        });
        let text = render_subagent_charter(&db, &loose).expect("charter");
        assert_reproduced_word_for_word("charter (loose terminal)", SUBAGENT_CHARTER, &text);
        assert!(
            text.contains("attached to no nyx workspace"),
            "it says the workspace is unknown:\n{text}"
        );
        assert!(
            text.contains("ask your orchestrator for the spec_id"),
            "it says what to do about the missing Spec:\n{text}"
        );
        // An unknown terminal id is the same graceful degradation, never an error.
        let unknown = render_subagent_charter(&db, "no-such-terminal").expect("charter");
        assert!(unknown.contains("attached to no nyx workspace"));
    }

    /// THE ANCHOR IS `spec_id` / `workspace_id` / `cwd`, and NEVER a run. A `run_id` in the payload
    /// anchors nothing — the engine is gone, and a manual rendered off a run would be rendered off a
    /// row that no longer exists.
    #[test]
    fn the_three_anchors_resolve_and_a_run_id_anchors_nothing() {
        let _g = db_guard();
        let (db, spec_id, workspace_id, cwd) = seeded();
        for (label, args) in [
            ("spec_id", json!({ "role": "dev", "spec_id": spec_id })),
            (
                "workspace_id",
                json!({ "role": "dev", "workspace_id": workspace_id }),
            ),
            ("cwd", json!({ "role": "dev", "cwd": cwd })),
        ] {
            let out = agent_context(&db, &args).unwrap_or_else(|e| panic!("{label}: {e:?}"));
            assert_eq!(out.result["spec_id"], json!(spec_id), "{label} resolves it");
        }
        // A `run_id` ALONE is no anchor: /ndev then has no Spec and is refused.
        let err = agent_context(&db, &json!({ "role": "dev", "run_id": "whatever" }))
            .expect_err("a run_id anchors nothing");
        assert!(matches!(err.code, RpcCode::InvalidId));
    }

    /// THE SHORT-REFERENCE GRAMMAR is nyx's, and `agent_context` speaks all four forms — the hook
    /// forwards the token exactly as the human typed it, and nyx translates.
    #[test]
    fn every_reference_form_the_human_may_type_resolves() {
        let _g = db_guard();
        let (db, spec_id, _ws, cwd) = seeded();
        let project = project_of(&db, &spec_id);
        let key = format!("{}-1", project.key);
        for token in [spec_id.as_str(), key.as_str(), "#1", "1"] {
            let out = agent_context(&db, &json!({ "role": "dev", "spec_id": token, "cwd": cwd }))
                .unwrap_or_else(|e| panic!("the form '{token}' must resolve: {e:?}"));
            assert_eq!(out.result["spec_id"], json!(spec_id), "form '{token}'");
        }
    }

    /// THE REFUSAL THAT MOVED HERE FROM `run_start` (PDR-0031). A reference designating no Spec is
    /// refused BY `agent_context`, and the refusal carries what the session hook relays to the
    /// model: the token as typed, the accepted forms, and what the project actually holds.
    #[test]
    fn an_unknown_reference_is_refused_naming_the_forms_and_the_holdings() {
        let _g = db_guard();
        let (db, _spec_id, _ws, cwd) = seeded();
        for role in ["spec", "dev", "doc"] {
            let err = agent_context(&db, &json!({ "role": role, "spec_id": "42", "cwd": cwd }))
                .expect_err("refused");
            assert!(matches!(err.code, RpcCode::InvalidId), "{role}");
            assert!(err.message.contains("'42'"), "{role}: {}", err.message);
            assert!(
                err.message.contains("KEY-3") && err.message.contains("canonical id"),
                "{role}: the refusal names the accepted forms: {}",
                err.message
            );
            assert!(
                err.message.contains("My Feature"),
                "{role}: the refusal says what the project holds: {}",
                err.message
            );
        }
    }

    /// The N case of the 0/1/N contract: a location anchoring several Specs is REFUSED, with the
    /// machine reason and the candidates — never resolved on the most recent.
    #[test]
    fn a_location_anchoring_several_specs_is_refused_with_its_candidates() {
        let _g = db_guard();
        let (db, _spec_id, workspace_id, _cwd) = seeded();
        db.with_conn(|c| {
            let ws = db::get_workspace(c, &workspace_id).unwrap().unwrap();
            db::create_spec_with_workspace(
                c,
                &ws.project_id,
                &ws.id,
                "A Second Feature",
                db::SpecSections::default(),
            )
            .expect("second spec");
        });
        let err = agent_context(&db, &json!({ "role": "dev", "workspace_id": workspace_id }))
            .expect_err("refused");
        assert!(matches!(err.code, RpcCode::InvalidArgument));
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some(AMBIGUOUS_SPEC_REASON),
            "the machine reason a caller keys on"
        );
        assert!(
            err.message.contains("A Second Feature") && err.message.contains("My Feature"),
            "the candidates are listed: {}",
            err.message
        );
    }

    /// THE 0 CASE SPLITS BY ROLE, and that is the whole point of the three stubs: `/nspec` authors a
    /// new Spec (so zero anchored is its NOMINAL state), `/ndev` and `/ndoc` work ON a Spec and are
    /// refused rather than started on a guess.
    #[test]
    fn zero_anchored_specs_authors_for_nspec_and_refuses_for_ndev_and_ndoc() {
        let _g = db_guard();
        let (db, _project_id, _ws, cwd) = seeded_project_without_spec();
        let spec = agent_context(&db, &json!({ "role": "spec", "cwd": cwd }))
            .expect("/nspec is never refused for having no Spec");
        assert_eq!(spec.result["spec_id"], Value::Null);
        for role in ["dev", "doc"] {
            let err = agent_context(&db, &json!({ "role": role, "cwd": cwd })).expect_err("refused");
            assert!(matches!(err.code, RpcCode::InvalidId), "{role}");
            assert!(
                err.message.contains(&format!("/{role} works ON a Spec")),
                "{role}: {}",
                err.message
            );
            assert!(
                err.message.contains("holds no Spec at all"),
                "{role}: the refusal says what the project holds: {}",
                err.message
            );
            assert_eq!(
                err.data
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str),
                Some(NO_SPEC_ON_LOCATION_WARNING_CODE),
                "{role}: the machine reason travels with the refusal"
            );
        }
    }

    /// A call that resolves NO workspace at all is the OTHER case 0 — an invocation mistake, and it
    /// must not share a code with the legitimate one.
    #[test]
    fn a_cwd_matching_no_workspace_is_diagnosed_as_an_invocation_mistake() {
        let _g = db_guard();
        let (db, _project_id, _ws, _cwd) = seeded_project_without_spec();
        let out = agent_context(&db, &json!({ "role": "spec", "cwd": "/nowhere/at/all" }))
            .expect("still renders the authoring manual");
        assert_eq!(
            out.result["warnings"][0]["code"],
            NO_LOCATION_WARNING_CODE,
            "a cwd nyx tracks nothing for is not the same as a workspace with no Spec"
        );
    }

    /// `{{tasks}}` — the plan's order, the statuses, the BLOCKING dependencies and the phase as a
    /// LABEL. This is what an orchestrator builds its frontier from.
    #[test]
    fn the_tasks_block_carries_status_blockers_and_the_phase_as_a_label() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Dev, &spec_id)
            .unwrap()
            .unwrap();
        let block = text
            .split("## Tasks — the plan, in order")
            .nth(1)
            .expect("the tasks block");
        let first = block.find("Extract the lexer").expect("the first task");
        let second = block.find("Wire the parser").expect("the second task");
        assert!(first < second, "the plan's order is preserved:\n{block}");
        assert!(
            block.contains("- [todo] Extract the lexer"),
            "the status is on the line:\n{block}"
        );
        assert!(
            block.contains("phase=\"Foundation\"") && block.contains("phase=\"UI\""),
            "the phase is a LABEL on the line:\n{block}"
        );
        assert!(
            block.contains("blocked by: nothing — this task can start now"),
            "an unblocked task says so:\n{block}"
        );
        assert!(
            block.contains("blocked by: [todo] Extract the lexer"),
            "a blocked task names its blocker, with its status:\n{block}"
        );
    }

    /// `{{commands}}` — the workspace's managed commands, each with its NOTE FOR THE AGENT under
    /// it when the human wrote one, and nothing at all under it otherwise: the note is what says
    /// WHEN to run a command, and a command without one must read exactly as before notes existed.
    #[test]
    fn the_commands_block_renders_the_lines_and_the_notes_under_them() {
        let noted = db::WorkspaceCommandLine {
            description: Some("  the gate before any commit — run it, never skip it  ".to_string()),
            ..command_row("inst-2", "test", "bun run test", "idle")
        };
        let rows = [command_row("inst-1", "dev", "bun run dev", "running"), noted];
        let block = commands_block(&rows);
        assert!(block.contains("- dev · bun run dev · running · instance_id=inst-1\n- test"));
        assert!(
            block.contains(
                "- test · bun run test · idle · instance_id=inst-2\n  · the gate before any commit — run it, never skip it"
            ),
            "{block}"
        );
        // The charter's half is the same lines, without the header (the charter carries its own).
        let charter_half = commands_with_notes_block(&rows);
        assert!(charter_half.starts_with("- dev · "), "{charter_half}");
        assert!(!charter_half.contains("## Managed commands"));
        // And an empty workspace says so rather than rendering a bare header.
        assert!(commands_block(&[]).contains("None registered on this workspace"));
        assert!(commands_with_notes_block(&[]).contains("(none registered on this workspace)"));
    }

    /// The category discipline is keyed on the (role, PERSISTED slug) pair, and `doc` never carries
    /// one — it syncs documentation after the fact and decides nothing about a proof.
    #[test]
    fn the_bug_discipline_reaches_the_spec_author_and_the_orchestrator_only() {
        assert!(category_discipline(AgentRole::Spec, db::CATEGORY_SLUG_BUG).is_some());
        assert!(category_discipline(AgentRole::Dev, db::CATEGORY_SLUG_BUG).is_some());
        assert!(category_discipline(AgentRole::Doc, db::CATEGORY_SLUG_BUG).is_none());
        for role in AgentRole::ALL {
            assert!(
                category_discipline(role, "feature").is_none(),
                "{}: a category with no discipline owes none",
                role.slug()
            );
        }
        assert!(subagent_category_discipline(db::CATEGORY_SLUG_BUG).is_some());
        assert!(subagent_category_discipline("feature").is_none());
    }

    /// A Spec with NO category renders no discipline block — and the marker still disappears.
    #[test]
    fn an_unclassified_spec_renders_no_discipline_block() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        for role in AgentRole::ALL {
            let text = render_agent_context(&db, role, &spec_id).unwrap().unwrap();
            assert!(
                !text.contains(CATEGORY_DISCIPLINE_HEADING),
                "{}: no category, no discipline:\n{text}",
                role.slug()
            );
            assert!(!text.contains("{{"), "{}: no marker survives", role.slug());
        }
    }

    /// The Spec block carries the identity an agent needs to CALL a tool (`spec_id`), the key the
    /// human reads (`KEY-N`) and the sections of the ACTIVE revision.
    #[test]
    fn the_spec_block_carries_the_key_the_id_and_the_sections() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let project = project_of(&db, &spec_id);
        let text = render_agent_context(&db, AgentRole::Doc, &spec_id)
            .unwrap()
            .unwrap();
        assert!(text.contains(&format!("spec_id  : {spec_id}")), "{text}");
        assert!(
            text.contains(&format!("{}-1", project.key)),
            "the human key: {text}"
        );
        assert!(
            text.contains("the problem") && text.contains("the solution"),
            "the sections of the active revision"
        );
    }

    /// The workspace block resolves the anchored workspace, its path and its repos — and says what
    /// is missing when nothing is anchored.
    #[test]
    fn the_workspace_block_resolves_or_names_what_is_missing() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let text = render_agent_context(&db, AgentRole::Dev, &spec_id)
            .unwrap()
            .unwrap();
        assert!(text.contains("- path: "), "the workspace path:\n{text}");
        assert!(text.contains("- repo: api → ."), "the project's repo");
        assert!(workspace_block(None).contains("create_workspace(spec_id=…)"));
    }

    /// An UNKNOWN Spec id renders nothing rather than half a manual.
    #[test]
    fn an_unknown_spec_renders_nothing() {
        let _g = db_guard();
        let (db, _spec_id, _ws, _cwd) = seeded();
        assert!(render_agent_context(&db, AgentRole::Dev, "no-such-spec")
            .unwrap()
            .is_none());
    }

    /// A `project_id` that DISAGREES with the resolved Spec's own project is named, never silently
    /// dropped: the caller believed it had scoped the render.
    #[test]
    fn a_project_id_disagreeing_with_the_spec_is_refused() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        let err = agent_context(
            &db,
            &json!({ "role": "dev", "spec_id": spec_id, "project_id": "another-project" }),
        )
        .expect_err("refused");
        assert!(matches!(err.code, RpcCode::InvalidArgument));
        assert!(err.message.contains("belongs to project"), "{}", err.message);
    }

    /// A PROJECT-RELATIVE form on a call that anchors no project is refused BY NAME, before any read
    /// — the grammar nyx advertises is the grammar nyx accepts, in both directions.
    #[test]
    fn a_bare_number_with_no_project_anchor_is_refused_by_name() {
        let _g = db_guard();
        let (db, _spec_id, _ws, _cwd) = seeded();
        let err =
            agent_context(&db, &json!({ "role": "dev", "spec_id": "3" })).expect_err("refused");
        assert!(matches!(err.code, RpcCode::InvalidArgument));
        assert!(err.message.contains("PROJECT-RELATIVE"), "{}", err.message);
    }

    /// The dispatcher routes `agent_context` and nothing else.
    #[test]
    fn the_dispatcher_routes_only_its_own_tool() {
        let _g = db_guard();
        let (db, spec_id, _ws, _cwd) = seeded();
        assert!(dispatch_context_tool(
            &db,
            AGENT_CONTEXT_TOOL,
            &json!({ "role": "dev", "spec_id": spec_id })
        )
        .is_some());
        assert!(dispatch_context_tool(&db, "spec_get", &json!({})).is_none());
    }

    // --- the ambient session-commands block ---------------------------------

    #[test]
    fn the_session_block_renders_both_halves_with_their_directives() {
        let workspace = [command_row("inst-1", "dev", "bun run dev", "running")];
        let system = [command_row("sys-1", "postgres", "docker start pg", "idle")];
        let block = session_commands_block(&workspace, &system, |_| None);
        assert!(block.contains(WORKSPACE_BLOCK_HEADER));
        assert!(block.contains(SYSTEM_BLOCK_HEADER));
        assert!(block.contains(&format!("nature={}", db::PROJECT_KIND_SYSTEM)));
        assert!(block.contains(SYSTEM_SHARED_DIRECTIVE_LINE));
        assert!(block.contains(SYSTEM_AUTHORIZATION_DIRECTIVE_LINE));
        assert!(block.contains(SNAPSHOT_DIRECTIVE_LINE));
        // No group rendered ⇒ not a word about groups.
        assert!(!block.contains(GROUP_DIRECTIVE_LINE));
    }

    #[test]
    fn two_empty_halves_render_no_block_at_all() {
        assert!(session_commands_block(&[], &[], |_| None).is_empty());
    }

    #[test]
    fn the_live_runner_overlay_wins_over_the_persisted_state() {
        let workspace = [command_row("inst-1", "dev", "bun run dev", "idle")];
        let block = session_commands_block(&workspace, &[], |id| {
            (id == "inst-1").then_some(RunState::Running)
        });
        assert!(block.contains("· running ·"), "{block}");
    }

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
                description: None,
            }
        }

        /// The same row, carrying a NOTE FOR THE AGENT — the only field the note-rendering cases
        /// vary, so they say so and nothing else.
        fn row_with_note(
            id: &str,
            name: &str,
            command: &str,
            last_state: &str,
            note: &str,
        ) -> db::WorkspaceCommandLine {
            db::WorkspaceCommandLine {
                description: Some(note.to_string()),
                ..row(id, name, command, last_state)
            }
        }

        /// The overlay that knows nothing: every row falls back to its persisted `last_state`.
        fn no_overlay(_: &str) -> Option<RunState> {
            None
        }

        /// THE NOTE FOR THE AGENT gets its OWN indented line, under the command it belongs to — and
        /// only when there is one.
        ///
        /// The two halves are one decision. A command WITHOUT a note must render exactly what it
        /// rendered before notes existed, or every workspace on the machine would pay a blank line
        /// per command in every session's first tokens for a feature nobody used. And a note that
        /// IS there must not be appended to the command line: that line ends on the `instance_id`
        /// an agent copies, and free prose in front of it is the one way to make that id hard to
        /// find.
        #[test]
        fn a_note_renders_as_its_own_indented_line_and_only_when_present() {
            let rows = vec![
                row_with_note(
                    "inst-check",
                    "check",
                    "bun run check",
                    db::STATE_IDLE,
                    "The gate before any commit.",
                ),
                row("inst-dev", "dev", "bun run dev", db::STATE_RUNNING),
            ];
            let block = session_commands_block(&rows, &[], no_overlay);

            assert!(
                block.contains("- check · bun run check · idle · instance_id=inst-check\n  · The gate before any commit."),
                "the note sits on the NEXT line, indented, under its own command:\n{block}",
            );
            // The command with no note is followed straight by the next command's line — nothing
            // was inserted between them.
            assert!(
                block.contains("- dev · bun run dev · running · instance_id=inst-dev\n"),
                "a command without a note renders exactly one line:\n{block}",
            );
            assert_eq!(
                block.matches(NOTE_PREFIX).count(),
                1,
                "one note in, one note line out:\n{block}",
            );
        }

        /// A LONG note is cut at [`MAX_NOTE_CHARS`], ellipsis included — the block is read on the
        /// `SessionStart` hook's 1000 ms budget, and a human can paste a paragraph into a column
        /// that constrains nothing. What is cut is not lost: `list_commands` returns it whole.
        #[test]
        fn a_long_note_is_truncated_within_its_own_bound() {
            let long = "z".repeat(MAX_NOTE_CHARS * 2);
            let rows = vec![row_with_note(
                "inst-1",
                "long",
                "echo hi",
                db::STATE_IDLE,
                &long,
            )];
            let block = session_commands_block(&rows, &[], no_overlay);
            let rendered = block
                .split(NOTE_PREFIX)
                .nth(1)
                .expect("the note line is rendered")
                .lines()
                .next()
                .expect("the note occupies one line");

            assert_eq!(
                rendered.chars().count(),
                MAX_NOTE_CHARS,
                "the ellipsis counts INSIDE the cap, so the rendered note is never longer:\n{block}",
            );
            assert!(rendered.ends_with('…'), "the cut is visible: {rendered}");
        }

        /// A note is FLATTENED onto one line, and a BLANK one renders nothing at all.
        ///
        /// The multiline case is not theoretical: the field is a textarea, so a user pressing Enter
        /// twice would otherwise inject two unindented lines into the middle of the command list —
        /// where a reader (and a model) would take them for commands. The blank case guards a row
        /// written before `set_template_description` normalised blanks, or straight into SQLite: an
        /// empty bullet says a note exists when none does.
        #[test]
        fn a_note_is_flattened_onto_one_line_and_a_blank_one_renders_nothing() {
            let rows = vec![
                row_with_note(
                    "inst-1",
                    "multi",
                    "echo hi",
                    db::STATE_IDLE,
                    "First line.\nSecond line.",
                ),
                row_with_note("inst-2", "blank", "echo ho", db::STATE_IDLE, "   "),
            ];
            let block = session_commands_block(&rows, &[], no_overlay);

            assert!(
                block.contains("\n  · First line. Second line."),
                "the newline becomes a space — one note, one line:\n{block}",
            );
            assert_eq!(
                block.matches(NOTE_PREFIX).count(),
                1,
                "a blank note is not a note:\n{block}",
            );
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
            let out = session_commands_block(&rows, &[], no_overlay);
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
            let out = session_commands_block(&[], &[], no_overlay);
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
            let out = session_commands_block(&rows, &[], no_overlay);
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
            let out = session_commands_block(&rows, &[], |id: &str| {
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
            let out = session_commands_block(&rows, &[], no_overlay);
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
            let out = session_commands_block(&rows, &[], no_overlay);
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
            let out12 = session_commands_block(&rows[..12], &[], no_overlay);
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
            let out = session_commands_block(&rows, &[], no_overlay);
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
            let kept = session_commands_block(&fit, &[], no_overlay);
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

            let with_group = session_commands_block(&[grouped, solo.clone()], &[], no_overlay);
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

            let solo_only = session_commands_block(&[solo], &[], no_overlay);
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
            let out = session_commands_block(&[row("i", "dev", "c", db::STATE_IDLE)], &[], no_overlay);
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
            let out = session_commands_block(&[row("i", "dev", "c", db::STATE_IDLE)], &[], no_overlay);
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
            let out = session_commands_block(&rows, &[], no_overlay);
            assert!(
                out.contains("- dev · bun run dev · idle · instance_id="),
                "the command IS rendered: {out}"
            );
            assert!(
                !out.contains("s3cr3t-do-not-inject") && !out.contains("VAULT_TOKEN"),
                "…and its environment is nowhere near it: {out}"
            );
        }

        // --- THE SHARED HALF (review `01M1EJ84VCWRD6F4NDVD2M0QWJ`) ------------------
        //
        // The Système commands ride in the SAME block as the workspace's, because they are
        // available to the same session — and they are told apart, because acting on them does not
        // cost the same and, above all, does not affect the same people.

        /// The shared half renders under its OWN header, and EVERY one of its lines carries the
        /// `nature=system` marker — the same value `list_commands` puts on the wire, not a second
        /// label. Per line and not only in the header: a header is exactly what a model drops when
        /// it compresses, and the line is what it will act on.
        #[test]
        fn the_shared_half_renders_under_its_own_header_and_marks_every_line() {
            let ws = vec![row("i-dev", "dev", "bun run dev", db::STATE_RUNNING)];
            let sys = vec![
                row("i-svc", "services", "docker compose up", db::STATE_RUNNING),
                row("i-tick", "Commande factice", "node ticker.js", db::STATE_IDLE),
            ];
            let out = session_commands_block(&ws, &sys, no_overlay);

            assert!(
                out.starts_with("Managed commands of this workspace:\n- dev "),
                "the workspace half still opens the block, unchanged: {out}"
            );
            assert!(
                out.contains(SYSTEM_BLOCK_HEADER),
                "the shared half has its own header: {out}"
            );
            assert!(
                out.contains("\n- services · docker compose up · running · instance_id=i-svc · nature=system\n")
                    && out.contains("\n- Commande factice · node ticker.js · idle · instance_id=i-tick · nature=system\n"),
                "each shared line carries its state, its launchable id and its nature: {out}"
            );
            assert!(
                !out.contains("- dev · bun run dev · running · instance_id=i-dev · nature="),
                "…and the workspace's own lines carry no nature at all: {out}"
            );
            // The two headers, in this order, and the workspace's first: the session's own working
            // set leads, the shared context follows.
            let ws_at = out.find(WORKSPACE_BLOCK_HEADER).expect("workspace header");
            let sys_at = out.find(SYSTEM_BLOCK_HEADER).expect("system header");
            assert!(ws_at < sys_at, "the workspace half comes first: {out}");
        }

        /// THE DIRECTIVE THE WHOLE FINDING IS ABOUT. Three facts, all of them already enforced by
        /// the backend and none of them previously SAID: the services are shared machine-wide, a
        /// start is free, and every other verb blocks on a human approval.
        #[test]
        fn the_system_directive_states_the_sharing_the_free_start_and_the_blocking_approval() {
            let out = session_commands_block(
                &[row("i-dev", "dev", "c", db::STATE_IDLE)],
                &[row("i-svc", "services", "c", db::STATE_RUNNING)],
                no_overlay,
            );

            // 1. SHARED, machine-wide, and what that implies — in the main clause, not an aside.
            let shared = out
                .lines()
                .find(|l| l.contains("SHARED"))
                .unwrap_or_else(|| panic!("the sharing is stated: {out}"));
            assert!(
                shared.contains("machine-wide")
                    && shared.contains("every project")
                    && shared.contains("every agent"),
                "the reach is named — machine-wide, every project, every agent: {shared}"
            );
            assert!(
                shared.contains("ALL of them at once"),
                "…and the consequence of a stop is spelled out, not implied: {shared}"
            );
            assert!(
                shared.find("SHARED").unwrap() < shared.find("Stopping").unwrap(),
                "the fact leads and the consequence follows it: {shared}"
            );

            // 2 + 3. The two authorization halves, in one sentence: free start, blocking approval.
            let auth = out
                .lines()
                .find(|l| l.contains("free"))
                .unwrap_or_else(|| panic!("the authorization rule is stated: {out}"));
            assert!(
                auth.contains("Starting a stopped System command is free"),
                "starting is free, said plainly: {auth}"
            );
            for verb in ["Stopping", "relaunching", "editing", "deleting"] {
                assert!(
                    auth.contains(verb),
                    "the protected verb {verb} is named: {auth}"
                );
            }
            assert!(
                auth.contains("BLOCKS"),
                "…and that the call WAITS for the human, which is what stops an agent from \
                 reading it as a hung tool: {auth}"
            );
            assert!(
                auth.contains("list_system_commands"),
                "…and the door that carries the rest is named: {auth}"
            );

            // The block stays LEAN: two headers, two generic directives, two system ones, the
            // snapshot. Nothing else was smuggled in.
            let prose: Vec<&str> = out.lines().filter(|l| !l.starts_with("- ")).collect();
            assert!(
                prose.len() <= 7,
                "the block stays lean — 7 non-command lines at most, got {}: {prose:#?}",
                prose.len()
            );
        }

        /// NOT A WORD ABOUT SHARING when nothing shared is on screen — the same rule the group
        /// directive follows. A machine with no Système command must read exactly as it did before.
        #[test]
        fn no_shared_command_means_no_system_header_and_no_system_directive() {
            let out = session_commands_block(&[row("i", "dev", "c", db::STATE_IDLE)], &[], no_overlay);
            assert!(
                !out.contains(SYSTEM_BLOCK_HEADER)
                    && !out.contains(SYSTEM_SHARED_DIRECTIVE_LINE)
                    && !out.contains(SYSTEM_AUTHORIZATION_DIRECTIVE_LINE)
                    && !out.contains("nature="),
                "nothing shared on screen ⇒ nothing about sharing: {out}"
            );
        }

        /// EITHER HALF ALONE renders. A workspace with no command of its own still gets the shared
        /// services (and no empty "Managed commands of this workspace:" header above them); two
        /// empty halves still render nothing at all.
        #[test]
        fn the_shared_half_renders_on_its_own_when_the_workspace_has_no_command() {
            let sys = vec![row("i-svc", "services", "docker compose up", db::STATE_RUNNING)];
            let out = session_commands_block(&[], &sys, no_overlay);
            assert!(
                out.starts_with(SYSTEM_BLOCK_HEADER),
                "the block opens on the shared header: {out}"
            );
            assert!(
                !out.contains(WORKSPACE_BLOCK_HEADER),
                "no empty workspace header is announced: {out}"
            );
            assert!(
                out.contains("· instance_id=i-svc · nature=system")
                    && out.contains(SYSTEM_SHARED_DIRECTIVE_LINE)
                    && out.contains(SNAPSHOT_DIRECTIVE_LINE),
                "the shared half carries its lines and its full directive: {out}"
            );
            assert!(
                session_commands_block(&[], &[], no_overlay).is_empty(),
                "…and two empty halves are still no block at all"
            );
        }

        /// THE ARBITRATION ON THE CAPS, asserted rather than merely argued: the two halves have
        /// SEPARATE budgets, so a workspace at its own ceiling cannot push the shared services out
        /// of the block, and a machine full of services cannot bury the workspace. Each half counts
        /// its own surplus and names its own listing tool.
        #[test]
        fn the_two_caps_are_independent_so_neither_half_can_starve_the_other() {
            let ws: Vec<db::WorkspaceCommandLine> = (0..15)
                .map(|i| row(&format!("i-w{i}"), &format!("ws-{i}"), "c", db::STATE_IDLE))
                .collect();
            let sys: Vec<db::WorkspaceCommandLine> = (0..12)
                .map(|i| row(&format!("i-s{i}"), &format!("svc-{i}"), "c", db::STATE_IDLE))
                .collect();
            let out = session_commands_block(&ws, &sys, no_overlay);

            let system_lines = out.lines().filter(|l| l.contains("nature=system")).count();
            let all_lines = out.lines().filter(|l| l.starts_with("- ")).count();
            assert_eq!(
                system_lines, MAX_SYSTEM_COMMAND_LINES,
                "the shared half gets its OWN eight lines even though the workspace filled \
                 its twelve: {out}"
            );
            assert_eq!(
                all_lines - system_lines,
                MAX_COMMAND_LINES,
                "…and the workspace half is untouched by the shared one: {out}"
            );
            assert!(
                out.contains("\n+3 more commands (list_commands)"),
                "the workspace surplus points at list_commands: {out}"
            );
            assert!(
                out.contains("\n+4 more System commands (list_system_commands)"),
                "the shared surplus points at its own door: {out}"
            );
        }

        /// The shared half is rendered in the order it ARRIVES, exactly like the workspace one —
        /// `db::list_command_lines_for_workspace` already ordered it by `order_index` then `id`,
        /// which is the order of the Système page's own list.
        #[test]
        fn the_shared_half_keeps_the_reader_order_and_is_never_re_sorted() {
            let sys = vec![
                row("i-3", "zulu", "c", db::STATE_IDLE),
                row("i-1", "alpha", "c", db::STATE_IDLE),
                row("i-2", "mike", "c", db::STATE_IDLE),
            ];
            let out = session_commands_block(&[], &sys, no_overlay);
            let order: Vec<&str> = out
                .lines()
                .filter(|l| l.starts_with("- "))
                .map(|l| l.split(" · ").next().unwrap_or_default())
                .collect();
            assert_eq!(
                order,
                vec!["- zulu", "- alpha", "- mike"],
                "the shared half is the reader's order, not an alphabetical one: {out}"
            );
        }

        /// NO ENVIRONMENT VALUE reaches the SHARED half either — proven through the REAL reader on a
        /// REAL bootstrapped Système scope, not by re-asserting the struct. A Système service is
        /// precisely the kind of command whose template carries a token, and it goes through the
        /// very same lean reader, which never selects `env`.
        #[test]
        fn no_environment_value_reaches_the_shared_render() {
            let db = Db::in_memory();
            let rows = db
                .with_conn(|c| {
                    let scope = crate::system_scope::ensure_system_scope(
                        c,
                        &crate::system_scope::FixedHomeDirProvider::at(if cfg!(windows) {
                            "C:\\Users\\kris"
                        } else {
                            "/home/kris"
                        }),
                    )
                    .expect("bootstrap the Système scope");
                    let tpl = db::create_template(
                        c,
                        &scope.project.id,
                        "services",
                        "docker compose up",
                        None,
                        db::CommandSource::default(),
                    )?;
                    db::set_template_env(
                        c,
                        &tpl.id,
                        Some(r#"{"SYSTEM_VAULT_TOKEN":"sys-s3cr3t-do-not-inject"}"#),
                    )?;
                    db::list_command_lines_for_workspace(c, &scope.workspace.id)
                })
                .expect("seeded the Système workspace");
            assert_eq!(rows.len(), 1, "the Système template materialized one instance");
            let out = session_commands_block(&[], &rows, no_overlay);
            assert!(
                out.contains("- services · docker compose up · idle · instance_id=")
                    && out.contains("· nature=system"),
                "the shared command IS rendered: {out}"
            );
            assert!(
                !out.contains("sys-s3cr3t-do-not-inject") && !out.contains("SYSTEM_VAULT_TOKEN"),
                "…and its environment is nowhere near it: {out}"
            );
        }
    #[test]
    fn a_long_command_is_capped_ellipsis_included() {
        let long = "x".repeat(MAX_COMMAND_CHARS * 2);
        let rendered = render_command_text(&long);
        assert_eq!(rendered.chars().count(), MAX_COMMAND_CHARS);
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn each_half_counts_its_own_overflow_against_its_own_cap() {
        let workspace: Vec<db::WorkspaceCommandLine> = (0..MAX_COMMAND_LINES + 3)
            .map(|i| command_row(&format!("w{i}"), "w", "run", "idle"))
            .collect();
        let system: Vec<db::WorkspaceCommandLine> = (0..MAX_SYSTEM_COMMAND_LINES + 2)
            .map(|i| command_row(&format!("s{i}"), "s", "run", "idle"))
            .collect();
        let block = session_commands_block(&workspace, &system, |_| None);
        assert!(block.contains("+3 more commands (list_commands)"), "{block}");
        assert!(
            block.contains("+2 more System commands (list_system_commands)"),
            "{block}"
        );
    }
}
