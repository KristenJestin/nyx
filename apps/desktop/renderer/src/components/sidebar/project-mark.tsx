import { useState, type ComponentType, type CSSProperties } from "react";
import {
  BoxIcon,
  CircleIcon,
  CodeIcon,
  CompassIcon,
  FlaskConicalIcon,
  FolderIcon,
  GlobeIcon,
  LayersIcon,
  RocketIcon,
  ServerIcon,
  SparklesIcon,
  TerminalIcon,
  WrenchIcon,
} from "lucide-react";

import { cn } from "@/lib/utils";
import type { ProjectRecord } from "./use-projects";

/**
 * `<ProjectMark>` — the square that SAYS WHICH PROJECT, in one glance and without a line of text.
 *
 * The project rail already needed it; the home hors-projet needs it more, because there a session
 * of any project can sit on any row and the mark is the only thing separating them. So the mark is
 * ONE component both surfaces render, not a tile drawn twice.
 *
 * Identity is CHOSEN, never derived (a deterministic hash on the project key was explicitly ruled
 * out): the backend stores an `accent` colour and, in exactly one of two spellings, a mark — an
 * `icon` token or an `image_path`. All three are optional, and the ABSENCE is a first-class case,
 * not a degraded one: a project nobody dressed renders its INITIAL on the neutral tile, which is
 * what every project looked like before this existed.
 *
 * The three sources are resolved in one order — image, then icon, then the initial — and each
 * falls through to the next when it cannot render: an unknown icon token or a picture that fails
 * to load lands on the initial rather than on an empty box or an exception.
 *
 * ## THE RULE — read this before drawing a project anywhere
 *
 * WHEREVER A SURFACE HAS TO SAY WHICH PROJECT AN ITEM BELONGS TO, IT RENDERS `<ProjectMark>`.
 * There is no second project mark in this app and there is not going to be one: no local tinted
 * square, no initial computed by hand, no `<Avatar>` fed a project name. The identity a project
 * chose (`accent` + `icon` / `image_path`, and the initial-on-neutral fallback when it chose
 * nothing) is resolved HERE, once — the moment a page redraws any part of that, the two drift and
 * the same project wears two faces on two screens.
 *
 * This matters more the wider the app's scope goes: in the cross-project scope (`active-project.
 * tsx`) EVERY page can show items from several projects at once, and the mark is the only thing
 * telling them apart.
 *
 * The component owns the IDENTITY and nothing else, which is what lets every surface use it:
 *
 *  - SIZE is the caller's, through `className` — this component sets no dimension, so a 14px
 *    corner mark and a 36px rail tile are the same component with a different class;
 *  - PLACEMENT is the caller's. The precedent set by the homes, for a surface whose items are
 *    ROWS WITH A GLYPH: the mark rides the glyph's TOP-LEFT corner, the state dot rides the
 *    bottom-right, and the project's NAME is never written on the row (`home-glyph.tsx`). A
 *    surface with another anatomy (cards, a grid) places it coherently with that anatomy — and
 *    says so — but never in a column of its own;
 *  - NAMING is `labelled`. A mark is a DRAWING, so it is `aria-hidden` by default and the
 *    surrounding control carries the name (the rail's tile button, the home's `<h1>`). When the
 *    mark IS the only thing naming the project — a corner mark on a row — `labelled` turns it
 *    into a `role="img"` with `title` + `aria-label`, instead of every caller wrapping it in a
 *    hand-written span of its own.
 *
 * If some surface genuinely cannot go through this component, the REASON goes in that file, named
 * (the standing expectation after finding `01M15AVQ65PR74HH8451VXWTRP`). "It was easier" is not
 * one: extend this component instead — that is what it is for, and every consumer gets the
 * extension.
 */

/**
 * The icon tokens a project may pick as its mark. A CURATED registry, the
 * `AGENT_PROVIDERS` reflex: the stored value is an opaque string, so resolving it through a map
 * keeps the bundle honest (no whole icon set pulled in for a value nobody picked) and makes an
 * unknown token a MISS the caller handles rather than a crash. Adding a token is one entry.
 */
export const PROJECT_ICONS: Readonly<Record<string, ComponentType<{ className?: string }>>> = {
  box: BoxIcon,
  circle: CircleIcon,
  code: CodeIcon,
  compass: CompassIcon,
  flask: FlaskConicalIcon,
  folder: FolderIcon,
  globe: GlobeIcon,
  layers: LayersIcon,
  rocket: RocketIcon,
  server: ServerIcon,
  sparkles: SparklesIcon,
  terminal: TerminalIcon,
  wrench: WrenchIcon,
};

/** The uppercase initial shown on a project tile (first letter of its name, else `?`). */
export function projectInitial(name: string): string {
  const trimmed = name.trim();
  return trimmed.length > 0 ? trimmed[0]!.toUpperCase() : "?";
}

/**
 * Resolve a stored icon token to its component, or `undefined` when the token is unknown / absent
 * — the caller then falls through to the next source. Total and side-effect-free.
 */
export function projectIconFor(
  icon: string | null | undefined,
): ComponentType<{ className?: string }> | undefined {
  if (!icon) return undefined;
  return PROJECT_ICONS[icon];
}

/**
 * The stored `image_path` as something an `<img>` can load: an already-schemed value (`http(s):`,
 * `data:`, `file:`) is used as-is, an absolute local path is wrapped in a `file://` URL. Returns
 * `undefined` for an absent/blank value so the caller falls through to the icon/initial.
 */
export function projectImageSrc(imagePath: string | null | undefined): string | undefined {
  const raw = imagePath?.trim();
  if (!raw) return undefined;
  // A SCHEME is at least two characters before the colon — the guard that keeps a Windows drive
  // letter (`D:\…`) from reading as one and being handed to the `<img>` unwrapped.
  if (/^[a-z][a-z0-9+.-]+:/i.test(raw)) return raw;
  // A bare absolute path (`D:/logo.png`, `/home/me/logo.png`) — the shape the backend stores.
  return `file:///${raw.replace(/\\/g, "/").replace(/^\/+/, "")}`;
}

/**
 * The tile's colours for a chosen `accent`. The accent is an OPAQUE string the backend never
 * interpreted, so it is used as a CSS colour: a soft wash of it as the background and the accent
 * itself as the foreground, mixed through `color-mix` so ANY hue stays legible in both themes
 * without the component knowing which one was picked. An absent accent returns `undefined` and the
 * tile keeps its neutral token classes. An unparseable value is dropped by the CSSOM on
 * assignment, which lands back on the neutral tile — never on a broken square.
 */
function accentStyle(accent: string | null | undefined): CSSProperties | undefined {
  const raw = accent?.trim();
  if (!raw) return undefined;
  return {
    backgroundColor: `color-mix(in oklab, ${raw} 22%, transparent)`,
    color: raw,
  };
}

export interface ProjectMarkProps {
  /** The project whose identity is drawn — only its name + the three identity fields are read. */
  project: Pick<ProjectRecord, "name" | "accent" | "icon" | "image_path">;
  /** Extra classes for the tile (its SIZE lives here — the component sets no dimension). */
  className?: string;
  /**
   * NAME the project on the mark itself: `role="img"` with `title` + `aria-label`, instead of the
   * default `aria-hidden` drawing.
   *
   * Default OFF, because the common case is a mark INSIDE a control that already names the project
   * (the rail's tile button, the home header's `<h1>`), where a second name is noise for a reader.
   * ON is for the corner mark of a cross-project row, where this square is the ONLY thing saying
   * whose item it is — and where the name must NOT be written as text (no column, no label).
   */
  labelled?: boolean;
  /** Test hook, forwarded to the tile — so a surface can find its own mark without a wrapper. */
  "data-testid"?: string;
}

export function ProjectMark({
  project,
  className,
  labelled = false,
  "data-testid": testId,
}: ProjectMarkProps) {
  // A picture that fails to load (moved, deleted, unreadable) degrades to the icon/initial rather
  // than leaving a broken-image square. The FAILED SOURCE is remembered rather than a boolean, so
  // pointing the project at a different picture retries instead of staying blacklisted.
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const src = projectImageSrc(project.image_path);
  const Icon = projectIconFor(project.icon);
  const accent = accentStyle(project.accent);
  return (
    <span
      {...(labelled
        ? ({ role: "img", title: project.name, "aria-label": `Project ${project.name}` } as const)
        : ({ "aria-hidden": true } as const))}
      data-testid={testId}
      style={accent}
      className={cn(
        "flex items-center justify-center overflow-hidden rounded-lg font-medium text-xs",
        // The REPLI, and the base every source paints over: a neutral tile with the muted
        // foreground. Only a chosen accent overrides it (inline, above).
        !accent && "bg-muted text-foreground/85",
        className,
      )}
    >
      {src && failedSrc !== src ? (
        <img
          src={src}
          alt=""
          className="size-full object-cover"
          onError={() => setFailedSrc(src)}
        />
      ) : Icon ? (
        <Icon className="size-1/2" />
      ) : (
        projectInitial(project.name)
      )}
    </span>
  );
}
