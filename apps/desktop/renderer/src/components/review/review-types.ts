/**
 * `review-types` — the view-model shapes of the local review SCREEN (phase 3 of v2.4).
 *
 * The review is a VUE DÉRIVÉE, not a stored entity (PDR-0015): the screen aggregates one
 * derived `RepoDiff` PER `(workspace, repository)` pair (a repo TAB), each carrying that
 * repo's OWN Git identity so two repos' SHAs/blobs are never mixed. These types are the
 * renderer-side glue between the bridge wire shapes (`RepoDiff` / `RepoGitView` from
 * `@/bridge`) and the components — they hold NO backend logic (the engine is phase 1).
 */
import type { ChangeKind, DiffKind, DiffMode, FileDiff, RepoDiff } from "@/bridge";

/**
 * ONE `(workspace, repository)` pair the review aggregates — a repo TAB. The identity is
 * the (workspaceId, repositoryId) couple; everything else (name, absolute path, branch) is
 * carried so a tab never reaches into another tab's repo. Never share a `RepoDiff` between
 * two tabs: each holds its own repo's diff (multi-repo identity isolation, PDR-0015).
 */
export interface ReviewRepo {
  /** The workspace this repo belongs to (half of the Git-identity key). */
  readonly workspaceId: string;
  /** The logical repository id (the other half of the identity key). */
  readonly repositoryId: string;
  /** The repository's logical name (the tab label). */
  readonly name: string;
  /** The workspace's display name — disambiguates same-named repos across workspaces. */
  readonly workspaceName: string;
  /** The repo's absolute on-disk path in this workspace (identity anchor for the user). */
  readonly absolutePath: string;
  /** The current HEAD branch, when known (shown in the header range line). */
  readonly branch: string | null;
  /** How many files Git reports changed in the working tree (the tab's count badge). */
  readonly changedFileCount: number;
}

/** A stable key for a repo tab that can NEVER collide across workspaces/repos. */
export function reviewRepoKey(repo: Pick<ReviewRepo, "workspaceId" | "repositoryId">): string {
  return `${repo.workspaceId}::${repo.repositoryId}`;
}

/**
 * WHAT surface of a repo the review shows, plus the LIVING/PINNED variety (PDR-0015).
 * `kind` maps to the engine's `DiffKind`; `rev`/`base`/`target` are the rev-specs a kind
 * needs; `mode` is the explicit living/pinned override the user picked (or `undefined` to
 * let the target's intrinsic default apply — worktree/branches are living, commit/range
 * are pinned by construction).
 */
export interface ReviewTarget {
  readonly kind: DiffKind;
  readonly rev?: string;
  readonly base?: string;
  readonly target?: string;
  readonly mode?: DiffMode;
}

/** The default surface a repo opens on: its full working tree (the living "what changed"). */
export const DEFAULT_REVIEW_TARGET: ReviewTarget = { kind: "working_tree" };

/**
 * Whether a target FOLLOWS the current repo state (living) — so a `repos://changed` push
 * must re-pull it — or is FROZEN on a SHA (pinned). Mirrors the engine's `default_mode`:
 * the worktree/index/untracked surfaces and a branch comparison are living; a commit or an
 * explicit range is pinned. An explicit `mode` override wins.
 */
export function isLivingTarget(target: ReviewTarget): boolean {
  if (target.mode) return target.mode === "living";
  switch (target.kind) {
    case "working_tree":
    case "staged":
    case "unstaged":
    case "untracked":
    case "branches":
      return true;
    case "commit":
    case "range":
      return false;
  }
}

/**
 * Whether `target`'s REQUIRED rev-specs are all present, so `repo_diff` can be invoked without
 * the backend rejecting it (Finding 43). Mirrors `git_diff::target_from_params`: a `commit` needs
 * a `rev`; a `range`/`branches` needs BOTH a `base` and a `target`; the working-tree family needs
 * nothing. An empty / whitespace-only string counts as MISSING — so selecting the Commit / Range /
 * Branches surface does NOT fire a throwing `repo_diff` before the user has typed the rev-spec(s).
 */
export function targetReady(target: ReviewTarget): boolean {
  const has = (s: string | undefined): boolean => s != null && s.trim().length > 0;
  switch (target.kind) {
    case "commit":
      return has(target.rev);
    case "range":
    case "branches":
      return has(target.base) && has(target.target);
    default:
      return true;
  }
}

/** The additions/deletions counts of a file diff — folded from its hunks' line roles. */
export interface FileStat {
  readonly additions: number;
  readonly deletions: number;
}

/** Fold a `FileDiff`'s hunks into +/- counts for the file-section badge. */
export function fileStat(file: FileDiff): FileStat {
  let additions = 0;
  let deletions = 0;
  for (const hunk of file.hunks) {
    for (const line of hunk.lines) {
      if (line.kind === "added") additions++;
      else if (line.kind === "removed") deletions++;
    }
  }
  return { additions, deletions };
}

/** The path the renderer keys a file on: the NEW side when present, else the OLD side. */
export function filePath(file: FileDiff): string {
  return file.new?.path ?? file.old?.path ?? "";
}

/** All changed paths of a diff, in the engine's deterministic (path-sorted) order. */
export function diffPaths(diff: RepoDiff | undefined): string[] {
  return diff ? diff.files.map(filePath) : [];
}

/**
 * Make a raw diff path list SAFE to feed to `@pierre/trees` `useFileTree`. The library builds a
 * path store where a name is EITHER a file OR a directory: a path that is BOTH a file (`src`) and
 * the prefix of a deeper path (`src/anchor.rs`) throws "Path collides with an existing file while
 * creating directory" at construction — synchronously in `useState(() => new FileTree(...))` — which
 * the panel's error boundary catches, blanking the whole tree (Finding 55). Our gix tree diff used
 * to emit a changed DIRECTORY as a file entry (fixed at the engine root); this is the renderer-side
 * belt-and-suspenders so NO backend path shape can ever crash the tree again. It drops blank paths,
 * de-duplicates, and removes any path that is an ANCESTOR directory of another path — keeping the
 * real leaf files. Order is preserved (the engine already sorts).
 */
export function sanitizeTreePaths(paths: readonly string[]): string[] {
  const seen = new Set<string>();
  const unique: string[] = [];
  for (const raw of paths) {
    const path = raw.trim();
    if (path.length === 0 || seen.has(path)) continue;
    seen.add(path);
    unique.push(path);
  }
  // Every ancestor-directory prefix present in the set (e.g. `a`, `a/b` for `a/b/c.ts`).
  const ancestorPrefixes = new Set<string>();
  for (const path of unique) {
    const segments = path.split("/");
    for (let i = 1; i < segments.length; i += 1) {
      ancestorPrefixes.add(segments.slice(0, i).join("/"));
    }
  }
  // Drop any entry that is itself a directory prefix of another entry (a directory-as-file).
  return unique.filter((path) => !ancestorPrefixes.has(path));
}

/** A short human label for a change kind (the file-section status badge text). */
export function changeLabel(change: ChangeKind): string {
  switch (change) {
    case "added":
      return "Added";
    case "deleted":
      return "Deleted";
    case "modified":
      return "Modified";
  }
}
