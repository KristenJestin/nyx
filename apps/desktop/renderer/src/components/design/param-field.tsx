import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Segmented } from "@/components/ui/segmented";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";

import type { ParamDescriptor } from "./params";

/**
 * `ParamField` — one editable control for a component param (v2.7 Phase 3, task #11). It renders the
 * right control for the descriptor's type: a text input, a MULTILINE textarea for an `html` content
 * param (a content param can be plain text OR raw html — PDR-0010), a picker for a `state` (the
 * state IS a param) or `select`, a switch for a boolean, and a number input. The live preview
 * re-renders on every change (the detail owns the value + preview).
 *
 * An ENUMERATED param (`state` / `select`) gets a control that ADAPTS to its option count (finding
 * `01KYPJ1SCHGZ2STSXZ4VCR1K33`): the options come from the SYNTHESIS, so their number is unknown by
 * construction — a `button` declares 8 CVA variants, a `badge` 13 — and a fixed one-row segmented
 * control turned the params panel into a horizontal scrollbar. Below the threshold the segmented
 * picker stays (every option visible and one click away, WRAPPING onto as many rows as it needs);
 * above it, a dropdown, which is O(1) wide whatever the count. Either way the panel never scrolls
 * sideways — at 8, 15 or 30 states.
 *
 * And when the control CANNOT be the one the synthesis asked for, the field says so beside the type
 * (finding `01KYPSG0HPG2D28CMJEXRKNEVK`) rather than passing a constrained choice off as a free
 * field — see `typeNote`.
 */
export interface ParamFieldProps {
  descriptor: ParamDescriptor;
  value: unknown;
  onChange: (value: unknown) => void;
}

/**
 * Above this many options, the enumerated control becomes a DROPDOWN. Six is where a wrapped
 * segmented picker still reads as one glanceable group in a ~20 rem panel; past it the wrap eats the
 * panel and a list is both smaller and easier to scan.
 */
export const SEGMENTED_OPTIONS_MAX = 6;

export function ParamField({ descriptor, value, onChange }: ParamFieldProps) {
  const id = `param-${descriptor.name}`;
  const label = descriptor.label ?? descriptor.name;
  const isState = descriptor.type === "state";
  /**
   * A REQUIRED param emptied by hand (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point C). Clearing a
   * mandatory field renders a component the caller may never build — a coloured pill with no label
   * — and the preview did it in complete silence: no message, no border, nothing to distinguish it
   * from a component that simply looks like that. The rendering STAYS honest (it shows exactly what
   * an empty value produces, which is the point of a preview); what is added is the statement that
   * this state is a hole, and whose it is.
   */
  const requiredButEmpty =
    descriptor.required === true && (value == null || String(value).trim() === "");
  // The same statement, on the control itself: a hole one can SEE without reading the line above.
  const controlClass = cn(
    "w-full",
    requiredButEmpty && "border-destructive/60 focus-visible:ring-destructive/40",
  );
  /**
   * A CONSTRAINED CHOICE THE FORM COULD NOT HONOUR SAYS SO (finding `01KYPSG0HPG2D28CMJEXRKNEVK`).
   * Two ways a synthesis asks for a picker and gets a free-text input, both SILENT until now:
   *   * an unknown `type` (`"enum"`, `"string"`…) — coerced to `text`, and its `options` dropped
   *     with it: "j'ai bien fourni options, l'UI les a ignorées";
   *   * a `select` with NO `options` — nothing to choose from, so the control degrades.
   * The write path now REFUSES both on a new synthesis; this is what the form owes a row stored
   * BEFORE that guard — name the declared word, and say why the control does not match it.
   */
  const emptySelect = descriptor.type === "select" && (descriptor.options?.length ?? 0) === 0;
  const typeNote = descriptor.coercedFrom
    ? {
        text: `${descriptor.coercedFrom} → text`,
        title: `The synthesis declared the type "${descriptor.coercedFrom}", which is not one of the six nyx renders (text, html, state, boolean, number, select) — it is shown as a plain text field and any options it declared are ignored. Re-synthesize the component with a known type.`,
      }
    : emptySelect
      ? {
          text: "select · no options",
          title:
            "This param is a select but the synthesis declared no options — there is nothing to choose from, so it degrades to a free-text field. Re-synthesize it with its allowed values.",
        }
      : null;

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <Label htmlFor={id} className="text-xs text-muted-foreground">
          {label}
        </Label>
        <span className="flex shrink-0 items-center gap-1 text-2xs text-muted-foreground/60">
          {/* REQUIRED is part of the component's CONTRACT (finding `01KYPJ1RC6WJX1Z9CPPV23FG27`):
              the caller MUST provide this param — the component has no value of its own. Said here
              (beside the type, never inside the label, which stays the param's plain name) it is
              also the honest reading of an empty control: nothing is missing from the form, the
              component simply demands a value. */}
          {descriptor.required ? (
            <span
              data-testid={`param-required-${descriptor.name}`}
              title={
                requiredButEmpty
                  ? "Required, and empty — the preview above renders the component without it, which is what an unprovided value really looks like. Type a value, or leave it empty to see the hole."
                  : "Required — the caller must provide this param (the component has no value of its own)"
              }
              className={cn(
                "font-medium uppercase",
                requiredButEmpty ? "text-destructive" : "text-destructive/80",
              )}
            >
              {requiredButEmpty ? "required · empty" : "required"}
            </span>
          ) : null}
          {typeNote ? (
            <span
              data-testid={`param-type-note-${descriptor.name}`}
              title={typeNote.title}
              className="font-medium text-warning"
            >
              {typeNote.text}
            </span>
          ) : (
            <span>{descriptor.type}</span>
          )}
        </span>
      </div>
      {renderControl()}
    </div>
  );

  function renderControl() {
    switch (descriptor.type) {
      case "html":
        return (
          <textarea
            id={id}
            value={String(value ?? "")}
            onChange={(e) => onChange(e.target.value)}
            rows={4}
            aria-invalid={requiredButEmpty || undefined}
            className={cn(
              "resize-y rounded-md border border-input bg-card px-2.5 py-1.5 font-mono text-xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring",
              controlClass,
            )}
          />
        );
      case "boolean":
        return (
          <div className="flex items-center">
            <Switch
              id={id}
              checked={Boolean(value)}
              onCheckedChange={(checked: boolean) => onChange(checked)}
            />
          </div>
        );
      case "number":
        return (
          <Input
            id={id}
            type="number"
            value={value == null ? "" : String(value)}
            onChange={(e) => onChange(e.target.value === "" ? "" : Number(e.target.value))}
            aria-invalid={requiredButEmpty || undefined}
            className={controlClass}
          />
        );
      case "state":
      case "select": {
        const options = descriptor.options ?? [];
        const groupLabel = isState ? "State" : label;
        if (options.length > SEGMENTED_OPTIONS_MAX) {
          // MANY options (a `badge` has 13 variant axes): a dropdown is constant-width, so the
          // panel never gains a horizontal scrollbar however long the list — or the names — get.
          return (
            <select
              id={id}
              aria-label={groupLabel}
              value={String(value ?? options[0])}
              onChange={(e) => onChange(e.target.value)}
              className={cn(
                // `min-w-0` + `truncate`: an over-long option name shortens the CLOSED control
                // instead of widening it (the native popup still shows the full name).
                "min-w-0 truncate rounded-md border border-input bg-card px-2 py-1 text-xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring",
                controlClass,
              )}
            >
              {options.map((o) => (
                <option key={o} value={o}>
                  {o}
                </option>
              ))}
            </select>
          );
        }
        if (options.length > 0) {
          return (
            <Segmented<string>
              aria-label={groupLabel}
              value={String(value ?? options[0])}
              onChange={onChange}
              options={options.map((o) => ({ v: o, label: o }))}
              size="md"
              // The options are DATA: even a short list can be wider than the panel, so it wraps.
              wrap
              className={controlClass}
            />
          );
        }
        return (
          <Input
            id={id}
            value={String(value ?? "")}
            onChange={(e) => onChange(e.target.value)}
            aria-invalid={requiredButEmpty || undefined}
            className={controlClass}
          />
        );
      }
      default:
        return (
          <Input
            id={id}
            value={String(value ?? "")}
            onChange={(e) => onChange(e.target.value)}
            aria-invalid={requiredButEmpty || undefined}
            className={controlClass}
          />
        );
    }
  }
}
