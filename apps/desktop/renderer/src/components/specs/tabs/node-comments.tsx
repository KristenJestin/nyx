import { useState } from "react";
import {
  BotIcon,
  CheckIcon,
  ChevronDownIcon,
  CrosshairIcon,
  LayersIcon,
  MessageSquareIcon,
  UnlinkIcon,
  UserIcon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import {
  ANCHOR_STATUS_ORPHANED,
  FEEDBACK_AUTHOR_USER,
  FEEDBACK_STATUS_OPEN,
  type ProtoTree,
  type PrototypeFeedback,
} from "../proto-types";
import type { ProtoActions } from "../use-proto-actions";

/**
 * `<NodeComments>` — the PRE-DEV review THREAD (T28, review #24). It LISTS the prototype's comments
 * — open and resolved — each carrying its anchor: a comment whose node still exists is CARRIED
 * (`anchor_status: anchored`); a comment whose node vanished is "TO RE-ANCHOR"
 * (`anchor_status: orphaned`) — visually DISTINCT, code-review style (a dashed amber card + a
 * "To re-anchor" badge). An open comment is RESOLVABLE (`proto_comment_resolve`).
 *
 * # How a comment is ANCHORED (finding `01KYW32N1BVDVTGS0WPMG2PJW2`)
 *
 * It used to be a DROPDOWN OF `node_key`s: the composer listed the shown variant's flattened tree
 * and asked the reviewer to pick `Root/Filters/Reset` from it. That is asking someone looking at a
 * PAGE to name a path — the reviewer sees a button, not a key, and the list says nothing about
 * which line of the rendering each entry is.
 *
 * So this panel now offers exactly two gestures, and neither is a path:
 *
 *  - a GLOBAL comment — the composer here, for what is about the page rather than about one element
 *    (`proto_comment_add` with no `nodeKey`, which the backend has always accepted);
 *  - PIN MODE — the toggle here arms the RENDER: the next click in the page opens the anchored
 *    composer on the element itself ([`PinPopup`]), and the comment is stored on the `node_key` that
 *    element belongs to. The mechanism is depot's; the anchor is a NODE, because here the tree is
 *    the contract.
 *
 * These comments are a PRE-DEV review artifact: they are stored on the prototype and shown here, but
 * are NEVER wired into any coder context (PDR-0009 — the corrected composition ships, not the
 * comments). This component sends them NOWHERE but the two `proto_comment_*` writes.
 *
 * # THE THREAD READS DOWNWARDS (finding `01KZ3AN7VVAPXMYFQG4AP441BC`)
 *
 * The composer used to sit on TOP — a button, a paragraph explaining what an anchored comment is,
 * and a textarea — pushing the thread itself below the fold. A thread reads like a thread: the
 * COMMENTS first, the box to add one LAST, the way every conversation surface is laid out. The pin
 * toggle went down with it, into the composer's action row: it is a verb, a sibling of `Comment on
 * the page`, and a verb has no business displacing the list. The paragraph is gone outright — the
 * PAGE ITSELF takes a primary border while the mode is armed (finding
 * `01KZ41YKMB63GFJ3QMK2ZHJN1C`), which says it where the click is about to land.
 *
 * # …AND IT DOES NOT WALK AWAY (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`)
 *
 * `LAST` was taken literally: the composer was the last child of the ONE scrolling column, so the
 * more comments a page carried, the further one scrolled to write the next one. In a panel host
 * (`flush`) the thread now SPLITS its height — the list scrolls, the box below it does not — and the
 * RESOLVED comments, which ask nothing of anybody, sit in a block FOLDED by default under their
 * count. The order is unchanged; only the scroller is.
 *
 * # POINTING AT A COMMENT POINTS AT ITS ELEMENT (same finding)
 *
 * Hovering an ANCHORED card designates its node IN THE RENDER, through the host's existing channel —
 * the one the params selection already rings with — so the page has ONE way of saying "this
 * element", whoever is asking. It fires ONLY when the variant on screen is the one the comment was
 * raised on: a `node_key` is a path inside ONE composition, and ringing it under another variant
 * would point at whatever element happens to share that path. In that case nothing is highlighted
 * and the card's own variant badge is the answer.
 *
 * # THE THREAD FOLLOWS THE PAGE (finding `01KZ3JN72TM2XGBW2CTQRCBNER`, CRITICAL)
 *
 * It listed `tree.open_feedback` — every comment of the PROTOTYPE — whatever page was on screen. On
 * a two-page proto that is the same thread twice, counter included: a remark written on page 2 read
 * as `page-wide` on page 1, with nothing saying where it came from, and the five comments of page 1
 * reappeared on page 2. `page-wide` stops meaning anything the moment there are two pages.
 *
 * Nothing had to be added to store it. `prototype_feedback.origin_variant_id` has existed since
 * T28 — the agent's own `proto_comment_add` tool fills it, the read model carries it, and
 * [`panelPagesOf`] already counts a page's comments through it. The renderer simply never SENT it:
 * every comment written from this panel landed with a null origin, which is why they all looked
 * page-less and why the rail's per-page badge stayed at zero. {@link ProtoActions.addComment} now
 * carries the shown variant, and this thread reads it back:
 *
 *  - a comment whose origin variant belongs to the page on screen is IN the thread;
 *  - it shows the VARIANT it was raised on, next to its anchor. A comment pinned on `Clear search`
 *    in variant C used to render identically while variant A — which has no such button — was on
 *    screen; the only reason the demo comment made sense was that its author had typed "Variant B"
 *    into the body by hand;
 *  - a comment with NO recorded origin (written before this, or by a tool that omitted it) belongs
 *    to no page and is not hidden: it sits in its own group, which says exactly that. Attributing it
 *    to the page that happens to be on screen would be inventing the very fact that is missing.
 */

/** WHERE a comment was raised, resolved from its `origin_variant_id` against the tree. */
export interface CommentOrigin {
  readonly pageId: string;
  readonly pageTitle: string;
  /** The variant's label (`A` / `B` / `C`) — what makes a pinned remark re-findable. */
  readonly variantLabel: string;
}

/**
 * The ORIGIN of every variant the tree carries, indexed by variant id — including the variants of
 * ARCHIVED versions, because a comment raised on a page's older version still belongs to that page.
 */
export function commentOrigins(tree: ProtoTree): Map<string, CommentOrigin> {
  const index = new Map<string, CommentOrigin>();
  for (const page of tree.pages) {
    for (const version of page.versions) {
      for (const variant of version.variants) {
        index.set(variant.id, {
          pageId: page.page.id,
          pageTitle: page.page.title,
          variantLabel: variant.label,
        });
      }
    }
  }
  return index;
}

/** A comment's thread + the origin the display names beside it. */
export interface ScopedComment {
  readonly feedback: PrototypeFeedback;
  /** `null` when the comment records no origin — it belongs to no page, and that is said. */
  readonly origin: CommentOrigin | null;
}

/** The comments of ONE page, and the ones that belong to no page at all. */
export interface ScopedComments {
  readonly open: readonly ScopedComment[];
  readonly resolved: readonly ScopedComment[];
  /** Comments with no recorded origin — shown apart, never attributed to the page on screen. */
  readonly unattributed: readonly ScopedComment[];
  /**
   * How many OPEN comments THIS THREAD SHOWS — the page's own, plus the page-less ones it lists
   * apart. One rule for every counter of this screen (the pane's segment, the folded stub, the
   * collapsible header): each of them says how many open comments the thread below it holds. The
   * rail's per-page badge answers a different, differently-labelled question — what has been said
   * ABOUT that page — and the two coincide the moment every comment records its origin.
   */
  readonly openCount: number;
}

/**
 * SCOPE the prototype's comments to ONE page — the single reading every surface of this screen uses,
 * so the thread, its counter and the rail's per-page badge cannot say three different things about
 * the same page.
 *
 * `pageId` is `null` for a host that shows NO page in particular (the Spec tab's entry card, which
 * speaks of the whole prototype): the thread is then the whole prototype's, and each comment carries
 * the page it was raised on instead. Both readings satisfy the same rule — a comment is never shown
 * without saying where it comes from.
 */
export function scopeComments(tree: ProtoTree, pageId: string | null): ScopedComments {
  const origins = commentOrigins(tree);
  const open: ScopedComment[] = [];
  const resolved: ScopedComment[] = [];
  const unattributed: ScopedComment[] = [];
  const sort = (list: readonly PrototypeFeedback[], into: ScopedComment[]): void => {
    for (const feedback of list) {
      const origin =
        feedback.origin_variant_id != null
          ? (origins.get(feedback.origin_variant_id) ?? null)
          : null;
      if (origin === null) unattributed.push({ feedback, origin: null });
      else if (pageId === null || origin.pageId === pageId) into.push({ feedback, origin });
    }
  };
  sort(tree.open_feedback, open);
  sort(tree.resolved_feedback, resolved);
  const openCount =
    open.length + unattributed.filter((c) => c.feedback.status === FEEDBACK_STATUS_OPEN).length;
  return { open, resolved, unattributed, openCount };
}

/** `created_at` (epoch seconds or ms) → a short `dd/mm hh:mm` label. */
function formatTime(ts: number): string {
  // The proto timestamps cross the wire as epoch SECONDS; normalize to ms for `Date`.
  const ms = ts < 1e12 ? ts * 1000 : ts;
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** A compact author chip: an icon (human vs agent session) + a label. */
function AuthorChip({ kind }: { kind: string }) {
  const isUser = kind === FEEDBACK_AUTHOR_USER;
  const Icon = isUser ? UserIcon : BotIcon;
  return (
    <span className="inline-flex items-center gap-1 text-2xs text-muted-foreground">
      <Icon aria-hidden className="size-3" />
      <span className="font-medium text-foreground/80">{isUser ? "You" : "Agent"}</span>
    </span>
  );
}

/** ONE comment card: author + time + its ORIGIN + its NODE anchor (distinct when orphaned) + body. */
function CommentCard({
  feedback,
  origin,
  showPage = false,
  onResolve,
  onHoverNode,
  designates = false,
  pending,
}: {
  feedback: PrototypeFeedback;
  /** WHERE it was raised — `null` when the comment records no origin. */
  origin: CommentOrigin | null;
  /** Name the PAGE beside the variant — for a thread that is not scoped to one page. */
  showPage?: boolean;
  /**
   * SHOW THE ELEMENT THIS CARD IS ABOUT (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`). Hovering an ANCHORED
   * comment designates its node in the render — `null` on leave. Absent, or `designates` false, and
   * the card designates nothing at all.
   */
  onHoverNode?: (nodeKey: string | null) => void;
  /**
   * Can this card designate? True only when the comment carries a node AND the variant on screen is
   * the one it was raised on. A `node_key` is a path inside ONE composition, so ringing it while
   * another variant is displayed would point at whatever element happens to share that path — the
   * card already says which variant it belongs to, and that badge is the honest answer here.
   */
  designates?: boolean;
  /**
   * Resolve handler — absent when the card offers no `Resolve`: an already-resolved comment, or a
   * READ-ONLY thread. Its absence is NOT what says "resolved" (finding `01KZ3JXB3SN8BMK7FS16PPSZ6T`):
   * the STATUS says that, and reading it off the missing handler is what would badge an open comment
   * `Resolved` the moment the thread stops offering the button.
   */
  onResolve?: (id: string) => void;
  pending: boolean;
}) {
  const orphaned = feedback.anchor_status === ANCHOR_STATUS_ORPHANED;
  const resolved = feedback.status !== FEEDBACK_STATUS_OPEN;
  const nodeKey = feedback.node_key;
  // The hover DESIGNATES only when there is something to designate: an anchored comment, raised on
  // the variant on screen. Everything else hands the host nothing rather than a wrong element.
  const canDesignate = designates && nodeKey != null && onHoverNode !== undefined;
  return (
    <div
      data-testid={`proto-comment-${feedback.id}`}
      data-anchor-status={feedback.anchor_status}
      data-designates={canDesignate || undefined}
      // The RENDER is where this comment's subject is, so pointing at the card points at the
      // element (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`). It reuses the host's existing channel — the
      // one the params selection rings with — so the page has ONE way of saying "this element",
      // whoever is asking. `focus`/`blur` carry the same gesture to the keyboard.
      onMouseEnter={canDesignate ? () => onHoverNode(nodeKey) : undefined}
      onMouseLeave={canDesignate ? () => onHoverNode(null) : undefined}
      onFocus={canDesignate ? () => onHoverNode(nodeKey) : undefined}
      onBlur={canDesignate ? () => onHoverNode(null) : undefined}
      className={cn(
        "rounded-lg border bg-card px-3 py-2 text-sm shadow-sm",
        resolved && "opacity-80",
        // Orphaned ("to re-anchor") → a DISTINCT, code-review-style card (dashed amber).
        orphaned ? "border-warning/50 border-dashed bg-warning/[0.04]" : "border-border",
      )}
    >
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <AuthorChip kind={feedback.author_kind} />
        <span className="font-mono text-2xs text-muted-foreground/60">
          {formatTime(feedback.created_at)}
        </span>
        {/* The NODE anchor — the `node_key`, mono. Absent (unanchored) → a discreet fallback. */}
        {nodeKey != null ? (
          <span
            className={cn(
              "inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-2xs",
              orphaned
                ? "border-warning/40 bg-warning/10 text-warning line-through"
                : "border-border/60 bg-muted/40 text-muted-foreground",
            )}
          >
            {nodeKey}
          </span>
        ) : (
          <span className="text-2xs text-muted-foreground/60 italic">page-wide</span>
        )}
        {/* THE VARIANT IT WAS RAISED ON. An anchor alone is not a location on a page that offers
            three of them: `Root/Filters/Reset` exists in the variant the reviewer pinned it on and
            may exist nowhere else. Stated for every comment, anchored or page-wide, because a
            page-wide remark about variant C is not a remark about variant A either. */}
        {origin !== null ? (
          <span
            data-testid={`proto-comment-origin-${feedback.id}`}
            className="inline-flex items-center gap-1 rounded-md border border-border/60 bg-muted/40 px-1.5 py-0.5 text-2xs text-muted-foreground"
          >
            <LayersIcon aria-hidden className="size-3" />
            {showPage ? `${origin.pageTitle} · ${origin.variantLabel}` : origin.variantLabel}
          </span>
        ) : (
          /* NO ORIGIN AT ALL — said, never guessed. The comment predates the origin being sent, so
             nothing on this screen can tell which page or variant it was about. */
          <span
            data-testid={`proto-comment-no-origin-${feedback.id}`}
            className="inline-flex items-center gap-1 rounded-md border border-dashed border-border/60 px-1.5 py-0.5 text-2xs text-muted-foreground/70"
          >
            <LayersIcon aria-hidden className="size-3" />
            no variant recorded
          </span>
        )}
        {orphaned ? (
          <Badge
            variant="warning"
            className="gap-1"
            data-testid={`proto-comment-orphaned-${feedback.id}`}
          >
            <UnlinkIcon aria-hidden />
            To re-anchor
          </Badge>
        ) : null}
        {resolved ? (
          <Badge variant="success" className="gap-1">
            <CheckIcon aria-hidden />
            Resolved
          </Badge>
        ) : null}
      </div>

      <p className="whitespace-pre-wrap text-sm leading-relaxed text-foreground/90">
        {feedback.body}
      </p>

      {feedback.resolution_note != null && feedback.resolution_note.length > 0 ? (
        <p className="mt-1.5 border-border/50 border-t pt-1.5 text-2xs text-muted-foreground">
          Resolution: {feedback.resolution_note}
        </p>
      ) : null}

      {onResolve != null ? (
        <div className="mt-1.5 flex justify-end">
          <Button
            size="xs"
            variant="ghost"
            disabled={pending}
            data-testid={`proto-comment-resolve-${feedback.id}`}
            onClick={() => onResolve(feedback.id)}
          >
            <CheckIcon aria-hidden />
            Resolve
          </Button>
        </div>
      ) : null}
    </div>
  );
}

export function NodeComments({
  tree,
  actions,
  pageId = null,
  originVariantId = null,
  composer = true,
  flush = false,
  readOnly = false,
  pinMode = false,
  onTogglePinMode,
  onHoverNode,
}: {
  tree: ProtoTree;
  actions: ProtoActions;
  /**
   * The page ON SCREEN — the thread is scoped to it. `null` (a host showing no page) leaves nothing
   * attributable, which is what the "no page recorded" group then says of every comment.
   */
  pageId?: string | null;
  /**
   * The VARIANT on screen — recorded on every comment written from here
   * (`prototype_feedback.origin_variant_id`), which is what makes a comment belong to a page at all.
   * `null` on a page with no variant: the comment is then stored with no origin, and says so.
   */
  originVariantId?: string | null;
  /**
   * Show the COMPOSER (the global comment + the pin toggle). Off on a surface with no composition on
   * screen — the Spec tab's entry card reads the thread, it does not anchor into a page it is not
   * showing: pinning needs the render, and the render is on the page.
   */
  composer?: boolean;
  /**
   * FLUSH: drop the card chrome and the collapse, and let the list fill its host. That is the shape
   * inside the prototype page's right panel, which already IS the card and already owns the segment
   * that selects this pane (finding `01KYVMSMMBVSZNBBSBE18PRZAK` — no panel inside a panel).
   */
  flush?: boolean;
  /**
   * READ THE THREAD, WRITE NOTHING (finding `01KZ3JXB3SN8BMK7FS16PPSZ6T`). Consulting a PAST round
   * used to withdraw the whole right flank — Comments included — because both its panes AUTHOR and
   * one does not author into the past. The rule was right and the casualty was wrong: re-reading the
   * review thread WHILE looking at the round it produced is precisely what one opens a past round
   * for. So the thread comes back, stripped of every sink: no composer, no pin toggle, no `Resolve`.
   *
   * It is a structural absence, not a disabled control: with `readOnly` this component renders no
   * element bound to a `proto_comment_*` write at all, which is what makes "no mutation" provable by
   * what is on screen rather than by five `disabled` attributes.
   */
  readOnly?: boolean;
  /** Is the RENDER armed for pinning right now? The host owns the mode (it owns the frame). */
  pinMode?: boolean;
  /** Arm / disarm pin mode. Absent → no pin affordance (a host with no render to arm). */
  onTogglePinMode?: (next: boolean) => void;
  /**
   * A comment card is POINTED AT — its `node_key`, or `null` on leave (finding
   * `01KZ41ZKMW5SE7YSKW8XW4VKYD`). The host shows that node in the render. Only ANCHORED comments
   * raised on the variant `originVariantId` names ever fire it: designating an element of another
   * variant would point at a node that is not the one being discussed.
   *
   * Absent → the thread designates nothing (the Spec tab's card, which has no render to point into).
   */
  onHoverNode?: (nodeKey: string | null) => void;
}) {
  const prototypeId = tree.prototype.id;
  // THE PAGE'S OWN THREAD — the one reading every counter of this screen agrees with.
  const { open, resolved, unattributed, openCount } = scopeComments(tree, pageId);

  const [expanded, setExpanded] = useState(true);
  /**
   * THE RESOLVED BLOCK IS FOLDED (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`, point 3). A resolved comment
   * asks nothing of anybody: it took as much room as an open one, above the open ones, and pushed
   * the live thread out of the pane. Folded by default, with its COUNT on the header — which is the
   * whole of what a settled remark owes a reviewer until they ask for it.
   */
  const [resolvedOpen, setResolvedOpen] = useState(false);
  const [body, setBody] = useState("");

  // The summary counts what the FOLDED list holds — the page-less cards included, since the fold
  // hides those too.
  const orphanCount = [...open, ...unattributed].filter(
    (c) =>
      c.feedback.anchor_status === ANCHOR_STATUS_ORPHANED &&
      c.feedback.status === FEEDBACK_STATUS_OPEN,
  ).length;
  const canSubmit = body.trim().length > 0 && !actions.pending;

  // The GLOBAL comment: no `nodeKey`, which is what the backend already stores for a page-wide
  // remark. What is about ONE element goes through pin mode instead — on the element. The ORIGIN
  // travels either way: "page-wide" means about THIS page, in the variant that is on screen.
  const submit = (): void => {
    if (!canSubmit) return;
    void actions.addComment(prototypeId, body, null, originVariantId);
    setBody("");
  };

  const shown = flush || expanded;
  /**
   * Can a card designate its node in the render? Only where there IS a render to point into
   * (`onHoverNode`) and only for a comment raised on the VARIANT ON SCREEN. `originVariantId` is
   * exactly the variant the host is displaying — the same value every comment written from here
   * records — so this comparison is the whole rule, and it fails closed: with no variant on screen
   * nothing designates.
   */
  const designatesOn = (feedback: PrototypeFeedback): boolean =>
    originVariantId !== null && feedback.origin_variant_id === originVariantId;

  /** THE COMPOSER, written once and placed by the layout (in the scroller, or beside it). */
  const composerBox =
    composer && !readOnly ? (
      <div className="space-y-2 rounded-lg border border-border/50 bg-muted/20 p-2.5">
        <textarea
          data-testid="proto-comment-body"
          rows={2}
          placeholder="Comment on this page (review before dev)…"
          value={body}
          onChange={(e) => setBody(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") submit();
          }}
          className="w-full resize-y rounded-md border border-input bg-background px-2 py-1.5 text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
        <div className="flex flex-wrap items-center justify-end gap-1.5">
          {onTogglePinMode !== undefined ? (
            <Button
              size="xs"
              variant={pinMode ? "default" : "outline"}
              className="mr-auto"
              data-testid="proto-comment-pin-toggle"
              aria-pressed={pinMode}
              onClick={() => onTogglePinMode(!pinMode)}
            >
              <CrosshairIcon aria-hidden />
              {pinMode ? "Pinning" : "Pin on an element"}
            </Button>
          ) : null}
          <Button
            size="xs"
            data-testid="proto-comment-submit"
            disabled={!canSubmit}
            onClick={submit}
          >
            <MessageSquareIcon aria-hidden />
            Comment on the page
          </Button>
        </div>
      </div>
    ) : null;

  return (
    <section
      data-testid="proto-comments"
      aria-label="Pre-dev review comments"
      className={cn(
        // FLUSH: the thread OWNS its host's height, and splits it — the list scrolls, the composer
        // does not (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`, point 2). It used to be one scrolling
        // column with the box at the bottom of it, so the more comments a page had, the further one
        // scrolled to write the next one.
        flush
          ? "flex h-full min-h-0 flex-col"
          : "shrink-0 rounded-xl border border-border/60 bg-background",
      )}
    >
      {flush ? null : (
        <button
          type="button"
          data-testid="proto-comments-toggle"
          aria-expanded={expanded}
          onClick={() => setExpanded((v) => !v)}
          className="flex w-full items-center gap-2 px-3 py-2 text-left"
        >
          <MessageSquareIcon aria-hidden className="size-4 text-muted-foreground" />
          <span className="font-medium text-sm text-foreground">Comments</span>
          <span className="text-2xs text-muted-foreground/70">pre-dev review</span>
          {openCount > 0 ? (
            <Badge variant="muted" className="shrink-0">
              {openCount} open
            </Badge>
          ) : null}
          {orphanCount > 0 ? (
            <Badge variant="warning" className="shrink-0" data-testid="proto-comments-orphan-count">
              {orphanCount} to re-anchor
            </Badge>
          ) : null}
          <ChevronDownIcon
            aria-hidden
            className={cn(
              "ml-auto size-4 shrink-0 text-muted-foreground transition-transform",
              expanded && "rotate-180",
            )}
          />
        </button>
      )}

      {shown ? (
        <div
          className={cn(
            "space-y-3",
            flush
              ? "min-h-0 flex-1 overflow-y-auto px-3 py-3"
              : "max-h-72 overflow-y-auto border-border/60 border-t px-3 py-3",
          )}
          data-testid={flush ? "proto-comments-scroll" : undefined}
        >
          {/* NO "N to re-anchor" TALLY over an OPEN list: every orphaned card below carries its own
              `To re-anchor` badge, so the count would be the same fact, counted. It survives on the
              COLLAPSIBLE header above, where the list is folded away and a summary is the only
              thing there is to read. */}

          {/* OPEN comments — resolvable. Orphaned ones surface distinctly (handled in the card). */}
          {open.length > 0 ? (
            <ul className="space-y-2">
              {open.map((c) => (
                <li key={c.feedback.id}>
                  <CommentCard
                    feedback={c.feedback}
                    origin={c.origin}
                    showPage={pageId === null}
                    pending={actions.pending}
                    onResolve={readOnly ? undefined : (id) => void actions.resolveComment(id)}
                    onHoverNode={onHoverNode}
                    designates={designatesOn(c.feedback)}
                  />
                </li>
              ))}
            </ul>
          ) : null}

          {/* RESOLVED comments — a read-only record (the design was corrected before election),
              FOLDED, with its count on the summary. They used to sit expanded above the open ones,
              taking the same room while asking for nothing. */}
          {resolved.length > 0 ? (
            <div className="space-y-2">
              <button
                type="button"
                data-testid="proto-comments-resolved-toggle"
                aria-expanded={resolvedOpen}
                onClick={() => setResolvedOpen((v) => !v)}
                className="flex w-full items-center gap-1.5 rounded-md py-0.5 text-left font-medium text-2xs uppercase tracking-wide text-muted-foreground/70 hover:text-foreground"
              >
                <ChevronDownIcon
                  aria-hidden
                  className={cn(
                    "size-3.5 shrink-0 transition-transform",
                    resolvedOpen ? "rotate-0" : "-rotate-90",
                  )}
                />
                Resolved · {resolved.length}
              </button>
              {resolvedOpen ? (
                <ul className="space-y-2" data-testid="proto-comments-resolved-list">
                  {resolved.map((c) => (
                    <li key={c.feedback.id}>
                      <CommentCard
                        feedback={c.feedback}
                        origin={c.origin}
                        showPage={pageId === null}
                        pending={actions.pending}
                        onHoverNode={onHoverNode}
                        designates={designatesOn(c.feedback)}
                      />
                    </li>
                  ))}
                </ul>
              ) : null}
            </div>
          ) : null}

          {open.length === 0 && resolved.length === 0 ? (
            <p
              data-testid="proto-comments-empty"
              className="py-2 text-center text-xs text-muted-foreground"
            >
              {/* A thread that takes no write does not invite one: telling a reader to pin a remark
                  on a round they are only consulting would name a gesture this surface withholds. */}
              {readOnly
                ? "No comment was raised on this page."
                : "No comment on this page yet. Pin a remark on an element, or comment on the page, before dev."}
            </p>
          ) : null}

          {/* COMMENTS THAT BELONG TO NO PAGE — written before the origin was recorded, or by a tool
              that omitted it. They are shown APART, under their own heading, on every page: folding
              them into the page on screen would attribute them to a page nobody chose, which is the
              exact defect this scoping exists to end. */}
          {unattributed.length > 0 ? (
            <div className="space-y-2" data-testid="proto-comments-unattributed">
              <span className="font-medium text-2xs uppercase tracking-wide text-muted-foreground/70">
                No page recorded · {unattributed.length}
              </span>
              <ul className="space-y-2">
                {unattributed.map((c) => (
                  <li key={c.feedback.id}>
                    <CommentCard
                      feedback={c.feedback}
                      origin={null}
                      pending={actions.pending}
                      onResolve={
                        c.feedback.status === FEEDBACK_STATUS_OPEN && !readOnly
                          ? (id) => void actions.resolveComment(id)
                          : undefined
                      }
                    />
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          {/* THE BOX TO ADD ONE IS NOT IN THIS SCROLLER when the thread fills a panel — see below. */}
          {flush ? null : composerBox}
        </div>
      ) : null}

      {/* …AND THE BOX TO ADD ONE, LAST — AND ALWAYS VISIBLE (finding `01KZ41ZKMW5SE7YSKW8XW4VKYD`,
          point 2). It is a SIBLING of the scroller, not its last child: the list takes the height
          that is left and scrolls inside it, the composer keeps its own. The thread still reads
          downwards — comments first, the box to add one last — it simply stops walking away as the
          thread grows.

          Two gestures, side by side as the two verbs they are: `Pin` arms the RENDER (the composer
          then opens ON the element), `Comment on the page` posts what the textarea holds. */}
      {flush && shown ? <div className="shrink-0 px-3 pb-3">{composerBox}</div> : null}
    </section>
  );
}
