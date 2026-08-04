/**
 * `composition-markers` — make the RENDER designable: give every node of a composition a trace in
 * the markup, so a click inside the frame resolves to a `node_key` of the NAMED TREE.
 *
 * # Why this exists
 *
 * The prototype page's whole gesture is "the render IS the composition" (finding
 * `01KYW32N1BVDVTGS0WPMG2PJW2`): clicking a component in the page opens its params, and a pinned
 * comment anchors on the element one clicked. Depot does the same thing by computing a CSS SELECTOR
 * in the frame (`src/web/api/prototype-shim.ts` → `computeSelector`) — a homegrown walk up the tree
 * until `querySelectorAll(...).length === 1`. **nyx must not do that.** Here the tree IS the
 * contract: a comment is stored on `prototype_feedback.node_key`, it is RE-ANCHORED across rounds by
 * that key, and the coder seam reads the same `Root/Header/Title` paths. A selector would be a
 * SECOND identity for the same thing — one that breaks the moment a class changes, which is the one
 * thing a design round does constantly.
 *
 * # The problem it solves
 *
 * `design_render` answers ONE markup string. A component's rendered template goes in verbatim and a
 * node's children are injected into it as `{{{children}}}`, so nothing in the result says where a
 * node begins and ends: the only node names in the markup are the ones on the PLACEHOLDERS of the
 * nodes that could NOT be rendered (`data-unrendered-node`). Everything that renders is anonymous.
 *
 * So the renderer asks for the SAME composition with an inert MARKER between siblings: each child is
 * bracketed by two `[inline]` nodes whose markup is an HTML COMMENT naming it. The sidecar renders
 * them like any other inline node, in place, and the frame reads them back
 * ({@link PROTO_SHIM_SOURCE}, `indexNodes`) to attribute every element to the deepest node that
 * encloses it.
 *
 * # Why a COMMENT, and why between siblings
 *
 *  - A comment is the only thing that is legal EVERYWHERE the markup can put a child — inside a
 *    `<ul>`, a `<table>`, a `<select>` — and that the HTML parser never hoists elsewhere. A
 *    `<template>` or a `<span>` marker would be an ELEMENT: it would shift `:first-child`,
 *    `:nth-child`, `+`/`~` and `> *` under the page's own stylesheet. A comment shifts nothing, is
 *    never laid out, and is invisible to CSS.
 *  - BETWEEN siblings, because that is the only place that delimits a node's markup exactly. Inside
 *    the node (as a child) it would be dropped for every LEAF component — a `Button` template has no
 *    `{{{children}}}` — and leaves are precisely what one clicks.
 *
 * The node_key convention is untouched: a `node_key` is the `/`-join of the SEGMENT labels down the
 * tree, and inserting siblings changes no segment. The markers only add keys of their own
 * (`…/__nyx-mark`), which nothing addresses. `unrendered[]` therefore comes back naming the same
 * nodes it always did.
 *
 * # This is a SEAM, and it is meant to be deleted
 *
 * The right place for a node's name in the markup is the sidecar, next to the walk that knows it —
 * exactly where `data-nyx-css` is stamped. The day the render stamps the node key itself, this
 * module and its half of the shim go away and the frame reads the attribute instead. Until then the
 * renderer states its need in the composition it sends, rather than guessing at a selector.
 */

import { childNodeKey, nodeSegmentLabel, type RawCompositionNode } from "./composition-nodes";

/**
 * The comment body that OPENS a node's region: `<!--nyx-node:<encoded key>-->`.
 *
 * THE SHIM RE-TYPED THIS LITERAL (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 4). It cannot import
 * it — `PROTO_SHIM_SOURCE` is a STRING evaluated inside a sandboxed frame — so the arrimage is a
 * SEAM TEST instead: `shim-seam.test.ts` builds its DOM from THIS module's own output and runs the
 * shim source over it, so renaming either half stops the designation gesture and fails.
 */
export const NODE_MARK_OPEN = "nyx-node:";
/** The comment body that CLOSES it: `<!--/nyx-node:<encoded key>-->`. */
export const NODE_MARK_CLOSE = "/nyx-node:";

/**
 * The key, encoded so it can never terminate the comment it travels in. `encodeURIComponent` covers
 * `<`, `>` and the `/` of the path; the extra `-` escape is what makes `-->` unwritable, so a node
 * whose author named it `a--b>` cannot cut the marker in half. The shim decodes with
 * `decodeURIComponent`, which reads `%2D` back as `-`.
 */
function encodeKey(nodeKey: string): string {
  return encodeURIComponent(nodeKey).replace(/-/g, "%2D");
}

/**
 * The raw wire node, and the node_key convention itself, are IMPORTED — never re-implemented. This
 * module used to carry its own byte-for-byte copy of `segmentLabel` (finding
 * `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 2), which is how the rule deciding where a stored comment
 * re-anchors would have drifted from the rule deciding which node a params write lands on.
 */
type RawNode = RawCompositionNode;

/** One marker node: an `[inline]` one-off whose whole markup is an HTML comment. */
function marker(body: string): RawNode {
  return { key: "__nyx-mark", inline: `<!--${body}-->` };
}

/**
 * The same composition, with every node BRACKETED by two comment markers naming its `node_key`.
 *
 * Pure and total: a `null`, empty or malformed composition is handed back UNCHANGED — the marking is
 * an affordance, and a page must render with or without it. The ROOT node gets no bracket (it has no
 * parent to hold one); the frame treats `#depot-root` as its region and is told the root's key
 * separately.
 */
export function markCompositionNodes(compositionJson: string | null | undefined): string | null {
  if (compositionJson == null || compositionJson.length === 0) return compositionJson ?? null;
  let root: RawNode;
  try {
    const parsed: unknown = JSON.parse(compositionJson);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
      return compositionJson;
    }
    root = parsed as RawNode;
  } catch {
    return compositionJson;
  }

  const walk = (node: RawNode, nodeKey: string): RawNode => {
    const children = Array.isArray(node.children) ? (node.children as unknown[]) : null;
    if (children === null || children.length === 0) return node;
    const marked: unknown[] = [];
    for (const raw of children) {
      if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
        // A child this convention cannot NAME is still a child: it is carried over UNMARKED rather
        // than dropped. Dropping it made the marked composition render a page the unmarked one
        // refuses — the stage would have shown a tree with a node silently missing from it.
        marked.push(raw);
        continue;
      }
      const child = raw as RawNode;
      const childKey = childNodeKey(nodeKey, child);
      const encoded = encodeKey(childKey);
      marked.push(marker(`${NODE_MARK_OPEN}${encoded}`));
      marked.push(walk(child, childKey));
      marked.push(marker(`${NODE_MARK_CLOSE}${encoded}`));
    }
    return { ...node, children: marked };
  };

  return JSON.stringify(walk(root, nodeSegmentLabel(root)));
}

/** The ROOT node's `node_key` — the region `#depot-root` itself stands for. `null` if unreadable. */
export function rootNodeKey(compositionJson: string | null | undefined): string | null {
  if (compositionJson == null || compositionJson.length === 0) return null;
  try {
    const parsed: unknown = JSON.parse(compositionJson);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return nodeSegmentLabel(parsed as RawNode);
  } catch {
    return null;
  }
}
