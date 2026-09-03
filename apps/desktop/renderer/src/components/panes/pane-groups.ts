/**
 * PANE GROUPS — the COLLECTION model that replaced the single, permanent pane tree.
 *
 * ## Why the model changed (human review 01M0ABR6V501D0SPR2BYBVZC4A, critical)
 *
 * v1 gave a project ONE tree and nothing else, so `use-pane-layout` had no way to show a terminal
 * that was not in it: picking one in the sidebar SWAPPED it into the focused pane
 * (`setPaneTerminal`). That was the specified behaviour ("un seul pane focal", "aucun picker ni
 * pane vide en v1") and it made the tiling INESCAPABLE — once split, the user could never get back
 * to a single terminal filling the frame. The fix is the model, not a patch:
 *
 *  - a project holds N GROUPS **plus** N SOLO terminals;
 *  - a terminal belongs to ZERO or ONE group, never two;
 *  - a group holds AT LEAST TWO members — removing the second-to-last DISSOLVES it and leaves the
 *    survivor solo;
 *  - each group carries ITS OWN {@link PaneNode} tree — the exact structure, mutators, separators,
 *    180×120px floor and restore compaction that were already audited. Nothing of that is rewritten
 *    here: a group is a tree PLUS an identity plus two optional labels;
 *  - a solo terminal has NO tree. It fills the frame.
 *
 * A terminal is therefore never "missing from the layout": it is either in a group or on its own,
 * and both are showable. `setPaneTerminal` has no caller left in the selection path.
 *
 * ## Membership is derived, never stored twice
 *
 * There is no `terminalId -> groupId` map. The ONLY record of membership is the leaf sitting in a
 * group's tree, so "at most one group" is a consequence of {@link groupOfTerminal} scanning the
 * collection and of every mutator removing a terminal from wherever it was BEFORE placing it. A
 * second index would be a second truth to keep in sync.
 *
 * Everything here is PURE: mutators return a NEW collection and never touch the terminal store, so
 * dissolving a group or pulling a terminal out of one closes NOTHING (PTY, xterm buffer and DB
 * record are untouched).
 *
 * ## THIS MODULE IS SHARED — WHICH IS WHY IT LIVES IN `components/panes/`
 *
 * The grouping was designed inside `components/terminals/` and used to sit there. It does not any
 * more: the model it defines is the vocabulary of BOTH surfaces — `components/terminals/` draws
 * TERMINAL groups with it, `components/command/` draws COMMAND groups with it — so leaving it
 * under one of the two made that one the owner of the other's vocabulary and closed an import
 * CYCLE between the directories (review finding 01M0ZMHM9GSC39SCN2DBRQ8VX3).
 *
 * `components/panes/` is the NEUTRAL module both depend on and neither owns: `pane-tree`,
 * `pane-geometry`, `pane-groups`, `group-list-blocks`, `group-block`, `group-panel`. Nothing here
 * imports `@/components/terminals` or `@/components/command` — that is the invariant that keeps
 * the dependency a tree instead of a cycle, and it is what makes the two features unable to drift
 * into two models the user would have to learn twice. Think of BOTH callers before changing them.
 */

import {
  clampRatio,
  closePane,
  createPaneLayout,
  DEFAULT_SPLIT_RATIO,
  findPaneByTerminal,
  isPaneNode,
  movePaneToEdge,
  newPaneId,
  paneLeaves,
  pruneStaleTerminals,
  splitPane,
  type PaneEdge,
  type PaneNode,
  type SplitDirection,
} from "./pane-tree";

/**
 * How many `--group-N` tokens exist — the ONE place the palette's size is written on this side.
 *
 * It is pinned to the CSS by `group-palette.test.ts` (which derives the token names from this very
 * constant) and to the backend by the Rust `the_palette_size_is_the_same_number_on_all_five_layers`
 * test (`crates/nyx-core/src/db.rs`, `mod tests`), which reads this line, the `--group-N` tokens in
 * `globals.css`, `COMMAND_GROUP_COLOR_MAX`, the `create_command_groups` `up.sql` CHECK, the
 * `manage_command_group` MCP schema and `CATEGORY_COLOR_TOKENS`, and refuses any disagreement.
 * Nothing may restate the number.
 */
export const GROUP_COLOR_COUNT = 6;

/**
 * `1 | 2 | … | GROUP_COLOR_COUNT` — DERIVED from the count rather than spelled out, so the literal
 * union cannot say six while the constant says seven. Counts up an accumulator tuple and collects
 * its length at each step; at this size the recursion is trivial.
 */
type OneToN<N extends number, Acc extends 0[] = [], Out = never> = Acc["length"] extends N
  ? Out
  : OneToN<N, [...Acc, 0], Out | [...Acc, 0]["length"]>;

/**
 * The colour a group may CHOOSE, as the 1-based index of a `--group-N` token. `null` — the
 * DEFAULT — is not a colour: it is the ABSENCE of a choice, painted with the design system's
 * existing neutral (`--group-0`, the value `--ring` carries in the dark theme). The tokens
 * themselves live in `globals.css` (the "CATEGORICAL GROUP PALETTE" block) and are pinned by
 * `group-palette.test.ts`.
 */
export type GroupColor = OneToN<typeof GROUP_COLOR_COUNT>;

/** Whether `value` is a usable {@link GroupColor} (the untrusted-input guard). */
export function isGroupColor(value: unknown): value is GroupColor {
  return (
    typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= GROUP_COLOR_COUNT
  );
}

/**
 * A GROUP: an identity, a pane tree, and two OPTIONAL labels.
 *
 * `name` and `color` are independent and both default to "unset" — a group born from a split is
 * grey and nameless, and stays fully usable that way forever. Neither conditions the group's
 * existence: clearing both leaves the group intact (user arbitrage).
 */
export interface PaneGroup {
  readonly id: string;
  /** This group's own pane tree — always at least two leaves. */
  readonly tree: PaneNode;
  /**
   * The optional user-given name. `null` = unnamed. The block ALWAYS opens on a head (review
   * 01M0BPP79090KAQ9ZAFJ23ZRTX) — but where there is no name the head shows NO WORDS AT ALL
   * (review 01M0CY3F075ACP4TFBK1KJ49X8: « si pas de nom, on affiche rien »), just its grip, its `…`
   * and `data-group-head-label="unnamed"`. That attribute, never a string, is what separates a
   * named group from an anonymous one.
   */
  readonly name: string | null;
  /** The optional user-chosen colour. `null` = the neutral default. */
  readonly color: GroupColor | null;
}

/**
 * HOW A GROUP IS NAMED WHERE IT IS NOT ON SCREEN — the ONE rule.
 *
 * A group needs no name (grey + unnamed is the default, and a group born from a split stays that
 * way forever), so a menu entry that must designate ITS target cannot fall back to a bare "group":
 * that label repeats IDENTICALLY as soon as a second unnamed group exists — the nominal case, not
 * an edge one — and the user picks blind. The fallback is therefore the group's RANK in the
 * project's list, which is what the list itself shows.
 *
 * ONE caller names groups off screen today: the commands' `command-catalog.tsx` ("Add to …").
 * The terminals had a second one — the sidebar row's "Join <group>" — and it is gone: a terminal
 * joins a group by being DROPPED in its block, so the target is pointed at rather than named. The
 * rule still lives in the shared brick because it is the shared brick that owns what a group's
 * name IS, and structural typing keeps it honest — a `PaneGroup` and a `CommandGroup` differ
 * everywhere except the optional `name`, which is all this needs.
 */
export function groupLabel(group: { readonly name: string | null }, index: number): string {
  const name = group.name?.trim();
  return name ? `“${name}”` : `group ${index + 1}`;
}

/**
 * The project's whole layout model: its groups. Solo terminals are NOT listed — "solo" is the
 * absence of membership, so it needs no record and cannot drift out of sync with the live terminal
 * set (a terminal that disappears simply stops being solo).
 */
export interface PaneCollection {
  readonly groups: readonly PaneGroup[];
}

/** The empty collection — every terminal solo. */
export const EMPTY_COLLECTION: PaneCollection = { groups: [] };

/** A fresh, collision-free group id. */
export function newGroupId(): string {
  const rand =
    globalThis.crypto?.randomUUID?.().replace(/-/g, "") ?? Math.random().toString(36).slice(2);
  return `group-${rand}`;
}

/** The group holding `terminalId`, or `null` when the terminal is SOLO. */
export function groupOfTerminal(collection: PaneCollection, terminalId: string): PaneGroup | null {
  return (
    collection.groups.find((group) => findPaneByTerminal(group.tree, terminalId) != null) ?? null
  );
}

/** The group with `groupId`, or `null`. */
export function findGroup(collection: PaneCollection, groupId: string): PaneGroup | null {
  return collection.groups.find((group) => group.id === groupId) ?? null;
}

/** Every terminal id that belongs to SOME group, in group then leaf order. */
export function groupedTerminalIds(collection: PaneCollection): string[] {
  return collection.groups.flatMap((group) => paneLeaves(group.tree).map((l) => l.terminalId));
}

/** The members of `group`, in the tree's visual reading order. */
export function groupMemberIds(group: PaneGroup): string[] {
  return paneLeaves(group.tree).map((leaf) => leaf.terminalId);
}

/**
 * Replace `groupId`'s tree, applying the ONE structural rule of the collection: a tree that fell
 * to a SINGLE leaf is not a group any more — the group is dropped and its last member becomes
 * solo. `null` (an emptied tree) drops the group outright.
 *
 * Every mutator below funnels through this, which is why "a group has at least two members" holds
 * for any sequence of operations rather than being re-checked at each call site.
 */
function withTree(
  collection: PaneCollection,
  groupId: string,
  tree: PaneNode | null,
): PaneCollection {
  const index = collection.groups.findIndex((group) => group.id === groupId);
  if (index < 0) return collection;
  const groups = [...collection.groups];
  if (tree == null || paneLeaves(tree).length < 2) {
    groups.splice(index, 1);
  } else {
    const current = groups[index];
    if (current.tree === tree) return collection;
    groups[index] = { ...current, tree };
  }
  return { groups };
}

/**
 * Remove `terminalId` from whatever group holds it — the tree COMPACTS around the hole (the
 * audited `closePane` compaction), and a group left with one member DISSOLVES.
 *
 * Returns the collection UNCHANGED when the terminal was already solo, so callers can call it
 * unconditionally before placing a terminal somewhere else.
 */
export function detachTerminal(collection: PaneCollection, terminalId: string): PaneCollection {
  const group = groupOfTerminal(collection, terminalId);
  if (group == null) return collection;
  const leaf = findPaneByTerminal(group.tree, terminalId);
  if (leaf == null) return collection;
  // `closePane` refuses to empty a root leaf, so a 2-member group needs the direct drop; for
  // anything larger it is the audited compaction.
  const tree = group.tree.kind === "leaf" ? null : closePane(group.tree, leaf.id);
  return withTree(collection, group.id, tree);
}

/**
 * DISSOLVE a group: every member becomes solo. The panel's "Dégrouper tout".
 * An unknown id is a no-op.
 */
export function dissolveGroup(collection: PaneCollection, groupId: string): PaneCollection {
  const groups = collection.groups.filter((group) => group.id !== groupId);
  return groups.length === collection.groups.length ? collection : { groups };
}

/**
 * CREATE a group of two from a SOLO terminal that has just been split: `terminalId` keeps the
 * original pane and `newTerminalId` takes the new one, on the requested side. This is the ONLY way
 * a group is born from the pointer — a split inside a group goes through {@link splitInGroup}.
 *
 * Returns the new collection AND the group so the caller can display and focus it without a second
 * lookup.
 */
export function createGroupFromSplit(
  collection: PaneCollection,
  options: {
    readonly terminalId: string;
    readonly newTerminalId: string;
    readonly direction: SplitDirection;
    readonly paneId?: string;
    readonly newPaneId?: string;
    readonly groupId?: string;
  },
): { collection: PaneCollection; group: PaneGroup; paneId: string } | null {
  // The terminal must really be solo: splitting a member is `splitInGroup`'s job, and silently
  // moving it here would let one terminal sit in two groups.
  if (groupOfTerminal(collection, options.terminalId) != null) return null;
  if (groupOfTerminal(collection, options.newTerminalId) != null) return null;
  if (options.terminalId === options.newTerminalId) return null;

  const rootPaneId = options.paneId ?? newPaneId();
  const created = options.newPaneId ?? newPaneId();
  const tree = splitPane(createPaneLayout(options.terminalId, rootPaneId), {
    paneId: rootPaneId,
    direction: options.direction,
    terminalId: options.newTerminalId,
    newPaneId: created,
  });
  if (tree == null) return null;
  const group: PaneGroup = { id: options.groupId ?? newGroupId(), tree, name: null, color: null };
  return { collection: { groups: [...collection.groups, group] }, group, paneId: created };
}

/**
 * SPLIT a pane INSIDE an existing group — a leaf is added to that group's own tree, exactly as
 * before. Returns `null` when the split is impossible (unknown group/pane, or the terminal is
 * already placed somewhere).
 */
export function splitInGroup(
  collection: PaneCollection,
  options: {
    readonly groupId: string;
    readonly paneId: string;
    readonly direction: SplitDirection;
    readonly terminalId: string;
    readonly newPaneId?: string;
  },
): { collection: PaneCollection; paneId: string } | null {
  const group = findGroup(collection, options.groupId);
  if (group == null) return null;
  if (groupOfTerminal(collection, options.terminalId) != null) return null;
  const created = options.newPaneId ?? newPaneId();
  const tree = splitPane(group.tree, {
    paneId: options.paneId,
    direction: options.direction,
    terminalId: options.terminalId,
    newPaneId: created,
  });
  if (tree == null) return null;
  return { collection: withTree(collection, group.id, tree), paneId: created };
}

/**
 * CLOSE a pane inside a group — a pure LAYOUT edit: the terminal keeps running and simply becomes
 * SOLO (it is still in the sidebar, still reachable, still alive). A group left with one member
 * dissolves, so closing a pane of a pair leaves both terminals solo.
 */
export function closeGroupPane(
  collection: PaneCollection,
  groupId: string,
  paneId: string,
): PaneCollection {
  const group = findGroup(collection, groupId);
  if (group == null) return collection;
  if (group.tree.kind === "leaf") return collection;
  const tree = closePane(group.tree, paneId);
  if (tree === group.tree) return collection;
  return withTree(collection, groupId, tree);
}

/**
 * The direction the NEXT member of a tree lands on — "à droite, puis en bas", the same order a
 * pointer split produces. A 1-leaf tree grows to the right, a 2-leaf tree grows downwards, and it
 * alternates from there.
 */
export function nextAppendDirection(tree: PaneNode): SplitDirection {
  return paneLeaves(tree).length % 2 === 1 ? "row" : "column";
}

/**
 * WHERE an insertion at `index` SPLITS — the one place the drop position becomes a pane operation.
 *
 * "Entering a group" takes the SPLIT's path and no other (review 01M0BPP79090KAQ9ZAFJ23ZRTX): the
 * newcomer is not appended to a derived layout, it splits an existing leaf exactly as a pointer
 * split would. That is what makes the 180×120px floor apply to it for free — the caller runs
 * `canSplitPane` on THIS anchor before committing, and refuses on the same answer the pane's own
 * split button gets.
 *
 * The anchor is the member the newcomer lands NEXT TO: the one before the insertion point, or the
 * first member when the drop is at the very top of the block (the new leaf then travels to index 0
 * through {@link withMemberOrder}, so the anchor only decides WHICH rectangle is halved).
 */
export function groupInsertAnchor(
  group: PaneGroup,
  index: number,
): { paneId: string; direction: SplitDirection } {
  const leaves = paneLeaves(group.tree);
  const anchorIndex = Math.min(Math.max(index - 1, 0), leaves.length - 1);
  return { paneId: leaves[anchorIndex].id, direction: nextAppendDirection(group.tree) };
}

/**
 * Re-assign the terminals of a tree's leaves so the READING order becomes `orderedIds`.
 *
 * The STRUCTURE is untouched — same nodes, same directions, same ratios, same pane ids — only which
 * terminal sits in which rectangle changes. That is what "re-ordering the members re-orders the
 * panes" has to mean for a tree that the user has already sized: a drag in the sidebar must not
 * silently rebuild the tiling they arranged.
 *
 * Returns the tree UNCHANGED (by reference) when `orderedIds` is not a permutation of its leaves or
 * already is its order.
 */
export function withMemberOrder(tree: PaneNode, orderedIds: readonly string[]): PaneNode {
  const leaves = paneLeaves(tree);
  if (leaves.length !== orderedIds.length) return tree;
  const current = leaves.map((leaf) => leaf.terminalId);
  if (current.every((id, i) => id === orderedIds[i])) return tree;
  if (new Set(orderedIds).size !== orderedIds.length) return tree;
  if (!current.every((id) => orderedIds.includes(id))) return tree;
  let cursor = 0;
  const rewrite = (node: PaneNode): PaneNode => {
    if (node.kind === "leaf") {
      const terminalId = orderedIds[cursor++];
      return node.terminalId === terminalId ? node : { ...node, terminalId };
    }
    const first = rewrite(node.children[0]);
    const second = rewrite(node.children[1]);
    if (first === node.children[0] && second === node.children[1]) return node;
    return { ...node, children: [first, second] };
  };
  return rewrite(tree);
}

/**
 * RE-ORDER a group's members — the sidebar's "drop a row elsewhere INSIDE its own block" (gesture
 * 1). Membership is untouched; the panes swap occupants through {@link withMemberOrder}.
 */
export function orderGroupMembers(
  collection: PaneCollection,
  groupId: string,
  orderedIds: readonly string[],
): PaneCollection {
  const group = findGroup(collection, groupId);
  if (group == null) return collection;
  const tree = withMemberOrder(group.tree, orderedIds);
  return tree === group.tree ? collection : withTree(collection, groupId, tree);
}

/**
 * INSERT a terminal into a group AT A POSITION — the sidebar's "drop a row INSIDE a block"
 * (gesture 3). It leaves whatever group held it first (which may dissolve that one), splits the
 * anchor {@link groupInsertAnchor} names, then permutes the leaves so the newcomer reads exactly
 * where the insertion line was.
 *
 * Returns `null` when the move is impossible (unknown group, or the terminal is already a member of
 * that very group). The FLOOR is NOT checked here — geometry lives in `pane-geometry`, and the
 * caller checks it on the same anchor BEFORE calling, so the refusal can be shown during the drag.
 */
export function insertIntoGroup(
  collection: PaneCollection,
  groupId: string,
  terminalId: string,
  index: number,
  paneId: string = newPaneId(),
): { collection: PaneCollection; paneId: string } | null {
  const current = groupOfTerminal(collection, terminalId);
  if (current != null && current.id === groupId) return null;
  const detached = detachTerminal(collection, terminalId);
  // Detaching may have DISSOLVED the target group (it held the terminal's last partner) — re-read.
  const group = findGroup(detached, groupId);
  if (group == null) return null;
  const members = groupMemberIds(group);
  const at = Math.min(Math.max(index, 0), members.length);
  const anchor = groupInsertAnchor(group, at);
  const split = splitPane(group.tree, {
    paneId: anchor.paneId,
    direction: anchor.direction,
    terminalId,
    newPaneId: paneId,
  });
  if (split == null) return null;
  const ordered = [...members.slice(0, at), terminalId, ...members.slice(at)];
  return { collection: withTree(detached, groupId, withMemberOrder(split, ordered)), paneId };
}

/**
 * MOVE a pane WITHIN a group by dropping it on an EDGE of another of the group's panes — the
 * view's restructure gesture (review 01M0CY7R5Z7KV5R49FQYXH9QRN). A pure LAYOUT edit through
 * {@link movePaneToEdge}: no leaf is added or removed, so membership, terminal instances and PTYs
 * are untouched; only WHERE the moved leaf sits in the tree changes. The moved pane keeps its id
 * (returned as `paneId`), which is what lets the caller keep the FOCUS on it.
 *
 * Returns `null` when the move is impossible (unknown group/pane, same pane twice). The FLOOR is
 * NOT checked here — the caller judges the candidate tree (`layoutFitsFloor`) BEFORE calling, the
 * same division of labour as {@link insertIntoGroup}.
 */
export function movePaneInGroup(
  collection: PaneCollection,
  groupId: string,
  draggedPaneId: string,
  targetPaneId: string,
  edge: PaneEdge,
): { collection: PaneCollection; paneId: string } | null {
  const group = findGroup(collection, groupId);
  if (group == null) return null;
  const tree = movePaneToEdge(group.tree, draggedPaneId, targetPaneId, edge);
  if (tree == null) return null;
  return { collection: withTree(collection, groupId, tree), paneId: draggedPaneId };
}

/*
 * THERE IS NO SEPARATE "JOIN" ANY MORE (review 01M0BSNTEN5N2GC9QQBASJYHHB).
 *
 * `joinGroup(collection, groupId, terminalId)` — and the `appendToTree` it was the only caller of —
 * lived here for the sidebar row's "Rejoindre" menu entry. That entry is gone (the row speaks only
 * of the row now; joining IS the drag, gesture 3), and the operation it named is the END-INDEX case
 * of {@link insertIntoGroup}, not a second one: at `index = members.length`, {@link
 * groupInsertAnchor} returns the LAST leaf and {@link nextAppendDirection} — i.e. exactly the split
 * `appendToTree` performed — and re-ordering the members to `[...members, terminalId]` is the
 * identity. One operation, one function, and the 180×120px floor applies to it like to any other
 * insertion.
 */

/** Set (or, with `null`, clear) a group's NAME. The group survives either way. */
export function setGroupName(
  collection: PaneCollection,
  groupId: string,
  name: string | null,
): PaneCollection {
  const trimmed = name == null ? null : name.trim();
  const next = trimmed == null || trimmed.length === 0 ? null : trimmed;
  const groups = collection.groups.map((group) =>
    group.id === groupId && group.name !== next ? { ...group, name: next } : group,
  );
  return groups.every((g, i) => g === collection.groups[i]) ? collection : { groups };
}

/** Set (or, with `null`, clear) a group's COLOUR. The group survives either way. */
export function setGroupColor(
  collection: PaneCollection,
  groupId: string,
  color: GroupColor | null,
): PaneCollection {
  const groups = collection.groups.map((group) =>
    group.id === groupId && group.color !== color ? { ...group, color } : group,
  );
  return groups.every((g, i) => g === collection.groups[i]) ? collection : { groups };
}

/**
 * RECONCILE the whole collection against the live terminal set: a terminal that really disappeared
 * loses its leaf, its group compacts, and a group left with fewer than two members dissolves.
 * Referentially stable when nothing changed, so it is safe to run on every snapshot push.
 */
export function pruneCollection(
  collection: PaneCollection,
  liveTerminalIds: Iterable<string>,
): PaneCollection {
  const live = new Set(liveTerminalIds);
  let changed = false;
  const groups: PaneGroup[] = [];
  for (const group of collection.groups) {
    const tree = pruneStaleTerminals(group.tree, live);
    if (tree == null || paneLeaves(tree).length < 2) {
      changed = true;
      continue;
    }
    if (tree === group.tree) groups.push(group);
    else {
      changed = true;
      groups.push({ ...group, tree });
    }
  }
  return changed ? { groups } : collection;
}

/**
 * SHAPE VALIDATION of an untrusted (persisted) collection. Rejects wholesale rather than repairing:
 * every tree must be a valid {@link PaneNode} with at least two leaves, every group id must be
 * unique, and NO terminal may appear in two groups (the invariant the whole model rests on).
 */
export function isPaneCollection(value: unknown): value is PaneCollection {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as { readonly groups?: unknown };
  if (!Array.isArray(candidate.groups)) return false;

  const groupIds = new Set<string>();
  const terminalIds = new Set<string>();
  for (const raw of candidate.groups) {
    if (typeof raw !== "object" || raw === null) return false;
    const group = raw as {
      readonly id?: unknown;
      readonly tree?: unknown;
      readonly name?: unknown;
      readonly color?: unknown;
    };
    if (typeof group.id !== "string" || group.id.length === 0) return false;
    if (groupIds.has(group.id)) return false;
    groupIds.add(group.id);
    if (group.name !== null && typeof group.name !== "string") return false;
    if (group.color !== null && !isGroupColor(group.color)) return false;
    if (!isPaneNode(group.tree)) return false;
    const leaves = paneLeaves(group.tree);
    if (leaves.length < 2) return false;
    for (const leaf of leaves) {
      // ONE group per terminal — a persisted entry that breaks it is not a collection our mutators
      // could have produced, so it is discarded rather than silently deduplicated.
      if (terminalIds.has(leaf.terminalId)) return false;
      terminalIds.add(leaf.terminalId);
    }
  }
  return true;
}

/* ────────────────────────── THE AUTOMATIC DISPOSITION ──────────────────────────
 *
 * {@link nextAppendDirection} grows a tree the way a POINTER split does — one leaf at a time, right
 * then down — which is what a terminal joining a group needs, and what {@link groupInsertAnchor}
 * hands {@link insertIntoGroup} at the end index. A COMMANDS group has no pointer split at
 * all (user arbitrage: composition happens in the settings screen, never in the render), so its
 * tree is DERIVED from its ordered member list instead of being grown: reordering the vignettes in
 * the settings screen re-derives the whole disposition, which is exactly what "réglable" means.
 *
 * The shapes are the validated ones:
 *   2 → side by side;               3 → the third passes UNDERNEATH the second (the first keeps
 *   the full-height left column);   4 → a quadrant, filled row-major (1 2 / 3 4).
 * Beyond four it generalises the quadrant: two columns, filled row-major.
 */
/**
 * Stack `nodes` into a right-leaning `column` chain (top to bottom), in EQUAL bands. One node stays
 * as it is.
 *
 * The ratio is `1 / (how many nodes this split still has to share)`, never a flat
 * {@link DEFAULT_SPLIT_RATIO}. A nested chain at 0.5 gives the FIRST node half the box and squeezes
 * every following one into the remainder — a column of three came out 50/25/25 instead of thirds,
 * and a column of four 50/25/12.5/12.5, so the automatic disposition was visibly lopsided from five
 * members on (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding 01M11XTRXVVPZ9XMDW5ARWKWHA — a latent
 * defect found while designing the shapes, not something the shapes introduced).
 *
 * Two nodes still cut at 0.5, which is why the 2/3/4-member layouts are untouched by the fix.
 */
function stackAlong(
  direction: SplitDirection,
  nodes: readonly PaneNode[],
  mint: (prefix: string) => string,
): PaneNode {
  let node = nodes[nodes.length - 1];
  for (let i = nodes.length - 2; i >= 0; i -= 1) {
    // This split owns `nodes[i..]` — `remaining` bands — and its FIRST child must take exactly one
    // of them, leaving the rest to the chain underneath (which applies the same rule).
    const remaining = nodes.length - i;
    node = {
      kind: "split",
      id: mint("split"),
      direction,
      // THROUGH `clampRatio`, like EVERY other ratio writer in the model. `1 / remaining` falls
      // under `MIN_STORED_RATIO` (2%) from 51 members on, and `pane-tree.ts` states as a tree-wide
      // invariant that a stored ratio is always in band — `isPaneNode` REJECTS a tree that is not.
      // This was the one writer that did not end here, so a long chain minted a tree the model
      // declares impossible (the floor refuses such a group long before, which is exactly why the
      // gap was invisible rather than absent).
      ratio: clampRatio(1 / remaining),
      children: [nodes[i], node],
    };
  }
  return node;
}

/** {@link stackAlong} in the `column` direction — the automatic disposition's own stacker. */
function stackColumn(nodes: readonly PaneNode[], mint: (prefix: string) => string): PaneNode {
  return stackAlong("column", nodes, mint);
}

/**
 * The AUTOMATIC disposition of an ordered member list — the tree a COMMANDS group shows.
 *
 * Derived, not grown: the caller hands the members in the order the settings screen holds them and
 * gets the whole tree back, so a reorder in that screen IS the disposition change. `mintId` is
 * injectable so a test can pin the exact rectangles against stable ids.
 *
 * Fewer than two ids yields `null` — that is not a group.
 */
export function autoLayoutTree(
  terminalIds: readonly string[],
  mintId: (prefix: string) => string = (prefix) =>
    prefix === "pane" ? newPaneId() : `split-${Math.random().toString(36).slice(2)}`,
): PaneNode | null {
  if (terminalIds.length < 2) return null;
  const leaf = (terminalId: string): PaneNode => ({
    kind: "leaf",
    id: mintId("pane"),
    terminalId,
  });
  const [first, second, third] = terminalIds;
  if (terminalIds.length === 2) {
    return {
      kind: "split",
      id: mintId("split"),
      direction: "row",
      ratio: DEFAULT_SPLIT_RATIO,
      children: [leaf(first), leaf(second)],
    };
  }
  if (terminalIds.length === 3) {
    // The THIRD passes underneath the second; the first keeps the full-height left column.
    return {
      kind: "split",
      id: mintId("split"),
      direction: "row",
      ratio: DEFAULT_SPLIT_RATIO,
      children: [
        leaf(first),
        {
          kind: "split",
          id: mintId("split"),
          direction: "column",
          ratio: DEFAULT_SPLIT_RATIO,
          children: [leaf(second), leaf(third)],
        },
      ],
    };
  }
  // FOUR AND BEYOND — two columns filled ROW-MAJOR, which is the quadrant at four (1 2 / 3 4).
  const left: PaneNode[] = [];
  const right: PaneNode[] = [];
  terminalIds.forEach((terminalId, index) => {
    (index % 2 === 0 ? left : right).push(leaf(terminalId));
  });
  return {
    kind: "split",
    id: mintId("split"),
    direction: "row",
    ratio: DEFAULT_SPLIT_RATIO,
    children: [stackColumn(left, mintId), stackColumn(right, mintId)],
  };
}

/* ─────────────────────────── THE CHOSEN DISPOSITION — SHAPES ───────────────────────────
 *
 * The tiling of a COMMAND group used to be DEDUCED from the member count and nothing else: two side
 * by side, three in an L, four in a quadrant. There was no "four in a column" and no "one big plus
 * three small", and the user's verdict on the whole layout system was that it had to be redone
 * (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding 01M11XTRXVVPZ9XMDW5ARWKWHA).
 *
 * The shape is now CHOSEN, out of a CLOSED and NAMED vocabulary. Closed is what keeps the choice
 * symmetric between the hand and the agent (finding #90): the agent sends `shape:"main-left"`, the
 * hand clicks the same thumbnail, and there is no free-form tree to reconcile — the pane tree is
 * still DERIVED, now from (ordered members + one label) instead of (ordered members) alone.
 */

/**
 * THE SIX SHAPES a command group can be poured into — the renderer's half of the ONE vocabulary,
 * mirrored by `nyx_core::db::COMMAND_GROUP_SHAPES`, by the SQL `CHECK` of the
 * `20260827120000_command_groups_layout_shape` migration and by the MCP `shape` action's enum.
 *
 * `auto` is FIRST and is the default: it is exactly {@link autoLayoutTree}, so every group that
 * existed before the shapes were opened keeps the disposition it already had.
 */
export const COMMAND_GROUP_SHAPES = [
  "auto",
  "row",
  "column",
  "grid",
  "main-left",
  "main-top",
] as const;

/** One of {@link COMMAND_GROUP_SHAPES}. */
export type CommandGroupShape = (typeof COMMAND_GROUP_SHAPES)[number];

/** The shape a group has until someone chooses another — the behaviour of before. */
export const DEFAULT_COMMAND_GROUP_SHAPE: CommandGroupShape = "auto";

/** Narrow an untrusted value (a DB row, an IPC payload) to a shape, falling back to the default. */
export function asCommandGroupShape(value: unknown): CommandGroupShape {
  return COMMAND_GROUP_SHAPES.includes(value as CommandGroupShape)
    ? (value as CommandGroupShape)
    : DEFAULT_COMMAND_GROUP_SHAPE;
}

/**
 * The share the BIG pane takes in `main-left` / `main-top` — the "1 big + N small" the user named
 * as missing. Not `DEFAULT_SPLIT_RATIO`: at 0.5 the shape would be indistinguishable from `row` /
 * `column` on two members, and "main" would mean nothing.
 */
const MAIN_PANE_RATIO = 0.6;

/**
 * THE TREE OF AN ORDERED MEMBER LIST UNDER A CHOSEN SHAPE — one table, six entries.
 *
 * The list ORDERS (which member goes where), the shape DISPOSES (what the grid looks like); the two
 * are separate edits because a re-order and a re-shape are separate intents. Every named shape lays
 * its bands out at `1/n` through {@link stackAlong}, so a column of three really is thirds.
 *
 * Fewer than two ids yields `null` — that is not a group. `mintId` is injectable so a test can pin
 * the exact rectangles against stable ids, exactly as {@link autoLayoutTree} allows.
 */
export function shapeTree(
  terminalIds: readonly string[],
  shape: CommandGroupShape = DEFAULT_COMMAND_GROUP_SHAPE,
  mintId: (prefix: string) => string = (prefix) =>
    prefix === "pane" ? newPaneId() : `split-${Math.random().toString(36).slice(2)}`,
): PaneNode | null {
  if (terminalIds.length < 2) return null;
  if (shape === "auto") return autoLayoutTree(terminalIds, mintId);
  const leaves = terminalIds.map(
    (terminalId): PaneNode => ({ kind: "leaf", id: mintId("pane"), terminalId }),
  );
  switch (shape) {
    case "row":
      return stackAlong("row", leaves, mintId);
    case "column":
      return stackAlong("column", leaves, mintId);
    case "grid": {
      // Two columns filled ROW-MAJOR — the quadrant at four, generalised, and with EQUAL bands.
      const left = leaves.filter((_, index) => index % 2 === 0);
      const right = leaves.filter((_, index) => index % 2 === 1);
      if (right.length === 0) return stackAlong("column", left, mintId);
      return {
        kind: "split",
        id: mintId("split"),
        direction: "row",
        ratio: DEFAULT_SPLIT_RATIO,
        children: [stackAlong("column", left, mintId), stackAlong("column", right, mintId)],
      };
    }
    case "main-left":
    case "main-top": {
      // The FIRST member is the big one — which is why the ordered list still matters under a
      // "1 + N" shape: dragging a member to the top promotes it.
      const [main, ...rest] = leaves;
      const along: SplitDirection = shape === "main-left" ? "column" : "row";
      return {
        kind: "split",
        id: mintId("split"),
        direction: shape === "main-left" ? "row" : "column",
        ratio: MAIN_PANE_RATIO,
        children: [main, stackAlong(along, rest, mintId)],
      };
    }
  }
}
