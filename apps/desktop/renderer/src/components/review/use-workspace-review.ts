import { keepPreviousData, useMutation, useQueries, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";

import { isBridgeError, nyxBridge } from "@/bridge";
import type { ReviewComment } from "@/bridge";
import { reviewKeys } from "@/lib/query";

import { type CommentThread, buildThreads } from "./review-threads";
import { type ReviewRepo, reviewRepoKey } from "./review-types";

/**
 * `use-workspace-review` — the AGGREGATE review-comment feed of a Spec's WHOLE dev workspace, for
 * the Review TAB (which groups comments by repo, PDR-0028/0015), on TanStack Query (PDR-0029). Where
 * `use-review-comments` keeps ONE `(workspace, repository)` live for the full-screen page, this hook
 * keeps EVERY repo of the workspace live at once (one `useQuery` per repo via `useQueries`) and
 * buckets the threads per repo — the same real T5 bridge (`review_comment_list` /
 * `review_comment_post` / `review_comment_resolve` / `_reopen`), NO mock, and it SHARES the per-repo
 * cache entry (`reviewKeys.comments`) with the full-screen page.
 *
 * The single global `<ReviewLiveSync>` re-pulls on the coarse `repos://changed` push, so a human OR
 * agent comment landing anywhere in the workspace appears live. Resolution stays HUMAN-only.
 */

/** One repo's grouped review threads (roots + replies), oldest-first. */
export interface RepoThreads {
  readonly repo: ReviewRepo;
  readonly threads: readonly CommentThread[];
  readonly openCount: number;
  readonly resolvedCount: number;
  readonly outdatedCount: number;
}

export interface UseWorkspaceReview {
  /** Per-repo grouped threads, in the workspace's repo order. */
  readonly byRepo: readonly RepoThreads[];
  /** Open (unresolved) root-comment count across the whole workspace. */
  readonly openTotal: number;
  /** Total root-comment count across the whole workspace. */
  readonly total: number;
  /** `true` while the first aggregate load is in flight. */
  readonly loading: boolean;
  /** The last load/mutation error, or `null`. */
  readonly error: string | null;
  /** Reply into a thread — derives the target repo from the parent comment. */
  readonly reply: (parent: ReviewComment, body: string) => Promise<void>;
  /** Resolve a comment — HUMAN-only. */
  readonly resolve: (id: string) => Promise<void>;
  /** Re-open a resolved comment — HUMAN-only. */
  readonly reopen: (id: string) => Promise<void>;
  /** Force a re-pull of every repo's feed. */
  readonly refresh: () => Promise<void>;
}

const EMPTY: readonly ReviewComment[] = [];

const errText = (e: unknown): string =>
  isBridgeError(e) ? e.message : e instanceof Error ? e.message : String(e);

/** Count the OPEN roots of a repo's threads (a root whose status is not resolved). */
function countThreads(threads: readonly CommentThread[]) {
  let openCount = 0;
  let resolvedCount = 0;
  let outdatedCount = 0;
  for (const t of threads) {
    if (t.root.status === "resolved") resolvedCount++;
    else openCount++;
    if (t.root.outdated) outdatedCount++;
  }
  return { openCount, resolvedCount, outdatedCount };
}

/**
 * Load + keep-live the review comments of EVERY repo of `repos` (the Spec workspace's repos),
 * SCOPED to `specId` (Finding 41). An empty list disables the hook. Each repo's feed is fetched with
 * its OWN `(workspaceId, repositoryId)` cache key so identities never mix (PDR-0015), and each
 * fetch/post carries `specId` so a Spec's comments never LEAK onto another Spec sharing the same
 * repos (a reply omits it — the backend inherits its root's Spec).
 */
export function useWorkspaceReview(
  repos: readonly ReviewRepo[],
  specId: string | null,
): UseWorkspaceReview {
  const queryClient = useQueryClient();

  const queries = useQueries({
    queries: repos.map((r) => ({
      queryKey: reviewKeys.comments(r.workspaceId, r.repositoryId, specId),
      queryFn: async () => {
        const list = await nyxBridge.invoke<readonly ReviewComment[]>("review_comment_list", {
          workspaceId: r.workspaceId,
          repositoryId: r.repositoryId,
          specId: specId ?? undefined,
        });
        return list ?? EMPTY;
      },
      placeholderData: keepPreviousData,
    })),
    combine: (results) => ({
      // Bucket each repo's comments by its identity key, in repo order.
      byKey: new Map(repos.map((r, i) => [reviewRepoKey(r), results[i]?.data ?? EMPTY] as const)),
      loading: results.some((q) => q.isLoading),
      error: results.find((q) => q.error)?.error ?? null,
    }),
  });

  // Optimistic mutation over ONE repo's comment feed (identified by its cache key).
  const mutation = useMutation<
    unknown,
    unknown,
    {
      key: readonly unknown[];
      run: () => Promise<unknown>;
      optimistic: (prev: readonly ReviewComment[]) => readonly ReviewComment[];
    },
    { key: readonly unknown[]; snapshot: readonly ReviewComment[] | undefined }
  >({
    mutationFn: ({ run }) => run(),
    onMutate: async ({ key, optimistic }) => {
      await queryClient.cancelQueries({ queryKey: key });
      const snapshot = queryClient.getQueryData<readonly ReviewComment[]>(key);
      queryClient.setQueryData<readonly ReviewComment[]>(key, (prev) => optimistic(prev ?? EMPTY));
      return { key, snapshot };
    },
    onError: (_err, _vars, ctx) => {
      if (ctx) queryClient.setQueryData<readonly ReviewComment[]>(ctx.key, ctx.snapshot ?? EMPTY);
    },
    onSettled: (_data, _err, vars) => {
      void queryClient.invalidateQueries({ queryKey: vars.key });
    },
  });

  /** Find the repo cache key that currently holds comment `id` (for resolve/reopen). */
  const keyForComment = useCallback(
    (id: string): readonly unknown[] | null => {
      for (const r of repos) {
        const key = reviewKeys.comments(r.workspaceId, r.repositoryId, specId);
        const list = queryClient.getQueryData<readonly ReviewComment[]>(key);
        if (list?.some((c) => c.id === id)) return key;
      }
      return null;
    },
    [repos, specId, queryClient],
  );

  const reply = useCallback(
    (parent: ReviewComment, body: string) =>
      mutation
        .mutateAsync({
          key: reviewKeys.comments(parent.workspace_id, parent.repository_id, specId),
          run: () =>
            nyxBridge.invoke("review_comment_post", {
              workspaceId: parent.workspace_id,
              repositoryId: parent.repository_id,
              parentId: parent.id,
              body,
            }),
          optimistic: (prev) => [
            ...prev,
            {
              id: `optimistic-${Math.random().toString(36).slice(2)}`,
              workspace_id: parent.workspace_id,
              repository_id: parent.repository_id,
              spec_id: parent.spec_id,
              parent_id: parent.id,
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
              body,
              status: "open",
              resolved_by_type: null,
              created_at: Date.now(),
            },
          ],
        })
        .then(
          () => undefined,
          () => undefined,
        ),
    [mutation, specId],
  );

  const resolve = useCallback(
    (id: string) => {
      const key = keyForComment(id);
      if (!key) return Promise.resolve();
      return mutation
        .mutateAsync({
          key,
          run: () => nyxBridge.invoke("review_comment_resolve", { id }),
          optimistic: (prev) =>
            prev.map((c) =>
              c.id === id ? { ...c, status: "resolved", resolved_by_type: "human" } : c,
            ),
        })
        .then(
          () => undefined,
          () => undefined,
        );
    },
    [mutation, keyForComment],
  );
  const reopen = useCallback(
    (id: string) => {
      const key = keyForComment(id);
      if (!key) return Promise.resolve();
      return mutation
        .mutateAsync({
          key,
          run: () => nyxBridge.invoke("review_comment_reopen", { id }),
          optimistic: (prev) =>
            prev.map((c) => (c.id === id ? { ...c, status: "open", resolved_by_type: null } : c)),
        })
        .then(
          () => undefined,
          () => undefined,
        );
    },
    [mutation, keyForComment],
  );

  const byRepo = useMemo<RepoThreads[]>(() => {
    return repos.map((repo) => {
      const comments = queries.byKey.get(reviewRepoKey(repo)) ?? EMPTY;
      const threads = buildThreads(repo.repositoryId, comments);
      return { repo, threads, ...countThreads(threads) };
    });
  }, [repos, queries.byKey]);

  const { openTotal, total } = useMemo(() => {
    let openTotal = 0;
    let total = 0;
    for (const r of byRepo) {
      openTotal += r.openCount;
      total += r.threads.length;
    }
    return { openTotal, total };
  }, [byRepo]);

  const error = queries.error
    ? errText(queries.error)
    : mutation.error
      ? errText(mutation.error)
      : null;

  const refresh = useCallback(async () => {
    await queryClient.invalidateQueries({
      predicate: (q) => q.queryKey[0] === "review" && q.queryKey[1] === "comments",
    });
  }, [queryClient]);

  return {
    byRepo,
    openTotal,
    total,
    loading: queries.loading,
    error,
    reply,
    resolve,
    reopen,
    refresh,
  };
}
