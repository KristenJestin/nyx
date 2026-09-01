import { useEffect, useMemo } from "react";
import { useRouterState } from "@tanstack/react-router";
import { FolderGitIcon, LayersIcon } from "lucide-react";

import { Spinner } from "@/components/ui/spinner";
import { ProjectMark } from "@/components/sidebar/project-mark";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import { useProjects } from "@/components/sidebar/use-projects";
import { useActiveProject } from "@/components/terminals/active-project";

import {
  HomeServicesPanel,
  HomeSessionsPanel,
  HomeSpecsPanel,
  HomeTerminalsPanel,
} from "./home-panels";
import { useHomeFocus } from "./use-home-focus";
import { useHomeSnapshot } from "./use-home-snapshot";

/**
 * `<HomePage>` — the synthesis page, and it is ONE page in two scopes.
 *
 * `/home` is the ENTERED project's recap; `/home?scope=all` is the same page across every project.
 * Not two components and not two read models: the scope picks which `projectId` goes to
 * `home_snapshot` (`null` = every project) and whether each row wears its project mark. Everything
 * else — the columns, the panels, the row anatomy, the sort — is literally the same code, which is
 * the only way the two pages cannot drift.
 *
 * ## The grid
 *
 * Two columns: (Agent sessions, Recent specs) | (Services, Terminals). The order is the reading
 * order: what is BLOCKED ON YOU is the first thing on the page, top-left.
 *
 * ## The sort is global, and it is the backend's
 *
 * The sessions arrive already ordered `waiting > error > ready > working > idle`, oldest first
 * within a rank — the app's own `STATE_PRIORITY`. The page does not re-sort and, in the global
 * scope, does not group by project: a calm project never floats above a project that is waiting.
 *
 * ## No second door
 *
 * The page links to the entities it lists and to nothing else. There is no "see all projects"
 * button, no nav shortcut: the rail already routes everywhere, and a second entry point onto a
 * screen the rail owns is the duplication the elected design removed.
 */
export function HomePage({ scope = "project" }: { scope?: "project" | "all" }) {
  const global = scope === "all";
  const { activeProjectId, setGlobalScope } = useActiveProject();
  const { projects, loading: projectsLoading } = useProjects();
  // THIS PAGE'S URL IS THE APP'S SCOPE, WRITTEN DOWN — so it writes it back.
  //
  // The scope is an application state (`active-project.tsx`), not a parameter of this page: it is
  // what every OTHER page reads too, and it has to survive leaving the home. `?scope=` stays the
  // one url that spells it out, which makes this page the app's scope BOOKMARK: arriving here
  // through `/home?scope=all` — the `All projects` tile, but also a deep link or a restored route
  // — puts the app in the global scope, and arriving on the bare `/home` puts it back in the
  // entered project's. One direction only, and only from the route that names a scope: the four
  // other pages never touch it, which is what lets the mode hold across them.
  //
  // GUARDED ON BEING THE CURRENT SCREEN, and that guard is load-bearing rather than defensive: a
  // page is still mounted while the router commits a navigation AWAY from it, and a re-render in
  // that window — with the destination's search, which of course carries no `scope` — would write
  // "project scope" on the way out and silently cancel the mode the user is carrying to the next
  // page. The mirror only speaks while the home IS the screen.
  const onHomeRoute = useRouterState({ select: (s) => s.location.pathname === "/home" });
  useEffect(() => {
    if (!onHomeRoute) return;
    setGlobalScope(global);
  }, [global, onHomeRoute, setGlobalScope]);
  // THE SCOPE, in one expression. On the project page the read WAITS for the entered project (the
  // rail resolves it one hop after mount) rather than falling through to the global read, which
  // would flash another project's rows on this one.
  const {
    snapshot,
    loading: reading,
    error,
  } = useHomeSnapshot(global ? { global: true } : { global: false, projectId: activeProjectId });
  // A FRESH INSTALL has no project to enter, so the project home has no scope to read and the
  // read never fires. That is not "still loading": the tree HAS answered, with nothing. The page
  // then shows its four empty states — the honest picture — instead of spinning forever on a
  // read that will never be made.
  // NO project EXISTS — not merely "none entered yet", which is a real (short) wait while the rail
  // resolves the entered one and must keep the loading screen.
  const nothingToScope = !global && !projectsLoading && projects.length === 0;
  const loading = reading && !nothingToScope;
  // The LIVE per-terminal agent activity — the same read model the sidebar rows and the rail dot
  // consume. This page lives under the `<Outlet/>`, OUTSIDE the `<AgentSessionsProvider>` the
  // sidebar mounts, so it takes the hook directly (which owns its own subscription), exactly as
  // `<ProjectRailDot>` does. It feeds a row's information slot; the row's STATE stays the
  // backend's.
  const { byActivity } = useAgentSessions();
  const focus = useHomeFocus();

  const project = useMemo(
    () => projects.find((p) => p.project.id === activeProjectId) ?? null,
    [projects, activeProjectId],
  );
  // ONE clock for the whole page, taken per render: every age on screen is read against the same
  // instant, and a test can pin it by pinning the clock.
  const now = Date.now();

  return (
    <div
      data-testid="home-page"
      data-scope={global ? "all" : "project"}
      className="flex h-app flex-col overflow-y-auto bg-background px-6 pt-10 pb-6"
    >
      <header className="mb-5 flex items-center gap-3">
        {global ? (
          <span className="flex size-8 items-center justify-center rounded-lg bg-muted text-muted-foreground">
            <LayersIcon className="size-4" />
          </span>
        ) : project ? (
          <ProjectMark project={project.project} className="size-8" />
        ) : null}
        <h1 className="font-heading text-xl font-semibold tracking-tight text-foreground">
          {global ? "All projects" : (project?.project.name ?? "Home")}
        </h1>
        <span className="ml-auto flex items-center gap-1.5 text-2xs text-muted-foreground">
          <FolderGitIcon className="size-3.5" />
          {global
            ? `${projects.length} ${projects.length === 1 ? "project" : "projects"}`
            : `${project?.workspaces.length ?? 0} ${
                (project?.workspaces.length ?? 0) === 1 ? "workspace" : "workspaces"
              }`}
        </span>
      </header>

      {/* LOADING — the page's own state, before the first read of this scope resolves. The panels
          are not drawn empty in the meantime: an empty state is a STATEMENT ("nothing is
          running"), and making it the loading screen would state something untrue. */}
      {loading ? (
        <div
          data-testid="home-loading"
          className="flex flex-1 items-center justify-center text-muted-foreground"
        >
          <Spinner className="size-6" />
        </div>
      ) : error ? (
        <div
          role="alert"
          data-testid="home-error"
          className="flex flex-1 flex-col items-center justify-center gap-1 text-center"
        >
          <p className="text-sm text-muted-foreground">The home could not be read.</p>
          <p className="text-2xs text-muted-foreground/70">
            It will refresh on its own as soon as something changes.
          </p>
        </div>
      ) : (
        <div className="grid min-h-0 grid-cols-1 items-start gap-4 lg:grid-cols-2">
          <div className="flex min-w-0 flex-col gap-4">
            <HomeSessionsPanel
              sessions={snapshot.sessions}
              byActivity={byActivity}
              showProject={global}
              now={now}
              onOpen={(session) =>
                focus({
                  projectId: session.project?.id ?? null,
                  search: { terminal: session.terminal_id },
                })
              }
            />
            <HomeSpecsPanel specs={snapshot.specs} showProject={global} now={now} />
          </div>
          <div className="flex min-w-0 flex-col gap-4">
            <HomeServicesPanel
              services={snapshot.services}
              showProject={global}
              onOpen={(service) =>
                focus({
                  projectId: service.project.id,
                  search: { command: service.instance_id },
                })
              }
            />
            <HomeTerminalsPanel
              terminals={snapshot.terminals}
              showProject={global}
              onOpen={(terminal) =>
                focus({
                  projectId: terminal.project?.id ?? null,
                  search: { terminal: terminal.terminal_id },
                })
              }
            />
          </div>
        </div>
      )}
    </div>
  );
}
