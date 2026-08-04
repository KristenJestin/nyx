import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { bridgeFake, mockIPC } from "@/bridge/test-harness";

import type { ProtoTree, PrototypeFeedback } from "../proto-types";
import { useProtoActions } from "../use-proto-actions";
import { NodeComments } from "./node-comments";

/**
 * T28 — the PRE-DEV review THREAD. These assert the two gestures the panel really offers (finding
 * `01KYW32N1BVDVTGS0WPMG2PJW2`): a GLOBAL comment (`proto_comment_add` with NO `nodeKey`, authored
 * as the USER) and the PIN MODE toggle that arms the render — and that the `node_key` DROPDOWN is
 * gone, because a reviewer looking at a page must not be asked to name a path. Plus the resolution
 * (`proto_comment_resolve`), the open/resolved display, and the "to re-anchor" distinction. The
 * write hook is the REAL `useProtoActions`, so a click drives the sidecar command through the bridge
 * fake — the SAME pattern as the variant-election tests.
 */

function feedback(overrides: Partial<PrototypeFeedback>): PrototypeFeedback {
  return {
    id: "f1",
    prototype_id: "proto1",
    origin_variant_id: null,
    node_key: "Root/Save",
    anchor_status: "anchored",
    body: "serrer l’espacement",
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
    ...overrides,
  };
}

function tree(open: PrototypeFeedback[], resolved: PrototypeFeedback[]): ProtoTree {
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
    pages: [],
    open_feedback: open,
    resolved_feedback: resolved,
  };
}

/**
 * Wires the REAL write hook so a click drives the sidecar `proto_comment_*` command. `flush` is the
 * shape the prototype workbench mounts (no card chrome, no fold) — the one the ordering finding
 * `01KZ3AN7VVAPXMYFQG4AP441BC` is about.
 */
function Harness({
  t,
  onPin,
  flush,
  pageId = null,
  originVariantId = null,
}: {
  t: ProtoTree;
  onPin?: (next: boolean) => void;
  flush?: boolean;
  pageId?: string | null;
  originVariantId?: string | null;
}) {
  const actions = useProtoActions();
  return (
    <NodeComments
      tree={t}
      actions={actions}
      flush={flush}
      pageId={pageId}
      originVariantId={originVariantId}
      onTogglePinMode={onPin}
    />
  );
}

/**
 * A TWO-PAGE prototype whose pages each offer variants — the shape finding
 * `01KZ3JN72TM2XGBW2CTQRCBNER` is about. `origin_variant_id` is what attaches a comment to one of
 * them, and it is the field the renderer never sent.
 */
function twoPageTree(open: PrototypeFeedback[], resolved: PrototypeFeedback[] = []): ProtoTree {
  const variant = (id: string, label: string, versionId: string) => ({
    id,
    page_version_id: versionId,
    label,
    title: label,
    composition_json: "{}",
    states_json: null,
    is_main: label === "a",
    position: 0,
    created_at: 0,
  });
  const page = (id: string, slug: string, title: string, labels: string[]) => ({
    page: { id, prototype_id: "proto1", slug, title, position: 0, created_at: 0 },
    versions: [
      {
        version: {
          id: `pv-${id}`,
          page_id: id,
          label: "v1",
          summary: null,
          created_at: 0,
          archived_at: null,
        },
        variants: labels.map((l) => variant(`var-${id}-${l}`, l, `pv-${id}`)),
      },
    ],
    manifest_entry: null,
    effective_variant_id: null,
  });
  return {
    ...tree(open, resolved),
    pages: [
      page("pg-home", "home", "Home", ["a", "b", "c"]),
      page("pg-set", "settings", "Settings", ["a"]),
    ],
  };
}

describe("<NodeComments> (T28 — the pre-dev review thread)", () => {
  it("writes a GLOBAL comment (no node_key) as the user", async () => {
    mockIPC((cmd) => (cmd === "proto_comment_add" ? { feedback: {} } : undefined));
    render(<Harness t={tree([], [])} />);

    fireEvent.change(screen.getByTestId("proto-comment-body"), {
      target: { value: "  serrer l’espacement  " },
    });
    fireEvent.click(screen.getByTestId("proto-comment-submit"));

    await waitFor(() => {
      const call = bridgeFake().calls.find((c) => c.command === "proto_comment_add");
      expect(call?.args).toMatchObject({
        prototypeId: "proto1",
        body: "serrer l’espacement",
        authorKind: "user",
      });
      // A page-wide comment carries NO anchor — the backend stores it global.
      expect(call?.args?.nodeKey).toBeUndefined();
    });
  });

  it("offers PIN MODE instead of a node_key dropdown — the anchor is picked ON the render", () => {
    mockIPC(() => undefined);
    const picks: boolean[] = [];
    render(<Harness t={tree([], [])} onPin={(next) => picks.push(next)} />);

    // The dropdown of paths is GONE: one does not name a node, one points at it.
    expect(screen.queryByTestId("proto-comment-node-select")).toBeNull();
    fireEvent.click(screen.getByTestId("proto-comment-pin-toggle"));
    expect(picks).toEqual([true]);
  });

  it("resolves an open comment via proto_comment_resolve", async () => {
    mockIPC((cmd) => (cmd === "proto_comment_resolve" ? { feedback: {} } : undefined));
    render(<Harness t={tree([feedback({ id: "f1" })], [])} />);

    fireEvent.click(screen.getByTestId("proto-comment-resolve-f1"));

    await waitFor(() => {
      const call = bridgeFake().calls.find((c) => c.command === "proto_comment_resolve");
      expect(call?.args).toMatchObject({ feedbackId: "f1" });
    });
  });

  it("shows the OPEN (resolvable) and RESOLVED (read-only) comments", () => {
    mockIPC(() => undefined);
    render(
      <Harness
        t={tree(
          [feedback({ id: "open1", body: "revoir le bouton" })],
          [feedback({ id: "res1", body: "corrigé", status: "resolved", resolution_note: "fait" })],
        )}
      />,
    );

    expect(screen.getByTestId("proto-comment-open1")).toBeTruthy();
    expect(screen.getByTestId("proto-comment-res1")).toBeTruthy();
    expect(screen.getByText("revoir le bouton")).toBeTruthy();
    // The open comment is resolvable; the resolved one is read-only (no resolve button).
    expect(screen.getByTestId("proto-comment-resolve-open1")).toBeTruthy();
    expect(screen.queryByTestId("proto-comment-resolve-res1")).toBeNull();
  });

  it("distinguishes an orphaned comment (to re-anchor) from an anchored one, code-review style", () => {
    mockIPC(() => undefined);
    render(
      <Harness
        t={tree(
          [
            feedback({ id: "anch", anchor_status: "anchored" }),
            feedback({ id: "orph", anchor_status: "orphaned", node_key: "Root/Gone" }),
          ],
          [],
        )}
      />,
    );

    // Only the orphaned comment carries the "To re-anchor" marker + the orphaned card status.
    expect(screen.getByTestId("proto-comment-orphaned-orph")).toBeTruthy();
    expect(screen.queryByTestId("proto-comment-orphaned-anch")).toBeNull();
    expect(screen.getByTestId("proto-comment-orph").getAttribute("data-anchor-status")).toBe(
      "orphaned",
    );
    expect(screen.getByTestId("proto-comment-anch").getAttribute("data-anchor-status")).toBe(
      "anchored",
    );
    // The COLLAPSIBLE header keeps the "to re-anchor" tally: there the list can be folded away, and
    // a summary of something hidden is the only thing left to read.
    expect(screen.getByTestId("proto-comments-orphan-count").textContent).toMatch(/1 to re-anchor/);
  });

  /**
   * …but NOT over an OPEN list (finding `01KZ3AN7VVAPXMYFQG4AP441BC`). In the workbench's `flush`
   * shape the cards are right there, each badged `To re-anchor`: a count above them is the same
   * fact, counted.
   */
  it("carries no re-anchor tally over the flush (always-open) list", () => {
    mockIPC(() => undefined);
    render(
      <Harness
        flush
        t={tree([feedback({ id: "orph", anchor_status: "orphaned", node_key: "Root/Gone" })], [])}
      />,
    );

    expect(screen.queryByTestId("proto-comments-orphan-count")).toBeNull();
    expect(screen.getByTestId("proto-comment-orphaned-orph")).toBeTruthy();
  });

  /**
   * DONE-CRITERION of finding `01KZ3AN7VVAPXMYFQG4AP441BC` — THE THREAD READS DOWNWARDS. « quand on
   * est logique on affiche la liste des commentaires et en dessous le formulaire pour en rajouter un
   * nouveau ». The composer used to sit on top; the list is first now, the box last, and the pin
   * TOGGLE went down with the box (it is a verb, not a field, and it must not displace the list).
   */
  it("puts the LIST first and the composer last, pin toggle included", () => {
    mockIPC(() => undefined);
    render(<Harness flush t={tree([feedback({ id: "open1" })], [])} onPin={() => {}} />);

    const list = screen.getByTestId("proto-comment-open1");
    const box = screen.getByTestId("proto-comment-body");
    const pin = screen.getByTestId("proto-comment-pin-toggle");
    // DOCUMENT_POSITION_FOLLOWING = 4: the composer comes AFTER the comment it is under.
    expect(list.compareDocumentPosition(box) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(list.compareDocumentPosition(pin) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    // …and the paragraph that explained what an anchored comment is went with it. The stage says it,
    // while the mode is armed, where the click will land.
    expect(screen.queryByTestId("proto-comment-pin-hint")).toBeNull();
    expect(screen.queryByText(/An anchored comment is placed ON the rendering/i)).toBeNull();
  });

  it("shows an empty state when the prototype has no comments", () => {
    mockIPC(() => undefined);
    render(<Harness t={tree([], [])} />);
    expect(screen.getByTestId("proto-comments-empty")).toBeTruthy();
  });

  it("disables submit until a body is typed", () => {
    mockIPC(() => undefined);
    render(<Harness t={tree([], [])} />);
    expect((screen.getByTestId("proto-comment-submit") as HTMLButtonElement).disabled).toBe(true);
  });

  /**
   * DONE-CRITERION (finding `01KZ3JN72TM2XGBW2CTQRCBNER`, CRITICAL) — THE THREAD FOLLOWS THE PAGE.
   *
   * It listed the whole prototype's comments whatever page was on screen, counter included: a remark
   * written on page 2 read as `page-wide` on page 1 with nothing saying where it came from.
   */
  it("shows only the comments of the page on screen", () => {
    mockIPC(() => undefined);
    render(
      <Harness
        flush
        pageId="pg-home"
        t={twoPageTree([
          feedback({ id: "on-home", origin_variant_id: "var-pg-home-a" }),
          feedback({ id: "on-settings", origin_variant_id: "var-pg-set-a" }),
        ])}
      />,
    );
    expect(screen.getByTestId("proto-comment-on-home")).toBeTruthy();
    expect(screen.queryByTestId("proto-comment-on-settings")).toBeNull();
  });

  /**
   * …AND AN ANCHORED COMMENT SAYS WHICH VARIANT IT WAS RAISED ON. A `node_key` alone is not a place
   * on a page offering three variants: `Root/ClearSearch` exists in C and in neither A nor B.
   */
  it("names the variant a comment was raised on", () => {
    mockIPC(() => undefined);
    render(
      <Harness
        flush
        pageId="pg-home"
        t={twoPageTree([
          feedback({
            id: "pinned",
            node_key: "Root/ClearSearch",
            origin_variant_id: "var-pg-home-c",
          }),
        ])}
      />,
    );
    expect(screen.getByTestId("proto-comment-origin-pinned").textContent).toContain("c");
  });

  /**
   * A comment with NO recorded origin belongs to no page — it is shown APART rather than attributed
   * to whichever page happens to be on screen, which would be inventing the missing fact.
   */
  it("keeps a comment with no recorded origin out of the page's thread, and says so", () => {
    mockIPC(() => undefined);
    render(
      <Harness
        flush
        pageId="pg-home"
        t={twoPageTree([
          feedback({ id: "legacy", origin_variant_id: null }),
          feedback({ id: "on-home", origin_variant_id: "var-pg-home-a" }),
        ])}
      />,
    );
    const group = screen.getByTestId("proto-comments-unattributed");
    expect(within(group).getByTestId("proto-comment-legacy")).toBeTruthy();
    expect(within(group).queryByTestId("proto-comment-on-home")).toBeNull();
    expect(screen.getByTestId("proto-comment-no-origin-legacy")).toBeTruthy();
  });

  /** A host that shows NO page in particular lists them all, each carrying its page. */
  it("names the PAGE of each comment when the thread is not scoped to one", () => {
    mockIPC(() => undefined);
    render(
      <Harness
        flush
        t={twoPageTree([
          feedback({ id: "on-home", origin_variant_id: "var-pg-home-a" }),
          feedback({ id: "on-settings", origin_variant_id: "var-pg-set-a" }),
        ])}
      />,
    );
    expect(screen.getByTestId("proto-comment-origin-on-home").textContent).toContain("Home");
    expect(screen.getByTestId("proto-comment-origin-on-settings").textContent).toContain(
      "Settings",
    );
  });

  /** …and the ORIGIN is what a comment written from here now records. */
  it("records the variant on screen as the comment's origin", async () => {
    mockIPC((cmd) => (cmd === "proto_comment_add" ? { feedback: {} } : undefined));
    render(<Harness flush pageId="pg-home" originVariantId="var-pg-home-c" t={twoPageTree([])} />);

    fireEvent.change(screen.getByTestId("proto-comment-body"), { target: { value: "trop dense" } });
    fireEvent.click(screen.getByTestId("proto-comment-submit"));

    await waitFor(() => {
      const call = bridgeFake().calls.find((c) => c.command === "proto_comment_add");
      expect(call?.args).toMatchObject({ originVariantId: "var-pg-home-c" });
    });
  });
});
