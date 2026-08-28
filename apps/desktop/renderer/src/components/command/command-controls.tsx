import { useCallback, useRef, useState, type ReactNode } from "react";
import { PlayIcon, RotateCcwIcon, SquareIcon } from "lucide-react";
import { errorText, nyxBridge } from "@/bridge";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { DRAG_INERT_ATTR } from "@/components/panes/reorder-dnd";
import type { ExecState } from "@/components/sidebar/use-terminals";
import { CommandStateDot } from "./command-state-dot";

/** The three lifecycle gestures, whatever they are applied to. */
export type CommandAction = "start" | "stop" | "relaunch";

/** The three, in the order they are drawn — one list, so no surface can reorder them by accident. */
const ACTIONS: readonly CommandAction[] = ["start", "stop", "relaunch"];

const ACTION_ICON = { start: PlayIcon, stop: SquareIcon, relaunch: RotateCcwIcon } as const;
const ACTION_TOOLTIP = { start: "Start", stop: "Stop", relaunch: "Relaunch" } as const;

export interface CommandControlsShellProps {
  /**
   * PERFORM one gesture. A rejection is NOT swallowed: the shell catches it, marks itself failed
   * (so the caller's lead can paint an error) and hands the message to {@link onError}.
   */
  run: (action: CommandAction) => Promise<unknown>;
  /** Whether each button is available. DERIVED BY THE CALLER — the shell re-derives nothing. */
  enabled: Readonly<Record<CommandAction, boolean>>;
  /** The accessible name of each button — one instance and a whole group do not say the same thing. */
  labels: Readonly<Record<CommandAction, string>>;
  /** The failure seam: the action that rejected plus the backend's real reason. */
  onError?: (action: CommandAction, message: string) => void;
  /**
   * What sits BEFORE the buttons (a run-state dot, an aggregate counter…). It receives the shell's
   * own `failed` flag, because a rejection is only known here and must be visible wherever these
   * controls live — a sidebar row has no panel to host an inline message.
   */
  lead?: (failed: boolean) => ReactNode;
  /**
   * A live value whose CHANGE clears a previous failure (the instance's state, a group's running
   * tally). A retry that works, or a fresh transition pushed by the backend, un-sticks the dot.
   */
  resetKey: unknown;
  /** Button size — `icon-sm` for a view header (default), `icon-xs` for a sidebar row. */
  buttonSize?: "icon-sm" | "icon-xs";
  /**
   * When `true` the controls live inside a selectable ROW: the wrapper stops click + pointer-down
   * propagation so acting never also selects/drags the row. Defaults to `false`.
   */
  inRow?: boolean;
  className?: string;
}

/**
 * `<CommandControlsShell>` — THE THREE BUTTONS, and nothing about WHAT they drive.
 *
 * ## Why it was extracted (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding 01M11XVEFEQ524XRT9ARPQFGSV)
 *
 * `<CommandControls>` was welded to an `instanceId`: it built the invoke arguments itself, hardcoded
 * the three route names and derived availability from one `ExecState`. A GROUP needs the identical
 * shell — same three buttons, same in-flight `pending`, same "a rejection is surfaced, never
 * swallowed" — over completely different actions (`command_group_*`, `{ groupId, workspaceId }`) and
 * a completely different availability rule (play is live while ONE member is stopped, not while the
 * one instance is). Copying the shell would have duplicated the `pending`/`failed` state machine and
 * the error seam, which is exactly the pair that had already been got wrong once (the empty
 * `catch {}` that made a refused Start look like a no-op).
 *
 * So the shell owns what is the same — the layout, the `pending` spinner, the sticky-until-next-
 * transition `failed` flag, the propagation guard — and takes what differs as data: `run`,
 * `enabled`, `labels`. Two implementations feed it: {@link CommandControls} for one instance and
 * `<CommandGroupControls>` for a whole group.
 */
export function CommandControlsShell({
  run,
  enabled,
  labels,
  onError,
  lead,
  resetKey,
  buttonSize = "icon-sm",
  inRow = false,
  className,
}: CommandControlsShellProps) {
  // Track an in-flight lifecycle call so the buttons show a spinner / can't be double-fired while
  // the backend round-trips.
  const [pending, setPending] = useState<CommandAction | null>(null);
  // A lifecycle call that REJECTED. Handed to `lead` so the failure is visible everywhere these
  // controls live. Cleared the moment `resetKey` moves (a successful retry, a live transition).
  const [failed, setFailed] = useState(false);

  const perform = useCallback(
    async (action: CommandAction) => {
      setPending(action);
      try {
        await run(action);
        setFailed(false);
      } catch (err) {
        // Surface the failure instead of swallowing it: mark it locally and hand the message up.
        setFailed(true);
        onError?.(action, errorText(err, "The command failed."));
      } finally {
        setPending(null);
      }
    },
    [run, onError],
  );

  const lastKeyRef = useRef(resetKey);
  if (lastKeyRef.current !== resetKey) {
    lastKeyRef.current = resetKey;
    if (failed) setFailed(false);
  }

  const busy = pending !== null;
  // In a selectable row, the controls stop propagation in the BUBBLE phase so a click acts on the
  // button (its own `onClick` fires first) but never bubbles up to also select/drag the row. Guarded
  // at the WRAPPER, not per-button, and never in the capture phase (capture-phase stopPropagation
  // would also cancel the button's own handler). Mirrors the terminal row's close-button guard.
  //
  // The marker next to it is the half React cannot do: a group HEAD is a drag handle
  // (`group-block.tsx`, human review 01M1312FSV6MAR0P63SEG2RF6C) and these buttons live inside one,
  // so Enter on a Start would both launch the group and lift the block — React's delegated
  // `stopPropagation` runs after dnd-kit's own listener and cannot prevent it. `preventActivation`
  // reads this attribute (`reorder-dnd.ts`) from inside dnd-kit instead of racing it.
  const guard = inRow
    ? {
        [DRAG_INERT_ATTR]: "",
        onPointerDown: (e: React.PointerEvent) => e.stopPropagation(),
        onClick: (e: React.MouseEvent) => e.stopPropagation(),
      }
    : {};

  return (
    <div className={cn("flex items-center gap-2", className)}>
      {lead?.(failed)}
      <div className="flex shrink-0 items-center gap-1" {...guard}>
        {ACTIONS.map((action) => {
          const Icon = ACTION_ICON[action];
          return (
            <Tooltip key={action} label={ACTION_TOOLTIP[action]}>
              <Button
                variant="ghost"
                size={buttonSize}
                aria-label={labels[action]}
                disabled={!enabled[action] || busy}
                loading={pending === action}
                onClick={() => void perform(action)}
              >
                <Icon />
              </Button>
            </Tooltip>
          );
        })}
      </div>
    </div>
  );
}

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
  onError?: (action: CommandAction, message: string) => void;
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
 *
 * The BUTTONS themselves are {@link CommandControlsShell}: this component is now the
 * INSTANCE implementation of that shell — the three routes, the `{ instanceId }` payload and the
 * one-state availability rule — and the group's controls are the other one.
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
  const run = useCallback(
    async (action: CommandAction) => {
      const command =
        action === "start"
          ? ("command_start" as const)
          : action === "stop"
            ? ("command_stop" as const)
            : ("command_relaunch" as const);
      const next = await nyxBridge.invoke<string>(command, { instanceId });
      onStateChange?.(next);
    },
    [instanceId, onStateChange],
  );

  const running = isRunning(state);

  return (
    <CommandControlsShell
      run={run}
      enabled={{ start: !running, stop: running, relaunch: true }}
      labels={{
        start: "Start command",
        stop: "Stop command",
        relaunch: "Relaunch command",
      }}
      onError={onError}
      resetKey={state}
      buttonSize={buttonSize}
      inRow={inRow}
      className={className}
      lead={(failed) => (
        <>
          {/* The dot reflects the live state, but a swallowed→now-surfaced lifecycle error
              overrides it to `error` until the next real transition clears it. */}
          {showDot && <CommandStateDot state={failed ? "error" : state} />}
          {label && (
            <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">
              {label}
            </span>
          )}
        </>
      )}
    />
  );
}
