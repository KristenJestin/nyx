import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import type {
  ProtoTree,
  ProtoTreePage,
  PrototypeFeedback,
  PrototypeVariant,
} from "../specs/proto-types";
import { FEEDBACK_STATUS_OPEN } from "../specs/proto-types";
import { OverlayPrototypes } from "./overlay-prototypes";

/**
 * THE PROTOTYPE, REACHABLE FROM THE OVERLAY (finding `01KYW342EE5DN6F70YB8VSTJR8`, point 1).
 *
 * The overlay is what a user reads while an agent works, and it said nothing at all about the
 * prototype: reaching a page meant leaving the overlay, opening the Spec, its Prototype tab, then
 * the page. This suite holds the list to the three things that make it worth existing — it LISTS the
 * pages, it opens the WORKBENCH (never a scene of its own) and it opens it ON THE RIGHT PAGE — plus
 * the silence it owes a Spec that has no prototype.
 */

// `useNavigate` is stubbed to a shared spy (via `vi.hoisted`, so the factory can reference it):
// a click must call THIS with the typed route + its `?page=` deep-link, and touch no real router.
const { navigate } = vi.hoisted(() => ({ navigate: vi.fn() }));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => navigate,
}));

function variant(overrides: Partial<PrototypeVariant>): PrototypeVariant {
  return {
    id: "v1",
    page_version_id: "pv1",
    label: "A",
    title: "Variant A",
    composition_json: '{"component":"Root"}',
    states_json: null,
    is_main: true,
    position: 0,
    created_at: 0,
    ...overrides,
  };
}

/** One page of the tree: `variants` drives what is still to elect, `chosen` the retained one. */
function page(
  id: string,
  slug: string,
  title: string,
  variants: PrototypeVariant[],
  chosen: string | null,
): ProtoTreePage {
  return {
    page: { id, prototype_id: "proto1", slug, title, position: 0, created_at: 0 },
    versions: [
      {
        version: {
          id: `${id}-v`,
          page_id: id,
          label: "v1",
          summary: null,
          created_at: 0,
          archived_at: null,
        },
        variants,
      },
    ],
    manifest_entry: null,
    effective_variant_id: chosen,
  };
}

function feedback(overrides: Partial<PrototypeFeedback>): PrototypeFeedback {
  return {
    id: "f1",
    prototype_id: "proto1",
    origin_variant_id: null,
    node_key: null,
    anchor_status: "anchored",
    body: "…",
    status: FEEDBACK_STATUS_OPEN,
    resolution_note: null,
    resolution_via_variant_id: null,
    resolved_at: null,
    ignored_reason: null,
    ignored_at: null,
    author_kind: "user",
    author_ref: null,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

function tree(pages: ProtoTreePage[], openFeedback: PrototypeFeedback[] = []): ProtoTree {
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
    current_round: null,
    design_lock: null,
    pages,
    open_feedback: openFeedback,
    resolved_feedback: [],
  };
}

/** Install `proto_tree` — `answer` is what the db-command returns (`null` = no prototype). */
function installTree(answer: ProtoTree | null | "throw") {
  mockIPC((cmd) => {
    if (cmd !== "proto_tree") throw new Error(`unexpected ${cmd}`);
    if (answer === "throw") throw new Error("read failed");
    return answer;
  });
}

const HOME = variant({ id: "v-home" });
const SETTINGS_A = variant({ id: "v-set-a", label: "A" });
const SETTINGS_B = variant({ id: "v-set-b", label: "B", is_main: false });

describe("<OverlayPrototypes> — the Spec's prototype pages, inside the overlay", () => {
  beforeEach(() => navigate.mockClear());

  it("lists one row per page, with its title and its slug", async () => {
    installTree(
      tree([
        page("p-home", "home", "Home", [HOME], "v-home"),
        page("p-set", "settings", "Settings", [SETTINGS_A, SETTINGS_B], null),
      ]),
    );
    render(<OverlayPrototypes specId="s1" />);

    const section = await screen.findByTestId("overlay-prototype-pages");
    expect(section).toHaveTextContent("2 pages");
    expect(screen.getByTestId("overlay-proto-page-home")).toHaveTextContent("Home");
    expect(screen.getByTestId("overlay-proto-page-home")).toHaveTextContent("home");
    expect(screen.getByTestId("overlay-proto-page-settings")).toHaveTextContent("Settings");
  });

  /**
   * THE CLICK LANDS ON THE PAGE, NOT IN THE OVERLAY. The workbench moved to its own route because a
   * prototype stands for a real web page; a 380 px panel is even less the size of one than the tab
   * was. So the row opens `/work/$specId/prototype` — and on the page that was clicked, through the
   * route's own `?page=<slug>` deep-link.
   */
  it("opens the WORKBENCH route on the clicked page (`?page=<slug>`)", async () => {
    installTree(
      tree([
        page("p-home", "home", "Home", [HOME], "v-home"),
        page("p-set", "settings", "Settings", [SETTINGS_A], "v-set-a"),
      ]),
    );
    render(<OverlayPrototypes specId="s42" />);

    fireEvent.click(await screen.findByTestId("overlay-proto-page-settings"));
    expect(navigate).toHaveBeenCalledWith({
      to: "/work/$specId/prototype",
      params: { specId: "s42" },
      search: { page: "settings" },
    });

    fireEvent.click(screen.getByTestId("overlay-proto-page-home"));
    expect(navigate).toHaveBeenLastCalledWith({
      to: "/work/$specId/prototype",
      params: { specId: "s42" },
      search: { page: "home" },
    });
  });

  /** What a page still OWES, in the words the two prototype surfaces already use. */
  it("flags a page that still owes an arbitration and its open comments", async () => {
    installTree(
      tree(
        [
          page("p-home", "home", "Home", [HOME], "v-home"),
          page("p-set", "settings", "Settings", [SETTINGS_A, SETTINGS_B], null),
        ],
        [feedback({ id: "f1", origin_variant_id: "v-set-a" })],
      ),
    );
    render(<OverlayPrototypes specId="s1" />);

    const settings = await screen.findByTestId("overlay-proto-page-settings");
    expect(settings).toHaveTextContent("to elect");
    expect(settings).toHaveTextContent("1");
    // The elected page owes nothing and says nothing.
    expect(screen.getByTestId("overlay-proto-page-home")).not.toHaveTextContent("to elect");
  });

  /** A jump must never read as a toggle of the panel it was launched from. */
  it("does NOT bubble the click to a parent handler", async () => {
    installTree(tree([page("p-home", "home", "Home", [HOME], "v-home")]));
    const onParentClick = vi.fn();
    render(
      <div onClick={onParentClick}>
        <OverlayPrototypes specId="s1" />
      </div>,
    );
    fireEvent.click(await screen.findByTestId("overlay-proto-page-home"));
    expect(onParentClick).not.toHaveBeenCalled();
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  /**
   * SILENCE FOR A SPEC WITH NO PROTOTYPE. The overlay rides over a live terminal; a block claiming
   * an absence would be chrome charged to every Spec that has none. `proto_tree` answers `null` for
   * such a Spec — a valid state, not an error — and a read that FAILED degrades to the same silence
   * rather than to a broken card over the terminal.
   */
  it("renders nothing at all when the Spec has no prototype, no page, or the read failed", async () => {
    for (const answer of [null, tree([]), "throw" as const]) {
      installTree(answer);
      const { container, unmount } = render(<OverlayPrototypes specId="s1" />);
      await waitFor(() => expect(container).toBeEmptyDOMElement());
      unmount();
    }
  });
});
