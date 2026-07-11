//! The v2.4 GIT DIFF ENGINE (PRD `01KX0EEP5H1MJW4T3QCZEWPHFK`, task
//! `01KX0F6FD836YASVYT17YV3FTW`, PDR-0007 / PDR-0012 / PDR-0015) — the source of
//! everything the local review shows.
//!
//! ## Why this module exists
//!
//! The review is a VUE DÉRIVÉE of the Git diff (PDR-0015): there is no stored review
//! entity, only a diff computed on demand for a `(workspace, repository)` pair. This
//! module is that diff engine, living in the shell-agnostic core so the SIDECAR
//! (PDR-0007) can drive it and expose the result to the renderer AND to MCP — without
//! any UI, real-time, or comment/anchoring concern (those are phases 3/2/4).
//!
//! ## gix PRIMARY, git CLI FALLBACK (the coherence contract)
//!
//! Every diff is computed by [`gix`] FIRST — repo discovery, rev-spec → SHA resolution,
//! tree/blob reads, and the line-level hunks (gix's `blob::UnifiedDiff`, i.e.
//! imara-diff). When gix cannot open the repository or resolve a side (e.g. a corrupt
//! index, an unusual layout, or a gix limitation), the SAME operation falls back to the
//! `git` CLI (`crate::proc_util::command`, hardened against the Windows console flash).
//! Both backends normalize to the SAME [`RepoDiff`] shape, and each result records
//! which [`DiffBackend`] produced it, so a caller (and the coherence tests) can assert
//! gix and the CLI agree. There is NO remote (the review is 100% local): `base` is a
//! local branch or a bare SHA — nothing here assumes an `origin`.
//!
//! ## Multi-repo without mixing Git identities
//!
//! The engine is keyed on ONE repository at a time by its ABSOLUTE path (the
//! per-workspace membership path from [`crate::repository::workspace_members`]). A
//! [`RepoDiff`] carries the repository's own `git_dir`, resolved `base`/`target` SHAs,
//! and per-side blob ids — so the aggregating renderer can lay several reviews (one per
//! `(workspace, repository)`) side by side WITHOUT ever mixing one repo's blobs/refs
//! with another's. No cross-repo state lives here.
//!
//! ## Two modes: LIVING and PINNED (PDR-0015)
//!
//! [`DiffMode::Living`] follows the current state (the target side is re-read live —
//! e.g. the working tree, or a branch tip that moves), whereas [`DiffMode::Pinned`]
//! freezes the target on a specific SHA. The mode is a property of the REQUEST, not of
//! the target kind: a branch-vs-base request is living, a commit/range request is
//! pinned by construction. The engine records the resolved SHAs so a pinned review can
//! be re-derived byte-for-byte later.
//!
//! Nothing here is persisted — the diff is transient, re-derived on demand (PDR-0013).

use std::path::Path;

/// Which backend produced a diff. Recorded on every [`RepoDiff`] so a caller — and the
/// coherence tests — can tell whether gix or the `git` CLI fallback served it. gix is
/// always tried first; the CLI is used only when gix cannot open/resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffBackend {
    /// The pure-Rust `gix` primary backend produced this diff.
    Gix,
    /// The `git` CLI fallback produced this diff (gix could not open/resolve).
    GitCli,
}

/// LIVING vs PINNED (PDR-0015). A living diff follows the current repo state (the
/// target side is whatever Git says right now); a pinned diff is frozen on a resolved
/// SHA and re-derives identically forever. The mode is carried on the result so the
/// renderer knows whether to expect the view to move under a real-time refresh (phase
/// 2) or to stay fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffMode {
    /// Follows the current state (working tree, or a branch tip that can move).
    Living,
    /// Frozen on a specific commit SHA (a commit, a range end, or an explicit pin).
    Pinned,
}

/// WHAT to diff for a repository. Each variant maps to one of the review surfaces the
/// PRD enumerates (working tree / staged / unstaged / untracked / commit / range /
/// branches). The engine resolves each into an `(old, new)` pair of sides and produces
/// the per-file hunks between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    /// The FULL working-tree change vs `HEAD`: staged + unstaged + untracked together
    /// (`git diff HEAD` plus untracked), i.e. "everything not yet committed".
    WorkingTree,
    /// Only what is STAGED (index vs `HEAD`): `git diff --cached`.
    Staged,
    /// Only what is UNSTAGED (working tree vs index): `git diff`.
    Unstaged,
    /// Only the UNTRACKED files (present on disk, unknown to Git): each shown as an
    /// all-added file (old side empty).
    Untracked,
    /// A single COMMIT vs its first parent (`git show <sha>`), or vs the empty tree for
    /// a root commit. `rev` is any rev-spec gix/git resolves to a commit.
    Commit { rev: String },
    /// A RANGE `base..target` (two rev-specs): the diff of `target`'s tree vs `base`'s
    /// tree. Nothing is assumed about ancestry — it is a plain tree-to-tree diff.
    Range { base: String, target: String },
    /// BETWEEN BRANCHES: `target` branch/rev vs `base` branch/rev. Identical mechanics
    /// to [`DiffTarget::Range`] but named for the review's living "branch vs base"
    /// case; the SHAs are resolved and recorded so a living review can re-read them.
    Branches { base: String, target: String },
}

impl DiffTarget {
    /// The intrinsic [`DiffMode`] of this target: the worktree/index/untracked surfaces
    /// and a branch-vs-base comparison FOLLOW the current state (living); a specific
    /// commit or an explicit range is FROZEN on resolved SHAs (pinned). The caller may
    /// still override via [`diff_repo_with_mode`] (e.g. to pin a branch review on the
    /// SHA it currently resolves to), but this is the natural default.
    pub fn default_mode(&self) -> DiffMode {
        match self {
            DiffTarget::WorkingTree
            | DiffTarget::Staged
            | DiffTarget::Unstaged
            | DiffTarget::Untracked
            | DiffTarget::Branches { .. } => DiffMode::Living,
            DiffTarget::Commit { .. } | DiffTarget::Range { .. } => DiffMode::Pinned,
        }
    }
}

/// The kind of change a file underwent between the two sides. Mirrors Git's own
/// status letters so the renderer can show the right affordance without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The file exists only on the NEW side (added / untracked).
    Added,
    /// The file exists only on the OLD side (deleted).
    Deleted,
    /// The file exists on both sides with different content (modified).
    Modified,
}

/// One side of a file within a diff: which repository BLOB it came from (`None` when the
/// file is absent on that side — an add has no old blob, a delete has no new blob). The
/// blob id is the repo's OWN object id: it never mixes with another repository's blobs
/// (multi-repo identity isolation).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileSide {
    /// The path on this side (differs from the other side only under a rename; the
    /// engine reports the path per side so the renderer can label both).
    pub path: String,
    /// The 40-hex blob id on this side within THIS repository, or `None` when the file
    /// is absent on this side.
    pub blob: Option<String>,
    /// `true` when this side's blob is binary (no textual hunks are produced then).
    pub binary: bool,
}

/// A contiguous change region within a file, in unified-diff coordinates (1-based line
/// numbers, matching `@@ -old,+new @@`). Produced by gix's `blob::UnifiedDiff` on the
/// two blobs (primary) or parsed from `git diff` output (fallback) — both normalized to
/// this identical shape so the coherence tests can compare them directly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hunk {
    /// 1-based start line on the OLD side.
    pub old_start: u32,
    /// Number of lines the hunk spans on the OLD side.
    pub old_lines: u32,
    /// 1-based start line on the NEW side.
    pub new_start: u32,
    /// Number of lines the hunk spans on the NEW side.
    pub new_lines: u32,
    /// The hunk's lines, each tagged context / removed / added, WITHOUT the leading
    /// unified-diff prefix char (the tag carries it). This is the material the virtual
    /// diff viewer (phase 3) renders.
    pub lines: Vec<DiffLine>,
}

/// One line inside a [`Hunk`], tagged by role.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DiffLine {
    /// The line's role: context / removed / added.
    pub kind: LineKind,
    /// The line content, WITHOUT the trailing newline and WITHOUT the diff prefix char.
    pub content: String,
}

/// The role of a [`DiffLine`] within its hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// Present, unchanged, on both sides (a context line).
    Context,
    /// Present only on the OLD side (removed).
    Removed,
    /// Present only on the NEW side (added).
    Added,
}

/// The diff of ONE file between the two sides: both sides' identity + its hunks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileDiff {
    /// How the file changed (added / deleted / modified).
    pub change: ChangeKind,
    /// The OLD side (absent for an added file).
    pub old: Option<FileSide>,
    /// The NEW side (absent for a deleted file).
    pub new: Option<FileSide>,
    /// `true` when either side is binary — then `hunks` is empty (no textual diff).
    pub binary: bool,
    /// The line-level hunks (empty for a pure rename, a binary change, or an
    /// unreadable side).
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// The path the renderer keys this file on: the NEW path when present (adds /
    /// modifies), else the OLD path (deletes). Never empty for a well-formed diff.
    pub fn path(&self) -> &str {
        self.new
            .as_ref()
            .map(|s| s.path.as_str())
            .or_else(|| self.old.as_ref().map(|s| s.path.as_str()))
            .unwrap_or("")
    }
}

/// The full diff of ONE repository for ONE [`DiffTarget`] — the unit the renderer draws
/// as a single review pane. Carries the repository's own identity (its `git_dir` +
/// resolved `base`/`target` SHAs) so several of these can be aggregated in one UI
/// without mixing Git identities.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoDiff {
    /// The absolute path of the repository work tree this diff is for.
    pub repo_path: String,
    /// The resolved `.git` directory of this repository — its identity anchor. Distinct
    /// per repo, so the renderer never confuses two repos' objects.
    pub git_dir: Option<String>,
    /// LIVING or PINNED (PDR-0015).
    pub mode: DiffMode,
    /// The resolved OLD-side commit SHA, when the old side is a commit/tree (`None` for
    /// the index or an empty tree — e.g. staged-vs-HEAD with an unborn HEAD).
    pub base_sha: Option<String>,
    /// The resolved NEW-side commit SHA, when the new side is a commit/tree (`None` for
    /// the working tree / index — those have no commit SHA).
    pub target_sha: Option<String>,
    /// Which backend produced this diff (gix primary, git CLI fallback).
    pub backend: DiffBackend,
    /// One entry per changed file, sorted by path for a deterministic render order.
    pub files: Vec<FileDiff>,
}

/// An engine error. Kept minimal: the engine degrades gracefully (a non-git path yields
/// an `Err`, not a panic), and the CLI fallback is attempted before an error surfaces.
#[derive(Debug)]
pub enum DiffError {
    /// The path is not a Git work tree (neither gix nor the CLI could open it).
    NotAGitRepo(String),
    /// A rev-spec (commit / branch / range endpoint) did not resolve on either backend.
    BadRevspec(String),
    /// Any other backend failure, with a readable message.
    Backend(String),
}

impl std::fmt::Display for DiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiffError::NotAGitRepo(p) => write!(f, "not a git work tree: {p}"),
            DiffError::BadRevspec(r) => write!(f, "unresolvable rev-spec: {r}"),
            DiffError::Backend(m) => write!(f, "git diff backend error: {m}"),
        }
    }
}

impl std::error::Error for DiffError {}

/// Compute the [`RepoDiff`] for `repo_path` and `target`, using the target's intrinsic
/// mode ([`DiffTarget::default_mode`]). This is the common entry point; use
/// [`diff_repo_with_mode`] to override the mode (e.g. pin a branch review).
///
/// gix is tried first; on an open/resolve failure the `git` CLI fallback runs, so the
/// result is coherent regardless of which backend served it. Errors are returned (not
/// panicked): a non-git path is [`DiffError::NotAGitRepo`], an unresolvable rev-spec is
/// [`DiffError::BadRevspec`].
pub fn diff_repo(repo_path: &str, target: &DiffTarget) -> Result<RepoDiff, DiffError> {
    let mode = target.default_mode();
    diff_repo_with_mode(repo_path, target, mode)
}

/// Like [`diff_repo`] but with an explicit [`DiffMode`] override — used when a review
/// wants to PIN a target that is living by default (freeze a branch review on the SHA it
/// currently resolves to), or vice versa. The diff CONTENT is identical for the same
/// resolved sides; only the recorded `mode` (and the presence of resolved SHAs) differs.
pub fn diff_repo_with_mode(
    repo_path: &str,
    target: &DiffTarget,
    mode: DiffMode,
) -> Result<RepoDiff, DiffError> {
    // PRIMARY: gix. On ANY failure to open/resolve, fall through to the CLI fallback so
    // the engine stays coherent (PDR-0007). A gix success is returned as-is.
    match gix_backend::diff(repo_path, target, mode) {
        Ok(diff) => Ok(diff),
        Err(gix_err) => {
            // FALLBACK: git CLI. If the CLI also cannot open/resolve, surface the more
            // specific of the two errors so the caller sees a useful message.
            match cli_backend::diff(repo_path, target, mode) {
                Ok(mut diff) => {
                    diff.backend = DiffBackend::GitCli;
                    Ok(diff)
                }
                Err(cli_err) => Err(pick_error(gix_err, cli_err)),
            }
        }
    }
}

/// Prefer a specific error (NotAGitRepo / BadRevspec) over a generic backend error when
/// picking which of the two backends' failures to surface.
fn pick_error(gix_err: DiffError, cli_err: DiffError) -> DiffError {
    match (&gix_err, &cli_err) {
        (DiffError::Backend(_), _) => cli_err,
        _ => gix_err,
    }
}

/// Normalize raw blob bytes into the `Vec<DiffLine>`-friendly line vector AND report
/// whether the content looks binary (a NUL byte in the first 8 KiB — Git's own
/// heuristic). Shared by both backends so their hunks are byte-identical.
fn looks_binary(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(8192)];
    window.contains(&0)
}

/// Produce the unified-diff hunks between two blobs using gix's `blob::UnifiedDiff`
/// (imara-diff). This is the PRIMARY hunk source; the CLI fallback parses `git diff`
/// output into the SAME [`Hunk`] shape. Returns an empty vec when either side is binary
/// (no textual diff) or the two sides are identical.
fn hunks_between_blobs(old: &[u8], new: &[u8]) -> Vec<Hunk> {
    if looks_binary(old) || looks_binary(new) {
        return Vec::new();
    }
    use gix::bstr::ByteSlice;
    // Line tokens for imara-diff: split on '\n', keeping content without the newline.
    let input = gix::diff::blob::InternedInput::new(old.as_bstr(), new.as_bstr());
    let algorithm = gix::diff::blob::Algorithm::Histogram;
    let diff = gix::diff::blob::diff_with_slider_heuristics(algorithm, &input);

    // A ConsumeHunk sink that materializes each hunk into our [`Hunk`] shape.
    struct Sink {
        hunks: Vec<Hunk>,
    }
    impl gix::diff::blob::unified_diff::ConsumeHunk for Sink {
        type Out = Vec<Hunk>;
        fn consume_hunk(
            &mut self,
            header: gix::diff::blob::unified_diff::HunkHeader,
            lines: &[(gix::diff::blob::unified_diff::DiffLineKind, &[u8])],
        ) -> std::io::Result<()> {
            use gix::diff::blob::unified_diff::DiffLineKind;
            let lines = lines
                .iter()
                .map(|(kind, bytes)| {
                    let kind = match kind {
                        DiffLineKind::Context => LineKind::Context,
                        DiffLineKind::Remove => LineKind::Removed,
                        DiffLineKind::Add => LineKind::Added,
                    };
                    DiffLine {
                        kind,
                        content: String::from_utf8_lossy(bytes)
                            .trim_end_matches('\n')
                            .to_string(),
                    }
                })
                .collect();
            // Match git's unified-diff convention for a ZERO-length side: git encodes a
            // hunk that adds to (or removes from) nothing as `@@ -0,0 @@` — the start is
            // the line-count BEFORE which content is inserted, i.e. `start - 1` when the
            // side spans 0 lines. gix's `UnifiedDiff` always emits a 1-based `start`, so
            // normalize the zero-length case so both backends agree byte-for-byte.
            let old_start = if header.before_hunk_len == 0 {
                header.before_hunk_start.saturating_sub(1)
            } else {
                header.before_hunk_start
            };
            let new_start = if header.after_hunk_len == 0 {
                header.after_hunk_start.saturating_sub(1)
            } else {
                header.after_hunk_start
            };
            self.hunks.push(Hunk {
                old_start,
                old_lines: header.before_hunk_len,
                new_start,
                new_lines: header.after_hunk_len,
                lines,
            });
            Ok(())
        }
        fn finish(self) -> Self::Out {
            self.hunks
        }
    }

    let sink = Sink { hunks: Vec::new() };
    // A binary/encoding hiccup in the sink degrades to "no textual hunks" (the empty
    // default) rather than failing the whole diff — the file still shows as changed.
    gix::diff::blob::UnifiedDiff::new(
        &diff,
        &input,
        sink,
        gix::diff::blob::unified_diff::ContextSize::symmetrical(3),
    )
    .consume()
    .unwrap_or_default()
}

/// The gix PRIMARY backend: repo discovery, rev-spec resolution, tree/blob reads and
/// the line-level hunks — all via `gix`, no subprocess.
mod gix_backend {
    use super::*;
    use gix::bstr::BStr;

    /// Open `repo_path` with gix, or map the failure to a [`DiffError`] so the caller
    /// can fall back to the CLI.
    fn open(repo_path: &str) -> Result<gix::Repository, DiffError> {
        gix::open(repo_path).map_err(|_| DiffError::NotAGitRepo(repo_path.to_string()))
    }

    /// Resolve a rev-spec to a commit id + its tree bytes. `None`-tolerant callers use
    /// [`tree_of_head`] / the empty tree instead.
    fn resolve_commit(repo: &gix::Repository, spec: &str) -> Result<gix::ObjectId, DiffError> {
        let id = repo
            .rev_parse_single(BStr::new(spec.as_bytes()))
            .map_err(|_| DiffError::BadRevspec(spec.to_string()))?;
        Ok(id.detach())
    }

    /// The tree of a resolved commit id.
    fn tree_of<'r>(
        repo: &'r gix::Repository,
        commit: gix::ObjectId,
    ) -> Result<gix::Tree<'r>, DiffError> {
        repo.find_object(commit)
            .map_err(|e| DiffError::Backend(e.to_string()))?
            .peel_to_tree()
            .map_err(|e| DiffError::Backend(e.to_string()))
    }

    /// The tree of `HEAD`, or `None` when HEAD is unborn (no commit yet).
    fn tree_of_head(repo: &gix::Repository) -> Result<Option<gix::ObjectId>, DiffError> {
        match repo.head_id() {
            Ok(id) => Ok(Some(id.detach())),
            Err(_) => Ok(None), // unborn HEAD → compare against the empty tree
        }
    }

    /// Read a blob's bytes by id.
    fn blob_bytes(repo: &gix::Repository, id: gix::ObjectId) -> Result<Vec<u8>, DiffError> {
        let obj = repo
            .find_object(id)
            .map_err(|e| DiffError::Backend(e.to_string()))?;
        Ok(obj.data.clone())
    }

    /// The main gix entry: dispatch on the target kind, resolve both sides, and build
    /// the [`RepoDiff`].
    pub fn diff(
        repo_path: &str,
        target: &DiffTarget,
        mode: DiffMode,
    ) -> Result<RepoDiff, DiffError> {
        let repo = open(repo_path)?;
        let git_dir = Some(repo.git_dir().to_string_lossy().to_string());

        match target {
            DiffTarget::Commit { rev } => {
                let commit_id = resolve_commit(&repo, rev)?;
                let commit = repo
                    .find_object(commit_id)
                    .map_err(|e| DiffError::Backend(e.to_string()))?
                    .try_into_commit()
                    .map_err(|e| DiffError::Backend(e.to_string()))?;
                let new_sha = commit_id.to_string();
                // Parent tree (first parent), or the empty tree for a root commit.
                let parent_id = commit.parent_ids().next().map(|p| p.detach());
                let old_sha = parent_id.map(|p| p.to_string());
                let files = tree_diff_files(
                    &repo,
                    parent_id.map(|p| tree_of(&repo, p)).transpose()?,
                    Some(tree_of(&repo, commit_id)?),
                )?;
                Ok(RepoDiff {
                    repo_path: repo_path.to_string(),
                    git_dir,
                    mode,
                    base_sha: old_sha,
                    target_sha: Some(new_sha),
                    backend: DiffBackend::Gix,
                    files,
                })
            }
            DiffTarget::Range { base, target: tgt }
            | DiffTarget::Branches { base, target: tgt } => {
                let base_id = resolve_commit(&repo, base)?;
                let target_id = resolve_commit(&repo, tgt)?;
                let files = tree_diff_files(
                    &repo,
                    Some(tree_of(&repo, base_id)?),
                    Some(tree_of(&repo, target_id)?),
                )?;
                Ok(RepoDiff {
                    repo_path: repo_path.to_string(),
                    git_dir,
                    mode,
                    base_sha: Some(base_id.to_string()),
                    target_sha: Some(target_id.to_string()),
                    backend: DiffBackend::Gix,
                    files,
                })
            }
            DiffTarget::Staged => {
                // Index vs HEAD. gix cannot always express this as a plain tree diff
                // (the index is not a tree object), so we defer the ENUMERATION to the
                // shared status walk (git-porcelain), then read blobs via gix for the
                // hunks. This keeps gix as the blob/hunk authority while staying correct.
                let head = tree_of_head(&repo)?;
                let files = staged_files(&repo, repo_path, head)?;
                Ok(RepoDiff {
                    repo_path: repo_path.to_string(),
                    git_dir,
                    mode,
                    base_sha: head.map(|h| h.to_string()),
                    target_sha: None,
                    backend: DiffBackend::Gix,
                    files,
                })
            }
            DiffTarget::Unstaged | DiffTarget::WorkingTree | DiffTarget::Untracked => {
                // Working-tree surfaces: the NEW side is on disk (no commit SHA). The
                // file set comes from the shared porcelain walk; blobs from gix (old
                // side) + the filesystem (new side).
                let head = tree_of_head(&repo)?;
                let files = worktree_files(&repo, repo_path, head, target)?;
                Ok(RepoDiff {
                    repo_path: repo_path.to_string(),
                    git_dir,
                    mode,
                    base_sha: head.map(|h| h.to_string()),
                    target_sha: None,
                    backend: DiffBackend::Gix,
                    files,
                })
            }
        }
    }

    /// The per-file diff between two trees (either may be `None` = the empty tree),
    /// using gix's tree-to-tree change enumeration for the file set and gix blob reads
    /// for the hunks.
    fn tree_diff_files(
        repo: &gix::Repository,
        old_tree: Option<gix::Tree<'_>>,
        new_tree: Option<gix::Tree<'_>>,
    ) -> Result<Vec<FileDiff>, DiffError> {
        let changes = repo
            .diff_tree_to_tree(old_tree.as_ref(), new_tree.as_ref(), None)
            .map_err(|e| DiffError::Backend(e.to_string()))?;

        let mut files = Vec::new();
        for change in changes {
            use gix::object::tree::diff::ChangeDetached;
            // Skip DIRECTORY (tree) entries. gix's tree-to-tree diff reports a changed
            // directory (e.g. `src`) as a Modification of the tree entry ITSELF, IN ADDITION
            // to recursing into its changed blob children. A diff is a set of FILE changes, so
            // the tree entry is spurious noise — and, worse, a path that is BOTH a file (`src`)
            // and the prefix of a directory (`src/anchor.rs`) makes the renderer's @pierre/trees
            // changed-files tree THROW while building its path store ("Path collides with an
            // existing file while creating directory", Finding 55), blanking the tree behind its
            // error boundary. Keep BLOB changes only, from whichever side carries the entry mode.
            let is_tree = match &change {
                ChangeDetached::Addition { entry_mode, .. }
                | ChangeDetached::Deletion { entry_mode, .. }
                | ChangeDetached::Rewrite { entry_mode, .. } => entry_mode.is_tree(),
                ChangeDetached::Modification {
                    entry_mode,
                    previous_entry_mode,
                    ..
                } => entry_mode.is_tree() || previous_entry_mode.is_tree(),
            };
            if is_tree {
                continue;
            }
            let file = match change {
                ChangeDetached::Addition { location, id, .. } => {
                    let path = location.to_string();
                    let new_bytes = blob_bytes(repo, id).unwrap_or_default();
                    let binary = looks_binary(&new_bytes);
                    FileDiff {
                        change: ChangeKind::Added,
                        old: None,
                        new: Some(FileSide {
                            path: path.clone(),
                            blob: Some(id.to_string()),
                            binary,
                        }),
                        binary,
                        hunks: hunks_between_blobs(b"", &new_bytes),
                    }
                }
                ChangeDetached::Deletion { location, id, .. } => {
                    let path = location.to_string();
                    let old_bytes = blob_bytes(repo, id).unwrap_or_default();
                    let binary = looks_binary(&old_bytes);
                    FileDiff {
                        change: ChangeKind::Deleted,
                        old: Some(FileSide {
                            path: path.clone(),
                            blob: Some(id.to_string()),
                            binary,
                        }),
                        new: None,
                        binary,
                        hunks: hunks_between_blobs(&old_bytes, b""),
                    }
                }
                ChangeDetached::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                } => {
                    let path = location.to_string();
                    let old_bytes = blob_bytes(repo, previous_id).unwrap_or_default();
                    let new_bytes = blob_bytes(repo, id).unwrap_or_default();
                    let binary = looks_binary(&old_bytes) || looks_binary(&new_bytes);
                    FileDiff {
                        change: ChangeKind::Modified,
                        old: Some(FileSide {
                            path: path.clone(),
                            blob: Some(previous_id.to_string()),
                            binary: looks_binary(&old_bytes),
                        }),
                        new: Some(FileSide {
                            path: path.clone(),
                            blob: Some(id.to_string()),
                            binary: looks_binary(&new_bytes),
                        }),
                        binary,
                        hunks: hunks_between_blobs(&old_bytes, &new_bytes),
                    }
                }
                // Rewrites (rename/copy) are reported by gix only when rewrite tracking
                // is on; with the default options above they surface as add+delete,
                // which the renderer pairs by path. Treat any other detached variant as
                // a modification-less location to stay forward-compatible.
                other => {
                    let path = other.location().to_string();
                    FileDiff {
                        change: ChangeKind::Modified,
                        old: None,
                        new: Some(FileSide {
                            path,
                            blob: None,
                            binary: false,
                        }),
                        binary: false,
                        hunks: Vec::new(),
                    }
                }
            };
            files.push(file);
        }
        files.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(files)
    }

    /// The STAGED file set (index vs HEAD), enumerated by the shared porcelain walk and
    /// hunked via gix blob reads (old side from HEAD tree, new side from the index blob).
    fn staged_files(
        repo: &gix::Repository,
        repo_path: &str,
        head: Option<gix::ObjectId>,
    ) -> Result<Vec<FileDiff>, DiffError> {
        let entries = super::porcelain::staged_entries(repo_path)?;
        let head_tree = head.map(|h| tree_of(repo, h)).transpose()?;
        let mut files = Vec::new();
        for entry in entries {
            let old_bytes = head_tree
                .as_ref()
                .and_then(|t| super::gix_backend::blob_at_path(repo, t, &entry.path))
                .unwrap_or_default();
            // New side = the STAGED (index) content, which on disk equals the worktree
            // file unless it was further modified; reading the index blob keeps it exact.
            let new_bytes =
                super::porcelain::index_blob(repo_path, &entry.path).unwrap_or_default();
            files.push(build_file_diff(
                entry.change,
                &entry.path,
                old_bytes,
                new_bytes,
            ));
        }
        files.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(files)
    }

    /// The WORKING-TREE file set for the requested surface (unstaged / full worktree /
    /// untracked), enumerated by porcelain and hunked with the on-disk NEW side.
    fn worktree_files(
        repo: &gix::Repository,
        repo_path: &str,
        head: Option<gix::ObjectId>,
        target: &DiffTarget,
    ) -> Result<Vec<FileDiff>, DiffError> {
        let entries = super::porcelain::worktree_entries(repo_path, target)?;
        let head_tree = head.map(|h| tree_of(repo, h)).transpose()?;
        let mut files = Vec::new();
        for entry in entries {
            // OLD side: for the full worktree it is HEAD; for unstaged it is the index.
            let old_bytes = match target {
                DiffTarget::Unstaged => {
                    super::porcelain::index_blob(repo_path, &entry.path).unwrap_or_default()
                }
                _ => head_tree
                    .as_ref()
                    .and_then(|t| super::gix_backend::blob_at_path(repo, t, &entry.path))
                    .unwrap_or_default(),
            };
            let new_bytes = if entry.change == ChangeKind::Deleted {
                Vec::new()
            } else {
                std::fs::read(Path::new(repo_path).join(&entry.path)).unwrap_or_default()
            };
            files.push(build_file_diff(
                entry.change,
                &entry.path,
                old_bytes,
                new_bytes,
            ));
        }
        files.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(files)
    }

    /// Read the blob bytes at `path` within `tree` (a HEAD/commit tree). `None` when the
    /// path is absent in the tree (e.g. a newly added file).
    pub(super) fn blob_at_path(
        repo: &gix::Repository,
        tree: &gix::Tree<'_>,
        path: &str,
    ) -> Option<Vec<u8>> {
        let _ = repo; // identity is carried by the tree's own repo handle
        let entry = tree.lookup_entry_by_path(Path::new(path)).ok()??;
        let obj = entry.object().ok()?;
        Some(obj.data.clone())
    }

    /// Assemble a [`FileDiff`] from a change kind + both sides' bytes, computing hunks
    /// and binary-ness the SAME way the tree diff does (so all surfaces are coherent).
    fn build_file_diff(
        change: ChangeKind,
        path: &str,
        old_bytes: Vec<u8>,
        new_bytes: Vec<u8>,
    ) -> FileDiff {
        let old_binary = looks_binary(&old_bytes);
        let new_binary = looks_binary(&new_bytes);
        let binary = old_binary || new_binary;
        let (old, new) = match change {
            ChangeKind::Added => (
                None,
                Some(FileSide {
                    path: path.to_string(),
                    blob: None,
                    binary: new_binary,
                }),
            ),
            ChangeKind::Deleted => (
                Some(FileSide {
                    path: path.to_string(),
                    blob: None,
                    binary: old_binary,
                }),
                None,
            ),
            ChangeKind::Modified => (
                Some(FileSide {
                    path: path.to_string(),
                    blob: None,
                    binary: old_binary,
                }),
                Some(FileSide {
                    path: path.to_string(),
                    blob: None,
                    binary: new_binary,
                }),
            ),
        };
        FileDiff {
            change,
            old,
            new,
            binary,
            hunks: hunks_between_blobs(&old_bytes, &new_bytes),
        }
    }
}

/// Shared PORCELAIN helpers: the file-set enumeration for the working-tree/index
/// surfaces (staged / unstaged / worktree / untracked) and the blob-content reads that
/// gix cannot always express as a tree diff. Uses `git` porcelain (stable, unambiguous
/// with `-z`) so BOTH backends agree on the change set.
mod porcelain {
    use super::*;

    /// One enumerated change for an index/worktree surface.
    pub(super) struct Entry {
        pub path: String,
        pub change: ChangeKind,
    }

    /// Run a `git` subcommand in `repo_path`, returning stdout on success.
    fn git(repo_path: &str, args: &[&str]) -> Result<Vec<u8>, DiffError> {
        let out = crate::proc_util::command("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .map_err(|e| DiffError::Backend(e.to_string()))?;
        if !out.status.success() {
            return Err(DiffError::Backend(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    }

    /// The STAGED entries (index vs HEAD): `git diff --cached --name-status -z`.
    pub(super) fn staged_entries(repo_path: &str) -> Result<Vec<Entry>, DiffError> {
        let stdout = git(
            repo_path,
            &["diff", "--cached", "--name-status", "--no-renames", "-z"],
        )?;
        Ok(parse_name_status(&stdout))
    }

    /// The WORKING-TREE entries for the requested surface. `Unstaged` is `git diff`
    /// (worktree vs index); `WorkingTree` is `git diff HEAD` plus untracked; `Untracked`
    /// is only the untracked files. Anything else is treated as `WorkingTree`.
    pub(super) fn worktree_entries(
        repo_path: &str,
        target: &DiffTarget,
    ) -> Result<Vec<Entry>, DiffError> {
        match target {
            DiffTarget::Unstaged => {
                let stdout = git(repo_path, &["diff", "--name-status", "--no-renames", "-z"])?;
                Ok(parse_name_status(&stdout))
            }
            DiffTarget::Untracked => Ok(untracked(repo_path)?),
            _ => {
                // Full working tree = tracked changes vs HEAD + untracked files.
                let stdout = git(
                    repo_path,
                    &["diff", "HEAD", "--name-status", "--no-renames", "-z"],
                )
                .unwrap_or_default();
                let mut entries = parse_name_status(&stdout);
                entries.extend(untracked(repo_path)?);
                Ok(entries)
            }
        }
    }

    /// The untracked files (each an addition): `git ls-files --others --exclude-standard -z`.
    fn untracked(repo_path: &str) -> Result<Vec<Entry>, DiffError> {
        let stdout = git(
            repo_path,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        Ok(String::from_utf8_lossy(&stdout)
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(|p| Entry {
                path: p.to_string(),
                change: ChangeKind::Added,
            })
            .collect())
    }

    /// Parse `--name-status -z` output: `STATUS\0PATH\0…` (renames/copies carry a second
    /// path field but `--no-renames` above prevents those, keeping the parse simple).
    fn parse_name_status(stdout: &[u8]) -> Vec<Entry> {
        let text = String::from_utf8_lossy(stdout);
        let mut fields = text.split('\0');
        let mut entries = Vec::new();
        while let Some(status) = fields.next() {
            if status.is_empty() {
                continue;
            }
            let Some(path) = fields.next() else { break };
            if path.is_empty() {
                continue;
            }
            let change = match status.chars().next() {
                Some('A') => ChangeKind::Added,
                Some('D') => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            };
            entries.push(Entry {
                path: path.to_string(),
                change,
            });
        }
        entries
    }

    /// The STAGED blob content at `path` (`git show :path` reads the index stage-0
    /// blob). Empty on any failure (e.g. the path is not staged).
    pub(super) fn index_blob(repo_path: &str, path: &str) -> Option<Vec<u8>> {
        let out = crate::proc_util::command("git")
            .arg("-C")
            .arg(repo_path)
            .arg("show")
            .arg(format!(":{path}"))
            .output()
            .ok()?;
        if out.status.success() {
            Some(out.stdout)
        } else {
            None
        }
    }
}

/// The `git` CLI FALLBACK backend: used only when gix cannot open/resolve. Produces the
/// SAME [`RepoDiff`] shape by parsing `git diff` porcelain. Because it shares the
/// porcelain enumeration and the `hunks_between_blobs` normalization is only used by
/// gix, this path parses unified diff text directly into [`Hunk`]s.
mod cli_backend {
    use super::*;

    /// Run a `git` subcommand, returning stdout on success (non-zero exit is an error).
    fn git(repo_path: &str, args: &[&str]) -> Result<Vec<u8>, DiffError> {
        let out = crate::proc_util::command("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .map_err(|e| DiffError::Backend(e.to_string()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("not a git repository") {
                return Err(DiffError::NotAGitRepo(repo_path.to_string()));
            }
            if stderr.contains("unknown revision") || stderr.contains("bad revision") {
                return Err(DiffError::BadRevspec(args.join(" ")));
            }
            return Err(DiffError::Backend(stderr.trim().to_string()));
        }
        Ok(out.stdout)
    }

    /// Ensure the path is a git work tree, or error so the caller sees NotAGitRepo.
    fn ensure_repo(repo_path: &str) -> Result<Option<String>, DiffError> {
        let out = git(repo_path, &["rev-parse", "--git-dir"])?;
        Ok(Some(String::from_utf8_lossy(&out).trim().to_string()))
    }

    /// Resolve a rev-spec to a full SHA via the CLI, or a BadRevspec error.
    fn resolve(repo_path: &str, spec: &str) -> Result<String, DiffError> {
        let out = git(
            repo_path,
            &["rev-parse", "--verify", &format!("{spec}^{{commit}}")],
        )
        .map_err(|_| DiffError::BadRevspec(spec.to_string()))?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// The `git diff` args that produce the unified diff for a given target.
    fn diff_args(target: &DiffTarget) -> Vec<String> {
        let base: Vec<String> = vec![
            "diff".into(),
            "--no-color".into(),
            "--no-ext-diff".into(),
            "--no-renames".into(),
            // Force the CLASSIC `a/`…`b/` path prefixes. Without this, git uses MNEMONIC
            // prefixes (`i/` index, `w/` worktree, `c/` commit) for HEAD/index diffs,
            // which our path normalizer would leave attached and break coherence with
            // gix's clean paths.
            "--src-prefix=a/".into(),
            "--dst-prefix=b/".into(),
            "-U3".into(),
        ];
        let mut args = base;
        match target {
            DiffTarget::Staged => args.push("--cached".into()),
            DiffTarget::Unstaged => {}
            DiffTarget::WorkingTree | DiffTarget::Untracked => args.push("HEAD".into()),
            DiffTarget::Commit { rev } => {
                // A commit vs its first parent; `git diff <sha>^!` expands to that.
                args.push(format!("{rev}^!"));
            }
            DiffTarget::Range { base, target } | DiffTarget::Branches { base, target } => {
                args.push(base.clone());
                args.push(target.clone());
            }
        }
        args
    }

    /// The CLI fallback diff: resolve SHAs, run `git diff`, and parse it into files/hunks.
    pub fn diff(
        repo_path: &str,
        target: &DiffTarget,
        mode: DiffMode,
    ) -> Result<RepoDiff, DiffError> {
        let git_dir = ensure_repo(repo_path)?;

        // Resolve base/target SHAs for the record (best-effort; None for worktree/index).
        let (base_sha, target_sha) = match target {
            DiffTarget::Commit { rev } => {
                let t = resolve(repo_path, rev)?;
                let base = resolve(repo_path, &format!("{rev}^")).ok();
                (base, Some(t))
            }
            DiffTarget::Range { base, target } | DiffTarget::Branches { base, target } => (
                Some(resolve(repo_path, base)?),
                Some(resolve(repo_path, target)?),
            ),
            DiffTarget::Staged | DiffTarget::WorkingTree | DiffTarget::Untracked => {
                (resolve(repo_path, "HEAD").ok(), None)
            }
            DiffTarget::Unstaged => (None, None),
        };

        let arg_strings = diff_args(target);
        let args: Vec<&str> = arg_strings.iter().map(String::as_str).collect();
        let stdout = git(repo_path, &args)?;
        let mut files = parse_unified(&String::from_utf8_lossy(&stdout));

        // Untracked files are not part of `git diff`; add them for the worktree/untracked
        // surfaces so the CLI path matches gix's file set.
        if matches!(target, DiffTarget::WorkingTree | DiffTarget::Untracked) {
            for entry in super::porcelain::worktree_entries(repo_path, &DiffTarget::Untracked)? {
                let new_bytes =
                    std::fs::read(Path::new(repo_path).join(&entry.path)).unwrap_or_default();
                let binary = looks_binary(&new_bytes);
                files.push(FileDiff {
                    change: ChangeKind::Added,
                    old: None,
                    new: Some(FileSide {
                        path: entry.path.clone(),
                        blob: None,
                        binary,
                    }),
                    binary,
                    hunks: super::hunks_between_blobs(b"", &new_bytes),
                });
            }
        }
        // For the pure-untracked surface, drop tracked-file diffs (keep only additions).
        if matches!(target, DiffTarget::Untracked) {
            files.retain(|f| f.change == ChangeKind::Added && f.old.is_none());
        }

        files.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(RepoDiff {
            repo_path: repo_path.to_string(),
            git_dir,
            mode,
            base_sha,
            target_sha,
            backend: DiffBackend::GitCli,
            files,
        })
    }

    /// Parse `git diff` unified output into [`FileDiff`]s with [`Hunk`]s. Handles the
    /// `diff --git`, `---`/`+++`, `@@` and binary markers; enough for the coherence
    /// tests and the renderer's needs.
    fn parse_unified(text: &str) -> Vec<FileDiff> {
        let mut files = Vec::new();
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            if !line.starts_with("diff --git ") {
                continue;
            }
            let mut old_path = None;
            let mut new_path = None;
            let mut binary = false;
            let mut hunks: Vec<Hunk> = Vec::new();
            let mut change = ChangeKind::Modified;

            while let Some(&peek) = lines.peek() {
                if peek.starts_with("diff --git ") {
                    break;
                }
                let l = lines.next().unwrap();
                if let Some(rest) = l.strip_prefix("--- ") {
                    old_path = normalize_diff_path(rest);
                } else if let Some(rest) = l.strip_prefix("+++ ") {
                    new_path = normalize_diff_path(rest);
                } else if l.starts_with("new file mode") {
                    change = ChangeKind::Added;
                } else if l.starts_with("deleted file mode") {
                    change = ChangeKind::Deleted;
                } else if l.starts_with("Binary files") || l.starts_with("GIT binary patch") {
                    binary = true;
                } else if l.starts_with("@@") {
                    if let Some(header) = parse_hunk_header(l) {
                        let mut hunk = header;
                        while let Some(&hpeek) = lines.peek() {
                            if hpeek.starts_with("@@")
                                || hpeek.starts_with("diff --git ")
                                || hpeek.starts_with("--- ")
                                || hpeek.starts_with("+++ ")
                            {
                                break;
                            }
                            let hl = lines.next().unwrap();
                            if let Some(c) = hl.strip_prefix('+') {
                                hunk.lines.push(DiffLine {
                                    kind: LineKind::Added,
                                    content: c.to_string(),
                                });
                            } else if let Some(c) = hl.strip_prefix('-') {
                                hunk.lines.push(DiffLine {
                                    kind: LineKind::Removed,
                                    content: c.to_string(),
                                });
                            } else if let Some(c) = hl.strip_prefix(' ') {
                                hunk.lines.push(DiffLine {
                                    kind: LineKind::Context,
                                    content: c.to_string(),
                                });
                            } else if hl == "\\ No newline at end of file" {
                                // ignore the no-newline marker
                            } else {
                                break;
                            }
                        }
                        hunks.push(hunk);
                    }
                }
            }

            let path = new_path
                .clone()
                .filter(|p| p != "/dev/null")
                .or_else(|| old_path.clone().filter(|p| p != "/dev/null"))
                .unwrap_or_default();
            let old = (old_path.as_deref() != Some("/dev/null")).then(|| FileSide {
                path: old_path.clone().unwrap_or_else(|| path.clone()),
                blob: None,
                binary,
            });
            let new = (new_path.as_deref() != Some("/dev/null")).then(|| FileSide {
                path: new_path.clone().unwrap_or_else(|| path.clone()),
                blob: None,
                binary,
            });
            files.push(FileDiff {
                change,
                old,
                new,
                binary,
                hunks: if binary { Vec::new() } else { hunks },
            });
        }
        files
    }

    /// Strip the `a/` or `b/` prefix (and any trailing tab-metadata) from a `---`/`+++`
    /// path line; `/dev/null` passes through unchanged.
    fn normalize_diff_path(rest: &str) -> Option<String> {
        let rest = rest.split('\t').next().unwrap_or(rest).trim();
        if rest == "/dev/null" {
            return Some("/dev/null".to_string());
        }
        let stripped = rest
            .strip_prefix("a/")
            .or_else(|| rest.strip_prefix("b/"))
            .unwrap_or(rest);
        Some(stripped.to_string())
    }

    /// Parse a `@@ -old_start,old_len +new_start,new_len @@` header into an empty-lined
    /// [`Hunk`]. A missing `,len` defaults to 1 (Git's convention).
    fn parse_hunk_header(line: &str) -> Option<Hunk> {
        let inner = line.strip_prefix("@@ ")?;
        let end = inner.find(" @@")?;
        let spec = &inner[..end];
        let mut parts = spec.split_whitespace();
        let old = parts.next()?.strip_prefix('-')?;
        let new = parts.next()?.strip_prefix('+')?;
        let (old_start, old_lines) = parse_range(old)?;
        let (new_start, new_lines) = parse_range(new)?;
        Some(Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines: Vec::new(),
        })
    }

    /// Parse a `start,len` (or bare `start`) range component.
    fn parse_range(s: &str) -> Option<(u32, u32)> {
        let mut it = s.split(',');
        let start: u32 = it.next()?.parse().ok()?;
        let len: u32 = it.next().map(|l| l.parse().unwrap_or(1)).unwrap_or(1);
        Some((start, len))
    }
}

/// A convenience for callers that hold a `(workspace, repository)` membership row: build
/// the diff from the ABSOLUTE repo path the membership already computed, so the caller
/// never has to touch a `PathBuf` join or the backend selection.
pub fn diff_member(absolute_path: &Path, target: &DiffTarget) -> Result<RepoDiff, DiffError> {
    diff_repo(&absolute_path.to_string_lossy(), target)
}

/// Read a repository BLOB's bytes by its 40-hex object id — gix PRIMARY, git CLI FALLBACK,
/// the same coherence contract as [`diff_repo`]. This is the blob-by-SHA reader the v2.4
/// comment RE-MAPPING (task `01KX0F6HEBPK34CV49EREHK3Y9`) rides on: a comment anchor records
/// the blob SHA it was captured on, and re-anchoring relocates its snippet inside the NEW
/// side's blob (read here) after a commit/checkout advances the repo. `None` when the id does
/// not resolve to a blob in `repo_path` (e.g. an unknown SHA, or a non-git path).
pub fn read_blob_by_sha(repo_path: &str, sha: &str) -> Option<Vec<u8>> {
    // PRIMARY: gix — open the repo and read the object by id.
    if let Ok(repo) = gix::open(repo_path) {
        if let Ok(id) = gix::ObjectId::from_hex(sha.as_bytes()) {
            if let Ok(obj) = repo.find_object(id) {
                if obj.kind == gix::object::Kind::Blob {
                    return Some(obj.data.clone());
                }
            }
        }
    }
    // FALLBACK: git CLI — `git cat-file blob <sha>` prints the blob bytes to stdout.
    let out = crate::proc_util::command("git")
        .arg("-C")
        .arg(repo_path)
        .args(["cat-file", "blob", sha])
        .output()
        .ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

/// The 40-hex blob id of `path` in the CURRENT `HEAD` tree of `repo_path`, or `None` when
/// the path is absent from HEAD / HEAD is unborn / not a git repo. This is the "where is the
/// file NOW" lookup the comment re-mapping uses to find the fresh blob a stored anchor should
/// relocate into after the repo advances (gix primary, git CLI fallback).
pub fn head_blob_sha(repo_path: &str, path: &str) -> Option<String> {
    // PRIMARY: gix.
    if let Ok(repo) = gix::open(repo_path) {
        if let Ok(head_id) = repo.head_id() {
            if let Ok(obj) = repo.find_object(head_id.detach()) {
                if let Ok(commit) = obj.try_into_commit() {
                    if let Ok(tree) = commit.tree() {
                        if let Ok(Some(entry)) = tree.lookup_entry_by_path(Path::new(path)) {
                            return Some(entry.oid().to_string());
                        }
                    }
                }
            }
        }
    }
    // FALLBACK: git CLI — `git rev-parse HEAD:<path>` yields the blob id at that path.
    let out = crate::proc_util::command("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", &format!("HEAD:{path}")])
        .output()
        .ok()?;
    if out.status.success() {
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!sha.is_empty()).then_some(sha)
    } else {
        None
    }
}

/// Build a [`DiffTarget`] from a `kind` string + optional rev-spec params — the ONE
/// place the sidecar db-command and the MCP tool share so both expose the SAME target
/// vocabulary to the renderer/agent (PDR-0005: one authority for UI + MCP). Returns a
/// readable error message on an unknown kind or a missing required rev-spec.
///
/// Accepted `kind`s (case-insensitive): `working_tree`, `staged`, `unstaged`,
/// `untracked`, `commit` (needs `rev`), `range` / `branches` (need `base` + `target`).
pub fn target_from_params(
    kind: &str,
    rev: Option<&str>,
    base: Option<&str>,
    target: Option<&str>,
) -> Result<DiffTarget, String> {
    match kind.to_ascii_lowercase().as_str() {
        "working_tree" | "worktree" | "working-tree" => Ok(DiffTarget::WorkingTree),
        "staged" | "cached" => Ok(DiffTarget::Staged),
        "unstaged" => Ok(DiffTarget::Unstaged),
        "untracked" => Ok(DiffTarget::Untracked),
        "commit" => rev
            .map(|r| DiffTarget::Commit { rev: r.to_string() })
            .ok_or_else(|| "diff kind 'commit' requires a 'rev' rev-spec".to_string()),
        "range" => match (base, target) {
            (Some(b), Some(t)) => Ok(DiffTarget::Range {
                base: b.to_string(),
                target: t.to_string(),
            }),
            _ => Err("diff kind 'range' requires 'base' and 'target' rev-specs".to_string()),
        },
        "branches" | "branch" => match (base, target) {
            (Some(b), Some(t)) => Ok(DiffTarget::Branches {
                base: b.to_string(),
                target: t.to_string(),
            }),
            _ => Err("diff kind 'branches' requires 'base' and 'target' rev-specs".to_string()),
        },
        other => Err(format!(
            "unknown diff kind '{other}' (expected working_tree | staged | unstaged | \
             untracked | commit | range | branches)"
        )),
    }
}

/// Parse an optional [`DiffMode`] override from a string (`living` / `pinned`), or
/// `None` to use the target's intrinsic default. A readable error on an unknown value.
pub fn mode_from_str(mode: Option<&str>) -> Result<Option<DiffMode>, String> {
    match mode.map(|m| m.to_ascii_lowercase()) {
        None => Ok(None),
        Some(m) if m == "living" => Ok(Some(DiffMode::Living)),
        Some(m) if m == "pinned" => Ok(Some(DiffMode::Pinned)),
        Some(other) => Err(format!(
            "unknown diff mode '{other}' (expected living | pinned)"
        )),
    }
}

#[cfg(test)]
mod tests;
