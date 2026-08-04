import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { bridgeFake, mockIPC } from "@/bridge/test-harness";

import type { ProtoTree } from "../proto-types";
import type { SpecDetail } from "../types";
import { PrototypeTab } from "./prototype-tab";

/**
 * The Prototype TAB, after the elected v8 design (finding `01KYVMSMMBVSZNBBSBE18PRZAK`): it is an
 * ENTRY POINT, not the workbench. The stage, the variant strip, the node params, the holes and the
 * elected composition all moved to `/work/$specId/prototype` (see `prototype-page.test.tsx`) —
 * because in a tab the stage measured 486 × 489.
 *
 * What must be asserted HERE is that the tab is exactly three things (entry card, contact sheet,
 * read-only thread), that it opens the page, and that it is no longer a workbench.
 */
vi.mock("../../design/composition-frame", () => ({
  CompositionFrame: (props: { markup: string; title?: string }) => (
    <div data-testid="composition-frame" title={props.title}>
      {props.markup}
    </div>
  ),
}));

/** A minimal `spec_detail` anchored on a project (so the tab has a projectId for the miniatures). */
function detailFixture(): SpecDetail {
  return {
    spec: { id: "s1", project_id: "p1", title: "Spec", number: 1 },
    revision: {},
    tasks: [],
    dependencies: [],
    workspace_context: {
      spec_id: "s1",
      workspace: { id: "w1", project_id: "p1", name: "wt", path: "/tmp/wt" },
      project: { id: "p1", name: "Nyx", key: "NYX" },
      repos: [],
    },
  } as unknown as SpecDetail;
}

/** A two-page prototype tree: Home (retained) + Settings (nothing retained yet). */
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

function installBackend(tree: ProtoTree | null): void {
  mockIPC((cmd, args) => {
    switch (cmd) {
      case "proto_tree":
        return tree;
      case "design_get":
        return { id: "d1", project_id: "p1", content: "# DESIGN", theme_tokens_json: null };
      case "design_render":
        return { markup: `rendered:${(args as { compositionJson?: string }).compositionJson}` };
      default:
        return undefined;
    }
  });
}

/** Render the tab under a memory router carrying the `/work/$specId/prototype` target route. */
function renderTab(detail: SpecDetail = detailFixture()) {
  const root = createRootRoute();
  const index = createRoute({
    getParentRoute: () => root,
    path: "/",
    component: () => <PrototypeTab detail={detail} />,
  });
  const proto = createRoute({
    getParentRoute: () => root,
    path: "/work/$specId/prototype",
    component: function Stub() {
      const search = proto.useSearch() as { page?: string };
      return <div data-testid="prototype-page-stub">PROTOTYPE PAGE {search.page ?? ""}</div>;
    },
    validateSearch: (s: Record<string, unknown>) =>
      typeof s.page === "string" ? { page: s.page } : {},
  });
  const router = createRouter({
    routeTree: root.addChildren([index, proto]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return render(<RouterProvider router={router} />);
}

describe("Prototype TAB — the entry point, not the workbench", () => {
  beforeEach(() => {
    installBackend(protoTree());
  });

  it("shows a clean empty state when the Spec has no prototype", async () => {
    installBackend(null);
    renderTab();
    expect(await screen.findByText(/no prototype yet/i)).toBeTruthy();
    expect(screen.queryByTestId("open-prototype")).toBeNull();
  });

  /**
   * DONE-CRITERION — THE TAB IS AN ENTRY CARD. It states what there is (pages, arbitrations owed)
   * and hands the user to the page that can hold a real web page — and it EXPLAINS NOTHING (finding
   * `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`): the two sentences describing what the button beside them does are
   * gone, and so is the comment count the thread underneath already carries.
   */
  it("states what there is to look at and offers Open prototype", async () => {
    renderTab();
    // 2 pages, one of which still owes an arbitration.
    expect(await screen.findByText("2 pages · 1 to elect")).toBeTruthy();
    expect(screen.getByTestId("open-prototype")).toBeTruthy();
    // The paragraph that described the dedicated page next to the button that opens it.
    expect(screen.queryByText(/the stage lives on its own page/i)).toBeNull();
    expect(screen.queryByText(/the tab keeps the thread/i)).toBeNull();
  });

  it("navigates to /work/$specId/prototype when Open prototype is clicked", async () => {
    renderTab();
    fireEvent.click(await screen.findByTestId("open-prototype"));
    expect(await screen.findByTestId("prototype-page-stub")).toBeTruthy();
  });

  /** The contact sheet says WHAT exists — and each card opens the page ON that page. */
  it("shows a contact sheet whose cards deep-link into their own page", async () => {
    renderTab();
    expect(await screen.findByTestId("proto-contact-sheet")).toBeTruthy();
    expect(screen.getByTestId("proto-sheet-home")).toBeTruthy();

    fireEvent.click(screen.getByTestId("proto-sheet-settings"));
    const stub = await screen.findByTestId("prototype-page-stub");
    expect(stub.textContent).toContain("settings");
  });

  /**
   * DONE-CRITERION — THE WORKBENCH IS GONE FROM THE TAB. Every one of these used to live here and
   * take height from a stage that had none to give.
   */
  it("no longer renders the stage, the strip, the params editor or the elected composition", async () => {
    renderTab();
    await screen.findByTestId("open-prototype");

    expect(screen.queryByTestId("proto-preview-surface")).toBeNull();
    expect(screen.queryByTestId("proto-variant-strip")).toBeNull();
    expect(screen.queryByTestId("node-params-panel")).toBeNull();
    expect(screen.queryByTestId("proto-composition")).toBeNull();
    expect(screen.queryByTestId("proto-composition-empty")).toBeNull();
    expect(screen.queryByTestId("proto-holes")).toBeNull();
  });

  /** The pre-dev thread stays readable in the tab — but anchoring needs the page's composition. */
  it("keeps the comment thread, read-only (no composer without a composition on screen)", async () => {
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
    renderTab();

    expect(await screen.findByText("Right-align the amounts.")).toBeTruthy();
    expect(screen.queryByTestId("proto-comment-node-select")).toBeNull();
    expect(screen.queryByTestId("proto-comment-submit")).toBeNull();
    // …AND THE COUNT IS WRITTEN ONCE (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`). It sat in the entry
    // card's summary AND on the thread's own header, five centimetres apart. The thread counts
    // itself, so the summary stopped.
    expect(screen.getByText("1 open")).toBeTruthy();
    expect(screen.queryByText(/open comment/i)).toBeNull();
    expect(screen.getByText("2 pages · 1 to elect")).toBeTruthy();
  });

  /** The rounds history stays in the tab header: it is a property of the prototype, not the stage. */
  it("keeps the rounds control in the header, and it CREATES no round", async () => {
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
    renderTab();

    const trigger = await screen.findByRole("button", { name: /rounds history/i });
    // WAIT FOR THE TRIGGER TO BE WIRED, not merely to exist (finding
    // `01KYW0SA15GX1CZ4QFVY9VD75G`). Base UI mounts the button first and attaches the menu
    // interaction — `aria-haspopup`, `aria-expanded`, the open handler — right after; a
    // `findBy*` resolves on presence and could hand back the bare button, and the click
    // then opened nothing at all (`aria-expanded` was absent, and the assertion below died
    // on a menu that was never going to appear). `aria-expanded` IS that wiring, so it is
    // the observable condition to wait on.
    await waitFor(() => expect(trigger).toHaveAttribute("aria-expanded", "false"));
    fireEvent.click(trigger);
    const earlier = await screen.findByTestId("proto-round-round0");

    // The history is CONSULTATION ONLY (finding `01KYW32N1BVDVTGS0WPMG2PJW2`): clicking an earlier
    // round says nothing to the sidecar, because opening a round is the agent's work.
    fireEvent.click(earlier);
    await waitFor(() => expect(screen.getByTestId("proto-round-round0")).toBeTruthy());
    expect(bridgeFake().calls.find((c) => c.command === "proto_round_rebase")).toBeUndefined();
  });
});
