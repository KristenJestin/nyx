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
 *
 * Beside those flat id hints, this module owns the ONE way to keep a STRUCTURED UI preference:
 * {@link saveVersionedJson} / {@link loadVersionedJson}. Same store, same guards, plus the two
 * things a shape needs that a bare string does not — a SCHEMA VERSION and a shape CHECK at read
 * time — so a stored value written by another app version, hand-edited, or truncated can never
 * reach a caller as if it were valid. The pane tiling (`pane-layout-persistence.ts`) is its first
 * user; the flat keys above are deliberately NOT migrated onto it (they are strings and mean the
 * same thing they always did).
 */

/** The last ENTERED project id. */
const PROJECT_KEY = "nyx.ui.lastProjectId";
/** Prefix for the per-project last SELECTED terminal id (`<prefix><projectId>`). */
const TERMINAL_KEY_PREFIX = "nyx.ui.lastTerminalId:";
/** Prefix for a GROUP BLOCK's folded/unfolded state (`<prefix><groupId>`). */
const GROUP_OPEN_KEY_PREFIX = "nyx.ui.groupOpen:";
/** Schema version of a group's fold state — a bare boolean. */
const GROUP_OPEN_VERSION = 1;

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

/**
 * Whether a GROUP BLOCK is unfolded — `null` when this group has never been folded or unfolded, in
 * which case its family's default applies (open for terminals, closed for commands: a terminal
 * group is a live workspace you look at, a command group is a launcher you press).
 *
 * It rides the SAME registry as everything else on this page rather than a store of its own (human
 * review 01M12MW1DV1V455EWBMV048TE6): pure per-machine UI state, no cross-device meaning, and the
 * versioned envelope below is exactly what keeps a hand-edited or downgraded entry from reaching a
 * caller as a boolean it is not. One key per group, so dissolving a group leaves one dead entry
 * behind and never a growing blob to migrate.
 */
export function loadGroupOpen(groupId: string): boolean | null {
  return loadVersionedJson(
    `${GROUP_OPEN_KEY_PREFIX}${groupId}`,
    GROUP_OPEN_VERSION,
    (value): value is boolean => typeof value === "boolean",
  );
}

/** Remember a group block's folded/unfolded state (or, for `null`, forget it). */
export function saveGroupOpen(groupId: string, open: boolean | null): void {
  saveVersionedJson(`${GROUP_OPEN_KEY_PREFIX}${groupId}`, GROUP_OPEN_VERSION, open);
}

/**
 * The envelope every versioned entry is written in: the SCHEMA VERSION next to the payload, in the
 * same JSON document. Keeping the version INSIDE the value (rather than in the key) is what lets a
 * reader recognise an entry it must not interpret — a key-per-version scheme would silently leave
 * the old entry behind and read nothing at all, which reads the same as "no preference".
 */
interface VersionEnvelope<T> {
  readonly v: number;
  readonly data: T;
}

/** Whether a parsed value is an envelope AT ALL (a version number and a `data` slot). */
function isEnvelope(value: unknown): value is VersionEnvelope<unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { v?: unknown }).v === "number" &&
    "data" in (value as object)
  );
}

/**
 * Read `key` as a VERSIONED JSON preference, or `null` when there is nothing usable there.
 *
 * `null` is returned — and the stored entry is LEFT ALONE — for every failure mode, because they
 * are indistinguishable from the caller's point of view ("no preference") and because destroying
 * an entry we merely failed to understand is not ours to do: a DOWNGRADE (an older build reading a
 * newer entry) must not cost the user their layout when they next launch the current one. So:
 *  - absent key, unparseable JSON, or a value that is not an envelope ⇒ `null`;
 *  - `v` different from `version` (older OR newer — a migration is the CALLER's job, by re-reading
 *    at the older version, as `loadPaneCollection` does) ⇒ `null`;
 *  - a payload the caller's own `isValid` guard rejects ⇒ `null`. The guard is mandatory: JSON
 *    round-trips anything, so the version alone proves nothing about the shape.
 *
 * A throwing `localStorage` (private mode, SecurityError) degrades to `null` like everything else.
 */
export function loadVersionedJson<T>(
  key: string,
  version: number,
  isValid: (value: unknown) => value is T,
): T | null {
  const raw = read(key);
  if (raw === null) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!isEnvelope(parsed) || parsed.v !== version) return null;
  return isValid(parsed.data) ? parsed.data : null;
}

/**
 * Write `value` at `key` under `version`, or FORGET the key when `value` is `null` (the same
 * "remember / forget" contract as the id hints above). Serialisation failures (a cyclic value) and
 * quota/private-mode rejections are swallowed: persistence is best-effort and a preference that
 * could not be written simply is not remembered.
 */
export function saveVersionedJson<T>(key: string, version: number, value: T | null): void {
  if (value === null) {
    write(key, null);
    return;
  }
  let serialized: string;
  try {
    serialized = JSON.stringify({ v: version, data: value } satisfies VersionEnvelope<T>);
  } catch {
    return;
  }
  write(key, serialized);
}
