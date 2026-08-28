/**
 * PER-PROJECT PANE LAYOUT PERSISTENCE — where the tiling of the Terminals page is remembered.
 *
 * The layout is a LOCAL UI PREFERENCE, exactly like the "last active terminal" hint next to it:
 * per-machine, no cross-device meaning, no backend. It is stored as VERSIONED JSON
 * ({@link loadVersionedJson}) under a key SCOPED BY PROJECT — `nyx.ui.paneLayout:<projectId>` — so
 * two projects can never read each other's tiling, whatever happens to their workspaces (the key
 * is keyed on the project id alone; workspaces are not part of the identity).
 *
 * ## What is stored (VERSION 2)
 *
 * v1 stored ONE tree per project. The model is now a COLLECTION of groups plus solo terminals
 * (`pane-groups.ts`), so v2 stores `{ groups: [{ id, tree, name, color }] }`. Solo terminals are
 * NOT stored: "solo" is the absence of membership, so it needs no record and cannot go stale.
 *
 * A v1 entry is MIGRATED, not discarded ({@link migrateV1}): its single tree becomes a single
 * unnamed, uncoloured group holding the same leaves with the same ratios — which is exactly what
 * that user was looking at. A v1 tree with a SINGLE leaf becomes no group at all: one terminal is
 * not a group, and it comes back as the solo terminal it visually already was.
 *
 * RESTORE is defensive, in this order ({@link restorePaneCollection}):
 *   1. unknown schema version / unparseable / invalid shape → discard the whole entry;
 *   2. drop the leaves whose terminal no longer exists in the project's live set, COMPACTING each
 *      tree and DISSOLVING any group left with fewer than two members (a terminal can be closed
 *      while the app is shut, or by an agent over MCP);
 *   3. pull every RATIO back into what its own sub-box allows at the MEASURED host
 *      ({@link compactPaneRatios}) — the px half of the validation, added in review round 3: shape
 *      and liveness said nothing about pixels, so an entry edited by hand to 0.98 restored a
 *      14.40px pane on a 720px host. Repaired, never rejected: one bad number must not cost the
 *      user the whole tiling. Skipped when the host is not measurable (there is nothing to judge
 *      with — the seam clamp and `canSplitPane` remain the guarantees, exactly as before).
 *
 * There is no "single pane fallback" here any more: a project whose groups all vanished simply has
 * no group, and the page shows the selected terminal SOLO — which is the escapable-tiling model's
 * whole point.
 */

import { compactPaneRatios, type HostSize } from "@/components/panes/pane-geometry";
import {
  EMPTY_COLLECTION,
  isPaneCollection,
  newGroupId,
  pruneCollection,
  type PaneCollection,
  type PaneGroup,
} from "@/components/panes/pane-groups";
import { isPaneNode, paneLeaves, type PaneNode } from "@/components/panes/pane-tree";
import { loadVersionedJson, saveVersionedJson } from "./ui-persistence";

/** Prefix of the per-project layout key (`<prefix><projectId>`). */
const LAYOUT_KEY_PREFIX = "nyx.ui.paneLayout:";

/**
 * Schema version of the persisted layout. BUMP IT whenever the stored shape changes; an entry
 * written under an older version is MIGRATED when a migration exists ({@link migrateV1}) and
 * discarded otherwise.
 */
export const PANE_LAYOUT_VERSION = 2;

/** The version that stored a single {@link PaneNode} per project (pre-groups). */
export const PANE_LAYOUT_VERSION_V1 = 1;

/** The `localStorage` key holding `projectId`'s layout. */
function layoutKey(projectId: string): string {
  return `${LAYOUT_KEY_PREFIX}${projectId}`;
}

/**
 * MIGRATE a v1 entry (one tree) into the v2 collection: the tree becomes ONE unnamed, uncoloured
 * group with the same panes and the same ratios. A single-leaf v1 tree yields NO group — that
 * terminal was already alone on screen and comes back solo.
 *
 * Exported for the migration test, which pins "same terminals, same ratios".
 */
export function migrateV1(tree: PaneNode, groupId: string = newGroupId()): PaneCollection {
  if (paneLeaves(tree).length < 2) return EMPTY_COLLECTION;
  const group: PaneGroup = { id: groupId, tree, name: null, color: null };
  return { groups: [group] };
}

/**
 * The RAW persisted collection of `projectId` — shape-validated and MIGRATED from v1 when needed,
 * but NOT reconciled against the live terminals (that is {@link restorePaneCollection}).
 * `null` when absent, corrupted, or written under a version with no migration.
 */
export function loadPaneCollection(projectId: string): PaneCollection | null {
  const key = layoutKey(projectId);
  const current = loadVersionedJson<PaneCollection>(key, PANE_LAYOUT_VERSION, isPaneCollection);
  if (current != null) return current;
  // No v2 entry: a v1 tree from a previous app version is upgraded rather than thrown away.
  const legacy = loadVersionedJson<PaneNode>(key, PANE_LAYOUT_VERSION_V1, isPaneNode);
  return legacy == null ? null : migrateV1(legacy);
}

/** Remember (or, for an empty collection, forget) `projectId`'s groups. */
export function savePaneCollection(projectId: string, collection: PaneCollection): void {
  saveVersionedJson(
    layoutKey(projectId),
    PANE_LAYOUT_VERSION,
    collection.groups.length === 0 ? null : collection,
  );
}

/**
 * The GROUPS to show for `projectId` given its live terminals — the full restore policy (see the
 * module header). Never returns a group referencing a dead terminal, and never a group with fewer
 * than two members.
 *
 * `host` is the tiling's measured px box when the caller has one. It is what makes step 3 (the px
 * compaction) possible at all; `null`/omitted keeps the pre-round-3 behaviour exactly.
 *
 * `entry` lets a caller that has ALREADY read the raw collection hand it in instead of paying a
 * second `localStorage` read + `JSON.parse` + full `isPaneCollection` walk of every tree — which
 * `usePaneLayout` did, because it needs the stored ids to work out which of them the current
 * runtime read cannot disown. `undefined` (the default) means "read it yourself", so every other
 * caller is untouched; an explicit `null` means "there is no entry" and is honoured as such.
 */
export function restorePaneCollection(
  projectId: string,
  liveTerminalIds: readonly string[],
  host: HostSize | null = null,
  entry?: PaneCollection | null,
): PaneCollection {
  const stored = entry === undefined ? loadPaneCollection(projectId) : entry;
  if (stored == null) return EMPTY_COLLECTION;
  const reconciled = pruneCollection(stored, liveTerminalIds);
  if (reconciled.groups.length === 0) return EMPTY_COLLECTION;
  return {
    groups: reconciled.groups.map((group) => ({
      ...group,
      tree: compactPaneRatios(group.tree, host),
    })),
  };
}
