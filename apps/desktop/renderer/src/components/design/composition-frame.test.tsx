import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CompositionFrame } from "./composition-frame";

/**
 * The TOKEN WIRING of `CompositionFrame` (review #42, finding `01KYJ7AP3A1B7JJZZVSMFSEJFP`).
 *
 * `theme-tokens.browser.test.ts` proves what a token layer DOES in a real engine (`bg-primary`
 * renders the app's colour instead of grey) — but it assembles the srcdoc itself, so the only link
 * that was covered nowhere is the one this file pins: the COMPONENT actually deriving BOTH halves of
 * the theme from its `themeTokensJson` prop and handing them to the builder. Dropping
 * `rootCss: theme.root` (or the prop at a call site) turns every preview grey again; jsdom cannot
 * run the frame's scripts, but it CAN read the document the component built, which is enough to make
 * that regression impossible to land unnoticed.
 */

/** The exact layer `nyx_core::design_tokens` ingests from a shadcn/Tailwind-v4 `globals.css`. */
const INGESTED_LAYER = JSON.stringify({
  sources: [{ repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 3 }],
  variables: {
    // The RAW app identity (belongs on `:root` — the `rootCss` half)…
    "--primary": "rgb(255, 0, 85)",
    // …and the app's own `@theme inline` alias onto it (the `themeCss` half), useless alone.
    "--color-primary": "var(--primary)",
  },
  dark: { "--primary": "rgb(20, 0, 10)" },
});

/** Mount the component and hand back the document it built for its iframe. */
function mountedSrcdoc(themeTokensJson?: string | null): string {
  const { container } = render(
    <CompositionFrame
      markup={`<div class="bg-primary">Primary</div>`}
      themeTokensJson={themeTokensJson}
      title="preview"
    />,
  );
  const iframe = container.querySelector("iframe");
  expect(iframe).not.toBeNull();
  return iframe?.getAttribute("srcdoc") ?? "";
}

describe("CompositionFrame theme wiring", () => {
  it("hands BOTH halves of the ingested layer to the frame document", () => {
    const srcdoc = mountedSrcdoc(INGESTED_LAYER);

    // The RAW `:root` stylesheet — this is `rootCss: theme.root`, the half whose absence renders
    // every `bg-primary` grey (the `@theme` alias below would point at an undefined variable).
    expect(srcdoc).toContain(":root {\n  --primary: rgb(255, 0, 85);\n}");
    // The `@theme` body that DEFINES the utility, referencing (never copying) the raw declaration.
    expect(srcdoc).toContain("--color-primary: var(--primary);");
    // The dark overrides travel as their own scope, and the tokens are INLINE (zero network).
    expect(srcdoc).toContain(".dark {\n  --primary: rgb(20, 0, 10);\n}");
    expect(srcdoc).not.toContain('<link rel="stylesheet"');
  });

  it("emits no theme at all when the project has no token layer", () => {
    for (const empty of [null, undefined, "", "not json"]) {
      const srcdoc = mountedSrcdoc(empty);
      expect(srcdoc).not.toContain("--primary");
      expect(srcdoc).not.toContain(":root {");
    }
  });

  it("re-derives the document when the project's tokens change", () => {
    const before = mountedSrcdoc(INGESTED_LAYER);
    const after = mountedSrcdoc(
      JSON.stringify({ variables: { "--primary": "rgb(0, 128, 0)" }, dark: {} }),
    );
    expect(before).toContain("--primary: rgb(255, 0, 85);");
    expect(after).toContain("--primary: rgb(0, 128, 0);");
  });
});
