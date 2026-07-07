import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

import { setCoreFactory } from "./src/components/terminal/ghostty-core";
import { FakeTerminalCore } from "./src/components/terminal/fake-core";

// jsdom cannot fetch/instantiate the ghostty WASM (no real `fetch` of a bundled
// asset, no WASM host), so the DEFAULT terminal core factory is swapped for a
// lightweight `FakeTerminalCore` for the whole unit suite. Any component that
// mounts a terminal (the deck, the pane host, a page) then builds its wterm on
// the fake core instead of hitting the WASM — the jsdom analogue of the canvas /
// ResizeObserver stubs below. Tests that need to inspect the written bytes
// override the factory themselves (capturing the core they build).
//
// NOTE: `ghostty-core` + `fake-core` are safe to import here (neither touches
// `@/bridge`). We DELIBERATELY do NOT statically import `terminal-store` /
// `wterm-instance` here — they capture `nyxBridge` from `@/bridge`, and a static
// import in this setup file would bind the REAL bridge before a test file's
// `vi.mock("@/bridge")` hoists, giving the terminal instance a different bridge
// than the test's fake. The store reset is done via a DYNAMIC import in the
// afterEach below, which resolves under the per-file mock.
setCoreFactory(async () => new FakeTerminalCore());

// jsdom has no ResizeObserver, but wterm's app-driven sizing path (`WtermInstance.
// observeResize`) constructs one to react to pane box changes. A no-op stub keeps
// terminal-mounting components mountable without affecting the assertions (which
// read the ghostty/FakeTerminalCore screen via `readScreenText` and the mocked
// IPC, not real layout/painting).
class ResizeObserverStub {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
// @ts-expect-error assigning the stub onto the jsdom global
globalThis.ResizeObserver = globalThis.ResizeObserver ?? ResizeObserverStub;

// Reset DOM + spies between tests so state never leaks. Order matters: unmount first
// (cleanup), then flush the microtask queue so any deferred PTY teardown runs before we
// restore spies. The bridge double (`@/bridge/test-harness`) auto-resets itself in its
// own afterEach (the Tauri `clearMocks()` is gone — only the Electron adapter ships).
afterEach(async () => {
  cleanup();
  // Let queued microtasks (deferred teardown) and timers settle.
  await new Promise((r) => setTimeout(r, 0));
  // Drop any module-level terminal instances a test created, so the no-host store
  // never leaks a live instance (or its PTY listeners) into the next test. Loaded
  // DYNAMICALLY (not statically at top) so it resolves under the test file's
  // `@/bridge` mock — see the note by `setCoreFactory` above.
  try {
    const { __resetTerminalStore } = await import("./src/components/terminal/terminal-store");
    __resetTerminalStore();
  } catch {
    // A test file without the terminal store in its graph: nothing to reset.
  }
  vi.restoreAllMocks();
});
