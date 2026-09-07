import { RotateCcwIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import type { TerminalExit } from "./xterm-instance";

/**
 * `<TerminalExitBar>` — the thin strip a pane shows when its PROCESS IS GONE, and the way back.
 *
 * A pane whose shell exited used to be a dead end: a grey `[process exited with code N]` line in
 * the scrollback and nothing else, with no second shell possible for the life of the record — the
 * only recourse was to close the terminal, losing the pane, its place in the tiling and its
 * history. This bar is the affordance that was missing: it STATES the terminal is finished (and
 * with which code) and offers ONE action, "Relaunch", which spawns a new shell in the SAME pane
 * against the SAME record.
 *
 * DISCREET BY DESIGN. It is a one-line strip pinned to the bottom of the terminal surface, in the
 * muted foreground on the terminal's own background — not a card, not a modal, not a takeover. A
 * terminal that has finished is a normal thing; the pane says so quietly and stays out of the way
 * of the scrollback above it, which is the actual content the user came back for.
 *
 * PRESENTATION ONLY. Every value is a design-system token (`border-border`, `bg-background`,
 * `text-muted-foreground`, the `xs` button size) — no raw colour, no arbitrary length. It decides
 * nothing: whether it is shown, and what relaunching means, belong to the caller and to
 * {@link import("./xterm-instance").XtermInstance.respawn}.
 *
 * The button is not the only way in: with the pane focused, a bare `Enter` inside the emulator
 * relaunches too (the chord lives in the instance's key handler, next to the copy/paste ones).
 */
export function TerminalExitBar({
  exit,
  onRelaunch,
  className,
}: {
  /** The exit this pane is sitting in — its code, and whether a relaunch already failed. */
  exit: TerminalExit;
  /** Spawn a new shell in this same pane / record. */
  onRelaunch: () => void;
  className?: string;
}) {
  const reduced = useReducedMotion();
  // "Exited · code 0"; a backend that could not read a status says only "Exited" rather than
  // inventing a number.
  const label = exit.code === null ? "Exited" : `Exited · code ${exit.code}`;

  return (
    <motion.div
      data-terminal-exit-bar=""
      role="status"
      initial={{ opacity: 0, y: 4 }}
      animate={{ opacity: 1, y: 0 }}
      transition={reduced ? { duration: 0 } : { type: "spring", stiffness: 520, damping: 38 }}
      className={cn(
        "absolute inset-x-0 bottom-0 z-10 flex items-center justify-between gap-2 border-t border-border bg-background px-3 py-1.5",
        className,
      )}
    >
      <span className="truncate text-muted-foreground text-xs select-none">
        {label}
        {exit.relaunchFailed ? (
          // The relaunch is best-effort (a sidecar that is busy / down answers nothing), and a
          // button that silently does nothing twice is worse than one that says it did not work.
          <span className="text-destructive"> · relance impossible</span>
        ) : null}
      </span>
      <Button size="xs" variant="outline" onClick={onRelaunch}>
        <RotateCcwIcon />
        Relaunch
      </Button>
    </motion.div>
  );
}

export default TerminalExitBar;
