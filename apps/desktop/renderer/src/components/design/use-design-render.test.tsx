import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";
import { bridgeFake, mockIPC } from "@/bridge/test-harness";

import {
  CSS_SLOT_FEATURE_LABELS,
  CSS_SLOT_REFUSAL_LABELS,
  cssSlotFeatureLabel,
  cssSlotRefusalLabel,
  NAVIGATION_STATUS_LABELS,
  navigationStatusLabel,
  UNRENDERED_REASON_LABELS,
  unrenderedReasonLabel,
  useDesignRender,
  type UnrenderedNode,
} from "./use-design-render";

/**
 * `useDesignRender` — the frame's only reading of `design_render`, and the ONE place the renderer
 * knows what an unrenderable node is called (finding `01KYVQE838PNSPRX9DNNDNWDN2`, the alignment of
 * the renderer on the backend).
 *
 * The renderer used to carry a `composition-holes.ts` of its own: it rewrote the composition before
 * sending it, and it knew TWO of the five reasons a node can fail to render — a template carrying a
 * block helper or a partial was a hole it could neither foresee nor name. That twin was DELETED. The
 * sidecar renders leniently and answers `{ markup, node_count, unrendered, unrendered_count }`; this
 * hook carries that shape, and the vocabulary of `reason` is pinned to
 * `contract/design-fixtures.json` in BOTH directions.
 */

describe("the unrendered vocabulary", () => {
  const fixtureReasons = designFixtures.unrendered_nodes.reasons;

  /**
   * BOTH DIRECTIONS, which is what makes the fixture an arrimage rather than a copy: a reason added
   * in Rust (`template_has_partial` was, and the renderer's twin never learned it) fails this because
   * it has no label here; a label invented here fails it because the fixture does not carry it.
   */
  it("maps exactly the reason codes the shared fixture pins, with the fixture's own wording", () => {
    expect(Object.keys(UNRENDERED_REASON_LABELS).sort()).toEqual(
      fixtureReasons.map((r) => r.code).sort(),
    );
    for (const reason of fixtureReasons) {
      expect(unrenderedReasonLabel(reason.code), reason.why).toBe(reason.short);
    }
  });

  /** A code no build ever produces still NAMES the defect rather than showing a blank badge. */
  it("shows an unknown code verbatim rather than swallowing it", () => {
    expect(unrenderedReasonLabel("a_reason_from_the_future")).toBe("a_reason_from_the_future");
  });
});

/**
 * THE CSS-SLOT VOCABULARY (findings `01KYVMTWB71R0D0CT1SV88J8DX` / `01KYVVEQZW2Y2VY7ZAPK5RECZS`).
 *
 * Rust OWNS the compilation and the scoping of a slot; the renderer DISPLAYS what the sidecar
 * already decided. What crosses the language boundary — and is therefore pinned in
 * `contract/design-fixtures.json` under `css_slot` — is the VOCABULARY the review card keys off: the
 * four feature codes it ticks and the nineteen refusal codes a stored slot may come back with.
 *
 * Arrimé in BOTH directions, exactly like the unrendered reasons above: a code added in Rust with no
 * label here fails this test, and a label invented here with no fixture line fails it too. That is
 * the difference between an arrimage and a copy — a copy silently stops reacting the day the code
 * changes, which for a REFUSAL means a card that says nothing about why nothing applied.
 */
describe("the css-slot vocabulary", () => {
  const fixture = designFixtures.css_slot;

  /**
   * THE WORDING IS THE CONTRACT HERE, and it was asserted as "not empty" (finding
   * `01KZ3JTWB82DCG0C6JT06T4CAF`, point 1). Renaming a displayed label left 409 tests green while
   * the human read something nobody had agreed to — and for a REFUSAL the label is the WHOLE of what
   * the reviewer learns about why nothing was applied. The codes were pinned in both directions; the
   * WORDS were not. They are now, by EQUALITY, exactly like the unrendered reasons above — whose
   * same mutation already failed, and failed only because their fixture carried a `short`.
   */
  it("labels exactly the four feature codes the shared fixture pins, with the fixture's wording", () => {
    expect(Object.keys(CSS_SLOT_FEATURE_LABELS).sort()).toEqual(
      fixture.features.map((f) => f.code).sort(),
    );
    for (const feature of fixture.features) {
      expect(cssSlotFeatureLabel(feature.code), feature.why).toBe(feature.short);
    }
  });

  it("labels exactly the refusal codes the shared fixture pins, all nineteen, word for word", () => {
    expect(Object.keys(CSS_SLOT_REFUSAL_LABELS).sort()).toEqual(
      fixture.refusals.map((r) => r.code).sort(),
    );
    expect(fixture.refusals).toHaveLength(19);
    for (const refusal of fixture.refusals) {
      expect(cssSlotRefusalLabel(refusal.code), refusal.why).toBe(refusal.short);
    }
  });

  /** A code no build ever produces still NAMES the defect rather than showing a blank line. */
  it("shows an unknown feature or refusal code verbatim rather than swallowing it", () => {
    expect(cssSlotFeatureLabel("a_feature_from_the_future")).toBe("a_feature_from_the_future");
    expect(cssSlotRefusalLabel("a_refusal_from_the_future")).toBe("a_refusal_from_the_future");
  });

  /** The two attributes the display leans on are the fixture's, not conventions of this file. */
  it("pins the attribute the MARK on a node is read from", () => {
    expect(fixture.scope_attribute).toBe("data-nyx-css");
    expect(fixture.style_attribute).toBe("data-nyx-css-slot");
  });
});

/**
 * THE NAVIGATION VOCABULARY (finding `01KYW34347CZSNXCYS35KDG3EY`, consumed by
 * `01KZ3JN69E32FYBF01NBE4FWJD`).
 *
 * Arrimé in BOTH directions like the two vocabularies above. It matters more than most here: there
 * is no status meaning "nothing happened", so a code this renderer cannot name is a dead link shown
 * as nothing at all — which is precisely the state the whole feature was found in.
 */
describe("the navigation vocabulary", () => {
  const fixtureStatuses = designFixtures.navigation.statuses;

  it("maps exactly the status codes the shared fixture pins, with the fixture's own wording", () => {
    expect(Object.keys(NAVIGATION_STATUS_LABELS).sort()).toEqual(
      fixtureStatuses.map((s) => s.code).sort(),
    );
    for (const status of fixtureStatuses) {
      expect(navigationStatusLabel(status.code), status.why).toBe(status.short);
    }
  });

  /** A code no build ever produces still NAMES the outcome rather than showing a blank badge. */
  it("shows an unknown code verbatim rather than swallowing it", () => {
    expect(navigationStatusLabel("a_status_from_the_future")).toBe("a_status_from_the_future");
  });
});

describe("useDesignRender", () => {
  /** The composition of the fixture case whose ORDER is the trap: children before their parent. */
  const nestedCase = designFixtures.unrendered_nodes.cases.find(
    (c) => c.id === "the-frame-keeps-the-vocabulary-the-agent-consumes-it",
  )!;

  it("is disabled — and empty, never pending — without a project or a composition", async () => {
    mockIPC(() => ({ markup: "<b/>", node_count: 1, unrendered: [], unrendered_count: 0 }));
    const { result } = renderHook(() => useDesignRender(null, '{"component":"Card"}'));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current).toMatchObject({ markup: null, nodeCount: 0, unrendered: [] });
  });

  /**
   * THE THREE ADDITIVE FIELDS, carried through as the sidecar sends them — INCLUDING THE ORDER of
   * `unrendered[]`, which is depth-first CHILDREN BEFORE THEIR PARENT. The hook must not sort it: a
   * host that received `Root` before `Root/Ghost` would state the hierarchy upside down.
   */
  it("carries markup, node_count and unrendered[] through in the sidecar's own order", async () => {
    mockIPC((cmd) =>
      cmd === "design_render"
        ? {
            markup: "<div>[UNRENDERED Ghost — not in the catalogue]</div>",
            node_count: nestedCase.expect.node_count,
            unrendered: nestedCase.expect.unrendered.map((u) => ({
              node_key: u.node_key,
              component: u.component,
              reason: u.reason,
              detail: `${u.component} — ${u.reason}`,
            })),
            unrendered_count: nestedCase.expect.unrendered.length,
          }
        : undefined,
    );
    const { result } = renderHook(() =>
      useDesignRender("p1", JSON.stringify(nestedCase.composition)),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.nodeCount).toBe(3);
    // The CHILD first, then its parent — the fixture's own order, untouched.
    expect(result.current.unrendered.map((u: UnrenderedNode) => u.node_key)).toEqual([
      "Root/Ghost",
      "Root",
    ]);
    expect(result.current.error).toBeNull();
  });

  /** A composition that does not PARSE is the only refusal left: a readable error, never a blank. */
  it("surfaces the sidecar's refusal and reports nothing rendered", async () => {
    mockIPC(() => {
      throw new Error("design_render: bad composition");
    });
    const { result } = renderHook(() => useDesignRender("p1", "{{{"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toContain("bad composition");
    expect(result.current).toMatchObject({ markup: null, nodeCount: 0, unrendered: [] });
  });

  /** A shell answering the OLD shape must not throw here — it simply reports no hole. */
  it("reads a payload without the additive fields defensively", async () => {
    mockIPC(() => ({ markup: "<b/>" }));
    const { result } = renderHook(() => useDesignRender("p1", '{"component":"Card"}'));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current).toMatchObject({
      markup: "<b/>",
      nodeCount: 0,
      unrendered: [],
      cssSlots: [],
      cssSlotCount: 0,
    });
  });

  /**
   * THE SCOPED CSS SLOTS, carried through as the sidecar sends them — the count included, because
   * `css_slot_count` is what the chrome's chip reports and the renderer counts nothing of its own.
   */
  it("carries css_slots[] and css_slot_count through", async () => {
    mockIPC(() => ({
      markup:
        '<button data-nyx-css="c-button-1">OK</button><style data-nyx-css-slot="c-button-1">…</style>',
      node_count: 1,
      unrendered: [],
      unrendered_count: 0,
      css_slots: [
        {
          scope_id: "c-button-1",
          component: "Button",
          scope: '[data-nyx-css~="c-button-1"]',
          reason: "a custom pulse keyframe",
          features: ["keyframes"],
          bytes: 96,
          css: "@keyframes c-button-1-pulse{50%{opacity:.5}}",
          refused: null,
          detail: null,
        },
      ],
      css_slot_count: 1,
    }));
    const { result } = renderHook(() => useDesignRender("p1", '{"component":"Button"}'));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.cssSlotCount).toBe(1);
    expect(result.current.cssSlots[0]).toMatchObject({
      component: "Button",
      scope: '[data-nyx-css~="c-button-1"]',
      features: ["keyframes"],
      refused: null,
    });
  });

  /**
   * A STORED SLOT THAT NO LONGER COMPILES is in the list with `refused` filled and NO css — it is
   * NAMED, never dropped. Same principle as an unrenderable node: a rule that silently does nothing
   * is the disappearance the slot exists to end.
   */
  it("keeps a refused slot in the list, named, with nothing emitted", async () => {
    mockIPC(() => ({
      markup: '<button data-nyx-css="c-button-1">OK</button>',
      node_count: 1,
      css_slots: [
        {
          scope_id: "c-button-1",
          component: "Button",
          scope: '[data-nyx-css~="c-button-1"]',
          reason: "a custom pulse keyframe",
          features: [],
          bytes: 40,
          css: "",
          refused: "hardcoded_colour",
          detail: "a colour written in the CSS instead of referenced from the token layer",
        },
      ],
      css_slot_count: 1,
    }));
    const { result } = renderHook(() => useDesignRender("p1", '{"component":"Button"}'));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.cssSlots).toHaveLength(1);
    expect(result.current.cssSlots[0]?.refused).toBe("hardcoded_colour");
    expect(result.current.cssSlots[0]?.css).toBe("");
    expect(cssSlotRefusalLabel(result.current.cssSlots[0]!.refused!)).toBe("hard-coded colour");
  });

  /**
   * THE PROTOTYPE ID REACHES THE SIDECAR (finding `01KZ3JN69E32FYBF01NBE4FWJD`, CRITICAL). It is the
   * page DIRECTORY the `navigate_to` of each node is resolved against, it is optional on the wire,
   * and no caller ever sent it — so every link of every prototype page came back `unknown_page` and
   * the whole inter-page parcours was dead with no error anywhere.
   */
  it("sends prototypeId when it has one, and omits the key when it has none", async () => {
    mockIPC(() => ({ markup: "<b/>", node_count: 1 }));
    const { result } = renderHook(() => useDesignRender("p1", '{"component":"Card"}', "proto1"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    const withId = bridgeFake()
      .calls.filter((c) => c.command === "design_render")
      .at(-1);
    expect(withId?.args).toMatchObject({ projectId: "p1", prototypeId: "proto1" });

    mockIPC(() => ({ markup: "<b/>", node_count: 1 }));
    const bare = renderHook(() => useDesignRender("p1", '{"component":"Card"}'));
    await waitFor(() => expect(bare.result.current.loading).toBe(false));
    const args =
      bridgeFake()
        .calls.filter((c) => c.command === "design_render")
        .at(-1)?.args ?? {};
    // OMITTED, not `null`: the sidecar reads an ABSENT key (`arg_opt_str`).
    expect("prototypeId" in args).toBe(false);
  });

  /**
   * THE AXES THE SHIM WILL PILOT REACH THE SIDECAR TOO (finding `01KZ41YK39SGNKF6XV87A35GPN`,
   * CRITICAL) — under the name the sidecar reads them by, `subjectStatesJson`, and omitted rather
   * than nulled when the frame gets no axes at all.
   *
   * It is the SEAM of the "one owner per axis" rule: the same `states_json` a host hands
   * [`CompositionFrame.axes`] (`window.__nyxAxes`) is what the render must leave alone, and every
   * OTHER axis a node declares is baked into that node's markup by the sidecar. A host that sent
   * neither had three dead controls in its params form — `variant`, `size` and `loading` changed
   * nothing at all; a host that sent only the frame's half would have the shim and the render
   * applying the same axis twice. Renaming this key on either side must fail here.
   */
  it("sends subjectStatesJson — the axes the shim pilots — and omits it when there are none", async () => {
    const states = '{"axes":[{"name":"loading","values":[{"name":"off"},{"name":"on"}]}]}';
    mockIPC(() => ({ markup: "<b/>", node_count: 1 }));
    const { result } = renderHook(() =>
      useDesignRender("p1", '{"component":"Card"}', null, states),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(
      bridgeFake()
        .calls.filter((c) => c.command === "design_render")
        .at(-1)?.args,
    ).toMatchObject({ projectId: "p1", subjectStatesJson: states });

    mockIPC(() => ({ markup: "<b/>", node_count: 1 }));
    const none = renderHook(() => useDesignRender("p1", '{"component":"Card"}'));
    await waitFor(() => expect(none.result.current.loading).toBe(false));
    const bareArgs =
      bridgeFake()
        .calls.filter((c) => c.command === "design_render")
        .at(-1)?.args ?? {};
    expect("subjectStatesJson" in bareArgs).toBe(false);
  });

  /**
   * `navigation[]` AND ITS TWO COUNTS travel through — a link that works and a link whose page is
   * gone are both facts about the page, and both are carried so a host can display them.
   */
  it("carries navigation[], navigation_count and navigation_broken_count through", async () => {
    mockIPC(() => ({
      markup: '<a data-nyx-page="pricing">Go</a>',
      node_count: 2,
      navigation: [
        {
          node_key: "Root/Go",
          page_id: "pg-pricing",
          page: "pricing",
          page_title: "Pricing",
          status: "linked",
          detail: "",
        },
        {
          node_key: "Root/Ghost",
          page_id: "pg-deleted",
          page: null,
          page_title: null,
          status: "unknown_page",
          detail: "the page this node links to no longer exists",
        },
      ],
      navigation_count: 2,
      navigation_broken_count: 1,
    }));
    const { result } = renderHook(() => useDesignRender("p1", "{}", "proto1"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.navigationCount).toBe(2);
    expect(result.current.navigationBrokenCount).toBe(1);
    expect(result.current.navigation[1]).toMatchObject({
      node_key: "Root/Ghost",
      status: "unknown_page",
      page: null,
      page_title: null,
    });
    expect(navigationStatusLabel(result.current.navigation[1]!.status)).toBe("page is gone");
  });

  /** A shell answering the older shape reports NO link rather than throwing. */
  it("reads a payload without the navigation fields defensively", async () => {
    mockIPC(() => ({ markup: "<b/>" }));
    const { result } = renderHook(() => useDesignRender("p1", "{}", "proto1"));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current).toMatchObject({
      navigation: [],
      navigationCount: 0,
      navigationBrokenCount: 0,
    });
  });
});
