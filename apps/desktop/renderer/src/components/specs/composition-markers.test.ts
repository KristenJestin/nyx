import { describe, expect, it } from "vitest";

import { compositionNodes } from "./composition-nodes";
import { markCompositionNodes, rootNodeKey } from "./composition-markers";

/**
 * THE MARKERS THAT MAKE THE RENDER DESIGNABLE (finding `01KYW32N1BVDVTGS0WPMG2PJW2`).
 *
 * `design_render` answers ONE markup string in which nothing says where a node begins and ends, so
 * the renderer asks for the SAME composition with an inert HTML comment bracketing each node. What
 * these assert is the whole contract that makes that safe: the node_key CONVENTION is untouched (the
 * keys are exactly the ones `compositionNodes` — the mirror of the backend's own walk — computes),
 * the markers are comments and only comments, and a composition that cannot be read crosses
 * VERBATIM rather than being mangled on the way to the sidecar.
 */

/** Every marker body found in an annotated composition, in document order. */
function markers(annotated: string | null): string[] {
  return [...(annotated ?? "").matchAll(/<!--(.*?)-->/g)].map((m) => m[1]!);
}

describe("markCompositionNodes", () => {
  const COMPOSITION = JSON.stringify({
    key: "Root",
    component: "Card",
    children: [
      { key: "Head", component: "Heading", params: { text: "Hi" } },
      {
        key: "Body",
        component: "Stack",
        children: [{ component: "Button", params: { label: "Save" } }],
      },
    ],
  });

  it("brackets every node but the root, with the key the node_key convention gives it", () => {
    const annotated = markCompositionNodes(COMPOSITION);
    // The keys the BACKEND's own convention produces, minus the root (which has no parent to hold
    // a bracket) — that is exactly what must appear in the markers, in that order.
    const keys = compositionNodes(COMPOSITION)
      .map((n) => n.nodeKey)
      .filter((k) => k !== "Root");
    expect(keys).toEqual(["Root/Head", "Root/Body", "Root/Body/Button"]);

    const open = markers(annotated)
      .filter((m) => m.startsWith("nyx-node:"))
      .map((m) => decodeURIComponent(m.slice("nyx-node:".length)));
    const close = markers(annotated)
      .filter((m) => m.startsWith("/nyx-node:"))
      .map((m) => decodeURIComponent(m.slice("/nyx-node:".length)));
    expect(open).toEqual(["Root/Head", "Root/Body", "Root/Body/Button"]);
    expect(close.sort()).toEqual([...keys].sort());
  });

  it("leaves the node_key convention untouched — the markers add keys, they move none", () => {
    const annotated = markCompositionNodes(COMPOSITION);
    const before = compositionNodes(COMPOSITION).map((n) => n.nodeKey);
    const after = compositionNodes(annotated).map((n) => n.nodeKey);
    // Every original key is still there, at the same path. The markers only ADD keys of their own,
    // which nothing addresses — a comment anchored on `Root/Body/Button` still finds its node.
    for (const key of before) expect(after).toContain(key);
    expect(after.filter((k) => !before.includes(k)).every((k) => k.endsWith("/__nyx-mark"))).toBe(
      true,
    );
  });

  it("emits COMMENTS and nothing else — no element that could shift a sibling selector", () => {
    const annotated = markCompositionNodes(COMPOSITION) ?? "";
    const parsed = JSON.parse(annotated) as { children: { inline?: string }[] };
    const inlines = parsed.children.filter((c) => c.inline !== undefined).map((c) => c.inline!);
    expect(inlines.length).toBeGreaterThan(0);
    for (const inline of inlines) expect(inline).toMatch(/^<!--[^<>]*-->$/);
  });

  it("encodes the key so it can never terminate the comment it travels in", () => {
    const hostile = JSON.stringify({
      key: "Root",
      children: [{ key: "a--b>c", inline: "<i></i>" }],
    });
    const annotated = markCompositionNodes(hostile) ?? "";
    const bodies = markers(annotated);
    // `-->` is unwritable in the encoded form, so the comment cannot be cut in half…
    for (const body of bodies) expect(body).not.toContain("--");
    // …and it still decodes to the exact key the tree names.
    const open = decodeURIComponent(bodies[0]!.slice("nyx-node:".length));
    expect(open).toBe("Root/a--b>c");
  });

  it("hands back a composition it cannot read VERBATIM — the marking is never a requirement", () => {
    expect(markCompositionNodes("COMP_HOME")).toBe("COMP_HOME");
    expect(markCompositionNodes("[1,2]")).toBe("[1,2]");
    expect(markCompositionNodes(null)).toBeNull();
    expect(markCompositionNodes("")).toBe("");
  });

  it("leaves a childless composition alone", () => {
    const leaf = JSON.stringify({ key: "Root", component: "Button" });
    expect(markers(markCompositionNodes(leaf))).toEqual([]);
  });
});

describe("rootNodeKey", () => {
  it("reads the root's segment label the way the backend does: key, else component, else inline", () => {
    expect(rootNodeKey(JSON.stringify({ key: "Root", component: "Card" }))).toBe("Root");
    expect(rootNodeKey(JSON.stringify({ component: "Card" }))).toBe("Card");
    expect(rootNodeKey(JSON.stringify({ inline: "<div></div>" }))).toBe("inline");
  });

  it("answers null for what it cannot read", () => {
    expect(rootNodeKey("COMP_HOME")).toBeNull();
    expect(rootNodeKey(null)).toBeNull();
  });
});
