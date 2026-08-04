/**
 * THE NAMED VIEWPORT WIDTHS — the three the human vocabulary uses, stated ONCE for this language
 * (finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 7).
 *
 * # Why they are a contract and not a UI detail
 *
 * The very same three widths are served to the AGENT by `nyx_core::render::viewport`
 * (`NAMED_VIEWPORTS`): `design_preview` resolves `viewport: "tablet"` to a width and then reasons
 * about which Tailwind breakpoints are in force at it — `md:` applies at 768 and does not at 767.
 * The renderer offers the same widths to the HUMAN as the stage presets of the prototype page, and
 * the two lists were joined by NOTHING: a bare list of numbers here, a Rust constant there. Move
 * `tablet` to 834 on one side and the agent reasons at 834 while the human validates at 768, each
 * convinced they looked at the same thing.
 *
 * `contract/design-fixtures.json` (`viewports`) is what holds the two together now, in both
 * directions: `render/viewport.rs::tests::the_shared_fixture_pins_the_named_viewport_widths` and
 * `viewports.test.ts`.
 *
 * # The width is EXACT
 *
 * The stage lays the page out at exactly this many CSS px (finding `01KZ3JPWNYP0B65Z31RRX1EEMT`):
 * `390` used to lay out at 388 because the surface sized `border-box` and the two gutter marks ate
 * the preset from the inside — and a media query at 390 px does not fire at 388, which is the whole
 * reason the preset exists.
 */

/** ONE named width: the word the agent's vocabulary uses, and the CSS px it resolves to. */
export interface ViewportPreset {
  /** The name (`mobile`), lower-case — the word `design_preview` accepts. */
  readonly name: string;
  /** The width in CSS px, applied EXACTLY by the stage. */
  readonly width: number;
}

/**
 * The three named widths, in the order they are offered. Ascending, and the order is part of the
 * contract: it is the order the presets sit in the stage's segmented control and the order the
 * agent's manual names them in.
 */
export const VIEWPORT_PRESETS: readonly ViewportPreset[] = [
  { name: "mobile", width: 390 },
  { name: "tablet", width: 768 },
  { name: "desktop", width: 1280 },
];

/** The width a NAMED viewport resolves to, `null` for a name nyx does not know. */
export function namedViewportWidth(name: string): number | null {
  const lowered = name.trim().toLowerCase();
  return VIEWPORT_PRESETS.find((p) => p.name === lowered)?.width ?? null;
}

/**
 * The width a page THUMBNAIL is laid out at before being scaled down — the `desktop` preset.
 *
 * A miniature is a REDUCTION of the whole page, so the page has to be laid out at a plausible
 * desktop viewport first: rendering it into the 40 × 32 px box itself produced two near-identical
 * crops of a top-left corner, in which the very thing each page is about sat outside the frame
 * (finding `01KZ3JPW0RFM7AZ0AG2YASKBNR`). It is the desktop preset rather than a fourth number, so
 * a reviewer's vignette and their `desktop` stage show the same reflow.
 */
export const THUMB_PAGE_WIDTH = namedViewportWidth("desktop") ?? 1280;
