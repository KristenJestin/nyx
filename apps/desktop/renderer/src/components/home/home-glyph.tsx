import type { ComponentType } from "react";

import { cn } from "@/lib/utils";
import { ProjectMark } from "@/components/sidebar/project-mark";
import { StatusDot, type RunState } from "@/components/ui/status-dot";

import type { HomeProject } from "./types";

/**
 * `<HomeGlyph>` — the LEADING GLYPH of a home row, and the anchor of its two corner marks.
 *
 * It carries three things and their placement is the contract:
 *
 *   * THE GLYPH ITSELF says the row's TYPE and only its type — a provider brand logo for an agent
 *     row, `SquareTerminal` for a shell, `ServerCog` for a service — exactly as the sidebar
 *     resolves it. Nothing here encodes a state.
 *   * THE STATE DOT rides the BOTTOM-RIGHT corner, the same corner the sidebar line hangs its
 *     pastille off, and it is the app's own `<StatusDot>`: so a QUIET state renders NO dot at all
 *     (`isQuietState`), which is why an idle row on the home carries none. It is the ONLY state
 *     signal on the row — no badge repeats it.
 *   * THE PROJECT MARK rides the TOP-LEFT corner — the OPPOSITE corner, a rounded SQUARE against
 *     the dot's circle, on the cross-project home only. It is `<ProjectMark>`, the same component
 *     the rail tile paints, so a project's identity (and its initial-on-neutral fallback) is drawn
 *     once — and it is rendered `labelled`, which is that component's own way of naming a project
 *     when the mark is the only thing that can. The project's NAME is never written on the row:
 *     it lives in `title`/`aria-label`. A row with no project (a loose terminal) carries NO mark
 *     and stays in the flow.
 *
 *     THIS PLACEMENT IS THE APP'S PRECEDENT for any surface whose items are rows with a glyph —
 *     the five cross-project pages included. See the rule in `sidebar/project-mark.tsx`.
 *
 * Two sizes, because the two families the home draws are not the same object: an agent session
 * gets a TILE (the sidebar's own avatar treatment), a service / bare terminal gets a BARE ICON at
 * the line size the rest of the app uses. The dot follows the glyph it hangs on — the app's size
 * step, not one size forced onto both.
 *
 * ## Why this is NOT `<SidebarLineGlyph>` (review 01M15AVQ65PR74HH8451VXWTRP)
 *
 * `ui/sidebar-line.tsx` declares the corner pastille ONE mechanic, at one set of metrics, so that
 * two files never have anything to align. That declaration is about the SIDEBAR LINE, and the home
 * row is not one — it is the one place in the app where the glyph is an ANCHOR with two occupied
 * corners rather than a lead icon with one. Three things follow, and each of them is a prop
 * `<SidebarLineGlyph>` deliberately does not have:
 *
 *  1. THE RING TAKES THE SURFACE BEHIND IT. The pastille is ringed in whatever it sits on so it
 *     reads as detached from the glyph: `ring-background` in `LINE_DOT` (the tree sits on the
 *     page), `ring-sidebar` in `sidebar/run-state.tsx` (the rail), `ring-card` here — the home's
 *     rows sit inside a `bg-card` panel. `SidebarLineGlyph` hard-codes its ring, and it is right
 *     to: a line always sits on the same surface. A row that can sit on a second one cannot use it
 *     without turning the one metric into a prop, which is precisely the drift that file forbids.
 *  2. THE DOT FOLLOWS THE GLYPH'S SIZE STEP. `LINE_DOT_SIZE` is one value on purpose — a line has
 *     one glyph size. The home draws TWO (an `size-8` session tile and a `size-4` line glyph), so
 *     an `xs` dot on the tile would read as a speck. Exposing the size would break the very
 *     invariant (`an agent's dot and a folder's rollup are one object`) that the constant states.
 *  3. THE OPPOSITE CORNER IS TAKEN. The cross-project home hangs the `<ProjectMark>` on the
 *     top-left — a rounded SQUARE against the dot's circle. That is a second corner mark on the
 *     same anchor, which the line's glyph has no notion of and no reason to grow one for.
 *
 * What IS reused is everything that can be: `<StatusDot>` paints the pastille (so `isQuietState`
 * and the colour vocabulary are the app's), `<ProjectMark>` paints the identity, and the FAMILY
 * GLYPHS handed in as `icon` are read from `LINE_FAMILY_ICON` by the callers. The placement rule
 * copied here is a placement (bottom-right, ringed in its surface), not a second declaration of
 * the line's geometry: this component draws no band, no height, no inset and no indent.
 */
export function HomeGlyph({
  icon: Icon,
  state,
  project,
  variant = "tile",
  className,
}: {
  /** The TYPE glyph. */
  icon: ComponentType<{ className?: string }>;
  /** The row's state, or `null` for a family that carries none (a bare terminal at rest). */
  state: RunState | null;
  /** The project mark to hang, or `null` — the project home passes `null` on every row. */
  project: HomeProject | null;
  /** `tile` = an agent session's avatar; `icon` = a service / terminal line's bare glyph. */
  variant?: "tile" | "icon";
  className?: string;
}) {
  const tile = variant === "tile";
  return (
    <span
      className={cn(
        "relative inline-flex shrink-0 items-center justify-center text-muted-foreground",
        tile ? "size-8 rounded-md bg-muted/50" : "size-4",
        className,
      )}
    >
      <Icon className="size-4" />
      {project ? (
        // `labelled`: here the mark is the ONLY thing saying whose row this is, so it carries the
        // name itself — a `title` for the pointer and an accessible name for a reader. That is the
        // whole of "the project is named in the title only": no column, no text on the row. The
        // naming lives IN `<ProjectMark>` rather than in a wrapper written here, so the five
        // cross-project pages get it from the component instead of each re-deriving it.
        <ProjectMark
          project={project}
          labelled
          data-testid="home-project-mark"
          className={cn(
            "absolute size-3.5 rounded ring-2 ring-card",
            tile ? "-top-1 -left-1" : "-top-1.5 -left-1.5",
          )}
        />
      ) : null}
      {state ? (
        // The app's line pastille, verbatim in placement (bottom-right, ringed in the surface
        // behind it) and sized off the glyph it hangs on.
        <StatusDot
          state={state}
          size={tile ? "sm" : "xs"}
          className="absolute -right-0.5 -bottom-0.5 rounded-full ring-2 ring-card"
        />
      ) : null}
    </span>
  );
}
