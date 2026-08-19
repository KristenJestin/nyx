import { useNavigate } from "@tanstack/react-router";
import { SearchIcon, XIcon } from "lucide-react";
import { useCallback, useMemo, useState } from "react";

import { errorText, nyxBridge } from "@/bridge";
import { Breadcrumb, BreadcrumbCurrent } from "@/components/ui/breadcrumb";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/segmented";
import { Spinner } from "@/components/ui/spinner";
import { useActiveProject } from "@/components/terminals/active-project";
import { cn } from "@/lib/utils";

import { ComponentCard } from "./component-card";
import {
  parseComponentRemoval,
  recipeSourceMeaning,
  removalNeedsTelling,
  verdictMeaning,
  type ComponentRemoval,
} from "./removal";
import { ThemeTokensPanel } from "./theme-tokens-panel";
import { componentNature, isOrphan, type DesignComponentRow } from "./types";
import { useComponents } from "./use-components";
import { useDesign } from "./use-design";

/**
 * `CataloguePage` (`/design`) — the DESIGN-SYSTEM catalogue (v2.7 Phase 3, task #10). It browses the
 * project's `design_components` vocabulary (DS + NEW) with a rendered preview per component, filters
 * by nature (DS / NEW) and by "liés-page" (referenced by a proto composition), and a name search.
 * Portée PROJET (global) — distinct from a page/variant composition. Editing a component is the
 * composant detail (task #11), not here (non-goal).
 *
 * READ / NAVIGATION ONLY — the AGENT is the only pilot (PDR-0009, review
 * 01KYKX2A0JGWF8XNWWZ9YFF5MG, finding 01KYKX7HBP4ZSSS6ZDM31T6A5E). The page used to carry an
 * "Inventorier / Sync" button that invoked `design_inventory` itself. That gesture DUPLICATED step 0
 * of the agent's manual and made the human the operator of a task that is not theirs, against the
 * doctrine "le flux prototype est agent-driven ; aucun geste UI ne pilote le travail". The button is
 * gone; the CAPABILITY is not — the `design_inventory` MCP tool and its db-command are untouched and
 * still tested (`nyx_core::design`, `nyx_core::mcp_tools_design`). The catalogue simply LISTENS: the
 * inventory emits `prototypes://changed` and `useComponents` re-pulls on it, with no poll and no
 * click. The one write left here is the card's explicit `design_component_delete` — a human decision
 * about human-reviewed work, never a pipeline step — and its DURABLE variant, the `excludeSource`
 * exclusion (finding `01KYSHSPGSPC98C49X68MWY60D`): deciding that a file is not a component of this
 * project is a judgement, not a step of the pipeline, so it stays here. What the page reports about
 * that gesture is the BACKEND's own text (its `warnings[]`, its refusal), never a paraphrase.
 */

type CatalogueFilter = "all" | "ds" | "new" | "linked" | "orphan";

/**
 * ONE NUMBER PER NOTION, AND IT IS THE ONE THE FILTER SHOWS (finding
 * `01KZ3JYARFJ67T5N1NC22XK69C`).
 *
 * The header used to carry a strip of totals — `80 components · 76 DS · 4 NEW · 15 linked to a page`
 * — beside filters that carried none. Three lies followed from that one arrangement:
 *
 *   * `15 linked` counted the names the project's compositions REFERENCE, catalogued or not (three
 *     of them, `heading` / `paragraph` / `banner`, are not components at all), while the `Linked`
 *     filter can only ever show the CARDS — 12 of them. Deleting a linked component left the 15
 *     where it was and took the filter down to 11: a number that does not follow a deletion is worse
 *     than no number, because it is a certainty that is false.
 *   * the strip announced the WHOLE catalogue while the grid below showed a selection: a search
 *     matching 2 cards still read `80 components`.
 *   * discovering that `Orphans` was empty took a click.
 *
 * So every count is now derived from the very predicate its filter applies, and it is DISPLAYED on
 * that filter. The header states what is on screen, and nothing else.
 */
function matchesFilter(
  component: DesignComponentRow,
  filter: CatalogueFilter,
  linkedNames: ReadonlySet<string>,
): boolean {
  switch (filter) {
    case "ds":
      return componentNature(component) === "ds";
    case "new":
      return componentNature(component) === "new";
    case "linked":
      return linkedNames.has(component.name);
    case "orphan":
      return isOrphan(component);
    default:
      return true;
  }
}

/**
 * What the BACKEND answered to the last removal — the epilogue a deleted card cannot carry itself.
 * `refusal` is a `design_component_delete` REFUSAL (a pinned entry, a glob character in the path, a
 * full exclusion budget): the backend deleted NOTHING and said why, in its own words.
 */
interface RemovalReport {
  readonly componentName: string;
  readonly removal: ComponentRemoval | null;
  readonly refusal: string | null;
  /**
   * The removal was CLEAN and DEFINITIVE — the backend had nothing to add. There is then nothing to
   * quote, only the acknowledgement that the gesture landed (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`).
   */
  readonly plain?: boolean;
}

/**
 * What an empty grid means UNDER EACH FILTER — the filter's own predicate, said in words. The one
 * sentence that used to serve for all of them ("No component matches the current filter") accused a
 * filter even when none was active.
 */
const EMPTY_FILTER_TEXT: Record<CatalogueFilter, string> = {
  all: "No component in this catalogue.",
  ds: "No component of this catalogue comes from the existing design system.",
  new: "No component of this catalogue is a new proposal.",
  linked: "No component of this catalogue is used by a page.",
  orphan: "No component of this catalogue has lost its source file.",
};

const FILTER_LABELS: readonly { readonly v: CatalogueFilter; readonly label: string }[] = [
  { v: "all", label: "All" },
  { v: "ds", label: "DS" },
  { v: "new", label: "NEW" },
  { v: "linked", label: "Linked" },
  { v: "orphan", label: "Orphans" },
];

export function CataloguePage() {
  const { activeProjectId } = useActiveProject();
  const navigate = useNavigate();
  const { components, linkedNames, loading, error, refresh } = useComponents(activeProjectId);
  const { design } = useDesign(activeProjectId);
  const themeTokensJson = design?.theme_tokens_json ?? null;

  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<CatalogueFilter>("all");

  // The catalogue REMOVAL (review #44): the inventory prunes on its own the rows whose source
  // really vanished, but it deliberately spares what it cannot judge (a repo it did not scan) and
  // never touches a greenfield `[NEW]`. This is the human's explicit way out for those — the UI twin
  // of the `design_delete_component` MCP tool, sharing the same `nyx_core::design::remove_component`
  // authority.
  //
  // AND IT REPORTS WHAT REALLY HAPPENED (finding `01KYSHSPGSPC98C49X68MWY60D`). The call used to be
  // fire-and-forget with a `catch {}` that swallowed everything: an EPHEMERAL removal (the source
  // still matches the recipe, so the next inventory brings the row back under a new id) looked
  // exactly like a definitive one, and a REFUSAL — where the backend deleted nothing on purpose —
  // vanished without a trace, leaving a card that "would not delete". Both now land in a notice
  // carrying the backend's OWN sentences: it is the side that knows the verdict and the recipe.
  const [report, setReport] = useState<RemovalReport | null>(null);
  const deleteComponent = useCallback(
    async (component: DesignComponentRow, options: { excludeSource: boolean }) => {
      setReport(null);
      try {
        const answer = await nyxBridge.invoke<unknown>("design_component_delete", {
          componentId: component.id,
          // The DURABLE half of the human's decision: the exclusion is written into the project's
          // recipe in the SAME transaction as the delete (they are one act — see `remove_component`).
          excludeSource: options.excludeSource,
        });
        const removal = parseComponentRemoval(answer);
        // EVERY removal is acknowledged (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`, point D). It used to
        // be reported only when the backend had a CAVEAT to pass on; a clean, definitive one said
        // nothing at all, on the theory that "the row leaving the grid is its own report". In a grid
        // of eighty tiles that reorders as it shrinks, a tile that is simply no longer there is not
        // a report — it is an absence the eye has to hunt for. The caveat still comes from the
        // backend, verbatim; what is added is the plain fact that the gesture landed, and on what.
        setReport({
          componentName: component.name,
          removal,
          refusal: removal.refused,
          plain: !removalNeedsTelling(removal),
        });
      } catch (e: unknown) {
        // A REFUSAL (pinned entry / glob in the path / exclusion budget full) crosses as an error and
        // NOTHING was deleted. Say it, verbatim: the card is still there, and the message names the
        // real lever.
        setReport({
          componentName: component.name,
          removal: null,
          refusal: errorText(e),
          plain: false,
        });
      }
      await refresh();
    },
    [refresh],
  );

  const q = query.trim();
  // The SEARCH is applied first and counted apart: a filter's count answers "how many of these are
  // there", and it must keep answering that while a search narrows the grid.
  const searched = useMemo(() => {
    const needle = q.toLowerCase();
    return needle ? components.filter((c) => c.name.toLowerCase().includes(needle)) : components;
  }, [components, q]);

  const visible = useMemo(
    () => searched.filter((c) => matchesFilter(c, filter, linkedNames)),
    [searched, filter, linkedNames],
  );

  // ONE count per filter, computed by the filter's OWN predicate over the searched set — so the
  // number on a chip is always the number of cards clicking it produces.
  const filterOptions = useMemo(
    () =>
      FILTER_LABELS.map(({ v, label }) => ({
        v,
        label: `${label} ${searched.filter((c) => matchesFilter(c, v, linkedNames)).length}`,
      })),
    [searched, linkedNames],
  );

  return (
    <div className="flex h-app flex-col overflow-x-clip bg-background px-6 pb-6 pt-10">
      <header className="mb-4 flex shrink-0 items-start gap-4">
        <div className="min-w-0">
          <Breadcrumb className="mb-2">
            <BreadcrumbCurrent>Design system</BreadcrumbCurrent>
          </Breadcrumb>
          <h1 className="font-heading text-xl font-semibold tracking-tight text-foreground">
            Catalogue
          </h1>
          {/* WHAT IS ON SCREEN — and the whole only when nothing narrows it. The nature / linked /
              orphan counts moved onto the filters that produce them (see `matchesFilter`). */}
          <p className="mt-0.5 text-sm text-muted-foreground" data-testid="catalogue-count">
            {visible.length} component{visible.length === 1 ? "" : "s"}
            {visible.length === components.length ? "" : ` of ${components.length}`}
          </p>
        </div>
      </header>

      {/* The OTHER half of the design system (review #42): the theme TOKENS the components' classes
          resolve against. Nothing ever read them from the app, so `bg-primary` resolved to nothing
          and every preview below rendered grey. The layer is a PROJECT singleton, referenced from
          the app's own stylesheet — editing one token re-themes every card at once. */}
      <ThemeTokensPanel themeTokensJson={themeTokensJson} className="mb-3" />

      {/* THE REMOVAL'S EPILOGUE (finding `01KYSHSPGSPC98C49X68MWY60D`) — the backend's own words,
          never a paraphrase: only it knows the discovery's verdict on the source and what the
          project's recipe holds. A REFUSAL is an alert (nothing was deleted, the card is still
          below); an ephemeral removal, or an exclusion that was written, is a status carrying a
          caveat. A removal that is complete and final now says so too, plainly and with no caveat
          to read (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8`): a gesture that destroys something and
          answers nothing is a gesture one cannot be sure of having made. */}
      {report ? (
        <section
          data-testid="catalogue-removal-report"
          role={report.refusal ? "alert" : "status"}
          className={cn(
            "mb-3 shrink-0 rounded-md border px-3 py-2",
            report.refusal
              ? "border-destructive/40 bg-destructive/5"
              : report.plain
                ? "border-border/60 bg-muted/30"
                : "border-warning/40 bg-warning/5",
          )}
        >
          <div className="flex items-start gap-2">
            <div className="min-w-0 flex-1">
              <p
                className={cn(
                  "text-xs font-medium",
                  report.refusal
                    ? "text-destructive"
                    : report.plain
                      ? "text-foreground"
                      : "text-warning",
                )}
              >
                {report.refusal
                  ? `"${report.componentName}" was NOT removed`
                  : report.plain
                    ? `"${report.componentName}" removed from the catalogue`
                    : `"${report.componentName}" removed from the catalogue — read this`}
              </p>
              {/* VERBATIM. The backend names the problem AND the correction (narrow the recipe /
                  exclude the source) — re-writing it here is how a surface starts drifting from the
                  truth it reports. */}
              {report.refusal ? (
                <p
                  className="mt-1 whitespace-pre-wrap text-2xs text-foreground"
                  data-testid="removal-refusal"
                >
                  {report.refusal}
                </p>
              ) : null}
              {report.removal?.warnings.map((warning) => (
                <p
                  key={warning}
                  className="mt-1 whitespace-pre-wrap text-2xs text-foreground"
                  data-testid="removal-warning"
                >
                  {warning}
                </p>
              ))}
              {/* The FACTS behind the sentence: which verdict, on which file, under which repo and
                  which recipe. This is what lets a human check the CLAIM instead of trusting it —
                  so it appears where there IS a claim. A removal that went through cleanly makes
                  none, and its acknowledgement stays a single line. */}
              {!report.plain && report.removal && report.removal.effect.verdict !== "" ? (
                <p className="mt-1 text-2xs text-muted-foreground" data-testid="removal-effect">
                  verdict <code className="font-mono">{report.removal.effect.verdict}</code>
                  {verdictMeaning(report.removal.effect.verdict)
                    ? ` — ${verdictMeaning(report.removal.effect.verdict)}`
                    : ""}
                  {report.removal.effect.source_path
                    ? ` · source ${report.removal.effect.repo ? `${report.removal.effect.repo}/` : ""}${report.removal.effect.source_path}`
                    : ""}
                  {report.removal.effect.recipe_source
                    ? ` · judged by ${recipeSourceMeaning(report.removal.effect.recipe_source)}`
                    : ""}
                </p>
              ) : null}
              {report.removal && report.removal.excluded.length > 0 ? (
                <p className="mt-1 text-2xs text-muted-foreground" data-testid="removal-excluded">
                  exclusion written into the recipe:{" "}
                  {report.removal.excluded.map((pattern) => (
                    <code key={pattern} className="mr-1 font-mono">
                      {pattern}
                    </code>
                  ))}
                  {report.removal.recipe_materialized
                    ? " — the project was running on the default recipe: it now has its own."
                    : ""}
                </p>
              ) : null}
            </div>
            <button
              type="button"
              onClick={() => setReport(null)}
              aria-label="Hide the removal report"
              className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:text-foreground"
            >
              <XIcon aria-hidden className="size-3.5" />
            </button>
          </div>
        </section>
      ) : null}

      {/* Toolbar: name search + nature / liés-page filter. */}
      <div className="mb-3 flex shrink-0 flex-wrap items-center gap-2">
        <div className="flex min-w-[16rem] flex-1 items-center gap-2 rounded-md border border-input bg-card px-2.5 py-1.5">
          <SearchIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
          <Input
            aria-label="Search components"
            placeholder="Search a component…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full border-0 bg-transparent px-0 py-0 focus-visible:ring-0"
          />
          {q ? (
            <button
              type="button"
              onClick={() => setQuery("")}
              aria-label="Clear the search"
              className="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
            >
              <XIcon aria-hidden className="size-3.5" />
            </button>
          ) : null}
        </div>
        <Segmented<CatalogueFilter>
          aria-label="Filter components"
          value={filter}
          onChange={setFilter}
          options={filterOptions}
          size="md"
        />
      </div>

      {/* Body: the component grid. */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {loading && components.length === 0 ? (
          <div className="flex h-40 items-center justify-center">
            <Spinner className="text-muted-foreground/50" />
          </div>
        ) : error ? (
          <EmptyState>Could not load the component catalogue.</EmptyState>
        ) : activeProjectId === null ? (
          <EmptyState>Open a project to browse its design-system components.</EmptyState>
        ) : visible.length === 0 ? (
          /* EMPTY-STATE (finding 01KYKX7HBP4ZSSS6ZDM31T6A5E): with the "Inventorier / Sync" button
             gone, an empty catalogue must SAY who fills it, or it reads as broken. Nothing here is
             the human's to do — the agent inventories the project and the grid repaints on its own.

             AND IT NAMES THE REAL CAUSE (finding `01KZ3JYARFJ67T5N1NC22XK69C`): a search with no hit
             used to be blamed on "the current filter" while the filter was `All`, without ever
             repeating the term looked for nor offering to drop it. */
          <EmptyState>
            {components.length === 0 ? (
              <span data-testid="catalogue-empty-agent-driven">
                Empty catalogue — the agent is the one that inventories the project. Components
                appear here on their own; nothing for you to do.
              </span>
            ) : q ? (
              <span data-testid="catalogue-empty-search">
                No component named &ldquo;{q}&rdquo;
                {filter === "all" ? "" : ` among the ${filter} ones`}.{" "}
                <button
                  type="button"
                  onClick={() => setQuery("")}
                  className="rounded underline underline-offset-2 outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                >
                  Clear the search
                </button>
              </span>
            ) : (
              <span data-testid="catalogue-empty-filter">{EMPTY_FILTER_TEXT[filter]}</span>
            )}
          </EmptyState>
        ) : (
          <div className="grid grid-cols-[repeat(auto-fill,minmax(15rem,1fr))] gap-3 pb-2">
            {visible.map((component) => (
              <ComponentCard
                key={component.id}
                projectId={activeProjectId}
                component={component}
                themeTokensJson={themeTokensJson}
                linked={linkedNames.has(component.name)}
                onOpen={(c) =>
                  void navigate({ to: "/design/$componentId", params: { componentId: c.id } })
                }
                onDelete={deleteComponent}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
