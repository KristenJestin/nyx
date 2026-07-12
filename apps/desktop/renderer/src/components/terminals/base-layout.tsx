import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router";
import {
  BookOpenIcon,
  BriefcaseIcon,
  PlusIcon,
  SettingsIcon,
  SlidersHorizontalIcon,
  SquareTerminalIcon,
} from "lucide-react";
import { Menu as BaseMenu } from "@base-ui/react/menu";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { startNavTransition } from "@/lib/nav-transition";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { MenuItem } from "@/components/ui/menu";
import { useProjects, type ProjectTree } from "@/components/sidebar/use-projects";
import { useManualAdd } from "@/components/sidebar/use-manual-add";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import { useReconcileTerminals } from "@/components/terminal/use-reconcile-terminals";
import { SettingsDialog, type SettingsDialogHandle } from "@/components/sidebar/settings-dialog";
import { ProjectSettingsDialog } from "@/components/sidebar/project-settings-dialog";
import { isQuietState, StatusDot } from "./status-dot";
import { projectRunState } from "./runtime-state";
import { useConsumedNotifications } from "./use-consumed-notifications";
import { useProjectRuntime } from "./use-project-runtime";
import { useActiveProject } from "./active-project";
import { loadLastProjectId } from "./ui-persistence";

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
  // The live agent-activity map (`terminal_id → AgentActivity`) from the SAME read model the
  // sidebar rows + terminal LINES use — the source of every AGENT-AWARE signal a hosted agent
  // contributes to the rollup (working→running, waiting→yellow, ready→green, error→red), none of
  // which is an `exec_state` or a DB session state. The FULL activity is threaded so the project
  // dot agregates exactly what each line shows. `useAgentSessions` owns its own subscription, so
  // the rail dot gets it live without needing the sidebar's provider.
  const { byActivity } = useAgentSessions();
  // The consumed-notifications registry (the SAME one the rows use) makes the rollup
  // notification-aware: a settled success/error the user has already visited stops contributing
  // to the project's aggregate dot, exactly as it stops contributing to its own row's dot.
  const { isConsumed } = useConsumedNotifications();
  const state = useMemo(
    () =>
      projectRunState(
        [...runtimeByWorkspace.values()],
        (terminalId) => byActivity.get(terminalId),
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
 * `<ProjectRailContextMenu>` — the per-project entry point on the rail: a RIGHT-CLICK
 * context menu on a project tile exposing "Project settings" (the only project-level
 * action rebranched here — the full kebab/rename/delete is PRD-B). It reuses the same
 * Base UI `Menu` + virtual-cursor-anchor pattern as `<TerminalContextMenu>`: a controlled
 * `Menu.Root` whose `Positioner` reads a zero-size box at the click point, so the popup
 * lands at the cursor. Left-click is untouched (it still enters the project); we only
 * intercept the secondary click and suppress the native OS menu.
 */
function ProjectRailContextMenu({
  projectName,
  onOpenSettings,
  children,
}: {
  projectName: string;
  onOpenSettings: () => void;
  children: ReactNode;
}) {
  const reduced = useReducedMotion();
  const [open, setOpen] = useState(false);
  // The cursor point the menu anchors to, kept in a ref so the virtual anchor's
  // getBoundingClientRect reads the latest value without re-creating the object.
  const point = useRef({ x: 0, y: 0 });

  const onContextMenu = useCallback((e: React.MouseEvent) => {
    // Replace the native context menu with ours, anchored at the cursor.
    e.preventDefault();
    point.current = { x: e.clientX, y: e.clientY };
    setOpen(true);
  }, []);

  // A zero-size virtual anchor at the cursor; Floating UI positions the popup against
  // it exactly like a real element.
  const anchor = useRef({
    getBoundingClientRect: () =>
      ({
        x: point.current.x,
        y: point.current.y,
        width: 0,
        height: 0,
        top: point.current.y,
        right: point.current.x,
        bottom: point.current.y,
        left: point.current.x,
      }) as DOMRect,
  });

  return (
    <BaseMenu.Root open={open} onOpenChange={setOpen} modal={false}>
      {/* `display:contents` wrapper so the tile keeps its own box/layout; the wrapper
          only captures the right-click. */}
      <div className="contents" onContextMenu={onContextMenu}>
        {children}
      </div>
      <BaseMenu.Portal>
        <BaseMenu.Positioner anchor={anchor.current} side="right" align="start" sideOffset={2}>
          <BaseMenu.Popup
            aria-label={`Project ${projectName} settings`}
            className={cn(
              "z-50 min-w-44 origin-[var(--transform-origin)] rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-lg outline-none",
            )}
            render={
              <motion.div
                initial={{ opacity: 0, scale: 0.96, y: -4 }}
                animate={{ opacity: 1, scale: 1, y: 0 }}
                transition={
                  reduced
                    ? { duration: 0 }
                    : { type: "spring", stiffness: 520, damping: 38, mass: 0.7 }
                }
              />
            }
          >
            <MenuItem icon={<SlidersHorizontalIcon />} onClick={onOpenSettings}>
              Project settings
            </MenuItem>
          </BaseMenu.Popup>
        </BaseMenu.Positioner>
      </BaseMenu.Portal>
    </BaseMenu.Root>
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
  const { projects, createProject, createWorkspace, updateProject, setProjectResumeAgentSessions } =
    useProjects();
  const { activeProjectId, setActiveProjectId } = useActiveProject();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const navigate = useNavigate();

  // APP-WIDE terminal-store reconciliation, mounted ONCE on the always-mounted shell. It disposes
  // a live xterm instance ONLY when its backing record is truly gone from the GLOBAL `list_terminals`
  // set (a real close, by the user or an agent over MCP) — NOT when the user switches project. This
  // is deliberately NOT in the per-project deck: the deck only sees the current project's terminals,
  // so reconciling there killed the previous project's terminals (and their agents) on every switch.
  useReconcileTerminals();

  // The rail's "+ Add project" flow — the SAME picker→create-modal→`create_project` path the
  // old sidebar head used, reused verbatim via `useManualAdd` (no new backend, no duplicated
  // folder-picker/dialog wiring). `addProject` opens the native folder picker then the create
  // modal (folder read-only + editable name); on confirm it calls the v2.1 `create_project` and
  // the new project appears in the rail (the projects list re-lists on success). `dialog` is the
  // mounted modal set, rendered once below.
  const { addProject, dialog: manualAddDialog } = useManualAdd({ createProject, createWorkspace });

  // The global Settings modal (gear at the foot of the rail → Integrations). The dialog
  // stays mounted (Motion animates its exit); we pull a fresh provider list ON THE OPEN
  // EVENT via its imperative handle — not from an effect watching `open` — exactly as the
  // removed `<TerminalManager>` did. This restores the ONLY entry point that installs the
  // Claude plugin (integration_list / install / remove), which the agent detection + session
  // status depend on.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsDialogRef = useRef<SettingsDialogHandle>(null);
  const openSettings = useCallback(() => {
    setSettingsOpen(true);
    settingsDialogRef.current?.reload();
  }, []);

  // The per-project settings modal (Global / Commands): which project's settings are open
  // (`null` = closed). Reached from the rail tile's right-click "Project settings" — the
  // project-level entry point rebranched into the active shell. Its Global pane finally
  // exposes the `resume_agent_sessions` toggle (persisted via `set_project_resume_agent_
  // sessions`), which was previously only mounted by the dead `<TerminalManager>`.
  const [projectSettingsTree, setProjectSettingsTree] = useState<ProjectTree | null>(null);
  // A per-open generation counter folded into the dialog `key` so each open REMOUNTS the
  // dialog (its `initialSection` seed re-applies and the Global fields re-seed from the
  // project's current name/toggle). Bumped on the closed→open edge.
  const projectSettingsGen = useRef(0);
  const projectSettingsWasOpen = useRef(false);
  if (projectSettingsTree !== null && !projectSettingsWasOpen.current)
    projectSettingsGen.current += 1;
  projectSettingsWasOpen.current = projectSettingsTree !== null;

  // Enter a project once the list loads (and recover when the entered project disappears, e.g.
  // it was deleted). RESTORE the LAST entered project (persisted, across relaunch) when it still
  // exists, else fall back to the FIRST — so a relaunch reopens where the user left off rather
  // than snapping to the first project. A no-op while a valid project is already entered, so the
  // user's own selection is never overridden; and it never wipes the selection during the brief
  // pre-load window (empty list + already-null id ⇒ nothing to do), which would otherwise drop
  // the persisted id before the tree arrives.
  useEffect(() => {
    if (projects.length === 0) {
      if (activeProjectId !== null) setActiveProjectId(null);
      return;
    }
    const stillPresent = projects.some((p) => p.project.id === activeProjectId);
    if (!stillPresent) {
      const remembered = loadLastProjectId();
      const restore =
        remembered != null && projects.some((p) => p.project.id === remembered)
          ? remembered
          : projects[0]!.project.id;
      setActiveProjectId(restore);
    }
  }, [projects, activeProjectId, setActiveProjectId]);

  // INERT e2e seam (`window.__nyxWork`), GATED behind the build-time `VITE_NYX_E2E=1` flag —
  // published ONLY by the `build:e2e` renderer build, tree-shaken out of a real production
  // build. It exposes the SAME v2.1 `create_project` the rail's "+ Add project" flow drives
  // (a thin pass-through, no new backend), so the v2.4 spec-workflow e2e gate can seed the dev
  // workspace the Spec anchors on WITHOUT driving the native folder-picker dialog. The shell
  // rewrite (PDR-0025) unmounted the old `<TerminalManager>` `window.__nyx` seam; this restores
  // a minimal project-seed seam on the ALWAYS-MOUNTED navigation shell. The Spec itself is
  // authored/transitioned purely through the real MCP tools by the gate — never through here.
  useEffect(() => {
    if (import.meta.env.VITE_NYX_E2E !== "1") return;
    const win = window as unknown as {
      __nyxWork?: {
        createProject: (name: string, rootPath: string, rootName?: string) => Promise<unknown>;
        listProjects: () => ProjectTree[];
      };
    };
    win.__nyxWork = {
      createProject: (name, rootPath, rootName) => createProject(name, rootPath, rootName),
      listProjects: () => projects,
    };
    return () => {
      delete (window as unknown as { __nyxWork?: unknown }).__nyxWork;
    };
  }, [createProject, projects]);

  return (
    <div className="grid h-svh grid-cols-[60px_1fr] bg-background text-foreground">
      <aside className="flex h-svh flex-col items-center border-r border-border/60 bg-muted/40 py-3">
        <ul className="flex flex-col items-center gap-1.5">
          {projects.map((tree) => {
            const active = tree.project.id === activeProjectId;
            return (
              <li key={tree.project.id} className="relative">
                {/* Right-click the tile → "Project settings" (the per-project entry point;
                    left-click still enters the project). */}
                <ProjectRailContextMenu
                  projectName={tree.project.name}
                  onOpenSettings={() => setProjectSettingsTree(tree)}
                >
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
                </ProjectRailContextMenu>
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

        {/* The global Settings entry point (rebranched from the removed sidebar head). It
            sits at the FOOT of the rail — the section-nav `<ul>` above is `flex-1`, so this
            is pushed to the bottom. Opening it pulls a fresh integration list (via the
            dialog's imperative `reload`) so the Integrations tab can install/uninstall the
            Claude plugin. */}
        <Tooltip label="Settings">
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Settings"
            onClick={openSettings}
            className="mt-1"
          >
            <SettingsIcon />
          </Button>
        </Tooltip>
      </aside>

      {/* min-w-0 lets the 1fr cell follow the track, not its content (else a wide page
          forces horizontal scroll). The routed page fills this column. */}
      <div className="min-w-0">
        <Outlet />
      </div>

      {/* The add-project modal set (folder picker → create modal). Mounted once, portalled. */}
      {manualAddDialog}

      {/* Global Settings modal (rail gear → Integrations: install/uninstall the Claude
          plugin). Stays mounted across open/close so its exit animation plays. */}
      <SettingsDialog
        ref={settingsDialogRef}
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
      />

      {/* Project-settings modal (rail tile right-click → "Project settings"). In this
          phase-1 shell entry point the Global pane exposes ONLY the `resume_agent_sessions`
          toggle (persisted via `set_project_resume_agent_sessions`); the rename field is
          hidden (`showRename={false}`) because project rename/delete belong to the PRD-B
          kebab CRUD. `onRename` stays wired so PRD-B (and the default-`true` callers) are
          unchanged. Resolve the LIVE tree from `projects` (not the captured snapshot) so the
          toggle reflects optimistic tree updates immediately; fall back to the captured tree
          until the live one is found. */}
      {(() => {
        const targetId = projectSettingsTree?.project.id ?? null;
        const liveTree =
          (targetId != null ? projects.find((t) => t.project.id === targetId) : null) ??
          projectSettingsTree ??
          null;
        // The workspace the Commands pane is scoped to: the ROOT, else the first — so the
        // pane is functional if navigated to (Commands itself is out of this task's scope).
        const manageWorkspace =
          liveTree?.workspaces.find((w) => w.is_root) ?? liveTree?.workspaces[0] ?? null;
        return (
          <ProjectSettingsDialog
            // Remount per open so the Global fields re-seed (see `projectSettingsGen`).
            key={`project-settings:${projectSettingsGen.current}`}
            open={projectSettingsTree !== null}
            projectId={liveTree?.project.id ?? null}
            projectName={liveTree?.project.name ?? ""}
            resumeAgentSessions={liveTree?.project.resume_agent_sessions ?? false}
            importWorkspaceId={manageWorkspace?.id ?? null}
            workspacePath={manageWorkspace?.path ?? null}
            initialSection="global"
            // Phase-1: expose only the resume toggle; rename/delete (kebab CRUD) = PRD-B.
            showRename={false}
            onRename={(name) => (targetId ? updateProject(targetId, name) : Promise.resolve())}
            onResumeChange={(resume) =>
              targetId ? setProjectResumeAgentSessions(targetId, resume) : Promise.resolve()
            }
            onClose={() => setProjectSettingsTree(null)}
          />
        );
      })()}
    </div>
  );
}

export default BaseLayout;
