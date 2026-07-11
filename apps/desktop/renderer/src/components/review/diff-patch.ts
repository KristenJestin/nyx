/**
 * `diff-patch` — pure adapters that turn the sidecar's `RepoDiff` (the engine's parsed hunks,
 * PDR-0015) into the shapes @pierre/diffs + @pierre/trees RENDER. The libraries only render; the
 * DATA still comes from `nyx-core`'s `repo_diff` bridge — we never re-diff on the renderer.
 *
 * - {@link fileToPatch} serialises ONE `FileDiff`'s hunks into a standard unified-diff patch
 *   string, which `@pierre/diffs` `PatchDiff` parses + highlights (a faithful round-trip of the
 *   engine's hunks — no full-file contents required, which the bridge does not carry).
 * - {@link diffGitStatus} projects the changed files into `@pierre/trees` `GitStatusEntry[]` so
 *   the file tree paints per-file status glyphs.
 */
import type { FileDiff, LineKind, RepoDiff } from "@/bridge";
import { getSingularPatch } from "@pierre/diffs";
import type { FileDiffMetadata } from "@pierre/diffs";
import type { GitStatus, GitStatusEntry } from "@pierre/trees";

import { filePath } from "./review-types";

/** The unified-diff sign for a line role. */
function sign(kind: LineKind): string {
  switch (kind) {
    case "added":
      return "+";
    case "removed":
      return "-";
    case "context":
      return " ";
  }
}

/**
 * Serialise ONE file's diff into a unified-diff patch string (the `git diff` text `PatchDiff`
 * parses). Added files diff against `/dev/null` on the old side, deleted files against `/dev/null`
 * on the new side; a hunk header carries the engine's 1-based `(old_start,old_lines)` /
 * `(new_start,new_lines)` coordinates verbatim. A binary file has no textual hunks — the caller
 * renders a "binary" note instead of a patch.
 */
export function fileToPatch(file: FileDiff): string {
  const oldPath = file.old?.path ?? file.new?.path ?? "file";
  const newPath = file.new?.path ?? file.old?.path ?? "file";
  const oldHeader = file.old ? `a/${oldPath}` : "/dev/null";
  const newHeader = file.new ? `b/${newPath}` : "/dev/null";

  const lines: string[] = [
    `diff --git a/${oldPath} b/${newPath}`,
    `--- ${oldHeader}`,
    `+++ ${newHeader}`,
  ];

  for (const hunk of file.hunks) {
    lines.push(`@@ -${hunk.old_start},${hunk.old_lines} +${hunk.new_start},${hunk.new_lines} @@`);
    for (const line of hunk.lines) {
      lines.push(`${sign(line.kind)}${line.content}`);
    }
  }

  // A trailing newline keeps the last line a complete diff line for the parser.
  return `${lines.join("\n")}\n`;
}

/** A tiny, stable djb2 hash of a string — the content-sensitive fallback for {@link fileCacheKey}. */
function hashString(input: string): string {
  let hash = 5381;
  for (let i = 0; i < input.length; i++) hash = ((hash << 5) + hash + input.charCodeAt(i)) | 0;
  // >>> 0 keeps it an unsigned 32-bit int; base36 keeps the key short.
  return (hash >>> 0).toString(36);
}

/**
 * A STABLE, content-sensitive cache key for a file's parsed diff (Finding 55). `@pierre/diffs`'
 * worker pool keys its off-thread syntax-highlight cache on `FileDiffMetadata.cacheKey`: a key that
 * stays the same across renders lets it SKIP re-tokenizing an already-highlighted file, and one that
 * changes only when the content changes re-tokenizes exactly when needed. The blob SHAs are ideal
 * (they change iff the file's bytes change); working-tree diffs may lack blobs, so we fall back to a
 * hash of the serialised patch (still content-sensitive). `path` scopes it so two blob-less files
 * never collide in the shared pool cache.
 */
export function fileCacheKey(file: FileDiff, patch: string): string {
  const path = filePath(file);
  const oldBlob = file.old?.blob;
  const newBlob = file.new?.blob;
  if (oldBlob || newBlob) return `${path}::${oldBlob ?? ""}::${newBlob ?? ""}`;
  return `${path}::${hashString(patch)}`;
}

/**
 * Parse ONE file's diff into the `@pierre/diffs` `FileDiffMetadata` (via {@link fileToPatch} +
 * `getSingularPatch`) and stamp it with a STABLE {@link fileCacheKey} (Finding 55). `DiffView`
 * memoises the result per file so the parsed object's IDENTITY is stable across actions — the
 * library's `render()` then early-returns instead of re-parsing/re-tokenizing, and the worker pool
 * serves the highlight from its cache keyed on `cacheKey`. Feeding this to `<FileDiff>` (rather than
 * `<PatchDiff patch={string}>`, which re-parses a fresh string each render) is what confines a
 * review action to re-rendering ONLY the file it touches.
 */
export function fileToDiffMetadata(file: FileDiff): FileDiffMetadata {
  const patch = fileToPatch(file);
  const meta = getSingularPatch(patch);
  meta.cacheKey = fileCacheKey(file, patch);
  return meta;
}

/** The `@pierre/trees` git-status glyph for an engine change kind. */
function changeGitStatus(change: FileDiff["change"]): GitStatus {
  switch (change) {
    case "added":
      return "added";
    case "deleted":
      return "deleted";
    case "modified":
      return "modified";
  }
}

/** Project a `RepoDiff`'s changed files into `@pierre/trees` git-status entries (per-file glyphs). */
export function diffGitStatus(diff: RepoDiff | undefined): GitStatusEntry[] {
  if (!diff) return [];
  return diff.files.map((file) => ({
    path: filePath(file),
    status: changeGitStatus(file.change),
  }));
}

/**
 * The text of a diff line at `(path, side, line)` — the SNIPPET the backend captures with a new
 * anchored comment so it can re-map the anchor across commits (PDR-0012). Looks the line up in the
 * engine's hunks by its 1-based old/new coordinate; `null` when the coordinate is a context/other
 * side (no anchorable content there).
 */
export function lineContentAt(
  diff: RepoDiff | undefined,
  path: string,
  side: "old" | "new",
  line: number,
): string | null {
  if (!diff) return null;
  const file = diff.files.find((f) => filePath(f) === path);
  if (!file) return null;
  for (const hunk of file.hunks) {
    let oldNo = hunk.old_start;
    let newNo = hunk.new_start;
    for (const l of hunk.lines) {
      const onOld = l.kind !== "added";
      const onNew = l.kind !== "removed";
      if (side === "old" && onOld && oldNo === line) return l.content;
      if (side === "new" && onNew && newNo === line) return l.content;
      if (onOld) oldNo++;
      if (onNew) newNo++;
    }
  }
  return null;
}

/** The blob SHA of a file on a diff side (helps the backend re-map). `null` when absent. */
export function diffFileBlob(
  diff: RepoDiff | undefined,
  path: string,
  side: "old" | "new",
): string | null {
  const file = diff?.files.find((f) => filePath(f) === path);
  return (side === "new" ? file?.new?.blob : file?.old?.blob) ?? null;
}
