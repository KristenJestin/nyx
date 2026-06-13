import "@/globals.css";

import { TerminalManager } from "@/components/sidebar/terminal-manager";

/**
 * `App` — the nyx shell. The whole multi-terminal experience (frameless chrome,
 * sidebar navigation, N mounted terminals) lives in `<TerminalManager>`; `App`
 * just mounts it full-bleed.
 *
 * The per-terminal E2E read seam used to live here as `window.__nyx` for the
 * single-terminal socle; it now lives on `window.__nyxDeck[<record-id>]`, keyed
 * by record id, so the end-to-end suite can read ANY terminal's xterm buffer
 * (including hidden ones) — see `<TerminalDeck>`.
 */
function App() {
  return <TerminalManager />;
}

export default App;
