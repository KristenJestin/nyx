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

import { CoreHost } from "./core-host";
import { registerCoreIpc } from "./core-ipc";
import { applyWaylandFlags } from "./wayland";
import { createMainWindow } from "./window";
import { registerWindowIpc } from "./window-ipc";

// --- Pre-ready: command-line flags ------------------------------------------
// Wayland/HiDPI switches must be appended before the app initializes Chromium.
applyWaylandFlags(app);

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

  // The dedicated core-host (Node-pure, owns nyx-napi + the PTY). Started on ready,
  // stopped on quit. Its boot-handshake/crash/shutdown POLICY is task #25; here we
  // only own its lifetime relative to the app.
  const coreHost = new CoreHost();

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
    // The renderer↔core-host relay (PTY invoke/ack + host-event push). Events are
    // pushed to the CURRENT main window (resolved at emit time so a window recreated
    // on macOS `activate` still receives them).
    registerCoreIpc(coreHost, () => mainWindow);

    // Surface the core-host lifecycle state. A degraded/fatal host is a readable
    // state the renderer can show — never an infinite load (the boot handshake is
    // bounded). Phase 3 forwards these over the allowlisted IPC to the front.
    coreHost.onState(({ state, reason }) => {
      const line = `[main] core-host state=${state}${reason ? ` (${reason})` : ""}`;
      if (state === "fatal" || state === "degraded") console.error(line);
      else console.log(line);
    });
    coreHost.onEvent((event) => {
      if (event.kind === "ready") {
        console.log(
          `[main] core-host ready — nyx-core ${event.info.coreVersion}, nodePure=${event.info.nodePure}, dataDir=${event.info.dataDir}`,
        );
      }
    });
    // Spawn + await the bounded boot handshake. A boot timeout or `.node` load
    // failure rejects here with a readable error (state already set to `fatal`);
    // the UI still comes up so the user sees the degraded state, not a frozen app.
    coreHost.start().catch((e) => {
      console.error("[main] core-host failed to boot:", (e as Error).message);
    });

    mainWindow = createMainWindow(app);

    // macOS: re-create the window when the dock icon is clicked and none are open.
    app.on("activate", () => {
      if (BrowserWindow.getAllWindows().length === 0) {
        mainWindow = createMainWindow(app);
      }
    });
  });

  // Ensure the core-host is torn down before the app fully exits, so no orphan
  // Node-pure host survives (task #25 hardens the forced/timeout path).
  app.on("before-quit", (event) => {
    if (!coreHost.alive) return;
    event.preventDefault();
    void coreHost.stop().finally(() => app.exit(0));
  });

  // Quit when all windows are closed, except on macOS where apps stay alive until
  // the user quits explicitly (standard platform convention).
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") {
      app.quit();
    }
  });
}
