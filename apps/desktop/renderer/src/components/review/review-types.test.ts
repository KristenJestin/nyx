import { describe, expect, it } from "vitest";

import {
  fileStat,
  isLivingTarget,
  reviewRepoKey,
  sanitizeTreePaths,
  type ReviewTarget,
} from "./review-types";
import { fileDiff } from "./test-fixtures";

describe("review-types", () => {
  it("living vs pinned follows the target kind (and an explicit override wins)", () => {
    expect(isLivingTarget({ kind: "working_tree" })).toBe(true);
    expect(isLivingTarget({ kind: "branches" })).toBe(true);
    expect(isLivingTarget({ kind: "commit", rev: "abc" })).toBe(false);
    expect(isLivingTarget({ kind: "range", base: "a", target: "b" })).toBe(false);
    // Override: pin a living surface, or make a pinned one living.
    expect(isLivingTarget({ kind: "working_tree", mode: "pinned" })).toBe(false);
    const pinnedRangeToLiving: ReviewTarget = {
      kind: "range",
      base: "a",
      target: "b",
      mode: "living",
    };
    expect(isLivingTarget(pinnedRangeToLiving)).toBe(true);
  });

  it("a repo key is unique per (workspace, repository) — never mixes identities", () => {
    const k1 = reviewRepoKey({ workspaceId: "w1", repositoryId: "r" });
    const k2 = reviewRepoKey({ workspaceId: "w2", repositoryId: "r" });
    // Same repository id in two workspaces yields DISTINCT keys.
    expect(k1).not.toBe(k2);
  });

  it("folds a file's hunk lines into +/- counts", () => {
    const stat = fileStat(fileDiff("a.ts", ["x", "y"], ["z"]));
    expect(stat).toEqual({ additions: 1, deletions: 2 });
  });

  // Finding 55 — a path that is BOTH a file and a directory prefix crashes `@pierre/trees`.
  it("sanitizeTreePaths drops directory-as-file entries, blanks, and duplicates", () => {
    // The exact shape the gix tree diff used to emit: the `src` directory as a file entry
    // alongside its children.
    expect(sanitizeTreePaths(["src", "src/anchor.rs", "src/mod_0.rs"])).toEqual([
      "src/anchor.rs",
      "src/mod_0.rs",
    ]);
    // Nested directory-as-file entries at multiple depths are all removed.
    expect(sanitizeTreePaths(["a", "a/b", "a/b/c.ts"])).toEqual(["a/b/c.ts"]);
    // Blank/whitespace paths and duplicates are dropped; order is preserved.
    expect(sanitizeTreePaths(["", "  ", "x.ts", "x.ts", "y.ts"])).toEqual(["x.ts", "y.ts"]);
    // A clean file-only list is unchanged.
    expect(sanitizeTreePaths(["src/a.ts", "src/b.ts", "README.md"])).toEqual([
      "src/a.ts",
      "src/b.ts",
      "README.md",
    ]);
  });
});
