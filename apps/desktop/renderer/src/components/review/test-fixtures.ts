/**
 * `test-fixtures` — synthetic `RepoDiff` builders for the review tests. They mirror the
 * engine's wire shape (`@/bridge`'s `RepoDiff`) so a test can feed the viewer/hooks REAL-
 * SHAPED data through the bridge fake without a live Git repo — including a deliberately BIG
 * diff for the virtualization test.
 */
import type { DiffMode, FileDiff, Hunk, RepoDiff } from "@/bridge";

/** A one-hunk modified-file diff from `old`/`new` line arrays. */
export function fileDiff(path: string, oldLines: string[], newLines: string[]): FileDiff {
  const lines = [
    ...oldLines.map((content) => ({ kind: "removed" as const, content })),
    ...newLines.map((content) => ({ kind: "added" as const, content })),
  ];
  const hunk: Hunk = {
    old_start: 1,
    old_lines: oldLines.length,
    new_start: 1,
    new_lines: newLines.length,
    lines,
  };
  return {
    change: "modified",
    old: { path, blob: "aaa", binary: false },
    new: { path, blob: "bbb", binary: false },
    binary: false,
    hunks: [hunk],
  };
}

/** Wrap files into a `RepoDiff` for a repo path, with a chosen mode + resolved SHAs. */
export function repoDiff(
  repoPath: string,
  files: FileDiff[],
  opts: Partial<Pick<RepoDiff, "mode" | "base_sha" | "target_sha" | "git_dir">> = {},
): RepoDiff {
  return {
    repo_path: repoPath,
    git_dir: opts.git_dir ?? `${repoPath}/.git`,
    mode: opts.mode ?? "living",
    base_sha: opts.base_sha ?? "0".repeat(40),
    target_sha: opts.target_sha ?? null,
    backend: "gix",
    files,
  };
}

/**
 * A BIG diff: `fileCount` files, each with `linesPerFile` added lines. Used to prove the
 * viewer virtualizes — the total row count is large but the viewer must render only a window.
 */
export function bigRepoDiff(
  repoPath: string,
  fileCount: number,
  linesPerFile: number,
  mode: DiffMode = "living",
): RepoDiff {
  const files: FileDiff[] = [];
  for (let f = 0; f < fileCount; f++) {
    const added = Array.from({ length: linesPerFile }, (_, i) => `const line_${f}_${i} = ${i};`);
    files.push({
      change: "added",
      old: null,
      new: { path: `src/file_${String(f).padStart(4, "0")}.ts`, blob: null, binary: false },
      binary: false,
      hunks: [
        {
          old_start: 0,
          old_lines: 0,
          new_start: 1,
          new_lines: linesPerFile,
          lines: added.map((content) => ({ kind: "added" as const, content })),
        },
      ],
    });
  }
  return repoDiff(repoPath, files, { mode });
}
