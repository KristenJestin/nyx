//! The agent-session RESUME DECISION (PRD-5 Phase 3, #5 + #6) — the PURE policy
//! that decides, for ONE terminal at relaunch, whether nyx should resume an agent
//! session and (if so) what shell command to inject; and the symmetric CLOSE-WARNING
//! policy that decides whether closing should warn the user about an unsaved live
//! session.
//!
//! # Why a pure module
//!
//! The execution side (spawning the shell, writing the resume line) and the DB side
//! (reading the option, the session row) live in the bridge / `db`. This module owns
//! ONLY the decision so it is exhaustively unit-testable WITHOUT a PTY, a Tauri app,
//! or even a database — the inputs are plain values. The bridge gathers those values
//! (project option, session state, whether the terminal was closed voluntarily, the
//! target shell) and calls [`decide_resume`]; the adapter builds the actual command
//! string (so the agent-specific shape stays in `agent.rs`).
//!
//! # The decision matrix (mirrors the PRD Impl Decisions)
//!
//! Resume is attempted only when ALL hold:
//!   * the project option is ON (`resume_agent_sessions = true`; a terminal with no
//!     project is OFF by construction — the caller passes `false`);
//!   * the terminal was NOT closed voluntarily by the user (a deliberate close must
//!     not come back — PRD: "terminal fermé volontairement ne revient pas");
//!   * the session is a RESUME CANDIDATE: `active` (incl. left-as-is after an app
//!     kill) or `unknown` (stale `active`, probable kill — still a candidate, but the
//!     doubt is flagged via [`ResumeDecision::resume_uncertain`]). A clean `ended`
//!     session or a prior `resume_failed` is NOT resumed automatically.
//!   * the target shell can host the resume. EVERY supported shell now qualifies —
//!     native Linux/WSL AND native Windows PowerShell/cmd: `claude --resume <uuid>` is
//!     a SHELL-AGNOSTIC invocation (`claude` on PATH + one `[A-Za-z0-9_-]` argument, no
//!     Unix syntax), and the line is injected with a real CR (`\r`, finding #76) so
//!     PSReadLine executes it instead of buffering it in a `>>` continuation prompt.
//!
//! Each non-resume path carries a [`ResumeSkipReason`] so the caller can log/observe
//! WHY a session was not resumed (and so the tests pin the exact branch).

use crate::agent::AgentAdapter;

/// The normalized lifecycle state of the candidate session, as the decision needs
/// it. Mirrors the `agent_sessions.state` vocabulary (`active` | `ended` | `unknown`
/// | `resume_failed`) — kept as a small enum here so the policy is a pure `match` and
/// an invalid string can't reach it. Built from the DB string via [`Self::from_db`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// In progress, or left as-is after an app kill — the primary resume candidate.
    Active,
    /// Clean `SessionEnd` observed — nothing to resume.
    Ended,
    /// Was `active`, went stale past the péremption threshold without a clean end
    /// (probable kill, state unconfirmed). STILL a resume candidate, but the decision
    /// flags the uncertainty.
    Unknown,
    /// A resume was already attempted and failed — not retried automatically.
    ResumeFailed,
}

impl SessionState {
    /// Map a `agent_sessions.state` string onto the enum, or `None` for an
    /// unrecognized value (defensive — the DB CHECK already constrains it).
    pub fn from_db(state: &str) -> Option<Self> {
        match state {
            crate::db::SESSION_STATE_ACTIVE => Some(SessionState::Active),
            crate::db::SESSION_STATE_ENDED => Some(SessionState::Ended),
            crate::db::SESSION_STATE_UNKNOWN => Some(SessionState::Unknown),
            crate::db::SESSION_STATE_RESUME_FAILED => Some(SessionState::ResumeFailed),
            _ => None,
        }
    }
}

/// The execution target for the resume command. ALL classified shells can host the
/// resume: a native Linux shell, WSL under Windows, AND a native Windows PowerShell/cmd.
/// `claude --resume <uuid>` is SHELL-AGNOSTIC (just `claude` on PATH + one safe id
/// argument, no Unix syntax), and the resume line is injected with a real CR (`\r`,
/// finding #76) so it executes on PSReadLine instead of stacking in a `>>` prompt — so
/// the earlier Windows-native exclusion is obsolete and the variant is kept only to
/// preserve the classification (every target now resumes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeTarget {
    /// A native Linux shell, or a WSL shell under Windows — resume is supported here.
    UnixOrWsl,
    /// A native Windows shell (PowerShell / cmd) — now ALSO resume-capable (the
    /// shell-agnostic `claude --resume` + CR injection works here too, finding #83).
    WindowsNative,
}

impl ResumeTarget {
    /// Classify the resolved shell command line into a [`ResumeTarget`]. WSL is
    /// detected from the shell program: `wsl` / `wsl.exe` / `bash.exe` (the Windows
    /// launcher that drops into the default WSL distro) → [`ResumeTarget::UnixOrWsl`].
    /// `pwsh`/`powershell`/`cmd` (with or without `.exe`) → [`ResumeTarget::WindowsNative`].
    /// Anything else (a bare `bash`/`sh`/`zsh`/an absolute Unix path) is treated as a
    /// native Unix shell → [`ResumeTarget::UnixOrWsl`].
    pub fn classify_shell(shell: &str) -> Self {
        let first = shell.split_whitespace().next().unwrap_or(shell);
        let base = first
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(first)
            .to_ascii_lowercase();
        let base = base.strip_suffix(".exe").unwrap_or(&base);
        match base {
            // Windows-native shells — now resume-capable (finding #83).
            "pwsh" | "powershell" | "cmd" => ResumeTarget::WindowsNative,
            // `wsl` / `bash.exe` launch a WSL distro on Windows; everything else
            // (bash/sh/zsh/fish/an absolute Unix path) is a native Unix shell.
            _ => ResumeTarget::UnixOrWsl,
        }
    }

    /// Can this target host the resume? EVERY classified target now can: native
    /// Unix/WSL and native Windows PowerShell/cmd alike (finding #83 — `claude --resume`
    /// is shell-agnostic and the CR injection from #76 makes it execute on PSReadLine).
    fn supports_resume(self) -> bool {
        matches!(self, ResumeTarget::UnixOrWsl | ResumeTarget::WindowsNative)
    }
}

/// The reason a resume was NOT attempted — one variant per non-resume branch so the
/// caller can log it and the tests can pin the exact path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeSkipReason {
    /// The project option is OFF (or the terminal has no project → OFF by construction).
    OptionOff,
    /// The terminal was closed voluntarily by the user — a deliberate close stays closed.
    ClosedVoluntarily,
    /// The session is not a resume candidate: a clean `ended` session.
    SessionEnded,
    /// The session is not a resume candidate: a prior resume already failed.
    AlreadyResumeFailed,
    /// The candidate session has NO conversation on disk: its `transcript_path` does
    /// not exist (Claude only writes the transcript on the first message, so a session
    /// the user never typed into has an id but no `.jsonl`; the file is also gone if the
    /// user deleted the conversation). Resuming such a session makes `claude --resume`
    /// fail with "No conversation found" and breaks the respawned terminal — so skip it.
    NoConversation,
    /// The execution target cannot host the resume. Currently NO classified target
    /// trips this (native Unix/WSL and native Windows PowerShell/cmd all resume since
    /// finding #83) — kept as the seam for any future non-resumable target.
    UnsupportedTarget,
    /// The adapter cannot build an exact-resume command for this session (e.g. an
    /// empty external id, or an agent without exact-resume support).
    NoResumeCommand,
}

/// The outcome of [`decide_resume`]. Either RESUME with the exact command to inject,
/// or SKIP with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeDecision {
    /// Resume: inject `command` into the respawned shell. `resume_uncertain` is `true`
    /// when the candidate was an `unknown` (stale) session — the resume is still
    /// attempted, but the caller may surface the doubt.
    Resume {
        /// The exact shell command line (e.g. `claude --resume <id>`).
        command: String,
        /// `true` if the candidate session was `unknown` (péremption / probable kill).
        resume_uncertain: bool,
    },
    /// Do not resume; `reason` records which branch was taken.
    Skip(ResumeSkipReason),
}

impl ResumeDecision {
    /// Convenience: is this a resume?
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_resume(&self) -> bool {
        matches!(self, ResumeDecision::Resume { .. })
    }
}

/// The inputs the bridge gathers for ONE terminal's resume decision. Plain values so
/// the policy is testable without a DB / PTY.
#[derive(Debug, Clone)]
pub struct ResumeInputs<'a> {
    /// The project option (`projects.resume_agent_sessions`). A terminal with no
    /// project is OFF by construction — the caller passes `false`.
    pub project_resume_on: bool,
    /// Was this terminal closed VOLUNTARILY by the user? A deliberate close must not
    /// be resumed (PRD). An app kill / crash leaves this `false` (the session row
    /// stays `active`, resumable next launch).
    pub closed_voluntarily: bool,
    /// The candidate session's lifecycle state.
    pub session_state: SessionState,
    /// The agent's OWN session id — what the resume command is built from.
    pub external_session_id: &'a str,
    /// Does the session's `transcript_path` EXIST on disk? The bridge does the single
    /// `stat` on the path already captured in `agent_sessions.transcript_path` and
    /// passes the result here (the policy stays pure — no FS access). `false` when the
    /// session has no transcript yet (user never typed) or it was deleted; `true` when a
    /// real conversation exists to resume. A candidate with NO transcript is skipped
    /// (`NoConversation`) so nyx never injects a `claude --resume` that would fail with
    /// "No conversation found" and break the respawned terminal.
    pub transcript_exists: bool,
    /// The execution target (native Unix/WSL vs. native Windows).
    pub target: ResumeTarget,
}

/// Decide whether to RESUME the given session for ONE terminal at relaunch, and with
/// what command. The single policy point (PRD-5 #5). `adapter` builds the agent-
/// specific exact-resume command (so the agent shape stays in `agent.rs`); the gates
/// (option, voluntary close, candidate state, target) are applied here in order so
/// each non-resume path yields a precise [`ResumeSkipReason`].
///
/// Order of the gates is deliberate (most-decisive / cheapest first):
///   1. project option OFF        → `Skip(OptionOff)`
///   2. terminal closed by user   → `Skip(ClosedVoluntarily)`
///   3. session not a candidate   → `Skip(SessionEnded | AlreadyResumeFailed)`
///   4. no conversation on disk   → `Skip(NoConversation)`
///   5. target can't host resume  → `Skip(UnsupportedTarget)`
///   6. adapter builds no command → `Skip(NoResumeCommand)`
///
/// otherwise → `Resume { command, resume_uncertain }`.
pub fn decide_resume(inputs: &ResumeInputs, adapter: &dyn AgentAdapter) -> ResumeDecision {
    if !inputs.project_resume_on {
        return ResumeDecision::Skip(ResumeSkipReason::OptionOff);
    }
    if inputs.closed_voluntarily {
        return ResumeDecision::Skip(ResumeSkipReason::ClosedVoluntarily);
    }
    // Only `ended`/`resume_failed` are non-candidates; `active`/`unknown` fall through to
    // the resume path below. A direct match keeps this exhaustive — no unreachable arm
    // returning a misleading skip reason.
    match inputs.session_state {
        SessionState::Ended => return ResumeDecision::Skip(ResumeSkipReason::SessionEnded),
        SessionState::ResumeFailed => {
            return ResumeDecision::Skip(ResumeSkipReason::AlreadyResumeFailed)
        }
        SessionState::Active | SessionState::Unknown => {}
    }
    if !inputs.transcript_exists {
        // The session id exists but there is no conversation on disk (never typed into,
        // or deleted): `claude --resume` would fail with "No conversation found" and
        // break the respawned terminal. Skip rather than inject a doomed resume.
        return ResumeDecision::Skip(ResumeSkipReason::NoConversation);
    }
    if !inputs.target.supports_resume() {
        return ResumeDecision::Skip(ResumeSkipReason::UnsupportedTarget);
    }
    let Some(command) = adapter.build_resume_command(inputs.external_session_id) else {
        return ResumeDecision::Skip(ResumeSkipReason::NoResumeCommand);
    };
    ResumeDecision::Resume {
        command,
        resume_uncertain: inputs.session_state == SessionState::Unknown,
    }
}

// --- Close-warning policy (PRD-5 #6) -------------------------------------
//
// The symmetric decision: at app/terminal close, WARN the user about a live agent
// session that nyx will NOT bring back. Per the PRD, the warning fires ONLY when a
// session is still live (`active` or `unknown` — not cleanly `ended`) AND the project
// does NOT auto-resume (because if it resumes, closing loses nothing). The message
// names the AGENT and the TERMINAL so the user knows what they would drop.

/// A human label for an `agent_kind`, distinguishing Claude / Codex / OpenCode /
/// custom in the close-warning message (PRD-5 #6: "Message distingue Claude/Codex/
/// OpenCode/custom"). Falls back to the raw kind for any future/unknown value.
pub fn agent_label(agent_kind: &str) -> &str {
    match agent_kind {
        crate::db::AGENT_KIND_CLAUDE_CODE => "Claude Code",
        crate::db::AGENT_KIND_CODEX => "Codex",
        crate::db::AGENT_KIND_OPENCODE => "OpenCode",
        crate::db::AGENT_KIND_CUSTOM => "a custom agent",
        other => other,
    }
}

/// Should closing WARN about this session? `true` iff the session is still LIVE
/// (`active` or `unknown`) AND the project does NOT auto-resume. A cleanly `ended` or
/// already-`resume_failed` session never warns (nothing live to lose); a session in a
/// resume-ON project never warns (nyx will bring it back). The pure gate behind the
/// close-warning command.
pub fn should_warn_on_close(session_state: SessionState, project_resume_on: bool) -> bool {
    if project_resume_on {
        return false;
    }
    matches!(session_state, SessionState::Active | SessionState::Unknown)
}

/// Build the close-warning MESSAGE for one live session (PRD-5 #6). Names the AGENT
/// (Claude/Codex/OpenCode/custom via [`agent_label`]) and the TERMINAL (its label when
/// set, else a short id), and notes the workspace when known. `terminal_label` is the
/// terminal's display label (`None` → fall back to the id); `workspace_name` is the
/// bound workspace's name when the session is attached.
pub fn close_warning_message(
    agent_kind: &str,
    terminal_label: Option<&str>,
    terminal_id: &str,
    workspace_name: Option<&str>,
) -> String {
    let agent = agent_label(agent_kind);
    let terminal = match terminal_label {
        Some(l) if !l.trim().is_empty() => l.to_string(),
        _ => format!("terminal {terminal_id}"),
    };
    match workspace_name {
        Some(ws) if !ws.trim().is_empty() => format!(
            "{agent} has an active session in {terminal} (workspace {ws}) that won't be resumed."
        ),
        _ => format!("{agent} has an active session in {terminal} that won't be resumed."),
    }
}

// --- Workspace context restore (v2.2 task 01KWHPV33M4H5YSMGYNW9TG6J0) --------
//
// The per-workspace RESTORE PLAN the cockpit reads after a restart to offer the user what
// to resume. It REUSES the existing resume policy [`decide_resume`] verbatim — the SAME gates
// (`resume_agent_sessions` project option, candidate state, transcript-on-disk, target) — so
// there is NO third resume system (Q-011). It only aggregates the per-session decisions for
// ONE workspace's candidate sessions into a plan the user acts on.

/// The candidate inputs the caller gathers per session (from
/// [`crate::db::resume_candidates_for_workspace`] + a transcript `stat`), plus the resolved
/// shell — everything [`decide_resume`] needs, without this module touching the DB/FS. Plain
/// borrowed values so the plan builder stays pure and exhaustively unit-testable.
#[derive(Debug, Clone)]
pub struct WorkspaceResumeCandidate<'a> {
    /// The hosting terminal id (so the plan maps back to which pane resumes).
    pub terminal_id: &'a str,
    /// The `agent_sessions.id` this decision concerns.
    pub session_id: &'a str,
    /// The agent kind — used to resolve the adapter that builds the exact-resume command.
    pub agent_kind: &'a str,
    /// The agent's OWN session id (`external_session_id`) — the resume command's argument.
    pub external_session_id: &'a str,
    /// The candidate session's lifecycle state (`active`/`unknown`; `ended`/`resume_failed`
    /// never reach here — the query filters to live candidates only).
    pub session_state: SessionState,
    /// Whether the project opts into `resume_agent_sessions` (OFF for a loose terminal).
    pub project_resume_on: bool,
    /// Whether the session's `transcript_path` EXISTS on disk (the caller `stat`s it; the
    /// policy stays pure). `false` → the decision skips with `NoConversation`.
    pub transcript_exists: bool,
    /// The resolved execution target (native Unix/WSL vs. native Windows).
    pub target: ResumeTarget,
}

/// One entry of the workspace restore plan: the [`decide_resume`] verdict for ONE candidate
/// session, keyed to its terminal + session so the cockpit can present + act on it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceResumeEntry {
    /// The terminal hosting the candidate session.
    pub terminal_id: String,
    /// The `agent_sessions.id` this entry concerns.
    pub session_id: String,
    /// The agent kind (Claude / Codex / …).
    pub agent_kind: String,
    /// The exact resume command to inject when `resume` is true (`claude --resume <id>`),
    /// else `None` (the session is not resumable — see `skip_reason`).
    pub resume_command: Option<String>,
    /// `true` when the policy decided to RESUME this session.
    pub resume: bool,
    /// `true` when the resumed candidate was `unknown` (stale / probable kill) — the resume
    /// is still offered, but the doubt is surfaced (parity with `ResumeDecision::resume_uncertain`).
    pub uncertain: bool,
    /// When NOT resuming, the machine reason (e.g. `option_off`, `no_conversation`), so the
    /// cockpit can explain WHY a live session is not offered for resume. `None` when resuming.
    pub skip_reason: Option<&'static str>,
}

/// A stable, serialisable tag for a [`ResumeSkipReason`] (for the plan entry + the cockpit).
fn skip_reason_tag(reason: ResumeSkipReason) -> &'static str {
    match reason {
        ResumeSkipReason::OptionOff => "option_off",
        ResumeSkipReason::ClosedVoluntarily => "closed_voluntarily",
        ResumeSkipReason::SessionEnded => "session_ended",
        ResumeSkipReason::AlreadyResumeFailed => "already_resume_failed",
        ResumeSkipReason::NoConversation => "no_conversation",
        ResumeSkipReason::UnsupportedTarget => "unsupported_target",
        ResumeSkipReason::NoResumeCommand => "no_resume_command",
    }
}

/// Build the workspace RESTORE PLAN: run the EXISTING [`decide_resume`] policy over each of a
/// workspace's candidate sessions and collect the per-session verdicts the cockpit offers to
/// the user (task `01KWHPV33M4H5YSMGYNW9TG6J0` — "sessions agents reprenables"). `resolve` maps
/// an `agent_kind` to its [`AgentAdapter`], and IS the single source of truth for that mapping:
/// pass [`crate::agent::AgentRegistry::get`] directly (its borrowed-lifetime signature matches),
/// so adding a resumable adapter to the registry is reflected here with no call-site edit.
/// A kind with no adapter yields a non-resume entry (`no_resume_command`) rather than a panic.
///
/// This function makes NO decision of its own — it delegates every gate to `decide_resume`,
/// so it introduces no new resume policy (Q-011). It is a PURE aggregation: the caller has
/// already gathered the candidates (DB) and probed the transcripts (FS), and the USER chooses
/// what to actually resume from the returned plan (nyx never blindly injects).
///
/// The `resolve` bound is borrowed (not `'static`): the resolved adapter only needs to outlive
/// each per-candidate [`decide_resume`] call, so an `&AgentRegistry`-borrowed adapter is enough.
pub fn workspace_resume_plan<'r>(
    candidates: &[WorkspaceResumeCandidate<'_>],
    resolve: &dyn Fn(&str) -> Option<&'r dyn AgentAdapter>,
) -> Vec<WorkspaceResumeEntry> {
    candidates
        .iter()
        .map(|c| {
            let inputs = ResumeInputs {
                project_resume_on: c.project_resume_on,
                // A terminal reached by the restore scan is `alive` (a voluntary close leaves it
                // `closed`, so it never reaches here) — the voluntary-close gate is structural.
                closed_voluntarily: false,
                session_state: c.session_state,
                external_session_id: c.external_session_id,
                transcript_exists: c.transcript_exists,
                target: c.target,
            };
            let decision = match resolve(c.agent_kind) {
                Some(adapter) => decide_resume(&inputs, adapter),
                // No adapter for this kind → treat as "cannot build a resume command".
                None => ResumeDecision::Skip(ResumeSkipReason::NoResumeCommand),
            };
            match decision {
                ResumeDecision::Resume {
                    command,
                    resume_uncertain,
                } => WorkspaceResumeEntry {
                    terminal_id: c.terminal_id.to_string(),
                    session_id: c.session_id.to_string(),
                    agent_kind: c.agent_kind.to_string(),
                    resume_command: Some(command),
                    resume: true,
                    uncertain: resume_uncertain,
                    skip_reason: None,
                },
                ResumeDecision::Skip(reason) => WorkspaceResumeEntry {
                    terminal_id: c.terminal_id.to_string(),
                    session_id: c.session_id.to_string(),
                    agent_kind: c.agent_kind.to_string(),
                    resume_command: None,
                    resume: false,
                    uncertain: false,
                    skip_reason: Some(skip_reason_tag(reason)),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentRegistry, ClaudeCodeAdapter, GenericAdapter};
    use crate::db;

    /// Baseline inputs that WOULD resume (option ON, not voluntarily closed, active
    /// session, transcript present on disk, valid id, Unix target). Each test flips one
    /// field to pin its branch.
    fn resumable<'a>(id: &'a str) -> ResumeInputs<'a> {
        ResumeInputs {
            project_resume_on: true,
            closed_voluntarily: false,
            session_state: SessionState::Active,
            external_session_id: id,
            transcript_exists: true,
            target: ResumeTarget::UnixOrWsl,
        }
    }

    /// Option ON + active session + Unix target → resume with the EXACT id (not
    /// `--continue`). This is the core "Option ON reprend les sessions actives" +
    /// "Resume utilise l'id exact" done-criterion.
    #[test]
    fn option_on_active_resumes_with_exact_id() {
        let claude = ClaudeCodeAdapter;
        let d = decide_resume(&resumable("sid-exact-1"), &claude);
        assert_eq!(
            d,
            ResumeDecision::Resume {
                command: "claude --resume sid-exact-1".to_string(),
                resume_uncertain: false,
            },
            "option ON + active resumes with the exact id, not --continue"
        );
        assert!(d.is_resume());
    }

    /// Option OFF → never resumes (done-criterion "Option OFF ne reprend pas").
    #[test]
    fn option_off_does_not_resume() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-1");
        inputs.project_resume_on = false;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Skip(ResumeSkipReason::OptionOff)
        );
    }

    /// A terminal closed VOLUNTARILY does not come back, even with the option ON and
    /// an active session (done-criterion "Terminal fermé volontairement ne revient
    /// pas").
    #[test]
    fn voluntary_close_does_not_resume() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-1");
        inputs.closed_voluntarily = true;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Skip(ResumeSkipReason::ClosedVoluntarily)
        );
    }

    /// A cleanly ENDED session is not resumed.
    #[test]
    fn ended_session_does_not_resume() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-1");
        inputs.session_state = SessionState::Ended;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Skip(ResumeSkipReason::SessionEnded)
        );
    }

    /// A session that already FAILED to resume is not retried automatically.
    #[test]
    fn resume_failed_session_does_not_resume() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-1");
        inputs.session_state = SessionState::ResumeFailed;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Skip(ResumeSkipReason::AlreadyResumeFailed)
        );
    }

    /// A candidate session WITH a transcript on disk resumes (the present→resume half of
    /// finding #53). The baseline `resumable` already sets `transcript_exists = true`, so
    /// this asserts the conversation gate does not block a real conversation.
    #[test]
    fn transcript_present_resumes() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-with-transcript");
        inputs.transcript_exists = true;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Resume {
                command: "claude --resume sid-with-transcript".to_string(),
                resume_uncertain: false,
            },
            "a candidate with a transcript on disk is resumed"
        );
    }

    /// A candidate session with NO transcript on disk is NOT resumed (the absent→skip
    /// half of finding #53): the session has an id but Claude never wrote a `.jsonl`
    /// (user never typed) or it was deleted, so `claude --resume` would fail with "No
    /// conversation found". The decision skips with `NoConversation` rather than inject a
    /// doomed resume that breaks the respawned terminal.
    #[test]
    fn missing_transcript_skips_resume() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-no-transcript");
        inputs.transcript_exists = false;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Skip(ResumeSkipReason::NoConversation),
            "a candidate with no conversation on disk is skipped, not resumed"
        );
        // Holds for an `unknown` (stale) candidate too — no transcript still wins.
        inputs.session_state = SessionState::Unknown;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Skip(ResumeSkipReason::NoConversation)
        );
    }

    /// An `unknown` (stale/probable-kill) session is STILL a resume candidate, but the
    /// decision flags the uncertainty (`resume_uncertain = true`).
    #[test]
    fn unknown_session_resumes_but_flags_uncertainty() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-unknown");
        inputs.session_state = SessionState::Unknown;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Resume {
                command: "claude --resume sid-unknown".to_string(),
                resume_uncertain: true,
            },
            "an unknown session is resumed but flagged uncertain"
        );
    }

    /// A native Windows shell target now RESUMES (finding #83): `claude --resume` is
    /// shell-agnostic and the CR injection (#76) makes it execute on PSReadLine, so the
    /// earlier `UnsupportedTarget` skip is gone — a PowerShell/cmd terminal resumes with
    /// the exact id like any Unix target. (Was: asserted `Skip(UnsupportedTarget)`.)
    #[test]
    fn windows_native_target_now_resumes() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-win-1");
        inputs.target = ResumeTarget::WindowsNative;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Resume {
                command: "claude --resume sid-win-1".to_string(),
                resume_uncertain: false,
            },
            "a Windows-native (PowerShell/cmd) target now resumes (finding #83)"
        );
        // An `unknown` (stale) PowerShell session is still a resume candidate too.
        inputs.session_state = SessionState::Unknown;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Resume {
                command: "claude --resume sid-win-1".to_string(),
                resume_uncertain: true,
            },
        );
    }

    /// An adapter that cannot build an exact-resume command (a placeholder agent, or
    /// an empty external id) yields `NoResumeCommand` rather than a broken line.
    #[test]
    fn no_resume_command_when_adapter_cannot_build() {
        // Placeholder adapter: build_resume_command always returns None.
        let generic = GenericAdapter::new(db::AGENT_KIND_CODEX);
        assert_eq!(
            decide_resume(&resumable("sid-1"), &generic),
            ResumeDecision::Skip(ResumeSkipReason::NoResumeCommand)
        );
        // Claude adapter but an empty external id → also no command.
        let claude = ClaudeCodeAdapter;
        let empty = resumable("");
        assert_eq!(
            decide_resume(&empty, &claude),
            ResumeDecision::Skip(ResumeSkipReason::NoResumeCommand)
        );
    }

    /// The gate ORDER is observable: with MULTIPLE gates closed, the FIRST gate in the
    /// order wins. Option OFF beats a voluntary close beats an ended session.
    #[test]
    fn gate_order_is_deterministic() {
        let claude = ClaudeCodeAdapter;
        let mut inputs = resumable("sid-1");
        inputs.project_resume_on = false;
        inputs.closed_voluntarily = true;
        inputs.session_state = SessionState::Ended;
        assert_eq!(
            decide_resume(&inputs, &claude),
            ResumeDecision::Skip(ResumeSkipReason::OptionOff),
            "option OFF is the first gate and wins"
        );
    }

    /// `SessionState::from_db` maps the DB vocabulary and rejects an unknown string.
    #[test]
    fn session_state_from_db_maps_the_vocabulary() {
        assert_eq!(
            SessionState::from_db(db::SESSION_STATE_ACTIVE),
            Some(SessionState::Active)
        );
        assert_eq!(
            SessionState::from_db(db::SESSION_STATE_ENDED),
            Some(SessionState::Ended)
        );
        assert_eq!(
            SessionState::from_db(db::SESSION_STATE_UNKNOWN),
            Some(SessionState::Unknown)
        );
        assert_eq!(
            SessionState::from_db(db::SESSION_STATE_RESUME_FAILED),
            Some(SessionState::ResumeFailed)
        );
        assert_eq!(SessionState::from_db("bogus"), None);
    }

    // --- Close-warning policy (PRD-5 #6) ---------------------------------

    /// Warn ONLY when a session is live (active/unknown) AND the project does NOT
    /// auto-resume. A resume-ON project never warns (it will bring the session back);
    /// an ended / resume_failed session never warns (nothing live to lose). Covers the
    /// done-criteria "Warning seulement quand necessaire" + "Tests couvrent option
    /// ON/OFF".
    #[test]
    fn warn_on_close_only_when_live_and_resume_off() {
        // OPTION OFF + live → warn.
        assert!(should_warn_on_close(SessionState::Active, false));
        assert!(should_warn_on_close(SessionState::Unknown, false));
        // OPTION ON → never warn (resume will bring it back), even when live.
        assert!(!should_warn_on_close(SessionState::Active, true));
        assert!(!should_warn_on_close(SessionState::Unknown, true));
        // Not live → never warn, regardless of the option.
        assert!(!should_warn_on_close(SessionState::Ended, false));
        assert!(!should_warn_on_close(SessionState::ResumeFailed, false));
        assert!(!should_warn_on_close(SessionState::Ended, true));
    }

    /// The message DISTINGUISHES the four agent kinds (done-criterion "Message
    /// distingue Claude/Codex/OpenCode/custom") and names the terminal + workspace.
    #[test]
    fn close_warning_message_distinguishes_agents_and_names_terminal() {
        // Claude, with a terminal label + workspace.
        let m = close_warning_message(
            db::AGENT_KIND_CLAUDE_CODE,
            Some("build"),
            "term-1",
            Some("api"),
        );
        assert!(m.contains("Claude Code"), "names Claude: {m}");
        assert!(m.contains("build"), "names the terminal label: {m}");
        assert!(m.contains("api"), "names the workspace: {m}");

        // Each kind yields its own distinct label.
        assert!(close_warning_message(db::AGENT_KIND_CODEX, None, "t", None).contains("Codex"));
        assert!(
            close_warning_message(db::AGENT_KIND_OPENCODE, None, "t", None).contains("OpenCode")
        );
        assert!(
            close_warning_message(db::AGENT_KIND_CUSTOM, None, "t", None).contains("custom agent")
        );

        // No label → falls back to the terminal id; no workspace → no workspace clause.
        let m2 = close_warning_message(db::AGENT_KIND_CLAUDE_CODE, None, "term-xyz", None);
        assert!(
            m2.contains("terminal term-xyz"),
            "falls back to the id: {m2}"
        );
        assert!(
            !m2.contains("workspace"),
            "no workspace clause when none: {m2}"
        );
    }

    /// `agent_label` maps the vocabulary and passes through an unknown kind.
    #[test]
    fn agent_label_maps_the_vocabulary() {
        assert_eq!(agent_label(db::AGENT_KIND_CLAUDE_CODE), "Claude Code");
        assert_eq!(agent_label(db::AGENT_KIND_CODEX), "Codex");
        assert_eq!(agent_label(db::AGENT_KIND_OPENCODE), "OpenCode");
        assert_eq!(agent_label(db::AGENT_KIND_CUSTOM), "a custom agent");
        assert_eq!(agent_label("future_agent"), "future_agent");
    }

    /// Shell classification routes each shell to its target: WSL launchers + Unix shells
    /// → `UnixOrWsl`; the three Windows-native shells → `WindowsNative` (with or without
    /// `.exe`, bare or path). Both targets are now resume-capable (finding #83); this
    /// test pins only the CLASSIFICATION, the resume capability is asserted separately.
    #[test]
    fn classify_shell_routes_targets() {
        for unix in [
            "bash",
            "/bin/bash",
            "sh",
            "/usr/bin/zsh",
            "wsl",
            "wsl.exe",
            "C:\\Windows\\System32\\wsl.exe",
            "bash.exe",
            "fish",
        ] {
            assert_eq!(
                ResumeTarget::classify_shell(unix),
                ResumeTarget::UnixOrWsl,
                "{unix} should be a Unix/WSL resume target"
            );
        }
        for win in [
            "pwsh",
            "pwsh.exe",
            "powershell",
            "powershell.exe",
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            "cmd",
            "cmd.exe",
        ] {
            assert_eq!(
                ResumeTarget::classify_shell(win),
                ResumeTarget::WindowsNative,
                "{win} should classify as a Windows-native target"
            );
        }
    }

    // --- Workspace resume plan (task 01KWHPV33M4H5YSMGYNW9TG6J0) ----------

    /// The plan tests resolve kind→adapter through the REAL [`AgentRegistry`] (the single
    /// source of truth), not a hand-rolled `claude_code` branch: `claude_code` → the production
    /// `ClaudeCodeAdapter`, every other kind → its placeholder (which builds no resume command),
    /// an unknown kind → `None`. This proves the borrowed-lifetime `resolve` contract lets
    /// `AgentRegistry::get` be passed straight in, so a future resumable adapter is picked up
    /// with no call-site change.
    fn plan_with_registry(
        candidates: &[WorkspaceResumeCandidate<'_>],
    ) -> Vec<WorkspaceResumeEntry> {
        let registry = AgentRegistry::default();
        workspace_resume_plan(candidates, &|kind| registry.get(kind))
    }

    fn candidate<'a>(
        terminal: &'a str,
        session: &'a str,
        kind: &'a str,
        ext: &'a str,
        state: SessionState,
        project_resume_on: bool,
        transcript_exists: bool,
    ) -> WorkspaceResumeCandidate<'a> {
        WorkspaceResumeCandidate {
            terminal_id: terminal,
            session_id: session,
            agent_kind: kind,
            external_session_id: ext,
            session_state: state,
            project_resume_on,
            transcript_exists,
            target: ResumeTarget::UnixOrWsl,
        }
    }

    /// The workspace restore plan DELEGATES to `decide_resume` per candidate — no new policy.
    /// A resumable active session yields a resume entry with the exact command; an `unknown`
    /// session resumes but is flagged uncertain; option-OFF and no-transcript yield precise
    /// skip reasons. Covers done-criterion "sessions reprenables" + "états douteux".
    #[test]
    fn workspace_resume_plan_reuses_decide_resume_per_candidate() {
        let candidates = vec![
            // active + option ON + transcript → resume with exact id.
            candidate(
                "t1",
                "s1",
                db::AGENT_KIND_CLAUDE_CODE,
                "ext-active",
                SessionState::Active,
                true,
                true,
            ),
            // unknown (stale/probable kill) → still resumes, flagged uncertain.
            candidate(
                "t2",
                "s2",
                db::AGENT_KIND_CLAUDE_CODE,
                "ext-unknown",
                SessionState::Unknown,
                true,
                true,
            ),
            // option OFF → skip(option_off).
            candidate(
                "t3",
                "s3",
                db::AGENT_KIND_CLAUDE_CODE,
                "ext-off",
                SessionState::Active,
                false,
                true,
            ),
            // no transcript on disk → skip(no_conversation).
            candidate(
                "t4",
                "s4",
                db::AGENT_KIND_CLAUDE_CODE,
                "ext-notx",
                SessionState::Active,
                true,
                false,
            ),
        ];
        let plan = plan_with_registry(&candidates);
        assert_eq!(plan.len(), 4);

        assert_eq!(
            plan[0],
            WorkspaceResumeEntry {
                terminal_id: "t1".into(),
                session_id: "s1".into(),
                agent_kind: db::AGENT_KIND_CLAUDE_CODE.into(),
                resume_command: Some("claude --resume ext-active".into()),
                resume: true,
                uncertain: false,
                skip_reason: None,
            },
        );
        // Unknown resumes but is flagged uncertain (the doubtful-state surfacing).
        assert!(
            plan[1].resume,
            "an unknown session is still a resume candidate"
        );
        assert!(plan[1].uncertain, "the unknown state surfaces as uncertain");
        assert_eq!(
            plan[1].resume_command.as_deref(),
            Some("claude --resume ext-unknown")
        );
        // Option OFF → skipped with the exact reason.
        assert!(!plan[2].resume);
        assert_eq!(plan[2].skip_reason, Some("option_off"));
        assert!(plan[2].resume_command.is_none());
        // No transcript → skipped with no_conversation (no doomed resume injected).
        assert!(!plan[3].resume);
        assert_eq!(plan[3].skip_reason, Some("no_conversation"));
    }

    /// A candidate whose `agent_kind` resolves to a NON-resumable adapter (`codex`'s registry
    /// placeholder builds no command) yields a non-resume entry (`no_resume_command`) rather
    /// than a panic — the plan is total. Routed through the real registry, so this also proves
    /// a representable-but-not-resumable kind stays `no_resume_command` end to end.
    #[test]
    fn workspace_resume_plan_handles_non_resumable_agent_kind() {
        let candidates = vec![candidate(
            "t1",
            "s1",
            db::AGENT_KIND_CODEX,
            "ext-1",
            SessionState::Active,
            true,
            true,
        )];
        let plan = plan_with_registry(&candidates);
        assert_eq!(plan.len(), 1);
        assert!(!plan[0].resume);
        assert_eq!(plan[0].skip_reason, Some("no_resume_command"));
    }

    /// A candidate whose `agent_kind` is NOT in the registry at all (an unknown/unrepresentable
    /// kind → `AgentRegistry::get` returns `None`) still yields `no_resume_command`, never a
    /// panic — the plan stays total whether the adapter is absent or merely non-resumable.
    #[test]
    fn workspace_resume_plan_handles_unresolvable_agent_kind() {
        let candidates = vec![candidate(
            "t1",
            "s1",
            "totally-unknown-kind",
            "ext-1",
            SessionState::Active,
            true,
            true,
        )];
        let plan = plan_with_registry(&candidates);
        assert_eq!(plan.len(), 1);
        assert!(!plan[0].resume);
        assert_eq!(plan[0].skip_reason, Some("no_resume_command"));
    }

    /// An empty candidate set yields an empty plan (a workspace with no live sessions).
    #[test]
    fn workspace_resume_plan_empty_is_empty() {
        let plan = plan_with_registry(&[]);
        assert!(plan.is_empty());
    }

    /// EVERY [`ResumeSkipReason`] carries its OWN stable tag — the single reason vocabulary the
    /// whole app reads. [`workspace_resume_plan`] puts it in [`WorkspaceResumeEntry::skip_reason`],
    /// the sidecar's `workspace_restore_plan` db-command serialises it into
    /// `resume_sessions[].skip_reason`, and the renderer's `use-lazy-resume` prints it VERBATIM
    /// into the console. That chain is what turns "it never resumes" into a named branch — so two
    /// reasons sharing a tag, or a tag drifting, would silently un-name a skip everywhere at once.
    ///
    /// `ALL` cannot fall behind the enum without the compiler saying so: the `match` below has NO
    /// wildcard arm (the same property [`skip_reason_tag`] itself relies on), so an eighth variant
    /// fails to COMPILE here. With the tags asserted pairwise distinct, `ALL` therefore holds seven
    /// distinct variants of an enum that has no variant outside them — i.e. exactly the enum.
    #[test]
    fn skip_reason_tag_is_stable_and_distinct_for_every_variant() {
        const ALL: [ResumeSkipReason; 7] = [
            ResumeSkipReason::OptionOff,
            ResumeSkipReason::ClosedVoluntarily,
            ResumeSkipReason::SessionEnded,
            ResumeSkipReason::AlreadyResumeFailed,
            ResumeSkipReason::NoConversation,
            ResumeSkipReason::UnsupportedTarget,
            ResumeSkipReason::NoResumeCommand,
        ];
        for reason in ALL {
            match reason {
                ResumeSkipReason::OptionOff
                | ResumeSkipReason::ClosedVoluntarily
                | ResumeSkipReason::SessionEnded
                | ResumeSkipReason::AlreadyResumeFailed
                | ResumeSkipReason::NoConversation
                | ResumeSkipReason::UnsupportedTarget
                | ResumeSkipReason::NoResumeCommand => {}
            }
        }
        let tags: Vec<&'static str> = ALL.iter().copied().map(skip_reason_tag).collect();
        assert_eq!(
            tags,
            vec![
                "option_off",
                "closed_voluntarily",
                "session_ended",
                "already_resume_failed",
                "no_conversation",
                "unsupported_target",
                "no_resume_command",
            ],
            "the skip tags are a published vocabulary — the plan, the wire and the renderer log \
             all read these exact strings"
        );
        let distinct: std::collections::BTreeSet<&'static str> = tags.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            tags.len(),
            "two skip reasons sharing a tag would make their two branches indistinguishable in \
             the log"
        );
    }
}
