import type * as React from "react";

import { cn } from "@/lib/utils";
import { inputFieldClassName } from "@/components/ui/input";

/**
 * `Textarea` — the MULTILINE twin of {@link import("./input").Input}, wearing the same
 * design-system field styling from the same source ({@link inputFieldClassName}), so a token change
 * on the border, the radius or the focus ring reaches both.
 *
 * ## Why it exists as a component
 *
 * Base UI has no textarea primitive (its `Input` renders an `<input>`), so a multiline control has
 * always been a raw `<textarea>` here — and the two that predate this file each wrote their own
 * class string (`review/comment-thread.tsx`, `design/param-field.tsx`). Two copies of a look is a
 * habit; a third would be the shape of a design system that has stopped being one. This is that
 * third one, factored: `resize-y` and a `rows` default on top of the shared field class, and
 * nothing else. The two older call sites are deliberately left alone — they carry their own
 * deviations (a mono `text-xs`, a `bg-card`) and rewriting them is not this change's business.
 *
 * Controlled like `Input` (`value` / `onChange`), and every native prop passes through, so a caller
 * keeps `rows`, `placeholder`, `aria-label` and the blur handlers a form needs.
 */
export function Textarea({
  className,
  rows = 3,
  ...props
}: React.ComponentPropsWithoutRef<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      rows={rows}
      className={cn(inputFieldClassName, "resize-y leading-relaxed", className)}
      {...props}
    />
  );
}
