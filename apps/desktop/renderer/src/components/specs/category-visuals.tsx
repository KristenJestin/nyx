import type { CSSProperties } from "react";

import { cn } from "@/lib/utils";

import type { RenameConflict } from "./use-category-actions";

/**
 * The presentational pieces of the CATEGORY axis (PDR-0017) drawn by its two surfaces — the Spec
 * detail's category block (`components/specs/spec-category.tsx`) and the project settings'
 * Categories section (`components/sidebar/categories-section.tsx`).
 *
 * They live here, next to {@link useCategoryActions} and {@link useProjectCategories}, for the same
 * reason those hooks do: the two screens are two halves of one feature, and every piece they shared
 * was found DUPLICATED between them — the tone derivation letter for letter under two names, and
 * the homonym notice as a copied JSX block, comment included (finding `01M1HB43KPF3W0VK9VACDMKJYR`).
 * A second copy of the tone rule is a second place to edit the day the backend adds a `--group-6`.
 *
 * {@link categoryTone} and {@link CategoryDot} are read by BOTH screens. {@link
 * RenameConflictNotice} is now read only by the settings section — the Spec detail stopped offering
 * a rename when the block was cut down to one line and one gesture — and stays here because a
 * rename verdict is a piece of the category vocabulary's vocabulary, not of the settings modal's.
 *
 * What is deliberately NOT here is anything a screen owns alone: the chip's typography, the row's
 * editing form, the picker.
 */

/**
 * The `--group-N` design token a category draws with, as the `--group-tone` custom property — or
 * the neutral when the backend named none.
 *
 * The token NAME is the backend's (`db::category_color_token`, held token-by-token against the
 * `--group-N` of `globals.css` by a Rust test), so the renderer never picks a colour and never
 * writes a literal one. The shape is nonetheless checked before it is interpolated into a `var(…)`:
 * a stored value that is not one of those names must fall back to the neutral rather than emit a
 * broken custom property.
 */
export function categoryTone(colorToken: string | null): CSSProperties {
  const token = colorToken !== null && /^group-[0-9]+$/.test(colorToken) ? colorToken : "group-0";
  return { "--group-tone": `var(--${token})` } as CSSProperties;
}

/**
 * The category's COLOUR DOT. It carries its own `--group-tone`, so a caller never has to remember
 * to pose the custom property on an ancestor for the dot to be anything but the neutral.
 *
 * `aria-hidden`: the colour is a redundant encoding of the name that always sits beside it, never
 * the only carrier of the information.
 */
export function CategoryDot({
  colorToken,
  className,
}: {
  colorToken: string | null;
  className?: string;
}) {
  return (
    <span
      aria-hidden
      style={categoryTone(colorToken)}
      className={cn("size-2 shrink-0 rounded-full bg-[var(--group-tone)]", className)}
    />
  );
}

/**
 * THE RENAME CONFLICT — not a refusal: the rename LANDED, and the name is now shared. The slugs are
 * what still tells the categories apart, which is why they are what is shown.
 *
 * A `role="status"` and not an `alert`: nothing failed and nothing is waiting on the user. Renders
 * NOTHING when there is no conflict, so a caller writes it unconditionally.
 *
 * `className` carries only the caller's spacing and density; the sentence itself is written once.
 */
export function RenameConflictNotice({
  conflict,
  className,
}: {
  conflict: RenameConflict | null;
  className?: string;
}) {
  if (conflict === null) return null;
  return (
    <p role="status" className={cn("text-xs text-warning", className)}>
      “{conflict.name}” now names {conflict.slugs.length} categories ({conflict.slugs.join(", ")}) —
      only the slug tells them apart.
    </p>
  );
}
