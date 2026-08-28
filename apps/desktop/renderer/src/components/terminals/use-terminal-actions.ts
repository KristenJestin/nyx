import { useCallback, useMemo, useRef } from "react";

import { useProjects } from "@/components/sidebar/use-projects";
import { useTerminals } from "@/components/sidebar/use-terminals";
import {
  useTerminalShortcuts,
  type ShortcutHandlers,
} from "@/components/sidebar/use-terminal-shortcuts";
import { resolveNewTerminalWorkspace } from "@/components/sidebar/resolve-new-terminal-workspace";
import { spliceWorkspaceOrder, type ReorderGroup } from "@/components/sidebar/reorder-utils";
import { useRuntimeSelection } from "./runtime-selection";
import type { WorkspaceRuntimeTerminal } from "@/components/runtime/use-workspace-runtime";

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
/** The PANE chords `useTerminalActions` forwards to the one shortcut registration it owns. */
export type PaneShortcutHandlers = Pick<
  ShortcutHandlers,
  | "onSplitRight"
  | "onSplitDown"
  | "onFocusLeft"
  | "onFocusRight"
  | "onFocusUp"
  | "onFocusDown"
  | "onClosePane"
>;

export interface UseTerminalActions {
  /** Open a NEW terminal in `workspaceId` (bound + spawned at `path`), then select it. */
  newTerminalInWorkspace: (workspaceId: string, path: string) => Promise<void>;
  /**
   * Create a terminal in `workspaceId` (bound + spawned at `path`) and resolve to its RECORD ID,
   * WITHOUT selecting it — the seam the pane SPLIT creates through.
   *
   * The split owns the selection itself: it places the new leaf and focuses that pane, and a
   * `select` fired here would race that (the tiling would briefly show the new terminal SOLO
   * before the leaf lands). Everything else — `create` + `attach`, the SAME sidebar CRUD, the same
   * `terminals://changed` push — is identical to {@link newTerminalInWorkspace}, which is now a
   * thin "…and select it" over this.
   */
  createTerminalInWorkspace: (workspaceId: string, path: string) => Promise<string>;
  /** Close the currently-selected terminal (a no-op when a service/nothing is selected). */
  closeSelected: () => Promise<void>;
  /** Close a SPECIFIC terminal by id (the per-row "×"); re-targets the selection if it was it. */
  closeTerminal: (id: string) => Promise<void>;
  /** Pin/clear a terminal's MANUAL label (the inline rename); `null` clears back to auto-naming. */
  renameTerminal: (id: string, label: string | null) => void;
  /**
   * Persist a drag-reorder within ONE group — a workspace's Terminals (`group` = workspace id) or
   * the loose section (`group` = `null`) — from that group's new id order. Splices it into the
   * GLOBAL terminal sequence (`spliceWorkspaceOrder`, so other groups keep their slots) and calls
   * the sidebar `reorder` (optimistic global re-sort + persisted `order_index` via the `reorder`
   * IPC). The order PERSISTS across a reload; the optimistic global order also drives the tree's
   * live repaint (see `orderedTerminalIds`).
   */
  reorderTerminals: (group: ReorderGroup, ids: string[]) => void;
  /**
   * The OPTIMISTIC global terminal id order (`useTerminals().terminals`, reorder-aware). The
   * runtime tree sorts its snapshot rows by this so a drop repaints in the new order immediately —
   * the pure `reorder` DB command emits no `terminals://changed`, so the snapshot never re-pulls.
   */
  orderedTerminalIds: string[];
}

export function useTerminalActions(
  orderedTerminals: readonly WorkspaceRuntimeTerminal[],
  /**
   * The active project's FALLBACK workspace (its root, else first) for a Ctrl+T that cannot inherit
   * a workspace from the selection. v2.5.6 finding #26 forbids terminals attached to nothing, so
   * Ctrl+T with no bound selection attaches HERE instead of spawning a loose terminal. `null`/absent
   * (e.g. a project with no workspaces) falls back to a loose terminal as a last resort.
   */
  fallbackWorkspace?: { id: string; path: string } | null,
  /**
   * The PANE chords (split / move focus / close pane) of the surface that TILES. They are
   * registered HERE, by the one `useTerminalShortcuts` call this hook owns: a second registration
   * would double-bind Ctrl+T / Ctrl+W and fire them twice. Omitted by any surface without a
   * tiling — the chords then exist but do nothing.
   */
  paneShortcuts?: PaneShortcutHandlers,
): UseTerminalActions {
  const { projects } = useProjects();
  // Borrow the sidebar CRUD surface ONLY — the page's terminal set is driven by the runtime
  // snapshot, so we opt OUT of `useTerminals`' bootstrap "open a default terminal" (visiting the
  // page must never spawn a phantom loose terminal). `reorder` (+ the live `terminals` order) is
  // the SAME optimistic + persisted path the v1 sidebar used for drag-reorder.
  const { terminals, create, attach, close, rename, reorder } = useTerminals({
    autoOpenDefault: false,
  });
  const { selected, select } = useRuntimeSelection();

  const selectedTerminalId = selected?.kind === "terminal" ? selected.id : null;

  // The ordered terminal set the Ctrl+Tab cycle + close re-target step through — the workspace-tree
  // terminals in their live order. Kept in a ref so `closeTerminal` reads the latest list without
  // re-creating its callback each render. (v2.5.6 finding #26 removed the "loose terminals" band;
  // no unattached terminals are surfaced, so this no longer merges a loose set.)
  const orderedAllRef = useRef(orderedTerminals);
  orderedAllRef.current = orderedTerminals;
  const selectedTerminalIdRef = useRef(selectedTerminalId);
  selectedTerminalIdRef.current = selectedTerminalId;

  // Create a terminal at `path`, bind it to `workspaceId` (so the snapshot groups it under the
  // right workspace), and select it. Reuses the sidebar `create` + `attach` — the SAME path the
  // old per-workspace "+" used — so the new record persists and `terminals://changed` re-pulls
  // the page snapshot, surfacing the terminal in the tree and mounting its live pane.
  const createTerminalInWorkspace = useCallback(
    async (workspaceId: string, path: string) => {
      const row = await create(path);
      await attach(row.id, workspaceId, "manual");
      return row.id;
    },
    [create, attach],
  );

  const newTerminalInWorkspace = useCallback(
    async (workspaceId: string, path: string) => {
      const id = await createTerminalInWorkspace(workspaceId, path);
      select({ kind: "terminal", id });
    },
    [createTerminalInWorkspace, select],
  );

  // Open a LOOSE (unattached) terminal — the ABSOLUTE last-resort Ctrl+T fallback when no workspace
  // can be resolved from the selection AND the active project has no fallback workspace (a project
  // with zero workspaces). Reuses the sidebar `create` with no `attach`, so it stays
  // `workspace_id == null`; select it so its pane shows. (v2.5.6 finding #26 removed the visible
  // "loose terminals" band; Ctrl+T now attaches to a workspace whenever one exists — see below.)
  const newLooseTerminal = useCallback(async () => {
    const row = await create();
    select({ kind: "terminal", id: row.id });
  }, [create, select]);

  // Ctrl+T "new terminal": open in the SAME workspace as the SELECTED terminal, resolved by the
  // shared pure `resolveNewTerminalWorkspace`. When that cannot inherit a workspace (no bound
  // selection / stale binding), attach to the active project's FALLBACK workspace instead of a
  // loose terminal (v2.5.6 finding #26: no terminal attached to nothing). Only when there is NO
  // workspace at all (a project with none) does it fall back to a loose terminal.
  const newTerminalSmart = useCallback(async () => {
    const target = resolveNewTerminalWorkspace(selectedTerminalId, terminals, projects);
    if (target) {
      await newTerminalInWorkspace(target.workspaceId, target.path);
      return;
    }
    if (fallbackWorkspace) {
      await newTerminalInWorkspace(fallbackWorkspace.id, fallbackWorkspace.path);
      return;
    }
    await newLooseTerminal();
  }, [
    selectedTerminalId,
    terminals,
    projects,
    fallbackWorkspace,
    newTerminalInWorkspace,
    newLooseTerminal,
  ]);

  // Pin/clear a terminal's MANUAL label (the inline rename). Reuses the sidebar `rename` (optimistic
  // + persisted via the `rename` IPC), the SAME callback the old shell threaded through
  // `use-terminal-rename`. Fire-and-forget: a failure leaves the optimistic label until the next list.
  const renameTerminal = useCallback(
    (id: string, label: string | null) => {
      void rename(id, label);
    },
    [rename],
  );

  // Persist a within-GROUP drag-reorder (a workspace's Terminals, or the loose section). The
  // sortable list hands us that group's new id order; `spliceWorkspaceOrder` splices it into the
  // GLOBAL sequence (every OTHER group keeps its slots) so the backend `reorder(ids)` — which
  // rewrites each terminal's `order_index` by its position in the FULL list — never corrupts
  // cross-group ordering. Reuses the sidebar `reorder` (optimistic global re-sort + persisted),
  // the SAME path the v1 sidebar drag used. Fire-and-forget: a failed persist leaves the optimistic
  // order; the next list reconciles it.
  const reorderTerminals = useCallback(
    (group: ReorderGroup, ids: string[]) => {
      void reorder(spliceWorkspaceOrder(terminals, group, ids));
    },
    [reorder, terminals],
  );

  // The live global terminal order (optimistically reordered by `reorder` above). The tree sorts
  // its snapshot rows by this so a drop repaints at once (the `reorder` IPC pushes no
  // `terminals://changed`, so the workspace snapshot itself does not re-pull).
  const orderedTerminalIds = useMemo(() => terminals.map((t) => t.id), [terminals]);

  // Close a SPECIFIC terminal by id (the per-row "×", v2.5.6, ALSO the Ctrl+W target below).
  // Persists `closed` via the sidebar `close` (drops the record, emits `terminals://changed` → the
  // tree drops the row). When the closed terminal was the SELECTED one, re-target the page
  // selection to a neighbour (nearest by position across the workspace-tree order) so
  // the pane never blanks on the very terminal the user just closed; when nothing remains, leave
  // the selection (the pane shows its empty state). Reads the live list/selection through refs so
  // the callback stays stable.
  const closeTerminal = useCallback(
    async (id: string) => {
      const list = orderedAllRef.current;
      const wasSelected = selectedTerminalIdRef.current === id;
      const idx = list.findIndex((t) => t.id === id);
      await close(id);
      if (wasSelected) {
        const remaining = list.filter((t) => t.id !== id);
        const next = remaining[Math.min(Math.max(idx, 0), remaining.length - 1)] ?? null;
        if (next) select({ kind: "terminal", id: next.id });
      }
    },
    [close, select],
  );

  // Ctrl+W: close the currently-selected terminal (a no-op when a service/nothing is selected).
  // Delegates to `closeTerminal` so it shares the same drop + re-target as the per-row "×".
  const closeSelected = useCallback(async () => {
    if (selectedTerminalId != null) await closeTerminal(selectedTerminalId);
  }, [selectedTerminalId, closeTerminal]);

  // Cycle the SELECTION across the page's terminal list (the workspace tree, snapshot order). Wraps
  // at the ends, mirroring the old shell's `activeNext`/`activePrev`. Steps from the
  // currently-selected terminal; with none selected, jumps to the first/last.
  const cycle = useCallback(
    (delta: number) => {
      const list = orderedAllRef.current;
      if (list.length === 0) return;
      const currentIndex = list.findIndex((t) => t.id === selectedTerminalIdRef.current);
      const from = currentIndex === -1 ? (delta > 0 ? -1 : 0) : currentIndex;
      const nextIndex = (from + delta + list.length) % list.length;
      const next = list[nextIndex];
      if (next) select({ kind: "terminal", id: next.id });
    },
    [select],
  );

  const handlers = useMemo(
    () => ({
      onNew: () => void newTerminalSmart(),
      onClose: () => void closeSelected(),
      onNext: () => cycle(1),
      onPrev: () => cycle(-1),
      ...paneShortcuts,
    }),
    [newTerminalSmart, closeSelected, cycle, paneShortcuts],
  );
  useTerminalShortcuts(handlers);

  return {
    newTerminalInWorkspace,
    createTerminalInWorkspace,
    closeSelected,
    closeTerminal,
    renameTerminal,
    reorderTerminals,
    orderedTerminalIds,
  };
}
