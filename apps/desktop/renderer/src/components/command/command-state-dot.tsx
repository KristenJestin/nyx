import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { CrossfadeFill } from "@/components/sidebar/run-state";
import type { ExecState } from "@/components/sidebar/use-terminals";

/**
 * The four run states the dot encodes, by COLOUR token + MOTION — both axes, so
 * the states are never confusable (the T9 requirement that `running` and
 * `success` are visually distinct: blue+motion vs green static):
 *
 *  - `idle`    → gray  (`--muted-foreground`), STATIC.
 *  - `running` → BLUE  (`--info`),        ANIMATED (a soft pulse).
 *  - `success` → GREEN (`--success`),     STATIC.
 *  - `error`   → RED   (`--destructive`), STATIC.
 *
 * Colours are design-system tokens only (no raw hex), painted by the SHARED
 * `<CrossfadeFill>` (the same colour layer the sidebar dots use), so a state SWAP
 * cross-fades the token rather than hard-cutting it. The `running` breathing pulse
 * is driven by Motion (`motion/react`) — chrome, never the xterm viewport.
 */

export interface CommandStateDotProps {
  state: ExecState;
  className?: string;
}

/**
 * `<CommandStateDot>` — the run-state dot for the COMMAND VIEW header (T9).
 * Distinct from the sidebar's lightweight `<StatusDot>`: here the `running` pulse
 * is driven by **Motion** (an opacity/scale breathing loop) rather than Tailwind's
 * `animate-pulse`, per the project ground rule (Base UI + Motion for chrome) and
 * the task's "animations via motion/react" criterion. The animation honours
 * `prefers-reduced-motion` (it collapses to a static dot). ONLY `running`
 * animates; `idle`/`success`/`error` are static — so `running` (blue + motion)
 * and `success` (green + static) read as unmistakably different states.
 */
export function CommandStateDot({ state, className }: CommandStateDotProps) {
  const reduced = useReducedMotion();
  const isRunning = state === "running";
  // Motion drives the running pulse: a gentle opacity + scale breathing loop.
  // Reduced motion (or any non-running state) → a static dot (no `animate` target).
  const animate =
    isRunning && !reduced
      ? { opacity: [1, 0.45, 1], scale: [1, 0.85, 1] }
      : { opacity: 1, scale: 1 };

  return (
    <motion.span
      role="status"
      aria-label={`Command status: ${state}`}
      data-state={state}
      data-animated={isRunning && !reduced ? "" : undefined}
      animate={animate}
      transition={
        isRunning && !reduced
          ? { duration: 1.4, repeat: Infinity, ease: "easeInOut" }
          : { duration: 0 }
      }
      // `relative` anchors the absolute colour layer; the host owns the SHAPE + a11y +
      // the Motion-driven running pulse, `<CrossfadeFill>` owns the cross-fading colour.
      className={cn("relative inline-block size-2.5 shrink-0 rounded-full", className)}
    >
      <CrossfadeFill state={state} />
    </motion.span>
  );
}
