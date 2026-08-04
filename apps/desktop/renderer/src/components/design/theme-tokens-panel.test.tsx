import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";
import { mockIPC } from "@/bridge/test-harness";

import { ThemeTokensPanel } from "./theme-tokens-panel";

/**
 * The `/design` TOKENS surface (review #42). The catalogue exposed COMPONENTS only, while the other
 * half of their look — the tokens their Tailwind classes resolve against — had no surface at all and
 * was never read from the app. These pin what the panel is: the tokens are READABLE, with the app
 * file they are referenced from and the sheets the ingest refused.
 *
 * What it is NOT, twice over:
 *   * a TRIGGER (finding 01KYKX7HBP4ZSSS6ZDM31T6A5E) — the "Tokens depuis l'app" button that
 *     invoked `design_tokens_ingest` is gone; that ingest is the AGENT's step and the panel receives
 *     its result through `themeTokensJson`;
 *   * an EDITOR (finding `01KZ3JQXGSVZAC2MHHNZAS681Y`) — a row used to be an input that wrote
 *     `design_tokens_set` on blur, with no re-render, no acknowledgement and no undo, and the write
 *     SURVIVED a restart. The tools and their db-commands are untouched; only the UI gestures are.
 */

/**
 * The layer `nyx_core::design_tokens` ingests from a real front — INCLUDING its provenance report
 * (`skipped` / `skipped_count` / `pruned_dirs`), which the ingest now PERSISTS into the blob instead
 * of returning it only in its own response (finding `01KYSK7GS9C3ZRJYW00ZA0H6GF`). This panel cannot
 * run an ingest, so the blob is the only place it could ever read that from.
 */
const LAYER = JSON.stringify({
  sources: [{ repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 2 }],
  variables: { "--primary": "#ff0055", "--radius": "0.625rem" },
  dark: { "--primary": "#220011" },
  skipped: [],
  skipped_count: 0,
  pruned_dirs: [".generated"],
});

/**
 * A layer ingested BEFORE the report was persisted: it names its sources and records nothing else.
 * That — and a theme written by hand — is all that is left of the "non enregistrés" case.
 */
const LEGACY_INGESTED = JSON.stringify({
  sources: [{ repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 2 }],
  variables: { "--primary": "#ff0055", "--radius": "0.625rem" },
  dark: { "--primary": "#220011" },
});

/**
 * A REAL app layer: raw variables PLUS the `@theme inline` aliases pointing back at them — the
 * shape every shadcn/Tailwind-v4 front produces, and the one that put 36 duplicate rows and 36 wrong
 * swatches on screen (findings `01KZ3JQY3FK9RRNS15H2BCCSN0` and `01KZ3JW5A63TJWX83V8ZKX62CN`).
 */
const ALIASED = JSON.stringify({
  sources: [{ repo: "web", path: "src/globals.css", variable_count: 6 }],
  variables: {
    "--secondary": "oklch(0.86 0.2 145)",
    "--color-secondary": "var(--secondary)",
    // An alias whose target the layer does NOT declare: unresolvable, and never nyx's own value.
    "--color-selection": "var(--selection)",
  },
  dark: {},
});

const open = () => fireEvent.click(screen.getByRole("button", { expanded: false }));

describe("ThemeTokensPanel", () => {
  beforeEach(() => {
    mockIPC(() => undefined);
  });

  it("shows the token count and WHERE in the app they are referenced from", () => {
    render(<ThemeTokensPanel themeTokensJson={LAYER} />);
    expect(screen.getByText(/from web\/src\/styles\.css/)).toBeTruthy();
    expect(screen.getByText(/2 tokens/)).toBeTruthy();
    expect(screen.getByText(/1 with a dark variant/)).toBeTruthy();
  });

  it("names the grey-preview cause AND who ingests the tokens when there are none", () => {
    // EMPTY-STATE: the ingest button is gone, so an empty layer must say who fills it — visible
    // WITHOUT opening the disclosure, and again in full once opened. No tool name in either: the
    // reader is told whose job it is, not which command runs (finding `01KZ3JW5A63TJWX83V8ZKX62CN`).
    render(<ThemeTokensPanel themeTokensJson={null} />);
    const short = screen.getByTestId("tokens-empty-agent-driven");
    expect(short.textContent).toContain("render grey");
    expect(short.textContent).toContain("the agent ingests them");

    open();
    const detail = screen.getByTestId("tokens-empty-detail");
    expect(detail.textContent).toContain("The agent ingests them");
    expect(detail.textContent).not.toContain("design_tokens");
  });

  /**
   * THE PANEL WRITES NOTHING (finding `01KZ3JQXGSVZAC2MHHNZAS681Y`). Editing a token wrote durably
   * into the project's data, changed no preview whatsoever, said nothing and could not be undone —
   * and the value was still there after a restart. The arbitration is the one already made for the
   * ingest: the layer is a READING of the app's own stylesheet, so the edit DISAPPEARS rather than
   * being disabled in silence. Pinned two ways: no field to type in, and every button clicked twice
   * reaches no write.
   */
  it("offers NO gesture that writes or ingests — the agent is the only pilot", async () => {
    const calls: string[] = [];
    mockIPC((cmd) => {
      calls.push(cmd);
      return undefined;
    });
    render(<ThemeTokensPanel themeTokensJson={LAYER} />);
    open();

    // The VALUE is on screen; there is simply nothing to type into.
    expect(screen.getByLabelText("--primary").textContent).toBe("#ff0055");
    expect(screen.queryAllByRole("textbox")).toHaveLength(0);
    expect(screen.queryByRole("button", { name: /Tokens depuis l'app/ })).toBeNull();

    for (const b of screen.getAllByRole("button")) fireEvent.click(b);
    for (const b of screen.getAllByRole("button")) fireEvent.click(b);
    await waitFor(() => expect(screen.getByTestId("theme-tokens-panel")).toBeTruthy());
    expect(calls).not.toContain("design_tokens_ingest");
    expect(calls).not.toContain("design_tokens_set");
  });

  it("shows the layer the AGENT ingested, with no click at all", () => {
    // The capability is untouched: the agent's `design_tokens` ingest writes the layer, `useDesign`
    // re-pulls it off `prototypes://changed`, and it arrives here as a prop. Re-rendering with the
    // new `themeTokensJson` is exactly that path, and it must repaint with zero gesture.
    const { rerender } = render(<ThemeTokensPanel themeTokensJson={null} />);
    expect(screen.getByTestId("tokens-empty-agent-driven")).toBeTruthy();

    rerender(<ThemeTokensPanel themeTokensJson={LAYER} />);
    expect(screen.queryByTestId("tokens-empty-agent-driven")).toBeNull();
    expect(screen.getByText(/from web\/src\/styles\.css/)).toBeTruthy();
    expect(screen.getByText(/2 tokens/)).toBeTruthy();
  });

  /**
   * ONE ROW PER TOKEN, AND ITS SWATCH IS THE PROJECT'S (findings `01KZ3JQY3FK9RRNS15H2BCCSN0` and
   * `01KZ3JW5A63TJWX83V8ZKX62CN`). `--color-secondary: var(--secondary)` is the same token under a
   * second name: it said nothing more, and the swatch beside it was painted by handing the raw
   * `var(…)` to the browser — which resolved it against NYX's stylesheet, so the two rows of one
   * token showed two different colours and an alias nyx does not carry showed none at all.
   */
  it("drops the alias rows and paints a swatch from the PROJECT's own layer", () => {
    render(<ThemeTokensPanel themeTokensJson={ALIASED} />);
    // Two rows, not three: `--color-secondary` is `--secondary` under another name.
    expect(screen.getByText(/2 tokens/)).toBeTruthy();
    open();
    const rows = screen.getAllByTestId("token-row");
    expect(rows).toHaveLength(2);
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining("--color-selection"),
      expect.stringContaining("--secondary"),
    ]);
    expect(screen.queryByLabelText("--color-secondary")).toBeNull();

    // The declared colour paints; the alias that resolves NOWHERE in this layer paints nothing —
    // never nyx's `--selection`. No swatch beats a false one.
    const swatches = screen.getAllByTestId("token-swatch");
    expect(swatches).toHaveLength(1);
    expect((swatches[0] as HTMLElement).style.background).toContain("oklch(0.86 0.2 145)");
  });

  it("states ONE number per notion: the tokens listed, and how many of THEM have a dark variant", () => {
    // Finding `01KZ3JYARFJ67T5N1NC22XK69C`: the bar used to say `103 token(s) … 48 in dark` while
    // the retained sheet announced `163 var.` and the list held exactly 103 rows, with nothing
    // relating the three. The sheet's own count is gone, and both remaining numbers are counted
    // over the rows the reader can see.
    const layer = JSON.stringify({
      sources: [{ repo: "web", path: "src/styles.css", variable_count: 999 }],
      variables: {
        "--primary": "#ff0055",
        "--radius": "0.625rem",
        "--color-primary": "var(--primary)",
      },
      // A dark override on the ALIAS counts for nothing: only a listed row can be counted.
      dark: { "--primary": "#220011", "--color-primary": "var(--primary)" },
    });
    render(<ThemeTokensPanel themeTokensJson={layer} />);
    expect(screen.getByText(/2 tokens · 1 with a dark variant/)).toBeTruthy();
    open();
    expect(screen.getAllByTestId("token-row")).toHaveLength(2);
    expect(screen.getByTestId("tokens-source-kept").textContent).not.toContain("999");
  });

  it("does not repeat the collapsed summary once the panel is open", () => {
    // Finding `01KZ3JW5A63TJWX83V8ZKX62CN`: the bar announced its sheet path and its discarded count,
    // and the provenance block a few pixels below said both again, word for word.
    const layer = JSON.stringify({
      sources: [{ repo: "web", path: "src/styles.css", variable_count: 42 }],
      skipped: [{ repo: "web", path: "src/legacy.css", reason: "recipe_exclude" }],
      skipped_count: 1,
      pruned_dirs: [],
      variables: { "--primary": "#ff0055" },
      dark: {},
    });
    render(<ThemeTokensPanel themeTokensJson={layer} />);
    expect(screen.getByText(/from web\/src\/styles\.css/)).toBeTruthy();
    expect(screen.getByTestId("tokens-skipped-summary")).toBeTruthy();

    open();
    expect(screen.queryByTestId("tokens-skipped-summary")).toBeNull();
    // Said ONCE, where the detail is.
    expect(screen.getByTestId("tokens-source-kept").textContent).toContain("web/src/styles.css");
    expect(screen.getByTestId("tokens-provenance").textContent).toContain("Sheets discarded (1)");
  });

  /**
   * WHERE THE LAYER COMES FROM (finding `01KYSHSPGSPC98C49X68MWY60D`, after
   * `01KYSCX562SWDXZBBAPX397X8F`). The panel named the best-ranked sheet and nothing else, so a layer
   * merged from a `.generated` mirror plus a docs theme looked exactly like a healthy one, and a
   * developer whose stylesheet had been skipped had no way to find out. The provenance block answers
   * both: what was RETAINED, what was DISCARDED and why, and which directories were never entered.
   */
  it("shows the layer's PROVENANCE: retained sheets, discarded ones with their reason, pruned dirs", () => {
    const layer = JSON.stringify({
      sources: [
        { repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 42 },
        { repository_id: "r1", repo: "web", path: "src/theme.css", variable_count: 6 },
      ],
      skipped: [
        { repo: "web", path: "docs/.vitepress/theme/theme.css", reason: "doc_tool_theme" },
        { repo: "web", path: "src/legacy.css", reason: "recipe_exclude" },
      ],
      skipped_count: 5,
      pruned_dirs: [".generated", "node_modules"],
      variables: { "--primary": "#ff0055" },
      dark: {},
    });
    render(<ThemeTokensPanel themeTokensJson={layer} />);

    // The pollution SIGNAL is visible before opening anything.
    expect(screen.getByTestId("tokens-skipped-summary").textContent).toContain("5 sheet(s)");

    open();
    const provenance = screen.getByTestId("tokens-provenance");
    expect(provenance.textContent).toContain("Sheets retained (2)");

    // RETAINED — each with the file it contributed from. The per-sheet variable count is NOT
    // repeated here (see the "one number per notion" test).
    const kept = screen.getAllByTestId("tokens-source-kept");
    expect(kept).toHaveLength(2);
    expect(kept[0]?.textContent).toContain("web/src/styles.css");

    // DISCARDED — with the backend's own reason CODE, and what it means.
    const skipped = screen.getAllByTestId("tokens-source-skipped");
    expect(skipped).toHaveLength(2);
    expect(skipped[0]?.textContent).toContain("docs/.vitepress/theme/theme.css");
    expect(skipped[0]?.textContent).toContain("doc_tool_theme");
    expect(skipped[0]?.textContent).toContain("another tool's theme directory");
    expect(skipped[1]?.textContent).toContain("recipe_exclude");
    // The report is BOUNDED: the ones not detailed are still counted, never hidden.
    expect(screen.getByTestId("tokens-skipped-more").textContent).toContain("3 more");

    // PRUNED — `.generated` here is the proof the generated mirror of a stylesheet was not swept.
    expect(screen.getByTestId("tokens-pruned-dirs").textContent).toContain(".generated");
    expect(screen.queryByTestId("tokens-provenance-unreported")).toBeNull();
  });

  it("counts the pruned directories the ingest really pruned, not the ones it detailed", () => {
    // FINDING `01KYSNRNY550NS778YCMC8CZGH`: this list is BOUNDED exactly as the discarded sheets are
    // (the recipe's `ignored_dirs` alone can hold 64 names), and the panel used to print
    // `prunedDirs.length` — the bounded head — as if it were the total. A truncation may never
    // become an under-declaration.
    const layer = JSON.stringify({
      sources: [{ repo: "web", path: "src/styles.css", variable_count: 42 }],
      skipped: [],
      skipped_count: 0,
      pruned_dirs: [".generated", "node_modules"],
      pruned_dirs_count: 66,
      variables: { "--primary": "#ff0055" },
      dark: {},
    });
    render(<ThemeTokensPanel themeTokensJson={layer} />);
    open();
    expect(screen.getByTestId("tokens-provenance").textContent).toContain(
      "Directories not descended into (66)",
    );
    expect(screen.getByTestId("tokens-pruned-dirs-more").textContent).toContain("64 more");

    // …and a report that was NOT truncated says nothing extra.
    cleanup();
    render(
      <ThemeTokensPanel
        themeTokensJson={JSON.stringify({
          sources: [{ repo: "web", path: "src/styles.css", variable_count: 42 }],
          skipped: [],
          skipped_count: 0,
          pruned_dirs: [".generated", "node_modules"],
          pruned_dirs_count: 2,
          variables: { "--primary": "#ff0055" },
          dark: {},
        })}
      />,
    );
    open();
    expect(screen.getByTestId("tokens-provenance").textContent).toContain(
      "Directories not descended into (2)",
    );
    expect(screen.queryByTestId("tokens-pruned-dirs-more")).toBeNull();
  });

  /**
   * THE MOST VALUABLE CASE, on the blob the BACKEND REALLY WRITES (finding
   * `01KYSNVW31C0E8YQPET0505HW2`). No token at all means every design-system class renders grey, so
   * this report is the ONLY diagnostic there is — and the ingest used to return BEFORE writing
   * anything as soon as it retained no sheet, so the panel had "aucun token" AND "non enregistrés"
   * to show, over the one layer it could explain nothing about.
   *
   * This test used to build its blob BY HAND, and built one no backend write could produce
   * (`sources: []` and `variables: {}` sitting next to a report). It now renders
   * `contract/design-fixtures.json`'s `empty_run_blob`, which the Rust suite asserts is what a real
   * `ingest_project_tokens` over such a tree stores — so the front is pinned to a shape the backend
   * really sends, and a change on either side fails the other's suite.
   */
  it("explains WHY a layer is empty, on the blob such a run really writes", () => {
    const layer = JSON.stringify(designFixtures.empty_run_blob.blob);
    // What the backend does NOT write there: no `variables`, no `dark`, no `sources` — a run that
    // retained nothing claims none of them (and leaves a hand-authored theme's alone).
    for (const absent of ["variables", "dark", "sources"]) {
      expect(Object.keys(designFixtures.empty_run_blob.blob)).not.toContain(absent);
    }

    render(<ThemeTokensPanel themeTokensJson={layer} />);
    // The pollution signal is visible before opening anything: three sheets went somewhere.
    expect(screen.getByTestId("tokens-skipped-summary").textContent).toContain("3 sheet(s)");
    open();

    // The empty-state still names the agent (it is still who ingests) …
    expect(screen.getByTestId("tokens-empty-detail")).toBeTruthy();
    // … and the provenance says WHAT HAPPENED instead of leaving the absence unexplained: which
    // sheets were refused, with the backend's own reason code, and where the walk never went.
    expect(screen.getByTestId("tokens-no-source").textContent).toContain("none");
    const skipped = screen.getAllByTestId("tokens-source-skipped");
    expect(skipped).toHaveLength(3);
    expect(skipped[0]?.textContent).toContain("docs/.vitepress/theme/custom.css");
    expect(skipped[0]?.textContent).toContain("doc_tool_theme");
    expect(skipped[1]?.textContent).toContain("foreign_namespace");
    expect(skipped[2]?.textContent).toContain("not_a_token_sheet");
    expect(screen.getByTestId("tokens-provenance").textContent).toContain("Sheets discarded (3)");
    // `.generated` here is the proof the generated mirror of a stylesheet was never swept — on a
    // grey catalogue, that is the answer to "where are my tokens?".
    expect(screen.getByTestId("tokens-pruned-dirs").textContent).toContain(".generated");
    expect(screen.getByTestId("tokens-provenance").textContent).toContain(
      "Directories not descended into (2)",
    );
    // …and the excuse is GONE: this run recorded its report, so nothing here says "not recorded".
    expect(screen.queryByTestId("tokens-provenance-unreported")).toBeNull();
  });

  /**
   * THE FLIP (finding `01KYSK7GS9C3ZRJYW00ZA0H6GF`). An INGESTED layer used to reach this panel with
   * no report at all — the backend computed it and dropped it at persistence — so the panel could only
   * say "non enregistrés", over a layer that may well have been polluted. Now the blob carries the
   * report, so an ingested layer shows the FACTS: a run that refused nothing SAYS it refused nothing,
   * and names the directories it never entered.
   */
  it("shows the FACTS for an ingested layer: nothing discarded, and where the walk did not go", () => {
    render(<ThemeTokensPanel themeTokensJson={LAYER} />);
    // Nothing was discarded, so there is no pollution signal to raise.
    expect(screen.queryByTestId("tokens-skipped-summary")).toBeNull();
    open();
    expect(screen.getByTestId("tokens-source-kept").textContent).toContain("web/src/styles.css");
    expect(screen.queryByTestId("tokens-source-skipped")).toBeNull();
    // `.generated` here is the proof the generated mirror of the stylesheet was not swept.
    expect(screen.getByTestId("tokens-pruned-dirs").textContent).toContain(".generated");
    // …and the honest-gap placeholder is GONE: the report exists, so there is nothing to excuse.
    expect(screen.queryByTestId("tokens-provenance-unreported")).toBeNull();
  });

  it("does not pass an UNRECORDED report off as an empty one", () => {
    // "rien n'a été écarté" and "personne n'a noté ce qui a été écarté" are different statements —
    // presenting the second as the first is how a polluted layer passed for a healthy one. Now that
    // the ingest STORES its report, what is left of this case is a layer ingested before it did.
    render(<ThemeTokensPanel themeTokensJson={LEGACY_INGESTED} />);
    expect(screen.queryByTestId("tokens-skipped-summary")).toBeNull();
    open();
    expect(screen.getByTestId("tokens-source-kept").textContent).toContain("web/src/styles.css");
    expect(screen.queryByTestId("tokens-source-skipped")).toBeNull();
    expect(screen.queryByTestId("tokens-pruned-dirs")).toBeNull();
    expect(screen.getByTestId("tokens-provenance-unreported").textContent).toContain(
      "not recorded by the last ingest",
    );

    // …and the case this branch does NOT cover, pinned so the comment above it stays true
    // (finding `01KYSNT38VZYJEAH4VEFJW8FHW`): a theme TYPED BY HAND has neither a source nor a
    // report, so the whole provenance block is silence — a project that never ran an ingest is not
    // told about a report it never asked for.
    cleanup();
    render(
      <ThemeTokensPanel
        themeTokensJson={JSON.stringify({ variables: { "--primary": "#ff0055" }, dark: {} })}
      />,
    );
    open();
    expect(screen.queryByTestId("tokens-provenance")).toBeNull();
    expect(screen.queryByTestId("tokens-provenance-unreported")).toBeNull();
  });
});
