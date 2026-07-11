import { describe, expect, it } from "vitest";

import type { ReviewComment } from "@/bridge";

import {
  anchorKey,
  anchorsByLine,
  buildThreads,
  groupThreadsByFile,
  partitionThreads,
} from "./review-threads";

/** A `ReviewComment` factory with sensible defaults (a human, open, global root). */
const c = (over: Partial<ReviewComment>): ReviewComment => ({
  id: "c1",
  workspace_id: "ws",
  repository_id: "r1",
  spec_id: null,
  parent_id: null,
  author_type: "human",
  author_name: "You",
  file_path: null,
  side: null,
  line_start: null,
  line_end: null,
  blob: null,
  snippet: null,
  context: null,
  outdated: false,
  body: "b",
  status: "open",
  resolved_by_type: null,
  created_at: 1,
  ...over,
});

describe("buildThreads", () => {
  it("groups each root with its replies (auto-threaded by parent_id)", () => {
    const comments = [
      c({ id: "c1", file_path: "a.ts" }),
      c({ id: "c2", parent_id: "c1", author_type: "agent", author_name: "nyx-agent" }),
      c({ id: "c3", repository_id: "r2" }),
    ];
    const threads = buildThreads("r1", comments);
    expect(threads).toHaveLength(1);
    expect(threads[0]!.replies.map((r) => r.id)).toEqual(["c2"]);
    expect(threads[0]!.replies[0]!.author_type).toBe("agent");
  });

  it("ignores comments of another repo", () => {
    expect(buildThreads("r1", [c({ id: "c3", repository_id: "r2" })])).toHaveLength(0);
  });
});

describe("partitionThreads", () => {
  it("splits anchored (has file_path) from global", () => {
    const threads = buildThreads("r1", [
      c({ id: "c1", file_path: "a.ts", side: "new", line_start: 2 }),
      c({ id: "c2" }),
    ]);
    const { anchored, global } = partitionThreads(threads);
    expect(anchored.map((t) => t.root.id)).toEqual(["c1"]);
    expect(global.map((t) => t.root.id)).toEqual(["c2"]);
  });
});

describe("groupThreadsByFile", () => {
  it("buckets anchored threads by file_path", () => {
    const threads = buildThreads("r1", [
      c({ id: "c1", file_path: "a.ts", side: "new", line_start: 1 }),
      c({ id: "c2", file_path: "a.ts", side: "new", line_start: 5 }),
      c({ id: "c3", file_path: "b.ts", side: "old", line_start: 3 }),
    ]);
    const byFile = groupThreadsByFile(threads);
    expect(byFile.get("a.ts")).toHaveLength(2);
    expect(byFile.get("b.ts")).toHaveLength(1);
  });
});

describe("anchorsByLine", () => {
  it("indexes anchored roots by (file, side, line_start) — the diff's O(1) lookup", () => {
    const threads = buildThreads("r1", [
      c({ id: "c1", file_path: "a.ts", side: "new", line_start: 4, line_end: 6 }),
      c({ id: "c2" }), // global — not indexed
    ]);
    const byLine = anchorsByLine(threads);
    expect(byLine.get(anchorKey("a.ts", "new", 4))?.[0]!.root.id).toBe("c1");
    expect(byLine.size).toBe(1);
  });
});
