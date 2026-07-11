//! Tests for the v2.4 diff engine (task `01KX0F6FD836YASVYT17YV3FTW`).
//!
//! The done_criterion is: "gix produit le diff working tree/staged/unstaged/untracked/
//! commit/range/branches multi-repo, fallback git CLI cohérent ; modes living (branche
//! vs base) et pinned (SHA) ; exposé au renderer sans mélanger les identités Git."
//!
//! Each test names the surface it covers, and the coherence tests assert the gix PRIMARY
//! output equals the git CLI FALLBACK output on the SAME inputs (the PRD's central
//! requirement). Every test skips gracefully when `git` is unavailable.

use super::*;
use uuid::Uuid;

/// A unique scratch dir under the OS temp dir (uniqueness from a v7 UUID so parallel
/// test threads never collide — mirrors the repository.rs helper).
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-gitdiff-{tag}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Is `git` on PATH? The engine is git-agnostic; tests that need a REAL repo skip when
/// git is absent (a CI image without git).
fn git_available() -> bool {
    crate::proc_util::command("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a git subcommand in `dir`, asserting success, with a deterministic identity and
/// signing forced off (a dev host may set commit.gpgsign globally).
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let status = crate::proc_util::command("git")
        .args([
            "-c",
            "user.email=test@nyx",
            "-c",
            "user.name=nyx-test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// `git init` a repo at `dir` on `branch` with one committed file.
fn init_repo(dir: &std::path::Path, branch: &str) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git_in(dir, &["init", "-q"]);
    git_in(
        dir,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
    );
    std::fs::write(dir.join("README.md"), b"line1\nline2\nline3\n").expect("seed file");
    git_in(dir, &["add", "README.md"]);
    git_in(dir, &["commit", "-q", "-m", "init"]);
}

/// The current full HEAD SHA of `dir`.
fn head_sha(dir: &std::path::Path) -> String {
    let out = crate::proc_util::command("git")
        .args(["-C", dir.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Compare two `RepoDiff`s for the COHERENCE assertion: same file set, same change
/// kinds, and same hunk geometry+content — regardless of which backend produced them.
/// The `backend`, `git_dir`, and `blob` ids are allowed to differ (only gix populates
/// blob ids; git_dir formatting differs), so we compare the render-facing content.
fn assert_coherent(gix: &RepoDiff, cli: &RepoDiff) {
    let gix_paths: Vec<&str> = gix.files.iter().map(|f| f.path()).collect();
    let cli_paths: Vec<&str> = cli.files.iter().map(|f| f.path()).collect();
    assert_eq!(
        gix_paths, cli_paths,
        "gix and CLI must agree on the file set"
    );
    for (g, c) in gix.files.iter().zip(cli.files.iter()) {
        assert_eq!(g.change, c.change, "change kind for {}", g.path());
        assert_eq!(g.binary, c.binary, "binary flag for {}", g.path());
        let g_hunks = strip_line_content(&g.hunks);
        let c_hunks = strip_line_content(&c.hunks);
        assert_eq!(
            g_hunks,
            c_hunks,
            "hunk geometry+content for {} (gix vs cli)",
            g.path()
        );
    }
}

/// The comparable shape of one hunk: its geometry `(old_start, old_lines, new_start,
/// new_lines)` plus its tagged lines. Both backends must produce this identically.
type HunkShape = (u32, u32, u32, u32, Vec<(LineKind, String)>);

/// The hunk geometry + line roles + line text, for coherence comparison (the shape both
/// backends must produce identically).
fn strip_line_content(hunks: &[Hunk]) -> Vec<HunkShape> {
    hunks
        .iter()
        .map(|h| {
            (
                h.old_start,
                h.old_lines,
                h.new_start,
                h.new_lines,
                h.lines
                    .iter()
                    .map(|l| (l.kind, l.content.clone()))
                    .collect(),
            )
        })
        .collect()
}

/// Force a diff through the gix primary backend (bypassing the fallback) — used by the
/// coherence tests to compare the two backends directly.
fn gix_only(repo: &str, target: &DiffTarget) -> RepoDiff {
    super::gix_backend::diff(repo, target, target.default_mode()).expect("gix diff")
}

/// Force a diff through the git CLI backend directly.
fn cli_only(repo: &str, target: &DiffTarget) -> RepoDiff {
    super::cli_backend::diff(repo, target, target.default_mode()).expect("cli diff")
}

// ---------------------------------------------------------------------------
// done_criterion: working tree / staged / unstaged / untracked / commit / range /
// branches — each produced by gix, with the CLI fallback COHERENT.
// ---------------------------------------------------------------------------

/// COMMIT surface: a single commit vs its parent, gix primary AND CLI coherent. Also
/// asserts the resolved SHAs are recorded (base = parent, target = the commit) so a
/// PINNED review can re-derive it.
#[test]
fn commit_diff_gix_and_cli_coherent_with_recorded_shas() {
    if !git_available() {
        eprintln!("skipping commit_diff: git unavailable");
        return;
    }
    let repo = scratch_dir("commit");
    init_repo(&repo, "main");
    let parent = head_sha(&repo);
    std::fs::write(repo.join("README.md"), b"line1\nCHANGED\nline3\nline4\n").unwrap();
    git_in(&repo, &["commit", "-aqm", "change"]);
    let commit = head_sha(&repo);
    let path = repo.to_str().unwrap();

    let target = DiffTarget::Commit {
        rev: commit.clone(),
    };
    let gix = gix_only(path, &target);
    let cli = cli_only(path, &target);

    assert_eq!(gix.backend, DiffBackend::Gix);
    assert_eq!(
        gix.mode,
        DiffMode::Pinned,
        "a commit diff is pinned by construction"
    );
    assert_eq!(gix.target_sha.as_deref(), Some(commit.as_str()));
    assert_eq!(gix.base_sha.as_deref(), Some(parent.as_str()));
    assert_eq!(gix.files.len(), 1, "one file changed");
    assert_eq!(gix.files[0].change, ChangeKind::Modified);
    assert!(!gix.files[0].hunks.is_empty(), "the modification has hunks");
    assert_coherent(&gix, &cli);

    let _ = std::fs::remove_dir_all(&repo);
}

/// RANGE + BRANCHES surface: `base..target` between two branches. Both backends agree,
/// SHAs recorded, and the LIVING mode is the default for a branch-vs-base review.
#[test]
fn range_and_branches_diff_gix_and_cli_coherent() {
    if !git_available() {
        eprintln!("skipping range/branches: git unavailable");
        return;
    }
    let repo = scratch_dir("range");
    init_repo(&repo, "main");
    let base = head_sha(&repo);
    git_in(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("README.md"), b"line1\nline2\nline3\nADDED\n").unwrap();
    std::fs::write(repo.join("new.txt"), b"brand new\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "feature work"]);
    let path = repo.to_str().unwrap();

    // Range form.
    let range = DiffTarget::Range {
        base: "main".into(),
        target: "feature".into(),
    };
    let gix_r = gix_only(path, &range);
    let cli_r = cli_only(path, &range);
    assert_eq!(
        gix_r.mode,
        DiffMode::Pinned,
        "a range is pinned by construction"
    );
    assert_eq!(gix_r.base_sha.as_deref(), Some(base.as_str()));
    assert_coherent(&gix_r, &cli_r);

    // Branches form: living (branch vs base), same two backends coherent.
    let branches = DiffTarget::Branches {
        base: "main".into(),
        target: "feature".into(),
    };
    let gix_b = gix_only(path, &branches);
    let cli_b = cli_only(path, &branches);
    assert_eq!(
        gix_b.mode,
        DiffMode::Living,
        "branch-vs-base is living by default"
    );
    let paths: Vec<&str> = gix_b.files.iter().map(|f| f.path()).collect();
    assert!(paths.contains(&"README.md") && paths.contains(&"new.txt"));
    assert!(
        gix_b
            .files
            .iter()
            .find(|f| f.path() == "new.txt")
            .unwrap()
            .change
            == ChangeKind::Added
    );
    assert_coherent(&gix_b, &cli_b);

    let _ = std::fs::remove_dir_all(&repo);
}

/// BRANCHES surface with files in a SUBDIRECTORY: the gix tree-to-tree diff must report only the
/// changed FILES, never the changed DIRECTORY entry itself (Finding 55). Before the fix, gix emitted
/// the `src` tree as a `Modified` file entry ALONGSIDE its children — an extra, spurious file whose
/// path (`src`) is both a file and the prefix of `src/a.rs`, which made the renderer's @pierre/trees
/// changed-files tree throw at construction and blank behind its error boundary. The git CLI backend
/// never lists directories, so this also asserts the two backends agree on a nested change.
#[test]
fn branches_diff_in_subdirectory_omits_the_directory_entry() {
    if !git_available() {
        eprintln!("skipping subdir branches: git unavailable");
        return;
    }
    let repo = scratch_dir("subdir-branches");
    init_repo(&repo, "main");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.rs"), b"fn a() {}\n").unwrap();
    std::fs::write(repo.join("src/b.rs"), b"fn b() {}\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "base subdir"]);
    git_in(&repo, &["branch", "-M", "base"]);

    git_in(&repo, &["checkout", "-q", "-b", "feature"]);
    // Change BOTH files in `src/` so the `src` tree object changes (the trigger for the phantom
    // directory entry) and add a nested file so a deeper tree changes too.
    std::fs::write(repo.join("src/a.rs"), b"fn a() { let x = 1; }\n").unwrap();
    std::fs::write(repo.join("src/b.rs"), b"fn b() { let y = 2; }\n").unwrap();
    std::fs::create_dir_all(repo.join("src/inner")).unwrap();
    std::fs::write(repo.join("src/inner/c.rs"), b"fn c() {}\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "feature subdir"]);
    let path = repo.to_str().unwrap();

    let branches = DiffTarget::Branches {
        base: "base".into(),
        target: "feature".into(),
    };
    let gix = gix_only(path, &branches);
    let cli = cli_only(path, &branches);

    let paths: Vec<&str> = gix.files.iter().map(|f| f.path()).collect();
    // Exactly the three FILE changes — never the `src` or `src/inner` directory entries.
    assert!(
        !paths.contains(&"src") && !paths.contains(&"src/inner"),
        "the diff must not contain directory entries, got {paths:?}"
    );
    assert!(paths.contains(&"src/a.rs") && paths.contains(&"src/b.rs") && paths.contains(&"src/inner/c.rs"));
    assert_eq!(gix.files.len(), 3, "only the 3 changed files, got {paths:?}");
    // Every entry is a real blob change with hunks (no binary/empty directory pseudo-file).
    for f in &gix.files {
        assert!(!f.hunks.is_empty(), "file {} has hunks", f.path());
    }
    // The git CLI backend (which never lists directories) agrees.
    assert_coherent(&gix, &cli);

    let _ = std::fs::remove_dir_all(&repo);
}

/// STAGED surface: index vs HEAD (`git diff --cached`). gix primary + CLI coherent.
#[test]
fn staged_diff_gix_and_cli_coherent() {
    if !git_available() {
        eprintln!("skipping staged: git unavailable");
        return;
    }
    let repo = scratch_dir("staged");
    init_repo(&repo, "main");
    std::fs::write(repo.join("README.md"), b"line1\nSTAGED\nline3\n").unwrap();
    std::fs::write(repo.join("added.txt"), b"staged add\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    let path = repo.to_str().unwrap();

    let target = DiffTarget::Staged;
    let gix = gix_only(path, &target);
    let cli = cli_only(path, &target);
    let paths: Vec<&str> = gix.files.iter().map(|f| f.path()).collect();
    assert!(paths.contains(&"README.md") && paths.contains(&"added.txt"));
    assert!(!gix
        .files
        .iter()
        .find(|f| f.path() == "README.md")
        .unwrap()
        .hunks
        .is_empty());
    assert_coherent(&gix, &cli);

    let _ = std::fs::remove_dir_all(&repo);
}

/// UNSTAGED surface: working tree vs index (`git diff`). gix primary + CLI coherent.
#[test]
fn unstaged_diff_gix_and_cli_coherent() {
    if !git_available() {
        eprintln!("skipping unstaged: git unavailable");
        return;
    }
    let repo = scratch_dir("unstaged");
    init_repo(&repo, "main");
    std::fs::write(repo.join("README.md"), b"line1\nUNSTAGED\nline3\n").unwrap();
    let path = repo.to_str().unwrap();

    let target = DiffTarget::Unstaged;
    let gix = gix_only(path, &target);
    let cli = cli_only(path, &target);
    assert_eq!(gix.files.len(), 1);
    assert_eq!(gix.files[0].path(), "README.md");
    assert_eq!(gix.files[0].change, ChangeKind::Modified);
    assert_coherent(&gix, &cli);

    let _ = std::fs::remove_dir_all(&repo);
}

/// UNTRACKED surface: files present on disk, unknown to Git — each an all-added file.
/// gix primary + CLI coherent, and ONLY untracked files appear (no tracked changes).
#[test]
fn untracked_diff_gix_and_cli_coherent() {
    if !git_available() {
        eprintln!("skipping untracked: git unavailable");
        return;
    }
    let repo = scratch_dir("untracked");
    init_repo(&repo, "main");
    // A tracked modification (must NOT appear in the untracked surface) + two untracked.
    std::fs::write(repo.join("README.md"), b"line1\nMOD\nline3\n").unwrap();
    std::fs::write(repo.join("u1.txt"), b"untracked one\n").unwrap();
    std::fs::write(repo.join("u2.txt"), b"untracked two\n").unwrap();
    let path = repo.to_str().unwrap();

    let target = DiffTarget::Untracked;
    let gix = gix_only(path, &target);
    let cli = cli_only(path, &target);
    let paths: Vec<&str> = gix.files.iter().map(|f| f.path()).collect();
    assert_eq!(
        paths,
        vec!["u1.txt", "u2.txt"],
        "only untracked files, sorted"
    );
    assert!(gix.files.iter().all(|f| f.change == ChangeKind::Added));
    assert_coherent(&gix, &cli);

    let _ = std::fs::remove_dir_all(&repo);
}

/// WORKING-TREE surface: staged + unstaged + untracked together (`git diff HEAD` +
/// untracked). gix primary + CLI coherent, and all three kinds of change surface.
#[test]
fn working_tree_diff_gix_and_cli_coherent() {
    if !git_available() {
        eprintln!("skipping working_tree: git unavailable");
        return;
    }
    let repo = scratch_dir("wt");
    init_repo(&repo, "main");
    // A committed file we delete, a modification, and an untracked add.
    std::fs::write(repo.join("todelete.txt"), b"bye\n").unwrap();
    git_in(&repo, &["add", "todelete.txt"]);
    git_in(&repo, &["commit", "-qm", "add todelete"]);
    std::fs::remove_file(repo.join("todelete.txt")).unwrap();
    std::fs::write(repo.join("README.md"), b"line1\nWT\nline3\n").unwrap();
    std::fs::write(repo.join("fresh.txt"), b"fresh add\n").unwrap();
    let path = repo.to_str().unwrap();

    let target = DiffTarget::WorkingTree;
    let gix = gix_only(path, &target);
    let cli = cli_only(path, &target);
    let paths: Vec<&str> = gix.files.iter().map(|f| f.path()).collect();
    assert!(paths.contains(&"README.md"), "modification present");
    assert!(paths.contains(&"todelete.txt"), "deletion present");
    assert!(paths.contains(&"fresh.txt"), "untracked add present");
    assert_eq!(
        gix.files
            .iter()
            .find(|f| f.path() == "todelete.txt")
            .unwrap()
            .change,
        ChangeKind::Deleted
    );
    assert_eq!(
        gix.files
            .iter()
            .find(|f| f.path() == "fresh.txt")
            .unwrap()
            .change,
        ChangeKind::Added
    );
    assert_coherent(&gix, &cli);

    let _ = std::fs::remove_dir_all(&repo);
}

/// LIVING vs PINNED modes (PDR-0015): the SAME branch-vs-base target rendered living
/// (follows the tip) and pinned (frozen on the resolved SHA) yields identical CONTENT
/// but a different recorded `mode`. Proves the mode override plumbs through.
#[test]
fn living_and_pinned_modes_record_correctly() {
    if !git_available() {
        eprintln!("skipping modes: git unavailable");
        return;
    }
    let repo = scratch_dir("modes");
    init_repo(&repo, "main");
    git_in(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("README.md"), b"line1\nMODE\nline3\n").unwrap();
    git_in(&repo, &["commit", "-aqm", "feature"]);
    let feature_sha = head_sha(&repo);
    let path = repo.to_str().unwrap();

    // LIVING: branch vs base, following the tips.
    let living = diff_repo_with_mode(
        path,
        &DiffTarget::Branches {
            base: "main".into(),
            target: "feature".into(),
        },
        DiffMode::Living,
    )
    .unwrap();
    assert_eq!(living.mode, DiffMode::Living);
    assert_eq!(living.target_sha.as_deref(), Some(feature_sha.as_str()));

    // PINNED: the same comparison but frozen on the resolved SHA.
    let pinned = diff_repo_with_mode(
        path,
        &DiffTarget::Branches {
            base: "main".into(),
            target: feature_sha.clone(),
        },
        DiffMode::Pinned,
    )
    .unwrap();
    assert_eq!(pinned.mode, DiffMode::Pinned);
    assert_eq!(pinned.target_sha.as_deref(), Some(feature_sha.as_str()));

    // Same rendered content regardless of the recorded mode.
    assert_eq!(
        strip_line_content(&living.files[0].hunks),
        strip_line_content(&pinned.files[0].hunks)
    );

    let _ = std::fs::remove_dir_all(&repo);
}

/// MULTI-REPO without mixing Git identities: two independent repos under one root,
/// diffed separately, carry DISTINCT `git_dir`s and only their OWN files/blobs. The
/// engine never cross-contaminates one repo's identity with the other's.
#[test]
fn multi_repo_diffs_do_not_mix_git_identities() {
    if !git_available() {
        eprintln!("skipping multi_repo: git unavailable");
        return;
    }
    let root = scratch_dir("multi");
    let api = root.join("api");
    let web = root.join("web");
    init_repo(&api, "main");
    init_repo(&web, "main");
    // Distinct changes per repo, with a same-named file to catch any cross-mixing.
    std::fs::write(api.join("README.md"), b"line1\nAPI\nline3\n").unwrap();
    std::fs::write(api.join("api-only.txt"), b"api\n").unwrap();
    std::fs::write(web.join("README.md"), b"line1\nWEB\nline3\n").unwrap();
    std::fs::write(web.join("web-only.txt"), b"web\n").unwrap();

    let api_diff = diff_repo(api.to_str().unwrap(), &DiffTarget::WorkingTree).unwrap();
    let web_diff = diff_repo(web.to_str().unwrap(), &DiffTarget::WorkingTree).unwrap();

    // Distinct identities.
    assert_ne!(
        api_diff.git_dir, web_diff.git_dir,
        "each repo has its OWN git_dir"
    );
    assert_eq!(api_diff.repo_path, api.to_str().unwrap());
    assert_eq!(web_diff.repo_path, web.to_str().unwrap());

    // Each diff carries ONLY its own repo's files (the api-only / web-only markers).
    let api_paths: Vec<&str> = api_diff.files.iter().map(|f| f.path()).collect();
    let web_paths: Vec<&str> = web_diff.files.iter().map(|f| f.path()).collect();
    assert!(api_paths.contains(&"api-only.txt") && !api_paths.contains(&"web-only.txt"));
    assert!(web_paths.contains(&"web-only.txt") && !web_paths.contains(&"api-only.txt"));

    // The same-named README changed differently in each — verify the content is the
    // repo's OWN (no blob mixing).
    let api_readme = api_diff
        .files
        .iter()
        .find(|f| f.path() == "README.md")
        .unwrap();
    let web_readme = web_diff
        .files
        .iter()
        .find(|f| f.path() == "README.md")
        .unwrap();
    let api_added: Vec<&str> = api_readme.hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    let web_added: Vec<&str> = web_readme.hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(api_added, vec!["API"], "api README shows API's own change");
    assert_eq!(web_added, vec!["WEB"], "web README shows WEB's own change");

    let _ = std::fs::remove_dir_all(&root);
}

/// The public `diff_repo` (gix primary with fallback) errors — not panics — on a
/// non-git path, and on an unresolvable rev-spec.
#[test]
fn non_git_and_bad_revspec_error_gracefully() {
    let missing = scratch_dir("nongit");
    let err = diff_repo(missing.to_str().unwrap(), &DiffTarget::WorkingTree).unwrap_err();
    assert!(matches!(err, DiffError::NotAGitRepo(_)), "got {err:?}");

    if git_available() {
        let repo = scratch_dir("badrev");
        init_repo(&repo, "main");
        let err = diff_repo(
            repo.to_str().unwrap(),
            &DiffTarget::Commit {
                rev: "does-not-exist".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, DiffError::BadRevspec(_)), "got {err:?}");
        let _ = std::fs::remove_dir_all(&repo);
    }
    let _ = std::fs::remove_dir_all(&missing);
}

/// The public `diff_repo` falls back to the CLI transparently and still yields a
/// coherent result: we compare the public entry (gix primary) against the CLI-only
/// output for the working tree, proving the fallback path produces the same shape.
#[test]
fn public_entry_is_coherent_with_cli_fallback() {
    if !git_available() {
        eprintln!("skipping public_entry coherence: git unavailable");
        return;
    }
    let repo = scratch_dir("public");
    init_repo(&repo, "main");
    std::fs::write(repo.join("README.md"), b"line1\nPUB\nline3\n").unwrap();
    std::fs::write(repo.join("extra.txt"), b"extra\n").unwrap();
    let path = repo.to_str().unwrap();

    let public = diff_repo(path, &DiffTarget::WorkingTree).unwrap();
    assert_eq!(
        public.backend,
        DiffBackend::Gix,
        "public entry uses gix primary"
    );
    let cli = cli_only(path, &DiffTarget::WorkingTree);
    assert_coherent(&public, &cli);

    let _ = std::fs::remove_dir_all(&repo);
}
