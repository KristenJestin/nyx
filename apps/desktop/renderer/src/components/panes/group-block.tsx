import type { CSSProperties, ReactNode } from "react";
import { MoreHorizontalIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { CollapsibleSection } from "@/components/sidebar/collapsible-section";
import { DRAG_INERT_ATTR } from "./reorder-dnd";
import {
  LINE_CHILD_INDENT,
  LINE_GLYPH_TARGET,
  LINE_GUIDE_X,
  rollupState,
  SidebarLine,
  SidebarLineActions,
  SidebarLineBand,
  SidebarLineFamilyGlyph,
  SidebarLineGlyph,
  SidebarLineLabel,
  SidebarLineLabelSpacer,
} from "@/components/ui/sidebar-line";
import type { RunState } from "@/components/ui/status-dot";
import type { GroupColor } from "./pane-groups";

/**
 * THE GROUP TREATMENT — one shared block, for terminals AND for commands.
 *
 * ## The rule the user posed (review 01M0ABR6V501D0SPR2BYBVZC4A), literally
 *
 *  - ONE LIST. There is no "Solo" / "Groups" section: every terminal stays in TERMINALS and every
 *    command stays in COMMANDS. A terminal joining a group does not change SECTION, it changes
 *    APPEARANCE. {@link groupListBlocks} is what keeps that true — it re-orders nothing into a
 *    second list, it only makes a group's members contiguous inside the one list.
 *  - THE DEFAULT IS GREY (`--group-0`, the design system's existing neutral). The block DELIMITS
 *    without HIERARCHISING: a solo terminal must not look secondary next to a group. This is why
 *    every tint is a `color-mix` against `transparent` rather than a flat surface token: it rides
 *    whatever the sidebar is painted with.
 *
 * ## THE BLOCK IS A FOLDER (human review 01M12MW1DV1V455EWBMV048TE6, maquette
 * `.shots/design/group-folder-v3.html`)
 *
 * Four things changed at once, and they are one idea: **the head is a LINE, and the block is a
 * tree node**.
 *
 *  1. **The grip became a FOLDER** — `FolderIcon`/`FolderOpenIcon`, at the line's own glyph size.
 *     It is the block's handle AND its toggle (see "one object, two gestures" below).
 *  2. **The head became a line** — and that is a PARITY, not a size. Verbatim: « l'idée est de dire
 *     que la ligne qui montre un dossier a la même taille qu'un terminal, l'icône même taille etc,
 *     c'est pas forcément 32 ». So this file states no height, no inset and no glyph size: it reads
 *     `<SidebarLine>` — the very component `<RuntimeRow>` composes — so whatever a line measures,
 *     the head measures. The name lost its uppercase `MicroLabel` and became an ordinary label.
 *  3. **The members are INDENTED** under the folder, joined by a 1 px guide line on the folder's own
 *     centre axis and stopping at the last row.
 *  4. **The folder opens and closes**, and remembers.
 *
 * The head is TALLER than the 20 px band it replaces, and that is the price of the parity rather
 * than a number anybody picked. What it buys: the block reads as an arborescence — a parent node OF
 * THE SAME RANK as its members, which when closed REPLACES them instead of adding to them. A closed
 * group of four commands costs one line where it used to cost five.
 *
 * ## DELIMITATION: no border, no fill at rest (variant B+)
 *
 * The group is said by its FOLDER, by the INDENT and by the GUIDE LINE — three signals a tree
 * reader already knows — so the border and the resting fill are gone. They were redundant with the
 * indent and they broke the very rule this file opens on: a frame plus a fill hierarchises, and a
 * solo row next to one reads as secondary.
 *
 * The tinted fill survives AT HOVER ONLY, over the whole block: hovering any member reveals how far
 * the group reaches, and at rest the sidebar stays flat so the block's tint never competes with the
 * rows' own hover/selection tint (the same visual register carrying two different meanings was
 * exactly variant C's failure). The two tints ADD UP on the hovered row — a line inside a hovered
 * block is a shade lighter than a solo line — and that is wanted, not tolerated: it reads as "this
 * line, IN this group". `terminals/sidebar-line.browser.test.tsx` measures it so nobody "fixes" it later.
 *
 * A FRAME IS NOW RESERVED FOR THE TRANSIENT DRAG STATES — armed, refused, dissolving. Drawing a box
 * only there is what makes the box mean something, and they are drawn as RINGS/OUTLINES so arming a
 * block never moves a pixel of the list under the pointer.
 *
 * ## ONE SURFACE, THREE GESTURES — and no arbitrage to write
 *
 * THE LINE SELECTS, THE GLYPH FOLDS, THE LINE DRAGS (human review 01M14112THEB4094B2G614SXR5, over
 * 01M1312FSV6MAR0P63SEG2RF6C — « c'est toute la ligne qui permet de drag and drop le truc »). The
 * head used to do ONE thing, fold, and that is what made a folder unactivable: to see a group you
 * had to open it and then click a member — « c'est trop chelou que je doive cliquer que un des 3
 * pour avoir le 3 ». The arborescence's own motif replaces it: the NAME is selected, the GLYPH is
 * unfolded.
 *
 * Nothing has to disambiguate the three. The shared dnd-kit sensor only arms a drag past 5 px of
 * movement (`reorder-dnd.ts`), so a pointerdown → pointerup that never moved is not a drag, it is a
 * click, and the click selects; the glyph is a button carrying its own click, which it stops from
 * bubbling. At the keyboard the split is the same one the sensor already makes: SPACE is swallowed
 * by the keyboard sensor to pick the block up, ENTER activates the folder button and folds it — and
 * because dnd-kit binds to `handle ?? element` and lets the event bubble, the folder remains that
 * keyboard entry even though it is no longer the handle.
 *
 * FOLDING AND SELECTION ARE INDEPENDENT AXES, which is what makes the widening safe: the maquette
 * already wrote « le pliage ne ferme rien : ni PTY, ni panneau, ni tiling », so folding an ACTIVE
 * folder leaves its group on screen and the head then wears its selected fill AND its rollup dot at
 * once — the most useful state of the device, not a corner of it.
 *
 * The `…` and the command controls `stopPropagation`, exactly as they already did, so pressing them
 * neither folds nor lifts nor selects.
 *
 * There is NO separate chevron: an open folder already draws its own state, and a chevron would
 * cost 14 px of head to say what the glyph says. And the folder carries NO hover state of its own
 * (human review 01M1311AQJ3GNDBS51TP4P9YWE): the line's hover already says the head is live, a
 * second one on the glyph added noise, and it diverged between the two families — which is proof
 * nobody had decided it.
 *
 * ## THE HEAD CARRIES A STATE, NEVER A NUMBER
 *
 * No member count, and no `n/m`. What a closed folder carries instead is the ROLLUP DOT, in the
 * corner of the folder glyph — the LINE'S OWN pastille (`SidebarLineGlyph`), not a copy of it —
 * shown while CLOSED, erased while OPEN, and subject to `isQuietState` like every other dot in the
 * app. It is the worktree accordion's rule, verbatim, and it is REQUIRED by the folding: a closed
 * folder hides its members' dots, so an agent waiting on an answer inside one would otherwise go
 * invisible. That is why both families get it, not just the commands.
 *
 * `commandGroupTally` is still computed — the three group controls read `canStart`/`canStop`/
 * `canRelaunch` off it — only its DISPLAY is gone. What the `n/m` said about a DIVERGENT group
 * comes back in the folder's {@link summary} tooltip, which costs no head pixel at all.
 *
 * ## NO NAME, NO WORDS (human review 01M0CY3F075ACP4TFBK1KJ49X8)
 *
 * Verbatim: « le message unnamed group non, si pas de nom, on affiche rien ». A group with no name
 * shows NOTHING where a name would be — no stand-in, no "group", no dash, no dimmed text. The label
 * zone becomes an inert spacer so nothing moves when the words go, and what tells a named group
 * from an anonymous one is `data-group-head-label` (`named` | `unnamed`), NEVER the string: a group
 * really called "unnamed group" is a NAMED group and must read as one. A folder without a name is
 * still a folder — which is precisely what a grip could never be.
 */
export function GroupBlock({
  color,
  name,
  groupId,
  children,
  className,
  onOpenPanel,
  containerRef,
  handleRef,
  actions,
  summary,
  open = true,
  onToggle,
  onSelect,
  selected,
  rollup,
  dragging = false,
  armed = false,
  refused = false,
  dissolving = false,
}: {
  /** The group's chosen colour, or `null` — the DEFAULT, which is the neutral, not a colour. */
  color: GroupColor | null;
  /** The group's optional name. `null`/blank renders NO label at all — see "NO NAME, NO WORDS". */
  name: string | null;
  groupId: string;
  /** The member rows, in list order. Rendered inside the indented, guide-lined body. */
  children: ReactNode;
  className?: string;
  /**
   * Open the shared name + colour panel on this group. The head's `…` is the ONE entry point: a
   * right-click on a member does not open it any more, and neither does the row's overflow menu.
   */
  onOpenPanel?: () => void;
  /** The sortable's element ref — the block IS the draggable item, not a wrapper around it. */
  containerRef?: (element: HTMLElement | null) => void;
  /**
   * The sortable's handle ref — THE WHOLE HEAD is what a pointer grabs the block by (human review
   * 01M1312FSV6MAR0P63SEG2RF6C), the same way a terminal row is grabbed anywhere on its surface.
   * Absent on the surfaces where a block does not travel (the commands SIDEBAR).
   *
   * It no longer decides the folder's CURSOR (human review 01M140TP2F7X0DGMRC0RQQFHAF): the glyph is
   * a toggle on every surface and wears a toggle's cursor on every surface. What it still decides is
   * the folder's accessible NAME, which states the keyboard drag only where the drag exists.
   */
  handleRef?: (element: HTMLElement | null) => void;
  /**
   * What the head carries at its END, before the `…` — the group's own lifecycle controls
   * (a command group's play/stop/restart). A terminal group has none: a terminal is open or closed,
   * it does not restart in bulk, and its composition happens at the pointer in the sidebar instead.
   *
   * They are REVEALED ON HOVER, like every other line's trailing controls — and that is not stated
   * here, it is `<SidebarLineActions>`'s (human review 01M13118SE5M04J4M9SJGZYAAT). A head is not a
   * special case of a line; it is a line.
   */
  actions?: ReactNode;
  /**
   * The folder's hover summary — where a number goes now that the head carries none. A command
   * group puts its `n/m` here, so "2 of 3 running" is one hover away instead of one glance, which
   * is the compensation the head's alignment was bought with.
   */
  summary?: ReactNode;
  /** Whether the members are shown. Controlled: the caller owns (and persists) the state. */
  open?: boolean;
  /** Fold / unfold. Absent ⇒ the head is inert (a block that does not collapse). */
  onToggle?: () => void;
  /**
   * ACTIVATE THIS FOLDER — show its group, exactly as clicking a member shows it (human review
   * 01M14112THEB4094B2G614SXR5). It is the commands modal's master/detail selection (finding
   * 01M132XFZ5KY8QMJ79HZZSBV3J) AND, since that review, the sidebar's own activation: a folder is a
   * node of the tree and a node of a tree is selectable.
   *
   * `undefined` on a surface with no selection at all — the head then keeps folding on click, which
   * is what it did before there was anything to select.
   *
   * THREE GESTURES COEXIST ON THE HEAD WITH NOTHING TO ARBITRATE. The shared sensor arms a drag past
   * 5 px (`reorder-dnd.ts`), so a press that never moved is a click and the click selects; a press
   * that moved is a drag; and the glyph keeps its own click for the fold, which it stops from
   * bubbling. None of the three had to be told about the others.
   */
  onSelect?: () => void;
  /**
   * Whether this block is the viewed one. `undefined` on a surface with no selection at all, so
   * `data-selected` is ABSENT there rather than asserting "not selected" about something that
   * cannot be — the same rule `<SidebarLine>` states for its own families.
   */
  selected?: boolean;
  /**
   * The aggregate state of the members, drawn as the corner pastille of the CLOSED folder. The
   * STATE is handed in — the two families aggregate different things — but the pastille itself, its
   * metrics and the open/closed rule belong to the line ({@link rollupState}), so a folder's rollup
   * and an agent line's dot are one object rather than two that resemble each other. `undefined` ⇒
   * this block carries no state at all.
   */
  rollup?: RunState;
  /** The block is the drag SOURCE right now (it is travelling). */
  dragging?: boolean;
  /** ARMED: the drop being aimed will land INSIDE this block (gestures 1 and 3). */
  armed?: boolean;
  /** REFUSED: the drop would land inside, but the 180×120px floor forbids it. */
  refused?: boolean;
  /** DISSOLVING: this two-member block is about to lose one and disappear with it. */
  dissolving?: boolean;
}) {
  const trimmed = name?.trim() ?? "";
  const named = trimmed.length > 0;
  /** Whether this block TRAVELS — i.e. whether the surface handed a sortable handle in at all. */
  const draggable = handleRef != null;
  // The folder's accessible name follows the same rule as the eye: it DESIGNATES the group by its
  // name when it has one, and says what it does when it has none. No invented noun ever reaches a
  // screen reader either. It also states BOTH of its jobs where it really has both — the drag is
  // taken on the whole head now, but the folder is still the KEYBOARD entry to it (see the head's
  // `ref` below), so the name still owes the reader both words.
  const what = draggable ? "Toggle and reorder group" : "Toggle group";
  const folderLabel = named ? `${what} ${trimmed}` : what;
  const menuLabel = named ? `Group options for ${trimmed}` : "Group options";
  // The folder's two faces, read from the LINE's own family registry rather than named here — a
  // family's glyph has one source, at the same rank as the template and the pastille (human review
  // 01M140VRZJG0RE5VMG91D8TH3T).
  const folderFamily = open ? "folder-open" : "folder";

  const folder = (
    <Button
      variant="ghost"
      size="icon-xs"
      data-group-folder={groupId}
      aria-label={folderLabel}
      aria-expanded={open}
      // THE FOLD IS THE GLYPH'S, ALWAYS (human review 01M14112THEB4094B2G614SXR5). It used to be
      // the whole head's, and to move here only on a surface that had something to select — which
      // made the fold a different gesture on two screens drawing the same folder. It is one gesture
      // now: the glyph folds, everywhere, and `stopPropagation` keeps the press from ALSO reaching
      // the head (which would select, or — on a surface with no selection — fold a second time).
      //
      // At the keyboard the split is unchanged: Enter on this button dispatches a real click and
      // folds; Space is swallowed by dnd-kit's keyboard sensor on the enclosing head and lifts the
      // block, because a `keydown` reaches that listener before React's delegated click ever runs.
      onClick={(event) => {
        event.stopPropagation();
        onToggle?.();
      }}
      className={cn(
        // 24 × 24 of TARGET, 16 px of LAYOUT — the glyph therefore lands where a LINE's icon lands
        // (see `LINE_GLYPH_TARGET`), which is the whole alignment in one class.
        LINE_GLYPH_TARGET,
        // The folder carries the group's tone at FULL strength: it is the block's main bearer of
        // the colour now that there is no border and no fill to carry it.
        "text-[var(--group-tone)]",
        // NO HOVER OF ITS OWN (human review 01M1311AQJ3GNDBS51TP4P9YWE) — « j'ai pas l'impression
        // que ça fasse grand chose, à dégager ce hover ». The glyph used to paint a tone-mixed fill
        // under itself on hover, which said nothing the LINE's own hover tint was not already
        // saying and which read differently between the two families. `hover:bg-transparent` is
        // what neutralises it: this is a `<Button variant="ghost">` and the variant brings
        // `hover:bg-accent`, so removing the mix alone would have swapped one glyph-local hover for
        // another rather than removing it. What STAYS is everything the finding kept: the 24×24
        // pointer target above, the cursor below, and the button's role as the fold toggle and the
        // keyboard entry to the drag.
        "hover:bg-transparent",
        // THE CURSOR OF A TOGGLE, IN BOTH FAMILIES (human review 01M140TP2F7X0DGMRC0RQQFHAF) — « sur
        // l'icon dossier y a toujours le mouse drag, là à dégager ». The glyph wore `cursor-grab`
        // wherever the block travelled, from the days when it WAS the handle. Since the grip became
        // the whole head (01M1312FSV6MAR0P63SEG2RF6C) a grab cursor on the glyph alone POINTS AT A
        // ZONE that has no special hold on the gesture, and it says the wrong thing about what the
        // glyph now does: it folds. The commands family already used `pointer`; that is the value,
        // and it is the value for both. The LINE's own cursor is a separate decision and is
        // untouched here.
        "cursor-pointer",
      )}
    >
      {/* THE LINE'S OWN GLYPH, so the rollup pastille is the very object an agent line's pastille
          is — same size, same offset, same ring — rather than a look-alike. `rollupState` is the
          shared rule and not a local `open ? … :` : a CLOSED node carries its children's aggregate,
          an OPEN one carries nothing because its children each carry their own. The workspace
          accordion reads the same expression. */}
      <SidebarLineGlyph
        icon={<SidebarLineFamilyGlyph family={folderFamily} />}
        state={rollup == null ? undefined : rollupState(open, rollup)}
        className="text-current"
      />
    </Button>
  );

  return (
    <div
      ref={containerRef}
      data-group-block={groupId}
      data-group-color={color ?? "none"}
      data-group-open={open}
      data-drop-armed={armed || undefined}
      data-drop-refused={refused || undefined}
      data-drop-dissolving={dissolving || undefined}
      data-dragging={dragging || undefined}
      // `--group-tone` is the ONE place the group's colour enters the block; the folder, the name,
      // the guide line and the hover fill are all mixes of it, so a group is re-tinted by changing
      // a single custom property.
      style={{ "--group-tone": `var(--group-${color ?? 0})` } as CSSProperties}
      className={cn(
        // NO border and NO fill at rest — variant B+. The radius is kept because the hover fill and
        // the drag rings are painted on this box.
        "rounded-md transition-colors",
        "hover:bg-[color-mix(in_oklch,var(--group-tone)_7%,transparent)]",
        // ARMED — the drop lands INSIDE. A RING, not a border: the block must not resize by a pixel
        // when it lights up, or the list moves under the pointer at the exact moment it must not.
        armed && "bg-ring/10 ring-1 ring-ring",
        // REFUSED — the floor forbids the addition. Never armed at the same time: the block must
        // not say "come in" while the drop is being turned away.
        refused && "ring-1 ring-destructive",
        // DISSOLVING — this block is about to lose its second-to-last member and vanish with it.
        // Announced DURING the gesture: the dissolution is irreversible and must not surprise.
        // An `outline` rather than a border, for the same no-reflow reason as the rings.
        dissolving && "opacity-50 outline-1 outline-dashed outline-muted-foreground/60",
        dragging && "opacity-60",
        className,
      )}
    >
      {/* THE HEAD IS A LINE. Its surface, its band and its hover tint are the very constants
          `<RuntimeRow>` reads (`components/ui/row-template.tsx`) — not a copy of them — so the
          band, the inset and the gap can only ever change for both at once — this file states no
          height and no offset of its own.
          It is a `<div>` and not a `<button>`: it contains the folder button, the `…` and (on a
          command group) three more buttons, and interactive content may not nest in a button. The
          keyboard path is the folder itself, which IS a button and IS a tab stop. */}
      <SidebarLine
        // THE WHOLE HEAD IS THE GRIP (human review 01M1312FSV6MAR0P63SEG2RF6C) — « tu as mis le
        // déplacement du groupement seulement sur l'icon, finalement on va suivre la même logique
        // que pour les terminaux c'est toute la ligne qui permet de drag and drop le truc ». The
        // sortable's handle left the folder and covers the head, exactly as a terminal row is
        // grabbed anywhere on its surface.
        //
        // NOTHING ARBITRATES between grabbing and folding, and nothing needs to: the shared sensor
        // only arms a drag past 5 px of movement (`reorder-dnd.ts`), so a pointerdown → pointerup
        // that never moved is a click, and the click folds. That is the very rule the folder alone
        // already lived by; widening the target changes no gesture, only its area. The `…` and the
        // command controls `stopPropagation` on pointerdown, so they neither fold nor lift.
        //
        // THE KEYBOARD SURVIVES THE WIDENING, twice over. dnd-kit binds both sensors to
        // `source.handle ?? source.element` and lets the event BUBBLE, so a Space pressed on the
        // focused folder — still a `<button>`, still a tab stop — reaches this element and lifts the
        // block exactly as before. And dnd-kit's own accessibility plugin gives a non-focusable
        // activator a `tabindex`/`role`, so the head becomes a tab stop in its own right: the entry
        // point was not moved, it was doubled.
        ref={handleRef}
        data-group-head={groupId}
        data-group-head-label={named ? "named" : "unnamed"}
        // THE LINE SELECTS; THE GLYPH FOLDS (human review 01M14112THEB4094B2G614SXR5) — the
        // arborescence's own motif, « le nom sélectionné, le glyphe déplié ». It replaces the
        // maquette's "the whole head band folds", and knowingly: a folder that only folded was
        // never ACTIVABLE, so seeing a group meant opening it and then clicking a member — « c'est
        // trop chelou que je doive cliquer que un des 3 pour avoir le 3 ».
        //
        // FOLDING AND SELECTION ARE TWO INDEPENDENT AXES. Folding closes nothing — no PTY, no
        // panel, no tiling — so folding an ACTIVE folder does not deactivate it: the group stays on
        // screen and the head then carries BOTH its selected fill and its rollup dot (which exists
        // precisely because its members are hidden). That is the useful case, not an edge one: four
        // commands folded cost one line, and you can still see them running and look at them.
        //
        // The fallback is for a surface with NO selection at all (a harness, a component test):
        // there is nothing to select there, so the head keeps folding rather than doing nothing.
        selected={selected}
        onClick={onSelect ?? onToggle}
        className={cn((onSelect ?? onToggle) && "cursor-pointer")}
        actions={
          <SidebarLineActions>
            {actions}
            {onOpenPanel ? (
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={menuLabel}
                // NEVER A DRAG ACTIVATOR — the head IS the sortable's handle since human review
                // 01M1312FSV6MAR0P63SEG2RF6C, so this button now sits INSIDE the surface dnd-kit
                // listens on, at the pointer and at the keyboard alike. The marker is what dnd-kit
                // itself reads (`reorder-dnd.ts`'s `preventActivation`); the React
                // `stopPropagation` below could not do it, because React delegates to the root and
                // arrives after dnd-kit's own listener has already run.
                {...{ [DRAG_INERT_ATTR]: "" }}
                // …and `stopPropagation` for what React DOES own: the click must not bubble to the
                // head and fold the group under the dialog it just opened.
                onPointerDown={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  onOpenPanel();
                }}
              >
                <MoreHorizontalIcon />
              </Button>
            ) : null}
          </SidebarLineActions>
        }
      >
        <SidebarLineBand>
          {summary != null ? (
            <Tooltip label={summary} side="top">
              {folder}
            </Tooltip>
          ) : (
            folder
          )}
          {named ? (
            <SidebarLineLabel
              className={cn(
                // The weight is the ONE thing a head's label adds to a line's — a parent node reads
                // as a parent, the way a worktree name does. Everything else is the line's.
                "font-medium",
                // 88 % of the tone against the foreground — legible as a name first, tinted second.
                "text-[color-mix(in_oklch,var(--group-tone)_88%,var(--foreground))]",
              )}
            >
              {trimmed}
            </SidebarLineLabel>
          ) : (
            // NO NAME → NO LABEL AT ALL. Not an empty label, not a dimmed stand-in: the line's own
            // inert spacer, which holds the band open to a full line so the head does not SHRINK
            // when the words go, and leaves the folder at one end and the `…` at the other exactly
            // where they are on a named group. The identity is carried by `data-group-head-label`.
            <SidebarLineLabelSpacer />
          )}
        </SidebarLineBand>
      </SidebarLine>

      {/* THE BODY — the sidebar's own collapsible, reused as it is (`collapsible-section.tsx`):
          `AnimatePresence` + an animated `height: auto`, the 500/40 spring, `prefers-reduced-motion`
          already honoured. The group therefore folds with the EXACT animation a workspace accordion
          folds with, and not one line of it is written twice. */}
      <CollapsibleSection open={open}>
        <div
          data-group-body={groupId}
          className={cn("relative flex flex-col gap-0.5 py-px", LINE_CHILD_INDENT)}
        >
          {/* THE GUIDE LINE — 1 px at 42 % of the tone, on the folder's own centre axis (8 px of
              band inset + half of a 16 px glyph = 16 px), stopping short of the bottom so it ENDS
              at the last row. It is what says where the group finishes, and everyone can read it
              without being taught. `aria-hidden`: it is a drawing, not content. */}
          <span
            aria-hidden
            data-group-guide={groupId}
            className={cn(
              "pointer-events-none absolute top-0 bottom-1 w-px rounded-full",
              LINE_GUIDE_X,
              "bg-[color-mix(in_oklch,var(--group-tone)_42%,transparent)]",
            )}
          />
          {children}
        </div>
      </CollapsibleSection>
    </div>
  );
}
