import { useCallback, useEffect, useMemo, useState } from "react";

import { nyxBridge } from "@/bridge";
import type { CommentAuthorType, ReviewComment } from "@/bridge";

import type { SpecWorkspaceContext } from "./types";

/**
 * `use-spec-review-comments` — the review COMMENTS of a Spec's dev workspace, read for the
 * OVERLAY/PANEL provenance derivation. A review FINDING (a `tasks kind=review` row) no longer
 * carries an `origin` column (dropped in v24, PDR-0016); its PROVENANCE — human feedback vs. an
 * auditor/agent — is derived instead from the `author_type` of its SOURCE comment (`from_comment_id`).
 * This hook fetches those source comments so the panel can mark each finding's provenance (human vs
 * auditor) with its per-row icon, and the rail can raise its "needs you" (magenta) state on an OPEN
 * human comment.
 *
 * It is a PLAIN bridge hook (like `use-spec-detail`), NOT TanStack — the closed rail overlay is
 * mounted without a `QueryClientProvider`, so it must not depend on one. A Spec with no dev
 * workspace yet (`workspaceContext === null`) fetches nothing (an empty map). The comments re-pull
 * on `repos://changed` (a new human/agent comment) AND `specs://changed` (a comment promoted into a
 * finding), so provenance stays live with no polling.
 */

/** The `workspace_git_summary` repo shape — we only need each repo's identity to list its comments. */
interface RepoGitSummaryWire {
  readonly repository_id: string;
}
interface WorkspaceGitSummaryWire {
  readonly repos?: readonly RepoGitSummaryWire[];
}

const EMPTY: readonly ReviewComment[] = [];

/** A lookup `comment id → author_type` — the source a finding's provenance is derived from. */
export function commentAuthorMap(
  comments: readonly ReviewComment[],
): ReadonlyMap<string, CommentAuthorType> {
  const map = new Map<string, CommentAuthorType>();
  for (const c of comments) map.set(c.id, c.author_type);
  return map;
}

/** Whether ANY review comment is an OPEN human comment — the rail's "besoin de toi" (magenta) signal. */
export function hasOpenHumanComment(comments: readonly ReviewComment[]): boolean {
  return comments.some((c) => c.author_type === "human" && c.status === "open");
}

export interface SpecReviewComments {
  /** The flat comment list across the Spec's dev-workspace repos (empty when no workspace). */
  readonly comments: readonly ReviewComment[];
  /** `comment id → author_type` — pass to the panel to derive each finding's provenance. */
  readonly authorByCommentId: ReadonlyMap<string, CommentAuthorType>;
  /** An OPEN human comment exists ⇒ the rail's current phase reads "besoin de toi" (magenta). */
  readonly hasPendingHumanComment: boolean;
}

/**
 * Load + keep-live the review comments of the Spec's dev workspace. `specId` scopes the feed
 * (Finding 41) so a Spec never reads another Spec's comments. Every repo of the workspace is
 * enumerated via `workspace_git_summary` (the same authority `use-review-repos` uses), then each
 * repo's `review_comment_list` is fetched and flattened. All failures degrade to an empty feed.
 */
export function useSpecReviewComments(
  workspaceContext: SpecWorkspaceContext | null,
  specId: string,
): SpecReviewComments {
  const workspaceId = workspaceContext?.workspace.id ?? null;
  const [comments, setComments] = useState<readonly ReviewComment[]>(EMPTY);

  const load = useCallback(async () => {
    if (workspaceId === null) {
      setComments(EMPTY);
      return;
    }
    try {
      const summary = await nyxBridge.invoke<WorkspaceGitSummaryWire>("workspace_git_summary", {
        workspaceId,
      });
      const repos = summary?.repos ?? [];
      const lists = await Promise.all(
        repos.map((r) =>
          nyxBridge
            .invoke<readonly ReviewComment[]>("review_comment_list", {
              workspaceId,
              repositoryId: r.repository_id,
              specId,
            })
            .then((list) => list ?? EMPTY)
            .catch(() => EMPTY),
        ),
      );
      setComments(lists.flat());
    } catch {
      setComments(EMPTY);
    }
  }, [workspaceId, specId]);

  useEffect(() => {
    void load().catch(() => {});
  }, [load]);

  // Live: re-pull on `repos://changed` (a new comment landed) AND `specs://changed` (a comment was
  // promoted into a finding) — StrictMode-safe teardown of both subscriptions.
  useEffect(() => {
    if (workspaceId === null) return;
    let torndown = false;
    const unlistens: Array<() => void> = [];
    const onChanged = () => {
      if (!torndown) void load().catch(() => {});
    };
    for (const topic of ["repos://changed", "specs://changed"] as const) {
      void nyxBridge.subscribe(topic, onChanged).then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlistens.push(un);
      });
    }
    return () => {
      torndown = true;
      for (const un of unlistens) void Promise.resolve(un()).catch(() => {});
    };
  }, [workspaceId, load]);

  const authorByCommentId = useMemo(() => commentAuthorMap(comments), [comments]);
  const hasPendingHumanComment = useMemo(() => hasOpenHumanComment(comments), [comments]);

  return { comments, authorByCommentId, hasPendingHumanComment };
}
