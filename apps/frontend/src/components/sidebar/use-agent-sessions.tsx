import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { nyxBridge } from "@/bridge";

/**
 * The Tauri event emitted whenever an agent session starts/ends OR a per-turn activity
 * hook fires (`bridge::AGENT_SESSIONS_CHANGED_EVENT`). A coalescing "re-pull" tick with
 * no payload: the hook re-fetches `agent_active_sessions` AND `agent_activity_snapshot`
 * to stay in sync, the same pattern the terminal deck uses for `terminals://changed`.
 */
const AGENT_SESSIONS_CHANGED_EVENT = "agent-sessions://changed" as const;

/**
 * One active agent session pair from `agent_active_sessions` (mirrors
 * `db::ActiveAgentSession`): which terminal hosts a live session, of which agent kind.
 */
interface ActiveAgentSession {
  terminal_id: string;
  agent_kind: string;
}

/**
 * The RUNTIME activity kinds a terminal's agent can be in (the live dot), mirroring
 * `nyx_core::agent_activity::Activity` flattened to a string:
 *  - `working` — Claude is on the current turn (between a prompt and its `Stop`) → the
 *    RUNNING dot. This REPLACES the PTY `busy` bit for an agent-hosting terminal.
 *  - `waiting` — Claude is blocked on the user (a permission/input `Notification`).
 *  - `idle`    — between turns; the dot follows nothing (only the session icon).
 * NEVER persisted: the map is empty at boot, so a resumed session starts `idle` (no
 * phantom running can survive a restart).
 */
export type AgentActivityKind = "working" | "waiting" | "idle";

/** One terminal's runtime activity snapshot (mirrors the napi `AgentActivitySnapshot`). */
interface AgentActivityRow {
  terminal_id: string;
  activity: AgentActivityKind;
  /**
   * `true` when a turn finished (`Stop`) and the user has NOT yet viewed the terminal —
   * the focus-aware "response ready" green dot, the SAME semantics as a settled
   * `exec_state_unread`. Cleared on focus via `agent_mark_ready_read`.
   */
  ready_unread: boolean;
  /**
   * The RED analogue of `ready_unread` — `true` when the last turn ended on an API error
   * (`StopFailure`) and the user has NOT yet viewed the terminal (#35). Cleared on focus via
   * `agent_mark_ready_read`, a new turn, and session restart. Optional like `plugin_outdated`.
   */
  error_unread?: boolean;
  /**
   * `true` when the nyx plugin THIS session loaded is OLDER/different than the version
   * nyx bundles (#18b) — the per-session "plugin périmé" badge inviting a session restart.
   * Runtime-only (never persisted); set once at SessionStart, cleared on session restart.
   */
  plugin_outdated?: boolean;
}

/** The per-terminal activity the row consumes (the `terminal_id` is the map key). */
export interface AgentActivity {
  activity: AgentActivityKind;
  readyUnread: boolean;
  /**
   * The RED analogue of `readyUnread` — the last turn ended on an API error (`StopFailure`)
   * and the user has not yet viewed the terminal (#35). The row renders a red dot (priority
   * over the green ready), cleared on focus, a new turn, and session restart.
   */
  errorUnread: boolean;
  /**
   * `true` when the session's loaded nyx plugin is stale vs. the bundled version (#18b).
   * The row shows a muted ⚠ affordance inviting a session restart. See `AgentActivityRow`.
   */
  pluginOutdated: boolean;
}

/**
 * The shared agent-sessions state every terminal ROW reads: the live session-icon map
 * (`terminal_id → agent_kind`) AND the live activity map (`terminal_id → AgentActivity`).
 * Both are re-pulled on `agent-sessions://changed` so a session start/end OR a per-turn
 * hook updates the row's icon + dot live, with ONE subscription for all rows.
 */
interface AgentSessionsState {
  byKind: ReadonlyMap<string, string>;
  byActivity: ReadonlyMap<string, AgentActivity>;
}

const EMPTY_STATE: AgentSessionsState = {
  byKind: new Map(),
  byActivity: new Map(),
};

/**
 * A failed IPC read, kept DISTINCT from a successful empty read (`[]`). A `.catch(() => [])`
 * collapses "the backend errored" into "no sessions", which would WRONGLY wipe the icon/dot
 * on a transient failure; we map a failure to this sentinel instead so the merge below skips
 * that channel and keeps the last good state for it.
 */
const READ_FAILED = Symbol("read-failed");

/**
 * Live agent-sessions state: pulls the session-icon map AND the activity map once on
 * mount, then re-pulls BOTH on every `agent-sessions://changed` event (emitted by the
 * backend after a SessionStart/SessionEnd, a per-turn activity hook, or a PTY-death
 * clear). StrictMode-safe — the listener is torn down on cleanup and a late `subscribe`
 * resolve after unmount is unlistened immediately.
 *
 * RESILIENCE (the "icône/dot qui saute" fix): the re-pull is a MERGE over the previous
 * state, not a blind whole-state replace, so a transient/incomplete read never blinks a
 * live row:
 *  - a FAILED read of a channel ([`READ_FAILED`]) leaves that channel's map UNTOUCHED — the
 *    old `.catch(() => [])` collapsed an IPC error into "empty" and wiped the icon/dot, the
 *    very transient blink we are killing. The next event recovers it.
 *  - a SUCCESSFUL read is the authority for its channel and is applied wholesale: the icon
 *    map (`byKind`) follows `agent_active_sessions` (a legitimately removed session — a real
 *    SessionEnd / close / supersede — IS dropped) and the activity map (`byActivity`)
 *    follows `agent_activity_snapshot` (an idled terminal drops out so its dot clears).
 *
 * The `/clear` blink — Claude fires SessionEnd then SessionStart on the SAME terminal, which
 * would briefly empty the active list — is fixed AT THE CORE (the `SessionEnd { reason:
 * clear|resume }` internal-transition guard keeps the row `active` across the swap), so a
 * SUCCESSFUL read never even sees the gap. This hook only has to stop an IPC FAILURE from
 * masquerading as "everything went away". Both maps start EMPTY at boot (no phantom across a
 * restart).
 */
export function useAgentSessions(): AgentSessionsState {
  const [state, setState] = useState<AgentSessionsState>(EMPTY_STATE);

  useEffect(() => {
    let torndown = false;

    const pull = () => {
      // Two reads in parallel: the persisted session icons + the runtime activity dots.
      // A rejected read resolves to READ_FAILED (NOT []) so the merge can tell "no sessions"
      // from "the read failed" and never wipe a live row on a transient IPC error.
      void Promise.all([
        nyxBridge
          .invoke<ActiveAgentSession[]>("agent_active_sessions")
          .catch((): typeof READ_FAILED => READ_FAILED),
        nyxBridge
          .invoke<AgentActivityRow[]>("agent_activity_snapshot")
          .catch((): typeof READ_FAILED => READ_FAILED),
      ])
        .then(([sessions, activity]) => {
          if (torndown) return;
          setState((prev) => {
            // ICON MAP — a SUCCESSFUL read is the authority (legitimate removals are
            // honoured); a FAILED read keeps the last good map (no blink on a transient IPC
            // error). The `/clear` gap is closed at the core, so a successful read is stable.
            let byKind = prev.byKind;
            if (Array.isArray(sessions)) {
              const next = new Map<string, string>();
              for (const r of sessions) next.set(r.terminal_id, r.agent_kind);
              byKind = next;
            }
            // ACTIVITY MAP — same rule: the live runtime authority on success (an idled
            // terminal leaves so its dot clears), the last good map on a failed read.
            let byActivity = prev.byActivity;
            if (Array.isArray(activity)) {
              const next = new Map<string, AgentActivity>();
              for (const a of activity) {
                next.set(a.terminal_id, {
                  activity: a.activity,
                  readyUnread: a.ready_unread,
                  errorUnread: a.error_unread ?? false,
                  pluginOutdated: a.plugin_outdated ?? false,
                });
              }
              byActivity = next;
            }
            if (byKind === prev.byKind && byActivity === prev.byActivity) return prev;
            return { byKind, byActivity };
          });
        })
        // A transient failure leaves the current state; the next event recovers it.
        .catch(() => {});
    };

    // Initial state.
    pull();

    // Re-pull on every change tick.
    let unlisten: (() => void) | undefined;
    void nyxBridge
      .subscribe(AGENT_SESSIONS_CHANGED_EVENT, () => {
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
      // If the event channel is unavailable (e.g. a test/host without the event bridge),
      // the initial pull still seeded the state; we just won't get live updates. Swallow
      // so a missing bridge never becomes an unhandled rejection.
      .catch(() => {});

    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, []);

  return state;
}

/**
 * BACK-COMPAT accessor: just the session-icon map (`terminal_id → agent_kind`), kept so
 * existing call-sites / tests that only need the icon map are unchanged.
 */
export function useActiveAgentSessions(): ReadonlyMap<string, string> {
  return useAgentSessions().byKind;
}

/**
 * Context carrying the live session + activity maps so every terminal ROW can read its
 * agent (icon) AND its activity (dot) WITHOUT prop-drilling through the deep
 * sidebar → project → workspace → list → item chain. A SINGLE [`AgentSessionsProvider`]
 * near the sidebar root holds the one subscription; rows read it cheaply. The default is
 * the empty state so a row rendered OUTSIDE the provider (e.g. an isolation test) simply
 * shows the generic terminal icon + no dot — never throws.
 */
const AgentSessionsContext = createContext<AgentSessionsState>(EMPTY_STATE);

/**
 * Provider that owns the ONE agent-sessions subscription (via [`useAgentSessions`]) and
 * shares the resulting maps with all descendant terminal rows. Mount it once around the
 * sidebar.
 */
export function AgentSessionsProvider({ children }: { children: ReactNode }) {
  const state = useAgentSessions();
  return <AgentSessionsContext.Provider value={state}>{children}</AgentSessionsContext.Provider>;
}

/**
 * Read the active `agent_kind` for ONE terminal from the shared context (finding #55), or
 * `null` when that terminal has no live session. Total + side-effect-free: a row outside
 * the provider reads the empty default → `null` → keeps the generic terminal icon.
 */
export function useTerminalAgentKind(terminalId: string): string | null {
  return useContext(AgentSessionsContext).byKind.get(terminalId) ?? null;
}

/**
 * Read the RUNTIME agent activity for ONE terminal from the shared context (the live
 * dot), or `null` when the terminal has no live activity (→ idle, no ready notification).
 * Total + side-effect-free: a row outside the provider reads the empty default → `null`.
 */
export function useTerminalAgentActivity(terminalId: string): AgentActivity | null {
  return useContext(AgentSessionsContext).byActivity.get(terminalId) ?? null;
}
