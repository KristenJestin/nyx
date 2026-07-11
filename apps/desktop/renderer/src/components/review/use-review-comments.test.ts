import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { bridgeFake } from "@/bridge/test-harness";
import { createQueryWrapper } from "@/lib/test-query";

import type { ReviewRepo } from "./review-types";
import { useReviewComments } from "./use-review-comments";

/**
 * Finding 62 — the review's mutation ACTIONS must be referentially STABLE across re-renders. TanStack's
 * `useMutation` returns a FRESH result object on every render, so an action wired to `[mutation]` would
 * change identity on every render; the review page's `commentActions` (built from resolve/reopen/reply/
 * promote) would then change every render and break `React.memo(FileSection)` for EVERY file — so a
 * single resolve (which re-renders the hook several times) reflowed the whole diff. The hook reads the
 * latest `mutateAsync` from a ref so the actions are created ONCE.
 */

const REPO: ReviewRepo = {
  workspaceId: "w1",
  repositoryId: "r1",
  name: "app",
  workspaceName: "root",
  absolutePath: "/repo",
  branch: "feature",
  changedFileCount: 0,
};

describe("use-review-comments — mutation action stability (Finding 62)", () => {
  it("keeps resolve / reopen / reply / postGlobal / postAnchored stable across re-renders", () => {
    const fake = bridgeFake();
    fake.on("review_comment_list", () => []);
    const { wrapper } = createQueryWrapper();

    const { result, rerender } = renderHook(() => useReviewComments(REPO, "spec1"), { wrapper });
    const first = {
      resolve: result.current.resolve,
      reopen: result.current.reopen,
      reply: result.current.reply,
      postGlobal: result.current.postGlobal,
      postAnchored: result.current.postAnchored,
    };

    // A plain re-render re-invokes `useMutation` (a NEW result object each time). The actions must NOT
    // change identity — else `commentActions` churns and every `FileSection` re-renders on a mutation.
    rerender();
    rerender();

    expect(result.current.resolve).toBe(first.resolve);
    expect(result.current.reopen).toBe(first.reopen);
    expect(result.current.reply).toBe(first.reply);
    expect(result.current.postGlobal).toBe(first.postGlobal);
    expect(result.current.postAnchored).toBe(first.postAnchored);
  });
});
