import type { ReactNode } from "react";

import { cn } from "@/lib/utils";
import { EmptyState } from "@/components/ui/empty-state";
import { MicroLabel } from "@/components/ui/micro-label";

/**
 * `<HomePanel>` — one of the home's FOUR blocks (Agent sessions · Recent specs · Services ·
 * Terminals), and the only container any of them gets.
 *
 * Placement (elected round v11, both pages): a header holding the block's name on the left and,
 * optionally, one muted NOTE on the right saying what the block shows — never a link, never a
 * second door onto a page the rail already reaches. Then the body: the rows, or the block's own
 * empty state.
 *
 * The four blocks are the same component so the two homes cannot drift into two panel shapes.
 *
 * ## Why the title is `<MicroLabel>` and the shell is NOT `<SectionPanel>`
 *
 * The TITLE is the design system's own token and nothing else: `<MicroLabel>` is the app's dense
 * uppercase mono label (16 consumers), so the home's four headers cannot be the only ones in the
 * repo carrying their own tracking. It is wrapped in an `<h2>` because these four ARE the page's
 * section headings — the element is the semantics, the span inside is the type.
 *
 * `<SectionPanel>` is the wrong shell for these, and not by a hair: it draws a TINTED, DIVIDED
 * header strip (`border-b bg-muted/40`) sized for a panel that holds a list under a banner. The
 * home's block is the opposite reading — an untinted, undivided header that lets the four blocks
 * sit on one page as four quiet cards rather than four banners. It also has no slot for this
 * header's two extras: the muted `note` on the right (a qualifier, NOT `SectionPanel`'s trailing
 * ACTION slot — the home puts no control there) and the `aria-label` that names the region.
 * Composing it and then overriding its header's tint, its divider, its padding and its trailing
 * slot would leave nothing of it standing.
 */
export function HomePanel({
  title,
  note,
  children,
  className,
}: {
  /** The block's name — the header's left slot. */
  title: string;
  /** The muted qualifier on the right (`live runtime`, `shells only`, …). Optional. */
  note?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section
      aria-label={title}
      className={cn("rounded-xl border border-border/70 bg-card", className)}
    >
      <header className="flex items-center justify-between gap-3 px-3 pt-3 pb-1">
        <h2>
          <MicroLabel>{title}</MicroLabel>
        </h2>
        {note ? <span className="text-2xs text-muted-foreground/70">{note}</span> : null}
      </header>
      <div className="p-1.5">{children}</div>
    </section>
  );
}

/**
 * The EMPTY state of a block — a headline and one line saying what would put something here.
 *
 * It is a state of the block, drawn inside it, not a page-level takeover: a home with three full
 * blocks and one empty one has to read as a home, not as an error. The dashed card is the app's
 * existing "nothing here" treatment and it is COMPOSED, not restated: `<EmptyState>` (9 consumers)
 * owns the border, the radius and the muted centring. What is local is only the CONTENT — two
 * lines instead of one, because the home's empty block has to say both what is missing and what
 * would fill it — and the tighter box that two lines need inside a small panel.
 */
export function HomeEmpty({ title, hint }: { title: string; hint: string }) {
  return (
    <EmptyState className="px-4 py-6">
      <strong className="block text-sm font-medium text-muted-foreground">{title}</strong>
      <span className="mt-0.5 block text-2xs text-muted-foreground/70">{hint}</span>
    </EmptyState>
  );
}
