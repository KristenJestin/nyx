import { useState } from "react";
import { getRouteApi, Link } from "@tanstack/react-router";
import {
  ActivityIcon,
  ListChecksIcon,
  type LucideIcon,
  MessageSquareIcon,
  PaperclipIcon,
  ScrollTextIcon,
  TerminalIcon,
} from "lucide-react";

import { Breadcrumb, BreadcrumbCurrent, breadcrumbLinkClass } from "@/components/ui/breadcrumb";
import { Spinner } from "@/components/ui/spinner";
import { Tabs } from "@/components/ui/tabs";

import { DependencyBadges } from "./dependency-badges";
import { SpecSidebar } from "./spec-sidebar";
import { specKey } from "./spec-meta";
import { useSpecDependencies } from "./use-spec-dependencies";
import { useSpecDetail } from "./use-spec-detail";
import { useSpecReviewComments } from "./use-spec-review-comments";
import { ActivityTab } from "./tabs/activity-tab";
import { AnnexesTab } from "./tabs/annexes-tab";
import { ReviewTab } from "./tabs/review-tab";
import { RuntimeTab } from "./tabs/runtime-tab";
import { SpecTab } from "./tabs/spec-tab";
import { TasksTab } from "./tabs/tasks-tab";
import type { SpecDetail } from "./types";

const routeApi = getRouteApi("/work/$specId");

/** The tab set, in the placement order the prototype's spec-page validated. */
type SpecTabKey = "spec" | "tasks" | "review" | "annexes" | "activity" | "runtime";

const TAB_META: Record<SpecTabKey, { label: string; icon: LucideIcon }> = {
  spec: { label: "Spec", icon: ScrollTextIcon },
  tasks: { label: "Tasks", icon: ListChecksIcon },
  review: { label: "Review", icon: MessageSquareIcon },
  annexes: { label: "Annexes", icon: PaperclipIcon },
  activity: { label: "Activity", icon: ActivityIcon },
  runtime: { label: "Runtime", icon: TerminalIcon },
};

const TAB_ORDER: SpecTabKey[] = ["spec", "tasks", "review", "annexes", "activity", "runtime"];

/** The tab count pill (open tasks on the Tasks tab). */
function tabCount(key: SpecTabKey, detail: SpecDetail): number | null {
  if (key === "tasks") {
    const open = detail.tasks.filter((t) => t.status !== "done" && t.status !== "cancelled").length;
    return open > 0 ? open : null;
  }
  return null;
}

/**
 * `<SpecDetailPage>` — the PRD detail (`/work/$specId`): the context SIDEBAR on the LEFT (the
 * shared `<SidebarShell>`, same geometry as the Terminals cockpit rail), then a header
 * (breadcrumb + title + human key) and the TABBED body (spec / tasks / review / annexes /
 * activity / runtime) filling the column to its RIGHT. Placement reproduced from the prototype's
 * `spec/spec-page` (+ sidebar): the tab ORDER and regions are the validated layout; the LOOK is
 * the real design system (the `Tabs` primitive + tokens). The tabs consume the phase-1/2/3 model
 * via `useSpecDetail`:
 *  - spec → the active revision; tasks → tasks + dependencies;
 *  - activity / runtime → the DERIVED workspace context (the phase-3 Spec→workspace link);
 *  - review → the Spec-scoped review comments + an "Open review" button onto the full-screen
 *    review page (`/work/$specId/review`), PDR-0028.
 */
export function SpecDetailPage() {
  const { specId } = routeApi.useParams();
  const { detail, loading, error } = useSpecDetail(specId);
  // The review comments' author map — the source a finding's provenance is derived from on the
  // Tasks tab (an OPEN human comment also drives the panel's provenance). Called before the early
  // returns (rules of hooks); a null workspace context fetches nothing.
  const { authorByCommentId } = useSpecReviewComments(detail?.workspace_context ?? null, specId);
  const deps = useSpecDependencies(specId);
  const [tab, setTab] = useState<SpecTabKey>("spec");

  if (loading) {
    return (
      <div className="flex h-svh items-center justify-center bg-background text-muted-foreground">
        <Spinner />
      </div>
    );
  }

  if (error || detail === null) {
    return (
      <div className="flex h-svh flex-col items-center justify-center gap-3 bg-background">
        <p className="text-sm text-muted-foreground">This Spec could not be loaded.</p>
        <Link to="/work" className="text-sm text-foreground underline-offset-4 hover:underline">
          Back to the board
        </Link>
      </div>
    );
  }

  const { spec } = detail;

  return (
    <div className="flex h-svh bg-background">
      {/* The context rail on the LEFT (shared `<SidebarShell>`, same as the Terminals cockpit);
          the header + tabs + tab content occupy the column to its RIGHT. */}
      <SpecSidebar detail={detail} />

      <div className="flex min-w-0 flex-1 flex-col">
        {/* Header — breadcrumb + title + human key. Uses the SHARED `<Breadcrumb>` so the board,
            this detail and the review page all render the same crumb chrome (Finding 47). */}
        <header className="shrink-0 px-8 pb-4 pt-7">
          <Breadcrumb className="mb-3">
            <Link to="/work" className={breadcrumbLinkClass}>
              Work
            </Link>
            <BreadcrumbCurrent mono>
              {specKey(detail.workspace_context?.project.key ?? "", spec.number)}
            </BreadcrumbCurrent>
          </Breadcrumb>
          <div className="flex items-start justify-between gap-4">
            <div className="flex min-w-0 items-center gap-2.5">
              <h1 className="min-w-0 truncate font-heading text-xl font-semibold tracking-tight text-foreground">
                {spec.title}
              </h1>
            </div>
            {/* Spec↔Spec dependency badges (PRD "badges dépendances sur le détail Spec"). Present
                only when non-zero; the same `<DependencyBadges>` the board card surfaces. */}
            <DependencyBadges
              blockedBy={deps.blocked_by_count}
              unblocks={deps.unblocks_count}
              className="shrink-0 pt-1"
            />
          </div>
        </header>

        {/* Tabbed body. */}
        <Tabs.Root
          value={tab}
          onValueChange={(v) => setTab(v as SpecTabKey)}
          className="flex min-h-0 flex-1 flex-col"
        >
          <div className="shrink-0 px-8">
            <Tabs.List fullWidth>
              {TAB_ORDER.map((key) => {
                const meta = TAB_META[key];
                const Icon = meta.icon;
                const count = tabCount(key, detail);
                return (
                  <Tabs.Tab key={key} value={key}>
                    <Icon />
                    {meta.label}
                    {count != null ? (
                      <span className="rounded-full bg-muted px-1.5 text-2xs font-medium text-muted-foreground">
                        {count}
                      </span>
                    ) : null}
                  </Tabs.Tab>
                );
              })}
            </Tabs.List>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto px-8 py-6">
            <Tabs.Panel value="spec">
              <SpecTab detail={detail} />
            </Tabs.Panel>
            <Tabs.Panel value="tasks">
              <TasksTab detail={detail} authorByCommentId={authorByCommentId} />
            </Tabs.Panel>
            <Tabs.Panel value="review" className="h-full">
              <ReviewTab detail={detail} />
            </Tabs.Panel>
            <Tabs.Panel value="annexes">
              <AnnexesTab detail={detail} />
            </Tabs.Panel>
            <Tabs.Panel value="activity">
              <ActivityTab detail={detail} />
            </Tabs.Panel>
            <Tabs.Panel value="runtime">
              <RuntimeTab detail={detail} />
            </Tabs.Panel>
          </div>
        </Tabs.Root>
      </div>
    </div>
  );
}

export default SpecDetailPage;
