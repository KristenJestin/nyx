import { Input as BaseInput } from "@base-ui/react/input";

import { cn } from "@/lib/utils";

/**
 * THE design-system FIELD styling — border, radius, padding, type scale, focus ring —
 * as one exported string.
 *
 * `Input` below is the ordinary way to wear it, and it is what a feature component
 * should reach for. This constant exists for the ONE case `Input` cannot serve: a
 * headless primitive that renders its OWN `<input>` and therefore cannot be wrapped
 * (Base UI's `Combobox.Input`, whose input is wired to the combobox's own state and
 * anchor measurement — passing our `Input` as its render prop would still leave the
 * class string to be written, and writing it inline is precisely what the rule below
 * forbids). Such a component names THIS and composes on top of it with `cn`:
 *
 *   <Combobox.Input className={cn(inputFieldClassName, "w-full pr-7")} />
 *
 * so a token change here reaches it too, which a copied class string never did.
 */
export const inputFieldClassName = cn(
  "rounded-md border border-input bg-background px-2 py-1.5 text-sm text-foreground outline-none",
  "focus-visible:ring-2 focus-visible:ring-ring",
);

/**
 * `Input` — the in-house text input, built on **Base UI's `Input`** (shadcn-style,
 * exactly like `Button`). The design-system field styling (border, radius,
 * padding, focus ring) lives HERE so feature components never re-declare an inline
 * `inputClass`, and never import `@base-ui/react/*` directly. Extra `className`
 * still composes (e.g. `font-mono`, `flex-1`) via `cn`.
 *
 * THE ONE SANCTIONED EXCEPTION to the second half of that rule is a HEADLESS
 * PRIMITIVE nyx has no wrapper for — today `@base-ui/react/combobox`, used directly
 * by `specs/spec-category.tsx` (as `terminal-context-menu.tsx`, `base-layout.tsx`
 * and `project-dialog.tsx` already do for their own primitives). A `ui/combobox.tsx`
 * would have to re-export seven sub-components (`Root`, `InputGroup`, `Input`,
 * `Trigger`, `Portal`, `Positioner`, `Popup`, `List`, `Item`, `ItemIndicator`,
 * `Empty`) to add nothing but a class string, and that class string is what
 * {@link inputFieldClassName} already gives it. The half of the rule that MATTERS —
 * one source for the field styling — is kept by naming that constant; the day a
 * second component needs the same combobox, the wrapper becomes worth its weight
 * and this paragraph is the place to record that it did.
 *
 * Base UI's `Input` renders a real `<input>` and auto-wires to a `Field` when
 * placed inside one; here it is used standalone (controlled `value`/`onChange`),
 * so it behaves as a plain styled input while keeping a single source of truth for
 * the look.
 */
export function Input({ className, ...props }: BaseInput.Props) {
  return <BaseInput data-slot="input" className={cn(inputFieldClassName, className)} {...props} />;
}
