import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
} from "@tanstack/react-router";

import { ActiveProjectProvider } from "@/components/terminals/active-project";
import { BaseLayout } from "@/components/terminals/base-layout";
import { TerminalsPage } from "@/components/terminals/terminals-page";
import { BoardPage } from "@/components/specs/board-page";
import { SpecDetailPage } from "@/components/specs/spec-detail-page";
import { KnowledgePage } from "@/components/knowledge/knowledge-page";

/**
 * The renderer's ROUTER — introduced in this pass to carry the multi-page navigation
 * shell (rail + section-nav + `<Outlet/>`). See the ADR: `docs/product/decisions/
 * PDR-0025-routeur-renderer-navigation-shell.md`.
 *
 * ## Why a router, and why THIS one (PDR-0025)
 * The renderer had no router; the removed cockpit used a hand-rolled hash seam. The
 * validated navigation model (the prototype) is multi-page — Terminals (`/`), Work
 * (`/work`), Knowledge (`/knowledge`) under one shell — which needs real routes, a
 * shared root layout, and an `<Outlet/>`. We adopt `@tanstack/react-router` (same family
 * as the app's TanStack Form / Hotkeys and the prototype of reference).
 *
 * ## Code-based routes + memory history
 * - CODE-BASED routing (this module), not the prototype's file-based `routeTree.gen.ts`
 *   codegen: the route tree is tiny, so a hand-written tree stays readable and testable
 *   without the route-generation Vite plugin / generated artifact.
 * - `createMemoryHistory`: the app is loaded over `file://` under Electron, where mutating
 *   the URL path is fragile; a memory history keeps navigation entirely in RAM
 *   (deterministic, scriptable in tests).
 */

/**
 * The ROOT route: the navigation shell. The `<BaseLayout>` (rail + section-nav + its own
 * `<Outlet/>`) is wrapped in the `<ActiveProjectProvider>` so the entered-project
 * selection is shared between the rail and every routed page.
 */
const rootRoute = createRootRoute({
  component: () => (
    <ActiveProjectProvider>
      <BaseLayout />
    </ActiveProjectProvider>
  ),
});

/**
 * The `/` search params — OPTIONAL deep-links the Spec detail's Runtime tab navigates to when a
 * runtime row is clicked (the only way to open a PRECISE runtime element by id):
 *  - `terminal`: open (select) a specific terminal on entry.
 *  - `command`: open (select) a specific managed command — its live output pane — on entry.
 * Omitted → the page keeps its default "auto-select the first terminal" behaviour.
 */
interface TerminalsSearch {
  readonly terminal?: string;
  readonly command?: string;
}

/** Terminals — the index route (`/`): the entered project's workspaces + runtime. */
const terminalsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: TerminalsPage,
  // Validate (and narrow) the `?terminal=<id>` / `?command=<id>` deep-link params. Non-strings
  // are dropped.
  validateSearch: (search: Record<string, unknown>): TerminalsSearch => {
    const out: { terminal?: string; command?: string } = {};
    if (typeof search.terminal === "string") out.terminal = search.terminal;
    if (typeof search.command === "string") out.command = search.command;
    return out;
  },
});

/** Work (`/work`) — the Specs BOARD (kanban / list of the entered project's Specs, v2.4). */
const workRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/work",
  component: BoardPage,
});

/**
 * Spec detail (`/work/$specId`) — the PRD detail with its tabs (spec / tasks / review-
 * placeholder / annexes / activity / runtime, v2.4). Reached from a board card / row.
 */
const specDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/work/$specId",
  component: SpecDetailPage,
});

/** Knowledge (`/knowledge`) — routed stub for now (real knowledge base ported later). */
const knowledgeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/knowledge",
  component: KnowledgePage,
});

const routeTree = rootRoute.addChildren([
  terminalsRoute,
  workRoute,
  specDetailRoute,
  knowledgeRoute,
]);

/**
 * Build a router over the route tree. Exposed as a factory so tests can spin up an
 * isolated router (with their own memory history / initial entry) without the app's
 * module-level singleton. The default entry is the Terminals index (`/`).
 */
export function createAppRouter(initialEntries: string[] = ["/"]) {
  return createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries }),
  });
}

/** The app's single router instance, mounted by `app.tsx` via `<RouterProvider>`. */
export const router = createAppRouter();

// Re-export `Outlet` so nested layouts can compose without importing the dep directly.
export { Outlet };

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof createAppRouter>;
  }
}
