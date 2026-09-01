import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router";
import {
  BookOpenIcon,
  BriefcaseIcon,
  ComponentIcon,
  CpuIcon,
  FolderPlusIcon,
  HouseIcon,
  LayersIcon,
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
import { ProjectMark } from "@/components/sidebar/project-mark";
import { useManualAdd } from "@/components/sidebar/use-manual-add";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import { useReconcileTerminals } from "@/components/terminal/use-reconcile-terminals";
import { SettingsDialog, type SettingsDialogHandle } from "@/components/sidebar/settings-dialog";
import { ProjectSettingsDialog } from "@/components/sidebar/project-settings-dialog";
import { ChromeBar } from "@/components/chrome/chrome-bar";
import { CloseGuard } from "@/components/chrome/close-guard";
import { SearchPalette } from "@/components/search/search-palette";
import { isQuietState, StatusDot } from "@/components/ui/status-dot";
import { homeSnapshotRunState } from "@/components/home/home-state";
import { useHomeSnapshot, type HomeReadScope } from "@/components/home/use-home-snapshot";
import { projectRunState } from "./runtime-state";
import { useConsumedNotifications } from "./use-consumed-notifications";
import { useProjectRuntime } from "./use-project-runtime";
import { useActiveProject } from "./active-project";
import { useSystemScope } from "@/components/system/use-system-scope";
import { SystemApprovalGate } from "@/components/system/system-approval-gate";
import { loadLastProjectId } from "./ui-persistence";

/**
 * The section-nav entries — the routed pages reachable from the rail. `Terminals` is
 * the index route (`/`); the four others are their own routes. Icons are lucide
 * (the app's icon set); the label doubles as the `title`/`aria-label`.
 *
 * HOME LEADS. It is the recap of the entered project — where the project IS — so it opens the
 * section-nav, above the surfaces one works IN. The cross-project home is not an entry here: it
 * belongs to the SCOPE region at the top of the rail (the "All projects" bar), because its scope is
 * the projects, not a section of one.
 *
 * `DESIGN` CLOSES IT, and it used to sit at the FOOT of the rail under the label `Catalogue`
 * (finding `01KZ41YKMB63GFJ3QMK2ZHJN1C`, since lifted). The foot was for the REFERENCE surfaces,
 * consulted FROM the screen one is on; the design system turned out to be a section of the project
 * like the others, so it came back up into this list — once, not duplicated — and took the name
 * the rest of the app uses for it. The ROUTE is untouched (`/design`): only the label moved.
 *
 * `IDEAS` is the label of `/knowledge`, and that is not a typo. `knowledge` is the GLOSSARY's
 * umbrella — the project knowledge nyx stores, which covers the ideas AND the design system — so
 * it stays the internal route name while the UI names the one thing this page renders. The router
 * runs on a memory history, so the path is never on screen. See `docs/product/README.md`.
 *
 * `crossProject` is what an entry means in the CROSS-PROJECT scope: `true` when the page has a
 * reading that spans every project, `false` when it only ever shows one. See {@link sectionNav}.
 */
const NAV = [
  { to: "/home", icon: HouseIcon, label: "Home", crossProject: true },
  { to: "/", icon: SquareTerminalIcon, label: "Terminals", crossProject: false },
  { to: "/work", icon: BriefcaseIcon, label: "Work", crossProject: true },
  { to: "/knowledge", icon: BookOpenIcon, label: "Ideas", crossProject: true },
  { to: "/engine", icon: WorkflowIcon, label: "Engine", crossProject: false },
  { to: "/design", icon: ComponentIcon, label: "Design", crossProject: false },
] as const;

/**
 * The entries the rail DRAWS for a scope — six in a project, three across every project
 * (`Home`, `Work`, `Ideas`).
 *
 * The three others are ABSENT, not disabled: `Terminals`, `Engine` and `Design` have no
 * cross-project reading that means anything (which is the conclusion that closed the three
 * findings asking for one), and an entry that is present but inert is a promise not kept — the
 * user was explicit. So the nav is DERIVED from the scope at render time: flipping the mode
 * changes the list live, with nothing to reload.
 */
function sectionNav(globalScope: boolean): readonly (typeof NAV)[number][] {
  return globalScope ? NAV.filter((item) => item.crossProject) : NAV;
}

/**
 * …and the FOOT of the same rail — the entries that do NOT depend on the entered project.
 *
 * It holds NO routed entry today, and the empty array is the honest statement of that: what
 * actually lives at the foot is the SYSTÈME tile (`<SystemRailTile>`) and, under it, the Settings
 * button (a dialog, not a route). Both are reachable whatever project one is in, which is what the
 * foot means; neither is a `<Link>`, which is why neither is listed here.
 *
 * The Système tile IS a routed page ({@link SYSTEM_ROUTE}) and still not a `FOOT_NAV` entry, for
 * reasons `renderNavItem` cannot cover: it only exists once the singleton's own read answers, and
 * it carries the scope's aggregate run-state dot. The array stays as the group's declared seam so a
 * PLAIN project-independent page has an obvious home and gets `renderNavItem`'s behaviour for free.
 */
const FOOT_NAV = [] as const;

/**
 * The SYSTÈME page's path — the singleton's terminals and commands (PRD « Scope Système »).
 *
 * It is a PATH and not a flag since human review `01M1EC7Y2625Q07C5386GKHZFS` (finding U1). The page
 * used to be rendered in the INDEX route's slot, switched by a `systemScope` boolean, which had
 * three consequences the user rejected in the real app: the entered project was de-selected, the
 * project's section-nav entries were masked, and — because the page WAS `/` — the rail lit
 * `Terminals` over a screen that was not the project's Terminals.
 *
 * With a path of its own there is nothing left to arbitrate: `pathname === SYSTEM_ROUTE` marks the
 * Système tile, `pathname === "/"` marks `Terminals`, and the entered project is untouched by
 * either. Système is a page one visits, not a place one moves into.
 *
 * It is NOT one of {@link NAV}'s entries and takes no `crossProject` flag: the cross-project scope
 * says WHICH PROJECTS a page reads, and the Système singleton belongs to none of them — the tile
 * shows in either scope, unchanged.
 */
const SYSTEM_ROUTE = "/system" as const;

/**
 * The scope the rail's own cross-project read is made in — hoisted to a module constant so the
 * object identity is stable and the always-mounted {@link AllProjectsRailDot} never re-subscribes.
 */
const GLOBAL_HOME_SCOPE: HomeReadScope = { global: true };

/**
 * The sliding SELECTION indicator — one bar that glides between items sharing the same
 * `layoutId` (Motion's shared-layout FLIP). Each GROUP of the rail owns its own `layoutId` —
 * the projects (`rail-project`), the section-nav (`rail-nav`) and the Système tile at the foot
 * (`rail-system`, a group of one) — so their indicators are independent and none of them ever
 * glides ACROSS a group boundary. It is the "indicateur d'actif animé" of the rail. Colour +
 * geometry are tokens only.
 */
function ActiveRail({ layoutId, className }: { layoutId: string; className?: string }) {
  return (
    <motion.span
      layoutId={layoutId}
      // The indicator NAMES itself in the DOM: it is a bare decorative span otherwise, with no
      // role and no text, and "which indicators are lit" is exactly what a test of the rail has
      // to be able to ask (review 01M1C8AHV63Y0D54BJCSN46737).
      data-rail={layoutId}
      transition={{ type: "spring", duration: 0.4, bounce: 0.25 }}
      // The default offsets are tuned for a `size-9` tile. `className` re-centers the bar on a
      // host of another shape (the `All projects` BAR) so every indicator of the rail still lands
      // on the SAME vertical line, whatever the entry it marks.
      className={cn("absolute -left-2.5 top-2 h-5 w-0.5 rounded-r-full bg-foreground", className)}
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
function RailAggregateDot({
  workspaceIds,
  active,
}: {
  /**
   * The workspaces whose runtime is folded into this tile's dot. A project hands its own; the
   * SYSTÈME tile hands the singleton's ONE primary workspace — same rollup, same rule, one
   * component (a second aggregate would be a second truth about the same colour).
   */
  workspaceIds: readonly string[];
  active: boolean;
}) {
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
 * The CROSS-PROJECT run-state dot — the `All projects` tile's badge, and the only signal that
 * tile carries. Same badge geometry and same quiet-state rule as {@link ProjectRailDot}, because
 * it answers the same question one scope wider: is anything, anywhere, asking for you.
 *
 * NO NUMBER GOES BESIDE IT, here or on any project tile (user decision H1): the dot already ranks
 * what matters (`waiting > error > success > running`, `STATE_PRIORITY`) and a counter next to it
 * would duplicate the signal — the same call as "no badge doubles the dot", frozen since the home
 * landed and checked by the home's own absence tests.
 *
 * IT ROLLS UP THE GLOBAL HOME SNAPSHOT, in ONE read, and that choice is the whole point. The rail
 * builds its project tiles from `list_projects` / `list_workspaces`, which return the USER's
 * projects; a rollup summed over those tiles would silently miss a `system` project, and the
 * cross-project view is precisely where the user refuses to miss it. `home_snapshot` in its global
 * scope joins terminals → workspaces → projects with no filter at all, so it sees every project by
 * construction — see {@link homeSnapshotRunState}. It is also ONE call for the whole scope, never
 * a per-project fan-out, which is the read model's standing rule.
 *
 * It is NOT masked while the app is in the global scope: unlike an entered project, whose detail
 * fills the screen under it, the global scope holds on every page and the tile stays the one place
 * that says what is happening across all of them.
 */
function AllProjectsRailDot() {
  const { snapshot } = useHomeSnapshot(GLOBAL_HOME_SCOPE);
  const state = useMemo(() => homeSnapshotRunState(snapshot), [snapshot]);
  if (isQuietState(state)) return null;
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
 *
 * BEING the entered project (`active`) and being SHOWN as the rail's selection (`selected`) are
 * two different things, and only in the GLOBAL SCOPE do they part ways — see `selected`.
 */
function SortableProjectTile({
  tree,
  index,
  active,
  selected,
  railActions,
  onEnter,
}: {
  tree: ProjectTree;
  index: number;
  /** THE ENTERED PROJECT — what the tile IS. Drives `aria-current` and the aggregate dot's mask. */
  active: boolean;
  /**
   * …and whether the rail DRAWS that selection. They differ in ONE situation, and one only: the
   * GLOBAL SCOPE, on ANY page. There the `All projects` tile is the lit selection because it is
   * what the pages are showing, and a ring on a project would say the screen is scoped to that
   * project — which is exactly what it is not. Two marks in the SAME region, contradicting each
   * other, is what the user reads as a second selection ("la double selection c'est hyper chelou").
   *
   * HOME IS NOT A CASE OF ITS OWN — it briefly was, and that was an over-correction (review
   * `01M1EJK7VF9WG86V85AJ5K8JFC`). The rule above was extended to `!onHome`, which took the ring
   * away on the PROJECT home too: the page displayed the project's name in its header while the
   * rail said "no project", and Home became the only page in the app that de-selects the project.
   * There is no contradiction to resolve there — one is inside a project, on that project's home,
   * and the ring says true. The rail's two regions answer two questions (the project list says
   * WHICH SCOPE, the section-nav says WHICH PAGE) and on the project home BOTH have something to
   * say at once: the project's ring AND the lit `Home` entry — the same shape already settled for
   * the global home, where the `All projects` tile and `Home` light together.
   *
   * The scope wins: in the global scope the entered project keeps its `aria-current` and its
   * place, and gives up only the ring — nothing takes its place, in no attenuated form, or there
   * would be two marks again.
   */
  selected: boolean;
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
            "flex size-9 items-center justify-center rounded-lg transition-transform hover:scale-105",
            selected && "ring-2 ring-primary/60",
          )}
        >
          {/* The project's CHOSEN identity — its accent + its icon/image — or, when it has
              none, the initial on the neutral tile the rail always showed. One component, the
              same one the home renders beside a cross-project session. */}
          <ProjectMark project={tree.project} className="size-full" />
        </button>
      </ProjectRailContextMenu>
      {/* The active-selection indicator shows ONLY on the tile the rail draws as selected — the
          entered one, and not while the global scope owns this region's mark. The aggregated
          run-state dot is unmoved by that: it keys off `active`, so the entered project stays
          masked in the global scope too and no mark slides in where the ring left. */}
      {selected ? <ActiveRail layoutId="rail-project" /> : null}
      <ProjectRailDot project={tree} active={active} />
    </li>
  );
}

/** The per-project reading of {@link RailAggregateDot} — its workspaces, folded to one dot. */
function ProjectRailDot({ project, active }: { project: ProjectTree; active: boolean }) {
  const workspaceIds = useMemo(() => project.workspaces.map((w) => w.id), [project.workspaces]);
  return <RailAggregateDot workspaceIds={workspaceIds} active={active} />;
}

/**
 * `<SystemRailTile>` — the entry point to the SYSTÈME PAGE, AT THE FOOT OF THE RAIL, above Settings
 * (human review `01M1E44265T9F3B6TY8END35TW`, finding S1).
 *
 * ## A SERVICE entry, not a peer of the projects
 *
 * It used to be pinned at the HEAD of the rail, over the projects — the placement the `rail-dedie`
 * prototype had (the annex now carries a supersession note on that one point). Testing it, that
 * head slot read the tile as one MORE scope to choose between, competing with the project
 * selection. It is not that: it is the HOST the app itself runs on. So it sits with the other
 * always-available service entry point — Settings — at the foot, in the foot group's list.
 *
 * ## It marks a PAGE, and takes nothing from the project (finding U1)
 *
 * `active` is `pathname === SYSTEM_ROUTE` — the same rule as any other rail entry, read off the
 * router. It used to be a `systemScope` flag that also switched the project tile off and masked the
 * project's section-nav; entering Système now changes the PAGE and only the page, so the project
 * tile beside it stays lit and Work / Ideas / Engine / Design stay clickable.
 *
 * ## OUTSIDE the drag-reorder
 *
 * It is rendered OUTSIDE the `<DragDropProvider>` that wraps the project tiles, in the foot list,
 * under its own separator — so it is structurally not a sortable item: there is no `useSortable`, no
 * index, and `reorderProjects` (which persists `projects_reorder` over the project array) can never
 * receive its id. That is the same invariant one level down: the singleton is absent from
 * `list_projects`, so `useProjects` holds only user projects, so the array being dragged has
 * nothing of the scope in it. The tile is pinned by CONSTRUCTION, not by a sort key — moving it to
 * the foot changed WHERE it is, not that.
 *
 * ## The SECTION-NAV's rule, not the project tile's skin (human review `01M1E7K6D81M057JG5MVV4J7QE`)
 *
 * `size-9`, `rounded-lg` — the geometry the project tiles and the section-nav icons SHARE. What it
 * no longer shares with a project tile is the SKIN: a permanent `bg-muted`, an `ring-2
 * ring-primary/60` for the active state and a `hover:scale-105`. That skin was inert while the tile
 * sat at the head among identical squares; at the foot, among bare icons, the same pixel started
 * saying "selected" all the time.
 *
 * So it follows [`renderNavItem`]'s rule verbatim: `bg-muted text-foreground` when the page is the
 * one on screen, `text-muted-foreground hover:bg-muted/60 hover:text-foreground` at rest, on
 * `transition-colors`. The background is not an ornament — it IS the active marker.
 *
 * THE RING AND THE SCALE ARE GONE, both, and deliberately. Once the background carries the state,
 * `ring-2 ring-primary/60` is a THIRD marker for one state (background + ring + the indicator bar
 * below) where a nav icon uses two — a redundancy that does not emphasise, it just makes this one
 * tile louder than its neighbours for the very state they all share. And `hover:scale-105` is a
 * project-tile affordance: those tiles wear a permanent background, so their hover had to be said
 * by GEOMETRY. A nav icon says hover with `hover:bg-muted/60` — the same channel as its active
 * state, which is what makes the family read as one. Keeping the scale would leave the tile
 * speaking two hover languages at once. Aligning is choosing one rule, not keeping half of each.
 *
 * What still tells it apart from a nav icon is the GLYPH (a chip, not a section) and the rule above
 * it, never a colour of its own: it takes no semantic token, because "system" is a nature, not a
 * state.
 *
 * ## No context menu
 *
 * A project tile right-clicks into rename / delete / add-workspace. The singleton has none of them —
 * the backend refuses all three — so the tile carries no menu at all rather than one that opens onto
 * refusals.
 *
 * Renders NOTHING when the scope is absent (a database that never bootstrapped): a tile onto a place
 * that does not exist would be a dead affordance.
 */
function SystemRailTile({
  workspaceId,
  active,
  onEnter,
}: {
  workspaceId: string;
  active: boolean;
  onEnter: () => void;
}) {
  const workspaceIds = useMemo(() => [workspaceId], [workspaceId]);
  return (
    <li className="group relative">
      <button
        type="button"
        onClick={onEnter}
        aria-current={active}
        aria-label="System"
        title="System"
        className={cn(
          // The SECTION-NAV rule, verbatim (see `renderNavItem`): the background IS the active
          // marker, so it appears with the state and not before it.
          "flex size-9 items-center justify-center rounded-lg transition-colors",
          active
            ? "bg-muted text-foreground"
            : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
        )}
      >
        <CpuIcon className="size-4" />
      </button>
      {/* Its OWN `layoutId` — a shared-layout group of ONE, so the bar fades in and out in place.
          It used to share `rail-project` with the project tiles ("one indicator for the whole
          rail"), which held while the tile was their peer at the head; from the foot it would make
          the bar GLIDE across the boundary between two distinct groups (finding S4). And it does
          not join `rail-nav` for the same reason: the section-nav is the group ABOVE the foot rule,
          a bar sliding from `Catalogue` down to here would cross that boundary too. One group, one
          `layoutId` — the rule the whole rail is built on, applied to a group of one.

          NOTE — the second reason this comment used to give ("`Terminals` already owns that
          indicator while the scope is entered") DIED WITH THE SCOPE (finding U1). Système has its
          own route now, so `Terminals` is at rest while this tile is active; the group-boundary
          argument above is the one that still holds, and it holds on its own. */}
      {active ? <ActiveRail layoutId="rail-system" /> : null}
      <RailAggregateDot workspaceIds={workspaceIds} active={active} />
    </li>
  );
}

/**
 * `<BaseLayout>` — the navigation SHELL, ported clean from the prototype against the
 * real design system:
 *
 *  - the window's own CHROME BAR (`<ChromeBar>`) as the top strip, ABOVE the rail: the
 *    window is frameless, so that bar is what moves / minimizes / maximizes / closes it;
 *  - a vertical RAIL on the left (~60px), opening on the `All projects` BAR in its own
 *    pinned region — the cross-project scope, ruled off from the tiles below it;
 *  - under that rule, the PROJECTS (`list_projects`), drag-sortable: a letter tile + an
 *    AGGREGATED run-state dot (the project's whole runtime folded to one state, hidden
 *    when idle) when not entered, replaced by an animated active indicator when it is
 *    the entered project;
 *  - a SECTION-NAV under the rail, DERIVED FROM THE SCOPE ({@link sectionNav}): Home ·
 *    Terminals · Work · Ideas · Engine · Design inside a project, Home · Work · Ideas
 *    across every project — each a router `<Link>` with an animated active indicator;
 *  - a FOOT GROUP under it: the SYSTÈME page's tile (`/system`), then the Settings button
 *    — what is reachable whatever project one is in, and in either scope;
 *  - the MAIN column = `<Outlet/>` (the routed page).
 *
 * One enters a PROJECT here (PDR-0008), never a workspace. The rail owns the ACTIVE
 * PROJECT selection (shared via `useActiveProject`) so the routed pages render that
 * project's runtime. No raw hex, no magic px — tokens and the token-backed scale only.
 *
 * THE ENTERED PROJECT SURVIVES EVERY PAGE, Système included (human review
 * `01M1EC7Y2625Q07C5386GKHZFS`, finding U1). The shell used to carry a SECOND selection beside the
 * project — a `systemScope` flag — which switched the project tile off, masked four of the five nav
 * entries and redirected project-scoped routes back to the index. All three are gone: what changes
 * when one visits Système is the ROUTE, and a route change has never de-selected a project here.
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
  const { activeProjectId, setActiveProjectId, globalScope, setGlobalScope } = useActiveProject();
  // The SYSTÈME singleton, read through its OWN door (`system_scope_detail`) — never through
  // `useProjects`, whose array is drag-reorderable and user-only by backend contract. `null` while
  // it loads, and on a database that has no scope yet: the tile simply does not exist then.
  const { scope: systemScopeDetail, loading: systemScopeLoading } = useSystemScope();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  // Is the SYSTÈME page the one on screen? The whole "am I in System" question, answered by the
  // router and by nothing else since finding U1 — there is no scope flag to keep in step with it.
  const onSystemPage = pathname === SYSTEM_ROUTE;
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
  // Is the CROSS-PROJECT home the SCREEN one is on? Distinct from `globalScope`, which is the
  // app-wide SCOPE and holds on every page: this one answers "is the global home already up", and
  // it is only ever used to decide whether a click on the `All projects` tile still has somewhere
  // to navigate. Selected to a boolean so the shell re-renders only on the edge.
  const onGlobalHome = useRouterState({
    select: (s) =>
      s.location.pathname === "/home" && (s.location.search as { scope?: unknown }).scope === "all",
  });
  // Is a HOME the current screen — EITHER scope? `/home` serves both, so the bare path answers it.
  // Its ONLY reader is `enterProject`: a home already IS the screen a project switch would take
  // you to, so there is nothing to navigate from there. It says nothing about the rail's marks —
  // the ring follows the SCOPE, on Home like on every other page (see `<SortableProjectTile>`'s
  // `selected`).
  const onHome = pathname === "/home";
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
   * entering a project is a NAVIGATION and a SCOPE change, not only a selection.
   *
   * The original defect (PRD fix 2): `onEnter` called `setActiveProjectId` alone, so switching
   * project from a page SCOPED to the previous one (a Spec detail, a review, a prototype) left the
   * view on that page — one ended up reading a page that does not belong to the entered project.
   *
   * THE LANDING PAGE IS THE PROJECT'S HOME (user feedback: « si je suis sur la home et que je
   * change de projet ca reste sur la home, si je suis sur une autre page et que je change de projet
   * au lieu d'aller dans terminals ca va sur la home »). It used to be `/` (Terminals) from every
   * route but a home, which made the switch answer two different questions depending on where you
   * clicked from. One answer now: entering a project SHOWS you that project, and the screen that
   * shows a project is its home. This LIFTS the Home task's declared non-goal ("Faire de Home la
   * cible du switch de projet"), which the user lifted explicitly after having it under their eyes.
   *
   * The landing search is `{}`, always, and that is what keeps THE STALE DEEP-LINK (review
   * `01M069MWRNR1PEZZW7YTPAHSKK`) buried. A Spec's Runtime tab opens a precise runtime object by
   * navigating to `/?terminal=<id>` (resp. `?command=`), and nothing clears that search afterwards.
   * Those ids belong to the project one is LEAVING: left in the url, `<TerminalsPage>` waits
   * forever for a target that will never appear in the entered project's runtime, and that pending
   * deep-link SUPPRESSES its last-terminal restore — an EMPTY deck. Landing on `/home` disposes of
   * it by leaving the route entirely; and because a deep-linked `/` in the BACK stack is the same
   * trap one step away, the navigation REPLACES it rather than stacking on top of it
   * ({@link hasRuntimeDeepLink}). From a plain page the navigation is a normal push: the page one
   * came from is a legitimate place to go back to.
   *
   * ON A HOME, THERE IS NOTHING TO NAVIGATE (user feedback: "quand je change de projet ca quitte la
   * home du coup c'est trop bizarre"). The home is the same screen for every project and answers
   * the switch on its own by re-reading in the new scope. From the PROJECT home nothing in the url
   * has to move at all; from the GLOBAL home the search must be dropped — entering a project from
   * the cross-project view is leaving it — and `replace` keeps the switch from stacking a second
   * identical screen.
   *
   * ENTERING A PROJECT LEAVES THE GLOBAL SCOPE, always and everywhere: the two are exclusive (see
   * `active-project.tsx`). This is the ONE gesture that says "not all of them — this one", so it is
   * the one that turns the mode off, whatever page it is made from.
   */
  const enterProject = useCallback(
    (projectId: string) => {
      setActiveProjectId(projectId);
      setGlobalScope(false);
      if (onHome) {
        if (onGlobalHome) void navigate({ to: "/home", search: {}, replace: true });
        return;
      }
      startNavTransition(() => {
        void navigate({ to: "/home", search: {}, replace: hasRuntimeDeepLink });
      });
    },
    [hasRuntimeDeepLink, navigate, onGlobalHome, onHome, setActiveProjectId, setGlobalScope],
  );

  /**
   * OPEN THE SYSTÈME PAGE — a plain navigation to {@link SYSTEM_ROUTE}, through the same
   * `startNavTransition` cross-fade every section-nav entry uses.
   *
   * It is a navigation and NOTHING ELSE (finding U1). It used to be the twin of
   * {@link enterProject} — it raised a `systemScope` flag, which de-selected the project tile,
   * masked the project's nav entries and armed a redirect off every project-scoped route, all to
   * land on `/` because the page had no path of its own. The page has one now, so opening it is
   * what opening `/work` is: a route change. The entered project is not touched, the section-nav
   * is not touched, and there is nothing to redirect — the user can read System and go straight
   * back to the Spec they came from.
   *
   * The `?terminal=`/`?command=` deep-link drop that used to live here left with the shared slot:
   * those search params belong to `/`, and `/system` is a different route with no search at all.
   */
  const openSystemPage = useCallback(() => {
    if (pathname === SYSTEM_ROUTE) return; // already here — no transition
    startNavTransition(() => {
      void navigate({ to: SYSTEM_ROUTE });
    });
  }, [navigate, pathname]);

  // The singleton can VANISH under the page (a database read that stops answering, or one that
  // never bootstrapped). The tile then stops being rendered, so the route it leads to would be a
  // screen nothing in the rail points at — leave it for the index rather than strand the user
  // there. Gated on `loading` so the FIRST read (which answers `null` until it lands) never
  // bounces someone who opened `/system` directly. `<SystemScopePage>` keeps its own honest
  // "not available on this database" state for the case where the read settles that way while it
  // is already on screen.
  useEffect(() => {
    if (!onSystemPage || systemScopeLoading || systemScopeDetail != null) return;
    void navigate({ to: "/", search: {}, replace: true });
  }, [onSystemPage, systemScopeLoading, systemScopeDetail, navigate]);

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
      // ON THE SYSTÈME PAGE, the recovery only re-points the remembered project: navigating would
      // eject the user off a page they explicitly opened because a project they are not looking at
      // disappeared. The selection is restored silently, ready for when they go back to it.
      if (activeProjectId === null || onSystemPage) setActiveProjectId(restore);
      else enterProject(restore);
    }
  }, [
    projectsLoading,
    projects,
    activeProjectId,
    setActiveProjectId,
    enterProject,
    onSystemPage,
  ]);

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
   * ONE entry of the rail's section-nav — written once and rendered by BOTH groups: every entry
   * {@link sectionNav} keeps in the current scope at the top, and {@link FOOT_NAV} at the foot
   * (empty today). So a link's behaviour never depends on which group it sits in, nor on the scope.
   *
   * The MAIN-SCREEN change (Terminals ↔ Work ↔ Ideas ↔ Design) is wrapped in a View
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
    to: (typeof NAV)[number]["to"];
    icon: typeof BriefcaseIcon;
    label: string;
  }) => {
    // THE RAIL ANSWERS TWO QUESTIONS IN TWO REGIONS, and this one is "WHICH PAGE?".
    //
    // Everything above the rule answers "WHICH SCOPE?" — the `All projects` bar and, under it, the
    // project tiles.
    // The two are INDEPENDENT axes, so being in the cross-project scope AND on the Home page is a
    // perfectly coherent state and BOTH regions say so — the `All projects` tile is lit and this
    // entry is lit (user feedback: « la route home est pas selectionne quand je suis dans "all
    // project" »).
    //
    // This entry used to be forced dark on `/home?scope=all`, which was an over-application of the
    // double-selection rule the user had removed (review `01M1C8AHV63Y0D54BJCSN46737`). That rule
    // is about two marks in the SAME region contradicting each other — the `All projects` tile lit
    // WHILE a project wore its entered ring. Two marks in two regions answering two questions are
    // not that. The same conclusion later had to be applied one step further, on the PROJECT home:
    // the ring stays ON there, beside this lit entry (review `01M1EJK7VF9WG86V85AJ5K8JFC`).
    //
    // The other entries are scope-NEUTRAL: `Work` and `Ideas` are the same screen in either scope,
    // which is the whole point of the mode ("si je vais sur toute les autres pages en etant sur le
    // all project ca m'affiches les pages normalement avec tout"), and the three that are not are
    // simply not drawn there.
    const active = pathname === item.to;
    // …AND A CLICK IS A NO-OP ONLY WHEN IT HAS NOWHERE TO GO, which is NOT the same question.
    // `Home` is the one entry that can be lit and still have somewhere to take you: on
    // `/home?scope=all` the path already matches while the SCREEN is the cross-project home, so
    // the click still has to run — it is what leaves the global scope for the project recap.
    const alreadyHere = active && !(item.to === "/home" && globalScope);
    return (
      // `data-active` is the rail's OWN answer to "which entry is the PAGE you are on" — the one
      // the sliding indicator follows, and the one a test reads (the router runs on memory
      // history, so `location` is never the observable). It answers the page axis only: the SCOPE
      // is said by the project list, in its own region, with its own indicator.
      <li key={item.to} data-active={active} className="relative">
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
            // `alreadyHere`, not `active` and not the bare path: on `/home?scope=all` this entry
            // is LIT (the page axis says Home) yet the screen is the OTHER home, so it still has
            // somewhere to go.
            if (alreadyHere) return; // already here — no transition
            e.preventDefault();
            // `Home` is the ENTERED PROJECT's recap — that is what the entry means, and what
            // separates it from the `All projects` tile that owns the other home. So it is also a
            // way OUT of the global scope, the mirror of the tile that is the way in. The four
            // other entries never touch the scope.
            if (item.to === "/home") setGlobalScope(false);
            startNavTransition(() => {
              // `search: {}` CLEARS the url's query. It is here for `/home`, which is the only
              // entry that can navigate to the path it is ALREADY on: from `/home?scope=all` the
              // screen is the cross-project home, so the Home entry still has somewhere to go —
              // and TanStack keeps the current search when a navigation does not replace it, so
              // without this the entry could never leave the global scope.
              //
              // It is inert for the others: `/work` and `/knowledge` take no search at all,
              // and `/`'s only ones are the `?terminal=` / `?command=` DEEP-LINKS, which are
              // consumed on arrival and belong to the url that CARRIED them — never to a rail
              // click, which means "take me to the Terminals screen", not "take me back to
              // whatever this url pointed at". Every rail entry therefore lands on its screen's
              // plain form, which is also the only behaviour that does not depend on where you
              // clicked from.
              void navigate({ to: item.to, search: {} });
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
          {/* ALL PROJECTS — its OWN region, pinned above the projects and ruled off from them.
            It answers the same question as the tiles below (WHICH SCOPE am I in), which is why it
            is here and not in the section-nav; but it is not one MORE project, and the rail now
            says so structurally instead of by decoration.

            IT LEFT THE LIST, AND THAT IS THE POINT (review `01M1EJK7VF9WG86V85AJ5K8JFC`). It used
            to be the first `<li>` of the sortable project list, wearing a project's `size-9`
            rounded square with a `layers` glyph. A PRD of project identity customisation is coming
            (`01M1C8DP7VBD0CAFPG1Y4Z6662`): the moment a user can pick a project's icon and accent,
            any distinction drawn in THAT vocabulary is one a user choice can undo. So the tile
            changes CATEGORY rather than decoration — its own region, its own geometry.

            THE GEOMETRY IS A BAR, not a square: `h-7 w-11` where a project is `size-9`. Wider than
            the column of squares and shorter than them, so it reads as a header over the list
            rather than an item of it. A user chooses what goes INSIDE their tile, never the SHAPE
            of the tile — which is what makes this distinction impossible to imitate.

            IT IS OUTSIDE THE `<DragDropProvider>` below, so it is structurally not a sortable
            item: no `useSortable`, no index, and `reorderProjects` (which persists the PROJECT
            array) can never receive it. It is pinned by CONSTRUCTION, not by a sort key. This is
            the same convention the Système chantier uses for its own pinned tile
            (`01KY4FJXT0J2XSP53KK61ZA5R8`, branch `feature/system-scope`): a region of its own,
            outside the provider, under its own rule. That one pins at the FOOT, above Settings,
            and this one at the HEAD — so the two merge without competing.

            It is DRAWN as selected from the app's scope, not from the route: the global mode holds
            on every page now (`Work`, `Knowledge`, …), so a tile lit only on the cross-project home
            would go dark the moment you navigate and leave the rail unable to say what the pages
            are showing. It still NAVIGATES to the cross-project home — entering the scope shows you
            the scope — which is why the "already here" guard reads the ROUTE (`onGlobalHome`) and
            not the mode: from `/work` in the global scope the tile has somewhere to go.

            It does not ENTER or LEAVE a project: `activeProjectId` is untouched, so the rail knows
            where to put you back the moment you enter a project again. */}
          <ul className="flex flex-col items-center">
            <li data-active={globalScope} className="relative">
              <Link
                to="/home"
                search={{ scope: "all" }}
                title="All projects"
                aria-label="All projects"
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
                  setGlobalScope(true);
                  if (onGlobalHome) return;
                  e.preventDefault();
                  startNavTransition(() => {
                    void navigate({ to: "/home", search: { scope: "all" } });
                  });
                }}
                className={cn(
                  // THE BAR — the one geometry of the rail no project tile can claim.
                  "flex h-7 w-11 items-center justify-center rounded-md transition-colors",
                  globalScope
                    ? "bg-muted text-foreground"
                    : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                )}
              >
                <LayersIcon className="size-4" />
              </Link>
              {/* Its OWN `layoutId`, still — and now for a second reason. It never was one of the
                  project tiles (entering the cross-project home leaves the entered project ENTERED,
                  so sharing `rail-project` would have Motion slide the project's bar up onto this
                  one and back down on the way out, animating a project switch that never happened);
                  and it now sits in another REGION, across a rule, which a shared-layout bar must
                  never glide over. A group of one: it fades in and out in place. The `className`
                  re-centers it on the bar's height so it lands on the same vertical line as the
                  project indicators below. */}
              {globalScope ? (
                <ActiveRail layoutId="rail-all-projects" className="-left-1.5 top-1" />
              ) : null}
              {/* The CROSS-PROJECT rollup badge — the same corner badge a project tile wears, over
                  every project at once (Système included). No counter beside it. */}
              <AllProjectsRailDot />
            </li>
          </ul>

          {/* The rule that MAKES the region: what is above it is the cross-project scope, what is
            below it is the projects. Tighter than the section-nav's rule further down (`my-4`),
            because these two regions answer the same question — the wider gap is what separates
            "which scope" from "which page". */}
          <div className="my-2 h-px w-7 bg-border/60" />

          {/* DRAG-REORDER the rail's PROJECTS (v2.5.6): one `DragDropProvider` around the project
            tiles (the "+ add project" tile is not sortable, and the `All projects` bar above is not
            even inside it). A drop reorders the tree optimistically and persists via
            `projects_reorder` (`reorderProjects`), so the order survives a reload. Reuses the SAME
            dnd-kit setup as the terminal rows. */}
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
                // THE ENTERED PROJECT IS THE ENTERED PROJECT, whatever page is on screen — Système
                // included (finding U1). This used to read `!systemScope && …`, which switched the
                // tile off the moment one opened a page that is not a project's.
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
                    selected={active && !globalScope}
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

          {/* THE SECTION-NAV, derived from the SCOPE ({@link sectionNav}) — six entries inside a
            project, three across every project. The three without a cross-project reading are not
            rendered at all there, so the list simply changes length when the mode flips; nothing
            reloads and no entry is left present-but-inert.

            THE SYSTÈME PAGE MASKS NONE OF THEM (finding U1). Four used to be hidden while the
            Système SCOPE was entered, on the reasoning that Work / Ideas / Engine / Design are
            surfaces of a code project and the singleton has none of the four aggregates. True of
            the SINGLETON, and beside the point once Système is a page: the entered project is
            still entered while one reads it, so those entries still lead to that project's
            surfaces. Losing the way back to one's own Spec board for the time of a visit is the
            defect, not the guard. The SCOPE is now the only thing that shortens this list. */}
          <ul className="flex flex-1 flex-col items-center gap-1">
            {sectionNav(globalScope).map((item) => renderNavItem(item))}
          </ul>

          {/* THE FOOT GROUP — see [`FOOT_NAV`]. It holds what is always available WHATEVER project
            one is in (the section-nav above is `flex-1`, so everything from here down is pushed to
            the bottom): the SYSTÈME tile, and under it — past the rule — the Settings button. The
            tile is absent until the scope's dedicated read answers, and absent for good on a
            database with no singleton: a tile onto a place that does not exist would be a dead
            affordance. */}
          <ul className="flex flex-col items-center gap-1">
            {FOOT_NAV.map((item) => renderNavItem(item))}
            {systemScopeDetail != null ? (
              <SystemRailTile
                workspaceId={systemScopeDetail.workspace.id}
                active={onSystemPage}
                onEnter={openSystemPage}
              />
            ) : null}
          </ul>

          {/* THE RULE SEPARATES WHAT NAVIGATES FROM WHAT OPENS (finding X2). It used to sit ABOVE
            the list, fencing the Système tile off from the section-nav — which made sense while
            Système was a SCOPE one entered instead of a project. It is a routed PAGE since U1,
            peer of Terminals / Work / Ideas / Engine / Design, so fencing it off from them
            drew the one boundary the model says does not exist. The boundary that DOES exist is
            the next one down: everything above the rule is a route, Settings below it is a dialog.

            UNCONDITIONAL, unlike the tile. It was conditioned on `systemScopeDetail != null` back
            when it bordered the group holding the tile — no tile, no group, no rule. It borders
            Settings now, and both of its sides survive the singleton's absence: Settings always
            renders, and the routes above it are whatever section entries the scope keeps, tile or
            no tile. A database with no singleton would otherwise glue the gear straight onto the
            last section entry and lose a distinction that was never the singleton's to carry. */}
          <div className="my-2 h-px w-7 bg-border/60" />

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

      {/* THE SYSTÈME APPROVAL SURFACE (PRD « Scope Système ») — the human's half of the one-shot
          protocol. Mounted HERE, on the always-mounted shell, for the same reason as the guard
          above: the request is raised by an agent that has no terminal and no screen, so the only
          place the answer can come from is the person at the machine — whatever project they
          happen to be looking at. Renders nothing until a request is pending. */}
      <SystemApprovalGate />
      {/* The `Mod+K` GLOBAL SEARCH palette, mounted ONCE here for the same reason the close
          guard is: it must exist on every route, and its chord is opened from wherever the
          keyboard happens to be — most often a focused terminal. It takes no DISPLAY data from
          this shell: each hit carries its own project and workspace NAMES, joined by the backend —
          the rail's tree only knows the projects it loaded, and knows no workspace names at all.
          It renders nothing until the chord fires.
          IT DOES TAKE ONE CAPABILITY. Its `Add a workspace to …` action must run the flow that
          ALREADY exists — `useManualAdd`'s native folder picker → name dialog → `create_workspace`,
          the very one the rail's context menu drives two hundred lines above (`onAddWorkspace`) —
          and that flow is mounted HERE, with its dialogs. So the shell hands it down rather than
          the palette writing a second creation path. The projectId→tree lookup is the shell's job
          too: `addWorkspace` needs the TREE (it seeds the default name from the project's root
          path), and the tree is what this component holds. */}
      <SearchPalette
        onAddWorkspace={(projectId) => {
          const tree = projects.find((p) => p.project.id === projectId);
          if (tree) void addWorkspace(tree);
        }}
      />

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
            importWorkspaceId={manageWorkspace?.id ?? null}
            workspacePath={manageWorkspace?.path ?? null}
            initialSection="global"
            // The Global pane, WIRED — which is also what puts it in the rail (a caller with
            // nothing to wire there, i.e. the Système singleton, gets no `Global` entry at all).
            // PRD-B: it exposes the project rename (kebab CRUD) alongside the resume toggle;
            // rename persists via `update_project`.
            globalSection={{
              resumeAgentSessions: liveTree?.project.resume_agent_sessions ?? false,
              showRename: true,
              onRename: (name) => (targetId ? updateProject(targetId, name) : Promise.resolve()),
              onResumeChange: (resume) =>
                targetId ? setProjectResumeAgentSessions(targetId, resume) : Promise.resolve(),
            }}
            onClose={() => setProjectSettingsTree(null)}
          />
        );
      })()}
    </div>
  );
}

export default BaseLayout;
