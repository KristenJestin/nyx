import { afterEach, expect, it, vi } from "vitest";

import { buildFrameSrcdoc } from "./frame-srcdoc";
import { PROTO_SHIM_SOURCE } from "./proto-shim";
import { TAILWIND_BROWSER_SOURCE } from "./tailwind-runtime";
import { buildThemeStyles } from "./theme-css";

/**
 * BROWSER-MODE proof of the review-#42 done-criterion: "l'aperçu iframe charge le stylesheet de
 * tokens (`bg-primary` rend la vraie couleur, pas gris)".
 *
 * Only a real engine can settle this. `bg-primary` is a POINTER — it compiles to
 * `background-color: var(--color-primary)` and emits NOTHING when the token is missing — so the
 * question "is the preview grey?" is literally "what does Chromium compute for that element?".
 * This spec builds the EXACT srcdoc `CompositionFrame` builds (same builder, same LOCAL inlined
 * Tailwind engine, same strict CSP, zero network) and reads the computed colour back:
 *
 *   * WITH the project's ingested token layer → the app's real colour;
 *   * WITHOUT it → transparent, i.e. the grey rendu this finding is about.
 *
 * The frame is mounted WITHOUT the `sandbox` attribute here — and only here — so the spec can read
 * `contentDocument`. The document's own CSP (`default-src 'none'`, nonce'd scripts) is unchanged and
 * still applies, so what is measured is the real styling pipeline.
 */

/** The exact layer `nyx_core::design_tokens` ingests from a shadcn/Tailwind-v4 `globals.css`. */
const INGESTED_LAYER = JSON.stringify({
  sources: [{ repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 3 }],
  variables: {
    // The RAW app identity…
    "--primary": "rgb(255, 0, 85)",
    "--primary-foreground": "rgb(255, 255, 255)",
    // …and the app's own `@theme inline` alias onto it (the hard case: a bare `@theme` entry that
    // is USELESS unless the raw `:root` declaration is loaded too).
    "--color-primary": "var(--primary)",
  },
  dark: {},
});

const MARKUP = `<div id="probe" class="bg-primary p-6">Primary</div>
<div id="control" class="p-6">Untouched</div>`;

function srcdocFor(themeTokensJson: string | null): string {
  const theme = buildThemeStyles(themeTokensJson);
  return buildFrameSrcdoc({
    markup: MARKUP,
    tailwindSource: TAILWIND_BROWSER_SOURCE,
    shimSource: PROTO_SHIM_SOURCE,
    themeCss: theme.theme,
    rootCss: theme.root,
    nonce: "NONCEFORTEST",
    states: [],
  });
}

/** Mount a srcdoc frame and resolve once its document is parsed. */
async function mount(srcdoc: string): Promise<HTMLIFrameElement> {
  const iframe = document.createElement("iframe");
  iframe.style.width = "480px";
  iframe.style.height = "240px";
  iframe.srcdoc = srcdoc;
  document.body.appendChild(iframe);
  await vi.waitFor(
    () => {
      expect(iframe.contentDocument?.getElementById("probe")).toBeTruthy();
    },
    { timeout: 8000, interval: 50 },
  );
  return iframe;
}

/** The computed background colour of `#probe` inside a mounted frame. */
function probeBackground(iframe: HTMLIFrameElement): string {
  const doc = iframe.contentDocument;
  const probe = doc?.getElementById("probe");
  if (!doc || !probe) return "";
  return iframe.contentWindow?.getComputedStyle(probe).backgroundColor ?? "";
}

afterEach(() => {
  document.body.replaceChildren();
});

it("renders bg-primary in the project's real colour once the token stylesheet is loaded", async () => {
  const iframe = await mount(srcdocFor(INGESTED_LAYER));

  // The LOCAL engine generates the utility, and it resolves through the `:root` declaration the
  // `@theme` alias references — the whole chain, in a real browser.
  await vi.waitFor(
    () => {
      expect(probeBackground(iframe)).toBe("rgb(255, 0, 85)");
    },
    { timeout: 10000, interval: 100 },
  );

  // The chain really is REFERENCED (not a copy inlined by the builder): the raw declaration is what
  // the frame carries, and the utility points at it. (The "zero network" guarantee of the document
  // itself is pinned by `frame-srcdoc.test.ts`, over a controlled engine source.)
  const html = srcdocFor(INGESTED_LAYER);
  expect(html).toContain("--primary: rgb(255, 0, 85);");
  expect(html).toContain("--color-primary: var(--primary);");

  // The tokens travel as INLINE CSS of the srcdoc — no stylesheet is ever fetched.
  expect(html).not.toContain('<link rel="stylesheet"');
});

it("is GREY (transparent) without the token layer — the regression this finding is about", async () => {
  const iframe = await mount(srcdocFor(null));

  // Give the engine the same budget it got above, then assert it produced NO background: an
  // unknown `bg-primary` emits nothing at all, which is exactly the grey rendu users reported.
  await new Promise((resolve) => setTimeout(resolve, 1500));
  const background = probeBackground(iframe);
  expect(background === "rgba(0, 0, 0, 0)" || background === "transparent").toBe(true);
});
