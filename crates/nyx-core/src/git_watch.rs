//! The v2.4 REAL-TIME REFRESH of the derived Git views (PRD
//! `01KX0EEP5H1MJW4T3QCZEWPHFK`, task `01KX0F6GDZEH3YN6QJDB7FC0WC`, Q-015).
//!
//! ## Why this module exists
//!
//! The derived Git views ([`crate::git_views`]) and the line-level diff
//! ([`crate::git_diff`]) are computed ON DEMAND — a caller asks, gets a fresh
//! snapshot. That is correct but STATIC: nothing tells a surface (the renderer, an
//! MCP client) that the repo changed under it. This module closes that gap for
//! Q-015 (deferred since v2.1): it WATCHES each reviewed repository and, on a real
//! change, RECOMPUTES the derived view and PUSHES the refreshed view out on the SAME
//! real-time channel the v2.1 domain events already use — WITHOUT polling.
//!
//! ## Trigger: an OS filesystem watch, NOT a poll
//!
//! The change source is the OS-native fs watcher ([`notify`]): inotify on Linux,
//! FSEvents on macOS, ReadDirectoryChangesW on Windows. The kernel reports a
//! work-tree edit or a `.git` ref/index change; there is NO timer, NO periodic
//! re-read. This is the "sans polling" guarantee: a change RINGS the watcher, and
//! only then do we recompute. We watch BOTH:
//!   * the WORK TREE (recursively) — covers `edit` (a file changed on disk); and
//!   * the `.git` DIR (recursively) — covers `commit` / `checkout` (HEAD, refs, the
//!     index all live under `.git`), including linked-worktree `.git` FILES that
//!     redirect to a `gitdir` elsewhere.
//!
//! ## Debounce: coalesce the burst
//!
//! A single `git checkout` or `git commit` fires MANY inotify events (every touched
//! ref, the index, each rewritten work-tree file). Recomputing per event would
//! thrash a large repo. So every raw event only ARMS a per-repo debounce timer; the
//! recompute+push fires ONCE per quiet window ([`DEBOUNCE`]). A steady stream of
//! edits is bounded to at most one refresh per window (leading events extend the
//! window until the burst settles), so the push frequency is capped regardless of
//! repo size.
//!
//! ## Multi-repo without mixing identities
//!
//! Each watched repo is registered with its FULL identity
//! ([`WatchedRepo`]: `workspace_id` + `repository_id` + the absolute `repo_path` +
//! the optional review `base`/`target`). The refresh the sink receives
//! ([`RepoViewRefresh`]) carries that identity VERBATIM, so a caller aggregating
//! several repos routes each refresh to the right `(workspace, repository)` pane —
//! one repo's branch/counts never leak into another's (cf. phase 1,
//! [`crate::git_views`]).
//!
//! ## Shell-agnostic (the frontier holds)
//!
//! `nyx-core` never names a transport. The recompute lives here; the PUSH is a
//! [`RepoViewSink`] the SHELL implements. In the sidecar that sink emits the v2.1
//! unsolicited `repos://changed` [`crate::frontier::ChangedTopic::Repos`] over the
//! pipe (the same `EventSink` the domain events ride), reaching BOTH the renderer
//! and — through the shared `domain_events`/re-pull path — the MCP surface (PDR-0005:
//! one authority, two façades). Tests inject a recording sink.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::event::EventKind;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::git_views::{self, RepoGitView};

/// The quiet window a repo must stay change-free before a refresh fires. A burst
/// (`git checkout` / `git commit` touches many paths) is coalesced into ONE refresh:
/// each raw event pushes the fire time out by this window, so the recompute runs once
/// the burst settles. Small enough to feel real-time, large enough to swallow a
/// multi-hundred-event checkout on a big repo.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// The identity + review endpoints of ONE repository the watcher tracks. Carried
/// VERBATIM into every [`RepoViewRefresh`] so a multi-repo caller never mixes Git
/// identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedRepo {
    /// The owning workspace id (part of the `(workspace, repository)` identity).
    pub workspace_id: String,
    /// The logical repository id.
    pub repository_id: String,
    /// The ABSOLUTE on-disk path of the repository work tree (the one
    /// [`crate::repository::member_absolute_path`] resolves).
    pub repo_path: String,
    /// The OPTIONAL review base endpoint (a branch or SHA), passed through to
    /// [`git_views::repo_view_with_target`] so ahead/behind stays correct across a
    /// refresh.
    pub base: Option<String>,
    /// The OPTIONAL review target endpoint; `None` defaults to the current branch.
    pub target: Option<String>,
}

impl WatchedRepo {
    /// A living-review watch of a repo with no explicit base/target (dirty/branch only).
    pub fn living(
        workspace_id: impl Into<String>,
        repository_id: impl Into<String>,
        repo_path: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            repository_id: repository_id.into(),
            repo_path: repo_path.into(),
            base: None,
            target: None,
        }
    }

    /// The stable key for this watch — the `(workspace, repository)` pair. Two
    /// registrations of the same pair share one watch (the endpoints of the latest win).
    fn key(&self) -> (String, String) {
        (self.workspace_id.clone(), self.repository_id.clone())
    }
}

/// A REFRESHED derived Git view pushed after a debounced change: the repo's identity
/// (so a multi-repo surface routes it) plus the freshly recomputed [`RepoGitView`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoViewRefresh {
    /// The workspace this refresh is for.
    pub workspace_id: String,
    /// The repository this refresh is for.
    pub repository_id: String,
    /// The freshly recomputed derived view (dirty / branch / ahead-behind / summary).
    pub view: RepoGitView,
}

/// The SHELL seam the watcher pushes a refresh through (frontier 1 — events out). The
/// sidecar's implementation emits the v2.1 `repos://changed`
/// [`crate::frontier::ChangedTopic::Repos`] over the pipe (and can carry the fresh view
/// itself); a test injects a recording fake. `nyx-core` never names the transport.
pub trait RepoViewSink: Send + Sync + 'static {
    /// A repo changed and its derived view was recomputed — push it to the surfaces.
    /// Called at most once per [`DEBOUNCE`] window per repo (coalesced burst).
    fn push(&self, refresh: RepoViewRefresh);
}

/// The command a watcher-manager thread understands.
enum Cmd {
    /// A raw fs event fired for the repo behind this watched PATH (work tree or `.git`).
    /// The manager maps the path back to its `WatchedRepo` and arms the debounce.
    Touched(PathBuf),
    /// Register / update a repo to watch (adds the OS watches, seeds a first refresh).
    Watch(WatchedRepo),
    /// Stop watching the `(workspace, repository)` pair (drops its OS watches).
    Unwatch((String, String)),
    /// Tear the manager thread down.
    Stop,
}

/// The live watcher: an OS fs watcher feeding a debouncing MANAGER thread that
/// recomputes + pushes. Built once ([`GitWatcher::start`]); repos are added/removed at
/// runtime ([`watch`](GitWatcher::watch) / [`unwatch`](GitWatcher::unwatch)). Dropping
/// it stops the thread and releases the OS watches.
pub struct GitWatcher {
    tx: Sender<Cmd>,
    /// Kept alive so its OS watches persist; the manager thread reconfigures it via the
    /// shared handle. `None` only if the OS watcher could not be created (a degraded,
    /// no-op watcher — never an error to the caller).
    watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
    handle: Option<JoinHandle<()>>,
}

impl GitWatcher {
    /// Start the watcher over `sink`. The OS watcher forwards every raw event as a
    /// [`Cmd::Touched`] to the manager thread, which debounces per repo and, on each
    /// quiet window, recomputes the derived view and calls `sink.push`. A failure to
    /// create the OS watcher degrades to a no-op watcher (the on-demand reads still
    /// serve) rather than failing the caller — Git watching is best-effort.
    pub fn start<S: RepoViewSink>(sink: S) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();

        // The OS watcher forwards raw events into the SAME command channel the manager
        // drains, so registration and fs events serialize through one queue (no lock on
        // the hot path). A watch error is dropped (best-effort).
        let event_tx = tx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                // ONLY react to MUTATIONS (create / modify / remove). inotify also reports
                // ACCESS events (open / read / close-nowrite) — and our OWN recompute READS
                // `.git/HEAD` / `refs` / the work tree, which would fire access events and
                // re-arm the debounce forever (a self-sustaining loop). Dropping access
                // events here breaks that loop at the source: a read never counts as a change.
                if is_mutation_event(&event.kind) {
                    for path in event.paths {
                        // Ignore the send error: it only fails once the manager stopped.
                        let _ = event_tx.send(Cmd::Touched(path));
                    }
                }
            }
        })
        .ok();
        let watcher = Arc::new(Mutex::new(watcher));

        let manager_watcher = Arc::clone(&watcher);
        let handle = std::thread::Builder::new()
            .name("nyx-git-watch".into())
            .spawn(move || run_manager(rx, manager_watcher, sink))
            .expect("spawn git-watch manager thread");

        Self {
            tx,
            watcher,
            handle: Some(handle),
        }
    }

    /// Register (or update the endpoints of) a repo to watch. Idempotent per
    /// `(workspace, repository)`: a re-`watch` of the same pair updates its base/target
    /// and re-seeds a refresh, it does not double-watch. A non-git or absent path is a
    /// no-op watch (nothing fires) — git-agnostic, never an error.
    pub fn watch(&self, repo: WatchedRepo) {
        let _ = self.tx.send(Cmd::Watch(repo));
    }

    /// Stop watching a `(workspace, repository)` pair (releases its OS watches). A pair
    /// that was never watched is a no-op.
    pub fn unwatch(&self, workspace_id: &str, repository_id: &str) {
        let _ = self.tx.send(Cmd::Unwatch((
            workspace_id.to_string(),
            repository_id.to_string(),
        )));
    }
}

impl Drop for GitWatcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        // Drop the OS watcher first so its callback (which sends on `tx`) stops before we
        // join — otherwise a late event could wedge the join on a full channel.
        *self.watcher.lock().unwrap() = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// One watched repo's runtime state inside the manager: its identity/endpoints, the
/// on-disk ROOTS the OS watcher fires for (work-tree root + resolved gitdir), and the
/// current debounce DEADLINE (`Some` while a burst is settling, `None` when idle).
struct RepoState {
    repo: WatchedRepo,
    /// The set of on-disk roots watched for this repo (the work-tree root, and the
    /// resolved gitdir when it lives OUTSIDE the work tree — a linked worktree). Used to
    /// map a raw event path back to this repo.
    watched_paths: Vec<PathBuf>,
    /// The repo's work-tree root — the reference for classifying an event path as a
    /// work-tree edit vs `.git` internal churn ([`is_significant_change`]).
    work_tree: PathBuf,
    /// The repo's resolved gitdir (`.git` dir, or a linked-worktree redirect target), when
    /// known — the reference for keeping only STATE-BEARING `.git` events.
    gitdir: Option<PathBuf>,
    /// When set, a refresh is due at this instant unless a new event pushes it later.
    deadline: Option<Instant>,
}

/// The debouncing MANAGER loop: drains commands (raw fs touches + register/unwatch),
/// maps a touched path back to its repo, arms/extends that repo's debounce, and — when a
/// repo's quiet window elapses — recomputes its derived view and pushes it ONCE.
fn run_manager<S: RepoViewSink>(
    rx: Receiver<Cmd>,
    watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
    sink: S,
) {
    // The watched repos, keyed by `(workspace, repository)`.
    let mut repos: HashMap<(String, String), RepoState> = HashMap::new();

    loop {
        // Wake either on the next command OR when the SOONEST pending debounce is due,
        // whichever comes first — so a settled burst fires without needing a further
        // event to nudge the loop (no polling: the timeout is the debounce deadline).
        let now = Instant::now();
        let next_deadline = repos.values().filter_map(|s| s.deadline).min();
        let recv = match next_deadline {
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(now);
                rx.recv_timeout(wait)
            }
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };

        match recv {
            Ok(Cmd::Touched(path)) => {
                arm_debounce_for_path(&mut repos, &path);
            }
            Ok(Cmd::Watch(repo)) => {
                register_repo(&mut repos, &watcher, repo, &sink);
            }
            Ok(Cmd::Unwatch(key)) => {
                if let Some(state) = repos.remove(&key) {
                    if let Some(w) = watcher.lock().unwrap().as_mut() {
                        for p in &state.watched_paths {
                            let _ = w.unwatch(p);
                        }
                    }
                }
            }
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => { /* fall through to fire due refreshes */ }
        }

        // Fire every repo whose debounce window has now elapsed (recompute + push ONCE).
        let now = Instant::now();
        for state in repos.values_mut() {
            if let Some(deadline) = state.deadline {
                if now >= deadline {
                    state.deadline = None;
                    let view = git_views::repo_view_with_target(
                        &state.repo.repo_path,
                        state.repo.base.as_deref(),
                        state.repo.target.as_deref(),
                    );
                    sink.push(RepoViewRefresh {
                        workspace_id: state.repo.workspace_id.clone(),
                        repository_id: state.repo.repository_id.clone(),
                        view,
                    });
                }
            }
        }
    }
}

/// Arm (or extend) the debounce for whichever watched repo owns `path`, but ONLY for a
/// SIGNIFICANT change ([`is_significant_change`]). A significant event pushes that repo's
/// deadline out by [`DEBOUNCE`], coalescing the burst. A path under no watched repo, or an
/// insignificant `.git` internal (loose-object writes, lock scratch, logs), is ignored so
/// the watcher never thrashes on the storm of writes a single commit produces.
fn arm_debounce_for_path(repos: &mut HashMap<(String, String), RepoState>, path: &Path) {
    let deadline = Instant::now() + DEBOUNCE;
    for state in repos.values_mut() {
        if !state
            .watched_paths
            .iter()
            .any(|root| path.starts_with(root))
        {
            continue;
        }
        if !is_significant_change(state, path) {
            continue;
        }
        state.deadline = Some(deadline);
    }
}

/// Whether an event on `path` (already known to belong to `state`) is a SIGNIFICANT
/// change that should refresh the derived view — the core noise filter.
///
/// Two cases:
///   * OUTSIDE the gitdir (a work-tree file) → SIGNIFICANT: it is an `edit`.
///   * INSIDE the gitdir → significant ONLY for the STATE-BEARING paths a
///     commit/checkout mutates ([`is_state_bearing_git_path`]): `HEAD`, `refs/…`,
///     `packed-refs`, `index`, `MERGE_HEAD`/`ORIG_HEAD`, `logs/HEAD`. Everything else
///     under `.git` — the flood of `objects/…` loose-object writes a commit emits, the
///     `*.lock` scratch, `COMMIT_EDITMSG`, hooks/config reads — is IGNORED. This is what
///     keeps a commit (hundreds of object writes) from arming the debounce hundreds of
///     times: only the final HEAD/refs/index update, the thing that actually changes the
///     derived view, counts.
fn is_significant_change(state: &RepoState, path: &Path) -> bool {
    match &state.gitdir {
        Some(gitdir) if path.starts_with(gitdir) => is_state_bearing_git_path(gitdir, path),
        // Also guard the normal-checkout case where the gitdir is `<work_tree>/.git`: an
        // event under `<work_tree>/.git` is `.git` churn even if `gitdir` was not resolved.
        _ if path.starts_with(state.work_tree.join(".git")) => {
            is_state_bearing_git_path(&state.work_tree.join(".git"), path)
        }
        _ => true, // a work-tree edit
    }
}

/// Whether a path INSIDE `gitdir` bears review-relevant STATE (so a change to it should
/// refresh the derived view). Kept deliberately small: the branch/dirty/ahead-behind view
/// only moves when HEAD, a ref, the packed refs, the index, or a merge-state pointer moves.
/// The `logs/HEAD` reflog also flags a commit/checkout. Loose objects, `COMMIT_EDITMSG`,
/// `*.lock` scratch, config, and hooks are NOT state-bearing for this view.
fn is_state_bearing_git_path(gitdir: &Path, path: &Path) -> bool {
    // A `*.lock` is the transient sibling of an atomic write; ignore it (the renamed
    // final target — `HEAD`, `index`, a ref — fires its own event).
    if path.extension().and_then(|e| e.to_str()) == Some("lock") {
        return false;
    }
    let Ok(rel) = path.strip_prefix(gitdir) else {
        return false;
    };
    // The top-level component under the gitdir decides.
    let mut comps = rel.components();
    let Some(first) = comps.next().and_then(|c| c.as_os_str().to_str()) else {
        return false;
    };
    match first {
        // Single-file state pointers a commit/checkout/merge rewrites.
        "HEAD" | "ORIG_HEAD" | "MERGE_HEAD" | "FETCH_HEAD" | "index" | "packed-refs" => true,
        // Any ref update (`refs/heads/…`, `refs/tags/…`, `refs/remotes/…`).
        "refs" => true,
        // The reflog: `logs/HEAD` (and per-ref logs) flag a commit/checkout.
        "logs" => true,
        // Everything else under `.git` is internal churn we do not refresh on.
        _ => false,
    }
}

/// Whether a raw fs event is a MUTATION worth reacting to: a create / modify / remove /
/// any. An ACCESS event (open / read / close-nowrite) is NOT — a read never changes the
/// repo, and our own recompute reads `.git`, so counting accesses would loop. `Other` is
/// kept (a coarse backend that cannot classify still signals a change).
fn is_mutation_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(_)
            | EventKind::Remove(_)
            | EventKind::Any
            | EventKind::Other
    )
}

/// Register (or update) a repo: resolve the paths to watch (work tree + gitdir), add the
/// OS watches, store the state, and SEED a first refresh so a freshly-opened surface has
/// the current view without waiting for the next change. A re-registration of the same
/// pair updates its endpoints and re-adds watches idempotently.
fn register_repo<S: RepoViewSink>(
    repos: &mut HashMap<(String, String), RepoState>,
    watcher: &Arc<Mutex<Option<RecommendedWatcher>>>,
    repo: WatchedRepo,
    sink: &S,
) {
    let key = repo.key();
    let work_tree = PathBuf::from(&repo.repo_path);
    let gitdir = resolve_gitdir(&work_tree);
    let watched_paths = watch_targets(&work_tree, gitdir.as_deref());

    // Add the OS watches (best-effort: a missing path just never fires). Watching the
    // work-tree root recursively covers BOTH work-tree edits and — since `.git` lives
    // under it — the state-bearing `.git` files (HEAD/refs/index). A linked worktree's
    // gitdir lives ELSEWHERE, so it is watched separately. The event FILTER
    // ([`is_significant_change`]) drops the object-write storm; here we just cast a wide
    // enough net to see every real change.
    if let Some(w) = watcher.lock().unwrap().as_mut() {
        for p in &watched_paths {
            let _ = w.watch(p, RecursiveMode::Recursive);
        }
    }

    repos.insert(
        key,
        RepoState {
            repo: repo.clone(),
            watched_paths,
            work_tree,
            gitdir,
            deadline: None,
        },
    );

    // Seed one refresh with the current view (so the surface starts from truth).
    let view = git_views::repo_view_with_target(
        &repo.repo_path,
        repo.base.as_deref(),
        repo.target.as_deref(),
    );
    sink.push(RepoViewRefresh {
        workspace_id: repo.workspace_id,
        repository_id: repo.repository_id,
        view,
    });
}

/// The on-disk roots to watch for a repo: the WORK TREE root (always, when it exists) plus
/// the resolved GITDIR ONLY when it lives OUTSIDE the work tree (a linked worktree — a
/// normal `.git` dir is already under the recursively-watched work tree). Watching the
/// gitdir is what makes a `commit`/`checkout` (which mutates HEAD/refs/index under the
/// gitdir, sometimes outside the work tree) ring the watcher. Non-existent paths are
/// dropped (best-effort).
fn watch_targets(work_tree: &Path, gitdir: Option<&Path>) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if work_tree.exists() {
        targets.push(work_tree.to_path_buf());
    }
    if let Some(gitdir) = gitdir {
        // Only add the gitdir separately when it is NOT already inside the watched work
        // tree (a linked-worktree gitdir lives elsewhere entirely; a normal `.git` dir is
        // already covered by the work-tree recursive watch).
        if gitdir.exists() && !targets.iter().any(|t| gitdir.starts_with(t)) {
            targets.push(gitdir.to_path_buf());
        }
    }
    targets
}

/// Resolve a work tree's GITDIR. A normal checkout has a `.git` DIRECTORY; a linked
/// worktree (`git worktree add`) has a `.git` FILE reading `gitdir: <path>` that
/// redirects to the real gitdir under the main repo's `worktrees/<name>`. Returns the
/// resolved path, or `None` when `.git` is absent / unreadable (a non-git path).
fn resolve_gitdir(work_tree: &Path) -> Option<PathBuf> {
    let dot_git = work_tree.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    // `.git` is a FILE (linked worktree): parse `gitdir: <path>`.
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let rest = contents.trim().strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(rest);
    let resolved = if path.is_absolute() {
        path
    } else {
        work_tree.join(path)
    };
    // Best-effort canonicalization; fall back to the joined path if it fails.
    Some(std::fs::canonicalize(&resolved).unwrap_or(resolved))
}

#[cfg(test)]
mod tests;
