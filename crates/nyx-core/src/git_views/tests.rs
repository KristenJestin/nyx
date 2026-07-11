//! Tests for the v2.4 derived Git views (task `01KX0F6FXE6AJ9BR4BYHRX6B1R`).
//!
//! done_criterion: "les vues dérivées par repo (dirty, branche courante + base/cible,
//! ahead/behind, résumé) sont exposées à la demande dans un format stable consommable
//! UI + MCP." Each test names the derived facet it covers; all skip gracefully when git
//! is unavailable.

use super::*;
use uuid::Uuid;

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-gitviews-{tag}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git_available() -> bool {
    crate::proc_util::command("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_in(dir: &std::path::Path, args: &[&str]) {
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

fn init_repo(dir: &std::path::Path, branch: &str) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git_in(dir, &["init", "-q"]);
    git_in(
        dir,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
    );
    std::fs::write(dir.join("README.md"), b"line1\nline2\nline3\n").expect("seed");
    git_in(dir, &["add", "README.md"]);
    git_in(dir, &["commit", "-q", "-m", "init"]);
}

/// DIRTY partition: a repo with a STAGED add, an UNSTAGED modification, and an UNTRACKED
/// file yields the three partitions correctly, and `clean=false`. The summary counts
/// match. Covers the "dirty (modifiés/staged/untracked)" done_criterion clause.
#[test]
fn dirty_partition_splits_staged_modified_untracked() {
    if !git_available() {
        eprintln!("skipping dirty_partition: git unavailable");
        return;
    }
    let repo = scratch_dir("dirty");
    init_repo(&repo, "main");
    // Staged add.
    std::fs::write(repo.join("staged.txt"), b"staged\n").unwrap();
    git_in(&repo, &["add", "staged.txt"]);
    // Unstaged modification of the tracked README.
    std::fs::write(repo.join("README.md"), b"line1\nMOD\nline3\n").unwrap();
    // Untracked file.
    std::fs::write(repo.join("untracked.txt"), b"untracked\n").unwrap();

    let view = repo_view(repo.to_str().unwrap(), None);
    assert!(view.is_git);
    assert!(!view.dirty.clean, "the repo is dirty");
    assert_eq!(view.dirty.staged, vec!["staged.txt"], "staged partition");
    assert_eq!(view.dirty.modified, vec!["README.md"], "modified partition");
    assert_eq!(
        view.dirty.untracked,
        vec!["untracked.txt"],
        "untracked partition"
    );

    // Summary mirrors the partition counts.
    assert_eq!(view.summary.staged, 1);
    assert_eq!(view.summary.modified, 1);
    assert_eq!(view.summary.untracked, 1);
    assert_eq!(view.summary.changed_files, 3);

    let _ = std::fs::remove_dir_all(&repo);
}

/// CLEAN tree: a freshly-committed repo reports `clean=true`, empty partitions, and the
/// current branch. Covers the "branche courante" clause.
#[test]
fn clean_repo_reports_current_branch_and_clean() {
    if !git_available() {
        eprintln!("skipping clean_repo: git unavailable");
        return;
    }
    let repo = scratch_dir("clean");
    init_repo(&repo, "trunk");

    let view = repo_view(repo.to_str().unwrap(), None);
    assert!(view.dirty.clean);
    assert!(view.dirty.staged.is_empty() && view.dirty.modified.is_empty());
    assert_eq!(view.branch.current.as_deref(), Some("trunk"));
    assert!(!view.branch.detached, "on a branch, not detached");
    assert_eq!(view.summary.changed_files, 0);

    let _ = std::fs::remove_dir_all(&repo);
}

/// BRANCH + BASE/TARGET + AHEAD/BEHIND: a feature branch two commits ahead of main and
/// one behind reports the right counts, and the branch view carries base=main,
/// target=feature. Covers the "base/cible" + "ahead/behind" clauses.
#[test]
fn branch_base_target_and_ahead_behind_counts() {
    if !git_available() {
        eprintln!("skipping ahead_behind: git unavailable");
        return;
    }
    let repo = scratch_dir("ab");
    init_repo(&repo, "main");
    // main gets one extra commit (feature will be 1 behind after branching earlier).
    git_in(&repo, &["checkout", "-q", "-b", "feature"]);
    // feature: two commits ahead.
    std::fs::write(repo.join("f1.txt"), b"f1\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "feature 1"]);
    std::fs::write(repo.join("f2.txt"), b"f2\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "feature 2"]);
    // main: one commit that feature does not have (feature is 1 behind).
    git_in(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("m1.txt"), b"m1\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "main 1"]);
    git_in(&repo, &["checkout", "-q", "feature"]);

    let view = repo_view(repo.to_str().unwrap(), Some("main"));
    assert_eq!(view.branch.current.as_deref(), Some("feature"));
    assert_eq!(view.branch.base.as_deref(), Some("main"));
    assert_eq!(
        view.branch.target.as_deref(),
        Some("feature"),
        "target defaults to current"
    );
    let ab = view
        .ahead_behind
        .expect("ahead/behind computed against base");
    assert_eq!(ab.ahead, 2, "feature has 2 commits main lacks");
    assert_eq!(ab.behind, 1, "main has 1 commit feature lacks");
    // Summary mirrors ahead/behind.
    assert_eq!(view.summary.ahead, 2);
    assert_eq!(view.summary.behind, 1);

    let _ = std::fs::remove_dir_all(&repo);
}

/// The gix PRIMARY and git CLI FALLBACK ahead/behind counts AGREE — the coherence
/// requirement, applied to the derived views' ahead/behind facet.
#[test]
fn ahead_behind_gix_and_cli_coherent() {
    if !git_available() {
        eprintln!("skipping ahead_behind coherence: git unavailable");
        return;
    }
    let repo = scratch_dir("abcoh");
    init_repo(&repo, "main");
    git_in(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("f1.txt"), b"f1\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "feature 1"]);
    let path = repo.to_str().unwrap();

    let gix = super::gix_ahead_behind(path, "main", "feature").expect("gix ahead/behind");
    let cli = super::cli_ahead_behind(path, "main", "feature").expect("cli ahead/behind");
    assert_eq!(gix, cli, "gix and git CLI ahead/behind must agree");
    assert_eq!(gix.ahead, 1);
    assert_eq!(gix.behind, 0);

    let _ = std::fs::remove_dir_all(&repo);
}

/// DETACHED HEAD: checking out a raw SHA reports `current=None, detached=true` — the
/// branch view degrades correctly rather than reporting a bogus branch.
#[test]
fn detached_head_reports_no_branch() {
    if !git_available() {
        eprintln!("skipping detached_head: git unavailable");
        return;
    }
    let repo = scratch_dir("detached");
    init_repo(&repo, "main");
    std::fs::write(repo.join("second.txt"), b"second\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "second"]);
    // Detach onto the first commit.
    let first = {
        let out = crate::proc_util::command("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "rev-list",
                "--max-parents=0",
                "HEAD",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git_in(&repo, &["checkout", "-q", &first]);

    let view = repo_view(repo.to_str().unwrap(), None);
    assert!(view.branch.current.is_none(), "detached: no current branch");
    assert!(view.branch.detached, "detached flag set");

    let _ = std::fs::remove_dir_all(&repo);
}

/// A NON-GIT path degrades to `is_git=false`, a clean/empty view, and no ahead/behind —
/// never an error. Proves the views are git-agnostic (PDR-0013).
#[test]
fn non_git_path_degrades_to_empty_view() {
    let dir = scratch_dir("nongit");
    let view = repo_view(dir.to_str().unwrap(), Some("main"));
    assert!(!view.is_git);
    assert!(view.dirty.clean);
    assert!(view.branch.current.is_none());
    assert!(view.ahead_behind.is_none());
    assert_eq!(view.summary.changed_files, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// STABLE FORMAT: the view serializes to a stable JSON shape (the contract the renderer
/// and MCP consume). Proves the "format stable consommable UI + MCP" clause — the field
/// names are exactly what both surfaces key on.
#[test]
fn view_serializes_to_stable_json_shape() {
    if !git_available() {
        eprintln!("skipping serialize: git unavailable");
        return;
    }
    let repo = scratch_dir("serialize");
    init_repo(&repo, "main");
    std::fs::write(repo.join("x.txt"), b"x\n").unwrap();

    let view = repo_view(repo.to_str().unwrap(), None);
    let json = serde_json::to_value(&view).unwrap();
    // Top-level keys the renderer/MCP rely on.
    for key in [
        "repo_path",
        "is_git",
        "dirty",
        "branch",
        "ahead_behind",
        "summary",
    ] {
        assert!(json.get(key).is_some(), "missing stable key '{key}'");
    }
    // Dirty sub-shape.
    for key in ["clean", "staged", "modified", "untracked"] {
        assert!(json["dirty"].get(key).is_some(), "missing dirty.{key}");
    }
    // Branch sub-shape.
    for key in ["current", "detached", "base", "target"] {
        assert!(json["branch"].get(key).is_some(), "missing branch.{key}");
    }
    // Summary sub-shape.
    for key in [
        "changed_files",
        "staged",
        "modified",
        "untracked",
        "ahead",
        "behind",
    ] {
        assert!(json["summary"].get(key).is_some(), "missing summary.{key}");
    }
    assert_eq!(json["is_git"], serde_json::json!(true));

    let _ = std::fs::remove_dir_all(&repo);
}

/// TARGET OVERRIDE: a review can pin the target endpoint (a SHA/branch) distinct from the
/// current branch, and ahead/behind is computed for that target vs base.
#[test]
fn target_override_drives_ahead_behind() {
    if !git_available() {
        eprintln!("skipping target_override: git unavailable");
        return;
    }
    let repo = scratch_dir("override");
    init_repo(&repo, "main");
    git_in(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("f.txt"), b"f\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "feature work"]);
    // Back on main; ask for a view whose TARGET is `feature` explicitly.
    git_in(&repo, &["checkout", "-q", "main"]);

    let view = repo_view_with_target(repo.to_str().unwrap(), Some("main"), Some("feature"));
    assert_eq!(
        view.branch.current.as_deref(),
        Some("main"),
        "current is main"
    );
    assert_eq!(
        view.branch.target.as_deref(),
        Some("feature"),
        "target overridden"
    );
    let ab = view.ahead_behind.unwrap();
    assert_eq!(ab.ahead, 1, "feature is 1 ahead of main");
    assert_eq!(ab.behind, 0);

    let _ = std::fs::remove_dir_all(&repo);
}
