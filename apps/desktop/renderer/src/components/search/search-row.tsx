import { cn } from "@/lib/utils";
import type { GlobalSearchHit } from "@/bridge/contract";
import { agentProviderFor } from "@/components/sidebar/agent-providers";

import { SearchContextBlock } from "./search-context-block";
import { SEARCH_KIND_META } from "./search-meta";

/**
 * `<SearchRow>` — ONE line of the palette, and the ONLY one.
 *
 * The suggestions and the results are drawn by this same component, because the backend answers the
 * same `SearchHit` for both: a single `global_search` call serves "just opened" and "typed
 * something", so a second row component would be a second truth about what a hit looks like. The
 * only difference the two states carry is the `time` a suggestion shows — the recency it is ORDERED
 * by, which a ranked result is not.
 *
 * THE CONTEXT IS NOT DECORATION. The search spans every project at once, so two Specs called "Auth"
 * from two projects are indistinguishable without the project — and activating one of them MOVES
 * the entered project durably. That block — determinant, then the project closing the line — is
 * drawn by {@link SearchContextBlock}, which the ACTION row renders from the same hit; what it
 * says and why it is ordered that way is written there, once.
 *
 * The row's own job is to place it at ONE stable place — the right edge — so every title still
 * begins at the same abscissa, and the block never competes with the `sm` full-contrast title,
 * which stays the dominant element of the line.
 *
 * An AGENT SESSION draws its provider's logo in place of the generic terminal glyph, from the one
 * registry the sidebar already reads (`sidebar/agent-providers`). Now that the results are one list
 * with no section headings, that leading glyph is the whole of what says which type a line is — and
 * on a session, which agent. Where the logo IS drawn it also takes the agent's name OUT of the
 * secondary line: the icon has already answered "which agent", and the line is then free for the
 * cwd. Where it is NOT — an unknown `agent_kind`, generic glyph — the name stays, because there it
 * is the only thing left saying what occupies the session.
 *
 * It is a `role="option"` inside the palette's listbox, not a `<button>`: the keyboard focus never
 * leaves the query input (one types and walks the list at the same time), so activation is driven
 * by the input's own key handling plus this row's pointer events, and `aria-selected` is what says
 * which line `↵` would open. `onMouseDown` is cancelled so a click never steals the focus the input
 * must keep.
 */
export function SearchRow({
  id,
  hit,
  active,
  time,
  onActivate,
  onHover,
}: {
  /** DOM id — the input points `aria-activedescendant` at the active row's. */
  id: string;
  hit: GlobalSearchHit;
  active: boolean;
  /** The relative recency (`now` / `Nm` / `Nh` / `Yest.` / `MMM D`) — suggestions only. */
  time?: string;
  onActivate: () => void;
  /**
   * ARM this row from the mouse — absent where hovering may not arm anything. The empty state is
   * exactly that place: nothing is preselected there on purpose, so a row that armed itself under a
   * drifting cursor would hand a reflex `↵` a target the keyboard never chose. A CLICK still
   * activates in every state; it is an explicit act, a mouse crossing the palette is not.
   */
  onHover?: () => void;
}) {
  // THE GLYPH SAYS THE TYPE — and on a SESSION, the type is not the whole answer: a session IS the
  // agent that occupies it, and that is what departs it from the next session at a glance, far more
  // surely than a terminal label. So the seventh kind draws its PROVIDER's logo where the other six
  // draw their kind's glyph.
  //
  // The mapping is `sidebar/agent-providers`, consumed as-is: it is already the single generic
  // `agent_kind → { icon, label }` registry (the sidebar rows read the same one), and a second table
  // here would be two truths about one thing. Its resolver is total — an unknown or absent kind
  // gives `undefined` and the generic terminal glyph stands, same box, same size, no layout shift.
  //
  // `detail` IS the `agent_kind` on this kind (`db::recent_search_sessions` aliases it), which is
  // why nothing had to be added to the wire hit.
  const meta = SEARCH_KIND_META[hit.kind];
  const provider = hit.kind === "session" ? agentProviderFor(hit.detail) : undefined;
  const Icon = provider?.icon ?? meta.icon;
  // The secondary line: what the source could say about the hit beyond its name — a command line or
  // an agent kind (`detail`), the FTS match window or a working directory (`extract`). Both when
  // both exist, neither when the source has nothing.
  //
  // ONE EXCEPTION, AND IT BELONGS TO THE LOGO. When a session RESOLVED a provider, its `detail` is
  // dropped from this line: `detail` on that kind is the raw `agent_kind` (`claude_code`) — a
  // technical identifier, not a label written for a human — and the glyph two columns to the left
  // already says which agent occupies the session. Drawn as well, it spent the row's ONE secondary
  // line restating the icon, in place of the cwd, which is the only other thing a session has to
  // say about itself.
  //
  // The removal is conditioned on the LOGO, never on the kind. An `agent_kind` the registry does
  // not know yet falls back to the generic terminal glyph, and there the agent's name is the ONLY
  // trace of what occupies the session — so it stays. No logo, no removal.
  //
  // Nothing changes BACKEND-side: `db::recent_search_sessions` keeps aliasing `agent_kind` into
  // `detail`, which is exactly what makes the fallback above possible — and what the aggregator
  // matches on.
  const secondary = [provider ? null : hit.detail, hit.extract].filter(
    (s): s is string => !!s && s.length > 0,
  );
  return (
    <div
      id={id}
      role="option"
      aria-selected={active}
      data-testid={`search-row-${hit.kind}-${hit.id}`}
      onMouseDown={(e) => e.preventDefault()}
      onClick={onActivate}
      onMouseMove={onHover}
      className={cn(
        "flex w-full cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 text-left",
        "transition-colors",
        // THE ACTIVE STATE IS THE FILL, and only the fill. It used to add a `primary` bar down the
        // left edge — a strong accent on a line that already carries a glyph, a title, a secondary
        // line, a key and a project. The fill is at FULL `muted` rather than the `muted/60` it had
        // under the bar, which is the app's own selected-line strength (`ui/sidebar-line`, where
        // `muted/60` is what HOVER means) — the state had to stay unmistakable once the accent left.
        active && "bg-muted",
      )}
    >
      {/* The glyph is decorative, but the LOGO is named — `title` on the wrapper, exactly as the
          sidebar row labels the same swap (`sidebar/terminal-item`), so "which agent is this"
          survives for a pointer and for assistive tech. Absent on the generic glyphs. */}
      <span className="flex shrink-0 items-center" title={provider?.label}>
        <Icon aria-hidden className="size-4 text-muted-foreground" />
      </span>
      <span className="flex min-w-0 flex-1 flex-col">
        {/* THE TITLE, and nothing before it — every row's starts at the same x. */}
        <span className="truncate text-sm text-foreground">{hit.label}</span>
        {secondary.length > 0 ? (
          <span className="truncate text-2xs text-muted-foreground/80">
            {secondary.join(" · ")}
          </span>
        ) : null}
      </span>
      {/* `determinant · [tile] Project` — the SAME block the action lines hanging under this row
          draw, from the same hit, so a line and its affordances say where they come from in one
          voice. Its content, its order and its type live in `./search-context-block`. */}
      <SearchContextBlock
        hit={hit}
        testId={`search-row-context-${hit.kind}-${hit.id}`}
        tileTestId={`search-row-tile-${hit.kind}-${hit.id}`}
      />
      {time ? (
        <span className="w-12 shrink-0 text-right font-mono text-2xs text-muted-foreground/70">
          {time}
        </span>
      ) : null}
    </div>
  );
}
