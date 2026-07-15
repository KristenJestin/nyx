//! Repository as a LOGICAL identity (PDR-0013, v2.1 task #5, finding #27).
//!
//! A repository is a logical Nyx identity within a project (e.g. `api`) — the SAME
//! object across the project's primary workspace and every dev worktree, so
//! review/history follow it. This module owns the behaviours the canonical model
//! demands on top of the `repositories` persistence socle in [`crate::db`]:
//!
//! 1. **Explicit configuration, NO disk scan** (finding #27). There is NO
//!    auto-detection: nyx never scans a workspace's `root_path` to guess repos. The
//!    repos of a project are configured EXPLICITLY, once per PROJECT, as a set of
//!    `relative_path` (+ name) associations ([`crate::db::set_project_repositories`]).
//!    A MONO-REPO is a single identity at `relative_path = "."`; a MULTI-REPO layout is
//!    one identity per subfolder — but which is which is the user's declared config, not
//!    a scan result.
//!
//! 2. **Derived membership + current-folder fallback** ([`workspace_members`]). A
//!    workspace does NOT store a checkout list — there is no `workspace_repositories`
//!    table (PDR-0013). A workspace's repos are DERIVED: the project's NON-OPTIONAL
//!    repositories, each resolved at `workspace.root_path` joined with the repo's
//!    `relative_path`. `optional` (the in-DB "nyxignore") excludes a repo. When a
//!    project has NO configured repos at all, membership FALLS BACK to the workspace's
//!    own folder as a single repo at `"."` (a synthetic, non-persisted identity),
//!    handled gracefully even when that folder is not a Git work tree — never a scan.
//!
//! 3. **Git state re-read** ([`read_git_state`]). Branch, dirty flag, and remotes
//!    are read from Git LIVE, on demand, and are NEVER written to the DB — the DB is
//!    not authoritative for Git state (PDR-0013). The returned [`GitState`] is a
//!    transient view, not a persisted row.
//!
//! Everything git-facing shells out through [`crate::proc_util::command`] (the
//! hardened, console-flash-free spawn), stays fully git-agnostic (a non-git path
//! simply yields an empty [`GitState`]), and never errors on a missing/So-so repo —
//! state reads are best-effort by contract.

use std::path::Path;

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::db::{self, Repository, Workspace};

/// The conventional `relative_path` of a mono-repo: the repository lives at the
/// workspace root itself. Kept as a shared const so config and membership agree.
pub const MONO_REPO_RELATIVE_PATH: &str = ".";

/// The stable-id PREFIX of the synthetic current-folder FALLBACK repository (finding
/// #27). When a project has no configured repos, [`workspace_members`] yields one
/// synthetic identity `"{FALLBACK_REPO_ID_PREFIX}{project_id}"` at `relative_path = "."`.
/// The id is DETERMINISTIC per project so it round-trips through
/// [`member_absolute_path`] (a `repo_diff`/`repo_git_view` call on it resolves back to
/// the workspace root) — even though the identity is never persisted in `repositories`.
pub const FALLBACK_REPO_ID_PREFIX: &str = "fallback:";

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

/// REGISTER a non-root workspace at `path` under `project_id`. This is the canonical
/// nyx-core entry point for "enregistrer un workspace" (done_criterion #5): it inserts
/// the workspace row via [`db::create_workspace`]. It NO LONGER scans the disk for repos
/// (finding #27): the project's repos are configured explicitly once, and this workspace
/// INHERITS them (derived at `root_path || relative_path`, see [`workspace_members`]).
///
/// Returns the freshly-created [`Workspace`]. The insert failing is propagated as-is (the
/// caller maps it to its error vocabulary).
pub fn register_workspace(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    path: &str,
) -> QueryResult<Workspace> {
    db::create_workspace(conn, project_id, name, path)
}

/// REGISTER a project plus its root workspace at `root_path`. The root-workspace analogue
/// of [`register_workspace`]: it creates the project and its single root workspace via
/// [`db::create_project`]. It does NOT scan the disk (finding #27) — the project's repos
/// are configured explicitly afterwards ([`db::set_project_repositories`]), or a project
/// left unconfigured falls back to the current folder at derivation time
/// ([`workspace_members`]). Returns the new project and its root workspace.
pub fn register_project(
    conn: &mut SqliteConnection,
    name: &str,
    root_path: &str,
    root_name: Option<&str>,
) -> QueryResult<(crate::db::Project, Workspace)> {
    db::create_project(conn, name, root_path, root_name)
}

/// The DERIVED membership of `workspace`: the project's NON-OPTIONAL repositories, each
/// resolved to its absolute path IN this workspace (`root_path` + `relative_path`).
///
/// There is NO checkout table (PDR-0013): membership is computed here from the project's
/// configured `repositories` (already filtered to the LIVE, non-archived set by
/// [`db::list_repositories`]) minus the `optional` ones (the in-DB "nyxignore"). The path
/// is COMPUTED per workspace, never stored — so the same logical repo lands at the right
/// place in the primary workspace and in every worktree.
///
/// FALLBACK (finding #27): when the project has NO configured repositories at all,
/// membership degrades to a SINGLE synthetic repo at `relative_path = "."` (the
/// workspace's own folder). This is never a disk scan and never persisted — it just lets
/// an unconfigured project's workspace still surface its current folder to
/// `workspace_git`/`repo_diff`/review, gracefully even when that folder is not a Git work
/// tree (the Git reads then degrade to an empty state). A project that HAS configured
/// repos but marked them all `optional` is a deliberate config: its membership is
/// legitimately empty (no fallback).
pub fn workspace_members(
    conn: &mut SqliteConnection,
    workspace: &Workspace,
) -> QueryResult<Vec<WorkspaceRepo>> {
    let repos = db::list_repositories(conn, &workspace.project_id)?;
    if repos.is_empty() {
        // No repo configured on the project → the current folder as the sole repo.
        return Ok(vec![fallback_member(workspace)]);
    }
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

/// Build the synthetic current-folder FALLBACK member for `workspace` (finding #27): a
/// single repo at `relative_path = "."` resolving to the workspace root, with a
/// deterministic synthetic id (`"{FALLBACK_REPO_ID_PREFIX}{project_id}"`) so it
/// round-trips through [`member_absolute_path`]. Never persisted, never a scan.
fn fallback_member(workspace: &Workspace) -> WorkspaceRepo {
    let now = crate::db::now_millis();
    let name = Path::new(&workspace.root_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    let repository = Repository {
        id: format!("{FALLBACK_REPO_ID_PREFIX}{}", workspace.project_id),
        project_id: workspace.project_id.clone(),
        name,
        relative_path: MONO_REPO_RELATIVE_PATH.to_string(),
        optional: false,
        created_at: now,
        updated_at: now,
        archived_at: None,
    };
    WorkspaceRepo {
        absolute_path: join_relative(&workspace.root_path, MONO_REPO_RELATIVE_PATH),
        repository,
    }
}

/// Resolve ONE `(workspace, repository)` membership to its ABSOLUTE on-disk path in that
/// workspace, or `None` when the pair does not resolve (unknown workspace, or the repo is
/// not a non-optional member of the workspace's project). This is the multi-repo-safe
/// path builder the v2.4 diff engine and derived-Git views key on: it never mixes two
/// repositories' identities because it returns exactly ONE repo's path, computed the same
/// way [`workspace_members`] does (`root_path` + `relative_path`, normalized).
pub fn member_absolute_path(
    conn: &mut SqliteConnection,
    workspace_id: &str,
    repository_id: &str,
) -> QueryResult<Option<String>> {
    let Some(workspace) = db::get_workspace(conn, workspace_id)? else {
        return Ok(None);
    };
    let members = workspace_members(conn, &workspace)?;
    Ok(members
        .into_iter()
        .find(|m| m.repository.id == repository_id)
        .map(|m| m.absolute_path))
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

/// One changed path in a work tree, as re-read from `git status --porcelain` — the
/// SUMMARY view (PDR-0013), NOT a line-level diff (that engine is v2.3). `status` is
/// the two-character porcelain XY code (e.g. ` M`, `??`, `A `, `R `); `path` is the
/// changed path (the destination for a rename). Transient — NEVER persisted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChangedFile {
    /// The porcelain XY status code (two chars, e.g. ` M`, `??`, `MM`, `R `).
    pub status: String,
    /// The changed path (for a rename `R  old -> new`, this is the NEW path).
    pub path: String,
}

/// The changed files of the work tree at `path`, re-read LIVE from `git status
/// --porcelain -z` (SUMMARY only — file list + status code, NOT a line-level diff,
/// which is v2.3). NOTHING is persisted (PDR-0013): the DB is not authoritative for
/// Git state, so this read is always fresh and cannot diverge from a DB that does not
/// store it. A non-git path / git error yields an empty vec. Paths are returned in
/// Git's own order (sorted-ish by path); a rename entry keeps only its destination
/// path (the summary's "what changed"), matching the `dirty` flag's granularity.
///
/// The `-z` form is used so a path with spaces or unusual characters is delimited by
/// NUL (never quoted/escaped), making the parse unambiguous. Each record is
/// `XY<space>PATH`; a rename/copy record (`R`/`C`) is followed by a SECOND NUL-delimited
/// field (the origin path) which we skip — the destination is the changed path.
pub fn read_changed_files(path: &str) -> Vec<ChangedFile> {
    let output = match crate::proc_util::command("git")
        .args(["-C", path, "status", "--porcelain", "-z"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut records = stdout.split('\0');
    let mut changed = Vec::new();
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        // A porcelain record is `XY PATH` (X,Y then a space then the path). Guard a
        // malformed short record defensively.
        if record.len() < 3 {
            continue;
        }
        let status = record[..2].to_string();
        let file = record[3..].to_string();
        // A rename (`R`) or copy (`C`) record is followed by a SECOND NUL-delimited
        // field carrying the ORIGIN path; consume it so it is not parsed as a record.
        let x = status.as_bytes()[0];
        if x == b'R' || x == b'C' {
            let _origin = records.next();
        }
        changed.push(ChangedFile { status, path: file });
    }
    changed
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

    /// FINDING #27: explicit config → repos visible by the workspace. A project with NO
    /// disk scan gets its repos declared via [`db::set_project_repositories`]; the
    /// workspace then DERIVES them at `root_path + relative_path` (PDR-0013 preserved).
    #[test]
    fn set_project_repositories_configures_repos_visible_by_workspace() {
        let mut conn = open_in_memory();
        #[cfg(windows)]
        let root = "C:\\proj";
        #[cfg(not(windows))]
        let root = "/proj";
        let (project, ws) = create_project(&mut conn, "Nyx", root, None).unwrap();

        // No repo configured yet → the workspace falls back to the current folder.
        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(members.len(), 1, "unconfigured → single fallback member");
        assert_eq!(members[0].repository.relative_path, MONO_REPO_RELATIVE_PATH);

        // Explicitly configure two sub-repos by relative path (+ an explicit name for one).
        let configured = db::set_project_repositories(
            &mut conn,
            &project.id,
            &[
                db::RepoConfig {
                    relative_path: "api".into(),
                    name: Some("backend".into()),
                },
                db::RepoConfig {
                    relative_path: "web".into(),
                    name: None,
                },
            ],
        )
        .unwrap();
        let rels: Vec<&str> = configured
            .iter()
            .map(|r| r.relative_path.as_str())
            .collect();
        assert_eq!(rels, vec!["api", "web"], "both configured, canonical order");
        assert_eq!(
            configured
                .iter()
                .find(|r| r.relative_path == "api")
                .unwrap()
                .name,
            "backend",
            "explicit name wins"
        );
        assert_eq!(
            configured
                .iter()
                .find(|r| r.relative_path == "web")
                .unwrap()
                .name,
            "web",
            "name defaults to the path basename"
        );

        // The workspace now DERIVES both, each at root_path + relative_path.
        let members = workspace_members(&mut conn, &ws).unwrap();
        let paths: Vec<String> = members.iter().map(|m| m.absolute_path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                join_relative(&ws.root_path, "api"),
                join_relative(&ws.root_path, "web"),
            ],
            "workspace inherits the project's configured repos (PDR-0013)"
        );
    }

    /// FINDING #27: re-running the config RECONCILES — a repo dropped from the set is
    /// archived (removed from membership) while the kept one survives with its history.
    #[test]
    fn set_project_repositories_reconciles_removed_repos() {
        let mut conn = open_in_memory();
        #[cfg(windows)]
        let root = "C:\\reconcile";
        #[cfg(not(windows))]
        let root = "/reconcile";
        let (project, ws) = create_project(&mut conn, "Nyx", root, None).unwrap();

        db::set_project_repositories(
            &mut conn,
            &project.id,
            &[
                db::RepoConfig {
                    relative_path: "api".into(),
                    name: None,
                },
                db::RepoConfig {
                    relative_path: "web".into(),
                    name: None,
                },
            ],
        )
        .unwrap();
        let api_id = list_repositories(&mut conn, &project.id)
            .unwrap()
            .into_iter()
            .find(|r| r.relative_path == "api")
            .unwrap()
            .id;

        // Re-config with only `api` → `web` is archived, `api` keeps its identity.
        let after = db::set_project_repositories(
            &mut conn,
            &project.id,
            &[db::RepoConfig {
                relative_path: "api".into(),
                name: None,
            }],
        )
        .unwrap();
        assert_eq!(after.len(), 1, "only the kept repo remains active");
        assert_eq!(
            after[0].id, api_id,
            "the kept repo keeps its stable identity"
        );
        assert_eq!(after[0].relative_path, "api");
        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(members.len(), 1, "membership reflects the reconcile");
        assert_eq!(members[0].repository.relative_path, "api");
    }

    /// FINDING #27: fallback to the CURRENT FOLDER when nothing is configured — the
    /// derived membership is a single repo at `"."` resolving to the workspace root, with
    /// a deterministic synthetic id (no disk scan, no persisted row).
    #[test]
    fn unconfigured_project_falls_back_to_current_folder() {
        let mut conn = open_in_memory();
        #[cfg(windows)]
        let root = "C:\\fallback";
        #[cfg(not(windows))]
        let root = "/fallback";
        let (project, ws) = create_project(&mut conn, "Nyx", root, None).unwrap();

        assert!(
            list_repositories(&mut conn, &project.id)
                .unwrap()
                .is_empty(),
            "no repo is persisted (fallback is derived, never a scan/write)"
        );
        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(members.len(), 1, "one fallback member");
        assert_eq!(members[0].repository.relative_path, MONO_REPO_RELATIVE_PATH);
        assert_eq!(
            members[0].absolute_path,
            crate::pathnorm::normalize(&ws.root_path),
            "the fallback repo resolves to the workspace root itself"
        );
        assert_eq!(
            members[0].repository.id,
            format!("{FALLBACK_REPO_ID_PREFIX}{}", project.id),
            "the fallback id is deterministic per project"
        );

        // Deterministic id ROUND-TRIPS through member_absolute_path (repo_diff can resolve
        // the fallback repo back to the workspace root).
        let resolved = member_absolute_path(&mut conn, &ws.id, &members[0].repository.id)
            .unwrap()
            .expect("fallback id resolves");
        assert_eq!(resolved, crate::pathnorm::normalize(&ws.root_path));
    }

    /// FINDING #27: registering a project/workspace NEVER scans the disk — a multi-repo
    /// tree on disk yields NO auto-detected repos; only the current-folder fallback until
    /// the user configures explicitly.
    #[test]
    fn register_does_not_scan_disk() {
        if !git_available() {
            eprintln!("skipping no-scan test: git unavailable");
            return;
        }
        let root = scratch_dir("noscan");
        init_repo(&root.join("api"), "main");
        init_repo(&root.join("web"), "main");

        let mut conn = open_in_memory();
        let (project, ws) =
            register_project(&mut conn, "Nyx", root.to_str().unwrap(), None).unwrap();

        assert!(
            list_repositories(&mut conn, &project.id)
                .unwrap()
                .is_empty(),
            "registering a project does NOT scan/persist the on-disk sub-repos"
        );
        // Membership is the single current-folder fallback (root itself, non-git container).
        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(
            members.len(),
            1,
            "only the fallback, never a scan of api/web"
        );
        assert_eq!(members[0].repository.relative_path, MONO_REPO_RELATIVE_PATH);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// FINDING #27: the fallback is GRACEFUL for a non-git folder — no crash, empty Git
    /// state (branch=None, clean, no changed files).
    #[test]
    fn fallback_is_graceful_for_non_git_folder() {
        let plain = scratch_dir("nongit");
        let mut conn = open_in_memory();
        let (_p, ws) = create_project(&mut conn, "Nyx", plain.to_str().unwrap(), None).unwrap();

        let members = workspace_members(&mut conn, &ws).unwrap();
        assert_eq!(
            members.len(),
            1,
            "fallback member even for a non-git folder"
        );
        // The Git re-read of a non-git path degrades to the default (no crash).
        let state = read_git_state(&members[0].absolute_path);
        assert_eq!(state, GitState::default());
        assert!(read_changed_files(&members[0].absolute_path).is_empty());

        let _ = std::fs::remove_dir_all(&plain);
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

    /// FINDING #27 (done_criterion): a SINGLE project config is INHERITED by every
    /// workspace/worktree — the repos of a workspace = the project's configured repos at
    /// `workspace.root_path + relative_path` (PDR-0013 preserved, without any auto-detection
    /// or per-workspace config). Proven through the real `register_project` /
    /// `register_workspace` seam plus one `set_project_repositories` config call.
    #[test]
    fn project_config_is_inherited_by_every_workspace() {
        let mut conn = open_in_memory();
        #[cfg(windows)]
        let (root, wt) = ("C:\\proj", "C:\\proj-feat");
        #[cfg(not(windows))]
        let (root, wt) = ("/proj", "/proj-feat");

        // Register a project (root workspace) and add a second worktree — NO disk scan.
        let (project, root_ws) = register_project(&mut conn, "Nyx", root, None).unwrap();
        let feat_ws = register_workspace(&mut conn, &project.id, "feat", wt).unwrap();

        // Configure the project's repos ONCE.
        db::set_project_repositories(
            &mut conn,
            &project.id,
            &[
                db::RepoConfig {
                    relative_path: "api".into(),
                    name: None,
                },
                db::RepoConfig {
                    relative_path: "web".into(),
                    name: None,
                },
            ],
        )
        .unwrap();

        // BOTH workspaces derive the SAME logical repos, each at their own root_path.
        for ws in [&root_ws, &feat_ws] {
            let members = workspace_members(&mut conn, ws).unwrap();
            let paths: Vec<String> = members.iter().map(|m| m.absolute_path.clone()).collect();
            assert_eq!(
                paths,
                vec![
                    join_relative(&ws.root_path, "api"),
                    join_relative(&ws.root_path, "web"),
                ],
                "the worktree inherits the ONE project config at its own root_path"
            );
        }
    }
}
