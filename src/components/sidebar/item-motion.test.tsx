import { describe, expect, it } from "vitest";

import { itemTransition, itemVariants } from "./item-motion";

describe("itemTransition (reduced-motion aware)", () => {
  it("returns a spring transition when motion is allowed", () => {
    const t = itemTransition(false) as Record<string, unknown>;
    expect(t.type).toBe("spring");
    expect(t.duration).toBeUndefined();
  });

  it("returns an instant (duration 0) transition when reduced motion is set", () => {
    const t = itemTransition(true) as Record<string, unknown>;
    expect(t.duration).toBe(0);
    expect(t.type).toBeUndefined();
  });

  it("treats null (unknown preference) as motion allowed", () => {
    const t = itemTransition(null) as Record<string, unknown>;
    expect(t.type).toBe("spring");
  });
});

describe("itemVariants (enter fade)", () => {
  it("is an opacity-only fade-in; positional motion is owned by Reorder", () => {
    expect(itemVariants.initial).toMatchObject({ opacity: 0 });
    expect(itemVariants.animate).toMatchObject({ opacity: 1 });
    // No height (Reorder slides the rows via `layout`) and no exit (a removed
    // row just unmounts; its neighbours slide up via `layout`).
    expect(itemVariants.initial).not.toHaveProperty("height");
    expect(itemVariants).not.toHaveProperty("exit");
  });
});
