import type { CSSProperties } from "react";

import { cn } from "@/lib/utils";
import { GROUP_COLOR_COUNT, type GroupColor } from "./pane-groups";

/**
 * THE GROUP COLOUR VOCABULARY, AS ONE CONTROL — the neutral plus the six tones, in one row.
 *
 * It was written inside `group-panel.tsx` and had exactly one caller until the commands modal grew
 * a folder DETAIL PANEL (finding 01M132XFZ5KY8QMJ79HZZSBV3J): the master/detail settles a group's
 * name and colour IN the panel rather than in a second modal opened over the first, so the swatch
 * row now has two homes. It is extracted rather than copied, because a second row of swatches is a
 * second answer to "which colours does a group have" — and `GROUP_COLOR_COUNT` exists precisely so
 * there is only one.
 *
 * ## The two rules it carries, so neither caller restates them
 *
 *  - **THE FIRST SWATCH IS NOT A COLOUR.** It is the ABSENCE of a choice — the neutral `--group-0`
 *    — and it comes first because it is the DEFAULT. Taking it back is how a colour is removed.
 *  - **A NO-OP IS NOT EMITTED.** Pressing the swatch a group already wears calls nothing at all,
 *    which is what makes "open it, look at it, close it" leave the model untouched.
 */
export function GroupSwatches({
  value,
  onSelect,
  className,
}: {
  /** The group's current colour, or `null` for the neutral (the absence of a choice). */
  value: GroupColor | null;
  /** Called ONLY when the pressed swatch differs from {@link value}. */
  onSelect: (color: GroupColor | null) => void;
  className?: string;
}) {
  const pick = (color: GroupColor | null) => {
    if (color !== value) onSelect(color);
  };
  return (
    <div role="group" aria-label="Group colour" className={cn("flex flex-wrap gap-1.5", className)}>
      <Swatch color={null} selected={value == null} onSelect={() => pick(null)} />
      {Array.from({ length: GROUP_COLOR_COUNT }, (_, i) => (i + 1) as GroupColor).map((color) => (
        <Swatch key={color} color={color} selected={value === color} onSelect={() => pick(color)} />
      ))}
    </div>
  );
}

/** One colour choice. `null` is the neutral — the first one, and the absence of a choice. */
function Swatch({
  color,
  selected,
  onSelect,
}: {
  color: GroupColor | null;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={color == null ? "No colour" : `Colour ${color}`}
      aria-pressed={selected}
      data-group-swatch={color ?? "none"}
      onClick={onSelect}
      style={{ "--group-tone": `var(--group-${color ?? 0})` } as CSSProperties}
      className={cn(
        "size-5 cursor-pointer rounded-md bg-[var(--group-tone)] transition-shadow",
        "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring",
        selected && "ring-2 ring-[var(--group-tone)] ring-offset-2 ring-offset-popover",
      )}
    />
  );
}
