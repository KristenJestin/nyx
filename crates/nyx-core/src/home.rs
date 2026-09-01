//! The HOME read model — what "where is this project at" reduces to, in one read.
//!
//! nyx has no synthesis view: one lands on a terminal or a Spec and never sees which agents are
//! running, which of them are BLOCKED ON A HUMAN, which services are up. The home pages (one
//! scoped to the active project, one across every project) are that view, and this module is the
//! projection they read.
//!
//! # ONE authority for the state, and it already exists
//!
//! The rule this module exists to NOT break: the state of an agent row is DERIVED from the
//! activity nyx already observes — [`crate::agent_activity::Activity`], fed by the Claude per-turn
//! hooks — crossed with the persisted `agent_sessions.state`. There is NO second state machine
//! here, no new observation, no new hook. [`session_attention`] is a total function over facts
//! three existing authorities already hold:
//!
//! ```text
//! agent_sessions.state  (persisted)  ─┐
//! ActivitySnapshot      (runtime)    ─┼─►  session_attention  ─►  Option<Attention>
//! PTY liveness          (runtime)    ─┘                            (None = out of the flow)
//! ```
//!
//! # The FIVE axes, not three
//!
//! [`Activity`] carries three kinds, but the same [`ActivitySnapshot`] carries two more bits that
//! the UI has always read as states of the row: `ready_unread` (the turn finished and nobody has
//! looked) and `error_unread` (it finished on an API error). The home reads all FIVE —
//! `waiting`, `error`, `ready`, `working`, `idle` — off that ONE snapshot. They are states of the
//! LINE, read on its dot; nothing here is a badge that doubles the dot.
//!
//! # The rank is the app's rank
//!
//! [`Attention::rank`] is `waiting > error > ready > working > idle` — the EXACT order of the
//! renderer's `STATE_PRIORITY` (`components/ui/status-dot.tsx`), which the terminal line, the
//! workspace accordion and the project rail dot already fold through. The home does not invent a
//! second order: it reuses that one, and `attention_order_is_the_app_priority` (here) plus
//! `home-state.test.ts` (renderer, which DERIVES its expectation from `STATE_PRIORITY` itself)
//! fail together the day the two diverge.

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::agent_activity::{Activity, ActivitySnapshot, AgentActivityStore};
use crate::command::RunState;
use crate::db::{self, SESSION_STATE_ACTIVE};
use crate::workspace_runtime::RunStateOverlay;

/// The state of ONE agent row on the home — the resolved dot, in the vocabulary the whole app
/// already sorts by. Five variants because the [`ActivitySnapshot`] carries five axes, not the
/// three of [`Activity`]: the two turn-end notifications are states of the row, not badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// The agent is BLOCKED on a human — an `AskUserQuestion` in flight or a permission /
    /// elicitation gate. `Waiting` IS the attention: there is no separate "needs you" flag.
    Waiting,
    /// The last turn ended on an API error (`StopFailure`) and nobody has looked yet
    /// ([`ActivitySnapshot::error_unread`]).
    Error,
    /// The turn finished and its answer has not been read yet
    /// ([`ActivitySnapshot::ready_unread`]).
    Ready,
    /// The agent is on the current turn (between a prompt and its `Stop`, or with a tool /
    /// sub-agent in flight).
    Working,
    /// Between turns, with nothing unread. The row is live but quiet.
    Idle,
}

/// The five states in RANK order — most attention-hungry first. This IS the order the rest of the
/// app folds by (`STATE_PRIORITY`), transcribed once so the sort and the tests read from the same
/// place.
pub const ATTENTION_ORDER: [Attention; 5] = [
    Attention::Waiting,
    Attention::Error,
    Attention::Ready,
    Attention::Working,
    Attention::Idle,
];

impl Attention {
    /// The wire form — the string the renderer reads on the row. Matches the renderer's
    /// `HomeSessionState` vocabulary one for one.
    pub fn as_str(self) -> &'static str {
        match self {
            Attention::Waiting => "waiting",
            Attention::Error => "error",
            Attention::Ready => "ready",
            Attention::Working => "working",
            Attention::Idle => "idle",
        }
    }

    /// Sort rank, 0 = most attention-hungry. `waiting > error > ready > working > idle`, the app's
    /// `STATE_PRIORITY` and not a second order of the home's own. Derived from
    /// [`ATTENTION_ORDER`] so there is exactly one place to reorder — and one place to get it
    /// wrong, which the tests watch.
    pub fn rank(self) -> usize {
        ATTENTION_ORDER
            .iter()
            .position(|a| *a == self)
            // Unreachable: ATTENTION_ORDER lists every variant, and the test below pins that.
            .unwrap_or(ATTENTION_ORDER.len())
    }
}

/// The state of ONE agent session on the home, or `None` when the session is OUT OF THE FLOW —
/// the single place both exclusions live, so a row can never be shown by one caller and hidden by
/// another.
///
/// The three inputs are the three authorities, unchanged:
///   * `session_state` — the persisted `agent_sessions.state`. ONLY `active` is in the flow.
///     `unknown` and `resume_failed` are not (a session nyx cannot vouch for is not a session one
///     can act on), and neither is `ended`.
///   * `pty_alive` — whether a PTY is still registered for the hosting terminal. THE DORMANT
///     FILTER: the idle-unload closes the PTY of a dormant session but deliberately leaves its row
///     `active` in the DB so a revisit can resume it. Such a session has no live process behind
///     it, so it is out of the flow — with no dedicated "off" state, because it is not a state,
///     it is an absence.
///   * `activity` — the runtime [`ActivitySnapshot`], `None` for a terminal with no live entry
///     (the boot state, and every terminal between turns with nothing unread) → [`Attention::Idle`].
///
/// # Why `ended` has NO exception
///
/// An earlier shape kept an `ended` session on the board while it still carried an
/// unacknowledged error — "the last thing the session has to say". That exception was removed,
/// for two reasons that hold together:
///
///   * It could never fire for the case it described. The one clean-end path calls
///     `activity.clear(terminal_id)` immediately BEFORE `db::mark_session_ended`
///     (`mcp_tools_core.rs`, the anti-phantom reflex), so the entry — and with it `error_unread`
///     — is gone by the time the row reads `ended`. The other path to `ended`,
///     [`crate::db::close_terminal`], flips the terminal to `closed` in the same transaction,
///     and [`crate::db::home_sessions`] only loads sessions on ALIVE terminals.
///   * If it HAD fired, it would have lied. [`AgentActivityStore`] is keyed by TERMINAL, not by
///     session: once a session ends, any later entry under that key belongs to its SUCCESSOR on
///     the same terminal. The exception would have dressed the dead row in a live session's
///     error — and shown the same terminal twice, once per row.
///
/// An unacknowledged error is therefore surfaced by the session that actually owns it, which is
/// the `active` one, on its own line.
pub fn session_attention(
    session_state: &str,
    pty_alive: bool,
    activity: Option<&ActivitySnapshot>,
) -> Option<Attention> {
    match session_state {
        SESSION_STATE_ACTIVE => {
            // DORMANT: the row says active, the process is gone. Out of the flow.
            if !pty_alive {
                return None;
            }
            Some(attention_of(activity))
        }
        // `ended` / `unknown` / `resume_failed` (and anything a later migration adds) stay out.
        _ => None,
    }
}

/// The SECONDARY INFORMATION of a row — the short sentence to the right of the label saying what
/// the agent is asking, or what it is doing. Resolved HERE, at display time, over the raw facts
/// the store holds ([`ActivitySnapshot::waiting_on`], [`ActivitySnapshot::tool`]), in ONE order:
///
///   1. **blocked on a human** → the question or the permission it is blocked on;
///   2. otherwise → the **tool in flight**;
///   3. otherwise → **nothing**.
///
/// Step 3 is a rule, not an oversight: there is NO filler text. A row with nothing to say says
/// nothing, and the same goes for a `Waiting` whose payload had no readable question — the yellow
/// dot already carries the message.
///
/// The engine node an agent sits on would be the natural third source, and is deliberately NOT
/// here: `workflow_runs` carries neither `terminal_id` nor `session_id`, so no run ↔ session link
/// exists to read. That is a schema gap, not a display choice.
///
/// # The twin, and what keeps it honest
///
/// This rule exists a second time on the other side of the boundary — `secondaryInfo` in
/// `apps/desktop/renderer/src/components/sidebar/use-agent-sessions.tsx` — for a renderer that
/// holds only the activity map and no resolved row. A rule written twice is a rule that drifts, so
/// the two are moored to ONE artifact the way the rank is moored to `STATE_PRIORITY`:
/// [`SECONDARY_INFO_CASES`] (`contract/home-secondary-info.json`) is replayed by BOTH suites, and
/// reordering the rule on either side fails the other.
pub fn secondary_info(activity: &ActivitySnapshot) -> Option<&str> {
    if activity.activity == Activity::Waiting {
        return activity.waiting_on.as_deref();
    }
    activity.tool.as_deref()
}

/// The SHARED cases for [`secondary_info`] and its renderer twin — see that function's doc. The
/// TS half imports the same file (`use-agent-sessions.test.tsx`); this is the Rust reader.
#[cfg(test)]
pub(crate) const SECONDARY_INFO_CASES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contract/home-secondary-info.json"
));

/// Fold ONE [`ActivitySnapshot`] to its row state — the five axes of a single snapshot, in the
/// same order the renderer's `terminalLineState` reads them: the LIVE kinds first (`waiting`,
/// `working` — never notifications, always surfaced), then, for an agent between turns, the
/// settled turn-end notification with the RED outranking the GREEN. A terminal with no entry at
/// all is `Idle`, which is the boot state of the whole store.
fn attention_of(activity: Option<&ActivitySnapshot>) -> Attention {
    let Some(snap) = activity else {
        return Attention::Idle;
    };
    match snap.activity {
        Activity::Waiting => Attention::Waiting,
        Activity::Working => Attention::Working,
        Activity::Idle => {
            if snap.error_unread {
                Attention::Error
            } else if snap.ready_unread {
                Attention::Ready
            } else {
                Attention::Idle
            }
        }
    }
}

/// A Spec card's ADVANCEMENT — the `done/total` tally its bar legends, and the phase the work
/// sits on. Returned as one value because the three come from the same scan.
struct SpecProgress {
    done: usize,
    total: usize,
    phase: Option<HomePhase>,
}

/// Fold ONE Spec's plan tasks into its card advancement. `task_rows` is that Spec's slice, already
/// grouped by the caller in one pass — this function never scans the other cards' tasks.
///
/// `done` counts `done`; `total` counts every plan task EXCEPT the cancelled ones — a cancelled
/// task is work that was taken back, so leaving it in the denominator would make a fully
/// delivered Spec read `4/5` forever.
///
/// The PHASE is the first one with unfinished plan work; when every phase is delivered it is the
/// LAST of the plan, so a finished Spec legends its final phase rather than falling back to
/// nothing. A Spec whose plan is empty (or unreadable — the column is free-form JSON and a
/// malformed value degrades to "no plan", never to an error) has no phase.
fn spec_progress(phases_json: &str, task_rows: &[&db::HomeSpecTaskRow]) -> SpecProgress {
    let mut done = 0usize;
    let mut total = 0usize;
    // The lowest phase number that still carries unfinished work — the phase the work is ON.
    let mut open_phase: Option<i64> = None;
    for row in task_rows.iter() {
        let (phase, status) = (row.phase, &row.status);
        match status.as_str() {
            db::TASK_STATUS_CANCELLED => continue,
            db::TASK_STATUS_DONE => {
                done += 1;
                total += 1;
            }
            _ => {
                total += 1;
                if let Some(number) = phase.map(i64::from) {
                    open_phase = Some(open_phase.map_or(number, |cur: i64| cur.min(number)));
                }
            }
        }
    }
    let entries: Vec<db::PhaseEntry> = serde_json::from_str(phases_json.trim()).unwrap_or_default();
    // The fallback to the LAST phase belongs to ONE case — no open work left, so the Spec legends
    // the phase it finished on. It must NOT also catch "there is open work, on a phase the plan
    // does not describe" (a truncated plan, an imported task, a phase dropped from the plan):
    // legending the last phase there would read as delivered while work is open. That case has no
    // honest legend, so it says nothing — the same answer as a Spec with no plan at all.
    let phase = match open_phase {
        Some(number) => entries.iter().find(|e| e.number == number),
        None => entries.last(),
    }
    .map(|entry| HomePhase {
        number: entry.number,
        total: entries.len(),
        name: entry.name.clone(),
    });
    SpecProgress { done, total, phase }
}

// --- The aggregate ---------------------------------------------------------------------

/// How many recently-touched Specs a home carries. A small fixed ceiling: the panel is a
/// "what moved lately" glance, not the Spec board.
pub const RECENT_SPECS: i64 = 8;

/// WHICH home is being read. The two pages are ONE read model with two scopes, not two read
/// models — the project home is the global one narrowed to a project, and nothing about the
/// shape, the state derivation or the sort differs between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeScope<'a> {
    /// The home of ONE project: only what belongs to it. A session on a loose terminal has no
    /// project, so it is not this project's — it belongs to the global view.
    Project(&'a str),
    /// The cross-project home: every project at once, in ONE flat attention-sorted flow.
    Global,
}

impl<'a> HomeScope<'a> {
    /// The scope as the queries take it — `None` is "every project".
    fn project_id(self) -> Option<&'a str> {
        match self {
            HomeScope::Project(id) => Some(id),
            HomeScope::Global => None,
        }
    }
}

/// The live per-terminal agent ACTIVITY seam — the runtime map the Claude per-turn hooks write.
/// Modelled as a trait (like [`RunStateOverlay`] for the command runner) so this read model does
/// not have to be handed the concrete store: the sidecar passes its shared
/// [`AgentActivityStore`], a test passes a closure, and a caller with no runtime passes
/// [`NoAgentActivity`] — for which every terminal reads idle, which is exactly the store's own
/// boot state.
pub trait AgentActivityOverlay {
    /// The live snapshot for `terminal_id`, or `None` when the terminal has no entry.
    fn activity(&self, terminal_id: &str) -> Option<ActivitySnapshot>;
}

/// The zero-overlay: no runtime activity at all, so every session reads `Idle` — the store's
/// boot state, and the honest answer for a caller that has no live half.
pub struct NoAgentActivity;

impl AgentActivityOverlay for NoAgentActivity {
    fn activity(&self, _terminal_id: &str) -> Option<ActivitySnapshot> {
        None
    }
}

/// The real store IS the overlay — no adapter type at the call site.
impl AgentActivityOverlay for AgentActivityStore {
    fn activity(&self, terminal_id: &str) -> Option<ActivitySnapshot> {
        self.snapshot(terminal_id)
    }
}

/// Any `Fn(&str) -> Option<ActivitySnapshot>` is an overlay — the ergonomic form for a test or a
/// caller that already holds a map.
impl<F> AgentActivityOverlay for F
where
    F: Fn(&str) -> Option<ActivitySnapshot>,
{
    fn activity(&self, terminal_id: &str) -> Option<ActivitySnapshot> {
        (self)(terminal_id)
    }
}

/// The PTY-liveness seam — whether a terminal record still has a live shell behind it. This is
/// what separates a working session from a DORMANT one, and it exists nowhere in the DB: the
/// idle-unload closes the PTY and leaves the row `active` on purpose, so only the process layer
/// can answer.
pub trait TerminalAliveOverlay {
    fn is_alive(&self, terminal_id: &str) -> bool;
}

/// The zero-overlay for a caller with NO PTY authority (a test, an MCP read, a cold read before
/// the runtime is up): every terminal reads ALIVE.
///
/// Deliberately "alive" and not "dead": a caller that cannot see processes cannot tell a dormant
/// session from a live one, and answering "dead" would empty the whole home for it — a far worse
/// lie than showing a session that happens to be unloaded. The sidecar, which owns the PTYs,
/// always passes the real answer.
pub struct AllTerminalsAlive;

impl TerminalAliveOverlay for AllTerminalsAlive {
    fn is_alive(&self, _terminal_id: &str) -> bool {
        true
    }
}

/// Any `Fn(&str) -> bool` is a liveness overlay — the form the sidecar uses over its
/// `PtyManager` without a named wrapper.
impl<F> TerminalAliveOverlay for F
where
    F: Fn(&str) -> bool,
{
    fn is_alive(&self, terminal_id: &str) -> bool {
        (self)(terminal_id)
    }
}

/// The three LIVE seams the home needs on top of the DB — bundled so [`snapshot`] keeps one
/// argument for "everything the process layer knows" instead of three.
pub struct HomeOverlays<'a> {
    /// The per-turn agent activity (the state and the secondary information).
    pub activity: &'a dyn AgentActivityOverlay,
    /// PTY liveness (the dormant filter).
    pub alive: &'a dyn TerminalAliveOverlay,
    /// The managed-command runner's FACTUAL outcome (the service state).
    pub services: &'a dyn RunStateOverlay,
}

impl Default for HomeOverlays<'_> {
    /// A pure-DB home: no runtime activity (everything idle), every terminal alive, every
    /// service on its persisted state. What a test or a cold read gets.
    fn default() -> Self {
        Self {
            activity: &NoAgentActivity,
            alive: &AllTerminalsAlive,
            services: &crate::workspace_runtime::NoRunStateOverlay,
        }
    }
}

/// A PROJECT as a CROSS-PROJECT ROW wears it — the identity that says which project a line
/// belongs to without a second line of text, and the exact set of fields the renderer's
/// `<ProjectMark>` paints. `None` on a home row whose terminal has no workspace.
///
/// It is named after the home because the home needed it first, and it is REUSED by every other
/// cross-project read model rather than copied (the Idées table's `idea_list` is the second): one
/// project identity on the wire, so a project cannot wear two faces on two screens.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomeProject {
    pub id: String,
    pub name: String,
    pub accent: Option<String>,
    pub icon: Option<String>,
    pub image_path: Option<String>,
}

/// A WORKSPACE as a home row wears it. `branch` is the workspace row's create-time value (this
/// module makes no Git call — same rule as `workspace_runtime`), and it is filled the SAME way on
/// every family that carries one: a `None` here always means "this workspace has no branch", never
/// "this producer did not bother to select it".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomeWorkspace {
    pub id: String,
    pub name: String,
    pub branch: Option<String>,
}

/// ONE agent session in the flow — a line of the home.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomeSession {
    pub session_id: String,
    /// The FOCUS target: selecting this terminal is what lands on the session.
    pub terminal_id: String,
    pub agent_kind: String,
    /// The row's state, in the shared vocabulary (`waiting`|`error`|`ready`|`working`|`idle`).
    pub state: &'static str,
    /// The short text saying what the agent is asking or doing — [`secondary_info`]. `None` is a
    /// legitimate answer and renders NO text (there is no filler).
    pub secondary: Option<String>,
    /// The terminal's manual label (`None` when never renamed) and cwd — the two inputs the
    /// renderer's `resolveDisplayName` needs to name the line exactly as the sidebar does.
    pub label: Option<String>,
    pub cwd: String,
    pub started_at: i64,
    /// `None` on a session hosted by a LOOSE terminal — returned without a project, never
    /// omitted and never attached to an arbitrary one.
    pub workspace: Option<HomeWorkspace>,
    pub project: Option<HomeProject>,
}

/// ONE service in the flow — a managed command that is RUNNING, or that FAILED and has not been
/// acknowledged. Nothing else is on the home: a stopped service is not news.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomeService {
    pub instance_id: String,
    pub name: String,
    /// `running` or `error` — the FACTUAL state, from the live runner when it backs the
    /// instance, else the persisted outcome.
    pub run_state: String,
    pub exit_code: Option<i32>,
    pub unread: bool,
    pub workspace: HomeWorkspace,
    pub project: HomeProject,
}

/// WHERE a Spec's work currently sits in its phase plan — the card's phase legend. `None` on a
/// Spec with no plan at all, and the card then simply says nothing about phases: the plan is
/// optional (PDR-0022), so its absence is a state, not a hole to fill.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomePhase {
    /// The 1-based number of the phase the work is ON — the FIRST phase with unfinished plan
    /// work, else the last of the plan (everything delivered).
    pub number: i64,
    /// How many phases the plan holds.
    pub total: usize,
    /// The phase's name, verbatim from the plan (may be empty — the plan allows it).
    pub name: String,
}

/// ONE recently-touched Spec.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomeSpec {
    pub spec_id: String,
    /// The human key, `PROJECTKEY-number`.
    pub key: String,
    pub title: String,
    pub status: String,
    pub updated_at: i64,
    /// Plan tasks reaching `done` (the numerator of the card's `done/total`).
    pub tasks_done: usize,
    /// Plan tasks in total — `0` on a Spec with no tasks, and the card draws NO progress line at
    /// all rather than an empty bar.
    pub tasks_total: usize,
    /// Where the work sits in the phase plan, or `None` when the Spec has none.
    pub phase: Option<HomePhase>,
    pub project: HomeProject,
}

/// ONE bare terminal — a shell with no agent on it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomeTerminal {
    pub terminal_id: String,
    pub label: Option<String>,
    pub cwd: String,
    pub exec_state: String,
    pub exec_state_unread: bool,
    pub workspace: Option<HomeWorkspace>,
    pub project: Option<HomeProject>,
}

/// The home read model — the four families, for one scope, from ONE call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HomeSnapshot {
    /// The agent sessions in the flow, sorted `waiting > error > ready > working > idle` and
    /// oldest-first within a rank. ONE flat list: never grouped by project, because the whole
    /// point is to see the one that is blocked whichever project it is in.
    pub sessions: Vec<HomeSession>,
    /// The services that are running or unacknowledged-failed, oldest first.
    pub services: Vec<HomeService>,
    /// The most recently touched Specs, newest first, capped at [`RECENT_SPECS`].
    pub specs: Vec<HomeSpec>,
    /// The bare terminals (no agent), in sidebar order.
    pub terminals: Vec<HomeTerminal>,
}

/// Build the home for `scope` — ONE call, four joined reads, no per-workspace fan-out.
///
/// The DB half selects rows and bounds them; every DECISION is taken here, over one authority
/// each: [`session_attention`] says which sessions are in the flow and in what state,
/// [`secondary_info`] says what each line reads, [`Attention::rank`] orders them, and the
/// service rule ("running always, failed while unacknowledged") is applied once.
///
/// Pure projection: reads only, never writes, never polls.
pub fn snapshot(
    conn: &mut SqliteConnection,
    scope: HomeScope<'_>,
    overlays: &HomeOverlays<'_>,
) -> QueryResult<HomeSnapshot> {
    let project_id = scope.project_id();

    // --- Sessions -----------------------------------------------------------------------
    let session_rows = db::home_sessions(conn, project_id)?;
    // Which terminals CARRY an agent: every one with a live session row, whether or not that
    // session made it into the flow (a dormant agent's terminal is still not a bare shell).
    // Collected from the rows already in hand — no fifth query to ask the same thing twice.
    // The query loads `active` rows only, so every row here is a live session by construction.
    let agent_terminals: std::collections::HashSet<&str> = session_rows
        .iter()
        .map(|r| r.terminal_id.as_str())
        .collect();

    let mut sessions: Vec<(usize, HomeSession)> = session_rows
        .iter()
        .filter_map(|row| {
            let activity = overlays.activity.activity(&row.terminal_id);
            let attention = session_attention(
                &row.session_state,
                overlays.alive.is_alive(&row.terminal_id),
                activity.as_ref(),
            )?;
            Some((
                attention.rank(),
                HomeSession {
                    session_id: row.session_id.clone(),
                    terminal_id: row.terminal_id.clone(),
                    agent_kind: row.agent_kind.clone(),
                    state: attention.as_str(),
                    secondary: activity
                        .as_ref()
                        .and_then(secondary_info)
                        .map(str::to_string),
                    label: row.terminal_label.clone(),
                    cwd: row.terminal_cwd.clone(),
                    started_at: row.started_at,
                    workspace: home_workspace(
                        row.workspace_id.as_deref(),
                        row.workspace_name.as_deref(),
                        row.workspace_branch.as_deref(),
                    ),
                    project: home_project(
                        row.project_id.as_deref(),
                        row.project_name.as_deref(),
                        row.project_accent.as_deref(),
                        row.project_icon.as_deref(),
                        row.project_image_path.as_deref(),
                    ),
                },
            ))
        })
        .collect();
    // THE CONTRACT SORT: by attention rank, oldest first within a rank. The query already
    // returned the rows oldest-first, and `sort_by_key` is stable, so the rank alone is the key —
    // the age ordering survives underneath it. Global: the flow is never grouped by project.
    sessions.sort_by_key(|(rank, _)| *rank);
    let sessions: Vec<HomeSession> = sessions.into_iter().map(|(_, s)| s).collect();

    // --- Services -----------------------------------------------------------------------
    // The FACTUAL state comes from the live runner when it backs the instance, and only from
    // the persisted outcome when it does not — never from `last_state` alone, which is a memory
    // of the last run and not a statement about now.
    let services: Vec<HomeService> = db::home_services(conn, project_id)?
        .into_iter()
        .filter_map(|row| {
            let (state, exit_code, unread) = match overlays.services.outcome(&row.instance_id) {
                Some(live) => live,
                None => (
                    RunState::from_db_str(&row.last_state),
                    row.last_exit_code,
                    row.unread,
                ),
            };
            // RUNNING is always news. A FAILURE is news until it is acknowledged. Everything
            // else — idle (including a declared command never launched), and a run that simply
            // succeeded — is not on the home.
            let keep = match state {
                RunState::Running => true,
                RunState::Error => unread,
                _ => false,
            };
            if !keep {
                return None;
            }
            Some(HomeService {
                instance_id: row.instance_id,
                name: row.name,
                run_state: state.as_db_str().to_string(),
                exit_code,
                unread,
                workspace: HomeWorkspace {
                    id: row.workspace_id,
                    name: row.workspace_name,
                    branch: row.workspace_branch,
                },
                project: HomeProject {
                    id: row.project_id,
                    name: row.project_name,
                    accent: row.project_accent,
                    icon: row.project_icon,
                    image_path: row.project_image_path,
                },
            })
        })
        .collect();

    // --- Specs --------------------------------------------------------------------------
    let spec_rows = db::home_specs(conn, project_id, RECENT_SPECS)?;
    // The plan tasks of the cards in hand, in ONE bounded query (never one per card). Folded to
    // a per-Spec tally + phase below.
    let spec_ids: Vec<String> = spec_rows.iter().map(|r| r.spec_id.clone()).collect();
    let task_rows = db::home_spec_tasks(conn, &spec_ids)?;
    // Grouped ONCE, by Spec, instead of re-scanning every task row per card — the same reflex the
    // batched query above applies to the DB, applied to the fold that reads it.
    let mut tasks_by_spec: std::collections::HashMap<&str, Vec<&db::HomeSpecTaskRow>> =
        std::collections::HashMap::new();
    for row in &task_rows {
        tasks_by_spec
            .entry(row.spec_id.as_str())
            .or_default()
            .push(row);
    }
    let specs: Vec<HomeSpec> = spec_rows
        .into_iter()
        .map(|row| {
            let progress = spec_progress(
                &row.phases,
                tasks_by_spec
                    .get(row.spec_id.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            );
            HomeSpec {
                key: format!("{}-{}", row.project_key, row.number),
                spec_id: row.spec_id,
                title: row.title,
                status: row.status,
                updated_at: row.updated_at,
                tasks_done: progress.done,
                tasks_total: progress.total,
                phase: progress.phase,
                project: HomeProject {
                    id: row.project_id,
                    name: row.project_name,
                    accent: row.project_accent,
                    icon: row.project_icon,
                    image_path: row.project_image_path,
                },
            }
        })
        .collect();

    // --- Terminals ----------------------------------------------------------------------
    // BARE shells only: a terminal hosting an agent lives in the sessions list and must not be
    // counted twice under a name that says nothing about the agent on it.
    let terminals: Vec<HomeTerminal> = db::home_terminals(conn, project_id)?
        .into_iter()
        .filter(|row| !agent_terminals.contains(row.terminal_id.as_str()))
        .map(|row| HomeTerminal {
            workspace: home_workspace(
                row.workspace_id.as_deref(),
                row.workspace_name.as_deref(),
                row.workspace_branch.as_deref(),
            ),
            project: home_project(
                row.project_id.as_deref(),
                row.project_name.as_deref(),
                row.project_accent.as_deref(),
                row.project_icon.as_deref(),
                row.project_image_path.as_deref(),
            ),
            terminal_id: row.terminal_id,
            label: row.label,
            cwd: row.cwd,
            exec_state: row.exec_state,
            exec_state_unread: row.exec_state_unread,
        })
        .collect();

    Ok(HomeSnapshot {
        sessions,
        services,
        specs,
        terminals,
    })
}

/// Fold the LEFT-joined workspace columns into an optional reference: present only when the
/// join actually matched (a loose terminal has no workspace, which is a state and not a gap).
fn home_workspace(
    id: Option<&str>,
    name: Option<&str>,
    branch: Option<&str>,
) -> Option<HomeWorkspace> {
    Some(HomeWorkspace {
        id: id?.to_string(),
        name: name.unwrap_or_default().to_string(),
        branch: branch.map(str::to_string),
    })
}

/// The same fold for the LEFT-joined project columns.
fn home_project(
    id: Option<&str>,
    name: Option<&str>,
    accent: Option<&str>,
    icon: Option<&str>,
    image_path: Option<&str>,
) -> Option<HomeProject> {
    Some(HomeProject {
        id: id?.to_string(),
        name: name.unwrap_or_default().to_string(),
        accent: accent.map(str::to_string),
        icon: icon.map(str::to_string),
        image_path: image_path.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{SESSION_STATE_ENDED, SESSION_STATE_RESUME_FAILED, SESSION_STATE_UNKNOWN};

    /// An [`ActivitySnapshot`] in one line — the three axes that matter here, with the
    /// stale-plugin badge (orthogonal to the state machine) always off and no secondary text.
    fn snap(activity: Activity, ready_unread: bool, error_unread: bool) -> ActivitySnapshot {
        ActivitySnapshot {
            activity,
            ready_unread,
            error_unread,
            plugin_outdated: false,
            waiting_on: None,
            tool: None,
        }
    }

    /// The same, carrying the two secondary-information facts.
    fn snap_with(
        activity: Activity,
        waiting_on: Option<&str>,
        tool: Option<&str>,
    ) -> ActivitySnapshot {
        ActivitySnapshot {
            waiting_on: waiting_on.map(str::to_string),
            tool: tool.map(str::to_string),
            ..snap(activity, false, false)
        }
    }

    /// THE RANK CONTRACT. `waiting > error > ready > working > idle` is the renderer's
    /// `STATE_PRIORITY`, not an order the home invented — the same fold the terminal line, the
    /// workspace accordion and the project rail dot already apply. This test pins the sequence
    /// here; `home-state.test.ts` pins the SAME sequence on the renderer side by DERIVING it from
    /// `STATE_PRIORITY` itself. Reorder either one and its own side fails.
    #[test]
    fn attention_order_is_the_app_priority() {
        assert_eq!(
            ATTENTION_ORDER.map(Attention::as_str),
            ["waiting", "error", "ready", "working", "idle"],
            "the home's rank IS STATE_PRIORITY (waiting > error > success/ready > running/working \
             > idle) — a divergence here is a second sort order, which is the thing not to build"
        );
        // The rank is strictly increasing along that order, and every variant has one.
        for (i, state) in ATTENTION_ORDER.iter().enumerate() {
            assert_eq!(state.rank(), i, "{} ranks at {i}", state.as_str());
        }
        assert!(
            Attention::Waiting.rank() < Attention::Error.rank(),
            "a human on the critical path outranks even a failure"
        );
        assert!(Attention::Error.rank() < Attention::Ready.rank());
        assert!(Attention::Ready.rank() < Attention::Working.rank());
        assert!(Attention::Working.rank() < Attention::Idle.rank());
    }

    /// The FIVE states come off ONE snapshot: the live kinds pass through, and an agent between
    /// turns reads its settled turn-end notification — red before green, exactly as the terminal
    /// line does. No entry at all is `Idle` (the store is empty at boot).
    #[test]
    fn the_five_states_are_derived_from_one_activity_snapshot() {
        let live = |a: Activity, ready, error| {
            session_attention(SESSION_STATE_ACTIVE, true, Some(&snap(a, ready, error)))
        };
        assert_eq!(
            live(Activity::Waiting, false, false),
            Some(Attention::Waiting)
        );
        assert_eq!(
            live(Activity::Working, false, false),
            Some(Attention::Working)
        );
        assert_eq!(live(Activity::Idle, false, false), Some(Attention::Idle));
        assert_eq!(
            live(Activity::Idle, true, false),
            Some(Attention::Ready),
            "a finished turn nobody read is `ready`"
        );
        assert_eq!(
            live(Activity::Idle, false, true),
            Some(Attention::Error),
            "a turn that ended on an API error is `error`"
        );
        assert_eq!(
            live(Activity::Idle, true, true),
            Some(Attention::Error),
            "the red outranks the green, as on the line"
        );
        // A LIVE kind is never a notification: it shows whatever is unread underneath.
        assert_eq!(
            live(Activity::Waiting, true, true),
            Some(Attention::Waiting),
            "blocked on a human wins over any settled notification"
        );
        assert_eq!(
            live(Activity::Working, true, true),
            Some(Attention::Working)
        );
        // No runtime entry (the boot state, and every quiet terminal) → idle, not absent.
        assert_eq!(
            session_attention(SESSION_STATE_ACTIVE, true, None),
            Some(Attention::Idle)
        );
    }

    /// THE DORMANT FILTER. The idle-unload closes the PTY and LEAVES the row `active` so a revisit
    /// can resume it — that row has no live process behind it and must not appear. There is no
    /// "off" state for it: it is simply out of the flow.
    #[test]
    fn a_dormant_session_is_not_returned() {
        assert_eq!(
            session_attention(SESSION_STATE_ACTIVE, false, None),
            None,
            "an `active` row whose PTY is gone is dormant — out of the flow"
        );
        assert_eq!(
            session_attention(
                SESSION_STATE_ACTIVE,
                false,
                Some(&snap(Activity::Idle, true, false))
            ),
            None,
            "…even carrying a stale ready notification"
        );
        // The SAME row with its PTY alive is in the flow — the filter is the liveness, nothing else.
        assert_eq!(
            session_attention(
                SESSION_STATE_ACTIVE,
                true,
                Some(&snap(Activity::Idle, true, false))
            ),
            Some(Attention::Ready)
        );
    }

    /// The persisted `agent_sessions.state` is the OTHER half of the cross: it separates a session
    /// that ENDED from one that is merely idle. `active` is the ONLY state in the flow — `ended`,
    /// `unknown` and `resume_failed` stay out whatever the runtime says, INCLUDING an `ended` row
    /// sitting on an unacknowledged error (see `session_attention`'s doc: the activity store is
    /// keyed by terminal, so that error belongs to the session that succeeded it, not to this one).
    #[test]
    fn session_state_separates_ended_from_idle() {
        let working_snap = snap(Activity::Working, false, false);
        let working = Some(&working_snap);
        assert_eq!(
            session_attention(SESSION_STATE_UNKNOWN, true, working),
            None,
            "a session nyx cannot vouch for is not on the board"
        );
        assert_eq!(
            session_attention(SESSION_STATE_RESUME_FAILED, true, working),
            None
        );
        assert_eq!(
            session_attention(SESSION_STATE_ENDED, true, working),
            None,
            "an ended session is out of the active flow"
        );
        assert_eq!(
            session_attention(
                SESSION_STATE_ENDED,
                true,
                Some(&snap(Activity::Idle, true, false))
            ),
            None,
            "…including one whose last turn merely finished"
        );
        assert_eq!(
            session_attention(
                SESSION_STATE_ENDED,
                true,
                Some(&snap(Activity::Idle, false, true))
            ),
            None,
            "…and including one carrying an unread error: no exception survives for `ended`"
        );
    }

    /// THE OTHER HALF of the same decision, over a REAL database: an `ended` session on an alive
    /// terminal, with an unacknowledged error posted on that terminal, is returned NOWHERE — not
    /// as a session line, and not as a bare terminal either while a live session still holds the
    /// terminal. This is the case the removed `ended` exception claimed to serve; it is also the
    /// case that would have shown one terminal on two lines.
    #[test]
    fn an_ended_session_with_an_unread_error_is_not_on_the_home() {
        use crate::agent_activity::ActivityEvent;

        let mut conn = open_in_memory();
        let (_proj, ws) = project(&mut conn, "Ended", "ended");
        let term = agent_terminal(&mut conn, Some(&ws), "ended-term");
        let session = crate::db::active_session_for(&mut conn, &term.id, "claude_code")
            .unwrap()
            .expect("the helper seeded an active session");

        // The session ends while the terminal stays ALIVE — the exact shape the exception described.
        crate::db::mark_session_ended(&mut conn, &session.id).unwrap();

        // …and the terminal carries an unacknowledged error in the runtime store.
        let store = AgentActivityStore::new();
        store.apply(&term.id, ActivityEvent::PromptSubmitted);
        store.apply(&term.id, ActivityEvent::TurnFailed);
        assert!(
            store.snapshot(&term.id).is_some_and(|s| s.error_unread),
            "fixture guard: the store really holds an unacknowledged error for this terminal"
        );

        let overlays = HomeOverlays {
            activity: &store,
            ..HomeOverlays::default()
        };
        let home = snapshot(&mut conn, HomeScope::Global, &overlays).unwrap();
        assert!(
            home.sessions.is_empty(),
            "an ended session is off the board even carrying an unread error: {:?}",
            home.sessions
        );
        // And the terminal is a bare shell again — ONCE. The invariant the exception broke was
        // never "it appears twice", it was "it appears at all under a dead session".
        assert_eq!(
            home.terminals
                .iter()
                .filter(|t| t.terminal_id == term.id)
                .count(),
            1,
            "the terminal is listed exactly once, and as a bare shell"
        );
    }

    // --- The AGGREGATE over a real database ------------------------------------------------

    use crate::db::{open_in_memory, CommandSource, SessionCapture};
    use crate::usecase::{self, DomainEventSource, NullEventSink};
    use std::collections::HashMap;

    /// A platform-correct absolute path (the paths are normalised on write, so a POSIX-shaped
    /// literal would not round-trip on Windows).
    fn root_path(tag: &str) -> String {
        #[cfg(windows)]
        {
            format!("C:\\home-{tag}")
        }
        #[cfg(not(windows))]
        {
            format!("/home-{tag}")
        }
    }

    /// A project plus its root workspace, through the shared use case.
    fn project(
        conn: &mut SqliteConnection,
        name: &str,
        tag: &str,
    ) -> (crate::db::Project, crate::db::Workspace) {
        usecase::create_project(
            conn,
            &NullEventSink,
            name,
            &root_path(tag),
            None,
            DomainEventSource::Ui,
        )
        .unwrap()
    }

    /// A terminal attached to `workspace`, hosting an agent session.
    fn agent_terminal(
        conn: &mut SqliteConnection,
        workspace: Option<&crate::db::Workspace>,
        tag: &str,
    ) -> crate::db::Terminal {
        let terminal = crate::db::create_terminal(conn, &root_path(tag), None).unwrap();
        if let Some(ws) = workspace {
            crate::db::attach_terminal(conn, &terminal.id, &ws.id, "auto").unwrap();
        }
        crate::db::record_session_start(
            conn,
            &terminal.id,
            "claude_code",
            SessionCapture {
                workspace_id: workspace.map(|w| w.id.clone()),
                external_session_id: format!("sess-{tag}"),
                cwd: root_path(tag),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        terminal
    }

    /// An overlay that reports a fixed activity per terminal (and `None` for the rest).
    fn activity_map(entries: Vec<(&str, ActivitySnapshot)>) -> HashMap<String, ActivitySnapshot> {
        entries
            .into_iter()
            .map(|(id, snap)| (id.to_string(), snap))
            .collect()
    }

    /// THE FOUR FAMILIES, in both scopes. The project home is the global one narrowed: same
    /// shape, same derivation, same sort — only the reach differs. And the global one carries
    /// the project/workspace identity on every row plus the terminal to focus, which is what
    /// makes a cross-project flow actionable.
    #[test]
    fn snapshot_returns_the_four_families_for_a_project_and_globally() {
        let mut conn = open_in_memory();
        let (proj_a, ws_a) = project(&mut conn, "Alpha", "a");
        let (proj_b, ws_b) = project(&mut conn, "Beta", "b");

        // One agent session per project, one service per project, one Spec per project, and a
        // bare terminal in A.
        let term_a = agent_terminal(&mut conn, Some(&ws_a), "a-agent");
        let term_b = agent_terminal(&mut conn, Some(&ws_b), "b-agent");
        let bare_a = crate::db::create_terminal(&mut conn, &root_path("a-bare"), None).unwrap();
        crate::db::attach_terminal(&mut conn, &bare_a.id, &ws_a.id, "auto").unwrap();
        for (project_id, ws, name) in [(&proj_a.id, &ws_a, "dev-a"), (&proj_b.id, &ws_b, "dev-b")] {
            crate::db::create_template(
                &mut conn,
                project_id,
                name,
                "npm run dev",
                None,
                CommandSource::default(),
            )
            .unwrap();
            let instance = crate::db::list_instances_for_workspace(&mut conn, &ws.id).unwrap()[0]
                .id
                .clone();
            crate::db::set_run_state(&mut conn, &instance, "running", None).unwrap();
        }
        crate::db::create_spec(&mut conn, &proj_a.id, "Spec A", Default::default()).unwrap();
        crate::db::create_spec(&mut conn, &proj_b.id, "Spec B", Default::default()).unwrap();
        // A's workspace is on a branch. There is no setter (the branch is Git-detected at create
        // time), so it is written straight in — the point being what the READ does with it.
        {
            use diesel::prelude::*;
            diesel::update(crate::schema::workspaces::table.find(&ws_a.id))
                .set(crate::schema::workspaces::branch.eq(Some("feature/home")))
                .execute(&mut conn)
                .unwrap();
        }

        // PROJECT SCOPE — only A.
        let overlays = HomeOverlays::default();
        let a = snapshot(&mut conn, HomeScope::Project(&proj_a.id), &overlays).unwrap();
        assert_eq!(a.sessions.len(), 1, "A's one session");
        assert_eq!(a.sessions[0].terminal_id, term_a.id);
        assert_eq!(a.services.len(), 1);
        assert_eq!(a.services[0].name, "dev-a");
        assert_eq!(a.specs.len(), 1);
        assert_eq!(a.specs[0].title, "Spec A");
        assert_eq!(a.terminals.len(), 1, "A's one bare shell");
        assert_eq!(a.terminals[0].terminal_id, bare_a.id);
        // ONE `HomeWorkspace`, filled the SAME way whichever family produces it: a `branch` that
        // reads `None` on a service while the session next to it — same workspace — reads a real
        // one would be the type lying about which producer you hold.
        let branch = Some("feature/home".to_string());
        assert_eq!(a.sessions[0].workspace.as_ref().unwrap().branch, branch);
        assert_eq!(a.services[0].workspace.branch, branch, "so does a service");
        assert_eq!(a.terminals[0].workspace.as_ref().unwrap().branch, branch);

        // GLOBAL SCOPE — every project, each row carrying its project + workspace + the
        // terminal to focus.
        let all = snapshot(&mut conn, HomeScope::Global, &overlays).unwrap();
        let session_terminals: Vec<&str> = all
            .sessions
            .iter()
            .map(|s| s.terminal_id.as_str())
            .collect();
        assert!(session_terminals.contains(&term_a.id.as_str()));
        assert!(
            session_terminals.contains(&term_b.id.as_str()),
            "the global home reaches into every project"
        );
        let b_row = all
            .sessions
            .iter()
            .find(|s| s.terminal_id == term_b.id)
            .unwrap();
        assert_eq!(b_row.project.as_ref().unwrap().id, proj_b.id);
        assert_eq!(b_row.project.as_ref().unwrap().name, "Beta");
        assert_eq!(b_row.workspace.as_ref().unwrap().id, ws_b.id);
        assert_eq!(all.services.len(), 2);
        assert_eq!(all.specs.len(), 2);
        assert_eq!(all.terminals.len(), 1, "only A has a bare shell");
    }

    /// A LOOSE terminal has no workspace, so its session has no project — and it is returned
    /// exactly like that: present, marked with nothing, never attached to a project it does not
    /// belong to. In a PROJECT scope it is simply not that project's.
    #[test]
    fn a_session_on_a_loose_terminal_is_returned_without_a_project() {
        let mut conn = open_in_memory();
        let (proj, _ws) = project(&mut conn, "Alpha", "loose-a");
        let loose = agent_terminal(&mut conn, None, "loose");

        let overlays = HomeOverlays::default();
        let all = snapshot(&mut conn, HomeScope::Global, &overlays).unwrap();
        let row = all
            .sessions
            .iter()
            .find(|s| s.terminal_id == loose.id)
            .expect("a loose session is on the global home, not dropped");
        assert_eq!(row.project, None, "no project — and no invented one");
        assert_eq!(row.workspace, None);
        assert_eq!(row.cwd, root_path("loose"), "still focusable and named");

        let scoped = snapshot(&mut conn, HomeScope::Project(&proj.id), &overlays).unwrap();
        assert!(
            scoped.sessions.is_empty(),
            "a session that belongs to no project is not this project's"
        );
    }

    /// SERVICES are the exception to "show the state": only what is NEWS shows. A running
    /// service always; a failure while it is unacknowledged; nothing else — not an idle command
    /// that was declared and never launched, not a run that simply succeeded, not a failure the
    /// user has already seen. And the state comes from the LIVE runner when it backs the
    /// instance, never from the persisted memory of the last run alone.
    #[test]
    fn only_running_and_unacknowledged_failed_services_are_returned() {
        let mut conn = open_in_memory();
        let (proj, ws) = project(&mut conn, "Alpha", "svc");
        for name in ["never-run", "up", "broken", "seen", "done"] {
            crate::db::create_template(
                &mut conn,
                &proj.id,
                name,
                "npm run dev",
                None,
                CommandSource::default(),
            )
            .unwrap();
        }
        let by_name: HashMap<String, String> =
            crate::db::list_instances_for_workspace(&mut conn, &ws.id)
                .unwrap()
                .into_iter()
                .map(|i| (i.name.clone(), i.id.clone()))
                .collect();
        crate::db::set_run_state(&mut conn, &by_name["up"], "running", None).unwrap();
        crate::db::set_run_state(&mut conn, &by_name["broken"], "error", Some(1)).unwrap();
        crate::db::set_run_state(&mut conn, &by_name["seen"], "error", Some(1)).unwrap();
        crate::db::acknowledge_instance(&mut conn, &by_name["seen"]).unwrap();
        crate::db::set_run_state(&mut conn, &by_name["done"], "success", Some(0)).unwrap();

        let overlays = HomeOverlays::default();
        let home = snapshot(&mut conn, HomeScope::Project(&proj.id), &overlays).unwrap();
        let names: Vec<&str> = home.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["up", "broken"],
            "running always; a failure only while it is still unacknowledged"
        );
        assert_eq!(home.services[0].run_state, "running");
        assert_eq!(home.services[1].run_state, "error");
        assert_eq!(home.services[1].exit_code, Some(1));

        // THE LIVE OVERLAY WINS: the runner says the "done" instance is running again, and the
        // "up" one has stopped. The persisted `last_state` says the opposite, and it loses.
        let up_id = by_name["up"].clone();
        let done_id = by_name["done"].clone();
        let live = move |id: &str| {
            if id == done_id {
                Some((RunState::Running, None, false))
            } else if id == up_id {
                Some((RunState::Idle, None, false))
            } else {
                None
            }
        };
        let overlays = HomeOverlays {
            services: &live,
            ..HomeOverlays::default()
        };
        let home = snapshot(&mut conn, HomeScope::Project(&proj.id), &overlays).unwrap();
        let names: Vec<&str> = home.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["broken", "done"],
            "the runner's word, not the row's memory"
        );
    }

    /// A terminal HOSTING an agent is not a bare shell: it is a session row, and it appears
    /// exactly once — under the agent, never a second time under a name that says nothing about
    /// it. This holds even when the session is dormant (its PTY closed): that terminal is still
    /// not a plain shell.
    #[test]
    fn a_terminal_hosting_an_agent_is_not_a_bare_terminal() {
        let mut conn = open_in_memory();
        let (proj, ws) = project(&mut conn, "Alpha", "bare");
        let hosting = agent_terminal(&mut conn, Some(&ws), "hosting");
        let bare = crate::db::create_terminal(&mut conn, &root_path("plain"), None).unwrap();
        crate::db::attach_terminal(&mut conn, &bare.id, &ws.id, "auto").unwrap();

        let overlays = HomeOverlays::default();
        let home = snapshot(&mut conn, HomeScope::Project(&proj.id), &overlays).unwrap();
        let terminals: Vec<&str> = home
            .terminals
            .iter()
            .map(|t| t.terminal_id.as_str())
            .collect();
        assert_eq!(terminals, vec![bare.id.as_str()]);
        assert_eq!(home.sessions.len(), 1);
        assert_eq!(home.sessions[0].terminal_id, hosting.id);

        // DORMANT (PTY closed, the row still `active`): the session leaves the flow — and its
        // terminal does NOT reappear among the bare shells.
        let hosting_id = hosting.id.clone();
        let dormant = move |id: &str| id != hosting_id;
        let overlays = HomeOverlays {
            alive: &dormant,
            ..HomeOverlays::default()
        };
        let home = snapshot(&mut conn, HomeScope::Project(&proj.id), &overlays).unwrap();
        assert!(
            home.sessions.is_empty(),
            "a dormant session is out of the flow"
        );
        assert_eq!(
            home.terminals
                .iter()
                .map(|t| t.terminal_id.as_str())
                .collect::<Vec<_>>(),
            vec![bare.id.as_str()],
            "…and it does not come back as a bare shell either"
        );
    }

    /// THE CONTRACT SORT over the real aggregate: `waiting > error > ready > working > idle`,
    /// oldest first WITHIN a rank, and ONE flat flow — a session of another project sits between
    /// two of this one if that is what its state says. Grouping by project would defeat the
    /// point, which is to see the blocked one first whoever it belongs to.
    #[test]
    fn the_flow_is_sorted_by_attention_then_by_age_across_projects() {
        let mut conn = open_in_memory();
        let (_proj_a, ws_a) = project(&mut conn, "Alpha", "sort-a");
        let (_proj_b, ws_b) = project(&mut conn, "Beta", "sort-b");
        // Created in this order, so this IS the age order (oldest first).
        let t_working_old = agent_terminal(&mut conn, Some(&ws_a), "s1");
        let t_idle = agent_terminal(&mut conn, Some(&ws_b), "s2");
        let t_waiting = agent_terminal(&mut conn, Some(&ws_b), "s3");
        let t_working_new = agent_terminal(&mut conn, Some(&ws_a), "s4");
        let t_ready = agent_terminal(&mut conn, Some(&ws_a), "s5");
        let t_error = agent_terminal(&mut conn, Some(&ws_b), "s6");

        let map = activity_map(vec![
            (&t_working_old.id, snap(Activity::Working, false, false)),
            (&t_waiting.id, snap(Activity::Waiting, false, false)),
            (&t_working_new.id, snap(Activity::Working, false, false)),
            (&t_ready.id, snap(Activity::Idle, true, false)),
            (&t_error.id, snap(Activity::Idle, false, true)),
        ]);
        let lookup = move |id: &str| map.get(id).cloned();
        let overlays = HomeOverlays {
            activity: &lookup,
            ..HomeOverlays::default()
        };
        let home = snapshot(&mut conn, HomeScope::Global, &overlays).unwrap();
        let order: Vec<(&str, &str)> = home
            .sessions
            .iter()
            .map(|s| (s.terminal_id.as_str(), s.state))
            .collect();
        assert_eq!(
            order,
            vec![
                (t_waiting.id.as_str(), "waiting"),
                (t_error.id.as_str(), "error"),
                (t_ready.id.as_str(), "ready"),
                // Two `working` rows: the OLDER one first.
                (t_working_old.id.as_str(), "working"),
                (t_working_new.id.as_str(), "working"),
                (t_idle.id.as_str(), "idle"),
            ],
            "rank first, age within a rank, projects interleaved"
        );
    }

    /// THE SECONDARY INFORMATION, resolved in ONE order: what the agent is BLOCKED on if it is
    /// blocked, otherwise the tool it is running, otherwise NOTHING. The last branch is the rule
    /// that matters — a row with nothing to say renders no filler.
    ///
    /// THE CASES ARE NOT WRITTEN HERE. This rule exists TWICE — [`secondary_info`] and the
    /// renderer's `secondaryInfo` (`components/sidebar/use-agent-sessions.tsx`) — so its cases
    /// live in `contract/home-secondary-info.json`, which BOTH suites replay. Reorder the rule on
    /// one side and the other side's suite fails: the same mechanism `design-fixtures.json` and
    /// `spec-ref-corpus.txt` use, and the equivalent of what `STATE_PRIORITY` does for the rank.
    /// The TS half is `use-agent-sessions.test.tsx`.
    #[test]
    fn secondary_info_prefers_the_block_then_the_tool_then_nothing() {
        let fixture: serde_json::Value = serde_json::from_str(SECONDARY_INFO_CASES)
            .expect("contract/home-secondary-info.json is valid JSON");
        let cases = fixture["cases"].as_array().expect("`cases` is an array");
        assert!(
            cases.len() >= 7,
            "the shared fixture lost cases — both suites read it, so a shrink here silently \
             weakens the renderer's half too"
        );

        for case in cases {
            let id = case["id"].as_str().expect("every case is named");
            let activity = match case["activity"].as_str().expect("`activity` is a string") {
                "waiting" => Activity::Waiting,
                "working" => Activity::Working,
                "idle" => Activity::Idle,
                other => panic!("{id}: unknown activity kind `{other}`"),
            };
            let snapshot = snap_with(activity, case["waiting_on"].as_str(), case["tool"].as_str());
            assert_eq!(
                secondary_info(&snapshot),
                case["secondary"].as_str(),
                "{id}: {}",
                case["why"].as_str().unwrap_or_default()
            );
        }
    }

    /// A Spec card's ADVANCEMENT — the `done/total` its bar legends, and the phase the work is on.
    ///
    /// The two rules worth pinning are the ones that would otherwise drift silently: a CANCELLED
    /// task leaves the denominator (work taken back is not work owed, so a delivered Spec reads
    /// `4/4` and not `4/5`), and the phase is the FIRST one with unfinished work — not the last
    /// delivered one, which is a different question the review anchor asks.
    #[test]
    fn spec_progress_counts_the_plan_and_names_the_phase_the_work_is_on() {
        let plan = r#"[{"number":1,"name":"Read model"},{"number":2,"name":"Recap blocks"}]"#;
        let rows = |entries: Vec<(&str, Option<i32>, &str)>| -> Vec<db::HomeSpecTaskRow> {
            entries
                .into_iter()
                .map(|(spec_id, phase, status)| db::HomeSpecTaskRow {
                    spec_id: spec_id.to_string(),
                    phase,
                    status: status.to_string(),
                })
                .collect()
        };
        // The caller groups by Spec before folding; the test does the same so each case reads the
        // slice the production path would hand it. A `fn` and not a closure: the borrow of the
        // rows has to outlive the argument, which closure inference will not tie on its own.
        fn only<'a>(
            rows: &'a [db::HomeSpecTaskRow],
            spec_id: &str,
        ) -> Vec<&'a db::HomeSpecTaskRow> {
            rows.iter().filter(|r| r.spec_id == spec_id).collect()
        }

        // Phase 1 delivered, phase 2 open: the card is ON phase 2, and only THIS Spec's rows count
        // (the neighbour's are in the same bounded read).
        let tasks = rows(vec![
            ("s1", Some(1), "done"),
            ("s1", Some(1), "done"),
            ("s1", Some(2), "in_progress"),
            ("s1", Some(2), "pending"),
            ("s2", Some(1), "pending"),
        ]);
        let p = spec_progress(plan, &only(&tasks, "s1"));
        assert_eq!((p.done, p.total), (2, 4));
        let phase = p.phase.expect("a plan and open work name a phase");
        assert_eq!(
            (phase.number, phase.total, phase.name.as_str()),
            (2, 2, "Recap blocks")
        );

        // A CANCELLED task leaves the denominator entirely.
        let cancelled = rows(vec![
            ("s1", Some(1), "done"),
            ("s1", Some(1), "cancelled"),
            ("s1", Some(2), "done"),
        ]);
        let p = spec_progress(plan, &only(&cancelled, "s1"));
        assert_eq!(
            (p.done, p.total),
            (2, 2),
            "a task taken back is not work owed"
        );
        // Everything delivered: the card legends the LAST phase rather than falling silent.
        assert_eq!(p.phase.expect("still a plan").number, 2);

        // NO PLAN — an absence, not a hole: the card simply says nothing about phases. A
        // MALFORMED plan degrades to the same absence and never to an error.
        for plan in ["", "[]", "{not json"] {
            let flat = rows(vec![("s1", None, "pending")]);
            let p = spec_progress(plan, &only(&flat, "s1"));
            assert_eq!(p.phase, None, "plan {plan:?} names no phase");
            assert_eq!((p.done, p.total), (0, 1));
        }

        // OPEN WORK ON A PHASE THE PLAN DOES NOT DESCRIBE — a truncated plan, an imported task, a
        // phase dropped from the plan. The card must NOT fall back to the last phase: that legend
        // is reserved for "nothing left open", and using it here would read as delivered while
        // work is owed. No honest legend exists, so the card says nothing about phases.
        let orphan = rows(vec![("s1", Some(1), "done"), ("s1", Some(7), "pending")]);
        let p = spec_progress(plan, &only(&orphan, "s1"));
        assert_eq!((p.done, p.total), (1, 2));
        assert_eq!(
            p.phase, None,
            "an open phase absent from the plan names no phase — never the last one"
        );
    }
}
