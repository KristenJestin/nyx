/**
 * `composition-nodes` — flatten a variant's `composition_json` into its NODES, mirroring the backend
 * `nyx_core::render::composition_nodes` node_key convention EXACTLY so a comment the UI anchors
 * (`proto_comment_add({ nodeKey })`) uses the same `Root/Header/…` path the backend stores and
 * re-anchors against across rounds (`anchor_status`, T28). The `node_key` PATH is the `/`-join of
 * each node's SEGMENT label from the root; a segment label is the node's explicit `key`, else its
 * `component` name, else `"inline"` (`CompositionNode::key`). A malformed composition yields an
 * EMPTY list (never a throw) — a picker source must not break the tab.
 */

/** A flattened composition node: its `node_key` PATH + the hint the picker labels it with. */
export interface CompositionNodeRef {
  /** The `/`-joined path key — the anchor stored on `prototype_feedback.node_key`. */
  readonly nodeKey: string;
  /** The catalogue component this node instantiates (`null` for an inline / structural node). */
  readonly component: string | null;
  /** Whether the node is an `[inline]` one-off (bespoke markup). */
  readonly inline: boolean;
  /**
   * The KEYS the node carries (its `params` object), mirroring the backend `CompositionNodeRef`.
   * They are what [`../design/composition-contract`] CONFRONTS with the params and axes the
   * referenced component declares (finding `01KYQ0R9414Q5K63TBV9CCMC9J`): a key the component
   * declares nowhere is a value nothing substitutes, and until that check existed the only symptom
   * was a node that rendered EMPTY.
   */
  readonly params: Readonly<Record<string, unknown>>;
}

/** The raw wire node — only the fields the node_key convention reads (parsing is lenient). */
export interface RawCompositionNode {
  readonly component?: unknown;
  readonly inline?: unknown;
  readonly key?: unknown;
  readonly params?: unknown;
  readonly children?: unknown;
  readonly [extra: string]: unknown;
}

type RawNode = RawCompositionNode;

/**
 * The character a `node_key` PATH joins its segments with. Stated once, because a path is READ back
 * apart from being written: the tree derives a node's depth and its displayed label from it.
 */
export const NODE_KEY_SEPARATOR = "/";

/** The segment label of a node that declares neither a `key` nor a `component` (backend parity). */
export const INLINE_SEGMENT_LABEL = "inline";

/**
 * A node's SEGMENT label: explicit `key`, else `component` name, else [`INLINE_SEGMENT_LABEL`].
 *
 * THE ONE IMPLEMENTATION IN THIS LANGUAGE (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 2). This
 * function was written twice, byte for byte, in two files — here and in the module that asked the
 * sidecar to bracket the very same nodes with comment markers (gone since the RENDER stamps each
 * node's key itself). A rule copied is a rule that drifts, and this one decides where a stored
 * comment RE-ANCHORS and which node a params write LANDS ON: both would have gone wrong silently.
 * `contract/design-fixtures.json` (`node_key`) pins it against the Rust `CompositionNode::key`.
 */
export function nodeSegmentLabel(node: RawCompositionNode): string {
  if (typeof node.key === "string" && node.key.length > 0) return node.key;
  if (typeof node.component === "string" && node.component.length > 0) return node.component;
  return INLINE_SEGMENT_LABEL;
}

/** A child's `node_key`, from its parent's path — the ONE place the separator is written. */
function childNodeKey(parentKey: string, child: RawCompositionNode): string {
  return `${parentKey}${NODE_KEY_SEPARATOR}${nodeSegmentLabel(child)}`;
}

/**
 * The SEGMENTS of a `node_key` path. The convention does NOT escape the separator, so a segment
 * that carries one really does split — read as a fact, never repaired here (the fixture pins it).
 */
export function nodeKeySegments(nodeKey: string): string[] {
  return nodeKey.split(NODE_KEY_SEPARATOR);
}

/** The LAST segment — the label the tree shows for a node. */
export function nodeKeyLeaf(nodeKey: string): string {
  const segments = nodeKeySegments(nodeKey);
  return segments[segments.length - 1] ?? nodeKey;
}

/** How deep a node sits: the root is 0. */
export function nodeKeyDepth(nodeKey: string): number {
  return Math.max(0, nodeKeySegments(nodeKey).length - 1);
}

/** A node's `params` object, or an empty one (an absent/malformed blob carries no key). */
function nodeParams(node: RawNode): Readonly<Record<string, unknown>> {
  const raw = node.params;
  return raw != null && typeof raw === "object" && !Array.isArray(raw)
    ? (raw as Record<string, unknown>)
    : {};
}

/**
 * FLATTEN a `composition_json` tree into every node with its `node_key` PATH (depth-first, root
 * first). Returns `[]` for a null/empty/malformed composition.
 */
export function compositionNodes(compositionJson: string | null | undefined): CompositionNodeRef[] {
  if (compositionJson == null || compositionJson.length === 0) return [];
  let root: RawNode;
  try {
    const parsed: unknown = JSON.parse(compositionJson);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return [];
    root = parsed as RawNode;
  } catch {
    return [];
  }

  const out: CompositionNodeRef[] = [];
  const walk = (node: RawNode, prefix: string): void => {
    out.push({
      nodeKey: prefix,
      component: typeof node.component === "string" ? node.component : null,
      inline: typeof node.inline === "string",
      params: nodeParams(node),
    });
    const children = Array.isArray(node.children) ? (node.children as RawNode[]) : [];
    for (const child of children) {
      if (child !== null && typeof child === "object") {
        walk(child, childNodeKey(prefix, child));
      }
    }
  };
  walk(root, nodeSegmentLabel(root));
  return out;
}
