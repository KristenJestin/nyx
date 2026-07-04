import "@/globals.css";

import { RouterProvider } from "@tanstack/react-router";

import { router } from "@/router";
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
    <>
      <RouterProvider router={router} />
      <Toaster />
    </>
  );
}

export default App;
