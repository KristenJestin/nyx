/**
 * EXPLICIT, in-code v2 app identity (review `01KWD3R7CYK5JNYKHCZZSW0284`, findings #1/#2).
 *
 * ## Why this exists (the safety story)
 *
 * The user's v1 nyx is RUNNING LIVE. Its Electron userData is `~/.config/@nyx/electron`
 * (derived from the old npm `name` `@nyx/electron`) and its live data sits there + under
 * `~/.local/share/com.netsirk.nyx`. If v2 ever resolved its data dir to `~/.config/@nyx/electron`
 * it would OPEN and MIGRATE the live v1 DB — clobbering it. `NYX_DATA_DIR` is only a dev/test
 * override and MUST NOT be the load-bearing safety mechanism (a forgotten unset = disaster).
 *
 * So v2 pins a DETERMINISTIC identity in code, as EARLY as possible in main (before any
 * `app.getPath('userData')`, before `whenReady`):
 *   - `app.setName('nyx-v2')` — so any name-derived path is `nyx-v2`, never `@nyx/electron`.
 *   - `app.setAppUserModelId('com.netsirk.nyxv2')` — distinct Windows AUMID / taskbar identity.
 *   - `app.setPath('userData', <appData>/nyx-v2)` — userData is EXPLICITLY `~/.config/nyx-v2`
 *     (Linux) / the per-OS appData `nyx-v2` subdir, never the v1 dir. This is what
 *     `resolveDataDir()` (sidecar.ts) falls back to when `NYX_DATA_DIR` is unset — so booting
 *     v2 without the override is SAFE BY CONSTRUCTION.
 *
 * The pure {@link deriveUserDataDir} is unit-tested (it never touches the real Electron `app`
 * nor the real userData), asserting the resolved path is `nyx-v2`-namespaced and DISTINCT from
 * the v1 `@nyx/electron` dir.
 */

/** The v2 app identity literals — distinct from v1 (`@nyx/electron` / `com.netsirk.nyx` / `nyx`). */
export const APP_NAME = "nyx-v2";
export const APP_USER_MODEL_ID = "com.netsirk.nyxv2";

/** The v1 userData dir name we must NEVER resolve to (defense-in-depth assertion target). */
export const V1_USERDATA_DIR_NAME = "@nyx/electron";

/**
 * The minimal slice of Electron's `app` we drive — so the identity application is testable
 * with a fake (no real Electron, no real userData). `getPath('appData')` is the per-OS
 * roaming config root (`~/.config` on Linux, `%APPDATA%` on Windows, `~/Library/Application
 * Support` on macOS); we join `nyx-v2` onto it deterministically.
 */
export interface IdentityApp {
  setName(name: string): void;
  setAppUserModelId(id: string): void;
  getPath(name: "appData"): string;
  setPath(name: "userData", value: string): void;
}

/**
 * PURE derivation of v2's userData dir from the OS `appData` root: `<appData>/nyx-v2`.
 * `sep` lets tests pin the separator; defaults to the host's. Never returns the v1
 * `@nyx/electron` dir — that is exactly what we are designing this away from.
 */
export function deriveUserDataDir(appData: string, sep: string): string {
  return [appData, APP_NAME].join(sep);
}

/**
 * Apply the explicit v2 identity to the Electron `app`, AS EARLY AS POSSIBLE (call this at
 * the very top of main, before any `getPath('userData')` and before `whenReady`). After this:
 *   - `app.getName()` is `nyx-v2`,
 *   - the Windows AUMID is `com.netsirk.nyxv2`,
 *   - `app.getPath('userData')` is deterministically `<appData>/nyx-v2` (never `@nyx/electron`).
 *
 * Returns the pinned userData dir (the same value the sidecar data-dir resolver falls back to
 * when `NYX_DATA_DIR` is unset), so the caller can log it.
 */
export function applyAppIdentity(app: IdentityApp, sep: string = require("node:path").sep): string {
  // Order matters: set the name FIRST (some path derivations key off it), then the AUMID, then
  // pin userData explicitly so it can never fall through to a name-derived `@nyx/electron`.
  app.setName(APP_NAME);
  app.setAppUserModelId(APP_USER_MODEL_ID);
  const userData = deriveUserDataDir(app.getPath("appData"), sep);
  app.setPath("userData", userData);
  return userData;
}
