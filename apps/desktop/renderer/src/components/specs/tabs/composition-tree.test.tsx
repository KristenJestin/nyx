import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { DesignComponentRow } from "../../design/types";
import type { RenderedCssSlot } from "../../design/use-design-render";
import { CompositionTree, compositionTreeNodes, type OpenNode } from "./composition-tree";

/**
 * `<CompositionTree>` — the named tree of the composition ON SCREEN (finding
 * `01KZ3JN5KJQWJTV2XNT604CWCT`).
 *
 * It replaces the view that read `proto_composition`, the FROZEN elected composition of the
 * design-locked round. What is asserted here is the property that view could not have: the tree is a
 * pure function of the composition the stage renders, so it cannot describe another variant, another
 * page version, or nothing at all for a page the frozen round does not pin.
 */

function component(overrides: Partial<DesignComponentRow>): DesignComponentRow {
  return {
    id: "cmp",
    project_id: "p1",
    name: "Button",
    source_repository_id: "r1",
    source_path: "src/Button.tsx",
    visual_hash: null,
    template: "<button>x</button>",
    params_json: null,
    states_json: null,
    created_at: 0,
    updated_at: 0,
    scanned_input_hash: null,
    orphaned_at: null,
    ...overrides,
  };
}

const COMPOSITION = JSON.stringify({
  key: "Root",
  children: [
    { key: "Header", children: [{ key: "Title", component: "Heading" }] },
    { key: "Reset", component: "Button" },
    { key: "Widget", component: "NewWidget" },
    { key: "Note", inline: "<p>hand-written</p>" },
  ],
});

describe("compositionTreeNodes — the coder seam's own classification, in the renderer", () => {
  const catalogue = [
    component({ id: "cmp-button", name: "Button", source_path: "src/Button.tsx" }),
    component({ id: "cmp-heading", name: "Heading", source_path: "src/Heading.tsx" }),
    // AUTHORED but never materialized: a greenfield row the agent proposed. Review #41 — the nature
    // is the ORIGIN, not the catalogue membership, so this stays `[NEW]` while still being openable.
    component({ id: "cmp-widget", name: "NewWidget", source_path: null }),
  ];

  it("classifies each node by ORIGIN, and omits the structural ones", () => {
    expect(compositionTreeNodes(COMPOSITION, catalogue)).toEqual([
      // `Root` and `Root/Header` group children and carry none of the three natures — omitted, as
      // `db::classify_elected_nodes` omits them.
      {
        nodeKey: "Root/Header/Title",
        nature: "ds",
        component: "Heading",
        componentRef: "cmp-heading",
        cssScope: null,
      },
      {
        nodeKey: "Root/Reset",
        nature: "ds",
        component: "Button",
        componentRef: "cmp-button",
        cssScope: null,
      },
      {
        nodeKey: "Root/Widget",
        nature: "new",
        component: "NewWidget",
        componentRef: "cmp-widget",
        cssScope: null,
      },
      {
        nodeKey: "Root/Note",
        nature: "inline",
        component: null,
        componentRef: null,
        cssScope: null,
      },
    ]);
  });

  /** A component the catalogue does not carry at all is `[NEW]` with nothing to open. */
  it("marks an un-catalogued component [NEW] with no reference", () => {
    const nodes = compositionTreeNodes(COMPOSITION, [catalogue[0]!, catalogue[1]!]);
    expect(nodes.find((n) => n.component === "NewWidget")).toMatchObject({
      nature: "new",
      componentRef: null,
    });
  });

  /**
   * THE CSS SCOPE IS THE RENDER'S ANSWER, not a second derivation: the sidecar decided which
   * components carry an exception and stamped `data-nyx-css` for exactly those, so the flag on the
   * row and the mark in the frame speak of the same thing.
   */
  it("points a node at its component's exception, off the render's own list", () => {
    const slots: RenderedCssSlot[] = [
      {
        scope_id: "c-button-1",
        component: "Button",
        scope: '[data-nyx-css~="c-button-1"]',
        reason: "a pulse keyframe",
        features: ["keyframes"],
        bytes: 40,
        css: "@keyframes x{}",
        refused: null,
        detail: null,
      },
    ];
    const nodes = compositionTreeNodes(COMPOSITION, catalogue, slots);
    expect(nodes.find((n) => n.nodeKey === "Root/Reset")?.cssScope).toBe("c-button-1");
    expect(nodes.find((n) => n.nodeKey === "Root/Widget")?.cssScope).toBeNull();
  });

  /** A composition that does not parse yields no node — a tree must not break the pane. */
  it("yields nothing for a null or malformed composition", () => {
    expect(compositionTreeNodes(null, catalogue)).toEqual([]);
    expect(compositionTreeNodes("{{{", catalogue)).toEqual([]);
  });
});

describe("<CompositionTree>", () => {
  const catalogue = [component({ id: "cmp-button", name: "Button" })];
  const simple = JSON.stringify({
    key: "Root",
    children: [{ key: "Reset", component: "Button" }],
  });

  /**
   * OPENABLE IS EDITABLE. The row offers `Open` on exactly the nodes the params editor can bind —
   * both answer "does the catalogue carry this component?" off the same rows. A tree proposing a
   * node the form refuses is the whole finding.
   */
  it("opens the placement it names: page, variant and node key", () => {
    const opened: Parameters<OpenNode>[0][] = [];
    render(
      <CompositionTree
        compositionJson={simple}
        variantLabel="c"
        pageId="pg-home"
        variantId="var-c"
        components={catalogue}
        onOpenNode={(t) => opened.push(t)}
      />,
    );
    expect(screen.getByTestId("proto-composition-variant").textContent).toBe("c");
    fireEvent.click(screen.getByTestId("proto-comp-node-Root/Reset"));
    expect(opened).toEqual([
      { pageId: "pg-home", componentId: "cmp-button", variantId: "var-c", nodeKey: "Root/Reset" },
    ]);
  });

  /**
   * THE VERDICT WAITS FOR THE CATALOGUE. Before it answers, every node looks un-catalogued: a tree
   * drawn then would badge the whole page `[NEW]` and offer no `Open` at all — the same rule the
   * params editor applies before it declares a placement missing.
   */
  it("waits for the catalogue rather than badging the page [NEW]", () => {
    render(
      <CompositionTree
        compositionJson={simple}
        pageId="pg-home"
        variantId="var-c"
        components={[]}
        componentsLoading
        onOpenNode={() => {}}
      />,
    );
    expect(screen.queryByTestId("proto-comp-node-Root/Reset")).toBeNull();
    expect(screen.queryByText("[NEW]")).toBeNull();
  });

  /** A page with no variant says so — it never borrows another page's tree. */
  it("says a page has no variant instead of showing someone else's", () => {
    render(
      <CompositionTree
        compositionJson={null}
        pageId="pg-home"
        variantId={null}
        components={catalogue}
        onOpenNode={() => {}}
      />,
    );
    expect(screen.getByTestId("proto-composition-no-variant")).toBeTruthy();
  });
});
