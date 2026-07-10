/**
 * Small REUSABLE presentational primitives shared across the Specs UI (board + detail):
 * a status `Dot`, a `ProgressBar`, and a `MicroLabel`. Built clean against the design
 * system — every colour/size is a token class (`bg-*` off `globals.css` variables, the
 * `text-2xs` micro-type token, `--radius-*` radii), NO raw hex/px. These are the shared
 * building blocks the prototype had as `status-dot` / `progress-bar` / `micro-label`,
 * rewritten against the real tokens rather than copied.
 */
import type * as React from "react";

import { cn } from "@/lib/utils";

const DOT_SIZE = {
  xs: "size-1.5",
  sm: "size-2",
  md: "size-2.5",
} as const;

/** A solid status dot. `tone` is a design-system `bg-*` token class (see `spec-meta.ts`). */
export function Dot({
  tone,
  size = "md",
  className,
}: {
  tone: string;
  size?: keyof typeof DOT_SIZE;
  className?: string;
}) {
  return (
    <span
      aria-hidden
      className={cn("inline-block shrink-0 rounded-full", DOT_SIZE[size], tone, className)}
    />
  );
}

/**
 * A thin progress bar (`done`/`total`). The track is `bg-muted`, the fill a token (`tone`,
 * defaulting to `bg-foreground/55`). Renders an empty track when `total` is 0.
 */
export function ProgressBar({
  done,
  total,
  tone = "bg-foreground/55",
  className,
}: {
  done: number;
  total: number;
  tone?: string;
  className?: string;
}) {
  const pct = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
  return (
    <div
      className={cn("h-1.5 w-full overflow-hidden rounded-full bg-muted", className)}
      role="progressbar"
      aria-valuenow={done}
      aria-valuemin={0}
      aria-valuemax={total}
    >
      <div
        className={cn("h-full rounded-full transition-[width]", tone)}
        style={{ width: `${pct}%` }}
      />
    </div>
  );
}

/** An uppercase micro-label (the dense meta caption). Uses the `text-2xs` type token. */
export function MicroLabel({
  className,
  children,
}: {
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <span
      className={cn(
        "font-medium text-2xs uppercase tracking-wide text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}
