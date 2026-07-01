/**
 * The bridge entrypoint every component imports. It exposes the single active
 * {@link NyxBridge} for the running shell — there is ONE shell now (Electron + the
 * supervised Rust sidecar), so this is the one place the implementation is chosen and
 * none of the ~100 `{ nyxBridge }` call-sites change.
 *
 * Selection: the Electron preload installs a deep-frozen `window.nyxCore` (+
 * `window.nyxWindow`) allowlist; when it is present we use the {@link electronBridge}.
 * When it is absent (jsdom unit tests that did NOT install the `@/bridge` mock, SSR),
 * we fall back to an in-memory {@link FakeNyxBridge} so a call never throws on a missing
 * shell. Component/hook tests normally mock `@/bridge` via `./test-harness`, which
 * replaces `nyxBridge` with a live fake outright, so this fallback is only the bare
 * safety net.
 *
 * Tauri / N-API / the Node core-host were the transitional migration path (PDR-0006 /
 * PDR-0007); they are removed — only the Electron adapter ships, and `@tauri-apps/*` is
 * no longer a dependency.
 */
export * from "./contract";
export { electronBridge } from "./electron";

import type { NyxBridge } from "./contract";
import { electronBridge } from "./electron";
import { FakeNyxBridge } from "./fake";

/**
 * Detect the Electron shell by the allowlisted `window.nyxCore` bridge the preload
 * installs (a stable, app-owned global — not a Chromium/Node sniff, which would be
 * brittle). Guarded for SSR/tests where `window` may be undefined.
 */
function isElectronShell(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof (window as { nyxCore?: unknown }).nyxCore !== "undefined"
  );
}

/**
 * The active bridge for this shell, chosen once at module load: the Electron adapter
 * when its preload bridge is present, else the in-memory fake (the bare test/SSR safety
 * net). No component sees the difference — they all depend on the {@link NyxBridge}
 * contract via `nyxBridge`.
 */
export const nyxBridge: NyxBridge = isElectronShell() ? electronBridge : new FakeNyxBridge();

export default nyxBridge;
