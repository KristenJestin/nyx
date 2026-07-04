import type { AgentActivityKind } from "@/components/sidebar/use-agent-sessions";
import type { ExecState } from "@/components/sidebar/use-terminals";
import type { RunState } from "./status-dot";
import { aggregateState } from "./status-dot";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "./use-workspace-runtime";

/**
 * Pure run-state derivations for the Terminals page — the ONLY place the renderer reduces the
 * live runtime aggregate to the presentational state its dots consume. No business logic:
 * these are total projections over the read model the backend already computed. Kept
 * side-effect-free + colocated-tested so the visuals are provable.
 *
 * The leaf projections return the wire {@link ExecState} (`idle|running|success|error`) — the
 * exact type the shared `<CommandControls>` also consumes — which is a strict subset of the
 * dot's {@link RunState}, so it flows into `<StatusDot>` unchanged.
 */

/**
 * A terminal's run state: its `exec_state` from the aggregate. A terminal never "succeeds" on
 * its own (that is a command exit) — the aggregate already carries the OSC-133-derived
 * `exec_state`, so this is a straight read (kept as a named projection so rows never touch the
 * wire field directly).
 */
export function terminalRunState(terminal: WorkspaceRuntimeTerminal): ExecState {
  return terminal.exec_state;
}

/** A service's run state: the FACTUAL `run_state` overlaid by the backend, read 1:1. */
export function serviceRunState(service: WorkspaceRuntimeService): ExecState {
  return service.run_state;
}

/**
 * The NOTIFICATION-aware run state a ROW's dot renders (finding 53 — the Attention-Inbox
 * semantics). The settled `success`/`error` dot is not the plain current state: it is a marker
 * of an UNSEEN result, so it is shown ONLY while:
 *   - the result is `success`/`error`, AND
 *   - it is still UNREAD (`unread`), AND
 *   - the row is NOT the currently-selected/viewed one (`selected`) — you are on it, so it is
 *     seen.
 * When the row is selected, or the result has been read, the settled dot is suppressed (folded to
 * `idle`, which `isQuietState` hides). A LIVE `running`/`waiting` state is NEVER a notification —
 * it always shows, selected or not. Pure so it is unit-testable without rendering.
 */
export function notifiedRunState(state: ExecState, unread: boolean, selected: boolean): ExecState {
  // Live states are not notifications — always surfaced regardless of selection/unread.
  if (state === "running") return state;
  // Settled success/error is a NOTIFICATION: shown only when unread AND not the viewed row.
  if (state === "success" || state === "error") {
    return unread && !selected ? state : "idle";
  }
  return state;
}

/**
 * Resolve ONE terminal's live AGENT activity (`working | waiting | idle`) from its id, or
 * `undefined` when the terminal has no live agent activity. This is the SAME read model the
 * sidebar rows consume (`agent_activity_snapshot`, exposed as the `byActivity` map of
 * `useAgentSessions`) — a caller passes `(id) => byActivity.get(id)?.activity`. Kept as a
 * function (not the map) so the rollup depends on the shape it needs, not the whole hook.
 */
export type AgentActivityLookup = (terminalId: string) => AgentActivityKind | undefined;

/**
 * Whether a runtime leaf (a terminal or a service) has been CONSUMED (visited/read) this
 * session — the SAME registry the rows use (`use-consumed-notifications`, a caller passes
 * `registry.isConsumed`). Threaded into the rollups so an already-seen settled result stops
 * contributing to the aggregate exactly as it stops contributing to its own row's dot. Kept as
 * a plain lookup (not the hook) so the rollup functions stay pure + unit-testable; absent ⇒
 * nothing is treated as locally consumed (the snapshot `unread` flag alone drives the rollup).
 */
export type ConsumedLookup = (kind: "terminal" | "service", id: string) => boolean;

/**
 * A terminal's NOTIFICATION-AWARE run state for the rollup — the SAME projection its row uses.
 * A settled `success`/`error` contributes ONLY while it is UNREAD: unread in the snapshot
 * (`exec_state_unread`) AND not locally consumed (`consumed`, the session registry that closes
 * the mark-read staleness gap). `running` always contributes. `selected` is `false` here: a
 * rollup is not a viewed row, so the only thing that clears a settled result is being read.
 */
function terminalRollupState(
  terminal: WorkspaceRuntimeTerminal,
  consumed?: ConsumedLookup,
): RunState {
  const unread = terminal.exec_state_unread && !(consumed?.("terminal", terminal.id) ?? false);
  return notifiedRunState(terminalRunState(terminal), unread, false);
}

/**
 * A service's NOTIFICATION-AWARE run state for the rollup — the SAME projection its row uses.
 * A settled `success`/`error` contributes ONLY while unread (`service.unread`) AND not locally
 * consumed. `running` always contributes; `selected` is `false` (a rollup is not a viewed row).
 */
function serviceRollupState(service: WorkspaceRuntimeService, consumed?: ConsumedLookup): RunState {
  const unread = service.unread && !(consumed?.("service", service.id) ?? false);
  return notifiedRunState(serviceRunState(service), unread, false);
}

/**
 * Whether a terminal is hosting an agent that is BLOCKED ON THE USER — the `waiting` state
 * (an `AskUserQuestion` / permission / elicitation prompt in flight). This is the ONE bit the
 * exec-state rollup cannot see: `waiting` is NOT an `exec_state` and NOT the DB session
 * `state` (`active`/`ended`/…) — it lives only in the runtime `agent_activity_snapshot`. We
 * derive it exactly like the sidebar row does (finding: reuse, don't reimplement): the
 * terminal must host a LIVE agent session ({@link activeAgentSession}) AND its live activity
 * (via `lookup`) must be `waiting`. A non-agent terminal, or an agent that is `working`/idle,
 * never contributes `waiting`.
 */
function terminalWaiting(terminal: WorkspaceRuntimeTerminal, lookup: AgentActivityLookup): boolean {
  if (!activeAgentSession(terminal)) return false;
  return lookup(terminal.id) === "waiting";
}

/**
 * The whole-workspace run-state rollup — the aggregate of every terminal's exec state, every
 * service's run state, AND every agent that is BLOCKED ON THE USER (`waiting`, yellow).
 *
 * NOTIFICATION-AWARE (finding): the exec-state axis rolls up the SAME notification-aware
 * projection the rows use ({@link notifiedRunState}), not the raw `exec_state`/`run_state`. A
 * settled `success`/`error` contributes to the rollup ONLY while it is still UNREAD — unread in
 * the snapshot (`exec_state_unread` / `unread`) AND not locally CONSUMED (the optional
 * `consumed` registry lookup, `use-consumed-notifications`). Once a leaf is visited/read, its
 * green/red stops contributing here exactly as its own dot disappears — no second notif rule.
 * A `running` leaf always contributes (blue); the agent `waiting` axis always contributes
 * (yellow) via the optional `agentActivity` lookup, even when the terminal's own `exec_state`
 * is idle. Both lookups are optional: without them the rollup is exec-state-only over the raw
 * `unread` flags (the historical behaviour), so callers that cannot see agent activity / the
 * consumed registry are unaffected. The result is one {@link RunState} `aggregateState` returns.
 */
export function workspaceRunState(
  snapshot: WorkspaceRuntimeSnapshot,
  agentActivity?: AgentActivityLookup,
  consumed?: ConsumedLookup,
): RunState {
  const waiting: RunState[] = agentActivity
    ? snapshot.terminals
        .filter((t) => terminalWaiting(t, agentActivity))
        .map((): RunState => "waiting")
    : [];
  return aggregateState([
    ...waiting,
    ...snapshot.terminals.map((t) => terminalRollupState(t, consumed)),
    ...snapshot.services.map((s) => serviceRollupState(s, consumed)),
  ]);
}

/**
 * The whole-PROJECT run-state rollup — every one of the project's workspaces folded into ONE
 * state, shown on the project's rail dot. It reuses the workspace rollup ({@link
 * workspaceRunState}) and the shared {@link aggregateState} priority, so the project dot and
 * the workspace dot agree by construction (no second aggregation rule) — INCLUDING the agent
 * `waiting` axis (via `agentActivity`) AND the NOTIFICATION-AWARE consumed/unread axis (via
 * `consumed`), both threaded through the same workspace rollup. An empty project (no loaded
 * snapshots) rolls up to `idle` — which the shared `isQuietState` hides on the rail.
 */
export function projectRunState(
  snapshots: readonly WorkspaceRuntimeSnapshot[],
  agentActivity?: AgentActivityLookup,
  consumed?: ConsumedLookup,
): RunState {
  return aggregateState(snapshots.map((s) => workspaceRunState(s, agentActivity, consumed)));
}

/** The wire session state a `db::AgentSession` reports (mirrors `db::SESSION_STATE_*`). */
const SESSION_STATE_ENDED = "ended";

/**
 * The agent session a terminal is currently HOSTING, if any — the last session that is not
 * `ended` (anything but `ended` is treated as live). Newest wins (the aggregate lists sessions
 * oldest-first). Returns `undefined` for a plain terminal. This is how the tree surfaces an
 * agent WITHOUT a cosmetic Agents category: the hosting terminal's glyph becomes the provider
 * logo.
 */
export function activeAgentSession(
  terminal: WorkspaceRuntimeTerminal,
): WorkspaceRuntimeTerminal["sessions"][number] | undefined {
  for (let i = terminal.sessions.length - 1; i >= 0; i--) {
    const session = terminal.sessions[i];
    if (session && session.state !== SESSION_STATE_ENDED) return session;
  }
  return undefined;
}
