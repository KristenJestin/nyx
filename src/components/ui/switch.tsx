import { Switch as BaseSwitch } from "@base-ui/react/switch";

import { cn } from "@/lib/utils";

/**
 * `Switch` — the in-house toggle, built on **Base UI's `Switch`** (shadcn-style,
 * like `Button`). It encapsulates the WHOLE switch: `Switch.Root` (the track) AND
 * `Switch.Thumb` (the knob) with ALL of their styling, so a caller renders a
 * single `<Switch ... />` and never re-declares the track/thumb markup or imports
 * `@base-ui/react/*` directly. This is the right home for the switch's look — the
 * thumb's `data-[checked]` translate and the track's checked colours belong to the
 * component, not the feature using it.
 *
 * Base UI's `Switch.Root` renders a non-native `role="switch"` `<span>` plus a
 * hidden labelable `<input>` carrying the passed `id`, so a wrapping `<label
 * htmlFor={id}>` associates with it explicitly (a11y); `aria-label` stays as the
 * accessible name. Controlled via `checked` / `onCheckedChange`.
 */
export function Switch({ className, ...props }: BaseSwitch.Root.Props) {
  return (
    <BaseSwitch.Root
      data-slot="switch"
      className={cn(
        "relative h-5 w-9 shrink-0 cursor-pointer rounded-full border border-input bg-input outline-none transition-colors",
        "data-[checked]:border-primary data-[checked]:bg-primary",
        "focus-visible:ring-2 focus-visible:ring-ring",
        className,
      )}
      {...props}
    >
      <BaseSwitch.Thumb
        data-slot="switch-thumb"
        className={cn(
          "block size-4 translate-x-0.5 rounded-full bg-background shadow-sm transition-transform",
          "data-[checked]:translate-x-4",
        )}
      />
    </BaseSwitch.Root>
  );
}
