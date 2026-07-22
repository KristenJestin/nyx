import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { statusMeta as specStatusMeta } from "@/components/specs/spec-meta";
import { Dot } from "@/components/specs/spec-primitives";

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
 */
export function IdeaRow({
  item,
  selected,
  onSelect,
  showTheme = false,
}: {
  item: IdeaListItem;
  selected: boolean;
  onSelect: () => void;
  /** FLAT mode: render the leading Theme cell (the by-theme grouping is off). */
  showTheme?: boolean;
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
      <span
        className={cn(
          "min-w-0 flex-1 truncate text-sm text-foreground",
          meta.closed && "text-muted-foreground line-through decoration-muted-foreground/40",
        )}
      >
        {item.title}
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
