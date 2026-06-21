import { Input as BaseInput } from "@base-ui/react/input";

import { cn } from "@/lib/utils";

/**
 * `Input` — the in-house text input, built on **Base UI's `Input`** (shadcn-style,
 * exactly like `Button`). The design-system field styling (border, radius,
 * padding, focus ring) lives HERE so feature components never re-declare an inline
 * `inputClass`, and never import `@base-ui/react/*` directly. Extra `className`
 * still composes (e.g. `font-mono`, `flex-1`) via `cn`.
 *
 * Base UI's `Input` renders a real `<input>` and auto-wires to a `Field` when
 * placed inside one; here it is used standalone (controlled `value`/`onChange`),
 * so it behaves as a plain styled input while keeping a single source of truth for
 * the look.
 */
export function Input({ className, ...props }: BaseInput.Props) {
  return (
    <BaseInput
      data-slot="input"
      className={cn(
        "rounded-md border border-input bg-background px-2 py-1.5 text-sm text-foreground outline-none",
        "focus-visible:ring-2 focus-visible:ring-ring",
        className,
      )}
      {...props}
    />
  );
}
