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
 */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  "aria-label": ariaLabel,
  className,
  size = "sm",
}: {
  value: T;
  onChange: (v: T) => void;
  options: readonly { readonly v: T; readonly label: string }[];
  "aria-label"?: string;
  className?: string;
  /** Visual density. `sm` = the compact default; `md` = toolbar-height (matches text-sm inputs). */
  size?: "sm" | "md";
}) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={cn(
        "flex items-center gap-0.5 rounded-md border border-input bg-card",
        size === "md" ? "p-1" : "p-0.5",
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
