import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/dom";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

/**
 * HOW LONG A `waitFor` / `findBy*` MAY WAIT (finding `01KYW0SA15GX1CZ4QFVY9VD75G`).
 *
 * Testing Library's default is ONE second, and that second is WALL-CLOCK. The suite runs one fork
 * per core, each mounting jsdom + React + xterm, so a worker regularly gets a fraction of a CPU:
 * `restore-reconstruction` timed out with ZERO `pty_spawn` recorded — not because the spawn never
 * came, but because the worker had not been scheduled enough to get there. That is a starved
 * machine, not a broken assertion, and a suite that reddens for it stops being a signal.
 *
 * Raising the bound weakens NOTHING: every one of these waits polls an OBSERVABLE condition and
 * returns the instant it holds, so a green run is not one millisecond slower. Only a test that is
 * genuinely failing waits longer before saying so — which is the right trade for a deadline whose
 * only job is to stop an infinite wait.
 */
configure({ asyncUtilTimeout: 5_000 });

// jsdom has no ResizeObserver; the Terminal uses one to drive FitAddon. A no-op
// stub keeps the component mountable without affecting the assertions (which
// target the xterm buffer and the mocked IPC, not real layout/painting).
class ResizeObserverStub {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
// @ts-expect-error assigning the stub onto the jsdom global
globalThis.ResizeObserver = globalThis.ResizeObserver ?? ResizeObserverStub;

// jsdom does not implement HTMLCanvasElement.getContext and logs a loud "Not
// implemented" notice each time xterm probes for a renderer context. Returning
// null is the honest "no 2d/webgl context here" answer and keeps the console
// clean; xterm degrades cleanly (we assert the BUFFER, not pixels — jsdom does
// not paint). Real GL rendering is exercised in phase 3 (Browser Mode).
if (typeof HTMLCanvasElement !== "undefined") {
  HTMLCanvasElement.prototype.getContext = (() =>
    null) as typeof HTMLCanvasElement.prototype.getContext;
}

// jsdom does not implement matchMedia; xterm's CoreBrowserService calls it to
// track devicePixelRatio. A minimal stub lets the terminal mount in jsdom.
if (typeof window !== "undefined" && !window.matchMedia) {
  window.matchMedia = (query: string): MediaQueryList =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
}

// Reset DOM + spies between tests so state never leaks. Order matters: unmount first
// (cleanup), then flush the microtask queue so any deferred PTY teardown runs before we
// restore spies. The bridge double (`@/bridge/test-harness`) auto-resets itself in its
// own afterEach (the Tauri `clearMocks()` is gone — only the Electron adapter ships).
//
// MIND THE ORDER ACROSS FILES: vitest runs `afterEach` hooks LAST-REGISTERED-FIRST, so a hook a
// SPEC FILE declares runs BEFORE this one — i.e. before the unmount. A spec that tears down
// module-level state of its own must therefore call `cleanup()` itself first, or it empties that
// state under a tree still mounted and alive to repopulate it (finding
// `01KYW0SA15GX1CZ4QFVY9VD75G`, see `sidebar/restore-reconstruction.test.tsx`). `cleanup()` is
// idempotent, so calling it there leaves this one a no-op.
afterEach(async () => {
  cleanup();
  // Let queued microtasks (deferred teardown) and timers settle.
  await new Promise((r) => setTimeout(r, 0));
  vi.restoreAllMocks();
  // Clear renderer UI-state persistence (the Terminals shell now remembers the last active
  // project/terminal in `localStorage`) so it never bleeds across tests — each test starts with
  // no remembered selection, matching a fresh install.
  try {
    window.localStorage?.clear();
  } catch {
    // No/hostile storage in this environment — nothing to reset.
  }
});
