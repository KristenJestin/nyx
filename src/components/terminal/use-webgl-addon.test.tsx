import { renderHook } from "@testing-library/react";
import type { Terminal as XTerm } from "@xterm/xterm";
import { describe, expect, it, vi } from "vitest";

import { useWebglAddon, type WebglLike } from "./use-webgl-addon";

/**
 * A fake xterm instance that records loaded addons, plus a fake WebGL addon
 * factory whose instances track dispose/contextLoss wiring. jsdom has no real
 * WebGL context, so we inject the factory to exercise the attach/dispose CYCLE
 * deterministically — the real context creation is proven only in Browser Mode
 * (phase 4), out of scope here.
 */
function makeFakeXterm() {
  const loaded: WebglLike[] = [];
  const term = {
    loadAddon: (addon: WebglLike) => {
      loaded.push(addon);
    },
  } as unknown as XTerm;
  return { term, loaded };
}

interface FakeAddon extends WebglLike {
  disposed: boolean;
  contextLossCb?: () => void;
}

function makeFactory() {
  const created: FakeAddon[] = [];
  const factory = (): WebglLike => {
    const addon: FakeAddon = {
      disposed: false,
      dispose() {
        this.disposed = true;
      },
      onContextLoss(cb: () => void) {
        addon.contextLossCb = cb;
      },
      clearTextureAtlas() {},
    };
    created.push(addon);
    return addon;
  };
  return { factory, created };
}

describe("useWebglAddon", () => {
  it("does NOT attach WebGL while inactive", () => {
    const { term } = makeFakeXterm();
    const { factory, created } = makeFactory();
    renderHook(() => useWebglAddon(term, false, { factory }));
    expect(created).toHaveLength(0);
  });

  it("attaches WebGL when active", () => {
    const { term, loaded } = makeFakeXterm();
    const { factory, created } = makeFactory();
    renderHook(() => useWebglAddon(term, true, { factory }));
    expect(created).toHaveLength(1);
    expect(loaded).toContain(created[0]);
  });

  it("disposes the addon when the terminal goes inactive (blur)", () => {
    const { term } = makeFakeXterm();
    const { factory, created } = makeFactory();
    const { rerender } = renderHook(
      ({ active }: { active: boolean }) => useWebglAddon(term, active, { factory }),
      { initialProps: { active: true } },
    );
    expect(created).toHaveLength(1);
    expect((created[0] as FakeAddon).disposed).toBe(false);

    // Blur → inactive: the WebGL context must be released.
    rerender({ active: false });
    expect((created[0] as FakeAddon).disposed).toBe(true);
  });

  it("re-attaches a fresh addon on re-focus", () => {
    const { term } = makeFakeXterm();
    const { factory, created } = makeFactory();
    const { rerender } = renderHook(
      ({ active }: { active: boolean }) => useWebglAddon(term, active, { factory }),
      { initialProps: { active: true } },
    );
    rerender({ active: false });
    rerender({ active: true });

    // A second, distinct addon was created and the first stays disposed.
    expect(created).toHaveLength(2);
    expect((created[0] as FakeAddon).disposed).toBe(true);
    expect((created[1] as FakeAddon).disposed).toBe(false);
  });

  it("never attaches a second context while staying active (no churn)", () => {
    const { term } = makeFakeXterm();
    const { factory, created } = makeFactory();
    const { rerender } = renderHook(
      ({ active }: { active: boolean }) => useWebglAddon(term, active, { factory }),
      { initialProps: { active: true } },
    );
    // Re-render with the SAME active value: must not create a 2nd context.
    rerender({ active: true });
    rerender({ active: true });
    expect(created).toHaveLength(1);
  });

  it("disposes on unmount", () => {
    const { term } = makeFakeXterm();
    const { factory, created } = makeFactory();
    const { unmount } = renderHook(() => useWebglAddon(term, true, { factory }));
    expect((created[0] as FakeAddon).disposed).toBe(false);
    unmount();
    expect((created[0] as FakeAddon).disposed).toBe(true);
  });

  it("falls back cleanly if the factory throws (WebGL unavailable)", () => {
    const { term } = makeFakeXterm();
    const factory = vi.fn(() => {
      throw new Error("no webgl");
    });
    // Must not throw out of the hook.
    expect(() => renderHook(() => useWebglAddon(term, true, { factory }))).not.toThrow();
  });

  it("is inert with a null instance", () => {
    const { factory, created } = makeFactory();
    renderHook(() => useWebglAddon(null, true, { factory }));
    expect(created).toHaveLength(0);
  });
});
