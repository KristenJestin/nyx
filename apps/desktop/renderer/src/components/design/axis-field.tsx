import { Label } from "@/components/ui/label";
import { Segmented } from "@/components/ui/segmented";
import { cn } from "@/lib/utils";

import { type VarianceAxis } from "./axes";
import { SEGMENTED_OPTIONS_MAX } from "./param-field";

/**
 * `AxisField` — ONE CONTROL FOR ONE AXIS of variance (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`).
 *
 * The composant detail used to expose a SINGLE state selector, because the model knew a single flat
 * axis. A real component has several orthogonal ones — `button` is `variant × size`, `avatar` is
 * `type × shape × size` — and a boolean (`loading`) is just an axis with two values. So the panel
 * now renders one of these per declared axis: picking `size = lg` leaves `variant` where it is, and
 * the frame re-renders the COMBINATION. No cartesian product anywhere.
 *
 * The control ADAPTS to the value count exactly like [`ParamField`] does (finding
 * `01KYPJ1SCHGZ2STSXZ4VCR1K33`): a wrapping segmented picker up to [`SEGMENTED_OPTIONS_MAX`], a
 * dropdown beyond it — the panel never gains a horizontal scrollbar, whether the axis has 2 values
 * or 13.
 *
 * A value that can change NOTHING — no classes, and no condition on its axis that renders it
 * differently from another value of the same axis ([`inertAxisValues`]) — is marked INERT right on
 * the option's tooltip rather than left to be clicked into the void, the same discipline as the
 * param form's coercion badge. A BASELINE is not inert: `{ "name": "off" }` beside a
 * `data-nyx-if="loading=on"` element is the manual's canonical boolean, and the badge must stay
 * silent on it (finding `01KYSCY4J8JANZEJPYME86B1VH`).
 */
export interface AxisFieldProps {
  axis: VarianceAxis;
  value: string;
  onChange: (value: string) => void;
  /** The values of THIS axis that can change nothing — flagged, never hidden. */
  inertValues?: readonly string[];
}

const controlClass = "w-full";

export function AxisField({ axis, value, onChange, inertValues = [] }: AxisFieldProps) {
  const id = `axis-${axis.name}`;
  // A LEGACY flat `states_json` is one ANONYMOUS axis: it keeps the name the whole chain has always
  // shown for it ("State"), so a component stored before the axes model looks and reads unchanged.
  const label = axis.label ?? (axis.legacy ? "State" : axis.name);
  const names = axis.values.map((v) => v.name);
  const current = names.includes(value) ? value : axis.defaultValue;
  const inert = new Set(inertValues);

  return (
    <div className="space-y-1.5" data-testid={`axis-field-${axis.name}`}>
      <div className="flex items-center justify-between gap-2">
        <Label htmlFor={id} className="text-xs text-muted-foreground">
          {label}
        </Label>
        <span className="flex shrink-0 items-center gap-1 text-2xs text-muted-foreground/60">
          {inert.size > 0 ? (
            <span
              data-testid={`axis-inert-${axis.name}`}
              title={`These values carry no classes, and nothing in the template renders them differently from the other values of this axis, so selecting them changes nothing: ${[...inert].join(", ")}. The synthesis must give each value its resolved classes (states_json), or render an element that only EXISTS at one of them (data-nyx-if).`}
              className="font-medium text-warning"
            >
              {inert.size} inert
            </span>
          ) : null}
          {/* The word `axis` used to sit here, on EVERY row, under a section titled AXES — the
              section says it once, for all of them (finding `01KZ3JW5A63TJWX83V8ZKX62CN`). What
              stays on this line is the only thing that varies per axis: how many of its values are
              inert. Where the type is genuinely informative — a PARAM, which can be any of six —
              `ParamField` still states it. */}
        </span>
      </div>
      {names.length > SEGMENTED_OPTIONS_MAX ? (
        <select
          id={id}
          aria-label={label}
          value={current}
          onChange={(e) => onChange(e.target.value)}
          className={cn(
            controlClass,
            "min-w-0 truncate rounded-md border border-input bg-card px-2 py-1 text-xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring",
          )}
        >
          {axis.values.map((v) => (
            <option key={v.name} value={v.name}>
              {v.label ?? v.name}
              {inert.has(v.name) ? " (inert)" : ""}
            </option>
          ))}
        </select>
      ) : (
        <Segmented<string>
          aria-label={label}
          value={current}
          onChange={onChange}
          options={axis.values.map((v) => ({ v: v.name, label: v.label ?? v.name }))}
          size="md"
          wrap
          className={controlClass}
        />
      )}
    </div>
  );
}
