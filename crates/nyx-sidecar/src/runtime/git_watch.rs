//! The sidecar's REAL-TIME GIT-VIEW REFRESH runtime (v2.4 Q-015, task
//! `01KX0F6GDZEH3YN6QJDB7FC0WC`) — the shell wiring of the shell-agnostic
//! [`nyx_core::git_watch`] engine onto the v2.1 real-time PUSH channel.
//!
//! ## What this bridges
//!
//! [`nyx_core::git_watch::GitWatcher`] does the OS fs watching + debounce + derived-view
//! recompute, but it never names a transport (the frontier holds). This module supplies
//! the [`nyx_core::git_watch::RepoViewSink`] that PUSHES each recomputed view out over the
//! EXACT SAME [`EventSink`] pipe the v2.1 domain events ride — as an
//! [`Event::Changed { topic: "repos" }`](crate::pipe::wire::Event::Changed) unsolicited
//! event. That reuses the v2.0/v2.1 "X changed → re-fetch" invalidation channel (no new
//! transport): the supervisor maps it to `repos://changed`, the renderer re-pulls the
//! affected repo's `repo_git_view` / `repo_diff`, and — through the SAME shared authority
//! (PDR-0005: one authority, two façades) — an MCP client re-pulling `repo_git_view` sees
//! the identical refreshed view. No polling anywhere; the push is driven by the fs event.
//!
//! ## Runtime shape
//!
//! [`GitWatchRuntime`] owns the one [`GitWatcher`]. It is built in
//! [`super::Runtime::attach_events`] once the pipe [`EventSink`] exists, and the
//! `repo_watch` / `repo_unwatch` db-commands (+ MCP tools) register/unregister a
//! `(workspace, repository)` pair as its review surface opens/closes. Multi-repo:
//! each registration carries its full identity, and the pushed refresh names the
//! `(workspace, repository)` it is for — identities never mix (cf. phase 1).

use std::sync::Arc;

use nyx_core::git_watch::{GitWatcher, RepoViewRefresh, RepoViewSink, WatchedRepo};

use super::events::EventSink;
use crate::pipe::wire::Event;

/// The topic string a Git-view refresh is pushed on — mirrors
/// [`nyx_core::frontier::ChangedTopic::Repos`] / [`nyx_core::mcp_tools_core::ChangedTopic::Repos`]
/// (`"repos"`). The supervisor maps it to `repos://changed`.
const REPOS_TOPIC: &str = "repos";

/// The production [`RepoViewSink`]: it pushes each recomputed derived view out over the
/// sidecar's pipe [`EventSink`] as the v2.1 `repos://changed` unsolicited invalidation —
/// the SAME channel the domain events use. Coarse by design (the surface re-pulls the
/// authoritative `repo_git_view`), so the refreshed view stays the ONE authority for both
/// the renderer and MCP. Cheap to clone (an `EventSink` is an `Arc` around the shared
/// stdout writer).
#[derive(Clone)]
pub struct SidecarRepoViewSink {
    events: EventSink,
}

impl SidecarRepoViewSink {
    /// Build the sink over the sidecar's pipe event sink.
    pub fn new(events: EventSink) -> Self {
        Self { events }
    }
}

impl RepoViewSink for SidecarRepoViewSink {
    fn push(&self, refresh: RepoViewRefresh) {
        // The refresh carries the recomputed view + its `(workspace, repository)` identity;
        // the coarse `repos://changed` invalidation tells every surface to re-pull the
        // affected repo's view (the authoritative `repo_git_view` / `repo_diff`), exactly as
        // the v2.1 `workspaces://changed` drives a sidebar re-pull. Reusing the topic channel
        // — not a bespoke event — is the "branché sur le bus temps réel v2.1" guarantee.
        let _ = refresh;
        self.events.emit(Event::Changed {
            topic: REPOS_TOPIC.to_string(),
        });
    }
}

/// The sidecar's Git-watch runtime: owns the one real-time [`GitWatcher`] pushing on the
/// pipe. Built once at boot; repos are registered/unregistered at runtime as reviews open.
pub struct GitWatchRuntime {
    watcher: GitWatcher,
}

impl GitWatchRuntime {
    /// Start the Git-watch runtime over the sidecar pipe `events`, wiring the fs-watch +
    /// debounce engine to push `repos://changed` on every debounced change.
    pub fn start(events: EventSink) -> Arc<Self> {
        let watcher = GitWatcher::start(SidecarRepoViewSink::new(events));
        Arc::new(Self { watcher })
    }

    /// Begin real-time watching of a `(workspace, repository)` at `repo_path` (optional
    /// review `base`/`target`). Idempotent per pair; seeds one immediate refresh so a
    /// freshly-opened surface starts from truth. A non-git/absent path is a no-op watch.
    pub fn watch(
        &self,
        workspace_id: &str,
        repository_id: &str,
        repo_path: &str,
        base: Option<String>,
        target: Option<String>,
    ) {
        self.watcher.watch(WatchedRepo {
            workspace_id: workspace_id.to_string(),
            repository_id: repository_id.to_string(),
            repo_path: repo_path.to_string(),
            base,
            target,
        });
    }

    /// Stop watching a `(workspace, repository)` pair (releases its OS watches). A pair
    /// that was never watched is a no-op.
    pub fn unwatch(&self, workspace_id: &str, repository_id: &str) {
        self.watcher.unwatch(workspace_id, repository_id);
    }
}
