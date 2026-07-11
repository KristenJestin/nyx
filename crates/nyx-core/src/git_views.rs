//! The v2.4 DERIVED GIT VIEWS (PRD `01KX0EEP5H1MJW4T3QCZEWPHFK`, task
//! `01KX0F6FXE6AJ9BR4BYHRX6B1R`, Q-015 / PDR-0013 / PDR-0015) — the per-repo Git state
//! the review and the cockpit need, in ONE stable, serializable shape consumed by BOTH
//! the renderer AND MCP.
//!
//! ## Why this module exists
//!
//! v2.1 deferred the "vues dérivées de Git" (Q-015). v2.4 slices them here: per
//! repository, the DIRTY state (modified / staged / untracked file lists), the CURRENT
//! BRANCH plus its base/target, the AHEAD/BEHIND counts, and a compact change SUMMARY.
//! These sit ALONGSIDE the line-level diff engine ([`crate::git_diff`]) — the diff shows
//! WHAT changed line by line; these views show the repo's OVERALL state at a glance.
//!
//! ## Computed ON DEMAND (no real-time here)
//!
//! Every field is RE-READ from Git each call and NEVER persisted (PDR-0013: the DB is
//! not authoritative for Git state). This module issues only reads. The real-time
//! refresh (watch fs/git, push to UI/MCP) is a SEPARATE phase-2 task; here the caller
//! asks and gets a fresh snapshot.
//!
//! ## gix PRIMARY, git CLI FALLBACK
//!
//! The branch/base/ahead-behind facts come from `gix` first (repo discovery, ref
//! resolution, ahead/behind via a hidden-tips revwalk); a `git` CLI fallback covers the
//! cases gix cannot resolve. The dirty file partition uses `git status --porcelain`
//! (the stable, unambiguous porcelain, already the codebase's authority for the
//! changed-file summary) so the renderer and MCP see identical file sets.
//!
//! ## Multi-repo without mixing identities
//!
//! Every derived view is keyed on ONE repository's absolute path (the per-workspace
//! membership path from [`crate::repository::member_absolute_path`]). A caller aggregates
//! several views (one per `(workspace, repository)`) without any cross-repo state living
//! here — one repo's branch/counts never leak into another's.

/// The DIRTY state of a repository: the three porcelain partitions (staged, unstaged/
/// modified, untracked), each a list of paths. Stable + serializable for the renderer
/// and MCP. Empty lists + `clean=true` for a clean tree or a non-git path.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct DirtyState {
    /// `true` when NOTHING is staged/modified/untracked (a clean tree, or non-git path).
    pub clean: bool,
    /// Paths with STAGED changes (index differs from HEAD — the porcelain `X` column).
    pub staged: Vec<String>,
    /// Paths with UNSTAGED changes (work tree differs from index — the `Y` column),
    /// excluding untracked.
    pub modified: Vec<String>,
    /// UNTRACKED paths (present on disk, unknown to Git — the `??` code).
    pub untracked: Vec<String>,
}

/// The BRANCH view of a repository: the current branch and its base/target for a review.
/// `base`/`target` are the review's comparison endpoints — for a living review, the
/// target is the current branch and the base is whatever the review compares against
/// (a local branch or a SHA; there is NO remote, so no `origin` is assumed).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct BranchView {
    /// The current HEAD branch, or `None` when detached / unborn / non-git.
    pub current: Option<String>,
    /// `true` when HEAD is detached (no current branch but a valid commit).
    pub detached: bool,
    /// The review BASE endpoint (a branch name or SHA), when one was requested.
    pub base: Option<String>,
    /// The review TARGET endpoint (a branch name or SHA); defaults to the current
    /// branch for a living review when none was requested.
    pub target: Option<String>,
}

/// The AHEAD/BEHIND counts of `target` relative to `base`: how many commits `target` has
/// that `base` lacks (ahead) and vice versa (behind). `None` when there is no base to
/// compare against, or either endpoint does not resolve. Computed on demand.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct AheadBehind {
    /// Commits on `target` not on `base`.
    pub ahead: u32,
    /// Commits on `base` not on `target`.
    pub behind: u32,
}

/// A compact CHANGE SUMMARY: the counts a cockpit badge shows without opening the diff.
/// Derived from the dirty partition + ahead/behind. Stable + serializable.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ChangeSummary {
    /// Total number of files with ANY pending change (staged ∪ modified ∪ untracked).
    pub changed_files: u32,
    /// How many of those are staged.
    pub staged: u32,
    /// How many are modified (unstaged, tracked).
    pub modified: u32,
    /// How many are untracked.
    pub untracked: u32,
    /// Commits ahead of base (mirrors [`AheadBehind::ahead`]), when a base was given.
    pub ahead: u32,
    /// Commits behind base (mirrors [`AheadBehind::behind`]), when a base was given.
    pub behind: u32,
}

/// The full DERIVED GIT VIEW of ONE repository — the unit the renderer/MCP consumes.
/// Carries the repo's identity (`repo_path`) so several can be aggregated without mixing
/// Git identities.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoGitView {
    /// The absolute path of the repository work tree this view is for.
    pub repo_path: String,
    /// `true` when `repo_path` is a Git work tree (all fields are best-effort otherwise).
    pub is_git: bool,
    /// The dirty file partition (staged / modified / untracked).
    pub dirty: DirtyState,
    /// The branch view (current + base/target).
    pub branch: BranchView,
    /// The ahead/behind counts vs base (`None` when no base / unresolved).
    pub ahead_behind: Option<AheadBehind>,
    /// The compact change summary.
    pub summary: ChangeSummary,
}

/// Derive the [`RepoGitView`] of the repository at `repo_path`, on demand.
///
/// `base` is the OPTIONAL review comparison endpoint (a local branch or a SHA — no
/// remote is assumed). When given, ahead/behind is computed for `target` (the current
/// branch, or `base_target_override` if provided) relative to `base`. A non-git path
/// degrades to `is_git=false` with empty/clean fields — never an error, never a panic
/// (git-agnostic, PDR-0013).
pub fn repo_view(repo_path: &str, base: Option<&str>) -> RepoGitView {
    repo_view_with_target(repo_path, base, None)
}

/// Like [`repo_view`] but with an explicit review TARGET endpoint (e.g. a specific
/// branch or SHA to compare against `base`, rather than the current branch). Used by a
/// review that pins the target.
pub fn repo_view_with_target(
    repo_path: &str,
    base: Option<&str>,
    target_override: Option<&str>,
) -> RepoGitView {
    let is_git =
        std::path::Path::new(repo_path).join(".git").exists() || gix::open(repo_path).is_ok();

    if !is_git {
        return RepoGitView {
            repo_path: repo_path.to_string(),
            is_git: false,
            dirty: DirtyState {
                clean: true,
                ..Default::default()
            },
            branch: BranchView::default(),
            ahead_behind: None,
            summary: ChangeSummary::default(),
        };
    }

    let dirty = dirty_state(repo_path);
    let (current, detached) = current_branch(repo_path);
    let target = target_override
        .map(str::to_string)
        .or_else(|| current.clone());

    let ahead_behind = match (base, target.as_deref()) {
        (Some(base_spec), Some(target_spec)) => {
            count_ahead_behind(repo_path, base_spec, target_spec)
        }
        _ => None,
    };

    let branch = BranchView {
        current: current.clone(),
        detached,
        base: base.map(str::to_string),
        target,
    };

    let summary = ChangeSummary {
        changed_files: (dirty.staged.len() + dirty.modified.len() + dirty.untracked.len()) as u32,
        staged: dirty.staged.len() as u32,
        modified: dirty.modified.len() as u32,
        untracked: dirty.untracked.len() as u32,
        ahead: ahead_behind.as_ref().map(|ab| ab.ahead).unwrap_or(0),
        behind: ahead_behind.as_ref().map(|ab| ab.behind).unwrap_or(0),
    };

    RepoGitView {
        repo_path: repo_path.to_string(),
        is_git: true,
        dirty,
        branch,
        ahead_behind,
        summary,
    }
}

/// The dirty file partition from `git status --porcelain -z`: the porcelain XY code
/// drives which partition a path lands in. Staged = `X` is not space/`?`; modified =
/// `Y` is not space; untracked = `??`. A path can be BOTH staged and modified (`MM`),
/// so it appears in both lists — matching Git's own semantics.
fn dirty_state(repo_path: &str) -> DirtyState {
    // `--no-optional-locks`: a plain `git status` REFRESHES + REWRITES `.git/index`
    // (the stat cache), a WRITE side effect. That is harmless for the on-demand read, but
    // it would make the v2.4 real-time watcher ([`crate::git_watch`]) — which watches the
    // gitdir — ring on our OWN recompute, a self-sustaining feedback loop. The flag makes
    // the status a TRUE read (no index write), so a recompute never re-triggers itself.
    let output = match crate::proc_util::command("git")
        .args([
            "-C",
            repo_path,
            "--no-optional-locks",
            "status",
            "--porcelain",
            "-z",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => {
            return DirtyState {
                clean: true,
                ..Default::default()
            }
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut records = stdout.split('\0');
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    while let Some(record) = records.next() {
        if record.is_empty() || record.len() < 3 {
            continue;
        }
        let x = record.as_bytes()[0];
        let y = record.as_bytes()[1];
        let path = record[3..].to_string();
        // A rename/copy record carries a SECOND NUL-delimited origin field — consume it.
        if x == b'R' || x == b'C' {
            let _origin = records.next();
        }
        if x == b'?' && y == b'?' {
            untracked.push(path);
            continue;
        }
        if x != b' ' && x != b'?' {
            staged.push(path.clone());
        }
        if y != b' ' && y != b'?' {
            modified.push(path);
        }
    }
    let clean = staged.is_empty() && modified.is_empty() && untracked.is_empty();
    DirtyState {
        clean,
        staged,
        modified,
        untracked,
    }
}

/// The current branch + detached flag. gix primary (`HEAD` ref), git CLI fallback.
/// Returns `(None, true)` for a detached HEAD, `(None, false)` for an unborn/non-git.
fn current_branch(repo_path: &str) -> (Option<String>, bool) {
    // gix primary.
    if let Ok(repo) = gix::open(repo_path) {
        if let Ok(head) = repo.head() {
            if let Some(name) = head.referent_name() {
                let short = name.shorten().to_string();
                return (Some(short), false);
            }
            // A valid HEAD with no symbolic referent → detached (points at a commit).
            if head.id().is_some() {
                return (None, true);
            }
            return (None, false); // unborn
        }
    }
    // git CLI fallback: `symbolic-ref` gives the branch; a failure with a valid HEAD
    // means detached.
    let branch = crate::proc_util::command("git")
        .args([
            "-C",
            repo_path,
            "symbolic-ref",
            "--quiet",
            "--short",
            "HEAD",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    if branch.is_some() {
        return (branch, false);
    }
    let has_head = crate::proc_util::command("git")
        .args(["-C", repo_path, "rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    (None, has_head)
}

/// Count `(ahead, behind)` of `target` vs `base`. gix primary (a revwalk that hides the
/// opposite side's ancestors), git CLI fallback (`git rev-list --count --left-right`).
/// `None` when either endpoint does not resolve.
fn count_ahead_behind(repo_path: &str, base: &str, target: &str) -> Option<AheadBehind> {
    // gix primary.
    if let Some(ab) = gix_ahead_behind(repo_path, base, target) {
        return Some(ab);
    }
    // git CLI fallback.
    cli_ahead_behind(repo_path, base, target)
}

/// gix ahead/behind: resolve both endpoints, then count commits reachable from one tip
/// while hiding the other tip's ancestry.
fn gix_ahead_behind(repo_path: &str, base: &str, target: &str) -> Option<AheadBehind> {
    use gix::bstr::BStr;
    let repo = gix::open(repo_path).ok()?;
    let base_id = repo
        .rev_parse_single(BStr::new(base.as_bytes()))
        .ok()?
        .detach();
    let target_id = repo
        .rev_parse_single(BStr::new(target.as_bytes()))
        .ok()?
        .detach();

    // ahead: commits reachable from target, hiding base's ancestry.
    let ahead = repo
        .rev_walk([target_id])
        .with_hidden([base_id])
        .all()
        .ok()?
        .count() as u32;
    // behind: commits reachable from base, hiding target's ancestry.
    let behind = repo
        .rev_walk([base_id])
        .with_hidden([target_id])
        .all()
        .ok()?
        .count() as u32;
    Some(AheadBehind { ahead, behind })
}

/// git CLI ahead/behind: `git rev-list --count --left-right base...target` prints
/// `behind\tahead` (left = base-only, right = target-only). `None` on any failure.
fn cli_ahead_behind(repo_path: &str, base: &str, target: &str) -> Option<AheadBehind> {
    let out = crate::proc_util::command("git")
        .args([
            "-C",
            repo_path,
            "rev-list",
            "--count",
            "--left-right",
            &format!("{base}...{target}"),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let behind: u32 = parts.next()?.parse().ok()?;
    let ahead: u32 = parts.next()?.parse().ok()?;
    Some(AheadBehind { ahead, behind })
}

#[cfg(test)]
mod tests;
