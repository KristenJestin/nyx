import "@/globals.css";

import { TerminalManager } from "@/components/sidebar/terminal-manager";
import { Toaster } from "@/components/ui/toast";

/**
 * `App` — the nyx shell. The whole multi-terminal experience (frameless chrome,
 * sidebar navigation, N mounted terminals) lives in `<TerminalManager>`; `App`
 * just mounts it full-bleed.
 *
 * `<Toaster>` is mounted ONCE here — the single app-wide toast host (bottom-right,
 * above every modal). Mutations across the app push onto its global manager via the
 * `toast.*` helper, so the system has one stack regardless of which surface fired it.
 *
 * The per-terminal E2E read seam used to live here as `window.__nyx` for the
 * single-terminal socle; it now lives on `window.__nyxDeck[<record-id>]`, keyed
 * by record id, so the end-to-end suite can read ANY terminal's xterm buffer
 * (including hidden ones) — see `<TerminalDeck>`.
 */
function App() {
  return (
    <>
      <TerminalManager />
      <Toaster />
    </>
  );
}

export default App;
