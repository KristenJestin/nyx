import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { bridgeFake } from "@/bridge/test-harness";
import { createQueryWrapper } from "@/lib/test-query";

import type { ReviewRepo } from "./review-types";
import { useReviewTasks } from "./use-review-tasks";

/**
 * Finding 62 — the task ACTIONS (`promote` feeds the review page's `commentActions`) must be
 * referentially STABLE across re-renders, for the same reason as `use-review-comments`: TanStack's
 * `useMutation` returns a fresh object each render, so a `[taskMutation]`-keyed action would churn and
 * break `React.memo(FileSection)` on every render (reflowing the diff on any promote/validate). The
 * hook reads the latest `mutateAsync` from refs so the actions are created once.
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

describe("use-review-tasks — action stability (Finding 62)", () => {
  it("keeps promote / validate / markRead stable across re-renders", () => {
    const fake = bridgeFake();
    fake.on("review_task_list", () => []);
    fake.on("review_unread", () => ({ workspace_id: "w1", repository_id: "r1", unread: 0 }));
    const { wrapper } = createQueryWrapper();

    const { result, rerender } = renderHook(() => useReviewTasks(REPO, "spec1"), { wrapper });
    const first = {
      promote: result.current.promote,
      validate: result.current.validate,
      markRead: result.current.markRead,
    };

    rerender();
    rerender();

    expect(result.current.promote).toBe(first.promote);
    expect(result.current.validate).toBe(first.validate);
    expect(result.current.markRead).toBe(first.markRead);
  });
});
