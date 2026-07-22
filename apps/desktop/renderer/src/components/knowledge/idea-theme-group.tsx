import type { ReactNode } from "react";
import { ChevronRightIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Dot } from "@/components/specs/spec-primitives";

import { IdeaStatusDots, type IdeaStatusCounts } from "./idea-status-dots";

/**
 * `<IdeaThemeGroup>` — a COLLAPSIBLE, ANIMATED theme group of the Idées table (PRD "groupes par
 * thème collapsables animés"). The header is a button (chevron + theme dot + name + count pill +
 * status mini-dots) that toggles the body; the body animates OPEN/CLOSED via a
 * `grid-template-rows: 0fr → 1fr` transition (the same technique the prototype validated, expressed
 * as design-system utilities — no fixed pixel height, so it animates ANY row count smoothly). The
 * chevron rotates 90° when open. Placement from the prototype's `grp`; look from the tokens.
 */
export function IdeaThemeGroup({
  label,
  tone,
  count,
  counts,
  open,
  onToggle,
  children,
}: {
  /** The theme name (or the "Ungrouped" sentinel). */
  label: string;
  /** The theme's design-system dot tone (`bg-*`). */
  tone: string;
  /** The count shown in the header pill (total, or the visible count while filtering). */
  count: number;
  /** The per-status distribution for the mini-dots. */
  counts: IdeaStatusCounts;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div className="border-b border-border/40">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className="flex w-full items-center gap-2.5 px-4 py-2.5 text-left outline-none transition-colors hover:bg-muted/30 focus-visible:bg-muted/30 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
      >
        <ChevronRightIcon
          aria-hidden
          className={cn(
            "size-4 shrink-0 text-muted-foreground transition-transform duration-200",
            open && "rotate-90",
          )}
        />
        <Dot tone={tone} size="sm" />
        <span className="text-sm font-semibold tracking-tight text-foreground">{label}</span>
        <span className="rounded-full bg-muted px-1.5 font-mono text-2xs text-muted-foreground">
          {count}
        </span>
        <IdeaStatusDots counts={counts} className="ml-auto" />
      </button>

      {/* The animated collapse: an outer grid whose single row track animates 0fr↔1fr, with an
          `overflow-hidden` inner wrapper so the rows clip as it closes. */}
      <div
        className={cn(
          "grid transition-[grid-template-rows] duration-300 ease-out",
          open ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
        )}
      >
        <div className="overflow-hidden">{children}</div>
      </div>
    </div>
  );
}
