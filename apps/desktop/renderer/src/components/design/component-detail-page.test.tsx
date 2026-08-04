import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";

import type { DesignComponentRow } from "./types";
import type { RenderedCssSlot } from "./use-design-render";

const COMPONENT: DesignComponentRow = {
  id: "c1",
  project_id: "p1",
  name: "Button",
  source_repository_id: null,
  source_path: "src/ui/Button.tsx",
  visual_hash: null,
  template: '<button data-state="{{state}}">{{label}}<i>{{{body}}}</i></button>',
  params_json: JSON.stringify([
    { name: "label", type: "text", default: "Hello" },
    { name: "body", type: "html", default: "<b>hi</b>" },
  ]),
  states_json: JSON.stringify(["idle", "loading"]),
  created_at: 0,
  updated_at: 0,
  scanned_input_hash: null,
  orphaned_at: null,
};

/**
 * A component with MANY declared states — the shape that broke the panel (finding
 * `01KYPJ1SCHGZ2STSXZ4VCR1K33`: a `button`'s 8 CVA variants in a one-row segmented control put a
 * horizontal scrollbar in the params panel).
 */
const MANY_STATES = [
  "default",
  "destructive",
  "outline",
  "secondary",
  "accent",
  "ghost",
  "ghost-destructive",
  "link",
];
const MANY_VARIANTS: DesignComponentRow = {
  ...COMPONENT,
  states_json: JSON.stringify(MANY_STATES),
};

/**
 * The DEGENERATE component of finding `01KYPSG1HM2GPJMNH2SP3DDADH`: three variants DECLARED, one
 * hard-coded rendering — the exact synthesis a test agent produced from a manual that never
 * mentioned `data-nyx-state-case`. Its state control cannot change one pixel.
 */
const NO_STATE_CASES: DesignComponentRow = {
  ...COMPONENT,
  template: '<button class="bg-primary px-4">{{label}}</button>',
  params_json: JSON.stringify([{ name: "label", type: "text", preview: "Enregistrer" }]),
  states_json: JSON.stringify(["default", "destructive", "outline"]),
};

/** The same component, synthesized ACCORDING to the contract but with one state left unrendered. */
const PARTIAL_STATE_CASES: DesignComponentRow = {
  ...NO_STATE_CASES,
  template:
    '<button data-nyx-state-case="default" class="bg-primary">{{label}}</button>' +
    '<button data-nyx-state-case="destructive" class="bg-destructive">{{label}}</button>',
};

/**
 * A component STORED BEFORE the param-type contract was written down (finding
 * `01KYPSG0HPG2D28CMJEXRKNEVK`): a guessed `"enum"` beside a correct `select`. The unknown type is
 * still coerced to `text` — a stored row must keep rendering — but the coercion is now SAID.
 */
const LEGACY_ENUM: DesignComponentRow = {
  ...COMPONENT,
  // Every declared param is substituted SOMEWHERE: a field the template never reads is not offered
  // at all (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point A), which is a different test's subject.
  template: '<button class="text-{{align}} {{tone}}">{{buttonVariant}}<i>{{unit}}</i></button>',
  params_json: JSON.stringify([
    { name: "buttonVariant", type: "enum", options: ["default", "destructive"] },
    { name: "align", type: "select", options: ["left", "center", "right"], default: "left" },
    { name: "unit", type: "select" },
    { name: "tone", type: "select", options: MANY_STATES },
  ]),
  states_json: null,
};

/**
 * A component whose params are REQUIRED and carry a PREVIEW value (finding
 * `01KYPJ1RC6WJX1Z9CPPV23FG27`) — a `button` has no default label, the caller must provide one.
 * `NODE` sets `label` but NOT `hint`, which is how the preview/persistence split is observable.
 */
const REQUIRED_PREVIEW: DesignComponentRow = {
  ...COMPONENT,
  template: "<button>{{label}}<small>{{hint}}</small></button>",
  params_json: JSON.stringify([
    { name: "label", type: "text", required: true, preview: "Enregistrer" },
    { name: "hint", type: "text", required: true, preview: "Astuce" },
  ]),
  states_json: null,
};

/**
 * A component the inventory DISCOVERED but that NO agent has synthesized yet (finding
 * `01KYSCW8RY8JKAR50C42QQ0B62`) — an EMPTY template, nothing declared: the catalogue's
 * "to synthesize" row, opened in the detail.
 */
const PENDING: DesignComponentRow = {
  ...COMPONENT,
  name: "Sheet",
  template: "",
  params_json: null,
  states_json: null,
  scanned_input_hash: "5d41402abc4b2a76b9719d911017c592",
};

/** The component `design_component_get` answers with — swappable per test. */
let currentComponent: DesignComponentRow = COMPONENT;

/** The `css_slots[]` the sidecar's `design_render` answers with — swappable per test. */
let currentCssSlots: RenderedCssSlot[] = [];

/**
 * The ONE strict escape hatch `Button` may carry — the shape `design_render` sends back
 * (`css_slots[]`, one entry per COMPONENT, never per placement).
 */
const BUTTON_SLOT: RenderedCssSlot = {
  scope_id: "c-button-4d1c",
  component: "Button",
  scope: '[data-nyx-css~="c-button-4d1c"]',
  reason: "a custom press keyframe — no Tailwind utility expresses a keyframe",
  features: ["keyframes"],
  bytes: 132,
  css: '[data-nyx-css~="c-button-4d1c"]{animation:c-button-4d1c-press 120ms ease-out}',
  refused: null,
  detail: null,
};

/** Records every backend call so a test can assert the persistence payload. */
let calls: { cmd: string; args: unknown }[] = [];

/**
 * A stand-in for the sidecar's `design_render`: substitutes the composition's params into the
 * component's template, exactly as the real one does for a logic-less template. It is what lets a
 * test OBSERVE that a param change changes the RENDERING, rather than a constant string. The
 * `css_slots` travel WITH the markup, exactly as they do on the wire.
 */
function fakeRender(compositionJson: string): {
  markup: string;
  css_slots: readonly RenderedCssSlot[];
  css_slot_count: number;
} {
  const { params } = JSON.parse(compositionJson) as { params: Record<string, unknown> };
  const markup = currentComponent.template
    .replace(/\{\{\{(\w+)\}\}\}/g, (_m, k: string) => String(params[k] ?? ""))
    .replace(/\{\{(\w+)\}\}/g, (_m, k: string) => String(params[k] ?? ""));
  return { markup, css_slots: currentCssSlots, css_slot_count: currentCssSlots.length };
}

function installBackend() {
  calls = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "list_projects":
        return [
          {
            id: "p1",
            name: "Project",
            key: "NYX",
            collapsed: false,
            created_at: 0,
            updated_at: 0,
            resume_agent_sessions: false,
          },
        ];
      case "list_workspaces":
        return [];
      case "design_get":
        return null;
      case "design_component_get":
        return currentComponent;
      case "design_render":
        return fakeRender((args as { compositionJson: string }).compositionJson);
      default:
        return undefined;
    }
  });
}

/** Mount the detail at `/design/c1` — the ONE way in, a catalogue card. */
function renderDetail() {
  const router = createAppRouter(["/design/c1"]);
  return { router, ...render(<RouterProvider router={router} />) };
}

describe("ComponentDetailPage", () => {
  beforeEach(() => {
    localStorage.clear();
    currentComponent = COMPONENT;
    currentCssSlots = [];
    installBackend();
  });

  it("renders editable params (state + text) with a live preview", async () => {
    renderDetail();
    expect(await screen.findByRole("heading", { name: "Button" })).toBeTruthy();
    // A text param control (seeded with its default) and the STATE param as a segmented picker.
    const label = (await screen.findByLabelText("label")) as HTMLInputElement;
    expect(label.value).toBe("Hello");
    expect(screen.getByRole("group", { name: "State" })).toBeTruthy();
    // The live preview iframe (once the sidecar render resolves).
    expect(await screen.findByTitle("Button live preview")).toBeTruthy();
  });

  /**
   * A COMPONENT WITH NOTHING TO PREVIEW SHOWS NO PREVIEW (finding `01KYSCW8RY8JKAR50C42QQ0B62`).
   * Opening a « à synthétiser » card mounted the frame on an EMPTY template: an empty document whose
   * canvas is the project's `--background`, i.e. a big white rectangle for a light design system,
   * indistinguishable from a broken render. The vignette had always short-circuited that case; the
   * detail now does the same — no frame, and no `design_render` spent on a result nobody can use.
   */
  it("shows an explicit state instead of a preview for an unsynthesized component", async () => {
    currentComponent = PENDING;
    renderDetail();

    expect(await screen.findByRole("heading", { name: "Sheet" })).toBeTruthy();
    // The state SAYS what is missing and WHO produces it — the agent, by its tool. In ENGLISH, like
    // every other sentence of this app (finding `01KYT10VE8624MKQ18AFHSE0W0`): this block was the
    // one French island of the design surfaces, and it read as a bug of its own.
    const notice = await screen.findByTestId("detail-not-synthesized");
    expect(notice.textContent).toContain("Not described yet");
    // …WHO produces it, never WHICH TOOL does (finding `01KZ3JW5A63TJWX83V8ZKX62CN`): the four-line
    // paragraph quoted an internal tool, an empty `template` and a stored row, to conclude there was
    // nothing for the reader to do.
    expect(notice.textContent).toContain("The agent does that");
    expect(notice.textContent).not.toContain("design_refresh_component");
    expect(notice.textContent).not.toContain("template");
    expect(notice.textContent).not.toMatch(/synthétis|inventorié|aperçu|synthèse/i);
    // NO frame at all — not even one on an empty document.
    expect(screen.queryByTitle("Sheet live preview")).toBeNull();
    expect(document.querySelector("iframe")).toBeNull();
    // …and not a single render was asked for.
    expect(calls.filter((c) => c.cmd === "design_render")).toEqual([]);
  });

  /** The counterpart: a SYNTHESIZED component still previews, and the notice is absent. */
  it("keeps the live preview (and no such notice) once the component IS synthesized", async () => {
    renderDetail();
    expect(await screen.findByTitle("Button live preview")).toBeTruthy();
    expect(screen.queryByTestId("detail-not-synthesized")).toBeNull();
    expect(calls.filter((c) => c.cmd === "design_render").length).toBeGreaterThan(0);
  });

  /**
   * ONE state control, and it is the PARAM (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`). The page used to
   * carry two: the `state` param in the panel AND the frame's chip-bar, unreconciled. The panel wins
   * — it is the face that gets PERSISTED per node — so the chip-bar is gone from THIS host. The
   * states still reach the shim (it must still apply the active one), and the proto-viewer, which
   * has no param panel, keeps its chips (`prototype-tab.test`).
   */
  it("shows the state ONCE — the param panel, not a chip-bar", async () => {
    renderDetail();
    const srcdoc = (await screen.findByTitle("Button live preview")).getAttribute("srcdoc") ?? "";
    expect(srcdoc).not.toContain(`id="depot-chip-bar"`);
    // The states are still handed to the shim — only the duplicate CONTROL went away.
    expect(srcdoc).toContain(`window.__nyxStates=["idle","loading"]`);
    // …and the single control is the param, in the panel.
    expect(screen.getByRole("group", { name: "State" })).toBeTruthy();
  });

  /**
   * THE BUG ITSELF: selecting a state must change what is RENDERED. `buildFrameSrcdoc` always read
   * an `activeState`, but nothing ever fed it, so the shim fell back to `states[0]` on every
   * re-render and the control was inert BY CONSTRUCTION. Both halves of the chaining are asserted:
   * the recompiled MARKUP (the composition sent to `design_render` carries the selected state) and
   * the shim's own `window.__nyxActiveState`, which decides which `[data-nyx-state-case]` shows.
   */
  it("selecting a state changes the rendering (markup AND the shim's active state)", async () => {
    renderDetail();
    const frameTitle = "Button live preview";
    // Seeded on the first declared state.
    await waitFor(() => {
      const srcdoc = screen.getByTitle(frameTitle).getAttribute("srcdoc") ?? "";
      expect(srcdoc).toContain(`data-state="idle"`);
      expect(srcdoc).toContain(`window.__nyxActiveState="idle"`);
    });

    fireEvent.click(screen.getByRole("button", { name: "loading" }));

    await waitFor(() => {
      const srcdoc = screen.getByTitle(frameTitle).getAttribute("srcdoc") ?? "";
      // 1. The MARKUP was recompiled with the selected state…
      expect(srcdoc).toContain(`data-state="loading"`);
      expect(srcdoc).not.toContain(`data-state="idle"`);
      // 2. …and the shim is told to APPLY it (this is the link that was missing).
      expect(srcdoc).toContain(`window.__nyxActiveState="loading"`);
    });
    // The composition actually sent to the sidecar carried the new state.
    const last = [...calls].reverse().find((c) => c.cmd === "design_render");
    expect(
      JSON.parse((last!.args as { compositionJson: string }).compositionJson) as unknown,
    ).toMatchObject({ params: { state: "loading" } });
  });

  /**
   * NO MORE SILENT DEGRADATION OF A CONSTRAINED CHOICE (finding `01KYPSG0HPG2D28CMJEXRKNEVK`).
   * "J'ai bien fourni options:[...] sur buttonVariant — l'UI les a ignorées et a affiché un champ
   * texte libre." The word was wrong (`enum` is not one of the six) and nothing said so: the parser
   * fell back to `text`, taking the options with it. A NEW write is now refused outright by
   * `design_refresh_component`; a row stored before that guard still renders — with the coercion
   * NAMED beside the control, so a free field never passes for a constrained one.
   */
  it("names a coerced param type instead of silently showing a free-text field", async () => {
    currentComponent = LEGACY_ENUM;
    renderDetail();

    const note = await screen.findByTestId("param-type-note-buttonVariant");
    expect(note.textContent).toContain("enum → text");
    expect(note.getAttribute("title")).toContain("text, html, state, boolean, number, select");
    // The control itself is the honest one: a plain text input (the type it was coerced to).
    expect((screen.getByLabelText("buttonVariant") as HTMLInputElement).type).toBe("text");
    // …and the OTHER silent degradation, an options-less `select`, is named the same way.
    expect(screen.getByTestId("param-type-note-unit").textContent).toContain("no options");
  });

  /**
   * …and the DOCUMENTED way to declare a constrained value works, from the same fixture: a `select`
   * WITH its options is a real picker, and picking a value reaches the rendering.
   */
  it("renders a select param with options as a picker that drives the preview", async () => {
    currentComponent = LEGACY_ENUM;
    renderDetail();

    const picker = await screen.findByRole("group", { name: "align" });
    expect([...picker.querySelectorAll("button")].map((b) => b.textContent)).toEqual([
      "left",
      "center",
      "right",
    ]);
    // No coercion note on a well-formed select.
    expect(screen.queryByTestId("param-type-note-align")).toBeNull();
    // …and past the segmented threshold the same `select` is a DROPDOWN (constant width, whatever
    // the synthesis declared — finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`), not a free-text field either.
    const dropdown = screen.getByRole("combobox", { name: "tone" }) as HTMLSelectElement;
    expect([...dropdown.options].map((o) => o.value)).toEqual(MANY_STATES);

    fireEvent.click(screen.getByRole("button", { name: "center" }));
    await waitFor(() => {
      expect(screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "").toContain(
        "text-center",
      );
    });
  });

  /**
   * A STATE CONTROL THAT CANNOT WORK SAYS SO (finding `01KYPSG1HM2GPJMNH2SP3DDADH`). The variant
   * mechanism is the shim's: it shows the `[data-nyx-state-case="<active>"]` element and hides
   * the others, so a state only renders if the SYNTHESIS put a case in the template. A component
   * that declares states and carries none used to give a fully-functional-looking dropdown over a
   * frozen rendering — "les dropdowns de variante sont cosmétiques". The page names the gap instead.
   */
  it("says when the template carries no state case (the dropdown that changes nothing)", async () => {
    currentComponent = NO_STATE_CASES;
    renderDetail();
    const note = await screen.findByTestId("state-cases-missing");
    expect(note.textContent).toContain("data-nyx-state-case");
    // The generalized wording (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): what is inert is an axis
    // VALUE — no classes and no structural case — and the three of them are named.
    expect(note.textContent).toContain("3 axis values");
    expect(note.textContent).toContain("default, destructive, outline");
    // The control is still there (the states ARE declared — what is missing is the rendering).
    expect(screen.getByRole("group", { name: "State" })).toBeTruthy();
  });

  /** The PARTIAL defect: a declared state with no case hides every case — an EMPTY aperçu. */
  it("names the declared states the template renders no case for", async () => {
    currentComponent = PARTIAL_STATE_CASES;
    renderDetail();
    const note = await screen.findByTestId("axis-cases-missing");
    expect(note.textContent).toContain("outline");
    expect(note.textContent).not.toContain("destructive");
  });

  /** …and a synthesis that HONOURS the contract is flagged for nothing. */
  it("stays silent when every declared state has its case", async () => {
    currentComponent = {
      ...PARTIAL_STATE_CASES,
      states_json: JSON.stringify(["default", "destructive"]),
    };
    renderDetail();
    expect(await screen.findByTitle("Button live preview")).toBeTruthy();
    expect(screen.queryByTestId("state-cases-missing")).toBeNull();
    expect(screen.queryByTestId("axis-cases-missing")).toBeNull();
  });

  /**
   * The state control must never widen the panel, whatever the synthesis declared (finding
   * `01KYPJ1SCHGZ2STSXZ4VCR1K33`: 8 variant names in one segmented row = a horizontal scrollbar).
   * Past `SEGMENTED_OPTIONS_MAX` the control BECOMES a dropdown — constant width by construction.
   * The pixel proof lives in `param-field.browser.test.tsx`; here we pin the SWITCH.
   */
  it("uses a dropdown for the state when the component declares many variants", async () => {
    currentComponent = MANY_VARIANTS;
    renderDetail();
    const select = (await screen.findByRole("combobox", { name: "State" })) as HTMLSelectElement;
    expect([...select.options].map((o) => o.value)).toEqual(MANY_STATES);
    // No segmented row of 8 buttons beside it.
    expect(screen.queryByRole("group", { name: "State" })).toBeNull();

    // …and it is a real control: picking a variant changes the rendering, same chaining as above.
    fireEvent.change(select, { target: { value: "ghost-destructive" } });
    await waitFor(() => {
      const srcdoc = screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "";
      expect(srcdoc).toContain(`data-state="ghost-destructive"`);
      expect(srcdoc).toContain(`window.__nyxActiveState="ghost-destructive"`);
    });
  });

  /**
   * "Je peux pas modifier le texte" (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`): the CONTENT params must
   * be editable from the panel and reach the preview — the text one (`text`) and the raw one
   * (`html`, a textarea: a content param can be plain text OR html, PDR-0010).
   */
  it("edits the content params (text AND html) and the preview follows", async () => {
    renderDetail();
    fireEvent.change(await screen.findByLabelText("label"), { target: { value: "Enregistrer" } });
    fireEvent.change(screen.getByLabelText("body"), { target: { value: "<em>ok</em>" } });

    await waitFor(() => {
      const srcdoc = screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "";
      expect(srcdoc).toContain("Enregistrer");
      // `{{{body}}}` is raw: the html param lands as markup, not as escaped text.
      expect(srcdoc).toContain("<em>ok</em>");
    });
  });

  /**
   * The other half of the CHROME, same split (finding `01KYPEG16E95WADT2VMCYBPCK5`): the dashed
   * `data-nyx-*` outlines say WHAT IS CLICKABLE. The catalogue vignette dropped them because
   * nothing is clickable there (`component-card.test` pins the absence); the DETAIL is a live,
   * interactive preview — it keeps them, exactly like the proto-viewer (`prototype-tab.test`).
   */
  it("keeps the data-nyx-* affordance outlines in the live preview", async () => {
    renderDetail();
    const srcdoc = (await screen.findByTitle("Button live preview")).getAttribute("srcdoc") ?? "";
    // Only the document's own styles: the inlined Tailwind engine that follows mentions every
    // utility name, `dashed` included.
    const css = srcdoc.slice(0, Math.max(srcdoc.indexOf("<script"), 0));
    expect(css).toContain("[data-nyx-page]{cursor:pointer;outline:1px dashed #2563eb");
    expect(css).toContain("[data-nyx-state]:not(body){cursor:pointer;outline:1px dashed #16a34a");
  });

  /**
   * THE VIGNETTE FIX, EXTENDED HERE (finding `01KYSA1N8SDT012R6E9XFXFJB0`). The card's finding
   * `01KYPEG16E95WADT2VMCYBPCK5` had put the detail out of scope, so this pane still carried the two
   * defects the card lost: a hard `bg-white` plate — glaring, since the app is dark — and no layout
   * at all, which glued the instance to the TOP-LEFT of a full-height pane. Both are pinned here, at
   * the DETAIL's own call site; the real geometry and the computed colour are measured in a browser
   * (`component-detail-preview.browser.test.tsx`).
   */
  it("centres the live preview and paints no hard-coded white", async () => {
    renderDetail();
    const frame = await screen.findByTitle("Button live preview");
    const srcdoc = frame.getAttribute("srcdoc") ?? "";
    const css = srcdoc.slice(0, Math.max(srcdoc.indexOf("<script"), 0));

    // 1. CENTRED — the very layout the vignette asks for, `safe center` included, so a full-page
    //    component starts at the top-left instead of being cropped on both sides.
    expect(css).toContain("align-items:safe center;justify-content:safe center");
    expect(css).toContain("max-width:100%;max-height:100%;overflow:hidden}");
    // 2. The canvas follows the PROJECT's background token (transparent when it declares none), and
    //    the no-token text fallback is readable on the app's dark surface.
    expect(css).toContain("background-color:var(--background,var(--color-background,transparent))");
    expect(css).toContain("color:var(--foreground,var(--color-foreground,#e2e8f0))");
    // 3. NO literal white left, neither on the frame nor on any container up to the pane.
    expect(frame.className).not.toContain("bg-white");
    for (let el = frame.parentElement; el !== null; el = el.parentElement) {
      expect(el.className).not.toContain("bg-white");
    }
    // 4. …and the frame FILLS the pane instead of auto-sizing to its content — `autoHeight` is what
    //    made the box exactly as tall as the instance, i.e. the top-glue this fixes.
    expect(frame.className).toContain("h-full");
  });

  /**
   * DONE-CRITERION (finding `01KYT10VE8624MKQ18AFHSE0W0`) — THE DETAIL SPEAKS OF THE COMPONENT
   * ALONE. It used to carry a « USED IN PROTOTYPES » section: the component's composition nodes,
   * selectable, pre-parameterized from the node and SAVEABLE back into its proto. Two notions on one
   * page (the catalogue's vocabulary and the protos' placements), one persisted and the other not.
   * The whole capability MOVED to the Spec's prototype tab — beside the composition it writes to —
   * and `specs/tabs/node-params.test.tsx` proves it arrived whole. What is pinned here is that it
   * really LEFT: no usage list, no node row, no per-node Save, and not one `design_component_nodes`
   * read to build a list that no longer exists.
   */
  it("carries no usages section, no node binding and no per-node Save", async () => {
    renderDetail();
    await screen.findByTitle("Button live preview");

    expect(screen.queryByTestId("usages-section")).toBeNull();
    expect(screen.queryByTestId("component-preview-section")).toBeNull();
    expect(screen.queryByTestId("representative-instance")).toBeNull();
    expect(screen.queryByTestId("node-v1-Root/Button")).toBeNull();
    expect(screen.queryByText(/used in prototypes/i)).toBeNull();
    // No Save of any kind: nothing on this page is persisted.
    expect(screen.queryByRole("button", { name: /save/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /select a node/i })).toBeNull();
    // …and the usages are not even READ: the list they fed is gone.
    expect(calls.filter((c) => c.cmd === "design_component_nodes")).toEqual([]);
    expect(calls.filter((c) => c.cmd === "proto_variant_set_node_params")).toEqual([]);
  });

  /**
   * …so the `Showing` cartouche has ONE thing to name. It used to arbitrate between the aperçu and a
   * bound usage; with the placements gone it names the representative preview — and NOT the
   * component, whose name is the page's own title two rows above (finding
   * `01KZ3JW5A63TJWX83V8ZKX62CN`: the name appeared three times inside 200 px).
   */
  it("names WHAT is previewed, and does not re-name the component", async () => {
    renderDetail();
    await screen.findByTitle("Button live preview");

    const scope = screen.getByTestId("preview-scope").textContent ?? "";
    expect(scope).toContain("the component alone, on its preview values");
    expect(scope).not.toContain("Button");
    expect(scope).not.toContain("usage");
    expect(scope).not.toContain("Root/Button");
  });

  /**
   * FOUR DEAD FIELDS AND A SILENT HOLE (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, points A and C).
   *
   *  - a param the template reads NOWHERE gets no control at all, and the declaration is named once;
   *  - a REQUIRED param left empty renders honestly (the preview shows what an unprovided value
   *    really looks like) and SAYS so, instead of a blank pill nothing distinguishes from a design.
   */
  it("offers no field for a param the template never reads, and names the declaration", async () => {
    currentComponent = {
      ...COMPONENT,
      template: "<button>{{label}}</button>",
      params_json: JSON.stringify([
        { name: "label", type: "text", default: "Hello" },
        { name: "class", type: "text" },
      ]),
      states_json: null,
    };
    renderDetail();

    expect(await screen.findByLabelText("label")).toBeTruthy();
    expect(screen.queryByLabelText("class")).toBeNull();
    const note = screen.getByTestId("param-unsubstituted");
    expect(note.textContent).toContain("class");
    expect(note.textContent).toContain("read nowhere by the template");
  });

  it("says a REQUIRED param is empty instead of showing a hole in silence", async () => {
    currentComponent = REQUIRED_PREVIEW;
    renderDetail();

    const field = (await screen.findByLabelText("label")) as HTMLInputElement;
    expect(screen.getByTestId("param-required-label").textContent).toBe("required");
    expect(field.getAttribute("aria-invalid")).toBeNull();

    fireEvent.change(field, { target: { value: "" } });
    await waitFor(() =>
      expect(screen.getByTestId("param-required-label").textContent).toBe("required · empty"),
    );
    expect(screen.getByLabelText("label").getAttribute("aria-invalid")).toBe("true");
    // The PREVIEW keeps telling the truth: an empty value renders empty.
    await waitFor(() => {
      const srcdoc = screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "";
      expect(srcdoc).not.toContain("Enregistrer");
    });
  });

  /**
   * THE PANE NEVER SPINS FOR EVER (finding `01KZ3JPW0RFM7AZ0AG2YASKBNR`'s family). `activeProjectId`
   * comes from the active-project store and CAN be null; `useDesignRender` then short-circuits to
   * `markup: null, loading: false, error: null`, and the old ladder read that as "still loading".
   */
  it("says the project could not be resolved instead of spinning for ever", async () => {
    calls = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      switch (cmd) {
        // NO project at all — the store resolves to `null`.
        case "list_projects":
          return [];
        case "list_workspaces":
          return [];
        case "design_get":
          return null;
        case "design_component_get":
          return currentComponent;
        default:
          return undefined;
      }
    });
    render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

    const dead = await screen.findByTestId("detail-preview-unavailable");
    expect(dead.textContent).toContain("No project could be resolved");
    // No frame, no spinner, and no render spent on a scope that does not exist.
    expect(screen.queryByTitle("Button live preview")).toBeNull();
    expect(document.querySelector("iframe")).toBeNull();
    expect(calls.filter((c) => c.cmd === "design_render")).toEqual([]);
  });

  it("does not repeat the word `axis` on every row of the AXES section", () => {
    // Finding `01KZ3JW5A63TJWX83V8ZKX62CN`: the section title says it once, for all of them.
    renderDetail();
    expect(screen.queryAllByText("axis")).toEqual([]);
  });

  /**
   * THE NAME, ONCE (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). Breadcrumb, title and caption all carried
   * it; the breadcrumb keeps only its actual job here — the way back to the catalogue — and `DS`,
   * which is the norm, stops being badged.
   */
  it("states the component's name exactly once, and badges only the exception", async () => {
    renderDetail();
    await screen.findByTitle("Button live preview");

    expect(screen.getAllByText("Button")).toHaveLength(1);
    expect(screen.getByRole("heading", { name: "Button" })).toBeTruthy();
    // The BREADCRUMB's own way back — scoped, because the app rail also routes to `/design` since
    // the catalogue link left the prototype page's chrome (finding `01KZ41YKMB63GFJ3QMK2ZHJN1C`).
    expect(
      within(screen.getByTestId("breadcrumb")).getByRole("link", { name: "Catalogue" }),
    ).toBeTruthy();
    expect(screen.queryByText("DS")).toBeNull();
  });

  /**
   * The DETAIL's own aperçu (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`). The representative instance
   * renders with `preview > default > rien`, so a component whose params are all REQUIRED is no
   * longer a blank preview. The panel also SAYS which params are required: the contract, not a
   * decoration. Those preview values are exactly what may never reach a composition — which is why
   * the node editor that CAN write to one seeds itself from the defaults instead
   * (`specs/tabs/node-params.test.tsx`).
   */
  it("seeds the representative instance with the PREVIEW values", async () => {
    currentComponent = REQUIRED_PREVIEW;
    renderDetail();

    expect(((await screen.findByLabelText("label")) as HTMLInputElement).value).toBe("Enregistrer");
    expect((screen.getByLabelText("hint") as HTMLInputElement).value).toBe("Astuce");
    expect(screen.getByTestId("param-required-label")).toBeTruthy();
    await waitFor(() => {
      const srcdoc = screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "";
      expect(srcdoc).toContain("Enregistrer");
    });
  });

  /**
   * THE SCOPED CSS SLOT, READABLE FROM THE COMPONENT (finding `01KYW342EE5DN6F70YB8VSTJR8`,
   * point 7). The exception was visible NOWHERE from this page — the rendering showed a dashed
   * outline attachable to nothing, and the only card stating the scope, the CSS and the lint lived
   * beside a PLACEMENT, in the prototype. The slot is carried by the COMPONENT (a node only points
   * at it), so it is read here, folded: closed by default, animated, and holding the very same
   * `<CssSlotCard>` — one card, one vocabulary, no edit control.
   */
  describe("the component's scoped CSS slot", () => {
    it("is CLOSED by default: the head names the exception, the CSS is not on screen", async () => {
      currentCssSlots = [BUTTON_SLOT];
      renderDetail();

      const toggle = await screen.findByTestId("component-css-slot-toggle");
      expect(toggle).toHaveAttribute("aria-expanded", "false");
      expect(toggle).toHaveTextContent("CSS slot");
      expect(toggle).toHaveTextContent("exception");
      // Folded ⇒ the card is not mounted at all (the animated reveal is presence-driven).
      expect(screen.queryByTestId("css-slot-c-button-4d1c")).toBeNull();
      expect(screen.queryByTestId("css-slot-code")).toBeNull();
    });

    /** Opened, it shows the COMPILED CSS, the scope, the reason and the lint — and no control. */
    it("opens onto the scope, the compiled CSS, the reason and the lint", async () => {
      currentCssSlots = [BUTTON_SLOT];
      renderDetail();

      fireEvent.click(await screen.findByTestId("component-css-slot-toggle"));

      const card = await screen.findByTestId("css-slot-c-button-4d1c");
      expect(screen.getByTestId("component-css-slot-toggle")).toHaveAttribute(
        "aria-expanded",
        "true",
      );
      expect(screen.getByTestId("css-slot-scope")).toHaveTextContent(
        '[data-nyx-css~="c-button-4d1c"]',
      );
      expect(screen.getByTestId("css-slot-code")).toHaveTextContent("c-button-4d1c-press");
      expect(screen.getByTestId("css-slot-reason")).toHaveTextContent(
        "no Tailwind utility expresses a keyframe",
      );
      expect(screen.getByTestId("css-slot-lint")).toHaveTextContent("tokens only");
      expect(screen.getByTestId("css-slot-feature-keyframes")).toHaveAttribute("data-used", "true");
      expect(screen.getByTestId("css-slot-foot")).toHaveTextContent("132 B authored");
      // NO edit control: a slot is authored in the catalogue by the agent, never from here.
      expect(card.querySelectorAll("input, textarea, select, button")).toHaveLength(0);
    });

    /**
     * THE OWNER IS THE COMPONENT, and on the component's OWN page there is no placement to deny —
     * the sentence states the REACH instead, and never speaks of a proto usage (the detail speaks
     * of the component alone, finding `01KYT10VE8624MKQ18AFHSE0W0`).
     */
    it("states the exception's reach without naming a placement or a prototype", async () => {
      currentCssSlots = [BUTTON_SLOT];
      renderDetail();
      fireEvent.click(await screen.findByTestId("component-css-slot-toggle"));

      const ownership = (await screen.findByTestId("css-slot-ownership")).textContent ?? "";
      expect(ownership).toContain("It belongs to Button itself");
      expect(ownership).toContain("every placement of it carries this same exception");
      expect(ownership).not.toContain("not to this placement");
      const section = screen.getByTestId("component-css-slot").textContent ?? "";
      expect(section).not.toMatch(/prototype|node|Root\//i);
    });

    /**
     * A STORED SLOT THAT NO LONGER COMPILES IS NAMED, NOT DROPPED. It comes back with `refused`
     * filled and nothing emitted; the head says so BEFORE anything is unfolded, so a refusal can
     * never be read as an absence.
     */
    it("names a refused slot — in the head, and in the card once opened", async () => {
      currentCssSlots = [
        {
          ...BUTTON_SLOT,
          css: "",
          features: [],
          refused: "unknown_token",
          detail: "var(--brand-glow) names a custom property the token layer does not declare",
        },
      ];
      renderDetail();

      const toggle = await screen.findByTestId("component-css-slot-toggle");
      expect(toggle).toHaveTextContent("refused");
      expect(toggle).not.toHaveTextContent("exception");

      fireEvent.click(toggle);
      expect(await screen.findByTestId("css-slot-c-button-4d1c")).toHaveAttribute(
        "data-refused",
        "true",
      );
      expect(screen.getByTestId("css-slot-refusal")).toHaveTextContent("Refused — unknown token.");
      expect(screen.getByTestId("css-slot-refusal")).toHaveTextContent("--brand-glow");
      expect(screen.queryByTestId("css-slot-code")).toBeNull();
    });

    /** A component carrying no slot is not taxed by the section at all — not even a closed head. */
    it("shows nothing for a component that carries no slot", async () => {
      currentCssSlots = [{ ...BUTTON_SLOT, component: "Spinner", scope_id: "c-spinner-1" }];
      renderDetail();
      await screen.findByTitle("Button live preview");
      expect(screen.queryByTestId("component-css-slot")).toBeNull();

      // …and the same for a render that met none at all.
      currentCssSlots = [];
      renderDetail();
      await screen.findAllByTitle("Button live preview");
      expect(screen.queryByTestId("component-css-slot")).toBeNull();
    });
  });
});
