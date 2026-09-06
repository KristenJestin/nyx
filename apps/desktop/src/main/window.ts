/**
 * Creates the single main `BrowserWindow` — frameless, secure, dev/prod-aware.
 *
 * Security posture (per the PRD's frozen decisions + the official Electron security
 * checklist, verified against electronjs.org/docs/latest/tutorial/security):
 *   - `contextIsolation: true` (default ≥ v12) — preload + Electron run in a
 *     separate JS context from the page.
 *   - `nodeIntegration: false` (default ≥ v5) — the renderer has no Node.
 *   - `sandbox: true` (default ≥ v20) — Chromium OS-level sandbox on the renderer.
 *   - a single `preload` that exposes ONLY the allowlisted `window.nyxWindow` API.
 *   - navigation locked down: `will-navigate` blocks in-page navigation away from
 *     the app origin, and `setWindowOpenHandler` denies ALL `window.open` / target
 *     `_blank` popups (the renderer can never spawn an uncontrolled BrowserWindow).
 *
 * Frameless: `frame: false` (cross-platform — same on Linux & Windows). The custom
 * chrome (drag region + min/max/close) is drawn by the React front — `<ChromeBar>`,
 * mounted at the top of the navigation shell — and driven through the `nyx:window/*`
 * IPC channels (see `./window-ipc.ts`). That bar is NOT decoration: under Windows the
 * OS gives an undecorated window no controls at all, so it is the only thing standing
 * between the user and a window they cannot move or close.
 */
import { existsSync } from "node:fs";
import { join } from "node:path";
import { BrowserWindow, shell, type App } from "electron";

import { WINDOW_CHANNELS } from "../shared/ipc";
import { devServerUrl } from "./env";

/**
 * Windows whose close has been CONFIRMED by the renderer's close-guard (agent
 * close-warning cleared / the user chose "Close anyway"). The `close` guard installed
 * in {@link createMainWindow} withholds the real close and asks the renderer FIRST;
 * once a window is in this set the guard steps aside and lets it close. A `WeakSet`
 * so a destroyed window is collected without bookkeeping.
 */
const confirmedClose = new WeakSet<BrowserWindow>();

/**
 * PROCEED with the real close of `win` after the renderer's close-guard cleared it —
 * the target of the `nyx:window/confirm-close` IPC. Marks the window confirmed so the
 * `close` guard no longer withholds, then closes it (which re-fires `close`, now a
 * no-op pass-through). Idempotent: a second call on an already-closing window is safe.
 */
export function confirmWindowClose(win: BrowserWindow): void {
  confirmedClose.add(win);
  if (!win.isDestroyed()) win.close();
}

/**
 * Build and show the main window. Returns the created `BrowserWindow` so the caller
 * (the single-instance focus handler, the lifecycle code) can address it later.
 *
 * @param app the Electron `app` singleton (for `isPackaged` / resource resolution).
 */
export function createMainWindow(app: App): BrowserWindow {
  // The nyx face, staged by `scripts/copy-resources.cjs` from `build/icon.png`. Linux X11 and
  // Windows read the window icon from here (Wayland takes it from the `.desktop` entry, macOS
  // from the bundle). Optional so a dev tree without the staged resources still opens.
  const icon = join(__dirname, "..", "resources", "icon.png");
  const win = new BrowserWindow({
    ...(existsSync(icon) ? { icon } : {}),
    // Mirror the Tauri window: nyx-v2, 800×600, frameless. The title is the v2-distinct
    // identity so the window is never confused with a running v1 `nyx`.
    title: "nyx-v2",
    width: 800,
    height: 600,
    // Frameless on every OS; the React front draws its own title bar + controls.
    frame: false,
    // Avoid a white flash: stay hidden until the renderer's first paint.
    show: false,
    // The DARK THEME's `--background` — `oklch(0.145 0 0)` in `renderer/src/globals.css` — as
    // sRGB hex. Electron reads this option when it creates the native window, long before any
    // renderer CSS exists, so it CANNOT read the token: the value is a KNOWING DUPLICATE of it,
    // and of the same literal in `renderer/index.html`'s boot splash. The three have to agree —
    // the window chrome, the splash and the first React screen paint one continuous surface, and
    // any drift between them shows up as a flash at the moment the window appears. It used to be
    // `#000000`, a colour the theme uses nowhere. `renderer/src/boot-splash.guard.test.ts`
    // converts the oklch token itself and fails on the day one of the three moves.
    backgroundColor: "#0a0a0a",
    webPreferences: {
      // The ONLY bridge into the renderer — built from the channel allowlist in
      // ../shared/ipc.ts (`WINDOW_CHANNELS` + `CORE_CHANNELS`), whose main-side
      // handlers live in ./window-ipc.ts and ./core-ipc.ts.
      preload: join(__dirname, "..", "preload", "index.js"),
      // Hardened renderer (these are the modern defaults; pinned explicitly so a
      // future Electron default-flip can't silently weaken the shell).
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      // Never expose the experimental in-renderer Node-in-worker surface.
      nodeIntegrationInWorker: false,
      webviewTag: false,
    },
  });

  // Show only once the page is ready to paint (no incremental-render flash).
  win.once("ready-to-show", () => {
    win.show();
  });

  // --- Close guard: agent close-warning before the window is destroyed --------
  // A close can arrive from the OS (Alt-F4, taskbar, quit) or from the app's own chrome
  // bar — whose X runs the warning itself and then comes back through
  // `nyx:window/confirm-close`, so it does not trip this guard twice. WITHHOLD any other
  // close and ask the RENDERER first: it runs the agent close-warning flow
  // (`agent_close_warnings`) and, only when clear or the user confirms, calls back
  // `confirmWindowClose` (the `confirm-close` IPC) which re-enters here having set the
  // confirmed flag, so the window actually closes. Fail-safe: if the webContents is
  // already gone (no one to ask), let the close proceed so the window is never trapped.
  win.on("close", (event) => {
    if (confirmedClose.has(win)) return;
    if (win.webContents.isDestroyed()) return;
    event.preventDefault();
    win.webContents.send(WINDOW_CHANNELS.closeRequested);
  });

  // --- Navigation lockdown (official security checklist 12 & 13) --------------
  // Block ANY attempt to navigate the top frame away from the app's own content.
  win.webContents.on("will-navigate", (event, url) => {
    const target = new URL(url);
    const current = win.webContents.getURL();
    // Allow only same-document/same-origin navigation; everything else is denied
    // and (if it's an http/https link) opened in the user's real browser.
    if (current && target.origin === new URL(current).origin) return;
    event.preventDefault();
    if (target.protocol === "http:" || target.protocol === "https:") {
      void shell.openExternal(url);
    }
  });

  // Deny ALL programmatic window creation (window.open, target=_blank). External
  // http/https links are handed to the OS browser instead of opening a Chromium
  // popup we'd have to secure.
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith("http:") || url.startsWith("https:")) {
      void shell.openExternal(url);
    }
    return { action: "deny" };
  });

  // --- Load the renderer (dev server vs packaged build) -----------------------
  // A packaged binary is ALWAYS prod (load the on-disk build), even if launched
  // with a stray dev env; only an unpackaged run honors the dev-server URL.
  const devUrl = app.isPackaged ? undefined : devServerUrl();
  if (devUrl) {
    void win.loadURL(devUrl);
    win.webContents.openDevTools({ mode: "detach" });
  } else {
    // Packaged: the renderer's static build is copied next to the compiled main
    // (see package.json `copy:renderer` — `renderer/dist` → `dist/renderer`). Loading
    // a file path keeps the renderer at the `file://` origin the nav-guard pins.
    void win.loadFile(join(__dirname, "..", "renderer", "index.html"));
  }

  return win;
}
