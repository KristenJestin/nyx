import { useCallback, useRef, useState } from "react";
import { PlayIcon, RotateCcwIcon, SquareIcon } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import type { ExecState } from "@/components/sidebar/use-terminals";
import { CommandStateDot } from "./command-state-dot";

export interface CommandControlsProps {
  /** The `command_instances.id` the controls drive. */
  instanceId: string;
  /** The instance's current run state (drives the dot + which buttons are active). */
  state: ExecState;
  /** Optional label shown next to the dot (the command name). */
  label?: string;
  className?: string;
  /**
   * Called after a lifecycle command resolves, with the backend's returned
   * `last_state` string. Optional — the live state otherwise arrives via
   * `command://state`. The seam the tests use to assert the invoke happened.
   */
  onStateChange?: (state: string) => void;
  /**
   * Called when a lifecycle `invoke` REJECTS (the backend returned `Err`), with the
   * failed action and the error message. This is the seam that surfaces an
   * otherwise-invisible failure (finding: the old empty `catch {}` swallowed every
   * lifecycle error, so a Start that the backend refused looked like a no-op).
   * The parent view renders it inline; on a failure the dot is also forced to
   * `error` locally so the failure is visible wherever the controls live (sidebar
   * row included), not only in a view that wires `onError`.
   */
  onError?: (action: "start" | "stop" | "relaunch", message: string) => void;
  /**
   * Whether to render the lead run-state dot. Defaults to `true` (the command VIEW
   * header). The SIDEBAR command row sets it `false` — the row already shows its own
   * lead `<StatusDot>`, so the controls there are buttons-only (finding 01KV63TEGB…).
   */
  showDot?: boolean;
  /** Button size — `icon-sm` for the view header (default), `icon-xs` for the row. */
  buttonSize?: "icon-sm" | "icon-xs";
  /**
   * When `true` the controls live inside a selectable ROW: each button stops click +
   * pointer-down propagation so acting on the command never also selects/drags the
   * row (mirrors the terminal row's close button). Defaults to `false`.
   */
  inRow?: boolean;
}

/** Whether the instance has a live process (the running-derived gating). */
function isRunning(state: ExecState): boolean {
  return state === "running";
}

/**
 * `<CommandControls>` — the run-state dot + start / stop / relaunch buttons for a
 * command instance (T9). Each button `invoke`s the matching Phase-3 lifecycle
 * command and is enabled per the CURRENT state:
 *
 *  - **Start** (`command_start`): enabled when NOT running (idle/success/error);
 *    disabled while running (a running instance has no second start — the backend
 *    is idempotent, but the UI also greys it).
 *  - **Stop** (`command_stop`): enabled ONLY while running; disabled otherwise
 *    (the task's explicit example: "arrêter" disabled if idle).
 *  - **Relaunch** (`command_relaunch`): always enabled for a valid instance — it
 *    stop-then-starts when running, or starts directly when not.
 *
 * The lead `<CommandStateDot>` reflects the state with colour + Motion (running =
 * blue + pulse, success = green static, …). All motion is chrome — there is NO
 * animation in the xterm viewport (the output panel is a sibling, never wrapped
 * here).
 */
export function CommandControls({
  instanceId,
  state,
  label,
  className,
  onStateChange,
  onError,
  showDot = true,
  buttonSize = "icon-sm",
  inRow = false,
}: CommandControlsProps) {
  // Track an in-flight lifecycle call so the buttons show a spinner / can't be
  // double-fired while the backend round-trips.
  const [pending, setPending] = useState<null | "start" | "stop" | "relaunch">(null);
  // A lifecycle invoke that REJECTED. We force the dot to `error` so the failure
  // is visible everywhere the controls live (the sidebar row has no output panel
  // to host an inline message). Cleared the moment a live `command://state`
  // transition moves the instance off its current state (a successful retry).
  const [failed, setFailed] = useState(false);

  const run = useCallback(
    async (action: "start" | "stop" | "relaunch", command: string) => {
      setPending(action);
      try {
        const next = await invoke<string>(command, { instanceId });
        setFailed(false);
        onStateChange?.(next);
      } catch (err) {
        // Surface the failure instead of swallowing it: mark it locally (dot →
        // error) and hand the message to the parent (inline panel message / toast).
        // The empty `catch {}` here is exactly what made a refused Start look like
        // it "did nothing".
        setFailed(true);
        const message = typeof err === "string" ? err : ((err as Error)?.message ?? String(err));
        onError?.(action, message);
      } finally {
        setPending(null);
      }
    },
    [instanceId, onStateChange, onError],
  );

  // A new live state from the props clears a previous failure (a retry succeeded,
  // or the backend reported a fresh transition), so the error dot is not sticky.
  const lastStateRef = useRef(state);
  if (lastStateRef.current !== state) {
    lastStateRef.current = state;
    if (failed) setFailed(false);
  }

  const running = isRunning(state);
  const busy = pending !== null;
  // The dot reflects the live state, but a swallowed→now-surfaced lifecycle error
  // overrides it to `error` until the next real transition clears it.
  const dotState: ExecState = failed ? "error" : state;

  // In a selectable row, the controls stop propagation in the BUBBLE phase so a
  // click acts on the button (its own `onClick` fires first) but never bubbles up to
  // also select/drag the row. We guard at the controls WRAPPER, not per-button, and
  // never in the capture phase (capture-phase stopPropagation would also cancel the
  // button's own handler). Mirrors the terminal row's close-button guard.
  const guard = inRow
    ? {
        onPointerDown: (e: React.PointerEvent) => e.stopPropagation(),
        onClick: (e: React.MouseEvent) => e.stopPropagation(),
      }
    : {};

  return (
    <div className={cn("flex items-center gap-2", className)}>
      {showDot && <CommandStateDot state={dotState} />}
      {label && (
        <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{label}</span>
      )}
      <div className="flex shrink-0 items-center gap-1" {...guard}>
        <Tooltip label="Start">
          <Button
            variant="ghost"
            size={buttonSize}
            aria-label="Start command"
            // Enabled only when there is no live process to start.
            disabled={running || busy}
            loading={pending === "start"}
            onClick={() => void run("start", "command_start")}
          >
            <PlayIcon />
          </Button>
        </Tooltip>
        <Tooltip label="Stop">
          <Button
            variant="ghost"
            size={buttonSize}
            aria-label="Stop command"
            // Only a running instance can be stopped (disabled when idle, etc.).
            disabled={!running || busy}
            loading={pending === "stop"}
            onClick={() => void run("stop", "command_stop")}
          >
            <SquareIcon />
          </Button>
        </Tooltip>
        <Tooltip label="Relaunch">
          <Button
            variant="ghost"
            size={buttonSize}
            aria-label="Relaunch command"
            // Relaunch is valid in any state (stop-then-start or direct start).
            disabled={busy}
            loading={pending === "relaunch"}
            onClick={() => void run("relaunch", "command_relaunch")}
          >
            <RotateCcwIcon />
          </Button>
        </Tooltip>
      </div>
    </div>
  );
}
