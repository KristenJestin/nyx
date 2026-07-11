import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

// REGRESSION 61 — do NOT mock `@pierre/diffs` / `@pierre/diffs/react` here. The old stubs (a fake
// `FileDiff` + a fake `getSingularPatch`) meant the REAL @pierre `FileDiff` constructor never ran,
// so these tests silently passed while every file section actually THREW at render (the incompatible
// `onGutterUtilityClick` + `renderGutterUtility` combo). We render the real engine so a render throw
// fails the test. The only thing stubbed is the jsdom-incompatible Web Worker factory (the `?worker`
// import): the diff still renders on the main thread (unhighlighted), which is all the chrome asserts.
vi.mock("./diffs-worker", () => ({
  createDiffsWorker: () =>
    ({
      addEventListener() {},
      removeEventListener() {},
      postMessage() {},
      terminate() {},
    }) as unknown as Worker,
}));

import { DiffView, SELECTION_CSS } from "./diff-view";
import type { CommentThread } from "./review-threads";
import { fileDiff, repoDiff } from "./test-fixtures";

/**
 * FINDING 52 — the per-file sticky header must actually PIN to the top of the scrolling diff area
 * and stay VISIBLE (not clipped, not hidden behind the page header) while scrolling that file.
 *
 * The visual stickiness is a LAYOUT property (only observable with real layout — the CDP gate
 * `review-gate-tab.e2e.cjs` drives that). What jsdom CAN guard is the DOM/CSS INVARIANTS the fix
 * depends on, which had regressed:
 *   1. the file card is NOT `overflow-hidden` (an `overflow: hidden` ancestor scopes the sticky to
 *      the non-scrolling card, so it never pins to `diff-scroll`);
 *   2. the header carries `sticky top-0` (+ a z-index so it sits above the rows scrolling under it);
 *   3. NO `overflow-hidden` element sits BETWEEN the header and the scroll container (the body's own
 *      corner-clip wrapper is a SIBLING of the header, never its ancestor).
 */
const NO_OP_ACTIONS = { onReply() {}, onResolve() {}, onReopen() {} };

function renderDiff() {
  const diff = repoDiff("/repo", [
    fileDiff("src/a.ts", ["x"], ["y"]),
    fileDiff("src/b.ts", ["p"], ["q"]),
  ]);
  return render(
    <DiffView
      diff={diff}
      loading={false}
      error={null}
      scrollToPath={null}
      diffStyle="unified"
      anchoredThreads={[]}
      commentActions={NO_OP_ACTIONS}
      selectedThreadId={null}
      onSelectThread={() => {}}
      onAddAnchored={() => {}}
    />,
  );
}

describe("DiffView sticky file header (Finding 52)", () => {
  it("does not wrap the file card in overflow-hidden (which would break the sticky header)", () => {
    const { container } = renderDiff();
    const cards = container.querySelectorAll('[data-testid="diff-file"]');
    expect(cards.length).toBeGreaterThan(0);
    for (const card of cards) {
      expect(card.className).not.toMatch(/\boverflow-hidden\b/);
    }
  });

  it("keeps each file header sticky at the top with a stacking z-index", () => {
    const { container } = renderDiff();
    const headers = container.querySelectorAll('[data-row="file-header"]');
    expect(headers.length).toBeGreaterThan(0);
    for (const header of headers) {
      expect(header.className).toMatch(/\bsticky\b/);
      expect(header.className).toMatch(/\btop-0\b/);
      expect(header.className).toMatch(/\bz-\d+\b/);
    }
  });

  it("does not nest the sticky header inside any overflow-hidden ancestor", () => {
    const { container } = renderDiff();
    for (const header of container.querySelectorAll('[data-row="file-header"]')) {
      // The only overflow-hidden in the subtree is the body's corner-clip, a SIBLING of the header.
      expect(header.closest(".overflow-hidden")).toBeNull();
    }
  });

  // FINDING 43 — when a surface still needs a rev-spec, DiffView shows a clear prompt (not an error,
  // not "no changes"). Mirrors the hook's `awaitingInput` guard.
  it("renders a fill-in-the-fields prompt when awaitingInput (Finding 43)", () => {
    const { getByTestId, queryByTestId } = render(
      <DiffView
        diff={undefined}
        loading={false}
        error={null}
        awaitingInput
        scrollToPath={null}
        diffStyle="unified"
        anchoredThreads={[]}
        commentActions={NO_OP_ACTIONS}
        selectedThreadId={null}
        onSelectThread={() => {}}
        onAddAnchored={() => {}}
      />,
    );
    expect(getByTestId("diff-awaiting-input")).toBeInTheDocument();
    // It is NOT the empty / "no changes" state.
    expect(queryByTestId("diff-file")).not.toBeInTheDocument();
  });
});

/**
 * FINDING 62 — a NON-code mutation (resolving a comment) must NOT reflow the whole diff. The comment
 * query re-pull hands `DiffView` a NEW `anchoredThreads` array identity even when nothing anchored
 * changed; if the per-file derivations rebuild fresh arrays, every `React.memo(FileSection)` breaks and
 * all N files re-render. `DiffView` structural-shares the per-file arrays by CONTENT, so an unchanged
 * file keeps its array identity across the re-pull → its section does NOT re-render.
 *
 * We PROVE it with the same render tally the CDP gate reads (`window.__nyxFileSectionRenders.byPath`):
 * a re-render with a NEW-but-content-equal `anchoredThreads` bumps NO file's render count.
 */
function anchoredThread(id: string, path: string): CommentThread {
  return {
    root: {
      id,
      workspace_id: "w",
      repository_id: "r",
      spec_id: null,
      parent_id: null,
      author_type: "human",
      author_name: "You",
      file_path: path,
      side: "new",
      line_start: 1,
      line_end: 1,
      blob: null,
      snippet: null,
      context: null,
      outdated: false,
      body: "hi",
      status: "open",
      resolved_by_type: null,
      created_at: 0,
    },
    replies: [],
  };
}

describe("DiffView per-file identity across a feed re-pull (Finding 62)", () => {
  it("does NOT re-render any file section when a re-pull changed nothing anchored", () => {
    const diff = repoDiff("/repo", [
      fileDiff("src/a.ts", ["x"], ["y"]),
      fileDiff("src/b.ts", ["p"], ["q"]),
    ]);
    const props = (threads: CommentThread[]) => ({
      diff,
      loading: false,
      error: null,
      scrollToPath: null,
      diffStyle: "unified" as const,
      anchoredThreads: threads,
      commentActions: NO_OP_ACTIONS,
      selectedThreadId: null,
      onSelectThread: () => {},
      onAddAnchored: () => {},
    });
    const tally = window as unknown as {
      __nyxFileSectionRenders?: { total: number; byPath: Record<string, number> };
    };
    tally.__nyxFileSectionRenders = undefined;

    const { rerender } = render(<DiffView {...props([anchoredThread("c1", "src/a.ts")])} />);
    const before = { ...tally.__nyxFileSectionRenders!.byPath };

    // A comment-feed re-pull: the SAME diff, but a NEW `anchoredThreads` array whose thread is a
    // NEW object with identical content (what React Query + our derivations yield on a re-pull).
    rerender(<DiffView {...props([anchoredThread("c1", "src/a.ts")])} />);
    const after = tally.__nyxFileSectionRenders!.byPath;

    // Neither the anchored file NOR the untouched file re-rendered — their arrays kept identity.
    expect(after["src/a.ts"]).toBe(before["src/a.ts"]);
    expect(after["src/b.ts"]).toBe(before["src/b.ts"]);
  });

  it("re-renders ONLY the file whose thread actually changed (e.g. a resolve)", () => {
    const diff = repoDiff("/repo", [
      fileDiff("src/a.ts", ["x"], ["y"]),
      fileDiff("src/b.ts", ["p"], ["q"]),
    ]);
    const props = (threads: CommentThread[]) => ({
      diff,
      loading: false,
      error: null,
      scrollToPath: null,
      diffStyle: "unified" as const,
      anchoredThreads: threads,
      commentActions: NO_OP_ACTIONS,
      selectedThreadId: null,
      onSelectThread: () => {},
      onAddAnchored: () => {},
    });
    const tally = window as unknown as {
      __nyxFileSectionRenders?: { total: number; byPath: Record<string, number> };
    };
    tally.__nyxFileSectionRenders = undefined;

    const open = anchoredThread("c1", "src/a.ts");
    const { rerender } = render(<DiffView {...props([open])} />);
    const before = { ...tally.__nyxFileSectionRenders!.byPath };

    // Resolve the thread on a.ts (status flips) — a.ts's signature changes, b.ts's does not.
    const resolved: CommentThread = {
      root: { ...open.root, status: "resolved", resolved_by_type: "human" },
      replies: [],
    };
    rerender(<DiffView {...props([resolved])} />);
    const after = tally.__nyxFileSectionRenders!.byPath;

    expect(after["src/a.ts"]).toBeGreaterThan(before["src/a.ts"]); // the changed file re-rendered
    expect(after["src/b.ts"]).toBe(before["src/b.ts"]); // the untouched file did NOT
  });
});

/**
 * FINDING 64 — the drag selection highlight AND the gutter "+" must be our ROSE `--primary`, not
 * @pierre/diffs' default BLUE accent (`--diffs-modified-base`, #009fff/#69b1ff). The recolor is an
 * `options.unsafeCSS` override (its `@layer unsafe` beats @pierre's base layer). The runtime COLOUR is
 * a shadow-DOM computed style (only observable in a real browser — the CDP gate asserts the resolved
 * rgb of the "+"); what jsdom guards is that the two override rules are actually authored: the whole-
 * selection hue via `--diffs-selection-base` and the "+" via `[data-utility-button]`, both → `--primary`.
 */
describe("DiffView selection accent colour (Finding 64 — rose --primary, not @pierre blue)", () => {
  it("overrides the selection hue toward var(--primary) (drives highlight + selected line-number fg)", () => {
    expect(SELECTION_CSS).toMatch(/--diffs-selection-base:\s*var\(--primary\)/);
  });

  it("recolours the native gutter '+' ([data-utility-button]) to var(--primary)", () => {
    expect(SELECTION_CSS).toMatch(
      /\[data-utility-button\]\s*\{[^}]*background-color:\s*var\(--primary\)/s,
    );
  });
});

/**
 * FINDING 66 — the diff is layout-virtualized ACROSS files with the browser's NATIVE content-visibility:
 * each file BODY carries `content-visibility: auto`, so an OFF-SCREEN file body is not laid out (its huge
 * line DOM is skipped) — the fully-expanded multi-file diff no longer re-lays-out EVERY line on a width
 * change (the comments-sidebar toggle reflow). The layout-SKIP itself needs real layout (the CDP gate
 * proves off-screen bodies are skipped); what jsdom guards is that the CSS is applied to every file body,
 * on the body SIBLING of the sticky header (so Finding 52's pinning is untouched), with a stable
 * `contain-intrinsic-size` placeholder.
 */
describe("DiffView cross-file layout virtualization (Finding 66 — content-visibility)", () => {
  it("marks every file body content-visibility:auto with a contain-intrinsic-size estimate", () => {
    const { container } = renderDiff();
    const cards = container.querySelectorAll('[data-testid="diff-file"]');
    expect(cards.length).toBe(2);
    for (const card of cards) {
      const body = card.querySelector<HTMLElement>('[style*="content-visibility"]');
      expect(body).not.toBeNull();
      expect(body!.style.contentVisibility).toBe("auto");
      expect(body!.style.containIntrinsicSize).toMatch(/^auto \d+px$/);
      // The virtualized body is a SIBLING of the sticky header, never its ancestor (Finding 52).
      expect(body!.querySelector('[data-row="file-header"]')).toBeNull();
    }
  });
});
