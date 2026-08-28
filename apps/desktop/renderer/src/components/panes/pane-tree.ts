/**
 * THE PANE TREE — the pure, deterministic model behind the Terminals page's TILING.
 *
 * A layout is a binary tree: every LEAF is one pane showing exactly one terminal
 * (`{ id: pane_id, terminalId: terminal_id }`), and every SPLIT has EXACTLY TWO children laid out
 * side by side (`row`) or stacked (`column`) with a `ratio` giving the FIRST child's share of the
 * axis, bounded to {@link MIN_STORED_RATIO}..{@link MAX_STORED_RATIO} so neither side can collapse
 * to nothing. The narrower AESTHETIC band ({@link MIN_SPLIT_RATIO}..{@link MAX_SPLIT_RATIO}) is
 * enforced one layer up, by `separatorRatioBounds`, which is the only place that knows the PIXELS
 * that may legitimately override it.
 *
 * INVARIANTS (enforced by every mutator here, re-checked by {@link isPaneNode} at restore):
 *  - a split has exactly two children — no n-ary nodes, no single-child split;
 *  - a `terminalId` appears AT MOST ONCE in the whole tree (one terminal = one pane). An agent is
 *    NOT a leaf kind: it is a property of the terminal session hosted by the leaf's terminal;
 *  - every `id` (pane and split alike) is unique in the tree;
 *  - a ratio is always finite and clamped.
 *
 * The model is PURE: mutators return a NEW tree and never touch the terminal store, so closing a
 * pane compacts the layout WITHOUT closing the terminal (its PTY, xterm buffer and DB record are
 * untouched — {@link closePane}). The reverse direction, a terminal that truly disappears, is
 * {@link pruneStaleTerminals}: the leaf is dropped and its parent compacted.
 *
 * COMPACTION is the single structural rule that keeps the tree canonical: when a leaf is removed,
 * its parent split is REPLACED by the surviving sibling subtree. So a tree never holds a split
 * with a hole, and a tree of N leaves always has exactly N-1 splits.
 */

/** Smallest share of the axis a split's FIRST child may take (15%) — the AESTHETIC band. */
export const MIN_SPLIT_RATIO = 0.15;
/** Largest share of the axis a split's FIRST child may take (85%) — the AESTHETIC band. */
export const MAX_SPLIT_RATIO = 0.85;
/** A fresh split cuts the pane in half. */
export const DEFAULT_SPLIT_RATIO = 0.5;

/* ─────────────────── THE BAND A RATIO IS *STORED* IN — wider than 15..85 ───────────────────
 *
 * USER ARBITRAGE (review round 2): when a sub-box is too narrow for the aesthetic 15..85 band to
 * keep BOTH sides above the ABSOLUTE PX floor while the box still HAS the room, the px floor WINS
 * and the seam legitimately parks OUTSIDE 15..85. 15..85 is what it always was — a heuristic of
 * proportion; 180×120px is the real usability constraint.
 *
 * This model has no pixels, so it cannot re-derive that band: `separatorRatioBounds` does, and
 * EVERY drag and EVERY key is clamped through it before reaching {@link setSplitRatio}. What is
 * enforced here is only what holds WITHOUT a host — a split keeps both of its sides on screen.
 *
 * WHERE THE TWO BANDS MEET. The 2% end sits under the px-floor demand for every sub-box SMALLER
 * than `MIN_PANE_HEIGHT_PX / 0.02` = 6000 CSS px tall (or `MIN_PANE_WIDTH_PX / 0.02` = 9000px
 * wide): under that size the floor asks for MORE than 2%, so it is always the tighter of the two
 * and this band is never consulted. BEYOND it the statement flips, and it really is reachable —
 * three 3840px monitors side by side make an 11520px row, and a 9001px sub-box whose first side
 * demands 8820.98px yields a px band of [0.98000000000000018, 0.98000222], ENTIRELY above 98%.
 * There the two bands genuinely fight, and THIS one wins: a ratio outside it cannot be read back,
 * so storing it would cost the user the whole tiling at the next launch (review round 4, major).
 * {@link clampRatio} is where that arbitration is made, and every ratio writer — the mutators here
 * AND `compactPaneRatios` — ends there.
 */

/** Smallest share of the axis a STORED ratio may hold. */
export const MIN_STORED_RATIO = 0.02;
/** Largest share of the axis a STORED ratio may hold. */
export const MAX_STORED_RATIO = 0.98;

/** `row` = children side by side (a vertical separator); `column` = stacked (horizontal one). */
export type SplitDirection = "row" | "column";

/**
 * The FOUR EDGES of a TARGET pane a dragged pane can be dropped on (the VS Code motif): the drop
 * divides the target's container perpendicular to the edge and inserts the moved leaf as its
 * NEIGHBOUR. `left`/`right` map to a `row` split, `up`/`down` to a `column` one.
 */
export type PaneEdge = "left" | "right" | "up" | "down";

/** A PANE: the leaf of the layout, showing exactly one terminal. */
export interface PaneLeaf {
  readonly kind: "leaf";
  /** The pane id — stable identity of the SLOT (survives ratio changes and sibling splits). */
  readonly id: string;
  /** The terminal shown in this pane. Unique across the whole tree. */
  readonly terminalId: string;
}

/** A SPLIT: exactly two children along `direction`, `ratio` being the FIRST child's share. */
export interface PaneSplit {
  readonly kind: "split";
  /** The split id — what a separator drag addresses to change `ratio`. */
  readonly id: string;
  readonly direction: SplitDirection;
  /** First child's share of the axis, always within [MIN_STORED_RATIO, MAX_STORED_RATIO]. */
  readonly ratio: number;
  readonly children: readonly [PaneNode, PaneNode];
}

/** Any node of the layout tree. */
export type PaneNode = PaneLeaf | PaneSplit;

/**
 * Clamp a ratio into the STORABLE band ({@link MIN_STORED_RATIO}..{@link MAX_STORED_RATIO}); a
 * non-finite value falls back to {@link DEFAULT_SPLIT_RATIO}. The AESTHETIC 15..85 band is NOT
 * applied here — it is `separatorRatioBounds`' business, because only it knows the pixels that may
 * override it (see the note above the two bands).
 */
export function clampRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return DEFAULT_SPLIT_RATIO;
  return Math.min(MAX_STORED_RATIO, Math.max(MIN_STORED_RATIO, ratio));
}

/** A fresh, collision-free node id (`pane-…` / `split-…`). */
function newId(prefix: string): string {
  const rand =
    globalThis.crypto?.randomUUID?.().replace(/-/g, "") ?? Math.random().toString(36).slice(2);
  return `${prefix}-${rand}`;
}

/** A fresh PANE id. Exported so a caller can pre-allocate the id it will focus. */
export function newPaneId(): string {
  return newId("pane");
}

/** A single-pane layout showing `terminalId` — the default tree (and the restore floor). */
export function createPaneLayout(terminalId: string, paneId: string = newPaneId()): PaneLeaf {
  return { kind: "leaf", id: paneId, terminalId };
}

/** Every leaf of `node`, left-to-right / top-to-bottom (the visual reading order). */
export function paneLeaves(node: PaneNode): PaneLeaf[] {
  if (node.kind === "leaf") return [node];
  return [...paneLeaves(node.children[0]), ...paneLeaves(node.children[1])];
}

/**
 * Every terminal id VISIBLE in the tree, in leaf order — the tiling's live set (what the page
 * hands to the idle-unload sweep). `null` (no layout yet) has nothing on screen: `[]`.
 */
export function paneTerminalIds(node: PaneNode | null): string[] {
  return node == null ? [] : paneLeaves(node).map((leaf) => leaf.terminalId);
}

/** The leaf with `paneId`, or `null`. */
export function findPane(node: PaneNode, paneId: string): PaneLeaf | null {
  return paneLeaves(node).find((leaf) => leaf.id === paneId) ?? null;
}

/** The leaf showing `terminalId`, or `null`. */
export function findPaneByTerminal(node: PaneNode, terminalId: string): PaneLeaf | null {
  return paneLeaves(node).find((leaf) => leaf.terminalId === terminalId) ?? null;
}

/** Every split of `node`, outermost first. */
function paneSplits(node: PaneNode): PaneSplit[] {
  if (node.kind === "leaf") return [];
  return [node, ...paneSplits(node.children[0]), ...paneSplits(node.children[1])];
}

/**
 * SPLIT a pane: replace the leaf `paneId` by a `direction` split holding the ORIGINAL leaf first
 * and a NEW leaf (showing `terminalId`) second — so "split right" puts the new pane on the right
 * and "split down" puts it below.
 *
 * Returns `null` — leaving the caller's tree untouched — when the split is IMPOSSIBLE: unknown
 * `paneId`, or `terminalId` already visible somewhere in the tree (the uniqueness invariant).
 * The caller creates the terminal FIRST and only then commits the returned tree, so a failed
 * creation never leaves an empty pane behind.
 */
export function splitPane(
  root: PaneNode,
  options: {
    readonly paneId: string;
    readonly direction: SplitDirection;
    readonly terminalId: string;
    /** Pre-allocated id for the new pane (so the caller can focus it); generated otherwise. */
    readonly newPaneId?: string;
    readonly ratio?: number;
  },
): PaneNode | null {
  if (findPane(root, options.paneId) == null) return null;
  if (findPaneByTerminal(root, options.terminalId) != null) return null;

  const leaf = createPaneLayout(options.terminalId, options.newPaneId ?? newPaneId());
  const replace = (node: PaneNode): PaneNode => {
    if (node.kind === "leaf") {
      if (node.id !== options.paneId) return node;
      return {
        kind: "split",
        id: newId("split"),
        direction: options.direction,
        ratio: clampRatio(options.ratio ?? DEFAULT_SPLIT_RATIO),
        children: [node, leaf],
      };
    }
    const first = replace(node.children[0]);
    const second = replace(node.children[1]);
    if (first === node.children[0] && second === node.children[1]) return node;
    return { ...node, children: [first, second] };
  };
  return replace(root);
}

/**
 * MOVE a pane by dropping it on an EDGE of another pane (the view's restructure gesture, review
 * 01M0CY7R5Z7KV5R49FQYXH9QRN): the leaf `draggedPaneId` is DETACHED (the audited compaction) and
 * RE-INSERTED as a NEIGHBOUR of `targetPaneId`, splitting the target's box perpendicular to `edge`
 * at {@link DEFAULT_SPLIT_RATIO} (50/50). `left`/`up` put the moved leaf FIRST, `right`/`down`
 * SECOND — "drop on the left edge" lands on the left.
 *
 * THE TERMINAL IS NEVER RECREATED: this is a pure tree edit. The moved leaf keeps its very object
 * — same pane id, same terminalId — so the terminal-store instance, its PTY and its single DOM
 * attachment all survive the gesture; only WHERE the leaf sits changes.
 *
 * Returns `null` — leaving `root` untouched — when the move is impossible: unknown pane id, the
 * dragged and the target being the same pane, or a one-leaf root (nothing to restructure, and no
 * second pane to drop onto).
 *
 * The FLOOR is NOT checked here — geometry lives in `pane-geometry`, and the caller judges the
 * CANDIDATE tree this returns (`layoutFitsFloor`) BEFORE committing, so an invalid drop can be
 * refused during the hover, indicator absent, exactly like the sidebar insertion.
 */
export function movePaneToEdge(
  root: PaneNode,
  draggedPaneId: string,
  targetPaneId: string,
  edge: PaneEdge,
): PaneNode | null {
  if (draggedPaneId === targetPaneId) return null;
  const dragged = findPane(root, draggedPaneId);
  if (dragged == null || findPane(root, targetPaneId) == null) return null;
  // Detach FIRST, then insert next to the target. Compaction only ever REPLACES split nodes —
  // every leaf keeps its id — so the target lookup below still resolves after the detach.
  const detached = root.kind === "leaf" ? null : dropLeaf(root, (l) => l.id === draggedPaneId);
  if (detached == null) return null;
  const direction: SplitDirection = edge === "left" || edge === "right" ? "row" : "column";
  const first = edge === "left" || edge === "up";
  const insert = (node: PaneNode): PaneNode => {
    if (node.kind === "leaf") {
      if (node.id !== targetPaneId) return node;
      return {
        kind: "split",
        id: newId("split"),
        direction,
        ratio: clampRatio(DEFAULT_SPLIT_RATIO),
        children: first ? [dragged, node] : [node, dragged],
      };
    }
    const newFirst = insert(node.children[0]);
    const newSecond = insert(node.children[1]);
    if (newFirst === node.children[0] && newSecond === node.children[1]) return node;
    return { ...node, children: [newFirst, newSecond] };
  };
  return insert(detached);
}

/**
 * The tiling's SHAPE, with the occupants (and the identities) rubbed out: nesting + split
 * DIRECTIONS only. Two trees with the same shape draw the same grid of rectangles in the same
 * reading order — which is precisely the question {@link paneSwapOf} has to answer.
 *
 * NODE IDS are deliberately out: {@link movePaneToEdge} mints a fresh split id on every insertion,
 * so comparing ids would call every move a restructure. RATIOS are out too, and that is an
 * ARBITRAGE: a two-pane tiling the user has dragged to 30/70 comes back at 50/50 after the swap, so
 * the rectangles do change size — but WHO SITS WHERE is what the gesture is about and what the user
 * named ("quand c'est du swap"), and keying on ratios would silently drop the recognition for every
 * tiling that has ever been resized, i.e. the common one.
 */
function paneShape(node: PaneNode): string {
  return node.kind === "leaf"
    ? "·"
    : `(${node.direction[0]}${paneShape(node.children[0])}${paneShape(node.children[1])})`;
}

/**
 * The two terminals a candidate tree SWAPS, or `null` when it really restructures.
 *
 * WHY THIS EXISTS (review finding 01M11WFPFZGAC68VGXJSG6QNMK). The drop indicator speaks ONE
 * vocabulary — an edge band, i.e. "I am about to CUT here" — and it is a lie in the case the user
 * pointed at: with two panes, dropping A on the far edge of B detaches A, leaves B alone, and
 * re-inserts A on the other side. Nothing is cut; the two terminals EXCHANGE places. The indicator
 * has to recognise that and say something else.
 *
 * IT IS NOT A SPECIAL CASE ON THE PANE COUNT. `movePaneToEdge` is pure, so the caller runs it DRY
 * and hands both trees here: same SHAPE ({@link paneShape}) and exactly two leaves that traded
 * their terminals ⇒ permutation, and the pair is returned so the view can name BOTH panes. That is
 * exact by construction and stays exact wherever a nested split happens to produce the same
 * permutation — a `panes.length === 2` shortcut would be right today and wrong the moment it does.
 *
 * Anything else — a different shape, one leaf moved without a counterpart, more than one
 * transposition — is a real restructure and reads `null`.
 */
export function paneSwapOf(before: PaneNode, after: PaneNode): readonly [string, string] | null {
  if (paneShape(before) !== paneShape(after)) return null;
  const from = paneLeaves(before).map((leaf) => leaf.terminalId);
  const to = paneLeaves(after).map((leaf) => leaf.terminalId);
  const moved: number[] = [];
  for (let i = 0; i < from.length; i += 1) {
    if (from[i] !== to[i]) moved.push(i);
  }
  // A TRANSPOSITION, nothing looser: exactly two positions changed, and each took the other's
  // occupant. Three positions rotating is a re-ordering the "these two exchange" wording cannot
  // describe, so it falls back to the edge band rather than mis-naming what will happen.
  if (moved.length !== 2) return null;
  const [i, j] = moved;
  if (from[i] !== to[j] || from[j] !== to[i]) return null;
  return [from[i], from[j]];
}

/**
 * Drop the leaf matching `predicate` and COMPACT: its parent split is replaced by the surviving
 * sibling. Returns `null` when the whole tree is gone (the removed leaf was the root).
 */
function dropLeaf(node: PaneNode, predicate: (leaf: PaneLeaf) => boolean): PaneNode | null {
  if (node.kind === "leaf") return predicate(node) ? null : node;
  const first = dropLeaf(node.children[0], predicate);
  const second = dropLeaf(node.children[1], predicate);
  if (first == null && second == null) return null;
  // COMPACTION: a split that lost one child collapses onto the survivor — no orphan splits.
  if (first == null) return second;
  if (second == null) return first;
  if (first === node.children[0] && second === node.children[1]) return node;
  return { ...node, children: [first, second] };
}

/**
 * CLOSE A PANE (the user closing a tile): drop the leaf and compact its parent. This NEVER closes
 * the terminal — the instance, its PTY and its DB record stay alive (a closed pane is a layout
 * edit, and the terminal remains reachable from the sidebar).
 *
 * The LAST pane is KEPT: a layout always shows at least one terminal, so closing the only pane is
 * a no-op (the same tree is returned).
 */
export function closePane(root: PaneNode, paneId: string): PaneNode {
  if (root.kind === "leaf") return root;
  return dropLeaf(root, (leaf) => leaf.id === paneId) ?? root;
}

/**
 * Drop every leaf whose terminal is NO LONGER live and compact — the reconciliation against the
 * project's current terminal set, at restore AND on every snapshot push. This is the ONE way a
 * terminal that really disappeared (closed by the user, an agent over MCP, another window) loses
 * its pane: a single-terminal removal is that same call with the id left out of the live set.
 *
 * Unlike {@link closePane} it may EMPTY the tree — `null` means "no pane left", and the caller
 * falls back to a fresh single-pane layout.
 */
export function pruneStaleTerminals(
  root: PaneNode,
  liveTerminalIds: Iterable<string>,
): PaneNode | null {
  const live = new Set(liveTerminalIds);
  return dropLeaf(root, (leaf) => !live.has(leaf.terminalId));
}

/*
 * THE SUBSTITUTION MUTATOR IS GONE (review 01M0ABR6V501D0SPR2BYBVZC4A, critical).
 *
 * `setPaneTerminal(root, paneId, terminalId)` used to SWAP another terminal into an existing pane —
 * the sidebar path of v1, where picking a terminal that was not on screen replaced the focused
 * pane's one "instead of opening a picker or an empty tile". That is precisely what made the tiling
 * inescapable: once split, no selection could ever bring the user back to a single terminal filling
 * the frame, because every selection landed INSIDE the tiling.
 *
 * The model now answers that need structurally (`pane-groups.ts`): a terminal is in a group or it
 * is solo, and BOTH are showable — so nothing ever needs to be substituted into a pane. The mutator
 * is deleted rather than left unused: keeping it would leave the old behaviour one call away.
 */

/** Set a split's `ratio` (clamped). Unknown `splitId` returns the tree unchanged. */
export function setSplitRatio(root: PaneNode, splitId: string, ratio: number): PaneNode {
  if (root.kind === "leaf") return root;
  const next = clampRatio(ratio);
  const walk = (node: PaneNode): PaneNode => {
    if (node.kind === "leaf") return node;
    if (node.id === splitId) return node.ratio === next ? node : { ...node, ratio: next };
    const first = walk(node.children[0]);
    const second = walk(node.children[1]);
    if (first === node.children[0] && second === node.children[1]) return node;
    return { ...node, children: [first, second] };
  };
  return walk(root);
}

/**
 * The pane that should take the FOCUS once `paneId` is closed: the first leaf of its SIBLING
 * subtree (the tile that visually grows into the freed space), else the first remaining leaf.
 * `null` when `paneId` is the only pane (nothing else to focus).
 */
export function paneAfterClose(root: PaneNode, paneId: string): string | null {
  const sibling = (node: PaneNode): PaneNode | null => {
    if (node.kind === "leaf") return null;
    const [first, second] = node.children;
    if (first.kind === "leaf" && first.id === paneId) return second;
    if (second.kind === "leaf" && second.id === paneId) return first;
    return sibling(first) ?? sibling(second);
  };
  const target = sibling(root);
  if (target) return paneLeaves(target)[0]?.id ?? null;
  const remaining = paneLeaves(root).filter((leaf) => leaf.id !== paneId);
  return remaining[0]?.id ?? null;
}

/**
 * SHAPE VALIDATION of an untrusted (persisted) tree — the guard the restore runs before honouring
 * anything read back from `localStorage`. It checks the node shapes AND the tree-wide invariants
 * (unique ids, unique terminal ids, in-band ratios): a tree that violates any of them is REJECTED
 * wholesale rather than silently repaired, so a corrupted layout degrades to the default single
 * pane instead of producing a tree the mutators could not have built.
 */
export function isPaneNode(value: unknown): value is PaneNode {
  const ids = new Set<string>();
  const terminalIds = new Set<string>();

  /** The untrusted, field-by-field view of a parsed node (nothing is assumed about its shape). */
  interface RawNode {
    readonly kind?: unknown;
    readonly id?: unknown;
    readonly terminalId?: unknown;
    readonly direction?: unknown;
    readonly ratio?: unknown;
    readonly children?: unknown;
  }

  const walk = (node: unknown): boolean => {
    if (typeof node !== "object" || node === null) return false;
    const candidate = node as RawNode;
    if (typeof candidate.id !== "string" || candidate.id.length === 0) return false;
    if (ids.has(candidate.id)) return false;
    ids.add(candidate.id);

    if (candidate.kind === "leaf") {
      if (typeof candidate.terminalId !== "string" || candidate.terminalId.length === 0)
        return false;
      // One terminal = one pane: a duplicate makes the whole tree untrustworthy.
      if (terminalIds.has(candidate.terminalId)) return false;
      terminalIds.add(candidate.terminalId);
      return true;
    }
    if (candidate.kind !== "split") return false;
    if (candidate.direction !== "row" && candidate.direction !== "column") return false;
    if (typeof candidate.ratio !== "number" || !Number.isFinite(candidate.ratio)) return false;
    // The STORABLE band, not the aesthetic one: a layout persisted while the px floor was
    // overriding 15..85 (user arbitrage) is a tree the mutators really can produce, and rejecting
    // it here would throw the user's whole tiling away at the next restore.
    if (candidate.ratio < MIN_STORED_RATIO || candidate.ratio > MAX_STORED_RATIO) return false;
    if (!Array.isArray(candidate.children) || candidate.children.length !== 2) return false;
    return walk(candidate.children[0]) && walk(candidate.children[1]);
  };

  return walk(value);
}

/** The number of panes (leaves) in the tree — one visible terminal each. */
export function paneCount(node: PaneNode): number {
  return paneLeaves(node).length;
}

/** The number of splits — always `paneCount - 1` for a compacted tree (test seam). */
export function splitCount(node: PaneNode): number {
  return paneSplits(node).length;
}
