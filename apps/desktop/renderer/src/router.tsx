import type { QueryClient } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRouteWithContext,
  createRoute,
  createRouter,
  Outlet,
} from "@tanstack/react-router";

import { errorText } from "@/bridge";
import { queryClient } from "@/lib/query";
import { ActiveProjectProvider } from "@/components/terminals/active-project";
import { BaseLayout } from "@/components/terminals/base-layout";
import { TerminalsPage } from "@/components/terminals/terminals-page";
import { HomePage } from "@/components/home/home-page";
import { BoardPage } from "@/components/specs/board-page";
import { SpecDetailPage } from "@/components/specs/spec-detail-page";
import { PrototypePage } from "@/components/specs/prototype-page";
import { ReviewPage } from "@/components/review/review-page";
import { KnowledgePage } from "@/components/knowledge/knowledge-page";
import { EnginePage } from "@/components/engine/engine-page";
import { CataloguePage } from "@/components/design/catalogue-page";
import { ComponentDetailPage } from "@/components/design/component-detail-page";
import { Spinner } from "@/components/ui/spinner";

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
 * The router CONTEXT — carries the app's TanStack Query client so a route loader can
 * `context.queryClient.ensureQueryData(...)` to PRECHARGE a page's data (the official TanStack
 * Query ↔ Router integration, PDR-0029). Threaded in at `createRouter({ context: { queryClient } })`
 * below; the same client backs `<QueryClientProvider>` at the app root, so a loader-primed cache is
 * the one the page's `useQuery` reads.
 */
export interface RouterContext {
  readonly queryClient: QueryClient;
}

/**
 * The ROOT route: the navigation shell. The `<BaseLayout>` (rail + section-nav + its own
 * `<Outlet/>`) is wrapped in the `<ActiveProjectProvider>` so the entered-project
 * selection is shared between the rail and every routed page. `createRootRouteWithContext`
 * types the `queryClient` context every child route (and its loader) receives.
 */
const rootRoute = createRootRouteWithContext<RouterContext>()({
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

/**
 * The `/home` search params — WHICH SCOPE the one home page is read in:
 *  - absent / `scope=project`: the ENTERED project's recap;
 *  - `scope=all`: the same page across every project (the cross-project home).
 *
 * A search param rather than a second route because the two homes are ONE page over one read
 * model in two scopes (`nyx_core::home::HomeScope`) — a second route would invite a second
 * component, which is the drift the elected design spent five rounds removing.
 */
interface HomeSearch {
  readonly scope?: "all";
}

/**
 * Home (`/home`) — the recap of the entered project, and (with `?scope=all`) of every project:
 * the agent sessions in one attention-sorted flow, the running services, the Specs that moved and
 * the bare shells. Reached from the rail — the Home entry at the head of the section-nav for the
 * project scope, the "All projects" bar in its own region at the top of the rail for the global
 * one — and
 * from nowhere else.
 */
const homeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/home",
  component: HomeRouteComponent,
  validateSearch: (search: Record<string, unknown>): HomeSearch =>
    search.scope === "all" ? { scope: "all" } : {},
});

/** Hand the validated `?scope=` down as the page's one prop. */
function HomeRouteComponent() {
  const { scope } = homeRoute.useSearch();
  return <HomePage scope={scope === "all" ? "all" : "project"} />;
}

/** Work (`/work`) — the Specs BOARD (kanban / list of the entered project's Specs, v2.4). */
const workRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/work",
  component: BoardPage,
});

/**
 * Spec detail (`/work/$specId`) — the PRD detail with its tabs (spec / tasks / review / annexes /
 * activity / runtime). The Review tab shows the Spec-scoped comments + an "Open review" button onto
 * the full-screen review page (`/work/$specId/review`, PDR-0028). Reached from a board card / row.
 */
const specDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/work/$specId",
  component: SpecDetailPage,
});

/**
 * Review (`/work/$specId/review`) — the FULL-SCREEN review page of a Spec (PDR-0028). Spec-scoped,
 * NOT a top-level route: it is reached ONLY from the Spec detail's Review tab "Open review" button
 * (no nav item, no `/review` top-level route — the "no provisional exposure" invariant). It re-
 * derives the Spec's workspace context from `spec_detail` to scope the repos + comments, and links
 * back to `/work/$specId`.
 */
const specReviewRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/work/$specId/review",
  component: ReviewPage,
});

/**
 * The `/work/$specId/prototype` search params — an OPTIONAL deep-link the Spec's Prototype tab uses
 * to open the workbench ON a given page (its contact sheet points each card at its own page):
 *  - `page`: the prototype page's SLUG. Omitted / unknown → the workbench opens on the first page.
 */
interface PrototypeSearch {
  readonly page?: string;
}

/**
 * Prototype (`/work/$specId/prototype`) — the FULL-PAGE prototype workbench of a Spec (finding
 * `01KYVMSMMBVSZNBBSBE18PRZAK`, the elected v8 design). Spec-scoped and reached ONLY from the Spec
 * detail's Prototype tab (its "Open prototype" button + its contact sheet), in STRICT PARITY with
 * the review page: no nav item, no top-level `/prototype` route — the same "no provisional exposure"
 * invariant.
 *
 * The route exists because a prototype stands for a REAL WEB PAGE and a tab is not the size of one:
 * inside the tab, the stage measured 486 × 489.
 */
const specPrototypeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/work/$specId/prototype",
  component: PrototypePage,
  validateSearch: (search: Record<string, unknown>): PrototypeSearch =>
    typeof search.page === "string" ? { page: search.page } : {},
});

/** Knowledge (`/knowledge`) — routed stub for now (real knowledge base ported later). */
const knowledgeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/knowledge",
  component: KnowledgePage,
});

/**
 * Engine (`/engine`) — the READ-ONLY React Flow view of the orchestration behavior tree (v2.8 Phase
 * 6): the static graph (custom nodes / group nodes / custom edges / ELK compound layout), the runtime overlay
 * (current node + blackboard) and the node inspector (spawn → rendered effector context). Authoring
 * stays MCP-first; this route never mutates the graph.
 */
const engineRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/engine",
  component: EnginePage,
});

/**
 * Design-system CATALOGUE (`/design`, v2.7 Phase 3, task #10) — browse the project's
 * `design_components` vocabulary (DS + NEW) with a rendered preview per component and DS/NEW/
 * liés-page filters. Portée PROJET. The composant detail (task #11) opens from a card.
 */
const catalogueRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/design",
  component: CataloguePage,
});

/**
 * Composant DETAIL (`/design/$componentId`, task #11) — ONE component: its representative preview,
 * its axes and its params. Reached from a catalogue card, and from nowhere else.
 *
 * IT TAKES NO SEARCH PARAMS ANY MORE (finding `01KYT10VE8624MKQ18AFHSE0W0`). It used to accept a
 * `?variant=&node=` USAGE deep-link (finding `01KYSNWNDG6HBFJX017RE203SZ`) because the per-node
 * editing lived here; that editing now lives in the Spec's PROTOTYPE tab, beside the composition it
 * writes to, and the elected composition selects the node THERE instead of navigating here. A route
 * still advertising a contract nothing honours is the same silent lie the deep-link was written to
 * kill, so the contract leaves with the capability.
 */
const componentDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/design/$componentId",
  component: ComponentDetailPage,
});

const routeTree = rootRoute.addChildren([
  terminalsRoute,
  homeRoute,
  workRoute,
  specDetailRoute,
  specReviewRoute,
  specPrototypeRoute,
  knowledgeRoute,
  engineRoute,
  catalogueRoute,
  componentDetailRoute,
]);

/**
 * The router-wide fallback for an UNCAUGHT throw inside a routed page. Setting it is what makes
 * TanStack wrap each route in an error boundary at all (without a `defaultErrorComponent`/per-route
 * `errorComponent`, `<Match>` uses a passthrough fragment and a page crash unmounts the whole app).
 * It renders in the route's slot — i.e. inside `<BaseLayout>`'s `<Outlet/>` — so the navigation SHELL
 * (rail + section-nav) SURVIVES a page crash and the user can navigate away, instead of a blank window.
 */
function RouteErrorFallback({ error }: { error: Error }) {
  // TanStack types it `Error`, but what a page throws is often a BRIDGE ERROR OBJECT — read through
  // the one path that knows the difference, or the screen quotes `[object Object]` at the user.
  const message = errorText(error, "");
  return (
    <div className="flex h-app flex-col items-center justify-center gap-3 bg-background p-6 text-center">
      <p className="text-sm text-muted-foreground">Something went wrong on this screen.</p>
      {message ? (
        <p className="max-w-md font-mono text-2xs text-muted-foreground/70">{message}</p>
      ) : null}
    </div>
  );
}

/**
 * The router-wide fallback for a route whose data is not there YET — the missing counterpart of
 * `RouteErrorFallback` above. The pair was asymmetric: a page that FAILED had a screen, a page that
 * was merely LOADING had none, so the interval between "React mounted" and "first routed screen"
 * painted nothing at all — the same silence the static boot splash in `index.html` was added to
 * break, one layer higher up.
 *
 * Setting it is also what CREATES the suspense boundary, exactly as `defaultErrorComponent` creates
 * the error one: `<Match>` only wraps a route in `React.Suspense` when a pending component exists
 * (`wrapInSuspense ?? PendingComponent ?? …`), and `<Matches>` uses it as the fallback of the
 * top-level boundary that covers the router's INITIAL load. Without it, a suspended match has
 * nothing to show and the slot stays empty.
 *
 * It carries `bg-background` — the token whose dark value the boot splash duplicates as a literal —
 * so the hand-over from the static markup to this screen changes nothing the eye can catch, and
 * `<Spinner>` (the design system's lucide `loader-circle` under `animate-spin`), which is the glyph
 * the splash inlines. Same box as `RouteErrorFallback`: it renders in the route's slot, inside
 * `<BaseLayout>`'s `<Outlet/>`, so the navigation shell stays up while a page loads.
 *
 * TIMING IS THE LIBRARY'S, UNTOUCHED — and it is worth knowing what it does, because it is not
 * "immediately". `defaultPendingMs` (1 s) is a GRACE: the router holds the previous screen — and,
 * on the very first load, an empty slot — for that long before showing this component, so a loader
 * that answers in 40 ms never makes it flash. `defaultPendingMinMs` (500 ms) is the other half: once
 * shown, it stays that long even if the data has landed. Neither is set here; both are what TanStack
 * fills in, and `router-pending.test.tsx` fails if the app ever starts overriding them — adding
 * waiting time of our own is the one thing this correction must not do.
 *
 * SO THIS SCREEN IS NOT WHAT COVERS THE START-UP. Today no app route declares a loader, so the
 * first routed screen commits as soon as the matches resolve; what covers the interval before that
 * is the static splash in `index.html`. This component covers the LOADS THAT COME AFTER — and the
 * day a route takes PDR-0029 up on `ensureQueryData`, the second and following seconds of its wait.
 */
function RoutePendingFallback() {
  return (
    <div className="flex h-app items-center justify-center bg-background">
      <Spinner className="size-6 text-muted-foreground" />
    </div>
  );
}

/**
 * Build a router over the route tree. Exposed as a factory so tests can spin up an
 * isolated router (with their own memory history / initial entry) without the app's
 * module-level singleton. The default entry is the Terminals index (`/`).
 */
export function createAppRouter(initialEntries: string[] = ["/"]) {
  return createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries }),
    defaultErrorComponent: RouteErrorFallback,
    defaultPendingComponent: RoutePendingFallback,
    // The review data layer's TanStack Query client, made available to every route's loader
    // (PDR-0029). Tests pass the module singleton implicitly via this factory.
    context: { queryClient },
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
