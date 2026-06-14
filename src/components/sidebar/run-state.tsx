import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import type { ExecState } from "./use-terminals";

/**
 * The RUN-STATE channel (finding 01KV305BGS69RWCSWCAF0KD2SJ) — ORTHOGONAL to the
 * selection channel (the ActiveRail). Two presentational primitives drive it from
 * an `exec_state` prop:
 *
 *  - `<StatusDot>`         — a small filled dot in the LEAD position of a COMMAND
 *    row, colored by state (gray idle / blue-pulse running / green success / red
 *    error).
 *  - `<TerminalStateBadge>` — a tiny ring-punched corner badge on a TERMINAL
 *    row's glyph, with NOTIFICATION/UNREAD semantics: idle → no badge; running →
 *    blue pulsing; success → green static; error → red static. Shown ONLY on a
 *    NON-active terminal (selecting/viewing it CLEARS the badge — the unread
 *    model), though a still-running terminal MAY keep the blue pulse even when
 *    active.
 *
 * Colors come from design-system tokens only: `--muted-foreground` (idle),
 * `--info` (running), `--success`, `--destructive`. The "running" pulse (Tailwind's
 * built-in `animate-pulse` — a soft breathing halo) + the badge "pop" on appear
 * (driven by Motion, reduced-motion aware) are the only motion here; chrome-only.
 *
 * SCOPE NOTE: this PRD builds the COMPONENTS + the clear-on-select logic; NO
 * backend feeds real states yet (that is PRD 01KV300RVJ0WSVQ7K57KS37MX9), so
 * live every state is `idle`. The components must still render all four states
 * correctly from the prop — exercised in tests/stories.
 */

/** Background token class for a state's dot/badge fill. */
function stateBgClass(state: ExecState): string {
  switch (state) {
    case "running":
      return "bg-info";
    case "success":
      return "bg-success";
    case "error":
      return "bg-destructive";
    case "idle":
    default:
      return "bg-muted-foreground/50";
  }
}

export interface StatusDotProps {
  state: ExecState;
  className?: string;
  /** Optional ref to the dot element — lets a command row anchor the sliding rail. */
  ref?: React.Ref<HTMLSpanElement>;
}

/**
 * `<StatusDot>` — the lead-position run-state dot for a COMMAND row. Renders for
 * every state including idle (a command row always reserves the dot, unlike a
 * terminal badge which is suppressed when idle). The running state pulses.
 */
export function StatusDot({ state, className, ref }: StatusDotProps) {
  return (
    <span
      ref={ref}
      role="status"
      aria-label={`Status: ${state}`}
      data-state={state}
      className={cn(
        "inline-block size-2 shrink-0 rounded-full",
        stateBgClass(state),
        state === "running" && "motion-safe:animate-pulse",
        className,
      )}
    />
  );
}

export interface TerminalStateBadgeProps {
  state: ExecState;
  /**
   * Whether the terminal this badge belongs to is the ACTIVE (selected/viewed)
   * one. The badge is the UNREAD indicator, so an active terminal CLEARS it —
   * `idle`/`success`/`error` render nothing when active. The exception is
   * `running`, which MAY keep its pulse even when active (it signals "still
   * running"), per the finding.
   */
  active?: boolean;
  className?: string;
}

/**
 * `<TerminalStateBadge>` — the ~6px ring-punched corner badge on a terminal row's
 * glyph. Position it on a `relative` glyph wrapper; it pins bottom-right and
 * "punches" a ring out of the row background (`ring-sidebar`) so it reads as a
 * notification dot sitting on the glyph.
 *
 * UNREAD MODEL:
 *  - `idle`                 → NO badge (nothing to notify).
 *  - active (selected/read) → NO badge for idle/success/error (cleared on read);
 *    `running` keeps its blue pulse (still-running signal).
 *  - otherwise (unread)     → running (blue, pulse) / success (green) / error
 *    (red), each popping in on appear.
 *
 * Returns `null` when there is nothing to show — the lead glyph renders alone.
 */
export function TerminalStateBadge({ state, active = false, className }: TerminalStateBadgeProps) {
  const reduced = useReducedMotion();

  // Idle never shows a badge. An active terminal clears the unread badge for the
  // settled states, but a still-running one keeps its pulse.
  if (state === "idle") return null;
  if (active && state !== "running") return null;

  return (
    <motion.span
      role="status"
      aria-label={`Terminal status: ${state}`}
      data-state={state}
      // Badge "pop" on APPEAR, driven by Motion (reduced-motion → no scale/fade,
      // it just snaps in). A snappy spring matches the proto's overshoot pop.
      initial={reduced ? false : { scale: 0, opacity: 0 }}
      animate={{ scale: 1, opacity: 1 }}
      transition={reduced ? { duration: 0 } : { type: "spring", stiffness: 700, damping: 22 }}
      className={cn(
        // ~6px badge, bottom-right corner of the glyph, ring-punched against the
        // sidebar background so it reads as a separate notification dot.
        "absolute -right-0.5 -bottom-0.5 size-1.5 rounded-full ring-2 ring-sidebar",
        stateBgClass(state),
        // Running additionally pulses (soft breathing halo via Tailwind's built-in).
        state === "running" && "motion-safe:animate-pulse",
        className,
      )}
    />
  );
}
