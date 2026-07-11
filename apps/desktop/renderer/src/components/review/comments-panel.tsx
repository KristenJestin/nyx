import { useEffect, useRef, useState } from "react";
import { AnimatePresence, motion, type Transition, useReducedMotion } from "motion/react";
import { CheckIcon, GlobeIcon, ListTodoIcon, MessageSquareIcon, PlusIcon } from "lucide-react";

import type { ReviewTask } from "@/bridge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/toast";
import { cn } from "@/lib/utils";

import { AuthorChip, type CommentActions, CommentComposer, CommentThread } from "./comment-thread";
import { type CommentThread as Thread, groupThreadsByFile } from "./review-threads";

/**
 * `comments-panel` — the RIGHT zone of the review screen (the prototype's comments panel,
 * absent in phase 3, added now). It reproduces the prototype's PLACEMENT: a header ("Comments"
 * + a status/author filter), a "New comment" affordance for a GLOBAL comment, then the
 * anchored threads GROUPED BY FILE, then the global threads. Rewritten against the real T5
 * model via `useReviewComments` (passed down as props) with the design system's tokens — no
 * proto pixels lifted.
 *
 * A panel row cross-selects the diff: clicking a thread's file/line asks the parent to scroll
 * the diff to it (`onSelectThread`), and the diff's selection highlights the matching row here.
 */

type StatusFilter = "all" | "open" | "resolved";
type AuthorFilter = "all" | "human" | "agent";

/** Finding 50a — the "New comment" composer's grow-in/collapse-out spring (matches the diff's). */
const COMPOSER_T: Transition = { type: "spring", duration: 0.4, bounce: 0.1 };

/** The last path segment (file name) — the panel's only path-shortening. */
const basename = (path: string) => path.split("/").pop() ?? path;

/**
 * A compact row for an anchored thread (file:line + author + a body preview) with the thread
 * ACTIONS underneath. Finding 44: an anchored thread in the panel must ALSO offer "Promote to task"
 * (it was previously only on the inline diff cards + the global threads). The selectable summary is
 * its own button so the Promote button is never nested inside another button (invalid HTML).
 */
function PanelThreadRow({
  thread,
  selected,
  onSelect,
  actions,
}: {
  thread: Thread;
  selected: boolean;
  onSelect: () => void;
  actions: CommentActions;
}) {
  const { root, replies } = thread;
  const resolved = root.status === "resolved";
  return (
    <div
      data-testid="panel-thread-row"
      className={cn(
        "rounded-lg border transition-colors",
        selected
          ? "border-primary bg-primary/5 ring-1 ring-primary/30"
          : "border-border hover:bg-muted/40",
      )}
    >
      <button type="button" onClick={onSelect} className="w-full px-2.5 py-2 text-left">
        <div className="mb-1 flex items-center justify-between gap-2">
          <code className="truncate font-mono text-2xs text-muted-foreground">
            {basename(root.file_path ?? "")}
            {root.line_start != null ? `:${root.line_start}` : ""}
          </code>
          <div className="flex shrink-0 items-center gap-1">
            {root.outdated ? <Badge variant="warning">outdated</Badge> : null}
            {resolved ? <Badge variant="success">resolved</Badge> : null}
          </div>
        </div>
        <div className="mb-1 flex items-center gap-1.5">
          <AuthorChip type={root.author_type} name={root.author_name} />
          {replies.length > 0 ? (
            <span className="text-2xs text-muted-foreground/70">+{replies.length}</span>
          ) : null}
        </div>
        <p className="line-clamp-2 text-foreground/80 text-xs leading-snug">{root.body}</p>
      </button>

      {/* PROMOTE an anchored thread into a coder task — the same grill step 1 the diff cards and the
          global threads offer. Only on an OPEN thread, only when the panel wired promotion. */}
      {actions.onPromote && !resolved ? (
        <div className="flex items-center border-border/60 border-t px-2 py-1">
          <Button
            size="xs"
            variant="ghost"
            data-testid="promote-comment"
            onClick={() => actions.onPromote?.(root.id)}
          >
            <ListTodoIcon />
            Promote to task
          </Button>
        </div>
      ) : null}
    </div>
  );
}

/** A tiny section label with a count + icon (the panel's group heading). */
function SectionLabel({
  icon: Icon,
  count,
  children,
}: {
  icon: typeof GlobeIcon;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-1.5 px-1 pt-1 pb-1.5 font-medium text-2xs text-muted-foreground uppercase tracking-wide">
      <Icon className="size-3" />
      <span>{children}</span>
      <span className="text-muted-foreground/60">{count}</span>
    </div>
  );
}

/** The grill-state → badge presentation (proposed → structured → validated). */
const GRILL_BADGE: Record<
  ReviewTask["grill_state"],
  { variant: "muted" | "info" | "success"; label: string }
> = {
  proposed: { variant: "muted", label: "proposed" },
  structured: { variant: "info", label: "structured" },
  validated: { variant: "success", label: "validated" },
};

/**
 * A compact TASK (= coder task) row: title + optional severity + grill-state badge, and — when
 * the task is `structured` — a HUMAN Validate button (the grill's third step). A `validated`
 * task shows a "ready" marker; a `proposed` one waits on the agent to structure it.
 */
function TaskRow({ task, onValidate }: { task: ReviewTask; onValidate: (id: string) => void }) {
  const badge = GRILL_BADGE[task.grill_state];
  return (
    <div
      data-testid="task-row"
      data-grill-state={task.grill_state}
      className="flex flex-col gap-1 rounded-lg border border-border bg-card px-2.5 py-2"
    >
      <div className="flex items-center justify-between gap-2">
        <p className="line-clamp-2 flex-1 text-foreground/90 text-xs leading-snug">{task.title}</p>
        <div className="flex shrink-0 items-center gap-1">
          {task.severity ? <Badge variant="warning">{task.severity}</Badge> : null}
          <Badge variant={badge.variant}>{badge.label}</Badge>
        </div>
      </div>
      {task.details ? (
        <p className="line-clamp-2 text-2xs text-muted-foreground leading-snug">{task.details}</p>
      ) : null}
      {task.grill_state === "structured" ? (
        <Button
          size="xs"
          variant="outline"
          className="self-start"
          data-testid="validate-task"
          onClick={() => onValidate(task.id)}
        >
          <CheckIcon />
          Validate
        </Button>
      ) : task.grill_state === "validated" ? (
        <span className="inline-flex items-center gap-1 text-2xs text-success">
          <CheckIcon className="size-3" />
          ready for the coder
        </span>
      ) : (
        <span className="text-2xs text-muted-foreground/70">awaiting the agent to structure…</span>
      )}
    </div>
  );
}

export interface CommentsPanelProps {
  readonly anchoredThreads: readonly Thread[];
  readonly globalThreads: readonly Thread[];
  readonly selectedThreadId: string | null;
  readonly onSelectThread: (id: string) => void;
  readonly onAddGlobal: (body: string) => void;
  readonly actions: CommentActions;
  readonly loading: boolean;
  readonly error: string | null;
  /** The promoted-task list of this repo — the grill's output. */
  readonly tasks: readonly ReviewTask[];
  /** The TASKS-hook error (a failed promote/validate) — surfaced so a failure is never silent. */
  readonly tasksError?: string | null;
  /** VALIDATE a structured task — the human's grill step. */
  readonly onValidateTask: (id: string) => void;
}

export function CommentsPanel({
  anchoredThreads,
  globalThreads,
  selectedThreadId,
  onSelectThread,
  onAddGlobal,
  actions,
  loading,
  error,
  tasks,
  tasksError,
  onValidateTask,
}: CommentsPanelProps) {
  const [status, setStatus] = useState<StatusFilter>("all");
  const [author, setAuthor] = useState<AuthorFilter>("all");
  const [addingGlobal, setAddingGlobal] = useState(false);
  const reduce = useReducedMotion();

  // Finding 44: give PROMOTE visible feedback. The promoted task lands `proposed` at the BOTTOM of
  // the panel, so a promote felt like "nothing happened". When the task count GROWS we (a) toast,
  // (b) flash an inline confirmation, and (c) scroll the Tasks section into view. The `prev` ref is
  // seeded on the first settled render so the initial 0→N load does NOT read as a fresh promotion.
  const [justPromoted, setJustPromoted] = useState(false);
  const prevTaskCount = useRef<number | null>(null);
  const tasksRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const prev = prevTaskCount.current;
    prevTaskCount.current = tasks.length;
    if (prev == null || tasks.length <= prev) return;
    setJustPromoted(true);
    toast.success("Promoted to a task", {
      description: "The agent will structure it; validate it here when it’s ready.",
    });
    // `scrollIntoView` is unimplemented in jsdom — guard so the unit tests never throw on it.
    try {
      tasksRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    } catch {
      /* no layout engine (jsdom) — the toast + banner still convey the feedback */
    }
    const t = setTimeout(() => setJustPromoted(false), 6000);
    return () => clearTimeout(t);
  }, [tasks.length]);

  const keep = (t: Thread) =>
    (status === "all" || t.root.status === status) &&
    (author === "all" || t.root.author_type === author);

  const anchored = anchoredThreads.filter(keep);
  const globals = globalThreads.filter(keep);
  const byFile = groupThreadsByFile(anchored);

  const empty = anchored.length === 0 && globals.length === 0;

  return (
    // Content-only: the full-screen review page wraps this in a right-flank, closable
    // `<SidebarShell side="right">` (PDR-0028) that owns the width + the left border + the surface.
    <div aria-label="Comments" className="flex h-full w-full flex-col">
      <div className="flex items-center justify-between gap-2 px-3 pt-3 pb-2">
        <span className="font-medium text-sm">Comments</span>
        <div className="flex items-center gap-1">
          <Segmented
            label="Status filter"
            value={status}
            onChange={setStatus}
            options={[
              { v: "all", label: "All" },
              { v: "open", label: "Open" },
              { v: "resolved", label: "Resolved" },
            ]}
          />
          <Segmented
            label="Author filter"
            value={author}
            onChange={setAuthor}
            options={[
              { v: "all", label: "Any" },
              { v: "human", label: "Human" },
              { v: "agent", label: "Agent" },
            ]}
          />
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 pb-4">
        {/* New GLOBAL (unanchored) comment. Finding 50a — the composer GROWS in (height + fade) on
            open and collapses out on cancel, instead of snapping in/out; reduced-motion is instant. */}
        <AnimatePresence initial={false} mode="wait">
          {addingGlobal ? (
            <motion.div
              key="composer"
              initial={reduce ? false : { opacity: 0, height: 0 }}
              animate={{ opacity: 1, height: "auto" }}
              exit={reduce ? { opacity: 0 } : { opacity: 0, height: 0 }}
              transition={reduce ? { duration: 0 } : COMPOSER_T}
              className="overflow-hidden"
            >
              <div className="overflow-hidden rounded-lg border border-primary/40 bg-card">
                <CommentComposer
                  placeholder="Leave a general comment…"
                  autoFocus
                  onSubmit={(body) => {
                    onAddGlobal(body);
                    setAddingGlobal(false);
                  }}
                  onCancel={() => setAddingGlobal(false)}
                />
              </div>
            </motion.div>
          ) : (
            <motion.div key="new-btn" initial={false}>
              <Button
                size="xs"
                variant="outline"
                className="self-start"
                onClick={() => setAddingGlobal(true)}
              >
                <PlusIcon />
                New comment
              </Button>
            </motion.div>
          )}
        </AnimatePresence>

        {error ? <p className="px-1 text-destructive text-xs">{error}</p> : null}

        {/* Anchored threads, grouped by file. */}
        {[...byFile.entries()].map(([file, threads]) => (
          <div key={file} className="flex flex-col gap-1.5">
            <SectionLabel icon={MessageSquareIcon} count={threads.length}>
              {basename(file)}
            </SectionLabel>
            {threads.map((t) => (
              <PanelThreadRow
                key={t.root.id}
                thread={t}
                selected={selectedThreadId === t.root.id}
                onSelect={() => onSelectThread(t.root.id)}
                actions={actions}
              />
            ))}
          </div>
        ))}

        {/* Global threads, expanded (they have no line to scroll to). */}
        {globals.length > 0 ? (
          <div className="flex flex-col gap-1.5">
            <SectionLabel icon={GlobeIcon} count={globals.length}>
              Global
            </SectionLabel>
            {globals.map((t) => (
              <CommentThread
                key={t.root.id}
                thread={t}
                actions={actions}
                selected={selectedThreadId === t.root.id}
                onSelect={() => onSelectThread(t.root.id)}
              />
            ))}
          </div>
        ) : null}

        {empty && !loading ? (
          <p className="px-1 pt-4 text-muted-foreground/60 text-xs">No comments for this filter.</p>
        ) : null}

        {/* PROMOTED TASKS (finding = task) — the grill's output. A task is a comment promoted
            for the coder; the human validates the agent-structured task here (grill step 3). The
            section carries the promote FEEDBACK banner + any promote/validate ERROR (Finding 44). */}
        {tasks.length > 0 || justPromoted || tasksError ? (
          <div
            ref={tasksRef}
            className="flex flex-col gap-1.5 border-border/60 border-t pt-2"
            data-testid="review-tasks"
          >
            <SectionLabel icon={ListTodoIcon} count={tasks.length}>
              Tasks
            </SectionLabel>
            {justPromoted ? (
              <p
                data-testid="promote-feedback"
                className="rounded-md bg-success/10 px-2 py-1.5 text-2xs text-success"
              >
                Promoted to a task — the agent will structure it, then validate it here.
              </p>
            ) : null}
            {tasksError ? (
              <p data-testid="tasks-error" className="px-1 text-destructive text-xs">
                {tasksError}
              </p>
            ) : null}
            {tasks.map((t) => (
              <TaskRow key={t.id} task={t} onValidate={onValidateTask} />
            ))}
          </div>
        ) : null}
      </div>
    </div>
  );
}

/** A tiny segmented control (the panel's status/author filter), design-token styled. */
function Segmented<T extends string>({
  label,
  value,
  onChange,
  options,
}: {
  label: string;
  value: T;
  onChange: (v: T) => void;
  options: { v: T; label: string }[];
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className="flex items-center gap-0.5 rounded-md border border-input bg-card p-0.5"
    >
      {options.map((opt) => (
        <button
          key={opt.v}
          type="button"
          aria-pressed={value === opt.v}
          onClick={() => onChange(opt.v)}
          className={cn(
            "rounded px-1.5 py-0.5 text-2xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
            value === opt.v
              ? "bg-muted text-foreground"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}
