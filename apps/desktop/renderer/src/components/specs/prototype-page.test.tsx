import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { bridgeFake, mockIPC } from "@/bridge/test-harness";
import { ActiveProjectProvider } from "@/components/terminals/active-project";

import { PrototypePage } from "./prototype-page";
import type { ProtoTree, RoundManifestPage } from "./proto-types";

/**
 * The DEDICATED prototype page (`/work/$specId/prototype`, finding `01KYVMSMMBVSZNBBSBE18PRZAK` —
 * the elected v8 `full-page` design). This suite carries what used to be asserted on the TAB, since
 * the workbench is here now, plus what the election added: the chassis, Focus, the viewport
 * selector, the foldable rail and the three-pane right panel.
 *
 * The REUSED renderer ([`CompositionFrame`]) is a sandboxed iframe whose shim / chip-bar /
 * `data-nyx-*` behaviour only executes in a real browser (covered by `composition-frame.browser`,
 * `frame-srcdoc`, `proto-shim` tests). Here it is mocked to a deterministic double surfacing the
 * props THIS page is responsible for wiring — the markup, the declared STATES (the chip-bar source)
 * and `onNavigate` (the `data-nyx-page` sink) — so the page's own logic is assertable in jsdom.
 */
const frameHarness = vi.hoisted(() => ({
  onNavigate: undefined as ((page: string) => void) | undefined,
  /** The frame's `select` sink — a plain click in the render, resolved to a node. */
  onNodeSelect: undefined as ((nodeKey: string) => void) | undefined,
  /** The frame's `pin` sink — a click in PIN mode, with its frame-relative point. */
  onNodePin: undefined as
    | ((pin: { nodeKey: string | null; x: number; y: number }) => void)
    | undefined,
}));

vi.mock("../design/composition-frame", () => ({
  CompositionFrame: (props: {
    markup: string;
    themeTokensJson?: string | null;
    states?: readonly string[];
    showChrome?: boolean;
    showCssSlotMark?: boolean;
    pickMode?: string;
    selectedNodeKey?: string | null;
    pinnedNodeKeys?: readonly string[];
    onNodeSelect?: (nodeKey: string) => void;
    onNodePin?: (pin: { nodeKey: string | null; x: number; y: number }) => void;
    onNavigate?: (page: string) => void;
    title?: string;
  }) => {
    // Only the MAIN surface wires a navigation sink; the rail miniatures are decorative, so they
    // must not clear the capture when they render alongside it.
    if (props.onNavigate) frameHarness.onNavigate = props.onNavigate;
    if (props.onNodeSelect) frameHarness.onNodeSelect = props.onNodeSelect;
    if (props.onNodePin) frameHarness.onNodePin = props.onNodePin;
    return (
      <div data-testid="composition-frame" title={props.title}>
        <span data-testid="frame-markup">{props.markup}</span>
        <span data-testid="frame-states">{(props.states ?? []).join(",")}</span>
        <span data-testid="frame-theme">{props.themeTokensJson ?? ""}</span>
        <span data-testid="frame-chrome">{String(props.showChrome ?? true)}</span>
        <span data-testid="frame-css-slot-mark">
          {String(props.showCssSlotMark ?? props.showChrome ?? true)}
        </span>
        <span data-testid="frame-pick-mode">{props.pickMode ?? "off"}</span>
        <span data-testid="frame-selected-node">{props.selectedNodeKey ?? ""}</span>
        <span data-testid="frame-pinned-nodes">{(props.pinnedNodeKeys ?? []).join(",")}</span>
      </div>
    );
  },
}));

/** The project's ingested token layer, exactly as `design_get` serves it (review #42). */
const THEME_TOKENS = JSON.stringify({
  variables: { "--primary": "rgb(255, 0, 85)" },
  dark: {},
});

/** The `spec_detail` the page re-derives its project + breadcrumb from. */
function specDetail(workspace = true): unknown {
  return {
    spec: { id: "s1", project_id: "p1", title: "Spec", number: 6 },
    revision: {},
    tasks: [],
    dependencies: [],
    workspace_context: workspace
      ? {
          spec_id: "s1",
          workspace: { id: "w1", project_id: "p1", name: "wt", path: "/tmp/wt" },
          project: { id: "p1", name: "Nyx", key: "NYX" },
          repos: [],
        }
      : null,
  };
}

/** A two-page prototype tree: Home (retained variant, two states) + Settings (agent-reco variant). */
function protoTree(): ProtoTree {
  return {
    prototype: {
      id: "proto1",
      spec_id: "s1",
      description: null,
      created_at: 0,
      updated_at: 0,
      archived_at: null,
    },
    rounds: [],
    current_round: {
      id: "round1",
      prototype_id: "proto1",
      label: "v1",
      summary: null,
      position: 0,
      presented_at: null,
      created_at: 0,
    },
    design_lock: null,
    pages: [
      {
        page: {
          id: "pg-home",
          prototype_id: "proto1",
          slug: "home",
          title: "Home",
          position: 0,
          created_at: 0,
        },
        versions: [
          {
            version: {
              id: "pv-home",
              page_id: "pg-home",
              label: "v1",
              summary: null,
              created_at: 0,
              archived_at: null,
            },
            variants: [
              {
                id: "var-home",
                page_version_id: "pv-home",
                label: "a",
                title: "Home A",
                composition_json: "COMP_HOME",
                states_json: '["default","loading"]',
                is_main: true,
                position: 0,
                created_at: 0,
              },
            ],
          },
        ],
        manifest_entry: {
          id: "rp-home",
          round_id: "round1",
          page_id: "pg-home",
          page_version_id: "pv-home",
          position: 0,
          chosen_variant_id: "var-home",
          decision_rationale: null,
          decided_by: null,
          decided_at: 0,
          created_at: 0,
        },
        effective_variant_id: "var-home",
      },
      {
        page: {
          id: "pg-set",
          prototype_id: "proto1",
          slug: "settings",
          title: "Settings",
          position: 1,
          created_at: 0,
        },
        versions: [
          {
            version: {
              id: "pv-set",
              page_id: "pg-set",
              label: "v1",
              summary: null,
              created_at: 0,
              archived_at: null,
            },
            variants: [
              {
                id: "var-set",
                page_version_id: "pv-set",
                label: "a",
                title: "Settings A",
                composition_json: "COMP_SETTINGS",
                states_json: null,
                is_main: true,
                position: 0,
                created_at: 0,
              },
            ],
          },
        ],
        manifest_entry: null,
        effective_variant_id: null,
      },
    ],
    open_feedback: [],
    resolved_feedback: [],
  };
}

/** The compositions the sidecar `design_render` was asked to compile — order of the renders. */
let renderCalls: string[];

/** The full ARGS of each `design_render` — `prototypeId` included (finding `01KZ3JN69E32FYBF01NBE4FWJD`). */
let renderArgs: Record<string, unknown>[];

/**
 * What the render answers `navigation[]` with — one entry per LINKING node, honoured or not
 * (finding `01KYW34347CZSNXCYS35KDG3EY`, consumed by `01KZ3JN69E32FYBF01NBE4FWJD`).
 */
let renderNavigation: readonly Record<string, unknown>[];

/**
 * What the sidecar's LENIENT render answers `unrendered[]` with (finding `01KYT78699XG6Q6P3VZC64W8H8`)
 * — the nodes it could not draw, in ITS order: depth-first, CHILDREN BEFORE THEIR PARENT.
 */
let unrenderedNodes: readonly {
  node_key: string;
  component: string | null;
  reason: string;
  detail: string;
}[];

/**
 * What the render answers `css_slots[]` with — the SCOPED CSS EXCEPTIONS the page reaches, one per
 * COMPONENT (finding `01KYVMTWB71R0D0CT1SV88J8DX`, displayed by `01KYVVEQZW2Y2VY7ZAPK5RECZS`).
 */
let renderCssSlots: readonly Record<string, unknown>[];

/**
 * The MANIFEST of each round, keyed by round id — what `proto_round_manifest` answers (finding
 * `01KZ3AP34K1PKS6W0E75RP7DF6`). Empty by default: only the consultation tests pin a past round.
 */
let roundManifests: Record<string, readonly RoundManifestPage[]>;

/** The common backend: the tree + the Spec detail + the theme + a pass-through renderer. */
function installBackend(
  tree: ProtoTree | null,
  extra?: (cmd: string, args: unknown) => unknown | undefined,
): void {
  renderCalls = [];
  renderArgs = [];
  unrenderedNodes = [];
  renderCssSlots = [];
  renderNavigation = [];
  roundManifests = {};
  mockIPC((cmd, args) => {
    const custom = extra?.(cmd, args);
    if (custom !== undefined) return custom;
    switch (cmd) {
      case "spec_detail":
        return specDetail();
      case "proto_tree":
        return tree;
      case "proto_round_manifest":
        return roundManifests[(args as { roundId: string }).roundId] ?? [];
      case "design_get":
        return {
          id: "d1",
          project_id: "p1",
          content: "# DESIGN",
          theme_tokens_json: THEME_TOKENS,
          created_at: 0,
          updated_at: 0,
        };
      case "design_render": {
        const composition = (args as { compositionJson?: string }).compositionJson ?? "";
        renderCalls.push(composition);
        renderArgs.push(args as Record<string, unknown>);
        return {
          markup: `rendered:${composition}`,
          node_count: 1 + unrenderedNodes.length,
          unrendered: unrenderedNodes,
          unrendered_count: unrenderedNodes.length,
          css_slots: renderCssSlots,
          css_slot_count: renderCssSlots.length,
          navigation: renderNavigation,
          navigation_count: renderNavigation.length,
          navigation_broken_count: renderNavigation.filter((l) => l.status !== "linked").length,
        };
      }
      default:
        return undefined;
    }
  });
}

/**
 * Render the page under a memory router carrying the real route path (+ its back targets), inside
 * the `<ActiveProjectProvider>` the app's ROOT route wraps every page in (`router.tsx`) — the page
 * now enters the Spec's owner project on load, and the provider is where that selection lives.
 */
function renderPage(search = "") {
  const root = createRootRoute({
    component: () => (
      <ActiveProjectProvider>
        <Outlet />
      </ActiveProjectProvider>
    ),
  });
  const proto = createRoute({
    getParentRoute: () => root,
    path: "/work/$specId/prototype",
    component: PrototypePage,
    validateSearch: (s: Record<string, unknown>) =>
      typeof s.page === "string" ? { page: s.page } : {},
  });
  const detail = createRoute({
    getParentRoute: () => root,
    path: "/work/$specId",
    component: () => <div data-testid="spec-detail-stub">SPEC</div>,
  });
  const work = createRoute({
    getParentRoute: () => root,
    path: "/work",
    component: () => <div>WORK</div>,
  });
  const design = createRoute({
    getParentRoute: () => root,
    path: "/design",
    component: () => <div data-testid="catalogue-stub">CATALOGUE</div>,
  });
  const router = createRouter({
    routeTree: root.addChildren([proto, detail, work, design]),
    history: createMemoryHistory({ initialEntries: [`/work/s1/prototype${search}`] }),
  });
  return render(<RouterProvider router={router} />);
}

describe("PrototypePage — the dedicated prototype route", () => {
  beforeEach(() => {
    renderCalls = [];
    unrenderedNodes = [];
    renderCssSlots = [];
    frameHarness.onNavigate = undefined;
  });

  /**
   * DONE-CRITERION — THE ROUTE EXISTS AND CARRIES THE REVIEW'S CHASSIS. A prototype stands for a real
   * web page; in the tab its stage measured 486 × 489. The page is `h-app`, flanked by the two
   * `SidebarShell` rails, exactly like the code-review page it is in parity with.
   */
  it("lays the review chassis out: h-app between a left pages rail and a right panel", async () => {
    installBackend(protoTree());
    renderPage();

    const page = await screen.findByTestId("prototype-page");
    expect(page.className).toContain("h-app");
    expect(screen.getByLabelText("Prototype pages")).toBeTruthy();
    expect(screen.getByLabelText("Comments and node params")).toBeTruthy();
    // The breadcrumb is the review's: Work / <specKey> / Prototype.
    const crumbs = screen.getByTestId("breadcrumb");
    expect(crumbs.textContent).toContain("Work");
    expect(crumbs.textContent).toContain("NYX-6");
    expect(crumbs.textContent).toContain("Prototype");
  });

  it("renders the elected composition of the first page and wires the state chip-bar", async () => {
    installBackend(protoTree());
    renderPage();

    expect(await screen.findByTestId("proto-page-home")).toBeTruthy();
    expect(screen.getByTestId("proto-page-settings")).toBeTruthy();

    // The RETAINED variant is surfaced in the strip under the surface (label + title + badge).
    expect(screen.getByText("Home A")).toBeTruthy();
    expect(screen.getByTestId("proto-variant-retenu-var-home").textContent).toContain("RETAINED");

    const frame = await screen.findByTitle("Home preview");
    await waitFor(() =>
      expect(within(frame).getByTestId("frame-markup").textContent).toBe("rendered:COMP_HOME"),
    );
    // The variant's declared states are wired into the frame (the chip-bar source)…
    expect(within(frame).getByTestId("frame-states").textContent).toBe("default,loading");
    // …the whole interaction CHROME is kept (a full, clickable page is where it makes sense)…
    expect(within(frame).getByTestId("frame-chrome").textContent).toBe("true");
    expect(renderCalls).toContain("COMP_HOME");
    // …as is the PROJECT's token layer (review #42): without it the proto preview renders grey.
    await waitFor(() =>
      expect(within(frame).getByTestId("frame-theme").textContent).toBe(THEME_TOKENS),
    );
  });

  it("switches page via the rail (re-renders the new page's composition)", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    fireEvent.click(screen.getByTestId("proto-page-settings"));

    const settings = await screen.findByTitle("Settings preview");
    await waitFor(() =>
      expect(within(settings).getByTestId("frame-markup").textContent).toBe(
        "rendered:COMP_SETTINGS",
      ),
    );
    expect(within(settings).getByTestId("frame-states").textContent).toBe("");
    expect(renderCalls).toContain("COMP_SETTINGS");
  });

  /** The tab's contact sheet points each card at ITS page — `?page=<slug>` must land there. */
  it("opens on the page named by the ?page= deep-link", async () => {
    installBackend(protoTree());
    renderPage("?page=settings");

    expect(await screen.findByTitle("Settings preview")).toBeTruthy();
    expect(screen.getByTestId("proto-page-settings").getAttribute("aria-current")).toBe("page");
  });

  it("follows a data-nyx-page navigation the frame reports (onNavigate → page switch)", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    expect(frameHarness.onNavigate).toBeTypeOf("function");
    act(() => frameHarness.onNavigate?.("settings"));

    expect(await screen.findByTitle("Settings preview")).toBeTruthy();
  });

  /**
   * DONE-CRITERION — FOCUS FOLDS BOTH FLANKS. There is no separate full-screen mode: the page IS the
   * full screen, and Focus is what gives the stage the flanks' width back.
   */
  it("folds BOTH flanks on Focus, and unfolds them again", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    const rail = screen.getByLabelText("Prototype pages");
    const panel = screen.getByLabelText("Comments and node params");
    expect(rail.getAttribute("data-open")).toBe("true");
    expect(panel.getAttribute("data-open")).toBe("true");

    fireEvent.click(screen.getByTestId("proto-focus"));

    await waitFor(() => expect(rail.getAttribute("data-open")).toBe("false"));
    expect(panel.getAttribute("data-open")).toBe("false");
    expect(screen.getByTestId("prototype-page").getAttribute("data-focused")).toBe("true");
    // Folded, the rail is still a rail: its miniatures keep navigating.
    expect(screen.getByTestId("proto-page-mini-settings")).toBeTruthy();

    fireEvent.click(screen.getByTestId("proto-focus"));
    await waitFor(() => expect(rail.getAttribute("data-open")).toBe("true"));
    expect(panel.getAttribute("data-open")).toBe("true");
  });

  /** The pages rail folds on its own too — and folding it never costs the navigation. */
  it("folds the pages rail to its stub, which still selects a page", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    fireEvent.click(screen.getByTestId("proto-rail-toggle"));
    await waitFor(() => expect(screen.queryByTestId("proto-pages-rail")).toBeNull());

    fireEvent.click(screen.getByTestId("proto-page-mini-settings"));
    expect(await screen.findByTitle("Settings preview")).toBeTruthy();
    // The right panel is untouched by the rail's own fold (Focus is what folds both).
    expect(screen.getByLabelText("Comments and node params").getAttribute("data-open")).toBe(
      "true",
    );
  });

  /**
   * Every LEAF of the visible text, trimmed — the shape a duplication audit is counted on. A leaf
   * (an element with no element child) is what carries one label, so `Home` in the rail row and
   * `Home A` in the variant strip are two different strings and never one occurrence counted twice.
   */
  function leafTexts(): string[] {
    return Array.from(document.body.querySelectorAll("*"))
      .filter((el) => el.children.length === 0)
      .map((el) => (el.textContent ?? "").trim())
      .filter((t) => t.length > 0);
  }
  const occurrences = (label: string): number => leafTexts().filter((t) => t === label).length;

  /**
   * DONE-CRITERION (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`, MAJOR) — THE PAGE IS NAMED ONCE, AND SO IS
   * ITS SLUG, form open AND closed.
   *
   * Measured in the running app before this: the title reached FOUR places on one screen and the
   * slug four — the rail row, the header title, the header slug beside it, the params form's
   * `as composed in <page> / <variant>`, the link `Open <component> in the catalogue` and the
   * heading of a card that reprinted title + slug. The rail is the page LIST, so it keeps the human
   * title; the header carries the SLUG, the page's durable identity. Nothing else names either.
   */
  it("names the page once and its slug once, with the node form open and closed", async () => {
    const tree = protoTree();
    const home = tree.pages[0]!;
    const version = home.versions[0]!;
    const composed: ProtoTree = {
      ...tree,
      pages: [
        {
          ...home,
          versions: [
            {
              ...version,
              variants: [
                {
                  ...version.variants[0]!,
                  composition_json: JSON.stringify({
                    key: "Root",
                    children: [
                      { key: "Reset", component: "Button", params: { buttonLabel: "Reset" } },
                    ],
                  }),
                },
              ],
            },
          ],
        },
        tree.pages[1]!,
      ],
    };
    installBackend(composed, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              {
                id: "cmp-button",
                project_id: "p1",
                name: "Button",
                source_path: "src/Button.tsx",
                template: "<button>{{buttonLabel}}</button>",
                params_json: JSON.stringify([{ name: "buttonLabel", type: "text" }]),
                states_json: null,
              },
            ],
            linked_names: ["Button"],
          }
        : undefined,
    );
    renderPage();
    await screen.findByTitle("Home preview");

    // FORM CLOSED — the rail row names the page, the header carries its slug. That is all.
    expect(occurrences("Home")).toBe(1);
    expect(occurrences("/home")).toBe(1);
    expect(screen.getByTestId("proto-page-identity").textContent).toBe("/home");

    // FORM OPEN — a node bound in the params pane adds neither.
    fireEvent.click(screen.getByRole("button", { name: "Params" }));
    fireEvent.click(await screen.findByTestId("proto-comp-node-Root/Reset"));
    await screen.findByTestId("node-edit-scope");
    expect(occurrences("Home")).toBe(1);
    expect(occurrences("/home")).toBe(1);
  });

  /**
   * DONE-CRITERION (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`) — THE THREE SENTENCES ARE GONE, in every
   * state. The reading note sat centred over EVERY screen, Focus included; the gutter caption
   * explained that the grey margin is grey, and contradicted the size readout while doing it.
   */
  it("carries no reading note and no gutter caption, at any width or in Focus", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    const noChatter = () => {
      expect(screen.queryByText(/only this frame proves the visual/i)).toBeNull();
      expect(screen.queryByTestId("proto-gutter-hint")).toBeNull();
      expect(screen.queryByText(/the stage is not/i)).toBeNull();
    };

    noChatter();
    fireEvent.click(screen.getByRole("button", { name: "390" }));
    noChatter();
    fireEvent.click(screen.getByTestId("proto-focus"));
    await waitFor(() =>
      expect(screen.getByTestId("prototype-page").getAttribute("data-focused")).toBe("true"),
    );
    noChatter();
  });

  /**
   * DONE-CRITERION (findings `01KZ3JXBS13KFH8JS2D804XZBM`, `01KZ41YKMB63GFJ3QMK2ZHJN1C`, then
   * `01KZ46P0FTMJXRTT32H6G5TEEN`) — THE ROUND SURVIVES EVERY STATE OF THE RAIL IT LIVES IN, AT ITS
   * FOOT.
   *
   * It lived in that rail's SCROLLING foot, so its chevron removed the only entry point to the
   * rounds — and so did Focus, which folds that rail by definition, at the very moment one asks for
   * the most room. It moved to the page header, then to the rail's header above `PAGES`, and is now
   * back at the FOOT where the user looks for it. The three states are asserted because the defect
   * only shows in two of them, and this is the whole argument that the requirements hold together:
   * the foot is the shell's PINNED `footer` slot (not the scrolled body), the rail folds to a STUB
   * rather than to nothing, and the round is mirrored in that stub.
   *
   * THE CATALOGUE IS NOT ASSERTED HERE ANY MORE, and its absence is: it is a project-wide reference
   * living at the foot of the APP rail ([`BaseLayout`]), which no state of this page touches. A
   * second link in this page's chrome was the duplication the finding names.
   */
  it("keeps the round selector reachable in all three rail states, and drops the catalogue link", async () => {
    const tree = protoTree();
    installBackend({
      ...tree,
      rounds: [
        {
          id: "round0",
          prototype_id: "proto1",
          label: "v0",
          summary: null,
          position: 0,
          presented_at: 10,
          created_at: 0,
        },
        tree.current_round!,
      ],
    });
    renderPage();
    await screen.findByTitle("Home preview");

    const reachable = () => {
      expect(screen.getByRole("button", { name: /rounds history/i })).toBeTruthy();
      // A GLOBAL link has no business in the chrome of one page: the app rail routes to /design.
      expect(screen.queryByTestId("proto-catalogue-link")).toBeNull();
    };

    // 1. RAIL OPEN — the selector sits UNDER the pages, at the foot of the rail…
    reachable();
    const chip = screen.getByTestId("proto-round-chip");
    const rail = screen.getByTestId("proto-pages-rail");
    // …AFTER the page list in the document…
    expect(rail.compareDocumentPosition(chip) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // …in a PINNED block, not in the scrolled body: that is the whole difference with the foot the
    // first finding condemned. The shell's own top border does the separating.
    const foot = chip.parentElement!;
    expect(foot.className).toContain("border-t");
    expect(foot.className).toContain("shrink-0");
    expect(rail.closest(".overflow-y-auto")).not.toBeNull();
    expect(chip.closest(".overflow-y-auto")).toBeNull();

    // 2. RAIL FOLDED — the pages become a strip of miniatures, the round does not go with them: it
    //    is mirrored in the stub, at its foot too, which is why living in this rail costs nothing.
    fireEvent.click(screen.getByTestId("proto-rail-toggle"));
    await waitFor(() => expect(screen.queryByTestId("proto-pages-rail")).toBeNull());
    reachable();
    expect(screen.getByTestId("proto-round-stub")).toBeTruthy();
    const stub = screen.getByTestId("proto-pages-rail-stub");
    const stubFoot = screen.getByTestId("proto-pages-rail-stub-foot");
    // `mt-auto` is what pins it to the bottom of the 52 px strip rather than trailing the last
    // miniature — asserted as the RULE, since jsdom lays nothing out.
    expect(stubFoot.className).toContain("mt-auto");
    expect(stub.lastElementChild).toBe(stubFoot);

    // 3. FOCUS — both flanks folded, which is exactly where the loss used to bite.
    fireEvent.click(screen.getByTestId("proto-focus"));
    await waitFor(() =>
      expect(screen.getByTestId("prototype-page").getAttribute("data-focused")).toBe("true"),
    );
    reachable();
    // …and the history really OPENS from there.
    fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));
    expect(await screen.findByTestId("proto-round-round0")).toBeTruthy();
  });

  /**
   * DONE-CRITERION (finding `01KZ3JXDFTFMMK50RQP7HECBEQ`) — THE TWO-STATE SELECTOR FILLS ITS BAR.
   *
   * Measured in the running app: 71 px + 47 px of segments in a 298 px pill — 60 % of it blank,
   * reading as a tab bar missing its tabs (a third segment lived there and was withdrawn without the
   * bar ever being resized). jsdom lays nothing out, so what is asserted is the RULE that produces
   * the fill: the control is stretched by its host and its options share it.
   */
  it("fills the Comments · Params bar with its two segments", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    const bar = screen.getByRole("group", { name: "Side panel" });
    expect(bar.className).toContain("flex-1");
    const segments = within(bar).getAllByRole("button");
    expect(segments).toHaveLength(2);
    for (const segment of segments) {
      expect(segment.className).toContain("flex-1");
      expect(segment.className).toContain("min-w-0");
    }
    // The strip that sizes itself to its content is untouched: it has no blank to fill.
    const presets = screen.getByRole("group", { name: "Viewport width" });
    expect(within(presets).getAllByRole("button")[0]!.className).not.toContain("flex-1");
  });

  /**
   * DONE-CRITERION — THE VIEWPORT SELECTOR REALLY CHANGES THE RENDERING WIDTH. Validating a page at
   * one width proves nothing about it; this is what makes a prototype testable.
   */
  it("renders the page at the picked viewport width, and says which", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    const surface = screen.getByTestId("proto-preview-surface");
    // `Fit` is the default: the page takes the stage's own width, no imposed box.
    expect(surface.getAttribute("data-viewport")).toBe("fit");
    expect(surface.style.width).toBe("");
    expect(surface.className).toContain("w-full");

    fireEvent.click(screen.getByRole("button", { name: "390" }));
    expect(surface.getAttribute("data-viewport")).toBe("390");
    expect(surface.style.width).toBe("390px");

    fireEvent.click(screen.getByRole("button", { name: "1280" }));
    expect(surface.getAttribute("data-viewport")).toBe("1280");
    expect(surface.style.width).toBe("1280px");

    // The readout exists and reads the RENDERED box (0 in jsdom, which paints nothing).
    expect(screen.getByTestId("proto-viewport-readout").textContent).toMatch(/\d+ × \d+/);
  });

  /**
   * DONE-CRITERION (finding `01KZ3JPWNYP0B65Z31RRX1EEMT`, MAJOR) — `390` MEANS 390.
   *
   * The preset writes its number into the surface's `width`, and the surface carries the two gutter
   * borders that mark the page's edges. Under the default `border-box` those borders are taken OUT
   * of the number: the page was laid out at 388 for `390`, 766 for `768`, 1278 for `1280`. A media
   * query at 390 px does not fire at 388, so validating a screen at that width proved nothing about
   * the width the reviewer asked for — and this selector exists for nothing else.
   *
   * The fix is `content-box`, and this is the assertion that MORDS on it: remove the box-sizing and
   * the surface's content box is the preset minus 2 again. It is asserted where jsdom can see it —
   * INLINE, beside the width it qualifies — because jsdom resolves no Tailwind class.
   */
  it("gives the page the exact width that was asked for, borders outside", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    const surface = screen.getByTestId("proto-preview-surface");
    for (const preset of ["390", "768", "1280"]) {
      fireEvent.click(screen.getByRole("button", { name: preset }));
      expect(surface.style.width).toBe(`${preset}px`);
      // The gutter borders are drawn OUTSIDE the page's box, so they take nothing from it.
      expect(surface.style.boxSizing).toBe("content-box");
      expect(getComputedStyle(surface).boxSizing).toBe("content-box");
      expect(surface.className).toContain("border-x");
    }

    // `Fit` imposes no box at all — nothing to size, nothing to qualify.
    fireEvent.click(screen.getByRole("button", { name: "Fit" }));
    expect(surface.style.width).toBe("");
    expect(surface.style.boxSizing).toBe("");
  });

  /**
   * DONE-CRITERION — THE SIZE READOUT MEASURES, AND KEEPS UP (finding `01KYVYNQGYP6THPCBYSYSWJ5B4`).
   *
   * The reading is the ONE thing that says at what width the reviewer is validating; a wrong one is
   * worse than none, because it hands out a certainty. It was measured in the packaged app stuck on
   * the stage's own width while the frame under it resized — and this screen's very first defect was
   * a reading frozen at `0 × 0`, for the same family of reason: a measure that never re-runs.
   *
   * These tests run with NO `ResizeObserver` (jsdom's is a no-op stub), which is exactly the point:
   * they pass only if the readout re-measures on the COMMIT each cause produces, instead of waiting
   * for a rendering-lifecycle callback that may never come.
   */
  describe("the size readout", () => {
    /**
     * jsdom lays nothing out, so it is given the layout a `content-box` surface really has: its
     * CONTENT box IS the picked width (`Fit` → the stage's own width), the gutter borders sitting
     * outside it, and the height is one less flank of chrome once Focus folds them.
     */
    function stubLayout(surface: HTMLElement, height: () => number) {
      Object.defineProperty(surface, "clientWidth", {
        configurable: true,
        get: () => (surface.style.width === "" ? 1489 : Number.parseInt(surface.style.width, 10)),
      });
      Object.defineProperty(surface, "clientHeight", { configurable: true, get: height });
    }

    const reading = () => screen.getByTestId("proto-viewport-readout").textContent;

    /** CAUSE 1 — the width pick. And what it reads is the FRAME's box, not the button's number. */
    it("follows a width pick, and reads the box the frame gets — not the width asked for", async () => {
      installBackend(protoTree());
      renderPage();
      await screen.findByTitle("Home preview");

      const surface = screen.getByTestId("proto-preview-surface");
      stubLayout(surface, () => 849);

      fireEvent.click(screen.getByRole("button", { name: "1280" }));
      // 1280 — and it is MEASURED, not echoed: the readout reads `clientWidth`, the box the frame
      // really gets. What changed with finding `01KZ3JPWNYP0B65Z31RRX1EEMT` is the BOX, not the
      // reading: the surface sizes `content-box`, so what it was told to be and what it lays the
      // page out in are finally the same number.
      await waitFor(() => expect(reading()).toBe("1280 × 849"));

      fireEvent.click(screen.getByRole("button", { name: "390" }));
      await waitFor(() => expect(reading()).toBe("390 × 849"));

      fireEvent.click(screen.getByRole("button", { name: "Fit" }));
      // …and `Fit` proves it is still a MEASURE: no preset was pressed, so the number can only come
      // from the box (the stage's own width), never from a button.
      await waitFor(() => expect(reading()).toBe("1489 × 849"));
    });

    /** CAUSE 2 — Focus. Folding both flanks gives the stage their width; the reading must say so. */
    it("follows the Focus toggle", async () => {
      installBackend(protoTree());
      renderPage();
      await screen.findByTitle("Home preview");

      const surface = screen.getByTestId("proto-preview-surface");
      // At `Fit` the surface is worth whatever the flanks leave it: measured 947 px on a 1680 px
      // window, 1489 once Focus folds them both.
      Object.defineProperty(surface, "clientWidth", {
        configurable: true,
        get: () =>
          screen.getByTestId("prototype-page").getAttribute("data-focused") === "true" ? 1489 : 947,
      });
      Object.defineProperty(surface, "clientHeight", { configurable: true, get: () => 849 });

      fireEvent.click(screen.getByTestId("proto-focus"));
      await waitFor(() => expect(reading()).toBe("1489 × 849"));

      fireEvent.click(screen.getByTestId("proto-focus"));
      await waitFor(() => expect(reading()).toBe("947 × 849"));
    });

    /** CAUSE 3 — the window. It produces no commit of its own, so it is listened for. */
    it("follows a window resize", async () => {
      installBackend(protoTree());
      renderPage();
      await screen.findByTitle("Home preview");

      const surface = screen.getByTestId("proto-preview-surface");
      let height = 849;
      stubLayout(surface, () => height);

      fireEvent(window, new Event("resize"));
      await waitFor(() => expect(reading()).toBe("1489 × 849"));

      height = 649;
      fireEvent(window, new Event("resize"));
      await waitFor(() => expect(reading()).toBe("1489 × 649"));
    });
  });

  // The election is wired with the current round + the active page, so a "Clear decision" on the
  // retained Home variant drives `proto_election_clear` for that (round, page).
  it("wires the per-page variant election through the page (clear → proto_election_clear)", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    fireEvent.click(screen.getByTestId("proto-variant-clear-var-home"));

    await waitFor(() => {
      const call = bridgeFake().calls.find((c) => c.command === "proto_election_clear");
      expect(call?.args).toMatchObject({ roundId: "round1", pageId: "pg-home" });
    });
  });

  /**
   * DONE-CRITERION — THE DECISION ROW NEVER REPEATS "RETAINED". The shipped strip put a `RETAINED`
   * badge next to a `Retained` button next to the pill's own `RETAINED` chip: the same word three
   * times and no verb. Retained, the only thing left to decide is to undo.
   */
  it("offers Clear decision — never a second 'Retained' — on the retained variant", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    const strip = screen.getByTestId("proto-variant-strip");
    expect(screen.getByTestId("proto-variant-clear-var-home").textContent).toContain(
      "Clear decision",
    );
    // ONE occurrence of the word in the whole strip: the pill's badge.
    const occurrences = (strip.textContent ?? "").match(/retained/gi) ?? [];
    expect(occurrences).toHaveLength(1);
    expect(screen.getByTestId("proto-variant-retenu-var-home").textContent).toContain("RETAINED");
  });

  /**
   * T26 + finding `01KZ3AP34K1PKS6W0E75RP7DF6` — the ROUNDS. The control lives in the rail's foot
   * once there is a prior round. It used to offer "go back to this round" (`proto_round_rebase` —
   * the UI OPENING a round, which is the agent's work); then it offered nothing at all, which is
   * how a rule about MUTATION got read as a rule about NAVIGATION. A round is now CONSULTABLE, and
   * still uncreatable.
   */
  describe("consulting a past round", () => {
    /**
     * A prototype with a HISTORY: `v0` (presented, frozen) pinned an EARLIER version of Home whose
     * retained variant was `var-home-v0`; the current `v1` pins the version the other tests use.
     * The tree carries BOTH versions — archived ones included — which is what makes the past round
     * showable without a second read of the composition itself.
     */
    function treeWithHistory(): ProtoTree {
      const tree = protoTree();
      const home = tree.pages[0]!;
      return {
        ...tree,
        rounds: [
          {
            id: "round0",
            prototype_id: "proto1",
            label: "v0",
            summary: null,
            position: 0,
            presented_at: 10,
            created_at: 0,
          },
          tree.current_round!,
        ],
        pages: [
          {
            ...home,
            versions: [
              ...home.versions,
              {
                version: {
                  id: "pv-home-v0",
                  page_id: "pg-home",
                  label: "v0",
                  summary: null,
                  created_at: 0,
                  archived_at: 5,
                },
                variants: [
                  {
                    id: "var-home-v0",
                    page_version_id: "pv-home-v0",
                    label: "a",
                    title: "Home A (v0)",
                    composition_json: "COMP_HOME_V0",
                    states_json: null,
                    is_main: true,
                    position: 0,
                    created_at: 0,
                  },
                ],
              },
            ],
          },
          tree.pages[1]!,
        ],
      };
    }

    /** What `proto_round_manifest` answers for `v0`: Home pinned at its v0, `var-home-v0` retained. */
    const V0_MANIFEST: readonly RoundManifestPage[] = [
      {
        entry: {
          id: "rp-home-v0",
          round_id: "round0",
          page_id: "pg-home",
          page_version_id: "pv-home-v0",
          position: 0,
          chosen_variant_id: "var-home-v0",
          decision_rationale: null,
          decided_by: null,
          decided_at: 0,
          created_at: 0,
        },
        effective_variant_id: "var-home-v0",
      },
    ];

    /** Open the rounds menu and click a round row. */
    async function consult(roundId: string): Promise<void> {
      fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));
      fireEvent.click(await screen.findByTestId(`proto-round-${roundId}`));
    }

    /**
     * DONE-CRITERION — a round OPENS: its pages, the variant retained in it, and the rendering AS
     * IT WAS. The proof is the composition the renderer is handed: `COMP_HOME_V0`, the version that
     * round pinned — not the current one.
     */
    it("shows the round's own pages, retained variant and rendering", async () => {
      installBackend(treeWithHistory());
      renderPage();
      await screen.findByTitle("Home preview");
      roundManifests = { round0: V0_MANIFEST };

      await consult("round0");

      await waitFor(() =>
        expect(
          within(screen.getByTitle("Home preview")).getByTestId("frame-markup").textContent,
        ).toBe("rendered:COMP_HOME_V0"),
      );
      // The pages are the MANIFEST's: `v0` pinned Home alone, so Settings is not part of it.
      expect(screen.getByTestId("proto-page-home")).toBeTruthy();
      expect(screen.queryByTestId("proto-page-settings")).toBeNull();
      // …and the variant RETAINED in that round is the one badged.
      expect(screen.getByTestId("proto-variant-sole-var-home-v0").textContent).toContain(
        "RETAINED",
      );
    });

    /**
     * The state is EVIDENT, and the way back is one click at a fixed place — AND IT IS SAID ONCE
     * (finding `01KZ41YKMB63GFJ3QMK2ZHJN1C`, point 2). A `Consulting v0 — read-only` tag used to sit
     * between a chip reading `v0 · past round` and a button reading `Back to current`, inside a
     * header painted amber edge to edge: four signals for one state. The tag went; the three that
     * carry it stayed, and the round's own SUMMARY — which nothing else says — stayed with them.
     */
    it("says it is consulting a past round ONCE, and returns to the current one", async () => {
      installBackend(treeWithHistory());
      renderPage();
      await screen.findByTitle("Home preview");
      roundManifests = { round0: V0_MANIFEST };
      expect(screen.getByTestId("prototype-page").getAttribute("data-consulting")).toBeNull();

      await consult("round0");

      await waitFor(() =>
        expect(screen.getByTestId("prototype-page").getAttribute("data-consulting")).toBe("round0"),
      );
      expect(screen.getByTestId("proto-round-chip").textContent).toMatch(/v0 · past round/);
      expect(screen.getByTestId("proto-round-back")).toBeTruthy();
      // THE TAG IS GONE — and with it the third and fourth spelling of the same state. (The rounds
      // MENU still explains that opening a round mutates nothing; that sentence is about the
      // gesture one is about to make, not a label on a state already on screen.)
      expect(screen.queryByTestId("proto-consulting-banner")).toBeNull();
      expect(screen.queryByText(/Consulting v0/i)).toBeNull();

      fireEvent.click(screen.getByTestId("proto-round-back"));
      await waitFor(() =>
        expect(
          within(screen.getByTitle("Home preview")).getByTestId("frame-markup").textContent,
        ).toBe("rendered:COMP_HOME"),
      );
      expect(screen.queryByTestId("proto-round-back")).toBeNull();
      expect(screen.getByTestId("prototype-page").getAttribute("data-consulting")).toBeNull();
    });

    /**
     * THE POINT THE FINDING INSISTS ON — consultation must not stack a SECOND set of panels, and it
     * must introduce NO mutation. The election Decision goes, the render stops taking picks, and NOT
     * ONE `proto_*` write reaches the sidecar: the only command the whole gesture issues is the
     * manifest READ.
     *
     * WHAT CHANGED (finding `01KZ3JXB3SN8BMK7FS16PPSZ6T`): the flank is no longer WITHDRAWN. It is
     * the same panel — same label, same fold control, no second one — holding the THREAD alone, with
     * every sink removed: no composer, no pin toggle, no `Resolve`, no Params form. The guarantee is
     * still structural (nothing bound to a write is on screen), and it is still proven by the calls.
     */
    it("keeps ONE panel, offers no mutation, and writes nothing", async () => {
      installBackend(treeWithHistory());
      renderPage();
      await screen.findByTitle("Home preview");
      // The live workbench HAS the flank and the election decision.
      expect(screen.getByLabelText("Comments and node params")).toBeTruthy();
      expect(screen.getByTestId("proto-variant-clear-var-home")).toBeTruthy();
      roundManifests = { round0: V0_MANIFEST };

      await consult("round0");
      await screen.findByTestId("proto-round-back");

      // ONE panel, the same one — not a second jeu de panneaux beside the first.
      expect(screen.getAllByLabelText("Comments and node params")).toHaveLength(1);
      expect(screen.getByTestId("proto-panel-toggle")).toBeTruthy();
      // …and every WRITE surface of it is absent, structurally.
      expect(screen.queryByTestId("proto-comment-body")).toBeNull();
      expect(screen.queryByTestId("proto-comment-submit")).toBeNull();
      expect(screen.queryByTestId("proto-comment-pin-toggle")).toBeNull();
      expect(screen.queryByTestId("node-params-panel")).toBeNull();
      expect(screen.queryByRole("button", { name: /^Params$/ })).toBeNull();
      expect(screen.queryByRole("button", { name: /^Resolve$/ })).toBeNull();
      // No election gesture on a round already decided, and no pick sink on the render.
      expect(screen.queryByTestId("proto-variant-clear-var-home-v0")).toBeNull();
      expect(screen.queryByTestId("proto-variant-elect-var-home-v0")).toBeNull();
      expect(
        within(screen.getByTitle("Home preview")).getByTestId("frame-pick-mode").textContent,
      ).toBe("off");

      // NOT ONE WRITE — no rebase, no election, no comment. Only the manifest read.
      const protoCalls = bridgeFake()
        .calls.map((c) => c.command)
        .filter((c) => c.startsWith("proto_"));
      expect(protoCalls.filter((c) => c !== "proto_tree" && c !== "proto_round_manifest")).toEqual(
        [],
      );
      expect(protoCalls).toContain("proto_round_manifest");
    });

    /**
     * DONE-CRITERION (finding `01KZ3JXB3SN8BMK7FS16PPSZ6T`) — THE THREAD IS READABLE WHILE ONE
     * CONSULTS. That is the whole reason a past round gets opened: reading the review that produced
     * it, against the round it produced. The comments withdrew with the flank, and so did the
     * chevron that could have called them back, so there was no gesture left at all.
     *
     * An OPEN comment must still read as open here: badging it `Resolved` because the panel stopped
     * offering the button would be a lie about the review, told by the absence of a control.
     */
    it("lets the review thread be READ while consulting, and resolves nothing", async () => {
      const tree = treeWithHistory();
      installBackend({
        ...tree,
        open_feedback: [
          {
            id: "fb-v0",
            prototype_id: "proto1",
            origin_variant_id: "var-home-v0",
            node_key: "Root/Save",
            anchor_status: "anchored",
            body: "The CTA sits under the fold.",
            status: "open",
            resolution_note: null,
            resolution_via_variant_id: null,
            resolved_at: null,
            ignored_reason: null,
            ignored_at: null,
            author_kind: "user",
            author_ref: null,
            created_at: 0,
            updated_at: 0,
          },
        ],
      });
      renderPage();
      await screen.findByTitle("Home preview");
      roundManifests = { round0: V0_MANIFEST };

      await consult("round0");
      await screen.findByTestId("proto-round-back");

      // The thread is THERE, and it is the page's own.
      expect(await screen.findByText("The CTA sits under the fold.")).toBeTruthy();
      expect(screen.getByTestId("proto-panel-readonly").textContent).toContain("Comments");
      // An open comment is not re-labelled by the missing button.
      expect(screen.queryByTestId("proto-comment-resolve-fb-v0")).toBeNull();
      expect(within(screen.getByTestId("proto-comment-fb-v0")).queryByText("Resolved")).toBeNull();
    });

    /**
     * …AND THE ROUND SAYS WHAT IT WAS FOR (finding `01KZ3JXCNK902DQXBD6QSYEE78`). Every round but the
     * first carries a `summary` the agent wrote when it opened it, and the app displayed it NOWHERE:
     * not in the list, not while consulting. It is the user's own content — shown verbatim.
     */
    it("shows the consulted round's summary, and its date in the history", async () => {
      const tree = treeWithHistory();
      const [past, ...rest] = tree.rounds;
      installBackend({
        ...tree,
        rounds: [{ ...past!, summary: "Rollback depuis v1", created_at: 1785140874962 }, ...rest],
      });
      renderPage();
      await screen.findByTitle("Home preview");
      roundManifests = { round0: V0_MANIFEST };

      // In the LIST, beside the date — before anything is consulted.
      fireEvent.click(screen.getByRole("button", { name: /rounds history/i }));
      const row = await screen.findByTestId("proto-round-round0");
      expect(row.textContent).toContain("Rollback depuis v1");
      expect(row.textContent).toMatch(/\d\d\/\d\d\/\d\d/);
      expect(row.getAttribute("title")).toBe("Rollback depuis v1");

      fireEvent.click(row);
      // …and again while the round is on screen: it says what one is looking at.
      expect(await screen.findByTestId("proto-consulting-summary")).toBeTruthy();
      expect(screen.getByTestId("proto-consulting-summary").textContent).toBe("Rollback depuis v1");
    });

    /** A round the read cannot answer never enters the mode: the live workbench stays live. */
    it("stays on the current round when the manifest read fails", async () => {
      installBackend(treeWithHistory(), (cmd) => {
        if (cmd === "proto_round_manifest") throw new Error("nope");
        return undefined;
      });
      renderPage();
      await screen.findByTitle("Home preview");

      await consult("round0");

      await waitFor(() =>
        expect(screen.getByTestId("prototype-page").getAttribute("data-consulting")).toBeNull(),
      );
      expect(
        within(screen.getByTitle("Home preview")).getByTestId("frame-markup").textContent,
      ).toBe("rendered:COMP_HOME");
    });
  });

  /**
   * DONE-CRITERION (finding `01KYQ0R9414Q5K63TBV9CCMC9J`) — THE NODE CONTRACT AT THE READ. A node
   * carrying `text` for a component declaring `buttonLabel` renders EMPTY and used to say nothing.
   * The verdict survives the move: it lives in the PARAMS pane — beside the editor that fixes it,
   * and off the stage, whose height it used to take.
   */
  it("names, in the params pane, what a composition node owes the component it references", async () => {
    const tree = protoTree();
    const composed: ProtoTree = {
      ...tree,
      pages: [
        {
          ...tree.pages[0]!,
          versions: [
            {
              ...tree.pages[0]!.versions[0]!,
              variants: [
                {
                  ...tree.pages[0]!.versions[0]!.variants[0]!,
                  composition_json: JSON.stringify({
                    key: "Root",
                    children: [
                      { component: "Button", key: "Reset", params: { text: "Reset filters" } },
                    ],
                  }),
                },
              ],
            },
          ],
        },
      ],
    };
    installBackend(composed, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              {
                id: "c1",
                project_id: "p1",
                name: "Button",
                params_json: JSON.stringify([
                  { name: "buttonLabel", type: "text", required: true },
                ]),
                states_json: JSON.stringify({
                  axes: [
                    { name: "variant", values: [{ name: "ghost", classes: "bg-transparent" }] },
                  ],
                }),
              },
            ],
            linked_names: ["Button"],
          }
        : undefined,
    );

    renderPage();
    await screen.findByTitle("Home preview");
    fireEvent.click(screen.getByRole("button", { name: "Params" }));

    const unknown = await screen.findByTestId("node-contract-unknown-key");
    expect(unknown.textContent).toContain("Root/Reset");
    expect(unknown.textContent).toContain("Button");
    expect(unknown.textContent).toContain("'text'");
    expect(unknown.textContent).toContain("buttonLabel");
    expect(unknown.textContent).toContain("Rename the key");
    expect(unknown.textContent).not.toContain("did you mean");

    const missing = screen.getByTestId("node-contract-missing-required");
    expect(missing.textContent).toContain("Root/Reset");
    expect(missing.textContent).toContain("'buttonLabel'");
    expect(missing.textContent).toContain("renders EMPTY");
  });

  /**
   * DONE-CRITERION (finding `01KZ3JN5KJQWJTV2XNT604CWCT`, CRITICAL) — THE TREE IS THE COMPOSITION ON
   * SCREEN, AND IT FOLLOWS THE VARIANT.
   *
   * The Params pane used to render `proto_composition` — the FROZEN elected composition of the
   * design-locked round. That is another variant, another page version and, for a page the round
   * does not pin, no entry at all: the tree named components the rendering never mentioned and did
   * not move one row when the reviewer went from variant A to B to C. Here the page offers THREE
   * variants with three different compositions: the tree must name the SHOWN one's nodes, and only
   * those, at every step.
   */
  it("shows the composition of the VARIANT on screen and follows a variant switch", async () => {
    const tree = protoTree();
    const home = tree.pages[0]!;
    const version = home.versions[0]!;
    const comp = (node: string, label: string) =>
      JSON.stringify({
        key: "Root",
        children: [{ key: node, component: "Button", params: { buttonLabel: label } }],
      });
    const threeVariants: ProtoTree = {
      ...tree,
      pages: [
        {
          ...home,
          versions: [
            {
              ...version,
              variants: [
                { ...version.variants[0]!, composition_json: comp("OnlyInA", "A") },
                {
                  ...version.variants[0]!,
                  id: "var-home-b",
                  label: "b",
                  is_main: false,
                  composition_json: comp("OnlyInB", "B"),
                },
                {
                  ...version.variants[0]!,
                  id: "var-home-c",
                  label: "c",
                  is_main: false,
                  composition_json: comp("ClearSearch", "Clear search"),
                },
              ],
            },
          ],
        },
        tree.pages[1]!,
      ],
    };
    installBackend(threeVariants, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              {
                id: "cmp-button",
                project_id: "p1",
                name: "Button",
                source_path: "src/Button.tsx",
                template: "<button>{{buttonLabel}}</button>",
                params_json: JSON.stringify([{ name: "buttonLabel", type: "text" }]),
                states_json: null,
              },
            ],
            linked_names: ["Button"],
          }
        : undefined,
    );

    renderPage();
    await screen.findByTitle("Home preview");
    fireEvent.click(screen.getByRole("button", { name: "Params" }));

    // Variant A: its node, and the heading NAMES the variant the tree describes.
    expect(await screen.findByTestId("proto-comp-node-Root/OnlyInA")).toBeTruthy();
    expect(screen.getByTestId("proto-composition-variant").textContent).toBe("a");
    expect(screen.queryByTestId("proto-comp-node-Root/OnlyInB")).toBeNull();

    // …B, then C — the tree MOVES with the strip. This is the assertion that used to be impossible:
    // the frozen composition was the same object whatever the strip said.
    fireEvent.click(screen.getByTestId("proto-variant-var-home-b"));
    expect(await screen.findByTestId("proto-comp-node-Root/OnlyInB")).toBeTruthy();
    expect(screen.getByTestId("proto-composition-variant").textContent).toBe("b");
    expect(screen.queryByTestId("proto-comp-node-Root/OnlyInA")).toBeNull();

    fireEvent.click(screen.getByTestId("proto-variant-var-home-c"));
    const cNode = await screen.findByTestId("proto-comp-node-Root/ClearSearch");
    expect(screen.queryByTestId("proto-comp-node-Root/OnlyInB")).toBeNull();

    // `Open` on a node the tree just proposed BINDS it — no amber "not among the editable nodes".
    // That warning was always right; the tree was the thing that was wrong.
    fireEvent.click(cNode);
    await waitFor(() =>
      expect((screen.getByLabelText("buttonLabel") as HTMLInputElement).value).toBe("Clear search"),
    );
    expect(screen.queryByTestId("usage-not-found")).toBeNull();
    expect(screen.getByTestId("node-edit-scope").textContent).toContain("Root/ClearSearch");
  });

  /**
   * DONE-CRITERION (finding `01KZ3JPVDT7RXEC3P9PVSJ95CN`, CRITICAL) — THE SECOND PAGE HAS PARAMS.
   *
   * `Settings` is NOT pinned in the round (`manifest_entry: null`), which is exactly the page whose
   * Params pane used to be empty under "This page is not pinned in the frozen round — its
   * composition is not part of the election", while the rail ticked it and the strip badged it. The
   * pane reads the page's own variant now, so an unpinned page has a composition like any other and
   * no surface talks about a frozen round the reviewer is not looking at.
   */
  it("shows the composition of a page the round does not pin, with no talk of a frozen round", async () => {
    const tree = protoTree();
    const settings = tree.pages[1]!;
    const version = settings.versions[0]!;
    const unpinned: ProtoTree = {
      ...tree,
      pages: [
        tree.pages[0]!,
        {
          ...settings,
          versions: [
            {
              ...version,
              variants: [
                {
                  ...version.variants[0]!,
                  composition_json: JSON.stringify({
                    key: "Root",
                    children: [{ key: "Reset", component: "Button", params: {} }],
                  }),
                },
              ],
            },
          ],
        },
      ],
    };
    installBackend(unpinned, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              {
                id: "cmp-button",
                project_id: "p1",
                name: "Button",
                source_path: "src/Button.tsx",
                template: "<button>x</button>",
                params_json: null,
                states_json: null,
              },
            ],
            linked_names: ["Button"],
          }
        : undefined,
    );

    renderPage();
    await screen.findByTitle("Home preview");
    fireEvent.click(screen.getByTestId("proto-page-settings"));
    await screen.findByTitle("Settings preview");
    fireEvent.click(screen.getByRole("button", { name: "Params" }));

    expect(await screen.findByTestId("proto-comp-node-Root/Reset")).toBeTruthy();
    expect(screen.queryByText(/not pinned in the frozen round/)).toBeNull();
    expect(screen.queryByText(/frozen round/)).toBeNull();
  });

  /**
   * DONE-CRITERION (finding `01KYT43KG9BR2J338MS6MVQXZW` defect 2, realigned on the backend by
   * `01KYVQE838PNSPRX9DNNDNWDN2`) — A COMPOSITION WITH `[NEW]` NODES STILL DISPLAYS: the renderable
   * part is drawn and every node the SIDECAR could not render is named IN PLACE, inside the frame.
   *
   * …AND THE PAGE DOES NOT RE-LIST THEM UNDER THE STAGE (finding `01KZ3JW5YR6Z7G5EW3VECSPBZZ`). It
   * did, with the same node keys and the same reasons, 600 px lower — while admitting in its own
   * words that each one was "named in place" — plus a sentence about the walk's ORDER that taught
   * nobody anything. What remains is a COUNT in the page's identity line, which costs the scene no
   * height and says the one thing the placeholders cannot: how many there are in total, without
   * scrolling the page to look for them. The sidecar's per-node explanation is one hover away,
   * verbatim, so nothing was lost with the list.
   */
  it("counts the nodes the sidecar could not render, and never re-lists them under the stage", async () => {
    const tree = protoTree();
    const composition = JSON.stringify({
      key: "Root",
      component: "card",
      children: [
        {
          key: "Empty",
          component: "transactions-empty-state",
          params: { title: "none" },
          children: [{ key: "Inner", component: "spinner-partial" }],
        },
        { key: "Reset", component: "button", params: { buttonLabel: "Reset" } },
      ],
    });
    const composed: ProtoTree = {
      ...tree,
      pages: [
        {
          ...tree.pages[0]!,
          versions: [
            {
              ...tree.pages[0]!.versions[0]!,
              variants: [
                { ...tree.pages[0]!.versions[0]!.variants[0]!, composition_json: composition },
              ],
            },
          ],
        },
      ],
    };
    installBackend(composed, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              { id: "c1", project_id: "p1", name: "card", template: "<div>{{{children}}}</div>" },
              { id: "c2", project_id: "p1", name: "button", template: "<button>x</button>" },
            ],
            linked_names: [],
          }
        : undefined,
    );

    // The sidecar's answer: a hole nested UNDER another hole, and the walk records the CHILD first.
    unrenderedNodes = [
      {
        node_key: "Root/Empty/Inner",
        component: "spinner-partial",
        reason: "template_has_partial",
        detail: "'spinner-partial' includes {{> Spinner}} — use data-nyx-use=\"Spinner\" instead.",
      },
      {
        node_key: "Root/Empty",
        component: "transactions-empty-state",
        reason: "not_in_catalogue",
        detail: "'transactions-empty-state' is not in this project's catalogue.",
      },
    ];

    renderPage();

    // 1. The page DREW — the renderable nodes are not held hostage by the unbuilt one.
    const frame = await screen.findByTitle("Home preview");
    await waitFor(() =>
      expect(within(frame).getByTestId("frame-markup").textContent).toContain("button"),
    );
    // 2. The composition crossed to the sidecar VERBATIM — nothing was rewritten on the way out.
    const sent = renderCalls.find((c) => c.includes("Root/Empty") || c.includes('"Empty"'));
    expect(sent).toBe(composition);
    // 3. THE COUNT, in the identity line — and the sidecar's own wording for every one of them,
    //    node key, reason and full explanation, carried verbatim as the badge's title.
    const count = await screen.findByTestId("proto-unrendered-count");
    expect(count.textContent).toContain("2 unrendered");
    const detail = count.getAttribute("title") ?? "";
    expect(detail).toContain("Root/Empty/Inner");
    expect(detail).toContain("template includes a partial");
    expect(detail).toContain("data-nyx-use");
    expect(detail).toContain("Root/Empty");
    expect(detail).toContain("not in the catalogue");
    // 4. NOT ONE ROW OF IT UNDER THE STAGE. The band, its per-node rows and the sentence about the
    //    order the walk records them in are gone — the frame names each node where it stands.
    expect(screen.queryByTestId("proto-holes")).toBeNull();
    expect(screen.queryByTestId("proto-hole-Root/Empty")).toBeNull();
    expect(screen.queryByTestId("proto-hole-Root/Empty/Inner")).toBeNull();
    expect(screen.queryByText(/named in place/i)).toBeNull();
    expect(screen.queryByText(/children before their parent/i)).toBeNull();
  });

  /**
   * …AND THE OTHER HALF OF THAT FINDING: WHY A COMPOSITION THAT LOOKS EMPTY RAISES NOTHING.
   *
   * The audit found a second spec whose composition draws almost nothing and shows no unrendered
   * verdict at all, and asked whether the trigger differs between the two. It does not: the count is
   * keyed on `unrendered[]`, which the sidecar fills for a node it COULD NOT DRAW. A node whose
   * component IS in the catalogue WITH a template is drawn — if what comes out is blank it is
   * because the values never reached it (the real case: `props` on the node where the component
   * declares `params`), and THAT is the node CONTRACT, which has its own verdict beside the node it
   * judges. Two different defects, two different verdicts, one behaviour.
   */
  it("raises no unrendered count for a node that draws empty, and names its contract instead", async () => {
    const tree = protoTree();
    // The shape of the real case: the value is under `props`, a key the component declares nowhere.
    const composition = JSON.stringify({
      key: "Root",
      component: "card",
      children: [{ key: "Empty", component: "table-empty-state", props: { message: "none" } }],
    });
    const composed: ProtoTree = {
      ...tree,
      pages: [
        {
          ...tree.pages[0]!,
          versions: [
            {
              ...tree.pages[0]!.versions[0]!,
              variants: [
                { ...tree.pages[0]!.versions[0]!.variants[0]!, composition_json: composition },
              ],
            },
          ],
        },
      ],
    };
    installBackend(composed, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              { id: "c1", project_id: "p1", name: "card", template: "<div>{{{children}}}</div>" },
              {
                id: "c2",
                project_id: "p1",
                name: "table-empty-state",
                template: "<p>{{message}}</p>",
                params_json: JSON.stringify([{ name: "message", type: "text", required: true }]),
              },
            ],
            linked_names: [],
          }
        : undefined,
    );
    // The sidecar DREW both nodes: there is no hole to report.
    unrenderedNodes = [];

    renderPage();
    await screen.findByTitle("Home preview");

    expect(screen.queryByTestId("proto-unrendered-count")).toBeNull();
    // The emptiness is named where it belongs — on the node, in the params pane.
    fireEvent.click(screen.getByRole("button", { name: /^Params$/ }));
    expect((await screen.findByTestId("node-contract-notice")).textContent).toContain("Root/Empty");
  });

  /**
   * DONE-CRITERION (finding `01KYT44FS853KEJE58HQNQPKCD`) — ONE CAN OPEN AND COMPARE EACH VARIANT,
   * and the ACTIVE one is visible. The screen could only RETAIN a variant: the election was blind.
   */
  it("displays the variant picked in the strip, without electing it", async () => {
    const tree = protoTree();
    const home = tree.pages[0]!;
    const twoVariants: ProtoTree = {
      ...tree,
      pages: [
        {
          ...home,
          versions: [
            {
              ...home.versions[0]!,
              variants: [
                home.versions[0]!.variants[0]!,
                {
                  ...home.versions[0]!.variants[0]!,
                  id: "var-home-b",
                  label: "b",
                  title: "Home B",
                  composition_json: "COMP_HOME_B",
                  states_json: null,
                  is_main: false,
                  position: 1,
                },
              ],
            },
          ],
        },
        tree.pages[1]!,
      ],
    };
    installBackend(twoVariants);
    renderPage();

    const frame = await screen.findByTitle("Home preview");
    await waitFor(() =>
      expect(within(frame).getByTestId("frame-markup").textContent).toBe("rendered:COMP_HOME"),
    );
    expect(screen.getByTestId("proto-variant-var-home").getAttribute("aria-pressed")).toBe("true");

    fireEvent.click(screen.getByTestId("proto-variant-var-home-b"));

    await waitFor(() =>
      expect(
        within(screen.getByTitle("Home preview")).getByTestId("frame-markup").textContent,
      ).toBe("rendered:COMP_HOME_B"),
    );
    expect(renderCalls).toContain("COMP_HOME_B");
    expect(screen.getByTestId("proto-variant-var-home-b").getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(screen.getByTestId("proto-variant-var-home").getAttribute("aria-pressed")).toBe("false");
    // The agent's MAIN and the user's RETAINED stay DISTINCT signifiers.
    expect(screen.getByTestId("proto-variant-reco-var-home").textContent).toContain("MAIN");
    expect(screen.getByTestId("proto-variant-retenu-var-home").textContent).toContain("RETAINED");
    // Comparing is not deciding: nothing was written.
    expect(bridgeFake().calls.find((c) => c.command === "proto_elect")).toBeUndefined();
  });

  /** The decision acts on the variant ON SCREEN — that is what makes the election no longer blind. */
  it("elects the variant currently displayed, from the strip under the surface", async () => {
    const tree = protoTree();
    const home = tree.pages[0]!;
    const withTwo: ProtoTree = {
      ...tree,
      pages: [
        {
          ...home,
          manifest_entry: { ...home.manifest_entry!, chosen_variant_id: null },
          effective_variant_id: null,
          versions: [
            {
              ...home.versions[0]!,
              variants: [
                home.versions[0]!.variants[0]!,
                {
                  ...home.versions[0]!.variants[0]!,
                  id: "var-home-b",
                  label: "b",
                  title: "Home B",
                  composition_json: "COMP_HOME_B",
                  is_main: false,
                  position: 1,
                },
              ],
            },
          ],
        },
        tree.pages[1]!,
      ],
    };
    installBackend(withTwo);
    renderPage();
    await screen.findByTitle("Home preview");

    fireEvent.click(screen.getByTestId("proto-variant-var-home-b"));
    fireEvent.click(await screen.findByTestId("proto-variant-elect-var-home-b"));

    await waitFor(() => {
      const call = bridgeFake().calls.find((c) => c.command === "proto_elect");
      expect(call?.args).toMatchObject({
        roundId: "round1",
        pageId: "pg-home",
        variantId: "var-home-b",
      });
    });
  });

  /** The variant switch lives UNDER the surface, never over it — depot's settled arbitration. */
  it("places the variant strip after the surface in the flow, never an overlay", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    const surface = screen.getByTestId("proto-preview-surface");
    const strip = screen.getByTestId("proto-variant-strip");
    expect(surface.contains(strip)).toBe(false);
    expect(surface.compareDocumentPosition(strip) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(strip.className).not.toContain("absolute");
  });

  /**
   * DONE-CRITERION (finding `01KYW32N1BVDVTGS0WPMG2PJW2`) — THE RENDER IS THE COMPOSITION.
   *
   * Clicking a component IN THE RENDER opens ITS params, and the form carries the link to that
   * component's detail page. This is the gesture that made the separate node list pointless: one
   * designates the element itself instead of naming a path. The frame resolves the click to a
   * `node_key` off the markers the page asked the sidecar to interleave — asserted here down to the
   * composition really sent to `design_render`.
   */
  it("opens a clicked component's params from the render, with a link to its detail", async () => {
    const tree = protoTree();
    const homeComposition = JSON.stringify({
      key: "Root",
      children: [{ key: "Reset", component: "Button", params: { buttonLabel: "Reset filters" } }],
    });
    const composed: ProtoTree = {
      ...tree,
      pages: [
        {
          ...tree.pages[0]!,
          versions: [
            {
              ...tree.pages[0]!.versions[0]!,
              variants: [
                { ...tree.pages[0]!.versions[0]!.variants[0]!, composition_json: homeComposition },
              ],
            },
          ],
        },
        tree.pages[1]!,
      ],
    };
    installBackend(composed, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              {
                id: "cmp-button",
                project_id: "p1",
                name: "Button",
                template: "<button>{{buttonLabel}}</button>",
                params_json: JSON.stringify([{ name: "buttonLabel", type: "text" }]),
                states_json: null,
              },
            ],
            linked_names: ["Button"],
          }
        : undefined,
    );
    renderPage();
    await screen.findByTitle("Home preview");

    // The composition CROSSES ONCE, exactly as it is stored — no marked-up second copy. What makes
    // a click resolvable is stamped by the RENDER, on the other side of the seam (finding
    // `01KYW77Y65A9HYNDZ69M67S6B8`); nothing here rewrites the tree on the way out.
    expect(renderCalls.length, "one render request per composition").toBeGreaterThan(0);
    for (const sent of renderCalls) {
      expect(sent, "the renderer sends the STORED composition, untouched").not.toContain(
        "nyx-node",
      );
    }
    expect(renderCalls.some((c) => c.includes('"Reset"'))).toBe(true);

    // A click in the render, reported by the frame as the node it landed in.
    act(() => frameHarness.onNodeSelect?.("Root/Reset"));

    await waitFor(() =>
      expect((screen.getByLabelText("buttonLabel") as HTMLInputElement).value).toBe(
        "Reset filters",
      ),
    );
    expect(screen.getByTestId("node-edit-scope").textContent).toContain("Root/Reset");
    // …the panel switched to Params by itself, and the selection is RINGED in the render.
    expect(
      within(screen.getByTitle("Home preview")).getByTestId("frame-selected-node").textContent,
    ).toBe("Root/Reset");
    // …and the form carries the way out to the component itself.
    expect(screen.getByTestId("node-component-link").getAttribute("href")).toContain(
      "/design/cmp-button",
    );
  });

  /** The pre-dev comments live in the right panel, with their anchor and their two gestures. */
  it("keeps the anchored pre-dev comments in the panel, composed by PIN, never by a path", async () => {
    const tree = protoTree();
    const withComment: ProtoTree = {
      ...tree,
      open_feedback: [
        {
          id: "fb1",
          prototype_id: "proto1",
          origin_variant_id: "var-home",
          node_key: "Root/Table",
          anchor_status: "anchored",
          body: "Right-align the amounts.",
          status: "open",
          resolution_note: null,
          resolution_via_variant_id: null,
          resolved_at: null,
          ignored_reason: null,
          ignored_at: null,
          author_kind: "user",
          author_ref: null,
          created_at: 0,
          updated_at: 0,
        },
      ],
    };
    installBackend(withComment);
    renderPage();
    await screen.findByTitle("Home preview");

    const panel = screen.getByLabelText("Comments and node params");
    expect(within(panel).getByText("Right-align the amounts.")).toBeTruthy();
    expect(within(panel).getByText("Root/Table")).toBeTruthy();
    // The author is resolved at render time from the comment's own kind — never a guessed name.
    expect(within(panel).getByText("You")).toBeTruthy();
    // The DROPDOWN OF node_keys is gone (finding `01KYW32N1BVDVTGS0WPMG2PJW2`): one PINS on the
    // render, or one comments on the page. Nobody types a path.
    expect(within(panel).queryByTestId("proto-comment-node-select")).toBeNull();
    expect(within(panel).getByTestId("proto-comment-pin-toggle")).toBeTruthy();
    expect(within(panel).getByTestId("proto-comment-submit")).toBeTruthy();
  });

  /**
   * DONE-CRITERION (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`, point 1) — POINTING AT A COMMENT POINTS
   * AT ITS ELEMENT, AND ONLY WHEN THAT ELEMENT IS THE ONE ON SCREEN.
   *
   * The channel already existed — the render rings the node the params pane is bound to — so this
   * REUSES it rather than inventing a second mark. What it must not do is designate under the wrong
   * variant: a `node_key` is a path inside ONE composition, and `Root/Table` in variant B is not the
   * `Root/Table` of variant A. In that case nothing is highlighted at all, and the card's own
   * variant badge is what answers.
   */
  it("highlights an anchored comment's node on hover — and nothing under another variant", async () => {
    const tree = protoTree();
    const home = tree.pages[0]!;
    const secondVariant = {
      ...home.versions[0]!.variants[0]!,
      id: "var-home-b",
      label: "b",
      title: "Home B",
      composition_json: "COMP_HOME_B",
      is_main: false,
      position: 1,
    };
    installBackend({
      ...tree,
      pages: [
        {
          ...home,
          versions: [
            { ...home.versions[0]!, variants: [...home.versions[0]!.variants, secondVariant] },
          ],
        },
        tree.pages[1]!,
      ],
      open_feedback: [
        {
          id: "f-anchored",
          prototype_id: "proto1",
          node_key: "Root/Table",
          origin_variant_id: "var-home",
          body: "right-align the amounts",
          status: "open",
          anchor_status: "anchored",
          author_kind: "user",
          resolution_note: null,
          resolution_via_variant_id: null,
          resolved_at: null,
          ignored_reason: null,
          ignored_at: null,
          author_ref: null,
          created_at: 1,
          updated_at: 1,
        },
        {
          id: "f-page-wide",
          prototype_id: "proto1",
          node_key: null,
          origin_variant_id: "var-home",
          body: "the whole page is too dense",
          status: "open",
          anchor_status: "anchored",
          author_kind: "user",
          resolution_note: null,
          resolution_via_variant_id: null,
          resolved_at: null,
          ignored_reason: null,
          ignored_at: null,
          author_ref: null,
          created_at: 2,
          updated_at: 2,
        },
      ],
    } as ProtoTree);
    renderPage();
    await screen.findByTitle("Home preview");

    const highlighted = () =>
      within(screen.getByTitle("Home preview")).getByTestId("frame-selected-node").textContent;
    expect(highlighted()).toBe("");

    // ANCHORED, on the variant displayed → the render designates its node.
    const anchored = screen.getByTestId("proto-comment-f-anchored");
    expect(anchored.getAttribute("data-designates")).toBe("true");
    fireEvent.mouseEnter(anchored);
    await waitFor(() => expect(highlighted()).toBe("Root/Table"));
    fireEvent.mouseLeave(anchored);
    await waitFor(() => expect(highlighted()).toBe(""));

    // A PAGE-WIDE comment anchors on nothing — there is no element to show.
    const pageWide = screen.getByTestId("proto-comment-f-page-wide");
    expect(pageWide.getAttribute("data-designates")).toBeNull();
    fireEvent.mouseEnter(pageWide);
    expect(highlighted()).toBe("");

    // ANOTHER VARIANT on screen → the same card designates NOTHING rather than a homonym node.
    fireEvent.click(screen.getByTestId("proto-variant-var-home-b"));
    await waitFor(() =>
      expect(
        within(screen.getByTitle("Home preview")).getByTestId("frame-markup").textContent,
      ).toBe("rendered:COMP_HOME_B"),
    );
    const stillThere = screen.getByTestId("proto-comment-f-anchored");
    expect(stillThere.getAttribute("data-designates")).toBeNull();
    fireEvent.mouseEnter(stillThere);
    expect(highlighted()).toBe("");
  });

  /**
   * DONE-CRITERION (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`, points 2 and 3) — THE LIST SCROLLS UNDER A
   * FIXED BOX, AND WHAT IS SETTLED IS FOLDED.
   *
   * jsdom lays nothing out, so what is asserted is the STRUCTURE that produces both: the composer is
   * NOT inside the scrolling region (it was its last child, so the more comments a page carried the
   * further one scrolled to write the next one), and the resolved block renders its COUNT with no
   * card under it until it is asked for.
   */
  it("scrolls the thread under a fixed composer, and folds the resolved comments", async () => {
    const tree = protoTree();
    const comment = (id: string, status: string, body: string) => ({
      id,
      prototype_id: "proto1",
      node_key: null,
      origin_variant_id: "var-home",
      body,
      status,
      anchor_status: "anchored",
      author_kind: "user",
      resolution_note: null,
      resolution_via_variant_id: null,
      resolved_at: null,
      ignored_reason: null,
      ignored_at: null,
      author_ref: null,
      created_at: 1,
      updated_at: 1,
    });
    installBackend({
      ...tree,
      open_feedback: [
        comment("o1", "open", "one"),
        comment("o2", "open", "two"),
        comment("o3", "open", "three"),
        comment("o4", "open", "four"),
      ],
      resolved_feedback: [comment("r1", "resolved", "settled")],
    } as ProtoTree);
    renderPage();
    await screen.findByTitle("Home preview");

    // THE SCROLLER holds the list — and not the box one writes in.
    const scroller = screen.getByTestId("proto-comments-scroll");
    expect(scroller.className).toMatch(/overflow-y-auto/);
    expect(within(scroller).getByTestId("proto-comment-o1")).toBeTruthy();
    expect(within(scroller).queryByTestId("proto-comment-body")).toBeNull();
    expect(screen.getByTestId("proto-comment-body")).toBeTruthy();

    // THE RESOLVED BLOCK is folded, and says how many it holds.
    const toggle = screen.getByTestId("proto-comments-resolved-toggle");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.textContent).toContain("1");
    expect(screen.queryByTestId("proto-comments-resolved-list")).toBeNull();
    expect(screen.queryByTestId("proto-comment-r1")).toBeNull();

    fireEvent.click(toggle);
    expect(screen.getByTestId("proto-comment-r1")).toBeTruthy();
  });

  /**
   * DONE-CRITERION (finding `01KZ41ZM4ZKR3F0WB53WSFJB0F`) — THE TREE IS THE SELECTOR, SO IT COMES
   * FIRST, AND THE ROW BEING EDITED SAYS SO.
   *
   * The form used to sit ABOVE the tree that chooses what it edits — the answer before the question
   * — and nothing linked the two while both were on screen. The DOM order is asserted because it IS
   * the reading order; the mark is asserted on the ROW itself (`aria-current`), which is what makes
   * it not a third badge beside `[DS]` and `Open`.
   */
  it("puts the composition tree above the node form, and marks the row being edited", async () => {
    const tree = protoTree();
    const composed: ProtoTree = {
      ...tree,
      pages: [
        {
          ...tree.pages[0]!,
          versions: [
            {
              ...tree.pages[0]!.versions[0]!,
              variants: [
                {
                  ...tree.pages[0]!.versions[0]!.variants[0]!,
                  composition_json: JSON.stringify({
                    key: "Root",
                    children: [
                      { component: "Button", key: "Reset", params: { buttonLabel: "Reset" } },
                    ],
                  }),
                },
              ],
            },
          ],
        },
        tree.pages[1]!,
      ],
    };
    installBackend(composed, (cmd) =>
      cmd === "design_component_list"
        ? {
            components: [
              {
                id: "c1",
                project_id: "p1",
                name: "Button",
                template: "<button>{{buttonLabel}}</button>",
                params_json: JSON.stringify([{ name: "buttonLabel", type: "text" }]),
                states_json: null,
              },
            ],
            linked_names: ["Button"],
          }
        : undefined,
    );
    renderPage();
    await screen.findByTitle("Home preview");
    fireEvent.click(screen.getByRole("button", { name: "Params" }));

    const tree$ = await screen.findByTestId("proto-composition");
    const form = screen.getByTestId("node-params-panel");
    // THE TREE COMES FIRST — `DOCUMENT_POSITION_FOLLOWING` on the form is exactly "the tree is
    // before it".
    expect(tree$.compareDocumentPosition(form) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    // NOTHING IS BEING EDITED YET — no row claims it.
    const row = screen.getByTestId("proto-comp-node-Root/Reset");
    expect(row.getAttribute("aria-current")).toBeNull();

    fireEvent.click(row);
    await waitFor(() =>
      expect(screen.getByTestId("proto-comp-node-Root/Reset").getAttribute("aria-current")).toBe(
        "true",
      ),
    );
    // …and it is the ROW that says it, not a third badge beside the NATURE and `Open`.
    expect(screen.getByTestId("proto-comp-node-Root/Reset").textContent).toMatch(/\[(DS|NEW)\]/);
    expect(screen.getByTestId("proto-comp-node-Root/Reset").textContent).toContain("Open");
    expect(screen.getByTestId("node-edit-scope").textContent).toContain("Root/Reset");
  });

  /**
   * DONE-CRITERION (finding `01KYW32N1BVDVTGS0WPMG2PJW2`) — PIN MODE ANCHORS ON THE NODE CLICKED.
   *
   * The mechanism is depot's (`docs/web/prototypes-workspace.md` § "Anchored pin popup"): arm the
   * mode, the shim intercepts the next click in the frame and posts the target with its
   * coordinates, the host opens a composer anchored there. What is NOT depot's is the target — a
   * NODE of the named tree, not a computed CSS selector — and this asserts the whole chain down to
   * the `node_key` that reaches `proto_comment_add`.
   */
  it("pins a comment on the node clicked in the render, at the click point", async () => {
    installBackend(protoTree(), (cmd) =>
      cmd === "proto_comment_add" ? { feedback: {} } : undefined,
    );
    renderPage();
    await screen.findByTitle("Home preview");

    // Nothing is armed yet: a click in the render DESIGNATES a node for its params.
    const stageFrame = () => within(screen.getByTitle("Home preview"));
    expect(stageFrame().getByTestId("frame-pick-mode").textContent).toBe("select");

    fireEvent.click(screen.getByTestId("proto-comment-pin-toggle"));
    await waitFor(() =>
      expect(stageFrame().getByTestId("frame-pick-mode").textContent).toBe("pin"),
    );
    // …AND THE PAGE SAYS SO, ON ITSELF (finding `01KZ41YKMB63GFJ3QMK2ZHJN1C`, point 4). A banner in
    // the stage's corner used to spell out what the next click would do, beside a toggle that was
    // already pressed and already read `Pinning`. The state is shown by the SURFACE the click will
    // land on: the page takes a primary border, and drops it when the mode is disarmed.
    expect(screen.queryByTestId("proto-pin-banner")).toBeNull();
    const surface = screen.getByTestId("proto-preview-surface");
    expect(surface.getAttribute("data-pin-armed")).toBe("true");
    expect(surface.className).toMatch(/outline-primary/);

    // The frame reports the click: node + frame-relative point.
    act(() => frameHarness.onNodePin?.({ nodeKey: "Root/Table", x: 120, y: 64 }));

    const popup = await screen.findByTestId("proto-pin-popup");
    expect(screen.getByTestId("proto-pin-node").textContent).toBe("Root/Table");
    expect(popup.style.left).toBe("120px");
    expect(popup.style.top).toBe("64px");

    fireEvent.change(screen.getByTestId("proto-pin-body"), {
      target: { value: "  right-align the amounts  " },
    });
    fireEvent.click(screen.getByTestId("proto-pin-submit"));

    await waitFor(() => {
      const call = bridgeFake().calls.find((c) => c.command === "proto_comment_add");
      expect(call?.args).toMatchObject({
        prototypeId: "proto1",
        body: "right-align the amounts",
        nodeKey: "Root/Table",
        authorKind: "user",
      });
    });
    // The composer closes on submit; the mode stays armed so a second remark costs one click.
    await waitFor(() => expect(screen.queryByTestId("proto-pin-popup")).toBeNull());
    expect(stageFrame().getByTestId("frame-pick-mode").textContent).toBe("pin");
  });

  /** The nodes that already carry an open comment are handed to the frame, to be ringed in pin mode. */
  it("tells the render which nodes already carry an open comment", async () => {
    const tree = protoTree();
    installBackend({
      ...tree,
      open_feedback: [
        {
          id: "fb1",
          prototype_id: "proto1",
          origin_variant_id: "var-home",
          node_key: "Root/Table",
          anchor_status: "anchored",
          body: "Right-align the amounts.",
          status: "open",
          resolution_note: null,
          resolution_via_variant_id: null,
          resolved_at: null,
          ignored_reason: null,
          ignored_at: null,
          author_kind: "user",
          author_ref: null,
          created_at: 0,
          updated_at: 0,
        },
      ],
    });
    renderPage();
    await screen.findByTitle("Home preview");
    expect(
      within(screen.getByTitle("Home preview")).getByTestId("frame-pinned-nodes").textContent,
    ).toBe("Root/Table");
  });

  /**
   * DONE-CRITERION (finding `01KYW32N1BVDVTGS0WPMG2PJW2`) — THE CSS-SLOT INDICATOR LEFT THIS VIEW.
   *
   * A scoped CSS exception belongs to a COMPONENT, and the surface that judges a component is its
   * DETAIL. Here it fired on a page one reads as a page: an amber count in the page's identity line
   * and a dashed `⚡ css slot` badge stamped INSIDE the rendering, over a component nobody is
   * reviewing on this screen. Both are gone — the chip from the header, the mark from the frame. The
   * slots still cross (the Params card names the selected node's own); nothing is recomputed here.
   */
  it("shows no CSS-slot chip and asks the frame for no slot mark", async () => {
    installBackend(protoTree());
    renderCssSlots = [
      {
        scope_id: "c-button-4d1c",
        component: "Button",
        scope: '[data-nyx-css~="c-button-4d1c"]',
        reason: "a custom pulse keyframe",
        features: ["keyframes"],
        bytes: 96,
        css: "…",
        refused: null,
        detail: null,
      },
    ];
    renderPage();
    await screen.findByTitle("Home preview");

    const frame = within(screen.getByTitle("Home preview"));
    expect(screen.queryByTestId("proto-css-slots")).toBeNull();
    expect(screen.queryByText(/CSS slot/i)).toBeNull();
    // …and the frame is told not to draw the mark — the rest of the chrome stays.
    expect(frame.getByTestId("frame-css-slot-mark").textContent).toBe("false");
    expect(frame.getByTestId("frame-chrome").textContent).toBe("true");
  });

  /** A REFUSED slot does not bring the chip back either: this view says nothing about slots. */
  it("says nothing about a refused slot on the prototype view", async () => {
    installBackend(protoTree());
    renderCssSlots = [
      {
        scope_id: "c-button-4d1c",
        component: "Button",
        scope: '[data-nyx-css~="c-button-4d1c"]',
        reason: "a custom pulse keyframe",
        features: [],
        bytes: 96,
        css: "",
        refused: "hardcoded_colour",
        detail: "a colour written in the CSS instead of referenced from the token layer",
      },
    ];
    renderPage();
    await screen.findByTitle("Home preview");

    expect(screen.queryByTestId("proto-css-slots")).toBeNull();
    expect(
      within(screen.getByTitle("Home preview")).getByTestId("frame-css-slot-mark").textContent,
    ).toBe("false");
  });

  /**
   * A REFUSED RENDER REACHES THE STAGE AS A SENTENCE (finding `01KYVT6HVA260DTAEREQA4DFJN`).
   *
   * The bridge rejects with a `BridgeError` OBJECT. This surface read it as `e instanceof Error`
   * first, so EVERY refusal of `design_render` was shown as `[object Object]` — seen in the running
   * app. The refusal is injected here in its TRUE shape, and what must appear is the sidecar's own
   * sentence: it names the composition and the cure, and this page owns neither.
   */
  it("shows the sidecar's own refusal on the stage, never [object Object]", async () => {
    const REFUSAL =
      "design_render: the composition of variant var-home is not valid JSON at line 3 — re-author it (proto_author_page)";
    installBackend(protoTree(), (cmd) => {
      if (cmd !== "design_render") return undefined;
      throw { kind: "command", message: REFUSAL, source: "design_render" };
    });
    renderPage();

    const error = await screen.findByTestId("proto-preview-error");
    expect(error.textContent).toContain("not valid JSON at line 3");
    expect(error.textContent).not.toContain("[object Object]");
  });

  /** No prototype → the clean cue, with the way back to the Spec. */
  it("shows a clean empty state (and a way back) when the Spec has no prototype", async () => {
    installBackend(null);
    renderPage();
    expect(await screen.findByText(/no prototype yet/i)).toBeTruthy();
    fireEvent.click(screen.getByTestId("proto-back"));
    expect(await screen.findByTestId("spec-detail-stub")).toBeTruthy();
  });

  /**
   * DONE-CRITERION (finding `01KZ3JN69E32FYBF01NBE4FWJD` volet A, CRITICAL) — THE RENDER IS GIVEN
   * THE PROTOTYPE.
   *
   * `prototypeId` is the page DIRECTORY every `navigate_to` is resolved against. It is optional on
   * the wire and no caller sent it, so every link of every page came back `unknown_page` — stamped
   * `data-nyx-page-missing`, carrying no `data-nyx-page`, un-navigable — and nothing anywhere said
   * so. The STAGE and the RAIL MINIATURES both render, so both must be given it.
   */
  it("renders every surface against the prototype's page directory", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");
    await waitFor(() => expect(renderArgs.length).toBeGreaterThan(0));
    for (const args of renderArgs) expect(args.prototypeId).toBe("proto1");
  });

  /**
   * …AND `navigation[]` HAS A CONSUMER. Nothing read the list, `navigation_count` or
   * `navigation_broken_count`: the parcours could be entirely dead and the screen said nothing.
   * A link that works and a link whose page is gone are both listed, each with the sidecar's status.
   */
  it("lists the page's links, and names the ones that no longer navigate", async () => {
    installBackend(protoTree());
    renderNavigation = [
      {
        node_key: "Root/Go",
        page_id: "pg-set",
        page: "settings",
        page_title: "Settings",
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
    ];
    renderPage();
    await screen.findByTitle("Home preview");

    const links = await screen.findByTestId("proto-links");
    expect(screen.getByTestId("proto-links-count").textContent).toContain("2 links");
    expect(screen.getByTestId("proto-links-count").textContent).toContain("1 broken");
    expect(within(links).getByTestId("proto-link-Root/Go").textContent).toContain("Settings");
    expect(within(links).getByTestId("proto-link-Root/Go").textContent).toContain("navigates");
    // The DEAD one is named with the id that resolved to nothing — the only thing left of it.
    const dead = within(links).getByTestId("proto-link-Root/Ghost");
    expect(dead.textContent).toContain("pg-deleted");
    expect(dead.textContent).toContain("page is gone");
  });

  /** A page that links nowhere owes the reviewer no line about links. */
  it("shows no links strip when the page links nowhere", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");
    expect(screen.queryByTestId("proto-links")).toBeNull();
  });

  /**
   * DONE-CRITERION (finding `01KZ3JN72TM2XGBW2CTQRCBNER`, CRITICAL) — THE THREAD AND ITS COUNTER
   * FOLLOW THE PAGE, and a comment written from here records the variant it was raised on.
   */
  it("scopes the comment thread and its counter to the page on screen", async () => {
    const tree = protoTree();
    const comment = (id: string, variantId: string) => ({
      id,
      prototype_id: "proto1",
      origin_variant_id: variantId,
      node_key: "Root/Save",
      anchor_status: "anchored",
      body: `about ${id}`,
      status: "open",
      resolution_note: null,
      resolution_via_variant_id: null,
      resolved_at: null,
      ignored_reason: null,
      ignored_at: null,
      author_kind: "user",
      author_ref: null,
      created_at: 0,
      updated_at: 0,
    });
    installBackend({
      ...tree,
      open_feedback: [comment("c-home", "var-home"), comment("c-set", "var-set")],
    });
    renderPage();
    await screen.findByTitle("Home preview");

    // HOME: its own comment, and the segment counts ONE — it used to read the prototype's total on
    // both pages.
    expect(screen.getByRole("button", { name: /^Comments/ }).textContent).toContain("1");
    expect(await screen.findByTestId("proto-comment-c-home")).toBeTruthy();
    expect(screen.queryByTestId("proto-comment-c-set")).toBeNull();

    // SETTINGS: the other one, and only it.
    fireEvent.click(screen.getByTestId("proto-page-settings"));
    await screen.findByTitle("Settings preview");
    expect(await screen.findByTestId("proto-comment-c-set")).toBeTruthy();
    expect(screen.queryByTestId("proto-comment-c-home")).toBeNull();
  });

  /** A comment written from the page records the VARIANT on screen — the field nothing ever sent. */
  it("records the variant on screen on a comment written from the page", async () => {
    installBackend(protoTree());
    renderPage();
    await screen.findByTitle("Home preview");

    fireEvent.change(screen.getByTestId("proto-comment-body"), { target: { value: "trop dense" } });
    fireEvent.click(screen.getByTestId("proto-comment-submit"));

    await waitFor(() => {
      const call = bridgeFake().calls.find((c) => c.command === "proto_comment_add");
      expect(call?.args).toMatchObject({ prototypeId: "proto1", originVariantId: "var-home" });
    });
  });
});
