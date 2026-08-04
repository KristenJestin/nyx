import { describe, expect, it } from "vitest";

import type { BridgeError } from "./contract";
import { errorText, UNREADABLE_ERROR } from "./error-text";

/**
 * THE ONE READING OF A FAILURE (finding `01KYVT6HVA260DTAEREQA4DFJN`).
 *
 * The bridge rejects with a serialized `BridgeError` OBJECT. Everything this function must do
 * follows from that one fact, and from what it costs when it is forgotten: the backend's refusals
 * are the messages that matter most (a removal verdict, a pinned component, a saturated budget, a
 * template carrying logic, the nineteen refusals of a CSS slot), and `[object Object]` is what the
 * user got instead of any of them.
 */

/** A REAL refusal as the bridge rejects with it — the shape, not an `Error` wearing a message. */
const REFUSAL: BridgeError = {
  kind: "command",
  message:
    "variant var-home is part of the FROZEN design (round v1 is locked): open a NEW round to iterate",
  source: "proto_variant_set_node_params",
};

describe("errorText", () => {
  /** THE BITE. `String(REFUSAL)` is `[object Object]`; the sentence is what has to come out. */
  it("reads the bridge's refusal, verbatim, where String() gave [object Object]", () => {
    expect(String(REFUSAL)).toBe("[object Object]");
    expect(errorText(REFUSAL)).toBe(REFUSAL.message);
    expect(errorText(REFUSAL)).not.toContain("[object Object]");
  });

  /** A `BridgeError` is NOT an `Error` — the fact the second occurrence of this defect tripped on. */
  it("does not depend on the value being an Error", () => {
    expect(REFUSAL instanceof Error).toBe(false);
    expect(errorText(REFUSAL, "fallback")).toBe(REFUSAL.message);
  });

  it("reads an Error, a string and a plain object carrying a message", () => {
    expect(errorText(new Error("boom"))).toBe("boom");
    expect(errorText("a raw string refusal")).toBe("a raw string refusal");
    expect(errorText({ message: "structured-cloned across a boundary" })).toBe(
      "structured-cloned across a boundary",
    );
  });

  /**
   * NOTHING READABLE → the CALLER'S sentence, which names the gesture, and never `[object Object]`
   * nor an empty string: an error that renders as nothing is a refusal swallowed on screen.
   */
  it("falls back to the caller's wording, never to a stringified object", () => {
    expect(errorText({ code: 7 }, "Could not rename the project.")).toBe(
      "Could not rename the project.",
    );
    expect(errorText({ code: 7 })).toBe(UNREADABLE_ERROR);
    expect(errorText(undefined)).toBe(UNREADABLE_ERROR);
    expect(errorText(null)).toBe(UNREADABLE_ERROR);
    expect(errorText([1, 2])).toBe(UNREADABLE_ERROR);
  });

  /** A refusal with a BLANK message is as empty as no message at all — the fallback speaks. */
  it("treats a blank message as no message", () => {
    expect(errorText({ kind: "command", message: "   " }, "Operation failed")).toBe(
      "Operation failed",
    );
    expect(errorText(new Error(""), "Operation failed")).toBe("Operation failed");
  });

  /** A caller that WANTS silence for an empty failure can ask for it explicitly. */
  it("honours an empty fallback", () => {
    expect(errorText({ nothing: true }, "")).toBe("");
  });
});
