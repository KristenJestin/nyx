import { useCallback, useMemo } from "react";

import { useProjects } from "@/components/sidebar/use-projects";
import { useTerminals } from "@/components/sidebar/use-terminals";
import { useTerminalShortcuts } from "@/components/sidebar/use-terminal-shortcuts";
import { resolveNewTerminalWorkspace } from "@/components/sidebar/resolve-new-terminal-workspace";
import { useRuntimeSelection } from "./runtime-selection";
import type { WorkspaceRuntimeTerminal } from "./use-workspace-runtime";

/**
 * `use-terminal-actions` — the RESTORED terminal-management affordances of the OLD shell,
 * recabled for the new project→Terminals page (regression finding 50). It REUSES the sidebar
 * primitives verbatim rather than reimplementing them:
 *
 *  - `useTerminals` (the CRUD single source of truth: `create` / `attach` / `close`, and the
 *    DB `create_terminal`/`attach_terminal`/`close_terminal` calls that emit `terminals://changed`
 *    — the very topic the page's runtime snapshot re-pulls on, so a created/closed terminal
 *    appears/disappears in the tree LIVE);
 *  - `resolveNewTerminalWorkspace` (the pure "open Ctrl+T in the ACTIVE terminal's workspace,
 *    else loose" resolution — FEEDBACK #27);
 *  - `useTerminalShortcuts` (the global Ctrl+T / Ctrl+W / cycle chord bindings).
 *
 * The PAGE selection (`useRuntimeSelection`) is the pivot for "which terminal is active": the
 * new-terminal target is resolved from the SELECTED terminal, close targets the selected one,
 * and the cycle steps the selection across the page's terminal list (`orderedTerminals`, the
 * snapshot order the tree shows). Creating a terminal selects it once it lands in the snapshot
 * (handled by the page's auto-select-first effect + `select` here).
 */
export interface UseTerminalActions {
  /** Open a NEW terminal in `workspaceId` (bound + spawned at `path`), then select it. */
  newTerminalInWorkspace: (workspaceId: string, path: string) => Promise<void>;
  /** Close the currently-selected terminal (a no-op when a service/nothing is selected). */
  closeSelected: () => Promise<void>;
}

export function useTerminalActions(
  orderedTerminals: readonly WorkspaceRuntimeTerminal[],
): UseTerminalActions {
  const { projects } = useProjects();
  // Borrow the sidebar CRUD surface ONLY — the page's terminal set is driven by the runtime
  // snapshot, so we opt OUT of `useTerminals`' bootstrap "open a default terminal" (visiting the
  // page must never spawn a phantom loose terminal).
  const { terminals, create, attach, close } = useTerminals({ autoOpenDefault: false });
  const { selected, select } = useRuntimeSelection();

  const selectedTerminalId = selected?.kind === "terminal" ? selected.id : null;

  // Create a terminal at `path`, bind it to `workspaceId` (so the snapshot groups it under the
  // right workspace), and select it. Reuses the sidebar `create` + `attach` — the SAME path the
  // old per-workspace "+" used — so the new record persists and `terminals://changed` re-pulls
  // the page snapshot, surfacing the terminal in the tree and mounting its live pane.
  const newTerminalInWorkspace = useCallback(
    async (workspaceId: string, path: string) => {
      const row = await create(path);
      await attach(row.id, workspaceId, "manual");
      select({ kind: "terminal", id: row.id });
    },
    [create, attach, select],
  );

  // Ctrl+T "new terminal": open in the SAME workspace as the SELECTED terminal (else loose),
  // resolved by the shared pure `resolveNewTerminalWorkspace`. A `null` resolution (no bound
  // selection) opens a loose terminal, exactly like the old shell.
  const newTerminalSmart = useCallback(async () => {
    const target = resolveNewTerminalWorkspace(selectedTerminalId, terminals, projects);
    if (target) {
      await newTerminalInWorkspace(target.workspaceId, target.path);
      return;
    }
    const row = await create();
    select({ kind: "terminal", id: row.id });
  }, [selectedTerminalId, terminals, projects, newTerminalInWorkspace, create, select]);

  // Ctrl+W: close the selected terminal. Reuses the sidebar `close` (drops the record, persists
  // `closed`, emits `terminals://changed` → the tree drops the row). No-op unless a terminal is
  // selected (a selected service has nothing to close here).
  const closeSelected = useCallback(async () => {
    if (selectedTerminalId != null) await close(selectedTerminalId);
  }, [selectedTerminalId, close]);

  // Cycle the SELECTION across the page's terminal list (snapshot order). Wraps at the ends,
  // mirroring the old shell's `activeNext`/`activePrev`. Steps from the currently-selected
  // terminal; with none selected, jumps to the first/last.
  const cycle = useCallback(
    (delta: number) => {
      if (orderedTerminals.length === 0) return;
      const currentIndex = orderedTerminals.findIndex((t) => t.id === selectedTerminalId);
      const from = currentIndex === -1 ? (delta > 0 ? -1 : 0) : currentIndex;
      const nextIndex = (from + delta + orderedTerminals.length) % orderedTerminals.length;
      const next = orderedTerminals[nextIndex];
      if (next) select({ kind: "terminal", id: next.id });
    },
    [orderedTerminals, selectedTerminalId, select],
  );

  const handlers = useMemo(
    () => ({
      onNew: () => void newTerminalSmart(),
      onClose: () => void closeSelected(),
      onNext: () => cycle(1),
      onPrev: () => cycle(-1),
    }),
    [newTerminalSmart, closeSelected, cycle],
  );
  useTerminalShortcuts(handlers);

  return { newTerminalInWorkspace, closeSelected };
}
