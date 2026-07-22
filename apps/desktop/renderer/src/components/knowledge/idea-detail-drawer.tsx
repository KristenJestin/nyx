import { ArrowUpRightIcon, XIcon } from "lucide-react";
import { Link } from "@tanstack/react-router";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { MicroLabel } from "@/components/ui/micro-label";
import { specKey, statusMeta as specStatusMeta } from "@/components/specs/spec-meta";
import { Dot } from "@/components/specs/spec-primitives";

import { formatShortDate, ideaStatusMeta, themeTone } from "./idea-meta";
import { UNGROUPED_THEME } from "./types";
import type { IdeaListItem } from "./types";

/**
 * `<IdeaDetailDrawer>` — the idea DETAIL drawer (PRD "détail idée en drawer"): the idea's theme +
 * status chips and a close control, its title (struck through when closed), a meta row, the full
 * body ("corps"), the linked Specs with their status ("Specs liées"), and — for a closed idea — the
 * closure motif ("motif de fermeture"). All three required sections render from the enriched
 * `IdeaListItem` (no second round-trip). Built on the shared `<Badge>`/`<Dot>`/`<MicroLabel>`
 * primitives + tokens — the prototype's `detail` placement, none of its pixels.
 */
export function IdeaDetailDrawer({ item, onClose }: { item: IdeaListItem; onClose: () => void }) {
  const meta = ideaStatusMeta(item.status);
  const themeLabel = item.theme ?? UNGROUPED_THEME;
  return (
    <aside
      aria-label="Idea detail"
      className="flex min-h-0 w-full flex-col border-l border-border bg-card/60"
    >
      <header className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-3">
        {item.theme != null ? (
          <span className="inline-flex items-center gap-1.5 text-2xs font-medium text-muted-foreground">
            <Dot tone={themeTone(item.theme)} size="xs" />
            {themeLabel}
          </span>
        ) : (
          <span className="text-2xs font-medium text-muted-foreground">{themeLabel}</span>
        )}
        <Badge variant={meta.variant}>{meta.label}</Badge>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close idea detail"
          className="ml-auto grid size-6 place-items-center rounded-md text-muted-foreground outline-none transition-colors hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          <XIcon className="size-4" />
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        <h2
          className={cn(
            "font-heading text-base font-semibold leading-snug tracking-tight text-foreground",
            meta.closed && "text-muted-foreground line-through decoration-muted-foreground/40",
          )}
        >
          {item.title}
        </h2>

        <dl className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 rounded-lg border border-border/60 bg-card px-3 py-2 text-2xs text-muted-foreground">
          <div className="flex items-center gap-1.5">
            <Dot tone={meta.dot} size="xs" />
            <dt className="sr-only">Status</dt>
            <dd className="font-medium text-foreground/80">{meta.label}</dd>
          </div>
          <div className="flex items-center gap-1.5">
            <dt>Theme</dt>
            <dd className="font-medium text-foreground/80">{themeLabel}</dd>
          </div>
          <div className="flex items-center gap-1.5">
            <dt>Captured</dt>
            <dd className="font-medium text-foreground/80">{item.source}</dd>
          </div>
          {item.created_at ? (
            <div className="ml-auto font-mono">{formatShortDate(item.created_at)}</div>
          ) : null}
        </dl>

        {item.body.trim() !== "" ? (
          <p className="mt-4 whitespace-pre-wrap text-sm leading-relaxed text-foreground/80">
            {item.body}
          </p>
        ) : null}

        {item.linked_specs.length > 0 ? (
          <section className="mt-5">
            <MicroLabel>Linked specs</MicroLabel>
            <ul className="mt-2 flex flex-col gap-1.5">
              {item.linked_specs.map((spec) => {
                const sm = specStatusMeta(spec.status);
                return (
                  <li key={spec.spec_id}>
                    {/* The whole row is a router Link to the Spec's detail (`/work/$specId`) — same
                        pattern as `<SpecCardView>`; the ↗ affordance is now live. */}
                    <Link
                      to="/work/$specId"
                      params={{ specId: spec.spec_id }}
                      title={`Open ${spec.title}`}
                      className="group flex items-center gap-2.5 rounded-lg border border-border/60 bg-card px-3 py-2 outline-none transition-colors hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <Dot tone={sm.dot} size="sm" />
                      <span className="min-w-0 flex-1 truncate text-sm text-foreground/80 group-hover:text-foreground">
                        {spec.title}
                      </span>
                      <span className="font-mono text-2xs text-muted-foreground">
                        {specKey("", spec.number)}
                      </span>
                      <Badge variant={sm.variant}>{sm.label}</Badge>
                      <ArrowUpRightIcon
                        aria-hidden
                        className="size-3.5 text-muted-foreground group-hover:text-foreground"
                      />
                    </Link>
                  </li>
                );
              })}
            </ul>
          </section>
        ) : null}

        {item.closed_reason != null && item.closed_reason.trim() !== "" ? (
          <section className="mt-5">
            <MicroLabel>Closure reason</MicroLabel>
            <p className="mt-2 flex items-center gap-2.5 rounded-lg border border-border/60 bg-card px-3 py-2 text-sm italic text-foreground/80">
              <Dot tone={meta.dot} size="sm" />
              {item.closed_reason}
            </p>
          </section>
        ) : null}
      </div>
    </aside>
  );
}
