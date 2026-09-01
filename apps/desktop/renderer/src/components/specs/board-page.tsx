import { useMemo, useState } from "react";
import { LayoutGridIcon, ListIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Breadcrumb, BreadcrumbCurrent } from "@/components/ui/breadcrumb";
import { Spinner } from "@/components/ui/spinner";
import { useActiveProject } from "@/components/terminals/active-project";

import { BoardFilters } from "./board-filters";
import { EMPTY_FILTERS, matches, type BoardFilters as Filters } from "./board-filtering";
import { KanbanColumn, type BoardColumn } from "./kanban-column";
import { DEFAULT_COLLAPSED_STATUSES, STATUS_ORDER } from "./spec-meta";
import { SpecTable } from "./spec-table";
import { useSpecs } from "./use-specs";
import type { SpecStatus } from "./types";

type BoardView = "kanban" | "list";

/**
 * `<BoardPage>` — the Specs board (`/work`): the Specs as a KANBAN (columns by status) or a LIST
 * (table), with a priority filter and a view toggle. Placement reproduced from the prototype's
 * `board-page` (header with title/subtitle + view toggle → filter row → kanban columns / table),
 * rewritten against the real design system and the live read model (`useSpecs` → the `spec_list`
 * db-command).
 *
 * ## ONE BOARD, TWO SCOPES — the same page, re-parameterised
 *
 * The board reads the app's {@link useActiveProject} SCOPE and hands it to `useSpecs` whole. In
 * the project scope it shows the entered project's Specs; in the cross-project scope it shows
 * EVERY project's, through the same one call with no project filter. Nothing else changes: same
 * columns, same card, same filters, same order — because it is the same code, which is the only
 * way the two cannot drift. There is no second board component and no grouping by project.
 *
 * ## THE PROJECT MARK — the key alone did not do it
 *
 * The standing rule is that a cross-project surface names each item's project with
 * `<ProjectMark>` (`sidebar/project-mark.tsx`). This board tried to be the exception the rule
 * allows for: the card LEADS with the human key `KEY-number` (`specKey`, on the card AND in the
 * table's Key cell), which IS the project's name — `BETA-1` says Beta in the place the eye already
 * goes, so a mark beside it would say the same thing twice.
 *
 * The user looked at the real board and did not recognise the project (« je suis en all dans
 * works, j'ai aucun sign du projet en question sur les card ? »): a key is a string to READ and
 * decode, a mark is SEEN, and `DEPOT-8` / `MUSICMAN-28` proved the difference in practice. So the
 * board follows the rule like every other cross-project surface — the mark in the cross-project
 * scope, nothing in the project one, on BOTH views (the kanban card and the table row would
 * otherwise disagree about what names a project).
 *
 * The two HOMES' Spec cards are NOT concerned: they are a different component
 * (`home/home-spec-card.tsx`) with the elected v11 anatomy, and they stay unmarked.
 */
export function BoardPage() {
  const { scope, globalScope } = useActiveProject();
  const { specs, loading, error } = useSpecs(scope);

  const [view, setView] = useState<BoardView>("kanban");
  const [filters, setFilters] = useState<Filters>(EMPTY_FILTERS);
  const [collapsed, setCollapsed] = useState<Set<SpecStatus>>(
    () => new Set(DEFAULT_COLLAPSED_STATUSES),
  );

  const toggleColumn = (status: SpecStatus) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(status)) next.delete(status);
      else next.add(status);
      return next;
    });

  // Group the FILTERED Specs into the fixed status columns.
  const columns = useMemo<BoardColumn[]>(() => {
    const visible = specs.filter((s) => matches(s, filters));
    return STATUS_ORDER.map((status) => ({
      status,
      specs: visible.filter((s) => (s.status as SpecStatus) === status),
    }));
  }, [specs, filters]);

  const visibleCount = columns.reduce((n, c) => n + c.specs.length, 0);
  const inProgress = specs.filter((s) => s.status === "in_progress").length;

  return (
    <div className="flex h-app flex-col bg-background px-6 pb-6 pt-10">
      {/* Header — the SHARED breadcrumb (Finding 47: `Work` — the board IS the Work root), the
          title + subtitle, and the kanban/list view toggle. The breadcrumb harmonises this header
          with the Spec detail (`Work / <specKey>`) and the review (`Work / <specKey> / Review`). */}
      <header className="mb-5 flex items-start justify-between gap-4">
        <div className="min-w-0">
          <Breadcrumb className="mb-2">
            <BreadcrumbCurrent>Work</BreadcrumbCurrent>
          </Breadcrumb>
          <h1 className="font-heading text-xl font-semibold tracking-tight text-foreground">
            Specs
          </h1>
          <p className="mt-0.5 text-sm text-muted-foreground">
            {specs.length} specs · {inProgress} in progress
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-0.5 rounded-lg border border-border bg-card p-0.5">
          {(
            [
              { v: "kanban", icon: LayoutGridIcon, label: "Board view" },
              { v: "list", icon: ListIcon, label: "List view" },
            ] as const
          ).map(({ v, icon: Icon, label }) => (
            <button
              key={v}
              type="button"
              onClick={() => setView(v)}
              aria-label={label}
              aria-pressed={view === v}
              className={cn(
                "flex size-7 items-center justify-center rounded-md transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring",
                view === v
                  ? "bg-muted text-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              <Icon className="size-4" />
            </button>
          ))}
        </div>
      </header>

      <div className="mb-4 shrink-0">
        <BoardFilters
          allSpecs={specs}
          filters={filters}
          onChange={setFilters}
          visibleCount={visibleCount}
        />
      </div>

      {loading ? (
        <div className="flex flex-1 items-center justify-center text-muted-foreground">
          <Spinner />
        </div>
      ) : error ? (
        <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
          Could not load the Specs.
        </div>
      ) : /* NO PROJECT IS LOADED — a fresh install, or the window before the rail resolves the
              entered one. It is read off the SCOPE, so it can no longer fire in the cross-project
              mode, where there is deliberately no project id to be `null`. */
      scope.kind === "project" && scope.projectId === null ? (
        <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
          Enter a project to see its Specs.
        </div>
      ) : specs.length === 0 ? (
        /* NOTHING TO SHOW in the scope one is in — the same statement in both, named after the
           scope so it says what was actually looked at (the homes' `HomeEmpty` hint precedent).
           It is the SCOPE's emptiness, not the filters': a filtered-out board keeps its columns
           and the table keeps its own "no match" line. */
        <div
          data-testid="board-empty"
          className="flex flex-1 items-center justify-center text-sm text-muted-foreground"
        >
          {scope.kind === "all" ? "No spec on any project yet." : "No spec in this project yet."}
        </div>
      ) : view === "kanban" ? (
        <div className="flex min-h-0 flex-1 items-stretch gap-3">
          {columns.map((col) => (
            <KanbanColumn
              key={col.status}
              col={col}
              collapsed={collapsed.has(col.status)}
              onToggle={() => toggleColumn(col.status)}
              showProject={globalScope}
            />
          ))}
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto">
          <SpecTable specs={columns.flatMap((c) => c.specs)} showProject={globalScope} />
        </div>
      )}
    </div>
  );
}

export default BoardPage;
