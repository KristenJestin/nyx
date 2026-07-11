import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { FileTreePanel } from "./file-tree-panel";
import { fileDiff, repoDiff } from "./test-fixtures";

/**
 * FINDING 42 — the LEFT changed-files tree must show the N changed files as soon as the diff is
 * loaded, with the count and the tree coherent, and a correct empty-state at 0 files.
 *
 * `@pierre/trees` `useFileTree` builds its model ONCE at mount from the initial `paths`; the empty
 * tree the review saw came from mounting the panel while `diff` was still `undefined` (paths=[])
 * and never rebuilding. The fix keys the panel on the diff identity in `review-page.tsx` so it
 * REMOUNTS (fresh model) when the diff resolves. Here we assert the panel itself renders the count
 * + the tree region for a loaded diff, and the empty-state for no diff — the create-once remount is
 * exercised end-to-end by the CDP gate (`review-gate-tab.e2e.cjs`).
 */
describe("FileTreePanel (Finding 42 — changed-files tree)", () => {
  it("shows the N changed files (count + tree, not the empty-state) once the diff is loaded", () => {
    const diff = repoDiff("/repo", [
      fileDiff("src/a.ts", ["x"], ["y"]),
      fileDiff("src/b.ts", ["p"], ["q"]),
      fileDiff("README.md", ["1"], ["2"]),
    ]);
    render(<FileTreePanel diff={diff} selectedPath={null} onSelect={() => {}} />);

    const panel = screen.getByTestId("review-file-tree");
    // The header count reflects the 3 changed files (reactive to `paths`).
    expect(panel.textContent).toMatch(/Changed files\s*3/);
    // The empty-state copy is NOT shown when the diff has files (the `<FileTree>` branch rendered).
    expect(screen.queryByText(/No changed files\./i)).not.toBeInTheDocument();
  });

  it("shows the empty-state (and count 0) when there are no changed files", () => {
    render(<FileTreePanel diff={repoDiff("/repo", [])} selectedPath={null} onSelect={() => {}} />);
    expect(screen.getByText(/No changed files\./i)).toBeInTheDocument();
  });

  it("shows the empty-state while the diff is still undefined (loading)", () => {
    render(<FileTreePanel diff={undefined} selectedPath={null} onSelect={() => {}} />);
    expect(screen.getByText(/No changed files\./i)).toBeInTheDocument();
  });

  // FINDING 55 — the branches diff blanked the tree behind its error boundary. The gix tree diff
  // emitted the changed DIRECTORY `src` as a file entry ALONGSIDE its children, so the paths fed to
  // `@pierre/trees` were `["src", "src/anchor.rs", ...]`, where `src` is BOTH a file AND a directory
  // prefix — `useFileTree` throws "Path collides with an existing file while creating directory"
  // SYNCHRONOUSLY in `useState(() => new FileTree(...))`, which the panel's boundary caught → the
  // gate saw "Couldn't render the file tree." The engine now drops tree entries; the panel ALSO
  // sanitizes defensively so no backend path shape can crash the tree. This is the exact path set
  // that used to throw (the leading `src` dir entry + the 13 real files, plus a blank + a dupe).
  it("renders (does NOT throw) on the directory-as-file path set that used to blank the tree", () => {
    const files = [
      fileDiff("src", ["x"], ["y"]), // the phantom directory-as-file entry (the trigger)
      fileDiff("src/anchor.rs", ["a"], ["b"]),
      fileDiff("src/mod_0.rs", ["a"], ["b"]),
      fileDiff("src/mod_1.rs", ["a"], ["b"]),
      fileDiff("src/mod_10.rs", ["a"], ["b"]),
      fileDiff("", ["a"], ["b"]), // a blank path (belt-and-suspenders)
      fileDiff("src/mod_0.rs", ["a"], ["b"]), // a duplicate (belt-and-suspenders)
    ];
    render(
      <FileTreePanel diff={repoDiff("/repo", files)} selectedPath={null} onSelect={() => {}} />,
    );

    // Pre-fix this `render` THREW (uncaught) and failed the test; now it renders. Sanitized: `src`
    // (dir), the blank, and the dup are dropped → 4 real leaf files remain.
    const panel = screen.getByTestId("review-file-tree");
    expect(panel.textContent).toMatch(/Changed files\s*4/);
    expect(screen.queryByText(/No changed files\./i)).not.toBeInTheDocument();
  });
});
