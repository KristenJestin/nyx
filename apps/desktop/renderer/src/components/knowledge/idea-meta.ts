/**
 * Display lookups for the idea enums — the ONE source of truth for how a canonical `ideas.status`
 * reads in the Idées view (rows, group mini-dots, drawer, filter).
 *
 * DESIGN-SYSTEM DISCIPLINE: every colour here is a design-system TOKEN class (the CSS variables in
 * `globals.css`: `--info` / `--warning` / `--success` / `--muted-foreground`, surfaced by Tailwind
 * as `bg-*`/`text-*`). There is NO raw hex / rgb — the look comes from the project's tokens, not the
 * prototype's pixels. `variant` is a real `<Badge>` variant so the status chip composes the existing
 * primitive, exactly like `spec-meta.ts` for the Specs UI.
 */
import type { BadgeProps } from "@/components/ui/badge";

import type { IdeaStatus } from "./types";

type BadgeVariant = NonNullable<BadgeProps["variant"]>;

export interface IdeaStatusMeta {
  /** Human label (the status chip / filter option). */
  readonly label: string;
  /** A solid dot colour — a design-system token `bg-*` class (the group mini-dots + row dot). */
  readonly dot: string;
  /** The `<Badge>` variant for the status chip. */
  readonly variant: BadgeVariant;
  /** A terminal (closed) status — the row title is struck through, the drawer shows the motif. */
  readonly closed: boolean;
}

/** The status axis, in the fixed order the mini-dots distribution + the filter render. */
export const IDEA_STATUS_ORDER: IdeaStatus[] = [
  "open",
  "planned",
  "implemented",
  "closed",
  "dropped",
];

export const IDEA_STATUS_META: Record<IdeaStatus, IdeaStatusMeta> = {
  open: { label: "Open", dot: "bg-info", variant: "info", closed: false },
  planned: { label: "Planned", dot: "bg-warning", variant: "warning", closed: false },
  implemented: { label: "Implemented", dot: "bg-success", variant: "success", closed: true },
  // `closed` = the MANUAL fermeture-par-constat — a resolved terminal DISTINCT from the Spec-driven
  // `implemented` (a neutral `source` chip + a primary-token dot so it reads apart from both the
  // green `implemented` and the dimmed `dropped`).
  closed: { label: "Closed", dot: "bg-primary", variant: "source", closed: true },
  dropped: { label: "Dropped", dot: "bg-muted-foreground/50", variant: "muted", closed: true },
};

/** Resolve a status meta from a raw wire string, defaulting to `open` for an unknown. */
export function ideaStatusMeta(status: string): IdeaStatusMeta {
  return IDEA_STATUS_META[status as IdeaStatus] ?? IDEA_STATUS_META.open;
}

/**
 * The palette of design-system `bg-*` token classes a theme dot draws from — the SAME tokens the
 * Specs UI uses (no raw hex). A theme keeps a STABLE colour (hashed from its name) so the same
 * theme always reads with the same dot across sessions.
 */
const THEME_TONES = [
  "bg-info",
  "bg-warning",
  "bg-success",
  "bg-primary",
  "bg-destructive",
  "bg-muted-foreground/60",
] as const;

/** A stable design-system dot tone for a theme name (hashed into {@link THEME_TONES}). */
export function themeTone(theme: string): string {
  let hash = 0;
  for (let i = 0; i < theme.length; i++) {
    hash = (hash * 31 + theme.charCodeAt(i)) >>> 0;
  }
  return THEME_TONES[hash % THEME_TONES.length];
}

/** A compact `Mon D` date for an epoch-ms timestamp (the row's capture date). Empty for `0`. */
export function formatShortDate(ms: number): string {
  if (!ms) return "";
  try {
    return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(
      new Date(ms),
    );
  } catch {
    return "";
  }
}
