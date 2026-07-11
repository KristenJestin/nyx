import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo, useRef } from "react";

import { isBridgeError, nyxBridge } from "@/bridge";
import type { AnchorSide, ReviewComment } from "@/bridge";
import { reviewKeys } from "@/lib/query";

import { type CommentThread, buildThreads, partitionThreads } from "./review-threads";
import type { ReviewRepo } from "./review-types";

/**
 * `use-review-comments` — the review COMMENT lifecycle of ONE `(workspace, repository)` behind
 * a single interface, on TanStack Query (PDR-0029): the loaded feed + its derived threads
 * (anchored / global) + the OPTIMISTIC mutations (post a global/anchored comment, reply,
 * resolve/reopen). The panel and the in-diff anchors only assemble the layout; this hook owns the
 * data + the bridge wiring.
 *
 * ## The T5 model, over the bridge (PDR-0005)
 * It reads the auto-threaded feed via `review_comment_list` and writes via
 * `review_comment_post` / `review_comment_resolve` / `review_comment_reopen` — the SAME
 * `nyx_core::db` model the MCP agent façade writes. So an AGENT reply posted through the MCP
 * `review_comment_post` tool lands in this same feed and, because that tool pushes
 * `repos://changed`, the single global `<ReviewLiveSync>` invalidates this query and the agent's
 * reply appears in the thread live — the "réponse agent via MCP" contract, with no reload.
 *
 * ## Optimistic (PDR-0029 — kill the lag)
 * Every mutation is OPTIMISTIC: `onMutate` cancels in-flight fetches, snapshots the cache, and
 * writes the expected next feed IMMEDIATELY (a post appends a provisional comment, resolve flips the
 * status), so the action shows instantly instead of after the 2-3 s db-command + re-pull. `onError`
 * rolls back to the snapshot; `onSettled` invalidates so the server truth reconciles (structural
 * sharing keeps identities stable through the reconcile).
 *
 * ## Resolution reserved to the human
 * The hook exposes `resolve`/`reopen` (the human's transitions). There is intentionally NO
 * agent-resolve path: the agent façade (MCP) advertises no resolve tool, so the ONLY way a
 * comment resolves is a human clicking Resolve here.
 *
 * ## Spec scoping (Finding 41)
 * The review is bound to ONE Spec, so `specId` is part of the query KEY AND is sent on every
 * `review_comment_list` / `review_comment_post` — comments no longer LEAK onto other Specs sharing
 * the same `(workspace, repository)`. A reply omits `specId`: the backend inherits the root's Spec.
 */

/** An anchor to post a comment on: a diff line/range on a side of a file. */
export interface CommentAnchorInput {
  readonly filePath: string;
  readonly side: AnchorSide;
  readonly lineStart: number;
  readonly lineEnd?: number;
  /** The blob SHA the anchor is captured on (helps the backend re-map across commits). */
  readonly blob?: string | null;
  /** The anchored line text (helps re-mapping / disambiguation). */
  readonly snippet?: string;
  /** A few surrounding lines (helps re-mapping disambiguation). */
  readonly context?: string;
}

export interface UseReviewComments {
  /** The loaded feed (flat), oldest-first. */
  readonly comments: readonly ReviewComment[];
  /** Anchored thread roots + replies (grouped/rendered by the panel + the diff). */
  readonly anchoredThreads: readonly CommentThread[];
  /** Global (unanchored) threads. */
  readonly globalThreads: readonly CommentThread[];
  /** `true` while the first load is in flight. */
  readonly loading: boolean;
  /** The last load/mutation error, or `null`. */
  readonly error: string | null;
  /** Post a GLOBAL comment (no anchor). */
  readonly postGlobal: (body: string) => Promise<void>;
  /** Post an ANCHORED comment on a diff line/range. */
  readonly postAnchored: (anchor: CommentAnchorInput, body: string) => Promise<void>;
  /** Reply into a thread (a new comment under `parentId`). */
  readonly reply: (parentId: string, body: string) => Promise<void>;
  /** Resolve a comment — HUMAN-only. */
  readonly resolve: (id: string) => Promise<void>;
  /** Re-open a resolved comment — HUMAN-only. */
  readonly reopen: (id: string) => Promise<void>;
  /**
   * RE-MAP the repo's anchored comments against its current HEAD (the phase-4 re-mapping
   * engine): an anchor whose snippet moved FOLLOWS the code, one whose snippet vanished flips
   * `outdated`. The living review calls this after the repo advances (a new commit) so the
   * anchors stay coherent with the moving diff; the re-mapped/outdated rows re-pull into the
   * feed. Resolves to the number of anchors re-written (0 = nothing moved).
   */
  readonly remap: () => Promise<number>;
  /** Force a re-pull of the feed. */
  readonly refresh: () => Promise<void>;
}

const EMPTY: readonly ReviewComment[] = [];

const errText = (e: unknown): string =>
  isBridgeError(e) ? e.message : e instanceof Error ? e.message : String(e);

/** Build a provisional comment for an optimistic post (reconciled by the `onSettled` re-pull). */
function optimisticComment(over: Partial<ReviewComment>): ReviewComment {
  return {
    id: `optimistic-${Math.random().toString(36).slice(2)}`,
    workspace_id: "",
    repository_id: "",
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
    body: "",
    status: "open",
    resolved_by_type: null,
    created_at: Date.now(),
    ...over,
  };
}

/**
 * Load + keep-live the comments of `repo`, SCOPED to `specId` (the Spec the review was opened from,
 * PDR-0004/0028). A `null` repo disables the query (empty feed, not loading). The single global
 * `<ReviewLiveSync>` re-pulls on the `repos://changed` push so a human OR agent comment landing on
 * the backend appears without a manual reload.
 */
export function useReviewComments(
  repo: ReviewRepo | null,
  specId: string | null,
): UseReviewComments {
  const queryClient = useQueryClient();
  const enabled = repo != null;
  const key = reviewKeys.comments(repo?.workspaceId ?? "", repo?.repositoryId ?? "", specId);

  const query = useQuery<readonly ReviewComment[]>({
    queryKey: key,
    queryFn: async () => {
      const next = await nyxBridge.invoke<readonly ReviewComment[]>("review_comment_list", {
        workspaceId: repo!.workspaceId,
        repositoryId: repo!.repositoryId,
        specId: specId ?? undefined,
      });
      return next ?? EMPTY;
    },
    enabled,
    placeholderData: keepPreviousData,
  });

  const comments = query.data ?? EMPTY;

  // A generic optimistic mutation runner: cancel in-flight fetches, snapshot the cache, apply
  // `optimistic` to the feed immediately, roll back on error, and re-pull on settle. Every
  // mutation below is a thin wrapper over it so the item appears/changes INSTANT (PDR-0029).
  const mutation = useMutation<
    unknown,
    unknown,
    {
      run: () => Promise<unknown>;
      optimistic: (prev: readonly ReviewComment[]) => readonly ReviewComment[];
    },
    { snapshot: readonly ReviewComment[] | undefined }
  >({
    mutationFn: ({ run }) => run(),
    onMutate: async ({ optimistic }) => {
      await queryClient.cancelQueries({ queryKey: key });
      const snapshot = queryClient.getQueryData<readonly ReviewComment[]>(key);
      queryClient.setQueryData<readonly ReviewComment[]>(key, (prev) => optimistic(prev ?? EMPTY));
      return { snapshot };
    },
    onError: (_err, _vars, ctx) => {
      if (ctx) queryClient.setQueryData<readonly ReviewComment[]>(key, ctx.snapshot ?? EMPTY);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: key });
    },
  });

  // Finding 62 — keep `run` (and therefore `resolve`/`reopen`/`reply`, and the `commentActions` the
  // review page derives from them) REFERENTIALLY STABLE. `useMutation` returns a FRESH result object on
  // EVERY render (`{ ...result, mutate, mutateAsync }`), so a `[mutation]` dep here would recreate `run`
  // — and every action built on it — on every render. A new `commentActions` breaks
  // `React.memo(FileSection)` for EVERY file, so a single resolve (which re-renders this hook several
  // times: optimistic write → pending → settle → re-pull) reflowed the WHOLE diff (the pre-#62 bug). We
  // read the latest `mutateAsync` from a ref (its underlying observer-bound fn is itself stable) so `run`
  // is created ONCE and the actions keep their identity across the resolve's re-render storm.
  const mutateAsyncRef = useRef(mutation.mutateAsync);
  mutateAsyncRef.current = mutation.mutateAsync;
  const run = useCallback(
    (
      fn: () => Promise<unknown>,
      optimistic: (prev: readonly ReviewComment[]) => readonly ReviewComment[],
      // Never REJECT the public promise: a failure is surfaced via the `error` field (the old
      // contract) after `onError` rolled the optimistic write back, so callers can `void` it safely.
    ) =>
      mutateAsyncRef.current({ run: fn, optimistic }).then(
        () => undefined,
        () => undefined,
      ),
    [],
  );

  const postGlobal = useCallback(
    (body: string) => {
      if (!repo) return Promise.resolve();
      return run(
        () =>
          nyxBridge.invoke("review_comment_post", {
            workspaceId: repo.workspaceId,
            repositoryId: repo.repositoryId,
            specId: specId ?? undefined,
            body,
          }),
        (prev) => [
          ...prev,
          optimisticComment({
            workspace_id: repo.workspaceId,
            repository_id: repo.repositoryId,
            spec_id: specId,
            body,
          }),
        ],
      );
    },
    [repo, specId, run],
  );

  const postAnchored = useCallback(
    (anchor: CommentAnchorInput, body: string) => {
      if (!repo) return Promise.resolve();
      return run(
        () =>
          nyxBridge.invoke("review_comment_post", {
            workspaceId: repo.workspaceId,
            repositoryId: repo.repositoryId,
            specId: specId ?? undefined,
            body,
            filePath: anchor.filePath,
            side: anchor.side,
            lineStart: anchor.lineStart,
            lineEnd: anchor.lineEnd ?? anchor.lineStart,
            blob: anchor.blob ?? undefined,
            snippet: anchor.snippet,
            context: anchor.context,
          }),
        (prev) => [
          ...prev,
          optimisticComment({
            workspace_id: repo.workspaceId,
            repository_id: repo.repositoryId,
            spec_id: specId,
            body,
            file_path: anchor.filePath,
            side: anchor.side,
            line_start: anchor.lineStart,
            line_end: anchor.lineEnd ?? anchor.lineStart,
            blob: anchor.blob ?? null,
            snippet: anchor.snippet ?? null,
          }),
        ],
      );
    },
    [repo, specId, run],
  );

  // A reply omits `specId` on purpose: the backend inherits the root's Spec so the reply always
  // lands in its root's thread (even when the root belongs to a different Spec-scoped view).
  const reply = useCallback(
    (parentId: string, body: string) => {
      if (!repo) return Promise.resolve();
      return run(
        () =>
          nyxBridge.invoke("review_comment_post", {
            workspaceId: repo.workspaceId,
            repositoryId: repo.repositoryId,
            parentId,
            body,
          }),
        (prev) => [
          ...prev,
          optimisticComment({
            workspace_id: repo.workspaceId,
            repository_id: repo.repositoryId,
            parent_id: parentId,
            body,
          }),
        ],
      );
    },
    [repo, run],
  );

  const resolve = useCallback(
    (id: string) =>
      run(
        () => nyxBridge.invoke("review_comment_resolve", { id }),
        (prev) =>
          prev.map((c) =>
            c.id === id ? { ...c, status: "resolved", resolved_by_type: "human" } : c,
          ),
      ),
    [run],
  );
  const reopen = useCallback(
    (id: string) =>
      run(
        () => nyxBridge.invoke("review_comment_reopen", { id }),
        (prev) =>
          prev.map((c) => (c.id === id ? { ...c, status: "open", resolved_by_type: null } : c)),
      ),
    [run],
  );

  // Re-map the repo's anchors against HEAD. Not optimistic (its outcome is a re-anchoring the backend
  // computes): fire the command, then invalidate so the re-mapped/outdated rows re-pull. Returns the
  // number of anchors re-written. A failure is swallowed to the error surface and leaves the feed as-is.
  const remap = useCallback(async (): Promise<number> => {
    if (!repo) return 0;
    try {
      const res = await nyxBridge.invoke<{ written: number }>("review_remap", {
        workspaceId: repo.workspaceId,
        repositoryId: repo.repositoryId,
      });
      await queryClient.invalidateQueries({ queryKey: key });
      return res?.written ?? 0;
    } catch {
      return 0;
    }
  }, [repo, queryClient, key]);

  const { anchored, global } = useMemo(() => {
    if (!repo) return { anchored: [] as CommentThread[], global: [] as CommentThread[] };
    return partitionThreads(buildThreads(repo.repositoryId, comments));
  }, [repo, comments]);

  const error = query.error
    ? errText(query.error)
    : mutation.error
      ? errText(mutation.error)
      : null;

  return {
    comments,
    anchoredThreads: anchored,
    globalThreads: global,
    loading: query.isLoading && enabled,
    error,
    postGlobal,
    postAnchored,
    reply,
    resolve,
    reopen,
    remap,
    refresh: async () => {
      await query.refetch();
    },
  };
}
