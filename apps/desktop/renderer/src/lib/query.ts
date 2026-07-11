import { QueryClient } from "@tanstack/react-query";

/**
 * `lib/query` — the renderer's single TanStack Query client + the review query-key factory
 * (PDR-0029). The review data layer migrated off hand-rolled `useState` + `nyxBridge.invoke` +
 * `subscribe("repos://changed")` onto `useQuery`/`useMutation` to KILL two review regressions at
 * the source:
 *
 *  - the LAG: mutations are now OPTIMISTIC (`onMutate` writes the cache before the round-trip), so
 *    a resolve/promote/post shows INSTANTLY instead of after the 2-3 s db-command + re-pull.
 *  - the "depop/pop" FLASH: React Query's default STRUCTURAL SHARING (`replaceEqualDeep`) returns
 *    the PREVIOUS object reference when a re-fetch yields deep-equal content. So a `repo_diff`
 *    re-pull that changes nothing keeps the SAME `RepoDiff` identity → `DiffView`'s `patchByFile`
 *    memo stays stable → `React.memo(FileSection)` bails out → `@pierre/diffs` early-returns instead
 *    of reconstructing the diff DOM. The flash is fixed at the SOURCE (identity), not patched over.
 *
 * `placeholderData: keepPreviousData` (set per query) keeps the last data on the screen while a new
 * key loads — no empty-list flicker on a repo/target switch.
 */

/**
 * Build the app's `QueryClient`. Exposed as a factory so tests spin up an isolated client (no
 * shared cache between test files) while the app uses the module singleton below.
 *
 * Defaults are conservative for a local-first desktop app driven by explicit `repos://changed`
 * pushes (Q-015): queries are `staleTime: 0` (a push invalidates them) with NO window-focus / mount
 * refetch churn, and one retry so a transient bridge hiccup self-heals without a visible error.
 */
export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        // The backend TELLS us when to refetch (`repos://changed` → `invalidateQueries`), so we do
        // not poll and do not refetch on window focus / reconnect (the desktop shell is always
        // "connected"). Structural sharing (RQ default) is what keeps identities stable.
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        retry: 1,
        staleTime: 0,
      },
      mutations: {
        retry: 0,
      },
    },
  });
}

/** The app's single query client, provided at the root via `<QueryClientProvider>` (app.tsx). */
export const queryClient = createQueryClient();

/**
 * The review query-key factory — every key is SCOPED to `(workspaceId, repositoryId, specId)` so the
 * spec-scoping (Finding 41/53) and the multi-repo identity isolation (PDR-0015) hold in the cache:
 * two Specs sharing the same repo, or two repos of one workspace, never share a cache entry. The
 * top-level `"review"` segment lets the global `repos://changed` sync invalidate the WHOLE review
 * data layer in one call (deduping the old per-hook double-refetch).
 */
export const reviewKeys = {
  /** The whole review namespace — the target of the single global invalidation. */
  all: ["review"] as const,
  /** One `(workspace, repository)`'s comment feed, scoped to the Spec under review. */
  comments: (workspaceId: string, repositoryId: string, specId: string | null) =>
    ["review", "comments", workspaceId, repositoryId, specId ?? null] as const,
  /** One `(workspace, repository)`'s promoted tasks, scoped to the Spec under review. */
  tasks: (workspaceId: string, repositoryId: string, specId: string | null) =>
    ["review", "tasks", workspaceId, repositoryId, specId ?? null] as const,
  /** One `(workspace, repository)`'s unread-comment count (repo-scoped watermark — no specId). */
  unread: (workspaceId: string, repositoryId: string) =>
    ["review", "unread", workspaceId, repositoryId] as const,
  /** One `(workspace, repository)`'s derived diff for a target surface (keyed on the target). */
  repoDiff: (workspaceId: string, repositoryId: string, targetKey: string) =>
    ["review", "repoDiff", workspaceId, repositoryId, targetKey] as const,
  /** A dev workspace's `(workspace, repository)` review tabs. */
  repos: (workspaceId: string) => ["review", "repos", workspaceId] as const,
  /** A Spec workspace's AGGREGATE comment feed (the Review TAB), scoped to the Spec. */
  workspaceComments: (workspaceId: string, repositoryId: string, specId: string | null) =>
    ["review", "workspaceComments", workspaceId, repositoryId, specId ?? null] as const,
} as const;
