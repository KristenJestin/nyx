import { Link } from "@tanstack/react-router";

import { Badge } from "@/components/ui/badge";
import { ProjectMark } from "@/components/sidebar/project-mark";

import { priorityMeta, specKey, statusMeta } from "./spec-meta";
import { Dot, ProgressBar } from "./spec-primitives";
import type { SpecListEntry } from "./types";

/**
 * `<SpecTable>` — the board's LIST view: every visible Spec as a table row (Key / Title /
 * Status / Priority / Progress). Placement reproduced from the prototype's `spec-table`;
 * built as a semantic `<table>` styled with design-system tokens (the real renderer has no
 * table primitive), the title a router `<Link>` to the PRD detail. No prototype markup/pixels.
 *
 * ## THE PROJECT MARK (`showProject`)
 *
 * The same answer as the kanban card, in this anatomy: in the cross-project scope the mark rides
 * INSIDE the existing Key cell, immediately before the key it completes — never a sixth column,
 * which would make the table shift shape between the two scopes. The two views are the same board
 * and they must not disagree about what names a project (finding `01M1ED16QRC2WHR5FKRK3NGX06`).
 */
export function SpecTable({
  specs,
  showProject = false,
}: {
  specs: readonly SpecListEntry[];
  /** CROSS-PROJECT scope: open each Key cell with the Spec's project mark. */
  showProject?: boolean;
}) {
  return (
    <div className="overflow-hidden rounded-xl border border-border/70 bg-card">
      <table className="w-full border-collapse text-sm">
        <thead>
          <tr className="border-b border-border/70 text-left text-xs text-muted-foreground">
            <th className="w-24 py-2.5 ps-4 font-medium">Key</th>
            <th className="py-2.5 font-medium">Title</th>
            <th className="w-32 py-2.5 font-medium">Status</th>
            <th className="w-24 py-2.5 font-medium">Priority</th>
            <th className="w-28 py-2.5 pe-4 text-right font-medium">Progress</th>
          </tr>
        </thead>
        <tbody>
          {specs.length > 0 ? (
            specs.map((spec) => <SpecRow key={spec.id} spec={spec} showProject={showProject} />)
          ) : (
            <tr>
              <td colSpan={5} className="py-12 text-center text-sm text-muted-foreground">
                No specs match the current filters.
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

function SpecRow({ spec, showProject }: { spec: SpecListEntry; showProject: boolean }) {
  const status = statusMeta(spec.status);
  const priority = priorityMeta(spec.priority);
  return (
    <tr className="border-b border-border/40 last:border-b-0 transition-colors hover:bg-muted/40">
      <td className="py-2.5 ps-4">
        <span className="flex items-center gap-2">
          {showProject && spec.project ? (
            <ProjectMark
              project={spec.project}
              labelled
              data-testid="spec-project-mark"
              className="size-5 shrink-0 rounded-md"
            />
          ) : null}
          <span data-testid="spec-key" className="font-mono text-xs text-muted-foreground">
            {specKey(spec.project_key, spec.number)}
          </span>
        </span>
      </td>
      <td className="py-2.5">
        <Link
          to="/work/$specId"
          params={{ specId: spec.id }}
          className="truncate font-medium text-foreground outline-none hover:underline focus-visible:underline"
        >
          {spec.title}
        </Link>
      </td>
      <td className="py-2.5">
        <span className="inline-flex items-center gap-1.5 text-sm">
          <Dot tone={status.dot} size="sm" />
          {status.label}
        </span>
      </td>
      <td className="py-2.5">
        <Badge variant={priority.variant}>{priority.label}</Badge>
      </td>
      <td className="py-2.5 pe-4">
        <div className="flex items-center justify-end gap-2">
          <ProgressBar done={spec.tasks_done} total={spec.tasks_total} className="w-14" />
          <span className="font-mono text-2xs tabular-nums text-muted-foreground">
            {spec.tasks_done}/{spec.tasks_total}
          </span>
        </div>
      </td>
    </tr>
  );
}
