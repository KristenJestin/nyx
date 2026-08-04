import { describe, expect, it } from "vitest";

import {
  parseComponentRemoval,
  recipeSourceMeaning,
  removalNeedsTelling,
  verdictMeaning,
} from "./removal";

/**
 * The `design_component_delete` answer, read the way the `/design` catalogue reads it (finding
 * `01KYSHSPGSPC98C49X68MWY60D`). Two properties matter here:
 *
 *   * the backend's `warnings[]` come out VERBATIM — the surface displays them, it never re-words
 *     them (only the backend knows the discovery's verdict and what the recipe holds);
 *   * a response that carries none of the new fields (an older sidecar answering `{ deleted: true }`,
 *     a `null`, anything) must yield a removal that claims NOTHING rather than throwing — a parse
 *     accident must not turn a successful delete into an error, nor invent a reassuring silence.
 */

/** The real shape `nyx_core::design::ComponentRemoval` serializes, ephemeral case. */
const EPHEMERAL = {
  deleted: true,
  effect: {
    ephemeral: true,
    verdict: "elected",
    repo: "web",
    source_path: "src/ui/Button.tsx",
    recipe_source: "stack_default",
  },
  excluded: [],
  recipe_materialized: false,
  refused: null,
  warnings: [
    "THIS REMOVAL IS EPHEMERAL: 'Button' is out of the catalogue, but its source " +
      "'src/ui/Button.tsx' of repo 'web' STILL matches this project's recipe, and a component's " +
      "identity IS its source — so the next design_inventory DISCOVERS IT AGAIN.",
  ],
};

describe("parseComponentRemoval", () => {
  it("reads the effect, the exclusion and the backend's warnings VERBATIM", () => {
    const removal = parseComponentRemoval(EPHEMERAL);
    expect(removal.deleted).toBe(true);
    expect(removal.effect).toEqual({
      ephemeral: true,
      verdict: "elected",
      repo: "web",
      source_path: "src/ui/Button.tsx",
      recipe_source: "stack_default",
    });
    // Not re-worded, not truncated, not summarised.
    expect(removal.warnings).toEqual(EPHEMERAL.warnings);
    expect(removalNeedsTelling(removal)).toBe(true);
  });

  it("reads the DURABLE removal: the patterns written and the materialized recipe", () => {
    const removal = parseComponentRemoval({
      ...EPHEMERAL,
      effect: { ...EPHEMERAL.effect, ephemeral: false, verdict: "excluded" },
      excluded: ["src/ui/Button.tsx", "src/ui/Button.html"],
      recipe_materialized: true,
      warnings: ["'src/ui/Button.tsx' is now in this project's recipe `exclude` list."],
    });
    expect(removal.excluded).toEqual(["src/ui/Button.tsx", "src/ui/Button.html"]);
    expect(removal.recipe_materialized).toBe(true);
    expect(removal.effect.ephemeral).toBe(false);
  });

  it("keeps a REFUSAL, which is the response that deleted nothing", () => {
    const removal = parseComponentRemoval({
      deleted: false,
      effect: EPHEMERAL.effect,
      excluded: [],
      recipe_materialized: false,
      refused: "'Card' was NOT deleted: its entry is PINNED in this project's recipe.",
      warnings: [],
    });
    expect(removal.deleted).toBe(false);
    expect(removal.refused).toContain("PINNED");
    expect(removalNeedsTelling(removal)).toBe(true);
  });

  it("claims nothing for a bare / absent / malformed answer, and never throws", () => {
    for (const value of [null, undefined, 42, "deleted", [], { deleted: true }]) {
      const removal = parseComponentRemoval(value);
      expect(removal.warnings).toEqual([]);
      expect(removal.excluded).toEqual([]);
      expect(removal.refused).toBeNull();
      expect(removal.effect.ephemeral).toBe(false);
      expect(removal.effect.verdict).toBe("");
      // A definitive removal (or an unknown shape) has NOTHING to tell: no notice at all.
      expect(removalNeedsTelling(removal)).toBe(false);
    }
    // The one fact a legacy `{ deleted: true }` does carry is still read.
    expect(parseComponentRemoval({ deleted: true }).deleted).toBe(true);
  });

  it("drops non-string entries instead of printing `undefined` at the human", () => {
    const removal = parseComponentRemoval({
      deleted: true,
      effect: { ephemeral: 1, verdict: 7, repo: "", source_path: null, recipe_source: "custom" },
      excluded: ["ok", 3, null, "  "],
      warnings: [null, "a real warning"],
    });
    expect(removal.effect.ephemeral).toBe(false); // only a real `true` is ephemeral
    expect(removal.effect.verdict).toBe("");
    expect(removal.effect.repo).toBeNull(); // an empty string is not a repo
    expect(removal.excluded).toEqual(["ok"]);
    expect(removal.warnings).toEqual(["a real warning"]);
  });
});

describe("verdict / recipe-source wording", () => {
  it("explains every verdict the backend can emit, and echoes an unknown one", () => {
    for (const verdict of [
      "elected",
      "pinned",
      "pruned_dir",
      "excluded",
      "no_rule",
      "no_marker",
      "missing",
      "unreadable",
      "greenfield",
    ]) {
      expect(verdictMeaning(verdict)).toBeTruthy();
    }
    // An unknown code has no French clause — the surface then shows the CODE alone rather than a
    // guess (a verdict added tomorrow must not print as "undefined").
    expect(verdictMeaning("brand_new_verdict")).toBeNull();
  });

  it("names where the recipe that judged the source came from", () => {
    expect(recipeSourceMeaning("custom")).toContain("project's own recipe");
    expect(recipeSourceMeaning("globs_override")).toContain("override");
    expect(recipeSourceMeaning("stack_default")).toContain("stack");
    expect(recipeSourceMeaning("whatever")).toBe("whatever");
  });
});
