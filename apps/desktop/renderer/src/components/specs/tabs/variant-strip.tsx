import {
  CheckIcon,
  CircleSlashIcon,
  TriangleAlertIcon,
  WandSparklesIcon,
  XIcon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import type { PrototypeVariant } from "../proto-types";
import type { ProtoNotice } from "../use-proto-actions";

/**
 * `<VariantStrip>` — the VARIANT SWITCH and the ELECTION, rendered UNDER the preview surface
 * (findings `01KYT44FS853KEJE58HQNQPKCD`, points 5 and 6). It replaces the stacked list of variants
 * that could only be ELECTED: one could retain a variant without ever having SEEN it, which is
 * electing blind — and seeing the options before deciding IS the prototype.
 *
 * The placement and the shape are the arbitrations depot's prototype workspace already rendered
 * (`prototype-variant-strip.tsx`, PRD 0045 / P3-P4 and PRD 0049 / P1-P3), applied here rather than
 * rediscovered:
 *
 *   * UNDER the surface, a SIBLING of the frame — never an overlay. That was settled against three
 *     prototyped alternatives: any overlay, however compact, still covers the very elements the
 *     reviewer wants to click.
 *   * ONE PILL PER VARIANT, the active one FULL (`bg-primary`) and carrying `aria-pressed`, so what
 *     is on screen is legible at a glance and reachable by a screen reader.
 *   * The MAIN / RETAINED badges stay readable ON the active pill: on the full primary fill the plain
 *     neutral/success badges wash out, so they swap to a translucent chip in the primary foreground
 *     (`data-on-primary`, PRD 0049 / P1).
 *   * A CONSTANT pill height (`h-7`, `leading-none` on the badges), so a pill with a badge is exactly
 *     as tall as one without and the row does not jump (PRD 0049 / P2).
 *   * The ELECTION DECISION lives in this same strip, next to the variants it decides between
 *     (PRD 0049 / P3) — a decision about "which variant" belongs where the variants are.
 *   * The DECISION row NEVER repeats the state the pill already carries (finding
 *     `01KYVMSMMBVSZNBBSBE18PRZAK`): it offers the ACTION left to take — `Retain this variant` when
 *     the shown variant is not the retained one, `Clear decision` when it is.
 *   * On a SINGLE-VARIANT page the switch disappears and only the label remains: there is nothing to
 *     choose between.
 *
 * What nyx keeps that depot has no equivalent of — the two signifiers PDR-0009 holds apart:
 *
 *   * MAIN — the AGENT's recommendation (`is_main`). Read-only: the agent poses it through the MCP
 *     `proto_*` tools, never a UI button.
 *   * RETAINED — the USER's decision, posed and RETIRED here (`proto_elect` / `proto_election_clear`,
 *     reversible). A mono-variant page is retained BY DEFAULT (a third, distinct badge) — reflected
 *     from `effective_variant_id`, nothing to click.
 *
 * Electing on a design-LOCKED round is refused by the backend (`FROZEN`); {@link ProtoNotice} carries
 * the orientation, rendered above the row.
 *
 * Purely presentational: the parent owns the selection and the writes, so the strip stays trivially
 * testable — the same discipline as the reference.
 */

/**
 * The badge style that survives the ACTIVE pill's full primary fill (PRD 0049 / P1). `leading-none`
 * keeps a badge from adding any vertical lead, which is what makes every pill the same height.
 */
const ON_PRIMARY_BADGE = cn(
  "inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 text-2xs font-medium leading-none",
  "border-primary-foreground/40 bg-primary-foreground/20 text-primary-foreground",
);

export function VariantStrip({
  variants,
  selectedVariantId,
  onSelect,
  chosenVariantId,
  effectiveVariantId,
  canElect,
  pending,
  onElect,
  onClear,
  notice,
  onDismissNotice,
  readOnly = false,
}: {
  /** The shown version's variants, in position order. */
  variants: readonly PrototypeVariant[];
  /** The variant currently DISPLAYED in the surface above. */
  selectedVariantId: string | null;
  /** Show another variant (the compare gesture). */
  onSelect: (variantId: string) => void;
  /** The USER's RETENU for this page in the current round, `null` when none. */
  chosenVariantId: string | null;
  /** The EFFECTIVE retained variant (the RETENU, else the mono-variant retained by default). */
  effectiveVariantId: string | null;
  /** Can an election be written at all? (A current round AND a page pinned in it.) */
  canElect: boolean;
  /** A proto write is in flight — gate the controls. */
  pending: boolean;
  onElect: (variantId: string) => void;
  onClear: () => void;
  /** The FREEZE-guard orientation / a write error, persistent until dismissed. */
  notice: ProtoNotice | null;
  onDismissNotice: () => void;
  /**
   * CONSULTATION: the strip shows a PAST round (finding `01KZ3AP34K1PKS6W0E75RP7DF6`). The variants
   * and their badges stay — reading which one was retained is the point of looking — and the whole
   * DECISION zone goes: `Clear decision` is a write, and it would have written on the CURRENT round
   * while the user reads an old one. A decision one cannot take is not a disabled button, it is not
   * there.
   */
  readOnly?: boolean;
}) {
  if (variants.length === 0) return null;

  const single = variants.length === 1;
  const selected = variants.find((v) => v.id === selectedVariantId) ?? variants[0] ?? null;
  const selectedIsChosen = selected !== null && selected.id === chosenVariantId;
  // A single, un-elected, effective variant is retained BY DEFAULT — automatic, not a user decision,
  // so it carries its own badge and offers no button.
  const selectedIsDefault =
    selected !== null && selected.id === effectiveVariantId && chosenVariantId === null && single;

  return (
    <div
      data-testid="proto-variant-strip"
      aria-label="Variants"
      className="border-t border-border/60 bg-muted/20 px-3 py-2"
    >
      {/* The FREEZE-guard orientation (a locked round) / a write error — persistent until the next
          election attempt or an explicit dismiss. The deterministic counterpart of the toast. */}
      {notice ? (
        <div
          data-testid="proto-election-notice"
          role={notice.kind === "frozen" ? "status" : "alert"}
          className={cn(
            "mb-2 flex items-start gap-2 rounded-lg border px-2.5 py-2 text-xs",
            notice.kind === "frozen"
              ? "border-info/40 bg-info/10 text-info"
              : "border-destructive/40 bg-destructive/10 text-destructive",
          )}
        >
          <TriangleAlertIcon aria-hidden className="mt-0.5 size-3.5 shrink-0" />
          <span className="min-w-0 flex-1">
            {notice.message}
            {/* …AND THE BACKEND'S OWN SENTENCE (finding `01KYVT6HVA260DTAEREQA4DFJN`): the line
                above names the gesture that failed, this one says WHY, verbatim. It used to live
                only in a toast — i.e. it was gone four seconds later. */}
            {notice.detail != null && notice.detail.length > 0 ? (
              <span data-testid="proto-election-notice-detail" className="block opacity-80">
                {notice.detail}
              </span>
            ) : null}
          </span>
          <button
            type="button"
            aria-label="Dismiss"
            onClick={onDismissNotice}
            className="-mt-0.5 -mr-0.5 shrink-0 rounded-md p-0.5 opacity-70 transition-opacity hover:opacity-100"
          >
            <XIcon className="size-3.5" />
          </button>
        </div>
      ) : null}

      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1.5">
        <div data-testid="proto-variants" className="flex flex-wrap items-center gap-1.5">
          <span className="font-medium text-2xs uppercase tracking-wide text-muted-foreground">
            Variant
          </span>

          {single ? (
            // Nothing to choose between: the label alone, no switch (PRD 0045 / S3).
            <span
              data-testid={`proto-variant-sole-${selected?.id ?? "none"}`}
              className="inline-flex h-7 items-center gap-1.5 text-sm font-medium text-foreground"
            >
              <span>{selected?.label}</span>
              <span className="text-muted-foreground">{selected?.title}</span>
              {selected?.is_main ? (
                <Badge
                  variant="muted"
                  className="leading-none"
                  data-testid={`proto-variant-reco-${selected.id}`}
                >
                  <WandSparklesIcon aria-hidden />
                  MAIN
                </Badge>
              ) : null}
              {selected !== null && chosenVariantId === selected.id ? (
                <Badge
                  variant="success"
                  className="leading-none"
                  data-testid={`proto-variant-retenu-${selected.id}`}
                >
                  <CheckIcon aria-hidden />
                  RETAINED
                </Badge>
              ) : null}
            </span>
          ) : (
            variants.map((v) => {
              const active = selected !== null && v.id === selected.id;
              return (
                <button
                  key={v.id}
                  type="button"
                  data-testid={`proto-variant-${v.id}`}
                  aria-pressed={active}
                  aria-label={`Variant ${v.label} — ${v.title}`}
                  title={v.title}
                  onClick={() => onSelect(v.id)}
                  className={cn(
                    // A CONSTANT height whatever the badges (PRD 0049 / P2).
                    "inline-flex h-7 max-w-56 items-center gap-1.5 rounded-full border px-2.5 text-xs transition-colors",
                    active
                      ? "border-primary bg-primary text-primary-foreground"
                      : "border-border/60 bg-card text-foreground hover:border-border hover:bg-accent/50",
                  )}
                >
                  <span className="font-medium leading-none">{v.label}</span>
                  <span
                    className={cn(
                      "min-w-0 truncate leading-none",
                      active ? "text-primary-foreground/80" : "text-muted-foreground",
                    )}
                  >
                    {v.title}
                  </span>
                  {/* The AGENT's recommendation — read-only, and legible on the active fill. */}
                  {v.is_main ? (
                    active ? (
                      <span
                        data-on-primary
                        className={ON_PRIMARY_BADGE}
                        data-testid={`proto-variant-reco-${v.id}`}
                      >
                        MAIN
                      </span>
                    ) : (
                      <Badge
                        variant="muted"
                        className="leading-none"
                        data-testid={`proto-variant-reco-${v.id}`}
                      >
                        MAIN
                      </Badge>
                    )
                  ) : null}
                  {/* The USER's decision — a distinct signifier, equally legible on the fill. */}
                  {chosenVariantId === v.id ? (
                    active ? (
                      <span
                        data-on-primary
                        className={ON_PRIMARY_BADGE}
                        data-testid={`proto-variant-retenu-${v.id}`}
                      >
                        RETAINED
                      </span>
                    ) : (
                      <Badge
                        variant="success"
                        className="leading-none"
                        data-testid={`proto-variant-retenu-${v.id}`}
                      >
                        RETAINED
                      </Badge>
                    )
                  ) : null}
                </button>
              );
            })
          )}
        </div>

        {/* THE DECISION, lodged with the variants it decides between (PRD 0049 / P3). It always acts
            on the variant ON SCREEN, which is the whole point: one retains what one has just seen.
            Absent while a PAST round is being consulted: there is no decision to take about what has
            already been decided, and the pills above already carry what was retained. */}
        {readOnly ? null : (
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium text-2xs uppercase tracking-wide text-muted-foreground">
              Decision
            </span>
            {selectedIsDefault ? (
              <Badge variant="info" data-testid={`proto-variant-office-${selected?.id ?? "none"}`}>
                Retained by default
              </Badge>
            ) : selectedIsChosen ? (
              // The word RETAINED is already on the pill above, as the state it is. A row that then
              // sets a `Retained` badge NEXT TO a `Retained` button says it three times and offers
              // no verb (finding `01KYVMSMMBVSZNBBSBE18PRZAK`): once retained, the only thing left
              // to decide is to UNDO — so the row carries that one action and nothing else.
              <Button
                size="xs"
                variant="ghost"
                disabled={pending}
                data-testid={`proto-variant-clear-${selected?.id ?? "none"}`}
                onClick={onClear}
              >
                <CircleSlashIcon aria-hidden />
                Clear decision
              </Button>
            ) : canElect && selected !== null ? (
              <Button
                size="xs"
                variant="outline"
                disabled={pending}
                data-testid={`proto-variant-elect-${selected.id}`}
                onClick={() => onElect(selected.id)}
              >
                <CheckIcon aria-hidden />
                Retain this variant
              </Button>
            ) : (
              // No election can be written: the page is not pinned in a current round, so there is
              // no manifest entry for a RETENU to hang off. Said, not shown as a dead button.
              <span
                data-testid="proto-election-unavailable"
                className="text-2xs text-muted-foreground/70"
              >
                Not pinned in the current round — nothing to retain against yet.
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
