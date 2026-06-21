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
 *    blue pulsing; success → green static; error → red static. A settled
 *    (`success`/`error`) badge is shown ONLY while the terminal's PERSISTED
 *    `exec_state_unread` flag is set — NOT merely while it is non-active. This is
 *    the load-bearing PRD-2.1 refactor: viewing a terminal calls the backend
 *    mark-read (clearing `exec_state_unread`), so the settled badge stays hidden
 *    even after the user re-deselects the terminal (user story #3) — a purely
 *    `active`-driven badge would WRONGLY re-appear on re-deselect. A still-running
 *    terminal MAY keep the blue pulse even when active (it is a live state, not a
 *    notification, so it is never gated by `unread`).
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
  /** The VISIBLE state — drives the dot fill + the running pulse. */
  state: ExecState;
  /**
   * The FACTUAL run state, when it differs from the visible `state`. Used by a
   * command row whose SETTLED badge is hidden after an acknowledge (`state` reverts
   * to `idle` so the badge disappears) while the row must still REFLECT the true
   * last-run outcome: `data-state` + the a11y label carry this factual value. When
   * omitted, it falls back to `state` (the common case — visible == factual).
   */
  factualState?: ExecState;
  className?: string;
  /** Optional ref to the dot element — lets a command row anchor the sliding rail. */
  ref?: React.Ref<HTMLSpanElement>;
}

/**
 * `<StatusDot>` — the lead-position run-state dot for a COMMAND row. Renders for
 * every state including idle (a command row always reserves the dot, unlike a
 * terminal badge which is suppressed when idle). The running state pulses. The FILL
 * follows the visible `state`; `data-state` reports the `factualState` (defaulting to
 * `state`) so the factual outcome stays observable even when a settled badge is
 * hidden on acknowledge.
 */
export function StatusDot({ state, factualState, className, ref }: StatusDotProps) {
  const factual = factualState ?? state;
  return (
    <span
      ref={ref}
      role="status"
      aria-label={`Status: ${factual}`}
      data-state={factual}
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
   * Whether the terminal's SETTLED result is UNREAD — mirrors the persisted
   * `exec_state_unread` flag on the record (PRD-2.1). This — NOT `active` — drives
   * settled-state visibility: a `success`/`error` badge shows ONLY while unread,
   * so once the user views the terminal (backend mark-read clears the flag) the
   * badge stays hidden even after re-deselecting (user story #3). Ignored for
   * `running` (a live state, never a notification) and `idle` (no badge).
   */
  unread?: boolean;
  /**
   * Whether the terminal this badge belongs to is the ACTIVE (visible) one. Kept
   * ONLY so the still-`running` pulse is unambiguous when active; it no longer
   * gates settled-state visibility (that is `unread`'s job now).
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
 * UNREAD MODEL (persisted-flag-driven — PRD-2.1):
 *  - `idle`            → NO badge (nothing to notify).
 *  - `running`         → ALWAYS shown (blue, pulsing), even when active — it is a
 *    live state, not a notification, so it is never gated by `unread`.
 *  - `success`/`error` → shown ONLY while `unread` (the persisted
 *    `exec_state_unread` flag). Once read it stays hidden even when the terminal
 *    later becomes inactive again — the visibility no longer depends on `active`.
 *
 * Returns `null` when there is nothing to show — the lead glyph renders alone.
 */
export function TerminalStateBadge({
  state,
  unread = false,
  active = false,
  className,
}: TerminalStateBadgeProps) {
  const reduced = useReducedMotion();

  // Idle never shows a badge. `running` always shows (a live state, not a
  // notification). A settled result (success/error) shows ONLY while unread AND
  // the terminal is NOT active: it is a notification for a terminal you are not
  // looking at, so the ACTIVE (viewed) terminal never shows it — this kills the
  // green/red "flash" when an instant command on the active terminal settles
  // unread for a frame before the active-settle mark-read clears the flag. The
  // persisted `unread` flag still gates re-deselect (user story #3): once read it
  // stays hidden even after the terminal becomes inactive again.
  if (state === "idle") return null;
  if (state !== "running" && (!unread || active)) return null;

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
