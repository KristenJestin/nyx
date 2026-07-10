/**
 * UI-STATE PERSISTENCE for the Terminals shell — the small, renderer-local memory of "where the
 * user last was" so a relaunch (or a return to the Terminals page from Work/Knowledge) restores
 * the LAST active project and, per project, its LAST active terminal instead of snapping back to
 * the first one (human review 01KX60545DCSASTKSCH7V3FWTW).
 *
 * Kept in the renderer's `localStorage` (not the DB): it is pure per-machine UI state, has no
 * cross-device meaning, and there is no sidecar `ui_state`/`set_setting` seam to piggyback on.
 * Every access is GUARDED (no `window`/`localStorage`, or a throwing private-mode/quota) so a
 * missing or hostile store degrades to "no memory" rather than crashing the shell — the restore
 * simply falls back to the first project/terminal.
 *
 * The stored ids are treated as HINTS only: the callers verify a restored id still exists in the
 * live project/runtime tree before honouring it (a project or terminal may have been deleted
 * while the app was closed), so a stale value can never pin the UI to something that is gone.
 */

/** The last ENTERED project id. */
const PROJECT_KEY = "nyx.ui.lastProjectId";
/** Prefix for the per-project last SELECTED terminal id (`<prefix><projectId>`). */
const TERMINAL_KEY_PREFIX = "nyx.ui.lastTerminalId:";

/** The `localStorage` handle, or `null` when it is absent / access throws (private mode, SSR). */
function store(): Storage | null {
  try {
    if (typeof window === "undefined" || !window.localStorage) return null;
    return window.localStorage;
  } catch {
    return null;
  }
}

/** Read a key, swallowing any access error (returns `null`). */
function read(key: string): string | null {
  try {
    return store()?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

/** Write (or, for a `null` value, remove) a key, swallowing any access/quota error. */
function write(key: string, value: string | null): void {
  const s = store();
  if (!s) return;
  try {
    if (value === null) s.removeItem(key);
    else s.setItem(key, value);
  } catch {
    // Private mode / quota — persistence is best-effort; ignore.
  }
}

/** The last entered project id, or `null` when none is remembered. */
export function loadLastProjectId(): string | null {
  return read(PROJECT_KEY);
}

/** Remember (or, for `null`, forget) the last entered project id. */
export function saveLastProjectId(projectId: string | null): void {
  write(PROJECT_KEY, projectId);
}

/** The last selected terminal id for `projectId`, or `null` when none is remembered. */
export function loadLastTerminalId(projectId: string): string | null {
  return read(`${TERMINAL_KEY_PREFIX}${projectId}`);
}

/** Remember (or, for `null`, forget) the last selected terminal id for `projectId`. */
export function saveLastTerminalId(projectId: string, terminalId: string | null): void {
  write(`${TERMINAL_KEY_PREFIX}${projectId}`, terminalId);
}
