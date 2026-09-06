import type { CSSProperties } from "react";

import { cn } from "@/lib/utils";
import { GROUP_COLOR_COUNT, type GroupColor } from "./pane-groups";

/**
 * THE PEER-IDENTITY TONE ROW, AS ONE CONTROL — the neutral plus the tones, in one row.
 *
 * It was written inside `group-panel.tsx` and had exactly one caller until the commands modal grew
 * a folder DETAIL PANEL (finding 01M132XFZ5KY8QMJ79HZZSBV3J): the master/detail settles a group's
 * name and colour IN the panel rather than in a second modal opened over the first, so the swatch
 * row now has two homes. It is extracted rather than copied, because a second row of swatches is a
 * second answer to "which colours does a group have" — and `GROUP_COLOR_COUNT` exists precisely so
 * there is only one.
 *
 * ## It serves BOTH consumers of the palette, and that is the point (ADR-0012)
 *
 * `--group-N` became `--identity-N` because groups and PROJECTS do the same thing — distinguish
 * peers without ranking them — so they share one vocabulary. The control follows the vocabulary:
 * the project-settings ACCENT picker is this row with thirteen options and the accent's naming, not
 * a second colour picker. What varies between the two callers is only what a swatch MEANS
 * ({@link GroupSwatchesProps.options}: a group stores the 1-based index, a project stores the token
 * NAME `identity-3`) and what it is CALLED ({@link SwatchNaming}). What does NOT vary is the row
 * itself — one tone per `--identity-N`, painted from the stylesheet, in one order.
 *
 * ## The two rules it carries, so no caller restates them
 *
 *  - **THE FIRST SWATCH IS NOT A COLOUR.** It is the ABSENCE of a choice — the neutral
 *    `--identity-0` — and it comes first because it is the DEFAULT. Taking it back is how a colour
 *    is removed. `options[0]` is therefore `null` for every caller, and that `null` is the value
 *    emitted: the eraser writes the absence, never the neutral's own name.
 *  - **A NO-OP IS NOT EMITTED.** Pressing the swatch a group already wears calls nothing at all,
 *    which is what makes "open it, look at it, close it" leave the model untouched.
 */

/**
 * The choices a command GROUP offers — the neutral first, then its six colours. EXPORTED and passed
 * explicitly by the two group call-sites: it used to be a default, but a default that only holds for
 * `T = GroupColor` cannot be expressed in the type system, so it was reached by a cast — and a
 * caller with STRING values who forgot `options` compiled silently, painted the six group colours
 * and emitted NUMBERS typed as strings. Naming it at the call-site closes that door with the
 * signature instead of with a comment.
 *
 * Six, not twelve: `GROUP_COLOR_COUNT` is the bound of ONE consumer, not the size of the palette
 * (see its comment in `pane-groups.ts`). Widening it here alone would paint a colour the write path
 * then rejects.
 */
export const GROUP_OPTIONS: readonly (GroupColor | null)[] = [
  null,
  ...Array.from({ length: GROUP_COLOR_COUNT }, (_, i) => (i + 1) as GroupColor),
];

/**
 * How a row NAMES itself and its swatches — the ONLY thing a second consumer of the palette has to
 * say, beyond what its swatches are worth.
 *
 * A tone has no name: the palette is hues at one lightness and one chroma, so "Accent 7" is
 * honest and "Sage" would not be — no invented colour names, here or anywhere.
 */
export interface SwatchNaming {
  /** Accessible name of the ROW (`role="group"`). */
  row: string;
  /** Accessible name of the first swatch — the one that ERASES. */
  none: string;
  /** Accessible name of the tone at 1-based `index`. */
  tone: (index: number) => string;
  /** Optional `title` for the swatch at `index` (`0` = the eraser). The accent row puts the stored
   *  TOKEN here, which is the only place a human can read what a choice is actually called. */
  title?: (index: number) => string;
}

/** The group-colour vocabulary — the default, so the existing callers read exactly as before. */
const GROUP_NAMING: SwatchNaming = {
  row: "Group colour",
  none: "No colour",
  tone: (index) => `Colour ${index}`,
};

export interface GroupSwatchesProps<T extends string | number> {
  /** The current choice, or `null` for the neutral (the absence of a choice). */
  value: T | null;
  /** Called ONLY when the pressed swatch differs from {@link GroupSwatchesProps.value}. */
  onSelect: (value: T | null) => void;
  /**
   * What each swatch is WORTH, in paint order: `options[i]` is painted with `--identity-i`, and
   * `options[0]` is the ABSENCE — hence `null`, the value the eraser emits.
   *
   * REQUIRED, and it is what fixes `T`: a group passes {@link GROUP_OPTIONS} (the 1-based index),
   * a project passes `PROJECT_ACCENTS`, whose entries are token NAMES and whose head is `null` for
   * exactly this reason. No default — see {@link GROUP_OPTIONS}.
   */
  options: readonly (T | null)[];
  /** Defaults to the group-colour vocabulary. */
  naming?: SwatchNaming;
  /**
   * Greys the whole row out and takes every swatch out of the tab order. For a caller that renders
   * before it knows WHAT it is recolouring — the project pane draws its block with `projectId ==
   * null` — so the row cannot stay the one live control among disabled ones.
   */
  disabled?: boolean;
  className?: string;
}

export function GroupSwatches<T extends string | number>({
  value,
  onSelect,
  options,
  naming = GROUP_NAMING,
  disabled = false,
  className,
}: GroupSwatchesProps<T>) {
  const current = value ?? null;
  const pick = (next: T | null) => {
    if (next !== current) onSelect(next);
  };
  return (
    <div role="group" aria-label={naming.row} className={cn("flex flex-wrap gap-1.5", className)}>
      {options.map((option, index) => (
        <Swatch
          key={index}
          tone={index}
          label={index === 0 ? naming.none : naming.tone(index)}
          title={naming.title?.(index)}
          selected={option === current}
          disabled={disabled}
          onSelect={() => pick(option)}
        />
      ))}
    </div>
  );
}

/**
 * One choice. `tone` is its POSITION in the row, which is also the `--identity-N` it paints — the
 * colour is never passed in, so no caller can put a hue on a swatch the stylesheet does not
 * declare. Tone `0` is the neutral: the first one, and the absence of a choice.
 */
function Swatch({
  tone,
  label,
  title,
  selected,
  disabled,
  onSelect,
}: {
  tone: number;
  label: string;
  title?: string;
  selected: boolean;
  disabled: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={selected}
      title={title}
      disabled={disabled}
      // Named after THIS COMPONENT, not after its consumer: one hook for one control, whichever
      // row it is drawing.
      data-group-swatch={tone === 0 ? "none" : tone}
      onClick={onSelect}
      style={{ "--group-tone": `var(--identity-${tone})` } as CSSProperties}
      className={cn(
        "size-5 cursor-pointer rounded-md bg-[var(--group-tone)] transition-shadow",
        "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring",
        "disabled:pointer-events-none disabled:opacity-64",
        selected && "ring-2 ring-[var(--group-tone)] ring-offset-2 ring-offset-popover",
      )}
    />
  );
}
