import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useEffect } from "react";

import { errorText, nyxBridge } from "@/bridge";
import type { RepoDiff } from "@/bridge";
import { reviewKeys } from "@/lib/query";

import { isLivingTarget, targetReady, type ReviewRepo, type ReviewTarget } from "./review-types";

/**
 * `use-repo-diff` — the LIVE derived diff of ONE `(workspace, repository)` for a review
 * target, on TanStack Query (PDR-0029). It loads the `RepoDiff` off the phase-1 engine via the
 * `repo_diff` db-command (the SAME authority the MCP `repo_diff` tool serves, PDR-0005), and — for
 * a LIVING target only — WATCHES the repo (`repo_watch`) so a work-tree/ref change PUSHES
 * `repos://changed`, on which the single global `<ReviewLiveSync>` invalidates this query. That
 * re-pull-on-push is the "sans polling" contract (Q-015): the diff moves live because the backend
 * TOLD it, not on a timer. A PINNED target (a commit / range / an explicit `mode:"pinned"`) is
 * FROZEN — no watch, and its query is tagged `meta: { frozen: true }` + `staleTime: Infinity` so the
 * global invalidation SKIPS it (fetched once, never re-pulled).
 *
 * ## The flash fix is HERE (PDR-0029)
 * React Query's default STRUCTURAL SHARING returns the PREVIOUS `RepoDiff` reference whenever a
 * re-pull yields deep-equal content. A comment/task mutation broadcasts `repos://changed` → this
 * living query re-fetches → identical content → SAME `diff` identity → `DiffView`'s `patchByFile`
 * memo stays stable → `React.memo(FileSection)` bails → `@pierre/diffs` early-returns instead of
 * reconstructing the diff DOM. The "depop/pop" is killed at the source. `placeholderData:
 * keepPreviousData` keeps the last diff on screen while a repo/target switch loads (no empty flash).
 *
 * ## Multi-repo identity isolation
 * The query key is `(workspaceId, repositoryId, targetKey)`; the returned `RepoDiff` carries that
 * repo's OWN `git_dir` + resolved SHAs, so an aggregating screen can hold several of these side by
 * side WITHOUT mixing Git identities (PDR-0015). Switching the active repo reads a fresh cache entry
 * keyed on the new pair — never a shared diff.
 */

export interface UseRepoDiff {
  /** The current diff, or `undefined` until the first load resolves / on a load failure. */
  readonly diff: RepoDiff | undefined;
  /** `true` while the FIRST load for the current key is in flight (not on a live re-pull). */
  readonly loading: boolean;
  /** The last load error message, or `null`. Cleared on a successful (re-)load. */
  readonly error: string | null;
  /**
   * `true` when the selected surface still NEEDS a rev-spec the user has not entered yet (a
   * `commit` without a `rev`, a `range`/`branches` without `base`+`target`, Finding 43). In this
   * state the hook does NOT call `repo_diff` (which would throw a raw backend error); the UI shows
   * a "fill in the fields" prompt instead. Never `true` for the working-tree family.
   */
  readonly awaitingInput: boolean;
  /** Force a re-pull on demand (the same load the live push triggers). */
  readonly refresh: () => Promise<void>;
}

/** A stable string key for the (repo, target) couple so the query re-keys only on a real change. */
function targetKeyOf(target: ReviewTarget): string {
  return [
    target.kind,
    target.rev ?? "",
    target.base ?? "",
    target.target ?? "",
    target.mode ?? "",
  ].join("::");
}

/**
 * Load + keep-live the diff of `repo` for `target`. A `null` repo disables the hook
 * (undefined diff, not loading). `target` defaults to the repo's working tree upstream.
 */
export function useRepoDiff(repo: ReviewRepo | null, target: ReviewTarget): UseRepoDiff {
  // The surface still needs a rev-spec the user hasn't typed → we must NOT fire `repo_diff` yet
  // (it would throw `diff kind '…' requires a '…' rev-spec`, Finding 43). Kept out of `enabled`.
  const awaitingInput = repo != null && !targetReady(target);
  const enabled = repo != null && !awaitingInput;
  const living = repo != null && isLivingTarget(target);

  const query = useQuery<RepoDiff>({
    queryKey: reviewKeys.repoDiff(
      repo?.workspaceId ?? "",
      repo?.repositoryId ?? "",
      targetKeyOf(target),
    ),
    queryFn: () =>
      nyxBridge.invoke<RepoDiff>("repo_diff", {
        workspaceId: repo!.workspaceId,
        repositoryId: repo!.repositoryId,
        kind: target.kind,
        rev: target.rev,
        base: target.base,
        target: target.target,
        mode: target.mode,
      }),
    enabled,
    // Keep the last diff on screen while a repo/target switch loads (no empty-diff flash), and let
    // structural sharing hold the reference stable when a re-pull is content-identical.
    placeholderData: keepPreviousData,
    // A PINNED diff is frozen on a SHA: never stale, and skipped by the global `repos://changed`
    // invalidation (`<ReviewLiveSync>` reads `meta.frozen`). A LIVING diff is invalidated on push.
    staleTime: living ? 0 : Number.POSITIVE_INFINITY,
    meta: { frozen: !living },
  });

  // LIVING targets only: WATCH the repo so a work-tree/ref change pushes `repos://changed` (which the
  // global sync turns into an invalidation of this query). A pinned target is frozen — no watch. The
  // watch is scoped to THIS repo, so an unrelated repo's change never re-pulls this one. There is NO
  // per-hook `repos://changed` subscription here anymore (one global subscription handles them all).
  useEffect(() => {
    if (!enabled || !repo || !living) return;
    let torndown = false;
    // A missing git-watch runtime is non-fatal (the diff still loads, it just won't auto-refresh).
    void nyxBridge
      .invoke("repo_watch", {
        workspaceId: repo.workspaceId,
        repositoryId: repo.repositoryId,
        base: target.base,
        target: target.target,
      })
      .catch(() => {});
    return () => {
      torndown = true;
      void nyxBridge
        .invoke("repo_unwatch", { workspaceId: repo.workspaceId, repositoryId: repo.repositoryId })
        .catch(() => {});
      void torndown;
    };
    // Re-scope the watch when the (repo, target) identity changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo?.workspaceId, repo?.repositoryId, targetKeyOf(target), enabled, living]);

  // ONE reading of a failure, the bridge's shape first — a `design_*`/`repo_*` refusal crosses as a
  // BridgeError OBJECT, and every other reading of it puts `[object Object]` on screen.
  const error = query.error ? errorText(query.error) : null;

  return {
    diff: query.data,
    // `isLoading` is true ONLY on the very first fetch with no data yet — never on a live re-pull
    // (which keeps the previous data via `keepPreviousData`), matching the old "first load" flag.
    loading: query.isLoading && enabled,
    error,
    awaitingInput,
    refresh: async () => {
      await query.refetch();
    },
  };
}
