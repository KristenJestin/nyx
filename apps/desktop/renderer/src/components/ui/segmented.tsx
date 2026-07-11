import { cn } from "@/lib/utils";

/**
 * `Segmented` — a generic segmented control `[ a · b · c ]`. Ported from the prototype against the
 * v2 tokens; one module, N call sites (the review tab's All/Open/Resolved filter, …). `aria-label`
 * names the group; each option is a pressed/unpressed button.
 */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  "aria-label": ariaLabel,
  className,
}: {
  value: T;
  onChange: (v: T) => void;
  options: readonly { readonly v: T; readonly label: string }[];
  "aria-label"?: string;
  className?: string;
}) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={cn(
        "flex items-center gap-0.5 rounded-md border border-input bg-card p-0.5",
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
            "rounded px-1.5 py-0.5 text-2xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
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
