import { ZapIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";

import {
  CSS_SLOT_FEATURE_LABELS,
  CSS_SLOT_REFUSAL_LABELS,
  cssSlotFeatureLabel,
  cssSlotRefusalLabel,
  type RenderedCssSlot,
} from "./use-design-render";

/**
 * The three refusals the lint line NAMES — the ones a reader recognizes at a glance (a selector
 * that escapes the component, `!important`, an at-rule with nothing to anchor). They are CODES, so
 * the fixture arrimage keeps them true; the rest are counted, never invented.
 */
const NAMED_REFUSALS = ["escaping_selector", "important", "at_rule"] as const;

/** How many refusal rules the sidecar checks BEYOND the three the line names. */
const OTHER_REFUSAL_COUNT = Object.keys(CSS_SLOT_REFUSAL_LABELS).length - NAMED_REFUSALS.length;

/**
 * `<CssSlotCard>` — the SCOPED CSS SLOT, shown where it is reviewed (findings
 * `01KYVMTWB71R0D0CT1SV88J8DX` for the model, `01KYVVEQZW2Y2VY7ZAPK5RECZS` for this display; the
 * FORM is the elected v8 variant `01KYVM1KM9WE6CVZGBT4M4ZJMM`).
 *
 * A component may carry ONE strict escape hatch for what Tailwind utilities cannot express. Its
 * fourth non-negotiable constraint is **SIGNALLED**: the exception must be VISIBLE to the human who
 * reviews the page — a slot nobody sees is an exception nobody arbitrated. This card is that
 * visibility, and it is one of three signals, all fed by the sidecar:
 *
 *  - HERE, the card: the scope, the compiled CSS, and the lint (what it may use, what it may not);
 *  - ON THE NODE, in the rendering: the frame styles `[data-nyx-css]`, the attribute the sidecar
 *    stamped on the component's root — the mark is READ FROM THE MARKUP, never re-derived;
 *  - IN THE CHROME, the count: `css_slot_count`, beside the page's identity.
 *
 * # The slot belongs to the COMPONENT, and the card says so
 *
 * A node only POINTS at its component's slot (`ElectedNode.css_scope`). A `Spinner` placed thirty
 * times has ONE exception, and every one of those placements shows this same card. So the card
 * NAMES the owner and states the blast radius: editing it is a catalogue gesture (the agent's
 * `design_author_component`), never something this panel does to one placement. There is no control
 * on it for the same reason — the renderer neither compiles nor scopes a slot.
 *
 * # A refused slot is NAMED, not dropped
 *
 * A slot STORED when the rules were laxer (or whose token layer moved under it) comes back with
 * `refused` filled, `css` empty and nothing emitted into the frame. It stays on screen as a NAMED
 * refusal — the same principle as an unrenderable node — because a rule that silently does nothing
 * is exactly the disappearance the slot exists to end.
 *
 * # The SAME card on two surfaces, and only the ownership sentence differs
 *
 * The card is now read from the COMPONENT's own detail too (finding `01KYW342EE5DN6F70YB8VSTJR8`,
 * point 7), where the exception belongs. Everything it states is the same there — one card, not a
 * second one — except the sentence that places the exception: beside a PLACEMENT it must deny that
 * the placement owns it; on the component's own page there is no placement to deny, and the
 * `component` surface says the blast radius instead ({@link CssSlotSurface}).
 */

/**
 * WHERE the card is being read — it changes ONE sentence and the footer's second clause, nothing
 * else:
 *  - `placement` (the default, the prototype's node panel): the exception is the COMPONENT's, and
 *    the placement on screen does not own it;
 *  - `component` (the catalogue detail): the exception is on its owner's own page, so what is owed
 *    is its REACH — every placement carries it — not a denial.
 */
export type CssSlotSurface = "placement" | "component";

export function CssSlotCard({
  slot,
  surface = "placement",
}: {
  slot: RenderedCssSlot;
  surface?: CssSlotSurface;
}) {
  const refused = slot.refused !== null;
  const used = new Set(slot.features);

  return (
    <section
      data-testid={`css-slot-${slot.scope_id}`}
      data-refused={refused}
      aria-label={`CSS slot of ${slot.component}`}
      className={
        refused
          ? "overflow-hidden rounded-lg border border-destructive/40 bg-destructive/[0.06]"
          : "overflow-hidden rounded-lg border border-warning/40 bg-warning/[0.06]"
      }
    >
      {/* THE TITLE BAR — the icon, the name and the state. It is DROPPED on the `component`
          surface: there the card is the body of a fold whose HEAD already carries exactly these
          three, and stating them twice in a row is the only difference the two surfaces would
          have shown. */}
      {surface === "placement" ? (
        <div className="flex items-center gap-1.5 border-b border-warning/20 px-2.5 py-1.5">
          <ZapIcon
            aria-hidden
            className={refused ? "size-3.5 text-destructive" : "size-3.5 text-warning"}
          />
          <span
            className={
              refused ? "font-medium text-xs text-destructive" : "font-medium text-xs text-warning"
            }
          >
            CSS slot · scoped
          </span>
          <span className="flex-1" />
          <Badge variant={refused ? "destructive" : "warning"} className="shrink-0">
            {refused ? "refused" : "exception"}
          </Badge>
        </div>
      ) : null}

      <div className="px-2.5 py-2">
        {/* THE SCOPE, LITERALLY — the selector every rule of this slot is anchored on. It is the
            component's, and the sentence beside it is what stops the reader believing this
            exception is a property of the placement they happen to have selected. */}
        <p
          data-testid="css-slot-scope"
          className="mb-1.5 flex flex-wrap items-center gap-1.5 font-mono text-2xs text-muted-foreground"
        >
          scope
          <code className="rounded border border-border/60 bg-background px-1.5 py-0.5 text-foreground">
            {slot.scope}
          </code>
        </p>
        <p
          data-testid="css-slot-ownership"
          className="mb-2 text-2xs leading-relaxed text-muted-foreground/80"
        >
          {/* No tool name, and no description of what this panel does (finding
              `01KZ3JW5A63TJWX83V8ZKX62CN`): what the reader owes is WHOSE exception it is and how
              far it reaches. Who authors it is the agent's manual, not this card. */}
          {surface === "component" ? (
            <>
              It belongs to <span className="font-medium text-foreground/80">{slot.component}</span>{" "}
              itself: every placement of it carries this same exception, and its rules can reach
              nothing outside that scope.
            </>
          ) : (
            <>
              It belongs to the component{" "}
              <span className="font-medium text-foreground/80">{slot.component}</span>, not to this
              placement: every placement of {slot.component} carries it, and its rules can reach
              nothing outside that scope.
            </>
          )}
        </p>

        {/* WHY IT EXISTS, in the agent's own words. Mandatory at the write; a row stored before it
            became mandatory can still be empty, and that is said rather than left blank. */}
        <p
          data-testid="css-slot-reason"
          className="mb-2 text-2xs leading-relaxed text-foreground/80"
        >
          {slot.reason.trim().length > 0 ? (
            slot.reason
          ) : (
            <span className="text-muted-foreground/70">
              No reason recorded — this slot predates the rule that requires one.
            </span>
          )}
        </p>

        {/* THE CSS — the COMPILED, scoped stylesheet the frame really applied, not what was
            authored: the selectors are already anchored and the @keyframes already renamed, which
            is exactly what the coder must reproduce. A refused slot emitted none, and says so. */}
        {refused ? (
          <p
            data-testid="css-slot-refusal"
            className="rounded-md border border-destructive/30 bg-destructive/5 px-2 py-1.5 text-2xs leading-relaxed text-destructive"
          >
            <span className="font-medium">
              Refused — {cssSlotRefusalLabel(slot.refused ?? "")}.
            </span>{" "}
            {slot.detail ?? "The stored slot no longer compiles; nothing was applied to the frame."}
          </p>
        ) : (
          <pre
            data-testid="css-slot-code"
            className="max-h-48 overflow-auto rounded-md border border-border/60 bg-background px-2 py-1.5 font-mono text-2xs leading-relaxed text-muted-foreground"
          >
            {slot.css}
          </pre>
        )}

        {/* THE LINT — the three statements the review needs, and the only strings this display
            keys off are the codes `contract/design-fixtures.json` pins. */}
        <ul className="mt-2 flex flex-col gap-1" data-testid="css-slot-lint">
          <li className="flex items-start gap-1.5 text-2xs text-muted-foreground">
            <span className="text-success">✓</span>
            <span>tokens only — no raw hex, no raw px outside the scale</span>
          </li>
          <li className="flex flex-wrap items-center gap-1.5 text-2xs text-muted-foreground">
            <span className={refused ? "text-muted-foreground/60" : "text-success"}>
              {refused ? "·" : "✓"}
            </span>
            <span>allowed:</span>
            {Object.keys(CSS_SLOT_FEATURE_LABELS).map((code) => (
              <code
                key={code}
                data-testid={`css-slot-feature-${code}`}
                data-used={used.has(code)}
                className={
                  used.has(code)
                    ? "rounded border border-warning/50 bg-warning/10 px-1 font-mono text-foreground"
                    : "font-mono text-muted-foreground/50"
                }
              >
                {cssSlotFeatureLabel(code)}
              </code>
            ))}
          </li>
          <li className="flex items-start gap-1.5 text-2xs text-muted-foreground">
            <span className="text-destructive">✕</span>
            <span>
              refused: {NAMED_REFUSALS.map(cssSlotRefusalLabel).join(" · ")} · and{" "}
              {OTHER_REFUSAL_COUNT} more rules
            </span>
          </li>
        </ul>

        <p
          data-testid="css-slot-foot"
          className="mt-2 border-t border-warning/20 pt-1.5 text-2xs text-muted-foreground/70"
        >
          {slot.bytes} B authored ·{" "}
          {surface !== "component"
            ? "flagged on the node in the rendering and in the composition tree — the coder inherits the exception, never discovers it."
            : refused
              ? "nothing of it reached the preview above."
              : "applied to the preview above."}
        </p>
      </div>
    </section>
  );
}

/**
 * The slot a COMPONENT carries among the ones this render met, `null` when it carries none.
 *
 * Keyed on the component NAME — the thing a slot belongs to. A node reaches its slot the same way
 * `ElectedNode.css_scope` does: by pointing at the component's, never by owning one.
 */
export function cssSlotOfComponent(
  slots: readonly RenderedCssSlot[],
  component: string | null | undefined,
): RenderedCssSlot | null {
  if (component == null) return null;
  return slots.find((s) => s.component === component) ?? null;
}
