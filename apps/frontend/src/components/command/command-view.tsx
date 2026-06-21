import { useRef, useState } from "react";
import { AlertTriangleIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import type { ExecState } from "@/components/sidebar/use-terminals";
import { CommandControls } from "./command-controls";
import { CommandInfoBar } from "./command-info-bar";
import { CommandOutputPanel } from "./command-output-panel";
import { useCommandExitCode } from "./use-command-exit-code";
import { useCommandState } from "./use-command-state";

export interface CommandViewProps {
  /** The `command_instances.id` to view + control. */
  instanceId: string;
  /** The command's display name (shown in the header next to the controls). */
  name: string;
  /** The instance's persisted `last_state` to seed the dot before live events. */
  initialState?: ExecState;
  /** The command line that runs (e.g. `bun run start`) — shown in the info bar. */
  command?: string;
  /** The resolved run directory (workspace + subfolder) — shown in the info bar. */
  cwd?: string;
  /** package.json script name if imported (else null) — drives the source field. */
  sourceScriptName?: string | null;
  /** package.json path the source points at (the source field's title hint). */
  sourcePackageJsonPath?: string | null;
  /** Whether this view is the active/visible one (drives WebGL attach). */
  active?: boolean;
  className?: string;
}

/**
 * `<CommandView>` — the composed command surface mounted in the main pane (T10):
 * a header carrying the run-state DOT + the START / STOP / RELAUNCH buttons (T9)
 * over the READ-ONLY output PANEL (T8). The live state for both the dot and the
 * buttons comes from `useCommandState` (seeded from the instance's `last_state`,
 * then driven by `command://state`).
 *
 * STRICTLY no stdin: the only surface is `<CommandOutputPanel>`, which is
 * read-only (no input path, `disableStdin`); the buttons drive the lifecycle
 * commands, never the process's stdin. All motion (the dot pulse, the button
 * chrome) is outside the xterm viewport.
 */
export function CommandView({
  instanceId,
  name,
  initialState = "idle",
  command,
  cwd,
  sourceScriptName,
  sourcePackageJsonPath,
  active = true,
  className,
}: CommandViewProps) {
  const state = useCommandState(instanceId, initialState);
  // The last LIVE exit code for the info bar (cold codes are out of scope).
  const exitCode = useCommandExitCode(instanceId);

  // The last lifecycle error surfaced by `<CommandControls>` (a Start/Stop/Relaunch
  // the backend REFUSED). Shown inline below the header so a failure that used to be
  // swallowed by the empty `catch {}` is now diagnosable (finding: Play did nothing).
  // A live state transition clears it (a retry took hold).
  const [error, setError] = useState<string | null>(null);
  const lastState = useRef(state);
  if (lastState.current !== state) {
    lastState.current = state;
    if (error) setError(null);
  }

  return (
    <div className={cn("flex h-full w-full flex-col bg-background", className)}>
      {/* Header chrome: dot + name + the three lifecycle buttons. */}
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <CommandControls
          instanceId={instanceId}
          state={state}
          label={name}
          className="w-full"
          onStateChange={() => setError(null)}
          onError={(action, message) => setError(`Failed to ${action} command: ${message}`)}
        />
      </div>
      {/* Compact info bar UNDER the controls: the command, its resolved run
          directory, the package.json source (if imported), and the live state +
          last exit code. Rendered only when the parent threaded the context
          (command/cwd) — the isolated header still works without it. */}
      {command != null && cwd != null && (
        <CommandInfoBar
          command={command}
          cwd={cwd}
          sourceScriptName={sourceScriptName}
          sourcePackageJsonPath={sourcePackageJsonPath}
          exitCode={exitCode}
        />
      )}
      {/* Inline lifecycle-error banner: a refused Start/Stop/Relaunch is now VISIBLE
          (read-only — no stdin), instead of silently doing nothing. */}
      {error && (
        <div
          role="alert"
          className="flex shrink-0 items-start gap-2 border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          <AlertTriangleIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
          <span className="min-w-0 break-words">{error}</span>
        </div>
      )}
      {/* The read-only output panel fills the rest. Keyed by instance so a
          different command rebuilds a clean buffer + rehydrates its history. */}
      <div className="min-h-0 flex-1">
        <CommandOutputPanel key={instanceId} instanceId={instanceId} active={active} />
      </div>
    </div>
  );
}
