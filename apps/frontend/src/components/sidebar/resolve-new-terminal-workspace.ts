import type { ProjectTree } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

/**
 * The destination for a smart "new terminal" (Ctrl+T): the path to spawn the
 * shell at plus the workspace id to bind it to. `null` means "no inheritable
 * workspace" → the caller falls back to a LOOSE terminal.
 */
export interface NewTerminalTarget {
  /** Workspace path to spawn the new shell at (== the bound workspace's path). */
  path: string;
  /** Workspace id to attach the new terminal to (manual binding). */
  workspaceId: string;
}

/**
 * Resolve where a `Ctrl+T` "new terminal" should open (FEEDBACK #27): in the
 * SAME workspace as the ACTIVE terminal, falling back to a loose terminal.
 *
 * PURE so it can be unit-tested without mounting the manager (mirrors the
 * codebase's "extract pure, test it" pattern — terminal-geometry.ts /
 * use-at-bottom.ts). The manager wires it as the `onNew` handler: a non-null
 * result is created+attached exactly like the per-workspace "+", a `null`
 * result spawns a loose terminal.
 *
 * Returns `null` (→ loose fallback) when:
 *  - there is no active terminal (`activeId` is null / unknown), OR
 *  - the active terminal is itself loose (no `workspace_id`), OR
 *  - the active terminal's `workspace_id` no longer resolves to a known
 *    workspace in the project tree (stale binding — never spawn at a phantom
 *    path).
 */
export function resolveNewTerminalWorkspace(
  activeId: string | null,
  terminals: TerminalRecord[],
  projects: ProjectTree[],
): NewTerminalTarget | null {
  if (activeId === null) return null;
  const active = terminals.find((t) => t.id === activeId);
  if (!active || !active.workspace_id) return null;
  const workspaceId = active.workspace_id;
  for (const tree of projects) {
    const ws = tree.workspaces.find((w) => w.id === workspaceId);
    if (ws) return { path: ws.path, workspaceId: ws.id };
  }
  return null;
}
