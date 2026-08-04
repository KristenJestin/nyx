import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { PROTO_SHIM_SOURCE } from "./proto-shim";

/**
 * The shim is shipped as a STRING (inlined into the iframe with a CSP nonce). These tests run that
 * exact string against a jsdom document, proving the three declarative interactions PDR-0010
 * freezes: the states chip-bar (`window.__nyxStates` + applier), `data-nyx-page` navigation
 * (posted to the parent), and `data-nyx-state` flip-in-place. In jsdom `window.parent === window`.
 */

function installShim() {
  // Run the exact inlined shim source against the current jsdom document.
  (0, eval)(PROTO_SHIM_SOURCE);
}

/** Build the fixture DOM via the DOM API (no innerHTML). */
function buildFixture() {
  document.body.replaceChildren();
  const el = (tag: string, attrs: Record<string, string>, text?: string) => {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
    if (text != null) node.textContent = text;
    return node;
  };
  document.body.appendChild(el("div", { id: "depot-chip-bar" }));
  const root = el("div", { id: "depot-root" });
  root.appendChild(el("button", { id: "nav", "data-nyx-page": "catalogue" }, "Open catalogue"));
  root.appendChild(el("button", { id: "flip", "data-nyx-state": "loading" }, "Show loading"));
  root.appendChild(el("div", { id: "case-idle", "data-nyx-state-case": "idle" }, "IDLE VIEW"));
  root.appendChild(el("div", { id: "case-loading", "data-nyx-state-case": "loading" }, "LOADING"));
  document.body.appendChild(root);
}

describe("proto shim", () => {
  let postSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    buildFixture();
    (window as unknown as { __nyxStates: string[] }).__nyxStates = ["idle", "loading", "error"];
    (window as unknown as { __nyxActiveState: string }).__nyxActiveState = "idle";
    postSpy = vi.spyOn(window.parent, "postMessage").mockImplementation(() => {});
  });

  afterEach(() => {
    postSpy.mockRestore();
    document.body.replaceChildren();
  });

  test("builds a flippable chip-bar from window.__nyxStates and applies the initial state", () => {
    installShim();
    const chips = document.querySelectorAll<HTMLButtonElement>("#depot-chip-bar .depot-chip");
    expect([...chips].map((c) => c.textContent)).toEqual(["idle", "loading", "error"]);
    // Initial applier: idle active → idle case shown, loading case hidden.
    expect(document.body.dataset.nyxState).toBe("idle");
    expect(document.getElementById("case-idle")?.hidden).toBe(false);
    expect(document.getElementById("case-loading")?.hidden).toBe(true);
    const active = [...chips].find((c) => c.getAttribute("aria-pressed") === "true");
    expect(active?.dataset.state).toBe("idle");
  });

  test("clicking a chip flips the declarative state (applier reflects it)", () => {
    installShim();
    const loadingChip = document.querySelector<HTMLButtonElement>(
      '#depot-chip-bar .depot-chip[data-state="loading"]',
    );
    loadingChip?.click();
    expect(document.body.dataset.nyxState).toBe("loading");
    expect(document.getElementById("case-idle")?.hidden).toBe(true);
    expect(document.getElementById("case-loading")?.hidden).toBe(false);
    expect(loadingChip?.getAttribute("aria-pressed")).toBe("true");
  });

  test("data-nyx-page click posts a navigate message to the parent (blue nav)", () => {
    installShim();
    postSpy.mockClear();
    document.getElementById("nav")?.click();
    expect(postSpy).toHaveBeenCalledWith(
      expect.objectContaining({ source: "depot-frame", type: "navigate", page: "catalogue" }),
      "*",
    );
  });

  test("data-nyx-state click flips the state in place (green flip)", () => {
    installShim();
    document.getElementById("flip")?.click();
    expect(document.body.dataset.nyxState).toBe("loading");
    expect(document.getElementById("case-loading")?.hidden).toBe(false);
  });
});

/**
 * THE AXES APPLIER (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — the refonte of the variance model. What
 * makes a component vary is ALWAYS an axis: `variant` is one, `size` is one, and `loading` is one
 * with two values. Each value carries its CONCRETE classes, and the SHIM applies the active
 * combination onto the target. The template holds ONE element — no cartesian product, ever.
 */
describe("proto shim — named axes", () => {
  let postSpy: ReturnType<typeof vi.spyOn>;

  /** ONE button, styled by TWO orthogonal axes + a structural boolean. The real `button` shape. */
  function buildAxesFixture() {
    document.body.replaceChildren();
    const bar = document.createElement("div");
    bar.id = "depot-chip-bar";
    document.body.appendChild(bar);
    const root = document.createElement("div");
    root.id = "depot-root";
    const button = document.createElement("button");
    button.id = "btn";
    button.className = "inline-flex items-center rounded-md";
    const spinner = document.createElement("i");
    spinner.id = "spinner";
    spinner.setAttribute("data-nyx-if", "loading=on");
    const label = document.createElement("span");
    label.id = "label";
    label.setAttribute("data-nyx-if-not", "loading=on");
    button.appendChild(spinner);
    button.appendChild(label);
    root.appendChild(button);
    document.body.appendChild(root);
    return button;
  }

  function installAxes(values: Record<string, string>) {
    const w = window as unknown as Record<string, unknown>;
    w.__nyxAxes = [
      {
        name: "variant",
        values: [
          { name: "primary", classes: "bg-primary text-primary-foreground" },
          { name: "destructive", classes: "bg-destructive text-destructive-foreground" },
        ],
      },
      {
        name: "size",
        values: [
          { name: "md", classes: "h-9 px-4 text-sm" },
          { name: "lg", classes: "h-11 px-6 text-base" },
        ],
      },
      {
        name: "loading",
        values: [{ name: "off" }, { name: "on", classes: "pointer-events-none opacity-70" }],
      },
    ];
    w.__nyxAxisValues = values;
    w.__nyxStates = undefined;
    w.__nyxActiveState = undefined;
    installShim();
  }

  beforeEach(() => {
    postSpy = vi.spyOn(window.parent, "postMessage").mockImplementation(() => {});
  });
  afterEach(() => {
    postSpy.mockRestore();
    document.body.replaceChildren();
    const w = window as unknown as Record<string, unknown>;
    delete w.__nyxAxes;
    delete w.__nyxAxisValues;
  });

  test("applies the ACTIVE COMBINATION of every axis onto the component root", () => {
    const button = buildAxesFixture();
    installAxes({ variant: "destructive", size: "lg", loading: "off" });
    // The template's own classes are untouched, and each axis added its value's classes.
    expect([...button.classList].sort()).toEqual(
      [
        "inline-flex",
        "items-center",
        "rounded-md",
        "bg-destructive",
        "text-destructive-foreground",
        "h-11",
        "px-6",
        "text-base",
      ].sort(),
    );
  });

  /** AXIS BY AXIS: flipping `size` must not disturb `variant`. That is the whole promise. */
  test("each axis is piloted INDEPENDENTLY — no cartesian product anywhere", () => {
    const button = buildAxesFixture();
    installAxes({ variant: "primary", size: "md", loading: "off" });
    expect(button.classList.contains("bg-primary")).toBe(true);
    expect(button.classList.contains("h-9")).toBe(true);

    // Flip SIZE from its chip — variant stays where it is.
    document
      .querySelector<HTMLButtonElement>('.depot-chip[data-axis="size"][data-state="lg"]')
      ?.click();
    expect(button.classList.contains("h-11")).toBe(true);
    expect(button.classList.contains("h-9")).toBe(false);
    expect(button.classList.contains("bg-primary")).toBe(true);

    // Flip VARIANT — size stays at lg. The previous value's classes are removed, and ONLY them.
    document
      .querySelector<HTMLButtonElement>(
        '.depot-chip[data-axis="variant"][data-state="destructive"]',
      )
      ?.click();
    expect(button.classList.contains("bg-destructive")).toBe(true);
    expect(button.classList.contains("bg-primary")).toBe(false);
    expect(button.classList.contains("h-11")).toBe(true);
    // …and the template's own classes survived every flip.
    expect(button.classList.contains("rounded-md")).toBe(true);
  });

  /** A BOOLEAN IS AN AXIS WITH TWO VALUES — and it really changes the rendering, both ways. */
  test("a boolean axis changes the rendering: classes AND structure", () => {
    const button = buildAxesFixture();
    installAxes({ variant: "primary", size: "md", loading: "off" });
    expect(document.getElementById("spinner")?.hidden).toBe(true);
    expect(document.getElementById("label")?.hidden).toBe(false);
    expect(button.classList.contains("opacity-70")).toBe(false);

    document
      .querySelector<HTMLButtonElement>('.depot-chip[data-axis="loading"][data-state="on"]')
      ?.click();
    // STRUCTURE: the spinner appears, the label goes — what classes cannot do.
    expect(document.getElementById("spinner")?.hidden).toBe(false);
    expect(document.getElementById("label")?.hidden).toBe(true);
    // …and the classes of the same value landed on the root.
    expect(button.classList.contains("opacity-70")).toBe(true);
    expect(button.classList.contains("pointer-events-none")).toBe(true);
  });

  /**
   * THE HIDE IS AN ATTRIBUTE, SO IT WORKS ON ANY KIND OF ELEMENT (finding
   * `01KYSCW89NRM1TSZ0TFXC8HD36`). `hidden` is an IDL attribute of `HTMLElement` ONLY: the applier
   * used to do `el.hidden = !show`, which on an `<svg>` (an `SVGElement`) created a JS EXPANDO and
   * reflected NO attribute at all — so a spinner marked `data-nyx-if="loading=on"` stayed visible for
   * ever, and wrapping the very same svg in a `<span>` "fixed" it. A stylesheet can only select the
   * ATTRIBUTE, so that is what the shim writes, on every element type.
   *
   * That the attribute then WINS over Preflight and the display utilities is the frame's own rule —
   * `frame-srcdoc.browser.test.ts` and `composition-frame.browser.test.tsx` measure the computed
   * `display` in a real browser, which is the only place the question exists.
   */
  test("hides a NON-HTML element (an svg spinner) through the hidden ATTRIBUTE", () => {
    const button = buildAxesFixture();
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.id = "svg-spinner";
    svg.setAttribute("data-nyx-if", "loading=on");
    button.appendChild(svg);

    installAxes({ variant: "primary", size: "md", loading: "off" });
    const el = document.getElementById("svg-spinner")!;
    // The ATTRIBUTE is set — the only thing `[hidden]` can match…
    expect(el.hasAttribute("hidden")).toBe(true);
    // …and NOT a stray own property on the element, which is all the old assignment produced here.
    expect(Object.prototype.hasOwnProperty.call(el, "hidden")).toBe(false);

    // Reversible, on the same element: the attribute goes, the svg is back.
    document
      .querySelector<HTMLButtonElement>('.depot-chip[data-axis="loading"][data-state="on"]')
      ?.click();
    expect(el.hasAttribute("hidden")).toBe(false);
  });

  test("data-nyx-axis redirects an axis onto a chosen element", () => {
    document.body.replaceChildren();
    const root = document.createElement("div");
    root.id = "depot-root";
    const wrapper = document.createElement("div");
    wrapper.id = "wrapper";
    const icon = document.createElement("i");
    icon.id = "icon";
    icon.setAttribute("data-nyx-axis", "size");
    wrapper.appendChild(icon);
    root.appendChild(wrapper);
    document.body.appendChild(root);
    installAxes({ variant: "primary", size: "lg", loading: "off" });
    // `size` went to the marked element…
    expect(icon.classList.contains("h-11")).toBe(true);
    expect(wrapper.classList.contains("h-11")).toBe(false);
    // …while the unmarked `variant` still defaults to the root.
    expect(wrapper.classList.contains("bg-primary")).toBe(true);
  });

  test("one chip ROW per axis, and the combination is echoed to the host", () => {
    buildAxesFixture();
    installAxes({ variant: "primary", size: "md", loading: "off" });
    const rows = [...document.querySelectorAll("#depot-chip-bar .depot-chip-row")];
    expect(rows.map((r) => r.getAttribute("data-axis"))).toEqual(["variant", "size", "loading"]);
    expect(postSpy).toHaveBeenCalledWith(
      expect.objectContaining({
        source: "depot-frame",
        type: "axes",
        values: { variant: "primary", size: "md", loading: "off" },
      }),
      "*",
    );
  });

  test("data-nyx-set flips ANY axis in place", () => {
    const button = buildAxesFixture();
    const trigger = document.createElement("button");
    trigger.id = "to-lg";
    trigger.setAttribute("data-nyx-set", "size=lg");
    document.getElementById("depot-root")?.appendChild(trigger);
    installAxes({ variant: "primary", size: "md", loading: "off" });
    document.getElementById("to-lg")?.click();
    expect(button.classList.contains("h-11")).toBe(true);
    expect(document.body.getAttribute("data-nyx-axis-size")).toBe("lg");
  });

  /** A condition on an axis nobody declared is IGNORED — a blank aperçu is the worse failure. */
  test("a dangling condition never blanks the preview", () => {
    document.body.replaceChildren();
    const root = document.createElement("div");
    root.id = "depot-root";
    const el = document.createElement("span");
    el.id = "ghost";
    el.setAttribute("data-nyx-if", "busy=yes");
    root.appendChild(el);
    document.body.appendChild(root);
    installAxes({ variant: "primary", size: "md", loading: "off" });
    expect(document.getElementById("ghost")?.hidden).toBe(false);
  });
});

/**
 * THE LEGACY READ TOLERANCE (finding `01KYS14G4V1Q5ERRAHB0QVW362`). The vocabulary was renamed
 * `data-depot-*` → `data-nyx-*` / `window.__depot*` → `window.__nyx*`; migration 37 rewrote every
 * STORED template and nothing emits or teaches the old prefix any more. But a template can still
 * reach the frame by ANOTHER path (a synthesis in flight, an import, an older export), and a shim
 * that ignored it would render the worst possible failure: every state case at once, a spinner
 * stuck on, a dead nav — a page that reads as a broken renderer.
 *
 * These tests are the ones that EXERCISE the tolerance block: the old spelling still drives the
 * whole model, the new spelling WINS when both are present, and — the other half of the contract —
 * the shim itself only ever EMITS the new one.
 */
describe("proto shim — the legacy prefix is still READ (never written)", () => {
  let postSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    postSpy = vi.spyOn(window.parent, "postMessage").mockImplementation(() => {});
  });
  afterEach(() => {
    postSpy.mockRestore();
    document.body.replaceChildren();
    const w = window as unknown as Record<string, unknown>;
    for (const k of [
      "__nyxAxes",
      "__nyxAxisValues",
      "__nyxStates",
      "__nyxActiveState",
      "__depotAxes",
      "__depotAxisValues",
      "__depotStates",
      "__depotActiveState",
    ]) {
      delete w[k];
    }
  });

  /** A pre-rename template + pre-rename globals: EXACTLY what an unmigrated frame looks like. */
  function buildLegacyFixture() {
    document.body.replaceChildren();
    const el = (tag: string, attrs: Record<string, string>) => {
      const node = document.createElement(tag);
      for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
      return node;
    };
    document.body.appendChild(el("div", { id: "depot-chip-bar" }));
    const root = el("div", { id: "depot-root" });
    root.appendChild(el("button", { id: "nav", "data-depot-page": "catalogue" }));
    root.appendChild(el("button", { id: "flip", "data-depot-state": "loading" }));
    root.appendChild(el("button", { id: "to-lg", "data-depot-set": "size=lg" }));
    root.appendChild(el("div", { id: "case-idle", "data-depot-state-case": "idle" }));
    root.appendChild(el("div", { id: "case-loading", "data-depot-state-case": "loading" }));
    root.appendChild(el("i", { id: "spinner", "data-depot-if": "state=loading" }));
    root.appendChild(el("span", { id: "label", "data-depot-if-not": "state=loading" }));
    document.body.appendChild(root);
  }

  test("an UNMIGRATED template still renders its structure instead of a blank page", () => {
    buildLegacyFixture();
    const w = window as unknown as Record<string, unknown>;
    w.__depotStates = ["idle", "loading"];
    w.__depotActiveState = "idle";
    installShim();

    // The legacy globals still declare the axis…
    expect(
      [...document.querySelectorAll("#depot-chip-bar .depot-chip")].map((c) => c.textContent),
    ).toEqual(["idle", "loading"]);
    // …and the legacy structural attributes are still obeyed: ONE case, not both.
    expect(document.getElementById("case-idle")?.hidden).toBe(false);
    expect(document.getElementById("case-loading")?.hidden).toBe(true);
    expect(document.getElementById("spinner")?.hidden).toBe(true);
    expect(document.getElementById("label")?.hidden).toBe(false);
  });

  test("the legacy click affordances still work (page, state flip, set)", () => {
    buildLegacyFixture();
    const w = window as unknown as Record<string, unknown>;
    w.__depotAxes = [
      { name: "state", values: [{ name: "idle" }, { name: "loading" }] },
      { name: "size", values: [{ name: "md" }, { name: "lg", classes: "h-11" }] },
    ];
    w.__depotAxisValues = { state: "idle", size: "md" };
    installShim();

    postSpy.mockClear();
    document.getElementById("nav")?.click();
    expect(postSpy).toHaveBeenCalledWith(
      expect.objectContaining({ type: "navigate", page: "catalogue" }),
      "*",
    );

    document.getElementById("flip")?.click();
    expect(document.getElementById("case-loading")?.hidden).toBe(false);
    expect(document.getElementById("spinner")?.hidden).toBe(false);

    document.getElementById("to-lg")?.click();
    expect(document.body.getAttribute("data-nyx-axis-size")).toBe("lg");
  });

  test("a legacy data-depot-axis still redirects an axis onto its element", () => {
    document.body.replaceChildren();
    const root = document.createElement("div");
    root.id = "depot-root";
    const wrapper = document.createElement("div");
    wrapper.id = "wrapper";
    const icon = document.createElement("i");
    icon.id = "icon";
    icon.setAttribute("data-depot-axis", "size");
    wrapper.appendChild(icon);
    root.appendChild(wrapper);
    document.body.appendChild(root);
    const w = window as unknown as Record<string, unknown>;
    w.__depotAxes = [{ name: "size", values: [{ name: "lg", classes: "h-11" }] }];
    installShim();
    expect(icon.classList.contains("h-11")).toBe(true);
    expect(wrapper.classList.contains("h-11")).toBe(false);
  });

  /** Belt AND braces: a half-migrated document (both spellings) obeys the NEW one. */
  test("the new prefix WINS when an element carries both spellings", () => {
    document.body.replaceChildren();
    const root = document.createElement("div");
    root.id = "depot-root";
    const el = document.createElement("span");
    el.id = "both";
    el.setAttribute("data-depot-if", "loading=on");
    el.setAttribute("data-nyx-if", "loading=off");
    root.appendChild(el);
    document.body.appendChild(root);
    const w = window as unknown as Record<string, unknown>;
    w.__nyxAxes = [{ name: "loading", values: [{ name: "off" }, { name: "on" }] }];
    w.__nyxAxisValues = { loading: "off" };
    w.__depotAxes = [{ name: "loading", values: [{ name: "on" }] }];
    installShim();
    // Read through the NEW attribute (`loading=off` matches) and the NEW globals.
    expect(document.getElementById("both")?.hidden).toBe(false);
  });

  /** The tolerance is READ-ONLY: nothing the shim WRITES carries the old prefix. */
  test("the shim emits the NEW prefix only", () => {
    buildLegacyFixture();
    const w = window as unknown as Record<string, unknown>;
    w.__depotStates = ["idle", "loading"];
    w.__depotActiveState = "loading";
    installShim();
    expect(document.body.dataset.nyxState).toBe("loading");
    expect(document.body.getAttribute("data-nyx-axis-state")).toBe("loading");
    expect(document.body.getAttribute("data-depot-state")).toBeNull();
    expect(document.body.getAttribute("data-depot-axis-state")).toBeNull();
    // …and no element gained an old-prefix attribute from the applier.
    expect(document.querySelectorAll("[data-depot-axis-state]").length).toBe(0);
  });

  /**
   * DESIGNATING A NODE (finding `01KYW32N1BVDVTGS0WPMG2PJW2`) — the render IS the composition.
   *
   * The node names are in the markup because the SIDECAR STAMPED THEM there: `data-nyx-node` on the
   * root element(s) of each node's own markup (`render.rs`'s `NODE_ATTR`, finding
   * `01KYW77Y65A9HYNDZ69M67S6B8`). The shim walks up from the clicked element to the nearest mark.
   * Ported from depot's feedback shim MINUS its `computeSelector`: here the anchor is a NODE of the
   * named tree, never a CSS selector.
   *
   * The markup here is a hand-written stand-in for what the render emits; the BYTE-EXACT article
   * Rust really produces is replayed through this same shim in `shim-seam.test.ts`, off
   * `contract/design-fixtures.json`.
   */
  describe("designating a node", () => {
    /** `Root` > (`Root/Card` > `Root/Card/Save`) + `Root/Aside`, marked as the sidecar emits them. */
    function buildMarkedFixture() {
      document.body.replaceChildren();
      const root = document.createElement("div");
      root.id = "depot-root";
      root.innerHTML =
        `<div data-nyx-node="Root">` +
        `<section id="card" data-nyx-node="Root/Card">` +
        `<button id="save" data-nyx-node="Root/Card/Save"><span id="label">Save</span></button>` +
        `</section>` +
        `<aside id="aside" data-nyx-node="Root/Aside"></aside>` +
        `</div>`;
      document.body.appendChild(root);
    }

    function host(message: Record<string, unknown>) {
      window.dispatchEvent(
        new MessageEvent("message", { data: { source: "depot-host", ...message } }),
      );
    }

    /**
     * The DISTINCT messages of one type the shim posted, as JSON. Every test in this file installs
     * the shim into the SAME jsdom document, and a document-level listener cannot be uninstalled —
     * so one click is answered by every instance ever installed, with the same payload. What is
     * asserted is therefore the SET of answers, never how many times it was given.
     */
    function postedOfType(type: string): unknown[] {
      const calls = postSpy.mock.calls as unknown as unknown[][];
      const all = calls.map((c) => c[0] as Record<string, unknown>);
      const seen: string[] = all.filter((m) => m.type === type).map((m) => JSON.stringify(m));
      return [...new Set(seen)].map((m) => JSON.parse(m) as unknown);
    }

    beforeEach(() => {
      buildMarkedFixture();
      delete (window as unknown as Record<string, unknown>).__nyxStates;
      delete (window as unknown as Record<string, unknown>).__nyxActiveState;
    });

    test("resolves a click to the DEEPEST node the element belongs to", () => {
      installShim();
      host({ type: "pick-mode", mode: "select" });
      postSpy.mockClear();

      // The span is inside the button, which is inside the card: the deepest region wins.
      document.getElementById("label")!.click();
      expect(postedOfType("select")).toEqual([
        { source: "depot-frame", type: "select", nodeKey: "Root/Card/Save" },
      ]);

      postSpy.mockClear();
      document.getElementById("aside")!.click();
      expect(postedOfType("select")).toEqual([
        { source: "depot-frame", type: "select", nodeKey: "Root/Aside" },
      ]);
    });

    test("an element in the ROOT node's own markup resolves to the root", () => {
      // The root node names ITSELF now, like every other: `#depot-root` is the host's wrapper, not a
      // node, so an element the root node draws resolves by the same ancestor walk as any other.
      const loose = document.createElement("p");
      document.querySelector('[data-nyx-node="Root"]')!.appendChild(loose);
      installShim();
      host({ type: "pick-mode", mode: "select" });
      postSpy.mockClear();
      loose.click();
      expect(postedOfType("select")).toEqual([
        { source: "depot-frame", type: "select", nodeKey: "Root" },
      ]);
    });

    test("select mode never steals a declared interaction (nav wins over designation)", () => {
      const root = document.querySelector<HTMLElement>('[data-nyx-node="Root"]')!;
      const link = document.createElement("a");
      link.id = "nav-link";
      link.setAttribute("data-nyx-page", "settings");
      root.insertBefore(link, root.firstChild);
      installShim();
      host({ type: "pick-mode", mode: "select" });
      postSpy.mockClear();

      link.click();
      expect(postedOfType("navigate")).toEqual([
        { source: "depot-frame", type: "navigate", page: "settings" },
      ]);
      expect(postedOfType("select")).toEqual([]);
    });

    test("designates nothing until the host asks for a mode", () => {
      installShim();
      // Disarm EVERY instance living in this document (see `postedOfType`), then click.
      host({ type: "pick-mode", mode: "off" });
      postSpy.mockClear();
      document.getElementById("save")!.click();
      expect(postedOfType("select")).toEqual([]);
    });

    test("pin mode mounts a capture layer that swallows the click and posts the node + point", () => {
      installShim();
      host({ type: "pick-mode", mode: "pin" });
      const layer = document.querySelector<HTMLElement>(".nyx-pick-capture");
      expect(layer).toBeTruthy();
      expect(layer?.getAttribute("aria-hidden")).toBe("true");

      // jsdom implements no layout and therefore no `elementFromPoint`; it is stubbed so the layer's
      // OWN resolution (the element under the cursor, disabled controls included) is what is
      // asserted, together with the point that anchors the composer.
      const doc = document as unknown as {
        elementFromPoint?: (x: number, y: number) => Element | null;
      };
      doc.elementFromPoint = () => document.getElementById("save");
      postSpy.mockClear();
      layer!.dispatchEvent(new MouseEvent("click", { bubbles: true, clientX: 120, clientY: 64 }));
      delete doc.elementFromPoint;

      expect(postedOfType("pin")).toEqual([
        {
          source: "depot-frame",
          type: "pin",
          nodeKey: "Root/Card/Save",
          x: 120,
          y: 64,
        },
      ]);

      // Disarming takes the layer away — plain reading of the page is untouched.
      host({ type: "pick-mode", mode: "off" });
      expect(document.querySelector(".nyx-pick-capture")).toBeNull();
    });

    test("rings the node's OWN root, never every descendant of it", () => {
      installShim();
      host({ type: "highlight-node", nodeKey: "Root/Card/Save" });
      // The button is the node's markup; the span inside it is the button's own template, not a
      // node — outlining it too draws a second ring inside the first (seen in the running app).
      expect(document.getElementById("save")?.classList.contains("nyx-pick-selected")).toBe(true);
      expect(document.getElementById("label")?.classList.contains("nyx-pick-selected")).toBe(false);
      // …and a click on that same span still resolves to the node: designating and RINGING are two
      // questions, and only the ring is limited to the region's roots.
      expect(document.getElementById("label")).toBeTruthy();
      host({ type: "highlight-node", nodeKey: null });
    });

    test("rings the selected node, and the already-commented ones only while pinning", () => {
      installShim();
      host({ type: "highlight-node", nodeKey: "Root/Aside" });
      expect(document.getElementById("aside")?.classList.contains("nyx-pick-selected")).toBe(true);

      // A new selection clears the previous ring rather than accumulating.
      host({ type: "highlight-node", nodeKey: "Root/Card" });
      expect(document.getElementById("aside")?.classList.contains("nyx-pick-selected")).toBe(false);
      expect(document.getElementById("card")?.classList.contains("nyx-pick-selected")).toBe(true);

      // The "already commented" marks belong to PIN mode (depot's own rule).
      host({ type: "pinned-nodes", nodeKeys: ["Root/Card/Save"] });
      expect(document.getElementById("save")?.classList.contains("nyx-pick-pinned")).toBe(false);
      host({ type: "pick-mode", mode: "pin" });
      expect(document.getElementById("save")?.classList.contains("nyx-pick-pinned")).toBe(true);
      host({ type: "pick-mode", mode: "off" });
      expect(document.getElementById("save")?.classList.contains("nyx-pick-pinned")).toBe(false);
    });

    test("ignores a message that does not name the host", () => {
      installShim();
      window.dispatchEvent(
        new MessageEvent("message", { data: { type: "pick-mode", mode: "pin" } }),
      );
      expect(document.querySelector(".nyx-pick-capture")).toBeNull();
    });
  });
});
