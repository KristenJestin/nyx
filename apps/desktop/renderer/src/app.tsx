import "@/globals.css";

import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";

import { queryClient } from "@/lib/query";
import { router } from "@/router";
import { ReviewLiveSync } from "@/components/review/review-live-sync";
import { Toaster } from "@/components/ui/toast";

/**
 * `App` — the nyx shell. It mounts the navigation SHELL through the router
 * (`<RouterProvider>`): the left rail of PROJECTS + the section-nav (Terminals / Work /
 * Knowledge) as the root layout, and the routed page in the `<Outlet/>`. See the ADR
 * `docs/product/decisions/PDR-0025-routeur-renderer-navigation-shell.md`.
 *
 * The v2.2 workspace COCKPIT top-level view was removed: the workspace is the RUNTIME
 * pivot in the database (PDR-0008), NOT a screen — one enters a PROJECT, never a
 * workspace. The runtime backend it consumed is preserved and re-consumed by the
 * Terminals page under the shell.
 *
 * `<QueryClientProvider>` wraps the whole app so the review data layer (and any future
 * migrated surface) shares ONE TanStack Query cache (PDR-0029). The SAME `queryClient` is
 * threaded into the router's context (`createRouter({ context: { queryClient } })`) so a
 * route loader can `ensureQueryData`. `<ReviewLiveSync>` installs the single global
 * `repos://changed` → `invalidateQueries` bridge (replacing the old per-hook subscriptions).
 *
 * `<Toaster>` is mounted ONCE here — the single app-wide toast host (bottom-right,
 * above every modal). Mutations across the app push onto its global manager via the
 * `toast.*` helper, so the system has one stack regardless of which surface fired it.
 *
 * The per-terminal E2E read seam lives on `window.__nyxDeck[<record-id>]`, keyed by
 * record id, so the end-to-end suite can read ANY terminal's xterm buffer (including
 * hidden ones) — see `<TerminalDeck>`.
 */
function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <ReviewLiveSync />
      <RouterProvider router={router} />
      <Toaster />
    </QueryClientProvider>
  );
}

export default App;
