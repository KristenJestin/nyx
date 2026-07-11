import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";

import { nyxBridge } from "@/bridge";
import type { SpecWorkspaceContext } from "@/components/specs/types";
import { reviewKeys } from "@/lib/query";

import type { ReviewRepo } from "./review-types";

/**
 * `use-review-repos` — the list of `(workspace, repository)` pairs the review screen shows as
 * repo TABS, scoped to the SINGLE dev workspace the Spec is anchored on (`detail.workspace_context`,
 * PDR-0013/0015), on TanStack Query (PDR-0029). It reuses the phase-2 `workspace_git_summary` read
 * model (the same authority the cockpit's "what changed" uses) to enumerate THAT workspace's
 * NON-OPTIONAL repositories with their identity + branch + changed-file count. Every pair keeps its
 * OWN `(workspaceId, repositoryId)` identity — the screen never mixes two repos' Git state (PDR-0015
 * multi-repo identity isolation).
 *
 * ## Scoped to the Spec's workspace (NOT the whole project)
 * The review lives INSIDE the Spec's Review tab, so its repos are exactly the repos of the Spec's
 * dev workspace — never the other workspaces of the project. The `SpecWorkspaceContext` the detail
 * already carries names that workspace; we enumerate ONLY it.
 *
 * ## Why the git summary, not a new command
 * The engine (phase 1) is keyed on ONE repo at a time; there is no "list every repo" bridge call,
 * but `workspace_git_summary` already returns the workspace's repos with their `repository_id` /
 * `name` / `absolute_path` / `branch`. Reading it for the Spec's workspace yields exactly the
 * multi-repo tab set — no new backend surface, no identity mixing.
 */

/** The `workspace_git_summary` wire shape (mirror of `nyx_core::workspace_git`). */
interface RepoGitSummaryWire {
  readonly repository_id: string;
  readonly name: string;
  readonly relative_path: string;
  readonly absolute_path: string;
  readonly branch: string | null;
  readonly dirty: boolean;
  readonly changed_files: readonly { readonly status: string; readonly path: string }[];
}
interface WorkspaceGitSummaryWire {
  readonly workspace_id: string;
  readonly repos: readonly RepoGitSummaryWire[];
}

export interface UseReviewRepos {
  /** The Spec workspace's `(workspace, repository)` tabs, in membership order. */
  readonly repos: readonly ReviewRepo[];
  /** `true` until the first enumeration of the workspace resolves. */
  readonly loading: boolean;
  /** Re-enumerate on demand (the same load the initial mount runs). */
  readonly refresh: () => Promise<void>;
}

const EMPTY_REPOS: readonly ReviewRepo[] = [];

/**
 * Enumerate the Spec's dev-workspace `(workspace, repository)` pairs. A Spec with no anchor yet
 * (`workspaceContext === null`) or a workspace with no repos yields an empty list (not loading). The
 * single global `<ReviewLiveSync>` re-pulls on `repos://changed` so a new/removed repo appears live.
 */
export function useSpecReviewRepos(workspaceContext: SpecWorkspaceContext | null): UseReviewRepos {
  const workspaceId = workspaceContext?.workspace.id ?? null;
  const workspaceName = workspaceContext?.workspace.name ?? "";
  const enabled = workspaceId !== null;

  const query = useQuery<readonly ReviewRepo[]>({
    queryKey: reviewKeys.repos(workspaceId ?? ""),
    queryFn: async () => {
      const summary = await nyxBridge.invoke<WorkspaceGitSummaryWire>("workspace_git_summary", {
        workspaceId: workspaceId!,
      });
      return summary.repos.map<ReviewRepo>((r) => ({
        workspaceId: workspaceId!,
        workspaceName,
        repositoryId: r.repository_id,
        name: r.name,
        absolutePath: r.absolute_path,
        branch: r.branch,
        changedFileCount: r.changed_files.length,
      }));
    },
    enabled,
    placeholderData: keepPreviousData,
  });

  const refresh = useCallback(async () => {
    await query.refetch();
  }, [query]);

  // A failed summary contributes no tabs (recoverable on refresh) rather than surfacing an error —
  // mirrors the old `catch { setRepos([]) }`.
  const repos = useMemo(
    () => (query.isError ? EMPTY_REPOS : (query.data ?? EMPTY_REPOS)),
    [query.isError, query.data],
  );

  return { repos, loading: query.isLoading && enabled, refresh };
}
