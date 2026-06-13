import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { isTerminalNavChord, useTerminalShortcuts } from "./use-terminal-shortcuts";

/** Build a minimal KeyboardEvent-like object for the pure predicate. */
function key(
  k: string,
  mods: Partial<{ ctrl: boolean; meta: boolean; shift: boolean }> = {},
): KeyboardEvent {
  return {
    key: k,
    ctrlKey: mods.ctrl ?? false,
    metaKey: mods.meta ?? false,
    shiftKey: mods.shift ?? false,
  } as KeyboardEvent;
}

describe("isTerminalNavChord (pure — drives the xterm yield)", () => {
  it("matches Ctrl/Cmd+T and Ctrl/Cmd+W", () => {
    expect(isTerminalNavChord(key("t", { ctrl: true }))).toBe(true);
    expect(isTerminalNavChord(key("t", { meta: true }))).toBe(true);
    expect(isTerminalNavChord(key("w", { ctrl: true }))).toBe(true);
    expect(isTerminalNavChord(key("w", { meta: true }))).toBe(true);
  });

  it("matches the Ctrl cycling chords (Tab / Shift+Tab / PageDown / PageUp)", () => {
    expect(isTerminalNavChord(key("Tab", { ctrl: true }))).toBe(true);
    expect(isTerminalNavChord(key("Tab", { ctrl: true, shift: true }))).toBe(true);
    expect(isTerminalNavChord(key("PageDown", { ctrl: true }))).toBe(true);
    expect(isTerminalNavChord(key("PageUp", { ctrl: true }))).toBe(true);
  });

  it("does NOT match plain keys, bare modifiers, or non-bound chords", () => {
    expect(isTerminalNavChord(key("t"))).toBe(false);
    expect(isTerminalNavChord(key("a", { ctrl: true }))).toBe(false);
    expect(isTerminalNavChord(key("Tab"))).toBe(false);
    // Cycling is Ctrl-only: Cmd+Tab is the OS app switcher, not ours.
    expect(isTerminalNavChord(key("Tab", { meta: true }))).toBe(false);
  });
});

describe("useTerminalShortcuts (TanStack Hotkeys, bound on document)", () => {
  function setup() {
    const handlers = {
      onNew: vi.fn(),
      onClose: vi.fn(),
      onNext: vi.fn(),
      onPrev: vi.fn(),
    };
    renderHook(() => useTerminalShortcuts(handlers));
    return handlers;
  }

  // jsdom is non-mac, so `Mod` resolves to Control: dispatch with ctrlKey.
  function dispatch(
    k: string,
    mods: Partial<{ ctrl: boolean; meta: boolean; shift: boolean }> = {},
  ): KeyboardEvent {
    const ev = new KeyboardEvent("keydown", {
      key: k,
      ctrlKey: mods.ctrl ?? false,
      metaKey: mods.meta ?? false,
      shiftKey: mods.shift ?? false,
      bubbles: true,
      cancelable: true,
    });
    document.dispatchEvent(ev);
    return ev;
  }

  it("fires onNew on Ctrl+T and prevents default", () => {
    const h = setup();
    const ev = dispatch("t", { ctrl: true });
    expect(h.onNew).toHaveBeenCalledTimes(1);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("fires onClose on Ctrl+W", () => {
    const h = setup();
    dispatch("w", { ctrl: true });
    expect(h.onClose).toHaveBeenCalledTimes(1);
  });

  it("cycles with Ctrl+Tab / Ctrl+Shift+Tab", () => {
    const h = setup();
    dispatch("Tab", { ctrl: true });
    expect(h.onNext).toHaveBeenCalledTimes(1);
    dispatch("Tab", { ctrl: true, shift: true });
    expect(h.onPrev).toHaveBeenCalledTimes(1);
  });

  it("cycles with Ctrl+PageDown / Ctrl+PageUp", () => {
    const h = setup();
    dispatch("PageDown", { ctrl: true });
    expect(h.onNext).toHaveBeenCalledTimes(1);
    dispatch("PageUp", { ctrl: true });
    expect(h.onPrev).toHaveBeenCalledTimes(1);
  });

  it("ignores unrelated keys", () => {
    const h = setup();
    dispatch("a", { ctrl: true });
    dispatch("t");
    expect(h.onNew).not.toHaveBeenCalled();
    expect(h.onClose).not.toHaveBeenCalled();
  });
});
