import {
  BotIcon,
  BriefcaseIcon,
  FolderIcon,
  LayoutGridIcon,
  LightbulbIcon,
  ListTodoIcon,
  SquareTerminalIcon,
} from "lucide-react";

import type { GlobalSearchKind } from "@/bridge/contract";

/**
 * The palette's DISPLAY vocabulary: what each of the seven kinds is called, and which glyph carries
 * it.
 *
 * THERE IS NO DISPLAY ORDER HERE ANY MORE. The results are ONE list ranked by the aggregator
 * (bucket, then score, then recency, then id — `nyx_core::search::by_rank`), types confounded, and
 * the palette draws them in the order it received them. Sections used to promote a type before the
 * query was even read; the row's own glyph already says which type a line is.
 */

/**
 * The seven kinds, in the sequence `nyx_core::search::GROUP_ORDER` still TRIMS on backend-side.
 *
 * It is no longer an order the renderer draws in — nothing here sorts. It is kept as the mirror of
 * the backend constant that `search-rules-contract.test.ts` pins across the two languages, and as
 * the list that test walks to prove every kind the backend can emit is a kind the palette can draw.
 */
export const SEARCH_GROUP_ORDER: readonly GlobalSearchKind[] = [
  "spec",
  "task",
  "idea",
  "workspace",
  "project",
  "command",
  "session",
];

/**
 * The human NAME of a kind (plural — it names the type, not one row) and the glyph that carries it
 * on every row of that kind.
 *
 * The glyph is what the label used to be: with no section headings left, the icon at the head of a
 * line is the whole of what says "this is a command, that is a Spec". The name stays declared for
 * every kind all the same — a kind that reaches the palette without display metadata is a kind that
 * cannot be drawn, which is what the shared-fixture test checks before a new one ever ships.
 */
export const SEARCH_KIND_META: Record<
  GlobalSearchKind,
  { readonly label: string; readonly icon: typeof BriefcaseIcon }
> = {
  spec: { label: "Specs", icon: BriefcaseIcon },
  task: { label: "Tasks", icon: ListTodoIcon },
  idea: { label: "Ideas", icon: LightbulbIcon },
  workspace: { label: "Workspaces", icon: FolderIcon },
  project: { label: "Projects", icon: LayoutGridIcon },
  command: { label: "Managed commands", icon: SquareTerminalIcon },
  session: { label: "Agent sessions", icon: BotIcon },
};

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** `MMM D` in `en-US` — the app's locale for dates, as on the Spec/idea surfaces. */
const MONTH_DAY = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });

/** Midnight of the calendar day `ts` falls on, in the local zone. */
function startOfDay(ts: number): number {
  const d = new Date(ts);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/**
 * Midnight of the day BEFORE `ts`, walked with the calendar (`setDate`) and not by subtracting
 * 86 400 000 ms — a DST day is 23 or 25 hours long, and "yesterday" is a calendar fact.
 */
function startOfPreviousDay(ts: number): number {
  const d = new Date(ts);
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - 1);
  return d.getTime();
}

/**
 * The RELATIVE recency shown on a suggestion — the five forms of the placement, with the bounds
 * the PRD fixes:
 *
 *   `now`    under a minute
 *   `Nm`     under an hour
 *   `Nh`     under a day
 *   `Yest.`  the previous CALENDAR day (only reachable past the 24 h bound above it)
 *   `MMM D`  beyond, in `en-US`
 *
 * The bounds are ORDERED, so they never overlap: something touched 20 h ago reads `20h` even when
 * the wall clock says yesterday. `now` also absorbs a timestamp in the future (clock skew between
 * the sidecar's `updated_at` and the renderer's clock) rather than printing a negative age.
 */
export function relativeSuggestionTime(ts: number, now: number = Date.now()): string {
  if (!Number.isFinite(ts) || ts <= 0) return "";
  const diff = now - ts;
  if (diff < MINUTE) return "now";
  if (diff < HOUR) return `${Math.floor(diff / MINUTE)}m`;
  if (diff < DAY) return `${Math.floor(diff / HOUR)}h`;
  if (startOfDay(ts) === startOfPreviousDay(now)) return "Yest.";
  return MONTH_DAY.format(new Date(ts));
}
