import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  copySelection,
  isCopyChord,
  isPasteChord,
  pasteFromClipboard,
  type ClipboardTerminal,
} from "./terminal-clipboard";

/** Build a minimal KeyboardEvent-like object for the pure predicates. */
function key(
  code: string,
  mods: Partial<{ ctrl: boolean; shift: boolean; meta: boolean; alt: boolean; type: string }> = {},
): KeyboardEvent {
  return {
    code,
    type: mods.type ?? "keydown",
    ctrlKey: mods.ctrl ?? false,
    shiftKey: mods.shift ?? false,
    metaKey: mods.meta ?? false,
    altKey: mods.alt ?? false,
  } as KeyboardEvent;
}

/** A fake xterm exposing only the clipboard surface, with a spy on `paste`. */
function fakeTerm(opts: { selection?: string } = {}): ClipboardTerminal {
  const selection = opts.selection ?? "";
  return {
    hasSelection: () => selection.length > 0,
    getSelection: () => selection,
    paste: vi.fn(),
  };
}

describe("isCopyChord / isPasteChord (pure)", () => {
  it("matches Ctrl+Shift+C / Ctrl+Shift+V", () => {
    expect(isCopyChord(key("KeyC", { ctrl: true, shift: true }))).toBe(true);
    expect(isPasteChord(key("KeyV", { ctrl: true, shift: true }))).toBe(true);
  });

  it("does NOT match plain Ctrl+C (must stay SIGINT)", () => {
    expect(isCopyChord(key("KeyC", { ctrl: true }))).toBe(false);
    // Symmetric: bare Ctrl+V is not our paste either.
    expect(isPasteChord(key("KeyV", { ctrl: true }))).toBe(false);
  });

  it("does NOT match Shift+C without Ctrl, or the wrong letter", () => {
    expect(isCopyChord(key("KeyC", { shift: true }))).toBe(false);
    expect(isCopyChord(key("KeyV", { ctrl: true, shift: true }))).toBe(false);
    expect(isPasteChord(key("KeyC", { ctrl: true, shift: true }))).toBe(false);
  });

  it("does NOT match when Meta or Alt is also held", () => {
    expect(isCopyChord(key("KeyC", { ctrl: true, shift: true, meta: true }))).toBe(false);
    expect(isCopyChord(key("KeyC", { ctrl: true, shift: true, alt: true }))).toBe(false);
    expect(isPasteChord(key("KeyV", { ctrl: true, shift: true, meta: true }))).toBe(false);
  });
});

describe("copySelection", () => {
  let writeText: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("writes the selection to the clipboard and reports handled", async () => {
    const term = fakeTerm({ selection: "copied-text-9f1" });
    const handled = await copySelection(term);
    expect(handled).toBe(true);
    expect(writeText).toHaveBeenCalledExactlyOnceWith("copied-text-9f1");
  });

  it("is a NO-OP with no selection (does not clobber the clipboard)", async () => {
    const term = fakeTerm({ selection: "" });
    const handled = await copySelection(term);
    expect(handled).toBe(false);
    expect(writeText).not.toHaveBeenCalled();
  });

  it("falls back to execCommand('copy') when writeText is unavailable", async () => {
    vi.stubGlobal("navigator", { clipboard: {} });
    const exec = vi.fn().mockReturnValue(true);
    // jsdom lacks execCommand; install a spy.
    Object.defineProperty(document, "execCommand", { value: exec, configurable: true });

    const term = fakeTerm({ selection: "fallback-text" });
    const handled = await copySelection(term);
    expect(handled).toBe(true);
    expect(exec).toHaveBeenCalledWith("copy");
  });

  it("swallows a writeText rejection without throwing (no leak)", async () => {
    writeText.mockRejectedValue(new Error("denied"));
    const exec = vi.fn().mockReturnValue(true);
    Object.defineProperty(document, "execCommand", { value: exec, configurable: true });

    const term = fakeTerm({ selection: "secret" });
    // Must resolve, never reject.
    const handled = await copySelection(term);
    expect(handled).toBe(true);
    // Falls back to execCommand after the async write was denied.
    expect(exec).toHaveBeenCalledWith("copy");
  });
});

describe("pasteFromClipboard", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("reads the clipboard and pastes it into the terminal", async () => {
    const readText = vi.fn().mockResolvedValue("pasted-bytes-42");
    vi.stubGlobal("navigator", { clipboard: { readText } });

    const term = fakeTerm();
    const handled = await pasteFromClipboard(term);
    expect(handled).toBe(true);
    expect(term.paste).toHaveBeenCalledExactlyOnceWith("pasted-bytes-42");
  });

  it("does NOT paste empty clipboard contents", async () => {
    const readText = vi.fn().mockResolvedValue("");
    vi.stubGlobal("navigator", { clipboard: { readText } });

    const term = fakeTerm();
    const handled = await pasteFromClipboard(term);
    expect(handled).toBe(false);
    expect(term.paste).not.toHaveBeenCalled();
  });

  it("returns false (no paste, no throw) when readText is unavailable", async () => {
    vi.stubGlobal("navigator", { clipboard: {} });
    const term = fakeTerm();
    const handled = await pasteFromClipboard(term);
    expect(handled).toBe(false);
    expect(term.paste).not.toHaveBeenCalled();
  });

  it("swallows a readText rejection without throwing", async () => {
    const readText = vi.fn().mockRejectedValue(new Error("denied"));
    vi.stubGlobal("navigator", { clipboard: { readText } });

    const term = fakeTerm();
    const handled = await pasteFromClipboard(term);
    expect(handled).toBe(false);
    expect(term.paste).not.toHaveBeenCalled();
  });
});
