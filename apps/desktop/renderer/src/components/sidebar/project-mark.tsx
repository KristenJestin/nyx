import { useState, type ComponentType, type CSSProperties } from "react";
import {
  BookIcon,
  BoxIcon,
  BracesIcon,
  BugIcon,
  ChartLineIcon,
  CircleIcon,
  CloudIcon,
  CodeIcon,
  CompassIcon,
  CpuIcon,
  DatabaseIcon,
  FlaskConicalIcon,
  FolderIcon,
  GlobeIcon,
  KeyIcon,
  LayersIcon,
  MonitorIcon,
  NetworkIcon,
  PaletteIcon,
  PenToolIcon,
  RocketIcon,
  ServerIcon,
  ShieldIcon,
  SmartphoneIcon,
  SparklesIcon,
  TerminalIcon,
  WalletIcon,
  WrenchIcon,
} from "lucide-react";

import { IDENTITY_COLOR_COUNT } from "@/components/panes/pane-groups";
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
 *
 * ## Twenty-eight, in FOUR ROWS OF SEVEN, and the order is the grid
 *
 * The declaration order IS what the picker paints (object key order), so it is editorial, not
 * alphabetical: four bands a reader recognises without a label — containers and places, the
 * making of things, the machine, the infrastructure. It is the order arbitrated on the round-3
 * prototype.
 *
 * TWENTY-EIGHT IS A CEILING. At three rows you choose at a glance; at four you sweep; beyond that
 * it is a catalogue that would need a filter — which changes what the control IS, not how long it
 * is. Growing this list past a fourth row is a product decision, not a one-line addition.
 *
 * NO BRAND, NO PRODUCT LOGO, NO NAMED TOOL — shapes only. That is the "nyx stays generic"
 * invariant, and an icon grid is the first place it goes: one framework's logo here and the app
 * has started having opinions about what its user builds.
 */
export const PROJECT_ICONS: Readonly<Record<string, ComponentType<{ className?: string }>>> = {
  // Containers and places.
  folder: FolderIcon,
  box: BoxIcon,
  layers: LayersIcon,
  circle: CircleIcon,
  compass: CompassIcon,
  globe: GlobeIcon,
  sparkles: SparklesIcon,
  // Making things.
  rocket: RocketIcon,
  book: BookIcon,
  palette: PaletteIcon,
  "pen-tool": PenToolIcon,
  monitor: MonitorIcon,
  smartphone: SmartphoneIcon,
  flask: FlaskConicalIcon,
  // The machine.
  code: CodeIcon,
  braces: BracesIcon,
  terminal: TerminalIcon,
  cpu: CpuIcon,
  database: DatabaseIcon,
  "chart-line": ChartLineIcon,
  server: ServerIcon,
  // Infrastructure.
  cloud: CloudIcon,
  network: NetworkIcon,
  shield: ShieldIcon,
  key: KeyIcon,
  wallet: WalletIcon,
  wrench: WrenchIcon,
  bug: BugIcon,
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
 * THE APPLICATION CHANNEL a project picture is drawn through, mirrored from the main process
 * (`apps/desktop/src/main/project-image.ts` — `PROJECT_IMAGE_SCHEME` / `PROJECT_IMAGE_HOST`).
 *
 * DECLARED HERE RATHER THAN IMPORTED, and that is the repo's shape, not a shortcut: the renderer
 * is its own TS project with its own `paths`, and reaches nothing under `apps/desktop/src` — the
 * same reason `bridge/electron.ts` re-declares `CoreEventEnvelope` instead of importing the
 * shared one. The two literals are pinned on BOTH sides by a spec (`project-mark.test.tsx` here,
 * `project-image.test.ts` there), so a rename that lands on one side alone turns red at once.
 */
const PROJECT_IMAGE_ORIGIN = "nyx-image://project-icons";

/**
 * The stored `image_path` as something an `<img>` can load — and the ONE place that translation
 * happens, on every surface that draws a project.
 *
 * IT USED TO SAY `file://…`, AND THAT IS THE DEFECT IT NOW CARRIES THE FIX FOR. A `file://`
 * subresource is refused by Chromium when the page itself is at another origin, which is precisely
 * the development shell: `window.ts` loads the Vite server by `loadURL`, so the renderer sits at
 * `http://localhost:<port>` and every project picture failed to load — silently, straight onto
 * `<ProjectMark>`'s fallback. The packaged build (`loadFile`, `file://` origin) did work, which is
 * exactly what made it invisible to a suite and to a build. See `project-image-protocol.ts`.
 *
 * So a stored PATH now names a file in the app's own channel, by its NAME alone: the main process
 * resolves that name inside `userData/project-icons` and refuses anything else. The URL is
 * identical in dev and packaged, because it no longer depends on where the page came from.
 *
 * A CONSEQUENCE, WRITTEN DOWN: only pictures nyx ADOPTED are drawable this way. `image_path` is a
 * `TEXT` an MCP agent may write (`db.rs`: "a file path or URL"), so an agent could name a picture
 * anywhere on disk — that value now resolves to a name the channel does not hold, and the mark
 * falls back to the icon/initial. It is not a loss: such a path could not render in dev before
 * either, and honouring it would mean handing the page a read primitive over the whole filesystem,
 * which is the one thing this channel exists not to do. An `http(s):` or `data:` value an agent
 * writes is untouched — it is already a URL, and it goes through below as it always did.
 *
 * Returns `undefined` for an absent/blank value so the caller falls through to the icon/initial.
 */
export function projectImageSrc(imagePath: string | null | undefined): string | undefined {
  const raw = imagePath?.trim();
  if (!raw) return undefined;
  // A SCHEME is at least two characters before the colon — the guard that keeps a Windows drive
  // letter (`D:\…`) from reading as one and being handed to the `<img>` unwrapped. An already-URL
  // value (an agent's `https://`, a `nyx-image://` this function produced and something round-
  // tripped) passes straight through; the channel's own gate is what vets the latter.
  if (/^[a-z][a-z0-9+.-]+:/i.test(raw)) return raw;
  // A bare path (`D:\…\project-icons\p1~mfl1a2b3.png`, `/home/me/.config/…/p1~mfl1a2b3.png`) — the
  // shape the backend stores. Only the NAME crosses: the directory is the main process's to know,
  // and a path that never leaves the renderer cannot be a path the renderer chose.
  const name = raw.split(/[\\/]/).pop();
  if (!name) return undefined;
  return `${PROJECT_IMAGE_ORIGIN}/${encodeURIComponent(name)}`;
}

/**
 * THE ACCENTS A HUMAN MAY PICK, in the order the picker paints them — the ERASER FIRST, and it is
 * not a colour: it is the ABSENCE of a choice, so it is spelled `null`, the value that goes to the
 * backend. The absence of an accent has exactly ONE spelling in the database and a second one
 * would have to be known by every reader.
 *
 * `null` RATHER THAN `"identity-0"`, and the type carries it (`readonly (string | null)[]`): the
 * whole list is a write path. A picker mapping over it and calling `setProjectAccent(id, accent)`
 * — the obvious way to wire it — therefore ERASES on the first swatch instead of storing the
 * second spelling, and does so without knowing the rule. `"identity-0"` is not offered here at
 * all, which is the only way that mistake stays impossible rather than merely documented: it
 * would render EXACTLY like the neutral (`projectAccentColor` returns `undefined` for it), so
 * nothing on screen would ever reveal it.
 *
 * Exported for the PICKER (a later phase), so what it paints is exactly what `projectAccentColor`
 * below knows how to resolve. `<ProjectMark>` does NOT read this list: it resolves whatever value
 * is STORED, which is the point of `projectAccentColor` — it must keep honouring accents that were
 * never offered here, such as the raw hex an MCP agent writes.
 *
 * The hues are derived from `IDENTITY_COLOR_COUNT` rather than spelled out, so widening the palette
 * (ADR-0012) offers the new ones here without a second edit — and cannot offer one the stylesheet
 * does not declare.
 */
export const PROJECT_ACCENTS: readonly (string | null)[] = [
  null,
  ...Array.from({ length: IDENTITY_COLOR_COUNT }, (_, i) => `identity-${i + 1}`),
];

/**
 * Resolve a stored `accent` to the CSS colour that paints it.
 *
 * A palette TOKEN NAME (`identity-3`) becomes `var(--identity-3)` — that is what the UI writes,
 * per ADR-0012: the row says WHICH CHOICE the human made, not which pixel came out of it, so a
 * palette that is one day re-cut repaints every project at once instead of stranding them on a
 * colour nothing produces any more.
 *
 * ANYTHING ELSE IS RETURNED AS-IS, deliberately. The MCP `set_project_settings` schema describes
 * `accent` without a format, so an agent legitimately reads it as "pass a colour" and writes
 * `#4f46e5`; the core has always stored the string opaquely, and that call must keep working. An
 * unintelligible value is then dropped by the CSSOM on assignment and the tile falls back to the
 * neutral — never a broken square, never an exception.
 *
 * `identity-0` resolves to NOTHING (`undefined`): it is the neutral, i.e. exactly what the absence
 * of an accent renders. Painting `var(--identity-0)` would give a grey wash that is ALMOST the
 * neutral tile — two spellings of one state, differing by a few percent, which is worse than one.
 */
export function projectAccentColor(accent: string | null | undefined): string | undefined {
  const raw = accent?.trim();
  if (!raw) return undefined;
  const token = /^identity-(\d+)$/.exec(raw);
  if (!token) return raw;
  const index = Number(token[1]);
  // `identity-0` is the neutral (no accent); anything past the palette is not a token we declare,
  // and painting `var(--identity-99)` would resolve to nothing at all.
  if (index <= 0 || index > IDENTITY_COLOR_COUNT) return undefined;
  return `var(--identity-${index})`;
}

/**
 * The tile's colours for a chosen `accent`: a soft wash of it as the background and the accent
 * itself as the foreground, mixed through `color-mix` so ANY hue stays legible in both themes
 * without the component knowing which one was picked. An absent (or neutral) accent returns
 * `undefined` and the tile keeps its neutral token classes.
 *
 * Mixed `in oklch` — the space the whole palette is authored in and the one every group surface
 * mixes in — so a project tile and a group pastille of the same token are literally comparable.
 */
function accentStyle(accent: string | null | undefined): CSSProperties | undefined {
  const color = projectAccentColor(accent);
  if (!color) return undefined;
  return {
    backgroundColor: `color-mix(in oklch, ${color} 22%, transparent)`,
    color,
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
  //
  // AND NOTHING HERE BUSTS A CACHE — deliberately. That memory, and the repaint a new picture owes
  // the user, both hang on the stored path CHANGING when the picture changes; making it change is
  // the writer's job and it is done there, once (`project-image.ts` names the copy
  // `<projectId>~<revision>.<ext>`). A nonce appended here would have to be invented again by every
  // surface that draws a project, would have to survive a re-list that hands back the same record,
  // and would put a cache concern in the one component whose whole point is that identity is
  // resolved in a single place.
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
