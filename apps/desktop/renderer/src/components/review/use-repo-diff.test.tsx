import { renderHook, waitFor } from "@testing-library/react";
import { bridgeFake, emit } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import { createQueryWrapper } from "@/lib/test-query";
import type { ReviewRepo, ReviewTarget } from "./review-types";
import { repoDiff, fileDiff } from "./test-fixtures";
import { useRepoDiff } from "./use-repo-diff";

/**
 * `use-repo-diff` on TanStack Query (PDR-0029). The wrapper mounts the REAL `<ReviewLiveSync>`, so a
 * `emit("repos://changed")` drives the SAME single global invalidation the app uses: a LIVING diff
 * re-pulls, a PINNED (frozen) diff does NOT. The per-hook `repos://changed` subscription is gone;
 * only the `repo_watch`/`repo_unwatch` lifecycle stays in the hook (living targets only).
 */

const REPO: ReviewRepo = {
  workspaceId: "w1",
  repositoryId: "r1",
  name: "app",
  workspaceName: "root",
  absolutePath: "/repo",
  branch: "feature",
  changedFileCount: 1,
};

describe("use-repo-diff (living/pinned + real-time refresh via TanStack Query)", () => {
  it("loads the diff off `repo_diff` for the (workspace, repository) + target", async () => {
    const fake = bridgeFake();
    fake.on("repo_diff", () => repoDiff("/repo", [fileDiff("a.ts", ["x"], ["y"])]));
    const { wrapper } = createQueryWrapper();
    const target: ReviewTarget = { kind: "working_tree" };

    const { result } = renderHook(() => useRepoDiff(REPO, target), { wrapper });
    await waitFor(() => expect(result.current.diff).toBeDefined());

    const call = fake.calls.find((c) => c.command === "repo_diff");
    expect(call?.args).toMatchObject({
      workspaceId: "w1",
      repositoryId: "r1",
      kind: "working_tree",
    });
    expect(result.current.diff?.files).toHaveLength(1);
  });

  it("LIVING target: watches the repo and RE-PULLS on `repos://changed` (no polling)", async () => {
    const fake = bridgeFake();
    let calls = 0;
    fake.on("repo_diff", () => {
      calls++;
      return repoDiff("/repo", [fileDiff("a.ts", ["x"], [`y${calls}`])], { mode: "living" });
    });
    fake.on("repo_watch", () => null);
    fake.on("repo_unwatch", () => null);
    const { wrapper } = createQueryWrapper();

    renderHook(() => useRepoDiff(REPO, { kind: "working_tree" }), { wrapper });
    await waitFor(() => expect(calls).toBe(1));
    // It registered a watch, and the global `<ReviewLiveSync>` subscribed to `repos://changed`.
    expect(fake.calls.some((c) => c.command === "repo_watch")).toBe(true);
    await waitFor(() => expect(fake.subscriberCount("repos://changed")).toBe(1));

    // A backend push re-pulls the living diff — event-driven, not a timer.
    await emit("repos://changed", undefined);
    await waitFor(() => expect(calls).toBe(2));
  });

  it("PINNED target: no watch, and a push does NOT re-pull — the diff is FROZEN", async () => {
    const fake = bridgeFake();
    let calls = 0;
    fake.on("repo_diff", () => {
      calls++;
      return repoDiff("/repo", [fileDiff("a.ts", ["x"], ["y"])], { mode: "pinned" });
    });
    const { wrapper } = createQueryWrapper();

    renderHook(() => useRepoDiff(REPO, { kind: "commit", rev: "abc123" }), { wrapper });
    await waitFor(() => expect(calls).toBe(1));
    // No watch registered for a pinned target, and the global sync IS subscribed.
    expect(fake.calls.some((c) => c.command === "repo_watch")).toBe(false);
    await waitFor(() => expect(fake.subscriberCount("repos://changed")).toBe(1));

    // A push does NOT re-pull a pinned diff (its query is `meta.frozen` → skipped by the sync).
    await emit("repos://changed", undefined);
    await new Promise((r) => setTimeout(r, 20));
    expect(calls).toBe(1);
  });

  it("releases the watch on unmount (StrictMode-safe)", async () => {
    const fake = bridgeFake();
    fake.on("repo_diff", () => repoDiff("/repo", []));
    fake.on("repo_watch", () => null);
    fake.on("repo_unwatch", () => null);
    const { wrapper } = createQueryWrapper();

    const { unmount } = renderHook(() => useRepoDiff(REPO, { kind: "working_tree" }), { wrapper });
    await waitFor(() => expect(fake.calls.some((c) => c.command === "repo_watch")).toBe(true));
    unmount();
    await waitFor(() => expect(fake.calls.some((c) => c.command === "repo_unwatch")).toBe(true));
  });

  it("surfaces a backend error without throwing", async () => {
    const fake = bridgeFake();
    fake.on("repo_diff", () => {
      throw new Error("not a git work tree");
    });
    const { wrapper } = createQueryWrapper();
    const { result } = renderHook(() => useRepoDiff(REPO, { kind: "working_tree" }), { wrapper });
    await waitFor(() => expect(result.current.error).toContain("not a git work tree"));
    expect(result.current.diff).toBeUndefined();
  });

  // FINDING 43 — selecting a Commit / Range / Branches surface must NOT fire `repo_diff` before the
  // rev-spec(s) are entered (that threw a raw backend error). The hook GUARDS on readiness and
  // exposes `awaitingInput` so the UI prompts for the fields instead (`enabled: false`).
  it("COMMIT with no rev: does NOT call repo_diff and flags awaitingInput (no throw)", async () => {
    const fake = bridgeFake();
    fake.on("repo_diff", () => repoDiff("/repo", [fileDiff("a.ts", ["x"], ["y"])]));
    const { wrapper } = createQueryWrapper();

    const { result } = renderHook(() => useRepoDiff(REPO, { kind: "commit" }), { wrapper });
    await new Promise((r) => setTimeout(r, 0));
    expect(result.current.awaitingInput).toBe(true);
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
    expect(result.current.diff).toBeUndefined();
    expect(fake.calls.some((c) => c.command === "repo_diff")).toBe(false);
  });

  it("RANGE with only a base: still awaitingInput until BOTH base and target are set", async () => {
    const fake = bridgeFake();
    fake.on("repo_diff", () => repoDiff("/repo", [fileDiff("a.ts", ["x"], ["y"])]));
    const { wrapper } = createQueryWrapper();
    const { result } = renderHook(
      () => useRepoDiff(REPO, { kind: "range", base: "main", target: "  " }),
      { wrapper },
    );
    await new Promise((r) => setTimeout(r, 0));
    expect(result.current.awaitingInput).toBe(true);
    expect(fake.calls.some((c) => c.command === "repo_diff")).toBe(false);
  });

  it("COMMIT loads once the rev is entered (the guard lifts)", async () => {
    const fake = bridgeFake();
    let calls = 0;
    fake.on("repo_diff", () => {
      calls++;
      return repoDiff("/repo", [fileDiff("a.ts", ["x"], ["y"])], { mode: "pinned" });
    });
    const { wrapper } = createQueryWrapper();

    const { result, rerender } = renderHook(({ target }) => useRepoDiff(REPO, target), {
      initialProps: { target: { kind: "commit" } as ReviewTarget },
      wrapper,
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(calls).toBe(0);
    expect(result.current.awaitingInput).toBe(true);

    rerender({ target: { kind: "commit", rev: "abc123" } });
    await waitFor(() => expect(result.current.diff).toBeDefined());
    expect(result.current.awaitingInput).toBe(false);
    expect(calls).toBe(1);
    const call = fake.calls.find((c) => c.command === "repo_diff");
    expect(call?.args).toMatchObject({ kind: "commit", rev: "abc123" });
  });
});
