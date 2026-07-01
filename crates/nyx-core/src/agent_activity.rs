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
const SUBAGENT_DONE_NOTIFICATION_TYPES: &[&str] =
    &["background_subagent_complete", "subagent_complete", "agent_complete"];

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

/// The activity SNAPSHOT the host surfaces to the front for one terminal: the resolved
/// [`Activity`] kind plus the focus-aware "response ready" unread bit. The `in_flight`
/// counter is an internal implementation detail and is NOT surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy)]
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
}

impl Entry {
    /// A fresh entry for a brand-new turn (`UserPromptSubmit`): `Working`, no tools/
    /// sub-agents in flight, no pending question, no deferred turn-end, ready cleared (a new
    /// turn supersedes a prior answer). `plugin_outdated` is CARRIED OVER from the prior
    /// entry — a new turn does NOT restart the session, so the stale-plugin verdict (set once
    /// at SessionStart) persists until the session actually restarts (which clears the entry).
    fn new_turn(plugin_outdated: bool) -> Self {
        Self {
            base: Activity::Working,
            in_flight: 0,
            subagents_in_flight: 0,
            ask_in_flight: false,
            ready_pending: false,
            pending_is_error: false,
            ready_unread: false,
            error_unread: false,
            plugin_outdated,
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
    fn clear_waiting_if_working(&mut self) {
        if self.base == Activity::Waiting {
            self.base = Activity::Working;
        }
    }
}

/// The normalized activity event parsed from a Claude hook payload. Distinct from the
/// SESSION-lifecycle [`crate::agent::AgentEvent`] (Start/End): these are the per-TURN
/// hooks that drive the live dot, NOT the persisted session row.
///
/// Several events carry a discriminator read from the hook payload (the tool name, the
/// notification type) so the same `hook_event_name` can mean different things — the core
/// reads `args.tool_name` / `args.notification_type` and builds the right variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityEvent {
    /// `UserPromptSubmit` — the user sent a prompt; a NEW turn begins → `Working`,
    /// `in_flight` reset to 0, ready cleared.
    PromptSubmitted,
    /// `PreToolUse` for an ordinary tool — one more tool in flight (`in_flight += 1`),
    /// the dot stays/goes `Working`.
    ToolStarted,
    /// `PreToolUse` for `AskUserQuestion` — Claude asks the user and blocks → `Waiting`,
    /// remembering the question is in flight so the paired `PostToolUse` lifts it.
    AskStarted,
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
    /// user → `Waiting`.
    AttentionNeeded,
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
    pub fn from_hook(
        name: &str,
        tool_name: Option<&str>,
        notification_type: Option<&str>,
    ) -> Option<Self> {
        match name {
            "UserPromptSubmit" => Some(ActivityEvent::PromptSubmitted),
            "PreToolUse" => {
                if tool_name == Some(ASK_USER_QUESTION_TOOL) {
                    Some(ActivityEvent::AskStarted)
                } else {
                    Some(ActivityEvent::ToolStarted)
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
                    Some(ActivityEvent::AttentionNeeded)
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

    /// BACK-COMPAT thin wrapper: map a hook name with no payload discriminators. A
    /// `PreToolUse`/`PostToolUse` without a `tool_name` is an ordinary tool; a
    /// `Notification` without a type is a no-op. Used where only the name is available.
    pub fn from_hook_event_name(name: &str) -> Option<Self> {
        Self::from_hook(name, None, None)
    }
}

/// The RUNTIME agent-activity registry: an in-memory `terminal_id → Entry` map, EMPTY
/// at construction (the anti-phantom boot guarantee — nothing is read from the DB). The
/// host holds ONE behind an `Arc`; the MCP dispatcher WRITES it ([`Self::apply`]) when
/// a Claude activity hook arrives, the read path SNAPSHOTS it for the front
/// ([`Self::snapshot`] / [`Self::snapshot_all`]), and the PTY/lifecycle layer CLEARS it
/// ([`Self::clear`]) on PTY death / SessionEnd / close.
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
        let mut entry = map.get(terminal_id).copied().unwrap_or(Entry {
            base: Activity::Idle,
            in_flight: 0,
            subagents_in_flight: 0,
            ask_in_flight: false,
            ready_pending: false,
            pending_is_error: false,
            ready_unread: false,
            error_unread: false,
            plugin_outdated: false,
        });

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
            ActivityEvent::ToolStarted => {
                entry.in_flight = entry.in_flight.saturating_add(1);
                entry.clear_waiting_if_working();
                if entry.base == Activity::Idle {
                    entry.base = Activity::Working;
                }
            }
            // AskUserQuestion in flight → explicit Waiting (yellow), remember it so the
            // paired Post lifts it. It still counts as in-flight work.
            ActivityEvent::AskStarted => {
                entry.in_flight = entry.in_flight.saturating_add(1);
                entry.ask_in_flight = true;
                entry.base = Activity::Waiting;
            }
            // An ordinary tool returned: one fewer in flight. The base stays Working (if
            // it was) — when in_flight hits 0 the dot stays Working until Stop (Claude is
            // generating). A tool RETURNING is also a "working again" signal, so it lifts a
            // stale `Waiting` (#26): a "chat about this" decline that resumes work via a
            // tool's PostToolUse must not leave the yellow stuck.
            ActivityEvent::ToolFinished => {
                entry.in_flight = entry.in_flight.saturating_sub(1);
                entry.clear_waiting_if_working();
            }
            // The user answered (or declined — "chat about this") an AskUserQuestion: lift
            // the Waiting back to Working and decrement the in-flight count for that tool.
            // The lift is now UNCONDITIONAL on the resume signal (#26): a decline resumes work
            // and the yellow must clear even if `ask_in_flight` was somehow already dropped.
            ActivityEvent::AskFinished => {
                entry.in_flight = entry.in_flight.saturating_sub(1);
                entry.ask_in_flight = false;
                entry.clear_waiting_if_working();
            }
            // A sub-agent started → one more sub-agent in flight (Working). Counted in the
            // DEDICATED sub-agent counter (NOT the tool `in_flight`) so a background
            // sub-agent outlives the main turn's Stop (#21).
            ActivityEvent::SubagentStarted => {
                entry.subagents_in_flight = entry.subagents_in_flight.saturating_add(1);
                // A sub-agent starting is a "working again" signal → lift a stale Waiting (#26).
                entry.clear_waiting_if_working();
                if entry.base == Activity::Idle {
                    entry.base = Activity::Working;
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
            ActivityEvent::AttentionNeeded => {
                entry.base = Activity::Waiting;
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

        map.insert(terminal_id.to_string(), entry);
        entry.snapshot()
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
                            base: Activity::Idle,
                            in_flight: 0,
                            subagents_in_flight: 0,
                            ask_in_flight: false,
                            ready_pending: false,
                            pending_is_error: false,
                            ready_unread: false,
                            error_unread: false,
                            plugin_outdated: true,
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
        // PreToolUse: ordinary tool vs AskUserQuestion.
        assert_eq!(
            ActivityEvent::from_hook("PreToolUse", Some("Bash"), None),
            Some(ActivityEvent::ToolStarted)
        );
        assert_eq!(
            ActivityEvent::from_hook("PreToolUse", Some(ASK_USER_QUESTION_TOOL), None),
            Some(ActivityEvent::AskStarted)
        );
        // PostToolUse + PostToolUseFailure, ordinary vs Ask.
        assert_eq!(
            ActivityEvent::from_hook("PostToolUse", Some("Bash"), None),
            Some(ActivityEvent::ToolFinished)
        );
        assert_eq!(
            ActivityEvent::from_hook("PostToolUseFailure", Some("Bash"), None),
            Some(ActivityEvent::ToolFinished)
        );
        assert_eq!(
            ActivityEvent::from_hook("PostToolUse", Some(ASK_USER_QUESTION_TOOL), None),
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
            ActivityEvent::from_hook("Notification", None, Some("permission_prompt")),
            Some(ActivityEvent::AttentionNeeded)
        );
        assert_eq!(
            ActivityEvent::from_hook("Notification", None, Some("elicitation_dialog")),
            Some(ActivityEvent::AttentionNeeded)
        );
        assert_eq!(
            ActivityEvent::from_hook("Notification", None, Some("idle_prompt")),
            Some(ActivityEvent::TurnFinished)
        );
        // A background sub-agent completion notification (defensive, best-guess strings) maps
        // to SubagentFinished — the same decrement as a SubagentStop.
        for t in SUBAGENT_DONE_NOTIFICATION_TYPES {
            assert_eq!(
                ActivityEvent::from_hook("Notification", None, Some(t)),
                Some(ActivityEvent::SubagentFinished),
                "a '{t}' notification decrements the sub-agent counter"
            );
        }
        // An unknown / absent notification type does not move the dot.
        assert_eq!(
            ActivityEvent::from_hook("Notification", None, Some("something_else")),
            None
        );
        assert_eq!(ActivityEvent::from_hook("Notification", None, None), None);
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
        assert!(s.ready_unread, "a finished turn raises the ready notification");
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
        store.apply("t1", ActivityEvent::ToolStarted); // a 10-minute tool begins.
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
        store.apply("t1", ActivityEvent::ToolStarted);
        store.apply("t1", ActivityEvent::ToolStarted); // two tools in flight.
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
        let s = store.apply("t1", ActivityEvent::AskStarted);
        assert_eq!(s.activity, Activity::Waiting, "asking the user blocks → yellow");
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
        store.apply("t1", ActivityEvent::ToolStarted); // a background tool.
        store.apply("t1", ActivityEvent::AskStarted); // + a question → Waiting wins.
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
        let s = store.apply("t1", ActivityEvent::AttentionNeeded); // permission_prompt.
        assert_eq!(s.activity, Activity::Waiting);
        assert!(!s.ready_unread, "a permission prompt is not a finished response");

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
        store.apply("t1", ActivityEvent::AskStarted); // Waiting (the asked question).
        assert_eq!(store.snapshot("t1").unwrap().activity, Activity::Waiting);
        let s = store.apply("t1", ActivityEvent::ToolStarted); // resumed via a tool.
        assert_eq!(
            s.activity,
            Activity::Working,
            "a tool starting after a decline lifts the stale yellow"
        );

        // A tool RETURNING after the Waiting lifts it (a permission Notification block this
        // time — no paired Ask Post — and the resume arrives as a tool's PostToolUse).
        let store = AgentActivityStore::new();
        store.apply("t2", ActivityEvent::PromptSubmitted);
        store.apply("t2", ActivityEvent::ToolStarted); // a tool is in flight.
        store.apply("t2", ActivityEvent::AttentionNeeded); // permission_prompt → Waiting.
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
        store.apply("t3", ActivityEvent::AttentionNeeded); // Waiting.
        let s = store.apply("t3", ActivityEvent::SubagentStarted);
        assert_eq!(
            s.activity,
            Activity::Working,
            "a sub-agent starting lifts the stale yellow"
        );

        // A NEW PROMPT (the existing reset path) also clears a stale Waiting.
        let store = AgentActivityStore::new();
        store.apply("t4", ActivityEvent::PromptSubmitted);
        store.apply("t4", ActivityEvent::AttentionNeeded); // Waiting.
        let s = store.apply("t4", ActivityEvent::PromptSubmitted);
        assert_eq!(s.activity, Activity::Working, "a new prompt resets the yellow");
    }

    /// #26 — `AskFinished` lifts the yellow UNCONDITIONALLY (the decline path), even though
    /// the existing answered-question test already passes: a decline ("chat about this") that
    /// still routes through the AskUserQuestion `PostToolUse` must resume work, not stay
    /// yellow, regardless of the internal `ask_in_flight` bookkeeping.
    #[test]
    fn ask_finished_lifts_waiting_on_decline() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::AskStarted); // the question → Waiting.
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
        store.apply("t1", ActivityEvent::ToolStarted);
        let s = store.apply("t1", ActivityEvent::TurnFinished); // Stop.
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread);
        assert!(!s.error_unread);
        // A fresh turn then StopFailure idles + resets too, but raises the RED, not the green.
        store.apply("t2", ActivityEvent::PromptSubmitted);
        store.apply("t2", ActivityEvent::ToolStarted);
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
        assert_eq!(s.activity, Activity::Idle, "a failed turn ends the turn → Idle");
        assert!(s.error_unread, "an errored turn raises the red error dot");
        assert!(!s.ready_unread, "an errored turn does NOT raise the green ready dot");
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
        assert!(!s.error_unread, "a successful turn must NOT raise the red error dot");
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
        assert!(!s.error_unread, "the red is DEFERRED while a background sub-agent runs");
        assert!(!s.ready_unread);
        // The background sub-agent finishes → the deferred turn-end applies as RED, not green.
        let s = store.apply("t1", ActivityEvent::SubagentFinished);
        assert_eq!(s.activity, Activity::Idle, "the last sub-agent settles the errored turn");
        assert!(s.error_unread, "the deferred RED is raised when the last sub-agent finishes");
        assert!(!s.ready_unread, "a deferred FAILED turn settles red, never green");
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
        assert!(!snap.error_unread, "viewing the terminal clears the red error dot");
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
        assert_eq!(s.activity, Activity::Idle, "the last sub-agent settles the turn");
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
        assert!(!s.ready_unread, "a sub-agent stop with no deferred Stop raises nothing");
        // The main turn then ends with no sub-agent in flight → immediate Idle + green.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Idle);
        assert!(s.ready_unread, "a synchronous sub-agent greens at the Stop as before");
    }

    /// Sub-agents and TOOLS are counted SEPARATELY: a `Stop` resets the tool counter (a lost
    /// tool Post is forgiven) but must NOT clear a still-running sub-agent — the dot stays
    /// Working until that sub-agent's own SubagentStop, then greens.
    #[test]
    fn stop_resets_tools_but_not_subagents() {
        let store = AgentActivityStore::new();
        store.apply("t1", ActivityEvent::PromptSubmitted);
        store.apply("t1", ActivityEvent::SubagentStarted); // background sub-agent.
        store.apply("t1", ActivityEvent::ToolStarted); // a tool whose Post will be "lost".
        // Stop: the tool bookkeeping is reset, but the sub-agent survives → still Working.
        let s = store.apply("t1", ActivityEvent::TurnFinished);
        assert_eq!(s.activity, Activity::Working, "the sub-agent keeps it Working");
        assert!(!s.ready_unread);
        // A stray late tool Post must not underflow nor settle the turn.
        let s = store.apply("t1", ActivityEvent::ToolFinished);
        assert_eq!(s.activity, Activity::Working, "stray Post does not settle a deferred turn");
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
        assert!(!s.ready_unread, "no green until the LAST sub-agent finishes");
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
            let ev = ActivityEvent::from_hook(name, None, ntype).expect("recognized hook");
            store.apply("t1", ev)
        };
        apply_hook("UserPromptSubmit", None);
        apply_hook("SubagentStart", None);
        let s = apply_hook("Stop", None);
        assert_eq!(s.activity, Activity::Working, "deferred while the background runs");
        assert!(!s.ready_unread);
        // The background completes via a NOTIFICATION (not a SubagentStop) → counter drops,
        // deferred green lifts.
        let s = apply_hook("Notification", Some("background_subagent_complete"));
        assert_eq!(s.activity, Activity::Idle, "notification completion settles the turn");
        assert!(s.ready_unread, "the deferred green is raised via the completion notification");
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
        store.apply("t1", ActivityEvent::AskStarted); // Waiting + in_flight=1 dangling.
        let s = store.apply("t1", ActivityEvent::PromptSubmitted);
        assert_eq!(s.activity, Activity::Working, "a new turn resets to Working");
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
        store.apply("t1", ActivityEvent::ToolStarted); // a tool in flight, no Stop (a kill).
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
        assert!(!snap.ready_unread, "viewing the terminal clears the green dot");
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
        store.apply("t2", ActivityEvent::AttentionNeeded);
        assert_eq!(store.snapshot("t2").unwrap().activity, Activity::Waiting);
        store.clear("t2");
        assert!(store.snapshot("t2").is_none(), "only a hard clear drops it");
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
        store.apply("t-d", ActivityEvent::AttentionNeeded); // Waiting.

        let all = store.snapshot_all();
        let ids: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["t-a", "t-b", "t-d"], "ordered, only live terminals");
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
        let snap = store.snapshot("t-stale").expect("stale badge materializes an entry");
        assert_eq!(snap.activity, Activity::Idle, "the badge does not imply working");
        assert!(!snap.ready_unread);
        assert!(snap.plugin_outdated, "the stale-plugin badge is set");
        let all = store.snapshot_all();
        assert!(
            all.iter().any(|(id, s)| id == "t-stale" && s.plugin_outdated),
            "an idle-but-stale terminal is surfaced by snapshot_all"
        );

        // The badge SURVIVES a turn (a new prompt is not a session restart).
        store.apply("t-stale", ActivityEvent::PromptSubmitted);
        let snap = store.snapshot("t-stale").unwrap();
        assert_eq!(snap.activity, Activity::Working);
        assert!(snap.plugin_outdated, "a new turn carries the stale-plugin badge over");

        // A hard clear (PTY death / SessionEnd / restart) drops the badge with everything.
        store.clear("t-stale");
        assert!(
            store.snapshot("t-stale").is_none(),
            "a restart clears the stale-plugin badge — a fresh session is not outdated"
        );
    }
}
