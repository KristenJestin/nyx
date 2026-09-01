import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { statusMeta as specStatusMeta } from "@/components/specs/spec-meta";
import { Dot } from "@/components/specs/spec-primitives";
import { ProjectMark } from "@/components/sidebar/project-mark";

import { formatShortDate, ideaStatusMeta, themeTone } from "./idea-meta";
import { UNGROUPED_THEME, type IdeaListItem } from "./types";

/**
 * `<IdeaRow>` — one dense row of the Idées table: the title (struck through when closed), the
 * linked-Spec badges, the status chip, and the capture date. In FLAT mode (`showTheme`) the row
 * gains a leading Theme cell so the theme stays legible without group headers. The whole row is a
 * button that opens the idea in the detail drawer (`selected` draws the left accent + tint). The
 * status is encoded ONCE, by the right-side badge — there is no redundant leading status dot (the
 * group-header mini-dots own the aggregate distribution). Built against the design system (the
 * shared `<Dot>` / `<Badge>` primitives + tokens) — the prototype's `trow` placement, none of its
 * pixels.
 *
 * ## THE PROJECT MARK (`showProject`)
 *
 * In the CROSS-PROJECT scope the table lists every project's ideas at once, and an idea has no
 * human key to name its project with (unlike a Spec card's `BETA-1`) — so the mark is the ONLY
 * thing saying whose row it is. It is `<ProjectMark labelled>`, the app's one project identity
 * (`components/sidebar/project-mark.tsx`), never a square drawn here.
 *
 * PLACEMENT, per that component's rule: it rides INSIDE the title cell, immediately before the
 * title — NOT in a column of its own. The row's anatomy is a single line of text, so the mark
 * reads as part of the name rather than as a new field, and nothing shifts in the project scope,
 * where it simply is not rendered. An idea whose project row is gone carries no mark and stays in
 * the flow.
 *
 * ITS SIZE IS NOT the row's text size, and that is the correction. It was `size-4` at the text
 * size and the user did not see it at all (« pareil dans Ideas » → « je l'ai pas du tout vu »).
 * The mark was RENDERING — checked in the real app on the user's own database, every row carried
 * one — it simply did not read as one: a 16px neutral tile at 12px type, flat against a dense row,
 * is a bullet. The homes' mark is SMALLER (`size-3.5`) and is read effortlessly because it is
 * DETACHED — a badge riding a glyph, ringed in the surface behind it. This row has no glyph to
 * ride, so it buys the same separation the other way: `size-5` with a hairline ring, which lifts
 * it off the row without adding a field or moving the title.
 *
 * The size lives HERE, in the caller's `className`, exactly as `<ProjectMark>`'s contract says it
 * must — so this correction cannot reach the rail tile, the home badge or the Spec card.
 */
export function IdeaRow({
  item,
  selected,
  onSelect,
  showTheme = false,
  showProject = false,
}: {
  item: IdeaListItem;
  selected: boolean;
  onSelect: () => void;
  /** FLAT mode: render the leading Theme cell (the by-theme grouping is off). */
  showTheme?: boolean;
  /** CROSS-PROJECT scope: name each row's project with its mark. */
  showProject?: boolean;
}) {
  const meta = ideaStatusMeta(item.status);
  const themeLabel = item.theme ?? UNGROUPED_THEME;
  const extraSpecs = item.linked_specs.length - 2;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={selected ? "true" : undefined}
      data-testid={`idea-row-${item.id}`}
      className={cn(
        "flex w-full items-center gap-3 border-t border-l-2 border-t-border/40 border-l-transparent px-4 py-2 text-left outline-none transition-colors",
        "hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
        selected && "border-l-primary bg-muted/60",
      )}
    >
      {showTheme ? (
        <span className="flex w-32 shrink-0 items-center gap-1.5 text-2xs font-medium text-muted-foreground">
          <Dot tone={themeTone(themeLabel)} size="xs" />
          <span className="truncate">{themeLabel}</span>
        </span>
      ) : null}
      <span className="flex min-w-0 flex-1 items-center gap-2">
        {showProject && item.project ? (
          <ProjectMark
            project={item.project}
            labelled
            data-testid="idea-project-mark"
            className="size-5 shrink-0 rounded-md ring-1 ring-border"
          />
        ) : null}
        <span
          data-testid="idea-title"
          className={cn(
            "min-w-0 flex-1 truncate text-sm text-foreground",
            meta.closed && "text-muted-foreground line-through decoration-muted-foreground/40",
          )}
        >
          {item.title}
        </span>
      </span>

      {item.linked_specs.length > 0 ? (
        <span className="hidden shrink-0 items-center gap-1 md:flex">
          {item.linked_specs.slice(0, 2).map((spec) => (
            <Badge
              key={spec.spec_id}
              variant={specStatusMeta(spec.status).variant}
              className="max-w-[9rem]"
              title={`${spec.title} · ${specStatusMeta(spec.status).label}`}
            >
              <span className="truncate">{spec.title}</span>
            </Badge>
          ))}
          {extraSpecs > 0 ? <Badge variant="muted">+{extraSpecs}</Badge> : null}
        </span>
      ) : null}

      <Badge variant={meta.variant} className="shrink-0">
        {meta.label}
      </Badge>

      <span className="hidden w-16 shrink-0 text-right font-mono text-2xs text-muted-foreground lg:block">
        {formatShortDate(item.created_at)}
      </span>
    </button>
  );
}
