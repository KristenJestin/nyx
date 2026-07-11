import { render } from "@testing-library/react";
import { FileDiff as PierreFileDiff } from "@pierre/diffs/react";
import { describe, expect, it, vi } from "vitest";

/**
 * NON-REGRESSION for the #55/#61 render regression.
 *
 * #55 bundled a perf refactor with Finding 56's native gutter RANGE hook (`onGutterUtilityClick`)
 * while keeping Finding 49's custom `renderGutterUtility` "+" button. @pierre/diffs 1.2.12 FORBIDS
 * pairing those two gutter-utility APIs and THROWS in the `FileDiff` constructor
 * (`resolveEnableGutterUtilityOption`), during React's ref-attach (commit) phase → the review-page
 * ErrorBoundary caught it ("Couldn't render this diff.") and NO file section ever rendered.
 *
 * The original unit test hid this: it mocked `@pierre/diffs/react`'s `FileDiff` and
 * `@pierre/diffs`' `getSingularPatch`, so the real constructor never ran. This suite renders the
 * REAL @pierre engine (no mock of `FileDiff` / `getSingularPatch`) so a render throw fails the test.
 */

// Mock ONLY the jsdom-incompatible worker factory (the `?worker` import cannot be built by vitest,
// and jsdom has no Web Worker). A benign no-op worker keeps the pool happy — the diff renders on the
// main thread (unhighlighted). This is NOT a mock of the diff engine: `getSingularPatch` / `FileDiff`
// run for real, so a render throw fails the test.
vi.mock("./diffs-worker", () => ({
  createDiffsWorker: () =>
    ({
      addEventListener() {},
      removeEventListener() {},
      postMessage() {},
      terminate() {},
    }) as unknown as Worker,
}));

import { DiffView } from "./diff-view";
import { fileToDiffMetadata } from "./diff-patch";
import { fileDiff, repoDiff } from "./test-fixtures";
import type { CommentActions } from "./comment-thread";
import type { FileDiff } from "@/bridge";

const NO_OP_ACTIONS: CommentActions = { onReply() {}, onResolve() {}, onReopen() {} };

// A realistic multi-line modified file (a slice of the demo seed's src/anchor.rs).
const realFile: FileDiff = {
  change: "modified",
  old: { path: "src/anchor.rs", blob: "3b65262", binary: false },
  new: { path: "src/anchor.rs", blob: "7e53158", binary: false },
  binary: false,
  hunks: [
    {
      old_start: 1,
      old_lines: 3,
      new_start: 1,
      new_lines: 8,
      lines: [
        { kind: "context", content: "fn anchor_demo() {" },
        { kind: "removed", content: "    old_stub();" },
        { kind: "added", content: "    setup();" },
        { kind: "added", content: "    validate(items);" },
        { kind: "added", content: "    normalize(items);" },
        { kind: "added", content: "    prepare(items);" },
        { kind: "added", content: "    let result = compute_total(items);" },
        { kind: "added", content: "    finish();" },
        { kind: "context", content: "}" },
      ],
    },
  ],
};

describe("DiffView render (non-regression #55/#61 — real @pierre, no engine mock)", () => {
  it("renders fileToDiffMetadata through <PierreFileDiff> without throwing", () => {
    const meta = fileToDiffMetadata(realFile);
    const { container } = render(
      <PierreFileDiff
        fileDiff={meta}
        options={{ diffStyle: "unified", theme: "pierre-dark", disableFileHeader: true }}
      />,
    );
    expect(container.querySelector("diffs-container")).not.toBeNull();
  });

  it("renders EVERY file section of the full DiffView (the diff no longer fails to render)", () => {
    const diff = repoDiff("/repo", [realFile, fileDiff("src/b.ts", ["p"], ["q"])]);
    const { container } = render(
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
    // Both file sections AND their @pierre diff bodies render (the regression rendered NEITHER).
    expect(container.querySelectorAll('[data-testid="diff-file"]').length).toBe(2);
    expect(container.querySelectorAll("diffs-container").length).toBe(2);
  });

  // Finding 63 — the review returns to the NATIVE @pierre selection: NON-CONTROLLED `selectedLines`
  // (undefined) + `enableLineSelection` + `enableGutterUtility` + `onGutterUtilityClick`, and NO
  // custom `renderGutterUtility`. That last point is load-bearing: pairing `onGutterUtilityClick`
  // with a custom `renderGutterUtility` makes @pierre 1.2.12 THROW in the real FileDiff constructor
  // (`resolveEnableGutterUtilityOption`, regression 61). This renders the REAL engine with the native
  // gutter config, so if that pairing were reintroduced the constructor throw would FAIL this test —
  // the diff `<pre>` (in the open shadow root) would never mount.
  it("constructs the REAL @pierre FileDiff with the native gutter config without throwing (Finding 63 / reg 61)", () => {
    const diff = repoDiff("/repo", [realFile]);
    const { container } = render(
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
    const host = container.querySelector("diffs-container");
    expect(host).not.toBeNull();
    // The diff body `<pre>` mounted in the (open) shadow root — the constructor ran to completion
    // (the native gutter-utility APIs did not collide).
    expect(host?.shadowRoot?.querySelector("pre")).not.toBeNull();
  });
});
