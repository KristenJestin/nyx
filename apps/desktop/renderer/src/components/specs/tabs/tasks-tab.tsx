import { Children, type ReactNode, useMemo } from "react";
import { ChevronRightIcon, LockIcon, UserIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { AuthorIcon } from "@/components/ui/author-icon";
import { Badge, type BadgeProps } from "@/components/ui/badge";
import type { CommentAuthorType } from "@/bridge";

import { priorityMeta, taskStatusMeta } from "../spec-meta";
import { Dot, ProgressBar } from "../spec-primitives";
import { parsePhases, type SpecDetail, type Task } from "../types";

type BadgeVariant = NonNullable<BadgeProps["variant"]>;

// A task `skip` stamps the terminal `cancelled` status (`nyx_core::db::TASK_STATUS_CANCELLED`,
// the sole `cancelled` target for a task). So on this tab a "skipped" task is exactly
// `status === "cancelled"` — segregated into the collapsed "Skipped" section below.
const SKIPPED_STATUS = "cancelled";

// A task is RESOLVED once it leaves the OPEN set — either `done` OR skipped (`cancelled`). This
// is the same set the dependency-blocker logic already treats as "prerequisite satisfied", so
// progress and blocker-resolution share ONE definition of "no longer open". (The ROW's struck
// styling is driven independently by `taskStatusMeta().struck` — the status→look source of truth.)
const RESOLVED_STATUSES = new Set(["done", SKIPPED_STATUS]);

const isSkipped = (t: Task) => t.status === SKIPPED_STATUS;
const isDone = (t: Task) => t.status === "done";
const isFinding = (t: Task) => t.kind === "review";

// PROGRESS FORMULA (finding #34, refine of #30) — the ONE source of truth shared by the phase
// cards AND the roll-up header, so the two can never diverge. Skipped tasks are EXCLUDED from
// BOTH the numerator AND the denominator: progress = done / (total − skipped) = done / active. A
// skip no longer inflates progress (skips + 0 done reads 0%; all active done reads 100%). EDGE
// CASE: a set whose tasks are ALL skipped has no active task (`active === 0`) — treated as
// complete/100% via an explicit guard (never a divide-by-zero / NaN), rendered as a full bar
// with the `complete ? 1 : x` ternary at the call site.
export function progressOf(tasks: readonly Task[]): {
  done: number;
  active: number;
  complete: boolean;
  pct: number;
} {
  const done = tasks.filter(isDone).length;
  const active = tasks.filter((t) => !isSkipped(t)).length; // total − skipped
  const complete = active === 0;
  const pct = complete ? 100 : Math.round((done / active) * 100);
  return { done, active, complete, pct };
}

/**
 * A review ROUND sub-group inside a phase (PDR-0022): the `tasks kind=review` findings of ONE
 * rework pass, rendered under the phase's plan tasks as a distinct sub-zone "N.M" (e.g. 3.1, 3.2).
 * `round` is the 1-based per-phase pass number (`Task.round`); `tasks` are that pass's findings.
 */
export interface RoundGroup {
  readonly round: number;
  readonly tasks: Task[];
}

/**
 * Split a phase's tasks into the PLAN tasks (the decomposition — `round` null/0) and the review
 * ROUND sub-groups (findings of a rework pass — `round ≥ 1`, grouped and returned ASCENDING). This
 * is the ONE place the sub-phase axis (`Task.round`, PDR-0022) is read for the rail/overlay: a
 * finding is a `tasks kind=review` row carrying its `round`, and grouping by it turns the flat pile
 * (findings mixed in with plan tasks, only tagged `finding`) into visible sub-phases "N.1 / N.2…".
 * ORDER within a group is preserved (the incoming task order); the plan keeps every non-review task
 * AND any legacy finding with no round (`round` null/0 ⇒ treated as plan so nothing is ever hidden).
 */
export function splitPhaseTasks(tasks: readonly Task[]): {
  plan: Task[];
  rounds: RoundGroup[];
} {
  const plan: Task[] = [];
  const byRound = new Map<number, Task[]>();
  for (const t of tasks) {
    const round = t.round ?? 0;
    if (round <= 0) {
      plan.push(t);
      continue;
    }
    const list = byRound.get(round) ?? [];
    list.push(t);
    byRound.set(round, list);
  }
  const rounds = [...byRound.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([round, roundTasks]) => ({ round, tasks: roundTasks }));
  return { plan, rounds };
}

/** The total number of rework passes across a Spec — distinct `(phase, round ≥ 1)` groups. */
export function reworkCount(tasks: readonly Task[]): number {
  const seen = new Set<string>();
  for (const t of tasks) {
    const round = t.round ?? 0;
    if (round >= 1) seen.add(`${t.phase ?? "?"}:${round}`);
  }
  return seen.size;
}

/**
 * The PROVENANCE of a review finding — human feedback vs. an auditor/agent. `origin` was dropped
 * from the finding row (v24, PDR-0016); provenance is DERIVED from the `author_type` of the source
 * comment (`from_comment_id`). An unknown/absent source ⇒ `agent` (auditor) — the safe default.
 */
export type Provenance = "human" | "agent";

export function findingProvenance(
  task: Task,
  authorByCommentId?: ReadonlyMap<string, CommentAuthorType>,
): Provenance {
  const id = task.from_comment_id;
  const author = id != null ? authorByCommentId?.get(id) : undefined;
  return author === "human" ? "human" : "agent";
}

// The per-finding provenance glyph look: magenta (primary) + user icon for human feedback, blue
// (info) + bot icon for the auditor. Provenance is surfaced ONLY as this per-row icon (finding #36)
// — never as a round-header label — so there is no user-facing text (and no FR strings) here.
const PROVENANCE_META: Record<Provenance, { icon: "human" | "agent"; tint: string }> = {
  human: { icon: "human", tint: "text-primary" },
  agent: { icon: "agent", tint: "text-info" },
};

/**
 * A finding's SEVERITY badge look — high/critical = destructive (red), medium = warning (amber),
 * low/info = muted. `null` (an un-triaged finding / non-finding task) ⇒ no badge.
 */
export function severityMeta(
  severity: string | null,
): { label: string; variant: BadgeVariant } | null {
  switch (severity) {
    case "critical":
      return { label: "critical", variant: "destructive" };
    case "high":
      return { label: "high", variant: "destructive" };
    case "medium":
      return { label: "med", variant: "warning" };
    case "low":
      return { label: "low", variant: "muted" };
    case "info":
      return { label: "info", variant: "muted" };
    default:
      return null;
  }
}

/** The bare provenance glyph (tinted author icon) — magenta for human, blue/info for the auditor. */
function ProvenanceMark({ provenance }: { provenance: Provenance }) {
  const meta = PROVENANCE_META[provenance];
  return <AuthorIcon type={meta.icon} className={meta.tint} />;
}

/** One task row: status dot, title (+ "blocked by"), severity/type marker, priority, provenance. */
function TaskRow({
  task,
  blocker,
  provenance,
}: {
  task: Task;
  blocker: Task | null;
  provenance?: Provenance;
}) {
  const status = taskStatusMeta(task.status);
  const priority = priorityMeta(task.priority);
  // Mute + strike any status flagged `struck` (done OR cancelled/skipped) — the ONE source of truth
  // (`taskStatusMeta`). This is what stops a skipped task from reading as pending; combined with the
  // attenuated dot AND living in the "Skipped" section, cancelled is unmistakable (≠ todo, ≠ done).
  const struck = status.struck;
  const human = task.executor === "human";
  const finding = isFinding(task);
  const severity = finding ? severityMeta(task.severity) : null;
  return (
    <div className="group flex items-center gap-3 px-3 py-2.5 transition-colors hover:bg-muted/60">
      <Dot tone={status.dot} size="sm" />
      <div className="min-w-0 flex-1">
        <span
          className={cn(
            "block min-w-0 truncate text-sm",
            struck
              ? "text-muted-foreground line-through decoration-foreground/25"
              : "text-foreground",
          )}
        >
          {task.title}
        </span>
        {blocker ? (
          <div className="mt-1 flex items-center gap-1 text-2xs text-destructive">
            <LockIcon className="size-3 shrink-0" />
            <span className="min-w-0 truncate">Blocked by: {blocker.title}</span>
          </div>
        ) : null}
      </div>
      <span className="flex w-20 shrink-0 justify-end">
        {finding ? (
          severity ? (
            <Badge variant={severity.variant} className="font-mono">
              {severity.label}
            </Badge>
          ) : null
        ) : human ? (
          <Badge variant="muted" className="font-mono">
            human
          </Badge>
        ) : task.type_ ? (
          <span className="hidden rounded bg-muted px-1.5 py-0.5 font-mono text-2xs text-muted-foreground/80 sm:inline">
            {task.type_}
          </span>
        ) : null}
      </span>
      <span className="flex w-16 shrink-0 justify-end">
        <Badge variant={priority.variant}>{priority.label}</Badge>
      </span>
      <span className="flex w-5 shrink-0 justify-center text-muted-foreground/70">
        {finding && provenance ? (
          <ProvenanceMark provenance={provenance} />
        ) : human ? (
          <UserIcon className="size-3.5" />
        ) : null}
      </span>
    </div>
  );
}

/**
 * A GROUP inside a phase card = a header (number pastille + name zone + progress bar + fraction)
 * over its task rows. The PHASE group AND every rework ROUND group render through THIS ONE
 * component, so their headers are STRICTLY identical (finding #36): same background, same border,
 * same layout — only the pastille number ("N" for the phase, "N.M" for a round) and the name (the
 * phase name vs. an empty zone for a round) differ. Groups after the first are set off by a filet
 * above (`separated`), so a round reads exactly like the phase with no asymmetric jump. PROGRESS is
 * the group's OWN tally: the phase header counts the PLAN only, each round its own findings.
 */
function TaskGroup({
  phaseGroup,
  roundGroup,
  label,
  name,
  progress,
  separated,
  children,
}: {
  /** Set on a PHASE group (the phase display number) — mutually exclusive with `roundGroup`. */
  phaseGroup?: number;
  /** Set on a rework ROUND group (the 1-based round number) — mutually exclusive with `phaseGroup`. */
  roundGroup?: number;
  label: string;
  name?: string;
  progress: { done: number; active: number; complete: boolean };
  separated: boolean;
  children: ReactNode;
}) {
  const { done, active, complete } = progress;
  return (
    <div
      data-phase-group={phaseGroup}
      data-round-group={roundGroup}
      className={cn(separated && "border-t border-border/60")}
    >
      <div data-group-header="" className="flex items-center gap-3 bg-muted/40 px-3 py-2.5">
        <span
          data-group-label={label}
          className="grid h-6 min-w-6 shrink-0 place-items-center rounded-md bg-muted px-1.5 font-mono text-2xs text-muted-foreground"
        >
          {label}
        </span>
        {name ? (
          <span className="min-w-0 flex-1 truncate font-heading text-sm font-semibold text-foreground">
            {name}
          </span>
        ) : (
          <span className="flex-1" />
        )}
        <div className="flex shrink-0 items-center gap-2">
          <ProgressBar done={complete ? 1 : done} total={complete ? 1 : active} className="w-16" />
          <span
            data-group-tally=""
            className="font-mono text-2xs tabular-nums text-muted-foreground"
          >
            {done}/{active}
          </span>
        </div>
      </div>
      {Children.count(children) > 0 ? (
        <div className="divide-y divide-border/40 border-t border-border/40">{children}</div>
      ) : null}
    </div>
  );
}

/** A phase group card: the PHASE group (header + plan rows) then its rework ROUND groups. */
function PhaseCard({
  index,
  name,
  tasks,
  blockerOf,
  authorByCommentId,
}: {
  index: number;
  name: string;
  tasks: Task[];
  blockerOf: (t: Task) => Task | null;
  authorByCommentId?: ReadonlyMap<string, CommentAuthorType>;
}) {
  // Phase DISPLAY number (1-based position) — the header pastille, reused as the "N" of each round
  // group "N.M".
  const phaseNumber = index + 1;
  // Split the phase into its PLAN tasks (round 0) and its rework ROUND groups (round ≥ 1). The plan
  // tasks sit DIRECTLY under the phase header (no separate round-0 plan sub-header, finding #36);
  // the findings render as their own groups "N.1 / N.2…" below (PDR-0022, proto rail-subphases).
  const { plan, rounds } = splitPhaseTasks(tasks);
  // Skipped PLAN tasks are SEGREGATED out of the phase's active list — the user has moved on from
  // them (finding #30). They live in the collapsed "Skipped" sub-section below, never mixed in.
  const activeTasks = plan.filter((t) => !isSkipped(t));
  const skippedTasks = plan.filter(isSkipped);
  // PHASE bar = the PLAN ONLY (finding #36): findings are excluded here — each round group owns its
  // own bar/fraction below. `progressOf` already drops skipped from numerator AND denominator.
  const planProgress = progressOf(plan);
  return (
    <div className="overflow-hidden rounded-xl border border-border/60 bg-card">
      {/* PHASE group — header (plan-only gauge) then the plan tasks, directly under it. */}
      <TaskGroup
        phaseGroup={phaseNumber}
        label={String(phaseNumber)}
        name={name}
        progress={planProgress}
        separated={false}
      >
        {activeTasks.map((t) => (
          <TaskRow key={t.id} task={t} blocker={blockerOf(t)} />
        ))}
      </TaskGroup>
      {/* Rework ROUND groups "N.1 / N.2…", ascending — each an IDENTICAL group (same header chrome),
          set off by a filet above. The header carries ONLY the round number + its own progress; a
          finding's PROVENANCE shows solely as the per-row icon (human magenta / auditor blue). */}
      {rounds.map((rg) => (
        <TaskGroup
          key={rg.round}
          roundGroup={rg.round}
          label={`${phaseNumber}.${rg.round}`}
          progress={progressOf(rg.tasks)}
          separated
        >
          {rg.tasks.map((t) => (
            <TaskRow
              key={t.id}
              task={t}
              blocker={blockerOf(t)}
              provenance={findingProvenance(t, authorByCommentId)}
            />
          ))}
        </TaskGroup>
      ))}
      {skippedTasks.length > 0 ? (
        <details className="group/skipped border-t border-border/60 bg-muted/20">
          <summary className="flex cursor-pointer list-none select-none items-center gap-2 px-3 py-2 text-2xs font-medium uppercase tracking-wide text-muted-foreground/80 transition-colors hover:text-muted-foreground marker:hidden [&::-webkit-details-marker]:hidden">
            <ChevronRightIcon className="size-3.5 shrink-0 transition-transform group-open/skipped:rotate-90" />
            <span>Skipped</span>
            <span className="font-mono tabular-nums text-muted-foreground/60">
              {skippedTasks.length}
            </span>
          </summary>
          <div className="divide-y divide-border/40 border-t border-border/40">
            {skippedTasks.map((t) => (
              <TaskRow key={t.id} task={t} blocker={blockerOf(t)} />
            ))}
          </div>
        </details>
      ) : null}
    </div>
  );
}

/**
 * `<TasksTab>` — the Spec's task decomposition, grouped by phase (from `spec.phases`, PDR-0022;
 * a single "Tasks" group when flat). The roll-up header carries the rework counter; each phase is a
 * GROUP (header + its plan tasks, directly), and every rework round N.1 / N.2… is an IDENTICAL group
 * (finding #36) — same header chrome, its own plan-independent progress bar/fraction. A finding's
 * PROVENANCE (human vs. auditor) shows ONLY as the per-row icon, derived from `authorByCommentId`
 * (the review comments' author map) — absent ⇒ every finding reads as the auditor (safe default).
 */
export function TasksTab({
  detail,
  authorByCommentId,
}: {
  detail: SpecDetail;
  authorByCommentId?: ReadonlyMap<string, CommentAuthorType>;
}) {
  const { tasks, dependencies, spec } = detail;

  const byId = useMemo(() => {
    const m = new Map<string, Task>();
    for (const t of tasks) m.set(t.id, t);
    return m;
  }, [tasks]);

  // The first unfinished `blocks` prerequisite of a blocked task — what to show as "blocked by".
  const blockerOf = useMemo(() => {
    const blocks = new Map<string, string[]>();
    for (const dep of dependencies) {
      if (dep.type_ !== "blocks") continue;
      const list = blocks.get(dep.task_id) ?? [];
      list.push(dep.depends_on_task_id);
      blocks.set(dep.task_id, list);
    }
    return (t: Task): Task | null => {
      if (t.status !== "blocked") return null;
      for (const prereqId of blocks.get(t.id) ?? []) {
        const prereq = byId.get(prereqId);
        if (prereq && !RESOLVED_STATUSES.has(prereq.status)) return prereq;
      }
      return null;
    };
  }, [dependencies, byId]);

  // Group tasks by phase. A task's `phase` indexes `spec.phases`; a task whose phase is null
  // OR points at no plan entry (legacy/mal-phased data) falls into the "Unphased" group so
  // nothing is ever hidden. A Spec with no phases renders a single flat group.
  const phases = parsePhases(spec.phases);
  const groups = useMemo(() => {
    if (phases.length === 0) {
      return [{ name: "Tasks", tasks: [...tasks] }];
    }
    const byPhase = phases.map((p) => ({
      name: p.name,
      tasks: tasks.filter((t) => t.phase === p.number),
    }));
    // Unphased = null OR a phase number absent from the plan (an orphan — never hidden).
    const unphased = tasks.filter((t) => !phases.some((p) => p.number === t.phase));
    if (unphased.length > 0) byPhase.push({ name: "Unphased", tasks: unphased });
    return byPhase;
  }, [phases, tasks]);

  // Spec-level roll-up via the same shared `progressOf` helper as the phase cards (finding #34):
  // done / (total − skipped), skipped excluded from numerator AND denominator, fully-skipped
  // (activeCount === 0) treated as complete/100% (no NaN).
  const { done: doneCount, active: activeCount, complete, pct } = progressOf(tasks);
  const total = tasks.length;
  const phaseCount = phases.length || 1;
  const reworks = reworkCount(tasks);

  if (total === 0) {
    return (
      <div className="mx-auto max-w-3xl">
        <p className="text-sm text-muted-foreground">No tasks yet for this Spec.</p>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-3xl">
      <div className="mb-5 flex items-center gap-4 rounded-xl border border-border/70 bg-muted/40 px-4 py-3">
        <div className="font-heading text-2xl tabular-nums text-foreground">
          {doneCount}
          <span className="text-lg text-muted-foreground/60">/{activeCount}</span>
        </div>
        <div className="flex-1">
          <div
            data-rollup=""
            className="mb-1.5 flex items-center justify-between text-xs text-muted-foreground"
          >
            <span>
              {doneCount}/{activeCount} tasks · {phaseCount} {phaseCount === 1 ? "phase" : "phases"}{" "}
              ·{" "}
              <span data-rework-count={reworks}>
                {reworks} {reworks === 1 ? "rework" : "reworks"}
              </span>
            </span>
            <span className="font-mono">{pct}%</span>
          </div>
          <ProgressBar
            done={complete ? 1 : doneCount}
            total={complete ? 1 : activeCount}
            tone="bg-success"
          />
        </div>
      </div>

      <div className="flex flex-col gap-3">
        {groups.map((g, i) => (
          <PhaseCard
            key={g.name}
            index={i}
            name={g.name}
            tasks={g.tasks}
            blockerOf={blockerOf}
            authorByCommentId={authorByCommentId}
          />
        ))}
      </div>
    </div>
  );
}
