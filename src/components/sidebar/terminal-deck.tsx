import { useEffect, useRef } from "react";
import type { Terminal as XTerm } from "@xterm/xterm";

import { Terminal } from "@/components/terminal/terminal";
import type { TerminalRecord } from "./use-terminals";

export interface TerminalDeckProps {
  /** Records to mount, in order. Each gets exactly one mounted `<Terminal>`. */
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
 * Read the whole xterm buffer (screen + scrollback) of an instance as a string.
 * The per-terminal test seam (`window.__nyxDeck[id]`) returns this so the unit
 * suite can prove an INACTIVE terminal still received its PTY output.
 */
function readBuffer(term: XTerm | null): string {
  if (!term) return "";
  const buf = term.buffer.active;
  let out = "";
  for (let i = 0; i < buf.length; i++) {
    const line = buf.getLine(i);
    if (line) out += line.translateToString(true) + "\n";
  }
  return out;
}

/**
 * `<TerminalDeck>` — mounts ONE `<Terminal>` per record and shows only the
 * active one. The crucial property: inactive terminals stay MOUNTED (their
 * xterm instance + PTY listeners + buffer are all alive) — they are merely
 * hidden with `display:none`. So a background terminal keeps receiving its
 * `pty://output` (routed by its own PTY id inside `usePty`) and its scrollback
 * is intact when the user switches back. We do NOT unmount/remount on switch
 * (that would kill the buffer and respawn the shell).
 *
 * Each pane carries `data-terminal-id` (the record id) and `data-active` so the
 * layer above (and the tests) can reason about visibility without inspecting
 * styles. The active pane is the only one without `display:none`.
 */
export function TerminalDeck({ terminals, activeId, onPtyId }: TerminalDeckProps) {
  // Per-record xterm instances, for the read seam. Lazy-init the Map once so we
  // don't allocate (and throw away) a fresh Map on every render.
  const instancesRef = useRef<Map<string, XTerm | null> | null>(null);
  instancesRef.current ??= new Map();
  const instances = instancesRef.current;

  // Publish per-terminal test seams on window so tests (and the e2e suite) can
  // act on any terminal by record id — including hidden ones. Both are INERT in
  // production (nothing reads them):
  //   - `__nyxDeck[id]()`       → read that terminal's xterm buffer (string);
  //   - `__nyxDeckInput[id](s)` → type `s` into that terminal as keystrokes
  //     (via xterm `input`, so the PTY echoes + runs it). The e2e suite needs
  //     this because xterm paints to a WebGL canvas — there is no DOM text to
  //     type into, so a WebDriver cannot enter input directly.
  useEffect(() => {
    const win = window as unknown as {
      __nyxDeck?: Record<string, () => string>;
      __nyxDeckInput?: Record<string, (data: string) => void>;
    };
    win.__nyxDeck = win.__nyxDeck ?? {};
    win.__nyxDeckInput = win.__nyxDeckInput ?? {};
    return () => {
      delete win.__nyxDeck;
      delete win.__nyxDeckInput;
    };
  }, []);

  // Per-record callbacks are CACHED (keyed by record id) so each <Terminal> sees
  // a STABLE onInstance/onPtyId identity across deck re-renders. Building a fresh
  // closure each render (the prior `makeOnInstance(id)` call-in-render) re-ran
  // <Terminal>'s onInstance effect on every switch/poll — pure churn — and
  // defeated the documented "stable callback" intent. The closures capture only
  // the id and read `window`/`onPtyIdRef.current` at call time, so no entry ever
  // goes stale; entries for closed terminals are harmless (the seam is inert).
  const onInstanceCbs = useRef(new Map<string, (term: XTerm | null) => void>());
  const getOnInstance = (id: string) => {
    let cb = onInstanceCbs.current.get(id);
    if (!cb) {
      cb = (term: XTerm | null) => {
        instances.set(id, term);
        const win = window as unknown as {
          __nyxDeck?: Record<string, () => string>;
          __nyxDeckInput?: Record<string, (data: string) => void>;
        };
        if (win.__nyxDeck) win.__nyxDeck[id] = () => readBuffer(term);
        if (win.__nyxDeckInput) {
          win.__nyxDeckInput[id] = (data: string) => term?.input(data, true);
        }
      };
      onInstanceCbs.current.set(id, cb);
    }
    return cb;
  };

  // Read onPtyId off a ref so the per-record callbacks stay stable (no <Terminal>
  // remount) even as the parent passes a fresh closure each render.
  const onPtyIdRef = useRef(onPtyId);
  onPtyIdRef.current = onPtyId;
  const onPtyIdCbs = useRef(new Map<string, (ptyId: number | null) => void>());
  const getOnPtyId = (recordId: string) => {
    let cb = onPtyIdCbs.current.get(recordId);
    if (!cb) {
      cb = (ptyId: number | null) => onPtyIdRef.current?.(recordId, ptyId);
      onPtyIdCbs.current.set(recordId, cb);
    }
    return cb;
  };

  // Render the panes in a STABLE order (by record id), INDEPENDENT of the sidebar
  // order. The deck only ever SHOWS the active pane (the others are
  // absolutely-positioned + `display:none`), so their DOM order is visually
  // irrelevant — but it matters MECHANICALLY: if a sidebar drag-reorder changed
  // this order, React would MOVE the active pane's DOM node among its siblings,
  // detaching + reattaching its live xterm canvas and forcing a visible "refresh".
  // The record id never changes on reorder, so sorting by it pins each pane's DOM
  // slot; reordering the sidebar no longer disturbs the running terminal. Add/close
  // still mount/unmount as expected.
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
            // Hidden panes stay mounted (buffer + PTY alive) but take no space.
            // `display:none` is what keeps the inactive terminal's xterm alive
            // while invisible — unmounting would destroy it.
            style={active ? undefined : { display: "none" }}
            className="absolute inset-0 h-full w-full"
          >
            <Terminal
              cwd={t.cwd}
              active={active}
              recordId={t.id}
              deadHistory={t.scrollback}
              onInstance={getOnInstance(t.id)}
              onPtyId={getOnPtyId(t.id)}
            />
          </div>
        );
      })}
    </div>
  );
}

export default TerminalDeck;
