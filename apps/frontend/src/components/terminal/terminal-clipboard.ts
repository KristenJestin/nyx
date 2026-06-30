/**
 * Terminal copy/paste — the clipboard glue for the xterm surface.
 *
 * xterm itself ships NO copy/paste keybinding (it forwards every keystroke to the
 * PTY), so on Linux/Wayland and Windows the conventional `Ctrl+Shift+C` /
 * `Ctrl+Shift+V` did nothing and selecting text was useless. This module owns the
 * two pieces that wiring needs:
 *
 *   1. {@link isCopyChord} / {@link isPasteChord} — pure predicates that recognise
 *      the copy/paste chords WITHOUT swallowing plain `Ctrl+C` (which must stay
 *      SIGINT). The distinguishing bit is `shiftKey`; we match on `e.code`
 *      (`"KeyC"`/`"KeyV"`) so the chord is layout-stable (a key event with Ctrl
 *      held can report an odd `key`, but `code` is the physical key).
 *
 *   2. {@link copySelection} / {@link pasteFromClipboard} — the async clipboard
 *      actions, written against the `navigator.clipboard` API.
 *
 * CLIPBOARD APPROACH — `navigator.clipboard`, NOT a new IPC channel:
 * the renderer is hardened (`sandbox: true`, `contextIsolation: true`,
 * `nodeIntegration: false`) and has no Node, but `navigator.clipboard` is a
 * standard web API that Electron grants for a same-origin, user-gesture-driven
 * call (the chord IS the user gesture). Going through the existing preload bridge
 * would mean a new allowlisted channel + main handler + bridge method — more
 * attack surface and a merge-conflict magnet against the core/preload work in
 * flight. Keeping copy/paste a pure renderer concern is both smaller and safer.
 * `copySelection` falls back to the legacy `document.execCommand("copy")` path
 * when the async API is unavailable (older/headless engines), so a copy never
 * silently no-ops.
 *
 * SECURITY: the clipboard payload is NEVER logged (no `console.*` of the text);
 * a failed clipboard op is swallowed so it can't leak via an unhandled rejection.
 */

/** The minimal xterm surface this module needs — keeps it trivially mockable. */
export interface ClipboardTerminal {
  hasSelection(): boolean;
  getSelection(): string;
  /** Inserts text the way a paste would (bracketed-paste aware). */
  paste(data: string): void;
}

/**
 * Whether `e` is the COPY chord (`Ctrl+Shift+C`). Plain `Ctrl+C` is deliberately
 * NOT matched — without Shift it must reach the PTY as SIGINT. We key off
 * `e.code === "KeyC"` (the physical key) rather than `e.key`, which can be
 * unreliable for letters while Ctrl is held. `metaKey`/`altKey` must be absent so
 * we don't hijack OS-level chords (macOS is a non-goal, but this stays correct).
 */
export function isCopyChord(e: KeyboardEvent): boolean {
  return e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey && e.code === "KeyC";
}

/**
 * Whether `e` is the PASTE chord (`Ctrl+Shift+V`). Symmetric to {@link isCopyChord}.
 */
export function isPasteChord(e: KeyboardEvent): boolean {
  return e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey && e.code === "KeyV";
}

/**
 * Copy the terminal's current selection to the system clipboard. No-op (and no
 * clipboard write) when there is no selection, so `Ctrl+Shift+C` with nothing
 * selected stays inert instead of clobbering the clipboard with an empty string.
 *
 * Returns `true` if a copy was performed (or attempted) — i.e. there was a
 * selection — so the caller can decide whether the chord was "handled". The
 * actual clipboard write is best-effort and async; we await `writeText` but never
 * surface its text on failure.
 *
 * @returns whether there was a selection to copy.
 */
export async function copySelection(term: ClipboardTerminal): Promise<boolean> {
  if (!term.hasSelection()) return false;
  const text = term.getSelection();
  if (!text) return false;

  // Preferred path: the async Clipboard API, granted on a user gesture.
  const nav = typeof navigator !== "undefined" ? navigator : undefined;
  if (nav?.clipboard?.writeText) {
    try {
      await nav.clipboard.writeText(text);
      return true;
    } catch {
      // Fall through to execCommand — never log the (potentially sensitive) text.
    }
  }

  // Legacy fallback for engines without async clipboard (or where it was denied):
  // write to a transient textarea, select it, and `execCommand("copy")`.
  copyViaExecCommand(text);
  return true;
}

/**
 * Read the system clipboard and paste it INTO the terminal via `term.paste`,
 * which routes through xterm's bracketed-paste handling (so the shell sees a
 * paste, not raw keystrokes). No-op on empty/unavailable clipboard.
 *
 * @returns whether non-empty text was pasted.
 */
export async function pasteFromClipboard(term: ClipboardTerminal): Promise<boolean> {
  const nav = typeof navigator !== "undefined" ? navigator : undefined;
  if (!nav?.clipboard?.readText) return false;
  let text: string;
  try {
    text = await nav.clipboard.readText();
  } catch {
    // Read denied/unavailable — never log; just don't paste.
    return false;
  }
  if (!text) return false;
  term.paste(text);
  return true;
}

/**
 * Synchronous clipboard write via the legacy `execCommand` path, for engines
 * where `navigator.clipboard.writeText` is missing or rejected. Best-effort: any
 * DOM failure is swallowed (the text is never logged).
 */
function copyViaExecCommand(text: string): void {
  if (typeof document === "undefined") return;
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    // Keep it out of the layout/flow and unfocusable to assistive tech.
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.top = "-9999px";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    document.body.removeChild(ta);
  } catch {
    // best-effort only
  }
}
