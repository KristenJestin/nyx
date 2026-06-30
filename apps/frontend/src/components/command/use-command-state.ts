import { useEffect, useState } from "react";
import { nyxBridge } from "@/bridge";

import type { ExecState } from "@/components/sidebar/use-terminals";

/**
 * Payload of the backend `command://state` event: an instance's derived run state
 * plus the natural exit code (for success/error; `null` otherwise). Mirrors the
 * Rust `CommandStatePayload` (`bridge.rs`). `state` is the DB CHECK vocabulary
 * (`idle` | `running` | `success` | `error`), which is exactly `ExecState`.
 */
export interface CommandStatePayload {
  instanceId: string;
  state: ExecState;
  code: number | null;
}

/** Narrow an arbitrary string to a known `ExecState`, defaulting to `idle`. */
function asExecState(s: string): ExecState {
  return s === "running" || s === "success" || s === "error" ? s : "idle";
}

/**
 * Track ONE command instance's live run state, driven by the backend
 * `command://state` event FILTERED by `instanceId`. Seeds from `initialState`
 * (the instance's persisted `last_state` from the listing) so the dot is correct
 * before the first transition, then follows each transition for that instance.
 *
 * The returned `state` is the single source of truth for both the status dot
 * (idle/running/success/error → colour + motion) and the buttons' enabled state.
 * Re-seeds if `instanceId` or `initialState` changes (a different command was
 * selected). StrictMode-safe: the listener is idempotent and unlistened on
 * cleanup.
 *
 * @param instanceId the `command_instances.id` to track (null = none)
 * @param initialState the instance's persisted `last_state` (defaults to idle)
 */
export function useCommandState(
  instanceId: string | null,
  initialState: ExecState = "idle",
): ExecState {
  const [state, setState] = useState<ExecState>(initialState);
  // Re-seed at RENDER time (not via an effect) when the tracked instance or its
  // persisted state changes, so a freshly selected command shows its stored state
  // immediately — without the extra commit a `useEffect(setState(...))` would cause.
  // This is React's "adjusting state during render" pattern: comparing the last
  // seed inputs and calling `setState` while rendering re-renders synchronously
  // before paint, with no flash of the previous instance's state.
  const [seed, setSeed] = useState({ instanceId, initialState });
  if (seed.instanceId !== instanceId || seed.initialState !== initialState) {
    setSeed({ instanceId, initialState });
    setState(initialState);
  }

  useEffect(() => {
    if (!instanceId) return;
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void nyxBridge
      .subscribe<CommandStatePayload>("command://state", (payload) => {
        if (torndown) return;
        if (payload.instanceId !== instanceId) return;
        setState(asExecState(payload.state));
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

  return state;
}
