//! Repository as a LOGICAL identity (PDR-0013, v2.1 task #5).
//!
//! A repository is a logical Nyx identity within a project (e.g. `api`) — the SAME
//! object across the project's primary workspace and every dev worktree, so
//! review/history follow it. This module owns the three behaviours the canonical
//! model demands on top of the `repositories` persistence socle in [`crate::db`]:
//!
//! 1. **Auto-detection** ([`detect_under_root`] / [`detect_and_register`]). Scan a
//!    workspace's `root_path` for Git work trees and register a logical identity per
//!    detected repo. A MONO-REPO (a repo at the root itself) yields a single
//!    identity with `relative_path = "."`; a MULTI-REPO layout (repos in immediate
//!    subfolders) yields one identity per subfolder. Registration is idempotent and
//!    NEVER clobbers a manual override (`crate::db::upsert_detected_repository`).
//!
//! 2. **Derived membership** ([`workspace_members`]). A workspace does NOT store a
//!    checkout list — there is no `workspace_repositories` table (PDR-0013).
//!    A workspace's repos are DERIVED: the project's NON-OPTIONAL repositories,
//!    each resolved at `workspace.root_path` joined with the repo's
//!    `relative_path`. `optional` (the in-DB "nyxignore") excludes a repo.
//!
//! 3. **Git state re-read** ([`read_git_state`]). Branch, dirty flag, and remotes
//!    are read from Git LIVE, on demand, and are NEVER written to the DB — the DB is
//!    not authoritative for Git state (PDR-0013). The returned [`GitState`] is a
//!    transient view, not a persisted row.
//!
//! Everything git-facing shells out through [`crate::proc_util::command`] (the
//! hardened, console-flash-free spawn), stays fully git-agnostic (a non-git path
//! simply yields no detection / an empty [`GitState`]), and never errors on a
//! missing/So-so repo — detection and state reads are best-effort by contract.

use std::path::Path;

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::db::{self, Repository, Workspace};

/// The conventional `relative_path` of a mono-repo: the repository lives at the
/// workspace root itself. Kept as a shared const so detection and membership agree.
pub const MONO_REPO_RELATIVE_PATH: &str = ".";

/// A repository detected under a workspace root, BEFORE it is registered: its
/// conventional `relative_path` within the project and a derived logical `name`.
/// `relative_path` is `"."` for a mono-repo, or the subfolder name for a repo found
/// one level down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedRepo {
    /// Derived logical name (the folder's basename, or the project folder name for a
    /// mono-repo). Disambiguated for uniqueness only at registration time.
    pub name: String,
    /// Conventional path within the project (`"."` mono-repo, else the subfolder).
    pub relative_path: String,
}

/// A repository resolved for a specific workspace: its logical identity plus the
/// ABSOLUTE on-disk path where it lives in THAT workspace (`root_path` joined with
/// `relative_path`). This is the derived-membership view — the path is computed, not
/// stored per workspace (PDR-0013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRepo {
    /// The logical identity (from the `repositories` table).
    pub repository: Repository,
    /// Absolute path of this repo IN this workspace: `root_path` + `relative_path`.
    pub absolute_path: String,
}

/// The LIVE Git state of a repo, re-read from Git on demand. NEVER persisted — the
/// DB is not authoritative for any of these fields (PDR-0013). All fields degrade
/// gracefully: a non-git path yields `branch = None`, `dirty = false`, `remotes = []`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct GitState {
    /// Current HEAD branch, or `None` when detached / unborn / not a git work tree.
    pub branch: Option<String>,
    /// `true` when the work tree has uncommitted changes (`git status --porcelain`
    /// prints at least one line). `false` for a clean tree OR a non-git path.
    pub dirty: bool,
    /// Configured remotes (`git remote`), sorted and de-duplicated. Empty for a repo
    /// with no remotes, or a non-git path.
    pub remotes: Vec<String>,
}

/// Whether `dir` is the top of a Git work tree: it directly contains a `.git` entry
/// (a directory in a normal clone, or a file in a linked worktree). Pure filesystem
/// check, no subprocess — cheap enough to run per candidate folder during a scan.
fn is_git_worktree(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Detect the Git repositories under a workspace `root_path`, returning one
/// [`DetectedRepo`] per repo found, ordered by `relative_path` for determinism.
///
/// Layouts handled (PDR-0013):
///   * **mono-repo** — `root_path` is itself a Git work tree → a single detected repo
///     at `relative_path = "."`, named after the root folder.
///   * **multi-repo** — `root_path` is NOT a work tree but immediate subfolders are →
///     one detected repo per such subfolder, `relative_path = <subfolder name>`.
///
/// Only the root and its IMMEDIATE children are scanned (a shallow, one-level walk):
/// nested repos deeper down are out of scope for auto-detection (a manual override
/// can still declare them). A non-existent or non-git `root_path` yields an empty
/// vec — detection is best-effort and git-agnostic.
pub fn detect_under_root(root_path: &str) -> Vec<DetectedRepo> {
    let root = Path::new(root_path);
    let mut found = Vec::new();

    // Mono-repo: the root folder itself is a Git work tree.
    if is_git_worktree(root) {
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo")
            .to_string();
        found.push(DetectedRepo {
            name,
            relative_path: MONO_REPO_RELATIVE_PATH.to_string(),
        });
        // A mono-repo IS the project's tree; do not also descend into its packages
        // (those are subfolders of ONE repo, not separate repositories).
        return found;
    }

    // Multi-repo: scan immediate subfolders for Git work trees.
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if is_git_worktree(&path) {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    found.push(DetectedRepo {
                        name: name.to_string(),
                        relative_path: name.to_string(),
                    });
                }
            }
        }
    }

    found.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    found
}

/// Auto-detect the repos under `workspace.root_path` and REGISTER each as a logical
/// identity in the workspace's project, idempotently. Delegates each detected repo to
/// [`db::upsert_detected_repository`], which keys on `(project_id, relative_path)` so a
/// re-scan converges (no duplicates) and manual `name`/`optional` overrides are
/// preserved. Returns the registered [`Repository`] rows in detection order.
///
/// This is the detection step that [`register_workspace`] / [`register_project`] run
/// right after they insert the workspace — the real registration flow. It never deletes
/// a previously-detected repo that vanished from disk (its identity + history are
/// permanent — the user archives it explicitly if desired). It is also called directly
/// on a RE-registration / re-scan of an existing workspace, where idempotence guarantees
/// convergence.
pub fn detect_and_register(
    conn: &mut SqliteConnection,
    workspace: &Workspace,
) -> QueryResult<Vec<Repository>> {
    let detected = detect_under_root(&workspace.root_path);
    let mut registered = Vec::with_capacity(detected.len());
    for d in detected {
        let repo =
            db::upsert_detected_repository(conn, &workspace.project_id, &d.name, &d.relative_path)?;
        registered.push(repo);
    }
    Ok(registered)
}

/// REGISTER a non-root workspace at `path` under `project_id`, then auto-detect and
/// persist the Git repositories under its `root_path`. This is the canonical
/// nyx-core entry point for "enregistrer un workspace" (done_criterion #5): it composes
/// the low-level [`db::create_workspace`] insert with [`detect_and_register`], so the
/// act of registering a workspace populates the project's `repositories`.
///
/// The two steps are kept as a COMPOSITION here rather than folded into
/// [`db::create_workspace`] on purpose: `db::create_workspace` is a pure persistence
/// write that must stay disk-agnostic (a workspace may be registered before its folder
/// exists — the MCP `create_workspace` tool `mkdir -p`s first, and detection then finds
/// an empty tree and registers nothing, which is correct). Detection is the domain
/// behaviour that reads the disk, so it lives in this `repository` layer that already
/// owns the filesystem scan.
///
/// The insert failing is propagated as-is (the caller maps it to its error vocabulary);
/// detection runs only after a successful insert. Returns the freshly-created
/// [`Workspace`] — the detected repos are queryable via [`db::list_repositories`] /
/// [`workspace_members`]. Detection is best-effort and NEVER clobbers a manual override,
/// so a re-registration converges without losing user `name`/`optional` edits.
pub fn register_workspace(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    path: &str,
) -> QueryResult<Workspace> {
    let workspace = db::create_workspace(conn, project_id, name, path)?;
    detect_and_register(conn, &workspace)?;
    Ok(workspace)
}

/// REGISTER a project plus its root workspace at `root_path`, then auto-detect and
/// persist the Git repositories under that root. The root-workspace analogue of
/// [`register_workspace`]: it composes [`db::create_project`] (which creates the
/// project and its single root workspace) with [`detect_and_register`] on that root
/// workspace, so creating a project on a multi-repo tree populates its `repositories`
/// in the same flow. Returns the new project and its root workspace.
pub fn register_project(
    conn: &mut SqliteConnection,
    name: &str,
    root_path: &str,
    root_name: Option<&str>,
) -> QueryResult<(crate::db::Project, Workspace)> {
    let (project, root) = db::create_project(conn, name, root_path, root_name)?;
    detect_and_register(conn, &root)?;
    Ok((project, root))
}

/// The DERIVED membership of `workspace`: the project's NON-OPTIONAL repositories,
/// each resolved to its absolute path IN this workspace (`root_path` + `relative_path`).
///
/// There is NO checkout table (PDR-0013): membership is computed here from the
/// project's `repositories` (already filtered to the LIVE, non-archived set by
/// [`db::list_repositories`]) minus the `optional` ones (the in-DB "nyxignore").
/// The path is COMPUTED per workspace, never stored — so the same logical repo lands
/// at the right place in the primary workspace and in every worktree.
#[cfg_attr(not(test), allow(dead_code))]
pub fn workspace_members(
    conn: &mut SqliteConnection,
    workspace: &Workspace,
) -> QueryResult<Vec<WorkspaceRepo>> {
    let repos = db::list_repositories(conn, &workspace.project_id)?;
    Ok(repos
        .into_iter()
        .filter(|r| !r.optional)
        .map(|r| {
            let absolute_path = join_relative(&workspace.root_path, &r.relative_path);
            WorkspaceRepo {
                repository: r,
                absolute_path,
            }
        })
        .collect())
}

/// Join a repo's `relative_path` onto a workspace `root_path`, normalized. A `"."`
/// (mono-repo) resolves to the `root_path` itself; any other relative path is joined
/// under it. The result is [`crate::pathnorm::normalize`]d so it matches the canonical
/// form the rest of nyx stores/compares.
fn join_relative(root_path: &str, relative_path: &str) -> String {
    if relative_path == MONO_REPO_RELATIVE_PATH || relative_path.is_empty() {
        crate::pathnorm::normalize(root_path)
    } else {
        crate::pathnorm::normalize(&format!("{root_path}/{relative_path}"))
    }
}

/// Re-read the LIVE Git state (branch / dirty / remotes) of the work tree at `path`.
///
/// Every field is read from Git via a hardened subprocess and returned as a transient
/// [`GitState`] — NOTHING here is persisted (PDR-0013: the DB is not authoritative for
/// Git state, so a read is always fresh and can never diverge from the DB because the
/// DB does not store it). A non-git path (or any git error) degrades to the default:
/// `branch = None`, `dirty = false`, `remotes = []`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn read_git_state(path: &str) -> GitState {
    GitState {
        branch: db::detect_branch(path),
        dirty: read_dirty(path),
        remotes: read_remotes(path),
    }
}

/// `true` when the work tree at `path` has uncommitted changes. Uses `git status
/// --porcelain`: any output line means the tree is dirty. A non-git path / git error
/// yields `false` (a path with no repo is, definitionally, not "dirty").
fn read_dirty(path: &str) -> bool {
    let output = match crate::proc_util::command("git")
        .args(["-C", path, "status", "--porcelain"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    !String::from_utf8_lossy(&output.stdout).trim().is_empty()
}

/// The configured remotes of the work tree at `path` (`git remote`), sorted and
/// de-duplicated. A non-git path / git error yields an empty vec.
fn read_remotes(path: &str) -> Vec<String> {
    let output = match crate::proc_util::command("git")
        .args(["-C", path, "remote"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let mut remotes: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    remotes.sort();
    remotes.dedup();
    remotes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        archive_repository, create_project, list_repositories, open_in_memory,
        set_repository_optional,
    };
    use uuid::Uuid;

    /// A unique scratch dir under the OS temp dir (mirrors db.rs's helper; no
    /// tempfile dev-dep — uniqueness from a v7 UUID so parallel threads never collide).
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nyx-repo-{tag}-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// Is `git` on PATH? Detection/state tests are git-agnostic by contract; the ones
    /// that need a REAL repo skip gracefully when git is absent (CI image without git).
    fn git_available() -> bool {
        crate::proc_util::command("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Run a git subcommand in `dir`, asserting success, with a deterministic identity
    /// and signing forced off (a dev host may set commit.gpgsign globally).
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

    /// `git init` a repo at `dir` on a known branch with one commit (so HEAD is born).
    fn init_repo(dir: &std::path::Path, branch: &str) {
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

    /// DONE-CRITERION (task #5): mono-repo detection. A `root_path` that is itself a
    /// Git work tree detects ONE repository at `relative_path = "."`.
    #[test]
    fn detects_mono_repo_at_dot() {
        if !git_available() {
            eprintln!("skipping mono-repo detection: git unavailable");
            return;
        }
        let root = scratch_dir("mono");
        init_repo(&root, "main");

        let detected = detect_under_root(root.to_str().unwrap());
        assert_eq!(detected.len(), 1, "one repo detected for a mono-repo");
        assert_eq!(
            detected[0].relative_path, MONO_REPO_RELATIVE_PATH,
            "mono-repo relative_path is '.'"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task #5): multi-repo detection. A `root_path` that is NOT a work
    /// tree but has Git subfolders detects one repo per subfolder, at its subfolder name.
    #[test]
    fn detects_multi_repo_per_subfolder() {
        if !git_available() {
            eprintln!("skipping multi-repo detection: git unavailable");
            return;
        }
        let root = scratch_dir("multi");
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "main");
        // A plain (non-git) subfolder is IGNORED.
        std::fs::create_dir_all(root.join("docs")).unwrap();

        let detected = detect_under_root(root.to_str().unwrap());
        let paths: Vec<&str> = detected.iter().map(|d| d.relative_path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["api", "web"],
            "one repo per Git subfolder, non-git subfolders ignored, sorted"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A non-git (or absent) root yields NO detection — detection is git-agnostic.
    #[test]
    fn non_git_root_detects_nothing() {
        let plain = scratch_dir("plain");
        assert!(detect_under_root(plain.to_str().unwrap()).is_empty());
        assert!(
            detect_under_root("/no/such/path/nyx-absent").is_empty(),
            "an absent root detects nothing"
        );
        let _ = std::fs::remove_dir_all(&plain);
    }

    /// DONE-CRITERION (task #5): "Enregistrer un workspace détecte ses repos ; override
    /// manuel possible (optional)." Registration is idempotent (a re-scan does not
    /// duplicate) and a manual `optional` override survives a re-scan.
    #[test]
    fn register_is_idempotent_and_preserves_optional_override() {
        if !git_available() {
            eprintln!("skipping register test: git unavailable");
            return;
        }
        let root = scratch_dir("reg");
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "main");

        let mut conn = open_in_memory();
        let (_p, ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();

        let first = detect_and_register(&mut conn, &ws).unwrap();
        assert_eq!(first.len(), 2, "both subfolder repos registered");

        // Manual override: mark `web` optional (the in-DB nyxignore).
        let web = first.iter().find(|r| r.relative_path == "web").unwrap();
        set_repository_optional(&mut conn, &web.id, true).unwrap();

        // Re-scan: no duplicates, and the override is preserved.
        let second = detect_and_register(&mut conn, &ws).unwrap();
        assert_eq!(second.len(), 2, "re-scan does not duplicate");
        let all = list_repositories(&mut conn, &ws.project_id).unwrap();
        assert_eq!(all.len(), 2, "still exactly two repositories after re-scan");
        let web_after = all.iter().find(|r| r.relative_path == "web").unwrap();
        assert!(
            web_after.optional,
            "the manual `optional` override survives a re-scan"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task #5): manual override — RENAME a detected repo. The rename
    /// changes the logical `name` only (identity `relative_path` untouched), and a later
    /// re-scan keeps the user's chosen name (detection is a no-op on an active identity).
    #[test]
    fn manual_rename_override_survives_rescan() {
        if !git_available() {
            eprintln!("skipping rename test: git unavailable");
            return;
        }
        let root = scratch_dir("rename");
        init_repo(&root.join("api"), "main");

        let mut conn = open_in_memory();
        let (_p, ws) = create_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
        let registered = detect_and_register(&mut conn, &ws).unwrap();
        let api = &registered[0];
        assert_eq!(api.name, "api", "detected name is the folder basename");

        // Manual rename to a friendlier logical name.
        db::rename_repository(&mut conn, &api.id, "backend").unwrap();

        // Re-scan: the identity (relative_path 'api') is unchanged; the NAME override wins.
        detect_and_register(&mut conn, &ws).unwrap();
        let all = list_repositories(&mut conn, &ws.project_id).unwrap();
        assert_eq!(all.len(), 1, "no duplicate identity after rename + rescan");
        assert_eq!(
            all[0].name, "backend",
            "the manual rename survives a re-scan"
        );
        assert_eq!(
            all[0].relative_path, "api",
            "the identity path is untouched"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task #5): derived membership. A workspace's repos = the
    /// project's NON-OPTIONAL repositories at `root_path + relative_path`; there is no
    /// checkout table — membership is computed, and `optional` excludes a repo.
    #[test]
    fn membership_is_derived_and_excludes_optional() {
        let mut conn = open_in_memory();
        let (_p, ws) = create_project(
            &mut conn,
            "Nyx",
            #[cfg(windows)]
            "C:\\proj",
            #[cfg(not(windows))]
            "/proj",
            None,
        )
        .unwrap();

        // Two logical repos, registered directly (no filesystem needed for membership).
        let api = db::create_repository(&mut conn, &ws.project_id, "api", "api", false).unwrap();
        let web = db::create_repository(&mut conn, &ws.project_id, "web", "web", false).unwrap();

        // Both non-optional → both are members, each at root_path + relative_path.
        let members = workspace_members(&mut conn, &ws).unwrap();
        let paths: Vec<String> = members.iter().map(|m| m.absolute_path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                join_relative(&ws.root_path, "api"),
                join_relative(&ws.root_path, "web"),
            ],
            "members resolve at root_path + relative_path (derived, not stored)"
        );

        // Mark `web` optional → it drops out of the derived membership.
        set_repository_optional(&mut conn, &web.id, true).unwrap();
        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(members.len(), 1, "optional repo excluded from membership");
        assert_eq!(members[0].repository.id, api.id);

        // An ARCHIVED repo is likewise not a live member.
        set_repository_optional(&mut conn, &web.id, false).unwrap();
        archive_repository(&mut conn, &web.id).unwrap();
        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(members.len(), 1, "archived repo excluded from membership");
        assert_eq!(members[0].repository.id, api.id);
    }

    /// A mono-repo member resolves to the workspace `root_path` itself (`"."`).
    #[test]
    fn mono_repo_member_resolves_to_root_path() {
        let mut conn = open_in_memory();
        #[cfg(windows)]
        let root = "C:\\mono";
        #[cfg(not(windows))]
        let root = "/mono";
        let (_p, ws) = create_project(&mut conn, "Mono", root, None).unwrap();
        db::create_repository(
            &mut conn,
            &ws.project_id,
            "mono",
            MONO_REPO_RELATIVE_PATH,
            false,
        )
        .unwrap();

        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(
            members[0].absolute_path,
            crate::pathnorm::normalize(&ws.root_path),
            "a '.' mono-repo resolves to the workspace root itself"
        );
    }

    /// DONE-CRITERION (task #5): "État Git relu ... pas de divergence DB/Git." Git
    /// state is read LIVE from Git and reflects a change (a new commit shifting dirty →
    /// clean, a branch checkout) WITHOUT any DB write — the DB never stores it, so it
    /// can never diverge.
    #[test]
    fn git_state_is_reread_live_never_from_db() {
        if !git_available() {
            eprintln!("skipping git-state test: git unavailable");
            return;
        }
        let repo = scratch_dir("state");
        init_repo(&repo, "work-prd5");
        let path = repo.to_str().unwrap();

        // A freshly-committed repo is clean, on its known branch.
        let s = read_git_state(path);
        assert_eq!(s.branch.as_deref(), Some("work-prd5"), "branch read live");
        assert!(!s.dirty, "a freshly-committed tree is clean");

        // Add a remote → the LIVE read reflects it (no DB involved).
        git_in(
            &repo,
            &["remote", "add", "origin", "https://example.test/x.git"],
        );
        assert_eq!(
            read_git_state(path).remotes,
            vec!["origin".to_string()],
            "remotes are re-read from Git"
        );

        // Dirty the tree → the LIVE read flips dirty=true.
        std::fs::write(repo.join("README.md"), b"changed").unwrap();
        assert!(
            read_git_state(path).dirty,
            "an uncommitted change makes the live read dirty"
        );

        // Switch branch → the LIVE read follows, with NO DB write anywhere in between.
        git_in(&repo, &["checkout", "-q", "-b", "other"]);
        assert_eq!(
            read_git_state(path).branch.as_deref(),
            Some("other"),
            "a branch switch is reflected by the live re-read"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A non-git path yields the default (empty) Git state — git-agnostic.
    #[test]
    fn git_state_of_non_git_path_is_default() {
        let plain = scratch_dir("plain-state");
        let s = read_git_state(plain.to_str().unwrap());
        assert_eq!(s, GitState::default());
        assert!(s.branch.is_none() && !s.dirty && s.remotes.is_empty());
        let _ = std::fs::remove_dir_all(&plain);
    }

    /// DONE-CRITERION (task #5, finding #13): "Enregistrer un workspace détecte ses
    /// repos." END-TO-END proof through the REAL registration flow — `register_project`
    /// (create_project + auto-detect on the root) and `register_workspace`
    /// (create_workspace + auto-detect) — with NO direct `detect_and_register` call in
    /// the test. After registering a MULTI-REPO tree, `list_repositories` returns the
    /// detected repos.
    #[test]
    fn registering_a_multi_repo_workspace_detects_and_persists_repos() {
        if !git_available() {
            eprintln!("skipping multi-repo registration e2e: git unavailable");
            return;
        }
        let root = scratch_dir("e2e-multi");
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "main");
        std::fs::create_dir_all(root.join("docs")).unwrap(); // non-git: ignored

        let mut conn = open_in_memory();

        // Registering the PROJECT (its root workspace is the multi-repo tree) detects
        // and persists both repos — via the real registration path, not a manual scan.
        let (project, _root) =
            register_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
        let repos = list_repositories(&mut conn, &project.id).unwrap();
        let paths: Vec<&str> = repos.iter().map(|r| r.relative_path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["api", "web"],
            "registering the project detected and persisted both subfolder repos"
        );

        // Registering an ADDED workspace on the SAME tree re-detects idempotently
        // (same identities, no duplicates) — the workspace-registration path also wires
        // detection.
        let ws2 = register_workspace(
            &mut conn,
            &project.id,
            "worktree",
            root.join("api").to_str().unwrap(),
        )
        .unwrap();
        // `worktree` root_path is `.../api`, itself a git repo → detected as a mono-repo
        // identity at `.` in the SAME project, added to the two subfolder repos.
        assert_eq!(ws2.project_id, project.id);
        let after = list_repositories(&mut conn, &project.id).unwrap();
        let rels: Vec<&str> = after.iter().map(|r| r.relative_path.as_str()).collect();
        assert_eq!(
            rels,
            vec![".", "api", "web"],
            "registering a workspace also triggers detection (adds the '.' mono identity)"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task #5, finding #13): a MONO-REPO tree — registering a workspace
    /// whose root is itself a Git work tree detects one `.` repository, via the real
    /// registration flow.
    #[test]
    fn registering_a_mono_repo_workspace_detects_the_dot_repo() {
        if !git_available() {
            eprintln!("skipping mono-repo registration e2e: git unavailable");
            return;
        }
        let root = scratch_dir("e2e-mono");
        init_repo(&root, "main");

        let mut conn = open_in_memory();
        let (project, _root) =
            register_project(&mut conn, "Mono", root.to_str().unwrap(), None).unwrap();

        let repos = list_repositories(&mut conn, &project.id).unwrap();
        assert_eq!(repos.len(), 1, "one repo detected at registration");
        assert_eq!(
            repos[0].relative_path, MONO_REPO_RELATIVE_PATH,
            "the mono-repo is registered at relative_path '.'"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// DONE-CRITERION (task #5, finding #13): a manual override (`optional` / rename)
    /// SURVIVES a RE-registration through the real flow. A repository identity is keyed
    /// on `(project_id, relative_path)`, so registering ANOTHER workspace in the SAME
    /// project on a tree with the SAME subfolder layout re-runs detection over the same
    /// identities (idempotently) — the user's `optional` and `name` edits must be
    /// preserved. Proven entirely through `register_project` / `register_workspace`, the
    /// real registration seam (no direct `detect_and_register` call in the test).
    #[test]
    fn manual_override_survives_reregistration_via_real_flow() {
        if !git_available() {
            eprintln!("skipping override-survival e2e: git unavailable");
            return;
        }
        // Root workspace tree: repos `api` + `web`.
        let root = scratch_dir("e2e-override-root");
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "main");
        // A SECOND worktree of the same project, same subfolder layout at a different
        // absolute path (a real added worktree — the DB's UNIQUE(project_id, root_path)
        // forbids reusing the root's path, but the derived identities coincide).
        let worktree = scratch_dir("e2e-override-wt");
        init_repo(&worktree.join("api"), "feat");
        init_repo(&worktree.join("web"), "feat");

        let mut conn = open_in_memory();
        let (project, _root) =
            register_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();

        // User overrides on the detected identities: mark `web` optional, rename `api`.
        let registered = list_repositories(&mut conn, &project.id).unwrap();
        let web = registered
            .iter()
            .find(|r| r.relative_path == "web")
            .unwrap();
        let api = registered
            .iter()
            .find(|r| r.relative_path == "api")
            .unwrap();
        set_repository_optional(&mut conn, &web.id, true).unwrap();
        db::rename_repository(&mut conn, &api.id, "backend").unwrap();

        // Register an ADDED workspace on the second tree → detection re-runs over the
        // SAME `(project_id, relative_path)` identities via the real seam.
        register_workspace(
            &mut conn,
            &project.id,
            "worktree",
            worktree.to_str().unwrap(),
        )
        .unwrap();

        let after = list_repositories(&mut conn, &project.id).unwrap();
        assert_eq!(
            after.len(),
            2,
            "re-registration does not duplicate identities"
        );
        let web_after = after.iter().find(|r| r.relative_path == "web").unwrap();
        let api_after = after.iter().find(|r| r.relative_path == "api").unwrap();
        assert!(
            web_after.optional,
            "the manual `optional` override survives re-registration"
        );
        assert_eq!(
            api_after.name, "backend",
            "the manual rename survives re-registration"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&worktree);
    }
}
