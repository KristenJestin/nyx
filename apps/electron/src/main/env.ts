/**
 * Dev/prod mode detection and env-driven knobs for the Electron shell.
 *
 * The shell must run in two modes from the SAME main code:
 *   - **dev** — load the renderer from the running Vite dev server
 *     (`http://localhost:1420`), open DevTools, no ASAR.
 *   - **prod** — load the renderer from the packaged static build
 *     (`apps/frontend/dist/index.html`) shipped inside the app.
 *
 * Detection prefers the explicit `NYX_DEV_SERVER_URL` (set by the `dev` script),
 * falling back to Electron's `app.isPackaged` so a packaged binary is always prod
 * even if launched oddly.
 */

/**
 * The Vite dev-server URL, when running in dev. Unset in a packaged build.
 * The `dev` npm script sets this so the main process knows to load the live server
 * instead of the on-disk build. Mirrors the Tauri `devUrl` (`http://localhost:1420`).
 */
export function devServerUrl(): string | undefined {
  const url = process.env.NYX_DEV_SERVER_URL;
  return url && url.length > 0 ? url : undefined;
}

/**
 * Whether the custom (frameless) window controls should render, from the OS env
 * `NYX_WINDOW_CONTROLS`. Contract (identical to the Tauri side's
 * `controls_visible_from_env`): controls are VISIBLE by default; ONLY the exact
 * string `"0"` hides them. Any other value (including unset/empty) keeps them
 * visible — a permissive default so the window is never left uncloseable.
 */
export function windowControlsVisible(): boolean {
  return process.env.NYX_WINDOW_CONTROLS !== "0";
}
