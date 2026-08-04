import { useNavigate } from "@tanstack/react-router";
import { ArrowUpRightIcon, LayoutTemplateIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";

import { MicroLabel } from "../specs/spec-primitives";
import { panelPagesOf } from "../specs/proto-page-model";
import { useProtoTree } from "../specs/use-proto-tree";

/**
 * `<OverlayPrototypes>` — the Spec's PROTOTYPE PAGES, listed inside the open overlay panel
 * (finding `01KYW342EE5DN6F70YB8VSTJR8`, point 1).
 *
 * # Why it exists
 *
 * The overlay is what a user reads while an agent works: it carried the Spec, its tasks and two
 * jumps ([`OverlayPanelActions`]), and NOTHING about the prototype. Reaching a prototype page from
 * there meant leaving the overlay, opening the Spec, finding the Prototype tab and then the page —
 * four gestures to look at the thing the agent is drawing. The pages are listed HERE, and each one
 * is a single click onto itself.
 *
 * # Where the click lands
 *
 * On the PAGE, never in the overlay. The Prototype tab became an ENTRY CARD and the workbench moved
 * to its own route (`/work/$specId/prototype`) because a prototype stands for a real web page and a
 * 380 px panel is even less the size of one than a tab was. So this list opens the WORKBENCH, on the
 * page that was clicked — the route's own `?page=<slug>` deep-link, the same one the tab's contact
 * sheet uses. It never mounts a stage of its own.
 *
 * # What it says, and what it stays silent about
 *
 * One row per page: its title, its slug (the deep-link's own key), whether it still owes an
 * arbitration and how many open comments are anchored in it — the same per-page summary the two
 * prototype surfaces already derive (`panelPagesOf`), so the three never diverge. NO miniature: a
 * thumbnail is a `design_render` per page, and this panel rides over a live terminal.
 *
 * A Spec with no prototype (or a read that failed) renders NOTHING AT ALL — the overlay is a
 * surcouche over a terminal, and an empty block claiming an absence would be chrome charged to every
 * Spec that has no prototype. That is also why the whole thing is only ever mounted inside an OPEN
 * panel: a closed languette costs no `proto_tree`.
 */
export function OverlayPrototypes({ specId }: { specId: string }) {
  const navigate = useNavigate();
  const { tree } = useProtoTree(specId);

  // No prototype, a failed read, or a prototype with no page yet → nothing. (`useProtoTree` answers
  // `null` for a Spec with no prototype: a valid state, not an error.)
  if (tree === null || tree.pages.length === 0) return null;

  const pages = panelPagesOf(tree);

  return (
    <section data-testid="overlay-prototype-pages" aria-label="Prototype pages">
      <div className="mb-2 flex items-center gap-1.5 px-1">
        <LayoutTemplateIcon className="size-3 shrink-0 text-muted-foreground" />
        <MicroLabel>Prototype</MicroLabel>
        <MicroLabel className="text-muted-foreground/60">
          {pages.length} {pages.length === 1 ? "page" : "pages"}
        </MicroLabel>
      </div>

      <div className="flex flex-col gap-1">
        {pages.map((page) => (
          <button
            key={page.id}
            type="button"
            data-testid={`overlay-proto-page-${page.slug}`}
            // The DEEP-LINK, not a re-opened scene: the workbench route, on THIS page.
            // `stopPropagation` for the same reason as the header actions — a jump must never be
            // read as a toggle of the panel it was launched from.
            onClick={(e) => {
              e.stopPropagation();
              void navigate({
                to: "/work/$specId/prototype",
                params: { specId },
                search: { page: page.slug },
              });
            }}
            className="group flex w-full items-center gap-2 rounded-md border border-border/60 bg-muted/30 px-2 py-1.5 text-left transition-colors hover:border-border hover:bg-accent/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
          >
            <span className="min-w-0 flex-1">
              <span className="block truncate text-xs font-medium text-foreground">
                {page.title}
              </span>
              <span className="block truncate font-mono text-2xs text-muted-foreground">
                {page.slug}
              </span>
            </span>
            {/* What the page still OWES, in the same words the two prototype surfaces use. */}
            {!page.retained ? (
              <Badge variant="warning" className="shrink-0">
                to elect
              </Badge>
            ) : null}
            {page.openCommentCount > 0 ? (
              <Badge variant="info" className="shrink-0">
                {page.openCommentCount}
              </Badge>
            ) : null}
            <ArrowUpRightIcon className="size-3.5 shrink-0 text-muted-foreground/60 transition-colors group-hover:text-foreground" />
          </button>
        ))}
      </div>
    </section>
  );
}
