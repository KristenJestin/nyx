//! The workspace GIT CHANGES read model (v2.2 Phase 1, task
//! `01KWHPV33EGTB3Z41J0RQR1BAK`) — the cockpit's "what changed" summary, re-read from
//! Git, never authoritative in the DB (PDR-0013).
//!
//! ## Why this module exists
//!
//! The v2.2 cockpit shows what changed in a workspace WITHOUT embedding a diff engine:
//! per non-optional repository, the current branch, whether the work tree is dirty,
//! and the list of changed files (a SUMMARY, not a line-level diff). Membership is the
//! v2.1 DERIVED set ([`crate::repository::workspace_members`]: the project's
//! non-optional repos at `root_path || relative_path`, no checkout table), and every
//! Git fact is RE-READ from Git on demand ([`crate::repository::read_git_state`] +
//! [`crate::repository::read_changed_files`]) and NEVER written to the DB — so the view
//! can never diverge from Git (the DB does not store it).
//!
//! ```text
//! UI  (db_command.rs `workspace_git_summary`) --\
//!                                                -> workspace_git::summary
//! MCP (mcp_tools_core `workspace_git` tool)    --/   -> repository::{workspace_members,
//!                                                          read_git_state, read_changed_files}
//! ```
//!
//! ## Scope (PDR-0013, Q-015)
//!
//! - IN: current branch, dirty flag, changed-file list (status + path), per repo,
//!   aggregated over the workspace's NON-OPTIONAL repositories (multi-repo).
//! - OUT: line-level diff + comments (the review engine is v2.3); real-time refresh of
//!   the Git view (v2.3) — the summary is read on demand, not pushed.
//!
//! ## Never authoritative in the DB
//!
//! This module issues ONLY reads. It never touches a mutating `db::*` function and never
//! records a `domain_event`: Git state is transient, re-read from Git each call. That is
//! the guarantee behind "aucun état Git stocké comme vérité en DB".

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::db::{self, Workspace};
use crate::repository::{self, ChangedFile};

/// The Git CHANGES summary of ONE repository within a workspace, re-read from Git.
/// Identity fields (`repository_id` / `name` / `relative_path` / `absolute_path`) come
/// from the derived membership; the Git fields (`branch` / `dirty` / `changed_files`)
/// are re-read LIVE and never persisted (PDR-0013).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoGitSummary {
    /// The logical repository identity id (from the `repositories` table).
    pub repository_id: String,
    /// The repository's logical name.
    pub name: String,
    /// Its conventional path within the project (`"."` in a mono-repo).
    pub relative_path: String,
    /// Its ABSOLUTE on-disk path IN this workspace (`root_path || relative_path`).
    pub absolute_path: String,
    /// The current HEAD branch, re-read from Git (`None` = detached/unborn/non-git).
    pub branch: Option<String>,
    /// `true` when the work tree has uncommitted changes (re-read from Git).
    pub dirty: bool,
    /// The changed files (status code + path), re-read from Git. A SUMMARY, not a diff.
    pub changed_files: Vec<ChangedFile>,
}

/// The workspace Git CHANGES read model: the branch/dirty/changed-files summary for
/// EACH non-optional repository of the workspace, aggregated (multi-repo). Every field
/// is re-read from Git; nothing is persisted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceGitSummary {
    /// The workspace id this summary is for.
    pub workspace_id: String,
    /// One summary per non-optional repository of the workspace, in membership order.
    pub repos: Vec<RepoGitSummary>,
}

/// Build the [`WorkspaceGitSummary`] for `workspace_id`, or `None` when the id does not
/// resolve to a workspace. READ-ONLY: resolves the workspace, derives its non-optional
/// repositories ([`repository::workspace_members`]), and RE-READS each one's Git
/// branch/dirty/changed-files from Git — persisting NOTHING (PDR-0013). A repo whose
/// folder is absent or not a Git work tree degrades to `branch=None, dirty=false,
/// changed_files=[]` (Git reads are best-effort / git-agnostic).
pub fn summary(
    conn: &mut SqliteConnection,
    workspace_id: &str,
) -> QueryResult<Option<WorkspaceGitSummary>> {
    let Some(workspace) = db::get_workspace(conn, workspace_id)? else {
        return Ok(None);
    };
    let repos = repo_summaries(conn, &workspace)?;
    Ok(Some(WorkspaceGitSummary {
        workspace_id: workspace.id,
        repos,
    }))
}

/// The per-repo summaries for `workspace` — the derived non-optional membership, each
/// with its Git branch/dirty/changed-files re-read LIVE. Split out so the DB read (the
/// membership) and the Git re-read (per repo) are clearly separated: the ONLY DB access
/// is the membership query; everything Git is a transient re-read.
fn repo_summaries(
    conn: &mut SqliteConnection,
    workspace: &Workspace,
) -> QueryResult<Vec<RepoGitSummary>> {
    let members = repository::workspace_members(conn, workspace)?;
    Ok(members
        .into_iter()
        .map(|member| {
            // Git fields RE-READ from Git (never from the DB): branch + dirty from the
            // shared `read_git_state`, the changed-file list from `read_changed_files`.
            let state = repository::read_git_state(&member.absolute_path);
            let changed_files = repository::read_changed_files(&member.absolute_path);
            RepoGitSummary {
                repository_id: member.repository.id,
                name: member.repository.name,
                relative_path: member.repository.relative_path,
                absolute_path: member.absolute_path,
                branch: state.branch,
                dirty: state.dirty,
                changed_files,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::repository::register_project;
    use uuid::Uuid;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nyx-wsgit-{tag}-{}", Uuid::now_v7()));
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
        std::fs::write(dir.join("README.md"), b"nyx").expect("seed file");
        git_in(dir, &["add", "README.md"]);
        git_in(dir, &["commit", "-q", "-m", "init"]);
    }

    /// done_criterion #1 — the cockpit lists branch/dirty/changed-files of a workspace,
    /// re-read from Git. A MULTI-REPO workspace (`api` + `web`): `api` clean, `web`
    /// dirtied with one modified + one untracked file → the summary reflects each repo's
    /// live branch/dirty/files. done_criterion #2 (no DB truth) is proven structurally:
    /// this module issues no mutating `db::*` call, and the summary carries no persisted
    /// row — a second read after dirtying reflects the change with no DB write between.
    #[test]
    fn summary_lists_branch_dirty_and_changed_files_per_repo_reread_from_git() {
        if !git_available() {
            eprintln!("skipping workspace-git summary: git unavailable");
            return;
        }
        let root = scratch_dir("multi");
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "feature");

        let mut conn = open_in_memory();
        let (_project, root_ws) =
            register_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();

        // Baseline: both repos clean, on their branches, no changed files.
        let s0 = summary(&mut conn, &root_ws.id).unwrap().unwrap();
        assert_eq!(s0.repos.len(), 2, "both non-optional repos summarized");
        let api0 = s0.repos.iter().find(|r| r.relative_path == "api").unwrap();
        let web0 = s0.repos.iter().find(|r| r.relative_path == "web").unwrap();
        assert_eq!(api0.branch.as_deref(), Some("main"));
        assert_eq!(web0.branch.as_deref(), Some("feature"));
        assert!(!api0.dirty && api0.changed_files.is_empty());
        assert!(!web0.dirty && web0.changed_files.is_empty());

        // Dirty `web`: modify the tracked file + add an untracked one.
        std::fs::write(root.join("web/README.md"), b"changed").unwrap();
        std::fs::write(root.join("web/new.txt"), b"new").unwrap();

        // A SECOND read reflects the change — LIVE from Git, no DB write in between.
        let s1 = summary(&mut conn, &root_ws.id).unwrap().unwrap();
        let api1 = s1.repos.iter().find(|r| r.relative_path == "api").unwrap();
        let web1 = s1.repos.iter().find(|r| r.relative_path == "web").unwrap();
        assert!(!api1.dirty, "api stays clean");
        assert!(web1.dirty, "web is now dirty");
        let paths: Vec<&str> = web1.changed_files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.contains(&"README.md") && paths.contains(&"new.txt"),
            "changed files re-read from Git: {paths:?}"
        );
        let untracked = web1
            .changed_files
            .iter()
            .find(|f| f.path == "new.txt")
            .unwrap();
        assert_eq!(untracked.status, "??", "untracked file carries the ?? code");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// An OPTIONAL repo (the in-DB "nyxignore") is EXCLUDED from the Git summary — the
    /// summary aggregates over the NON-OPTIONAL membership only.
    #[test]
    fn summary_excludes_optional_repos() {
        if !git_available() {
            eprintln!("skipping optional-exclusion test: git unavailable");
            return;
        }
        let root = scratch_dir("opt");
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "main");

        let mut conn = open_in_memory();
        let (project, root_ws) =
            register_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();
        // Mark `web` optional → it drops out of the derived membership.
        let repos = db::list_repositories(&mut conn, &project.id).unwrap();
        let web = repos.iter().find(|r| r.relative_path == "web").unwrap();
        db::set_repository_optional(&mut conn, &web.id, true).unwrap();

        let s = summary(&mut conn, &root_ws.id).unwrap().unwrap();
        let rels: Vec<&str> = s.repos.iter().map(|r| r.relative_path.as_str()).collect();
        assert_eq!(rels, vec!["api"], "the optional repo is excluded");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A non-git member degrades to branch=None, dirty=false, no changed files — the
    /// summary is git-agnostic (a workspace with a declared repo whose folder is absent).
    #[test]
    fn summary_of_non_git_member_is_empty_state() {
        let mut conn = open_in_memory();
        #[cfg(windows)]
        let root = "C:\\nyx-wsgit-nongit";
        #[cfg(not(windows))]
        let root = "/tmp/nyx-wsgit-nongit-absent";
        let (project, root_ws) = db::create_project(&mut conn, "Nyx", root, None).unwrap();
        // A declared repo whose absolute path is not a git tree.
        db::create_repository(&mut conn, &project.id, "api", ".", false).unwrap();

        let s = summary(&mut conn, &root_ws.id).unwrap().unwrap();
        assert_eq!(s.repos.len(), 1);
        assert!(s.repos[0].branch.is_none());
        assert!(!s.repos[0].dirty);
        assert!(s.repos[0].changed_files.is_empty());
    }

    /// An unknown workspace id resolves to `None`.
    #[test]
    fn summary_unknown_workspace_is_none() {
        let mut conn = open_in_memory();
        assert!(summary(&mut conn, "nope").unwrap().is_none());
    }
}
