import { useEffect, useMemo } from "react";
import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router";
import { BookOpenIcon, BriefcaseIcon, PlusIcon, SquareTerminalIcon } from "lucide-react";
import { motion } from "motion/react";

import { cn } from "@/lib/utils";
import { startNavTransition } from "@/lib/nav-transition";
import { useProjects, type ProjectTree } from "@/components/sidebar/use-projects";
import { useManualAdd } from "@/components/sidebar/use-manual-add";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import { isQuietState, StatusDot } from "./status-dot";
import { projectRunState } from "./runtime-state";
import { useConsumedNotifications } from "./use-consumed-notifications";
import { useProjectRuntime } from "./use-project-runtime";
import { useActiveProject } from "./active-project";

/**
 * The section-nav entries — the routed pages reachable from the rail. `Terminals` is
 * the index route (`/`); `Work` and `Knowledge` are their own routes. Icons are lucide
 * (the app's icon set); the label doubles as the `title`/`aria-label`.
 */
const NAV = [
  { to: "/", icon: SquareTerminalIcon, label: "Terminals" },
  { to: "/work", icon: BriefcaseIcon, label: "Work" },
  { to: "/knowledge", icon: BookOpenIcon, label: "Knowledge" },
] as const;

/** The uppercase initial shown on a project tile (first letter of its name, else `?`). */
function projectInitial(project: ProjectTree["project"]): string {
  const trimmed = project.name.trim();
  return trimmed.length > 0 ? trimmed[0]!.toUpperCase() : "?";
}

/**
 * The sliding SELECTION indicator — one bar that glides between items sharing the same
 * `layoutId` (Motion's shared-layout FLIP). The rail (projects) and the section-nav each
 * own their own `layoutId` so the two indicators are independent. It is the "indicateur
 * d'actif animé" of the rail. Colour + geometry are tokens only.
 */
function ActiveRail({ layoutId }: { layoutId: string }) {
  return (
    <motion.span
      layoutId={layoutId}
      transition={{ type: "spring", duration: 0.4, bounce: 0.25 }}
      className="absolute -left-2.5 top-2 h-5 w-0.5 rounded-r-full bg-foreground"
    />
  );
}

/**
 * The AGGREGATED run-state dot on a rail project tile: the project's WHOLE runtime — every
 * terminal + service across ALL its workspaces, PLUS any agent BLOCKED ON THE USER (`waiting`,
 * yellow) — folded to ONE state via `projectRunState`. It reuses the same `workspaceRunState`
 * + `aggregateState` the workspace dots use (so the project and workspace rollups agree by
 * construction) and the agent `waiting` axis comes from the SAME `agent_activity_snapshot`
 * read model the sidebar rows consume (`useAgentSessions().byActivity`), passed as the
 * activity lookup — no re-implementation of the agent logic.
 *
 * PLACEMENT (finding): it is pinned as a BADGE at the tile's BOTTOM-RIGHT corner (absolute,
 * ring-punched against the rail so it reads as a status badge on the square). It is a SUMMARY
 * for the OTHER projects the user is NOT inside: on the ACTIVE/entered project the detail is
 * already on screen (its workspaces + rows), so the aggregate is redundant and MASKED there
 * (`active` ⇒ render nothing) — it shows on the non-active tiles only. Like every dot in the
 * tree it also HIDES itself in a quiet grey/empty/idle state (`isQuietState` ⇒ `<StatusDot>`
 * renders nothing), so it shows ONLY on a non-active tile once the aggregate is non-quiet
 * (waiting/error/success/running). Rendered inside the tile's `relative` `<li>`.
 */
function ProjectRailDot({ project, active }: { project: ProjectTree; active: boolean }) {
  const workspaceIds = useMemo(() => project.workspaces.map((w) => w.id), [project.workspaces]);
  const { runtimeByWorkspace } = useProjectRuntime(workspaceIds);
  // The live agent-activity map (`terminal_id → activity`) from the SAME read model the
  // sidebar rows use — the only source of the agent `waiting` (blocked-on-user) signal, which
  // is neither an `exec_state` nor a DB session state. `useAgentSessions` owns its own
  // subscription, so the rail dot gets it live without needing the sidebar's provider.
  const { byActivity } = useAgentSessions();
  // The consumed-notifications registry (the SAME one the rows use) makes the rollup
  // notification-aware: a settled success/error the user has already visited stops contributing
  // to the project's aggregate dot, exactly as it stops contributing to its own row's dot.
  const { isConsumed } = useConsumedNotifications();
  const state = useMemo(
    () =>
      projectRunState(
        [...runtimeByWorkspace.values()],
        (terminalId) => byActivity.get(terminalId)?.activity,
        isConsumed,
      ),
    [runtimeByWorkspace, byActivity, isConsumed],
  );
  // Masked on the project the user is INSIDE (active): the detail is already visible there, so
  // the summary dot only shows on the OTHER tiles (finding). Hidden entirely when quiet
  // (idle/empty) so an idle project stays clean; shown as a corner badge the moment the
  // aggregate earns attention. `<StatusDot>` already renders nothing for a quiet state, but we
  // also skip the wrapper so no empty badge box lingers.
  if (active || isQuietState(state)) return null;
  return (
    <span className="absolute -right-0.5 -bottom-0.5 rounded-full bg-background p-px ring-2 ring-muted/40">
      <StatusDot state={state} size="sm" />
    </span>
  );
}

/**
 * `<BaseLayout>` — the navigation SHELL, ported clean from the prototype against the
 * real design system:
 *
 *  - a vertical RAIL on the left (~60px) listing the PROJECTS (`list_projects`): a
 *    letter tile + an AGGREGATED run-state dot (the project's whole runtime folded to
 *    one state, hidden when idle) when not entered, replaced by an animated active
 *    indicator when it is the entered project;
 *  - a SECTION-NAV under the rail: Terminals (`/`) · Work (`/work`) · Knowledge
 *    (`/knowledge`), each a router `<Link>` with an animated active indicator;
 *  - the MAIN column = `<Outlet/>` (the routed page).
 *
 * One enters a PROJECT here (PDR-0008), never a workspace. The rail owns the ACTIVE
 * PROJECT selection (shared via `useActiveProject`) so the routed pages render that
 * project's runtime. No raw hex, no magic px — tokens and the token-backed scale only.
 */
export function BaseLayout() {
  const { projects, createProject, createWorkspace } = useProjects();
  const { activeProjectId, setActiveProjectId } = useActiveProject();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const navigate = useNavigate();

  // The rail's "+ Add project" flow — the SAME picker→create-modal→`create_project` path the
  // old sidebar head used, reused verbatim via `useManualAdd` (no new backend, no duplicated
  // folder-picker/dialog wiring). `addProject` opens the native folder picker then the create
  // modal (folder read-only + editable name); on confirm it calls the v2.1 `create_project` and
  // the new project appears in the rail (the projects list re-lists on success). `dialog` is the
  // mounted modal set, rendered once below.
  const { addProject, dialog: manualAddDialog } = useManualAdd({ createProject, createWorkspace });

  // Default the entered project to the FIRST one once the list loads (and recover when
  // the entered project disappears, e.g. it was deleted). A no-op while a valid project
  // is already entered, so the user's own selection is never overridden.
  useEffect(() => {
    if (projects.length === 0) {
      if (activeProjectId !== null) setActiveProjectId(null);
      return;
    }
    const stillPresent = projects.some((p) => p.project.id === activeProjectId);
    if (!stillPresent) setActiveProjectId(projects[0]!.project.id);
  }, [projects, activeProjectId, setActiveProjectId]);

  return (
    <div className="grid h-svh grid-cols-[60px_1fr] bg-background text-foreground">
      <aside className="flex h-svh flex-col items-center border-r border-border/60 bg-muted/40 py-3">
        <ul className="flex flex-col items-center gap-1.5">
          {projects.map((tree) => {
            const active = tree.project.id === activeProjectId;
            return (
              <li key={tree.project.id} className="relative">
                <button
                  type="button"
                  onClick={() => setActiveProjectId(tree.project.id)}
                  aria-current={active}
                  aria-label={`Project ${tree.project.name}`}
                  title={tree.project.name}
                  className={cn(
                    "flex size-9 items-center justify-center rounded-lg bg-muted font-medium text-xs text-foreground/85 transition-transform hover:scale-105",
                    active && "ring-2 ring-primary/60",
                  )}
                >
                  {projectInitial(tree.project)}
                </button>
                {/* The active-selection indicator shows ONLY on the entered tile; the
                    aggregated run-state dot shows on the OTHER (non-active) tiles — it is
                    masked on the active one (the detail is already on screen) and hides itself
                    when the aggregate is quiet. */}
                {active ? <ActiveRail layoutId="rail-project" /> : null}
                <ProjectRailDot project={tree} active={active} />
              </li>
            );
          })}
          <li>
            <button
              type="button"
              onClick={() => void addProject()}
              className="flex size-9 items-center justify-center rounded-lg border border-dashed border-border/70 text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
              aria-label="Add project"
              title="Add project"
            >
              <PlusIcon className="size-4" />
            </button>
          </li>
        </ul>

        <div className="my-4 h-px w-7 bg-border/60" />

        <ul className="flex flex-1 flex-col items-center gap-1">
          {NAV.map((item) => {
            const active = pathname === item.to;
            return (
              <li key={item.to} className="relative">
                <Link
                  to={item.to}
                  title={item.label}
                  aria-label={item.label}
                  // The MAIN-SCREEN change (Terminals ↔ Work ↔ Knowledge) is wrapped in a
                  // View Transition cross-fade so the screen — terminal included — fades
                  // instead of snapping in ("collé", user feedback). We intercept the click
                  // and drive the navigation ourselves inside `startNavTransition`: it
                  // snapshots the old DOM, cross-fades to the new, and only calls `navigate`
                  // inside the callback. The persistent terminal instance is untouched (no
                  // destroy/respawn) — the VT is pure chrome over the same detach→reattach.
                  // `startNavTransition` no-ops the VT under `prefers-reduced-motion` and when
                  // the API is absent (jsdom / old Chromium), so navigation always works.
                  // We keep `<Link>` (real anchor, active state, a11y) but let a modified /
                  // non-primary click fall through to the browser's default so it is not
                  // hijacked.
                  onClick={(e) => {
                    if (
                      e.defaultPrevented ||
                      e.button !== 0 ||
                      e.metaKey ||
                      e.ctrlKey ||
                      e.shiftKey ||
                      e.altKey
                    ) {
                      return;
                    }
                    if (pathname === item.to) return; // already here — no transition
                    e.preventDefault();
                    startNavTransition(() => {
                      void navigate({ to: item.to });
                    });
                  }}
                  className={cn(
                    "group flex size-9 items-center justify-center rounded-lg transition-colors",
                    active
                      ? "bg-muted text-foreground"
                      : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                  )}
                >
                  <item.icon className="size-4" />
                </Link>
                {active ? <ActiveRail layoutId="rail-nav" /> : null}
              </li>
            );
          })}
        </ul>
      </aside>

      {/* min-w-0 lets the 1fr cell follow the track, not its content (else a wide page
          forces horizontal scroll). The routed page fills this column. */}
      <div className="min-w-0">
        <Outlet />
      </div>

      {/* The add-project modal set (folder picker → create modal). Mounted once, portalled. */}
      {manualAddDialog}
    </div>
  );
}

export default BaseLayout;
