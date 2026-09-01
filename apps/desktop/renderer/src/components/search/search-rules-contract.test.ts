import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import searchFixtures from "../../../../../../contract/search-fixtures.json";

import { SEARCH_GROUP_ORDER, SEARCH_KIND_META } from "./search-meta";
import { SUGGESTION_CAP } from "./search-palette";
import { MIN_QUERY_LEN } from "./use-global-search";

/**
 * THE THREE PRODUCT RULES OF THE PALETTE, PINNED ACROSS THE TWO LANGUAGES.
 *
 * The group ORDER (Specs → Tasks → Ideas → Workspaces → Projects → Managed commands → Agent
 * sessions), the per-type CAP (8) and the query FLOOR (2) each exist twice — once in
 * `crates/nyx-core/src/search.rs`, once here (`search-meta.ts`, `search-palette.tsx`,
 * `use-global-search.ts`) — and each duplication is DELIBERATE: the palette must name every kind
 * the backend's trim can hand it, must know how many suggestions fit before the answer arrives, and
 * must issue no read below the floor. What was missing is not the second copy, it is
 * anything that makes the two fail TOGETHER (review finding 01M15D67D0F686ET3DMXYJCZY9): a cap
 * raised on one side alone shipped a backend answering more rows than the palette drew, with every
 * suite green.
 *
 * `contract/search-fixtures.json` is the statement of the three rules. It is replayed HERE and by
 * `crates/nyx-core/src/search/tests.rs`
 * (`the_shared_fixture_pins_the_three_product_rules_the_renderer_repeats`), so adding a case fails
 * both suites until both implementations agree.
 *
 * This half goes one step further and reads the RUST DECLARATIONS themselves, the way
 * `command-group-ipc-contract.test.ts` reads the sidecar's dispatch: without it, editing `search.rs`
 * alone would redden Rust and leave TypeScript green until someone thought to touch the fixture.
 * With it, a constant moved on either side turns this file red.
 */

const REPO = join(process.cwd(), "..", "..", "..");
const AGGREGATOR = join(REPO, "crates", "nyx-core", "src", "search.rs");
const rust = readFileSync(AGGREGATOR, "utf8");

/** A `pub const NAME: usize = N;` of the aggregator, as a number. */
function rustUsize(name: string): number {
  const found = rust.match(new RegExp(`pub const ${name}: usize = ([0-9_]+);`));
  expect(found, `${name} is declared in crates/nyx-core/src/search.rs`).toBeTruthy();
  // `8` today, `1_000` tomorrow: Rust's digit separators are stripped before the parse.
  return Number(found![1].split("_").join(""));
}

/**
 * The variants of `GROUP_ORDER`, lowercased into the wire names.
 *
 * Every kind is a single word today, so lowercasing IS the serde `snake_case` rename. A future
 * multi-word variant would not silently pass here — it would produce `specrevision` where the
 * fixture says `spec_revision` and turn this red, which is the outcome one wants.
 */
function rustGroupOrder(): string[] {
  const block = rust.match(/pub const GROUP_ORDER: \[SearchKind; \d+\] = \[([^\]]*)\];/);
  expect(block, "GROUP_ORDER is declared in crates/nyx-core/src/search.rs").toBeTruthy();
  return [...block![1].matchAll(/SearchKind::(\w+)/g)].map((match) => match[1].toLowerCase());
}

describe("the shared Rust ⟷ TS fixture — the palette's three product rules", () => {
  // Since the review that made the results ONE ranked list, this order is no longer what the
  // palette DRAWS — the aggregator ranks across the types and the renderer touches nothing. It is
  // still the order the overall cap of 40 trims on backend-side, and still the roster of kinds the
  // palette must be able to draw, which is exactly what the two halves have to agree on.
  it("the GROUP ORDER is the fixture's, on both sides", () => {
    expect(SEARCH_GROUP_ORDER).toEqual(searchFixtures.group_order);
    expect(rustGroupOrder()).toEqual(searchFixtures.group_order);
    // …and the renderer can DRAW every kind the order names: a kind with no name and no glyph
    // would crash the palette on the first hit of that type.
    for (const kind of searchFixtures.group_order) {
      const meta = SEARCH_KIND_META[kind as keyof typeof SEARCH_KIND_META];
      expect(meta?.label, `${kind} has a name`).toBeTruthy();
      expect(meta?.icon, `${kind} has a glyph`).toBeTruthy();
    }
  });

  it("the PER-TYPE CAP is the fixture's, on both sides", () => {
    expect(SUGGESTION_CAP).toBe(searchFixtures.cap_type);
    expect(rustUsize("CAP_TYPE")).toBe(searchFixtures.cap_type);
  });

  it("the QUERY FLOOR is the fixture's, on both sides", () => {
    expect(MIN_QUERY_LEN).toBe(searchFixtures.min_query_len);
    expect(rustUsize("MIN_QUERY_LEN")).toBe(searchFixtures.min_query_len);
  });
});
