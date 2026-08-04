import { describe, expect, it } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import { markCompositionNodes, NODE_MARK_OPEN, rootNodeKey } from "./composition-markers";
import {
  compositionNodes,
  INLINE_SEGMENT_LABEL,
  NODE_KEY_SEPARATOR,
  nodeKeyDepth,
  nodeKeyLeaf,
  nodeKeySegments,
  nodeSegmentLabel,
} from "./composition-nodes";

/**
 * T28 — the node_key flattener MUST mirror the backend `nyx_core::render::composition_nodes`
 * convention (`Root/Header/…`, each segment = explicit `key` else `component` else `inline`) so a
 * comment the UI anchors uses the same path the backend stores/re-anchors against.
 */
describe("compositionNodes", () => {
  it("flattens the tree into `/`-joined node_key paths (key ?? component ?? inline)", () => {
    const json = JSON.stringify({
      component: "Card",
      key: "Root",
      params: { title: "Facturation" },
      children: [
        { component: "Button", key: "Save", params: { label: "Enregistrer" } },
        { component: "Button", key: "Cancel", params: { label: "Annuler" } },
        // No `key` → the segment falls back to the component name.
        { component: "PricingTable", params: { plan: "pro" } },
        // Neither key nor component → the segment is "inline".
        { inline: "<p>{{note}}</p>", params: { note: "brouillon" } },
      ],
    });

    const nodes = compositionNodes(json);
    expect(nodes.map((n) => n.nodeKey)).toEqual([
      "Root",
      "Root/Save",
      "Root/Cancel",
      "Root/PricingTable",
      "Root/inline",
    ]);

    // The nature hints ride along for the picker label.
    expect(nodes[0]).toMatchObject({ nodeKey: "Root", component: "Card", inline: false });
    expect(nodes[3]).toMatchObject({ nodeKey: "Root/PricingTable", component: "PricingTable" });
    expect(nodes[4]).toMatchObject({ nodeKey: "Root/inline", component: null, inline: true });
  });

  /**
   * The node's KEYS ride along too (finding `01KYQ0R9414Q5K63TBV9CCMC9J`) — they are what the node
   * contract confronts with the component's declared params and axes. A node with no `params` (or a
   * malformed one) carries no key rather than breaking the walk.
   */
  it("carries each node's params, and an empty map when it declares none", () => {
    const json = JSON.stringify({
      key: "Root",
      children: [
        { component: "Button", key: "Save", params: { label: "Enregistrer" } },
        { component: "Button", key: "Bare" },
        { component: "Button", key: "Broken", params: ["nope"] },
      ],
    });
    const nodes = compositionNodes(json);
    expect(nodes[1]?.params).toEqual({ label: "Enregistrer" });
    expect(nodes[2]?.params).toEqual({});
    expect(nodes[3]?.params).toEqual({});
  });

  it("recurses into nested children (deep node_key path)", () => {
    const json = JSON.stringify({
      key: "Root",
      children: [{ key: "Header", children: [{ component: "Title" }] }],
    });
    expect(compositionNodes(json).map((n) => n.nodeKey)).toEqual([
      "Root",
      "Root/Header",
      "Root/Header/Title",
    ]);
  });

  it("returns an empty list for null / empty / malformed compositions (never throws)", () => {
    expect(compositionNodes(null)).toEqual([]);
    expect(compositionNodes(undefined)).toEqual([]);
    expect(compositionNodes("")).toEqual([]);
    expect(compositionNodes("not json")).toEqual([]);
    expect(compositionNodes("[]")).toEqual([]);
    expect(compositionNodes("42")).toEqual([]);
  });
});

/**
 * THE ARRIMAGE of the `node_key` CONVENTION (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 2).
 *
 * ONE RULE, written four times: the Rust `CompositionNode::key` + `composition_nodes` +
 * `node_key_path`, and — in this language — a `segmentLabel` duplicated byte for byte in
 * `composition-markers.ts` plus two more derivations of the separator in `composition-tree.tsx`.
 * There is one implementation here now, and `contract/design-fixtures.json` (`node_key`) is what
 * holds it to the backend's: the same cases run through
 * `render.rs::tests::the_shared_fixture_pins_the_node_key_convention`.
 *
 * It matters more than its size suggests. This path is what a pinned comment is STORED on and
 * RE-ANCHORED by across rounds, and what a params write ADDRESSES. Move one segment on one side and
 * the comments stop finding their node while the writes land on the wrong one — silently, both.
 */
describe("the shared Rust ⟷ TS fixture — the node_key convention", () => {
  const fixture = designFixtures.node_key;

  it("joins segments with the pinned separator and falls back to the pinned inline label", () => {
    expect(NODE_KEY_SEPARATOR).toBe(fixture.separator);
    expect(INLINE_SEGMENT_LABEL).toBe(fixture.inline_label);
    expect(nodeSegmentLabel({ key: "Header", component: "Card" })).toBe("Header");
    expect(nodeSegmentLabel({ component: "Card" })).toBe("Card");
    expect(nodeSegmentLabel({ inline: "<hr/>" })).toBe(fixture.inline_label);
    // An EMPTY string is not a label: it would produce `Root//Leaf`, a path nothing can address.
    expect(nodeSegmentLabel({ key: "", component: "Card" })).toBe("Card");
    expect(nodeSegmentLabel({ key: "", component: "" })).toBe(fixture.inline_label);
  });

  it.each(fixture.cases.map((c) => [c.id, c] as const))(
    "replays `%s` exactly as the backend resolves it",
    (_id, testCase) => {
      const json = JSON.stringify(testCase.composition);
      expect(
        compositionNodes(json).map((n) => n.nodeKey),
        testCase.why,
      ).toEqual(testCase.node_keys);
      // The MARKERS bracket the very same paths — that is what makes a click in the frame resolve
      // to the node a comment is stored on. Every non-root key must appear as a marker body.
      const marked = markCompositionNodes(json) ?? "";
      for (const nodeKey of testCase.node_keys.slice(1)) {
        const encoded = encodeURIComponent(nodeKey).replace(/-/g, "%2D");
        expect(marked, `${testCase.id}: ${nodeKey} is bracketed`).toContain(
          `<!--${NODE_MARK_OPEN}${encoded}-->`,
        );
      }
      expect(rootNodeKey(json)).toBe(testCase.node_keys[0]);
    },
  );

  it("reads a path back the way the tree displays it — depth and leaf label", () => {
    // The separator is NOT escaped, and that is the convention, not an oversight: a segment
    // carrying one really does split. Pinned so no implementation invents an escaping the other
    // side does not perform.
    const split = fixture.cases.find((c) => c.id.startsWith("the-separator-is-not-escaped"));
    expect(split).toBeTruthy();
    const deepest = split!.node_keys[split!.node_keys.length - 1];
    expect(nodeKeySegments(deepest)).toEqual(deepest.split(fixture.separator));
    expect(nodeKeyLeaf(deepest)).toBe("Leaf");
    expect(nodeKeyDepth(deepest)).toBe(nodeKeySegments(deepest).length - 1);
    expect(nodeKeyDepth(fixture.cases[0]!.node_keys[0]!)).toBe(0);
  });
});
