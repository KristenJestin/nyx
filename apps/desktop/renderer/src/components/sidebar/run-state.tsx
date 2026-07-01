import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import {
  DOT_PRESENCE,
  dotAppearTransition,
  dotCrossfadeTransition,
  dotExitTransition,
} from "./dot-motion";
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

/**
 * The terminal-badge state SUPERSET: every {@link ExecState} plus `"waiting"`, the
 * AGENT-only "blocked on the user" state (an `AskUserQuestion` tool in flight, or a
 * permission/elicitation prompt). It maps to the YELLOW (`--warning`) badge and is a LIVE
 * state like `running` (never gated by `unread`), so the user sees at a glance that Claude
 * is waiting on them rather than busy working. A NON-agent terminal never produces it.
 */
export type BadgeState = ExecState | "waiting";

/** Background token class for a state's dot/badge fill. */
function stateBgClass(state: BadgeState): string {
  switch (state) {
    case "running":
      return "bg-info";
    case "waiting":
      return "bg-warning";
    case "success":
      return "bg-success";
    case "error":
      return "bg-destructive";
    case "idle":
    default:
      return "bg-muted-foreground/50";
  }
}

/**
 * `<CrossfadeFill>` — the COLOUR layer shared by every run-state dot/badge here
 * (and by `<CommandStateDot>` in the command view). The host element owns the
 * SHAPE, the a11y role, `data-state`, and the running pulse; this fills it with
 * the state's TOKEN colour and CROSS-FADES whenever `state` changes.
 *
 * HOW: an `AnimatePresence mode="popLayout"` keyed on `state`. On a state change
 * the old colour layer EXITS (fades out) while the incoming one ENTERS (fades in),
 * so a `running`→`waiting`→`success`/`error` swap — including to/from the yellow
 * `waiting` — reads as a colour cross-fade, never an animated `backgroundColor`
 * (the fills are `oklch(...)` design-system tokens; we keep the Tailwind token
 * CLASSES and overlap two stacked layers instead — see `dot-motion.ts`).
 *
 * The layer is `aria-hidden` + `absolute inset-0`: purely decorative paint over
 * the host, which keeps the single `role="status"` in the a11y tree. Reduced
 * motion ⇒ `dotCrossfadeTransition` returns `{ duration: 0 }` so the colour snaps.
 */
export function CrossfadeFill({ state }: { state: BadgeState }) {
  const reduced = useReducedMotion();
  return (
    <AnimatePresence initial={false} mode="popLayout">
      <motion.span
        key={state}
        aria-hidden
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={dotCrossfadeTransition(reduced)}
        className={cn("absolute inset-0 rounded-full", stateBgClass(state))}
      />
    </AnimatePresence>
  );
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
        // The host owns the SHAPE + a11y + pulse; `relative` anchors the absolute
        // colour layer. `state` is always present here (a command row reserves the
        // dot for every state, idle included), so there is no appear/disappear —
        // only the colour cross-fade between states.
        "relative inline-block size-2 shrink-0 rounded-full",
        state === "running" && "motion-safe:animate-pulse",
        className,
      )}
    >
      <CrossfadeFill state={state} />
    </span>
  );
}

export interface TerminalStateBadgeProps {
  /**
   * The badge state — every {@link ExecState} plus the agent-only `"waiting"` (yellow,
   * "blocked on the user"). `running`/`waiting` are LIVE states (always shown, pulsing);
   * `success`/`error` are settled NOTIFICATIONS (shown only while `unread` + not active);
   * `idle` shows nothing.
   */
  state: BadgeState;
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
 *  - `waiting`         → ALWAYS shown (yellow, pulsing) — the agent-only "blocked on the
 *    user" live state; like `running` it is a live state, never gated by `unread`.
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

  // `running` and `waiting` are LIVE states — a pulsing dot shown ALWAYS (never gated by
  // `unread`/`active`), so a busy or blocked agent is always visible.
  const isLive = state === "running" || state === "waiting";

  // VISIBILITY DECISION — unchanged from the prior round. Idle never shows a badge. A live
  // state (`running`/`waiting`) always shows. A settled result (success/error) shows ONLY
  // while unread AND the terminal is NOT active: it is a notification for a terminal you are
  // not looking at, so the ACTIVE (viewed) terminal never shows it — this kills the
  // green/red "flash" when an instant command on the active terminal settles unread for a
  // frame before the active-settle mark-read clears the flag. The persisted `unread` flag
  // still gates re-deselect (user story #3): once read it stays hidden even after the
  // terminal becomes inactive again.
  const shown = state !== "idle" && (isLive || (unread && !active));

  // WRAP in `AnimatePresence` so the badge POPS in on appear AND fades/scales OUT on
  // disappear (the prior round's bare `return null` killed the node instantly). The
  // resolvers in `dot-motion.ts` honour reduced motion (instant snap) and are shared with
  // the dots so all three animate identically. `initial={false}` is on the host: the
  // appear pop is driven by `DOT_PRESENCE.initial`, but a badge already mounted on first
  // render (e.g. a row that loads already running) must not replay the pop.
  return (
    <AnimatePresence initial={false}>
      {shown && (
        <motion.span
          // Keyed on `state` so swapping a LIVE running↔waiting badge (or success↔error)
          // re-runs the presence pop rather than silently morphing — paired with the
          // inner colour cross-fade.
          key={state}
          role="status"
          aria-label={`Terminal status: ${state}`}
          data-state={state}
          // Badge "pop" on APPEAR + fade/scale OUT on EXIT, driven by Motion via the
          // shared `dot-motion` presence target + reduced-motion-aware transitions. The
          // per-phase transition rides ON each target (appear = spring, exit = short fade)
          // — DOT_PRESENCE stays a frozen shared target; we never mutate it.
          initial={DOT_PRESENCE.initial}
          animate={{ ...DOT_PRESENCE.animate, transition: dotAppearTransition(reduced) }}
          exit={{ ...DOT_PRESENCE.exit, transition: dotExitTransition(reduced) }}
          className={cn(
            // ~6px badge, bottom-right corner of the glyph, ring-punched against the
            // sidebar background so it reads as a separate notification dot. `relative`
            // is implied by the absolute corner pin; the colour layer pins to it.
            "absolute -right-0.5 -bottom-0.5 size-1.5 rounded-full ring-2 ring-sidebar",
            // Live states (running/waiting) pulse (soft breathing halo via Tailwind's built-in).
            isLive && "motion-safe:animate-pulse",
            className,
          )}
        >
          <CrossfadeFill state={state} />
        </motion.span>
      )}
    </AnimatePresence>
  );
}
