/**
 * The `before-quit` ROUTING for the agent close-warning (PRD v2.5.6 #1, review finding
 * `01KXBSWRCPAYR4VZY5DVTF0PCX`).
 *
 * The story #1 criterion requires that "closing the window **OR quitting** with live agent
 * sessions triggers a close-warning". The WM close button already routes through the guard
 * (`window.ts` `win.on("close")` → withhold → `closeRequested` → renderer `CloseGuard` →
 * `confirmClose`). But the APP-QUIT gesture (the default menu `File>Quit`, the `Ctrl+Q` /
 * `Cmd+Q` accelerator, any programmatic `app.quit()`) reaches `before-quit`, which USED to
 * `event.preventDefault()` then `sidecar.stop().finally(() => app.exit(0))` — a HARD exit that
 * never fires any window `close`, so the guard was bypassed and live sessions were killed with
 * NO warning.
 *
 * The fix routes the quit through the SAME guard instead of forking a parallel one: if a live
 * main window is still open, `before-quit` WITHHOLDS the quit and asks that window to close
 * (`win.close()`), which re-enters `window.ts`'s `close` guard exactly as the WM button does —
 * so the renderer's agent close-warning runs, and only a confirmed close proceeds. Once the
 * window is gone (guard cleared / user confirmed / no live sessions), the SAME `before-quit`
 * fires again with no window left and performs the ordered sidecar teardown + hard exit.
 *
 * This module is intentionally PURE — it takes its collaborators through {@link QuitGuardDeps}
 * (structural window / sidecar surfaces, `preventDefault`, `exit`, `markQuitting`) and imports
 * NO Electron — so the routing is unit-testable under `bun test` without spinning up a runtime.
 */

/** The minimal window surface the quit guard drives (a real `BrowserWindow` satisfies it). */
export interface GuardableWindow {
  /** Whether the window has already been destroyed (its close fully completed). */
  isDestroyed(): boolean;
  /**
   * Ask the window to close. In the real app this re-enters `window.ts`'s `close` guard, which
   * (unless already confirmed) withholds the close and emits `closeRequested` to the renderer.
   */
  close(): void;
}

/** The minimal sidecar surface the quit guard needs for the ordered teardown. */
export interface StoppableSidecar {
  /** Whether the sidecar child is spawned and not yet exited. */
  readonly alive: boolean;
  /** Ordered shutdown (snapshot → stop PTYs/commands → exit), then force-kill if it lingers. */
  stop(): Promise<void>;
}

/** Everything {@link handleBeforeQuit} needs, injected so the routing stays Electron-free. */
export interface QuitGuardDeps {
  /** Resolve the current main window (or `null` if none is open). */
  getWindow: () => GuardableWindow | null;
  /** The supervised sidecar to tear down before the process exits. */
  sidecar: StoppableSidecar;
  /** Withhold the default quit (the `before-quit` event's `preventDefault`). */
  preventDefault: () => void;
  /** Hard-exit the process once the sidecar is down (`app.exit(0)`). */
  exit: () => void;
  /**
   * Latch "a quit was requested" so that, once the guarded window actually closes, the app
   * completes the quit even on macOS (where closing the last window would otherwise keep the
   * app resident — see `window-all-closed`).
   */
  markQuitting: () => void;
}

/**
 * The branch {@link handleBeforeQuit} took — returned so callers/tests can observe the routing
 * without reaching into Electron.
 *   - `guard-window` — a live window is open; the quit was withheld and routed through the
 *     window's close guard (the agent close-warning). NOTHING is killed yet.
 *   - `stop-sidecar` — no window to guard; the sidecar teardown + hard exit was started.
 *   - `proceed` — no window and no live sidecar; let the default quit proceed.
 */
export type QuitBranch = "guard-window" | "stop-sidecar" | "proceed";

/**
 * Handle a `before-quit`. See the module header for the full rationale.
 *
 * CRITICAL invariant (the finding): when a live window is still open, this MUST route through
 * the close guard (`win.close()`) and MUST NOT stop the sidecar or exit — otherwise the quit
 * would kill live agent sessions before the warning is shown. Only with NO window left does it
 * perform the teardown.
 */
export function handleBeforeQuit(deps: QuitGuardDeps): QuitBranch {
  const win = deps.getWindow();
  if (win && !win.isDestroyed()) {
    // A window is still open: route the quit through its close guard (the SAME path as the WM
    // close button). Latch the quit intent, withhold the default quit, and ask the window to
    // close — the renderer's agent close-warning decides whether the close proceeds. No kill.
    deps.markQuitting();
    deps.preventDefault();
    win.close();
    return "guard-window";
  }
  // No window to guard (the guard already cleared / user confirmed / none was open): ensure the
  // sidecar is torn down in order before the process exits, so no orphan sidecar / PTY survives.
  if (deps.sidecar.alive) {
    deps.preventDefault();
    void deps.sidecar.stop().finally(() => deps.exit());
    return "stop-sidecar";
  }
  return "proceed";
}
