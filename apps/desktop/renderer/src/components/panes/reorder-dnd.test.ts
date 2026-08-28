import {
  KeyboardSensor,
  PointerSensor,
  type KeyboardSensorOptions,
  type PointerSensorOptions,
} from "@dnd-kit/dom";
import { describe, expect, it } from "vitest";

import { reorderSensors, sameOrder } from "./reorder-dnd";

/**
 * The shared reorder sensor config (v2.5.6 finding #23 — "le drag ne démarre jamais"). The bug:
 * both reorder surfaces cover their whole `useSortable` element with a single `<button>` (the
 * terminal row's select button, the project tile's enter button), and dnd-kit's DEFAULT
 * `preventActivation` cancels any drag whose `pointerdown` lands on an interactive element that is
 * neither `source.element` nor `source.handle` — so the sensor never activated on the covering
 * button and the reorder was completely dead.
 *
 * The real pointer gesture is undrivable in the unit (jsdom has no layout / the WebDriver pointer
 * API is unusable in the Electron harness), but the ROOT CAUSE is a pure predicate: whether the
 * sensor's `preventActivation` blocks a press on that covering button. We lock in that our config
 * supplies a `preventActivation` that does NOT block that exact scenario — and, as a contrast,
 * that dnd-kit's default WOULD have blocked it (proving the override is what unblocks the drag).
 */

type PreventActivation = NonNullable<PointerSensorOptions["preventActivation"]>;

/** The configured options of the shared sensor set's PointerSensor descriptor (not the keyboard). */
function pointerOptions(): PointerSensorOptions {
  const descriptor = reorderSensors.find(
    (s) =>
      typeof s === "object" &&
      s != null &&
      "plugin" in s &&
      (s as { plugin: unknown }).plugin === PointerSensor,
  );
  if (!descriptor) throw new Error("reorderSensors must include a configured PointerSensor");
  return (descriptor as { options?: PointerSensorOptions }).options ?? {};
}

/** Same reading for the KEYBOARD descriptor — a BARE `KeyboardSensor` has no options at all. */
function keyboardOptions(): KeyboardSensorOptions {
  const descriptor = reorderSensors.find(
    (s) =>
      typeof s === "object" &&
      s != null &&
      "plugin" in s &&
      (s as { plugin: unknown }).plugin === KeyboardSensor,
  );
  if (!descriptor) throw new Error("reorderSensors must include a configured KeyboardSensor");
  return (descriptor as { options?: KeyboardSensorOptions }).options ?? {};
}

type KeyboardPreventActivation = NonNullable<KeyboardSensorOptions["preventActivation"]>;

/**
 * The bug scenario as dnd-kit sees it: a `pointerdown` whose `target` is the `<button>` that COVERS
 * the sortable element (`source.element`), with no dedicated drag handle. Real DOM nodes so the
 * default predicate's `isElement` / `getInteractiveElement(target)` resolve like they do in the app.
 */
function coveredButtonGesture(): { event: PointerEvent; source: Parameters<PreventActivation>[1] } {
  const element = document.createElement("div"); // the useSortable element (source.element)
  const button = document.createElement("button"); // the covering select/enter button
  element.appendChild(button);
  const event = { target: button, pointerType: "mouse" } as unknown as PointerEvent;
  const source = { element, handle: undefined } as unknown as Parameters<PreventActivation>[1];
  return { event, source };
}

describe("reorderSensors — preventActivation override (finding #23)", () => {
  it("configures a preventActivation predicate on the PointerSensor", () => {
    // A regression that drops the override leaves `preventActivation` undefined here — the drag
    // would silently fall back to the blocking default again.
    expect(typeof pointerOptions().preventActivation).toBe("function");
  });

  it("does NOT block a drag starting on the button that covers the sortable element", () => {
    const preventActivation = pointerOptions().preventActivation as PreventActivation;
    const { event, source } = coveredButtonGesture();
    // The whole point of the fix: a press on the covering button must be allowed to activate.
    expect(preventActivation(event, source)).toBe(false);
  });

  it("is the OVERRIDE that unblocks it — dnd-kit's default WOULD have blocked the same gesture", () => {
    const { event, source } = coveredButtonGesture();
    const defaultPredicate = PointerSensor.defaults.preventActivation as PreventActivation;
    expect(typeof defaultPredicate).toBe("function");
    // Proves the covered-button case is exactly what the default cancels (the observed bug), so the
    // override is load-bearing, not cosmetic.
    expect(defaultPredicate(event, source)).toBe(true);
  });
});

/**
 * THE SAME OVERRIDE ON THE KEYBOARD SENSOR — the half that was missing.
 *
 * The pointer half above was found and fixed in v2.5.6; the keyboard sensor was added BARE next to
 * it and kept dnd-kit's default guard, which is the identical predicate: the keydown must land on
 * `source.handle ?? source.element`. A terminal row has no handle and its sortable element is the
 * row wrapper, which is not focusable — the only thing a user can Tab to is the covering `<button>`
 * — so Space never started a drag and three of the four gestures were unreachable without a mouse.
 * (A group's block escaped it: its grip IS the handle AND the focusable element, which is why the
 * one keyboard gesture that worked was "move the whole block".) A real Chromium spec drives the
 * gesture end to end; this locks the predicate itself, next to its pointer twin.
 */
describe("reorderSensors — the KeyboardSensor carries the same override", () => {
  it("configures a preventActivation predicate on the KeyboardSensor", () => {
    expect(typeof keyboardOptions().preventActivation).toBe("function");
  });

  it("does NOT block a Space pressed on the button that covers the sortable element", () => {
    const preventActivation = keyboardOptions().preventActivation as KeyboardPreventActivation;
    const { event, source } = coveredButtonGesture();
    expect(preventActivation(event as unknown as KeyboardEvent, source)).toBe(false);
  });

  it("is the OVERRIDE that unblocks it — dnd-kit's default WOULD have blocked the same gesture", () => {
    const { event, source } = coveredButtonGesture();
    const defaultPredicate = KeyboardSensor.defaults.preventActivation as KeyboardPreventActivation;
    expect(typeof defaultPredicate).toBe("function");
    expect(defaultPredicate(event as unknown as KeyboardEvent, source)).toBe(true);
  });
});

describe("sameOrder", () => {
  it("is true only for same-length, same-order sequences", () => {
    expect(sameOrder(["a", "b", "c"], ["a", "b", "c"])).toBe(true);
    expect(sameOrder(["a", "b"], ["b", "a"])).toBe(false);
    expect(sameOrder(["a", "b"], ["a", "b", "c"])).toBe(false);
  });
});
