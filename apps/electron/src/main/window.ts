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
 * chrome (drag region + min/max/close) is drawn by the React front and driven
 * through the `nyx:window/*` IPC channels (see `./ipc.ts`).
 */
import { join } from "node:path";
import { BrowserWindow, shell, type App } from "electron";

import { devServerUrl } from "./env";

/**
 * Build and show the main window. Returns the created `BrowserWindow` so the caller
 * (the single-instance focus handler, the lifecycle code) can address it later.
 *
 * @param app the Electron `app` singleton (for `isPackaged` / resource resolution).
 */
export function createMainWindow(app: App): BrowserWindow {
  const win = new BrowserWindow({
    // Mirror the Tauri window: nyx, 800×600, frameless.
    title: "nyx",
    width: 800,
    height: 600,
    // Frameless on every OS; the React front draws its own title bar + controls.
    frame: false,
    // Avoid a white flash: stay hidden until the renderer's first paint.
    show: false,
    backgroundColor: "#000000",
    webPreferences: {
      // The ONLY bridge into the renderer — built from the allowlist in ./ipc.ts.
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
    // Packaged: the frontend's static build is copied next to the compiled main
    // (see package.json `build` — `apps/frontend/dist` → `dist/renderer`). Loading
    // a file path keeps the renderer at the `file://` origin the nav-guard pins.
    void win.loadFile(join(__dirname, "..", "renderer", "index.html"));
  }

  return win;
}
