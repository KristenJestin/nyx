import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { Terminal as XTerm } from "@xterm/xterm";

// Force reduced-motion so `scrollToBottom` takes the deterministic INSTANT path
// (the rAF ease-out is GUI-validated, not unit-tested) — keeps the test free of
// requestAnimationFrame timing.
vi.mock("motion/react", () => ({ useReducedMotion: () => true }));

import { computeAtBottom, useAtBottom } from "./use-at-bottom";

describe("computeAtBottom (pure at-bottom decision)", () => {
  it("is true at the exact bottom (viewportY === baseY)", () => {
    expect(computeAtBottom(100, 100)).toBe(true);
  });
  it("is true within the 1-row slack", () => {
    expect(computeAtBottom(99, 100)).toBe(true);
  });
  it("is false when scrolled up past the slack", () => {
    expect(computeAtBottom(50, 100)).toBe(false);
    expect(computeAtBottom(98, 100)).toBe(false);
  });
  it("is true for an empty / single-page buffer (baseY 0)", () => {
    expect(computeAtBottom(0, 0)).toBe(true);
  });
});

/** Minimal mutable xterm fake exposing just what `useAtBottom` reads/subscribes. */
class FakeTerm {
  buffer = { active: { viewportY: 0, baseY: 0 } };
  scrollToBottom = vi.fn();
  scrollLines = vi.fn();
  private scrollCb?: () => void;
  private renderCb?: () => void;
  onScroll(cb: () => void) {
    this.scrollCb = cb;
    return { dispose: () => {} };
  }
  onRender(cb: () => void) {
    this.renderCb = cb;
    return { dispose: () => {} };
  }
  setPosition(viewportY: number, baseY: number) {
    this.buffer.active = { viewportY, baseY };
  }
  emitScroll() {
    this.scrollCb?.();
  }
  emitRender() {
    this.renderCb?.();
  }
}

describe("useAtBottom (live tracking)", () => {
  it("seeds atBottom from the instance's current position", () => {
    const term = new FakeTerm();
    term.setPosition(100, 100);
    const { result } = renderHook(() => useAtBottom(term as unknown as XTerm));
    expect(result.current.atBottom).toBe(true);
  });

  it("flips to false on scroll-up and back to true on scroll-down", () => {
    const term = new FakeTerm();
    term.setPosition(100, 100);
    const { result } = renderHook(() => useAtBottom(term as unknown as XTerm));

    act(() => {
      term.setPosition(40, 100);
      term.emitScroll();
    });
    expect(result.current.atBottom).toBe(false);

    act(() => {
      term.setPosition(100, 100);
      term.emitScroll();
    });
    expect(result.current.atBottom).toBe(true);
  });

  it("re-hides once new output (onRender moves baseY) lands back at the bottom", () => {
    const term = new FakeTerm();
    term.setPosition(40, 100);
    const { result } = renderHook(() => useAtBottom(term as unknown as XTerm));
    expect(result.current.atBottom).toBe(false);
    // New output advanced the buffer and the viewport followed to the bottom.
    act(() => {
      term.setPosition(160, 160);
      term.emitRender();
    });
    expect(result.current.atBottom).toBe(true);
  });

  it("scrollToBottom (reduced motion) calls the instance's instant scrollToBottom", () => {
    const term = new FakeTerm();
    term.setPosition(0, 100);
    const { result } = renderHook(() => useAtBottom(term as unknown as XTerm));
    act(() => result.current.scrollToBottom());
    expect(term.scrollToBottom).toHaveBeenCalledTimes(1);
    expect(term.scrollLines).not.toHaveBeenCalled();
  });

  it("treats a null instance as at-bottom (no button)", () => {
    const { result } = renderHook(() => useAtBottom(null));
    expect(result.current.atBottom).toBe(true);
    // No-op, must not throw.
    act(() => result.current.scrollToBottom());
  });
});
