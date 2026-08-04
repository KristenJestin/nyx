import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import { PagesRail, type PagesPanelPage, type ThumbnailContext } from "./pages-panel";

/**
 * THE MINIATURES (finding `01KZ3JPW0RFM7AZ0AG2YASKBNR`, CRITICAL).
 *
 * `PageThumbnail` re-implemented, by hand, the decision `design/preview-state.ts` was written to
 * close — and made the exact mistake that function exists to prevent: it read `loading || markup ===
 * null` as "loading". `markup` is `null` in three situations and only ONE of them is a request in
 * flight. When the project id is `null` the render hook short-circuits to
 * `{ loading: false, markup: null, error: null }` and nothing will ever arrive, so every miniature —
 * rail, folded stub, contact sheet — showed a spinner that could not resolve.
 *
 * What is asserted is therefore the property, not the copy: for EVERY input, the miniature either
 * draws or states a verdict, and it spins only while something is really in flight.
 */

vi.mock("../../design/composition-frame", () => ({
  CompositionFrame: (props: { markup: string; title?: string }) => (
    <div data-testid="thumb-frame" title={props.title}>
      {props.markup}
    </div>
  ),
}));

function page(overrides: Partial<PagesPanelPage> = {}): PagesPanelPage {
  return {
    id: "pg-home",
    title: "Home",
    slug: "home",
    compositionJson: '{"key":"Root","children":[]}',
    statesJson: null,
    retained: true,
    variantCount: 1,
    openCommentCount: 0,
    ...overrides,
  };
}

function ctx(overrides: Partial<ThumbnailContext> = {}): ThumbnailContext {
  return { projectId: "p1", themeTokensJson: null, prototypeId: "proto1", ...overrides };
}

function rail(pages: PagesPanelPage[], c: ThumbnailContext) {
  return render(<PagesRail pages={pages} activePageId="pg-home" onSelect={() => {}} ctx={c} />);
}

describe("PageThumbnail — the state is previewState's, and only its", () => {
  /**
   * THE CASE THAT SPAN FOR EVER. No project id ⇒ the render hook is DISABLED and resolves instantly
   * to nothing. That is a dead end, not a wait, and it must read as one.
   */
  it("states a dead end instead of spinning when the project cannot be resolved", async () => {
    mockIPC(() => undefined);
    rail([page()], ctx({ projectId: null }));

    await waitFor(() => expect(screen.getByTestId("proto-thumb-unavailable")).toBeTruthy());
    expect(document.querySelector('[data-testid="thumb-frame"]')).toBeNull();
    // NO SPINNER anywhere in the rail — the symptom itself.
    expect(document.querySelector("[data-slot='spinner'], .animate-spin")).toBeNull();
  });

  it("says a page with no variant has none", () => {
    mockIPC(() => undefined);
    rail([page({ compositionJson: null })], ctx());
    expect(screen.getByTestId("proto-thumb-no-variant")).toBeTruthy();
  });

  it("names a failed render rather than burying it under a spinner", async () => {
    mockIPC((cmd) => {
      if (cmd === "design_render") throw new Error("design_render: bad composition");
      return undefined;
    });
    rail([page()], ctx());
    await waitFor(() => expect(screen.getByTestId("proto-thumb-error")).toBeTruthy());
  });

  it("says a composition that compiled to nothing compiled to nothing", async () => {
    mockIPC((cmd) => (cmd === "design_render" ? { markup: "  ", node_count: 0 } : undefined));
    rail([page()], ctx());
    await waitFor(() => expect(screen.getByTestId("proto-thumb-empty")).toBeTruthy());
  });

  it("draws the page when the render answers markup", async () => {
    mockIPC((cmd) =>
      cmd === "design_render" ? { markup: "<main>home</main>", node_count: 1 } : undefined,
    );
    rail([page()], ctx());
    await waitFor(() => expect(screen.getByTitle("Home thumbnail")).toBeTruthy());
  });

  /**
   * THE PROTOTYPE ID REACHES THE MINIATURE'S OWN RENDER (finding `01KZ3JN69E32FYBF01NBE4FWJD`):
   * without it the render resolves every link of the page against an empty directory and stamps the
   * missing marker into markup the vignette then draws.
   */
  it("renders the miniature against the prototype's page directory", async () => {
    const calls: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd !== "design_render") return undefined;
      calls.push(args);
      return { markup: "<main/>", node_count: 1 };
    });
    rail([page()], ctx());
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]).toMatchObject({ projectId: "p1", prototypeId: "proto1" });
  });
});
