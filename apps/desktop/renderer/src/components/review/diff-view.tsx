import {
  type AnnotationSide,
  type DiffLineAnnotation,
  FileDiff as PierreFileDiff,
  type SelectedLineRange,
  WorkerPoolContextProvider,
} from "@pierre/diffs/react";
import type { FileDiffMetadata } from "@pierre/diffs";
import { ChevronDownIcon, ChevronRightIcon } from "lucide-react";
import { AnimatePresence, motion, type Transition, useReducedMotion } from "motion/react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { AnchorSide, FileDiff, RepoDiff } from "@/bridge";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

import { CommentThread, NewCommentComposer } from "./comment-thread";
import type { CommentActions } from "./comment-thread";
import { diffFileBlob, fileToDiffMetadata, lineContentAt } from "./diff-patch";
import { createDiffsWorker } from "./diffs-worker";
import { groupThreadsByFile } from "./review-threads";
import type { CommentThread as Thread } from "./review-threads";
import { changeLabel, fileStat, filePath } from "./review-types";

/**
 * `DiffView` — the review's central diff, rendered with `@pierre/diffs` (`FileDiff` + the Shiki
 * worker pool, PDR-0028). The engine's `RepoDiff` hunks are serialised to a unified-diff patch per
 * file (`diff-patch`), parsed ONCE into a `FileDiffMetadata` (memoised, with a stable `cacheKey`),
 * and handed to `FileDiff`, which syntax-highlights it off the main thread (the worker pool
 * virtualizes big files internally). The library RENDERS; the data stays the sidecar's `repo_diff`
 * bridge.
 *
 * Comments live ON the diff as `@pierre/diffs` line ANNOTATIONS: an anchored thread renders inline
 * on its `(file, side, line)`, and a gutter "+" / a line-number click opens a NEW-comment composer
 * inline. Resolution/reply/promote are HUMAN actions (the agent replies via MCP) — this view only
 * renders both authors' comments and offers the human's controls.
 *
 * PERF (Finding 55): every `FileSection` is `React.memo`'d and receives ONLY referentially-stable
 * props — the anchored threads/composers are indexed ONCE per feed change into `Map`s at this level,
 * the parsed patch + its `cacheKey` are memoised per diff, and every callback is a stable, path-keyed
 * handler. So a review action (select a file, open the "+" composer, reply) re-renders ONLY the file
 * it touches — never all 77 — and the untouched files never re-tokenize. `window.__nyxFileSectionRenders`
 * exposes a render tally so the CDP gate can PROVE a single action re-renders a single file.
 */

// Finding 50 — the collapse/expand of a file section + the appearance of an inline composer animate
// (open AND close) with the SAME spring feel as the review-tab's `CommentRow`. Reduced-motion
// collapses them to instant (each call site passes `{ duration: 0 }` under `useReducedMotion`).
const HEIGHT_T: Transition = { type: "spring", duration: 0.4, bounce: 0.1 };

// Stable empty defaults so a file with no threads / no open composer keeps the SAME array identity
// across renders (Finding 55) — the `React.memo` bail-out depends on referential stability.
const EMPTY_THREADS: Thread[] = [];
const EMPTY_COMPOSERS: Composer[] = [];

// Finding 63 — the FRANC selection highlight. `@pierre/diffs` paints a drag selection by mixing the
// line background with `--diffs-selection-base` at `--mix-selection-*` (its defaults, ~82%/75%, are
// nearly invisible — that was the "sélection qu'on ne voit pas"). We drop the mix percentages so the
// selection color dominates, injected through the library's `options.unsafeCSS` (its `@layer unsafe`
// beats the base layer regardless of specificity), keeping the theme-aware color-mix in light + dark.
//
// Finding 64 — and we recolor the accent from @pierre's default BLUE to our ROSE `--primary` (matching
// the file tree, #57 `--trees-accent-override`). `--diffs-selection-base` (default `--diffs-modified-base`,
// #009fff/#69b1ff — see `@pierre/diffs/dist/style.js`) drives BOTH the drag highlight's color-mix target
// AND the selected line-number foreground, so overriding it on `:host` tints the WHOLE selection at once.
// The native gutter "+" (`[data-utility-button]`) is painted with `--diffs-modified-base` directly, so we
// recolor just that button too — leaving the blue "modified"-file semantics untouched. `--primary` inherits
// into the shadow root (custom properties pierce the boundary), so both stay theme-aware in light + dark.
export const SELECTION_CSS = `
  :host {
    --diffs-selection-base: var(--primary);
  }
  [data-selected-line] {
    --mix-selection-light: 58%;
    --mix-selection-dark: 44%;
  }
  [data-utility-button] {
    background-color: var(--primary);
  }
`;

/** The render tally the CDP gate reads to prove a review action re-renders a single `FileSection`. */
interface FileSectionRenderTally {
  total: number;
  byPath: Record<string, number>;
}

/** The anchor a NEW comment is posted on (the diff line/RANGE coordinates + its captured snippet). */
export interface LineAnchor {
  readonly path: string;
  readonly side: AnchorSide;
  readonly lineStart: number;
  /**
   * The 1-based END line of the anchor (Finding 51 — multi-line drag selection). Equals `lineStart`
   * for a single-line anchor; `> lineStart` when the human drag-selected a RANGE of lines.
   */
  readonly lineEnd: number;
  /** The blob SHA of the anchored side (helps the backend re-map across commits). */
  readonly blob: string | null;
  /** The anchored line text (the re-mapping snippet). */
  readonly snippet: string;
}

/** The @pierre side (`additions`/`deletions`) for an engine anchor side (`new`/`old`). */
const toAnnotationSide = (side: AnchorSide): AnnotationSide =>
  side === "new" ? "additions" : "deletions";
/** The engine anchor side for a @pierre annotation side. */
const toAnchorSide = (side: AnnotationSide): AnchorSide => (side === "additions" ? "new" : "old");

/** The inline annotation payload: an anchored thread, or an open new-comment composer. */
type AnnMeta =
  | { readonly kind: "thread"; readonly threadId: string }
  | {
      readonly kind: "composer";
      readonly side: AnnotationSide;
      readonly line: number;
      /** The range END (equals `line` for a single-line composer, `> line` for a drag range). */
      readonly lineEnd: number;
    };

/** An open inline composer, keyed by its file + @pierre side + start line. */
interface Composer {
  readonly path: string;
  readonly side: AnnotationSide;
  readonly line: number;
  /** The range END the anchored comment will span (Finding 51). Equals `line` when single-line. */
  readonly lineEnd: number;
}

/**
 * Finding 62 — structural-share the per-file derivations so identity survives a feed re-pull. React
 * Query's structural sharing stabilises the DATA (the `ReviewComment` objects), but `groupThreadsByFile`
 * and the composer bucketing rebuild FRESH per-file arrays on every feed change. Handing a `React.memo`'d
 * `FileSection` a new array for a file whose threads did NOT change breaks its bail-out and reflows the
 * WHOLE diff — the pre-#62 bug where resolving ONE (even unanchored/global) comment re-rendered all N
 * files. We compare each path's CONTENT signature and reuse the PREVIOUS array reference when unchanged,
 * so only the file whose threads actually changed gets a new identity → a resolve reflows ONE file, not all.
 */
interface StableBucket<T> {
  readonly arr: T[];
  readonly sig: string;
}

function stabilizeBuckets<T>(
  next: Map<string, T[]>,
  prev: Map<string, StableBucket<T>>,
  sig: (list: readonly T[]) => string,
): Map<string, StableBucket<T>> {
  const out = new Map<string, StableBucket<T>>();
  for (const [path, list] of next) {
    const s = sig(list);
    const before = prev.get(path);
    out.set(path, before && before.sig === s ? before : { arr: list, sig: s });
  }
  return out;
}

/** A per-file threads signature — changes ONLY when a rendered thread field (status/body/anchor/replies) changes. */
function threadsSignature(list: readonly Thread[]): string {
  let s = "";
  for (const t of list) {
    const r = t.root;
    s += `${r.id}:${r.status}:${r.resolved_by_type ?? ""}:${r.outdated ? 1 : 0}:${r.side ?? ""}:${r.line_start ?? ""}:${r.line_end ?? ""}:${r.body}|`;
    for (const rep of t.replies) s += `${rep.id}/${rep.status}/${rep.body},`;
    s += ";";
  }
  return s;
}

/** A per-file composers signature — changes only when an open composer's anchor coordinates change. */
function composersSignature(list: readonly Composer[]): string {
  let s = "";
  for (const c of list) s += `${c.side}:${c.line}:${c.lineEnd}|`;
  return s;
}

/** Follow the app's `.dark` class so the diff theme (pierre-dark/pierre-light) matches the UI. */
function useIsDark(): boolean {
  const [dark, setDark] = useState(
    () => typeof document !== "undefined" && document.documentElement.classList.contains("dark"),
  );
  useEffect(() => {
    if (typeof document === "undefined") return;
    const el = document.documentElement;
    const obs = new MutationObserver(() => setDark(el.classList.contains("dark")));
    obs.observe(el, { attributes: true, attributeFilter: ["class"] });
    return () => obs.disconnect();
  }, []);
  return dark;
}

interface FileSectionProps {
  file: FileDiff;
  /** The file's parsed diff (memoised + `cacheKey`-stamped upstream); `null` for a binary file. */
  patchMeta: FileDiffMetadata | null;
  threads: Thread[];
  composerLines: Composer[];
  actions: CommentActions;
  /** The selected thread id ONLY when it belongs to THIS file (else `null` — confines re-renders). */
  selectedThreadId: string | null;
  onSelectThread: (id: string) => void;
  onOpenComposer: (path: string, side: AnnotationSide, line: number, lineEnd: number) => void;
  onSubmitComposer: (path: string, side: AnnotationSide, line: number, body: string) => void;
  onCancelComposer: (path: string, side: AnnotationSide, line: number) => void;
  onToggleCollapse: (path: string) => void;
  registerRef: (path: string, el: HTMLDivElement | null) => void;
  /** Report a committed @pierre selection (Finding 65) so `DiffView` can clear / track it. */
  onLineCommitted: (path: string, range: SelectedLineRange | null) => void;
  /** Finding 65 — when `true`, this file renders `selectedLines={null}` for one commit to CLEAR
   *  @pierre's painted selection (a stuck 1-line click / a leftover plage after a composer closes). */
  clearing: boolean;
  theme: string;
  diffStyle: "unified" | "split";
  collapsed: boolean;
  active: boolean;
}

/**
 * One collapsible file section: our own sticky header + the `@pierre/diffs` `FileDiff` body.
 * `React.memo`'d (Finding 55) — with the stable props `DiffView` feeds it, it re-renders ONLY when
 * ITS OWN file's state changes (collapsed / active / its threads / its open composer), so a review
 * action does not cascade a global re-render of every file.
 */
const FileSection = memo(function FileSection({
  file,
  patchMeta,
  threads,
  composerLines,
  actions,
  selectedThreadId,
  onSelectThread,
  onOpenComposer,
  onSubmitComposer,
  onCancelComposer,
  onToggleCollapse,
  registerRef,
  onLineCommitted,
  clearing,
  theme,
  diffStyle,
  collapsed,
  active,
}: FileSectionProps) {
  const path = filePath(file);
  const stat = fileStat(file);
  const reduce = useReducedMotion();

  // Finding 55 — count each ACTUAL re-render (a memo bail-out runs no effect), keyed by path, so the
  // CDP gate can assert a review action re-renders a SINGLE file. Runs on every commit (no deps).
  useEffect(() => {
    if (typeof window === "undefined") return;
    const w = window as unknown as { __nyxFileSectionRenders?: FileSectionRenderTally };
    const tally = (w.__nyxFileSectionRenders ??= { total: 0, byPath: {} });
    tally.total += 1;
    tally.byPath[path] = (tally.byPath[path] ?? 0) + 1;
  });

  // Finding 63 — the selection is NON-CONTROLLED by default: `selectedLines={undefined}` lets
  // `@pierre/diffs` PAINT the drag itself (its `InteractionManager.updateSelection` schedules a
  // `requestAnimationFrame(renderSelection)`), so the highlight tracks the drag in REAL TIME and
  // PERSISTS after release (the controlled wiring we used before masked all of that). We switch to
  // CONTROLLED only while a RANGE composer is open — passing the frozen range FREEZES the plage while
  // the human types. A single-line composer stays non-controlled (nothing to freeze).
  //
  // Finding 65 — non-controlled means @pierre KEEPS its painted selection forever (that is the point
  // for the drag→"+" flow), which left a 1-line click and the post-composer plage stuck. To CLEAR, we
  // render `selectedLines={null}` for exactly ONE commit: the React wrapper turns that into
  // `InteractionManager.setSelection(null)` — @pierre's own clear — then `DiffView` flips `clearing`
  // back off so we return to non-controlled `undefined` and the live drag paint still works.
  const openRangeComposer = composerLines.find((c) => c.lineEnd > c.line);
  const selectedLines: SelectedLineRange | null | undefined = openRangeComposer
    ? {
        start: openRangeComposer.line,
        end: openRangeComposer.lineEnd,
        side: openRangeComposer.side,
      }
    : clearing
      ? null
      : undefined;

  // Finding 65 — a SELECTION commit (`onLineSelected` fires at pointerup): report it up so `DiffView`
  // clears a stuck single-line click (start === end) and tracks a KEPT multi-line plage (for the
  // click-outside clear), WITHOUT touching the drag→plage→"+"→composer flow (#63).
  const onLineSelected = useCallback(
    (range: SelectedLineRange | null) => onLineCommitted(path, range),
    [onLineCommitted, path],
  );

  // Finding 66 — a per-file body-height ESTIMATE for `contain-intrinsic-size`, so the browser can SKIP
  // LAYOUT of an off-screen file body (`content-visibility: auto`, applied on the body wrapper below)
  // without collapsing the scroll height. ~1 rendered row per unified diff line + one hunk separator,
  // at the 20px `--diffs-line-height`; `contain-intrinsic-size: auto` then remembers the MEASURED size.
  const estimatedBodyHeight = useMemo(() => {
    if (file.binary) return 40;
    let rows = 0;
    for (const h of file.hunks) rows += h.lines.length + 1;
    return Math.max(48, rows * 20 + 24);
  }, [file]);

  const annotations = useMemo<DiffLineAnnotation<AnnMeta>[]>(() => {
    const out: DiffLineAnnotation<AnnMeta>[] = [];
    for (const t of threads) {
      if (t.root.side == null || t.root.line_start == null) continue;
      out.push({
        side: toAnnotationSide(t.root.side),
        lineNumber: t.root.line_start,
        metadata: { kind: "thread", threadId: t.root.id },
      });
    }
    for (const c of composerLines) {
      out.push({
        side: c.side,
        lineNumber: c.line,
        metadata: { kind: "composer", side: c.side, line: c.line, lineEnd: c.lineEnd },
      });
    }
    return out;
  }, [threads, composerLines]);

  // Finding 63 — open the composer from the NATIVE gutter-utility gesture. `@pierre/diffs` calls
  // `onGutterUtilityClick(range)` when the human presses the native "+" (single line) OR presses it
  // and DRAGS to extend the plage (painted live), and also when they drag-select on the line NUMBERS
  // then press the "+" that the library auto-places on the selection. The range's `side`/`start`/`end`
  // ARE the final plage — we just open the inline composer over it. This is @pierre's own Diffhub
  // mechanism; there is NO custom pointer wiring to fight the library anymore.
  const openComposerFromRange = useCallback(
    (range: SelectedLineRange) => {
      const side: AnnotationSide = range.side ?? "additions";
      onOpenComposer(
        path,
        side,
        Math.min(range.start, range.end),
        Math.max(range.start, range.end),
      );
    },
    [onOpenComposer, path],
  );

  // Finding 55 — the `options` object is MEMOISED so its identity only changes with `diffStyle`/theme
  // (never per render): `@pierre/diffs` diffs options by identity (`areOptionsEqual`) and would else
  // `forceRender:true` (a full re-tokenize) on EVERY render.
  //
  // Finding 63 — the review uses the NATIVE gutter utility now: `enableGutterUtility` +
  // `onGutterUtilityClick` (and NO custom `renderGutterUtility`, so `resolveEnableGutterUtilityOption`
  // no longer throws — that pairing was regression 61). `enableLineSelection` + non-controlled
  // `selectedLines` give the live drag painting. `onLineNumberClick` is GONE: a plain number click used
  // to open a parasitic single-line composer; now a click just selects that line (native), and the "+"
  // opens the composer. `unsafeCSS` makes the highlight franc (see `SELECTION_CSS`).
  const options = useMemo(
    () => ({
      diffStyle,
      theme,
      disableFileHeader: true,
      enableGutterUtility: true,
      enableLineSelection: true,
      onGutterUtilityClick: openComposerFromRange,
      // Finding 65 — observe selection commits to clear stuck single-line clicks / track plages.
      onLineSelected,
      unsafeCSS: SELECTION_CSS,
    }),
    [diffStyle, theme, openComposerFromRange, onLineSelected],
  );

  const refCallback = useCallback(
    (el: HTMLDivElement | null) => registerRef(path, el),
    [registerRef, path],
  );

  const renderAnnotation = (annotation: DiffLineAnnotation<AnnMeta>) => {
    const meta = annotation.metadata;
    if (meta.kind === "composer") {
      // Finding 50a — the inline composer GROWS in (height + fade) instead of snapping into place.
      return (
        <motion.div
          initial={reduce ? false : { opacity: 0, height: 0 }}
          animate={{ opacity: 1, height: "auto" }}
          transition={reduce ? { duration: 0 } : HEIGHT_T}
          className="overflow-hidden"
        >
          <div className="max-w-2xl py-1 pr-3 pl-2" data-testid="line-composer">
            <NewCommentComposer
              rangeLabel={
                meta.lineEnd > meta.line ? `Lines ${meta.line}–${meta.lineEnd}` : undefined
              }
              onSubmit={(body) => onSubmitComposer(path, meta.side, meta.line, body)}
              onCancel={() => onCancelComposer(path, meta.side, meta.line)}
            />
          </div>
        </motion.div>
      );
    }
    const thread = threads.find((t) => t.root.id === meta.threadId);
    if (!thread) return null;
    return (
      <div className="max-w-2xl py-1 pr-3 pl-2" data-testid="anchor-marker">
        <CommentThread
          thread={thread}
          actions={actions}
          selected={thread.root.id === selectedThreadId}
          onSelect={() => onSelectThread(thread.root.id)}
        />
      </div>
    );
  };

  // Finding 52: the card must NOT be `overflow-hidden` — a `position: sticky` header inside an
  // `overflow: hidden` box sticks relative to THAT box (which does not scroll), so it never pins to
  // the top of the scrolling `diff-scroll` area (it slid away / hid behind the page header). The
  // card keeps `rounded-lg` for its shape; the ROUNDED CORNERS are preserved by rounding the header
  // top + wrapping the diff BODY (not the header) in its own `overflow-hidden rounded-b-lg` clip —
  // that clip is a SIBLING of the sticky header, never its ancestor, so it can't re-break sticky.
  return (
    <div
      ref={refCallback}
      data-testid="diff-file"
      data-path={path}
      className={cn(
        "scroll-mt-2 rounded-lg border bg-card/40",
        active ? "border-primary/50" : "border-border/70",
      )}
    >
      <button
        type="button"
        onClick={() => onToggleCollapse(path)}
        aria-expanded={!collapsed}
        data-row="file-header"
        className={cn(
          // `z-20` keeps the pinned header ABOVE the diff rows scrolling under it; it sticks to the
          // `diff-scroll` container (below the ReviewHeader), so it stays fully VISIBLE, not clipped.
          "sticky top-0 z-20 flex w-full items-center gap-2 rounded-t-lg bg-card/95 px-3 py-2 text-left backdrop-blur transition-colors hover:bg-muted/40",
          collapsed ? "rounded-b-lg" : "border-border/60 border-b",
        )}
      >
        <span aria-hidden className="text-muted-foreground">
          {collapsed ? (
            <ChevronRightIcon className="size-3.5" />
          ) : (
            <ChevronDownIcon className="size-3.5" />
          )}
        </span>
        <Badge
          variant={
            file.change === "added" ? "success" : file.change === "deleted" ? "warning" : "muted"
          }
        >
          {changeLabel(file.change)}
        </Badge>
        <code className="truncate font-mono text-foreground text-xs">{path}</code>
        <span className="ml-auto flex shrink-0 items-center gap-2 font-mono text-2xs">
          <span className="text-success">+{stat.additions}</span>
          <span className="text-destructive">-{stat.deletions}</span>
          {threads.length > 0 ? <Badge variant="info">{threads.length}</Badge> : null}
        </span>
      </button>

      {/* Finding 50b — the section body's height animates OPEN and CLOSE symmetrically (the
          `CommentRow` idiom): `AnimatePresence` keeps it mounted through the exit so the collapse is
          animated (not an instant unmount), then drops it (a collapsed file keeps no heavy FileDiff
          in the DOM). `overflow-hidden` on the animating wrapper is a SIBLING of the sticky header,
          so Finding 52's pinning is untouched. */}
      <AnimatePresence initial={false}>
        {collapsed ? null : (
          <motion.div
            key="body"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={reduce ? { duration: 0 } : HEIGHT_T}
            className="overflow-hidden rounded-b-lg"
            // Finding 66 — layout-virtualize the diff ACROSS files with the browser's NATIVE
            // content-visibility: an OFF-SCREEN file body is not laid out (its huge line DOM is
            // skipped), so the fully-expanded multi-file diff no longer re-lays-out EVERY line when the
            // diff width changes (the comments-sidebar toggle reflow that "ramed"), and it scales to
            // many more files. `contain-intrinsic-size: auto <estimate>` reserves an accurate
            // placeholder and REMEMBERS the measured height after first render, so the scroll height
            // stays stable. This lives on the body — a SIBLING of the sticky header — so Finding 52's
            // pinning (which broke under an `overflow-hidden`/containing ANCESTOR) is untouched.
            style={{
              contentVisibility: "auto",
              containIntrinsicSize: `auto ${estimatedBodyHeight}px`,
            }}
          >
            {file.binary || patchMeta == null ? (
              <p className="px-3 py-2 font-mono text-2xs text-muted-foreground italic">
                Binary file not shown
              </p>
            ) : (
              <PierreFileDiff<AnnMeta>
                fileDiff={patchMeta}
                // Finding 63 — `undefined` = NON-CONTROLLED: @pierre paints the drag selection itself
                // (live, persistent). It becomes a frozen range object ONLY while a range composer is
                // open, to hold the plage still while the human types (see `selectedLines` above). NO
                // custom `renderGutterUtility` — the NATIVE gutter "+" (`onGutterUtilityClick`) drives
                // the composer, which is what makes the multi-line selection Diffhub-native again.
                selectedLines={selectedLines}
                options={options}
                lineAnnotations={annotations}
                renderAnnotation={renderAnnotation}
              />
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
});

export interface DiffViewProps {
  /** The active repo's derived diff, or `undefined` while loading / on error. */
  readonly diff: RepoDiff | undefined;
  /** `true` while the first load is in flight (renders a skeleton note). */
  readonly loading: boolean;
  /** A load error to surface, or `null`. */
  readonly error: string | null;
  /**
   * `true` when the chosen surface still needs a rev-spec the user hasn't entered (Finding 43):
   * render a clear "fill in the fields" prompt instead of loading / an empty diff / a raw throw.
   */
  readonly awaitingInput?: boolean;
  /** A path to scroll into view (from the tree / header selection); `null` to leave scroll. */
  readonly scrollToPath: string | null;
  /** The diff surface style (unified vs split). */
  readonly diffStyle: "unified" | "split";
  /** The ANCHORED threads to place inline on the diff. */
  readonly anchoredThreads: readonly Thread[];
  /** The thread actions (reply/resolve/reopen/promote) for an inline thread card. */
  readonly commentActions: CommentActions;
  /** The thread the panel cross-selection points at (highlights its inline card). */
  readonly selectedThreadId: string | null;
  /** Cross-select a thread from the diff (highlights it in the panel too). */
  readonly onSelectThread: (id: string) => void;
  /** Post a new anchored comment on a diff line (commits the inline composer). */
  readonly onAddAnchored: (anchor: LineAnchor, body: string) => void;
}

export function DiffView({
  diff,
  loading,
  error,
  awaitingInput,
  scrollToPath,
  diffStyle,
  anchoredThreads,
  commentActions,
  selectedThreadId,
  onSelectThread,
  onAddAnchored,
}: DiffViewProps) {
  const isDark = useIsDark();
  const theme = isDark ? "pierre-dark" : "pierre-light";
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const [composers, setComposers] = useState<Composer[]>([]);
  // Finding 65 — the set of file paths whose @pierre selection is being CLEARED. A file in this set
  // renders `selectedLines={null}` for ONE commit (the null pulse → `setSelection(null)`), then the
  // reset effect drops it, returning that file to non-controlled so the live drag paint (#63) survives.
  const [clearing, setClearing] = useState<ReadonlySet<string>>(() => new Set());
  const refs = useRef(new Map<string, HTMLDivElement>());
  // The scrolling diff surface (a click OUTSIDE it clears a lingering plage, Finding 65c).
  const scrollRef = useRef<HTMLDivElement | null>(null);
  // The file that currently holds a KEPT (multi-line) selection — tracked so a click outside the diff
  // clears it. A single-line click is cleared on release, so it never lands here.
  const selectedFilePathRef = useRef<string | null>(null);

  // Refs mirror the latest props/state so the composer callbacks can stay STABLE (Finding 55) — they
  // read `.current` instead of closing over a fresh value each render.
  const composersRef = useRef(composers);
  composersRef.current = composers;
  const diffRef = useRef(diff);
  diffRef.current = diff;
  const onAddAnchoredRef = useRef(onAddAnchored);
  onAddAnchoredRef.current = onAddAnchored;
  const onSelectThreadRef = useRef(onSelectThread);
  onSelectThreadRef.current = onSelectThread;

  // Finding 55 — index the anchored threads / open composers by file ONCE per feed change (was an
  // O(files×threads) `.filter` PER file, PER render). A file with none keeps a STABLE empty array so
  // its `FileSection` never re-renders on an unrelated file's change.
  //
  // Finding 62 — and structural-share those per-file arrays across feed re-pulls (see `stabilizeBuckets`):
  // resolving a comment invalidates the COMMENTS query, so `anchoredThreads` gets a new identity even
  // when the resolved comment is GLOBAL (unanchored) and no anchored thread changed. Without this, every
  // file's `threads` array would be new → every `FileSection` re-renders → the whole diff reflows. With
  // it, only the file whose threads actually changed gets a new array; a global resolve reflows ZERO files.
  const anchorBucketsRef = useRef(new Map<string, StableBucket<Thread>>());
  const anchorsByFile = useMemo(() => {
    const stable = stabilizeBuckets(
      groupThreadsByFile(anchoredThreads) as Map<string, Thread[]>,
      anchorBucketsRef.current,
      threadsSignature,
    );
    anchorBucketsRef.current = stable;
    return stable;
  }, [anchoredThreads]);
  const composerBucketsRef = useRef(new Map<string, StableBucket<Composer>>());
  const composersByFile = useMemo(() => {
    const byFile = new Map<string, Composer[]>();
    for (const c of composers) {
      const list = byFile.get(c.path);
      if (list) list.push(c);
      else byFile.set(c.path, [c]);
    }
    const stable = stabilizeBuckets(byFile, composerBucketsRef.current, composersSignature);
    composerBucketsRef.current = stable;
    return stable;
  }, [composers]);
  // Finding 55 — parse + `cacheKey`-stamp each file's patch ONCE per diff. The stable identity lets
  // `@pierre/diffs` skip re-parsing, and the `cacheKey` lets the worker pool skip re-tokenizing.
  const patchByFile = useMemo(() => {
    const byFile = new Map<string, FileDiffMetadata>();
    if (diff)
      for (const f of diff.files) if (!f.binary) byFile.set(filePath(f), fileToDiffMetadata(f));
    return byFile;
  }, [diff]);

  // Reset transient view state when the diff identity changes (repo/target switch).
  const diffId = diff
    ? `${diff.repo_path}::${diff.base_sha ?? ""}::${diff.target_sha ?? ""}::${diff.files.length}`
    : "";
  useEffect(() => {
    setCollapsed(new Set());
    setComposers([]);
    setClearing(new Set());
    selectedFilePathRef.current = null;
  }, [diffId]);

  // Finding 65 — request a one-shot clear of `path`'s selection (the null pulse + our tracking). Stable.
  const requestClear = useCallback((path: string) => {
    if (selectedFilePathRef.current === path) selectedFilePathRef.current = null;
    setClearing((prev) => {
      if (prev.has(path)) return prev;
      const next = new Set(prev);
      next.add(path);
      return next;
    });
  }, []);

  // Drop every clearing flag on the NEXT commit: each cleared file has by now rendered `null` once (so
  // @pierre's `setSelection(null)` ran in the wrapper's layout effect), and flipping back to `undefined`
  // restores the non-controlled live-drag selection for the next gesture. Confined to the cleared file
  // (every other file keeps `clearing.has(path) === false`, so its `React.memo` bail-out holds, #55).
  useEffect(() => {
    if (clearing.size === 0) return;
    setClearing(new Set());
  }, [clearing]);

  // Finding 65 — a selection committed on `path` (pointerup): clear a single-line plain click (b),
  // track a multi-line plage for the click-outside clear (c), and forget a tracked plage when cleared.
  const onLineCommitted = useCallback(
    (path: string, range: SelectedLineRange | null) => {
      if (range == null) {
        if (selectedFilePathRef.current === path) selectedFilePathRef.current = null;
        return;
      }
      if (range.start === range.end) requestClear(path);
      else selectedFilePathRef.current = path;
    },
    [requestClear],
  );

  // Finding 65c — a pointerdown OUTSIDE the diff scroll area clears a lingering (multi-line) selection.
  // Guarded by a tracked path, so it is a no-op for the overwhelming majority of clicks.
  useEffect(() => {
    const onDocPointerDown = (e: PointerEvent) => {
      const path = selectedFilePathRef.current;
      if (!path) return;
      const el = scrollRef.current;
      if (el && e.target instanceof Node && el.contains(e.target)) return;
      requestClear(path);
    };
    document.addEventListener("pointerdown", onDocPointerDown, true);
    return () => document.removeEventListener("pointerdown", onDocPointerDown, true);
  }, [requestClear]);

  // Scroll to (and expand) the targeted file section when the selection comes from elsewhere.
  useEffect(() => {
    if (!scrollToPath) return;
    setCollapsed((prev) => {
      if (!prev.has(scrollToPath)) return prev;
      const next = new Set(prev);
      next.delete(scrollToPath);
      return next;
    });
    const el = refs.current.get(scrollToPath);
    if (el) el.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [scrollToPath]);

  // Every callback below is STABLE (Finding 55): a path-keyed handler (path passed as an argument, so
  // there is no fresh closure per file) reading refs for the latest state.
  const onToggleCollapse = useCallback((path: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  // Open a composer anchored at `line` (its start), spanning to `lineEnd` (Finding 51 — equals
  // `line` for a single-line anchor, `> line` for a drag range). Keyed by (path, side, start line).
  const openComposer = useCallback(
    (path: string, side: AnnotationSide, line: number, lineEnd: number) =>
      setComposers((prev) =>
        prev.some((c) => c.path === path && c.side === side && c.line === line)
          ? prev
          : [...prev, { path, side, line, lineEnd }],
      ),
    [],
  );
  const closeComposer = useCallback(
    (path: string, side: AnnotationSide, line: number) => {
      setComposers((prev) =>
        prev.filter((c) => !(c.path === path && c.side === side && c.line === line)),
      );
      // Finding 65a — a range composer freezes @pierre's selection (controlled); on submit OR cancel,
      // clear that leftover plage + "+" (the null pulse) instead of leaving them stuck after typing.
      requestClear(path);
    },
    [requestClear],
  );
  const submitComposer = useCallback(
    (path: string, side: AnnotationSide, line: number, body: string) => {
      const anchorSide = toAnchorSide(side);
      // Recover the composer's range END (Finding 51) so the anchor stores the REAL lineStart..lineEnd
      // (never lineEnd = lineStart when the human drag-selected a range).
      const composer = composersRef.current.find(
        (c) => c.path === path && c.side === side && c.line === line,
      );
      const lineEnd = composer?.lineEnd ?? line;
      onAddAnchoredRef.current(
        {
          path,
          side: anchorSide,
          lineStart: line,
          lineEnd,
          blob: diffFileBlob(diffRef.current, path, anchorSide),
          snippet: lineContentAt(diffRef.current, path, anchorSide, line) ?? "",
        },
        body,
      );
      closeComposer(path, side, line);
    },
    [closeComposer],
  );
  const handleSelectThread = useCallback((id: string) => onSelectThreadRef.current(id), []);
  const registerRef = useCallback((path: string, el: HTMLDivElement | null) => {
    if (el) refs.current.set(path, el);
    else refs.current.delete(path);
  }, []);

  if (error) {
    return (
      <div className="grid h-full place-items-center px-4 text-center text-destructive text-sm">
        {error}
      </div>
    );
  }
  // The surface needs a rev-spec the user hasn't typed yet (Finding 43): a clear prompt pointing at
  // the header inputs, NOT a loading spinner / "no changes" / a raw backend throw.
  if (awaitingInput) {
    return (
      <div
        data-testid="diff-awaiting-input"
        className="grid h-full place-items-center px-4 text-center text-muted-foreground text-sm"
      >
        Enter the rev-spec(s) above to load this diff.
      </div>
    );
  }
  if (loading && !diff) {
    return (
      <div className="grid h-full place-items-center text-muted-foreground text-sm">
        Loading diff…
      </div>
    );
  }
  if (!diff || diff.files.length === 0) {
    return (
      <div className="grid h-full place-items-center text-muted-foreground text-sm">
        No changes to review.
      </div>
    );
  }

  return (
    <WorkerPoolContextProvider
      poolOptions={{ workerFactory: createDiffsWorker, poolSize: 2 }}
      highlighterOptions={{}}
    >
      <div ref={scrollRef} data-testid="diff-scroll" className="h-full overflow-auto bg-background">
        {/* Finding 52 — NO top padding above the first card: a `pt-*` here offsets the first file
            header BELOW the scroll viewport's top, so at scrollTop 0 (or before a tall diff has
            finished rendering) NO `sticky top-0` header sits flush at the top and nothing is pinned.
            Keep the horizontal + bottom breathing room (`px-3 pb-3`) and the inter-card `gap-3`, but
            let the first header pin flush to the top edge. */}
        <div className="flex flex-col gap-3 px-3 pb-3" data-testid="review-diff">
          {diff.files.map((file) => {
            const path = filePath(file);
            const threads = anchorsByFile.get(path)?.arr ?? EMPTY_THREADS;
            // Finding 55 — confine the cross-select: only the file that HOLDS the selected thread sees
            // a changing `selectedThreadId`; every other file keeps a stable `null` (no re-render).
            const selForFile =
              selectedThreadId != null && threads.some((t) => t.root.id === selectedThreadId)
                ? selectedThreadId
                : null;
            return (
              <FileSection
                key={path}
                file={file}
                patchMeta={patchByFile.get(path) ?? null}
                threads={threads}
                composerLines={composersByFile.get(path)?.arr ?? EMPTY_COMPOSERS}
                actions={commentActions}
                selectedThreadId={selForFile}
                onSelectThread={handleSelectThread}
                onOpenComposer={openComposer}
                onSubmitComposer={submitComposer}
                onCancelComposer={closeComposer}
                onToggleCollapse={onToggleCollapse}
                registerRef={registerRef}
                onLineCommitted={onLineCommitted}
                clearing={clearing.has(path)}
                theme={theme}
                diffStyle={diffStyle}
                collapsed={collapsed.has(path)}
                active={scrollToPath === path}
              />
            );
          })}
        </div>
      </div>
    </WorkerPoolContextProvider>
  );
}
