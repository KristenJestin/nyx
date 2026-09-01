import { cn } from "@/lib/utils";
import type { GlobalSearchHit } from "@/bridge/contract";

import type { SearchAction } from "./search-actions";
import { SearchContextBlock } from "./search-context-block";

/**
 * `<SearchActionRow>` — one AFFORDANCE line: something one can do to the entity drawn just above.
 *
 * It is a sibling of `<SearchRow>`, not a mode of it. The two draw different species: a hit is a
 * thing that EXISTS and carries an identity, a time; an action is something one may DO, and has
 * neither. Folding both into one component would have meant a row that renders half of itself
 * conditionally, and a `hit` prop that is sometimes not a hit.
 *
 * IT IS NEVERTHELESS A LINE LIKE THE OTHERS, AND THAT IS THE POINT. It used to be drawn one step
 * under a title (`xs`, `muted-foreground` against the hit's `sm` at full foreground) and with no
 * context at all — the user's « pourquoi ça a pas la même gueule que le reste ? on a pas le projet,
 * le workspace, le texte est muted ». A derived line is not a lesser line: it sits between two
 * full-strength entities and, drawn dimmer and shorter, it read as a caption of the row above
 * rather than as something one may run. So it now carries the SAME `sm` full-foreground label as a
 * title and the SAME context block, rendered by {@link SearchContextBlock} from the very hit it
 * derives from, so an action says where it will act exactly as its entity says where it lives.
 *
 * WHAT MAKES IT READ AS AN ACTION is therefore the ICON and the VERB, and nothing else:
 *  - the GLYPH is a `+` — a creation, which no kind's glyph in `search-meta` is;
 *  - the LABEL starts with a verb (`Open a new terminal`), which no entity's name does.
 *
 * A typographic downgrade is not allowed to carry that distinction — it costs legibility to say
 * something two unmistakable signals already say.
 *
 * THE LABEL NO LONGER NAMES ITS TARGET. `Open a new terminal in audio player` became `Open a new
 * terminal`, because `audio player · [M] Music Manager` is now read at the right edge of the same
 * line. It is the rule the agent label already follows on a session row — where the provider's logo
 * is drawn, the agent's name leaves the text — and it holds here for the same reason: one
 * information is not said twice on one line.
 *
 * NO TIME. A suggestion's relative time is the recency it is ORDERED by; an action is ordered by
 * the entity it hangs under, and there are no actions in the suggestions at all.
 *
 * WHAT MAKES IT BEHAVE LIKE EVERY OTHER LINE: it is a `role="option"` of the same listbox, at the
 * same box geometry, walked by the same `↑` `↓`, activated by the same `↵`, armed by the same
 * hover — the palette's row list is ONE sequence with the actions expanded into it, so nothing
 * about the keyboard changes because a line happens to be an action.
 */
export function SearchActionRow({
  id,
  hit,
  action,
  active,
  onActivate,
  onHover,
}: {
  /** DOM id — the input points `aria-activedescendant` at the active row's. */
  id: string;
  /** The entity this action derives from — it supplies the context the action is drawn with. */
  hit: GlobalSearchHit;
  action: SearchAction;
  active: boolean;
  onActivate: () => void;
  onHover?: () => void;
}) {
  const Icon = action.icon;
  return (
    <div
      id={id}
      role="option"
      aria-selected={active}
      data-testid={`search-action-${action.id}-${hit.id}`}
      // Cancelled for the same reason as on a hit row: a click must never take the focus the query
      // input has to keep.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onActivate}
      onMouseMove={onHover}
      className={cn(
        "flex w-full cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 text-left",
        "transition-colors",
        active && "bg-muted",
      )}
    >
      <span className="flex shrink-0 items-center">
        <Icon aria-hidden className="size-4 text-muted-foreground" />
      </span>
      {/* The same column a hit row draws its title in — one child instead of two, since an action
          has no secondary line — so the label starts at the very abscissa every title starts at. */}
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate text-sm text-foreground">{action.label}</span>
      </span>
      {/* THE CONTEXT OF THE ENTITY, inherited whole: same component, same hit, same order. The test
          ids differ from the entity's because both blocks are on screen at once — two nodes may not
          answer to one id. */}
      <SearchContextBlock
        hit={hit}
        testId={`search-action-context-${action.id}-${hit.id}`}
        tileTestId={`search-action-tile-${action.id}-${hit.id}`}
      />
    </div>
  );
}
