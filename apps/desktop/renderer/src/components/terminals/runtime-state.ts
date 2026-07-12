import type { AgentActivity } from "@/components/sidebar/use-agent-sessions";
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
 * A terminal's run state — the TERMINAL MODE (v2.5.5 review T1): the OS `busy` bit takes
 * priority over the OSC-133 `exec_state`. A foreground command running (a build, a `sleep`, a
 * hand-launched agent) sets `busy` LIVE from the PTY layer, which no `exec_state` can express
 * (OSC 133 only ever settles to `success`/`error`, never `running`) — so a busy terminal reads
 * `running` and lights the LINE's dot. When idle at the prompt (`busy` false/absent) it falls
 * back to the settled `exec_state` (the success/error notification badge). This is the OS
 * busy/idle "mode terminal"; the AGENT mode (working/waiting/ready) is layered on top by the
 * row for terminals that host an agent session (review T2).
 */
export function terminalRunState(terminal: WorkspaceRuntimeTerminal): ExecState {
  if (terminal.busy) return "running";
  return terminal.exec_state;
}

/**
 * The presentational dot state of a terminal LINE — the TWO-MODE fold (review T2), the SAME
 * logic v1's `terminal-item` applies, brought to the runtime-tree line (not just the workspace
 * rollup). It reuses the live agent-activity read model (`AgentActivity`, from the hook that
 * feeds the rollup) — no second store.
 *
 *  - AGENT MODE (the line CARRIES an agent — it shows a provider glyph: a live hook session
 *    {@link activeAgentSession} OR a passively-detected foreground agent `detected_agent_kind`,
 *    the SAME {@link lineAgentKind} the row resolves for the icon). The agent activity PRIMES the
 *    dot — `working → running` (blue), `waiting → waiting` (yellow); an idle agent between turns
 *    shows its settled focus-aware notification, `ready → success` (green) or `error → error`
 *    (red, priority), suppressed while the row is the viewed one (`selected`) and once read. When
 *    there is NO live activity entry (`activity === undefined`) OR the activity is a plain idle
 *    with no notification, the dot is `idle` (empty). The always-on OS `busy` bit is IGNORED for
 *    an agent line (v1 parity): a fresh/idle agent at the prompt shows NO dot — it does NOT fall
 *    back to the busy/exec_state terminal mode (the bug this fixes).
 *  - TERMINAL MODE (a plain PTY — NO agent glyph): falls back to the OS busy / exec_state terminal
 *    mode via {@link terminalRunState} (task T1), notification-aware over `terminalUnread`.
 *
 * Pure so it is unit- and render-testable. `activity` is the resolved per-terminal activity
 * (`byActivity.get(id)`), `undefined` for a terminal with no live agent activity.
 */
export function terminalLineState(
  terminal: WorkspaceRuntimeTerminal,
  activity: AgentActivity | undefined,
  opts: { terminalUnread: boolean; selected: boolean },
): RunState {
  // AGENT MODE — the line CARRIES an agent (it renders a provider glyph). This is the ONLY gate:
  // once a line hosts a session OR passively detects an agent, the OS `busy` bit is IGNORED and
  // the dot comes SOLELY from the live agent activity (v1 parity). No activity entry (idle at the
  // prompt) ⇒ `idle` (empty), NEVER the busy-derived `running`.
  if (lineAgentKind(terminal) !== undefined) {
    if (!activity) return "idle";
    // Live states are never notifications — always surfaced (selected/read irrelevant).
    if (activity.activity === "working") return "running";
    if (activity.activity === "waiting") return "waiting";
    // Idle agent between turns: the settled "response ready" (green) / turn-error (red)
    // notification — error outranks ready — with the SAME focus-aware unread semantics as a
    // terminal's exec_state (shown only while unread AND not the viewed row).
    if (activity.errorUnread) return notifiedRunState("error", true, opts.selected);
    if (activity.readyUnread) return notifiedRunState("success", true, opts.selected);
    return "idle";
  }
  // TERMINAL MODE — the OS busy / exec_state dot (task T1), notification-aware.
  return notifiedRunState(terminalRunState(terminal), opts.terminalUnread, opts.selected);
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
 * Resolve ONE terminal's live AGENT activity — the FULL {@link AgentActivity}
 * (`activity` + `readyUnread`/`errorUnread`/`pluginOutdated`), or `undefined` when the terminal
 * has no live agent activity. This is the SAME read model the sidebar rows AND the terminal LINE
 * consume (`agent_activity_snapshot`, exposed as the `byActivity` map of `useAgentSessions`) — a
 * caller passes `(id) => byActivity.get(id)`. It carries the WHOLE activity (not just the kind) so
 * the rollup can reuse {@link terminalLineState} verbatim: the agent's settled `readyUnread`
 * (green) / `errorUnread` (red) notification remounts to the aggregate exactly as it does on the
 * line, not only the live `working`/`waiting`. Kept as a function (not the map) so the rollup
 * depends on the shape it needs, not the whole hook.
 */
export type AgentActivityLookup = (terminalId: string) => AgentActivity | undefined;

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
 * A terminal's run state for the rollup — the EXACT SAME agent-aware projection its LINE uses
 * ({@link terminalLineState} with `selected: false`), not a second, terminal-only fold. This is
 * the fix for the rollup ignoring the agent mode: the rollup now agregates what the line shows.
 *
 *  - AGENT MODE (the terminal carries an agent glyph — a live session or a passively-detected
 *    foreground agent): the contribution comes SOLELY from the live `activity` — `working →
 *    running`, `waiting → waiting`, an idle agent's settled `readyUnread → success` /
 *    `errorUnread → error` notification (error outranks ready), and `idle` (no dot) when there is
 *    no activity entry. The OS `busy` bit is IGNORED (v1 parity): a busy-but-idle agent at the
 *    prompt contributes `idle`, never `running`.
 *  - TERMINAL MODE (a plain PTY): the OS busy / `exec_state` fold, NOTIFICATION-AWARE. A settled
 *    `success`/`error` contributes ONLY while UNREAD: unread in the snapshot (`exec_state_unread`)
 *    AND not locally consumed (`consumed`, the session registry that closes the mark-read
 *    staleness gap). `running` (busy) always contributes.
 *
 * `selected` is `false` here: a rollup is not a viewed row, so a settled result is cleared only by
 * being read (its own `readyUnread`/`errorUnread` / `exec_state_unread` flag), never by focus.
 */
function terminalRollupState(
  terminal: WorkspaceRuntimeTerminal,
  activity: AgentActivity | undefined,
  consumed?: ConsumedLookup,
): RunState {
  const terminalUnread =
    terminal.exec_state_unread && !(consumed?.("terminal", terminal.id) ?? false);
  return terminalLineState(terminal, activity, { terminalUnread, selected: false });
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
 * The whole-workspace run-state rollup — the aggregate of every terminal's AGENT-AWARE line
 * state ({@link terminalRollupState}) and every service's run state, folded by the shared
 * {@link aggregateState}/`STATE_PRIORITY` (waiting > error > success > running > idle).
 *
 * AGENT-AWARE (the fix): each terminal contributes the EXACT projection its own LINE shows —
 * the two-mode {@link terminalLineState}, primed by the terminal's live agent activity resolved
 * via the optional `agentActivity` lookup (`byActivity.get(id)`). So an agent that is `working`
 * contributes `running`, `waiting` contributes yellow, a settled `readyUnread`/`errorUnread`
 * contributes green/red, and an idle/busy agent at the prompt contributes `idle` (the OS `busy`
 * bit is IGNORED for an agent, exactly as on the line). A plain PTY still contributes its OS
 * busy / `exec_state` (busy → running).
 *
 * NOTIFICATION-AWARE: a settled `success`/`error` contributes ONLY while UNREAD — for a plain
 * terminal, unread in the snapshot (`exec_state_unread`) AND not locally CONSUMED (the optional
 * `consumed` registry, `use-consumed-notifications`); for an agent, its own `readyUnread`/
 * `errorUnread` flag. Once a leaf is read/consumed its green/red stops contributing here exactly
 * as its own dot disappears — no second notif rule. A `running`/`waiting` leaf always contributes.
 * Both lookups are optional: without `agentActivity` an agent terminal has no activity entry so it
 * contributes `idle` (busy ignored, as on the line); without `consumed` the raw snapshot `unread`
 * flags drive the notification axis. The result is one {@link RunState} `aggregateState` returns.
 */
export function workspaceRunState(
  snapshot: WorkspaceRuntimeSnapshot,
  agentActivity?: AgentActivityLookup,
  consumed?: ConsumedLookup,
): RunState {
  return aggregateState([
    ...snapshot.terminals.map((t) => terminalRollupState(t, agentActivity?.(t.id), consumed)),
    ...snapshot.services.map((s) => serviceRollupState(s, consumed)),
  ]);
}

/**
 * The whole-PROJECT run-state rollup — every one of the project's workspaces folded into ONE
 * state, shown on the project's rail dot. It reuses the workspace rollup ({@link
 * workspaceRunState}) and the shared {@link aggregateState} priority, so the project dot and
 * the workspace dot agree by construction (no second aggregation rule) — INCLUDING the full
 * AGENT-AWARE axis (via `agentActivity`, the per-terminal {@link AgentActivity}) AND the
 * NOTIFICATION-AWARE consumed/unread axis (via `consumed`), both threaded through the same
 * workspace rollup. An empty project (no loaded
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

/**
 * The agent kind a terminal LINE carries — the `providerKind` its row resolves for the glyph, or
 * `undefined` for a plain terminal. A live HOOK session ({@link activeAgentSession}, authoritative,
 * resumable) takes PRECEDENCE; only when there is none does the PASSIVELY-DETECTED foreground agent
 * (`detected_agent_kind`, no session) supply the kind (review T3). This is the SINGLE predicate
 * that puts a line in AGENT mode: "the line shows an agent glyph" ⟺ "agent mode (OS busy ignored)".
 * The row uses the identical expression for its icon, so icon and dot-mode agree by construction.
 */
export function lineAgentKind(terminal: WorkspaceRuntimeTerminal): string | undefined {
  return activeAgentSession(terminal)?.agent_kind ?? terminal.detected_agent_kind ?? undefined;
}
