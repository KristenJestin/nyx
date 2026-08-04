import { ScrollTextIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";

import type { SpecDetail, SpecRevision } from "../types";

/** One structured section of the revision: a heading + its markdown body (as plain text). */
function Section({ heading, body }: { heading: string; body: string }) {
  const trimmed = body.trim();
  if (trimmed.length === 0) return null;
  return (
    <section>
      <h2 className="mb-3 font-heading text-base font-semibold tracking-tight text-foreground">
        {heading}
      </h2>
      {/* The section bodies are markdown authored by the agent; rendered as pre-wrapped
          text here (a rich markdown renderer is a later concern). `break-words` lets a long
          unbroken token (a path/URL) WRAP instead of pushing the width out — harmless on the
          wide detail page, and the belt that keeps the 380px overlay from overflowing. */}
      <p className="whitespace-pre-wrap break-words text-base leading-7 text-foreground/85">
        {trimmed}
      </p>
    </section>
  );
}

/** The revision's six structured sections, in the PRD's canonical order. */
const SECTIONS: Array<{ heading: string; pick: (r: SpecRevision) => string }> = [
  { heading: "Problem", pick: (r) => r.problem },
  { heading: "Solution", pick: (r) => r.solution },
  { heading: "Implementation decisions", pick: (r) => r.implementation_decisions },
  { heading: "Testing decisions", pick: (r) => r.testing_decisions },
  { heading: "User stories", pick: (r) => r.user_stories },
  { heading: "Out of scope", pick: (r) => r.out_of_scope },
];

/**
 * `<SpecTab>` — the Spec's structured revision content: an optional summary banner (the active
 * revision number, read-only marker) then the six sections (problem / solution / implementation /
 * testing / user-stories / out-of-scope). Placement reproduced from the prototype's `spec-tab`
 * (summary banner + section list); consumes `detail.revision` (the phase-1 Spec model).
 *
 * `showBanner` (default `true`) keeps the banner on the full Spec detail page. The terminal↔Spec
 * OVERLAY passes `showBanner={false}`: there the drawer's own header already carries the
 * key/title/status, so the banner would DUPLICATE it and — squeezed to 380px — break the title
 * into a per-word staircase (review finding, test terrain).
 */
export function SpecTab({
  detail,
  showBanner = true,
}: {
  detail: SpecDetail;
  showBanner?: boolean;
}) {
  const { revision } = detail;
  const anyBody = SECTIONS.some((s) => s.pick(revision).trim().length > 0);
  return (
    <div className="mx-auto max-w-3xl">
      {showBanner ? (
        <div className="mb-6 flex items-start gap-3 rounded-xl border border-border/70 bg-muted/40 px-4 py-3.5">
          <ScrollTextIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <div>
            <div className="mb-1 flex items-center gap-2">
              <span className="text-sm font-medium text-foreground">{revision.title}</span>
              <Badge variant="muted" className="font-mono">
                revision v{revision.revision_number}
              </Badge>
              <Badge variant="muted" className="font-mono">
                agent-written · read-only
              </Badge>
            </div>
          </div>
        </div>
      ) : null}

      {anyBody ? (
        <div className="flex flex-col gap-8">
          {SECTIONS.map((s) => (
            <Section key={s.heading} heading={s.heading} body={s.pick(revision)} />
          ))}
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">This revision has no content yet.</p>
      )}
    </div>
  );
}
