import { useEffect } from "react";
import { nyxBridge } from "@/bridge";

import type { BackendEvent } from "@/bridge";

/**
 * `use-runtime-live` — the ONE shared live-runtime engine for the project→workspaces Terminals
 * page. It owns the single source of truth for WHICH backend pushes invalidate a runtime
 * aggregate (`RUNTIME_CHANGED_EVENTS`) and the StrictMode-safe subscribe/re-pull/teardown
 * primitive (`useRuntimeChangeSubscription`) that the project runtime read hook builds on — so
 * the topic list and the teardown pattern live in exactly one place, reused rather than copied.
 *
 * ## Why re-pull on the coarse topics (domain_events → live, no polling)
 * A workspace's runtime aggregate is its terminals + their agent sessions + its managed-command
 * services (with live run state). Any RUNTIME mutation of that aggregate is announced by the
 * backend as an UNSOLICITED event on the v2.0 push channel (sidecar → main → renderer):
 *   - `commands://changed`      — a service start/stop/exit OR a template mutation;
 *   - `agent-sessions://changed`— an agent SessionStart/SessionEnd (the MCP hook);
 *   - `terminals://changed`     — a terminal record mutation (create/close/attach);
 *   - `workspaces://changed`    — an identity mutation (the domain-event `PushEventSink`);
 *   - `terminal://exec-state`   — a terminal's exec-state badge transition (OSC 133), a FINE
 *                                 per-terminal event that also lives in the aggregate as
 *                                 `WorkspaceRuntimeTerminal.exec_state` (never on
 *                                 `terminals://changed`).
 * Subscribing to those and re-pulling the snapshot on receipt is exactly the "sans polling"
 * contract: the page reflects a runtime mutation live because the backend TOLD it something
 * changed, not because it asked on a timer.
 */
export const RUNTIME_CHANGED_EVENTS = [
  "commands://changed",
  "agent-sessions://changed",
  "terminals://changed",
  "workspaces://changed",
  "terminal://exec-state",
] as const satisfies readonly BackendEvent[];

/**
 * Subscribe to every `RUNTIME_CHANGED_EVENTS` topic and invoke `onChange` on each push — the
 * "sans polling" re-pull trigger. `enabled` gates the subscription (no target → no listeners).
 * `onChange` MUST be stable (wrap it in `useCallback`) so the effect only re-subscribes when the
 * target actually changes.
 *
 * StrictMode-safe: every subscription is torn down on cleanup and a late-resolving unlisten that
 * arrives after teardown is released immediately, so no listener survives an unmount.
 */
export function useRuntimeChangeSubscription(enabled: boolean, onChange: () => void): void {
  useEffect(() => {
    if (!enabled) return;
    let torndown = false;
    const unlistens: Array<() => void> = [];
    for (const event of RUNTIME_CHANGED_EVENTS) {
      void nyxBridge
        .subscribe(event, () => {
          if (torndown) return;
          onChange();
        })
        .then((un) => {
          if (torndown) {
            void Promise.resolve(un()).catch(() => {});
            return;
          }
          unlistens.push(un);
        });
    }
    return () => {
      torndown = true;
      for (const un of unlistens) void Promise.resolve(un()).catch(() => {});
    };
  }, [enabled, onChange]);
}
