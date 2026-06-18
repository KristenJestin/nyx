import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/**
 * The Tauri event emitted whenever an agent session starts or ends
 * (`bridge::AGENT_SESSIONS_CHANGED_EVENT`). A coalescing "re-pull" tick with no payload:
 * the hook re-fetches `agent_active_sessions` to stay in sync, the same pattern the
 * terminal deck uses for `terminals://changed`.
 */
const AGENT_SESSIONS_CHANGED_EVENT = "agent-sessions://changed";

/**
 * One active agent session pair from `agent_active_sessions` (mirrors
 * `db::ActiveAgentSession`): which terminal hosts a live session, of which agent kind.
 */
interface ActiveAgentSession {
  terminal_id: string;
  agent_kind: string;
}

/**
 * Live map `terminal_id → agent_kind` of the terminals that currently host an ACTIVE
 * agent session (finding #55). The sidebar reads this to swap a terminal row's icon to
 * the agent's provider logo while a session is live, reverting to the terminal icon when
 * it ends.
 *
 * Reactivity: pulls once on mount, then re-pulls on every `agent-sessions://changed`
 * event (emitted by the backend after a SessionStart/SessionEnd). StrictMode-safe — the
 * listener is torn down on cleanup and a late `listen` resolve after unmount is
 * unlistened immediately. A transient IPC failure keeps the last good map; the next
 * event recovers it.
 */
export function useActiveAgentSessions(): ReadonlyMap<string, string> {
  const [byTerminal, setByTerminal] = useState<ReadonlyMap<string, string>>(() => new Map());

  useEffect(() => {
    let torndown = false;

    const pull = () => {
      void invoke<ActiveAgentSession[]>("agent_active_sessions")
        .then((rows) => {
          if (torndown) return;
          const next = new Map<string, string>();
          // Defensive: a missing/stubbed backend may return a non-array — treat it as
          // "no active sessions" rather than throwing while iterating.
          if (Array.isArray(rows)) {
            for (const r of rows) next.set(r.terminal_id, r.agent_kind);
          }
          setByTerminal(next);
        })
        // A transient failure leaves the current map; the next event recovers it.
        .catch(() => {});
    };

    // Initial state.
    pull();

    // Re-pull on every change tick.
    let unlisten: (() => void) | undefined;
    void listen(AGENT_SESSIONS_CHANGED_EVENT, () => {
      if (torndown) return;
      pull();
    })
      .then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlisten = un;
      })
      // If the event channel is unavailable (e.g. a test/host without the Tauri event
      // bridge), the initial pull still seeded the map; we just won't get live updates.
      // Swallow so a missing bridge never becomes an unhandled rejection.
      .catch(() => {});

    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, []);

  return byTerminal;
}

/**
 * Context carrying the live `terminal_id → agent_kind` map so every terminal ROW can
 * read its agent (if any) WITHOUT prop-drilling through the deep
 * sidebar → project → workspace → list → item chain. A SINGLE
 * [`AgentSessionsProvider`] near the sidebar root holds the one subscription; rows read
 * it cheaply. The default is an empty map so a row rendered OUTSIDE the provider (e.g. an
 * isolation test) simply shows the generic terminal icon — never throws.
 */
const AgentSessionsContext = createContext<ReadonlyMap<string, string>>(new Map());

/**
 * Provider that owns the ONE `agent_active_sessions` subscription (via
 * [`useActiveAgentSessions`]) and shares the resulting map with all descendant terminal
 * rows. Mount it once around the sidebar.
 */
export function AgentSessionsProvider({ children }: { children: ReactNode }) {
  const byTerminal = useActiveAgentSessions();
  return (
    <AgentSessionsContext.Provider value={byTerminal}>{children}</AgentSessionsContext.Provider>
  );
}

/**
 * Read the active `agent_kind` for ONE terminal from the shared context (finding #55), or
 * `null` when that terminal has no live session. Total + side-effect-free: a row outside
 * the provider reads the empty default → `null` → keeps the generic terminal icon.
 */
export function useTerminalAgentKind(terminalId: string): string | null {
  const byTerminal = useContext(AgentSessionsContext);
  return byTerminal.get(terminalId) ?? null;
}
