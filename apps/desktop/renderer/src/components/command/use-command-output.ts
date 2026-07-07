import { useEffect, useRef } from "react";
import type { WTerm } from "@wterm/dom";
import { nyxBridge, type Unsubscribe } from "@/bridge";

/** RIS (Reset to Initial State) — clears the screen AND scrollback in the VT core. */
const RESET_SEQUENCE = "\x1bc";

/**
 * Payload of the backend `command://output` event: coalesced output for ONE
 * command instance. `bytes` is the raw output since the last flush (a JSON byte
 * array over the IPC boundary); the front decodes + writes it. Mirrors the Rust
 * `CommandOutputPayload` (`bridge.rs`).
 */
interface CommandOutputPayload {
  instanceId: string;
  bytes: number[];
}

/**
 * Payload of the backend `command://output-cleared` event (review R-OUTPUT): the
 * MCP `clear_command_output` tool emptied an instance's captured buffer.
 */
interface CommandOutputClearedPayload {
  instanceId: string;
}

/**
 * Wire a wterm instance to a managed-command instance's output stream — STRICTLY
 * READ-ONLY. The deliberate contrast with the interactive terminal: there is
 * **no input path at all**. (The panel builds its `WTerm` with no `onData`, so
 * even the built-in input never reaches the process.)
 *
 *  - On open it REHYDRATES the history via `command_output(instanceId)` (live
 *    in-memory buffer while running, else persisted scrollback after a restart)
 *    and writes that snapshot FIRST.
 *  - It then subscribes to `command://output` FILTERED by `instanceId` and writes
 *    each coalesced chunk, so live output appends below the rehydrated history.
 *
 * A `running` transition or a `command://output-cleared` RESETS the surface
 * (RIS: screen + scrollback) so a fresh run does not pile under prior output.
 *
 * StrictMode-safe: state on a ref keyed to the wterm instance, idempotent setup,
 * clean teardown. Re-runs only when the instance or `instanceId` changes.
 *
 * @param term the wterm instance to drive (null until it is created)
 * @param instanceId the `command_instances.id` whose output to show (null = none)
 */
export function useCommandOutput(term: WTerm | null, instanceId: string | null): void {
  const stateRef = useRef<{
    instanceId: string;
    torndown: boolean;
    unlisten?: Unsubscribe;
    unlistenState?: Unsubscribe;
    unlistenCleared?: Unsubscribe;
  } | null>(null);

  useEffect(() => {
    if (!term || !instanceId) return;

    const state = {
      instanceId,
      torndown: false,
    } as NonNullable<typeof stateRef.current>;
    stateRef.current = state;

    const decode = (bytes: number[]): Uint8Array => Uint8Array.from(bytes);

    let rehydrated = false;
    let generation = 0;
    const pending: CommandOutputPayload[] = [];

    void (async () => {
      const unlisten = await nyxBridge.subscribe<CommandOutputPayload>(
        "command://output",
        (payload) => {
          if (state.torndown) return;
          if (payload.instanceId !== state.instanceId) return;
          if (!rehydrated) {
            pending.push(payload);
            return;
          }
          term.write(decode(payload.bytes));
        },
      );
      if (state.torndown) {
        void Promise.resolve(unlisten()).catch(() => {});
        return;
      }
      state.unlisten = unlisten;

      // CLEAR ON NEW RUN: a `running` transition marks a fresh start/relaunch. We
      // RIS-reset the surface (screen + scrollback) so the new run does not pile
      // under prior output, drop buffered pre-clear output, and bump `generation`
      // so an in-flight rehydrate cannot paint pre-clear history over the reset.
      const unlistenState = await nyxBridge.subscribe<{ instanceId: string; state: string }>(
        "command://state",
        (payload) => {
          if (state.torndown) return;
          if (payload.instanceId !== state.instanceId) return;
          if (payload.state === "running") {
            term.write(RESET_SEQUENCE);
            pending.length = 0;
            generation += 1;
          }
        },
      );
      if (state.torndown) {
        void Promise.resolve(unlistenState()).catch(() => {});
        return;
      }
      state.unlistenState = unlistenState;

      // CLEAR ON `clear_command_output`: the MCP tool emptied the backend buffer
      // (review R-OUTPUT) and emits `command://output-cleared`. Reuse the reset
      // path (RIS + drop buffered output + invalidate in-flight rehydrate).
      const unlistenCleared = await nyxBridge.subscribe<CommandOutputClearedPayload>(
        "command://output-cleared",
        (payload) => {
          if (state.torndown) return;
          if (payload.instanceId !== state.instanceId) return;
          term.write(RESET_SEQUENCE);
          pending.length = 0;
          generation += 1;
        },
      );
      if (state.torndown) {
        void Promise.resolve(unlistenCleared()).catch(() => {});
        return;
      }
      state.unlistenCleared = unlistenCleared;

      // REHYDRATE: live in-memory buffer if running, else persisted scrollback
      // (cold history after a restart). Written FIRST so it sits above live output.
      const gen = generation;
      const history = await nyxBridge
        .invoke<string>("command_output", { instanceId })
        .catch(() => "");
      if (state.torndown) return;
      if (gen === generation && history) term.write(history);

      rehydrated = true;
      for (const payload of pending) {
        if (payload.instanceId === state.instanceId) term.write(decode(payload.bytes));
      }
      pending.length = 0;
    })();

    return () => {
      state.torndown = true;
      const unlisten = state.unlisten;
      state.unlisten = undefined;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
      const unlistenState = state.unlistenState;
      state.unlistenState = undefined;
      if (unlistenState) void Promise.resolve(unlistenState()).catch(() => {});
      const unlistenCleared = state.unlistenCleared;
      state.unlistenCleared = undefined;
      if (unlistenCleared) void Promise.resolve(unlistenCleared()).catch(() => {});
    };
  }, [term, instanceId]);
}
