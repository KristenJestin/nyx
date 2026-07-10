import { useMemo } from "react";
import { LockIcon, UserIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";

import { priorityMeta, taskStatusMeta } from "../spec-meta";
import { Dot, ProgressBar } from "../spec-primitives";
import { parsePhases, type SpecDetail, type Task } from "../types";

const DONE_STATUSES = new Set(["done", "cancelled"]);

/** One task row: status dot, title (+ "blocked by"), type/human marker, priority, executor. */
function TaskRow({ task, blocker }: { task: Task; blocker: Task | null }) {
  const status = taskStatusMeta(task.status);
  const priority = priorityMeta(task.priority);
  const done = task.status === "done";
  const human = task.executor === "human";
  return (
    <div className="group flex items-center gap-3 px-3 py-2.5 transition-colors hover:bg-muted/60">
      <Dot tone={status.dot} size="sm" />
      <div className="min-w-0 flex-1">
        <span
          className={cn(
            "block min-w-0 truncate text-sm",
            done
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
        {human ? (
          <Badge variant="muted" className="font-mono">
            human
          </Badge>
        ) : task.kind === "review" ? (
          <Badge variant="warning" className="font-mono">
            finding
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
        {human ? <UserIcon className="size-3.5" /> : null}
      </span>
    </div>
  );
}

/** A phase group card: header (index + name + state + progress) then its task rows. */
function PhaseCard({
  index,
  name,
  tasks,
  blockerOf,
}: {
  index: number;
  name: string;
  tasks: Task[];
  blockerOf: (t: Task) => Task | null;
}) {
  const done = tasks.filter((t) => t.status === "done").length;
  const total = tasks.length;
  return (
    <div className="overflow-hidden rounded-xl border border-border/60 bg-card">
      <div className="flex items-center gap-3 bg-muted/40 px-3 py-2.5">
        <span className="grid size-6 shrink-0 place-items-center rounded-md bg-muted font-mono text-2xs text-muted-foreground">
          {index + 1}
        </span>
        <span className="min-w-0 flex-1 truncate font-heading text-sm font-semibold text-foreground">
          {name}
        </span>
        <div className="flex shrink-0 items-center gap-2">
          <ProgressBar done={done} total={total} className="w-16" />
          <span className="font-mono text-2xs tabular-nums text-muted-foreground">
            {done}/{total}
          </span>
        </div>
      </div>
      <div className="divide-y divide-border/40 border-t border-border/50">
        {tasks.map((t) => (
          <TaskRow key={t.id} task={t} blocker={blockerOf(t)} />
        ))}
      </div>
    </div>
  );
}

/**
 * `<TasksTab>` — the Spec's task decomposition, grouped by phase (from `spec.phases`, PDR-0022;
 * a single "Tasks" group when flat). Each task shows its status, type/human/finding marker,
 * priority, and — for a `blocked` task — what it is blocked by (from the dependency edges).
 * Placement reproduced from the prototype's `tasks-tab` (summary header + phase cards + rows);
 * consumes `detail.tasks` + `detail.dependencies` (the phase-2 task model).
 */
export function TasksTab({ detail }: { detail: SpecDetail }) {
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
        if (prereq && !DONE_STATUSES.has(prereq.status)) return prereq;
      }
      return null;
    };
  }, [dependencies, byId]);

  // Group tasks by phase. A task's `phase` indexes `spec.phases`; unphased tasks (or a Spec
  // with no phases) fall into a single flat group so nothing is hidden.
  const phases = parsePhases(spec.phases);
  const groups = useMemo(() => {
    if (phases.length === 0) {
      return [{ name: "Tasks", tasks: [...tasks] }];
    }
    const byPhase = phases.map((p) => ({
      name: p.name,
      tasks: tasks.filter((t) => t.phase === p.number),
    }));
    const unphased = tasks.filter((t) => t.phase == null);
    if (unphased.length > 0) byPhase.push({ name: "Unphased", tasks: unphased });
    return byPhase;
  }, [phases, tasks]);

  const doneCount = tasks.filter((t) => t.status === "done").length;
  const total = tasks.length;
  const pct = total ? Math.round((doneCount / total) * 100) : 0;

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
          <span className="text-lg text-muted-foreground/60">/{total}</span>
        </div>
        <div className="flex-1">
          <div className="mb-1.5 flex items-center justify-between text-xs text-muted-foreground">
            <span>
              Tasks done · {phases.length || 1} {phases.length === 1 ? "phase" : "phases"}
            </span>
            <span className="font-mono">{pct}%</span>
          </div>
          <ProgressBar done={doneCount} total={total} tone="bg-success" />
        </div>
      </div>

      <div className="flex flex-col gap-3">
        {groups.map((g, i) => (
          <PhaseCard key={g.name} index={i} name={g.name} tasks={g.tasks} blockerOf={blockerOf} />
        ))}
      </div>
    </div>
  );
}
