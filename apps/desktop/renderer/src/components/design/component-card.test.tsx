import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import { ComponentCard } from "./component-card";
import type { DesignComponentRow } from "./types";

/**
 * `ComponentCard` — the "à synthétiser" branch (review #40): nyx DISCOVERS a component's bundle,
 * an AGENT writes its template. Between the two the catalogue holds a row with an EMPTY template,
 * and the card must say so instead of showing a blank preview — and must not spend a `design_render`
 * IPC on a component that has nothing to render.
 */

/** A `design_components` row fixture (discovered `[DS]`, template filled by the caller). */
function comp(overrides: Partial<DesignComponentRow> = {}): DesignComponentRow {
  return {
    id: "c1",
    project_id: "p1",
    name: "Button",
    source_repository_id: "r1",
    source_path: "src/ui/Button.tsx",
    visual_hash: null,
    template: "",
    params_json: null,
    states_json: null,
    created_at: 0,
    updated_at: 0,
    scanned_input_hash: null,
    orphaned_at: null,
    ...overrides,
  };
}

/** Install the backend, recording every `design_render` call so we can assert on them. */
function installBackend() {
  const renders: unknown[] = [];
  mockIPC((cmd, args) => {
    if (cmd === "design_render") {
      renders.push(args);
      return { markup: "<button>preview</button>" };
    }
    return undefined;
  });
  return renders;
}

/**
 * The frame's own STYLESHEETS: everything before the first `<script>` of the srcdoc (the theme, the
 * raw tokens and the frame CSS). The inlined Tailwind ENGINE that follows is a minified bundle
 * containing every utility name under the sun (`dashed`, `justify-content:center`, …), so a CSS
 * assertion has to look at the document's own styles, not at the whole string.
 */
function frameStyles(srcdoc: string): string {
  const at = srcdoc.indexOf("<script");
  return at < 0 ? srcdoc : srcdoc.slice(0, at);
}

function renderCard(component: DesignComponentRow, themeTokensJson: string | null = null) {
  return render(
    <ComponentCard
      projectId="p1"
      component={component}
      themeTokensJson={themeTokensJson}
      linked={false}
    />,
  );
}

/** A card WITH the delete affordance, returning the `onDelete` spy the tests assert on. */
function renderDeletableCard(component: DesignComponentRow) {
  const onDelete = vi.fn();
  const onOpen = vi.fn();
  render(
    <ComponentCard
      projectId="p1"
      component={component}
      themeTokensJson={null}
      linked={false}
      onOpen={onOpen}
      onDelete={onDelete}
    />,
  );
  return { onDelete, onOpen };
}

describe("ComponentCard", () => {
  it('shows "to synthesize" for a discovered-but-unsynthesized component, and renders nothing', async () => {
    const renders = installBackend();

    renderCard(comp({ template: "" }));

    expect(await screen.findByText("to synthesize")).toBeInTheDocument();
    // No preview frame at all — the empty template has nothing to show.
    expect(screen.queryByTitle("Button preview")).toBeNull();
    // …and the render IPC was never spent on it (the hook is short-circuited).
    await waitFor(() => {
      expect(renders).toHaveLength(0);
    });
    // The card still identifies the component by its name — and carries no `DS` chip: only the
    // exception is badged (finding `01KZ3JW5A63TJWX83V8ZKX62CN`).
    expect(screen.getByText("Button")).toBeInTheDocument();
    expect(screen.queryByText("DS")).toBeNull();
  });

  /**
   * A DEAD END READS AS A DEAD END (finding `01KZ3JPW0RFM7AZ0AG2YASKBNR`'s family).
   * `useDesignRender` SHORT-CIRCUITS on a null project — `markup: null, loading: false,
   * error: null` — and this tile's hand-written ladder fell off its end into `null`: a name, a
   * delete button, and a blank rectangle above them saying nothing. The state is
   * [`previewState`]'s now, and only its.
   */
  it("says it cannot render when the project is not resolved, instead of a blank tile", async () => {
    const renders = installBackend();
    render(
      <ComponentCard
        projectId={null}
        component={comp({ template: "<button>{{label}}</button>" })}
        themeTokensJson={null}
        linked={false}
      />,
    );

    const cue = await screen.findByTestId("component-card-preview-unavailable");
    expect(cue.textContent).toContain("cannot render");
    // The full sentence is one hover away — a 160 px tile has no room for it.
    expect(cue.getAttribute("title")).toContain("No project could be resolved");
    // No frame, no spinner, and not a single render spent on a scope that does not exist.
    expect(screen.queryByTitle("Button preview")).toBeNull();
    await waitFor(() => expect(renders).toHaveLength(0));
  });

  it("renders the representative preview once the component IS synthesized", async () => {
    const renders = installBackend();

    renderCard(comp({ template: "<button>{{label}}</button>" }));

    // The preview frame appears…
    expect(await screen.findByTitle("Button preview")).toBeInTheDocument();
    // …fed by exactly one `design_render` for this project.
    expect(renders).toHaveLength(1);
    expect(renders[0]).toMatchObject({ projectId: "p1" });
    // …and the "to synthesize" branch is NOT taken.
    expect(screen.queryByText("to synthesize")).toBeNull();
  });

  it("feeds the preview the project's token layer (review #42 — the grey catalogue)", async () => {
    installBackend();
    // The layer as `nyx_core::design_tokens` ingests it: the raw app identity + the app's own
    // `@theme` alias onto it. The card is the CATALOGUE call site of the renderer — dropping the
    // prop here (or the `rootCss` wiring behind it) is exactly what made every card grey.
    const layer = JSON.stringify({
      variables: { "--primary": "rgb(255, 0, 85)", "--color-primary": "var(--primary)" },
      dark: {},
    });

    renderCard(comp({ template: "<button>{{label}}</button>" }), layer);

    const frame = await screen.findByTitle("Button preview");
    const srcdoc = frame.getAttribute("srcdoc") ?? "";
    expect(srcdoc).toContain(":root {\n  --primary: rgb(255, 0, 85);\n}");
    expect(srcdoc).toContain("--color-primary: var(--primary);");
  });

  /**
   * DONE-CRITERION of finding `01KYPD8K75EC2AAY1KVAS2XZNX`: the tile is « un élément avec tous les
   * params par défaut », NOT a listing of variants. The chip-bar the shim builds from the declared
   * states is a full-page affordance (it filled the 160 px tile with 3-4 rows of chips for a `badge`
   * and its 13 CVA axes) — the CARD must not carry it. Its counterpart lives in
   * `component-detail-page.test` (the detail KEEPS the flip) and in `prototype-tab.test`.
   */
  it("renders the DEFAULT instance without a chip-bar — but still state-aware", async () => {
    installBackend();

    renderCard(
      comp({
        template: "<button>{{label}}</button>",
        states_json: JSON.stringify(["default", "destructive", "outline"]),
        params_json: JSON.stringify([{ name: "label", type: "text", default: "Enregistrer" }]),
      }),
    );

    const srcdoc = (await screen.findByTitle("Button preview")).getAttribute("srcdoc") ?? "";
    // No chip-bar HOST element → the shim's builder returns immediately and appends no chip at
    // all: no variant enumeration on the tile. (The shim SOURCE still mentions the id — it is the
    // same frozen script everywhere; what changes is that its host is not in the document.)
    expect(srcdoc).not.toContain(`id="depot-chip-bar"`);
    expect(srcdoc).not.toContain(`aria-label="States"`);
    // The states are STILL wired: the frame renders the ACTIVE state (and hides the other cases).
    // Dropping them would show every state case at once — the very listing we removed.
    expect(srcdoc).toContain(`window.__nyxStates=["default","destructive","outline"]`);
    expect(srcdoc).toContain(`window.__nyxActiveState="default"`);
  });

  /**
   * DONE-CRITERION of finding `01KYPEG16E95WADT2VMCYBPCK5` — the other half of the chrome. The
   * dashed `data-nyx-*` outlines are the PROTO-VIEWER's affordance (they say "this is clickable",
   * PDR-0010); on a tile that is `pointer-events-none` they are debug noise, and the green one
   * literally boxed the whole preview (the shim marks the active state ON THE BODY). The card must
   * carry NONE of them — their presence at the detail and at the proto-viewer is pinned by
   * `component-detail-page.test` and `prototype-tab.test`.
   */
  it("renders the vignette WITHOUT the proto-viewer chrome (no dashed affordance outline)", async () => {
    installBackend();

    renderCard(comp({ template: "<button>{{label}}</button>" }));

    const srcdoc = (await screen.findByTitle("Button preview")).getAttribute("srcdoc") ?? "";
    const css = frameStyles(srcdoc);
    expect(css).not.toContain("outline:1px dashed");
    expect(css).not.toContain("[data-nyx-page]{");
    expect(css).not.toContain("[data-nyx-state]:not(body){");
    expect(srcdoc).not.toContain(`id="depot-chip-bar"`);
  });

  /**
   * The same finding's layout + surface halves, at the CARD's own call site: the instance is CENTRED
   * and clipped in the tile (it used to stick to the top-left corner of 160 px of emptiness), and
   * the tile's surface comes from the project's `--background` token — the container no longer
   * paints a hard `bg-white` plate inside a dark app.
   */
  it("centres the instance and drops the hard-coded white surface", async () => {
    installBackend();

    renderCard(comp({ template: "<button>{{label}}</button>" }));

    const frame = await screen.findByTitle("Button preview");
    const css = frameStyles(frame.getAttribute("srcdoc") ?? "");
    // Centred on both axes, with a padding, and clamped so a wide component is cut — not scrolled.
    expect(css).toContain("align-items:safe center;justify-content:safe center");
    expect(css).toContain("max-width:100%;max-height:100%;overflow:hidden}");
    // The canvas is the project's background token (transparent when it has none).
    expect(css).toContain("background-color:var(--background,var(--color-background,transparent))");
    // …and neither the preview container nor the frame itself paints white any more.
    const preview = frame.parentElement;
    expect(preview?.className).not.toContain("bg-white");
    expect(frame.className).not.toContain("bg-white");
  });

  /**
   * THE VIGNETTE FOLLOWS THE APP'S APPEARANCE (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`) — by selecting
   * the variant the DESIGN SYSTEM declares, never by repainting over it. The tile is the second
   * surface the frame renders on, and the report ("l'aperçu est blanc dans une app sombre") was made
   * about both.
   */
  it("selects the project's OWN dark variant, and names the light-only case", async () => {
    // nyx's own appearance — the single switch the preview follows (`index.html` ships `.dark`).
    document.documentElement.classList.add("dark");
    installBackend();
    const withDark = JSON.stringify({
      variables: { "--background": "oklch(99.107% 0.00011 271.152)" },
      dark: { "--background": "oklch(18% 0.00411 285.902)" },
    });
    const { unmount } = renderCard(comp({ template: "<button>{{label}}</button>" }), withDark);
    let frame = await screen.findByTitle("Button preview");
    // The document is MARKED, so the project's own `.dark` block is the one that applies.
    expect(frame.getAttribute("srcdoc") ?? "").toContain(`<html lang="en" class="dark">`);
    // …and nothing claims a variant that does not exist: the tile stays quiet.
    expect(frame.parentElement?.title ?? "").not.toContain("CLAIRE");
    unmount();

    // A LIGHT-ONLY layer has no dark variant to select. The tile renders the one that exists and
    // SAYS which one — a 160 px tile has no room for a badge, so it goes in the tooltip.
    installBackend();
    const lightOnly = JSON.stringify({ variables: { "--background": "oklch(1 0 0)" } });
    renderCard(comp({ template: "<button>{{label}}</button>" }), lightOnly);
    frame = await screen.findByTitle("Button preview");
    expect(frame.getAttribute("srcdoc") ?? "").not.toContain(`class="dark"`);
    expect(frame.parentElement?.title ?? "").toContain("LIGHT variant");
    document.documentElement.classList.remove("dark");
  });

  /**
   * The other half of the finding: a param the SYNTHESIS left without a usable value renders empty
   * (an unlabeled pill, an empty box). The card does not fabricate content in its place — it names
   * the gap in a tooltip, with zero visual weight on the tile.
   */
  it("names the params with no preview value (and stays quiet otherwise)", async () => {
    installBackend();
    const { unmount } = renderCard(
      comp({
        template: "<button>{{label}}</button>",
        params_json: JSON.stringify([
          { name: "label", type: "text" },
          { name: "icon", type: "text", default: "check" },
        ]),
      }),
    );

    const hint = (await screen.findByTitle("Button preview")).parentElement;
    expect(hint?.getAttribute("title")).toContain("label");
    expect(hint?.getAttribute("title")).not.toContain("icon");
    // No generic placeholder was invented for it: the render still gets the empty default.
    unmount();

    installBackend();
    renderCard(
      comp({
        template: "<button>{{label}}</button>",
        params_json: JSON.stringify([{ name: "label", type: "text", default: "Enregistrer" }]),
      }),
    );
    // No gap is reported. Asserted on WHAT the tooltip says about the params, not on an empty
    // string: the same tooltip also carries the theme-variant caveat (finding
    // `01KYSVEZMJP1Z8V7SYJ8YC5H9W`), which is a statement about the PROJECT, not about this
    // component's params.
    expect((await screen.findByTitle("Button preview")).parentElement?.title).not.toContain(
      "preview value",
    );
  });

  /**
   * THE END OF THE EMPTY VIGNETTE (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`). A `button`'s label is
   * REQUIRED — the component has no label of its own — so the old model left only two options: LIE
   * (write a `default` the component does not have) or render a coloured pill with nothing in it.
   * The PREVIEW value is the third: an example, used to render the aperçu and nothing else.
   */
  it("renders a REQUIRED param from its preview value, and says nothing is missing", async () => {
    const renders = installBackend();

    renderCard(
      comp({
        template: "<button>{{label}}</button>",
        params_json: JSON.stringify([
          { name: "label", type: "text", required: true, preview: "Enregistrer" },
        ]),
      }),
    );

    await screen.findByTitle("Button preview");
    // The composition sent to the sidecar carries the EXAMPLE — this is what fills the tile.
    const sent = renders[0] as { compositionJson: string };
    expect(JSON.parse(sent.compositionJson) as unknown).toMatchObject({
      params: { label: "Enregistrer" },
    });
    // …and there is no gap left to report (see above on why this reads the param sentence, not
    // the whole tooltip).
    expect(screen.getByTitle("Button preview").parentElement?.title).not.toContain("preview value");
  });

  /**
   * …and when the synthesis marked a param REQUIRED but gave it no example, the card SAYS SO — in
   * those words — and STILL invents nothing: the composition goes out with the empty value, so an
   * empty tile reads as "incomplete synthesis", never as a plausible-looking lie.
   */
  it("reports a REQUIRED param that has no preview value, without inventing content", async () => {
    const renders = installBackend();

    renderCard(
      comp({
        template: "<button>{{label}}</button>",
        params_json: JSON.stringify([{ name: "label", type: "text", required: true }]),
      }),
    );

    const hint = (await screen.findByTitle("Button preview")).parentElement;
    expect(hint?.getAttribute("title")).toContain("REQUIRED");
    expect(hint?.getAttribute("title")).toContain("label");
    // NOT a fabricated label: the render really is given the empty value.
    const sent = renders[0] as { compositionJson: string };
    expect(JSON.parse(sent.compositionJson) as unknown).toMatchObject({ params: { label: "" } });
  });

  /**
   * THE DELETE STOPS PROMISING WHAT IT CANNOT KEEP (finding `01KYSHSPGSPC98C49X68MWY60D`).
   * A `[DS]` row whose file is still on disk comes BACK at the next inventory — the row is deleted,
   * the component is not. The armed panel must name that (EPHEMERAL, the source, the scan that
   * re-discovers it) instead of a silent trash icon.
   *
   * …in the reader's own words (finding `01KZ3JW5A63TJWX83V8ZKX62CN`): the panel used to quote three
   * internal tool names and describe what the backend would answer.
   */
  it("names the EPHEMERAL removal before deleting a [DS] component, without quoting a tool", () => {
    installBackend();
    renderDeletableCard(comp({ template: "<button/>" }));

    // Nothing before the gesture: the confirmation is where this belongs.
    expect(screen.queryByTestId("delete-ephemeral-note")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Remove Button from the catalogue" }));

    const note = screen.getByTestId("delete-ephemeral-note");
    expect(note.textContent).toContain("Ephemeral");
    expect(note.textContent).toContain("src/ui/Button.tsx");
    expect(note.textContent).toContain("the next inventory brings it back");
    for (const tool of ["design_inventory", "design_recipe", "design_component_delete"]) {
      expect(note.textContent).not.toContain(tool);
    }
    // The correction is still one click away, said as a decision rather than as a command name.
    expect(screen.getByRole("checkbox").getAttribute("aria-label")).toContain("recipe");
  });

  /**
   * WHAT THE REMOVAL COSTS, IN PAGES (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). The card shows a
   * `linked` badge right above the trash — "used somewhere, at least once" — and the confirmation
   * never turned that into the number that decides the gesture. It is read ON ARM, so eighty tiles
   * cost nothing, and a read that fails claims NO count rather than a zero.
   */
  it("says how many pages use the component, and only once armed", async () => {
    const asked: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "design_component_nodes") {
        asked.push(args);
        // Two placements, on ONE page: what is counted is the pages, not the nodes.
        return {
          nodes: [
            { page_title: "Checkout", node_key: "Root/Pay" },
            { page_title: "Checkout", node_key: "Root/Cancel" },
            { page_title: "Account", node_key: "Root/Save" },
          ],
        };
      }
      if (cmd === "design_render") return { markup: "<button>preview</button>" };
      return undefined;
    });
    renderDeletableCard(comp({ template: "<button/>" }));
    expect(asked).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "Remove Button from the catalogue" }));
    const usage = await screen.findByTestId("delete-usage");
    expect(usage.textContent).toContain("2 pages");
    expect(asked).toEqual([{ projectId: "p1", componentName: "Button" }]);
  });

  it("claims no usage count when the read fails", async () => {
    mockIPC((cmd) => {
      if (cmd === "design_component_nodes") throw new Error("db is locked");
      if (cmd === "design_render") return { markup: "<button>preview</button>" };
      return undefined;
    });
    renderDeletableCard(comp({ template: "<button/>" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove Button from the catalogue" }));
    // A count one could not obtain is NOT a zero: the line is simply absent.
    await waitFor(() => expect(screen.getByTestId("delete-ephemeral-note")).toBeTruthy());
    expect(screen.queryByTestId("delete-usage")).toBeNull();
  });

  /**
   * THE TRASH IS REACHABLE WITHOUT A MOUSE (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point B). The card
   * opens the detail on Enter/Space; the keydown on the trash BUBBLED to it, so pressing Enter on
   * the focused trash NAVIGATED and the confirmation never opened — deleting a component was
   * impossible from the keyboard.
   */
  it("opens the confirmation on Enter, and does not navigate", () => {
    installBackend();
    const { onOpen, onDelete } = renderDeletableCard(comp({ template: "<button/>" }));
    const trash = screen.getByRole("button", { name: "Remove Button from the catalogue" });

    trash.focus();
    // What a browser does with Enter on a focused button: the keydown bubbles, then a click fires.
    fireEvent.keyDown(trash, { key: "Enter" });
    fireEvent.click(trash);

    expect(onOpen).not.toHaveBeenCalled();
    expect(onDelete).not.toHaveBeenCalled();
    expect(screen.getByTestId("component-card-confirm-c1")).toBeTruthy();
  });

  it("offers the DURABLE removal — the exclusion travels WITH the delete", async () => {
    installBackend();
    const { onDelete, onOpen } = renderDeletableCard(comp({ template: "<button/>" }));

    // ARM: the first click never deletes (and never opens the detail either).
    fireEvent.click(screen.getByRole("button", { name: "Remove Button from the catalogue" }));
    expect(onDelete).not.toHaveBeenCalled();
    expect(onOpen).not.toHaveBeenCalled();

    // The human turns the removal into a DURABLE one, then confirms.
    // (the box sits inside its own <label>, so its accessible name carries the sentence too)
    fireEvent.click(screen.getByRole("checkbox", { name: /^Exclude Button's source/ }));
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm removing Button from the catalogue" }),
    );

    await waitFor(() => expect(onDelete).toHaveBeenCalledTimes(1));
    // The choice is part of the SAME call: the backend writes the exclusion and the delete in one
    // transaction, and once the row is gone there is no id left to exclude anything for.
    expect(onDelete).toHaveBeenCalledWith(expect.objectContaining({ id: "c1" }), {
      excludeSource: true,
    });
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("deletes WITHOUT excluding when the human does not ask for it", async () => {
    installBackend();
    const { onDelete } = renderDeletableCard(comp({ template: "<button/>" }));

    fireEvent.click(screen.getByRole("button", { name: "Remove Button from the catalogue" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm removing Button from the catalogue" }),
    );

    await waitFor(() => expect(onDelete).toHaveBeenCalledTimes(1));
    expect(onDelete).toHaveBeenCalledWith(expect.anything(), { excludeSource: false });
  });

  it("abandons the removal on Cancel (and on the trash icon a second time)", () => {
    installBackend();
    const { onDelete } = renderDeletableCard(comp({ template: "<button/>" }));

    fireEvent.click(screen.getByRole("button", { name: "Remove Button from the catalogue" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByTestId("component-card-confirm-c1")).toBeNull();
    expect(onDelete).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Remove Button from the catalogue" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel removing Button from the catalogue" }),
    );
    expect(screen.queryByTestId("component-card-confirm-c1")).toBeNull();
    expect(onDelete).not.toHaveBeenCalled();
  });

  /**
   * The exclusion is offered ONLY where it can work. The backend REFUSES it (and then deletes
   * nothing) for a greenfield `[NEW]` — there is no source for the scan to exclude — and it is
   * pointless for an ORPHAN, whose source the last inventory no longer found. Offering a control
   * whose refusal is certain would be one more lie at the exact spot this finding is about.
   */
  it("calls a definitive removal definitive: no exclusion for a [NEW] nor for an orphan", () => {
    installBackend();
    const { unmount } = render(
      <ComponentCard
        projectId="p1"
        component={comp({ id: "cnew", name: "Fancy", source_path: null, template: "<div/>" })}
        themeTokensJson={null}
        linked={false}
        onDelete={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Remove Fancy from the catalogue" }));
    expect(screen.getByTestId("delete-definitive-note").textContent).toContain("Definitive");
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByTestId("delete-ephemeral-note")).toBeNull();
    unmount();

    installBackend();
    render(
      <ComponentCard
        projectId="p1"
        component={comp({ id: "corph", name: "Ghosted", orphaned_at: 1, template: "<div/>" })}
        themeTokensJson={null}
        linked={false}
        onDelete={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Remove Ghosted from the catalogue" }));
    expect(screen.getByTestId("delete-definitive-note").textContent).toContain(
      "no longer found its source",
    );
    expect(screen.queryByRole("checkbox")).toBeNull();
  });
});
