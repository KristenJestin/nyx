/**
 * `review-threads` — pure DERIVED views over the flat `ReviewComment` feed. No React, no DOM:
 * this is the thread-model's unit-test surface, rewritten against the real `@/bridge`
 * `ReviewComment` (a faithful trace of the prototype's `threads.ts`, PDR-0012/0015).
 *
 * The backend stores ONE auto-threaded table (a comment's `parent_id` = its thread root, null
 * for a root). These helpers fold that flat list into thread objects and bucket the anchored
 * ones by file — exactly what the panel + the in-diff anchors render.
 */
import type { ReviewComment } from "@/bridge";

/** A thread: its root comment + its replies (oldest-first), as the panel/diff render it. */
export interface CommentThread {
  readonly root: ReviewComment;
  readonly replies: readonly ReviewComment[];
}

/**
 * Fold the flat feed of a repo into threads: each ROOT (`parent_id == null`) with its replies
 * (comments whose `parent_id` is that root's id), preserving the feed's oldest-first order.
 * Comments of another repo are ignored (the feed is already scoped, but this stays robust).
 */
export function buildThreads(
  repositoryId: string,
  comments: readonly ReviewComment[],
): CommentThread[] {
  const roots = comments.filter((c) => c.repository_id === repositoryId && c.parent_id == null);
  return roots.map((root) => ({
    root,
    replies: comments.filter((c) => c.parent_id === root.id),
  }));
}

/** Split threads into the ANCHORED ones (root has a `file_path`) and the GLOBAL ones. */
export function partitionThreads(threads: readonly CommentThread[]): {
  anchored: CommentThread[];
  global: CommentThread[];
} {
  const anchored: CommentThread[] = [];
  const global: CommentThread[] = [];
  for (const t of threads) {
    if (t.root.file_path != null) anchored.push(t);
    else global.push(t);
  }
  return { anchored, global };
}

/** Group anchored threads by their root's `file_path` (insertion order preserved). */
export function groupThreadsByFile(
  threads: readonly CommentThread[],
): Map<string, CommentThread[]> {
  const byFile = new Map<string, CommentThread[]>();
  for (const t of threads) {
    const file = t.root.file_path ?? "";
    const list = byFile.get(file) ?? [];
    list.push(t);
    byFile.set(file, list);
  }
  return byFile;
}

/** A stable anchor key `(file, side, line)` — the identity a line row and a thread share. */
export function anchorKey(filePath: string, side: string, line: number): string {
  return `${filePath}::${side}::${line}`;
}

/**
 * Index the ANCHORED thread roots by their `(file, side, line_start)` anchor key, so the diff
 * viewer can look up "is there a comment on THIS line" in O(1) while it windows rows. Only
 * roots with a complete anchor participate; a range anchors on its `line_start`.
 */
export function anchorsByLine(threads: readonly CommentThread[]): Map<string, CommentThread[]> {
  const byLine = new Map<string, CommentThread[]>();
  for (const t of threads) {
    const { file_path, side, line_start } = t.root;
    if (file_path == null || side == null || line_start == null) continue;
    const key = anchorKey(file_path, side, line_start);
    const list = byLine.get(key) ?? [];
    list.push(t);
    byLine.set(key, list);
  }
  return byLine;
}
