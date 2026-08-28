import { cloneElement, type ReactElement, type ReactNode } from "react";
import { FolderIcon, FolderOpenIcon, ServerCogIcon, SquareTerminalIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { StatusDot, type RunState } from "./status-dot";

/**
 * `<SidebarLine>` — THE line of the runtime tree. Not a base class with a family switch: a set of
 * slots every family FILLS.
 *
 * ## Why there is only one of these (human review 01M12MW1DV1V455EWBMV048TE6)
 *
 * Verbatim: « fait en carrément un composant commun avec des customisation si c'est un terminal, un
 * agent, un dossier, une commande etc ». The tree draws six things that are all the same object —
 * a terminal, an agent (a terminal wearing a provider glyph), a managed command, a folder of
 * terminals, a folder of commands, a worktree — and they were six pieces of geometry that HAPPENED
 * to agree. The parity the group folder needed with a terminal row is not something to maintain
 * between two files; it is something to make structural, so there is nothing left to align.
 *
 * ## What "one place decides" means here, exactly
 *
 * The height, the glyph size, the insets, the gap, the per-level indent and the hover tint are the
 * constants in this file and appear NOWHERE else. They are class strings and not pixel values on
 * purpose: two files reading a shared `32` still agree about a number, and numbers drift. Two files
 * rendering the same component do not.
 *
 * And nothing here is a target. Verbatim again: « c'est pas forcément 32 ». A line is as tall as
 * {@link LINE_BAND}'s padding plus {@link LINE_SURFACE}'s type scale make it; retune those and every
 * family follows on the same commit. The spec that guards this measures families AGAINST EACH OTHER
 * (`sidebar-line.browser.test.tsx`), never against a literal, so it survives a legitimate retune and
 * still fails the day one family stops following.
 *
 * ## Composition, not a `variant` prop
 *
 * A `variant: "terminal" | "folder" | …` would move the problem into a switch that the next family
 * lengthens. Instead:
 *
 *  - {@link SidebarLine} is the SURFACE — radius, type scale, hover/selection tint, trailing gutter;
 *  - {@link SidebarLineBand} is the GEOMETRY — glyph, gap, insets, padding. Its `render` prop swaps
 *    the ELEMENT (a `<button>` for a line you select, a `<div>` for a head that contains buttons),
 *    which is a structural need and not a family branch;
 *  - {@link SidebarLineGlyph} is the LEAD — the icon and its corner pastille;
 *  - {@link SidebarLineLabel} is the NAME;
 *  - {@link SidebarLineActions} is the trailing cluster, revealed on hover / focus-visible.
 *
 * A terminal is this with a terminal glyph and a select-button band. An agent is the same line whose
 * glyph happens to be a provider logo and whose pastille happens to be yellow. A folder is this with
 * a `Folder` glyph that is also a drag handle and a fold toggle. A command is this with three
 * lifecycle buttons in the trailing slot. None of them knows about the others.
 *
 * ## The pastille is part of the line, not a per-family garnish
 *
 * There is ONE corner-pastille mechanic ({@link SidebarLineGlyph}'s `state`) with one set of
 * metrics, and ONE display rule: `isQuietState` (an `idle`/`closed` state renders nothing at all —
 * see `status-dot.tsx`), plus {@link rollupState} for any node that can hide its children. A closed
 * node shows its children's aggregate; an open one shows nothing, because its children each show
 * their own. That was the worktree accordion's rule; the folder INHERITS it here instead of
 * restating it.
 *
 * ## Where it lives
 *
 * `components/ui/`, with `status-dot.tsx`, because both the terminals' sidebar (`terminals/`) and
 * the commands modal (`command/`, through `panes/group-block.tsx`) draw these lines, and
 * `command/` → `terminals/` is the import edge finding 01M0ZMHM9GSC39SCN2DBRQ8VX3 closed. `ui/` is
 * the floor everything already stands on.
 */

/**
 * THE SURFACE — the radius the tint is painted with, the TYPE SCALE (half of where a line's height
 * comes from), and the gutter the trailing controls sit in.
 */
const LINE_SURFACE = "group flex items-center gap-1 rounded-md pr-1 text-sm transition-colors";

/** The hover tint of a line. A group block's own hover fill deliberately ADDS to this one. */
const LINE_HOVER = "hover:bg-muted/60";

/** The selected tint — the flat one, so a selected line never reads as a second hover. */
const LINE_SELECTED = "bg-muted";

/**
 * THE BAND — the geometry: the lead glyph, the gap to the label, the left inset, the vertical
 * padding. Together with the surface's type scale this is what SETS the height. Nothing anywhere
 * states that height.
 */
const LINE_BAND = "flex min-w-0 flex-1 items-center gap-2.5 px-2 py-1.5";

/** The lead glyph's size — one class for every icon that opens a line, folders included. */
export const LINE_GLYPH_SIZE = "size-4";

/**
 * THE FAMILY GLYPHS — one source, exactly like the template and the pastille.
 *
 * ## Why they are here (human review 01M140VRZJG0RE5VMG91D8TH3T)
 *
 * Verbatim: « Dans les settings commande, j'ai pas la même icons que dans la sidebar ? ». A managed
 * command was drawn with `ServerCogIcon` in the sidebar and with `TerminalIcon` in the settings
 * catalogue — the same object wearing two signs depending on which screen you looked at. That is the
 * very drift this file exists to make impossible: the height, the insets and the pastille all have
 * ONE declaration, and a family's GLYPH is a presentation decision of exactly the same rank.
 *
 * ## Why the COMMAND is `ServerCogIcon` and not a terminal glyph
 *
 * It is chosen for agreement with the family it sits among, not for being the one that happened to
 * be there. In the sidebar a terminal already reads `SquareTerminalIcon` and an agent reads its
 * provider logo; a command drawn as a bare terminal would be a second, near-identical terminal glyph
 * on a list whose whole job is telling the two apart — and the sidebar and the Spec runtime tab
 * (`specs/tabs/runtime-tab.tsx`) already agree on `ServerCogIcon` for a command, against the
 * catalogue alone. So the catalogue is the outlier and it is the one that moves.
 *
 * ## Shape
 *
 * A record of icon COMPONENTS rather than a `variant` prop: nothing here switches on a family, a
 * caller simply reads the one glyph its family owns. {@link SidebarLineFamilyGlyph} is the
 * convenience that also applies {@link LINE_GLYPH_SIZE}, so a family cannot pick a size either.
 */
export const LINE_FAMILY_ICON = {
  terminal: SquareTerminalIcon,
  command: ServerCogIcon,
  folder: FolderIcon,
  "folder-open": FolderOpenIcon,
} as const;

/** A family of the runtime tree — the key of {@link LINE_FAMILY_ICON}. */
export type SidebarLineFamily = keyof typeof LINE_FAMILY_ICON;

/** A family's glyph, at the line's own size. The one way to draw one. */
export function SidebarLineFamilyGlyph({
  family,
  className,
}: {
  family: SidebarLineFamily;
  className?: string;
}) {
  const Icon = LINE_FAMILY_ICON[family];
  return <Icon className={cn(LINE_GLYPH_SIZE, className)} />;
}

/** The glyph's box, and the anchor its corner pastille hangs off. */
const LINE_GLYPH = "relative inline-flex shrink-0 text-muted-foreground";

/** The corner pastille: hung off the glyph's bottom-right, ringed in the surface behind it. */
const LINE_DOT = "-right-0.5 -bottom-0.5 absolute rounded-full ring-2 ring-background";

/** The pastille's size step. One value, so an agent's dot and a folder's rollup are one object. */
const LINE_DOT_SIZE = "xs" as const;

/**
 * A FULL-SIZE POINTER TARGET THAT DOES NOT DICTATE THE BAND — what the group head's `…` already did
 * (review 01M0BSNTEN5N2GC9QQBASJYHHB) and what a folder glyph needs, since it is a button.
 *
 * The design system's smallest icon button is the floor for a pointer target; {@link LINE_GLYPH_SIZE}
 * is what the band reserves for a glyph. `-m-1` reconciles them: the button keeps its whole box and
 * OVERFLOWS its layout by that much on every side, so its glyph lands where a plain line's icon
 * lands and the label after it at the same offset. The reconciliation is CHECKED by measurement, not
 * trusted to arithmetic — the parity spec compares the offsets in a real browser.
 *
 * The two `[&_svg]` resets undo what `buttonVariants` does to any icon it wraps: a `-mx-0.5` that
 * would shrink the glyph's box (dragging the pastille off a line's) and an `opacity-80` a group's
 * folder must not wear (it carries the group's tone at full strength).
 */
export const LINE_GLYPH_TARGET = "size-6 -m-1 [&_svg]:mx-0 [&_svg]:opacity-100";

/**
 * THE INDENT of a node's children, and the x of the GUIDE LINE that joins them.
 *
 * Both are functions of the band: the guide sits on the parent glyph's centre axis, i.e. the band's
 * left inset plus half a glyph. Tailwind classes are static so the arithmetic is not re-derived at
 * runtime; it is CHECKED, by measuring the guide against a real folder's real centre.
 */
export const LINE_CHILD_INDENT = "pl-5";
export const LINE_GUIDE_X = "left-4";

/**
 * THE ROLLUP RULE of a node that can hide its children — one expression, read by every such node.
 *
 * OPEN ⇒ the children are on screen and each carries its own pastille, so the aggregate would be a
 * duplicate: the node shows nothing. CLOSED ⇒ the children's dots are gone with them, so the node
 * carries their aggregate. Folding it to `idle` rather than to `undefined` is what makes it ride
 * `isQuietState` for free — the same reason the workspace accordion has always written it this way.
 */
export function rollupState(open: boolean, rollup: RunState): RunState {
  return open ? "idle" : rollup;
}

/**
 * THE LEAD GLYPH and its corner pastille — the one mechanic, at the one set of metrics.
 *
 * `state` omitted ⇒ no pastille at all (a line with no run state to carry). A quiet state renders
 * nothing either, by `isQuietState` inside `<StatusDot>` — so an idle line is a clean line.
 */
export function SidebarLineGlyph({
  icon,
  state,
  className,
}: {
  icon: ReactNode;
  state?: RunState;
  className?: string;
}) {
  return (
    <span className={cn(LINE_GLYPH, className)}>
      {icon}
      {state === undefined ? null : (
        <StatusDot state={state} size={LINE_DOT_SIZE} className={LINE_DOT} />
      )}
    </span>
  );
}

/**
 * A line with NO name — an inert spacer that still HOLDS THE BAND OPEN to a full line.
 *
 * A group nobody named writes nothing where a name would be (human review 01M0CY3F075ACP4TFBK1KJ49X8),
 * and a band whose height came from its text would then be shorter than every other line — which is
 * exactly what happened, and what `group-block.browser.test.tsx` caught: 28 px unnamed against 32 px
 * named. `1lh` is one line-height OF THE BAND'S OWN TYPE SCALE, so the spacer is as tall as the
 * label it replaces without either of them naming a number; retune the scale and both follow.
 *
 * It carries no text node at all, so a head is still `textContent === ""` and no reader, human or
 * assistive, is told a word that was never written.
 */
export function SidebarLineLabelSpacer() {
  return <span aria-hidden className="min-h-[1lh] min-w-0 flex-1" />;
}

/** The line's NAME — takes the width it can and ellipsizes rather than pushing the line wider. */
export function SidebarLineLabel({
  children,
  className,
  onDoubleClick,
}: {
  children: ReactNode;
  className?: string;
  onDoubleClick?: () => void;
}) {
  return (
    <span className={cn("min-w-0 flex-1 truncate", className)} onDoubleClick={onDoubleClick}>
      {children}
    </span>
  );
}

/**
 * THE BAND — everything that sets the line's geometry, and the only thing that does.
 *
 * `render` swaps the ELEMENT the band is: a `<button>` where the whole band selects the line, a
 * plain `<div>` where the band contains its own buttons (a folder head has a folder, a `…` and
 * possibly three lifecycle controls inside it, and interactive content may not nest in a button).
 * It is the design system's own `render` idiom (`<Button render={…}>`), not a family branch: what
 * varies is the tag, never the geometry.
 */
export function SidebarLineBand({
  render,
  className,
  children,
  ...rest
}: {
  render?: ReactElement<Record<string, unknown>>;
  className?: string;
  children: ReactNode;
} & React.HTMLAttributes<HTMLElement>) {
  const merged = cn(LINE_BAND, className);
  if (render) {
    // `rest` FIRST, the element's own props second. A band is routinely handed to a wrapper that
    // needs to reach the real DOM node — Base UI's `<Tooltip.Trigger render={…}>` clones this
    // element with the `id`, `aria-describedby` and hover/focus handlers that make the tooltip
    // work. Dropping them silently is how a row's CPU/RAM tooltip stops opening while every unit
    // test that only reads the DOM keeps passing, so they are forwarded; the element's own
    // `onClick`/`type`/`aria-expanded` still win, since nothing outside supplies those.
    return cloneElement(render, {
      ...rest,
      ...render.props,
      className: cn(merged, render.props.className as string | undefined),
      children,
    });
  }
  return (
    <div className={merged} {...rest}>
      {children}
    </div>
  );
}

/**
 * THE TRAILING CLUSTER — a line's own controls, at the end, outside the band so they never eat the
 * label's width.
 *
 * ## THE REVEAL IS THE LINE'S, NOT THE FAMILY'S (human review 01M13118SE5M04J4M9SJGZYAAT)
 *
 * Verbatim, twice, about two different families: « pour une commande il faut ça que suive la même
 * logique les boutons de play pose relauch ne doivent apparaître que au hover sur le groupe » and
 * « comme pour les actions terminaux, le 3 petits points dossier doit apparaître que au hover ». A
 * terminal row already hid its rename/close until hover; a folder head showed its `…` and a command
 * group's three lifecycle buttons permanently. That was a PRESENTATION DECISION taken twice, which
 * is the very thing this component exists to make impossible — so the reveal is no longer a `reveal`
 * prop a family opts into, it is what a trailing cluster IS. There is nothing left to opt out of and
 * nothing left for a seventh family to forget.
 *
 * ## The two things it must not cost
 *
 *  1. **THE TEMPLATE MUST NOT JUMP.** The cluster is hidden with OPACITY and never leaves the flow,
 *     so a line is exactly as tall and its label starts at exactly the same x whether the controls
 *     are showing or not. `sidebar-line.browser.test.tsx` measures rest against hover rather than
 *     trusting the class: `hidden`/`w-0` would read the same in the DOM and move the line.
 *  2. **THE KEYBOARD MUST STILL REACH IT.** `opacity: 0` removes no element from the tab order and
 *     no name from the accessibility tree, so the buttons are still tabbable — and
 *     `has-[:focus-visible]` shows them the moment the keyboard lands on one, exactly as the pointer
 *     does. An action only a mouse can reach would be a regression, so the reveal answers to BOTH.
 *
 * Three triggers, one expression: the line is HOVERED, the line is the VIEWED one (a selected row
 * keeps its controls out), or the KEYBOARD is inside the cluster.
 */
export function SidebarLineActions({
  children,
  className,
  ...rest
}: {
  children: ReactNode;
  className?: string;
} & React.HTMLAttributes<HTMLSpanElement>) {
  return (
    <span
      // The structural marker every family's cluster carries, so the spec that measures the reveal
      // can find it WITHOUT knowing which family it is looking at — which is the only way an
      // assertion of "one mechanic" can be honest.
      data-sidebar-line-actions=""
      className={cn(
        "flex shrink-0 items-center gap-0.5",
        "opacity-0 transition-opacity",
        // The `group` these two key off is {@link LINE_SURFACE}'s — the line itself, so hovering a
        // line reveals ITS controls and nobody else's. A member row inside a hovered block keeps its
        // own controls hidden, which is what "the same logic as a terminal row" has always meant.
        "group-hover:opacity-100 group-data-[selected=true]:opacity-100",
        // …AND THE KEYBOARD, at the same rank as the pointer.
        "has-[:focus-visible]:opacity-100",
        className,
      )}
      {...rest}
    >
      {children}
    </span>
  );
}

/**
 * THE LINE ITSELF — the surface every family sits on.
 *
 * It carries the tint (hover, or the flat selected fill), the radius, the type scale and the
 * `group` the trailing reveal keys off. Everything about WHAT the line is — which glyph, which
 * pastille, which controls, whether the band selects or folds — is composed by the caller.
 */
export function SidebarLine({
  children,
  actions,
  selected,
  className,
  ref,
  ...rest
}: {
  /** The band (and anything else the surface holds before the trailing cluster). */
  children: ReactNode;
  /** The trailing controls, already wrapped in {@link SidebarLineActions}. */
  actions?: ReactNode;
  /**
   * The element ref — a drag-sortable line hands dnd-kit's own here, since the LINE is the sortable
   * item rather than a wrapper around it (React 19 passes `ref` as an ordinary prop).
   */
  ref?: React.Ref<HTMLDivElement>;
  /**
   * Whether the line is the viewed one. `undefined` for a line that is not selectable at all (a
   * head), which is why it is not defaulted to `false`: `data-selected` is then absent rather than
   * asserting "not selected" about something that cannot be.
   */
  selected?: boolean;
  className?: string;
} & React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      ref={ref}
      data-sidebar-line=""
      data-selected={selected}
      className={cn(LINE_SURFACE, selected ? LINE_SELECTED : LINE_HOVER, className)}
      {...rest}
    >
      {children}
      {actions}
    </div>
  );
}
