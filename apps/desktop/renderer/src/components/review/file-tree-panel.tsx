import { FileTree, useFileTree } from "@pierre/trees/react";
import type * as React from "react";
import { useEffect, useMemo, useRef } from "react";

import type { RepoDiff } from "@/bridge";

import { diffGitStatus } from "./diff-patch";
import { diffPaths, sanitizeTreePaths } from "./review-types";

/**
 * Theme the `@pierre/trees` shadow DOM via its documented `-override` custom properties (they inherit
 * across the shadow boundary from the host's inline style).
 *
 * - Finding 57 — recolour the selection/focus from the library's default BLUE accent (`#009fff`) to
 *   our ROSE `--primary`: `--trees-accent-override` drives the selection bg + focus border, and we
 *   pin the focus-border + a rose-tinted selection bg explicitly for an exact match (NO `::part`).
 * - Finding 58 — tighten the per-level indent (`--trees-level-gap-override`, default
 *   `calc(8px * density)`) to `6px` so deeply-nested file names stay readable.
 */
const TREE_STYLE = {
  "--trees-accent-override": "var(--primary)",
  "--trees-selected-focused-border-color-override": "var(--primary)",
  "--trees-selected-bg-override": "color-mix(in lab, var(--primary) 15%, var(--background))",
  "--trees-level-gap-override": "6px",
} as React.CSSProperties;

/**
 * `FileTreePanel` — the LEFT panel of the review screen: the active repo's changed files, rendered
 * with `@pierre/trees` (the validated design, PDR-0028). The engine's `RepoDiff` is projected into
 * the tree's `paths` + per-file `gitStatus` glyphs (`diff-patch`); selecting a node lifts its path
 * up so the diff scrolls to that file. The tree is a DERIVED view of ONE repo tab — it never shows
 * another repo's files (multi-repo identity isolation is enforced upstream by the tab).
 *
 * The `@pierre/trees` model is created ONCE per mount from the initial paths; the parent remounts
 * this panel (a `key` on the repo/diff identity) when the diff changes, so a repo/target switch
 * yields a fresh tree rather than a stale one.
 */
export function FileTreePanel({
  diff,
  selectedPath,
  onSelect,
}: {
  readonly diff: RepoDiff | undefined;
  readonly selectedPath: string | null;
  readonly onSelect: (path: string) => void;
}) {
  // Sanitize the raw diff paths before building the tree: `@pierre/trees` throws at construction on
  // a path that is both a file and a directory prefix (Finding 55). Keep `gitStatus` consistent with
  // the surviving paths so the per-file glyphs still line up.
  const paths = useMemo(() => sanitizeTreePaths(diffPaths(diff)), [diff]);
  const gitStatus = useMemo(() => {
    const kept = new Set(paths);
    return diffGitStatus(diff).filter((entry) => kept.has(entry.path));
  }, [diff, paths]);

  // Ref to avoid a stale closure: the model's `onSelectionChange` is bound once at creation.
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;

  const { model } = useFileTree({
    paths,
    gitStatus,
    initialExpansion: "open",
    // Finding 58 — VSCode-style COMPACT FOLDERS: fold a chain of single-child directories into ONE
    // row (e.g. `migrations/0000…_comments`), so a deep tree doesn't bury the file names behind a
    // stack of half-truncated folder rows. Default is already `true`; pinned EXPLICITLY so the
    // compaction can't silently drift off (branching folders still nest normally).
    flattenEmptyDirectories: true,
    initialSelectedPaths: selectedPath ? [selectedPath] : undefined,
    onSelectionChange: (selected) => {
      const p = selected[0];
      if (p) onSelectRef.current(p);
    },
  });

  // Programmatic control: align the tree when the selection comes from ELSEWHERE (the header /
  // a comment cross-select), not from clicking the tree itself.
  useEffect(() => {
    if (!selectedPath) return;
    if (model.getSelectedPaths()[0] === selectedPath) return;
    model.getItem(selectedPath)?.select();
    model.scrollToPath(selectedPath, { offset: "center" });
  }, [selectedPath, model]);

  return (
    <div className="flex h-full flex-col" data-testid="review-file-tree">
      <div className="flex items-center justify-between px-3 pt-3 pb-2">
        <span className="font-mono text-2xs text-muted-foreground uppercase tracking-wide">
          Changed files
        </span>
        <span data-testid="file-tree-count" className="font-mono text-2xs text-muted-foreground/70">
          {paths.length}
        </span>
      </div>
      {paths.length === 0 ? (
        <p className="px-3 py-1.5 text-2xs text-muted-foreground">No changed files.</p>
      ) : (
        // Finding 57/58 — the `-override` custom props recolour the selection (rose) + tighten the
        // indent through the shadow boundary. Finding 60 (best-effort, animate folder open/close):
        // NOT done — `@pierre/trees` is a VIRTUALIZED list (fixed `itemHeight`, an overscan window),
        // so opening a folder does not GROW a container; it inserts/removes rows in the virtual window
        // and RECYCLES the row DOM nodes (their text is rewritten in place). There is no enter/exit
        // hook and no animatable height, so a CSS opacity/height transition can't fire on the recycled
        // rows — and animating the rows' transform/top would jank on every scroll. Per the finding, we
        // leave expand/collapse INSTANT rather than ship a fragile hack (documented, not faked).
        <FileTree model={model} style={TREE_STYLE} className="min-h-0 flex-1 px-1.5" />
      )}
    </div>
  );
}
