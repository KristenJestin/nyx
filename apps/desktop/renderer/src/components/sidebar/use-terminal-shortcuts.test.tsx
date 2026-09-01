import { renderHook } from "@testing-library/react";
import { matchesKeyboardEvent } from "@tanstack/react-hotkeys";
import { describe, expect, it, vi } from "vitest";

import {
  isTerminalNavChord,
  SEARCH_PALETTE_CHORD,
  useTerminalShortcuts,
} from "./use-terminal-shortcuts";

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

  it("YIELDS Ctrl/Cmd+K — without it the search palette is unreachable from a terminal", () => {
    // The palette is opened from a focused terminal BY DEFINITION, and xterm stops propagation on
    // every key it consumes. If `k` leaves this predicate, `Mod+K` never leaves the textarea and
    // the whole global search dies silently — so this assertion is the feature's tripwire.
    expect(isTerminalNavChord(key("k", { ctrl: true }))).toBe(true);
    expect(isTerminalNavChord(key("K", { ctrl: true }))).toBe(true);
    expect(isTerminalNavChord(key("k", { meta: true }))).toBe(true);
  });

  it("does NOT match plain keys, bare modifiers, or non-bound chords", () => {
    expect(isTerminalNavChord(key("t"))).toBe(false);
    expect(isTerminalNavChord(key("k"))).toBe(false); // a bare `k` is text, and stays text
    expect(isTerminalNavChord(key("a", { ctrl: true }))).toBe(false);
    expect(isTerminalNavChord(key("Tab"))).toBe(false);
    // Cycling is Ctrl-only: Cmd+Tab is the OS app switcher, not ours.
    expect(isTerminalNavChord(key("Tab", { meta: true }))).toBe(false);
  });
});

describe("SEARCH_PALETTE_CHORD (`Mod` is resolved by TanStack, per platform)", () => {
  // The platform mapping belongs to the library — it is MOCKED here (`matchesKeyboardEvent`'s
  // `platform` argument) rather than re-implemented, so this proves OUR constant resolves to
  // Cmd on macOS and Ctrl elsewhere without duplicating a table that would then drift.
  // A REAL `KeyboardEvent` here (not the minimal literal above): the library compares all four
  // modifier flags, so an absent `altKey` would fail the match for the wrong reason.
  const withMods = (mods: Partial<{ ctrl: boolean; meta: boolean }>) =>
    new KeyboardEvent("keydown", {
      key: "k",
      ctrlKey: mods.ctrl ?? false,
      metaKey: mods.meta ?? false,
    });

  it("is Cmd+K on macOS — and Ctrl+K does NOT fire there", () => {
    expect(matchesKeyboardEvent(withMods({ meta: true }), SEARCH_PALETTE_CHORD, "mac")).toBe(true);
    expect(matchesKeyboardEvent(withMods({ ctrl: true }), SEARCH_PALETTE_CHORD, "mac")).toBe(false);
  });

  it("is Ctrl+K on Windows and Linux — and Cmd+K does NOT fire there", () => {
    for (const platform of ["windows", "linux"] as const) {
      expect(matchesKeyboardEvent(withMods({ ctrl: true }), SEARCH_PALETTE_CHORD, platform)).toBe(
        true,
      );
      expect(matchesKeyboardEvent(withMods({ meta: true }), SEARCH_PALETTE_CHORD, platform)).toBe(
        false,
      );
    }
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
