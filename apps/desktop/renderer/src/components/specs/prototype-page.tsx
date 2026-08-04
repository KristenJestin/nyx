import { getRouteApi, Link } from "@tanstack/react-router";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  HistoryIcon,
  LayoutTemplateIcon,
  MaximizeIcon,
  MessageSquareIcon,
  MinimizeIcon,
  PuzzleIcon,
  RouteIcon,
  UndoIcon,
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Breadcrumb, BreadcrumbCurrent, breadcrumbLinkClass } from "@/components/ui/breadcrumb";
import { Button } from "@/components/ui/button";
import { Segmented } from "@/components/ui/segmented";
import { SidebarShell } from "@/components/ui/sidebar-shell";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import { parseAxes, primaryAxisStates } from "../design/axes";
import { compositionContractReport } from "../design/composition-contract";
import { CompositionFrame } from "../design/composition-frame";
import { NodeContractNotice } from "../design/node-contract-notice";
import { previewState } from "../design/preview-state";
import { VIEWPORT_PRESETS } from "../design/viewports";
import { useComponents } from "../design/use-components";
import { useDesign } from "../design/use-design";
import {
  navigationStatusLabel,
  unrenderedReasonLabel,
  useDesignRender,
} from "../design/use-design-render";

import { specKey } from "./spec-meta";
import { markCompositionNodes, rootNodeKey } from "./composition-markers";
import { defaultVariant, displayVersion, panelPagesOf, treeAtRound } from "./proto-page-model";
import type { ProtoTreePage } from "./proto-types";
import { type OpenNode, CompositionTree } from "./tabs/composition-tree";
import { NodeComments, scopeComments } from "./tabs/node-comments";
import { NodeParamsPanel, type RequestedUsage } from "./tabs/node-params";
import { PagesRail, PagesRailStub } from "./tabs/pages-panel";
import { PinPopup, type PinTarget } from "./tabs/pin-popup";
import { RoundHistory } from "./tabs/round-history";
import { VariantStrip } from "./tabs/variant-strip";
import { useProtoActions } from "./use-proto-actions";
import { useProtoTree } from "./use-proto-tree";
import { useRoundManifest } from "./use-round-manifest";
import { useSpecDetail } from "./use-spec-detail";

/**
 * `<PrototypePage>` — the Spec's prototype WORKBENCH, on a route of its own
 * (`/work/$specId/prototype`), in strict parity with the code-review page (PDR-0028).
 *
 * # Why it left the tab (finding `01KYVMSMMBVSZNBBSBE18PRZAK`, CRITICAL, elected v8 `full-page`)
 *
 * A prototype stands for a REAL WEB PAGE. Measured in the shipped app, the tab gave it an iframe of
 * **486 × 489** — a vignette, inside a Spec detail already spending its width on a spec rail and its
 * height on six stacked cards. Multipage made it worse: the pages rail ate the width exactly when
 * the page needed it most. No amount of re-arranging inside a tab fixes that, because the tab is not
 * the size of the thing it shows. The user elected the answer the review already uses: a DEDICATED
 * PAGE, and the tab demoted to an ENTRY CARD ({@link PrototypeTab}).
 *
 * # The screen
 *
 * Same chassis as the review: `flex h-svh` between two [`SidebarShell`] flanks.
 *
 *  - LEFT — the PAGES rail ([`PagesRail`]) with the ROUND selector pinned at its FOOT, the whole
 *    thing foldable to a 52 px strip ([`PagesRailStub`]) that still carries both. It navigates PAGES
 *    and nothing else: the variants live in ONE place, the strip under the surface, beside what is
 *    being judged.
 *  - CENTER — the header (breadcrumb `Work / <specKey> / Prototype`; the page identity, the VIEWPORT
 *    selector + the size readout, Focus) over the STAGE: the surface, the variant strip UNDER it
 *    with the decision in it, and the page's links.
 *  - RIGHT — Comments · Params, as SEGMENTS of one panel. Each of those used to be a card STACKED
 *    under the surface, and each stacked card stole height from the scene — which IS the grievance.
 *    In a panel they cost the scene nothing.
 *
 * # WHERE THE ROUND AND THE CATALOGUE LIVE — and why the fold is not the argument it looked like
 *
 * They both sat in the FOOT of the left rail, inside its scrolling body. So the chevron that folds a
 * rail of PAGES carried away the only entry point to the ROUNDS and to the catalogue, and `Focus`,
 * which folds that rail by definition, did the same at the exact moment the user asked for the most
 * room (finding `01KZ3JXBS13KFH8JS2D804XZBM`). They were moved to the header, which no fold removes.
 *
 * That fixed the fold and created a duplication: a project-wide `Catalogue` link in the chrome of
 * ONE page, and a round chip a long way from the pages it decides between (finding
 * `01KZ41YKMB63GFJ3QMK2ZHJN1C`). Both are settled now, and neither costs the fold guarantee:
 *
 *  - the CATALOGUE is at the FOOT OF THE APP RAIL ([`BaseLayout`]'s `FOOT_NAV`) — the navigation of
 *    the app, present on every screen, in Focus like everywhere else. It is a reference surface, not
 *    a property of this page;
 *  - the ROUND is back at the FOOT of the pages rail, under the list, where the user looks for it
 *    (finding `01KZ46P0FTMJXRTT32H6G5TEEN`) — but in the shell's `footer` SLOT, which is pinned and
 *    never scrolled, and MIRRORED in the 52 px stub. The rail folds to a stub, not to nothing, and
 *    `Focus` folds it to that same stub: rail open, rail folded, Focus — reachable in all three.
 *    The SCROLLING foot was the defect; the foot never was.
 *
 * # WHAT IS ALREADY ON SCREEN IS NOT SAID AGAIN (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`)
 *
 * Three sentences were deleted rather than reworded, and each for the same reason:
 *
 *  - the reading note centred over every screen (`You are looking at the page at a chosen width…`),
 *    Focus included — where one has just asked for nothing around the scene. It was already absent
 *    while consulting a round, which is the proof it was optional;
 *  - the GUTTER caption, which explained that the grey margin is grey — and contradicted the size
 *    readout while doing it (`the page is 390 px wide` beside a readout saying 388);
 *  - the tab's two sentences describing what its own `Open prototype` button makes plain
 *    ({@link PrototypeTab}).
 *
 * The page TITLE and its SLUG are stated ONCE — in the header identity line. The params form used to
 * repeat both (`as composed in <page> / <variant>`) beside a tree that badges the variant and a strip
 * that selects it. The comment count is written once per surface it describes.
 *
 * # THE RENDER IS THE COMPOSITION (finding `01KYW32N1BVDVTGS0WPMG2PJW2`)
 *
 * There were THREE disconnected panes — Comments, Params, Composition — and each one asked the
 * reviewer to name, in words, an element they were looking at: a `node_key` picked from a dropdown to
 * anchor a comment, a chip picked from a row to open a form, a tree listing every page of the round
 * whatever page was on screen. The subject of this screen is the RENDERING; so the rendering is the
 * surface one designates ON:
 *
 *  - a plain CLICK in the page opens that node's params ({@link NodeParamsPanel}), and the form
 *    carries the link to the component's own detail page. The page's declared affordances keep
 *    priority — a `data-nyx-page` link still navigates, a `data-nyx-state` element still flips.
 *  - PIN MODE (armed from the Comments pane) arms the next click to open an anchored composer where
 *    it lands ({@link PinPopup}), and stores the comment on the `node_key` that element belongs to.
 *  - the COMPOSITION pane is gone as a third tab: the elected (frozen) named tree now sits in the
 *    Params pane, SCOPED TO THE PAGE ON SCREEN. It used to show the first page of the round whatever
 *    page was selected, and clicking a node in it dragged the stage back to that page.
 *
 * The mechanism under it all is depot's own (`docs/web/prototypes-workspace.md`): a shim inside the
 * frame intercepts the click and posts the target to the parent, which opens a popup anchored at the
 * click point. What is NOT depot's is the target: depot computes a CSS SELECTOR, nyx resolves a NODE
 * of the named tree — the tree IS the contract here (`prototype_feedback.node_key`, re-anchored
 * across rounds, read by the coder seam), and a selector would be a second identity for it that dies
 * the first time a class is renamed. The node names reach the markup as inert comment markers the
 * renderer asks the sidecar to interleave ({@link markCompositionNodes}).
 *
 * # CONSULTING A PAST ROUND — IN PLACE (finding `01KZ3AP34K1PKS6W0E75RP7DF6`)
 *
 * The rounds panel listed `v7 … v1`, padlocks included, and none of it could be opened. That was a
 * confusion between two different rules: NO MUTATION (the UI must not open a round — still true,
 * still enforced) and NO NAVIGATION (never asked for, and the actual grievance).
 *
 * The consultation happens HERE, on this very screen, and it adds NO panel. `proto_tree` already
 * carries every version and variant a page ever had; the only thing tied to the current round is
 * the ELECTION it resolves. So consulting is a SUBSTITUTION, not an addition
 * ([`treeAtRound`] + [`useRoundManifest`]): the tree is re-sourced from the consulted round's
 * manifest, and the rail, the stage and the variant strip show that round WITHOUT ONE BRANCH OF
 * THEIR OWN. A separate surface would have meant a second rail, a second stage, a second strip —
 * i.e. the second set of panels the previous finding was about.
 *
 * What CHANGES on screen is what proves the mode, and each change is a withdrawal:
 *
 *  - the header turns amber, states `Consulting <round>` WITH THAT ROUND'S SUMMARY — the agent's own
 *    sentence, stored since T26 and displayed nowhere until finding `01KZ3JXCNK902DQXBD6QSYEE78` —
 *    and carries `Back to current`: one click, always at the same spot;
 *  - the right panel keeps ONE pane, the THREAD, in `readOnly` (finding `01KZ3JXB3SN8BMK7FS16PPSZ6T`).
 *    It used to be withdrawn ENTIRELY, on a good argument — both panes AUTHOR, and one does not
 *    author into the past — but the argument only ever applied to the WRITING, and it took the
 *    reading with it: re-reading the review thread while looking at the round it produced is what one
 *    opens a past round for. The guarantee is unchanged, and still structural: `readOnly` renders no
 *    composer, no pin toggle, no `Resolve`, and PARAMS — a form with nothing to read — is not there
 *    at all. Not one element bound to a write is on screen;
 *  - the variant strip keeps the variants and their `MAIN` / `RETAINED` badges (reading what was
 *    retained IS the point) and drops the Decision zone;
 *  - the render stops taking picks (`pickMode="off"`) and pin mode is disarmed.
 *
 * The ONLY new command behind all this is a pure read (`proto_round_manifest`). Nothing here opens
 * a round, rolls back, or re-elects.
 *
 * # Focus
 *
 * There is no separate full-screen mode: the page IS the full screen. `Focus` folds BOTH flanks to
 * their stubs. Measured in the running app on a 1680 px window: **980 → 1520 px** of stage.
 *
 * # The viewport selector
 *
 * 390 / 768 / 1280 / Fit, with the RENDERED size MEASURED back beside it ({@link useRenderedSize}):
 * the box the frame actually gets, not the number that was asked for. A false reading is worse than
 * no reading, because it is the one thing that says at what width the reviewer is validating — so it
 * is measured, and re-measured on all three things that move it (finding
 * `01KYVYNQGYP6THPCBYSYSWJ5B4`). Validating a page at a single width proves nothing about it; this
 * is what makes the prototype testable. A preset narrower than the stage centres the page; one wider
 * scrolls the stage.
 *
 * AND THE PRESET IS EXACT (finding `01KZ3JPWNYP0B65Z31RRX1EEMT`). `390` used to lay the page out at
 * 388, `768` at 766, `1280` at 1278: the two gutter borders ate the preset from the inside, because
 * the surface sized `border-box`. A media query at 390 px does not fire at 388, so the selector was
 * failing at the one job it exists for. The earlier correction moved the READING onto the content box
 * — honest about a frame that was wrong, which is still wrong by two pixels. The SURFACE is
 * `content-box` now, so the frame receives the number that was asked for, and the readout keeps
 * MEASURING that box rather than echoing the button.
 *
 * There is NO image export: nothing in the product captures a rendering (the sidecar has no
 * rasterizer), so no button promises one.
 */

const routeApi = getRouteApi("/work/$specId/prototype");

/**
 * The viewport presets — `fit` renders at the stage's own width.
 *
 * The three WIDTHS are not this file's to choose: they are the ones `design_preview` serves the
 * agent ({@link VIEWPORT_PRESETS}, arrimé to `nyx_core::render::viewport::NAMED_VIEWPORTS` through
 * `contract/design-fixtures.json`). They used to be a list of bare numbers here, joined to that one
 * by nothing — so moving `tablet` to 834 would have left the agent reasoning at 834 while the human
 * validated at 768 (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 7).
 */
type ViewportKey = string;

const VIEWPORT_OPTIONS: readonly { v: ViewportKey; label: string }[] = [
  ...VIEWPORT_PRESETS.map((p) => ({ v: String(p.width), label: String(p.width) })),
  { v: "fit", label: "Fit" },
];

/** Which pane of the right panel is on screen. */
type PanelPane = "comments" | "params";

/** The rail's folded width (a strip of miniatures) and the right panel's folded width (a stub). */
const RAIL_OPEN = 288;
const RAIL_FOLDED = 52;
const PANEL_OPEN = 352;
const PANEL_FOLDED = 48;

/**
 * The element's RENDERED box, live. This is the readout's only source: the header must state the
 * size the page is actually being judged AT, not the size that was asked for — a preset wider than
 * the stage, a scrollbar, a folded flank all make those two differ.
 *
 * # It measures the CONTENT box, which is the box the page lays out in
 *
 * `clientWidth`/`clientHeight`, not `getBoundingClientRect()`. The content box is where the frame —
 * and therefore the page — is really laid out; the border box is the surface plus its gutter marks,
 * which the page never gets. Measuring the content box also keeps the readout a MEASURE rather than
 * an echo of the button just pressed: at `Fit` no preset was pressed at all, and the number can only
 * come from the box (947 px with both flanks open, 1486 in Focus — measured in the running app).
 *
 * It used to read 388 for the `390` preset, and that was true of the frame: the surface sized
 * `border-box`, so the two gutter borders came out of the number the preset had written. The BOX was
 * the defect, not the reading (finding `01KZ3JPWNYP0B65Z31RRX1EEMT`) — the surface is `content-box`
 * now, and the measure answers 390 for 390 because the page really is 390 wide.
 *
 * # It follows the three things that move that box, and assumes none of them
 *
 *  - A WIDTH pick and a FOCUS toggle are STATE changes, so the measure re-runs after EVERY commit,
 *    before paint. A `ResizeObserver` alone would not do: its callbacks run as part of the
 *    RENDERING LIFECYCLE, so a readout wired only to one goes stale the moment that lifecycle is
 *    throttled while layout stays live — exactly the shape of finding `01KYVYNQGYP6THPCBYSYSWJ5B4`,
 *    where the frame resized under the reviewer and the reading did not.
 *  - A WINDOW resize announces no commit, so it is covered twice over: the `ResizeObserver` on the
 *    element, and the window's own `resize` event, which is a plain task and needs no lifecycle.
 *
 * It hands back a CALLBACK REF, not a `RefObject`, and that is the whole point: the measured element
 * mounts LATER than this hook (the page renders a spinner, then an empty state, then the stage), and
 * an effect keyed on a `RefObject` never re-runs — it reads `null` once and the readout stays
 * `0 × 0` for ever. Measured in the running app before the fix. Keying on the ELEMENT re-measures
 * the moment it appears.
 */
function useRenderedSize(): [(el: HTMLElement | null) => void, { w: number; h: number }] {
  const [el, setEl] = useState<HTMLElement | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });

  const measure = useCallback(() => {
    if (el == null) return;
    const next = { w: el.clientWidth, h: el.clientHeight };
    setSize((prev) => (prev.w === next.w && prev.h === next.h ? prev : next));
  }, [el]);

  // AFTER EVERY COMMIT — a width pick, a Focus toggle, the frame appearing under a spinner. No dep
  // array on purpose: the causes are not enumerable here, the layout is, so it is re-read instead
  // of predicted. It settles in one extra pass (an unchanged box hands back the SAME state object).
  useLayoutEffect(measure);

  // WHAT NO COMMIT ANNOUNCES — the window being resized.
  useLayoutEffect(() => {
    if (el == null) return;
    window.addEventListener("resize", measure);
    // `ResizeObserver` is absent in jsdom (stubbed to a no-op there), which is what makes the unit
    // tests prove the readout keeps up WITHOUT one.
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(el);
    return () => {
      window.removeEventListener("resize", measure);
      observer?.disconnect();
    };
  }, [el, measure]);

  return [setEl, size];
}

export function PrototypePage() {
  const { specId } = routeApi.useParams();
  const { page: requestedSlug } = routeApi.useSearch();
  const { detail, loading: detailLoading } = useSpecDetail(specId);

  /**
   * THE PROJECT the design system is scoped to — the SPEC's own (`spec.project_id`), never the
   * optional dev WORKSPACE's: a prototype has no reason to wait for a workspace, and the catalogue,
   * the tokens and the renderer are all project-scoped (finding `01KYT43KG9BR2J338MS6MVQXZW`).
   */
  const projectId = detail?.spec.project_id ?? null;
  const { tree: liveTree, loading, error, refresh: refreshTree } = useProtoTree(specId);
  const { design } = useDesign(projectId);
  const themeTokensJson = design?.theme_tokens_json ?? null;
  const actions = useProtoActions();
  const { components, loading: componentsLoading } = useComponents(projectId);

  const [activePageId, setActivePageId] = useState<string | null>(null);
  const [selectedVariantId, setSelectedVariantId] = useState<string | null>(null);
  const [railOpen, setRailOpen] = useState(true);
  const [panelOpen, setPanelOpen] = useState(true);
  const [pane, setPane] = useState<PanelPane>("comments");
  const [viewport, setViewport] = useState<ViewportKey>("fit");
  const [requestedUsage, setRequestedUsage] = useState<RequestedUsage | null>(null);
  /** The node the Params pane is bound to — RINGED in the render, so the selection reads there. */
  const [selectedNodeKey, setSelectedNodeKey] = useState<string | null>(null);
  /**
   * The node a COMMENT CARD is pointing at right now (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`) — the
   * thread hands it over on hover and takes it back on leave. It BORROWS the ring rather than adding
   * a second mark: one ring, one meaning — "the element this screen is talking about" — whichever
   * pane is doing the talking. `null` restores whatever the Params pane had selected.
   */
  const [hoveredNodeKey, setHoveredNodeKey] = useState<string | null>(null);
  /** Is the render armed to place a comment? Owned here, because the frame is. */
  const [pinMode, setPinMode] = useState(false);
  /** The pending anchored composer: where the click landed, and on which node. */
  const [pin, setPin] = useState<PinTarget | null>(null);
  /** The PAST round being consulted — `null` = the live current round (finding `01KZ3AP34K1PKS6W0E75RP7DF6`). */
  const [consultedRoundId, setConsultedRoundId] = useState<string | null>(null);

  /**
   * THE CONSULTED ROUND'S MANIFEST, and the tree seen through it. The swap is ATOMIC: until the
   * manifest lands, `consulting` is false and the live workbench stays on screen untouched — a
   * header that announced a round the stage was not yet showing would be the one lie this mode
   * cannot afford. A round the tree does not list, or a read that failed, never enters the mode.
   */
  const { manifest, error: manifestError } = useRoundManifest(consultedRoundId);
  const consultedRound =
    liveTree?.rounds.find(
      (r) => r.id === consultedRoundId && r.id !== liveTree.current_round?.id,
    ) ?? null;
  const consulting = consultedRound !== null && manifest !== null && !manifestError;
  const tree = useMemo(
    () =>
      consulting && liveTree !== null && manifest !== null
        ? treeAtRound(liveTree, manifest)
        : liveTree,
    [consulting, liveTree, manifest],
  );

  /** Enter / leave consultation — and disarm what only makes sense on the live round. */
  const consultRound = useCallback((roundId: string | null) => {
    setConsultedRoundId(roundId);
    setRequestedUsage(null);
    setSelectedNodeKey(null);
    setHoveredNodeKey(null);
    setPinMode(false);
    setPin(null);
  }, []);

  const pages: readonly ProtoTreePage[] = tree?.pages ?? [];
  /**
   * The page ON SCREEN. The `?page=<slug>` deep-link (the tab's contact sheet points each card at
   * its own page) only ever seeds it — an explicit rail pick wins from then on, so the link does not
   * fight the user for the selection on every re-pull.
   */
  const deepLinked =
    requestedSlug != null ? pages.find((p) => p.page.slug === requestedSlug) : null;
  const activePage =
    pages.find((p) => p.page.id === activePageId) ?? deepLinked ?? pages[0] ?? null;
  const activeVersion = activePage ? displayVersion(activePage) : null;
  const variants = activeVersion?.variants ?? [];
  const variant =
    variants.find((v) => v.id === selectedVariantId) ??
    (activePage ? defaultVariant(activePage) : null);
  const composition = variant?.composition_json ?? null;

  /**
   * WHAT IS SENT TO THE RENDERER: the same composition, with an inert comment marker bracketing each
   * node so the frame can attribute a clicked element back to its `node_key`
   * ({@link markCompositionNodes}). The STORED composition is untouched — `NodeParamsPanel` and the
   * comments read `variant.composition_json` as they always did; only the render request carries the
   * markers, and a marker renders to an HTML comment, which the page can neither see nor lay out.
   */
  const renderComposition = useMemo(() => markCompositionNodes(composition), [composition]);
  const rootKey = useMemo(() => rootNodeKey(composition), [composition]);

  const variantId = variant?.id ?? null;
  const axes = useMemo(() => parseAxes(variant?.states_json), [variant?.states_json]);
  const states = useMemo(() => primaryAxisStates(axes), [axes]);

  /**
   * THE RENDER, AND WHAT IT COULD NOT DRAW. The sidecar renders LENIENTLY (finding
   * `01KYT78699XG6Q6P3VZC64W8H8`): the composition goes over as it is, the page comes back drawn,
   * and every node that could not be rendered comes back as a NAMED placeholder in place plus one
   * `unrendered[]` entry. The renderer computes no notion of a hole of its own — it used to, in a
   * `composition-holes.ts` that knew two of the five reasons and had to wait for the catalogue
   * before it could hand the sidecar anything.
   */
  const {
    markup,
    unrendered,
    cssSlots,
    navigation,
    navigationBrokenCount,
    loading: renderLoading,
    error: renderError,
    // The VARIANT's axes — what the frame hands the shim below, and therefore the only axes the
    // render leaves alone. Every axis a NODE declares and the variant does not is baked into that
    // node's markup, which is what makes the params form's axis controls act at all (finding
    // `01KZ41YK39SGNKF6XV87A35GPN`).
  } = useDesignRender(
    projectId,
    renderComposition,
    tree?.prototype.id ?? null,
    variant?.states_json ?? null,
  );

  const preview = previewState({
    hasVariant: variant !== null,
    projectId,
    compositionJson: renderComposition,
    renderLoading,
    renderError,
    markup,
  });

  /** What the whole composition owes the components it references — read-side, in the params pane. */
  const contractReport = useMemo(
    () => compositionContractReport(composition, components),
    [composition, components],
  );

  const onNavigate = useCallback(
    (slug: string) => {
      const target = tree?.pages.find((p) => p.page.slug === slug);
      if (target) setActivePageId(target.page.id);
    },
    [tree],
  );

  /**
   * A node clicked in the elected composition → put that PLACEMENT on screen: its page, its variant,
   * and the params pane bound to it. A request, not a value (a new object per click), so the same
   * node clicked twice re-binds instead of being swallowed as "unchanged".
   */
  const onOpenNode = useCallback<OpenNode>(({ pageId, variantId, nodeKey }) => {
    setActivePageId(pageId);
    setSelectedVariantId(variantId);
    setRequestedUsage(variantId === null ? null : { variantId, nodeKey });
    setPane("params");
    setPanelOpen(true);
  }, []);

  /**
   * A NODE DESIGNATED IN THE RENDER — the gesture this screen is built around. It is the same
   * request the named tree makes, so it lands in the same place and gets the same honesty: a node
   * the panel cannot edit (an `[inline]` one, a `[NEW]` the catalogue does not carry) is NAMED there
   * rather than silently selecting nothing.
   */
  const onNodeSelect = useCallback(
    (nodeKey: string) => {
      if (variantId === null) return;
      setRequestedUsage({ variantId, nodeKey });
      setPane("params");
      setPanelOpen(true);
    },
    [variantId],
  );

  /** A PIN placed in the render → the anchored composer opens where the click landed. */
  const onNodePin = useCallback((target: PinTarget) => setPin(target), []);

  // The stage's own box is no longer READ — nothing on screen speaks of it since the gutter caption
  // went (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`). The ref stays: it is what makes the stage the
  // scrolling box the surface is centred in.
  const [stageRef] = useRenderedSize();
  const [surfaceRef, rendered] = useRenderedSize();
  /**
   * The SURFACE element itself. The pin coordinates arrive in the FRAME's own coordinates, and the
   * frame fills this box — so this is the box the anchored composer is positioned and clamped in.
   * `surfaceRef` is a `useState` setter (stable), so composing the two refs costs no re-render.
   */
  const [surfaceEl, setSurfaceEl] = useState<HTMLElement | null>(null);
  const bindSurface = useCallback(
    (el: HTMLDivElement | null) => {
      surfaceRef(el);
      setSurfaceEl(el);
    },
    [surfaceRef],
  );

  // FOCUS = both flanks folded. One button, one meaning: the scene takes the width back.
  const focused = !railOpen && !panelOpen;
  const toggleFocus = () => {
    const next = !focused;
    setRailOpen(!next);
    setPanelOpen(!next);
  };

  // A page switch drops a node request that belonged to the previous composition — and closes a pin
  // composer that was anchored in a page that is no longer on screen.
  useEffect(() => {
    setRequestedUsage(null);
    setPin(null);
    // …and a ring borrowed by a comment card of the page one has just left.
    setHoveredNodeKey(null);
  }, [activePage?.page.id]);

  if (loading || (detailLoading && detail === null)) {
    return (
      <div className="flex h-svh items-center justify-center bg-background text-muted-foreground">
        <Spinner />
      </div>
    );
  }

  // A CONSULTED ROUND THAT PINNED NOTHING — a real, if rare, state of the history, and NOT the
  // "this Spec has no prototype" cue below, which would be a plain falsehood here. The way out is
  // the same one the header carries: back to the current round.
  if (consulting && activePage === null) {
    return (
      <div className="flex h-svh flex-col items-center justify-center gap-3 bg-background px-6 text-center">
        <span className="grid size-12 place-items-center rounded-full bg-muted text-muted-foreground">
          <HistoryIcon className="size-5" />
        </span>
        <p data-testid="proto-round-empty" className="max-w-md text-sm text-muted-foreground">
          {consultedRound?.label ?? "This round"} pinned no page — there is nothing to show of it.
        </p>
        <Button
          size="xs"
          variant="outline"
          data-testid="proto-round-back"
          onClick={() => consultRound(null)}
        >
          <UndoIcon aria-hidden />
          Back to current
        </Button>
      </div>
    );
  }

  // No prototype (or a read error) → the clean cue, with the way back. `proto_tree` returns null for
  // a Spec with no prototype: a valid state, not a failure.
  if (error || tree === null || activePage === null) {
    return (
      <div className="flex h-svh flex-col items-center justify-center gap-3 bg-background px-6 text-center">
        <span className="grid size-12 place-items-center rounded-full bg-muted text-muted-foreground">
          <LayoutTemplateIcon className="size-5" />
        </span>
        <p className="max-w-md text-sm text-muted-foreground">
          {error
            ? "The prototype could not be loaded."
            : "This Spec has no prototype yet. The agent proposes a prototype as an annexe of the Spec; once it does, its pages open here."}
        </p>
        <Link
          to="/work/$specId"
          params={{ specId }}
          data-testid="proto-back"
          className="text-sm text-foreground underline-offset-4 hover:underline"
        >
          Back to the Spec
        </Link>
      </div>
    );
  }

  const roundLabel = tree.current_round?.label ?? null;
  const roundId = tree.current_round?.id ?? null;
  const currentLocked =
    tree.design_lock != null && roundId != null && tree.design_lock.locked_round_id === roundId;
  const chosenVariantId = activePage.manifest_entry?.chosen_variant_id ?? null;
  // NOT WHILE CONSULTING: the manifest entry on screen belongs to the PAST round, and `roundId` is
  // the current one — an election written from here would land on a round the user is not looking
  // at. The whole Decision zone goes rather than being disabled (`readOnly` on the strip).
  const canElect = !consulting && roundId !== null && activePage.manifest_entry !== null;

  const railPages = panelPagesOf(tree);
  const thumbnailCtx = { projectId, themeTokensJson, prototypeId: tree.prototype.id };
  /**
   * THE COMMENTS OF THE PAGE ON SCREEN — the count the segment shows, computed by the very function
   * the thread lists with, so the number and the list cannot disagree (finding
   * `01KZ3JN72TM2XGBW2CTQRCBNER`). It used to be `tree.open_feedback.length`: the whole prototype's,
   * identical on every page.
   */
  const openComments = scopeComments(tree, activePage.page.id).openCount;
  /**
   * The nodes that already carry an OPEN comment — ringed in the render while pin mode is on, so a
   * reviewer sees what has already been said before saying it twice (depot's own "déjà commenté"
   * marker, adapted from its selectors to our node keys).
   *
   * OF THIS VARIANT, not of the prototype: a `node_key` is a path inside ONE composition, so ringing
   * the whole prototype's keys marks whatever element of the page on screen happens to share a path
   * with a comment raised somewhere else entirely.
   */
  const pinnedNodeKeys = tree.open_feedback
    .filter((f) => f.origin_variant_id !== null && f.origin_variant_id === variantId)
    .map((f) => f.node_key)
    .filter((k): k is string => k !== null);

  const railToggle = (
    <Button
      size="icon-xs"
      variant="ghost"
      data-testid="proto-rail-toggle"
      aria-label={railOpen ? "Fold the pages rail" : "Unfold the pages rail"}
      aria-expanded={railOpen}
      onClick={() => setRailOpen((v) => !v)}
    >
      {railOpen ? <ChevronLeftIcon /> : <ChevronRightIcon />}
    </Button>
  );

  const panelToggle = (
    <Button
      size="icon-xs"
      variant="ghost"
      data-testid="proto-panel-toggle"
      aria-label={panelOpen ? "Fold the side panel" : "Unfold the side panel"}
      aria-expanded={panelOpen}
      onClick={() => setPanelOpen((v) => !v)}
    >
      {panelOpen ? <ChevronRightIcon /> : <ChevronLeftIcon />}
    </Button>
  );

  /**
   * THE ROUND ON SCREEN, AND THE WAY INTO THE OTHERS — at the FOOT OF THE PAGES RAIL, under the
   * pages (finding `01KZ46P0FTMJXRTT32H6G5TEEN`, point 2).
   *
   * # The three requirements, and why none of them is in conflict
   *
   * It has been round the houses: rail foot → page header → rail TOP, above `PAGES` → and now back
   * to the foot, which is where the user wants to look for it ("trop bizarre qu'il soit tout en
   * haut"). Each move answered a real defect, and the defects still hold:
   *
   *  - the chevron that folds a rail of PAGES must not carry away the only entry point to the
   *    ROUNDS, and `Focus` — which folds that rail by definition — must not remove it exactly where
   *    the user asked for the most room (finding `01KZ3JXBS13KFH8JS2D804XZBM`);
   *  - a round is a property of the PROTOTYPE, decided between its pages, so it belongs to this
   *    page's own rail and not to the app's global navigation (finding `01KZ41YKMB63GFJ3QMK2ZHJN1C`);
   *  - and it reads as a foot control, not as a title bar.
   *
   * What made the first two look incompatible was never the FOOT — it was the foot being laid inside
   * the SCROLLING BODY, which a folded rail does not render at all. So it sits in the shell's own
   * `footer` slot: pinned, never scrolled, under the page list; and it is MIRRORED in the 52 px STUB
   * ([`PagesRailStub`]), at the foot there too. The rail folds to that stub rather than to nothing,
   * and `Focus` folds it to that same stub. Rail open, rail folded, Focus: three states, the round
   * reachable in all three.
   *
   * It is ONE control whatever the mode: consulting swaps its content and never adds a second one,
   * because there is only ever one round being looked at.
   */
  const roundSelector = (
    <div
      data-testid="proto-round-chip"
      className={cn(
        "flex min-w-0 items-center gap-2 rounded-lg border px-2 py-1",
        consulting ? "border-warning/50 bg-warning/10" : "border-border/60 bg-card",
      )}
    >
      <span
        className={cn("size-1.5 shrink-0 rounded-full", consulting ? "bg-warning" : "bg-primary")}
        aria-hidden
      />
      <span
        className={cn(
          "min-w-0 flex-1 truncate text-2xs",
          consulting ? "font-medium text-warning" : "text-muted-foreground",
        )}
      >
        {consulting
          ? `${consultedRound?.label ?? ""} · past round`
          : `${roundLabel ?? "No round"} · current`}
      </span>
      {currentLocked && !consulting ? (
        <Badge variant="info" className="shrink-0" data-testid="proto-round-frozen">
          Frozen
        </Badge>
      ) : null}
      <span className="shrink-0">
        <RoundHistory
          tree={liveTree ?? tree}
          consultedRoundId={consulting ? consultedRoundId : null}
          onConsult={consultRound}
        />
      </span>
    </div>
  );

  /** The same selector at 52 px: the history button alone, with the round it is on as its title. */
  const roundStub = (
    <span
      data-testid="proto-round-stub"
      title={
        consulting
          ? `${consultedRound?.label ?? ""} · past round`
          : `${roundLabel ?? "No round"} · current`
      }
      className={cn(
        "flex items-center justify-center rounded-md border",
        consulting ? "border-warning/50 bg-warning/10" : "border-border/60 bg-card",
      )}
    >
      <RoundHistory
        tree={liveTree ?? tree}
        consultedRoundId={consulting ? consultedRoundId : null}
        onConsult={consultRound}
      />
    </span>
  );

  const viewportWidth = viewport === "fit" ? null : Number(viewport);

  return (
    <div
      data-testid="prototype-page"
      data-focused={focused}
      // The mode, on the root — so a test (and a screenshot) reads it off the page itself rather
      // than off one of the several things it changes.
      data-consulting={consulting ? (consultedRound?.id ?? "") : undefined}
      className="flex h-svh w-full overflow-hidden bg-background text-foreground"
    >
      {/* LEFT — the pages rail. Folded, it keeps navigating (the miniatures stay clickable). */}
      <SidebarShell
        side="left"
        collapsible
        open={railOpen}
        width={RAIL_OPEN}
        collapsedWidth={RAIL_FOLDED}
        aria-label="Prototype pages"
        contentClassName="flex flex-col"
        header={
          <div className="flex items-center gap-2">
            <span className="font-medium text-2xs uppercase tracking-wide text-muted-foreground">
              Pages
            </span>
            <Badge variant="muted" className="shrink-0">
              {railPages.length}
            </Badge>
            <span className="ml-auto">{railToggle}</span>
          </div>
        }
        // THE FOOT OF THE RAIL — pinned by the shell, so it neither rides the page list's scroll nor
        // needs a rule drawn under it: the shell's own top border is the separation.
        footer={roundSelector}
        collapsed={
          <PagesRailStub
            pages={railPages}
            activePageId={activePage.page.id}
            onSelect={setActivePageId}
            ctx={thumbnailCtx}
            control={railToggle}
            // …AND FOLDED, THE ROUND IS STILL THERE, at the foot of the stub as at the foot of the
            // open rail — see `roundSelector` for why that settles all three requirements at once.
            roundControl={roundStub}
          />
        }
      >
        <PagesRail
          pages={railPages}
          activePageId={activePage.page.id}
          onSelect={setActivePageId}
          ctx={thumbnailCtx}
        />
      </SidebarShell>

      {/* CENTER — header over the stage. */}
      <div className="flex min-w-0 flex-1 flex-col">
        <header
          className={cn(
            "flex shrink-0 flex-col gap-2 border-b px-4 py-2.5",
            // CONSULTING A PAST ROUND repaints the header rather than adding a banner under it: the
            // whole strip changes colour, so the state is read before a single word of it is.
            consulting ? "border-warning/50 bg-warning/10" : "border-border/60 bg-muted/40",
          )}
        >
          <div className="flex items-center gap-3">
            <Breadcrumb>
              <Link to="/work" className={breadcrumbLinkClass}>
                Work
              </Link>
              <Link
                to="/work/$specId"
                params={{ specId }}
                data-testid="proto-back"
                aria-label="Back to the Spec"
                className={cn(breadcrumbLinkClass, "font-mono")}
              >
                {specKey(detail?.workspace_context?.project.key ?? "", detail?.spec.number ?? 0)}
              </Link>
              <BreadcrumbCurrent>Prototype</BreadcrumbCurrent>
            </Breadcrumb>

            {/* WHILE CONSULTING A PAST ROUND: WHAT IT WAS FOR, AND THE WAY BACK — and nothing that
                says a third time what the screen already says twice (finding
                `01KZ41YKMB63GFJ3QMK2ZHJN1C`, point 2).

                A `Consulting v6 — read-only` tag used to sit here, between a chip reading
                `v6 · past round` and a button reading `Back to current`, inside a header painted
                amber from edge to edge. Four signals for one state. The tag went: the AMBER says one
                is in the past, the chip NAMES the round, the button is the way out, and a control
                that offers no write is what says read-only — a label cannot make that true and did
                not need to say it.

                The SUMMARY stays, because nothing else carries it: it is the agent's own sentence
                about what that round was for, stored since T26 and displayed nowhere until finding
                `01KZ3JXCNK902DQXBD6QSYEE78`. One does not know what one is looking at without it.

                THE CATALOGUE LINK ALSO WENT (point 1). It is a project-wide reference, not a
                property of this page: it lives at the FOOT OF THE APP RAIL now, reachable from every
                screen, including this one, including in Focus. A global link has no business in the
                chrome of one page.

                THE ROUND CHIP moved too — to the top of the pages rail, above `PAGES` — see
                `roundSelector`. */}
            {consulting ? (
              <div className="ml-auto flex min-w-0 items-center gap-2">
                {consultedRound?.summary != null && consultedRound.summary.length > 0 ? (
                  <span
                    data-testid="proto-consulting-summary"
                    title={consultedRound.summary}
                    className="min-w-0 truncate text-2xs text-muted-foreground"
                  >
                    {consultedRound.summary}
                  </span>
                ) : null}
                <Button
                  size="xs"
                  variant="outline"
                  className="shrink-0"
                  data-testid="proto-round-back"
                  onClick={() => consultRound(null)}
                >
                  <UndoIcon aria-hidden />
                  Back to current
                </Button>
              </div>
            ) : null}
          </div>

          <div className="flex flex-wrap items-center gap-2">
            {/* THE PAGE, NAMED ONCE (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`). This line used to carry
                the TITLE and the SLUG side by side — two spellings of one fact, thirty centimetres
                from the rail row that already reads the title, and a few more from a params form
                that restated both. The rail is the page LIST, so it keeps the human title where it
                labels the navigation; this line carries the SLUG, which is the page's durable
                identity — what `?page=` deep-links by, what a `data-nyx-page` link resolves to and
                what the links strip under the stage names. One title, one slug, one screen. */}
            <code
              data-testid="proto-page-identity"
              className="min-w-0 truncate font-mono text-sm text-foreground"
            >
              /{activePage.page.slug}
            </code>
            {activeVersion ? (
              <Badge variant="muted" className="shrink-0">
                {activeVersion.version.label}
              </Badge>
            ) : null}
            {/* NO CSS-SLOT CHIP HERE, and no mark in the frame either (finding
                `01KYW32N1BVDVTGS0WPMG2PJW2`). A scoped CSS exception belongs to a COMPONENT, and the
                surface that judges a component is its DETAIL. On a page it read as a verdict on the
                page: an amber count in the page's identity line and a dashed badge stamped over the
                rendering of a component nobody is reviewing here. The card in the Params pane still
                says WHAT the selected node's component carries — that one is beside the node it
                speaks of, which is the whole difference. */}

            {/* HOW MANY HOLES THE PAGE HAS — a COUNT, and nothing else (finding
                `01KZ3JW5YR6Z7G5EW3VECSPBZZ`). The nodes themselves are NAMED IN PLACE inside the
                frame, at the exact spot each one occupies: the amber section that used to sit under
                the stage re-listed those very rows, with the very same reasons, 600 px lower —
                and admitted it, in its own words, "each one named in place". It took height from
                the scene to say what the scene already said. What a count adds that the placeholders
                cannot is a TOTAL, readable without scrolling the page to look for them; the
                sidecar's per-node explanation stays one hover away, verbatim. */}
            {unrendered.length > 0 ? (
              <Badge
                variant="warning"
                className="shrink-0"
                data-testid="proto-unrendered-count"
                title={unrendered
                  .map((n) => `${n.node_key} — ${unrenderedReasonLabel(n.reason)}: ${n.detail}`)
                  .join("\n")}
              >
                <PuzzleIcon aria-hidden />
                {unrendered.length === 1 ? "1 unrendered" : `${unrendered.length} unrendered`}
              </Badge>
            ) : null}

            <span className="flex-1" />

            <Segmented
              aria-label="Viewport width"
              value={viewport}
              onChange={setViewport}
              options={VIEWPORT_OPTIONS}
            />
            <span
              data-testid="proto-viewport-readout"
              className="shrink-0 rounded-md border border-border/60 bg-card px-2 py-1 font-mono text-2xs tabular-nums text-muted-foreground"
            >
              <b className="font-semibold text-foreground">{rendered.w}</b> × {rendered.h}
            </span>
            <Button
              size="xs"
              variant="outline"
              data-testid="proto-focus"
              aria-pressed={focused}
              onClick={toggleFocus}
              title="Fold both panels — the stage takes the width back"
            >
              {focused ? <MinimizeIcon aria-hidden /> : <MaximizeIcon aria-hidden />}
              Focus
            </Button>
          </div>
        </header>

        {/* THE STAGE — the subject of the screen. Everything above and below it is either the page's
            identity, the decision it is waiting for, or the verdict on what could not be drawn. */}
        <div className="flex min-h-0 flex-1 flex-col px-4 pt-3 pb-3">
          <div
            ref={stageRef}
            data-testid="proto-stage"
            className="relative min-h-0 flex-1 overflow-auto rounded-t-xl border border-border/60 bg-muted/20"
          >
            {/* NO PIN BANNER (finding `01KZ41YKMB63GFJ3QMK2ZHJN1C`, point 4). A bubble used to sit
                in the stage's top-right corner reading `Pin mode — click an element to comment on
                its node`, while the control that armed the mode — in the Comments pane, two
                centimetres away — was already pressed and already read `Pinning`. The state is now
                shown BY THE SURFACE IT CONCERNS: the PAGE takes a primary border while the mode is
                armed (see the surface below), so the arming is read exactly where the click is
                about to land, and a label saying what a click will do is replaced by the thing the
                click will happen on. */}
            {/* NO GUTTER CAPTION (finding `01KZ3JW4PAM2MAJ3NVFR0XJSWZ`). A bubble in the top-left
                margin explained that the grey area is grey — "the page is 390 px wide, the stage is
                not" — while the readout thirty centimetres away read 388, so the one sentence the
                stage carried was both idle and WRONG. The margins already read as margins: they are
                a different surface, and the page is bordered on both edges. */}
            {/* `inline-flex` + `min-w-full` is what makes the page CENTRED when it is narrower than
                the stage and SCROLLABLE (from its left edge) when it is wider — a plain
                `justify-center` block clips the overflow out of reach on the left. The HEIGHT is
                `h-full`, not `min-h-full`: the frame IS the page's viewport and scrolls its own
                content, so a stage that also grew vertically would nest a second scrollbar around
                the first (seen in the running app before the fix). */}
            {/* `align-bottom` kills the INLINE baseline leading an inline-level box carries under
                it — measured at 7 px in the running app, which alone raised a parasitic vertical
                scrollbar on a stage whose content fits exactly. */}
            <div className="inline-flex h-full min-w-full justify-center align-bottom">
              <div
                ref={bindSurface}
                data-testid="proto-preview-surface"
                data-viewport={viewport}
                // THE PAGE GETS THE WIDTH THAT WAS ASKED FOR — exactly (finding
                // `01KZ3JPWNYP0B65Z31RRX1EEMT`). `boxSizing: content-box` is the whole fix, and it
                // is set HERE, inline, beside the width it qualifies: the preset writes its number
                // into `width`, and under the default `border-box` the two gutter borders ate it
                // from the inside — 390 asked for, 388 laid out, 768 → 766, 1280 → 1278. A media
                // query at 390 px DOES NOT FIRE at 388, and the whole reason this selector exists is
                // to make the prototype testable at the widths the real page will meet.
                //
                // The previous correction went the other way round: it moved the READOUT from the
                // border box to the content box, which made the reading honest about a frame that
                // was wrong. Honest about a lie is still a lie by two pixels. The frame is made
                // right instead, and the readout keeps MEASURING the content box — the box the page
                // is really laid out in — so it still cannot restate the button that was pressed.
                style={
                  viewportWidth != null
                    ? { width: viewportWidth, boxSizing: "content-box" }
                    : undefined
                }
                // ARMED READS ON THE PAGE ITSELF (finding `01KZ41YKMB63GFJ3QMK2ZHJN1C`, point 4):
                // the surface the next click will land on takes a primary border.
                //
                // An OUTLINE, and neither a border nor a ring, for two measured reasons. A BORDER
                // would lay out, and the page must keep the width the preset gave it to the pixel
                // (finding `01KZ3JPWNYP0B65Z31RRX1EEMT`). A `ring-inset` is an inset BOX-SHADOW,
                // which paints UNDER the element's own children — and this element's child is an
                // opaque iframe filling it edge to edge, so the mark was computed, present in the
                // DOM, and invisible on screen (seen in the running app). An outline is painted in
                // the last phase, over the descendants, and `-outline-offset-2` pulls it inside the
                // box so nothing of it is clipped by the stage's scroll box.
                data-pin-armed={pinMode || undefined}
                className={cn(
                  // `relative` is what the anchored pin composer is positioned against: the frame's
                  // own box, which is the coordinate system its clicks are reported in.
                  "relative flex h-full shrink-0 flex-col",
                  viewportWidth != null ? "border-x border-border/60" : "w-full",
                  pinMode && "outline-2 -outline-offset-2 outline-primary",
                )}
              >
                {preview.kind === "no-variant" ? (
                  <div className="flex flex-1 items-center justify-center p-4 text-center text-xs text-muted-foreground">
                    This page has no variant to preview yet.
                  </div>
                ) : preview.kind === "loading" ? (
                  <div className="flex flex-1 items-center justify-center">
                    <Spinner className="text-muted-foreground/50" />
                  </div>
                ) : preview.kind === "error" ? (
                  <div
                    data-testid="proto-preview-error"
                    className="flex flex-1 items-center justify-center p-4 text-center text-xs text-destructive/80"
                  >
                    {preview.message}
                  </div>
                ) : preview.kind === "unavailable" ? (
                  <div
                    data-testid="proto-preview-unavailable"
                    className="flex flex-1 items-center justify-center p-4 text-center text-xs text-muted-foreground"
                  >
                    {preview.message}
                  </div>
                ) : preview.kind === "empty" ? (
                  <div
                    data-testid="proto-preview-empty"
                    className="flex flex-1 items-center justify-center p-4 text-center text-xs text-muted-foreground"
                  >
                    This composition compiled to nothing — every node of it renders empty.
                  </div>
                ) : (
                  <CompositionFrame
                    markup={preview.markup}
                    themeTokensJson={themeTokensJson}
                    states={states}
                    axes={axes}
                    onNavigate={onNavigate}
                    // THE RENDER IS THE COMPOSITION: a click designates a node — for its params, or,
                    // in pin mode, for the comment about to be anchored on it. A CONSULTED round
                    // takes no pick at all: both sinks lead to a panel that is not there, and
                    // neither params nor comments are things one edits in the past.
                    pickMode={consulting ? "off" : pinMode ? "pin" : "select"}
                    rootNodeKey={rootKey}
                    // The hovered COMMENT wins while it lasts, then the Params selection comes back:
                    // one ring, and the pane currently speaking owns it.
                    selectedNodeKey={hoveredNodeKey ?? selectedNodeKey}
                    pinnedNodeKeys={pinnedNodeKeys}
                    onNodeSelect={onNodeSelect}
                    onNodePin={onNodePin}
                    // The CSS-slot mark belongs to the composant detail, not to a page under review.
                    showCssSlotMark={false}
                    className="min-h-0 flex-1"
                    title={`${activePage.page.title} preview`}
                  />
                )}

                {/* THE ANCHORED COMPOSER — opened AT the click, over the very element it is about.
                    Adapted from depot's pin popup; what it shows is a NODE of the named tree, not a
                    CSS selector. */}
                {pin !== null ? (
                  <PinPopup
                    pin={pin}
                    wrapper={surfaceEl}
                    pending={actions.pending}
                    onCancel={() => setPin(null)}
                    onSubmit={(body) => {
                      // THE VARIANT IT WAS PINNED ON travels with it: an anchor alone does not say
                      // where the element the reviewer clicked lives (finding
                      // `01KZ3JN72TM2XGBW2CTQRCBNER`).
                      void actions.addComment(tree.prototype.id, body, pin.nodeKey, variantId);
                      setPin(null);
                    }}
                  />
                ) : null}
              </div>
            </div>
          </div>

          {/* THE VARIANT STRIP — UNDER the surface, a sibling of it, never an overlay, with the
              election decision lodged among the variants it decides between. */}
          <div className="shrink-0 rounded-b-xl border border-t-0 border-border/60">
            <VariantStrip
              variants={variants}
              selectedVariantId={variant?.id ?? null}
              onSelect={setSelectedVariantId}
              chosenVariantId={chosenVariantId}
              effectiveVariantId={activePage.effective_variant_id}
              canElect={canElect}
              pending={actions.pending}
              onElect={(variantId) =>
                roundId && void actions.elect(roundId, activePage.page.id, variantId)
              }
              onClear={() => roundId && void actions.clearElection(roundId, activePage.page.id)}
              notice={actions.notice}
              onDismissNotice={actions.dismissNotice}
              readOnly={consulting}
            />
          </div>

          {/* NO AMBER SECTION UNDER THE STAGE for the holes (finding `01KZ3JW5YR6Z7G5EW3VECSPBZZ`).
              It listed every unrenderable node with its reason — the SAME rows the sidecar already
              draws IN PLACE inside the frame, at the exact spot each node occupies — and said so
              itself: "each one named in place". It then added "listed as the walk records them,
              children before their parent", a sentence about the ORDER of a list nobody needed. Two
              readings of one fact, the second one stealing height from the scene, which is the
              grievance this whole screen exists to answer. The count survives, in the page's
              identity line above, where it costs the stage nothing.

              WHY THE SECTION DID NOT FIRE ON THE OTHER SPEC, and why that was never a bug: it is
              keyed on `unrendered[]`, and the sidecar fills that list for a node it COULD NOT DRAW —
              five named reasons, from an absent catalogue entry to a template carrying logic. A node
              whose component IS in the catalogue WITH a template is drawn; if it then looks empty on
              screen, it is because the values it needed never reached it (a `props` key where the
              component declares `params`, a `required` param nobody set). That is a CONTRACT defect,
              not a hole, and it has its own verdict — {@link NodeContractNotice}, in the params
              pane, on the node it judges. The two specs differ in their compositions, never in this
              screen's behaviour. */}

          {/* WHERE THIS PAGE GOES — the render's own `navigation[]`, displayed (finding
              `01KZ3JN69E32FYBF01NBE4FWJD`). Nothing read this list, `navigation_count` or
              `navigation_broken_count` anywhere: an inter-page parcours could be entirely dead — as
              it was, every render being given no page directory — and the screen said nothing.

              A LINK THAT NAVIGATES AND A LINK WHOSE PAGE IS GONE ARE BOTH FACTS ABOUT THIS PAGE, so
              both are listed, each with the sidecar's own status. The strip only exists when the page
              links somewhere: a page with no `navigate_to` owes the reviewer no line about links. */}
          {navigation.length > 0 ? (
            <section
              data-testid="proto-links"
              aria-label="Pages this one links to"
              className={cn(
                "mt-2 shrink-0 rounded-lg border px-3 py-1.5",
                navigationBrokenCount > 0
                  ? "border-warning/40 bg-warning/[0.06]"
                  : "border-border/60 bg-muted/20",
              )}
            >
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <RouteIcon
                  aria-hidden
                  className={cn(
                    "size-3.5 shrink-0",
                    navigationBrokenCount > 0 ? "text-warning" : "text-muted-foreground",
                  )}
                />
                <span
                  data-testid="proto-links-count"
                  className={cn(
                    "font-medium text-xs",
                    navigationBrokenCount > 0 ? "text-warning" : "text-foreground",
                  )}
                >
                  {navigation.length === 1 ? "1 link" : `${navigation.length} links`}
                  {navigationBrokenCount > 0 ? ` · ${navigationBrokenCount} broken` : ""}
                </span>
              </div>
              <ul className="mt-0.5 flex flex-col gap-0.5">
                {navigation.map((link) => {
                  const linked = link.status === "linked";
                  return (
                    <li
                      key={`${link.node_key}:${link.page_id}`}
                      data-testid={`proto-link-${link.node_key}`}
                      // The sidecar's full sentence — what happened and what to do about it.
                      title={link.detail.length > 0 ? link.detail : undefined}
                      className="flex flex-wrap items-center gap-1.5 text-2xs"
                    >
                      <span className="font-mono text-foreground">{link.node_key}</span>
                      <span className="text-muted-foreground/70">→</span>
                      {/* The destination READS as its title when the id resolved; as the raw id
                          when it did not, because that id is the only thing left of it. */}
                      <span className={linked ? "text-muted-foreground" : "font-mono text-warning"}>
                        {link.page_title ?? link.page_id}
                      </span>
                      <Badge variant={linked ? "muted" : "warning"} className="shrink-0">
                        {navigationStatusLabel(link.status)}
                      </Badge>
                    </li>
                  );
                })}
              </ul>
            </section>
          ) : null}
        </div>
      </div>

      {/* RIGHT — Comments · Params. TWO panes of one panel: stacked under the stage they each cost it
          height, which is the whole grievance; here they cost it nothing. There were three, and the
          third (Composition) was a node list disconnected from both — it is now the page's own named
          tree inside Params, because what one designates a node ON is the render.

          CONSULTING A PAST ROUND KEEPS THE PANEL, AND EMPTIES IT OF EVERY SINK (finding
          `01KZ3JXB3SN8BMK7FS16PPSZ6T`). The flank used to be WITHDRAWN outright, on a good argument:
          both panes AUTHOR — one writes a comment, the other saves params into a variant — and one
          does not author into the past; removing them makes "no mutation" structural rather than a
          row of disabled buttons. But it took the REVIEW THREAD with it, and re-reading the thread
          while looking at the round it produced is exactly what one opens a past round for. It also
          took the chevron, so there was no way to ask for it back.

          So the guarantee is kept and the casualty is not. The panel stays, it is THE SAME PANEL (no
          second flank, no extra width, the same fold control), and while consulting it holds ONE
          pane — the thread, in `readOnly`, which renders no composer, no pin toggle and no `Resolve`
          at all. PARAMS IS NOT THERE: a params form is a write surface with nothing to read in it,
          and its own segment would be a control leading nowhere. The header therefore states the
          pane instead of offering a choice between one thing and a refusal. */}
      <SidebarShell
        side="right"
        collapsible
        open={panelOpen}
        width={PANEL_OPEN}
        collapsedWidth={PANEL_FOLDED}
        aria-label="Comments and node params"
        // EACH PANE OWNS ITS OWN SCROLLING (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`). One scroller
        // around both is what put the comment composer at the bottom of an ever-growing column;
        // giving the panel body a definite height instead lets the thread split it — list scrolls,
        // box stays — and lets the Params pane keep its tree pinned above its form.
        contentClassName="flex min-h-0 flex-col overflow-hidden"
        header={
          <div className="flex items-center gap-1.5">
            {consulting ? (
              <span
                data-testid="proto-panel-readonly"
                className="min-w-0 flex-1 truncate font-medium text-2xs text-muted-foreground"
              >
                Comments{openComments > 0 ? ` · ${openComments} open` : ""}
              </span>
            ) : (
              /* `fill` — the two segments SHARE the bar (finding `01KZ3JXDFTFMMK50RQP7HECBEQ`).
                 Measured in the running app: 71 px + 47 px of segments inside a 298 px pill, i.e.
                 60 % of a two-state selector reading as a tab bar with its tabs missing (a third
                 segment, Composition, did live there and was withdrawn — the bar was never resized
                 after it). Stretched by the header AND filled by its options, the control now IS
                 what it shows. */
              <Segmented
                aria-label="Side panel"
                className="min-w-0 flex-1"
                fill
                value={pane}
                onChange={setPane}
                options={[
                  {
                    v: "comments" as const,
                    label: `Comments${openComments > 0 ? ` ${openComments}` : ""}`,
                  },
                  { v: "params" as const, label: "Params" },
                ]}
              />
            )}
            {panelToggle}
          </div>
        }
        collapsed={
          <div className="flex h-full flex-col items-center gap-2 py-2.5">
            {panelToggle}
            {openComments > 0 ? (
              <Badge variant="warning" data-testid="proto-panel-stub-count">
                {openComments}
              </Badge>
            ) : (
              <MessageSquareIcon aria-hidden className="size-3.5 text-muted-foreground/60" />
            )}
          </div>
        }
      >
        {consulting || pane === "comments" ? (
          <NodeComments
            tree={tree}
            actions={actions}
            // THE THREAD OF THIS PAGE, and the variant every comment written from it records.
            pageId={activePage.page.id}
            originVariantId={variantId}
            flush
            // A ROUND ONE ONLY READS TAKES NO WRITE: no composer, no pin toggle, no Resolve, and
            // the render takes no pick either (`pickMode="off"` above).
            readOnly={consulting}
            pinMode={!consulting && pinMode}
            onTogglePinMode={
              consulting
                ? undefined
                : (next) => {
                    setPinMode(next);
                    if (!next) setPin(null);
                  }
            }
            // POINTING AT A COMMENT POINTS AT ITS ELEMENT (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`).
            // Not while consulting: there the render takes no pick at all, and a ring drawn by a
            // pane the user cannot act from would be the only live thing on a read-only screen.
            onHoverNode={consulting ? undefined : setHoveredNodeKey}
          />
        ) : (
          /* THE TREE IS THE SELECTOR, SO IT COMES FIRST (finding `01KZ41ZM4ZKR3F0WB53WSFJB0F`).
             The form used to sit on top of the tree that chooses what it edits — the answer above
             the question. It is the same order the thread next door follows: the list, then the box
             one acts with. The TREE scrolls (it is the long one, and it grows with the page); the
             FORM keeps its own height under it, exactly as the composer does in Comments. */
          <div className="flex min-h-0 flex-1 flex-col">
            <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 py-3">
              {/* The WHOLE composition's contract at the read, beside the per-node editor that
                  fixes it — and off the stage, which is what it used to sit on. */}
              {contractReport.issues.length > 0 ? (
                <NodeContractNotice report={contractReport} />
              ) : null}
              {/* THE COMPOSITION ON SCREEN — the named tree of the very variant the stage renders,
                  with each node's nature (`[DS]` / `[NEW]` / `[inline]`). It used to read the
                  FROZEN elected composition of the design-locked round: another variant, another
                  page version, and no entry at all for a page that round did not pin — so it
                  listed components the rendering never named, did not move when the variant
                  changed, and every `Open` in it pointed at a node the form below could not bind
                  (findings `01KZ3JN5KJQWJTV2XNT604CWCT`, `01KZ3JPVDT7RXEC3P9PVSJ95CN`). */}
              <CompositionTree
                compositionJson={composition}
                variantLabel={variant?.label ?? null}
                pageId={activePage.page.id}
                variantId={variantId}
                components={components}
                componentsLoading={componentsLoading}
                cssSlots={cssSlots}
                // WHICH ROW IS BEING EDITED — the panel's own answer, so the tree and the form
                // cannot disagree about what is open.
                editingNodeKey={selectedNodeKey}
                onOpenNode={onOpenNode}
              />
            </div>
            <div className="min-h-0 shrink-0 overflow-y-auto border-border/60 border-t px-3 py-3">
              <NodeParamsPanel
                variant={variant}
                components={components}
                componentsLoading={componentsLoading}
                requestedUsage={requestedUsage}
                cssSlots={cssSlots}
                onSaved={() => void refreshTree()}
                onSelectedChange={setSelectedNodeKey}
              />
            </div>
          </div>
        )}
      </SidebarShell>
    </div>
  );
}

export default PrototypePage;
