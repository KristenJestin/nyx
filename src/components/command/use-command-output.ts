import { useEffect, useRef } from "react";
import type { IDisposable, Terminal as XTerm } from "@xterm/xterm";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Payload of the backend `command://output` event: coalesced output for ONE
 * command instance. `bytes` is the raw output since the last flush (a JSON byte
 * array over the IPC boundary); the front decodes + writes it to xterm. Mirrors
 * the Rust `CommandOutputPayload` (`bridge.rs`).
 */
interface CommandOutputPayload {
  instanceId: string;
  bytes: number[];
}

/**
 * Payload of the backend `command://output-cleared` event (review R-OUTPUT): the
 * MCP `clear_command_output` tool emptied an instance's captured buffer. Carries
 * ONLY the instance id (camelCase, load-bearing — mirrors the Rust
 * `CommandOutputClearedPayload` in `bridge.rs`); clearing wipes the bytes, not the
 * factual state/outcome, so there is no `state`/`code`.
 */
interface CommandOutputClearedPayload {
  instanceId: string;
}

/**
 * Wire an xterm instance to a managed-command instance's output stream — STRICTLY
 * READ-ONLY. This is the deliberate contrast with `usePty` (the interactive
 * terminal): there is **no input path at all** here.
 *
 *  - On open it REHYDRATES the history by invoking `command_output(instanceId)`,
 *    which returns the live in-memory buffer while the instance is running, else
 *    the persisted scrollback read back from the DB (cold rehydration after a nyx
 *    restart). That snapshot is written to xterm FIRST.
 *  - It then subscribes to `command://output`, FILTERED by `instanceId`, and
 *    writes each coalesced chunk to xterm, so live output appends below the
 *    rehydrated history.
 *
 * What it deliberately does NOT do (the read-only-strict guarantee, T8):
 *  - NO `term.onData(...)` → it never calls `pty_write` / any write/stdin command.
 *  - NO `term.onResize(...)` → it never pushes a resize to the process (a resize
 *    would be a stdin-adjacent backchannel; this panel is watch-only).
 * Selection + scroll stay available (those are xterm-local, no backend traffic).
 *
 * StrictMode-safe: the live state lives on a ref keyed to the xterm instance, the
 * setup is idempotent (a single rehydrate + a single listener), and teardown
 * unlistens cleanly. The effect re-runs only when the xterm instance or the
 * `instanceId` changes (a different command was selected), never per render.
 *
 * @param term the xterm instance to drive (null until it is created)
 * @param instanceId the `command_instances.id` whose output to show (null = none)
 */
export function useCommandOutput(term: XTerm | null, instanceId: string | null): void {
  // Track the live wiring on a ref so StrictMode's setup → cleanup → setup
  // double-invoke reuses it instead of double-rehydrating / double-listening.
  const stateRef = useRef<{
    instanceId: string;
    torndown: boolean;
    unlisten?: UnlistenFn;
    unlistenState?: UnlistenFn;
    unlistenCleared?: UnlistenFn;
    disposables: IDisposable[];
  } | null>(null);

  useEffect(() => {
    if (!term || !instanceId) return;

    const state = {
      instanceId,
      torndown: false,
      disposables: [] as IDisposable[],
    } as NonNullable<typeof stateRef.current>;
    stateRef.current = state;

    const decode = (bytes: number[]): Uint8Array => Uint8Array.from(bytes);

    // Output events that land BEFORE the rehydrate resolves are buffered, then
    // replayed AFTER the history snapshot is written, so live output never lands
    // above the rehydrated scrollback (the same ordering guarantee `usePty` makes
    // for dead history).
    let rehydrated = false;
    // Bumped by every clear (a run-start `running` reset, or `command://output-cleared`).
    // The rehydrate captures this before its round-trip and re-checks it after, so a clear
    // that lands DURING the round-trip WINS: we must not paint the pre-clear history over
    // the freshly cleared panel (output that arrived after the clear still drains).
    let generation = 0;
    const pending: CommandOutputPayload[] = [];

    void (async () => {
      // Subscribe BEFORE the rehydrate round-trip so no live chunk is dropped in
      // the gap. While not yet rehydrated we buffer; once rehydrated we write live.
      const unlisten = await listen<CommandOutputPayload>("command://output", (event) => {
        if (state.torndown) return;
        if (event.payload.instanceId !== state.instanceId) return;
        if (!rehydrated) {
          pending.push(event.payload);
          return;
        }
        term.write(decode(event.payload.bytes));
      });
      if (state.torndown) {
        // Torn down while awaiting the subscription: drop it, don't leak.
        void Promise.resolve(unlisten()).catch(() => {});
        return;
      }
      state.unlisten = unlisten;

      // CLEAR ON NEW RUN: a `running` transition for THIS instance marks a fresh
      // start/relaunch. We wipe the xterm (buffer + scrollback) so the new run does
      // NOT pile under the previously rehydrated output — each run starts from a
      // clean panel. The backend also resets the instance's scrollback at run start
      // (see `CommandRunner::start`), so a later cold rehydrate is clean too.
      // `term.reset()` clears the screen AND the scrollback (vs `clear()`, which
      // keeps the current row). We bump `generation` so an in-flight rehydrate cannot
      // paint pre-clear history on top of the cleared panel.
      const unlistenState = await listen<{ instanceId: string; state: string }>(
        "command://state",
        (event) => {
          if (state.torndown) return;
          if (event.payload.instanceId !== state.instanceId) return;
          if (event.payload.state === "running") {
            term.reset();
            // Drop anything still buffered from a prior run so it can't replay over
            // the cleared panel, and invalidate any in-flight rehydrate.
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

      // CLEAR ON `clear_command_output`: the MCP tool emptied THIS instance's backend
      // buffer and emits `command://output-cleared` (review R-OUTPUT). It is the analog
      // of the run-start clear above but WITHOUT a state transition (clearing the log is
      // not a start/relaunch), so we reuse the exact same reset path: wipe the xterm
      // (screen + scrollback) and drop any output still buffered pre-rehydrate so it
      // cannot replay over the cleared panel. Filtered by `instanceId`.
      const unlistenCleared = await listen<CommandOutputClearedPayload>(
        "command://output-cleared",
        (event) => {
          if (state.torndown) return;
          if (event.payload.instanceId !== state.instanceId) return;
          term.reset();
          pending.length = 0;
          generation += 1;
        },
      );
      if (state.torndown) {
        void Promise.resolve(unlistenCleared()).catch(() => {});
        return;
      }
      state.unlistenCleared = unlistenCleared;

      // REHYDRATE: the live in-memory buffer if running, else the persisted
      // scrollback (cold history after a nyx restart). Written FIRST so it sits
      // above any live output. Best-effort: a missing instance / IPC failure
      // leaves the panel empty rather than throwing.
      const gen = generation;
      const history = await invoke<string>("command_output", { instanceId }).catch(() => "");
      if (state.torndown) return;
      // Paint the rehydrated history ONLY if no clear landed during the round-trip — a
      // clear bumps `generation`, and its cleared panel must win over stale pre-clear
      // history. Output that arrived AFTER the clear is in `pending` and still drains below.
      if (gen === generation && history) term.write(history);

      // Drain any output that arrived during the rehydrate round-trip, in order,
      // then switch to live writes.
      rehydrated = true;
      for (const payload of pending) {
        if (payload.instanceId === state.instanceId) term.write(decode(payload.bytes));
      }
      pending.length = 0;
    })();

    return () => {
      state.torndown = true;
      for (const d of state.disposables) d.dispose();
      state.disposables = [];
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
    // Re-run only when the instance or the bound xterm changes — re-running per
    // render would re-rehydrate and re-subscribe.
  }, [term, instanceId]);
}
