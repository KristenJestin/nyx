//! Tests for the v2.4 real-time refresh of the derived Git views (Q-015, task
//! `01KX0F6GDZEH3YN6QJDB7FC0WC`). Each test proves a slice of the done criterion:
//! "une modif du repo (commit/checkout/edit) rafraîchit les vues et pousse la MAJ …,
//! sans polling, avec debounce ; branché sur le bus temps réel v2.1."

use super::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// A recording [`RepoViewSink`] — the test twin of the sidecar's pipe sink. It captures
/// every pushed [`RepoViewRefresh`] so a test asserts WHICH repo refreshed and HOW MANY
/// times (the coalescing / no-polling guarantees).
#[derive(Clone, Default)]
struct RecordingSink {
    pushes: Arc<Mutex<Vec<RepoViewRefresh>>>,
}

impl RecordingSink {
    fn all(&self) -> Vec<RepoViewRefresh> {
        self.pushes.lock().unwrap().clone()
    }
    /// Pushes for a specific `(workspace, repository)` pair, in order.
    fn for_repo(&self, workspace_id: &str, repository_id: &str) -> Vec<RepoViewRefresh> {
        self.all()
            .into_iter()
            .filter(|r| r.workspace_id == workspace_id && r.repository_id == repository_id)
            .collect()
    }
    fn count(&self) -> usize {
        self.pushes.lock().unwrap().len()
    }
}

impl RepoViewSink for RecordingSink {
    fn push(&self, refresh: RepoViewRefresh) {
        self.pushes.lock().unwrap().push(refresh);
    }
}

fn git_available() -> bool {
    crate::proc_util::command("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_in(dir: &Path, args: &[&str]) {
    let status = crate::proc_util::command("git")
        .args([
            "-c",
            "user.email=test@nyx",
            "-c",
            "user.name=nyx-test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-gitwatch-{tag}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn init_repo(dir: &Path, branch: &str) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git_in(dir, &["init", "-q"]);
    git_in(
        dir,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
    );
    std::fs::write(dir.join("README.md"), b"nyx").expect("seed file");
    git_in(dir, &["add", "README.md"]);
    git_in(dir, &["commit", "-q", "-m", "init"]);
}

/// Wait until `pred` holds or `timeout` elapses (event-driven test aid: the watcher
/// pushes asynchronously). Returns whether the predicate held in time.
fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pred()
}

/// done_criterion — "une modif du repo (**edit**) rafraîchit les vues et pousse la MAJ,
/// sans polling, avec debounce." A watched clean repo, then a WORK-TREE edit → the sink
/// receives a refresh whose recomputed view is now DIRTY. The push arrives from an fs
/// event (inotify/FSEvents), not a timer — no poll loop exists in the manager.
#[test]
fn a_work_tree_edit_pushes_a_refreshed_dirty_view() {
    if !git_available() {
        eprintln!("skipping git-watch edit test: git unavailable");
        return;
    }
    let repo = scratch_dir("edit");
    init_repo(&repo, "main");

    let sink = RecordingSink::default();
    let watcher = GitWatcher::start(sink.clone());
    watcher.watch(WatchedRepo::living("ws1", "repoA", repo.to_str().unwrap()));

    // The seed refresh lands: clean repo, on `main`.
    assert!(
        wait_until(Duration::from_secs(3), || !sink
            .for_repo("ws1", "repoA")
            .is_empty()),
        "the initial seed refresh was pushed"
    );
    let seed = sink.for_repo("ws1", "repoA").pop().unwrap();
    assert!(seed.view.dirty.clean, "seed view is clean");
    assert_eq!(seed.view.branch.current.as_deref(), Some("main"));
    let seed_count = sink.count();

    // EDIT the work tree — an fs event the watcher must ring on.
    std::fs::write(repo.join("README.md"), b"changed on disk").unwrap();

    // A refresh with a now-DIRTY view arrives (recomputed via git_views, not the DB).
    let got_dirty = wait_until(Duration::from_secs(5), || {
        sink.for_repo("ws1", "repoA")
            .iter()
            .any(|r| !r.view.dirty.clean)
    });
    assert!(got_dirty, "the edit pushed a refreshed DIRTY view");
    let last = sink.for_repo("ws1", "repoA").pop().unwrap();
    assert!(!last.view.dirty.clean, "recomputed view reflects the edit");
    assert!(
        last.view.dirty.modified.iter().any(|p| p == "README.md"),
        "the modified file is in the refreshed view: {:?}",
        last.view.dirty.modified
    );
    assert!(sink.count() > seed_count, "the edit produced a fresh push");

    drop(watcher);
    let _ = std::fs::remove_dir_all(&repo);
}

/// done_criterion — "une modif du repo (**commit**) rafraîchit les vues." A commit
/// mutates HEAD/refs/index UNDER `.git`; the gitdir watch must ring so the derived view
/// (branch/summary) is recomputed and pushed. After committing the dirty file, the
/// refreshed view is clean again.
#[test]
fn a_commit_pushes_a_refreshed_view() {
    if !git_available() {
        eprintln!("skipping git-watch commit test: git unavailable");
        return;
    }
    let repo = scratch_dir("commit");
    init_repo(&repo, "main");
    // Make the repo dirty BEFORE watching, so the seed is dirty and the commit flips it clean.
    std::fs::write(repo.join("README.md"), b"pending change").unwrap();

    let sink = RecordingSink::default();
    let watcher = GitWatcher::start(sink.clone());
    watcher.watch(WatchedRepo::living("ws1", "repoA", repo.to_str().unwrap()));

    assert!(
        wait_until(Duration::from_secs(3), || sink
            .for_repo("ws1", "repoA")
            .iter()
            .any(|r| !r.view.dirty.clean)),
        "seed refresh shows the pending change (dirty)"
    );

    // COMMIT — mutates `.git` (HEAD/refs/index). The gitdir watch must fire.
    git_in(&repo, &["commit", "-aqm", "apply change"]);

    let got_clean = wait_until(Duration::from_secs(5), || {
        sink.for_repo("ws1", "repoA")
            .iter()
            .any(|r| r.view.dirty.clean)
    });
    assert!(got_clean, "the commit pushed a refreshed CLEAN view");

    drop(watcher);
    let _ = std::fs::remove_dir_all(&repo);
}

/// done_criterion — "une modif du repo (**checkout**) rafraîchit les vues." A branch
/// switch mutates `.git/HEAD` + `.git/logs/HEAD` (and `.git/index`), all state-bearing
/// under the gitdir watch ([`is_state_bearing_git_path`]). The watch must ring so the
/// recomputed view carries the NEW current branch. We seed on `main`, then
/// `git checkout -b feature`, and assert a refresh lands whose `branch.current` is
/// `feature`.
#[test]
fn a_checkout_pushes_a_refreshed_view_on_the_new_branch() {
    if !git_available() {
        eprintln!("skipping git-watch checkout test: git unavailable");
        return;
    }
    let repo = scratch_dir("checkout");
    init_repo(&repo, "main");

    let sink = RecordingSink::default();
    let watcher = GitWatcher::start(sink.clone());
    watcher.watch(WatchedRepo::living("ws1", "repoA", repo.to_str().unwrap()));

    // The seed refresh lands on `main`.
    assert!(
        wait_until(Duration::from_secs(3), || sink
            .for_repo("ws1", "repoA")
            .iter()
            .any(|r| r.view.branch.current.as_deref() == Some("main"))),
        "seed refresh carries the starting branch (main)"
    );

    // CHECKOUT a new branch — mutates `.git/HEAD` + `.git/logs/HEAD` (state-bearing).
    git_in(&repo, &["checkout", "-b", "feature"]);

    // A refresh whose recomputed view is now on `feature` arrives (recomputed via
    // git_views, from the gitdir event — not a poll).
    let switched = wait_until(Duration::from_secs(5), || {
        sink.for_repo("ws1", "repoA")
            .iter()
            .any(|r| r.view.branch.current.as_deref() == Some("feature"))
    });
    assert!(
        switched,
        "the checkout pushed a refreshed view on the new branch"
    );
    let last = sink.for_repo("ws1", "repoA").pop().unwrap();
    assert_eq!(
        last.view.branch.current.as_deref(),
        Some("feature"),
        "the recomputed view reflects the branch switch"
    );

    drop(watcher);
    let _ = std::fs::remove_dir_all(&repo);
}

/// done_criterion — "**avec debounce**" + "sans polling". A BURST of many rapid edits
/// (what a `git checkout` looks like on the fs) must coalesce into a BOUNDED number of
/// pushes — far fewer than the number of events — and, once the burst settles, NO further
/// push happens (no polling: an idle repo is silent). We assert the extra pushes after the
/// seed are strictly bounded and that the repo goes quiet afterwards.
#[test]
fn a_burst_of_edits_is_coalesced_and_then_silent() {
    if !git_available() {
        eprintln!("skipping git-watch debounce test: git unavailable");
        return;
    }
    let repo = scratch_dir("burst");
    init_repo(&repo, "main");

    let sink = RecordingSink::default();
    let watcher = GitWatcher::start(sink.clone());
    watcher.watch(WatchedRepo::living("ws1", "repoA", repo.to_str().unwrap()));
    assert!(
        wait_until(Duration::from_secs(3), || sink.count() >= 1),
        "seed refresh landed"
    );
    let after_seed = sink.count();

    // Fire a rapid BURST: 50 writes to many files within one debounce window.
    for i in 0..50 {
        std::fs::write(repo.join(format!("f{i}.txt")), format!("v{i}")).unwrap();
        std::fs::write(repo.join("README.md"), format!("burst {i}")).unwrap();
    }

    // Wait past the debounce window for the coalesced refresh to fire.
    assert!(
        wait_until(Duration::from_secs(5), || sink.count() > after_seed),
        "the burst produced at least one coalesced refresh"
    );
    // Let the loop settle well past the debounce window.
    std::thread::sleep(DEBOUNCE * 4);
    let coalesced = sink.count() - after_seed;
    assert!(
        coalesced <= 4,
        "the 100 fs writes coalesced into at most a few pushes (got {coalesced}) — debounce, not per-event"
    );
    // The final refreshed view reflects the burst (dirty with the new files).
    let last = sink.for_repo("ws1", "repoA").pop().unwrap();
    assert!(!last.view.dirty.clean, "coalesced view reflects the burst");

    // NO POLLING: with the repo now idle, no further push happens over a window several
    // times the debounce. A poll loop would keep pushing; an event-driven watcher stays silent.
    let quiescent = sink.count();
    std::thread::sleep(DEBOUNCE * 6);
    assert_eq!(
        sink.count(),
        quiescent,
        "an idle repo produces NO further pushes — event-driven, not polled"
    );

    drop(watcher);
    let _ = std::fs::remove_dir_all(&repo);
}

/// done_criterion — multi-repo "sans mélange d'identités". TWO watched repos; editing
/// ONE pushes a refresh ONLY for that `(workspace, repository)` identity, carrying its own
/// path/branch — the other repo's view never leaks in. (cf. phase 1 identity keying.)
#[test]
fn multi_repo_refreshes_stay_keyed_to_their_own_identity() {
    if !git_available() {
        eprintln!("skipping git-watch multi-repo test: git unavailable");
        return;
    }
    let root = scratch_dir("multi");
    let api = root.join("api");
    let web = root.join("web");
    init_repo(&api, "main");
    init_repo(&web, "feature");

    let sink = RecordingSink::default();
    let watcher = GitWatcher::start(sink.clone());
    watcher.watch(WatchedRepo::living("ws1", "api", api.to_str().unwrap()));
    watcher.watch(WatchedRepo::living("ws1", "web", web.to_str().unwrap()));

    // Both seeds land, each keyed to its own repo with its own branch.
    assert!(
        wait_until(Duration::from_secs(3), || !sink
            .for_repo("ws1", "api")
            .is_empty()
            && !sink.for_repo("ws1", "web").is_empty()),
        "both repos seeded"
    );
    assert_eq!(
        sink.for_repo("ws1", "api")[0]
            .view
            .branch
            .current
            .as_deref(),
        Some("main"),
        "api's seed carries api's branch"
    );
    assert_eq!(
        sink.for_repo("ws1", "web")[0]
            .view
            .branch
            .current
            .as_deref(),
        Some("feature"),
        "web's seed carries web's branch"
    );
    let web_pushes_before = sink.for_repo("ws1", "web").len();

    // Edit ONLY `api`.
    std::fs::write(api.join("README.md"), b"api changed").unwrap();

    // A dirty refresh arrives for `api` ONLY.
    assert!(
        wait_until(Duration::from_secs(5), || sink
            .for_repo("ws1", "api")
            .iter()
            .any(|r| !r.view.dirty.clean)),
        "api got a dirty refresh"
    );
    // `web` produced NO extra push (its identity never mixed with api's edit).
    std::thread::sleep(DEBOUNCE * 3);
    assert_eq!(
        sink.for_repo("ws1", "web").len(),
        web_pushes_before,
        "web received no spurious refresh from api's edit — identities stay separate"
    );
    // Every api refresh carries api's own path (never web's).
    for r in sink.for_repo("ws1", "api") {
        assert_eq!(r.view.repo_path, api.to_str().unwrap());
        assert!(!r.view.repo_path.contains("web"), "no identity leak");
    }

    drop(watcher);
    let _ = std::fs::remove_dir_all(&root);
}

/// Unwatch releases a repo: after `unwatch`, an edit produces NO further push for that
/// pair (the OS watch was dropped). Guards the runtime add/remove path the sidecar uses
/// as reviews open/close.
#[test]
fn unwatch_stops_refreshes_for_a_pair() {
    if !git_available() {
        eprintln!("skipping git-watch unwatch test: git unavailable");
        return;
    }
    let repo = scratch_dir("unwatch");
    init_repo(&repo, "main");

    let sink = RecordingSink::default();
    let watcher = GitWatcher::start(sink.clone());
    watcher.watch(WatchedRepo::living("ws1", "repoA", repo.to_str().unwrap()));
    assert!(
        wait_until(Duration::from_secs(3), || sink.count() >= 1),
        "seed landed"
    );

    watcher.unwatch("ws1", "repoA");
    // Give the unwatch command time to drop the OS watch.
    std::thread::sleep(DEBOUNCE * 2);
    let baseline = sink.count();

    std::fs::write(repo.join("README.md"), b"after unwatch").unwrap();
    std::thread::sleep(DEBOUNCE * 6);
    assert_eq!(
        sink.count(),
        baseline,
        "no refresh after unwatch — the OS watch was released"
    );

    drop(watcher);
    let _ = std::fs::remove_dir_all(&repo);
}
