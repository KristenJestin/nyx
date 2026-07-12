import { useEffect } from "react";

import { nyxBridge } from "@/bridge";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { reconcileTerminals } from "./terminal-store";

/**
 * Backend event broadcast whenever a terminal RECORD is created, closed, or re-bound
 * (`create_terminal` / `close_terminal` / `attach_terminal`, from the UI OR the MCP terminal
 * tools) — mirrors `bridge::TERMINALS_CHANGED_EVENT`. It is the single push that announces the
 * GLOBAL terminal-record set changed, so it is exactly the trigger to re-check which live store
 * instances still have a backing record.
 */
const TERMINALS_CHANGED_EVENT = "terminals://changed" as const;

/**
 * `useReconcileTerminals` — the APP-WIDE terminal-store reconciliation.
 *
 * Live xterm instances live in a module-level store ({@link reconcileTerminals}) SHARED by every
 * project's deck. An instance must be DISPOSED (its PTY closed) ONLY when its backing terminal
 * RECORD is truly gone — closed by the user, closed by an agent over MCP, or hard-deleted — and
 * NEVER because the user switched to another project (a switch merely HIDES the other project's
 * panes; those records are still alive). So the reconciliation MUST run against the GLOBAL set of
 * every live terminal across ALL projects, not any per-project deck subset.
 *
 * That is why it does NOT live in the deck: the deck only ever sees the CURRENT project's
 * terminals, so reconciling against them disposed the PREVIOUS project's terminals — and their
 * running agents — on every project switch (the bug this hook fixes). Instead this hook is mounted
 * ONCE at the top (the always-mounted shell). It loads `list_terminals` — the authoritative global
 * record set (every project, PLUS loose/unattached terminals, which a per-workspace snapshot would
 * miss) — and re-pulls it on each `terminals://changed` push, then disposes any store instance
 * whose id is no longer in the ALIVE set. A pure project switch fires no `terminals://changed` and
 * leaves `list_terminals` unchanged, so it never disposes another project's terminal.
 *
 * SAFETY: it reconciles ONLY against a REAL array result. A null/failed `list_terminals` is not a
 * reliable "these are all the terminals that exist" signal, so it is SKIPPED rather than treated
 * as "everything is gone" — a transient IPC error can never wipe live instances.
 */
export function useReconcileTerminals(): void {
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;

    const reconcile = () => {
      void nyxBridge
        .invoke<TerminalRecord[]>("list_terminals")
        .then((rows) => {
          if (torndown) return;
          // Only reconcile against a REAL global set — never dispose on an unreliable result.
          if (!Array.isArray(rows)) return;
          const aliveIds = rows.filter((r) => r.status === "alive").map((r) => r.id);
          reconcileTerminals(aliveIds);
        })
        // Transient list failure: keep every instance; the next event recovers.
        .catch(() => {});
    };

    // Initial global reconcile, then live on each terminal-record mutation.
    reconcile();
    void nyxBridge
      .subscribe(TERMINALS_CHANGED_EVENT, () => {
        if (torndown) return;
        reconcile();
      })
      .then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlisten = un;
      })
      // Swallow a missing event bridge (test/host without Tauri) so it never becomes an
      // unhandled rejection — the same guard the other terminal subscriptions use.
      .catch(() => {});

    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, []);
}
