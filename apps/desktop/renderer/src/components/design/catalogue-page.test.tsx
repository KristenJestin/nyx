import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";

import type { DesignComponentRow } from "./types";

/** A `design_components` row fixture. */
function comp(overrides: Partial<DesignComponentRow>): DesignComponentRow {
  return {
    id: "c0",
    project_id: "p1",
    name: "Component",
    source_repository_id: null,
    source_path: null,
    visual_hash: null,
    template: "<div>{{label}}</div>",
    params_json: null,
    states_json: null,
    created_at: 0,
    updated_at: 0,
    scanned_input_hash: null,
    orphaned_at: null,
    ...overrides,
  };
}

// A DS component (has a source path), a NEW greenfield one, and another DS that is linked-page.
const COMPONENTS: DesignComponentRow[] = [
  comp({ id: "c1", name: "Button", source_path: "src/ui/Button.tsx" }),
  comp({ id: "c2", name: "FancyBanner", source_path: null }), // NEW
  comp({ id: "c3", name: "Card", source_path: "src/ui/Card.tsx" }), // DS + linked
];
const LINKED = ["Card"];

function installBackend(components = COMPONENTS, linked = LINKED) {
  mockIPC((cmd, args) => {
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
      case "design_component_list":
        return { components, linked_names: linked };
      case "design_render":
        return { markup: "<div>preview</div>" };
      // The composant detail the card navigates to (reachability from the catalogue).
      case "design_component_get": {
        const id = (args as { componentId?: string })?.componentId;
        return components.find((c) => c.id === id) ?? null;
      }
      default:
        return undefined;
    }
  });
}

function renderPage() {
  const router = createAppRouter(["/design"]);
  return { router, ...render(<RouterProvider router={router} />) };
}

describe("CataloguePage", () => {
  beforeEach(() => {
    localStorage.clear();
    installBackend();
  });

  it("lists the project's components (DS + NEW) with provenance badges", async () => {
    renderPage();
    // The page heading — `Design`, the ONE name the rail says too — plus all three components
    // (DS and NEW) with a rendered preview each.
    expect(await screen.findByRole("heading", { name: "Design" })).toBeTruthy();
    expect(await screen.findByTestId("component-card-c1")).toBeTruthy();
    expect(screen.getByTestId("component-card-c2")).toBeTruthy();
    expect(screen.getByTestId("component-card-c3")).toBeTruthy();
    // Each card carries ONE representative preview iframe (not a param list) — rendered once the
    // sidecar `design_render` markup resolves.
    expect(await screen.findByTitle("Button preview")).toBeTruthy();
    // Provenance: ONLY the exception is badged (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). `DS` was on
    // 76 of 79 tiles — a mark every card wears marks nothing; `NEW` is the one worth seeing.
    expect(within(screen.getByTestId("component-card-c1")).queryByText("DS")).toBeNull();
    expect(within(screen.getByTestId("component-card-c2")).getByText("NEW")).toBeTruthy();
    // The linked-to-a-page marker on the referenced component.
    expect(within(screen.getByTestId("component-card-c3")).getByText("linked")).toBeTruthy();
  });

  it("filters by NEW, by DS, and by linked-to-a-page", async () => {
    renderPage();
    await screen.findByTestId("component-card-c1");

    // NEW → only the greenfield component remains.
    fireEvent.click(screen.getByRole("button", { name: /^NEW/ }));
    await waitFor(() => expect(screen.queryByTestId("component-card-c1")).toBeNull());
    expect(screen.getByTestId("component-card-c2")).toBeTruthy();

    // DS → the two sourced components remain, the NEW one drops out.
    fireEvent.click(screen.getByRole("button", { name: /^DS/ }));
    await waitFor(() => expect(screen.queryByTestId("component-card-c2")).toBeNull());
    expect(screen.getByTestId("component-card-c1")).toBeTruthy();
    expect(screen.getByTestId("component-card-c3")).toBeTruthy();

    // Linked → only the linked component (Card) remains.
    fireEvent.click(screen.getByRole("button", { name: /^Linked/ }));
    await waitFor(() => expect(screen.queryByTestId("component-card-c1")).toBeNull());
    expect(screen.getByTestId("component-card-c3")).toBeTruthy();
  });

  /**
   * EVERY COUNT IS THE ONE ITS FILTER PRODUCES, AND IT FOLLOWS (finding
   * `01KZ3JYARFJ67T5N1NC22XK69C`). The header used to announce `15 linked to a page` — the names the
   * project's compositions REFERENCE, catalogued or not — while the `Linked` filter could only ever
   * show CARDS, 12 of them; a deletion moved the filter and left the 15 where it was. And the same
   * strip announced the whole catalogue under a search that showed two tiles.
   */
  it("counts what each filter shows, on the filter, and follows a search", async () => {
    // `Ghost` is REFERENCED by a composition and is not a component of the catalogue: exactly the
    // case that inflated the old header. It may raise no count here.
    installBackend(COMPONENTS, ["Card", "Ghost"]);
    renderPage();
    await screen.findByTestId("component-card-c1");

    expect(screen.getByRole("button", { name: "All 3" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "DS 2" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "NEW 1" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Linked 1" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Orphans 0" })).toBeTruthy();
    expect(screen.getByTestId("catalogue-count").textContent).toBe("3 components");

    // A search narrows BOTH the header and every filter count.
    fireEvent.change(screen.getByLabelText("Search components"), { target: { value: "card" } });
    await waitFor(() =>
      expect(screen.getByTestId("catalogue-count").textContent).toBe("1 component of 3"),
    );
    expect(screen.getByRole("button", { name: "All 1" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "NEW 0" })).toBeTruthy();
  });

  it("names the SEARCH as the reason for an empty grid, and offers to drop it", async () => {
    renderPage();
    await screen.findByTestId("component-card-c1");
    fireEvent.change(screen.getByLabelText("Search components"), { target: { value: "zzz" } });

    const empty = await screen.findByTestId("catalogue-empty-search");
    // The term is repeated — a message that accused "the current filter" while none was active did
    // not even say what had been looked for.
    expect(empty.textContent).toContain("zzz");
    fireEvent.click(within(empty).getByRole("button", { name: "Clear the search" }));
    await waitFor(() => expect(screen.getByTestId("component-card-c1")).toBeTruthy());
  });

  it("narrows the list with the name search", async () => {
    renderPage();
    await screen.findByTestId("component-card-c1");
    fireEvent.change(screen.getByRole("textbox", { name: "Search components" }), {
      target: { value: "fancy" },
    });
    await waitFor(() => expect(screen.queryByTestId("component-card-c1")).toBeNull());
    expect(screen.getByTestId("component-card-c2")).toBeTruthy();
  });

  it("opens the composant detail from a card (atteignable depuis le catalogue)", async () => {
    renderPage();
    fireEvent.click(await screen.findByTestId("component-card-c1"));
    // The detail route mounts on the clicked component.
    expect(await screen.findByRole("heading", { name: "Button" })).toBeTruthy();
    expect(await screen.findByTitle("Button live preview")).toBeTruthy();
  });

  it("offers NO inventory / sync gesture — the agent is the only pilot", async () => {
    // Finding 01KYKX7HBP4ZSSS6ZDM31T6A5E: the "Inventorier / Sync" button duplicated step 0 of the
    // agent's manual and made the human the operator of a task that is not theirs (PDR-0009). The
    // page is READ/NAVIGATION only now: NO control on it may reach `design_inventory`. Pinned by
    // clicking every button the page offers.
    const calls: string[] = [];
    mockIPC((cmd, args) => {
      calls.push(cmd);
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
        case "design_component_list":
          return { components: COMPONENTS, linked_names: LINKED };
        case "design_render":
          return { markup: "<div>preview</div>" };
        case "design_component_get": {
          const id = (args as { componentId?: string })?.componentId;
          return COMPONENTS.find((c) => c.id === id) ?? null;
        }
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("component-card-c1");
    expect(screen.queryByRole("button", { name: /Inventorier/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /Sync/i })).toBeNull();

    // Every button on the page, clicked twice (a delete needs its confirmation) — and none of them
    // is an inventory trigger.
    for (const b of screen.getAllByRole("button")) fireEvent.click(b);
    await waitFor(() => expect(calls.length).toBeGreaterThan(0));
    expect(calls).not.toContain("design_inventory");
  });

  it("repaints on the AGENT's `prototypes://changed`, with no click at all", async () => {
    // The capability is untouched: `design_inventory` (MCP tool + db-command) still runs, still
    // emits `prototypes://changed`, and `useComponents` re-pulls on it. That event IS the seam that
    // replaces the removed button — a newly inventoried component must land in the grid by itself.
    let listed = COMPONENTS;
    mockIPC((cmd) => {
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
        case "design_component_list":
          return { components: listed, linked_names: LINKED };
        case "design_render":
          return { markup: "<div>preview</div>" };
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("component-card-c1");
    expect(screen.queryByTestId("component-card-c4")).toBeNull();

    // The agent's inventory discovered one more component and emitted the invalidation.
    listed = [
      ...COMPONENTS,
      comp({ id: "c4", name: "Freshly", source_path: "src/ui/Freshly.tsx" }),
    ];
    await emit("prototypes://changed");

    expect(await screen.findByTestId("component-card-c4")).toBeTruthy();
  });

  it("says WHO inventories the catalogue when it is empty (empty-state)", async () => {
    // With the trigger gone, an empty catalogue would otherwise read as broken — nothing on the
    // page would hint at what fills it. The empty-state names WHO fills it — and no longer the
    // internal tool it calls (finding `01KZ3JW5A63TJWX83V8ZKX62CN`: a text meant for a human does
    // not quote the machinery's names).
    installBackend([], []);
    renderPage();
    const empty = await screen.findByTestId("catalogue-empty-agent-driven");
    expect(empty.textContent).toContain("the agent is the one that inventories the project");
    expect(empty.textContent).not.toContain("design_inventory");
  });

  /**
   * THE INVARIANT, NEGATIVELY — **no inventory gesture in the UI**, and NOT ONLY when the catalogue
   * is full (finding `01KZ3JTWB82DCG0C6JT06T4CAF`, point 4).
   *
   * The gesture was removed because it duplicated step 0 of the agent's manual and made the human
   * the operator of a task that is not theirs (PDR-0009, finding `01KYKX7HBP4ZSSS6ZDM31T6A5E`). What
   * protected it on the EMPTY page — the one place a "there is nothing here, fill it" button is most
   * tempting to add back — was a check on the empty-state's SENTENCE. A sentence is a copy of a text,
   * not a guarantee: it goes on passing beside a brand-new button.
   *
   * So this is the same discipline the tokens panel already had: every control the page offers is
   * clicked, and NOTHING may reach the sidecar's inventory. It is asserted on the EMPTY page, where
   * the tokens panel's own negative test has no equivalent.
   */
  it("offers no inventory gesture on the EMPTY page either — not a control, not a word", async () => {
    const calls: string[] = [];
    mockIPC((cmd) => {
      calls.push(cmd);
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
        case "design_component_list":
          return { components: [], linked_names: [] };
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("catalogue-empty-agent-driven");

    // NOT ONE control on the empty page names the gesture — under any of the words it has worn.
    for (const wording of [/inventor/i, /\bsync\b/i, /\bscan\b/i, /refresh/i, /re-?scan/i]) {
      expect(screen.queryByRole("button", { name: wording })).toBeNull();
      expect(screen.queryByRole("link", { name: wording })).toBeNull();
    }

    // …and clicking EVERY control the empty page does offer reaches no inventory command. The
    // capability is untouched — `design_inventory` still exists, and `prototypes://changed` still
    // repaints this grid — it simply has no trigger HERE.
    const controls = [...screen.queryAllByRole("button"), ...screen.queryAllByRole("link")];
    for (const control of controls) fireEvent.click(control);
    await waitFor(() => expect(calls.length).toBeGreaterThan(0));
    expect(calls).not.toContain("design_inventory");
    expect(calls).not.toContain("design_refresh_component");
    expect(calls).not.toContain("design_component_delete");
  });

  it("removes a component from the catalogue, after a confirmation (review #44)", async () => {
    // review #44: the catalogue had NO removal at all, so an entry a wrong glob had catalogued (or a
    // `[NEW]` authored by mistake) was stuck there forever. The card's delete drives the sidecar
    // `design_component_delete`, and only after a SECOND click — a destructive catalogue write is
    // never one stray click away, and the card's own click opens the detail.
    const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
    let listed = COMPONENTS;
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
        case "design_component_list":
          return { components: listed, linked_names: LINKED };
        case "design_render":
          return { markup: "<div>preview</div>" };
        case "design_component_delete": {
          const id = (args as { componentId?: string })?.componentId;
          listed = listed.filter((c) => c.id !== id);
          return { deleted: true };
        }
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("component-card-c2");
    const remove = within(screen.getByTestId("component-card-c2")).getByRole("button", {
      name: "Remove FancyBanner from the catalogue",
    });

    // FIRST click ARMS it — nothing is deleted yet, and the card did NOT navigate to the detail.
    fireEvent.click(remove);
    expect(calls.some((c) => c.cmd === "design_component_delete")).toBe(false);
    expect(screen.getByRole("heading", { name: "Design" })).toBeTruthy();

    // SECOND click removes it, and the catalogue re-pulls without the row.
    fireEvent.click(
      within(screen.getByTestId("component-card-c2")).getByRole("button", {
        name: "Confirm removing FancyBanner from the catalogue",
      }),
    );
    await waitFor(() =>
      expect(
        calls.some(
          (c) =>
            c.cmd === "design_component_delete" &&
            (c.args as { componentId?: string } | undefined)?.componentId === "c2",
        ),
      ).toBe(true),
    );
    await waitFor(() => expect(screen.queryByTestId("component-card-c2")).toBeNull());
    // The other components are untouched.
    expect(screen.getByTestId("component-card-c1")).toBeTruthy();
    expect(screen.getByTestId("component-card-c3")).toBeTruthy();
  });

  /**
   * THE REMOVAL STOPS LYING (finding `01KYSHSPGSPC98C49X68MWY60D`, backend half
   * `01KYSCX4PNKTMX17YVG9XD8KBZ`). Deleting the ROW of a component whose file still matches the
   * project's recipe is UNDONE by the next inventory: the source is re-discovered under a new id.
   * The backend now SAYS so — `effect.ephemeral` + its `warnings[]`, which name the problem AND the
   * correction — and the page must show that text, VERBATIM. It used to `catch {}` the whole call.
   */
  it("shows the BACKEND's ephemeral warning after a removal, verbatim", async () => {
    const EPHEMERAL_WARNING =
      "THIS REMOVAL IS EPHEMERAL: 'Button' is out of the catalogue, but its source " +
      "'src/ui/Button.tsx' of repo 'web' STILL matches this project's recipe, and a component's " +
      "identity IS its source — so the next design_inventory DISCOVERS IT AGAIN, as a pending " +
      "component with a new id. To make it last: RESTRICT the recipe (design_recipe), or re-run " +
      "this delete with exclude_source: true.";
    let listed = COMPONENTS;
    mockIPC((cmd, args) => {
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
        case "design_component_list":
          return { components: listed, linked_names: LINKED };
        case "design_render":
          return { markup: "<div>preview</div>" };
        case "design_component_delete": {
          const id = (args as { componentId?: string })?.componentId;
          listed = listed.filter((c) => c.id !== id);
          return {
            deleted: true,
            effect: {
              ephemeral: true,
              verdict: "elected",
              repo: "web",
              source_path: "src/ui/Button.tsx",
              recipe_source: "stack_default",
            },
            excluded: [],
            recipe_materialized: false,
            refused: null,
            warnings: [EPHEMERAL_WARNING],
          };
        }
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("component-card-c1");
    fireEvent.click(
      within(screen.getByTestId("component-card-c1")).getByRole("button", {
        name: "Remove Button from the catalogue",
      }),
    );
    fireEvent.click(
      within(screen.getByTestId("component-card-c1")).getByRole("button", {
        name: "Confirm removing Button from the catalogue",
      }),
    );

    // The row is gone from the grid — and the warning is NOT: it is the whole point.
    await waitFor(() => expect(screen.queryByTestId("component-card-c1")).toBeNull());
    const warning = await screen.findByTestId("removal-warning");
    expect(warning.textContent).toBe(EPHEMERAL_WARNING);
    // The FACTS behind it, so a human can check the claim instead of trusting it.
    const effect = screen.getByTestId("removal-effect");
    expect(effect.textContent).toContain("elected");
    expect(effect.textContent).toContain("web/src/ui/Button.tsx");
    expect(effect.textContent).toContain("stack");
  });

  it("sends `excludeSource` when the human asks for a DURABLE removal, and shows what was written", async () => {
    const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
    let listed = COMPONENTS;
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
        case "design_component_list":
          return { components: listed, linked_names: LINKED };
        case "design_render":
          return { markup: "<div>preview</div>" };
        case "design_component_delete": {
          const id = (args as { componentId?: string })?.componentId;
          listed = listed.filter((c) => c.id !== id);
          return {
            deleted: true,
            effect: {
              ephemeral: false,
              verdict: "excluded",
              repo: "web",
              source_path: "src/ui/Button.tsx",
              recipe_source: "custom",
            },
            excluded: ["src/ui/Button.tsx"],
            recipe_materialized: true,
            refused: null,
            warnings: [
              "'src/ui/Button.tsx' is now in this project's recipe `exclude` list, so the next " +
                "design_inventory will NOT re-discover 'Button'.",
            ],
          };
        }
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("component-card-c1");
    const card = () => screen.getByTestId("component-card-c1");
    fireEvent.click(
      within(card()).getByRole("button", { name: "Remove Button from the catalogue" }),
    );
    fireEvent.click(within(card()).getByRole("checkbox", { name: /^Exclude Button's source/ }));
    fireEvent.click(
      within(card()).getByRole("button", { name: "Confirm removing Button from the catalogue" }),
    );

    await waitFor(() =>
      expect(
        calls.some(
          (c) =>
            c.cmd === "design_component_delete" &&
            c.args?.componentId === "c1" &&
            c.args?.excludeSource === true,
        ),
      ).toBe(true),
    );
    // What the recipe now holds, and that it became the project's own.
    const excluded = await screen.findByTestId("removal-excluded");
    expect(excluded.textContent).toContain("src/ui/Button.tsx");
    expect(excluded.textContent).toContain("default recipe");
  });

  it("shows a REFUSAL legibly — and the component is still there", async () => {
    // A pinned entry / a glob character in the path / a full exclusion budget: the backend refuses
    // and deletes NOTHING. The page used to swallow that error entirely, so the card just "would not
    // delete". The refusal names the real lever, and it must reach the human as it is.
    const REFUSAL =
      "design_component_delete: 'Card' was NOT deleted: its entry is PINNED in this project's " +
      "recipe, and a pin names its files verbatim and bypasses every rule — including `exclude`. " +
      "Remove the pin first (design_recipe with the pins you keep), then delete.";
    mockIPC((cmd, args) => {
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
        case "design_component_list":
          return { components: COMPONENTS, linked_names: LINKED };
        case "design_render":
          return { markup: "<div>preview</div>" };
        case "design_component_get": {
          const id = (args as { componentId?: string })?.componentId;
          return COMPONENTS.find((c) => c.id === id) ?? null;
        }
        case "design_component_delete":
          // A REAL `BridgeError` — the OBJECT the bridge rejects with, not an `Error` wearing a
          // message (finding `01KYVT6HVA260DTAEREQA4DFJN`): `String(e)` on this is `[object Object]`,
          // which is exactly what this surface used to print.
          throw { kind: "command", message: REFUSAL, source: "design_component_delete" };
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("component-card-c3");
    fireEvent.click(
      within(screen.getByTestId("component-card-c3")).getByRole("button", {
        name: "Remove Card from the catalogue",
      }),
    );
    fireEvent.click(
      within(screen.getByTestId("component-card-c3")).getByRole("button", {
        name: "Confirm removing Card from the catalogue",
      }),
    );

    // VERBATIM: the surface must not re-word what the backend refused, nor why — and it must not
    // stringify the refusal OBJECT either.
    const refusal = await screen.findByTestId("removal-refusal");
    expect(refusal.textContent).toContain(REFUSAL);
    expect(refusal.textContent).not.toContain("[object Object]");
    // An ALERT, not a whisper — and nothing was removed.
    expect(screen.getByTestId("catalogue-removal-report").getAttribute("role")).toBe("alert");
    expect(screen.getByTestId("component-card-c3")).toBeTruthy();
    // …and it can be dismissed once read.
    fireEvent.click(screen.getByRole("button", { name: "Hide the removal report" }));
    expect(screen.queryByTestId("catalogue-removal-report")).toBeNull();
  });

  it("acknowledges a complete and final removal, with nothing to read", async () => {
    // Finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point D. A definitive removal (a greenfield `[NEW]`)
    // used to answer NOTHING, on the theory that the row leaving the grid was its own report — in a
    // grid of eighty tiles that reorders as it shrinks, that is an absence to hunt for, not a
    // report. It is acknowledged now, PLAINLY: no warning colour, no backend caveat, nothing to
    // read past the fact that the gesture landed.
    let listed = COMPONENTS;
    mockIPC((cmd, args) => {
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
        case "design_component_list":
          return { components: listed, linked_names: LINKED };
        case "design_render":
          return { markup: "<div>preview</div>" };
        case "design_component_delete": {
          const id = (args as { componentId?: string })?.componentId;
          listed = listed.filter((c) => c.id !== id);
          return {
            deleted: true,
            effect: {
              ephemeral: false,
              verdict: "greenfield",
              repo: null,
              source_path: null,
              recipe_source: null,
            },
            excluded: [],
            recipe_materialized: false,
            refused: null,
            warnings: [],
          };
        }
        default:
          return undefined;
      }
    });

    renderPage();
    await screen.findByTestId("component-card-c2");
    fireEvent.click(
      within(screen.getByTestId("component-card-c2")).getByRole("button", {
        name: "Remove FancyBanner from the catalogue",
      }),
    );
    fireEvent.click(
      within(screen.getByTestId("component-card-c2")).getByRole("button", {
        name: "Confirm removing FancyBanner from the catalogue",
      }),
    );

    await waitFor(() => expect(screen.queryByTestId("component-card-c2")).toBeNull());
    const report = await screen.findByTestId("catalogue-removal-report");
    expect(report.getAttribute("role")).toBe("status");
    expect(report.textContent).toContain('"FancyBanner" removed from the catalogue');
    // Nothing to read past the acknowledgement: no backend caveat, no verdict line, no exclusion.
    expect(report.textContent).not.toContain("read this");
    expect(within(report).queryByTestId("removal-warning")).toBeNull();
    expect(within(report).queryByTestId("removal-effect")).toBeNull();
  });

  it("makes an ORPHAN identifiable, counted and filterable (review CRITICAL)", async () => {
    // A row the inventory flagged an ORPHAN — its source file gone, its synthesis deliberately KEPT
    // because no scan is ever proof enough to destroy human-reviewed LLM work — must not pass for a
    // live component. The flag rides on the ROW (`orphaned_at`, migration v35), so the catalogue
    // reports it on every read, not only right after a run: that is precisely what lets this survive
    // the removal of the "Inventorier / Sync" button, whose per-run report was the only surface
    // before. The run's own counters (pruned / scan errors) are reported by `design_inventory`
    // itself to the agent that calls it — covered in `nyx_core::design`.
    const orphaned = comp({
      id: "c9",
      name: "Ghosted",
      source_path: "src/ui/Ghosted.tsx",
      template: '<div class="precious"/>',
      orphaned_at: 1_700_000_000_000,
    });
    const listed: DesignComponentRow[] = [...COMPONENTS, orphaned];
    installBackend(listed, LINKED);

    renderPage();
    const card = await screen.findByTestId("component-card-c9");
    expect(within(card).getByText("orphan")).toBeTruthy();
    // COUNTED on the filter that shows them, never in a header strip that a deletion leaves behind
    // (finding `01KZ3JYARFJ67T5N1NC22XK69C`).
    expect(screen.getByRole("button", { name: "Orphans 1" })).toBeTruthy();

    // …and reachable through its own filter.
    fireEvent.click(screen.getByRole("button", { name: "Orphans 1" }));
    await waitFor(() => expect(screen.queryByTestId("component-card-c1")).toBeNull());
    expect(screen.getByTestId("component-card-c9")).toBeTruthy();
  });
});
