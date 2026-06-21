/**
 * The bridge entrypoint every component imports. It exposes the single active
 * {@link NyxBridge} for the running shell, selected AT RUNTIME by sniffing the
 * environment — this is the ONE place the shell is chosen, so none of the ~100
 * `{ nyxBridge }` call-sites change when the shell does.
 *
 * Selection (phase 3, task #11): the Electron preload installs a deep-frozen
 * `window.nyxCore` (+ `window.nyxWindow`) allowlist; the Tauri shell injects
 * `window.__TAURI_INTERNALS__`. We pick the Electron adapter when `window.nyxCore`
 * is present, the Tauri adapter otherwise. The Tauri adapter stays the fallback so a
 * Tauri run (and the unit tests, which drive the Tauri mock IPC) are unaffected —
 * the migration swaps the IMPLEMENTATION here, never the call-sites.
 *
 * `@tauri-apps/*` stays confined to `./tauri.ts`; `./electron.ts` imports none. Both
 * adapter modules are statically importable (so either builds), but only the
 * selected one is wired to `nyxBridge`.
 */
export * from "./contract";
export { tauriBridge } from "./tauri";
export { electronBridge } from "./electron";

import type { NyxBridge } from "./contract";
import { electronBridge } from "./electron";
import { tauriBridge } from "./tauri";

/**
 * Detect the host shell. Electron is identified by the allowlisted `window.nyxCore`
 * bridge the preload installs (a stable, app-owned global — not a Chromium/Node
 * sniff, which would be brittle). Anything else is the Tauri shell (its
 * `window.__TAURI_INTERNALS__` is implied by the fallback). Guarded for SSR/tests
 * where `window` may be undefined.
 */
function isElectronShell(): boolean {
  return typeof window !== "undefined" && typeof (window as { nyxCore?: unknown }).nyxCore !== "undefined";
}

/**
 * The active bridge for this shell, chosen once at module load. The selection is the
 * seam phase 3 swaps: Electron when its preload bridge is present, Tauri otherwise.
 * No component sees the difference — they all depend on the {@link NyxBridge}
 * contract via `nyxBridge`.
 */
export const nyxBridge: NyxBridge = isElectronShell() ? electronBridge : tauriBridge;

export default nyxBridge;
