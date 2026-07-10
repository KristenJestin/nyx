import { useEffect } from "react";

import { Terminal } from "@/components/terminal/terminal";
import { getTerminal, reconcileTerminals } from "@/components/terminal/terminal-store";
import type { TerminalRecord } from "./use-terminals";

export interface TerminalDeckProps {
  /** Records to mount, in order. Each gets exactly one `<Terminal>` pane. */
  terminals: TerminalRecord[];
  /** Record id of the terminal to show; all others are mounted-but-hidden. */
  activeId: string | null;
  /**
   * Notify the parent of the live PTY id for a record (and `null` on exit), so
   * the sidebar's auto-naming can read `terminal_info` for that terminal.
   */
  onPtyId?: (recordId: string, ptyId: number | null) => void;
}

/**
 * `<TerminalDeck>` — mounts ONE `<Terminal>` pane per record and shows only the
 * active one. The live terminals live in the module-level
 * {@link getTerminal store}, NOT in these components: each pane merely attaches
 * its record's persistent DOM node into the visible container and detaches on
 * unmount. So a terminal survives BOTH a selection switch (the pane stays mounted,
 * hidden with `display:none`) AND a full navigation (the whole deck unmounts, each
 * pane detaches — never disposes — and the instance stays in the store, reattached
 * losslessly on return with its buffer intact).
 *
 * The deck also RECONCILES the store against its live record set (see the effect):
 * a record no longer present (closed by the user or by an agent over MCP) has its
 * store instance disposed, so a churn of create/close never leaks a PTY.
 *
 * Each pane carries `data-terminal-id` (the record id) and `data-active` so the
 * layer above (and the tests) can reason about visibility without inspecting
 * styles. The active pane is the only one without `display:none`.
 */
export function TerminalDeck({ terminals, activeId, onPtyId }: TerminalDeckProps) {
  // Publish per-terminal test/e2e seams on window, backed by the STORE (so they
  // address any terminal by record id, including hidden or navigated-away ones):
  //   - `__nyxDeck[id]()`       → that terminal's buffer text (screen + scrollback);
  //   - `__nyxDeckInput[id](s)` → feed `s` as input (routes through the PTY).
  // Both are inert in production (nothing reads them).
  useEffect(() => {
    const win = window as unknown as {
      __nyxDeck?: Record<string, () => string>;
      __nyxDeckInput?: Record<string, (data: string) => void>;
    };
    win.__nyxDeck = new Proxy(
      {},
      { get: (_t, id: string) => () => getTerminal(id)?.readScreenText() ?? "" },
    ) as Record<string, () => string>;
    win.__nyxDeckInput = new Proxy(
      {},
      { get: (_t, id: string) => (data: string) => getTerminal(id)?.sendInput(data) },
    ) as Record<string, (data: string) => void>;
    return () => {
      delete win.__nyxDeck;
      delete win.__nyxDeckInput;
    };
  }, []);

  // RECONCILE the store against the live record set: dispose (real close) any store
  // instance whose record is no longer present. Runs after render so a
  // freshly-added record's pane has mounted (created its instance) first. Keyed on
  // the id SET (not the array identity).
  const liveIds = terminals.map((t) => t.id).join(",");
  useEffect(() => {
    reconcileTerminals(terminals.map((t) => t.id));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [liveIds]);

  // Render the panes in a STABLE order (by record id), independent of the sidebar
  // order: the deck only ever SHOWS the active pane (others are `display:none`), so
  // their DOM order is visually irrelevant, but sorting by id pins each pane's slot
  // so a sidebar reorder never moves the active pane's DOM node.
  const ordered = [...terminals].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));

  return (
    <div className="relative h-full w-full">
      {ordered.map((t) => {
        const active = t.id === activeId;
        return (
          <div
            key={t.id}
            data-terminal-id={t.id}
            data-active={active ? "true" : "false"}
            // Hidden panes stay mounted; the store instance stays alive regardless.
            // `display:none` gives the pane a 0×0 box — the instance's app-driven
            // resize observer skips it (0×0 guard), so it never resizes to 1×1.
            style={active ? undefined : { display: "none" }}
            className="absolute inset-0 h-full w-full"
          >
            <Terminal
              cwd={t.cwd}
              active={active}
              recordId={t.id}
              deadHistory={t.scrollback}
              onPtyId={(ptyId) => onPtyId?.(t.id, ptyId)}
            />
          </div>
        );
      })}
    </div>
  );
}

export default TerminalDeck;
