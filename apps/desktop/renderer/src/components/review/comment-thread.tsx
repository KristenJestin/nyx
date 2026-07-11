import { useState } from "react";
import {
  BotIcon,
  CheckIcon,
  ClockAlertIcon,
  CornerDownRightIcon,
  ListTodoIcon,
  RotateCcwIcon,
  UserIcon,
} from "lucide-react";

import type { CommentAuthorType, ReviewComment } from "@/bridge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import type { CommentThread as Thread } from "./review-threads";

/**
 * `comment-thread` — the reusable REVIEW-COMMENT building blocks: an author chip, a comment
 * card, a composer, and the thread (root + replies + actions). Used both in the side panel and
 * inline on the diff. The PLACEMENT traces the prototype's `comment-thread.tsx` (author line +
 * body, replies indented under the root, an actions row with Resolve/Reply, and a composer that
 * opens on Reply); the LOOK is the real design system's tokens/primitives (`Badge`, `Button`,
 * `cn`, CSS vars) — no proto pixels are lifted.
 *
 * The human comments, resolves and replies as a human. The agent replies (and resolves NEVER)
 * on its own side via MCP; this UI simply renders both authors' comments and never offers the
 * human a way to resolve AS the agent.
 */

/** The mutations the thread UI drives — all HUMAN actions (the agent acts via MCP). */
export interface CommentActions {
  readonly onReply: (parentId: string, body: string) => void;
  readonly onResolve: (id: string) => void;
  readonly onReopen: (id: string) => void;
  /**
   * PROMOTE a comment into a coder task — the human's step of the "grill" (lâche brut). Optional:
   * a panel that has no promotion wiring omits it and the button is not rendered. The agent then
   * STRUCTURES the proposed task via MCP; the human VALIDATES it (in the tasks section).
   */
  readonly onPromote?: (commentId: string) => void;
}

/** A compact author chip: an icon (human vs agent) + the display name. */
export function AuthorChip({ type, name }: { type: CommentAuthorType; name: string }) {
  const Icon = type === "agent" ? BotIcon : UserIcon;
  return (
    <span className="inline-flex items-center gap-1 text-2xs text-muted-foreground">
      <Icon className="size-3" />
      <span className="font-medium text-foreground/80">{name}</span>
    </span>
  );
}

/** `created_at` (epoch ms) → a short `dd/mm hh:mm` label. */
function formatTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** ONE comment's card: author + time (+ an `outdated` badge on the anchored root) + its body. */
function CommentCard({ comment }: { comment: ReviewComment }) {
  return (
    <div className="px-3 py-2">
      <div className="mb-1 flex items-center gap-2">
        <AuthorChip type={comment.author_type} name={comment.author_name} />
        <span className="font-mono text-2xs text-muted-foreground/60">
          {formatTime(comment.created_at)}
        </span>
        {comment.outdated ? (
          <Badge variant="warning" className="gap-1" data-testid="comment-outdated">
            <ClockAlertIcon />
            outdated
          </Badge>
        ) : null}
      </div>
      <p className="whitespace-pre-wrap text-foreground/90 text-sm leading-relaxed">
        {comment.body}
      </p>
    </div>
  );
}

/**
 * A reusable text composer. Submits on the button or ⌘/Ctrl+Enter; cancel clears it. The
 * `<textarea>` is styled from the design system's field tokens (border/radius/ring), not an
 * ad-hoc inline look.
 */
export function CommentComposer({
  placeholder,
  onSubmit,
  onCancel,
  submitLabel = "Comment",
  autoFocus = false,
}: {
  placeholder: string;
  onSubmit: (body: string) => void;
  onCancel?: () => void;
  submitLabel?: string;
  autoFocus?: boolean;
}) {
  const [body, setBody] = useState("");
  const submit = () => {
    const trimmed = body.trim();
    if (!trimmed) return;
    onSubmit(trimmed);
    setBody("");
  };
  return (
    <div className="flex flex-col gap-2 px-3 py-2">
      <textarea
        // biome-ignore lint/a11y/noAutofocus: opened on an explicit user action (Reply / New).
        autoFocus={autoFocus}
        rows={3}
        placeholder={placeholder}
        value={body}
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") submit();
        }}
        className="w-full resize-y rounded-md border border-input bg-background px-2 py-1.5 text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
      />
      <div className="flex items-center gap-2">
        <Button size="xs" onClick={submit} disabled={!body.trim()}>
          {submitLabel}
        </Button>
        {onCancel ? (
          <Button size="xs" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
        ) : null}
      </div>
    </div>
  );
}

/**
 * The standalone composer the diff gutter opens to anchor a NEW comment on a line — or, when
 * `rangeLabel` is set (Finding 51 — a drag-selected RANGE), on a span of lines. `rangeLabel`
 * ("Lines X–Y") makes the plage the comment covers explicit while the human types.
 */
export function NewCommentComposer({
  onSubmit,
  onCancel,
  rangeLabel,
}: {
  onSubmit: (body: string) => void;
  onCancel: () => void;
  rangeLabel?: string;
}) {
  return (
    <div className="my-1 overflow-hidden rounded-lg border border-primary/40 bg-card shadow-sm">
      {rangeLabel ? (
        <div
          data-testid="composer-range"
          className="border-border/60 border-b px-3 pt-2 font-medium text-2xs text-muted-foreground"
        >
          {rangeLabel}
        </div>
      ) : null}
      <CommentComposer
        placeholder={
          rangeLabel ? "Leave a comment on these lines…" : "Leave a comment on this line…"
        }
        submitLabel="Comment"
        onSubmit={onSubmit}
        onCancel={onCancel}
        autoFocus
      />
    </div>
  );
}

/**
 * A full thread: the root comment card, its replies (indented), an actions row (Resolve/Reopen
 * + Reply), and an inline reply composer that opens on Reply. `selected` highlights the thread
 * the panel/diff cross-selection points at.
 */
export function CommentThread({
  thread,
  actions,
  selected,
  onSelect,
}: {
  thread: Thread;
  actions: CommentActions;
  selected?: boolean;
  onSelect?: () => void;
}) {
  const [replying, setReplying] = useState(false);
  const { root, replies } = thread;
  const resolved = root.status === "resolved";

  return (
    <div
      data-testid="comment-thread"
      className={cn(
        "overflow-hidden rounded-lg border bg-card shadow-sm transition-shadow",
        selected ? "border-primary ring-2 ring-primary/30" : "border-border",
        resolved && "opacity-80",
      )}
      onClick={onSelect}
    >
      <CommentCard comment={root} />

      {replies.length > 0 ? (
        <div className="border-border/60 border-t">
          {replies.map((r) => (
            <div key={r.id} className="border-border/40 border-b last:border-b-0">
              <div className="flex items-start gap-1.5 pl-2">
                <CornerDownRightIcon className="mt-3 size-3.5 shrink-0 text-muted-foreground/50" />
                <div className="flex-1">
                  <CommentCard comment={r} />
                </div>
              </div>
            </div>
          ))}
        </div>
      ) : null}

      <div className="flex flex-wrap items-center gap-1.5 border-border/60 border-t bg-muted/30 px-2 py-1.5">
        {resolved ? (
          <>
            <Badge variant="success" className="gap-1">
              <CheckIcon />
              resolved
            </Badge>
            <Button size="xs" variant="ghost" onClick={() => actions.onReopen(root.id)}>
              <RotateCcwIcon />
              Reopen
            </Button>
          </>
        ) : (
          <Button
            size="xs"
            variant="ghost"
            data-testid="resolve-comment"
            onClick={() => actions.onResolve(root.id)}
          >
            <CheckIcon />
            Resolve
          </Button>
        )}

        <Button size="xs" variant="ghost" onClick={() => setReplying((v) => !v)}>
          <CornerDownRightIcon />
          Reply
        </Button>

        {/* PROMOTE a comment into a coder task — the human lâche brut (grill step 1). Only on an
            OPEN thread (a resolved comment needs no work) and only when the panel wired promotion. */}
        {actions.onPromote && !resolved ? (
          <Button
            size="xs"
            variant="ghost"
            data-testid="promote-comment"
            onClick={() => actions.onPromote?.(root.id)}
          >
            <ListTodoIcon />
            Promote to task
          </Button>
        ) : null}
      </div>

      {replying ? (
        <div className="border-border/60 border-t">
          <CommentComposer
            placeholder="Reply…"
            submitLabel="Reply"
            autoFocus
            onSubmit={(body) => {
              actions.onReply(root.id, body);
              setReplying(false);
            }}
            onCancel={() => setReplying(false)}
          />
        </div>
      ) : null}
    </div>
  );
}
