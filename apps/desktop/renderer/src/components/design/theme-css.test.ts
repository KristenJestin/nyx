import { describe, expect, test } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import {
  buildThemeCss,
  buildThemeStyles,
  parseTokenLayer,
  resolveTokenColor,
  resolveTokenValue,
  RESERVED_KEYS,
  themeHasDark,
} from "./theme-css";

/**
 * The THEME derivation (review #42 — "l'aperçu rend gris"). What these pin is the CAUSAL chain:
 * an ingested token layer must produce BOTH the `@theme` entries that make `bg-primary` a real
 * utility AND the `:root` declaration that utility points at — REFERENCED with `var(--primary)`,
 * never copied — so one edit re-themes everything. The end-to-end proof (a real Chromium computing
 * the background colour) lives in `theme-tokens.browser.test.tsx`.
 */

/** The layer `nyx_core::design_tokens` stores for a shadcn/Tailwind-v4 front. */
const INGESTED = JSON.stringify({
  source: { repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 6 },
  sources: [{ repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 6 }],
  variables: {
    "--primary": "oklch(0.55 0.21 264)",
    "--primary-foreground": "oklch(0.985 0 0)",
    "--background": "oklch(1 0 0)",
    "--radius": "0.625rem",
    "--font-sans": '"Inter", ui-sans-serif',
    "--color-primary": "var(--primary)",
  },
  dark: { "--primary": "oklch(0.72 0.16 264)" },
});

describe("buildThemeStyles", () => {
  test("emits the RAW app variables on :root and the utility entries in @theme", () => {
    const { theme, root } = buildThemeStyles(INGESTED);

    // The raw identity lives ONCE, on `:root`.
    expect(root).toContain(":root {");
    expect(root).toContain("--primary: oklch(0.55 0.21 264);");
    expect(root).toContain("--background: oklch(1 0 0);");

    // A name already in a Tailwind namespace IS a theme entry (never duplicated onto :root).
    expect(theme).toContain("--radius: 0.625rem;");
    expect(theme).toContain('--font-sans: "Inter", ui-sans-serif;');
    expect(root).not.toContain("--radius:");
    expect(root).not.toContain("--font-sans:");
  });

  test("aliases a raw colour into the utility namespace BY REFERENCE, never by copy", () => {
    const { theme } = buildThemeStyles(INGESTED);
    // `bg-background` exists, and points BACK at the single `:root` declaration.
    expect(theme).toContain("--color-background: var(--background);");
    // The value itself is NOT duplicated into the theme block.
    expect(theme).not.toContain("--color-background: oklch(1 0 0);");
    // The app's OWN alias wins — it is never emitted twice.
    expect(theme.match(/--color-primary:/g)).toHaveLength(1);
    expect(theme).toContain("--color-primary: var(--primary);");
  });

  test("a non-colour raw variable gets no colour alias", () => {
    const { theme, root } = buildThemeStyles(
      JSON.stringify({ variables: { "--gutter": "12px", "--brand": "#ff0055" } }),
    );
    expect(root).toContain("--gutter: 12px;");
    expect(theme).not.toContain("--color-gutter");
    expect(theme).toContain("--color-brand: var(--brand);");
  });

  test("dark overrides re-declare the RAW variable under .dark (the aliases follow)", () => {
    const { theme, root } = buildThemeStyles(INGESTED);
    expect(root).toContain(".dark {");
    expect(root).toContain("--primary: oklch(0.72 0.16 264);");
    // Nothing dark-specific leaks into @theme — the utilities already read `var(--primary)`.
    expect(theme).not.toContain("0.72 0.16 264");
  });

  test("says WHETHER the design system has a dark variant at all", () => {
    // The layer declares dark overrides → the frame may select them (finding
    // `01KYSVEZMJP1Z8V7SYJ8YC5H9W`).
    expect(buildThemeStyles(INGESTED).hasDark).toBe(true);
    expect(themeHasDark(INGESTED)).toBe(true);

    // A LIGHT-ONLY layer: there is no dark variant to select, and saying otherwise would be a lie
    // with no rendering behind it. `dark: {}` is the same answer as no `dark` key at all — an empty
    // map declares nothing, so no `.dark` block is emitted and none may be selected.
    const lightOnly = JSON.stringify({ variables: { "--background": "oklch(1 0 0)" } });
    const emptyDark = JSON.stringify({ variables: { "--background": "oklch(1 0 0)" }, dark: {} });
    for (const layer of [lightOnly, emptyDark, null, "", "{not json"]) {
      expect(buildThemeStyles(layer).hasDark).toBe(false);
      expect(buildThemeStyles(layer).root).not.toContain(".dark {");
      expect(themeHasDark(layer)).toBe(false);
    }
  });

  test("source metadata never emits CSS", () => {
    const { theme, root } = buildThemeStyles(INGESTED);
    for (const body of [theme, root]) {
      expect(body).not.toContain("src/styles.css");
      expect(body).not.toContain("repository_id");
      expect(body).not.toContain("variable-count");
    }
  });

  test("keeps the legacy shapes working (namespaced maps + the `css` escape hatch)", () => {
    const legacy = buildThemeStyles(
      JSON.stringify({ colors: { primary: "#123456", primaryForeground: "#fff" }, radius: "8px" }),
    );
    expect(legacy.theme).toContain("--color-primary: #123456;");
    expect(legacy.theme).toContain("--color-primary-foreground: #fff;");
    expect(legacy.theme).toContain("--radius: 8px;");
    expect(legacy.root).toBe("");

    const hatch = buildThemeStyles(JSON.stringify({ css: "--color-primary: red;" }));
    expect(hatch.theme).toBe("--color-primary: red;");
  });

  test("a blank / malformed / non-object blob yields empty bodies instead of throwing", () => {
    for (const blob of [null, undefined, "", "   ", "not json", "[1,2]", '"a"']) {
      expect(buildThemeStyles(blob)).toEqual({ theme: "", root: "", hasDark: false });
    }
  });

  test("buildThemeCss stays the @theme body (the historical entry point)", () => {
    expect(buildThemeCss(INGESTED)).toBe(buildThemeStyles(INGESTED).theme);
  });
});

describe("parseTokenLayer", () => {
  test("exposes the source REFERENCE and the name-sorted maps", () => {
    const layer = parseTokenLayer(INGESTED);
    expect(layer.sources).toHaveLength(1);
    expect(layer.sources[0]).toMatchObject({ repo: "web", path: "src/styles.css" });
    expect(layer.variables.map(([n]) => n)).toEqual([
      "--background",
      "--color-primary",
      "--font-sans",
      "--primary",
      "--primary-foreground",
      "--radius",
    ]);
    expect(layer.dark).toEqual([["--primary", "oklch(0.72 0.16 264)"]]);
  });

  test("degrades to an empty layer on anything unreadable", () => {
    for (const blob of [null, "", "not json", "[]", JSON.stringify({ colors: { a: "b" } })]) {
      const layer = parseTokenLayer(blob);
      expect(layer.variables).toEqual([]);
      expect(layer.dark).toEqual([]);
      expect(layer.skipped).toEqual([]);
      expect(layer.prunedDirs).toEqual([]);
      // Nothing was RECORDED — which is not the same claim as "nothing was discarded".
      expect(layer.provenanceReported).toBe(false);
    }
  });

  /**
   * The ingest's PROVENANCE report (finding `01KYSHSPGSPC98C49X68MWY60D`): the sheets the run did NOT
   * merge, with the backend's reason code, and the directories it refused to enter. Read as
   * tolerantly as `sources` — a hand-authored layer carries none of it, and that is not an error.
   */
  test("reads the discarded sheets, their reason and the pruned directories", () => {
    const layer = parseTokenLayer(
      JSON.stringify({
        sources: [{ repo: "web", path: "src/styles.css", variable_count: 3 }],
        skipped: [
          { repo: "web", path: "docs/theme.css", reason: "doc_tool_theme" },
          { repo: "web", path: "x.css" }, // no reason → not a report entry
          "nope",
        ],
        skipped_count: 9,
        pruned_dirs: [".generated", "", 7],
        variables: { "--primary": "#fff" },
      }),
    );
    expect(layer.skipped).toEqual([
      { repo: "web", path: "docs/theme.css", reason: "doc_tool_theme" },
    ]);
    // The TOTAL is the backend's, never the length of the bounded head it sent.
    expect(layer.skippedCount).toBe(9);
    expect(layer.prunedDirs).toEqual([".generated"]);
    expect(layer.provenanceReported).toBe(true);
  });

  test("the provenance keys are METADATA: they never leak into the emitted CSS", () => {
    // `sources` was already reserved; the report keys must be too, or the generic group flattener
    // turns `pruned_dirs` into a `--pruned-dirs-0` custom property inside `@theme`.
    const styles = buildThemeStyles(
      JSON.stringify({
        skipped: [{ repo: "web", path: "docs/theme.css", reason: "doc_tool_theme" }],
        skipped_count: 4,
        pruned_dirs: [".generated"],
        repos_scanned: 2,
        candidates: 7,
        variables: { "--primary": "#fff" },
      }),
    );
    expect(styles.theme).not.toContain("skipped");
    expect(styles.theme).not.toContain("pruned");
    expect(styles.theme).not.toContain("candidates");
    expect(styles.root).not.toContain("skipped");
    expect(styles.root).toContain("--primary: #fff;");
  });

  /**
   * THE CROSS-LANGUAGE GUARD on the token layer (finding `01KYSNQWQD1324E0NA0172M66J`). Rust's
   * `TokenLayer::to_json` documents that every key it emits is reserved here, and the correspondence
   * was checked BY HAND in two unrelated tests — so nothing broke the day a key appeared on one side
   * only, and the symptom would have been a `--pruned-dirs-count` custom property inside `@theme`.
   * The list is now DATA (`contract/design-fixtures.json`): the Rust suite asserts `to_json` emits
   * exactly those keys, this one asserts every one of them is reserved.
   */
  test("every key the Rust layer emits is RESERVED here — the shared fixture says which", () => {
    const keys: string[] = designFixtures.token_layer_keys.keys;
    expect(keys.length).toBeGreaterThanOrEqual(8);
    for (const key of keys) {
      expect(RESERVED_KEYS.has(key), `\`${key}\` is emitted by to_json but not reserved`).toBe(
        true,
      );
    }
  });

  test("the pruned-directory TOTAL travels with its bounded list", () => {
    // A truncation may never become an under-declaration: the head is what fits, the count is what
    // really happened (finding `01KYSNRNY550NS778YCMC8CZGH`).
    const layer = parseTokenLayer(
      JSON.stringify({
        variables: { "--primary": "#fff" },
        skipped: [],
        skipped_count: 0,
        pruned_dirs: [".generated", "dist"],
        pruned_dirs_count: 70,
      }),
    );
    expect(layer.prunedDirs).toEqual([".generated", "dist"]);
    expect(layer.prunedDirsCount).toBe(70);
    // A blob with no usable total (a legacy one, a hand-edited one) falls back to the LENGTH, which
    // is then exact — never to a smaller number.
    const legacy = parseTokenLayer(
      JSON.stringify({ variables: {}, pruned_dirs: [".generated"], pruned_dirs_count: "lots" }),
    );
    expect(legacy.prunedDirsCount).toBe(1);
  });
});

/**
 * THE ALIAS ROWS, AND THE COLOUR THEY REALLY STAND FOR (findings `01KZ3JQY3FK9RRNS15H2BCCSN0` for
 * the swatch, `01KZ3JW5A63TJWX83V8ZKX62CN` for the duplicate row).
 *
 * A shadcn/Tailwind-v4 front declares its raw tokens AND an `@theme inline` alias per token
 * (`--color-secondary: var(--secondary)`). The panel listed both — the same token twice — and painted
 * the alias's chip by handing `var(--secondary)` to the browser, which resolved it in NYX's own
 * document. Two rows of one token, two different colours, and an alias nyx does not carry showed no
 * colour at all. The resolution is the LAYER's, or it does not happen.
 */
describe("token aliases", () => {
  const LAYER = parseTokenLayer(
    JSON.stringify({
      variables: {
        "--secondary": "oklch(0.86 0.2 145)",
        "--color-secondary": "var(--secondary)",
        // A chain, and a self-reference: neither may hang nor lie.
        "--brand": "var(--color-secondary)",
        "--loop": "var(--loop)",
        // A reference OUT of the layer: nyx may well declare `--selection`; this layer does not.
        "--color-selection": "var(--selection)",
        // …and the same, with a fallback the project DOES state.
        "--edge": "var(--nope, #ff0055)",
        "--radius": "0.625rem",
      },
      dark: {},
    }),
  );

  test("names the variables that are the SAME token under a second name", () => {
    expect([...LAYER.aliasOf.entries()].sort()).toEqual([
      ["--brand", "--color-secondary"],
      ["--color-secondary", "--secondary"],
    ]);
    // A self-reference is not an alias of another row, and a reference the layer does not declare
    // is not one either: both stay visible as rows of their own.
    expect(LAYER.aliasOf.has("--loop")).toBe(false);
    expect(LAYER.aliasOf.has("--color-selection")).toBe(false);
  });

  test("resolves a value in the PROJECT's layer, never in the host document", () => {
    expect(resolveTokenValue(LAYER, "var(--secondary)")).toBe("oklch(0.86 0.2 145)");
    // Through a chain of aliases, and through a fallback the layer cannot follow.
    expect(resolveTokenValue(LAYER, "var(--brand)")).toBe("oklch(0.86 0.2 145)");
    expect(resolveTokenValue(LAYER, "var(--nope, #ff0055)")).toBe("#ff0055");
    // Out of the layer with no fallback → NOTHING. This is where nyx's own `--selection` used to
    // paint the chip.
    expect(resolveTokenValue(LAYER, "var(--selection)")).toBeNull();
    // A cycle terminates instead of hanging.
    expect(resolveTokenValue(LAYER, "var(--loop)")).toBeNull();
  });

  test("paints a swatch only for a colour the layer can answer for", () => {
    expect(resolveTokenColor(LAYER, "var(--color-secondary)")).toBe("oklch(0.86 0.2 145)");
    expect(resolveTokenColor(LAYER, "#fff")).toBe("#fff");
    // Not a colour, and not resolvable: no chip in either case — a missing swatch is a gap, a wrong
    // one is a lie.
    expect(resolveTokenColor(LAYER, "0.625rem")).toBeNull();
    expect(resolveTokenColor(LAYER, "var(--selection)")).toBeNull();
    expect(resolveTokenColor(LAYER, "")).toBeNull();
  });
});
