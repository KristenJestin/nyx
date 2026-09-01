import type { ReactNode } from "react";
import { act, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { mockIPC } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

/**
 * The dnd-kit `onDragEnd` GLUE of the project RAIL in `<BaseLayout>` (task
 * `01KXBNX51EPQ69MVKYG4W960HD`, follow-up finding `01KXCCYT26ZFAJJ5M9XRV5HF4Z`). The reorder GATE
 * drives `reorderProjects` through the `__nyxWork.reorderProjects` seam, which BYPASSES the rail's
 * `onDragEnd` (`move(ids, event)` → `sameOrder` guard → `reorderProjects(next)`). The WebDriver
 * pointer sensor is undrivable in the Electron/CDP harness, but the `onDragEnd` computation is pure
 * JS: we mock dnd-kit's `<DragDropProvider>` to a pass-through that CAPTURES the rail's `onDragEnd`,
 * render the REAL `<BaseLayout>`, then fire a SYNTHETIC `dragend` event through it and assert the
 * moved order reaches the backend `projects_reorder` (what `reorderProjects` persists). `move` and
 * the `sameOrder` guard stay REAL — only the untestable pointer machinery is stubbed.
 */

/** The minimal shape of the dnd-kit `dragend` event the handler + `move` read. */
interface FakeDragEndEvent {
  canceled: boolean;
  operation: {
    canceled: boolean;
    source: { id: string } | null;
    target: { id: string } | null;
  };
}

// Captured lazily when the mocked provider renders; `vi.hoisted` so the holder exists by the time
// the mock factory (hoisted above imports) runs.
const dnd = vi.hoisted(() => ({ onDragEnd: null as null | ((event: unknown) => void) }));

vi.mock("@dnd-kit/react", () => ({
  DragDropProvider: ({
    children,
    onDragEnd,
  }: {
    children: ReactNode;
    onDragEnd: (event: unknown) => void;
  }) => {
    dnd.onDragEnd = onDragEnd;
    return <>{children}</>;
  },
}));

// The rail tiles read only `{ ref, isDragging }`; a no-op stands in for the real (context- and
// DOM-bound) sortable so the rail renders under the stubbed provider. It also RECORDS every id
// registered as sortable — the observable for "what the drag can actually grab" (see the pinned
// `All projects` test below).
const sortable = vi.hoisted(() => ({ registered: [] as string[] }));

vi.mock("@dnd-kit/react/sortable", () => ({
  useSortable: ({ id }: { id: string }) => {
    if (!sortable.registered.includes(id)) sortable.registered.push(id);
    return { ref: () => {}, isDragging: false };
  },
}));

import { ActiveProjectProvider } from "./active-project";
import { BaseLayout } from "./base-layout";

/** A `dragend` that drags `sourceId` onto `targetId` (both live), the drop `move` reorders on. */
function drop(sourceId: string, targetId: string): FakeDragEndEvent {
  return {
    canceled: false,
    operation: { canceled: false, source: { id: sourceId }, target: { id: targetId } },
  };
}

function project(id: string, name: string): ProjectRecord {
  return { id, name, collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
}

function root(id: string, projectId: string): WorkspaceRecord {
  return {
    id,
    project_id: projectId,
    name: "root",
    path: `/${id}`,
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

interface Backend {
  /** Every `projects_reorder` call's id sequence, in the order received. */
  reorders: string[][];
}

/** Serve the rail's read model and CAPTURE each `projects_reorder` the reorder glue persists. */
function installBackend(projects: ProjectRecord[]): Backend {
  const backend: Backend = { reorders: [] };
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_projects":
        return projects;
      case "list_workspaces":
        return [root(`ws-${a.projectId as string}`, a.projectId as string)];
      case "workspace_runtime_snapshot":
        return {
          workspace: { id: a.workspaceId as string, name: "root", path: "/x" },
          terminals: [],
          agent_sessions: [],
          services: [],
        };
      case "agent_active_sessions":
      case "agent_activity_snapshot":
        return [];
      case "projects_reorder": {
        backend.reorders.push(a.ids as string[]);
        return undefined;
      }
      default:
        return undefined;
    }
  });
  return backend;
}

/** Mount `<BaseLayout>` inside a real memory router with stub routed pages (matches base-layout.test). */
function renderLayout() {
  const rootRoute = createRootRoute({
    component: () => (
      <ActiveProjectProvider>
        <BaseLayout />
      </ActiveProjectProvider>
    ),
  });
  const mk = (path: string, text: string) =>
    createRoute({ getParentRoute: () => rootRoute, path, component: () => <div>{text}</div> });
  const routeTree = rootRoute.addChildren([
    mk("/", "TERMINALS PAGE"),
    mk("/work", "WORK PAGE"),
    mk("/knowledge", "KNOWLEDGE PAGE"),
  ]);
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return render(<RouterProvider router={router} />);
}

describe("BaseLayout — project rail onDragEnd glue (dnd-kit)", () => {
  beforeEach(() => {
    dnd.onDragEnd = null;
    sortable.registered = [];
  });

  it("a drag that CHANGES the order persists projects_reorder with the reordered ids", async () => {
    const backend = installBackend([
      project("p1", "Alpha"),
      project("p2", "Beta"),
      project("p3", "Gamma"),
    ]);
    renderLayout();
    // Wait for the rail to load its projects (so the captured handler closes over [p1,p2,p3]).
    await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(dnd.onDragEnd).not.toBeNull());

    // Drag Alpha (p1) onto Beta (p2): move([p1,p2,p3], p1→p2) = [p2,p1,p3].
    await act(async () => {
      dnd.onDragEnd!(drop("p1", "p2"));
    });

    await waitFor(() => expect(backend.reorders.at(-1)).toEqual(["p2", "p1", "p3"]));
  });

  it("a NO-OP drop (source === target ⇒ sameOrder) never persists a reorder (guard)", async () => {
    const backend = installBackend([project("p1", "Alpha"), project("p2", "Beta")]);
    renderLayout();
    await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(dnd.onDragEnd).not.toBeNull());

    // Dropping Alpha onto itself yields the SAME order → the `sameOrder` guard skips the commit.
    await act(async () => {
      dnd.onDragEnd!(drop("p1", "p1"));
    });

    // Give any (unexpected) async persist a chance to fire, then assert none did.
    await new Promise((r) => setTimeout(r, 0));
    expect(backend.reorders).toEqual([]);
  });

  /**
   * THE PINNED `All projects` BAR IS NOT DRAGGABLE, and it is not draggable BY CONSTRUCTION —
   * review `01M1EJK7VF9WG86V85AJ5K8JFC`. It used to be the FIRST `<li>` of this very list; it now
   * lives in its own region above, OUTSIDE the `<DragDropProvider>`. So it registers no
   * `useSortable`, holds no index, and the id array the drop reorders (`projects.map(…)`) is the
   * PROJECT array — the bar has no id in it to be moved to, or moved away from.
   *
   * The registry the sortable mock keeps is the observable: what a drag can grab is exactly what
   * called `useSortable`. The "+ add project" tile has never been in it either.
   */
  it("registers ONLY the projects as sortable — the pinned `All projects` bar is not draggable", async () => {
    installBackend([project("p1", "Alpha"), project("p2", "Beta"), project("p3", "Gamma")]);
    renderLayout();
    await screen.findByRole("button", { name: "Project Alpha" });
    // The bar IS rendered — it is simply not part of the sortable set.
    expect(screen.getByRole("link", { name: "All projects" })).toBeTruthy();
    await waitFor(() => expect(sortable.registered).toEqual(["p1", "p2", "p3"]));
  });

  /**
   * …and the drop still commits ONLY project ids, in the project count. A stray entry inside the
   * list would surface here as an extra id in what `projects_reorder` persists.
   */
  it("persists a reorder made of project ids ONLY, nothing else of the rail", async () => {
    const backend = installBackend([
      project("p1", "Alpha"),
      project("p2", "Beta"),
      project("p3", "Gamma"),
    ]);
    renderLayout();
    await screen.findByRole("button", { name: "Project Alpha" });
    await waitFor(() => expect(dnd.onDragEnd).not.toBeNull());

    await act(async () => {
      dnd.onDragEnd!(drop("p3", "p1"));
    });

    await waitFor(() => expect(backend.reorders).toHaveLength(1));
    expect([...backend.reorders[0]!].sort()).toEqual(["p1", "p2", "p3"]);
  });
});
