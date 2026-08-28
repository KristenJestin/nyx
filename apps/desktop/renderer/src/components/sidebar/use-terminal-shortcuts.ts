import { useHotkey, type Hotkey } from "@tanstack/react-hotkeys";

/**
 * SPLIT the focused pane to the RIGHT (a `row` split) — the tiling's creation gesture, and the
 * chord `<PaneTreeHost>` prints in its own tooltips (`formatForDisplay`), so the binding and the
 * label are the SAME constant and cannot drift.
 *
 * `Mod+Shift+←/→/↑/↓` is free in a terminal (xterm sends nothing for it) and `Mod` keeps the
 * platform habit: Ctrl on Windows/Linux, Cmd on macOS.
 */
export const SPLIT_RIGHT_CHORD: Hotkey = "Mod+Shift+ArrowRight";
/** SPLIT the focused pane DOWNWARDS (a `column` split). Twin of {@link SPLIT_RIGHT_CHORD}. */
export const SPLIT_DOWN_CHORD: Hotkey = "Mod+Shift+ArrowDown";
/**
 * CLOSE the focused PANE — a layout edit (the terminal keeps running), deliberately distinct from
 * `Mod+W`, which closes the TERMINAL. Same "Mod+Shift" family as the splits: the pane-level
 * commands are the Shift'ed ones.
 */
export const CLOSE_PANE_CHORD: Hotkey = "Mod+Shift+W";
/** Move the focus to the geometric neighbour — one chord per direction, no wrap. */
export const FOCUS_LEFT_CHORD: Hotkey = "Alt+ArrowLeft";
export const FOCUS_RIGHT_CHORD: Hotkey = "Alt+ArrowRight";
export const FOCUS_UP_CHORD: Hotkey = "Alt+ArrowUp";
export const FOCUS_DOWN_CHORD: Hotkey = "Alt+ArrowDown";

export interface ShortcutHandlers {
  onNew: () => void;
  onClose: () => void;
  onNext: () => void;
  onPrev: () => void;
  /** Split the focused pane right / down — OMITTED ⇒ the chord is not registered at all. */
  onSplitRight?: () => void;
  onSplitDown?: () => void;
  /** Move the focus to the geometric neighbour in that direction. */
  onFocusLeft?: () => void;
  onFocusRight?: () => void;
  onFocusUp?: () => void;
  onFocusDown?: () => void;
  /** Close the focused PANE (not the terminal). */
  onClosePane?: () => void;
}

/**
 * Whether a keyboard event is one of our app-level terminal-navigation chords.
 *
 * This is the bridge to xterm. `useTerminalShortcuts` registers the chords with
 * TanStack Hotkeys, whose manager listens on `document` in the BUBBLE phase — but
 * xterm handles keys on its own textarea and STOPS PROPAGATION for the ones it
 * consumes, so a chord pressed while a terminal is focused would never bubble up
 * to the hotkey listener. `<Terminal>` feeds this predicate to
 * `attachCustomKeyEventHandler` so xterm YIELDS these chords (lets them through
 * untouched, no PTY byte, no stopPropagation) and they reach TanStack.
 *
 * KEEP THIS IN SYNC with the `useHotkey` bindings below:
 *  - new        : Ctrl/Cmd + T
 *  - close      : Ctrl/Cmd + W
 *  - next       : Ctrl + Tab         | Ctrl + PageDown
 *  - prev       : Ctrl + Shift + Tab | Ctrl + PageUp
 *  - split      : Ctrl/Cmd + Shift + → / ↓
 *  - close pane : Ctrl/Cmd + Shift + W
 *  - move focus : Alt + ← → ↑ ↓
 *
 * The PANE chords matter here even more than the others: a tiling is driven while a terminal has
 * the keyboard BY DEFINITION (the focused pane's xterm owns the textarea), so a pane chord xterm
 * did not yield would never reach the manager and the tiling would only be drivable by mouse.
 */
export function isTerminalNavChord(e: KeyboardEvent): boolean {
  // The Alt+arrow focus moves carry NO Ctrl/Meta, so they are tested before the primary guard.
  if (e.altKey && !e.ctrlKey && !e.metaKey && ARROW_KEYS.has(e.key)) return true;
  const primary = e.ctrlKey || e.metaKey;
  if (!primary) return false;
  const lower = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  // Mod+Shift+W (close pane) shares its letter with Mod+W (close terminal) — both must be yielded.
  if (lower === "t" || lower === "w") return true;
  if (e.shiftKey && (e.key === "ArrowRight" || e.key === "ArrowDown")) return true;
  if (e.ctrlKey && (e.key === "Tab" || e.key === "PageDown" || e.key === "PageUp")) {
    return true;
  }
  return false;
}

/** The four arrow keys, for the Alt+arrow focus moves. */
const ARROW_KEYS = new Set(["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"]);

/**
 * Register the global terminal-navigation shortcuts via TanStack Hotkeys
 * (new / close / next / prev). `Mod` resolves to Ctrl on Windows/Linux and Cmd
 * on macOS; the cycling chords use Ctrl specifically (Ctrl+Tab is the
 * conventional "next tab"; Cmd+Tab is the OS app switcher).
 *
 * TanStack's defaults already `preventDefault` + `stopPropagation`, and for
 * Ctrl/Meta chords `ignoreInputs` defaults to false — so these fire whether the
 * sidebar OR a terminal/input is focused (the terminal-focus case relies on
 * `isTerminalNavChord` making xterm yield; see above). Callbacks are synced every
 * render by `useHotkey`, so they always see the latest terminal list.
 */
export function useTerminalShortcuts(handlers: ShortcutHandlers): void {
  useHotkey("Mod+T", handlers.onNew);
  useHotkey("Mod+W", handlers.onClose);
  useHotkey("Control+Tab", handlers.onNext);
  useHotkey("Control+Shift+Tab", handlers.onPrev);
  useHotkey("Control+PageDown", handlers.onNext);
  useHotkey("Control+PageUp", handlers.onPrev);
  // The PANE chords. `enabled` follows the handler: a surface that does not tile (the sidebar
  // shell) hands none, and the chord then exists in the registry but fires nothing — which is what
  // keeps the registration list stable across surfaces instead of churning it per mount.
  useHotkey(SPLIT_RIGHT_CHORD, () => handlers.onSplitRight?.(), {
    enabled: handlers.onSplitRight != null,
  });
  useHotkey(SPLIT_DOWN_CHORD, () => handlers.onSplitDown?.(), {
    enabled: handlers.onSplitDown != null,
  });
  useHotkey(CLOSE_PANE_CHORD, () => handlers.onClosePane?.(), {
    enabled: handlers.onClosePane != null,
  });
  // `ignoreInputs: false` is REQUIRED for the Alt chords: TanStack skips a Shift/Alt combo by
  // default when the event comes from an input-like element, and the focused pane's xterm keeps a
  // textarea focused at ALL times — so the default would make the focus moves dead exactly where
  // they are meant to be used. The Mod chords above already default to `false` (Ctrl/Meta family).
  useHotkey(FOCUS_LEFT_CHORD, () => handlers.onFocusLeft?.(), {
    enabled: handlers.onFocusLeft != null,
    ignoreInputs: false,
  });
  useHotkey(FOCUS_RIGHT_CHORD, () => handlers.onFocusRight?.(), {
    enabled: handlers.onFocusRight != null,
    ignoreInputs: false,
  });
  useHotkey(FOCUS_UP_CHORD, () => handlers.onFocusUp?.(), {
    enabled: handlers.onFocusUp != null,
    ignoreInputs: false,
  });
  useHotkey(FOCUS_DOWN_CHORD, () => handlers.onFocusDown?.(), {
    enabled: handlers.onFocusDown != null,
    ignoreInputs: false,
  });
}
