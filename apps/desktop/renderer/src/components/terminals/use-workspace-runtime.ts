import type { ExecState, TerminalRecord } from "@/components/sidebar/use-terminals";

/**
 * `use-workspace-runtime` — the wire SHAPES of a workspace's LIVE runtime aggregate (the
 * `WorkspaceRuntimeSnapshot` served by the `workspace_runtime_snapshot` db-command, the same
 * authority the MCP `workspace_runtime` tool serves, PDR-0005). These types are the shared read
 * model the whole Terminals feature-area consumes: `use-project-runtime` (the live hook), the
 * runtime tree, the accordion, the pane host, and the run-state derivations.
 *
 * The LIVE engine that keeps a snapshot fresh (the `RUNTIME_CHANGED_EVENTS` topics + the
 * subscribe/re-pull/teardown primitive) lives in `use-runtime-live.ts` — its single source of
 * truth — and is consumed by `use-project-runtime`. There is no per-workspace read hook here:
 * the project→workspaces page reads MANY workspaces via `use-project-runtime` (PDR-0008 — the
 * workspace stays the runtime pivot in the DB, not a screen).
 */

/** A terminal record in the snapshot (the wire shape from `db::Terminal`, snake_case). */
export interface WorkspaceRuntimeTerminal {
  readonly id: string;
  readonly cwd: string;
  readonly label: string | null;
  readonly workspace_id: string | null;
  /** Reuses `TerminalRecord.status`'s literal (`"alive" | "closed"`) — the same wire field. */
  readonly status: TerminalRecord["status"];
  readonly exec_state: ExecState;
  /**
   * The "unseen settled result" flag (the wire `db::Terminal.exec_state_unread`, flattened into
   * the snapshot terminal). True for a `success`/`error` the user has NOT yet viewed on this
   * terminal; a mark-read (`terminal_exec_mark_read`) clears it. This is what turns the settled
   * success/error dot into a NOTIFICATION marker (finding 53) — it shows only while unread AND
   * the terminal is not the one being viewed.
   */
  readonly exec_state_unread: boolean;
  /**
   * The OS BUSY bit (v2.5.5 review T1): `true` while a command runs in this terminal's
   * FOREGROUND process group (a build, a `sleep`, a hand-launched agent), `false` at the idle
   * prompt. It is a LIVE signal the backend OVERLAYS onto the snapshot from the PTY layer
   * (`apply_terminal_live_overlay`), NEVER persisted — so a restored idle terminal reads
   * `false` by construction (no phantom running). It is the running source OSC 133's
   * `exec_state` cannot express (that never carries `running`); `terminalRunState` folds it in.
   * Defaults to `false` when absent (a pure-DB / MCP snapshot with no live overlay).
   */
  readonly busy?: boolean;
  /**
   * The PASSIVELY-DETECTED agent kind (v2.5.5 review T3): when the terminal's FOREGROUND command
   * is a known agent (resolved by the backend's provider-agnostic `AgentRegistry::detect` over the
   * foreground `comm`), this is that agent's `agent_kind` (e.g. `"claude_code"`) — else `null`.
   * It lets the tree ADOPT a hand-launched agent (show its provider icon + a busy-derived status)
   * WITHOUT waiting for the hook's `SessionStart`. It is a LIVE overlay, DISTINCT from
   * `sessions[]`, and creates NO agent session: the RECONCILIATION lives in the row — a real hook
   * `sessions[]` entry (resumable) takes PRECEDENCE over this passive kind (no double icon), and
   * when the agent stops (`null`) the passive icon disappears. Absent on a pure-DB / MCP snapshot.
   */
  readonly detected_agent_kind?: string | null;
  /** The agent sessions hosted on this terminal (every state, oldest first). */
  readonly sessions: readonly WorkspaceRuntimeSession[];
}

/** An agent session in the snapshot (the wire shape from `db::AgentSession`). */
export interface WorkspaceRuntimeSession {
  readonly id: string;
  readonly terminal_id: string;
  readonly workspace_id: string | null;
  readonly agent_kind: string;
  readonly external_session_id: string;
  readonly state: string;
}

/** A managed-command service in the snapshot, with its live run state overlaid. */
export interface WorkspaceRuntimeService {
  /** `command_instances.id` — the launchable id. */
  readonly id: string;
  readonly workspace_id: string;
  readonly name: string;
  readonly command: string;
  /** The FACTUAL current run state (live-runner overlay, else persisted). */
  readonly run_state: ExecState;
  readonly exit_code: number | null;
  readonly unread: boolean;
}

/** The workspace runtime aggregate (the wire shape from `WorkspaceRuntimeSnapshot`). */
export interface WorkspaceRuntimeSnapshot {
  readonly workspace: { readonly id: string; readonly name: string; readonly path: string };
  readonly terminals: readonly WorkspaceRuntimeTerminal[];
  readonly agent_sessions: readonly WorkspaceRuntimeSession[];
  readonly services: readonly WorkspaceRuntimeService[];
}
