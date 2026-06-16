import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import type { CommandStatePayload } from "./use-command-state";

/**
 * Track ONE command instance's last LIVE exit code, read from the backend
 * `command://state` event (filtered by `instanceId`). The payload carries `code`
 * for a terminal transition (`success`/`error`) and `null` for `idle`/`running`;
 * this hook keeps the latest non-null code so the info bar can show "exit 0" /
 * "exit 1" after a run ends, and clears it back to `null` when a fresh run starts
 * (a `running` transition).
 *
 * This is a READ-ONLY observer of the SAME event `useCommandState` consumes — it
 * does not touch that hook's wiring. It is deliberately a separate, additive
 * listener so the live-state stream (owned elsewhere) is untouched. A COLD exit
 * code (the persisted last code with no live event) is out of scope: this only
 * reflects codes observed this session.
 *
 * @param instanceId the `command_instances.id` to track (null = none)
 * @returns the last observed exit code, or `null` if none seen yet this session
 */
export function useCommandExitCode(instanceId: string | null): number | null {
  const [code, setCode] = useState<number | null>(null);

  // Reset when the tracked instance changes (a different command was selected):
  // its session-local exit code does not carry over.
  const [seenInstance, setSeenInstance] = useState(instanceId);
  if (seenInstance !== instanceId) {
    setSeenInstance(instanceId);
    setCode(null);
  }

  useEffect(() => {
    if (!instanceId) return;
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen<CommandStatePayload>("command://state", (event) => {
      if (torndown) return;
      const { instanceId: id, state, code } = event.payload;
      if (id !== instanceId) return;
      // A new run starting clears the previous code; a terminal transition records
      // the natural exit code; `idle` leaves the last code in place to read.
      if (state === "running") setCode(null);
      else if (code !== null) setCode(code);
    }).then((un) => {
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

  return code;
}
