/**
 * Vitest test harness for `nyxBridge` — the SHELL-AGNOSTIC double that replaces the
 * direct `@tauri-apps/api/mocks` (`mockIPC`) + `@tauri-apps/api/event` (`emit`) usage
 * in component/hook tests (phase 3, task #26).
 *
 * It mocks the `@/bridge` module so every `import { nyxBridge } from "@/bridge"` in
 * the code under test resolves to a single live {@link FakeNyxBridge} — the same
 * in-memory adapter the contract suite holds to the contract. Tests drive it through
 * a `mockIPC` / `emit` / `clearMocks` shim with the SAME shape they already use, so
 * migrating a file off the Tauri mocks is a near-mechanical import swap:
 *
 *   import { mockIPC } from "@tauri-apps/api/mocks";   →   from "@/bridge/test-harness"
 *   import { emit }    from "@tauri-apps/api/event";   →   from "@/bridge/test-harness"
 *
 * Importing this module ANYWHERE in a test file installs the `@/bridge` mock for that
 * file (the `vi.mock` is hoisted by vitest). `mockIPC(handler)` installs a catch-all
 * backend so every `nyxBridge.invoke(cmd,args)` (and the typed `pty*` methods that
 * delegate to it) routes to `handler(cmd,args)`; `emit(channel,payload)` pushes an
 * event to that channel's live subscribers; `clearMocks()` (auto-called by
 * `vitest.setup.ts`'s afterEach) resets the fake between tests.
 */
import { afterEach, vi } from "vitest";

import { FakeNyxBridge } from "./fake";
import type { BackendEvent } from "./contract";

/** The single live fake for the current test file. A `clearMocks()` resets it
 *  IN PLACE (clearing handlers/subscribers) so the captured `nyxBridge` reference in
 *  the code under test always points at the live double. */
const fake = new FakeNyxBridge();

/** Install the `@/bridge` module mock so `nyxBridge` IS the fake. Hoisted by vitest. */
vi.mock("@/bridge", async (importOriginal) => {
  // Keep the real contract exports (types, isBridgeError); only swap the bridge.
  const actual = await importOriginal<typeof import("./index")>();
  return { ...actual, nyxBridge: fake, default: fake };
});

// Auto-reset the double between tests so a migrated file gets the same auto-clean
// the Tauri `clearMocks()` gave it — without each test wiring its own afterEach. The
// global `vitest.setup.ts` afterEach runs FIRST (unmount + flush + clearMocks for any
// residual Tauri mock); this one then resets the bridge double.
afterEach(() => {
  fake.reset();
});

/** The live fake instance for direct assertions (subscriberCount, windowCalls, …). */
export function bridgeFake(): FakeNyxBridge {
  return fake;
}

/**
 * Install a catch-all backend handler — the drop-in for `@tauri-apps/api/mocks`'
 * `mockIPC`. `handler(cmd, args)` returns the result (resolved) or throws (rejected
 * as a BridgeError). The second `options` arg (Tauri's `{ shouldMockEvents }`) is
 * accepted and ignored — the fake always supports events.
 */
export function mockIPC(
  handler: (cmd: string, args?: Record<string, unknown>) => unknown,
  _options?: { shouldMockEvents?: boolean },
): void {
  fake.onAnyCommand((cmd, args) => handler(cmd, args));
}

/**
 * Push an event to the channel's live subscribers — the drop-in for
 * `@tauri-apps/api/event`'s `emit`. The payload is delivered UNCHANGED to every
 * `subscribe*` listener on `channel` (the fake's `subscribe*` map it exactly as the
 * real adapters do). Returns a resolved promise so `await emit(...)` works.
 */
export function emit(channel: BackendEvent | string, payload?: unknown): Promise<void> {
  fake.emit(channel, payload);
  return Promise.resolve();
}
