import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router";
import {
  BookOpenIcon,
  BriefcaseIcon,
  ComponentIcon,
  FolderPlusIcon,
  PencilIcon,
  PlusIcon,
  SettingsIcon,
  SlidersHorizontalIcon,
  SquareTerminalIcon,
  Trash2Icon,
  WorkflowIcon,
} from "lucide-react";
import { Menu as BaseMenu } from "@base-ui/react/menu";
import { motion, useReducedMotion } from "motion/react";
import { DragDropProvider } from "@dnd-kit/react";
import { useSortable } from "@dnd-kit/react/sortable";
import { move } from "@dnd-kit/helpers";

import { cn } from "@/lib/utils";
import { reorderSensors, sameOrder } from "@/components/panes/reorder-dnd";
import { startNavTransition } from "@/lib/nav-transition";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { MenuGroup, MenuGroupLabel, MenuItem, MenuSeparator } from "@/components/ui/menu";
import {
  useProjects,
  type ProjectTree,
  type WorkspaceRecord,
} from "@/components/sidebar/use-projects";
import { useManualAdd } from "@/components/sidebar/use-manual-add";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import { useReconcileTerminals } from "@/components/terminal/use-reconcile-terminals";
import { SettingsDialog, type SettingsDialogHandle } from "@/components/sidebar/settings-dialog";
import { ProjectSettingsDialog } from "@/components/sidebar/project-settings-dialog";
import { ChromeBar } from "@/components/chrome/chrome-bar";
import { CloseGuard } from "@/components/chrome/close-guard";
import { isQuietState, StatusDot } from "@/components/ui/status-dot";
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
  { to: "/engine", icon: WorkflowIcon, label: "Engine" },
] as const;

/**
 * …and the FOOT of the same rail — the REFERENCE surfaces, under the work ones (finding
 * `01KZ41YKMB63GFJ3QMK2ZHJN1C`).
 *
 * The CATALOGUE is a project-wide reference, not a place one works in: it is consulted FROM the
 * screen one is on. It used to be duplicated as a `Catalogue` link in the PROTOTYPE page's own
 * header — a global link living in the chrome of one page, three centimetres from a rail that
 * already routed to it. The link is gone from there and the entry lives here, at the foot, beside
 * the other always-available entry point (Settings). One place, whatever screen one is on.
 */
const FOOT_NAV = [{ to: "/design", icon: ComponentIcon, label: "Catalogue" }] as const;

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

/** The per-project rail actions, threaded into the right-click context menu. */
interface ProjectRailActions {
  /** Open the project-settings modal (Global pane — rename + resume). */
  onOpenSettings: () => void;
  /** Add a (non-root) workspace to the project (folder picker → name dialog). */
  onAddWorkspace: () => void;
  /** Open the RENAME dialog for a single workspace (`rename_workspace`), root included. */
  onRenameWorkspace: (workspace: WorkspaceRecord) => void;
  /** Open the destructive-confirm flow for a single (non-root) workspace. */
  onRemoveWorkspace: (workspace: WorkspaceRecord) => void;
  /** Open the destructive-confirm flow for the whole project. */
  onDeleteProject: () => void;
}

/**
 * `<ProjectRailMenuItems>` — the shared body of the project actions menu, rendered inside the
 * right-click context menu (v2.5.6 finding #24: the ⋮ kebab trigger was removed — right-click is
 * the sole entry point). It bundles the project-level CRUD the cockpit rebranches:
 *
 *  - **Project settings** → the modal whose Global pane owns the project RENAME
 *    (`update_project`) + the resume toggle;
 *  - **Add workspace** → the folder-picker/name flow (`create_workspace`);
 *  - one GROUP per NON-ROOT workspace, labelled with its name, carrying that workspace's own
 *    **Rename workspace** (`rename_workspace`) and **Remove workspace**
 *    (`workspace_delete`);
 *  - **Delete project** → `delete_project`.
 *
 * The per-workspace group replaces the former flat "Remove <workspace>" list (review
 * `01M04WQEQDDPH7NQEK9MPYCXKS`): the workspace LINE is where the user looks for what to do to
 * that workspace, and rename had no UI caller at all — `rename_workspace` existed in the bridge
 * and in `use-projects`, and nothing ever invoked it.
 *
 * The ROOT workspace has NO group (review `01M0A7QGBK43MZB10RF51QR6TS`). It briefly got one, on
 * the reasoning that renaming it is legitimate, with its Remove shown disabled and explained —
 * but a section whose only content is one greyed row plus an explanation is a dead end that costs
 * a read to discover. The root's lifecycle is the project's, and Delete project is the entry that
 * reaches it. Its rename goes with the group, by the user's explicit call.
 *
 * The DESTRUCTIVE actions (remove workspace, delete project) are tinted `destructive` and route
 * through `useManualAdd`'s confirm dialogs, so nothing is dropped without an explicit
 * confirmation step.
 */
function ProjectRailMenuItems({
  tree,
  onOpenSettings,
  onAddWorkspace,
  onRenameWorkspace,
  onRemoveWorkspace,
  onDeleteProject,
}: { tree: ProjectTree } & ProjectRailActions) {
  const name = tree.project.name;
  return (
    <>
      <MenuItem
        icon={<SlidersHorizontalIcon className="size-4" />}
        onClick={onOpenSettings}
        aria-label={`Project settings for ${name}`}
      >
        Project settings
      </MenuItem>
      <MenuItem
        icon={<FolderPlusIcon className="size-4" />}
        onClick={onAddWorkspace}
        aria-label={`Add workspace to ${name}`}
      >
        Add workspace
      </MenuItem>
      {/* THE ROOT WORKSPACE GETS NO GROUP AT ALL (review `01M0A7QGBK43MZB10RF51QR6TS`). It used
          to be listed with an active Rename and a DISABLED Remove carrying the backend's reason
          — a section that cost a read to learn there was nothing to do in it. The root's whole
          lifecycle is the project's: it is created with it, removed with it, and Delete project
          (below) is the action that reaches it. Its rename goes with it, by the user's explicit
          call. Non-root workspaces keep both entries, live. */}
      {tree.workspaces
        .filter((ws) => !ws.is_root)
        .map((ws) => (
          <MenuGroup key={ws.id}>
            <MenuSeparator />
            <MenuGroupLabel>{ws.name}</MenuGroupLabel>
            <MenuItem
              icon={<PencilIcon className="size-4" />}
              onClick={() => onRenameWorkspace(ws)}
              aria-label={`Rename workspace ${ws.name} in ${name}`}
            >
              Rename workspace
            </MenuItem>
            <MenuItem
              destructive
              icon={<Trash2Icon className="size-4" />}
              onClick={() => onRemoveWorkspace(ws)}
              aria-label={`Remove workspace ${ws.name} from ${name}`}
            >
              Remove workspace
            </MenuItem>
          </MenuGroup>
        ))}
      <MenuSeparator />
      <MenuItem
        destructive
        icon={<Trash2Icon className="size-4" />}
        onClick={onDeleteProject}
        aria-label={`Delete project ${name}`}
      >
        Delete project
      </MenuItem>
    </>
  );
}

/**
 * `<ProjectRailContextMenu>` — the RIGHT-CLICK entry point to a project tile's actions
 * (v2.5.6 finding #24: the ⋮ kebab was removed, so this is the SOLE way to reach the project
 * CRUD — rename / delete project · add / remove workspace). It reuses the Base UI
 * `Menu` + virtual-cursor-anchor pattern as `<TerminalContextMenu>`: a controlled
 * `Menu.Root` whose `Positioner` reads a zero-size box at the click point, so the popup
 * lands at the cursor. Left-click is untouched (it still enters the project); we only
 * intercept the secondary click and suppress the native OS menu.
 */
function ProjectRailContextMenu({
  tree,
  children,
  ...actions
}: { tree: ProjectTree; children: ReactNode } & ProjectRailActions) {
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
            aria-label={`Project ${tree.project.name} actions`}
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
            <ProjectRailMenuItems tree={tree} {...actions} />
          </BaseMenu.Popup>
        </BaseMenu.Positioner>
      </BaseMenu.Portal>
    </BaseMenu.Root>
  );
}

/**
 * `<SortableProjectTile>` — one drag-sortable project tile of the rail (v2.5.6, task
 * `01KXBNX51EPQ69MVKYG4W960HD`). The rebranch of v1's `sortable-project-item.tsx` into the ACTIVE
 * shell's rail: the OUTER `<li>` is the PLAIN element dnd-kit owns (`useSortable`'s `ref`), so it
 * drives the tile's transform during a drag/reflow while Motion (the `<ActiveRail>` indicator)
 * owns only its own animation — separate elements, nothing to fight (the same split v1 used).
 *
 * Whole-tile drag with the 5px pointer activation (`reorderSensors`): a plain click still ENTERS
 * the project and the right-click context menu still fires (a click under 5px is never a drag), so
 * the drag never breaks the phase-2/3 rail affordances. A drop hands the new project id order to
 * `reorderProjects` (persisted via `projects_reorder`).
 */
function SortableProjectTile({
  tree,
  index,
  active,
  railActions,
  onEnter,
}: {
  tree: ProjectTree;
  index: number;
  active: boolean;
  railActions: ProjectRailActions;
  onEnter: () => void;
}) {
  const { ref, isDragging } = useSortable({ id: tree.project.id, index });
  return (
    <li ref={ref} className={cn("group relative", isDragging && "opacity-60")}>
      {/* Right-click the tile → the project actions (v2.5.6 finding #24: the RIGHT-CLICK menu is
          the ONLY entry point — no visible 3-dot kebab trigger); left-click still enters. */}
      <ProjectRailContextMenu tree={tree} {...railActions}>
        <button
          type="button"
          onClick={onEnter}
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
      {/* The active-selection indicator shows ONLY on the entered tile; the aggregated run-state
          dot shows on the OTHER (non-active) tiles — masked on the active one and hidden when the
          aggregate is quiet. */}
      {active ? <ActiveRail layoutId="rail-project" /> : null}
      <ProjectRailDot project={tree} active={active} />
    </li>
  );
}

/**
 * `<BaseLayout>` — the navigation SHELL, ported clean from the prototype against the
 * real design system:
 *
 *  - the window's own CHROME BAR (`<ChromeBar>`) as the top strip, ABOVE the rail: the
 *    window is frameless, so that bar is what moves / minimizes / maximizes / closes it;
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
  const {
    projects,
    loading: projectsLoading,
    createProject,
    createWorkspace,
    updateProject,
    deleteProject,
    deleteWorkspace,
    renameWorkspace,
    reorderProjects,
    setProjectResumeAgentSessions,
  } = useProjects();
  const { activeProjectId, setActiveProjectId } = useActiveProject();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  // Does the CURRENT url carry one of the `/` route's RUNTIME deep-links (`?terminal=<id>` /
  // `?command=<id>`, written by a Spec's Runtime tab)? Those target a precise runtime object of
  // the project one is currently IN, so they go stale the instant another project is entered —
  // see `enterProject`. Selected to a boolean so the shell re-renders only on the edge.
  const hasRuntimeDeepLink = useRouterState({
    select: (s) => {
      const search = s.location.search as { terminal?: unknown; command?: unknown };
      return search.terminal != null || search.command != null;
    },
  });
  const navigate = useNavigate();

  // APP-WIDE terminal-store reconciliation, mounted ONCE on the always-mounted shell. It disposes
  // a live xterm instance ONLY when its backing record is truly gone from the GLOBAL `list_terminals`
  // set (a real close, by the user or an agent over MCP) — NOT when the user switches project. This
  // is deliberately NOT in the per-project deck: the deck only sees the current project's terminals,
  // so reconciling there killed the previous project's terminals (and their agents) on every switch.
  useReconcileTerminals();

  // The rail's project/workspace CRUD flows — the SAME picker→modal→backend paths the old
  // sidebar head + project kebab used, reused verbatim via `useManualAdd` (no new backend, no
  // duplicated folder-picker/dialog wiring):
  //   - `addProject` opens the native folder picker then the create modal (folder read-only +
  //     editable name); on confirm it calls `create_project` and the new project appears in the
  //     rail;
  //   - `addWorkspace` opens the picker then the add-workspace name dialog (`create_workspace`);
  //   - `removeProject` / `removeWorkspace` open the DESTRUCTIVE-confirm dialog before dropping a
  //     project (`delete_project`) or a single non-root workspace (`workspace_delete`) — so the
  //     kebab's destructive actions always route through an explicit confirmation;
  //   - `editWorkspace` opens the workspace RENAME dialog (`rename_workspace`) — the same
  //     `<ProjectDialog>` shell with `entity="workspace"`, no second modal.
  // `dialog` is the mounted modal set (create/edit/delete project + add/rename/remove workspace),
  // rendered once below. The PROJECT rename is NOT taken from here — the context menu opens the
  // project-settings modal for that.
  const {
    addProject,
    addWorkspace,
    editWorkspace,
    removeProject,
    removeWorkspace,
    dialog: manualAddDialog,
  } = useManualAdd({
    createProject,
    createWorkspace,
    deleteProject,
    deleteWorkspace,
    renameWorkspace,
  });

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

  /**
   * ENTER a project — the ONE path the rail (and its recovery effect) goes through, because
   * entering a project is a NAVIGATION, not only a selection (PRD fix 2).
   *
   * The defect: `onEnter` used to call `setActiveProjectId` alone, so switching project from a
   * page SCOPED to the previous one (a Spec detail, a review, a prototype) left the view on that
   * page — one ended up reading a page that does not belong to the entered project and had to
   * re-navigate by hand.
   *
   * The landing page is `/` (Terminals) — NOT a new target: it is already scoped to the active
   * project and already RESTORES that project's last terminal (`loadLastTerminalId(projectId)` in
   * `<TerminalsPage>`), falling back to its existing empty state when the project has none. The
   * change is routed through the SAME `startNavTransition` the section-nav uses, so the screen
   * cross-fades instead of snapping (and it no-ops cleanly under reduced motion / jsdom).
   *
   * NO STACKING: when the route is ALREADY `/` there is nothing to navigate to — the page simply
   * re-scopes to the newly entered project — so we skip the navigation entirely rather than push
   * a second identical history entry.
   *
   * THE STALE DEEP-LINK (review `01M069MWRNR1PEZZW7YTPAHSKK`): "already on `/`" is NOT always
   * "nothing to do". A Spec's Runtime tab opens a precise terminal/command by navigating to
   * `/?terminal=<id>` (resp. `?command=`), and nothing clears that search afterwards. Those ids
   * belong to the project one is LEAVING, so after the switch `<TerminalsPage>` waits forever for
   * a target that will never appear in the entered project's runtime — and that pending deep-link
   * SUPPRESSES its last-terminal restore, landing the user on an EMPTY deck. So when the url still
   * carries one, we drop the search. `replace` keeps the no-stacking guarantee (and never leaves a
   * back step pointing at another project's runtime object). The landing search is `{}` in the
   * cross-route case too, so a stale deep-link can never ride along.
   */
  const enterProject = useCallback(
    (projectId: string) => {
      setActiveProjectId(projectId);
      if (pathname === "/") {
        if (hasRuntimeDeepLink) void navigate({ to: "/", search: {}, replace: true });
        return;
      }
      startNavTransition(() => {
        void navigate({ to: "/", search: {} });
      });
    },
    [hasRuntimeDeepLink, navigate, pathname, setActiveProjectId],
  );

  // Enter a project once the list loads (and recover when the entered project disappears, e.g.
  // it was deleted). RESTORE the LAST entered project (persisted, across relaunch) when it still
  // exists, else fall back to the FIRST — so a relaunch reopens where the user left off rather
  // than snapping to the first project. A no-op while a valid project is already entered, so the
  // user's own selection is never overridden; and it stays SILENT for the whole pre-load window
  // (the `loading` guard below), which would otherwise drop the persisted id — and any selection
  // a deep-linked page made meanwhile — before the tree arrives.
  //
  // RECOVERY FROM A VANISHED PROJECT (PRD fix 2): when the project the user was INSIDE disappears
  // (deleted), the current route may still be one of ITS pages, so the recovery goes through the
  // same `enterProject` as an explicit switch — the fallback project is entered AND the view
  // leaves the dead project's route. The FIRST entry (nothing entered yet, `activeProjectId ===
  // null`) only SELECTS: at that point no route belongs to a previous project, and navigating
  // would hijack a legitimate deep-link (a Spec opened directly) as the tree lands.
  useEffect(() => {
    // AN EMPTY TREE MEANS "NO PROJECT" ONLY ONCE THE TREE HAS ACTUALLY LOADED. `projects` is also
    // empty for the whole initial-load window (two IPC hops: `list_projects` then `list_workspaces`
    // per project), and a Spec page deep-linked at that same moment ALREADY writes the entered
    // project as soon as its `spec_detail` lands — one hop, so it can easily win the race. Without
    // this guard the reset below then wiped that fresh selection AND its persisted hint
    // (`setActiveProjectId(null)` → `saveLastProjectId(null)`), and nothing ever put it back: the
    // Spec sync is once-per-loaded-Spec by design (`useSpecProjectSync`'s `applied` ref, which is
    // what keeps a rail click from being undone by the page it left), so the user stayed in the
    // WRONG project on the right Spec. While `loading`, the tree has no opinion yet — say nothing.
    if (projectsLoading) return;
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
      if (activeProjectId === null) setActiveProjectId(restore);
      else enterProject(restore);
    }
  }, [projectsLoading, projects, activeProjectId, setActiveProjectId, enterProject]);

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
        reorderProjects: (ids: string[]) => void;
      };
    };
    win.__nyxWork = {
      createProject: (name, rootPath, rootName) => createProject(name, rootPath, rootName),
      listProjects: () => projects,
      // Drive the SAME project reorder the rail drag commits (`projects_reorder` → persisted
      // `order_index`). The WebDriver Actions pointer API is unusable in this Electron harness
      // (`Browser.getWindowForTarget` unsupported), so the reorder gate drives this real callback
      // and still asserts the DOM rail order changed + survived a relaunch (honest persistence).
      reorderProjects: (ids) => reorderProjects(ids),
    };
    return () => {
      delete (window as unknown as { __nyxWork?: unknown }).__nyxWork;
    };
  }, [createProject, projects, reorderProjects]);

  /**
   * ONE entry of the rail's section-nav — written once and rendered by BOTH groups ({@link NAV} at
   * the top, {@link FOOT_NAV} at the foot), so a link's behaviour never depends on which group it
   * sits in.
   *
   * The MAIN-SCREEN change (Terminals ↔ Work ↔ Knowledge ↔ Catalogue) is wrapped in a View
   * Transition cross-fade so the screen — terminal included — fades instead of snapping in
   * ("collé", user feedback). We intercept the click and drive the navigation ourselves inside
   * `startNavTransition`: it snapshots the old DOM, cross-fades to the new, and only calls
   * `navigate` inside the callback. The persistent terminal instance is untouched (no
   * destroy/respawn) — the VT is pure chrome over the same detach→reattach. `startNavTransition`
   * no-ops the VT under `prefers-reduced-motion` and when the API is absent (jsdom / old Chromium),
   * so navigation always works. We keep `<Link>` (real anchor, active state, a11y) but let a
   * modified / non-primary click fall through to the browser's default so it is not hijacked.
   */
  const renderNavItem = (item: {
    to: (typeof NAV)[number]["to"] | (typeof FOOT_NAV)[number]["to"];
    icon: typeof BriefcaseIcon;
    label: string;
  }) => {
    const active = pathname === item.to;
    return (
      <li key={item.to} className="relative">
        <Link
          to={item.to}
          title={item.label}
          aria-label={item.label}
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
  };

  return (
    // The SHELL ROOT is the one box that spans the FULL window — hence `h-[100svh]` and not
    // `h-app`, the project utility (globals.css) that means "the app's USABLE height", i.e.
    // the viewport MINUS the chrome bar, which is what every routed page below claims. The
    // bar is drawn INSIDE this box, so this one has to include it.
    <div className="flex h-[100svh] flex-col bg-background text-foreground">
      {/* THE WINDOW'S OWN TITLE BAR, above everything — the rail included. The window is
          frameless (`frame: false`, src/main/window.ts), so this strip is the ONLY thing
          that lets a mouse move, minimize, maximize or close it: under Windows an
          undecorated window gets nothing from the OS, and a shell that never mounted this
          left the user with a window they could not even close (review finding
          `01M04Z5GCYA3S0T0K6XE125KZS`). It is mounted HERE, on the always-mounted
          navigation shell, so the chrome is present on every route. `controlsVisible` is
          left unset on purpose: the bar resolves it at runtime from the OS env
          (`NYX_WINDOW_CONTROLS`, permissive default) rather than from a hard-coded prop. */}
      <ChromeBar />

      <div className="grid min-h-0 flex-1 grid-cols-[60px_1fr]">
        <aside className="flex h-full flex-col items-center border-r border-border/60 bg-muted/40 py-3">
          {/* DRAG-REORDER the rail's PROJECTS (v2.5.6): one `DragDropProvider` around the project
            tiles (the "+ add project" tile is not sortable). A drop reorders the tree
            optimistically and persists via `projects_reorder` (`reorderProjects`), so the order
            survives a reload. Reuses the SAME dnd-kit setup as the terminal rows. */}
          <DragDropProvider
            sensors={reorderSensors}
            onDragEnd={(event) => {
              if (event.canceled) return;
              const ids = projects.map((p) => p.project.id);
              const next = move(ids, event);
              if (!sameOrder(next, ids)) void reorderProjects(next);
            }}
          >
            <ul className="flex flex-col items-center gap-1.5">
              {projects.map((tree, index) => {
                const active = tree.project.id === activeProjectId;
                // The project CRUD actions, threaded into the right-click context menu (the sole
                // entry point — finding #24). Rename opens the project-settings modal (its Global
                // pane owns `update_project`); the destructive actions route through `useManualAdd`'s
                // confirm dialogs (`removeProject` / `removeWorkspace`).
                const railActions = {
                  onOpenSettings: () => setProjectSettingsTree(tree),
                  onAddWorkspace: () => void addWorkspace(tree),
                  onRenameWorkspace: (ws: WorkspaceRecord) => editWorkspace(ws),
                  onRemoveWorkspace: (ws: WorkspaceRecord) => removeWorkspace(ws),
                  onDeleteProject: () => removeProject(tree),
                };
                return (
                  <SortableProjectTile
                    key={tree.project.id}
                    tree={tree}
                    index={index}
                    active={active}
                    railActions={railActions}
                    onEnter={() => enterProject(tree.project.id)}
                  />
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
          </DragDropProvider>

          <div className="my-4 h-px w-7 bg-border/60" />

          <ul className="flex flex-1 flex-col items-center gap-1">
            {NAV.map((item) => renderNavItem(item))}
          </ul>

          {/* THE REFERENCE ENTRIES, at the foot — see [`FOOT_NAV`]. Same element, same active
            indicator, same transition as the work entries above; only the place differs, because
            what they are differs. */}
          <ul className="flex flex-col items-center gap-1">
            {FOOT_NAV.map((item) => renderNavItem(item))}
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
      </div>

      {/* The OS close-warning guard: intercepts the WM close / quit and confirms before
          dropping LIVE agent sessions. Mounted once on the always-mounted shell so the
          guard is live on every screen; renders nothing until the OS asks to close. */}
      <CloseGuard />

      {/* The add-project modal set (folder picker → create modal). Mounted once, portalled. */}
      {manualAddDialog}

      {/* Global Settings modal (rail gear → Integrations: install/uninstall the Claude
          plugin). Stays mounted across open/close so its exit animation plays. */}
      <SettingsDialog
        ref={settingsDialogRef}
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
      />

      {/* Project-settings modal (rail kebab / right-click → "Project settings"). Its Global
          pane owns the project RENAME (`showRename` — `update_project`) alongside the
          `resume_agent_sessions` toggle (`set_project_resume_agent_sessions`); this is the
          rename half of the PRD-B kebab CRUD (the delete half + add/remove workspace live in
          the kebab's `useManualAdd` confirm dialogs). Resolve the LIVE tree from `projects`
          (not the captured snapshot) so the name + toggle reflect optimistic tree updates
          immediately; fall back to the captured tree until the live one is found. */}
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
            // PRD-B: the Global pane now exposes the project rename (kebab CRUD) alongside
            // the resume toggle. Rename persists via `update_project`.
            showRename
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
