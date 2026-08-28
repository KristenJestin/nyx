import { useEffect, useState } from "react";

import { nyxBridge } from "@/bridge";
import type { CommandStatePayload } from "./use-command-state";
import type { ExecState } from "@/components/sidebar/use-terminals";

/**
 * WHAT A COMMAND'S PANE SAYS ABOUT ITSELF — the five phases a user recognises, told apart from the
 * FOUR the runtime knows.
 *
 * ## The bug this exists for (review 01M0ABR6V501D0SPR2BYBVZC4A, minor)
 *
 * A command that had NEVER been launched showed the word `idle` in the middle of its pane. The
 * word is not wrong in the runtime's vocabulary — `ExecState` really is
 * `idle | running | success | error`, where `idle` means "no live process" — but it is wrong in the
 * user's: they read INACTIVITY, i.e. "it is running but doing nothing", while the command had never
 * run at all. Verbatim: « en lisant la suite je viens de capté que idle de commande c'est
 * l'inactivité, sauf que là je l'ai quand la commande ne tourne pas, donc c'est très bizarre ».
 *
 * ORIGIN, ESTABLISHED BY READING THE CODE: the word came from `command-idle-empty.tsx:32`, which
 * rendered `service.run_state` RAW as its only content, shown by `runtime-pane-host.tsx` whenever
 * a selected service was `idle`. That is the only place `idle` was ever painted as WORDS. The
 * OTHER `idle`s are technical and STAY: the `ExecState` union itself, `RunState` in `status-dot.ts`
 * and the status dots' `aria-label="Command status: <state>"` / `data-state` — those name the
 * runtime's own state and have a real meaning at that layer.
 *
 * ## Why five phases from four states
 *
 * `ExecState` cannot express the distinction the user needs, because the runner collapses two very
 * different situations onto `idle`: a command that was NEVER started, and one the user STOPPED
 * (`CommandRunner::stop` sets `Idle` and records no exit code — a kill is not a natural exit). The
 * missing bit is not in the wire model and this finding does not touch the execution engine, so it
 * is recovered where it already exists: the renderer WATCHED the run. `command://state` carries the
 * `running` transition, so "this instance has run in this session" is observable without asking the
 * backend anything new ({@link useCommandRanThisSession}) — exactly how `useCommandExitCode` already
 * recovers the last exit code.
 */
export type CommandRunPhase = "never-run" | "running" | "succeeded" | "failed" | "stopped";

/**
 * The phase to SHOW, from the runtime state plus the two facts the state alone cannot carry.
 *
 * Pure and total: every `ExecState` maps somewhere, and `idle` splits on `ranThisSession` — the one
 * bit that tells "never started" from "stopped".
 */
export function commandRunPhase({
  state,
  ranThisSession = false,
}: {
  readonly state: ExecState;
  /** Whether a `running` transition was observed for this instance in this session. */
  readonly ranThisSession?: boolean;
}): CommandRunPhase {
  switch (state) {
    case "running":
      return "running";
    case "success":
      return "succeeded";
    case "error":
      return "failed";
    default:
      return ranThisSession ? "stopped" : "never-run";
  }
}

/**
 * The user-facing sentence for a phase — one DISTINCT wording each, and never the runtime's own
 * vocabulary. The failure carries its exit code, which is the single most useful thing to read
 * about a command that ended badly.
 *
 * `never-run` deliberately says nothing about inactivity: it names what is true (it has not run)
 * and points at the trigger, so the empty pane INVITES the action instead of reporting a state.
 */
export function commandRunPhaseLabel(
  phase: CommandRunPhase,
  options: { readonly exitCode?: number | null } = {},
): string {
  switch (phase) {
    case "never-run":
      return "Not run yet";
    case "running":
      return "Running…";
    case "succeeded":
      return "Finished successfully";
    case "failed":
      return options.exitCode == null
        ? "Finished with an error"
        : `Finished with an error — exit ${options.exitCode}`;
    case "stopped":
      return "Stopped";
  }
}

/**
 * Whether THIS instance has been seen RUNNING in this session — the bit that tells a command that
 * was never started from one the user stopped.
 *
 * A READ-ONLY observer of the same `command://state` event `useCommandState` and
 * `useCommandExitCode` consume, added as its own listener so neither of those is touched. It resets
 * when the tracked instance changes: the fact is per-instance and per-session, exactly like the
 * exit code next to it. `initial` seeds it for a caller that already knows the instance is running
 * (a snapshot arriving before any transition).
 */
export function useCommandRanThisSession(instanceId: string | null, initial = false): boolean {
  const [ran, setRan] = useState(initial);

  // Re-seed at RENDER time when the tracked instance changes (the same "adjust state during
  // render" pattern `useCommandState` uses), so a freshly selected command never inherits the
  // previous one's history.
  const [seen, setSeen] = useState({ instanceId, initial });
  if (seen.instanceId !== instanceId || seen.initial !== initial) {
    setSeen({ instanceId, initial });
    setRan(initial);
  }

  useEffect(() => {
    if (!instanceId) return;
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void nyxBridge
      .subscribe<CommandStatePayload>("command://state", (payload) => {
        if (torndown || payload.instanceId !== instanceId) return;
        if (payload.state === "running") setRan(true);
      })
      .then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlisten = un;
      });
    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, [instanceId]);

  return ran;
}
