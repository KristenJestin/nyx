import { getRouteApi, Link } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";

import { Spinner } from "@/components/ui/spinner";
import { SidebarShell } from "@/components/ui/sidebar-shell";
import { ErrorBoundary } from "@/components/ui/error-boundary";
import { toast } from "@/components/ui/toast";

import { CommentsPanel } from "./comments-panel";
import type { CommentActions } from "./comment-thread";
import { DiffView, type LineAnchor } from "./diff-view";
import { FileTreePanel } from "./file-tree-panel";
import { ReviewHeader } from "./review-header";
import {
  DEFAULT_REVIEW_TARGET,
  isLivingTarget,
  reviewRepoKey,
  type ReviewRepo,
  type ReviewTarget,
} from "./review-types";
import { useRepoDiff } from "./use-repo-diff";
import { useReviewComments } from "./use-review-comments";
import { useReviewTasks } from "./use-review-tasks";
import { useSpecReviewRepos } from "./use-review-repos";
import { useSpecDetail } from "@/components/specs/use-spec-detail";
import { specKey } from "@/components/specs/spec-meta";

/**
 * `ReviewPage` — the FULL-SCREEN review page (route `/work/$specId/review`, PDR-0028). Reached ONLY
 * from the Spec's Review tab "Open review" button (NO top-level nav / route — the invariant), it
 * re-derives the Spec's dev-workspace context from the `spec_detail` read model and lays out the
 * validated 4-zone screen:
 *
 *  - LEFT (`SidebarShell side="left">`): the active repo's changed files (`@pierre/trees`);
 *    selecting one scrolls the diff.
 *  - CENTER: the multi-repo `ReviewHeader` (repo tabs + surface/variety/unified-split selectors +
 *    a Back link) over the diff (`@pierre/diffs` `FileDiff` via `DiffView`), carrying inline
 *    anchored threads + composers.
 *  - RIGHT (`SidebarShell side="right">`, CLOSABLE): the comments panel + threads + the promotion
 *    grill (`CommentsPanel`).
 *
 * The review is the real T5 model via the bridge, scoped to the ACTIVE `(workspace, repository)` so
 * two repos never mix (PDR-0015). A human OR agent comment landing re-pulls live (the agent replies
 * via MCP). The repo tabs are the Spec's SINGLE dev workspace only.
 */

const routeApi = getRouteApi("/work/$specId/review");

export function ReviewPage() {
  const { specId } = routeApi.useParams();
  const { detail, loading: detailLoading, error: detailError } = useSpecDetail(specId);
  const workspaceContext = detail?.workspace_context ?? null;

  const { repos, loading: reposLoading } = useSpecReviewRepos(workspaceContext);

  // The active repo TAB, kept as the identity key so it survives a repos re-enumeration.
  const [activeKey, setActiveKey] = useState<string | null>(null);
  // Per-repo target/mode selection — each tab keeps its OWN surface + living/pinned choice.
  const [targets, setTargets] = useState<ReadonlyMap<string, ReviewTarget>>(() => new Map());
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [diffStyle, setDiffStyle] = useState<"unified" | "split">("unified");
  // Finding 59 — the LEFT files panel is ALWAYS open now (no toggle); only the RIGHT comments panel
  // keeps a controlled open state.
  const [commentsPanelOpen, setCommentsPanelOpen] = useState(true);
  // The thread cross-selected between the panel and the in-diff card.
  const [selectedThreadId, setSelectedThreadId] = useState<string | null>(null);

  // Default the active tab to the first repo once the list loads (and recover if it vanishes).
  useEffect(() => {
    if (repos.length === 0) {
      if (activeKey !== null) setActiveKey(null);
      return;
    }
    const stillPresent = repos.some((r) => reviewRepoKey(r) === activeKey);
    if (!stillPresent) setActiveKey(reviewRepoKey(repos[0]!));
  }, [repos, activeKey]);

  const activeRepo = useMemo<ReviewRepo | null>(
    () => repos.find((r) => reviewRepoKey(r) === activeKey) ?? null,
    [repos, activeKey],
  );
  const activeTarget = (activeKey && targets.get(activeKey)) || DEFAULT_REVIEW_TARGET;

  const { diff, loading, error, awaitingInput } = useRepoDiff(activeRepo, activeTarget);
  const {
    anchoredThreads,
    globalThreads,
    loading: commentsLoading,
    error: commentsError,
    postGlobal,
    postAnchored,
    reply,
    resolve,
    reopen,
    remap,
  } = useReviewComments(activeRepo, specId);
  // The promotion "grill" (finding = task) + the live unread counter, scoped to the active repo AND
  // the Spec under review (Finding 53 — a task promoted on one Spec never leaks onto another).
  const {
    tasks,
    unread,
    promote,
    validate,
    markRead,
    error: tasksError,
  } = useReviewTasks(activeRepo, specId);

  // Finding 44: a promote/validate FAILURE was silent (the panel only ever showed the COMMENTS
  // hook's error, never the TASKS hook's). Surface it prominently as a toast (visible even when the
  // comments panel is closed); the panel also renders it inline. Keyed on the message so it fires
  // once per distinct failure.
  useEffect(() => {
    if (tasksError) {
      toast.error("Couldn’t update the task", { description: tasksError });
    }
  }, [tasksError]);

  // Mark the review feed READ once the comments panel is open for the active repo (the human is
  // looking at it) — the unread badge drops to 0. Re-runs when the feed changes while open.
  useEffect(() => {
    if (commentsPanelOpen && activeRepo && unread > 0) void markRead();
  }, [commentsPanelOpen, activeRepo, unread, markRead]);

  // RE-MAP the repo's anchors when a LIVING diff's HEAD advances. A living re-pull (driven by
  // `repos://changed` after a commit/checkout) lands a fresh `target_sha`; that is the signal the
  // repo moved under the anchors, so we re-anchor them once against the new HEAD — an anchor whose
  // snippet moved follows the code, one whose snippet vanished flips `outdated` (PDR-0012). Keyed
  // on the resolved HEAD so it fires ONCE per advance, and only for a living target (a pinned diff
  // is frozen). The re-mapped rows re-pull into the feed (`review_remap` broadcasts `repos://changed`).
  const livingHead = diff && isLivingTarget(activeTarget) ? diff.target_sha : null;
  const remappedHeadRef = useRef<string | null>(null);
  useEffect(() => {
    if (!activeRepo || !livingHead) return;
    if (remappedHeadRef.current === livingHead) return;
    remappedHeadRef.current = livingHead;
    void remap();
  }, [activeRepo, livingHead, remap]);
  // Reset the "already re-mapped this HEAD" guard when the active repo changes.
  useEffect(() => {
    remappedHeadRef.current = null;
  }, [activeKey]);

  const commentActions: CommentActions = useMemo(
    () => ({
      onReply: (parentId, body) => void reply(parentId, body),
      onResolve: (id) => void resolve(id),
      onReopen: (id) => void reopen(id),
      // The human lâche brut — promote a comment into a coder task (grill step 1). The agent then
      // structures it via MCP; the human validates it in the tasks section.
      onPromote: (commentId) => void promote(commentId),
    }),
    [reply, resolve, reopen, promote],
  );

  const onAddAnchored = (anchor: LineAnchor, body: string) => {
    if (!activeRepo) return;
    void postAnchored(
      {
        filePath: anchor.path,
        side: anchor.side,
        lineStart: anchor.lineStart,
        // Finding 51 — propagate the drag range's REAL end (was implicitly lineEnd = lineStart).
        lineEnd: anchor.lineEnd,
        blob: anchor.blob,
        snippet: anchor.snippet,
      },
      body,
    );
  };

  const setActiveTarget = (target: ReviewTarget) => {
    if (!activeKey) return;
    setTargets((prev) => {
      const next = new Map(prev);
      next.set(activeKey, target);
      return next;
    });
    setSelectedPath(null);
  };

  const onSelectRepo = (repo: ReviewRepo) => {
    setActiveKey(reviewRepoKey(repo));
    setSelectedPath(null);
    setSelectedThreadId(null);
  };

  // Selecting a thread scrolls the diff to its file (anchored) and highlights it both places.
  const onSelectThread = (id: string) => {
    setSelectedThreadId(id);
    const thread = anchoredThreads.find((t) => t.root.id === id);
    if (thread?.root.file_path) setSelectedPath(thread.root.file_path);
  };

  // The detail is still loading — a full-screen spinner (the page owns the whole viewport).
  if (detailLoading && detail === null) {
    return (
      <div className="flex h-svh items-center justify-center bg-background text-muted-foreground">
        <Spinner />
      </div>
    );
  }

  // A Spec that could not be loaded OR has no dev workspace yet has no review — a clean empty state
  // with a way back to the Spec (mirrors `RuntimeTab`'s null-workspace copy). Placed AFTER every
  // hook so the hook order is stable.
  if (detailError || detail === null || workspaceContext === null) {
    return (
      <div className="flex h-svh flex-col items-center justify-center gap-3 bg-background">
        <p className="text-sm text-muted-foreground">
          {detailError || detail === null
            ? "This Spec could not be loaded."
            : "No review — this Spec has no dev workspace yet."}
        </p>
        <Link
          to="/work/$specId"
          params={{ specId }}
          data-testid="review-back"
          className="text-sm text-foreground underline-offset-4 hover:underline"
        >
          Back to the Spec
        </Link>
      </div>
    );
  }

  // Reset key for the review VIEWER boundaries: it moves when the active repo switches OR the derived
  // diff resolves. The `@pierre/*` viewers can throw while tearing down on a repo switch (esp. into an
  // empty diff); the boundaries below contain that so the header/panel/rail survive, and re-mount the
  // viewer once the fresh diff lands — keying on the diff content is what lets them recover.
  const viewerResetKeys = [activeKey, diff?.target_sha ?? null, diff?.files.length ?? -1];

  return (
    <div
      data-testid="review-page"
      className="flex h-svh w-full overflow-hidden bg-background text-foreground"
    >
      {/* LEFT: changed-files tree (@pierre/trees), an ALWAYS-OPEN left rail (Finding 59 — no collapse
          toggle). Widened to 300px (Finding 58) so deep, compacted folder paths stay readable. The
          tree model is keyed on the diff identity so a repo/target switch yields a fresh tree. */}
      <SidebarShell
        side="left"
        width={300}
        aria-label="Changed files"
        contentClassName="overflow-hidden"
      >
        <ErrorBoundary
          resetKeys={viewerResetKeys}
          fallback={
            <div className="grid h-full place-items-center px-3 text-center text-2xs text-muted-foreground">
              Couldn’t render the file tree.
            </div>
          }
        >
          {/* Key on the DIFF identity, not the repo alone (Finding 42): `@pierre/trees`
              `useFileTree` builds its model ONCE at mount, so a panel keyed on the repo only mounts
              while `diff` is still `undefined` (paths=[]) and never rebuilds when the diff lands →
              a permanently EMPTY tree. Folding `target_sha` + `files.length` into the key remounts
              the panel the moment the diff resolves (and on every subsequent HEAD advance), so the
              tree shows the N changed files. Mirrors `viewerResetKeys` above. */}
          <FileTreePanel
            key={`${activeKey ?? "none"}:${diff?.target_sha ?? ""}:${diff?.files.length ?? 0}`}
            diff={diff}
            selectedPath={selectedPath}
            onSelect={setSelectedPath}
          />
        </ErrorBoundary>
      </SidebarShell>

      {/* CENTER: header + the @pierre/diffs viewer. */}
      <div className="flex min-w-0 flex-1 flex-col">
        <ReviewHeader
          repos={repos}
          activeKey={activeKey}
          onSelectRepo={onSelectRepo}
          activeRepo={activeRepo}
          activeDiff={diff}
          target={activeTarget}
          onTargetChange={setActiveTarget}
          specId={specId}
          specKey={specKey(detail.workspace_context?.project.key ?? "", detail.spec.number)}
          diffStyle={diffStyle}
          onDiffStyleChange={setDiffStyle}
          commentsPanelOpen={commentsPanelOpen}
          onToggleCommentsPanel={() => setCommentsPanelOpen((v) => !v)}
          unread={unread}
        />
        <div className="min-h-0 flex-1">
          {repos.length === 0 && !reposLoading ? (
            <div className="grid h-full place-items-center text-sm text-muted-foreground">
              No repositories to review in this project.
            </div>
          ) : (
            <ErrorBoundary
              resetKeys={viewerResetKeys}
              fallback={
                <div className="grid h-full place-items-center px-4 text-center text-sm text-muted-foreground">
                  Couldn’t render this diff.
                </div>
              }
            >
              <DiffView
                diff={diff}
                loading={loading}
                error={error}
                awaitingInput={awaitingInput}
                scrollToPath={selectedPath}
                diffStyle={diffStyle}
                anchoredThreads={anchoredThreads}
                commentActions={commentActions}
                selectedThreadId={selectedThreadId}
                onSelectThread={onSelectThread}
                onAddAnchored={onAddAnchored}
              />
            </ErrorBoundary>
          )}
        </div>
      </div>

      {/* RIGHT: the comments panel — a CLOSABLE right rail (the SAME SidebarShell in mirror). It is
          opened/closed SOLELY by the ReviewHeader's comment button (Finding 46): the rail carries no
          redundant internal "Close panel" arrow, so both flanks toggle the same way (their header
          button is the single control) — no more left/right asymmetry. */}
      <SidebarShell
        side="right"
        collapsible
        open={commentsPanelOpen && activeRepo != null}
        width={360}
        aria-label="Comments"
      >
        {activeRepo ? (
          <CommentsPanel
            anchoredThreads={anchoredThreads}
            globalThreads={globalThreads}
            selectedThreadId={selectedThreadId}
            onSelectThread={onSelectThread}
            onAddGlobal={(body) => void postGlobal(body)}
            actions={commentActions}
            loading={commentsLoading}
            error={commentsError}
            tasks={tasks}
            tasksError={tasksError}
            onValidateTask={(id) => void validate(id)}
          />
        ) : null}
      </SidebarShell>
    </div>
  );
}

export default ReviewPage;
