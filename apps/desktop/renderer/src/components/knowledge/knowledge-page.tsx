import { useEffect, useMemo, useState } from "react";
import {
  ChevronsDownUpIcon,
  ChevronsUpDownIcon,
  LightbulbIcon,
  ListIcon,
  ListTreeIcon,
  SearchIcon,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { Breadcrumb, BreadcrumbCurrent } from "@/components/ui/breadcrumb";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/segmented";
import { Spinner } from "@/components/ui/spinner";
import { useActiveProject } from "@/components/terminals/active-project";

import { IdeaDetailDrawer } from "./idea-detail-drawer";
import { IdeaRow } from "./idea-row";
import { IdeaThemeGroup } from "./idea-theme-group";
import { themeTone } from "./idea-meta";
import { themeKey, type IdeaListItem, type IdeaStatus } from "./types";
import { useIdeas, useIdeaSearch } from "./use-ideas";

/**
 * The status filter keys: the individual statuses, `all`, and the `active` shortcut (= open +
 * planned, the live backlog). `active` is the DEFAULT so a fresh load shows only the actionable
 * ideas; every status stays reachable through the other segments.
 */
type StatusFilter = "active" | "all" | IdeaStatus;

const STATUS_OPTIONS: readonly { readonly v: StatusFilter; readonly label: string }[] = [
  { v: "active", label: "Active" },
  { v: "open", label: "Open" },
  { v: "planned", label: "Planned" },
  { v: "implemented", label: "Done" },
  { v: "closed", label: "Closed" },
  { v: "dropped", label: "Dropped" },
  { v: "all", label: "All" },
];

/** How the Idées table is laid out: theme-GROUPED (collapsible headers) or FLAT (one dense list). */
type IdeaLayout = "grouped" | "flat";

/** localStorage key persisting the grouped↔flat toolbar choice across sessions. */
const LAYOUT_STORAGE_KEY = "nyx.knowledge.ideas.layout";

/** Read the persisted layout (defensively — private-mode / disabled storage falls back to grouped). */
function readStoredLayout(): IdeaLayout {
  try {
    return localStorage.getItem(LAYOUT_STORAGE_KEY) === "flat" ? "flat" : "grouped";
  } catch {
    return "grouped";
  }
}

/** One theme group ready to render: its header meta + the rows visible under the active filters. */
interface RenderGroup {
  readonly key: string;
  readonly label: string;
  readonly tone: string;
  readonly total: number;
  readonly counts: {
    open: number;
    planned: number;
    implemented: number;
    closed: number;
    dropped: number;
  };
  readonly rows: readonly IdeaListItem[];
}

/**
 * `<KnowledgePage>` — the routed Knowledge surface (`/knowledge`): the native IDÉES view (layout L1).
 * A dense table GROUPED BY THEME with collapsible animated groups (header = counter + status
 * mini-dots), a toolbar (search + status filter + theme facet + expand/collapse all), and the idea
 * DETAIL in a drawer (body + linked Specs with status + closure reason). Placement reproduced from
 * the validated prototype annex `idees-liste-l1`; the LOOK is this project's design system (tokens +
 * the shared UI primitives), never the mockup's pixels. Data flows through the bridge read models
 * (`idea_themes` / `idea_list` / `idea_search`), one board per ENTERED project (`useActiveProject`).
 */
export function KnowledgePage() {
  const { activeProjectId } = useActiveProject();
  const { themes, ideas, loading, error } = useIdeas(activeProjectId);

  const [selectedId, setSelectedId] = useState<string | null>(null);
  // Default = the ACTIVE backlog (open + planned): a fresh load hides Done/Dropped (finding).
  const [status, setStatus] = useState<StatusFilter>("active");
  const [query, setQuery] = useState("");
  // The grouped↔flat table layout — restored from (and persisted to) localStorage.
  const [layout, setLayout] = useState<IdeaLayout>(readStoredLayout);
  // Collapsed theme keys (EMPTY = every group open — the browse default). A per-group toggle
  // adds/removes; "collapse all" fills it, "expand all" clears it.
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  // Selected theme FACET keys (EMPTY = no facet, all themes shown). Multi-select chips.
  const [facet, setFacet] = useState<ReadonlySet<string>>(new Set());

  // Persist the layout choice so it survives a reload (the other toolbar state stays session-local).
  useEffect(() => {
    try {
      localStorage.setItem(LAYOUT_STORAGE_KEY, layout);
    } catch {
      /* storage unavailable (private mode) — the in-memory choice still applies this session. */
    }
  }, [layout]);

  // Backend FTS5 + fuzzy search → the SET of matching ids (null while the query is blank).
  const matchIds = useIdeaSearch(activeProjectId, query);
  const filtering = status !== "all" || query.trim() !== "";

  // Group the enriched rows by theme, then walk the AUTHORITATIVE theme order (`idea_themes`,
  // biggest group first) so headers/counters stay stable while the rows filter client-side.
  const groups = useMemo<RenderGroup[]>(() => {
    const byTheme = new Map<string, IdeaListItem[]>();
    for (const item of ideas) {
      const key = themeKey(item.theme);
      const bucket = byTheme.get(key);
      if (bucket) bucket.push(item);
      else byTheme.set(key, [item]);
    }
    const statusVisible = (item: IdeaListItem) => {
      if (status === "all") return true;
      if (status === "active") return item.status === "open" || item.status === "planned";
      return item.status === status;
    };
    const rowVisible = (item: IdeaListItem) =>
      statusVisible(item) && (matchIds === null || matchIds.has(item.id));

    return themes
      .map((group): RenderGroup => {
        const key = themeKey(group.theme);
        const all = byTheme.get(key) ?? [];
        return {
          key,
          label: key,
          tone: themeTone(key),
          total: group.total,
          counts: {
            open: group.open,
            planned: group.planned,
            implemented: group.implemented,
            closed: group.closed,
            dropped: group.dropped,
          },
          rows: all.filter(rowVisible),
        };
      })
      .filter((group) => {
        if (facet.size > 0 && !facet.has(group.key)) return false;
        // While filtering (status/search), hide groups with nothing left; otherwise keep them.
        return filtering ? group.rows.length > 0 : true;
      });
  }, [ideas, themes, status, matchIds, facet, filtering]);

  // FLAT mode: every visible row in one dense list (theme-major order preserved from `groups`, which
  // already applies the status/search/facet filters). Empty groups contribute nothing.
  const flatRows = useMemo<IdeaListItem[]>(() => groups.flatMap((g) => g.rows), [groups]);
  const isEmpty = layout === "flat" ? flatRows.length === 0 : groups.length === 0;

  const selected = useMemo(
    () => ideas.find((i) => i.id === selectedId) ?? null,
    [ideas, selectedId],
  );

  // Keep the LAST shown idea mounted through the CLOSE animation: as the drawer column collapses to
  // 0, the content stays rendered (clipped by the narrowing cell) so the close reads as a slide-away
  // rather than a pop-out — the same "children always mounted, dimension animates" shape the
  // theme-group uses. The `selected != null` state drives the column open/closed; this only decides
  // WHAT the (persistently mounted) cell renders.
  const [lastShown, setLastShown] = useState<IdeaListItem | null>(null);
  useEffect(() => {
    if (selected != null) setLastShown(selected);
  }, [selected]);
  const drawerItem = selected ?? lastShown;

  const openTracked = ideas.filter((i) => i.status === "open" || i.status === "planned").length;
  const allThemeKeys = themes.map((g) => themeKey(g.theme));

  const toggleTheme = (key: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const toggleFacet = (key: string) =>
    setFacet((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  return (
    // `overflow-x-clip` is now belt-and-suspenders: the list↔drawer split below is a CSS GRID whose
    // two columns SUM to the container width, so no child can push a horizontal overflow — on open OR
    // close — making the page/shell-level scrollbar flash STRUCTURALLY impossible (it used to escape
    // the old Motion width:auto over-measure up to the shell's Outlet wrapper and onto the document).
    // We keep the clip anyway (harmless): `clip` clips the X axis WITHOUT a scroll container, leaves
    // `overflow-y` VISIBLE (no coercion to `auto`) and establishes no BFC, so the `h-svh` vertical
    // flow is untouched — a cheap guard against any unrelated future horizontal overshoot.
    <div className="flex h-svh flex-col overflow-x-clip bg-background px-6 pb-6 pt-10">
      <header className="mb-4 shrink-0">
        <Breadcrumb className="mb-2">
          <BreadcrumbCurrent>Knowledge</BreadcrumbCurrent>
        </Breadcrumb>
        <h1 className="font-heading text-xl font-semibold tracking-tight text-foreground">Ideas</h1>
        <p className="mt-0.5 text-sm text-muted-foreground">
          {ideas.length} ideas · {openTracked} open/tracked · {themes.length} themes
        </p>
      </header>

      {/* Toolbar — ONE cohesive control group: every shell is `h≈34px · border-input · bg-card`
          (search + status Segmented(md) + grouped/flat toggle + expand/collapse-all), then the
          theme facet chips underneath. */}
      <div className="mb-3 shrink-0 space-y-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <div className="flex min-w-[16rem] flex-1 items-center gap-2 rounded-md border border-input bg-card px-2.5 py-1.5">
            <SearchIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
            <Input
              aria-label="Search ideas"
              placeholder="Search an idea or a theme…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="w-full border-0 bg-transparent px-0 py-0 focus-visible:ring-0"
            />
          </div>
          <Segmented<StatusFilter>
            aria-label="Filter by status"
            value={status}
            onChange={setStatus}
            options={STATUS_OPTIONS}
            size="md"
          />
          {/* Grouped ↔ flat table layout. */}
          <div className="flex items-center gap-0.5 rounded-md border border-input bg-card p-0.5">
            {(
              [
                { v: "grouped", icon: ListTreeIcon, label: "Group by theme" },
                { v: "flat", icon: ListIcon, label: "Flat list" },
              ] as const
            ).map(({ v, icon: Icon, label }) => (
              <button
                key={v}
                type="button"
                onClick={() => setLayout(v)}
                aria-label={label}
                aria-pressed={layout === v}
                className={cn(
                  "flex size-7 items-center justify-center rounded outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                  layout === v
                    ? "bg-muted text-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                <Icon className="size-4" />
              </button>
            ))}
          </div>
          {/* Expand/collapse-all only makes sense with group headers → grouped mode only. */}
          {layout === "grouped" ? (
            <div className="flex items-center gap-0.5 rounded-md border border-input bg-card p-0.5">
              <button
                type="button"
                onClick={() => setCollapsed(new Set())}
                aria-label="Expand all themes"
                className="flex size-7 items-center justify-center rounded text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <ChevronsUpDownIcon className="size-4" />
              </button>
              <button
                type="button"
                onClick={() => setCollapsed(new Set(allThemeKeys))}
                aria-label="Collapse all themes"
                className="flex size-7 items-center justify-center rounded text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                <ChevronsDownUpIcon className="size-4" />
              </button>
            </div>
          ) : null}
        </div>

        {themes.length > 0 ? (
          <div
            role="group"
            aria-label="Filter by theme"
            className="flex flex-wrap items-center gap-1.5"
          >
            {themes.map((group) => {
              const key = themeKey(group.theme);
              const on = facet.has(key);
              return (
                <button
                  key={key}
                  type="button"
                  aria-pressed={on}
                  onClick={() => toggleFacet(key)}
                  className={cn(
                    "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-2xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                    on
                      ? "border-border bg-muted text-foreground"
                      : "border-border/60 text-muted-foreground hover:text-foreground",
                  )}
                >
                  <span className={cn("size-1.5 rounded-full", themeTone(key))} aria-hidden />
                  {key}
                  <span className="font-mono text-muted-foreground">{group.total}</span>
                </button>
              );
            })}
          </div>
        ) : null}
      </div>

      {/* Body: the list table (left) + the detail drawer (right). The split is a CSS GRID whose two
          columns ALWAYS sum to the container width — a `minmax(0,1fr)` list track + the drawer track
          that animates between `0rem` (closed) and `clamp(30rem,45vw,42rem)` (open). Because the
          tracks sum to the container, neither column can EVER overflow it horizontally, on open OR on
          close — the scrollbar flash is impossible by construction, not by clipping the overflow
          chain. The drawer column animates via a pure CSS `transition-[grid-template-columns]` (the
          SAME technique the theme-group uses vertically with `grid-template-rows: 0fr↔1fr`), on this
          ALWAYS-MOUNTED container so the transition fires reliably in WebKitGTK — which is why the old
          Motion width:auto animation (whose transient over-measure flashed the scrollbar on close) is
          gone. `motion-reduce:transition-none` makes the column change instant under reduced motion.
          Both track lists have the same 2-track shape and second-track LENGTHS (`0rem` ↔ a clamp),
          so `grid-template-columns` interpolates cleanly. */}
      <div
        className={cn(
          "grid min-h-0 flex-1 overflow-hidden rounded-xl border border-border/60 bg-card/30",
          "transition-[grid-template-columns] duration-200 ease-out motion-reduce:transition-none",
          selected != null
            ? "grid-cols-[minmax(0,1fr)_clamp(30rem,45vw,42rem)]"
            : "grid-cols-[minmax(0,1fr)_0rem]",
        )}
      >
        {/* The list column takes the `minmax(0,1fr)` track and reflows (shrinks/reclaims) as the
            drawer track grows/shrinks beside it — the "push" effect. `min-w-0` lets it shrink below
            its content's min-size so the fixed drawer track always wins its width. `overflow-x-hidden`
            keeps only the VERTICAL scroll (`overflow-y-auto` alone would coerce `overflow-x` to `auto`
            and flash a column-level bar as `shrink-0` row badges momentarily overflow the narrowing
            column); the VERTICAL scroll stays intact. */}
        <div className="min-h-0 min-w-0 overflow-x-hidden overflow-y-auto">
          {loading ? (
            <div className="flex h-full items-center justify-center text-muted-foreground">
              <Spinner />
            </div>
          ) : error ? (
            <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
              Could not load the ideas.
            </div>
          ) : activeProjectId === null ? (
            <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
              Enter a project to see its ideas.
            </div>
          ) : isEmpty ? (
            <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center text-muted-foreground">
              <LightbulbIcon className="size-6 opacity-40" />
              <p className="text-sm">
                {ideas.length === 0 ? "No ideas captured yet." : "No idea matches the filters."}
              </p>
            </div>
          ) : layout === "flat" ? (
            // FLAT: no group headers, all rows in one dense list, each with its Theme cell.
            <div>
              {flatRows.map((item) => (
                <IdeaRow
                  key={item.id}
                  item={item}
                  selected={item.id === selectedId}
                  onSelect={() => setSelectedId(item.id)}
                  showTheme
                />
              ))}
            </div>
          ) : (
            groups.map((group) => (
              <IdeaThemeGroup
                key={group.key}
                label={group.label}
                tone={group.tone}
                count={filtering ? group.rows.length : group.total}
                counts={group.counts}
                open={!collapsed.has(group.key)}
                onToggle={() => toggleTheme(group.key)}
              >
                {group.rows.map((item) => (
                  <IdeaRow
                    key={item.id}
                    item={item}
                    selected={item.id === selectedId}
                    onSelect={() => setSelectedId(item.id)}
                  />
                ))}
              </IdeaThemeGroup>
            ))
          )}
        </div>

        {/* The DETAIL drawer cell — a PUSH, not an overlay: it takes the animated grid track, and
            `overflow-hidden` clips its FIXED-width inner content while the track is narrower than it
            (0 when closed, and mid-animation) so the drawer never squishes and nothing escapes the
            cell. The content stays MOUNTED (last shown idea) through the close so the collapsing track
            reads as a slide-away; the 0-width track + clip hide it once closed. No Motion, no
            AnimatePresence — the open/close animation is entirely the grid column above.
            `relative` makes the cell the CONTAINING BLOCK for its subtree, so `overflow-hidden` also
            clips ABSOLUTELY-positioned descendants — the drawer's `.sr-only` labels are `position:
            absolute`; without a positioned ancestor their containing block is the viewport, so they
            ESCAPE the cell's clip and, with the content still mounted behind a 0-width track (closed),
            push a few px of horizontal overflow onto the DOCUMENT (the measured root cause of the
            rail-scrolling bug). `min-w-0` keeps this grid item from contributing its content's
            min-content width to the track. */}
        <div className="relative min-h-0 min-w-0 overflow-hidden">
          {drawerItem != null ? (
            // Fixed inner width — comfortable for the body + linked Specs, clamped to a share of the
            // window so the list stays usable; `min-w` keeps it from squishing as the track narrows.
            <div className="flex h-full min-w-[clamp(30rem,45vw,42rem)]">
              <IdeaDetailDrawer item={drawerItem} onClose={() => setSelectedId(null)} />
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}

export default KnowledgePage;
