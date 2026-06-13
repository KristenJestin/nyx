import { useCallback, useEffect, useState } from "react";

import { ChromeBar } from "@/components/chrome/chrome-bar";
import { resolveDisplayName } from "./auto-label";
import { Sidebar } from "./sidebar";
import { TerminalDeck } from "./terminal-deck";
import { useTerminals, type TerminalRecord } from "./use-terminals";
import { useTerminalShortcuts } from "./use-terminal-shortcuts";

/**
 * The inert end-to-end control seam published on `window.__nyx`. It exposes the
 * multi-terminal control surface the e2e suite (tauri-driver) needs but cannot
 * reach otherwise: xterm paints to a WebGL canvas (no DOM text to read/type),
 * and the sidebar's drag/keyboard intents are awkward to drive over WebDriver.
 *
 * It is INERT in production — nothing in the app reads it; it only mirrors the
 * actions a user performs (create at a cwd, type into a terminal, read its
 * buffer, reorder, close) so the e2e can script the restore scenario and read
 * back state. The buffer read/type delegate to the per-terminal deck seams
 * (`__nyxDeck` / `__nyxDeckInput`), keyed by record id, so they work for hidden
 * panes too.
 */
interface NyxE2eSeam {
  /** Snapshot of the current records (id, cwd, label, status, order). */
  list: () => TerminalRecord[];
  /** Record id of the active (visible) terminal, or null. */
  activeId: () => string | null;
  /** Create a terminal at `cwd` (distinct dirs for the 3-terminal scenario). */
  create: (cwd: string) => Promise<void>;
  /** Make `id` the active terminal. */
  setActive: (id: string) => void;
  /** Close a terminal (marks the record closed → not re-spawned). */
  close: (id: string) => Promise<void>;
  /** Persist a new sidebar order. */
  reorder: (ids: string[]) => Promise<void>;
  /** Read a terminal's xterm buffer by record id (works for hidden panes). */
  readBuffer: (id: string) => string;
  /** Type `data` into a terminal by record id (as keystrokes → PTY runs it). */
  typeInto: (id: string, data: string) => void;
}

/**
 * `<TerminalManager>` — the top-level multi-terminal shell: the thin chrome bar,
 * the left sidebar (navigation), and the terminal deck (N mounted terminals,
 * only the active visible). It owns no state itself — `useTerminals` is the
 * single source of truth — it just wires the sidebar intents, the keyboard
 * shortcuts, and the active-item title into one layout.
 */
export function TerminalManager() {
  const {
    terminals,
    activeId,
    create,
    close,
    setActive,
    activeNext,
    activePrev,
    reorder,
    rename,
  } = useTerminals();

  // Global new/close/next/prev shortcuts. `close` targets the active terminal.
  const closeActive = useCallback(() => {
    if (activeId !== null) void close(activeId);
  }, [activeId, close]);

  useTerminalShortcuts({
    onNew: () => void create(),
    onClose: closeActive,
    onNext: activeNext,
    onPrev: activePrev,
  });

  // Live record→PTY id map, populated by the deck as each shell spawns/exits.
  // The sidebar reads `terminal_info(ptyId)` per item for the auto label.
  const [ptyIds, setPtyIds] = useState<Map<string, number | null>>(
    () => new Map(),
  );
  const handlePtyId = useCallback((recordId: string, ptyId: number | null) => {
    setPtyIds((prev) => {
      if (prev.get(recordId) === ptyId) return prev; // no-op churn guard
      const next = new Map(prev);
      next.set(recordId, ptyId);
      return next;
    });
  }, []);

  // Publish the inert e2e control seam on window. Refreshed whenever the records
  // or active id change so `list()`/`activeId()` always read the latest. Inert
  // in production; only the e2e (tauri-driver) reads it.
  useEffect(() => {
    const win = window as unknown as { __nyx?: NyxE2eSeam };
    win.__nyx = {
      list: () => terminals,
      activeId: () => activeId,
      create: (cwd: string) => create(cwd),
      setActive,
      close,
      reorder,
      readBuffer: (id: string) => {
        const seam = (window as unknown as { __nyxDeck?: Record<string, () => string> })
          .__nyxDeck;
        return seam?.[id]?.() ?? "";
      },
      typeInto: (id: string, data: string) => {
        const seam = (
          window as unknown as {
            __nyxDeckInput?: Record<string, (data: string) => void>;
          }
        ).__nyxDeckInput;
        seam?.[id]?.(data);
      },
    };
    return () => {
      delete (window as unknown as { __nyx?: NyxE2eSeam }).__nyx;
    };
  }, [terminals, activeId, create, setActive, close, reorder]);

  // Discreet active-item title for the chrome bar (manual label wins; auto/cwd
  // fall back). The chrome title uses the record-only resolution — the live auto
  // label is rendered per-item in the sidebar.
  const activeIndex = terminals.findIndex((t) => t.id === activeId);
  const title =
    activeIndex === -1
      ? undefined
      : resolveDisplayName(terminals[activeIndex], activeIndex, null);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-background">
      <ChromeBar title={title} />
      <div className="flex min-h-0 flex-1">
        <Sidebar
          terminals={terminals}
          activeId={activeId}
          ptyIds={ptyIds}
          onSelect={setActive}
          onClose={(id) => void close(id)}
          onCreate={() => void create()}
          onReorder={(ids) => void reorder(ids)}
          onRename={(id, label) => void rename(id, label)}
        />
        <div className="min-w-0 flex-1">
          <TerminalDeck
            terminals={terminals}
            activeId={activeId}
            onPtyId={handlePtyId}
          />
        </div>
      </div>
    </div>
  );
}

export default TerminalManager;
