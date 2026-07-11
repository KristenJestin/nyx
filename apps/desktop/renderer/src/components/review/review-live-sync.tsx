import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";

import { nyxBridge } from "@/bridge";
import { reviewKeys } from "@/lib/query";

/**
 * `useReviewLiveSync` / `<ReviewLiveSync>` — the SINGLE global `repos://changed` subscription for the
 * whole review data layer (PDR-0029). It replaces the FIVE per-hook subscriptions (one in each
 * review hook + the diff hook) that each re-pulled on every push: a single mutation used to fan out
 * into 3-4 parallel reloads (the "double-refetch"). Now ONE subscription invalidates the review
 * query namespace and React Query refetches each ACTIVE query exactly once (request-deduped).
 *
 * ## The pinned-diff invariant survives (Finding — pinned is frozen)
 * A PINNED diff (a commit / an explicit range) is fetched once and NEVER re-pulled — the engine
 * froze it on a SHA, so a `repos://changed` push is irrelevant to it. The `use-repo-diff` hook tags
 * a pinned query `meta: { frozen: true }`; the predicate below SKIPS those, so a push re-pulls the
 * living diffs + comments/tasks/unread but leaves a pinned diff untouched (no watch, no re-pull),
 * exactly as before the migration.
 */
export function useReviewLiveSync(): void {
  const queryClient = useQueryClient();
  useEffect(() => {
    let torndown = false;
    let unsubscribe: (() => void) | null = null;
    void nyxBridge
      .subscribe("repos://changed", () => {
        if (torndown) return;
        // Invalidate the ENTIRE review namespace in one call (comments / tasks / unread / repos /
        // living diffs), skipping frozen (pinned) diffs. `invalidateQueries` refetches only the
        // ACTIVE (mounted) queries by default, so off-screen entries just go stale.
        void queryClient.invalidateQueries({
          predicate: (q) => q.queryKey[0] === reviewKeys.all[0] && q.meta?.["frozen"] !== true,
        });
      })
      .then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unsubscribe = un;
      });
    return () => {
      torndown = true;
      if (unsubscribe) void Promise.resolve(unsubscribe()).catch(() => {});
    };
  }, [queryClient]);
}

/** Invisible mount point for {@link useReviewLiveSync} — rendered once under the app's QueryClient. */
export function ReviewLiveSync(): null {
  useReviewLiveSync();
  return null;
}
