//! RUNTIME (in-memory) AGENT ACTIVITY — the live "what is Claude doing right now"
//! signal that drives a terminal's dot when it hosts an agent session, replacing the
//! PTY `busy` bit for those terminals (the feature).
//!
//! # Why this is RUNTIME-ONLY (the anti-phantom contract)
//!
//! Activity is EPHEMERAL by design. "Working" is the window between a Claude
//! `UserPromptSubmit` and the matching `Stop` — it has no meaning across a restart: if
//! nyx (or Claude) dies mid-turn there is no live process to "still be working", so a
//! persisted `working` would be a PHANTOM dot stuck forever. The store therefore lives
//! ONLY in memory (this map, held by an `Arc` in the host): it is EMPTY at boot, so
//! every session — including a `resume` candidate revived from the DB — starts `Idle`
//! and only goes `Working` on a NEW `UserPromptSubmit`. There is no DB column, no
//! migration, nothing to survive a restart. This is the same reflex the PTY `busy`
//! signal already uses (derived live from the OS, never persisted): the dot can never
//! be "stuck".
//!
//! # The `in_flight` tool counter (why a long tool NEVER drops the dot, with 0 timer)
//!
//! The PRINCIPLE: **as long as `in_flight > 0` the terminal is `working`**, whatever the
//! wall-clock duration. Each `PreToolUse` increments `in_flight`, the matching
//! `PostToolUse`/`PostToolUseFailure` decrements it (saturating at 0). A tool that runs for
//! 10 minutes fires exactly one `PreToolUse` and (eventually) one `PostToolUse` — between
//! them `in_flight == 1`, so the dot stays `working` the WHOLE time with NO time-based
//! staleness. This is what fixes the old "stuck running if `Stop` never arrives" + "no
//! signal during a long tool" pain WITHOUT re-introducing a timer (the 0-stale decision,
//! below). When the last tool returns (`in_flight == 0`) the turn is NOT over: Claude is
//! still generating the answer text, so the dot stays `working` until the real
//! `Stop`/`StopFailure`.
//!
//! # The SEPARATE `subagents_in_flight` counter (the background-subagent fix, #21)
//!
//! Sub-agents are counted in a DEDICATED counter, NOT in the tool `in_flight`, because a
//! BACKGROUND sub-agent OUTLIVES the main turn. The hook order for a background sub-agent is
//! `SubagentStart` → **`Stop`** (the main turn finishes while the background keeps running)
//! → … → `SubagentStop` (the background actually finishes). A `Stop` that blindly reset
//! everything + raised the green "ready" dot was therefore PREMATURELY green while a
//! sub-agent was still working (the bug). The fix:
//!   * `Stop`/`StopFailure`/`idle_prompt` reset only the TOOL bookkeeping (`in_flight`,
//!     `ask_in_flight`) — NEVER `subagents_in_flight`;
//!   * if a sub-agent is still in flight at the `Stop`, the dot STAYS `working` (the
//!     resolve override keeps it blue) and the "ready" green is DEFERRED via a
//!     `ready_pending` flag instead of raised;
//!   * the LAST `SubagentStop` (counter → 0) is what finally settles the turn: if a `Stop`
//!     was deferred (`ready_pending`) it now goes `Idle` + raises the green; otherwise (a
//!     SYNCHRONOUS sub-agent, whose `SubagentStop` arrives BEFORE the `Stop`) it just
//!     decrements and the still-pending `Stop` settles the turn normally.
//!
//! A synchronous sub-agent (`SubagentStart` → `SubagentStop` → `Stop`) is unaffected: its
//! counter is already 0 at the `Stop`, so the `Stop` idles + greens immediately as before.
//!
//! # No TIME-BASED staleness (the 0-stale decision)
//!
//! There is deliberately NO temporal staleness on a `Working`/`Waiting` entry: a turn
//! stays running until a real terminating signal arrives, however long it takes. The
//! earlier lazy-expiry guard (a `Working` older than N minutes read back `Idle`) was
//! REMOVED: it made the running dot "jump"/disappear on a genuinely long turn (deep
//! research, large edits, a slow tool) — a wrong claim ("Claude stopped") on a session
//! that is still very much working. We instead TRUST the plugin's per-turn hooks (now
//! including `Pre`/`PostToolUse`, which bound even a long single tool) and bound the dot
//! ONLY by hard structural clears.
//!
//! ## The one ACCEPTED residual hole (documented, NOT fixed with a timer)
//!
//! A "bare" interrupt — the user hits `Esc` to abort the turn — fires NO hook at all
//! (Claude emits no `Stop`, no `PostToolUse`). So the dot can stay `working`/`waiting`
//! until the NEXT `UserPromptSubmit`, which resets `in_flight = 0` and re-enters
//! `Working` cleanly. This is the SOLE gap and it self-heals on the next prompt; the
//! decision (per the spec) is to accept it rather than add staleness that would make a
//! genuinely long turn flicker.
//!
//! Two clears back the anti-phantom contract up (each is exercised by a test):
//!   1. **Boot** — the map is empty (no construction reads the DB), so all sessions are
//!      `Idle` at launch. A resumed session shows no dot until its next prompt.
//!   2. **PTY death / SessionEnd / terminal close** — the host calls [`AgentActivityStore::clear`]
//!      for the terminal, dropping the entry to `Idle` (the agent-activity analogue of
//!      the "emit busy=false on PTY death" reflex). A killed Claude that never sent
//!      `Stop` cannot leave a `working` dot behind.
//!
//! # The `waiting` (yellow) state — PRECISE attention, not raw Notification
//!
//! `Waiting` means Claude is BLOCKED on the user. It is raised by two precise signals,
//! not by every `Notification`:
//!   * a `PreToolUse` for the `AskUserQuestion` tool (Claude asks the user a question and
//!     blocks on the answer) — lifted by that tool's `PostToolUse`/`PostToolUseFailure`;
//!   * a `Notification` whose `notification_type == "permission_prompt"` (a permission
//!     gate) — and, optionally, `elicitation_dialog`.
//! Other notifications (e.g. `idle_prompt`, which means the turn ended and Claude awaits a
//! prompt) do NOT mean "blocked": `idle_prompt` is treated as a turn end (`Idle` + ready).
//!
//! ## Defensively clearing a stale `Waiting` (#26)
//!
//! Besides the precise lifts above, ANY signal that the agent is demonstrably working again
//! lifts `Waiting → Working` — a tool starting/returning (`ToolStarted`/`ToolFinished`), a
//! sub-agent starting (`SubagentStarted`), or a new prompt. This is the fix for the
//! "chat about this" decline on an `AskUserQuestion`: declining the structured question
//! RESUMES work via a hook whose EXACT identity is empirically uncertain (it may not be the
//! question's paired `PostToolUse`), so relying solely on `AskFinished` could leave the dot
//! stuck yellow while Claude is clearly running. Routing every working signal through
//! [`Entry::clear_waiting_if_working`] makes the yellow self-heal on the first real work
//! event. A `Notification` (the *entry* into `Waiting`) and a `Stop`/`idle_prompt` (the turn
//! end) are NOT working signals and never call it, so entering and idling are unchanged.
//!
//! # The "ready" notification (the green dot) is FOCUS-AWARE, like exec_state_unread
//!
//! When Claude finishes a turn (`Stop`) the terminal carries a "response ready"
//! notification — the SAME semantics as the settled `exec_state_unread` flag: it is a
//! notification for a terminal you are NOT looking at, so it must clear the moment the
//! user views/focuses that terminal. The store records the unread bit ([`ActivitySnapshot::ready_unread`]);
//! the front clears it on focus exactly as it clears the settled badge (the existing
//! `mark-read` mechanic). The store also clears it itself whenever activity restarts
//! (a new `Working`) or is cleared (PTY death), so a stale "ready" never lingers.
//!
//! # Why a separate channel from `agent_sessions`
//!
//! The DB `agent_sessions` row is the AUTHORITY for "this terminal hosts a session of
//! kind X" (the icon, the resume candidate, the close warning) and MUST persist. The
//! activity is the opposite — transient, never persisted. Keeping them in separate
//! structures keeps the persisted/ephemeral split clean (AGENTS.md §0: "distingue les
//! couches"). They are surfaced to the front together (one `agent-sessions` change
//! topic, one read), but only the SESSION half touches SQLite.

use std::collections::HashMap;
use std::sync::Mutex;

/// The tool name Claude uses to ask the user a question and block on the answer. A
/// `PreToolUse` for it means "blocked on the user" → `Waiting` (yellow), lifted by the
/// matching `PostToolUse`/`PostToolUseFailure`.
///
/// NOTE (to validate empirically in the GUI): that `AskUserQuestion` flows through
/// `PreToolUse`/`PostToolUse` like any other tool is PROBABLE but UNCONFIRMED. The code
/// matches it defensively on the tool name; if Claude ever names it differently the worst
/// case is that the dot stays blue (`working`) instead of yellow — never stuck, since the
/// turn's `Stop` still clears everything.
pub const ASK_USER_QUESTION_TOOL: &str = "AskUserQuestion";

/// `notification_type` values that mean "Claude is BLOCKED on the user" → `Waiting`.
/// `permission_prompt` is the confirmed permission gate; `elicitation_dialog` is the
/// MCP elicitation prompt (also a block). Matched defensively — an unknown value is a
/// no-op (it does not move the dot), never an error.
const WAITING_NOTIFICATION_TYPES: &[&str] = &["permission_prompt", "elicitation_dialog"];

/// The `notification_type` that means the turn ENDED and Claude awaits the next prompt
/// (the green "ready" + idle state), the same effect as a `Stop`.
const IDLE_NOTIFICATION_TYPE: &str = "idle_prompt";

/// How long a turn nyx JUST SUBMITTED counts as `Working` for a bounded wait, while the agent's
/// own `UserPromptSubmit` hook is still on its way (see
/// [`AgentActivityStore::note_prompt_submitted`]).
///
/// It bounds ONE latency: a Claude Code hook is a `node` process that has to start and POST back to
/// nyx. Cold, on Windows, that is the better part of a second, and a loaded machine makes it worse
/// — so a window measured in hundreds of milliseconds would put the race straight back. Four
/// seconds is the far side of that latency with margin, and it is a CEILING, not a delay: the first
/// hook event cancels it ([`AgentActivityStore::apply`]), so the normal turn is armed for as long
/// as the hook took and no longer. The only call that pays the full window is one where no hook
/// ever comes — an `enter` that submitted nothing — and paying four seconds once to find that out
/// is cheaper than handing an agent a `resolved: true` that is not true.
const SUBMISSION_ARMING_WINDOW: std::time::Duration = std::time::Duration::from_millis(4_000);

/// How long, after nyx wrote an `enter`, a standing `Waiting` keeps reading `Working` for a bounded
/// wait ([`Entry::resolve_after_submission`]) when no hook has landed. It must cover one FULL
/// `wait_for_terminal` at its maximum timeout (60 s) so the first wait after an approval never
/// regresses to the yellow it just answered; past it, an `enter` that answered nothing (a dialog left
/// open, an empty field) stops being hidden from every wait that follows.
const SUBMISSION_LIFT_WINDOW: std::time::Duration = std::time::Duration::from_millis(90_000);

/// `notification_type` values that mean a BACKGROUND sub-agent COMPLETED — treated exactly
/// like a `SubagentStop` (decrement the sub-agent counter, possibly settling a deferred
/// turn-end). See the #21 deferral.
///
/// ## UNCERTAINTY (to validate empirically in the GUI — see module note on #21)
///
/// Claude Code's release notes mention "background subagent completion notifications", but
/// it is NOT confirmed whether a background sub-agent's END reaches us as a `SubagentStop`
/// hook (the PRIMARY path this fix relies on) OR ONLY as a `Notification` of one of these
/// types. We match BOTH defensively so the sub-agent counter is decremented whichever hook
/// fires:
///   * if `SubagentStop` DOES fire for backgrounds → these notification types are a
///     harmless extra signal (a stray decrement saturates at 0; a no-op if already settled);
///   * if ONLY a `Notification` fires → THIS is what lowers the counter and lifts the
///     deferred green.
///
/// The exact string(s) are a BEST GUESS (Claude does not document them); an UNKNOWN
/// notification type stays a no-op (it does not move the dot), so a wrong guess here is
/// SAFE on the upside but leaves the WORST CASE intact: if the real completion hook is
/// NEITHER `SubagentStop` NOR a string we match, a deferred background turn would keep the
/// dot BLUE until the next `UserPromptSubmit` (which resets everything) — never a wrong
/// green, but a possibly-stuck blue. THIS IS THE ONE THING TO VALIDATE IN THE GUI.
const SUBAGENT_DONE_NOTIFICATION_TYPES: &[&str] = &[
    "background_subagent_complete",
    "subagent_complete",
    "agent_complete",
];

/// The live activity KIND of ONE agent-hosting terminal — the resolved dot color. There
/// is NO `since` stamp and NO time-based expiry (the 0-stale decision): an entry holds
/// its kind until a NEW event or a hard [`AgentActivityStore::clear`] (PTY death /
/// SessionEnd / close) changes it. There is NO persisted form (the enum is never
/// serialized to SQLite).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// The agent is between turns (no live work). The dot follows the SESSION state
    /// only (the icon), not a running indicator. A `ready_unread` may still be set —
    /// it is the "response ready" notification, orthogonal to `Idle` vs `Working`.
    Idle,
    /// The agent is working on the current turn (between `UserPromptSubmit` and `Stop`,
    /// OR while at least one tool/subagent is in flight). The blue dot (`--info`).
    Working,
    /// The agent is BLOCKED waiting on the user — an `AskUserQuestion` tool in flight or
    /// a permission/elicitation `Notification`. The yellow dot (`--warning`). Distinct
    /// from `Working` so the UI shows an "attention" affordance rather than a "busy" one.
    Waiting,
}

impl Activity {
    /// The lowercase wire spelling — `idle` | `working` | `waiting`.
    ///
    /// ONE vocabulary for every surface that reads this store: the renderer's dot (through the
    /// sidecar db-command), the MCP `list_terminals` / `read_terminal` / `wait_for_terminal`
    /// answers, and the `until` set a caller waits on. Written here rather than at each call site
    /// precisely so an agent's `until: ["idle"]` and the UI's blue dot cannot drift apart.
    pub fn as_str(self) -> &'static str {
        match self {
            Activity::Idle => "idle",
            Activity::Working => "working",
            Activity::Waiting => "waiting",
        }
    }
}

/// The activity SNAPSHOT the host surfaces to the front for one terminal: the resolved
/// [`Activity`] kind plus the focus-aware "response ready" unread bit. The `in_flight`
/// counter is an internal implementation detail and is NOT surfaced.
///
/// It also carries the SECONDARY INFORMATION — the short sentence that says what the agent is
/// asking or doing ([`Self::waiting_on`], [`Self::tool`]). Those two are RAW FACTS, not a resolved
/// label: which of them (if either) a row shows is decided at display time by
/// [`crate::home::secondary_info`]. Like every other field here they are RUNTIME-ONLY — nothing
/// added here is persisted, and the store is empty at boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySnapshot {
    pub activity: Activity,
    /// `true` when a turn finished (`Stop`/`idle_prompt`) and the user has NOT yet viewed
    /// the terminal — the "response ready" green dot. Cleared on focus (front mark-read),
    /// on a new `Working`, and on [`AgentActivityStore::clear`].
    pub ready_unread: bool,
    /// The RED analogue of [`Self::ready_unread`] — set when the last turn ended on an API
    /// error (`StopFailure`) and the user has NOT yet viewed the terminal. Cleared on focus
    /// (mark-read), a new `Working`, and on [`AgentActivityStore::clear`].
    pub error_unread: bool,
    /// `true` when the nyx plugin THIS session loaded is OLDER/DIFFERENT than the version
    /// nyx bundles (#18b — the per-session "plugin périmé" badge). Set ONCE at SessionStart
    /// from the hook-reported `plugin_version`; runtime-only (never persisted). A session
    /// loads its hooks once at start, so the fix is to restart the session — the badge
    /// surfaces that invitation. Cleared on [`AgentActivityStore::clear`] (PTY death /
    /// SessionEnd / close), so a fresh (restarted) session starts NOT outdated.
    pub plugin_outdated: bool,
    /// WHAT the agent is blocked on, while it is: the `AskUserQuestion` question or the
    /// permission/elicitation message that raised the current [`Activity::Waiting`]. `None` when
    /// nothing is blocking, and ALSO `None` when the block exists but the hook payload carried no
    /// readable text — an unexpected shape degrades to "no info", never to an error and never to a
    /// placeholder sentence. Cleared the instant the `Waiting` lifts, including through the
    /// defensive [`Entry::clear_waiting_if_working`] path: an answered question must not survive
    /// as a caption on a row that is demonstrably working again.
    pub waiting_on: Option<String>,
    /// The name of the tool currently IN FLIGHT — the ONLY fallback for the secondary
    /// information, read when the agent is not blocked. `None` when no tool is running, or when
    /// the `PreToolUse` payload carried no `tool_name`. Cleared when the last tool returns and at
    /// every turn end, so it never captions an agent that has stopped doing that thing.
    pub tool: Option<String>,
}

/// One stored entry — the full per-terminal runtime state machine.
///
/// `in_flight` is the count of TOOLS currently running for this terminal: a `PreToolUse`
/// increments it, the matching `PostToolUse` decrements it. While `in_flight > 0` the
/// terminal is `Working` (see [`Entry::resolve`]), so a long-running tool NEVER drops the
/// dot with NO timer. `ask_in_flight` is whether an `AskUserQuestion` tool is the in-flight
/// tool driving the explicit `Waiting`; it is tracked separately so its `PostToolUse` lifts
/// the yellow `Waiting` back to `Working` while a *permission* `Notification` `Waiting`
/// (which has no paired tool event) is lifted only by the turn ending.
///
/// `subagents_in_flight` is a SEPARATE counter for sub-agents (`SubagentStart` ++ /
/// `SubagentStop` --), kept apart from the tool counter because a BACKGROUND sub-agent
/// OUTLIVES the main turn's `Stop` (the #21 fix). A `Stop` resets the tool bookkeeping but
/// NEVER this counter; while `subagents_in_flight > 0` the dot stays `Working`.
#[derive(Debug, Clone)]
struct Entry {
    /// The "base" activity the explicit events set, BEFORE the in-flight override.
    /// `Waiting` here means a permission/elicitation block OR an `AskUserQuestion` block;
    /// `Idle` means the turn ended; `Working` is the normal mid-turn base.
    base: Activity,
    /// Count of in-flight TOOLS (not sub-agents). While `> 0` the resolved activity is
    /// `Working` (unless `base == Waiting`, which takes precedence — an attention block
    /// outranks a busy tool). Saturating at 0 on decrement (a stray `Post` without a `Pre`
    /// is safe).
    in_flight: u32,
    /// Count of in-flight SUB-AGENTS, tracked separately from tools so a background
    /// sub-agent that outlives the main turn keeps the dot `Working` past the `Stop` (#21).
    /// While `> 0` the resolved activity is forced `Working` like `in_flight`. Saturating
    /// at 0 on decrement. NEVER reset by a `Stop` — only its own `SubagentStop` lowers it.
    subagents_in_flight: u32,
    /// `true` while an `AskUserQuestion` tool is the in-flight tool that raised `Waiting`.
    /// Its `PostToolUse`/`PostToolUseFailure` lifts the `Waiting` back to `Working`.
    ask_in_flight: bool,
    /// `true` when a `Stop` arrived while sub-agents were still in flight: the turn-end was
    /// DEFERRED (no green yet) and will be applied when the LAST sub-agent finishes. This is
    /// what keeps the dot blue between the `Stop` and the trailing `SubagentStop` (#21).
    ready_pending: bool,
    /// Records, for a DEFERRED turn-end (a sub-agent still in flight), whether the deferred
    /// end was an ERROR (`StopFailure`) so the trailing `SubagentStop` raises the RED
    /// `error_unread` instead of the green `ready_unread`. Only meaningful while
    /// `ready_pending` is set.
    pending_is_error: bool,
    ready_unread: bool,
    /// The RED analogue of `ready_unread` — set when the last turn ended on an API error
    /// (`StopFailure`) and the user has not yet viewed the terminal.
    error_unread: bool,
    /// `true` when the plugin THIS session loaded is stale vs. the bundled version (#18b).
    /// Orthogonal to the activity state machine: set ONCE at SessionStart and never touched
    /// by the per-turn events — only [`AgentActivityStore::set_plugin_outdated`] writes it and
    /// [`AgentActivityStore::clear`] drops it. So a working/idle transition never clears the
    /// badge, and a SessionEnd/restart (which clears the entry) does.
    plugin_outdated: bool,
    /// The question / permission text that raised the current `Waiting` (see
    /// [`ActivitySnapshot::waiting_on`]). It lives and dies with the `Waiting` it explains.
    waiting_on: Option<String>,
    /// The name of the last tool that STARTED and has not returned — the in-flight tool the
    /// secondary information falls back on (see [`ActivitySnapshot::tool`]). Tracked as ONE name
    /// rather than a stack: with several tools in flight the last one started is the one worth
    /// naming, and a `PostToolUse` cannot say WHICH tool returned, so the name is dropped when the
    /// counter reaches 0 rather than pretending to pair Posts with Pres.
    tool: Option<String>,
    /// The instant nyx ITSELF submitted a turn into this terminal (an `enter` written by
    /// `send_keys` / `send_to_terminal` while an agent session was active), and no hook has
    /// landed since. See [`AgentActivityStore::note_prompt_submitted`] for what it is for; it is
    /// read ONLY by [`AgentActivityStore::wait_activity`] and NEVER by [`Entry::resolve`], so the
    /// dot is untouched by it.
    submitted_at: Option<std::time::Instant>,
}

impl Entry {
    /// The ZERO entry — `Idle`, nothing in flight, nothing unread, no text. What a terminal the
    /// store has never heard of resolves to, and the base the other constructors spread from.
    fn empty() -> Self {
        Self {
            base: Activity::Idle,
            in_flight: 0,
            subagents_in_flight: 0,
            ask_in_flight: false,
            ready_pending: false,
            pending_is_error: false,
            ready_unread: false,
            error_unread: false,
            plugin_outdated: false,
            waiting_on: None,
            tool: None,
            submitted_at: None,
        }
    }

    /// A fresh entry for a brand-new turn (`UserPromptSubmit`): `Working`, no tools/
    /// sub-agents in flight, no pending question, no deferred turn-end, ready cleared (a new
    /// turn supersedes a prior answer). `plugin_outdated` is CARRIED OVER from the prior
    /// entry — a new turn does NOT restart the session, so the stale-plugin verdict (set once
    /// at SessionStart) persists until the session actually restarts (which clears the entry).
    /// The secondary-information texts are NOT carried over: a new turn is neither asking the old
    /// question nor running the old tool.
    fn new_turn(plugin_outdated: bool) -> Self {
        Self {
            base: Activity::Working,
            plugin_outdated,
            ..Self::empty()
        }
    }

    /// The RESOLVED activity the front sees, applying the in-flight override on top of the
    /// base: an attention block (`Waiting`) ALWAYS wins (the user must act); otherwise any
    /// in-flight tool OR sub-agent forces `Working`; otherwise the base (`Working` mid-turn
    /// while Claude generates text, or `Idle` between turns) shows through. This is the
    /// single place the "tool/subagent in flight ⇒ working, 0-stale" principle lives.
    fn resolve(&self) -> Activity {
        match self.base {
            Activity::Waiting => Activity::Waiting,
            _ if self.in_flight > 0 || self.subagents_in_flight > 0 => Activity::Working,
            other => other,
        }
    }

    fn snapshot(&self) -> ActivitySnapshot {
        ActivitySnapshot {
            activity: self.resolve(),
            ready_unread: self.ready_unread,
            error_unread: self.error_unread,
            plugin_outdated: self.plugin_outdated,
            waiting_on: self.waiting_on.clone(),
            tool: self.tool.clone(),
        }
    }

    /// DEFENSIVE clear of a stale `Waiting` (yellow) when the agent is demonstrably working
    /// again (#26). A `Waiting` is entered by an `AskUserQuestion` block or a permission/
    /// elicitation `Notification`; it is normally lifted by the paired `AskFinished` or by the
    /// turn ending. But a "chat about this" decline on an `AskUserQuestion` RESUMES work via a
    /// hook whose EXACT identity is empirically uncertain (it may not be the paired Post), so
    /// the `Waiting` could stay stuck yellow while Claude is clearly running. The robust rule:
    /// ANY signal that the agent is actively working again (a tool/sub-agent starting or
    /// returning) lifts `Waiting → Working`. Idempotent and a no-op unless `base == Waiting`.
    ///
    /// This does NOT touch `ask_in_flight` (its `PostToolUse` bookkeeping is handled by
    /// `AskFinished`) and does NOT touch a finished turn: only the explicit working signals
    /// call it, so `Notification → Waiting`, `Stop → Idle`, and the deferred-background (#21)
    /// paths are untouched.
    ///
    /// It DOES drop `waiting_on`: the question/permission text explains the block, so the moment
    /// the block lifts the text is wrong. Clearing it HERE (and not only on the precise lifts) is
    /// what stops a resolved question from surviving as a caption on a row that is working again —
    /// the same reason this defensive path exists for the dot.
    fn clear_waiting_if_working(&mut self) {
        if self.base == Activity::Waiting {
            self.base = Activity::Working;
            self.waiting_on = None;
        }
    }

    /// Drop the in-flight tool NAME once nothing is in flight any more. A `PostToolUse` does not
    /// say which tool returned, so the name is only trustworthy while the counter is non-zero;
    /// past that, keeping it would caption a row with a tool that has finished. With tools still
    /// running the last-started name stays, which is the one worth showing.
    fn forget_tool_when_idle(&mut self) {
        if self.in_flight == 0 {
            self.tool = None;
        }
    }

    /// [`Self::resolve`] as a BOUNDED WAIT must read it once nyx has SUBMITTED a turn into this
    /// terminal and no hook has landed since — the only difference being that a standing `Waiting`
    /// is read as `Working`.
    ///
    /// # Why the `Waiting` is stale by construction here (#K5)
    ///
    /// A `Waiting` is entered by an `AskUserQuestion` or by a permission/elicitation
    /// `Notification`: the agent is blocked ON THE USER. When nyx then writes an `enter` into that
    /// terminal, nyx IS that user, and the block it was waiting on has just been answered — the
    /// approval. No hook says so: the plugin registers no `PermissionRequest`, `PreToolUse` fires
    /// BEFORE the dialog, and the approved tool's `PostToolUse` only lands when the tool ENDS. So
    /// between the approval and that end — minutes, for a long tool — the store legitimately still
    /// holds the `Waiting` of a block nobody is blocked on any more.
    ///
    /// Reading it verbatim is what made `wait_for_terminal` answer `resolved: true,
    /// agent_activity: "waiting"` right after an approval: the default `until` (idle+waiting)
    /// matched, the pilot concluded the agent was blocked, and sent ANOTHER `enter` — this one into
    /// the input box of a session that was working, corrupting it. The lift is placed HERE, at the
    /// same mark as the submission, because nyx is the only party that knows: it is the one that
    /// answered.
    ///
    /// It is bounded twice. First by the SAME thing the arming mark is bounded by — any hook event
    /// clears `submitted_at` ([`AgentActivityStore::apply`]), so the lift dies with the first thing
    /// the agent says about itself. Second by [`SUBMISSION_LIFT_WINDOW`]
    /// ([`AgentActivityStore::wait_activity`]): an `enter` that answered NOTHING — a dialog left
    /// open, an empty free-text field — produces no hook at all, and without a ceiling the lift
    /// would hide a real block from every wait that follows, forever. Past the ceiling the wait
    /// reads the truth again; its worst case is one late `waiting` on an approved tool that ran
    /// longer than the ceiling.
    ///
    /// And it does NOT apply while an `AskUserQuestion` is in flight (`ask_in_flight`): that tool
    /// asks several questions in ONE call and ends with ONE `PostToolUse`, so an `enter` nyx wrote
    /// on question 1 did not answer the block — question 2 is now waiting, and no hook will say so.
    ///
    /// It is read ONLY by [`AgentActivityStore::wait_activity`]: the dot ([`Self::snapshot`]) keeps
    /// showing the yellow the hooks asserted, untouched.
    fn resolve_after_submission(&self) -> Activity {
        if self.ask_in_flight {
            return self.resolve();
        }
        match self.resolve() {
            Activity::Waiting => Activity::Working,
            other => other,
        }
    }
}

/// What a BOUNDED WAIT reads on a terminal — the answer [`AgentActivityStore::wait_activity`]
/// gives, and the state the `wait_for_terminal` poll loop compares against its `until` set.
///
/// # Why this is NOT just an [`Activity`] (#K3)
///
/// The submission arming window used to be expressed by RETURNING `Working`, which put a
/// SUPPOSITION on the same footing as a hook-asserted fact: `until: ["working"]` was satisfied
/// instantly by nyx's own guess, `waited_ms: 0`, on a terminal the store still read `idle` — and
/// `list_terminals`, reading the real snapshot, contradicted the very same field name for the four
/// seconds that followed.
///
/// The window's job is to stop the wait resolving on the state from BEFORE the caller's own
/// submission. That is `Unconfirmed`: a value no `until` set can contain, so the wait keeps
/// waiting whatever it was told to wait for, and no target is ever satisfied by a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitActivity {
    /// nyx has submitted a turn into this terminal and no hook has confirmed it yet — the store's
    /// value describes the state BEFORE that submission and must not resolve anything. Never a
    /// member of an `until` set, by construction.
    Unconfirmed,
    /// The activity as the hooks left it (with the post-submission `Waiting` lift of
    /// [`Entry::resolve_after_submission`] applied once the arming window has lapsed).
    Observed(Activity),
}

/// The normalized activity event parsed from a Claude hook payload. Distinct from the
/// SESSION-lifecycle [`crate::agent::AgentEvent`] (Start/End): these are the per-TURN
/// hooks that drive the live dot, NOT the persisted session row.
///
/// Several events carry a discriminator read from the hook payload (the tool name, the
/// notification type) so the same `hook_event_name` can mean different things — the core
/// reads `args.tool_name` / `args.notification_type` and builds the right variant.
/// The FACTS a hook payload can contribute beyond its name — the discriminators that make one
/// `hook_event_name` mean different things, PLUS the two texts the secondary information is built
/// from. Every field is optional and every field is read DEFENSIVELY: the `.cjs` hook forwards
/// Claude's stdin verbatim, so what arrives is whatever Claude sent, and a shape we did not expect
/// must degrade to "no info" rather than fail a hook.
#[derive(Debug, Default, Clone, Copy)]
pub struct HookFacts<'a> {
    /// `tool_name` on a `PreToolUse`/`PostToolUse` — the discriminator that separates an ordinary
    /// tool from the `AskUserQuestion` block, and the name shown as the in-flight tool.
    pub tool_name: Option<&'a str>,
    /// `tool_input` on a `PreToolUse` — read ONLY for `AskUserQuestion`, to pull the question out
    /// (see [`ask_question_text`]).
    pub tool_input: Option<&'a serde_json::Value>,
    /// `notification_type` on a `Notification` — permission/elicitation (a block), `idle_prompt`
    /// (a turn end), a background-sub-agent completion, or something we ignore.
    pub notification_type: Option<&'a str>,
    /// `message` on a `Notification` — Claude's own human-readable sentence for the prompt it is
    /// blocked on ("Claude needs your permission to use Bash"). Used verbatim as the permission
    /// text; absent ⇒ no text, and the dot still turns yellow.
    pub message: Option<&'a str>,
}

/// The `tool_input` field names an `AskUserQuestion` payload may carry the question under, in the
/// order they are tried. NAMED here rather than pattern-matched inline so the shapes nyx claims to
/// understand are readable in one place — and so the note on [`ASK_USER_QUESTION_TOOL`] (the tool's
/// payload shape is PROBABLE but UNCONFIRMED) has somewhere concrete to point.
///
/// Two shapes are covered: the structured one, `{ questions: [{ question, header, … }] }` (the
/// first entry wins — a row shows one line, not a form), and the flat one, `{ question }` /
/// `{ prompt }`. ANYTHING else — a different key, a nested object, a number, no `tool_input` at
/// all — yields `None`, which is "no info": the row shows the yellow dot and no caption.
const ASK_QUESTION_FIELDS: &[&str] = &["question", "header", "prompt"];

/// Pull the QUESTION out of an `AskUserQuestion` `tool_input`, or `None` when the payload does not
/// have a shape we recognise. Total and infallible by construction — there is no error branch,
/// because a hook must never fail over a caption.
pub fn ask_question_text(tool_input: &serde_json::Value) -> Option<String> {
    /// The first non-blank string among [`ASK_QUESTION_FIELDS`] on one JSON object.
    fn field_of(value: &serde_json::Value) -> Option<String> {
        // The blank test lives INSIDE the search, not after it: a payload that carries an EMPTY
        // `question` alongside a real `header` must fall through to the header. Filtering after
        // `find_map` would stop on the first field that merely EXISTS and then throw it away,
        // which reads as "no question" while the words were right there in the next field.
        ASK_QUESTION_FIELDS.iter().find_map(|key| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    }
    // The STRUCTURED shape first — `questions` is a list and a row shows ONE line, so the first
    // entry is the question. Then the FLAT shape, read off the payload itself.
    tool_input
        .get("questions")
        .and_then(|q| q.as_array())
        .and_then(|items| items.first())
        .and_then(field_of)
        .or_else(|| field_of(tool_input))
}

/// Normalise a hook-supplied text to what is worth storing: trimmed, and `None` when blank. Keeps
/// "the payload carried nothing" and "the payload carried whitespace" the same state, so a caption
/// is either real words or absent.
fn hook_text(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityEvent {
    /// `UserPromptSubmit` — the user sent a prompt; a NEW turn begins → `Working`,
    /// `in_flight` reset to 0, ready cleared.
    PromptSubmitted,
    /// `PreToolUse` for an ordinary tool — one more tool in flight (`in_flight += 1`),
    /// the dot stays/goes `Working`. Carries the tool's NAME when the payload had one: it is the
    /// fallback secondary information, shown when the agent is not blocked on anything.
    ToolStarted { tool: Option<String> },
    /// `PreToolUse` for `AskUserQuestion` — Claude asks the user and blocks → `Waiting`,
    /// remembering the question is in flight so the paired `PostToolUse` lifts it. Carries the
    /// QUESTION when the payload had a shape we recognise (see [`ask_question_text`]); `None`
    /// still blocks, it just says nothing about what is being asked.
    AskStarted { question: Option<String> },
    /// `PostToolUse`/`PostToolUseFailure` for an ordinary tool — one fewer in flight
    /// (`in_flight = in_flight.saturating_sub(1)`); if that was the last tool the dot
    /// stays `Working` (Claude is generating text) until `Stop`.
    ToolFinished,
    /// `PostToolUse`/`PostToolUseFailure` for `AskUserQuestion` — the user answered → lift
    /// the `Waiting` back to `Working`.
    AskFinished,
    /// `SubagentStart` — a sub-agent started; one more sub-agent in flight (`Working`),
    /// counted in the DEDICATED `subagents_in_flight` (not the tool counter) so a background
    /// sub-agent survives the main turn's `Stop`.
    SubagentStarted,
    /// `SubagentStop` — a sub-agent finished; one fewer sub-agent in flight. This does NOT
    /// end the MAIN turn (the explicit guard). If it is the LAST sub-agent AND a `Stop` was
    /// deferred (`ready_pending`, the background case), it now applies the deferred turn-end
    /// → `Idle` + green; otherwise it only decrements.
    SubagentFinished,
    /// `Notification(permission_prompt|elicitation_dialog)` — Claude is blocked on the
    /// user → `Waiting`. Carries the notification's `message` (Claude's own sentence for the
    /// prompt) when it had one; `None` blocks just the same, with no caption.
    AttentionNeeded { permission: Option<String> },
    /// `Stop`/`StopFailure`, or `Notification(idle_prompt)` — the main turn finished. Resets
    /// the TOOL bookkeeping (`in_flight`, `ask_in_flight`) and, IF no sub-agent is still in
    /// flight, goes `Idle` + raises the "response ready" notification. If a sub-agent is
    /// still running (a BACKGROUND sub-agent that outlives the turn) the turn-end is
    /// DEFERRED: the dot stays `Working` and the green is raised only when the last
    /// `SubagentStop` lands (#21).
    TurnFinished,
    /// `StopFailure` — the main turn ended on an API error; same turn-end bookkeeping as
    /// `TurnFinished` but raises the RED `error_unread` notification instead of the green
    /// `ready_unread`.
    TurnFailed,
}

impl ActivityEvent {
    /// Map a Claude `hook_event_name` (+ the relevant payload discriminators) to an
    /// activity event, or `None` for a hook that does not affect the live dot.
    ///
    /// `tool_name` is the `PreToolUse`/`PostToolUse` tool (so `AskUserQuestion` becomes a
    /// `Waiting` rather than a busy tool); `notification_type` is the `Notification` kind
    /// (so only a permission/elicitation block is `Waiting`, while `idle_prompt` is a turn
    /// end). Both are `Option` because not every caller has them; the matching is
    /// DEFENSIVE — an unrecognized tool is treated as an ordinary tool, an unrecognized
    /// notification is a no-op (`None`).
    ///
    /// `SessionStart`/`SessionEnd` are session lifecycle (handled elsewhere) → `None`.
    ///
    /// The texts ride the same way: the `AskUserQuestion` question is pulled from
    /// `facts.tool_input` and the permission sentence from `facts.message`. Both are BEST-EFFORT —
    /// a shape we did not expect produces an event with `None` text, never an error, so the dot
    /// moves exactly as it did before this existed and only the caption is missing.
    pub fn from_hook(name: &str, facts: HookFacts<'_>) -> Option<Self> {
        let HookFacts {
            tool_name,
            tool_input,
            notification_type,
            message,
        } = facts;
        match name {
            "UserPromptSubmit" => Some(ActivityEvent::PromptSubmitted),
            "PreToolUse" => {
                if tool_name == Some(ASK_USER_QUESTION_TOOL) {
                    Some(ActivityEvent::AskStarted {
                        question: tool_input.and_then(ask_question_text),
                    })
                } else {
                    Some(ActivityEvent::ToolStarted {
                        tool: hook_text(tool_name),
                    })
                }
            }
            "PostToolUse" | "PostToolUseFailure" => {
                if tool_name == Some(ASK_USER_QUESTION_TOOL) {
                    Some(ActivityEvent::AskFinished)
                } else {
                    Some(ActivityEvent::ToolFinished)
                }
            }
            "SubagentStart" => Some(ActivityEvent::SubagentStarted),
            "SubagentStop" => Some(ActivityEvent::SubagentFinished),
            "Notification" => match notification_type {
                Some(t) if WAITING_NOTIFICATION_TYPES.contains(&t) => {
                    Some(ActivityEvent::AttentionNeeded {
                        permission: hook_text(message),
                    })
                }
                Some(t) if t == IDLE_NOTIFICATION_TYPE => Some(ActivityEvent::TurnFinished),
                // A "background sub-agent completed" notification is treated like a
                // SubagentStop (decrement the sub-agent counter, settle a deferred turn-end
                // if it was the last). DEFENSIVE: matched on a best-guess string because it
                // is unconfirmed whether backgrounds end via SubagentStop or this — see
                // SUBAGENT_DONE_NOTIFICATION_TYPES.
                Some(t) if SUBAGENT_DONE_NOTIFICATION_TYPES.contains(&t) => {
                    Some(ActivityEvent::SubagentFinished)
                }
                // An unknown / absent notification type does not move the dot.
                _ => None,
            },
            "Stop" => Some(ActivityEvent::TurnFinished),
            "StopFailure" => Some(ActivityEvent::TurnFailed),
            // SessionStart/SessionEnd are session lifecycle; anything else is irrelevant.
            _ => None,
        }
    }

    /// BACK-COMPAT thin wrapper: map a hook name with NO payload facts at all. A
    /// `PreToolUse`/`PostToolUse` without a `tool_name` is an ordinary tool (with no name to
    /// show); a `Notification` without a type is a no-op. Used where only the name is available.
    pub fn from_hook_event_name(name: &str) -> Option<Self> {
        Self::from_hook(name, HookFacts::default())
    }
}

/// The RUNTIME agent-activity registry: an in-memory `terminal_id → Entry` map, EMPTY
/// at construction (the anti-phantom boot guarantee — nothing is read from the DB). The
/// host holds ONE behind an `Arc`; the MCP dispatcher WRITES it ([`Self::apply`]) when
/// a Claude activity hook arrives, the read path SNAPSHOTS it for the front
/// ([`Self::snapshot`] / [`Self::snapshot_all`]), and the PTY/lifecycle layer CLEARS it
/// ([`Self::clear`]) on PTY death / SessionEnd / close.
///
/// It holds the DOT and nothing else. The other reader of the same hook stream — the consent check,
/// which needs to know that a HUMAN spoke — is served by the PERSISTED ledger
/// ([`crate::db::HumanTurn`]) and not from here, and deliberately so: this store's whole contract is
/// that it dies with the session (a `working` dot must never outlive the process that could still be
/// working), and a human's turn must OUTLIVE it — a `claude -p` invocation is a whole session, so a
/// turn kept here would be gone before the answer to the question it authorises came back.
#[derive(Default)]
pub struct AgentActivityStore {
    map: Mutex<HashMap<String, Entry>>,
}

impl AgentActivityStore {
    /// A fresh, EMPTY store — the boot state (all terminals `Idle`, no phantom running).
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one activity event to `terminal_id`, returning the resolved snapshot AFTER
    /// the transition. The single WRITE entry point (the MCP `agent_session_event` tool
    /// calls this for the per-turn hooks). This is the whole state machine: it mutates
    /// `{ base, in_flight, ask_in_flight, ready_unread }` per the event, then resolves.
    pub fn apply(&self, terminal_id: &str, event: ActivityEvent) -> ActivitySnapshot {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        // Read-or-default the current entry (a missing terminal is Idle with no tools).
        let mut entry = map.get(terminal_id).cloned().unwrap_or_else(Entry::empty);

        match event {
            // A new turn starts fresh: Working, no tools, no question, ready cleared (the
            // old answer is stale). This also RESETS a `working`/`waiting` left dangling
            // by a bare Esc interrupt (the one accepted residual hole) — see module docs.
            // `plugin_outdated` is carried over (a new turn is not a session restart).
            ActivityEvent::PromptSubmitted => {
                entry = Entry::new_turn(entry.plugin_outdated);
            }
            // One more ordinary tool in flight → Working. A tool STARTING is a definitive
            // "agent is working again" signal, so it ALSO lifts a stale `Waiting` (#26): if
            // the user declined an AskUserQuestion ("chat about this") and Claude resumed by
            // running a tool, the yellow must not stay stuck.
            ActivityEvent::ToolStarted { tool } => {
                entry.in_flight = entry.in_flight.saturating_add(1);
                // The in-flight tool is the FALLBACK secondary information. The last tool to start
                // is the one named; a payload with no `tool_name` drops the caption rather than
                // keeping the previous tool's name, which would be a lie.
                entry.tool = tool;
                entry.clear_waiting_if_working();
                if entry.base == Activity::Idle {
                    // Work RESUMED after a turn-end (a tool started while the base was Idle — an
                    // agentic continuation, or a turn whose `UserPromptSubmit` was reordered behind
                    // this hook by the best-effort, UNORDERED hook-POST transport). Re-enter
                    // `Working` AND drop the now-stale turn-end notifications (#28): a fresh work
                    // signal supersedes the prior answer, exactly as `new_turn` does — so a
                    // resumed-and-working session reads a clean blue, never a stale green while it
                    // is demonstrably working again.
                    entry.base = Activity::Working;
                    entry.ready_unread = false;
                    entry.error_unread = false;
                }
            }
            // AskUserQuestion in flight → explicit Waiting (yellow), remember it so the
            // paired Post lifts it. It still counts as in-flight work.
            ActivityEvent::AskStarted { question } => {
                entry.in_flight = entry.in_flight.saturating_add(1);
                entry.ask_in_flight = true;
                entry.base = Activity::Waiting;
                // WHAT is being asked, when the payload said so. `None` is a legitimate outcome
                // (an unrecognised `tool_input` shape): the row still turns yellow, it just has
                // nothing to caption it with.
                entry.waiting_on = question;
            }
            // An ordinary tool returned: one fewer in flight. The base stays Working (if
            // it was) — when in_flight hits 0 the dot stays Working until Stop (Claude is
            // generating). A tool RETURNING is also a "working again" signal, so it lifts a
            // stale `Waiting` (#26): a "chat about this" decline that resumes work via a
            // tool's PostToolUse must not leave the yellow stuck.
            ActivityEvent::ToolFinished => {
                entry.in_flight = entry.in_flight.saturating_sub(1);
                entry.forget_tool_when_idle();
                entry.clear_waiting_if_working();
            }
            // The user answered (or declined — "chat about this") an AskUserQuestion: lift
            // the Waiting back to Working and decrement the in-flight count for that tool.
            // The lift is now UNCONDITIONAL on the resume signal (#26): a decline resumes work
            // and the yellow must clear even if `ask_in_flight` was somehow already dropped.
            ActivityEvent::AskFinished => {
                entry.in_flight = entry.in_flight.saturating_sub(1);
                entry.ask_in_flight = false;
                entry.forget_tool_when_idle();
                entry.clear_waiting_if_working();
                // The question is ANSWERED: drop its text even if the base was not `Waiting` any
                // more (a defensive lift already ran). `clear_waiting_if_working` only fires while
                // the base IS `Waiting`, so the caption needs its own unconditional clear here —
                // the same reason the yellow lift itself became unconditional (#26).
                entry.waiting_on = None;
            }
            // A sub-agent started → one more sub-agent in flight (Working). Counted in the
            // DEDICATED sub-agent counter (NOT the tool `in_flight`) so a background
            // sub-agent outlives the main turn's Stop (#21).
            ActivityEvent::SubagentStarted => {
                entry.subagents_in_flight = entry.subagents_in_flight.saturating_add(1);
                // A sub-agent starting is a "working again" signal → lift a stale Waiting (#26).
                entry.clear_waiting_if_working();
                if entry.base == Activity::Idle {
                    // Same as `ToolStarted` (#28): a sub-agent starting after a turn-end means work
                    // resumed → re-enter `Working` and drop the stale turn-end green/red so the dot
                    // is a clean blue while it is working again, never a stale ready.
                    entry.base = Activity::Working;
                    entry.ready_unread = false;
                    entry.error_unread = false;
                }
            }
            // A sub-agent finished → one fewer sub-agent in flight. CRUCIALLY this does NOT
            // by itself end the main turn. BUT if it is the LAST sub-agent AND a Stop was
            // deferred (ready_pending — the BACKGROUND case where Stop arrived first), it now
            // applies the deferred turn-end: Idle + raise the green. Otherwise (a
            // SYNCHRONOUS sub-agent, whose SubagentStop precedes the Stop) it only
            // decrements and the dot stays Working until the still-pending Stop.
            ActivityEvent::SubagentFinished => {
                entry.subagents_in_flight = entry.subagents_in_flight.saturating_sub(1);
                if entry.subagents_in_flight == 0 && entry.ready_pending {
                    entry.ready_pending = false;
                    entry.base = Activity::Idle;
                    if entry.pending_is_error {
                        entry.pending_is_error = false;
                        entry.error_unread = true;
                    } else {
                        entry.ready_unread = true;
                    }
                }
            }
            // A permission/elicitation block → Waiting (yellow). Leaves both counters as-is.
            ActivityEvent::AttentionNeeded { permission } => {
                entry.base = Activity::Waiting;
                entry.waiting_on = permission;
            }
            // The turn finished (Stop / StopFailure / idle_prompt): reset the TOOL
            // bookkeeping (in_flight / ask_in_flight) — a finished turn has no live tools —
            // but NEVER touch the sub-agent counter (a background sub-agent survives the
            // turn). If a sub-agent is still in flight, DEFER the turn-end: keep the dot
            // Working (resolve forces it) and arm `ready_pending` so the last SubagentStop
            // raises the green. Only with no sub-agent in flight do we go Idle + green now.
            ActivityEvent::TurnFinished => {
                entry.in_flight = 0;
                entry.ask_in_flight = false;
                // A finished turn asks nothing and runs nothing: both captions go with the tool
                // bookkeeping they belong to. (A DEFERRED end keeps the dot blue for a background
                // sub-agent, but the main turn's tool has still returned and its question is
                // still moot.)
                entry.waiting_on = None;
                entry.tool = None;
                if entry.subagents_in_flight > 0 {
                    // A background sub-agent outlives this turn → stay busy, defer the green.
                    entry.ready_pending = true;
                    entry.pending_is_error = false;
                } else {
                    entry.base = Activity::Idle;
                    entry.ready_pending = false;
                    entry.ready_unread = true;
                }
            }
            // The turn ended on an API error (`StopFailure`): same turn-end bookkeeping as
            // `TurnFinished` but it raises the RED `error_unread`, not the green `ready_unread`.
            // A still-running background sub-agent defers the RED exactly like the green.
            ActivityEvent::TurnFailed => {
                entry.in_flight = 0;
                entry.ask_in_flight = false;
                entry.waiting_on = None;
                entry.tool = None;
                if entry.subagents_in_flight > 0 {
                    // A background sub-agent outlives this errored turn → stay busy, defer the RED.
                    entry.ready_pending = true;
                    entry.pending_is_error = true;
                } else {
                    entry.base = Activity::Idle;
                    entry.ready_pending = false;
                    entry.pending_is_error = false;
                    entry.ready_unread = false; // the turn FAILED — not "ready"
                    entry.error_unread = true;
                }
            }
        }

        // ANY hook event DISARMS the submission window: the store has just been told the truth, so
        // the optimistic "a turn is starting" guess has nothing left to cover. This is what makes
        // the window a one-shot bridge rather than a delay — the ordinary case (`UserPromptSubmit`
        // lands in a few hundred ms) spends only that time armed, not the whole
        // [`SUBMISSION_ARMING_WINDOW`]. See [`AgentActivityStore::note_prompt_submitted`].
        entry.submitted_at = None;

        let snapshot = entry.snapshot();
        map.insert(terminal_id.to_string(), entry);
        snapshot
    }

    /// ARM the submission window on `terminal_id`: nyx has just written an `enter` into a terminal
    /// that hosts an ACTIVE agent session, so a turn is ABOUT to start even though no hook has said
    /// so yet. Called by `send_keys` / `send_to_terminal`; read back ONLY by [`Self::wait_activity`].
    ///
    /// # The race it closes (measured, gate 2026-09-02)
    ///
    /// `send_keys(["<texte>","enter"])` followed immediately by `wait_for_terminal` used to come
    /// back with `resolved: true`, `agent_activity: "idle"`, `waited_ms: 0`. Nothing was wrong with
    /// either tool: the write returns as soon as the bytes are in the PTY, while Claude's
    /// `UserPromptSubmit` hook is a node process that still has to start and POST — a few hundred
    /// milliseconds during which the store legitimately still reads `Idle` from the PREVIOUS turn.
    /// The default `until` (idle+waiting) therefore matched the state BEFORE the caller's own
    /// action, and the caller was handed back an empty answer and had to poll after all — the exact
    /// blind polling `wait_for_terminal` exists to remove.
    ///
    /// `wait_for_command` has no such race because starting a command mutates the run state
    /// SYNCHRONOUSLY before the wait can read it. A terminal's activity is reported by a THIRD
    /// party (the agent's hooks), so the equivalent synchronous mark has to be made here, by the
    /// only party that knows a turn was submitted: nyx, at the moment it writes the `enter`.
    ///
    /// # Why this is not the time-based staleness the module refuses
    ///
    /// The 0-stale decision (module docs) is about never EXPIRING a `Working` the hooks asserted —
    /// a long turn must not flicker to idle on a timer. This is the opposite direction and the
    /// opposite risk: a BOUNDED reticence about a turn nobody has asserted yet, which any hook
    /// event cancels and which lapses on its own after [`SUBMISSION_ARMING_WINDOW`]. It can make a
    /// wait return LATE, NEVER EARLY — and that is now literally true rather than nearly true:
    /// the window answers [`WaitActivity::Unconfirmed`], a value no `until` set can contain, so it
    /// SATISFIES nothing and only ever withholds. It never invents a `Working` the UI shows (the
    /// dot does not read this at all), and it cannot outlive its window. An `enter` that submits
    /// nothing (an empty prompt box) costs the next waiter one window and then reads the truth
    /// again.
    ///
    /// # The mark OUTLIVES the window, for the `Waiting` lift alone
    ///
    /// `submitted_at` stays set past [`SUBMISSION_ARMING_WINDOW`] until a hook clears it, because a
    /// SECOND thing hangs off it: an `enter` that approved a permission dialog answered a `Waiting`
    /// no hook will ever lift in time (see [`Entry::resolve_after_submission`]). Past the window the
    /// wait reads the store again — with that one substitution, and nothing else.
    ///
    /// Idempotent; creates no entry when none exists, because the caller already established that
    /// this terminal hosts an active agent session and a session always has an entry by then
    /// (SessionStart) — and materialising one here would put an `Idle`-with-nothing-to-show entry
    /// in the map that `snapshot_all` would have to filter.
    pub fn note_prompt_submitted(&self, terminal_id: &str) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(terminal_id.to_string())
            .or_insert_with(Entry::empty)
            .submitted_at = Some(std::time::Instant::now());
    }

    /// The activity as a BOUNDED WAIT must read it — [`Self::snapshot`]'s resolved kind, with the
    /// two things the submission mark changes and nothing else:
    ///
    ///   * a terminal ARMED by [`Self::note_prompt_submitted`] less than
    ///     [`SUBMISSION_ARMING_WINDOW`] ago reads [`WaitActivity::Unconfirmed`] — a turn nyx just
    ///     submitted has not given the hand back, and the store still describes the previous one,
    ///     so NOTHING may resolve on it (not even an explicit `until: ["working"]`);
    ///   * past that window, while no hook has landed yet, a standing `Waiting` reads `Working`
    ///     because nyx itself answered the block ([`Entry::resolve_after_submission`]).
    ///
    /// This is the ONLY reader of the arming window. The dot ([`Self::snapshot`],
    /// [`Self::snapshot_all`]) deliberately does NOT go through it: the UI shows what the hooks
    /// asserted, never what nyx guessed.
    pub fn wait_activity(&self, terminal_id: &str) -> WaitActivity {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = map.get(terminal_id) else {
            return WaitActivity::Observed(Activity::Idle);
        };
        let Some(at) = entry.submitted_at else {
            return WaitActivity::Observed(entry.resolve());
        };
        let since = at.elapsed();
        if since < SUBMISSION_ARMING_WINDOW {
            return WaitActivity::Unconfirmed;
        }
        if since >= SUBMISSION_LIFT_WINDOW {
            // The lift has a ceiling: past it, an `enter` that produced no hook at all is read
            // as what it is — a block still standing — rather than hidden from every wait.
            return WaitActivity::Observed(entry.resolve());
        }
        WaitActivity::Observed(entry.resolve_after_submission())
    }

    /// Whether nyx has SUBMITTED a turn into this terminal that no hook has confirmed yet — the
    /// arming mark, exposed as its own answer rather than folded into an activity value.
    ///
    /// This is the honest form of the signal `wait_for_terminal` used to smuggle into its
    /// `agent_activity` field: "nyx just submitted, and here is what it really knows". It is `true`
    /// from [`Self::note_prompt_submitted`] until the first hook event on that terminal
    /// ([`Self::apply`]) — so it stays `true` for the whole of an approved tool's run, which is
    /// precisely the stretch during which the store's own value cannot be taken at face value.
    pub fn submission_pending(&self, terminal_id: &str) -> bool {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(terminal_id)
            .is_some_and(|entry| entry.submitted_at.is_some())
    }

    /// FORCE a terminal's activity to `Idle` and drop its "ready" notification — the
    /// anti-phantom clear called on PTY death, `SessionEnd`, and terminal close. After
    /// this the terminal's dot follows nothing (no running, no ready), exactly the
    /// reflex the PTY busy=false-on-death already applies. Idempotent (an absent entry
    /// is a no-op). Removes the entry entirely so the map cannot grow unbounded across
    /// many short-lived terminals.
    pub fn clear(&self, terminal_id: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(terminal_id);
    }

    /// Set (or clear) the per-terminal STALE-PLUGIN flag (#18b). Called ONCE at SessionStart
    /// by [`crate::mcp_tools_core::agent_session_event`] after comparing the hook-reported
    /// `plugin_version` to the version nyx bundles: `true` when the loaded plugin is
    /// older/different (stale), `false` when it matches OR the version is unknown. Runtime-
    /// only (never persisted). Creates an entry if none exists yet (a SessionStart that
    /// precedes any per-turn hook), so the badge surfaces immediately on a stale session.
    /// Idempotent. The flag is dropped by [`Self::clear`] (PTY death / SessionEnd / close),
    /// so a restarted session starts NOT outdated.
    pub fn set_plugin_outdated(&self, terminal_id: &str, outdated: bool) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(terminal_id) {
            Some(entry) => entry.plugin_outdated = outdated,
            None => {
                // No entry yet (SessionStart before any activity hook). Only MATERIALIZE one
                // when the verdict is "outdated" — a `false` on a fresh terminal is a no-op so
                // we never create an idle/not-outdated entry that `snapshot_all` would then
                // have to filter out (keeps the boot-empty / anti-phantom invariant clean).
                if outdated {
                    map.insert(
                        terminal_id.to_string(),
                        Entry {
                            plugin_outdated: true,
                            ..Entry::empty()
                        },
                    );
                }
            }
        }
    }

    /// Clear ONLY the turn-end notifications for a terminal (the focus-aware mark-read),
    /// leaving the live `Working`/`Idle` activity intact. The activity analogue of
    /// `db::mark_exec_state_read`: viewing the terminal acknowledges the "ready" GREEN dot
    /// AND the errored-turn RED dot without disturbing a turn that may have started since.
    /// Both `ready_unread` and `error_unread` are cleared (focus acknowledges the red too,
    /// symmetric to the green). Idempotent.
    pub fn mark_ready_read(&self, terminal_id: &str) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get_mut(terminal_id) {
            entry.ready_unread = false;
            entry.error_unread = false;
        }
    }

    /// The snapshot for ONE terminal, or `None` when the terminal has no activity entry
    /// (→ `Idle`, no ready — the front treats a missing entry as idle).
    pub fn snapshot(&self, terminal_id: &str) -> Option<ActivitySnapshot> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.get(terminal_id).map(|e| e.snapshot())
    }

    /// Every terminal that has something to SHOW, as `(terminal_id, snapshot)` pairs: a
    /// NON-IDLE activity (working/waiting), a pending "ready" (green) notification, a pending
    /// errored-turn (red) notification, OR a stale-plugin badge (#18b). A terminal that is
    /// `Idle`, has no pending ready/error, AND is not plugin-outdated is OMITTED (nothing to
    /// show), so the front reads only the live/actionable signal. Used by the host to build
    /// the map pushed to the renderer on the `agent-sessions` change tick.
    pub fn snapshot_all(&self) -> Vec<(String, ActivitySnapshot)> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<(String, ActivitySnapshot)> = map
            .iter()
            .filter_map(|(id, e)| {
                let snap = e.snapshot();
                if snap.activity == Activity::Idle
                    && !snap.ready_unread
                    && !snap.error_unread
                    && !snap.plugin_outdated
                {
                    return None; // nothing live/actionable to show — omit.
                }
                Some((id.clone(), snap))
            })
            .collect();
        // Deterministic order for tests / stable iteration.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `PreToolUse`/`PostToolUse` payload with just its tool name.
    fn tool_facts(name: &str) -> HookFacts<'_> {
        HookFacts {
            tool_name: Some(name),
            ..HookFacts::default()
        }
    }

    /// A `Notification` payload with just its type.
    fn notif_facts(kind: &str) -> HookFacts<'_> {
        HookFacts {
            notification_type: Some(kind),
            ..HookFacts::default()
        }
    }

    /// BACK-DATE the arming marker of `terminal_id` so it reads as LAPSED, without sleeping out a
    /// real [`SUBMISSION_ARMING_WINDOW`]. The store is the only thing under test here, and a unit
    /// test that waits four real seconds is a unit test nobody runs.
    fn lapse_arming_window(store: &AgentActivityStore, terminal_id: &str) {
        let mut map = store.map.lock().unwrap();
        let entry = map.get_mut(terminal_id).expect("the marker created an entry");
        entry.submitted_at = std::time::Instant::now()
            .checked_sub(SUBMISSION_ARMING_WINDOW + std::time::Duration::from_millis(1));
        assert!(
            entry.submitted_at.is_some(),
            "the clock is far enough from its origin to back-date the marker"
        );
    }

    /// Back-date the submission mark past [`SUBMISSION_LIFT_WINDOW`] — the ceiling of the
    /// post-approval lift, not merely the arming window.
    fn lapse_lift_window(store: &AgentActivityStore, terminal_id: &str) {
        let mut map = store.map.lock().unwrap();
        let entry = map.get_mut(terminal_id).expect("the marker created an entry");
        entry.submitted_at = std::time::Instant::now()
            .checked_sub(SUBMISSION_LIFT_WINDOW + std::time::Duration::from_millis(1));
        assert!(entry.submitted_at.is_some(), "the clock can back-date past the lift ceiling");
    }

    /// THE LIFT HAS A CEILING. An `enter` that answered NOTHING — a dialog left open, an empty
    /// free-text field — produces no hook at all, so nothing ever clears the mark; without a
    /// ceiling the lift would hide that standing block from every wait that follows. Past
    /// [`SUBMISSION_LIFT_WINDOW`] the wait reads the truth again.
    #[test]
    fn the_lift_stops_hiding_a_block_past_its_ceiling() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::AttentionNeeded { permission: Some("dialog".into()) });
        assert_eq!(store.wait_activity("t1"), WaitActivity::Observed(Activity::Waiting));

        // nyx writes an enter that answers nothing. Within the lift, the Waiting is hidden…
        store.note_prompt_submitted("t1");
        lapse_arming_window(&store, "t1");
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Working),
            "inside the lift window the answered block is not shown"
        );

        // …and past the ceiling, with still no hook, the block is real again.
        lapse_lift_window(&store, "t1");
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Waiting),
            "past the ceiling an unanswered block is no longer hidden from the wait"
        );
        assert!(store.submission_pending("t1"), "the mark itself still stands until a hook");
    }

    /// AN `AskUserQuestion` IN FLIGHT IS NOT LIFTED BY ONE ANSWER. The tool asks several questions
    /// in ONE call and ends with ONE `PostToolUse`; nyx answering question 1 with an `enter` has
    /// not answered the block — question 2 is now waiting, and no hook will say so. The lift must
    /// leave the `Waiting` standing while `ask_in_flight`.
    #[test]
    fn an_ask_in_flight_is_not_lifted_by_answering_one_of_its_questions() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::AskStarted { question: Some("Q1?".into()) });
        store.apply("t1", ActivityEvent::AttentionNeeded { permission: Some("Q1?".into()) });
        assert_eq!(store.wait_activity("t1"), WaitActivity::Observed(Activity::Waiting));

        // nyx answers Q1. Past the arming window, with the ask still in flight, the wait must
        // STILL read Waiting: Q2 is on screen and nothing has said otherwise.
        store.note_prompt_submitted("t1");
        lapse_arming_window(&store, "t1");
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Waiting),
            "one answer inside a multi-question ask does not lift the block"
        );

        // The ask returns as a whole — THAT is what lifts it.
        store.apply("t1", ActivityEvent::AskFinished);
        assert!(!store.submission_pending("t1"));
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);
    }

    /// THE SUBMISSION ARMING WINDOW, at the store level — the three properties that make it safe:
    /// a wait reads `Unconfirmed` while it is armed, the DOT does not see the window at all, and
    /// the first hook cancels it.
    ///
    /// The dot half is the one worth pinning: the window is nyx's own reticence about a turn
    /// nobody has asserted yet, so it must never reach the UI. `snapshot`/`snapshot_all` are the
    /// UI's readers and they go on saying `idle`, which also keeps the boot-empty /
    /// nothing-to-show invariant (`snapshot_all` omits an idle entry) exactly as it was.
    #[test]
    fn a_submitted_turn_is_unconfirmed_for_a_wait_and_invisible_to_the_dot() {
        let store = AgentActivityStore::new();
        // A session exists and its last turn is over — the state the 2026-09-01 run read for
        // thirty seconds, and the one a wait must not mistake for "this turn is over".
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Idle)
        );
        assert!(!store.submission_pending("t1"));

        // nyx writes the `enter` itself: the wait now refuses to answer on the previous turn…
        store.note_prompt_submitted("t1");
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Unconfirmed,
            "a turn nyx just submitted has not given the hand back"
        );
        assert!(store.submission_pending("t1"));
        // …and the dot does NOT: the UI shows what the hooks asserted, never nyx's guess.
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Idle);

        // The hook lands: the window is cancelled and the real state machine takes over. Reading
        // `Working` here proves nothing on its own, so end the turn — an armed window would have
        // withheld the answer past the `Stop`; it reads `Idle`, so it was cleared.
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Idle),
            "any hook event disarms the window — it is a bridge, not a delay"
        );
        assert!(!store.submission_pending("t1"));

        // A terminal the store never heard of stays `Idle` for a wait too (no entry, no window).
        assert_eq!(
            store.wait_activity("unknown"),
            WaitActivity::Observed(Activity::Idle)
        );
        assert!(!store.submission_pending("unknown"));
    }

    /// The window LAPSES on its own. An `enter` that submitted nothing (an empty prompt box)
    /// leaves a marker no hook will ever cancel; once [`SUBMISSION_ARMING_WINDOW`] has passed the
    /// wait reads the truth again, so the worst case is one late answer and never a stuck one.
    #[test]
    fn the_arming_window_lapses_and_never_sticks() {
        let store = AgentActivityStore::new();
        store.note_prompt_submitted("t1");
        assert_eq!(store.wait_activity("t1"), WaitActivity::Unconfirmed);

        lapse_arming_window(&store, "t1");
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Idle),
            "past the window the wait reads the real state again"
        );
    }

    /// #K5 — AN `enter` THAT APPROVES A PERMISSION DIALOG LIFTS THE `Waiting` IT ANSWERED.
    ///
    /// The measured trap: `AttentionNeeded` (the `permission_prompt` Notification) puts the entry
    /// on `Waiting`; nyx approves with `send_keys(["enter"])`; and NO hook lifts it — the plugin
    /// registers no `PermissionRequest`, `PreToolUse` already fired BEFORE the dialog, and the
    /// approved tool's `PostToolUse` only lands when the tool ENDS. So once the arming window
    /// lapsed, `wait_for_terminal` answered `resolved: true, agent_activity: "waiting"` on an agent
    /// that was working, and the pilot sent another `enter` into its input box.
    ///
    /// nyx is the party that answered the block, so the arming mark is what lifts it. The DOT is
    /// untouched — the user's yellow is the hooks' business, not the wait's.
    #[test]
    fn approving_a_permission_lifts_the_waiting_for_the_wait_only() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply(
            "t1",
            ActivityEvent::AttentionNeeded {
                permission: Some("Bash(rm -rf .)".to_string()),
            },
        );
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Waiting),
            "before nyx answers, the block is real and the wait must resolve on it"
        );

        // nyx approves — the `enter` arms the submission mark exactly as a prompt would.
        store.note_prompt_submitted("t1");
        assert_eq!(store.wait_activity("t1"), WaitActivity::Unconfirmed);

        // PAST the window, with no hook in sight (the approved tool is still running): the wait
        // must NOT read `Waiting` any more — that block was answered by nyx itself.
        lapse_arming_window(&store, "t1");
        assert_eq!(
            store.wait_activity("t1"),
            WaitActivity::Observed(Activity::Working),
            "the permission nyx approved is not a block the caller is still waiting on"
        );

        // THE DOT KEEPS ITS CURRENT BEHAVIOUR: `snapshot`/`snapshot_all` never read the arming
        // mark, so the UI still shows what the hooks asserted.
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Waiting);
        assert_eq!(
            store
                .snapshot_all()
                .iter()
                .find(|(id, _)| id == "t1")
                .expect("the terminal is in the map")
                .1
                .activity,
            Activity::Waiting
        );

        // And the lift dies with the first hook: the tool returns, the state machine lifts the
        // yellow on its own terms, and nothing of the submission mark survives.
        store.apply("t1", ActivityEvent::ToolFinished);
        assert!(!store.submission_pending("t1"));
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);
    }

    /// `from_hook` maps the per-turn hooks, reading the tool name / notification type to
    /// disambiguate. The `AskUserQuestion` Pre/Post become `Ask*`; an ordinary tool's
    /// Pre/Post become `Tool*`; only a permission/elicitation `Notification` is
    /// `AttentionNeeded`; `idle_prompt`/`Stop`/`StopFailure` are `TurnFinished`.
    #[test]
    fn hook_name_maps_turn_events() {
        assert_eq!(
            ActivityEvent::from_hook_event_name("UserPromptSubmit"),
            Some(ActivityEvent::PromptSubmitted)
        );
        assert_eq!(
            ActivityEvent::from_hook_event_name("Stop"),
            Some(ActivityEvent::TurnFinished)
        );
        assert_eq!(
            ActivityEvent::from_hook_event_name("StopFailure"),
            Some(ActivityEvent::TurnFailed)
        );
        // PreToolUse: ordinary tool vs AskUserQuestion. An ordinary tool CARRIES its name — the
        // fallback secondary information — instead of the name being used as a discriminant and
        // then thrown away.
        assert_eq!(
            ActivityEvent::from_hook("PreToolUse", tool_facts("Bash")),
            Some(ActivityEvent::ToolStarted {
                tool: Some("Bash".to_string())
            })
        );
        // A `PreToolUse` with NO tool name at all is still an ordinary tool, with nothing to say.
        assert_eq!(
            ActivityEvent::from_hook("PreToolUse", HookFacts::default()),
            Some(ActivityEvent::ToolStarted { tool: None })
        );
        assert_eq!(
            ActivityEvent::from_hook("PreToolUse", tool_facts(ASK_USER_QUESTION_TOOL)),
            Some(ActivityEvent::AskStarted { question: None })
        );
        // PostToolUse + PostToolUseFailure, ordinary vs Ask.
        assert_eq!(
            ActivityEvent::from_hook("PostToolUse", tool_facts("Bash")),
            Some(ActivityEvent::ToolFinished)
        );
        assert_eq!(
            ActivityEvent::from_hook("PostToolUseFailure", tool_facts("Bash")),
            Some(ActivityEvent::ToolFinished)
        );
        assert_eq!(
            ActivityEvent::from_hook("PostToolUse", tool_facts(ASK_USER_QUESTION_TOOL)),
            Some(ActivityEvent::AskFinished)
        );
        // Subagent start/stop.
        assert_eq!(
            ActivityEvent::from_hook_event_name("SubagentStart"),
            Some(ActivityEvent::SubagentStarted)
        );
        assert_eq!(
            ActivityEvent::from_hook_event_name("SubagentStop"),
            Some(ActivityEvent::SubagentFinished)
        );
        // Notification discrimination.
        assert_eq!(
            ActivityEvent::from_hook("Notification", notif_facts("permission_prompt")),
            Some(ActivityEvent::AttentionNeeded { permission: None })
        );
        assert_eq!(
            ActivityEvent::from_hook("Notification", notif_facts("elicitation_dialog")),
            Some(ActivityEvent::AttentionNeeded { permission: None })
        );
        assert_eq!(
            ActivityEvent::from_hook("Notification", notif_facts("idle_prompt")),
            Some(ActivityEvent::TurnFinished)
        );
        // A background sub-agent completion notification (defensive, best-guess strings) maps
        // to SubagentFinished — the same decrement as a SubagentStop.
        for t in SUBAGENT_DONE_NOTIFICATION_TYPES {
            assert_eq!(
                ActivityEvent::from_hook("Notification", notif_facts(t)),
                Some(ActivityEvent::SubagentFinished),
                "a '{t}' notification decrements the sub-agent counter"
            );
        }
        // An unknown / absent notification type does not move the dot.
        assert_eq!(
            ActivityEvent::from_hook("Notification", notif_facts("something_else")),
            None
        );
        assert_eq!(
            ActivityEvent::from_hook("Notification", HookFacts::default()),
            None
        );
        // Session lifecycle + anything else is not an activity event.
        assert_eq!(ActivityEvent::from_hook_event_name("SessionStart"), None);
        assert_eq!(ActivityEvent::from_hook_event_name("SessionEnd"), None);
    }

    /// THE BOOT GUARANTEE: a fresh store is EMPTY — every terminal is `Idle` with no
    /// pending ready, including any "resume candidate". No phantom running can survive a
    /// restart because nothing is read from the DB into the store.
    #[test]
    fn fresh_store_is_idle_for_everyone() {
        let store = AgentActivityStore::new();
        assert!(store.snapshot("any-terminal").is_none());
        assert!(
            store.snapshot_all().is_empty(),
            "boot store has no live activity for any terminal"
        );
    }

    /// A full simple turn: prompt → Working; Stop → Idle WITH the focus-aware ready
    /// notification raised.
    #[test]
    fn prompt_then_stop_runs_then_notifies_ready() {
        let store = AgentActivityStore::new();
        let s = store.apply("t1", ActivityEvent::PromptSubmitted);
        assert_eq!(s.activity, Activity::Working);
        assert!(!s.ready_unread, "a started turn is not 'ready'");

        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(
            s.ready_unread,
            "a finished turn raises the ready notification"
        );
    }

    /// THE CORE FIX — a single tool that runs for "10 minutes": one `PreToolUse`, no
    /// `PostToolUse` for a long time. While the tool is in flight `in_flight == 1`, so the
    /// dot stays `Working` across arbitrarily many reads — NO time-based staleness, NO
    /// dependence on `Stop` arriving. Only the matching `PostToolUse` (then `Stop`) settles
    /// it.
    #[test]
    fn long_running_tool_stays_working_until_post() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::ToolStarted { tool: None }); // a 10-minute tool begins.
        for _ in 0..5 {
            assert_eq!(
                store.snapshot("t1").unwrap().activity,
                Activity::Working,
                "an in-flight tool keeps the dot Working with no timer"
            );
        }
        // The tool finally returns — still Working (Claude now generates the answer text).
        store.apply("t1", ActivityEvent::ToolFinished);
        assert_eq!(
            store.snapshot("t1").unwrap().activity,
            Activity::Working,
            "in_flight==0 mid-turn stays Working until Stop"
        );
        // The turn ends.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
    }

    /// The in-flight COUNTER: overlapping tools (e.g. parallel tool calls). Two starts,
    /// two finishes — only when the LAST finishes does in_flight reach 0, and the dot is
    /// Working the whole time. A stray Post without a Pre saturates at 0 (no underflow).
    #[test]
    fn in_flight_counter_balances_overlapping_tools() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::ToolStarted { tool: None });
        store.apply("t1", ActivityEvent::ToolStarted { tool: None }); // two tools in flight.
        store.apply("t1", ActivityEvent::ToolFinished); // one returns, one still running.
        assert_eq!(
            store.snapshot("t1").unwrap().activity,
            Activity::Working,
            "still one tool in flight"
        );
        store.apply("t1", ActivityEvent::ToolFinished); // the last returns.
        assert_eq!(
            store.snapshot("t1").unwrap().activity,
            Activity::Working,
            "in_flight 0 mid-turn → Working (generating)"
        );
        // A stray Post with nothing in flight must not underflow / change the kind.
        store.apply("t1", ActivityEvent::ToolFinished);
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);
    }

    /// `AskUserQuestion` → `Waiting` (yellow) on the `PreToolUse`, then back to `Working`
    /// on the paired `PostToolUse` (the user answered).
    #[test]
    fn ask_user_question_waits_then_resumes() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        let s = store.apply("t1", ActivityEvent::AskStarted { question: None });
        assert_eq!(
            s.activity,
            Activity::Waiting,
            "asking the user blocks → yellow"
        );
        // The user answers.
        let s = store.apply("t1", ActivityEvent::AskFinished);
        assert_eq!(
            s.activity,
            Activity::Working,
            "an answered question resumes work"
        );
    }

    /// An `AskUserQuestion` Waiting still respects OTHER tools in flight: if a normal tool
    /// is also running, answering the question drops back to Working (the other tool keeps
    /// it Working anyway), never to Idle.
    #[test]
    fn ask_waiting_with_other_tool_in_flight_stays_working_after_answer() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::ToolStarted { tool: None }); // a background tool.
        store.apply("t1", ActivityEvent::AskStarted { question: None }); // + a question → Waiting wins.
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Waiting);
        store.apply("t1", ActivityEvent::AskFinished); // answered.
        assert_eq!(
            store.snapshot("t1").unwrap().activity,
            Activity::Working,
            "the other tool keeps it Working"
        );
    }

    /// A permission `Notification` → `Waiting`; the `idle_prompt` notification → `Idle` +
    /// ready (a turn end, like `Stop`).
    #[test]
    fn notification_permission_waits_idle_prompt_finishes() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        let s = store.apply("t1", ActivityEvent::AttentionNeeded { permission: None }); // permission_prompt.
        assert_eq!(s.activity, Activity::Waiting);
        assert!(
            !s.ready_unread,
            "a permission prompt is not a finished response"
        );

        // idle_prompt behaves like Stop.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
    }

    /// #26 — a STALE `Waiting` (yellow) is lifted by ANY "working again" signal, not only by
    /// the question's paired `AskFinished`. This is the "chat about this" decline: the user
    /// declines an AskUserQuestion and Claude resumes work via SOME hook (empirically
    /// uncertain which), so a tool starting, a tool returning, OR a sub-agent starting must
    /// each clear the yellow back to `Working` — the dot can never stay stuck yellow while
    /// Claude is clearly running.
    #[test]
    fn waiting_is_cleared_by_any_working_signal() {
        // A tool STARTING after the Waiting lifts it.
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::AskStarted { question: None }); // Waiting (the asked question).
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Waiting);
        let s = store.apply("t1", ActivityEvent::ToolStarted { tool: None }); // resumed via a tool.
        assert_eq!(
            s.activity,
            Activity::Working,
            "a tool starting after a decline lifts the stale yellow"
        );

        // A tool RETURNING after the Waiting lifts it (a permission Notification block this
        // time — no paired Ask Post — and the resume arrives as a tool's PostToolUse).
        let store = AgentActivityStore::new();
        store.apply("t2", ActivityEvent::PromptSubmitted);
        store.apply("t2", ActivityEvent::ToolStarted { tool: None }); // a tool is in flight.
        store.apply("t2", ActivityEvent::AttentionNeeded { permission: None }); // permission_prompt → Waiting.
        assert_eq!(store.snapshot("t2").unwrap().activity, Activity::Waiting);
        let s = store.apply("t2", ActivityEvent::ToolFinished); // that tool returns → resumed.
        assert_eq!(
            s.activity,
            Activity::Working,
            "a tool returning lifts the stale yellow"
        );

        // A SUB-AGENT starting after the Waiting lifts it too.
        let store = AgentActivityStore::new();
        store.apply("t3", ActivityEvent::PromptSubmitted);
        store.apply("t3", ActivityEvent::AttentionNeeded { permission: None }); // Waiting.
        let s = store.apply("t3", ActivityEvent::SubagentStarted);
        assert_eq!(
            s.activity,
            Activity::Working,
            "a sub-agent starting lifts the stale yellow"
        );

        // A NEW PROMPT (the existing reset path) also clears a stale Waiting.
        let store = AgentActivityStore::new();
        store.apply("t4", ActivityEvent::PromptSubmitted);
        store.apply("t4", ActivityEvent::AttentionNeeded { permission: None }); // Waiting.
        let s = store.apply("t4", ActivityEvent::PromptSubmitted);
        assert_eq!(
            s.activity,
            Activity::Working,
            "a new prompt resets the yellow"
        );
    }

    /// #26 — `AskFinished` lifts the yellow UNCONDITIONALLY (the decline path), even though
    /// the existing answered-question test already passes: a decline ("chat about this") that
    /// still routes through the AskUserQuestion `PostToolUse` must resume work, not stay
    /// yellow, regardless of the internal `ask_in_flight` bookkeeping.
    #[test]
    fn ask_finished_lifts_waiting_on_decline() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::AskStarted { question: None }); // the question → Waiting.
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Waiting);
        // The user declines / answers — the AskUserQuestion Post fires.
        let s = store.apply("t1", ActivityEvent::AskFinished);
        assert_eq!(
            s.activity,
            Activity::Working,
            "a declined/answered question resumes work, never stuck yellow"
        );
    }

    /// `Stop` AND `StopFailure` both finish the turn → Idle, and reset in_flight (a finished
    /// turn has no live tools even if a Post was lost to an interrupt). `Stop` raises the
    /// GREEN ready; `StopFailure` raises the RED error instead (NOT the green).
    #[test]
    fn stop_and_stop_failure_both_finish_and_reset_in_flight() {
        let store = AgentActivityStore::new();
        // Stop with a tool still "in flight" (its Post was lost) → in_flight reset, Idle + green.
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::ToolStarted { tool: None });
        let s = store.apply("t1", ActivityEvent::TurnFinished); // Stop.
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
        assert!(!s.error_unread);
        // A fresh turn then StopFailure idles + resets too, but raises the RED, not the green.
        store.apply("t2", ActivityEvent::PromptSubmitted);
        store.apply("t2", ActivityEvent::ToolStarted { tool: None });
        let s = store.apply("t2", ActivityEvent::TurnFailed); // StopFailure.
        assert_eq!(s.activity, Activity::Idle);
        assert!(!s.ready_unread, "a failed turn is not 'ready' (green)");
        assert!(s.error_unread, "a failed turn raises the red error dot");
    }

    /// (#35) A `StopFailure` on a plain turn (no sub-agent) ends it on an API error → `Idle`
    /// with the RED `error_unread` raised and the GREEN `ready_unread` NOT raised. The dot
    /// kind is Idle (the front renders red from `error_unread`, not from the Activity kind).
    #[test]
    fn turn_failed_raises_red_not_green() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        let s = store.apply("t1", ActivityEvent::TurnFailed); // StopFailure.
        assert_eq!(
            s.activity,
            Activity::Idle,
            "a failed turn ends the turn → Idle"
        );
        assert!(s.error_unread, "an errored turn raises the red error dot");
        assert!(
            !s.ready_unread,
            "an errored turn does NOT raise the green ready dot"
        );
    }

    /// (#35 regression) A normal `Stop` still raises ONLY the green `ready_unread`, never the
    /// red `error_unread` — the symmetric error bit must not leak onto a successful turn-end.
    #[test]
    fn turn_finished_raises_green_not_red() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        let s = store.apply("t1", ActivityEvent::TurnFinished); // Stop.
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread, "a finished turn raises the green ready dot");
        assert!(
            !s.error_unread,
            "a successful turn must NOT raise the red error dot"
        );
    }

    /// (#35) A `StopFailure` while a BACKGROUND sub-agent is still in flight DEFERS the RED
    /// exactly like the green: the dot stays `Working` (blue) and `error_unread` is NOT yet
    /// set; only the trailing `SubagentStop` settles it to `Idle` + RED (not green).
    #[test]
    fn turn_failed_defers_red_for_a_background_subagent() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted); // a BACKGROUND sub-agent begins.
                                                           // The MAIN turn ERRORS while the background sub-agent is still running.
        let s = store.apply("t1", ActivityEvent::TurnFailed); // StopFailure.
        assert_eq!(
            s.activity,
            Activity::Working,
            "a StopFailure with a sub-agent still in flight stays Working — defers the red"
        );
        assert!(
            !s.error_unread,
            "the red is DEFERRED while a background sub-agent runs"
        );
        assert!(!s.ready_unread);
        // The background sub-agent finishes → the deferred turn-end applies as RED, not green.
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(
            s.activity,
            Activity::Idle,
            "the last sub-agent settles the errored turn"
        );
        assert!(
            s.error_unread,
            "the deferred RED is raised when the last sub-agent finishes"
        );
        assert!(
            !s.ready_unread,
            "a deferred FAILED turn settles red, never green"
        );
    }

    /// (#35) `mark_ready_read` (focus) clears the RED `error_unread` too — focus acknowledges
    /// the errored-turn notification symmetrically to the green ready.
    #[test]
    fn mark_ready_read_clears_the_red_error() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::TurnFailed); // red raised.
        assert!(store.snapshot("t1").unwrap().error_unread);
        store.mark_ready_read("t1");
        let snap = store.snapshot("t1").unwrap();
        assert!(
            !snap.error_unread,
            "viewing the terminal clears the red error dot"
        );
        assert_eq!(snap.activity, Activity::Idle);
    }

    /// (#35) A NEW prompt (a retry) after an errored turn clears the RED and re-enters
    /// `Working` (blue), exactly as a new prompt supersedes the green ready.
    #[test]
    fn new_prompt_after_error_clears_red_and_works() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::TurnFailed); // red raised.
        assert!(store.snapshot("t1").unwrap().error_unread);
        let s = store.apply("t1", ActivityEvent::PromptSubmitted); // the retry.
        assert_eq!(s.activity, Activity::Working, "a retry re-enters Working");
        assert!(!s.error_unread, "a new turn clears the stale red error dot");
        assert!(!s.ready_unread);
    }

    /// `SubagentStart`/`SubagentStop` move the counter but NEVER end the main turn: a
    /// sub-agent finishing while the main turn is mid-flight leaves it `Working`, never
    /// `Idle` (the explicit guard).
    #[test]
    fn subagent_lifecycle_does_not_end_main_turn() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted);
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(
            s.activity,
            Activity::Working,
            "a sub-agent finishing is not the main turn finishing"
        );
        assert!(!s.ready_unread, "SubagentStop must not raise the green dot");
    }

    /// THE #21 FIX — a BACKGROUND sub-agent that OUTLIVES the main turn. The hook order is
    /// `SubagentStart` → `Stop` (the main turn ends, the background survives) → `SubagentStop`
    /// (the background finally finishes). Between the `Stop` and the `SubagentStop` the dot
    /// MUST stay `Working` (blue) and NOT raise the green "ready" — only the trailing
    /// `SubagentStop` settles it to `Idle` + green.
    #[test]
    fn background_subagent_keeps_working_between_stop_and_subagent_stop() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted); // a BACKGROUND sub-agent begins.
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);

        // The MAIN turn finishes while the background sub-agent is still running.
        let s = store.apply("t1", ActivityEvent::TurnFinished); // Stop.
        assert_eq!(
            s.activity,
            Activity::Working,
            "a Stop with a sub-agent still in flight stays Working — NOT premature green"
        );
        assert!(
            !s.ready_unread,
            "the green 'ready' is DEFERRED while a background sub-agent runs"
        );
        // Reads in the gap keep it Working with no timer.
        for _ in 0..3 {
            let snap = store.snapshot("t1").unwrap();
            assert_eq!(snap.activity, Activity::Working);
            assert!(!snap.ready_unread);
        }

        // The background sub-agent finally finishes → NOW the deferred turn-end applies.
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(
            s.activity,
            Activity::Idle,
            "the last sub-agent settles the turn"
        );
        assert!(
            s.ready_unread,
            "the green 'ready' is raised when the last sub-agent finishes"
        );
    }

    /// A SYNCHRONOUS sub-agent (`SubagentStart` → `SubagentStop` → `Stop`) is unaffected by
    /// the deferral: its counter is already 0 at the `Stop`, so the `Stop` idles + greens
    /// immediately, exactly as before the fix.
    #[test]
    fn synchronous_subagent_greens_normally_at_stop() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted);
        // The sub-agent finishes BEFORE the turn ends (synchronous): still Working, no green.
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(s.activity, Activity::Working, "main turn still generating");
        assert!(
            !s.ready_unread,
            "a sub-agent stop with no deferred Stop raises nothing"
        );
        // The main turn then ends with no sub-agent in flight → immediate Idle + green.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(
            s.ready_unread,
            "a synchronous sub-agent greens at the Stop as before"
        );
    }

    /// Sub-agents and TOOLS are counted SEPARATELY: a `Stop` resets the tool counter (a lost
    /// tool Post is forgiven) but must NOT clear a still-running sub-agent — the dot stays
    /// Working until that sub-agent's own SubagentStop, then greens.
    #[test]
    fn stop_resets_tools_but_not_subagents() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted); // background sub-agent.
        store.apply("t1", ActivityEvent::ToolStarted { tool: None }); // a tool whose Post will be "lost".
                                                                      // Stop: the tool bookkeeping is reset, but the sub-agent survives → still Working.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(
            s.activity,
            Activity::Working,
            "the sub-agent keeps it Working"
        );
        assert!(!s.ready_unread);
        // A stray late tool Post must not underflow nor settle the turn.
        let s = store.apply("t1", ActivityEvent::ToolFinished);
        assert_eq!(
            s.activity,
            Activity::Working,
            "stray Post does not settle a deferred turn"
        );
        assert!(!s.ready_unread);
        // The sub-agent finishes → deferred green applies.
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
    }

    /// MULTIPLE overlapping background sub-agents: the deferred green is raised only when the
    /// LAST one finishes (the counter reaches 0), not the first.
    #[test]
    fn multiple_background_subagents_green_only_on_the_last() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted);
        store.apply("t1", ActivityEvent::SubagentStarted); // two background sub-agents.
        store.apply("t1", ActivityEvent::TurnFinished); // Stop while both run.
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);
        // First sub-agent finishes — one still in flight → stay Working, no green yet.
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(s.activity, Activity::Working, "one sub-agent still running");
        assert!(
            !s.ready_unread,
            "no green until the LAST sub-agent finishes"
        );
        // Last sub-agent finishes → deferred green.
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
    }

    /// A `Notification(idle_prompt)` (a turn-end via notification, not a `Stop`) defers the
    /// same way: a still-running background sub-agent keeps it Working until the SubagentStop.
    /// This proves the deferral lives on `TurnFinished`, covering ALL its hook sources.
    #[test]
    fn idle_prompt_also_defers_for_a_background_subagent() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted);
        // idle_prompt maps to TurnFinished — defers because a sub-agent is in flight.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Working);
        assert!(!s.ready_unread);
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
    }

    /// DEFENSIVE PATH for the #21 uncertainty: if a background sub-agent ends via a
    /// `Notification(background_subagent_complete)` instead of a `SubagentStop`, it still
    /// decrements the counter and lifts the deferred green — so the dot does not stay stuck
    /// blue. Drives the event THROUGH `from_hook` to prove the notification routing.
    #[test]
    fn background_subagent_completion_via_notification_lifts_deferred_green() {
        let store = AgentActivityStore::new();
        let apply_hook = |name: &str, ntype: Option<&str>| {
            let facts = HookFacts {
                notification_type: ntype,
                ..HookFacts::default()
            };
            let ev = ActivityEvent::from_hook(name, facts).expect("recognized hook");
            store.apply("t1", ev)
        };
        apply_hook("UserPromptSubmit", None);
        apply_hook("SubagentStart", None);
        let s = apply_hook("Stop", None);
        assert_eq!(
            s.activity,
            Activity::Working,
            "deferred while the background runs"
        );
        assert!(!s.ready_unread);
        // The background completes via a NOTIFICATION (not a SubagentStop) → counter drops,
        // deferred green lifts.
        let s = apply_hook("Notification", Some("background_subagent_complete"));
        assert_eq!(
            s.activity,
            Activity::Idle,
            "notification completion settles the turn"
        );
        assert!(
            s.ready_unread,
            "the deferred green is raised via the completion notification"
        );
    }

    /// A `clear` (PTY death / SessionEnd / close) during a deferred background turn drops
    /// EVERYTHING — no phantom blue dot is left behind if the trailing SubagentStop never
    /// arrives (the anti-phantom guard still holds with the new state).
    #[test]
    fn clear_drops_a_deferred_background_turn() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted);
        store.apply("t1", ActivityEvent::TurnFinished); // deferred (Working, ready_pending).
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);
        store.clear("t1");
        assert!(
            store.snapshot("t1").is_none(),
            "a hard clear drops a deferred background turn — no phantom blue dot"
        );
    }

    /// A new prompt SUPERSEDES a prior "ready" AND resets any dangling in-flight state
    /// (the bare-Esc residual hole self-heals): after a turn left Waiting/in-flight, a
    /// fresh UserPromptSubmit clears the ready and re-enters Working with in_flight=0.
    #[test]
    fn new_prompt_resets_everything() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert!(s.ready_unread);
        // Simulate a bare-Esc-then-new-prompt: leave a tool "in flight" then prompt again.
        store.apply("t1", ActivityEvent::AskStarted { question: None }); // Waiting + in_flight=1 dangling.
        let s = store.apply("t1", ActivityEvent::PromptSubmitted);
        assert_eq!(
            s.activity,
            Activity::Working,
            "a new turn resets to Working"
        );
        assert!(!s.ready_unread, "a new turn drops the stale 'ready'");
        // And the dangling in_flight is gone: a single Stop now idles cleanly.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Idle);
    }

    /// `clear` forces Idle and drops the ready — the PTY-death / SessionEnd / close
    /// anti-phantom reflex. A killed Claude mid-tool (Working, no Stop) cannot leave a
    /// running dot behind.
    #[test]
    fn clear_forces_idle_and_drops_ready_phantom() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::ToolStarted { tool: None }); // a tool in flight, no Stop (a kill).
        store.clear("t1");
        assert!(
            store.snapshot("t1").is_none(),
            "PTY death clears the working dot — no phantom"
        );
        // A ready notification is dropped on clear too.
        store.apply("t2", ActivityEvent::PromptSubmitted);
        store.apply("t2", ActivityEvent::TurnFinished); // ready raised.
        store.clear("t2");
        assert!(store.snapshot("t2").is_none());
        // Clearing an absent terminal is a no-op.
        store.clear("never-seen");
    }

    /// `mark_ready_read` (focus-aware) clears ONLY the ready notification, leaving the
    /// live activity intact.
    #[test]
    fn mark_ready_read_clears_only_the_notification() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::TurnFinished);
        assert!(store.snapshot("t1").unwrap().ready_unread);
        store.mark_ready_read("t1");
        let snap = store.snapshot("t1").unwrap();
        assert!(
            !snap.ready_unread,
            "viewing the terminal clears the green dot"
        );
        assert_eq!(snap.activity, Activity::Idle);
        // If a new turn started, mark_ready_read must NOT touch the Working state.
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.mark_ready_read("t1");
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Working);
    }

    /// THE 0-STALE CONTRACT: a `Working` (or `Waiting`) entry NEVER self-heals to `Idle`
    /// on its own — there is no time-based expiry. A long turn stays `Working` until a real
    /// terminating event (`Stop`/new prompt) or a hard [`AgentActivityStore::clear`]. This
    /// replaces the removed `ACTIVITY_STALE_AFTER_MS` lazy-expiry that made the dot jump.
    #[test]
    fn working_never_self_heals_without_an_event_or_clear() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        for _ in 0..5 {
            assert_eq!(
                store.snapshot("t1").unwrap().activity,
                Activity::Working,
                "Working holds with no time-based expiry"
            );
        }
        store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Idle);

        // Waiting holds the same way until a clear/event.
        store.apply("t2", ActivityEvent::AttentionNeeded { permission: None });
        assert_eq!(store.snapshot("t2").unwrap().activity, Activity::Waiting);
        store.clear("t2");
        assert!(store.snapshot("t2").is_none(), "only a hard clear drops it");
    }

    /// (#28) THE "Working tant que ça travaille" invariant ACROSS a turn boundary: after a `Stop`
    /// (Idle + green ready) or a `StopFailure` (Idle + red error), WORK RESUMING — a tool starting or
    /// a sub-agent starting — WITHOUT a fresh `UserPromptSubmit` (an agentic continuation, or a
    /// `UserPromptSubmit` REORDERED behind the work hook by the best-effort, unordered hook-POST
    /// transport) MUST re-enter `Working` and DROP the now-stale turn-end notification. A
    /// resumed-and-working session must never read `Idle` (which would let the lazy
    /// resume treat it as dormant and inject a `claude --resume` into a live agent — the #28 bug),
    /// nor keep a stale green/red while it is demonstrably working again.
    #[test]
    fn work_resuming_after_a_turn_end_re_enters_working_and_drops_the_stale_notification() {
        // A TOOL resuming work after a successful turn end (green ready pending).
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        let s = store.apply("t1", ActivityEvent::TurnFinished); // Idle + green.
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
        let s = store.apply("t1", ActivityEvent::ToolStarted { tool: None }); // work resumes, no new prompt.
        assert_eq!(
            s.activity,
            Activity::Working,
            "a tool starting after a Stop re-enters Working — never a faux-Idle on a working session"
        );
        assert!(
            !s.ready_unread,
            "the stale green is dropped once work demonstrably resumes"
        );

        // A SUB-AGENT resuming work after an ERRORED turn end (red error pending) behaves the same.
        let store = AgentActivityStore::new();
        store.apply("t2", ActivityEvent::PromptSubmitted);
        store.apply("t2", ActivityEvent::TurnFailed); // Idle + red error.
        assert!(store.snapshot("t2").unwrap().error_unread);
        let s = store.apply("t2", ActivityEvent::SubagentStarted);
        assert_eq!(
            s.activity,
            Activity::Working,
            "a sub-agent starting after a turn-end re-enters Working"
        );
        assert!(
            !s.error_unread,
            "the stale red is dropped once work resumes"
        );
    }

    /// `snapshot_all` returns only terminals with something LIVE to show (working/waiting
    /// or a pending ready), in deterministic id order; a cleared/idle terminal is omitted.
    #[test]
    fn snapshot_all_lists_only_live_terminals() {
        let store = AgentActivityStore::new();
        store.apply("t-b", ActivityEvent::PromptSubmitted); // Working.
        store.apply("t-a", ActivityEvent::PromptSubmitted);
        store.apply("t-a", ActivityEvent::TurnFinished); // Idle + ready.
        store.apply("t-c", ActivityEvent::PromptSubmitted);
        store.clear("t-c"); // gone.
        store.apply("t-d", ActivityEvent::PromptSubmitted);
        store.apply("t-d", ActivityEvent::AttentionNeeded { permission: None }); // Waiting.

        let all = store.snapshot_all();
        let ids: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["t-a", "t-b", "t-d"],
            "ordered, only live terminals"
        );
        // t-a is Idle but has a pending ready → still listed, with ready_unread.
        let a = all.iter().find(|(id, _)| id == "t-a").unwrap();
        assert_eq!(a.1.activity, Activity::Idle);
        assert!(a.1.ready_unread);
        // t-b is Working, t-d is Waiting.
        let b = all.iter().find(|(id, _)| id == "t-b").unwrap();
        assert_eq!(b.1.activity, Activity::Working);
        let d = all.iter().find(|(id, _)| id == "t-d").unwrap();
        assert_eq!(d.1.activity, Activity::Waiting);
    }

    /// #18b — `set_plugin_outdated` materializes a stale-plugin badge on an OTHERWISE-idle
    /// terminal (a SessionStart that precedes any per-turn hook), so `snapshot_all` surfaces
    /// it even though there is no working/waiting/ready signal. A `false` verdict on a fresh
    /// terminal is a no-op (no entry created → still omitted), preserving the boot-empty
    /// invariant. The flag survives a turn (new prompt) but is dropped by `clear` (restart).
    #[test]
    fn plugin_outdated_badge_surfaces_and_clears() {
        let store = AgentActivityStore::new();

        // A NOT-outdated verdict on a fresh terminal creates nothing — still idle/omitted.
        store.set_plugin_outdated("t-current", false);
        assert!(
            store.snapshot("t-current").is_none(),
            "a current plugin on a fresh terminal materializes no entry"
        );

        // An OUTDATED verdict materializes an idle entry whose badge `snapshot_all` surfaces.
        store.set_plugin_outdated("t-stale", true);
        let snap = store
            .snapshot("t-stale")
            .expect("stale badge materializes an entry");
        assert_eq!(
            snap.activity,
            Activity::Idle,
            "the badge does not imply working"
        );
        assert!(!snap.ready_unread);
        assert!(snap.plugin_outdated, "the stale-plugin badge is set");
        let all = store.snapshot_all();
        assert!(
            all.iter()
                .any(|(id, s)| id == "t-stale" && s.plugin_outdated),
            "an idle-but-stale terminal is surfaced by snapshot_all"
        );

        // The badge SURVIVES a turn (a new prompt is not a session restart).
        store.apply("t-stale", ActivityEvent::PromptSubmitted);
        let snap = store.snapshot("t-stale").unwrap();
        assert_eq!(snap.activity, Activity::Working);
        assert!(
            snap.plugin_outdated,
            "a new turn carries the stale-plugin badge over"
        );

        // A hard clear (PTY death / SessionEnd / restart) drops the badge with everything.
        store.clear("t-stale");
        assert!(
            store.snapshot("t-stale").is_none(),
            "a restart clears the stale-plugin badge — a fresh session is not outdated"
        );
    }

    // --- SECONDARY INFORMATION (the question / the tool) ----------------------------------

    /// The QUESTION is pulled out of the `AskUserQuestion` `tool_input`, in both shapes nyx claims
    /// to understand — the structured `{ questions: [ { question | header } ] }` and the flat
    /// `{ question | prompt }` — and from NOTHING ELSE. Every other shape is "no info": a `None`,
    /// never an error and never a placeholder.
    #[test]
    fn the_question_is_read_from_the_named_fields_and_nothing_else() {
        // The structured shape: the FIRST entry is the question (a row shows one line).
        assert_eq!(
            ask_question_text(&json!({
                "questions": [
                    { "question": "Ship it or hold?", "header": "Release" },
                    { "question": "…and the second one" }
                ]
            })),
            Some("Ship it or hold?".to_string())
        );
        // …falling back to its `header` when the entry has no `question`.
        assert_eq!(
            ask_question_text(&json!({ "questions": [{ "header": "Release" }] })),
            Some("Release".to_string())
        );
        // The flat shapes.
        assert_eq!(
            ask_question_text(&json!({ "question": "Ship it?" })),
            Some("Ship it?".to_string())
        );
        assert_eq!(
            ask_question_text(&json!({ "prompt": "Ship it?" })),
            Some("Ship it?".to_string())
        );
        // UNEXPECTED SHAPES → no info, no panic. A different key, a nested object where a string
        // was expected, a non-object payload, an empty list, blank text.
        assert_eq!(ask_question_text(&json!({ "ask": "Ship it?" })), None);
        assert_eq!(
            ask_question_text(&json!({ "question": { "text": "x" } })),
            None
        );
        assert_eq!(ask_question_text(&json!({ "question": 42 })), None);
        assert_eq!(ask_question_text(&json!({ "questions": [] })), None);
        assert_eq!(ask_question_text(&json!({ "questions": "Ship it?" })), None);
        assert_eq!(ask_question_text(&json!({ "question": "   " })), None);
        // A BLANK first field does not end the search: the next field that carries real words
        // wins. Reading "the first field that exists" instead of "the first non-blank one" would
        // report no question while the words sat in `header`.
        assert_eq!(
            ask_question_text(&json!({ "question": "  ", "header": "Ship it or hold?" })),
            Some("Ship it or hold?".to_string()),
        );
        assert_eq!(
            ask_question_text(&json!({ "question": "", "header": " ", "prompt": "Fold them?" })),
            Some("Fold them?".to_string()),
        );
        assert_eq!(ask_question_text(&json!([1, 2, 3])), None);
        assert_eq!(ask_question_text(&json!("just a string")), None);
        assert_eq!(ask_question_text(&json!(null)), None);
    }

    /// The `Waiting` text and its DISAPPEARANCE. A question (or a permission) is held for exactly
    /// as long as the block lasts: the answer, or any signal that the agent is working again,
    /// takes it away with the yellow — including through the defensive
    /// [`Entry::clear_waiting_if_working`] path, which is the one a "chat about this" decline
    /// travels.
    #[test]
    fn the_waiting_text_lives_and_dies_with_the_waiting() {
        // A QUESTION.
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        let s = store.apply(
            "t1",
            ActivityEvent::AskStarted {
                question: Some("Ship it or hold?".to_string()),
            },
        );
        assert_eq!(s.activity, Activity::Waiting);
        assert_eq!(s.waiting_on.as_deref(), Some("Ship it or hold?"));
        // Answered → Working, and the question is gone.
        let s = store.apply("t1", ActivityEvent::AskFinished);
        assert_eq!(s.activity, Activity::Working);
        assert_eq!(
            s.waiting_on, None,
            "an answered question does not survive into Working"
        );

        // A PERMISSION, lifted by the DEFENSIVE path (a tool starting, not the paired Post).
        let store = AgentActivityStore::new();
        store.apply("t2", ActivityEvent::PromptSubmitted);
        let s = store.apply(
            "t2",
            ActivityEvent::AttentionNeeded {
                permission: Some("Claude needs your permission to use Bash".to_string()),
            },
        );
        assert_eq!(s.activity, Activity::Waiting);
        assert_eq!(
            s.waiting_on.as_deref(),
            Some("Claude needs your permission to use Bash")
        );
        let s = store.apply(
            "t2",
            ActivityEvent::ToolStarted {
                tool: Some("Bash".to_string()),
            },
        );
        assert_eq!(s.activity, Activity::Working);
        assert_eq!(
            s.waiting_on, None,
            "the permission text goes with the yellow it explained"
        );

        // A turn END also drops it (a finished turn asks nothing).
        let store = AgentActivityStore::new();
        store.apply("t3", ActivityEvent::PromptSubmitted);
        store.apply(
            "t3",
            ActivityEvent::AttentionNeeded {
                permission: Some("allow?".to_string()),
            },
        );
        let s = store.apply("t3", ActivityEvent::TurnFinished);
        assert_eq!(s.waiting_on, None);
    }

    /// A `Waiting` whose payload had NO readable text still blocks — the dot is the message, and
    /// the row simply carries no caption. This is the "unexpected payload degrades to no info,
    /// never to an error" contract, driven end to end through `from_hook`.
    #[test]
    fn a_block_with_no_readable_payload_still_blocks_with_no_text() {
        let store = AgentActivityStore::new();
        // An AskUserQuestion whose tool_input is a shape we do not recognise.
        let ev = ActivityEvent::from_hook(
            "PreToolUse",
            HookFacts {
                tool_name: Some(ASK_USER_QUESTION_TOOL),
                tool_input: Some(&json!({ "unexpected": { "nested": true } })),
                ..HookFacts::default()
            },
        )
        .expect("an unexpected payload is still a recognized hook");
        let s = store.apply("t1", ev);
        assert_eq!(s.activity, Activity::Waiting, "the block still happens");
        assert_eq!(
            s.waiting_on, None,
            "…with no info rather than a placeholder"
        );

        // A permission Notification with no `message`.
        let ev = ActivityEvent::from_hook("Notification", notif_facts("permission_prompt"))
            .expect("recognized hook");
        let s = store.apply("t2", ev);
        assert_eq!(s.activity, Activity::Waiting);
        assert_eq!(s.waiting_on, None);
    }

    /// The TOOL IN FLIGHT is the fallback: it is the last tool to have started, and it is dropped
    /// when the last one returns and at every turn end — so it never captions an agent that has
    /// stopped doing that thing.
    #[test]
    fn the_in_flight_tool_is_held_only_while_a_tool_is_running() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        assert_eq!(
            store.snapshot("t1").unwrap().tool,
            None,
            "a turn with no tool running names none"
        );
        let s = store.apply(
            "t1",
            ActivityEvent::ToolStarted {
                tool: Some("Bash".to_string()),
            },
        );
        assert_eq!(s.tool.as_deref(), Some("Bash"));
        // A second tool: the LAST one started is the one named.
        let s = store.apply(
            "t1",
            ActivityEvent::ToolStarted {
                tool: Some("Read".to_string()),
            },
        );
        assert_eq!(s.tool.as_deref(), Some("Read"));
        // One returns — still one in flight, so the name stays.
        let s = store.apply("t1", ActivityEvent::ToolFinished);
        assert_eq!(s.tool.as_deref(), Some("Read"), "a tool is still running");
        // The last returns → nothing in flight, no name.
        let s = store.apply("t1", ActivityEvent::ToolFinished);
        assert_eq!(
            s.tool, None,
            "with nothing in flight there is no tool to name"
        );

        // A turn end drops it too, even with a Post lost to an interrupt.
        store.apply(
            "t1",
            ActivityEvent::ToolStarted {
                tool: Some("Bash".to_string()),
            },
        );
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.tool, None);
        // …and a new turn starts with a clean slate.
        let s = store.apply("t1", ActivityEvent::PromptSubmitted);
        assert_eq!(s.tool, None);
        assert_eq!(s.waiting_on, None);
    }

    /// THE RUNTIME-ONLY CONTRACT still holds with the added fields: the store is EMPTY at boot,
    /// and a hard `clear` (PTY death / SessionEnd / close) takes the texts with everything else.
    /// Nothing here is persisted — there is no column, no migration, nothing to survive a restart.
    #[test]
    fn the_secondary_information_is_runtime_only() {
        let store = AgentActivityStore::new();
        assert!(
            store.snapshot_all().is_empty(),
            "a fresh store holds no text for anyone"
        );
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply(
            "t1",
            ActivityEvent::AskStarted {
                question: Some("Ship it?".to_string()),
            },
        );
        assert_eq!(
            store.snapshot("t1").unwrap().waiting_on.as_deref(),
            Some("Ship it?")
        );
        store.clear("t1");
        assert!(
            store.snapshot("t1").is_none(),
            "a clear drops the question with the entry — nothing outlives the session"
        );
        // A brand-new store over the same terminal id knows nothing (the boot state).
        assert!(AgentActivityStore::new().snapshot("t1").is_none());
    }
}
