import type { TerminalCore } from "@wterm/core";
import { GhosttyCore } from "@wterm/ghostty";

/**
 * The scrollback ceiling handed to the ghostty core. Deliberately BOUNDED: nyx
 * treats scrollback as a SIDECAR concern (the front's core is a live-screen +
 * short backlog projection, not the archive — see the PRD's "nyx neutralise les
 * faiblesses de wterm"), so the front core only needs enough to feel continuous.
 */
const SCROLLBACK_LIMIT = 1000;

/**
 * A factory that produces a fresh `TerminalCore`. The default loads the real
 * ghostty WASM core; tests override it (see {@link setCoreFactory}) with a
 * lightweight fake since the WASM cannot be fetched/instantiated under jsdom.
 */
export type CoreFactory = () => Promise<TerminalCore>;

// The ghostty-vt WASM (~464 KB, full VT emulation) ships inside `@wterm/ghostty`.
// The package resolves it itself via `new URL("../wasm/ghostty-vt.wasm",
// import.meta.url)` (see its `wasm-bindings.js`). Vite understands that
// `new URL(..., import.meta.url)` idiom INSIDE a dependency: it turns the binary
// into a FIRST-CLASS BUILD ASSET (copied into the bundle, hashed, `base`-relative
// — our `base: "./"` keeps it loadable over Electron's `file://`). We deliberately
// do NOT deep-import `@wterm/ghostty/wasm/...` (its `exports` map doesn't expose
// that subpath), and let the package's own default do the resolution — the
// supported integration path. So `GhosttyCore.load()` needs no explicit `wasmPath`.
let coreFactory: CoreFactory = () => GhosttyCore.load({ scrollbackLimit: SCROLLBACK_LIMIT });

/**
 * Load a fresh terminal core, ready to hand to `new WTerm(el, { core })`.
 *
 * By default we use the ghostty core (`@wterm/ghostty`) — NOT wterm's bundled
 * 12 KB Zig mini-core — for VT CORRECTNESS (full Unicode grapheme handling, all
 * SGR attributes, alt-screen, truecolor). Each terminal instance gets its OWN
 * core (its own WASM instance + screen state); the module-level store keeps them
 * alive across navigation so the state survives detach/reattach.
 */
export function loadGhosttyCore(): Promise<TerminalCore> {
  return coreFactory();
}

/**
 * TEST SEAM: override the core factory (e.g. with a fake `TerminalCore`) since
 * the ghostty WASM cannot load under jsdom. Returns the previous factory so a
 * test can restore it. Not used in production.
 */
export function setCoreFactory(factory: CoreFactory): CoreFactory {
  const prev = coreFactory;
  coreFactory = factory;
  return prev;
}
