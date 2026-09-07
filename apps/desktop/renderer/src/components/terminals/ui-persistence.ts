// The persisted scope vocabulary is the LIVE one, imported rather than re-spelled: a fourth scope
// would otherwise be storable here without the app knowing about it. Type-only, so the module
// graph stays acyclic at runtime (`active-project.tsx` imports the readers below).
import type { ScopeKind } from "./active-project";

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
/** Which SCOPE the app was left in (`<ActiveProjectProvider>`'s `scope.kind`). */
const SCOPE_KIND_KEY = "nyx.ui.scopeKind";
/** Schema version of the scope kind — a bare string of {@link ScopeKind}'s three values. */
const SCOPE_KIND_VERSION = 1;
/**
 * The LEGACY two-state key this one replaces — a bare boolean, `true` meaning "all projects".
 * Still READ (never written) so an install that predates the Système scope reopens where it was
 * left instead of falling back to the project scope. Its own value is left in place: it costs
 * nothing, and a rollback would find it.
 */
const LEGACY_GLOBAL_SCOPE_KEY = "nyx.ui.globalScope";
/** Schema version of the legacy global-scope flag. */
const LEGACY_GLOBAL_SCOPE_VERSION = 1;
/** Prefix for the per-project last SELECTED terminal id (`<prefix><projectId>`). */
const TERMINAL_KEY_PREFIX = "nyx.ui.lastTerminalId:";
/** Prefix for a GROUP BLOCK's folded/unfolded state (`<prefix><groupId>`). */
const GROUP_OPEN_KEY_PREFIX = "nyx.ui.groupOpen:";
/** Schema version of a group's fold state — a bare boolean. */
const GROUP_OPEN_VERSION = 1;
/**
 * Prefix for a RUNTIME BAND's folded/unfolded state (`<prefix><workspaceId>:<band>`).
 *
 * PER WORKSPACE and PER BAND, both halves deliberate. Per workspace, because a fold is a statement
 * about ONE workspace's runtime ("I am not looking at this one's terminals right now"), and a
 * global "Terminals are folded" would collapse the band on the workspace the user is working in.
 * Per band, because the two answer different questions and a user routinely wants one shut and the
 * other open.
 */
const SECTION_OPEN_KEY_PREFIX = "nyx.ui.section:";
/** Schema version of a band's fold state — a bare boolean. */
const SECTION_OPEN_VERSION = 1;

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

/**
 * WHICH SCOPE the app was left in — the entered project, every project, or Système. `"project"`
 * when nothing is remembered, which is the app's default.
 *
 * It rides the SAME registry as the entered project right above, and for the same reason: the two
 * are ONE answer to "where was the user" and a relaunch has to restore them together, or reopening
 * in another scope would land on a rail that says otherwise. Unlike the project id, there is
 * nothing to VALIDATE against a live tree — "all projects" and "the host" are true of every tree,
 * the empty one included — so this hint is honoured as read, and the provider seeds itself from it
 * directly rather than waiting for a restore effect.
 *
 * IT FALLS BACK ON THE LEGACY BOOLEAN, once, and only to read it. This key replaces the two-state
 * `nyx.ui.globalScope` written before Système became a scope of its own; an install carrying that
 * value would otherwise reopen in the project scope after having been left in "All projects" — a
 * silent loss of the one thing this module exists to keep. The migration is a READ: the next scope
 * change writes the new key and the old one simply stops being consulted.
 */
export function loadScopeKind(): ScopeKind {
  const stored = loadVersionedJson(
    SCOPE_KIND_KEY,
    SCOPE_KIND_VERSION,
    (value): value is ScopeKind => value === "all" || value === "system" || value === "project",
  );
  if (stored != null) return stored;
  const legacyGlobal = loadVersionedJson(
    LEGACY_GLOBAL_SCOPE_KEY,
    LEGACY_GLOBAL_SCOPE_VERSION,
    (value): value is boolean => typeof value === "boolean",
  );
  // Nothing remembered at all → the app's default scope, the entered project's.
  return legacyGlobal === true ? "all" : "project";
}

/** Remember which scope the app is in. */
export function saveScopeKind(kind: ScopeKind): void {
  saveVersionedJson(SCOPE_KIND_KEY, SCOPE_KIND_VERSION, kind);
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

/** The two typed bands of a workspace's runtime tree — the second half of a band's storage key. */
export type RuntimeBand = "terminals" | "commands";

/**
 * The key ONE band of ONE workspace is remembered under. Exported so the fold hook, this module and
 * its test all name it the same way rather than each rebuilding the string.
 */
export function sectionOpenKey(workspaceId: string, band: RuntimeBand): string {
  return `${SECTION_OPEN_KEY_PREFIX}${workspaceId}:${band}`;
}

/**
 * Whether a workspace's TERMINALS / COMMANDS band is unfolded — `null` when it has never been
 * folded or unfolded, in which case the caller's default applies (both bands open: the runtime tree
 * is what a workspace accordion opens ONTO, so arriving on a folded one would be arriving on
 * nothing).
 *
 * Rides the same registry, the same guards and the same versioned envelope as
 * {@link loadGroupOpen} — one key per (workspace, band), so a deleted workspace leaves two dead
 * entries behind and never a growing blob to migrate.
 */
export function loadSectionOpen(workspaceId: string, band: RuntimeBand): boolean | null {
  return loadVersionedJson(
    sectionOpenKey(workspaceId, band),
    SECTION_OPEN_VERSION,
    (value): value is boolean => typeof value === "boolean",
  );
}

/** Remember a band's folded/unfolded state for one workspace (or, for `null`, forget it). */
export function saveSectionOpen(
  workspaceId: string,
  band: RuntimeBand,
  open: boolean | null,
): void {
  saveVersionedJson(sectionOpenKey(workspaceId, band), SECTION_OPEN_VERSION, open);
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
