//! The workspace RUNTIME read model (v2.2 Phase 1, task
//! `01KWHPV32YVCRBNY1JW5CDY4AY`) — the cockpit's single source of live state.
//!
//! ## Why this module exists
//!
//! The v2.2 cockpit is an on-screen aggregate of ONE workspace's live runtime:
//! its interactive terminals, the agent sessions running on them, and its managed
//! commands / services with their current run state. Before this module those three
//! facts were read piecemeal — `list_terminals`, `active_agent_sessions` /
//! `sessions_for_terminal`, `list_instances_for_workspace` — each by a different
//! caller (the UI bridge `db_command.rs`, the MCP tools `mcp_tools_core.rs`), with
//! the run-state overlay logic duplicated. This module assembles the ONE
//! [`WorkspaceRuntimeSnapshot`] both surfaces consume, so the UI (bridge) and the
//! MCP (an agent) read the SAME aggregation from the SAME authority (PDR-0005),
//! exactly as the identity use cases already share `usecase::*`.
//!
//! ```text
//! UI  (db_command.rs `workspace_runtime_snapshot`) --\
//!                                                     -> workspace_runtime::snapshot
//! MCP (mcp_tools_core `workspace_runtime` tool)     --/    -> db + repository reads
//! ```
//!
//! ## Read-only, no polling, no new authority
//!
//! Every field is READ from the authorities v2.1 already owns:
//!   * the identity (`workspaces` / `projects` / `repositories`) via [`crate::db`];
//!   * the terminals + their agent sessions via [`crate::db`];
//!   * the managed-command instances via [`crate::db::list_instances_for_workspace`],
//!     with each instance's FACTUAL run state overlaid from the LIVE managed-command
//!     runner through the injected [`RunStateOverlay`] (the SAME overlay the MCP
//!     `list_commands` tool applies) — falling back to the persisted `last_state`
//!     when the runner does not back the instance.
//!
//! This module NEVER polls and NEVER writes: it is a pure projection. The LIVE
//! update of this projection (re-pull on a `domain_event`-driven push, no polling)
//! is the SEPARATE cockpit-wiring task; here we own the shape + the aggregation.
//!
//! ## What is NOT here
//!
//! - No Git-derived view (branch/dirty/files re-read from Git) — that is its own
//!   v2.2 read model ([`crate::workspace_git`], PDR-0013), deliberately kept out so
//!   this snapshot stays a pure DB+runner projection with no filesystem/`git` calls.
//! - No Spec/plan/review (v2.3 / v2.4).
//! - No terminal byte stream / scrollback (the cockpit reads output through the
//!   existing per-terminal channels; this is the structural snapshot only).

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::command::RunState;
use crate::db::{self, AgentSession, InstanceWithTemplate, Terminal, Workspace};

/// The LIVE run-state overlay seam: given a command-instance id, return the runner's
/// FACTUAL `(state, exit_code, unread)` when a live runner backs it, else `None` (so
/// the snapshot falls back to the instance's PERSISTED `last_state`). This is the
/// exact contract of [`crate::command::CommandRunner::outcome`]; modelling it as a
/// trait keeps `nyx-core`'s read model decoupled from the concrete `CommandRunner<S>`
/// generic — a caller that HAS a runner passes an adapter over it, and a pure-DB
/// caller (a test, or a cold read before the runtime is up) passes [`NoRunStateOverlay`].
pub trait RunStateOverlay {
    /// The live outcome for `instance_id`, or `None` when the runner does not back it.
    fn outcome(&self, instance_id: &str) -> Option<(RunState, Option<i32>, bool)>;
}

/// The zero-overlay: no live runner, so every instance reports its PERSISTED state.
/// Used by pure-DB callers (tests, a cold read before the managed-command runtime is
/// up — where the persisted `last_state` is authoritative anyway).
pub struct NoRunStateOverlay;

impl RunStateOverlay for NoRunStateOverlay {
    fn outcome(&self, _instance_id: &str) -> Option<(RunState, Option<i32>, bool)> {
        None
    }
}

/// The LIVE per-terminal signals overlaid onto a runtime snapshot from the PTY layer —
/// facts that are NOT in the DB and MUST NOT be persisted (the anti-phantom guarantee).
/// Keyed by the persistent terminal RECORD id. v2.5.5 review:
///   * `busy` — a command runs in the terminal's foreground process group (T1).
///   * `detected_agent_kind` — the agent kind the FOREGROUND command resolves to via the
///     provider-agnostic `AgentRegistry::detect` (T3), for PASSIVE adoption.
///
/// Kept a plain data struct (not per-field methods) so a future signal is one field, and
/// so [`TerminalLiveOverlay`] resolves the whole set in one lookup per terminal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalLive {
    /// The OS busy bit for this terminal (see [`WorkspaceTerminal::busy`]).
    pub busy: bool,
    /// The PASSIVELY-DETECTED agent kind for this terminal (see
    /// [`WorkspaceTerminal::detected_agent_kind`]), or `None` when the foreground is not a
    /// known agent. PROVIDER-AGNOSTIC: resolved through `AgentRegistry::detect`, never a
    /// hardcoded `claude` — every registry adapter is eligible.
    pub detected_agent_kind: Option<String>,
}

/// The live-terminal overlay seam: given a terminal RECORD id, return its live
/// [`TerminalLive`] signals. The UI bridge passes an adapter over the live `PtyManager`
/// (`is_busy`); a pure-DB caller (a test, the MCP tool, a cold read) passes
/// [`NoTerminalLiveOverlay`] so every terminal reads its default (not busy). Modelling it
/// as a trait keeps `nyx-core`'s read model decoupled from the sidecar's concrete PTY
/// manager — exactly like [`RunStateOverlay`] does for the managed-command runner.
pub trait TerminalLiveOverlay {
    /// The live signals for the terminal RECORD `terminal_id` (default when unknown/dead).
    fn live(&self, terminal_id: &str) -> TerminalLive;
}

/// The zero-overlay: no live PTY layer, so every terminal reports its default (idle/not
/// busy). Used by the pure-DB callers — tests, the MCP `workspace_runtime` tool, and any
/// cold read before the live overlay is applied.
pub struct NoTerminalLiveOverlay;

impl TerminalLiveOverlay for NoTerminalLiveOverlay {
    fn live(&self, _terminal_id: &str) -> TerminalLive {
        TerminalLive::default()
    }
}

/// A blanket impl so any closure `Fn(&str) -> TerminalLive` is a [`TerminalLiveOverlay`] —
/// the ergonomic form the sidecar db-command uses to adapt the live `PtyManager`
/// (`|record_id| TerminalLive { busy: pty.is_busy_record(record_id) }`) without a named type.
impl<F> TerminalLiveOverlay for F
where
    F: Fn(&str) -> TerminalLive,
{
    fn live(&self, terminal_id: &str) -> TerminalLive {
        (self)(terminal_id)
    }
}

/// Overlay the LIVE per-terminal PTY signals onto an already-built [`WorkspaceRuntimeSnapshot`],
/// in place. This is deliberately a POST-processing step over the pure-DB [`snapshot`] (rather
/// than a parameter of it) so `snapshot` stays a pure DB+runner projection with NO live-PTY
/// dependency — the MCP tool and every test call it unchanged, and only the UI bridge (which
/// holds the live `PtyManager`) applies this overlay. Each terminal's `busy` (T1) and
/// `detected_agent_kind` (T3, passive adoption) are read from `overlay.live(record_id)`; a
/// terminal the overlay does not know reads its default (not busy, no detected agent).
pub fn apply_terminal_live_overlay(
    snapshot: &mut WorkspaceRuntimeSnapshot,
    overlay: &dyn TerminalLiveOverlay,
) {
    for wt in &mut snapshot.terminals {
        let live = overlay.live(&wt.terminal.id);
        wt.busy = live.busy;
        wt.detected_agent_kind = live.detected_agent_kind;
    }
}

/// A blanket impl so any closure `Fn(&str) -> Option<(RunState, Option<i32>, bool)>`
/// is a [`RunStateOverlay`] — the ergonomic form a caller uses to adapt a
/// `CommandRunner` (`|id| runner.outcome(id)`) without a named wrapper type.
impl<F> RunStateOverlay for F
where
    F: Fn(&str) -> Option<(RunState, Option<i32>, bool)>,
{
    fn outcome(&self, instance_id: &str) -> Option<(RunState, Option<i32>, bool)> {
        (self)(instance_id)
    }
}

/// One interactive terminal of the workspace, with the agent sessions hosted on it.
/// The terminal record is the v2.1 [`Terminal`]; `sessions` are its `agent_sessions`
/// rows (every state, oldest first — the cockpit shows the live one and the history).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceTerminal {
    /// The persisted terminal record (id, cwd, label, exec-state badge, …).
    #[serde(flatten)]
    pub terminal: Terminal,
    /// The agent sessions anchored to this terminal (every state), oldest started first.
    pub sessions: Vec<AgentSession>,
    /// The OS BUSY bit (v2.5.5 review T1): `true` while a command runs in this terminal's
    /// FOREGROUND process group (the shell handed the terminal to a child), `false` at the
    /// idle prompt. This is a LIVE signal from the PTY layer overlaid by
    /// [`apply_terminal_live_overlay`] — NOT a DB column — so it is NEVER persisted (the
    /// anti-phantom guarantee: a restored terminal with no foreground command reads idle by
    /// construction). It restores the "un process foreground rallume un point running" mode
    /// the OSC-133 `exec_state` cannot express (it never carries `running`). A pure-DB read
    /// (tests, the MCP tool, a cold read before the overlay) leaves it `false`.
    #[serde(default)]
    pub busy: bool,
    /// The PASSIVELY-DETECTED agent kind (v2.5.5 review T3): when the terminal's FOREGROUND
    /// command resolves to a known agent via the provider-agnostic `AgentRegistry::detect`
    /// (over the foreground `comm`), this is that adapter's `agent_kind` — else `None`. It is a
    /// LIVE overlay (never persisted, distinct from `sessions[]`) so nyx can ADOPT an agent
    /// launched by hand — show its provider icon + a busy-derived status — WITHOUT waiting for
    /// the hook's `SessionStart`. It NEVER creates an `agent_sessions` row (that stays reserved
    /// for the hook, which carries the resumable `external_session_id`); the renderer only shows
    /// this passive kind WHEN there is no hook session for the terminal — the hook session, once
    /// it fires, is the authoritative icon (no double icon, no flicker). `None` when the
    /// foreground is a plain shell/command, so a stopped agent's passive icon disappears.
    #[serde(default)]
    pub detected_agent_kind: Option<String>,
}

/// One managed-command SERVICE of the workspace: the instance (joined to its template
/// so the cockpit renders name/command/subfolder without a second query) with its
/// CURRENT run state overlaid from the live runner (or the persisted `last_state`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceService {
    /// The instance joined to its template (name, command, subfolder, provenance, …).
    #[serde(flatten)]
    pub instance: InstanceWithTemplate,
    /// The FACTUAL current run state (`idle` | `running` | `success` | `error`) — the
    /// LIVE runner state when it backs this instance, else the persisted `last_state`.
    pub run_state: String,
    /// The exit code of the last finished run (`None` while never-finished/running).
    pub exit_code: Option<i32>,
    /// The "unseen result" flag a UI acknowledge clears (live runner value when backed).
    pub unread: bool,
}

/// The workspace runtime read model: the ONE aggregate the cockpit renders and the
/// MCP `workspace_runtime` tool returns. Identity (the workspace + its project id)
/// plus its live runtime: terminals (with their sessions), the workspace-anchored
/// agent sessions, and the managed-command services (with live run state).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceRuntimeSnapshot {
    /// The workspace identity — id, name, `root_path`, `is_root`, … — read verbatim from
    /// the `workspaces` row via [`db::get_workspace`].
    ///
    /// BRANCH IS NOT LIVE HERE (PDR-0013, review 01KWKH9GQ5WXA86VFVWKCDCXGH). The row's
    /// `branch` field is the value DETECTED ONCE at workspace creation (`db::create_workspace`
    /// → `gitbranch::detect`) and never refreshed; it is a stale snapshot the moment the user
    /// switches branch. Per PDR-0013 ("l'état Git relu depuis Git, jamais autoritaire en DB"),
    /// Git state — the branch included — is NEVER authoritative in the DB. This module is a
    /// pure DB+runner projection with NO `git`/filesystem calls BY DESIGN (see the module
    /// docstring), so it does NOT re-detect the branch. The cockpit's authoritative branch
    /// comes from the SEPARATE Git read model [`crate::workspace_git`] (`RepoGitSummary.branch`,
    /// re-read from Git PER REPO) — NOT from this `workspace.branch`. Do NOT consume
    /// `workspace.branch` for the cockpit header; read it from `workspace_git` instead.
    /// (The `list_workspaces` MCP tool overlays a live branch onto its row for the sidebar's
    /// single-branch label; the cockpit uses the richer per-repo `workspace_git` view.)
    pub workspace: Workspace,
    /// The interactive terminals attached to this workspace, each with its sessions.
    /// Ordered by the terminal's sidebar `order_index` (then id) for a stable cockpit.
    pub terminals: Vec<WorkspaceTerminal>,
    /// Every agent session ANCHORED to this workspace (`agent_sessions.workspace_id`),
    /// across ALL its terminals — the cross-terminal "Agents" view of the workspace.
    /// Includes a session anchored to the workspace even on a loose (unattached) host
    /// terminal. Oldest started first.
    pub agent_sessions: Vec<AgentSession>,
    /// The managed-command instances (services) of this workspace, template-joined,
    /// with the live run-state overlay. Ordered by the template's sidebar order.
    pub services: Vec<WorkspaceService>,
}

/// Build the [`WorkspaceRuntimeSnapshot`] for `workspace_id`, or `None` when the id
/// does not resolve to a workspace. Pure projection — reads only; NEVER writes, NEVER
/// polls. `overlay` supplies the live run state per instance (an adapter over the
/// managed-command runner, or [`NoRunStateOverlay`] for a pure-DB read).
///
/// The aggregation reuses the v2.1 authorities verbatim:
///   * [`db::get_workspace`] for identity;
///   * [`db::list_terminals`] filtered to this workspace + [`db::sessions_for_terminal`]
///     for each terminal's sessions;
///   * the workspace-anchored sessions gathered from every terminal's sessions PLUS
///     any session anchored to the workspace on a loose host terminal;
///   * [`db::list_instances_for_workspace`] + the [`RunStateOverlay`] for services.
pub fn snapshot(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    overlay: &dyn RunStateOverlay,
) -> QueryResult<Option<WorkspaceRuntimeSnapshot>> {
    // Identity — a missing workspace short-circuits to `None` (the caller maps it to a
    // readable "unknown workspace" error) rather than an empty-but-present snapshot. The
    // row is served AS-IS: its `branch` is the create-time DB value, NOT re-detected here —
    // the cockpit reads the live branch from `workspace_git`, not from this field (PDR-0013;
    // see the `WorkspaceRuntimeSnapshot.workspace` doc, review 01KWKH9GQ5WXA86VFVWKCDCXGH).
    let Some(workspace) = db::get_workspace(conn, workspace_id)? else {
        return Ok(None);
    };

    // Terminals attached to this workspace, in sidebar order, each with its sessions.
    // `list_terminals` already orders alive-first by `order_index`; we keep that order
    // and filter to this workspace's ALIVE terminals (the cockpit shows live terminals).
    let mut terminals = Vec::new();
    // Collect the workspace-anchored sessions as we go, plus a set of the ids we have
    // already seen through a terminal so we do not double-count a loose-terminal session.
    let mut anchored: Vec<AgentSession> = Vec::new();
    for terminal in db::list_alive_terminals_for_workspace(conn, workspace_id)? {
        let sessions = db::sessions_for_terminal(conn, &terminal.id)?;
        for s in &sessions {
            if s.workspace_id.as_deref() == Some(workspace_id) {
                anchored.push(s.clone());
            }
        }
        // `busy` / `detected_agent_kind` default to their empty values here (a pure-DB
        // projection knows no live foreground state); the UI bridge overlays the live PTY
        // signals via `apply_terminal_live_overlay`.
        terminals.push(WorkspaceTerminal {
            terminal,
            sessions,
            busy: false,
            detected_agent_kind: None,
        });
    }

    // Sessions anchored to this workspace on a LOOSE (unattached) host terminal — the
    // ones not reached through a workspace-owned terminal above. Fold them into the
    // cross-terminal `agent_sessions` view so it is complete.
    for s in db::sessions_for_workspace(conn, workspace_id)? {
        if !anchored.iter().any(|seen| seen.id == s.id) {
            anchored.push(s);
        }
    }
    // Stable order for the cross-terminal view: oldest started first (id tiebreak).
    anchored.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    // Services: the workspace's command instances (template-joined), with each one's
    // FACTUAL run state overlaid from the live runner, else its persisted `last_state`.
    let services = db::list_instances_for_workspace(conn, workspace_id)?
        .into_iter()
        .map(|instance| {
            let (state, exit_code, unread) = match overlay.outcome(&instance.id) {
                Some((state, exit_code, unread)) => (state, exit_code, unread),
                None => (
                    RunState::from_db_str(&instance.last_state),
                    instance.last_exit_code,
                    instance.unread,
                ),
            };
            WorkspaceService {
                run_state: state.as_db_str().to_string(),
                exit_code,
                unread,
                instance,
            }
        })
        .collect();

    Ok(Some(WorkspaceRuntimeSnapshot {
        workspace,
        terminals,
        agent_sessions: anchored,
        services,
    }))
}

/// Build the runtime snapshot for a project's PRIMARY (free-work) workspace — the cockpit's
/// default entry point when a project is opened WITHOUT a Spec (task
/// `01KWHPV33H0FX8NA4C94T5W46J`; "le primary workspace est l'espace de travail libre par
/// défaut"). Resolves the primary/free-work workspace through
/// [`crate::usecase::resolve_primary_workspace`] (canonical `primary_workspace_id`, root
/// fallback) and then reuses [`snapshot`] verbatim — the free-work cockpit is the SAME
/// aggregate as any workspace cockpit, keyed on the workspace id, with NO Spec dependency
/// (there is no `specs` table in v2.2). `None` when the project resolves to no workspace.
pub fn snapshot_for_project(
    conn: &mut SqliteConnection,
    project_id: &str,
    overlay: &dyn RunStateOverlay,
) -> QueryResult<Option<WorkspaceRuntimeSnapshot>> {
    let Some(primary) = crate::usecase::resolve_primary_workspace(conn, project_id)? else {
        return Ok(None);
    };
    snapshot(conn, &primary.id, overlay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{open_in_memory, CommandSource, SessionCapture};
    use crate::usecase::{self, DomainEventSource, NullEventSink};

    fn root_path(tag: &str) -> String {
        #[cfg(windows)]
        {
            format!("C:\\wr-{tag}")
        }
        #[cfg(not(windows))]
        {
            format!("/wr-{tag}")
        }
    }

    /// The read model aggregates, for ONE workspace, its terminals + the agent sessions
    /// on them + its command/services with their current run state — the task's
    /// done_criterion #1. Uses [`NoRunStateOverlay`] (no live runner in a pure-DB test),
    /// so every service reports its PERSISTED `last_state`.
    #[test]
    fn snapshot_aggregates_terminals_sessions_and_services() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("agg"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("agg-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();

        // A managed command materializes one instance per workspace (this ws included).
        db::create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();

        // Two terminals on the workspace; the first hosts a live agent session.
        let t1 = db::create_terminal(&mut conn, &root_path("agg-wt"), None).unwrap();
        let t2 = db::create_terminal(&mut conn, &root_path("agg-wt"), None).unwrap();
        db::attach_terminal(&mut conn, &t1.id, &ws.id, "auto").unwrap();
        db::attach_terminal(&mut conn, &t2.id, &ws.id, "auto").unwrap();
        db::record_session_start(
            &mut conn,
            &t1.id,
            "claude_code",
            SessionCapture {
                workspace_id: Some(ws.id.clone()),
                external_session_id: "sess-1".to_string(),
                cwd: root_path("agg-wt"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        let snap = snapshot(&mut conn, &ws.id, &NoRunStateOverlay)
            .unwrap()
            .expect("workspace resolves");

        // Identity.
        assert_eq!(snap.workspace.id, ws.id);
        // Terminals (both attached, alive) with the session on t1.
        assert_eq!(snap.terminals.len(), 2);
        let t1_view = snap
            .terminals
            .iter()
            .find(|t| t.terminal.id == t1.id)
            .unwrap();
        assert_eq!(t1_view.sessions.len(), 1, "t1 hosts the live session");
        assert_eq!(t1_view.sessions[0].external_session_id, "sess-1");
        let t2_view = snap
            .terminals
            .iter()
            .find(|t| t.terminal.id == t2.id)
            .unwrap();
        assert!(t2_view.sessions.is_empty(), "t2 hosts no session");
        // Cross-terminal agent view: one session anchored to the workspace.
        assert_eq!(snap.agent_sessions.len(), 1);
        assert_eq!(snap.agent_sessions[0].external_session_id, "sess-1");
        // Services: the one materialized instance, PERSISTED-idle (no live runner).
        assert_eq!(snap.services.len(), 1);
        assert_eq!(snap.services[0].instance.name, "dev");
        assert_eq!(snap.services[0].run_state, "idle");
        assert!(snap.services[0].exit_code.is_none());
    }

    /// A session anchored to the workspace on a LOOSE (unattached) host terminal still
    /// surfaces in the cross-terminal `agent_sessions` view (it is NOT reached through a
    /// workspace-owned terminal) — proving the aggregate is complete, not
    /// terminal-membership-limited.
    #[test]
    fn snapshot_includes_workspace_anchored_session_on_a_loose_terminal() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("loose"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("loose-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();

        // A loose terminal (NOT attached to ws) hosting a session anchored to ws.
        let loose = db::create_terminal(&mut conn, &root_path("loose-host"), None).unwrap();
        db::record_session_start(
            &mut conn,
            &loose.id,
            "claude_code",
            SessionCapture {
                workspace_id: Some(ws.id.clone()),
                external_session_id: "sess-loose".to_string(),
                cwd: root_path("loose-host"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        let snap = snapshot(&mut conn, &ws.id, &NoRunStateOverlay)
            .unwrap()
            .unwrap();
        // No workspace-owned terminal, yet the anchored session surfaces.
        assert!(snap.terminals.is_empty());
        assert_eq!(snap.agent_sessions.len(), 1);
        assert_eq!(snap.agent_sessions[0].external_session_id, "sess-loose");
    }

    /// The live-runner overlay wins over the persisted `last_state`: an instance
    /// PERSISTED idle but reported `running` by the injected overlay surfaces as
    /// `running` — proving the read model reflects the LIVE runtime, not just the DB.
    #[test]
    fn snapshot_service_run_state_uses_the_live_overlay() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("ovl"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("ovl-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        db::create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            CommandSource::default(),
        )
        .unwrap();
        let instance_id = db::instance_ids_for_workspace(&mut conn, &ws.id)
            .unwrap()
            .pop()
            .expect("one instance");

        // The persisted state is idle; the overlay reports the instance RUNNING.
        let running = instance_id.clone();
        let overlay = move |id: &str| -> Option<(RunState, Option<i32>, bool)> {
            if id == running {
                Some((RunState::Running, None, false))
            } else {
                None
            }
        };
        let snap = snapshot(&mut conn, &ws.id, &overlay).unwrap().unwrap();
        assert_eq!(snap.services.len(), 1);
        assert_eq!(
            snap.services[0].run_state, "running",
            "the LIVE overlay state wins over the persisted idle"
        );
    }

    /// REVIEW 01KWKH9GQ5WXA86VFVWKCDCXGH (PDR-0013): the runtime snapshot serves the
    /// workspace row's `branch` AS PERSISTED — it does NOT re-detect the live branch (this
    /// module is a pure DB+runner projection with no `git` calls by design). This test PINS
    /// that contract: a `branch` written directly on the row surfaces UNCHANGED through the
    /// snapshot, even though the on-disk repo is on a different branch. The cockpit's live
    /// branch authority is `workspace_git` (re-read from Git per repo), not this field.
    #[test]
    fn snapshot_serves_persisted_branch_not_a_live_detection() {
        use diesel::prelude::*;
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("br"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("br-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();

        // Pin a KNOWN persisted branch on the row (the create-time snapshot). A non-repo
        // `root_path` would detect `None` live, so a non-`None` value here can ONLY come from
        // the persisted row — proving the snapshot reads the DB value, not a live detection.
        {
            use crate::schema::workspaces::dsl as w;
            diesel::update(w::workspaces.filter(w::id.eq(&ws.id)))
                .set(w::branch.eq(Some("persisted-branch")))
                .execute(&mut conn)
                .unwrap();
        }

        let snap = snapshot(&mut conn, &ws.id, &NoRunStateOverlay)
            .unwrap()
            .expect("workspace resolves");
        assert_eq!(
            snap.workspace.branch.as_deref(),
            Some("persisted-branch"),
            "the runtime snapshot serves the PERSISTED branch verbatim (no live git detection)"
        );
    }

    /// v2.5.5 review T1 — the LIVE busy overlay surfaces on the terminal LINE. The pure-DB
    /// snapshot reads every terminal `busy = false` (no live foreground knowledge); applying
    /// [`apply_terminal_live_overlay`] with an overlay that reports one terminal busy flips
    /// exactly that terminal's `busy` to `true`, leaving the others idle — the "un process
    /// foreground rallume un point running sur la LIGNE" wiring, proven at the read-model.
    #[test]
    fn terminal_live_overlay_sets_busy_on_the_matching_terminal() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("busy"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("busy-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        let t1 = db::create_terminal(&mut conn, &root_path("busy-wt"), None).unwrap();
        let t2 = db::create_terminal(&mut conn, &root_path("busy-wt"), None).unwrap();
        db::attach_terminal(&mut conn, &t1.id, &ws.id, "auto").unwrap();
        db::attach_terminal(&mut conn, &t2.id, &ws.id, "auto").unwrap();

        let mut snap = snapshot(&mut conn, &ws.id, &NoRunStateOverlay)
            .unwrap()
            .expect("workspace resolves");
        // Pure-DB: no terminal is busy (busy is never persisted — anti-phantom).
        assert!(
            snap.terminals.iter().all(|t| !t.busy),
            "the pure-DB snapshot knows no live foreground state"
        );

        // Overlay reports t1 busy (a foreground command running), t2 idle.
        let busy_id = t1.id.clone();
        let overlay = move |id: &str| TerminalLive {
            busy: id == busy_id,
            ..Default::default()
        };
        apply_terminal_live_overlay(&mut snap, &overlay);

        let t1_view = snap
            .terminals
            .iter()
            .find(|t| t.terminal.id == t1.id)
            .unwrap();
        let t2_view = snap
            .terminals
            .iter()
            .find(|t| t.terminal.id == t2.id)
            .unwrap();
        assert!(t1_view.busy, "the foreground-busy terminal lights running");
        assert!(!t2_view.busy, "the idle-prompt terminal stays quiet");
    }

    /// v2.5.5 review T3 — the PASSIVE agent-adoption overlay surfaces the detected kind on the
    /// terminal LINE, DISTINCT from `sessions[]` (it creates NO agent session). The pure-DB
    /// snapshot has no detected kind; applying an overlay that reports one terminal's foreground
    /// as a known agent sets exactly that terminal's `detected_agent_kind`, leaving `sessions`
    /// untouched (no fake resumable session) — the read-model half of passive adoption.
    #[test]
    fn terminal_live_overlay_sets_detected_agent_kind_without_a_session() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("passive"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("passive-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        let t1 = db::create_terminal(&mut conn, &root_path("passive-wt"), None).unwrap();
        let t2 = db::create_terminal(&mut conn, &root_path("passive-wt"), None).unwrap();
        db::attach_terminal(&mut conn, &t1.id, &ws.id, "auto").unwrap();
        db::attach_terminal(&mut conn, &t2.id, &ws.id, "auto").unwrap();

        let mut snap = snapshot(&mut conn, &ws.id, &NoRunStateOverlay)
            .unwrap()
            .expect("workspace resolves");
        assert!(
            snap.terminals
                .iter()
                .all(|t| t.detected_agent_kind.is_none()),
            "the pure-DB snapshot detects no passive agent"
        );

        // t1's foreground is a known agent (any registry kind), t2's is a plain shell.
        let agent_id = t1.id.clone();
        let overlay = move |id: &str| TerminalLive {
            busy: true,
            detected_agent_kind: (id == agent_id).then(|| "claude_code".to_string()),
        };
        apply_terminal_live_overlay(&mut snap, &overlay);

        let t1_view = snap
            .terminals
            .iter()
            .find(|t| t.terminal.id == t1.id)
            .unwrap();
        let t2_view = snap
            .terminals
            .iter()
            .find(|t| t.terminal.id == t2.id)
            .unwrap();
        assert_eq!(
            t1_view.detected_agent_kind.as_deref(),
            Some("claude_code"),
            "the foreground agent is adopted passively on its line"
        );
        assert!(
            t1_view.sessions.is_empty(),
            "passive adoption creates NO agent_sessions row (agent model not polluted)"
        );
        assert!(
            t2_view.detected_agent_kind.is_none(),
            "a plain shell adopts nothing"
        );
    }

    /// The `NoTerminalLiveOverlay` is a no-op: applying it leaves every terminal at its
    /// default (not busy) — the MCP tool / cold-read path never fabricates a running dot.
    #[test]
    fn no_terminal_live_overlay_leaves_busy_false() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("nobusy"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();
        let ws = usecase::register_workspace(
            &mut conn,
            &sink,
            &project.id,
            "wt",
            &root_path("nobusy-wt"),
            DomainEventSource::Ui,
        )
        .unwrap();
        let t = db::create_terminal(&mut conn, &root_path("nobusy-wt"), None).unwrap();
        db::attach_terminal(&mut conn, &t.id, &ws.id, "auto").unwrap();

        let mut snap = snapshot(&mut conn, &ws.id, &NoRunStateOverlay)
            .unwrap()
            .unwrap();
        apply_terminal_live_overlay(&mut snap, &NoTerminalLiveOverlay);
        assert!(snap.terminals.iter().all(|t| !t.busy));
    }

    /// An unknown workspace id resolves to `None` (not an empty snapshot).
    #[test]
    fn snapshot_unknown_workspace_is_none() {
        let mut conn = open_in_memory();
        assert!(snapshot(&mut conn, "nope", &NoRunStateOverlay)
            .unwrap()
            .is_none());
    }

    /// FREE-WORK cockpit (task `01KWHPV33H0FX8NA4C94T5W46J`): `snapshot_for_project` opens the
    /// project's PRIMARY workspace's runtime WITHOUT any Spec — the "open and work in a
    /// workspace with no Spec, end to end" done-criterion. A brand-new project (only its root,
    /// no worktree, no Spec — there is no `specs` table in v2.2) yields a valid snapshot keyed
    /// on the primary workspace, and it is byte-identical to the per-workspace `snapshot` for
    /// that same workspace (same aggregate, no Spec seam involved).
    #[test]
    fn snapshot_for_project_opens_the_primary_workspace_as_free_work() {
        let mut conn = open_in_memory();
        let sink = NullEventSink;
        let (project, _root) = usecase::create_project(
            &mut conn,
            &sink,
            "Nyx",
            &root_path("free"),
            None,
            DomainEventSource::Ui,
        )
        .unwrap();

        // A managed command materializes one instance in the (only) primary workspace, and a
        // terminal + live agent session are opened directly IN THAT free-work workspace — no
        // Spec anywhere.
        let primary = usecase::resolve_primary_workspace(&mut conn, &project.id)
            .unwrap()
            .expect("the project has a primary workspace");
        db::create_template(
            &mut conn,
            &project.id,
            "dev",
            "npm run dev",
            None,
            crate::db::CommandSource::default(),
        )
        .unwrap();
        let t = db::create_terminal(&mut conn, &root_path("free"), None).unwrap();
        db::attach_terminal(&mut conn, &t.id, &primary.id, "auto").unwrap();
        db::record_session_start(
            &mut conn,
            &t.id,
            "claude_code",
            SessionCapture {
                workspace_id: Some(primary.id.clone()),
                external_session_id: "free-sess".to_string(),
                cwd: root_path("free"),
                transcript_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        let by_project = snapshot_for_project(&mut conn, &project.id, &NoRunStateOverlay)
            .unwrap()
            .expect("free-work opens the primary workspace end to end");
        // It resolved the primary workspace and aggregated its runtime with NO Spec.
        assert_eq!(by_project.workspace.id, primary.id);
        assert_eq!(by_project.terminals.len(), 1);
        assert_eq!(by_project.agent_sessions.len(), 1);
        assert_eq!(
            by_project.services.len(),
            1,
            "the free-work service is present"
        );

        // Free-work opening is JUST the per-workspace snapshot keyed on the primary — no
        // separate Spec-dependent code path.
        let by_workspace = snapshot(&mut conn, &primary.id, &NoRunStateOverlay)
            .unwrap()
            .unwrap();
        assert_eq!(
            by_project, by_workspace,
            "free-work (by project) == the primary workspace's runtime (by id): no Spec seam"
        );

        // An unknown project resolves to None (not an empty snapshot).
        assert!(snapshot_for_project(&mut conn, "nope", &NoRunStateOverlay)
            .unwrap()
            .is_none());
    }
}
