import { Checkbox as BaseCheckbox } from "@base-ui/react/checkbox";
import { CheckIcon } from "lucide-react";

import { cn } from "@/lib/utils";

/**
 * `Checkbox` — the in-house checkbox, built on **Base UI's `Checkbox`**
 * (shadcn-style, like `Button`). It encapsulates BOTH `Checkbox.Root` (the box)
 * and `Checkbox.Indicator` (the tick) plus ALL their styling, so a caller renders
 * a single `<Checkbox ... />` and never re-declares the box/indicator markup or
 * imports `@base-ui/react/*` directly. The check glyph (lucide `CheckIcon`) is the
 * default indicator; the box covers checked / indeterminate / disabled states.
 *
 * Base UI's `Checkbox.Root` renders a non-native `role="checkbox"` `<span>` plus a
 * hidden labelable `<input>` carrying the passed `id` and reflecting state via
 * `aria-checked` / `aria-disabled` (not the DOM `disabled` attr) — the contract
 * the tests query by. Controlled via `checked` / `onCheckedChange`; supports
 * `indeterminate` and `disabled`.
 */
export function Checkbox({ className, ...props }: BaseCheckbox.Root.Props) {
  return (
    <BaseCheckbox.Root
      data-slot="checkbox"
      className={cn(
        "flex size-4 shrink-0 cursor-pointer items-center justify-center rounded border border-input bg-background outline-none",
        "data-[checked]:border-primary data-[checked]:bg-primary data-[checked]:text-primary-foreground",
        "data-[indeterminate]:border-primary data-[indeterminate]:bg-primary data-[indeterminate]:text-primary-foreground",
        "data-[disabled]:cursor-not-allowed data-[disabled]:opacity-40",
        "focus-visible:ring-2 focus-visible:ring-ring",
        className,
      )}
      {...props}
    >
      <BaseCheckbox.Indicator data-slot="checkbox-indicator">
        <CheckIcon className="size-3" />
      </BaseCheckbox.Indicator>
    </BaseCheckbox.Root>
  );
}
