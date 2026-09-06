/**
 * Electron main entrypoint for the nyx shell (PRD phase 2, task #1).
 *
 * Responsibilities (phase 2 — scaffold only; the core-host wiring is task #2, its
 * lifecycle task #25, packaging task #23):
 *   - apply the Wayland-native + HiDPI command-line flags BEFORE `app` is ready;
 *   - enforce SINGLE INSTANCE (a second launch focuses the existing window);
 *   - create the single FRAMELESS, hardened main window (dev server vs packaged);
 *   - register the allowlisted `nyx:window/*` IPC handlers;
 *   - apply the global `web-contents-created` security guards (defense in depth);
 *   - standard cross-platform app lifecycle (activate on macOS, quit on all-closed
 *     elsewhere).
 *
 * Security baseline verified against the official Electron security checklist
 * (electronjs.org/docs/latest/tutorial/security): contextIsolation, no
 * nodeIntegration, sandbox, locked navigation, denied window.open.
 */
import { app, BrowserWindow, shell } from "electron";

import { registerCoreIpc } from "./core-ipc";
import { applyAppIdentity } from "./identity";
import { registerProjectImageScheme, serveProjectImages } from "./project-image-protocol";
import { handleBeforeQuit } from "./quit-guard";
import { Sidecar } from "./sidecar";
import { applyWaylandFlags } from "./wayland";
import { createMainWindow } from "./window";
import { registerWindowIpc } from "./window-ipc";

// --- Pre-ready: EXPLICIT v2 IDENTITY (must be the FIRST thing) ---------------
// SAFETY (review findings #1/#2): pin v2's name / AppUserModelId / userData IN CODE, before
// ANY `app.getPath('userData')` and before `whenReady`. This makes v2's data dir
// deterministically `<appData>/nyx-v2` (e.g. `~/.config/nyx-v2`) — NEVER the live v1 dir
// `~/.config/@nyx/electron`. `NYX_DATA_DIR` stays a dev/test override but is NO LONGER
// load-bearing for safety: even unset, v2 cannot resolve onto v1's data.
const userDataDir = applyAppIdentity(app);
console.log(`[main] app identity pinned: name=${app.getName()} userData=${userDataDir}`);

// --- Pre-ready: command-line flags ------------------------------------------
// Wayland/HiDPI switches must be appended before the app initializes Chromium.
applyWaylandFlags(app);

// --- Pre-ready: the project-picture scheme -----------------------------------
// `nyx-image://project-icons/<copy>` — how the renderer draws a project's picture in BOTH modes.
// It has to be declared here, before Chromium initialises, or Blink never issues the request: a
// dev renderer sits at `http://localhost` and a packaged one at `file://`, and neither may load a
// `file://` subresource from the other's origin. The handler is installed on ready, below; the
// rules it enforces live in `project-image.ts`. See `project-image-protocol.ts` for the why.
registerProjectImageScheme();

// --- Single instance ---------------------------------------------------------
// One nyx per user session: a second launch must focus the running window, not
// open a duplicate. `requestSingleInstanceLock` returns false in the second
// process, which then quits immediately; the first process receives
// `second-instance` and re-focuses its window. (Official BrowserWindow docs.)
const gotTheLock = app.requestSingleInstanceLock();

if (!gotTheLock) {
  app.quit();
} else {
  let mainWindow: BrowserWindow | null = null;

  // Latched once a QUIT gesture (menu `File>Quit`, `Ctrl+Q`/`Cmd+Q`, a programmatic
  // `app.quit()`) is routed through the window close guard, so that after the guarded window
  // actually closes the app COMPLETES the quit even on macOS (where `window-all-closed` would
  // otherwise leave the app resident). Set by the `before-quit` router below.
  let quitting = false;

  // The supervised `nyx-sidecar` Rust binary — the runtime authority (PDR-0006/0007). It
  // OWNS the SQLite DB + PTY + managed commands + MCP; main owns the local stdin/stdout
  // MessagePack pipe to it. Started on ready, stopped on quit. This REPLACES the
  // transitional Node core-host (no more nyx-napi).
  const sidecar = new Sidecar();

  app.on("second-instance", () => {
    if (!mainWindow) return;
    if (mainWindow.isMinimized()) mainWindow.restore();
    mainWindow.show();
    mainWindow.focus();
  });

  // --- Global security guard (defense in depth) ------------------------------
  // Even with per-window guards in createMainWindow, lock down navigation + popups
  // + webview on EVERY WebContents the app ever creates, so a future window can't
  // ship without the lockdown. (Security checklist items 12–14.) These mirror the
  // per-window guards in window.ts so the GLOBAL handler is the real backstop, not
  // a partial one.
  app.on("web-contents-created", (_event, contents) => {
    // Block any navigation of the top frame away from the WebContents' own origin;
    // hand external http/https links to the OS browser. Same same-origin rule as
    // window.ts:62, but applied to EVERY WebContents (no future window can navigate
    // freely without opting into the lockdown).
    contents.on("will-navigate", (event, url) => {
      const target = new URL(url);
      const current = contents.getURL();
      if (current && target.origin === new URL(current).origin) return;
      event.preventDefault();
      if (target.protocol === "http:" || target.protocol === "https:") {
        void shell.openExternal(url);
      }
    });
    // Deny ALL programmatic window creation (window.open, target=_blank); external
    // http/https links go to the OS browser instead of an unsecured Chromium popup.
    contents.setWindowOpenHandler(({ url }) => {
      if (url.startsWith("http:") || url.startsWith("https:")) {
        void shell.openExternal(url);
      }
      return { action: "deny" };
    });
    contents.on("will-attach-webview", (event) => {
      // No <webview> tags are used; refuse any attempt to attach one.
      event.preventDefault();
    });
  });

  void app.whenReady().then(() => {
    registerWindowIpc();
    // Serve the adopted pictures on the scheme declared above. AFTER ready (`protocol.handle`
    // needs a session) and BEFORE the window, so the very first paint can already draw a mark.
    serveProjectImages();
    // The renderer↔sidecar relay (invoke/ack + sidecar-event push). Events are pushed to
    // the CURRENT main window (resolved at emit time so a window recreated on macOS
    // `activate` still receives them).
    registerCoreIpc(sidecar, () => mainWindow);

    // Surface the sidecar lifecycle state. A degraded/fatal sidecar is a readable state the
    // renderer can show — never an infinite load (the boot handshake is bounded).
    sidecar.onState(({ state, reason }) => {
      const line = `[main] sidecar state=${state}${reason ? ` (${reason})` : ""}`;
      if (state === "fatal" || state === "degraded") console.error(line);
      else console.log(line);
    });
    // Spawn + await the bounded boot handshake. A boot timeout / spawn error / protocol
    // mismatch rejects here with a readable error (state already set to `fatal`); the UI
    // still comes up so the user sees the degraded state, not a frozen app. Once ready, the
    // MCP HTTP server is started (start-once) so the Claude plugin can register against it
    // (G1) — a bind failure is a non-fatal warning, never a hard boot failure.
    sidecar
      .start()
      .then(() => sidecar.startMcp())
      .then((port) => {
        if (port > 0) console.log(`[main] sidecar MCP server listening on 127.0.0.1:${port}`);
      })
      .catch((e) => {
        console.error("[main] sidecar failed to boot:", (e as Error).message);
      });

    mainWindow = createMainWindow(app);

    // macOS: re-create the window when the dock icon is clicked and none are open.
    app.on("activate", () => {
      if (BrowserWindow.getAllWindows().length === 0) {
        mainWindow = createMainWindow(app);
      }
    });
  });

  // Route the QUIT gesture through the SAME agent close-warning as the WM close button, and
  // tear the sidecar down before the app fully exits (finding 01KXBSWRCPAYR4VZY5DVTF0PCX).
  //
  // Menu `File>Quit`, the `Ctrl+Q` / `Cmd+Q` accelerator, and any programmatic `app.quit()` all
  // reach `before-quit` with the window STILL OPEN. The old handler force-exited here
  // (`app.exit(0)`), bypassing every window `close` and killing live agent sessions with no
  // warning. Now, while a live window is open, we WITHHOLD the quit and `win.close()` it — which
  // re-enters `window.ts`'s close guard (the renderer's `agent_close_warnings` flow), exactly
  // like the WM close button. Only once the window is gone (guard cleared / user confirmed / no
  // live sessions) does the SAME `before-quit` fire again with no window left and perform the
  // ordered sidecar teardown, then the hard `app.exit(0)` (which does NOT re-enter `before-quit`).
  app.on("before-quit", (event) => {
    handleBeforeQuit({
      getWindow: () => mainWindow,
      sidecar,
      preventDefault: () => event.preventDefault(),
      exit: () => app.exit(0),
      markQuitting: () => {
        quitting = true;
      },
    });
  });

  // Quit when all windows are closed, except on macOS where apps stay alive until the user
  // quits explicitly (standard platform convention) — UNLESS a quit was explicitly requested
  // (a `Cmd+Q` routed through the close guard), which must actually terminate the app once its
  // guarded window has closed.
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin" || quitting) {
      app.quit();
    }
  });
}
