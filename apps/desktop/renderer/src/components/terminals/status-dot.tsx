import { cva, type VariantProps } from "class-variance-authority";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import {
  DOT_PRESENCE,
  dotAppearTransition,
  dotExitTransition,
} from "@/components/sidebar/dot-motion";

/**
 * The presentational run-state a dot renders — a VISUAL projection of the distinct
 * domain enums (a terminal status, an agent-session status, a command run status). The
 * projection itself lives in `runtime-state.ts`; this component only paints one state.
 *
 * Ported clean from the prototype's `status-dot` against the real design system: every
 * colour is a token class (`bg-info`, `bg-warning`, `bg-success`, `bg-destructive`,
 * `bg-muted-foreground/*`), every size is the token-backed Tailwind scale. No raw hex,
 * no magic px.
 */
export type RunState = "idle" | "running" | "success" | "error" | "waiting" | "closed";

const statusDotVariants = cva("relative flex shrink-0", {
  defaultVariants: { size: "sm" },
  variants: {
    size: {
      xs: "size-1.5",
      sm: "size-2",
      md: "size-2.5",
    },
  },
});

const statusDotCore = cva(
  // `transition-colors` animates the fill between two states (finding: dot colour
  // transition). It uses Tailwind's token-backed default duration/easing — no
  // hard-coded ms or colour — so a state change glides instead of snapping.
  "relative inline-flex h-full w-full rounded-full ring-1 ring-background/40 transition-colors",
  {
    defaultVariants: { state: "idle" },
    variants: {
      state: {
        running: "bg-info animate-pulse",
        waiting: "bg-warning animate-pulse",
        success: "bg-success",
        error: "bg-destructive",
        idle: "bg-muted-foreground/45",
        closed: "bg-muted-foreground/25",
      },
    },
  },
);

const statusDotPing = cva(
  "absolute inline-flex h-full w-full animate-ping rounded-full opacity-75",
  {
    defaultVariants: { state: "idle" },
    variants: {
      state: {
        running: "bg-info/60",
        waiting: "bg-warning/60",
        success: "hidden",
        error: "hidden",
        idle: "hidden",
        closed: "hidden",
      },
    },
  },
);

export type StatusDotSize = VariantProps<typeof statusDotVariants>["size"];

/**
 * The QUIET run-states — the grey / empty ones with nothing worth flagging (`idle`,
 * `closed`). This is the SHARED rule behind "hide the dot when there is nothing to
 * show": a quiet state renders NO dot at all, so the tree rows (terminals/services)
 * AND the project rail dot stay clean until a state actually earns attention. Factored
 * here so the terminal rows and the aggregated project dot never duplicate the test.
 */
export function isQuietState(state: RunState): boolean {
  return state === "idle" || state === "closed";
}

export interface StatusDotProps {
  state: RunState;
  size?: StatusDotSize;
  className?: string;
}

/**
 * A run-state dot. `running` / `waiting` are LIVE states — a pulsing core plus a soft
 * ping halo; the settled states (`success` / `error`) are static fills.
 *
 * A QUIET state (`idle` / `closed`) renders NOTHING (`isQuietState`): a grey dot on
 * every idle row is noise, so the dot only appears once a state is worth flagging. This
 * one rule is SHARED by every dot in the tree and the project rail — see `isQuietState`.
 * The ping is gated to the live states in `statusDotPing`.
 *
 * ENTER/LEAVE ANIMATION (finding 49): because a quiet state renders NOTHING, the dot
 * MOUNTS when a state earns attention and UNMOUNTS when it goes quiet again. That mount/
 * unmount would pop instantly, so it is wrapped in an `<AnimatePresence>` and the visible
 * dot is a `motion.span` that fades + slightly scales IN on appear and OUT on disappear —
 * in ADDITION to the `transition-colors` swap between two live states. Both the presence
 * variants (`DOT_PRESENCE`) and the transitions are the SHARED sidebar `dot-motion`
 * primitives (reused, not duplicated), so this dot animates identically to the sidebar
 * ones and respects `prefers-reduced-motion` (an instant snap when reduced).
 */
export function StatusDot({ state, size, className }: StatusDotProps) {
  const reduced = useReducedMotion();
  const visible = !isQuietState(state);
  // WRAP in `AnimatePresence` so the dot POPS in on appear AND fades/scales OUT on
  // disappear (the prior `return null` killed the node instantly). `initial={false}` on
  // the host means a dot already present on first render (a row that loads already running)
  // does not replay the pop; a dot that MOUNTS later (state leaves quiet) does. The
  // per-phase transition rides ON each target (appear = spring, exit = short fade) so the
  // shared `DOT_PRESENCE` stays a frozen target we never mutate.
  return (
    <AnimatePresence initial={false}>
      {visible ? (
        <motion.span
          // Keyed on `state` so swapping a LIVE running↔waiting (or success↔error) dot
          // re-runs the presence pop, paired with the inner `transition-colors` swap.
          key={state}
          data-state={state}
          initial={DOT_PRESENCE.initial}
          animate={{ ...DOT_PRESENCE.animate, transition: dotAppearTransition(reduced) }}
          exit={{ ...DOT_PRESENCE.exit, transition: dotExitTransition(reduced) }}
          className={cn(statusDotVariants({ size }), className)}
        >
          <span aria-hidden className={statusDotPing({ state })} />
          <span className={statusDotCore({ state })} />
        </motion.span>
      ) : null}
    </AnimatePresence>
  );
}

/**
 * States ordered by how much attention they want; the highest present wins a rollup.
 *
 * `waiting` (an agent BLOCKED on the user — a question/permission prompt) leads: a human is
 * on the critical path, so the yellow "needs you" dot outranks everything else, including a
 * red error. Then `error` (something failed) > `success` (a settled unseen result) >
 * `running` (busy, no action needed) > the quiet `idle`/`closed` (which `isQuietState` hides
 * — no dot). So the rail/accordion rollup reads: yellow(question) > red(error) >
 * green(success) > blue(running) > nothing.
 */
const STATE_PRIORITY: readonly RunState[] = [
  "waiting",
  "error",
  "success",
  "running",
  "idle",
  "closed",
];

/**
 * Reduce a bag of states to ONE representative state — the rollup shown on a project's
 * rail dot (its whole runtime collapsed) or any collapsed group. Empty bag ⇒ `idle`.
 */
export function aggregateState(states: readonly RunState[]): RunState {
  for (const candidate of STATE_PRIORITY) {
    if (states.includes(candidate)) return candidate;
  }
  return "idle";
}
