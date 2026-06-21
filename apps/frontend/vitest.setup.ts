import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { clearMocks } from "@tauri-apps/api/mocks";
import { afterEach, vi } from "vitest";

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

// Reset DOM + Tauri IPC/event mocks + spies between tests so state never leaks.
// Order matters: unmount first (cleanup), then flush the microtask queue so any
// deferred PTY teardown runs WHILE the IPC/event mocks are still installed, and
// only then clear the mocks. Otherwise a teardown would race a cleared mock.
afterEach(async () => {
  cleanup();
  // Let queued microtasks (deferred teardown) and timers settle.
  await new Promise((r) => setTimeout(r, 0));
  clearMocks();
  vi.restoreAllMocks();
});
