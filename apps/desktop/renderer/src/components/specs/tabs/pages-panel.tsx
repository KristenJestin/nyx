import { useCallback, useLayoutEffect, useMemo, useState, type ReactNode } from "react";
import { CheckIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import { parseAxes, primaryAxisStates } from "../../design/axes";
import { CompositionFrame } from "../../design/composition-frame";
import { previewState } from "../../design/preview-state";
import { THUMB_PAGE_WIDTH } from "../../design/viewports";
import { useDesignRender } from "../../design/use-design-render";

/**
 * The prototype's PAGES, in the TWO shapes the elected v8 design gives them (finding
 * `01KYVMSMMBVSZNBBSBE18PRZAK`):
 *
 *   * [`PagesRail`] — the dedicated page's LEFT rail: one row per page (miniature + name + what it
 *     still owes + its open comments), foldable to a 52 px strip of miniatures. It navigates PAGES
 *     and nothing else: the variants live in ONE place only, the strip under the surface, right next
 *     to what is being judged.
 *   * [`PagesSheet`] — the Spec tab's CONTACT SHEET: a grid of the same miniatures, each one a link
 *     INTO the page. The tab says WHAT exists; it is not a workspace shrunk to a thumbnail.
 *
 * Both draw the same live miniature ([`PageThumbnail`]) of the page's effective composition — the
 * same sidecar-compiled markup in the same frozen [`CompositionFrame`], under the same CSP and the
 * same local Tailwind engine as the big surface (depot embeds an HTTP `/raw` URL per card; nyx
 * renders a named TREE, so the transport differs and the frame does not).
 *
 * The thumbnails are DECORATIVE: no chrome, no pointer events, no state control. The row / the card
 * is the only thing that takes the click.
 */

export interface PagesPanelPage {
  readonly id: string;
  readonly title: string;
  readonly slug: string;
  /** The composition shown for that page (its effective/main variant), `null` when it has none. */
  readonly compositionJson: string | null;
  /** The states declared by that variant — handed to the shim so the thumbnail renders the first. */
  readonly statesJson: string | null;
  /** Is a design RETAINED for this page in the current round? */
  readonly retained: boolean;
  /** How many variants the shown version offers — what "still to elect" is measured against. */
  readonly variantCount: number;
  /** Open pre-dev comments anchored in this page's shown composition. */
  readonly openCommentCount: number;
}

/**
 * What every miniature needs to compile: the project scope and its theme.
 *
 * It used to carry the CATALOGUE too, because the renderer judged the holes itself and had to wait
 * for it. The sidecar renders LENIENTLY now (finding `01KYT78699XG6Q6P3VZC64W8H8`) — an unbuilt
 * `[NEW]` comes back as a named placeholder, not as a refusal — so a miniature owes the catalogue
 * nothing and draws as soon as the tree is here.
 */
export interface ThumbnailContext {
  readonly projectId: string | null;
  readonly themeTokensJson: string | null;
  /**
   * The prototype the pages belong to — the page directory the miniature's own links resolve
   * against (finding `01KZ3JN69E32FYBF01NBE4FWJD`). A thumbnail navigates nothing, but a render
   * given no directory answers `unknown_page` for every link and stamps the missing marker in the
   * markup, so the miniature would draw a page dressed in broken links the big surface does not have.
   */
  readonly prototypeId: string | null;
}

/**
 * The NOMINAL page box a miniature renders at, before it is scaled down. A thumbnail is a REDUCTION
 * of the whole page, so the page must first be laid out at a plausible desktop viewport: rendering it
 * into the 40 × 32 px box itself is what produced two near-identical crops of a top-left corner, in
 * which the very thing each page is about — its empty state, its table — sat outside the frame
 * (finding `01KZ3JPW0RFM7AZ0AG2YASKBNR`).
 *
 * The width is the `desktop` PRESET, imported rather than re-typed (finding
 * `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 7): a vignette and the stage's own `1280` must show the same
 * reflow, and both are the width `design_preview` resolves `desktop` to.
 */
const THUMB_PAGE_HEIGHT = 800;

/**
 * The factor that fits a {@link THUMB_PAGE_WIDTH} × {@link THUMB_PAGE_HEIGHT} page inside the box
 * this miniature was given — measured, never assumed, because the three placements are three sizes
 * (a 40 × 32 rail row, a 40 × 32 folded stub, an 80 px-tall contact-sheet card).
 *
 * `null` until a box has been measured: the frame then fills its host at 1:1 for exactly that one
 * layout pass, which no paint ever shows (the measure runs in a layout effect, before paint). It also
 * keeps the miniature RENDERABLE under jsdom, where nothing has a box at all.
 */
function useThumbnailScale(): [(el: HTMLElement | null) => void, number | null] {
  const [el, setEl] = useState<HTMLElement | null>(null);
  const [scale, setScale] = useState<number | null>(null);

  const measure = useCallback(() => {
    if (el === null) return;
    const { clientWidth: w, clientHeight: h } = el;
    if (w === 0 || h === 0) return;
    const next = Math.min(w / THUMB_PAGE_WIDTH, h / THUMB_PAGE_HEIGHT);
    setScale((prev) => (prev === next ? prev : next));
  }, [el]);

  // After every commit — the rail folds, the panel resizes, the card appears under a spinner.
  useLayoutEffect(measure);
  useLayoutEffect(() => {
    if (el === null) return;
    // `ResizeObserver` is absent in jsdom (stubbed to a no-op there).
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(el);
    return () => observer?.disconnect();
  }, [el, measure]);

  return [setEl, scale];
}

/**
 * ONE card's live miniature — its own render, so a page with no composition costs nothing.
 *
 * # It span for ever, and it was never loading (finding `01KZ3JPW0RFM7AZ0AG2YASKBNR`, CRITICAL)
 *
 * This re-implemented, by hand, the decision [`previewState`] was written to close — and made the
 * exact mistake that function exists to make impossible: it tested `loading || markup === null` and
 * called that "loading". `markup` is `null` in three different situations and only one of them is a
 * request in flight; the other two are DEAD ENDS. When the project id cannot be resolved the render
 * hook short-circuits to `{ loading: false, markup: null, error: null }` and nothing will ever
 * arrive — so every miniature of the rail, the stub and the contact sheet showed a spinner that
 * could not resolve. Three sizes of the same eternal spinner, from a copy of a decision that already
 * existed, five lines away, in a total function.
 *
 * So the state comes from `previewState` and from nowhere else. There is no second ladder here to
 * drift out of step with the stage's.
 */
function PageThumbnail({ page, ctx }: { page: PagesPanelPage; ctx: ThumbnailContext }) {
  const axes = useMemo(() => parseAxes(page.statesJson), [page.statesJson]);
  const states = useMemo(() => primaryAxisStates(axes), [axes]);
  const {
    markup,
    loading: renderLoading,
    error: renderError,
    // The axes this miniature hands the shim are the PAGE's own — so they are also what the
    // render must leave alone. One owner per axis (finding `01KZ41YK39SGNKF6XV87A35GPN`).
  } = useDesignRender(ctx.projectId, page.compositionJson, ctx.prototypeId, page.statesJson);
  const [boxRef, scale] = useThumbnailScale();

  const preview = previewState({
    hasVariant: page.compositionJson !== null,
    projectId: ctx.projectId,
    compositionJson: page.compositionJson,
    renderLoading,
    renderError,
    markup,
  });

  // A miniature has room for a few words, never for the sidecar's sentence — the big surface says it
  // in full. What matters here is that each dead end reads as a DEAD END rather than as a wait.
  const cue = (text: string, tone: string) => (
    <div
      data-testid={`proto-thumb-${preview.kind}`}
      title={
        preview.kind === "error" || preview.kind === "unavailable" ? preview.message : undefined
      }
      className={cn("flex h-full items-center justify-center p-1 text-center text-2xs", tone)}
    >
      {text}
    </div>
  );

  if (preview.kind === "no-variant") return cue("no variant", "text-muted-foreground/70");
  if (preview.kind === "unavailable") return cue("cannot render", "text-muted-foreground/70");
  if (preview.kind === "error") return cue("render failed", "text-destructive/80");
  if (preview.kind === "empty") return cue("empty", "text-muted-foreground/70");
  if (preview.kind === "loading") {
    return (
      <div className="flex h-full items-center justify-center">
        <Spinner className="size-4 text-muted-foreground/50" />
      </div>
    );
  }

  /**
   * THE REDUCTION. The page is laid out at a desktop viewport and the whole of it is scaled into the
   * box — so the two pages of a prototype look like the two different pages they are, instead of two
   * crops of the same top-left corner. `transform` scales the RENDERED frame: the iframe still lays
   * the page out at {@link THUMB_PAGE_WIDTH}, so nothing inside it reflows into a phone layout.
   */
  return (
    <span ref={boxRef} className="relative block h-full w-full overflow-hidden">
      <span
        aria-hidden
        className="pointer-events-none absolute top-0 left-0 origin-top-left"
        style={
          scale === null
            ? { width: "100%", height: "100%" }
            : {
                width: `${THUMB_PAGE_WIDTH}px`,
                height: `${THUMB_PAGE_HEIGHT}px`,
                transform: `scale(${scale})`,
              }
        }
      >
        <CompositionFrame
          markup={preview.markup}
          themeTokensJson={ctx.themeTokensJson}
          states={states}
          axes={axes}
          // A vignette DISPLAYS one instance: no chip-bar, no `data-nyx-*` affordance outlines, and
          // no pointer events at all — the row / card owns the click.
          showChrome={false}
          className="pointer-events-none h-full"
          title={`${page.title} thumbnail`}
        />
      </span>
    </span>
  );
}

/** The one-line verdict under a page's name in the rail: what it offers, and what it still owes. */
function pageSubtitle(page: PagesPanelPage): string {
  const variants = page.variantCount === 1 ? "1 variant" : `${page.variantCount} variants`;
  return page.retained ? variants : `${variants} · to elect`;
}

/**
 * `<PagesRail>` — the dedicated page's LEFT rail: the pages, and ONLY the pages.
 *
 * It used to carry a FOOT holding the round chip, the rounds history and the catalogue link — and
 * folding a rail of PAGES then removed the only access to the ROUNDS (finding
 * `01KZ3JXBS13KFH8JS2D804XZBM`). Those two belong to the prototype and to the project, not to a page
 * list, so they moved to the header of the page that owns them; this rail lost its `footer` slot with
 * them rather than keeping an empty seam for something to drift back into.
 */
export function PagesRail({
  pages,
  activePageId,
  onSelect,
  ctx,
}: {
  pages: readonly PagesPanelPage[];
  activePageId: string | null;
  onSelect: (pageId: string) => void;
  ctx: ThumbnailContext;
}) {
  return (
    <div data-testid="proto-pages-rail" className="flex h-full flex-col">
      <ul className="flex flex-col gap-1 p-2">
        {pages.map((p) => {
          const active = p.id === activePageId;
          return (
            <li key={p.id}>
              <button
                type="button"
                data-testid={`proto-page-${p.slug}`}
                aria-current={active ? "page" : undefined}
                onClick={() => onSelect(p.id)}
                className={cn(
                  "flex w-full items-center gap-2.5 rounded-lg border px-2 py-1.5 text-left transition-colors",
                  active
                    ? "border-border bg-muted text-foreground"
                    : "border-transparent hover:bg-accent/40",
                )}
              >
                <span
                  aria-hidden
                  className={cn(
                    "block h-8 w-10 shrink-0 overflow-hidden rounded border bg-muted/30",
                    active ? "border-primary/60" : "border-border/40",
                  )}
                >
                  <PageThumbnail page={p} ctx={ctx} />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-xs text-foreground">{p.title}</span>
                  <span className="block truncate text-2xs text-muted-foreground/70">
                    {pageSubtitle(p)}
                  </span>
                </span>
                {p.retained ? (
                  <CheckIcon
                    aria-label="design retained"
                    className="size-3 shrink-0 text-success"
                  />
                ) : null}
                {p.openCommentCount > 0 ? (
                  <Badge variant="warning" className="shrink-0">
                    {p.openCommentCount}
                  </Badge>
                ) : null}
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/**
 * `<PagesRailStub>` — the SAME rail folded to 52 px: the miniatures alone, still selectable, so
 * folding the rail costs the navigation nothing. The unfold control is handed in by the page.
 *
 * `roundControl` is the OTHER thing folding must not take away (finding
 * `01KZ41YKMB63GFJ3QMK2ZHJN1C`): the round selector sits at the FOOT of the open rail, under the
 * pages, and here at the foot of the FOLDED one — the same place, at 52 px. That is what lets it
 * live in a rail of pages without dying with the fold — and `Focus` folds this rail to exactly this
 * stub, so it survives that too. Optional: a host that shows no round hands nothing and the stub is
 * what it always was.
 *
 * The foot is pinned rather than trailing the list (finding `01KZ46P0FTMJXRTT32H6G5TEEN`): `mt-auto`
 * takes the slack, so a long strip of miniatures pushes it against the bottom edge and never past it.
 */
export function PagesRailStub({
  pages,
  activePageId,
  onSelect,
  ctx,
  control,
  roundControl,
}: {
  pages: readonly PagesPanelPage[];
  activePageId: string | null;
  onSelect: (pageId: string) => void;
  ctx: ThumbnailContext;
  control: ReactNode;
  /** The round selector, at 52 px — the folded half of the rail's FOOT. */
  roundControl?: ReactNode;
}) {
  return (
    <div
      data-testid="proto-pages-rail-stub"
      className="flex h-full flex-col items-center gap-2 overflow-hidden py-2.5"
    >
      {control}
      {pages.map((p) => {
        const active = p.id === activePageId;
        return (
          <button
            key={p.id}
            type="button"
            data-testid={`proto-page-mini-${p.slug}`}
            aria-current={active ? "page" : undefined}
            aria-label={p.title}
            title={p.title}
            onClick={() => onSelect(p.id)}
            className={cn(
              "block h-8 w-10 shrink-0 overflow-hidden rounded border bg-muted/30 transition-colors",
              active ? "border-primary/60 ring-1 ring-primary/40" : "border-border/40",
            )}
          >
            <span aria-hidden className="pointer-events-none block h-full w-full">
              <PageThumbnail page={p} ctx={ctx} />
            </span>
          </button>
        );
      })}
      {roundControl != null ? (
        <div
          data-testid="proto-pages-rail-stub-foot"
          className="mt-auto flex shrink-0 flex-col items-center gap-2 pt-1"
        >
          <span aria-hidden className="h-px w-7 shrink-0 bg-border/60" />
          {roundControl}
        </div>
      ) : null}
    </div>
  );
}

/**
 * `<PagesSheet>` — the Spec TAB's contact sheet. Each card is a miniature of a page plus its slug
 * and its marks; `renderLink` wraps it in whatever navigation the host owns (a router `<Link>` into
 * the dedicated page, pointed at that page). The tab shows WHAT exists — it never pretends to be a
 * stage.
 */
export function PagesSheet({
  pages,
  ctx,
  renderLink,
}: {
  pages: readonly PagesPanelPage[];
  ctx: ThumbnailContext;
  /** Wrap one card's content in the host's navigation element (a `<Link>` to the page). */
  renderLink: (page: PagesPanelPage, children: ReactNode) => ReactNode;
}) {
  if (pages.length === 0) return null;

  return (
    <ul
      data-testid="proto-contact-sheet"
      aria-label="Prototype pages"
      className="grid grid-cols-2 gap-2.5 sm:grid-cols-3"
    >
      {pages.map((p) => (
        <li key={p.id} className="min-w-0">
          {renderLink(
            p,
            <>
              <span
                aria-hidden
                className="block h-20 w-full overflow-hidden border-b border-border/50 bg-muted/30"
              >
                <PageThumbnail page={p} ctx={ctx} />
              </span>
              <span className="flex items-center gap-1.5 px-2 py-1.5">
                <span className="min-w-0 truncate text-2xs text-foreground">{p.title}</span>
                {p.retained ? (
                  <CheckIcon
                    aria-label="design retained"
                    className="size-3 shrink-0 text-success"
                  />
                ) : null}
                {p.openCommentCount > 0 ? (
                  <Badge variant="warning" className="shrink-0">
                    {p.openCommentCount}
                  </Badge>
                ) : null}
                <code className="ml-auto min-w-0 shrink truncate font-mono text-2xs text-muted-foreground/60">
                  /{p.slug}
                </code>
              </span>
            </>,
          )}
        </li>
      ))}
    </ul>
  );
}
