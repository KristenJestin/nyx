import { Link } from "@tanstack/react-router";
import { ChevronDownIcon, FileDiffIcon, GlobeIcon, MessageSquareIcon } from "lucide-react";
import { motion, type Transition, useReducedMotion } from "motion/react";
import { useMemo, useState } from "react";

import { AuthorIcon } from "@/components/ui/author-icon";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { MicroLabel } from "@/components/ui/micro-label";
import { SectionPanel } from "@/components/ui/section-panel";
import { Segmented } from "@/components/ui/segmented";
import { cn } from "@/lib/utils";

import { CommentThread, type CommentActions } from "@/components/review/comment-thread";
import type { CommentThread as Thread } from "@/components/review/review-threads";
import { type RepoThreads, useWorkspaceReview } from "@/components/review/use-workspace-review";
import { useSpecReviewRepos } from "@/components/review/use-review-repos";

import type { SpecDetail } from "../types";

/**
 * `<ReviewTab>` — the Spec's Review tab (PDR-0028). It is NOT the diff viewer anymore: it shows the
 * review COMMENTS of the Spec's whole dev workspace, grouped by repo, plus an entry card that opens
 * the full-screen review page (`/work/$specId/review`). The deep surface — the diff, the file tree,
 * the promotion grill — lives ONLY on that page.
 *
 * Comments come from the REAL bridge (`use-workspace-review`, aggregated cross-repo, NO mock),
 * grouped by repo with open/resolved/outdated badges; each comment opens IN PLACE to its thread
 * (replies + a reply composer + Resolve/Reopen — resolution is HUMAN-only). A status Segmented
 * filters All/Open/Resolved. A Spec with no dev workspace yet (`workspace_context === null`) shows
 * a clean empty state (mirrors `RuntimeTab`).
 */

type StatusFilter = "all" | "open" | "resolved";

/** The last path segment (file name) — the tab's only path shortening. */
const basename = (path: string) => path.split("/").pop() ?? path;

const CHEVRON_T: Transition = { type: "spring", duration: 0.4, bounce: 0.2 };
const OPEN_T: Transition = { type: "spring", duration: 0.42, bounce: 0.12 };
const CLOSE_T: Transition = { type: "spring", duration: 0.34, bounce: 0.06 };

/** One comment row that opens IN PLACE to its full thread (replies + composer + resolve/reopen). */
function CommentRow({
  thread,
  actions,
  expanded,
  onToggle,
}: {
  thread: Thread;
  actions: CommentActions;
  expanded: boolean;
  onToggle: () => void;
}) {
  const reduce = useReducedMotion();
  const { root, replies } = thread;
  const resolved = root.status === "resolved";

  return (
    <div className={cn("transition-colors", resolved && "opacity-75", expanded && "bg-muted/40")}>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={expanded}
        className="flex w-full items-start gap-3 px-3 py-2.5 text-left transition-colors hover:bg-muted/60"
      >
        <div className="min-w-0 flex-1">
          <div className="mb-1 flex flex-wrap items-center gap-1.5">
            <span className="inline-flex items-center gap-1 text-muted-foreground/80">
              <AuthorIcon type={root.author_type} />
              <span className="text-xs text-foreground/90">{root.author_name}</span>
            </span>
            {root.file_path ? (
              <code className="truncate font-mono text-2xs text-muted-foreground">
                {basename(root.file_path)}
                {root.line_start != null ? `:${root.line_start}` : ""}
              </code>
            ) : (
              <Badge variant="muted" className="gap-1">
                <GlobeIcon />
                global
              </Badge>
            )}
            {root.outdated ? <Badge variant="warning">outdated</Badge> : null}
            {resolved ? (
              <Badge variant="success">resolved</Badge>
            ) : (
              <Badge variant="info">open</Badge>
            )}
            <span className="ml-auto inline-flex items-center gap-1.5 font-mono text-2xs text-muted-foreground/50">
              {replies.length > 0 ? (
                <span className="text-muted-foreground/70">
                  {replies.length} {replies.length === 1 ? "reply" : "replies"}
                </span>
              ) : null}
              <motion.span
                aria-hidden
                animate={{ rotate: expanded ? 180 : 0 }}
                transition={reduce ? { duration: 0 } : CHEVRON_T}
                className="grid size-3.5 place-items-center"
              >
                <ChevronDownIcon className="size-3.5" />
              </motion.span>
            </span>
          </div>
          <p className="line-clamp-2 text-sm text-foreground/85 leading-relaxed">{root.body}</p>
        </div>
      </button>

      <motion.div
        initial={false}
        animate={{ height: expanded ? "auto" : 0, opacity: expanded ? 1 : 0 }}
        transition={reduce ? { duration: 0 } : expanded ? OPEN_T : CLOSE_T}
        className="overflow-hidden"
      >
        {/* Mount the full thread only while expanded — so a collapsed row does not keep a hidden
            duplicate of the root body in the DOM (the preview above already shows it). */}
        {expanded ? (
          <div className="px-3 pb-3">
            <CommentThread thread={thread} actions={actions} />
          </div>
        ) : null}
      </motion.div>
    </div>
  );
}

/** One repo group: a SectionPanel with per-repo badges + the repo's comment rows. */
function RepoGroup({
  group,
  status,
  expandedId,
  onToggle,
  actions,
}: {
  group: RepoThreads;
  status: StatusFilter;
  expandedId: string | null;
  onToggle: (id: string) => void;
  actions: (thread: Thread) => CommentActions;
}) {
  const threads = group.threads.filter((t) => status === "all" || t.root.status === status);
  if (threads.length === 0) return null;

  return (
    <SectionPanel
      title={
        <span className="inline-flex items-center gap-2">
          <FileDiffIcon className="size-3.5 text-muted-foreground/70" />
          {group.repo.name}
        </span>
      }
      action={
        <div className="flex items-center gap-1.5">
          {group.openCount > 0 ? <Badge variant="info">{group.openCount} open</Badge> : null}
          {group.resolvedCount > 0 ? (
            <Badge variant="success">{group.resolvedCount} resolved</Badge>
          ) : null}
          {group.outdatedCount > 0 ? (
            <Badge variant="warning">{group.outdatedCount} outdated</Badge>
          ) : null}
        </div>
      }
    >
      <div className="divide-y divide-border/40">
        {threads.map((t) => (
          <CommentRow
            key={t.root.id}
            thread={t}
            actions={actions(t)}
            expanded={expandedId === t.root.id}
            onToggle={() => onToggle(t.root.id)}
          />
        ))}
      </div>
    </SectionPanel>
  );
}

export function ReviewTab({ detail }: { detail: SpecDetail }) {
  const specId = detail.spec.id;
  const workspaceContext = detail.workspace_context;
  const { repos } = useSpecReviewRepos(workspaceContext);
  // Spec-scoped (Finding 41): the tab shows only THIS Spec's comments, never those of another
  // Spec sharing the same dev-workspace repos.
  const { byRepo, openTotal, total, reply, resolve, reopen } = useWorkspaceReview(repos, specId);

  const [status, setStatus] = useState<StatusFilter>("all");
  const [expandedId, setExpandedId] = useState<string | null>(null);

  // Per-thread actions: reply threads under the (root) comment, resolve/reopen by id (human-only).
  // No promote/grill here — that lives on the full-screen review page.
  const actionsFor = useMemo(
    () =>
      (thread: Thread): CommentActions => ({
        onReply: (_parentId: string, body: string) => void reply(thread.root, body),
        onResolve: (id: string) => void resolve(id),
        onReopen: (id: string) => void reopen(id),
      }),
    [reply, resolve, reopen],
  );

  // A Spec with no dev workspace yet has no repos to review — a clean empty state (mirrors
  // `RuntimeTab`'s null-workspace copy).
  if (workspaceContext === null) {
    return (
      <div className="mx-auto max-w-3xl">
        <p className="text-sm text-muted-foreground">
          No review — this Spec has no dev workspace yet.
        </p>
      </div>
    );
  }

  const hasComments = total > 0;

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4">
      {/* Entry card → the full-screen review page (route Spec-scopée `/work/$specId/review`). */}
      <div className="flex items-center gap-3 rounded-xl border border-border/70 bg-muted/40 px-4 py-3.5">
        <MessageSquareIcon className="size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium text-foreground">
            {openTotal} open comment{openTotal === 1 ? "" : "s"} · {total} total
          </div>
          <p className="text-xs text-muted-foreground">
            Click a comment to read its thread and reply. The full diff and findings open in the
            full-screen review.
          </p>
        </div>
        <Button
          size="sm"
          className="shrink-0"
          data-testid="open-review"
          render={<Link to="/work/$specId/review" params={{ specId }} />}
        >
          <FileDiffIcon />
          Open review
        </Button>
      </div>

      {hasComments ? (
        <>
          <div className="flex items-center justify-between gap-2">
            <MicroLabel>Comments by repo</MicroLabel>
            <Segmented
              aria-label="Comment status filter"
              value={status}
              onChange={setStatus}
              options={[
                { v: "all", label: "All" },
                { v: "open", label: "Open" },
                { v: "resolved", label: "Resolved" },
              ]}
            />
          </div>

          {byRepo.some(
            (g) => g.threads.filter((t) => status === "all" || t.root.status === status).length > 0,
          ) ? (
            <div className="flex flex-col gap-3">
              {byRepo.map((group) => (
                <RepoGroup
                  key={group.repo.repositoryId}
                  group={group}
                  status={status}
                  expandedId={expandedId}
                  onToggle={(id) => setExpandedId((cur) => (cur === id ? null : id))}
                  actions={actionsFor}
                />
              ))}
            </div>
          ) : (
            <EmptyState>No comments for this filter.</EmptyState>
          )}
        </>
      ) : (
        <EmptyState>No review comments yet.</EmptyState>
      )}
    </div>
  );
}
