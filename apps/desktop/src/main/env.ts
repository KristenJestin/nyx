/**
 * Dev/prod mode detection and env-driven knobs for the Electron shell.
 *
 * The shell must run in two modes from the SAME main code:
 *   - **dev** — load the renderer from the running Vite dev server (on a localhost port DERIVED
 *     per worktree by `scripts/dev-server-identity.cjs`, no longer a fixed 1420), open DevTools,
 *     no ASAR.
 *   - **prod** — load the renderer from the packaged static build
 *     (`dist/renderer/index.html`, relative to this package) shipped inside the app.
 *
 * Detection prefers the explicit `NYX_DEV_SERVER_URL` (set by the `dev` script),
 * falling back to Electron's `app.isPackaged` so a packaged binary is always prod
 * even if launched oddly.
 */

/**
 * The Vite dev-server URL, when running in dev. Unset in a packaged build.
 * The `dev` npm script sets this so the main process knows to load the live server
 * instead of the on-disk build.
 *
 * The value is NOT decided here: `scripts/dev-server-identity.cjs` holds the one definition of
 * the port, `vite.config.ts` binds it and `scripts/dev.cjs` hands it over — and only after it has
 * checked that the server answering there is THIS worktree's. The shell reads whatever it was
 * given, which is why the check has to happen before Electron is spawned rather than here.
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
