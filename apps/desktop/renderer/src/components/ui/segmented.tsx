import { cn } from "@/lib/utils";

/**
 * `Segmented` — a generic segmented control `[ a · b · c ]`. Ported from the prototype against the
 * v2 tokens; one module, N call sites (the review tab's All/Open/Resolved filter, …). `aria-label`
 * names the group; each option is a pressed/unpressed button.
 *
 * `size` scopes the visual density WITHOUT touching the default: `"sm"` (the original, compact
 * chrome-strip look every existing call site keeps) and `"md"`, a taller variant whose height lines
 * up with a text-`sm` input + `size-7` icon buttons so it reads as ONE cohesive toolbar group (the
 * Knowledge Idées toolbar). New variant, not a mutated default — the other call sites are untouched.
 *
 * `wrap` is the same kind of opt-in. A control with a HANDFUL of fixed options (a filter strip) is
 * one row by nature, but a call site whose options come from DATA cannot know their number nor their
 * length — and a single row of them overflows its container into a horizontal scrollbar (finding
 * `01KYPJ1SCHGZ2STSXZ4VCR1K33`: a component's 8 variant names inside a 20 rem side panel). Wrapping
 * makes the control grow DOWNWARDS instead, which a vertical panel absorbs.
 *
 * `fill` is the SAME opt-in taken the other way (finding `01KZ3JXDFTFMMK50RQP7HECBEQ`). A control
 * that is STRETCHED by its host — a two-state selector at the top of a side panel — sizes its
 * segments to their labels and leaves the rest of the pill blank: measured in the running app, a
 * 298 px bar holding 71 px + 47 px of segments, i.e. 60 % of it empty. That does not read as a
 * two-state selector, it reads as a tab bar missing its tabs (a third segment DID live there and was
 * withdrawn). With `fill` the segments SHARE the bar the host gave the control, so what is on screen
 * is the whole control. A strip that sizes itself to its content (the viewport presets, the review
 * filters) has no blank to fill and does not ask for it.
 */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  "aria-label": ariaLabel,
  className,
  size = "sm",
  wrap = false,
  fill = false,
}: {
  value: T;
  onChange: (v: T) => void;
  options: readonly { readonly v: T; readonly label: string }[];
  "aria-label"?: string;
  className?: string;
  /** Visual density. `sm` = the compact default; `md` = toolbar-height (matches text-sm inputs). */
  size?: "sm" | "md";
  /** Let the options flow onto SEVERAL rows instead of overflowing one (data-driven options). */
  wrap?: boolean;
  /** Let the options SHARE the width the host stretched the control to (no blank tail). */
  fill?: boolean;
}) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={cn(
        "flex items-center gap-0.5 rounded-md border border-input bg-card",
        size === "md" ? "p-1" : "p-0.5",
        // `min-w-0` matters as much as the wrapping itself: without it the flex box refuses to
        // shrink below its content and pushes the overflow up to the nearest scrollable ancestor.
        wrap && "min-w-0 flex-wrap",
        className,
      )}
    >
      {options.map((o) => (
        <button
          key={o.v}
          type="button"
          aria-pressed={value === o.v}
          onClick={() => onChange(o.v)}
          className={cn(
            "rounded font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
            size === "md" ? "px-2 py-1 text-xs" : "px-1.5 py-0.5 text-2xs",
            // A wrapping control must also survive ONE option longer than the whole row.
            wrap && "max-w-full break-words",
            // `min-w-0` beside `flex-1` is what makes the SHARE equal: without it a segment refuses
            // to shrink under its label and the longer one keeps stealing the difference.
            fill && "min-w-0 flex-1 truncate",
            value === o.v
              ? "bg-muted text-foreground"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
