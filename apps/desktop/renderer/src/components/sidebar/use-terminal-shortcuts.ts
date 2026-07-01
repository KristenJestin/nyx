import { useHotkey } from "@tanstack/react-hotkeys";

export interface ShortcutHandlers {
  onNew: () => void;
  onClose: () => void;
  onNext: () => void;
  onPrev: () => void;
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
 *  - new   : Ctrl/Cmd + T
 *  - close : Ctrl/Cmd + W
 *  - next  : Ctrl + Tab         | Ctrl + PageDown
 *  - prev  : Ctrl + Shift + Tab | Ctrl + PageUp
 */
export function isTerminalNavChord(e: KeyboardEvent): boolean {
  const primary = e.ctrlKey || e.metaKey;
  if (!primary) return false;
  const lower = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  if (lower === "t" || lower === "w") return true;
  if (e.ctrlKey && (e.key === "Tab" || e.key === "PageDown" || e.key === "PageUp")) {
    return true;
  }
  return false;
}

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
}
