import { InputHandler } from "@wterm/dom";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

/**
 * REGRESSION — Bug B, review 01KWY0KXSF0X8E6JDH2QVXTRRQ, task 01KWY0KY9BDFMGPZGX10T5MXFN.
 *
 * `@wterm/dom`'s `InputHandler` has TWO paths to `onData`: `handleKeyDown` (which
 * `preventDefault()`s then emits the key sequence) and `handleInput` (which emits
 * `textarea.value`). Normally `preventDefault()` on keydown suppresses the browser
 * `input` event, so only one fires. But fcitx5 (the user's IME) injects an `input`
 * event ANYWAY after the keydown, so each ASCII keystroke was emitted TWICE
 * ("lslsls...", "mmmm"). Our `bun patch` on `@wterm/dom` (`patches/@wterm%2Fdom@0.3.0.patch`)
 * dedups exactly that case WITHOUT touching the composition path.
 *
 * The real fcitx duplication is not reproducible with synthetic events (that final
 * check is manual — see the task), but the patched handler's LOGIC is unit-testable:
 * we drive the two event paths directly and assert the guard's contract.
 *
 * jsdom does NOT auto-generate `input` events from a dispatched `keydown` (a real
 * browser would); this lets us reproduce the fcitx5 shape precisely — a keydown
 * FOLLOWED BY a separately-injected `input` for the SAME character — and assert a
 * single emission.
 */
describe("InputHandler fcitx5 double-emit guard (patched @wterm/dom)", () => {
  let element: HTMLElement;
  let handler: InputHandler;
  let emitted: string[];

  // The hidden textarea the InputHandler creates as a child of `element`.
  function textarea(): HTMLTextAreaElement {
    const ta = element.querySelector("textarea");
    if (!ta) throw new Error("InputHandler did not create its textarea");
    return ta as HTMLTextAreaElement;
  }

  /** Dispatch a keydown for a plain printable key on the textarea. */
  function keydown(key: string): void {
    textarea().dispatchEvent(
      new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
    );
  }

  /**
   * Simulate the browser/IME committing `value` into the textarea and firing an
   * `input` event — the extra event fcitx5 injects after a preventDefault()'d keydown.
   */
  function inputEvent(value: string): void {
    textarea().value = value;
    textarea().dispatchEvent(new Event("input", { bubbles: true }));
  }

  beforeEach(() => {
    element = document.createElement("div");
    document.body.appendChild(element);
    emitted = [];
    handler = new InputHandler(
      element,
      (data) => emitted.push(data),
      () => null,
    );
  });

  afterEach(() => {
    handler.destroy();
    element.remove();
  });

  it("emits a plain ASCII keystroke ONCE even when an input event follows the keydown (fcitx5 shape)", () => {
    // keydown emits the char; fcitx5 then injects an `input` for the SAME char.
    keydown("l");
    inputEvent("l");
    expect(emitted, "the duplicated fcitx5 input must be dropped").toEqual(["l"]);
    // And the textarea was drained so it cannot leak on a later input.
    expect(textarea().value).toBe("");
  });

  it("emits each of a run of ASCII keystrokes exactly once (no 'lslsls' duplication)", () => {
    for (const ch of ["l", "s", "m"]) {
      keydown(ch);
      inputEvent(ch); // fcitx5's extra input per keystroke
    }
    expect(emitted).toEqual(["l", "s", "m"]);
  });

  it("still delivers an `input` event that has NO preceding matching keydown (paste-like insert)", () => {
    // No keydown armed the guard → a standalone input must go through.
    inputEvent("hello");
    expect(emitted).toEqual(["hello"]);
  });

  it("delivers an `input` whose value DIFFERS from what keydown emitted (guard is value-matched)", () => {
    keydown("a");
    // A different value arriving as input is a genuine insert, not the keydown's echo.
    inputEvent("b");
    // keydown emitted "a"; the differing input "b" is still delivered.
    expect(emitted).toEqual(["a", "b"]);
  });

  it("does NOT dedup control/navigation keys (they emit no matching input)", () => {
    keydown("Enter"); // → "\r", never a matching input
    // An input event with the Enter sequence would never be produced by the browser,
    // but even a stray printable input right after Enter must not be swallowed.
    inputEvent("x");
    expect(emitted).toEqual(["\r", "x"]);
  });

  it("leaves the composition (IME compose) path intact — compositionend still emits", () => {
    const ta = textarea();
    // Composition start: the guard must never engage while composing.
    ta.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    // During composition the browser fires `input` events; the handler ignores them.
    inputEvent("n"); // an intermediate composition input — must be dropped by the composing guard
    expect(emitted, "intermediate composition input is not emitted").toEqual([]);
    // Composition end commits the composed text via its own path.
    ta.dispatchEvent(new CompositionEvent("compositionend", { data: "你", bubbles: true }));
    expect(emitted, "composed text is emitted once via compositionend").toEqual(["你"]);
  });

  it("a plain keystroke AFTER a composition is not wrongly deduped (turn-scoped guard)", () => {
    const ta = textarea();
    ta.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    ta.dispatchEvent(new CompositionEvent("compositionend", { data: "好", bubbles: true }));
    expect(emitted).toEqual(["好"]);
    // Now a normal ASCII key with its fcitx5 duplicate input: still deduped to one.
    keydown("z");
    inputEvent("z");
    expect(emitted).toEqual(["好", "z"]);
  });
});
