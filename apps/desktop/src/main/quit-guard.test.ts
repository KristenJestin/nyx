/**
 * MAIN-process tests for the `before-quit` routing (PRD v2.5.6 #1, review finding
 * `01KXBSWRCPAYR4VZY5DVTF0PCX`).
 *
 * The gap these tests close: the QUIT gesture (menu `File>Quit`, `Ctrl+Q`/`Cmd+Q`, a
 * programmatic `app.quit()`) reached `before-quit`, which force-exited (`app.exit(0)`) WITHOUT
 * firing any window `close` — so the agent close-warning guard was bypassed and live sessions
 * were killed with NO warning. The renderer-side `<CloseGuard>` tests already cover
 * `closeRequested → warning`; what was untested is that a QUIT with a live window ROUTES into
 * that guard (via `win.close()`) BEFORE anything is killed.
 *
 * {@link handleBeforeQuit} is PURE (structural window/sidecar surfaces injected), so these run
 * under `bun test` with no Electron runtime. A fake window models the two states that matter —
 * OPEN (a real quit gesture) vs already DESTROYED (the guard has since cleared and the window
 * closed) — and records whether it was asked to `close()`.
 *
 * Run via `bun test` (the `smoke:quit-guard` npm script).
 */
import { describe, expect, test } from "bun:test";

import {
  handleBeforeQuit,
  type GuardableWindow,
  type StoppableSidecar,
} from "./quit-guard";

/** A fake window that records `close()` calls and reports a fixed destroyed state. */
function fakeWindow(destroyed: boolean): GuardableWindow & { closeCalls: number } {
  return {
    closeCalls: 0,
    isDestroyed() {
      return destroyed;
    },
    close() {
      this.closeCalls += 1;
    },
  };
}

/** A fake sidecar that records whether `stop()` was called; `alive` is fixed. */
function fakeSidecar(alive: boolean): StoppableSidecar & { stopCalls: number } {
  return {
    stopCalls: 0,
    alive,
    stop() {
      this.stopCalls += 1;
      return Promise.resolve();
    },
  };
}

describe("handleBeforeQuit routes the quit gesture through the close-warning guard", () => {
  // THE core done-criterion: quitting with a live window must route through the guard
  // (`win.close()` → renderer agent close-warning) and kill NOTHING yet.
  test("open window: withholds the quit, routes through win.close(), kills nothing", () => {
    const win = fakeWindow(false);
    const sidecar = fakeSidecar(true);
    let prevented = 0;
    let exited = 0;
    let quitting = false;

    const branch = handleBeforeQuit({
      getWindow: () => win,
      sidecar,
      preventDefault: () => (prevented += 1),
      exit: () => (exited += 1),
      markQuitting: () => (quitting = true),
    });

    expect(branch).toBe("guard-window");
    // The quit is WITHHELD and the window is asked to close (which fires the close guard →
    // `closeRequested` → the renderer's agent close-warning).
    expect(prevented).toBe(1);
    expect(win.closeCalls).toBe(1);
    expect(quitting).toBe(true);
    // Nothing is killed before the warning: the sidecar is NOT stopped and the process does
    // NOT exit while the window is still open.
    expect(sidecar.stopCalls).toBe(0);
    expect(exited).toBe(0);
  });

  // After the guarded window has closed (`before-quit` fires a SECOND time), there is no window
  // left, so the ordered sidecar teardown + hard exit run.
  test("no window + live sidecar: tears the sidecar down then exits", async () => {
    const sidecar = fakeSidecar(true);
    let prevented = 0;
    let exited = 0;

    const branch = handleBeforeQuit({
      getWindow: () => null,
      sidecar,
      preventDefault: () => (prevented += 1),
      exit: () => (exited += 1),
      markQuitting: () => {},
    });

    expect(branch).toBe("stop-sidecar");
    expect(prevented).toBe(1);
    expect(sidecar.stopCalls).toBe(1);
    // The exit is chained after `stop()` resolves — await the microtask queue to observe it.
    await Promise.resolve();
    await Promise.resolve();
    expect(exited).toBe(1);
  });

  // A DESTROYED window is treated as "gone" (its close already completed), so the teardown runs
  // rather than re-routing into an already-closed window.
  test("destroyed window + live sidecar: does not re-close, tears the sidecar down", async () => {
    const win = fakeWindow(true);
    const sidecar = fakeSidecar(true);
    let exited = 0;

    const branch = handleBeforeQuit({
      getWindow: () => win,
      sidecar,
      preventDefault: () => {},
      exit: () => (exited += 1),
      markQuitting: () => {},
    });

    expect(branch).toBe("stop-sidecar");
    expect(win.closeCalls).toBe(0);
    expect(sidecar.stopCalls).toBe(1);
    await Promise.resolve();
    await Promise.resolve();
    expect(exited).toBe(1);
  });

  // No window and a dead sidecar: nothing to guard or tear down — let the default quit proceed.
  test("no window + dead sidecar: proceeds without withholding", () => {
    const sidecar = fakeSidecar(false);
    let prevented = 0;
    let exited = 0;

    const branch = handleBeforeQuit({
      getWindow: () => null,
      sidecar,
      preventDefault: () => (prevented += 1),
      exit: () => (exited += 1),
      markQuitting: () => {},
    });

    expect(branch).toBe("proceed");
    expect(prevented).toBe(0);
    expect(sidecar.stopCalls).toBe(0);
    expect(exited).toBe(0);
  });
});
