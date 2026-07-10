/**
 * Display lookups for the Spec/task enums — the ONE source of truth for how a canonical
 * `spec_status` / `priority` / `task_status` reads in the Specs UI (board + detail).
 *
 * DESIGN-SYSTEM DISCIPLINE: every colour here is a design-system TOKEN class (the CSS
 * variables declared in `globals.css`: `--success` / `--warning` / `--info` /
 * `--destructive` / `--muted-foreground`, surfaced by Tailwind as `bg-*`/`text-*`). There
 * is NO raw hex / rgb — the look comes from the project's tokens, not the prototype's
 * pixels. `variant` is a real `<Badge>` variant (`badge.tsx`: `muted`/`info`/`warning`/
 * `success`), so the status/priority chips compose the existing primitive.
 */
import type { BadgeProps } from "@/components/ui/badge";

import type { SpecPriority, SpecStatus, TaskStatus } from "./types";

type BadgeVariant = NonNullable<BadgeProps["variant"]>;

export interface StatusMeta {
  /** Human label (the column header, the status chip). */
  readonly label: string;
  /** A solid dot colour — a design-system token `bg-*` class. */
  readonly dot: string;
  /** The `<Badge>` variant for the status chip. */
  readonly variant: BadgeVariant;
}

/**
 * The board columns, in the fixed order they render left→right. Every canonical
 * `spec_status` maps to exactly one column; the board groups the project's Specs by it.
 */
export const STATUS_ORDER: SpecStatus[] = [
  "draft",
  "ready",
  "in_progress",
  "blocked",
  "in_review",
  "completed",
  "cancelled",
  "archived",
];

/** Status columns collapsed by default on the board (the low-signal terminal states). */
export const DEFAULT_COLLAPSED_STATUSES: readonly SpecStatus[] = ["draft", "cancelled", "archived"];

export const STATUS_META: Record<SpecStatus, StatusMeta> = {
  draft: { label: "Draft", dot: "bg-muted-foreground/50", variant: "muted" },
  ready: { label: "Ready", dot: "bg-info", variant: "info" },
  in_progress: { label: "In progress", dot: "bg-warning", variant: "warning" },
  blocked: { label: "Blocked", dot: "bg-destructive", variant: "muted" },
  in_review: { label: "In review", dot: "bg-info", variant: "info" },
  completed: { label: "Done", dot: "bg-success", variant: "success" },
  cancelled: { label: "Cancelled", dot: "bg-muted-foreground/40", variant: "muted" },
  archived: { label: "Archived", dot: "bg-muted-foreground/30", variant: "muted" },
};

/** Resolve a status meta from a raw wire string, defaulting to `draft` for an unknown. */
export function statusMeta(status: string): StatusMeta {
  return STATUS_META[status as SpecStatus] ?? STATUS_META.draft;
}

export interface PriorityMeta {
  readonly label: string;
  /** A solid dot colour — a design-system token `bg-*` class. */
  readonly tone: string;
  readonly variant: BadgeVariant;
}

export const PRIORITY_ORDER: SpecPriority[] = ["urgent", "high", "med", "low"];

export const PRIORITY_META: Record<SpecPriority, PriorityMeta> = {
  urgent: { label: "Urgent", tone: "bg-destructive", variant: "warning" },
  high: { label: "High", tone: "bg-warning", variant: "warning" },
  med: { label: "Medium", tone: "bg-info", variant: "info" },
  low: { label: "Low", tone: "bg-muted-foreground/40", variant: "muted" },
};

/** Resolve a priority meta from a raw wire string, defaulting to `med`. */
export function priorityMeta(priority: string): PriorityMeta {
  return PRIORITY_META[priority as SpecPriority] ?? PRIORITY_META.med;
}

/** The task-status dot colour — a design-system token, reused by the tasks tab. */
export const TASK_STATUS_META: Record<TaskStatus, { label: string; dot: string }> = {
  todo: { label: "To do", dot: "bg-muted-foreground/40" },
  ready: { label: "Ready", dot: "bg-info" },
  in_progress: { label: "In progress", dot: "bg-warning" },
  blocked: { label: "Blocked", dot: "bg-destructive" },
  in_review: { label: "In review", dot: "bg-info" },
  done: { label: "Done", dot: "bg-success" },
  cancelled: { label: "Cancelled", dot: "bg-muted-foreground/30" },
};

/** Resolve a task-status meta from a raw wire string, defaulting to `todo`. */
export function taskStatusMeta(status: string): { label: string; dot: string } {
  return TASK_STATUS_META[status as TaskStatus] ?? TASK_STATUS_META.todo;
}

/** The human key of a Spec: `KEY-number` (e.g. `NYX-3`), the Linear identity. */
export function specKey(projectKey: string, number: number): string {
  return projectKey ? `${projectKey}-${number}` : `#${number}`;
}
