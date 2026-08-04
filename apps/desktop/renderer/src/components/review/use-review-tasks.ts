import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useRef } from "react";

import { errorText, nyxBridge } from "@/bridge";
import type { ReviewTask, ReviewUnread, TaskSeverity } from "@/bridge";
import { reviewKeys } from "@/lib/query";

import type { ReviewRepo } from "./review-types";

/**
 * `use-review-tasks` — the review PROMOTION ("grill") + UNREAD-counter lifecycle of ONE
 * `(workspace, repository)`, behind a single interface, on TanStack Query (PDR-0012/0029). It keeps
 * live the promoted tasks and the unread-comment count, and it exposes the HUMAN's grill steps as
 * OPTIMISTIC mutations.
 *
 * ## `finding = task` + the grill (PDR-0012)
 * A task IS a coder task promoted from a comment. The promotion is a THREE-actor grill:
 *   - the HUMAN lâche brut: `promote(commentId)` creates a `proposed` task (this hook);
 *   - the AGENT structures it via the MCP `review_task_structure` tool (NOT this hook — the
 *     UI never structures on the agent's behalf), landing it in `structured`;
 *   - the HUMAN validates: `validate(id)` moves it to `validated` — only THEN is it ready for
 *     the coder.
 * There is intentionally NO `structure` here and NO agent-validate anywhere: the UI is the
 * human façade (promote + validate), the MCP façade is the agent one (structure only).
 *
 * ## Spec scoping (Finding 53)
 * Like `useReviewComments` (Finding 41), `specId` is part of the tasks query KEY AND is sent on
 * every `review_task_list` — the backend returns ONLY this Spec's promoted tasks. `unread` /
 * `markRead` stay repo-scoped (the feed watermark is per repo, no specId).
 *
 * ## Real-time (PDR-0015, Q-015)
 * The single global `<ReviewLiveSync>` re-pulls both queries on the coarse `repos://changed` push —
 * so an agent structuring a task and a new comment landing (raising the unread count) both refresh
 * live, no reload.
 */

export interface UseReviewTasks {
  /** The promoted-task list, oldest-first. */
  readonly tasks: readonly ReviewTask[];
  /** The number of unread comments/replies in this repo's review feed. */
  readonly unread: number;
  /** `true` while the first load is in flight. */
  readonly loading: boolean;
  /** The last load/mutation error, or `null`. */
  readonly error: string | null;
  /** PROMOTE a comment into a coder task — the human lâche brut (→ `proposed`). */
  readonly promote: (
    commentId: string,
    opts?: { title?: string; severity?: TaskSeverity },
  ) => Promise<void>;
  /** VALIDATE a structured task — the human's step (→ `validated`, ready for the coder). */
  readonly validate: (id: string) => Promise<void>;
  /** Mark the review feed READ — zero the unread count. */
  readonly markRead: () => Promise<void>;
  /** Force a re-pull of the tasks + unread count. */
  readonly refresh: () => Promise<void>;
}

const EMPTY_TASKS: readonly ReviewTask[] = [];

/** Build a provisional task for an optimistic promote (reconciled by the `onSettled` re-pull). */
function optimisticTask(over: Partial<ReviewTask>): ReviewTask {
  const now = Date.now();
  return {
    id: `optimistic-${Math.random().toString(36).slice(2)}`,
    workspace_id: "",
    repository_id: "",
    from_comment_id: "",
    title: "Promoting…",
    details: null,
    severity: null,
    grill_state: "proposed",
    spec_id: null,
    created_at: now,
    updated_at: now,
    ...over,
  };
}

/**
 * Load + keep-live the tasks + unread count of `repo`, SCOPED to `specId` (Finding 53). A `null`
 * repo disables the queries (empty list, 0 unread, not loading). The single global `<ReviewLiveSync>`
 * re-pulls on the `repos://changed` push so a promotion, a validation, an agent-structured task OR a
 * new comment appears/updates without a reload. `specId` scopes the task LIST only — the unread
 * watermark stays per `(workspace, repository)`.
 */
export function useReviewTasks(repo: ReviewRepo | null, specId: string | null): UseReviewTasks {
  const queryClient = useQueryClient();
  const enabled = repo != null;
  const tasksKey = reviewKeys.tasks(repo?.workspaceId ?? "", repo?.repositoryId ?? "", specId);
  const unreadKey = reviewKeys.unread(repo?.workspaceId ?? "", repo?.repositoryId ?? "");

  const tasksQuery = useQuery<readonly ReviewTask[]>({
    queryKey: tasksKey,
    queryFn: async () => {
      const next = await nyxBridge.invoke<readonly ReviewTask[]>("review_task_list", {
        workspaceId: repo!.workspaceId,
        repositoryId: repo!.repositoryId,
        specId: specId ?? undefined,
      });
      return next ?? EMPTY_TASKS;
    },
    enabled,
    placeholderData: keepPreviousData,
  });

  const unreadQuery = useQuery<number>({
    queryKey: unreadKey,
    queryFn: async () => {
      const next = await nyxBridge.invoke<ReviewUnread>("review_unread", {
        workspaceId: repo!.workspaceId,
        repositoryId: repo!.repositoryId,
      });
      return next?.unread ?? 0;
    },
    enabled,
    placeholderData: keepPreviousData,
  });

  // Optimistic mutation over the TASKS feed: cancel, snapshot, apply, roll back on error, re-pull.
  const taskMutation = useMutation<
    unknown,
    unknown,
    {
      run: () => Promise<unknown>;
      optimistic: (prev: readonly ReviewTask[]) => readonly ReviewTask[];
    },
    { snapshot: readonly ReviewTask[] | undefined }
  >({
    mutationFn: ({ run }) => run(),
    onMutate: async ({ optimistic }) => {
      await queryClient.cancelQueries({ queryKey: tasksKey });
      const snapshot = queryClient.getQueryData<readonly ReviewTask[]>(tasksKey);
      queryClient.setQueryData<readonly ReviewTask[]>(tasksKey, (prev) =>
        optimistic(prev ?? EMPTY_TASKS),
      );
      return { snapshot };
    },
    onError: (_err, _vars, ctx) => {
      if (ctx)
        queryClient.setQueryData<readonly ReviewTask[]>(tasksKey, ctx.snapshot ?? EMPTY_TASKS);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: tasksKey });
    },
  });

  // Mark-read zeroes the UNREAD count optimistically (its own query key).
  const markReadMutation = useMutation<unknown, unknown, void, { snapshot: number | undefined }>({
    mutationFn: () => {
      if (!repo) return Promise.resolve();
      return nyxBridge.invoke("review_mark_read", {
        workspaceId: repo.workspaceId,
        repositoryId: repo.repositoryId,
      });
    },
    onMutate: async () => {
      await queryClient.cancelQueries({ queryKey: unreadKey });
      const snapshot = queryClient.getQueryData<number>(unreadKey);
      queryClient.setQueryData<number>(unreadKey, 0);
      return { snapshot };
    },
    onError: (_err, _vars, ctx) => {
      if (ctx) queryClient.setQueryData<number>(unreadKey, ctx.snapshot ?? 0);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: unreadKey });
    },
  });

  // Finding 62 — `useMutation` returns a FRESH result object each render, so a `[taskMutation]` /
  // `[markReadMutation]` dep would recreate `promote`/`validate`/`markRead` every render, and thus the
  // review page's `commentActions` — which breaks `React.memo(FileSection)` for EVERY file and reflows
  // the whole diff on any promote/validate. Read the latest `mutateAsync` from refs so these actions
  // keep a stable identity across the re-render storm a mutation triggers.
  const taskMutateRef = useRef(taskMutation.mutateAsync);
  taskMutateRef.current = taskMutation.mutateAsync;
  const markReadMutateRef = useRef(markReadMutation.mutateAsync);
  markReadMutateRef.current = markReadMutation.mutateAsync;

  const promote = useCallback(
    (commentId: string, opts?: { title?: string; severity?: TaskSeverity }) =>
      taskMutateRef
        .current({
          run: () =>
            nyxBridge.invoke("review_task_promote", {
              commentId,
              title: opts?.title,
              severity: opts?.severity,
            }),
          optimistic: (prev) => [
            ...prev,
            optimisticTask({
              workspace_id: repo?.workspaceId ?? "",
              repository_id: repo?.repositoryId ?? "",
              from_comment_id: commentId,
              title: opts?.title ?? "Promoting…",
              severity: opts?.severity ?? null,
              spec_id: specId,
            }),
          ],
        })
        .then(
          () => undefined,
          () => undefined,
        ),
    [repo, specId],
  );

  const validate = useCallback(
    (id: string) =>
      taskMutateRef
        .current({
          run: () => nyxBridge.invoke("review_task_validate", { id }),
          optimistic: (prev) =>
            prev.map((t) => (t.id === id ? { ...t, grill_state: "validated" } : t)),
        })
        .then(
          () => undefined,
          () => undefined,
        ),
    [],
  );

  const markRead = useCallback(
    () =>
      markReadMutateRef.current().then(
        () => undefined,
        () => undefined,
      ),
    [],
  );

  const error = tasksQuery.error
    ? errorText(tasksQuery.error)
    : taskMutation.error
      ? errorText(taskMutation.error)
      : markReadMutation.error
        ? errorText(markReadMutation.error)
        : null;

  return {
    tasks: tasksQuery.data ?? EMPTY_TASKS,
    unread: unreadQuery.data ?? 0,
    loading: tasksQuery.isLoading && enabled,
    error,
    promote,
    validate,
    markRead,
    refresh: async () => {
      await Promise.all([tasksQuery.refetch(), unreadQuery.refetch()]);
    },
  };
}
