import { describe, expect, it } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import { namedViewportWidth, THUMB_PAGE_WIDTH, VIEWPORT_PRESETS } from "./viewports";

/**
 * THE ARRIMAGE of the NAMED VIEWPORT WIDTHS (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 7).
 *
 * These three widths exist twice: `nyx_core::render::viewport::NAMED_VIEWPORTS` serves them to the
 * AGENT (`design_preview` resolves `viewport: "tablet"` to a width and then reasons about which
 * breakpoints are in force at it) and the prototype page offers them to the HUMAN as its stage
 * presets. Nothing joined the two — move `tablet` to 834 in Rust and the agent reasons at 834 while
 * the human validates at 768, with no test anywhere disagreeing about it.
 *
 * BOTH DIRECTIONS, and in ORDER: a width changed in the fixture fails here AND in
 * `render/viewport.rs::tests::the_shared_fixture_pins_the_named_viewport_widths`; a width changed
 * here alone fails here.
 */
describe("the shared Rust ⟷ TS fixture — the named viewport widths", () => {
  const fixture = designFixtures.viewports;

  it("offers exactly the widths the fixture pins, in the same order", () => {
    expect(VIEWPORT_PRESETS.map((p) => ({ name: p.name, width: p.width }))).toEqual(fixture.named);
  });

  it("resolves each name to its pinned width, and knows nothing else", () => {
    for (const preset of fixture.named) {
      expect(namedViewportWidth(preset.name)).toBe(preset.width);
      // The agent's own resolution trims and lower-cases; a stage preset must not be stricter.
      expect(namedViewportWidth(` ${preset.name.toUpperCase()} `)).toBe(preset.width);
    }
    expect(namedViewportWidth("phablet")).toBeNull();
  });

  /**
   * The thumbnail is laid out at the `desktop` preset, not at a fourth number of its own: a
   * reviewer's vignette and their `1280` stage must show the same reflow (finding
   * `01KZ3JPW0RFM7AZ0AG2YASKBNR` for why a thumbnail needs a nominal page box at all).
   */
  it("lays a page thumbnail out at the pinned desktop width", () => {
    expect(THUMB_PAGE_WIDTH).toBe(fixture.thumbnail_width);
    expect(THUMB_PAGE_WIDTH).toBe(namedViewportWidth("desktop"));
  });
});
