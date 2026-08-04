import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import type { DesignComponentRow } from "../../design/types";
import type { RenderedCssSlot } from "../../design/use-design-render";
import type { PrototypeVariant } from "../proto-types";
import { NodeParamsPanel, type RequestedUsage } from "./node-params";

/**
 * THE NODE PARAMS EDITOR, IN ITS NEW HOME (finding `01KYT10VE8624MKQ18AFHSE0W0`).
 *
 * The user arbitrated for a MOVE, not a removal: everything the composant detail's
 * « USED IN PROTOTYPES » section could do must still be doable, on the surface that shows the
 * composition. So this suite is the proof of ARRIVAL, capability by capability — its own params
 * (the node's DEFAULT base, never the component's `preview` examples), the contract at the read AND
 * at the write, the per-node Save, what is rendered, and the request from the elected composition
 * with its "usage introuvable" verdict.
 *
 * # THE PANEL HAS NO PICKER (finding `01KZ3AN7VVAPXMYFQG4AP441BC`)
 *
 * It used to open with one chip per editable node, above the form, while the elected composition
 * tree listed THE SAME NODES a few pixels below. Two lists of one thing. The chips are gone: every
 * selection now arrives as a {@link RequestedUsage} from something that already SHOWS the nodes —
 * the render, or the tree — which is why this suite drives the panel by re-requesting rather than
 * by clicking inside it.
 */

/** The `Button` of the catalogue: a REQUIRED param with a preview example, a plain one, and an axis. */
const BUTTON: DesignComponentRow = {
  id: "cmp-button",
  project_id: "p1",
  name: "Button",
  source_repository_id: null,
  source_path: "src/ui/Button.tsx",
  visual_hash: null,
  template: "<button>{{label}}<small>{{hint}}</small></button>",
  params_json: JSON.stringify([
    { name: "label", type: "text", required: true, preview: "Enregistrer" },
    { name: "hint", type: "text", preview: "Astuce" },
  ]),
  states_json: JSON.stringify({
    axes: [
      {
        name: "variant",
        values: [
          { name: "ghost", classes: "bg-transparent" },
          { name: "solid", classes: "bg-primary" },
        ],
      },
    ],
  }),
  created_at: 0,
  updated_at: 0,
  scanned_input_hash: null,
  orphaned_at: null,
};

/**
 * A composition with TWO Button placements and two nodes that are NOT editable: an `[inline]` one
 * (no component at all) and one referencing a component the catalogue does not carry (a `[NEW]`).
 */
const COMPOSITION = JSON.stringify({
  key: "Root",
  children: [
    { key: "Primary", component: "Button", params: { label: "FromPrimary" } },
    { key: "Secondary", component: "Button", params: { label: "FromSecondary", variant: "solid" } },
    { key: "Widget", component: "NewWidget", params: {} },
    { key: "Rule", inline: "<hr/>" },
  ],
});

function variantFixture(overrides?: Partial<PrototypeVariant>): PrototypeVariant {
  return {
    id: "var-home",
    page_version_id: "pv-home",
    label: "a",
    title: "Home A",
    composition_json: COMPOSITION,
    states_json: null,
    is_main: true,
    position: 0,
    created_at: 0,
    ...overrides,
  };
}

/** What `proto_variant_set_node_params` answers — the node contract it checked at the WRITE. */
let setNodeParamsWarnings: string[] = [];
/** A backend REFUSAL (a FROZEN design lock), thrown verbatim when set. */
let setNodeParamsRefusal: string | null = null;
let calls: { cmd: string; args: unknown }[] = [];

function installBackend(): void {
  calls = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === "proto_variant_set_node_params") {
      // A REAL `BridgeError` OBJECT — what the bridge rejects with. This is the surface where the
      // `[object Object]` defect was first seen (finding `01KYVT6HVA260DTAEREQA4DFJN`), so the
      // refusal is injected in its true shape rather than as an `Error`.
      if (setNodeParamsRefusal !== null)
        throw {
          kind: "command",
          message: setNodeParamsRefusal,
          source: "proto_variant_set_node_params",
        };
      return { variant: {}, warnings: setNodeParamsWarnings };
    }
    return undefined;
  });
}

type PanelProps = {
  variant?: PrototypeVariant | null;
  components?: readonly DesignComponentRow[];
  componentsLoading?: boolean;
  requestedUsage?: RequestedUsage | null;
  cssSlots?: readonly RenderedCssSlot[];
};

function Panel(props: PanelProps) {
  return (
    <NodeParamsPanel
      variant={props.variant === undefined ? variantFixture() : props.variant}
      components={props.components ?? [BUTTON]}
      componentsLoading={props.componentsLoading ?? false}
      requestedUsage={props.requestedUsage ?? null}
      cssSlots={props.cssSlots}
    />
  );
}

/**
 * The panel carries a LINK to the selected node's component detail (finding
 * `01KYW32N1BVDVTGS0WPMG2PJW2`), so it renders under a memory router carrying that route — the same
 * chassis the page's own suite uses.
 */
function renderPanel(props?: PanelProps) {
  let update: ((next: PanelProps) => void) | null = null;
  function PanelHost() {
    const [current, setCurrent] = useState<PanelProps>(props ?? {});
    update = (next) => setCurrent((prev) => ({ ...prev, ...next }));
    return <Panel {...current} />;
  }
  const root = createRootRoute();
  const host = createRoute({
    getParentRoute: () => root,
    path: "/",
    component: PanelHost,
  });
  const detail = createRoute({
    getParentRoute: () => root,
    path: "/design/$componentId",
    component: () => <div data-testid="component-detail-stub">COMPONENT</div>,
  });
  const router = createRouter({
    routeTree: root.addChildren([host, detail]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  const view = render(<RouterProvider router={router} />);
  const apply = (next: PanelProps) => act(() => update?.(next));
  return {
    ...view,
    // Re-render the PANEL with new props (the router owns the tree, so RTL's own `rerender` cannot).
    update: apply,
    /**
     * WHAT A CLICK IS NOW: a request coming from the render or from the elected-composition tree.
     * A FRESH object every time — the panel keys on identity, so asking twice for the same node is
     * two asks, exactly as two clicks were.
     */
    select: (nodeKey: string, variantId = "var-home") =>
      apply({ requestedUsage: { variantId, nodeKey } }),
  };
}

/** The `Button`'s SCOPED CSS SLOT as `design_render` answers it — one per COMPONENT, never per node. */
const BUTTON_SLOT: RenderedCssSlot = {
  scope_id: "c-button-4d1c",
  component: "Button",
  scope: '[data-nyx-css~="c-button-4d1c"]',
  reason: "a custom pulse keyframe — no Tailwind utility expresses a keyframe",
  features: ["keyframes"],
  bytes: 118,
  css: '[data-nyx-css~="c-button-4d1c"] small{animation:c-button-4d1c-pulse 1.4s infinite}',
  refused: null,
  detail: null,
};

/** The last `proto_variant_set_node_params` payload the backend received. */
function lastSave(): { variantId: string; nodeKey: string; paramsJson: string } | null {
  const call = [...calls].reverse().find((c) => c.cmd === "proto_variant_set_node_params");
  return call === undefined
    ? null
    : (call.args as { variantId: string; nodeKey: string; paramsJson: string });
}

describe("NodeParamsPanel", () => {
  beforeEach(() => {
    setNodeParamsWarnings = [];
    setNodeParamsRefusal = null;
    installBackend();
  });

  /**
   * DONE-CRITERION of finding `01KZ3AN7VVAPXMYFQG4AP441BC` — NO SECOND LIST. The chip row is gone,
   * with the gloss that explained it and the count badge that measured it. What is left, until
   * something is designated, is one line saying where to click.
   */
  it("shows NO node picker of its own — the render and the tree are the list", async () => {
    renderPanel();
    await screen.findByTestId("node-params-panel");

    for (const key of ["Root/Primary", "Root/Secondary", "Root/Widget", "Root/Rule"]) {
      expect(screen.queryByTestId(`proto-node-params-${key}`)).toBeNull();
    }
    // The paragraph the user could not stand ("on capte pas besoin de ton commentaire de con").
    expect(screen.queryByText(/Each node is a PLACEMENT/i)).toBeNull();
    // Nothing is bound until something is designated — and the panel says where to designate it.
    expect(screen.queryByTestId("node-edit-scope")).toBeNull();
    expect(screen.getByTestId("node-params-pick-cue").textContent).toMatch(/click a node/i);
  });

  /** The editable nodes are the PLACEMENTS of a catalogue component — nothing else answers a request. */
  it("binds only the catalogue-component nodes of the composition", async () => {
    const { select } = renderPanel();
    await screen.findByTestId("node-params-panel");

    select("Root/Primary");
    expect((await screen.findByTestId("node-edit-scope")).textContent).toContain("Root/Primary");

    // An `[inline]` node has no component, and a `[NEW]` one has no declaration to build a form
    // from: neither is editable, and asking for one is answered — never silently honoured.
    select("Root/Rule");
    expect((await screen.findByTestId("usage-not-found")).textContent).toContain("Root/Rule");
    select("Root/Widget");
    await waitFor(() =>
      expect(screen.getByTestId("usage-not-found").textContent).toContain("Root/Widget"),
    );
    expect(screen.queryByTestId("node-edit-scope")).toBeNull();
  });

  /**
   * THE BITE, ON THE SELECTION ITSELF. Picking a node must load THAT node's params — the second one,
   * not the first, not a hard-coded one. Both placements instantiate the SAME component with a
   * different parameterization, so a form seeded from the component (or from the head of the list)
   * fails here.
   */
  it("seeds the form from the SELECTED node's own params (not the first node's)", async () => {
    const { select } = renderPanel();
    await screen.findByTestId("node-params-panel");

    select("Root/Secondary");
    await waitFor(() =>
      expect((screen.getByLabelText("label") as HTMLInputElement).value).toBe("FromSecondary"),
    );
    // …including the AXIS the node carries: `variant=solid`, not the axis's default.
    expect(screen.getByRole("button", { name: "solid" }).getAttribute("aria-pressed")).toBe("true");

    // …and switching to the OTHER placement re-seeds from it, not from what was on screen.
    select("Root/Primary");
    await waitFor(() =>
      expect((screen.getByLabelText("label") as HTMLInputElement).value).toBe("FromPrimary"),
    );
    expect(screen.getByRole("button", { name: "ghost" }).getAttribute("aria-pressed")).toBe("true");
  });

  /**
   * A PREVIEW VALUE NEVER REACHES A COMPOSITION (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`, which this
   * move had to carry). `hint` declares the example "Astuce" and NO default; the node provides
   * nothing for it. What this form must show — and therefore what a Save must write — is the empty
   * DEFAULT, never the illustration a vignette uses.
   */
  it("seeds a node from the DEFAULTS, never from the component's preview examples", async () => {
    const { select } = renderPanel();
    await screen.findByTestId("node-params-panel");
    select("Root/Primary");

    await waitFor(() =>
      expect((screen.getByLabelText("label") as HTMLInputElement).value).toBe("FromPrimary"),
    );
    expect((screen.getByLabelText("hint") as HTMLInputElement).value).toBe("");

    fireEvent.click(screen.getByRole("button", { name: "Save to node" }));
    await waitFor(() => {
      const save = lastSave();
      expect(save).toBeTruthy();
      expect(JSON.parse(save!.paramsJson)).toMatchObject({ label: "FromPrimary", hint: "" });
    });
  });

  /** The per-node SAVE: the edited map, written back onto THAT placement of THIS variant. */
  it("persists the edited params PER NODE via proto_variant_set_node_params", async () => {
    const { select } = renderPanel();
    await screen.findByTestId("node-params-panel");
    select("Root/Secondary");
    await waitFor(() =>
      expect((screen.getByLabelText("label") as HTMLInputElement).value).toBe("FromSecondary"),
    );

    fireEvent.change(screen.getByLabelText("label"), { target: { value: "Edited" } });
    fireEvent.click(screen.getByRole("button", { name: "ghost" }));
    fireEvent.click(screen.getByRole("button", { name: "Save to node" }));

    await waitFor(() => {
      const save = lastSave();
      expect(save).toBeTruthy();
      expect(save!.variantId).toBe("var-home");
      expect(save!.nodeKey).toBe("Root/Secondary");
      expect(JSON.parse(save!.paramsJson)).toMatchObject({ label: "Edited", variant: "ghost" });
    });
  });

  /**
   * THE NODE CONTRACT, on both faces (finding `01KYQ0R9414Q5K63TBV9CCMC9J`) — the other capability
   * that had to survive the move. Selecting a node CONFRONTS what is STORED on it with what the
   * component declares (the READ); saving answers with the backend's own verdict (the WRITE). The two
   * defects stay distinct, and neither message ever guesses a mapping between neighbouring names.
   */
  it("names what the selected node owes the component, at the read AND at the write", async () => {
    const broken = JSON.stringify({
      key: "Root",
      children: [{ key: "Reset", component: "Button", params: { text: "Reset filters" } }],
    });
    setNodeParamsWarnings = [
      "node 'Root/Reset' carries key(s) component 'Button' declares NOWHERE: 'text'",
    ];
    const { select } = renderPanel({ variant: variantFixture({ composition_json: broken }) });

    // Nothing bound yet — no verdict about a node nobody is looking at.
    await screen.findByTestId("node-params-panel");
    expect(screen.queryByTestId("node-contract-unknown-key")).toBeNull();

    select("Root/Reset");

    const unknown = await screen.findByTestId("node-contract-unknown-key");
    expect(unknown.textContent).toContain("Root/Reset");
    expect(unknown.textContent).toContain("'text'");
    expect(unknown.textContent).toContain("label");
    expect(unknown.textContent).toContain("Rename the key");
    expect(unknown.textContent).not.toContain("did you mean");
    expect(screen.getByTestId("node-contract-missing-required").textContent).toContain("'label'");

    fireEvent.click(screen.getByRole("button", { name: "Save to node" }));
    await waitFor(() =>
      expect(screen.getByTestId("node-contract-save-warning").textContent).toContain(
        "declares NOWHERE",
      ),
    );
  });

  /**
   * WHAT IS EDITED vs WHAT IS RENDERED. The detail's `Showing` cartouche said which of two things the
   * frame was rendering; here the frame renders the whole page composition AS STORED, so the honest
   * statement is that an unsaved edit lives in this form and nowhere else.
   *
   * …AND IT NAMES NOTHING THE SCREEN ALREADY NAMES (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`). The line
   * used to end with `as composed in <page> / <variant>` — the page title the header carries and the
   * variant the strip selects and the composition tree badges. On one screen the page's title reached
   * four places; this was one of them, and the panel does not even receive it any more.
   */
  it("says which placement is being edited, and that the preview renders the STORED composition", async () => {
    const { select } = renderPanel();
    await screen.findByTestId("node-params-panel");
    select("Root/Secondary");

    const scope = (await screen.findByTestId("node-edit-scope")).textContent ?? "";
    expect(scope).toContain("Root/Secondary");
    expect(scope).toContain("Button");
    expect(scope).toContain("STORED");
    // The page and the variant are stated elsewhere on the screen — not a second time here.
    expect(scope).not.toContain("Home");
    expect(scope).not.toContain("as composed in");
    // …and the link out only has to say where it goes: the component is named one line above.
    expect(screen.getByTestId("node-component-link").textContent).toBe("Open in the catalogue");
  });

  /**
   * THE DEEP-LINK, AT ITS NEW DESTINATION (finding `01KYSNWNDG6HBFJX017RE203SZ`). A node clicked in
   * the elected composition asks for THAT placement; it must open here WITHOUT a click, with exactly
   * what a clicked node gets.
   */
  it("binds the requested placement without a click", async () => {
    renderPanel({ requestedUsage: { variantId: "var-home", nodeKey: "Root/Secondary" } });

    await waitFor(() =>
      expect((screen.getByLabelText("label") as HTMLInputElement).value).toBe("FromSecondary"),
    );
    expect(screen.getByTestId("node-edit-scope").textContent).toContain("Root/Secondary");
    expect(screen.queryByTestId("usage-not-found")).toBeNull();
    // …and the "where to click" cue steps aside the moment something IS bound.
    expect(screen.queryByTestId("node-params-pick-cue")).toBeNull();
  });

  /**
   * …and a request that MISSES is an OUTCOME, not a silent fallback — the verdict that had to survive
   * the move. Leaving the panel on nothing while the caller asked for a placement is the very defect
   * the deep-link was written to fix.
   */
  it("says when the requested placement is not in the composition (and binds nothing)", async () => {
    const { select } = renderPanel({
      requestedUsage: { variantId: "var-home", nodeKey: "Root/Gone" },
    });

    const notice = await screen.findByTestId("usage-not-found");
    expect(notice.textContent).toContain("Root/Gone");
    // Nothing is bound: no form, no Save that would write onto a placement nobody asked for.
    expect(screen.queryByTestId("node-edit-scope")).toBeNull();
    expect(screen.queryByRole("button", { name: "Save to node" })).toBeNull();

    // Designating a node that IS there answers the notice.
    select("Root/Primary");
    await waitFor(() => expect(screen.queryByTestId("usage-not-found")).toBeNull());
  });

  /**
   * …and the verdict WAITS for the catalogue. An empty list that never answered proves no absence:
   * every node looks un-editable while `design_component_list` is in flight, and declaring the
   * placement missing there would be the same lie in the other direction.
   */
  it("waits for the catalogue before declaring a requested placement missing", async () => {
    const requested: RequestedUsage = { variantId: "var-home", nodeKey: "Root/Secondary" };
    const { update } = renderPanel({
      components: [],
      componentsLoading: true,
      requestedUsage: requested,
    });
    await screen.findByTestId("node-params-panel");
    expect(screen.queryByTestId("usage-not-found")).toBeNull();

    // The catalogue lands → the placement resolves, and it is BOUND rather than declared gone.
    update({ components: [BUTTON], componentsLoading: false, requestedUsage: requested });
    await waitFor(() =>
      expect((screen.getByLabelText("label") as HTMLInputElement).value).toBe("FromSecondary"),
    );
    expect(screen.queryByTestId("usage-not-found")).toBeNull();
  });

  /**
   * A REFUSED write is REPORTED, verbatim. A variant that is the FROZEN elected composition of a
   * design-locked round is immutable (PDR-0009: one iterates by opening a NEW round) — the backend
   * owns that rule and its wording, and this surface prints it rather than paraphrasing it.
   */
  it("surfaces the backend's refusal verbatim when the composition is frozen", async () => {
    setNodeParamsRefusal =
      "variant var-home is part of the FROZEN design (round round1 is locked): its composition is immutable — open a NEW round to iterate (PDR-0009)";
    const { select } = renderPanel();
    await screen.findByTestId("node-params-panel");
    select("Root/Primary");
    fireEvent.click(await screen.findByRole("button", { name: "Save to node" }));

    const error = await screen.findByTestId("node-save-error");
    expect(error.textContent).toContain("FROZEN design");
    expect(error.textContent).toContain("open a NEW round to iterate");
    expect(error.textContent).not.toContain("[object Object]");
  });

  /** A composition that instantiates nothing from the catalogue says so — there is nothing to edit. */
  it("says when the composition instantiates no catalogue component", async () => {
    renderPanel({ components: [] });
    expect(await screen.findByText(/no catalogue component/i)).toBeTruthy();
    // …and it is the OTHER verdict: there is nothing to point at, so no "click a node" cue either.
    expect(screen.queryByTestId("node-params-pick-cue")).toBeNull();
  });

  /**
   * THE SCOPED CSS SLOT, VISIBLE IN REVIEW (finding `01KYVVEQZW2Y2VY7ZAPK5RECZS`).
   *
   * The slot's fourth non-negotiable constraint is SIGNALLED: the human re-reading the page must SEE
   * the exception. It is displayed against the node the reviewer selected — and it is the
   * COMPONENT's, which both placements of `Button` therefore show, worded so nobody reads it as a
   * property of one placement.
   */
  describe("the scoped CSS slot of the selected node", () => {
    it("shows the component's slot under the node's params, for every placement of it", async () => {
      const { select } = renderPanel({ cssSlots: [BUTTON_SLOT] });
      await screen.findByTestId("node-params-panel");
      // Nothing selected → nothing claimed.
      expect(screen.queryByTestId("css-slot-c-button-4d1c")).toBeNull();

      select("Root/Primary");
      expect(await screen.findByTestId("css-slot-scope")).toHaveTextContent(
        '[data-nyx-css~="c-button-4d1c"]',
      );
      expect(screen.getByTestId("css-slot-code")).toHaveTextContent("c-button-4d1c-pulse");
      expect(screen.getByTestId("css-slot-c-button-4d1c")).toHaveTextContent(
        "not to this placement",
      );

      // The OTHER placement of the same component shows the SAME exception — there is one slot.
      select("Root/Secondary");
      expect(await screen.findByTestId("css-slot-c-button-4d1c")).toBeTruthy();
    });

    /**
     * The per-chip exception flag went with the chips (finding `01KZ3AN7VVAPXMYFQG4AP441BC`). The
     * signal did NOT: the elected-composition tree flags every node whose component carries one, off
     * the same `css_scope` pointer — one list, one flag.
     */
    it("carries no per-node flag of its own — the tree is where the nodes are", async () => {
      renderPanel({ cssSlots: [BUTTON_SLOT] });
      await screen.findByTestId("node-params-panel");
      expect(screen.queryByTestId("proto-node-css-slot-Root/Primary")).toBeNull();
      expect(screen.queryByTestId("proto-node-css-slot-Root/Secondary")).toBeNull();
    });

    /** A component carrying none shows nothing at all — a clean page is not taxed by this. */
    it("shows nothing when the selected node's component carries no slot", async () => {
      const { select } = renderPanel({ cssSlots: [{ ...BUTTON_SLOT, component: "Spinner" }] });
      await screen.findByTestId("node-params-panel");
      select("Root/Primary");
      expect(await screen.findByTestId("node-edit-scope")).toBeTruthy();
      expect(screen.queryByTestId("css-slot-c-button-4d1c")).toBeNull();
    });

    /** A stored slot that no longer compiles is NAMED here, not silently absent. */
    it("names a refused slot instead of dropping it", async () => {
      const { select } = renderPanel({
        cssSlots: [
          {
            ...BUTTON_SLOT,
            css: "",
            features: [],
            refused: "unknown_token",
            detail: "var(--brand-glow) names a custom property the token layer does not declare",
          },
        ],
      });
      await screen.findByTestId("node-params-panel");
      select("Root/Primary");
      const card = await screen.findByTestId("css-slot-c-button-4d1c");
      expect(card).toHaveAttribute("data-refused", "true");
      expect(screen.getByTestId("css-slot-refusal")).toHaveTextContent("Refused — unknown token.");
      expect(screen.getByTestId("css-slot-refusal")).toHaveTextContent("--brand-glow");
      expect(screen.queryByTestId("css-slot-code")).toBeNull();
    });
  });
});
