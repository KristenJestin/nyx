/**
 * Derive the iframe's THEME STYLESHEET from a project's `design.theme_tokens_json` (v2.7 — Prototype :
 * machinerie, Phase 3, PDR-0010: "thème depuis design"; review #42: "l'aperçu rend gris").
 *
 * # Why this is the fix for the grey preview
 *
 * A synthesized component carries Tailwind CLASSES (`bg-primary`, `text-muted-foreground`). A class
 * is only ever a POINTER: `bg-primary` compiles to `background-color: var(--color-primary)`, and
 * emits nothing at all when that token does not exist. nyx never read the app's tokens, so the
 * frame's `@theme` was empty and every rendu came out grey. The Rust side now INGESTS the app's own
 * stylesheet into `theme_tokens_json` (`nyx_core::design_tokens`); THIS module turns that layer into
 * the two CSS blocks the frame needs:
 *
 *   * the `@theme` body — what DEFINES the Tailwind utilities (`--color-primary` ⇒ `bg-primary`);
 *   * the `:root` / `.dark` body — the RAW app variables (`--primary`, `--background`, …) exactly as
 *     the app declares them.
 *
 * The two are linked BY REFERENCE, never by copy: a raw `--primary` lands in `:root` once, and the
 * utility is aliased onto it (`--color-primary: var(--primary)`). That is the shadcn/Tailwind-v4
 * `@theme inline` idiom, and it is what makes `primary` a SINGLE identity — edit the one `:root`
 * declaration (or re-ingest it from the app) and every utility, component and preview follows.
 *
 * # Accepted shapes (all optional — an absent/blank theme yields empty bodies, a valid default)
 *
 *   * the INGESTED layer `{ variables: { "--primary": "…" }, dark: { … }, sources: [ … ] }` — the
 *     shape `nyx_core::design_tokens` stores. `sources`/`source` are METADATA (where in the app the
 *     tokens live) and never emit CSS — and so are the ingest run's PROVENANCE keys when the layer
 *     carries them (`skipped` / `skipped_count` / `pruned_dirs`: what was NOT merged and why, and
 *     where the walk did not go — finding `01KYSHSPGSPC98C49X68MWY60D`);
 *   * an escape hatch `{ "css": "--color-primary: oklch(…);" }` → used verbatim inside `@theme`;
 *   * namespaced maps `{ "colors": { "primary": "…" }, "fonts": { "sans": "…" }, "radius": "…",
 *     "spacing": { "gutter": "…" } }` → `--color-primary`, `--font-sans`, `--radius`,
 *     `--spacing-gutter`. Any other object is flattened generically as `--<key>-<subkey>`.
 */

/** Tailwind v4 namespace for a top-level token group (`colors` → `--color-*`, etc.). */
const NAMESPACE: Record<string, string> = {
  colors: "color",
  color: "color",
  fonts: "font",
  font: "font",
  spacing: "spacing",
  radius: "radius",
  shadow: "shadow",
  shadows: "shadow",
  breakpoints: "breakpoint",
  text: "text",
};

/**
 * The keys of the INGESTED layer that are handled explicitly (or are pure metadata), so the generic
 * group flattener never turns `sources[0].path` into a `--source-path` custom property.
 *
 * EXPORTED for the CROSS-LANGUAGE guard (finding `01KYSNQWQD1324E0NA0172M66J`): every key
 * `TokenLayer::to_json` (Rust, `design_tokens.rs`) emits must be in here, and that correspondence is
 * now DATA — `contract/design-fixtures.json` lists those keys, the Rust suite asserts `to_json`
 * emits exactly them, and `theme-css.test.ts` asserts every one of them is reserved. A key added on
 * one side and not the other fails a suite instead of silently coming back out as a
 * `--pruned-dirs-count` custom property inside `@theme`.
 */
export const RESERVED_KEYS = new Set([
  "variables",
  "dark",
  "source",
  "sources",
  "css",
  "extracted_at",
  // The ingest run's PROVENANCE report (finding `01KYSHSPGSPC98C49X68MWY60D`): the sheets the run
  // did NOT merge and the directories it refused to enter, each list with its own TOTAL (finding
  // `01KYSNRNY550NS778YCMC8CZGH`). Metadata, exactly like `sources` — a `--skipped-0-reason` custom
  // property would be nonsense.
  "skipped",
  "skipped_count",
  "pruned_dirs",
  "pruned_dirs_count",
  "repos_scanned",
  "candidates",
]);

/**
 * The Tailwind v4 THEME NAMESPACES. A variable whose name already starts with one of these IS a
 * theme entry (the app wrote `@theme { --color-primary: … }` itself), so it goes into `@theme`
 * verbatim; anything else is a raw app variable that belongs on `:root`.
 */
const THEME_PREFIXES = [
  "color-",
  "font-",
  "text-",
  "tracking-",
  "leading-",
  "breakpoint-",
  "container-",
  "spacing",
  "radius",
  "shadow",
  "inset-shadow-",
  "drop-shadow-",
  "blur-",
  "perspective-",
  "aspect-",
  "ease-",
  "animate-",
  "default-",
];

/** Values that make a token a COLOUR — the ones worth aliasing into a `bg-*` / `text-*` utility. */
const COLOR_VALUE = /^(#|rgba?\(|hsla?\(|hwb\(|oklch\(|oklab\(|lab\(|lch\(|color\(|var\(--)/i;
const COLOR_KEYWORDS = new Set(["transparent", "currentcolor", "white", "black", "inherit"]);

/** Kebab-case a token key so `primaryForeground` / `primary_foreground` → `primary-foreground`. */
function kebab(key: string): string {
  return key
    .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
    .replace(/_/g, "-")
    .toLowerCase();
}

/** A single `--name: value;` line, skipping non-scalar / empty values. */
function decl(name: string, value: unknown): string | null {
  if (value == null) return null;
  if (typeof value === "string" || typeof value === "number") {
    const v = String(value).trim();
    return v ? `  --${name}: ${v};` : null;
  }
  return null;
}

/** Emit `--<prefix>-<key>: value` for each scalar entry of `map` (nested objects flattened). */
function emitGroup(prefix: string, map: Record<string, unknown>, out: string[]): void {
  for (const [key, value] of Object.entries(map)) {
    const name = prefix ? `${prefix}-${kebab(key)}` : kebab(key);
    if (value != null && typeof value === "object" && !Array.isArray(value)) {
      emitGroup(name, value as Record<string, unknown>, out);
    } else {
      const line = decl(name, value);
      if (line) out.push(line);
    }
  }
}

/** `--primary` → `primary` (the bare name, whatever spelling the layer used). */
function bareName(name: string): string {
  return name.trim().replace(/^-+/, "");
}

/** Is this name ALREADY a Tailwind theme entry (so it belongs in `@theme`, not on `:root`)? */
export function isThemeNamespaced(name: string): boolean {
  const bare = bareName(name);
  return THEME_PREFIXES.some((p) => bare.startsWith(p));
}

/** Does this value look like a colour (worth an `--color-*` utility alias)? */
export function isColorValue(value: string): boolean {
  const v = value.trim();
  return COLOR_VALUE.test(v) || COLOR_KEYWORDS.has(v.toLowerCase());
}

/** A value that is ONE `var(--x)` / `var(--x, fallback)` reference — its name and its fallback. */
const VAR_REFERENCE = /^var\(\s*(--[A-Za-z0-9_-]+)\s*(?:,([\s\S]*))?\)$/;

function varParts(value: string): { name: string; fallback: string } | null {
  const m = value.trim().match(VAR_REFERENCE);
  if (!m) return null;
  return { name: m[1]!, fallback: (m[2] ?? "").trim() };
}

/**
 * The NAME a value is a bare reference to, or `null` when it is not one single reference (a real
 * colour, a gradient, a `calc(…)`).
 */
function varReference(value: string): string | null {
  return varParts(value)?.name ?? null;
}

/** How many `var()` hops a resolution follows before it declares the chain unreadable. */
const MAX_ALIAS_HOPS = 8;

/**
 * Resolve a token value INSIDE THE PROJECT'S OWN LAYER — never in the host document (finding
 * `01KZ3JQY3FK9RRNS15H2BCCSN0`).
 *
 * `--color-secondary: var(--secondary)` handed to the browser as an inline `background` is resolved
 * against **nyx's** stylesheet, where `--secondary` also exists: the swatch then painted nyx's dark
 * grey beside a project token that is a vivid green, and two rows describing ONE token showed two
 * different colours. Worse, a name nyx does not carry at all (`--accent`) painted nothing, so the
 * row read as a token with no value.
 *
 * So the chain is walked HERE, over the layer's own declarations, and the answer is either a literal
 * the project really declares or `null`. `null` means "not resolvable from this layer" — and the
 * caller shows NO swatch, because no swatch beats a false one.
 */
export function resolveTokenValue(layer: TokenLayer, value: string): string | null {
  const byName = new Map(layer.variables);
  let current = value.trim();
  for (let hop = 0; hop < MAX_ALIAS_HOPS; hop += 1) {
    const parts = varParts(current);
    if (parts === null) return current === "" ? null : current;
    const next = byName.get(parts.name);
    // A reference OUT of the layer resolves to its own fallback when it declares one, and to
    // nothing otherwise: the host document is never consulted.
    current = next !== undefined ? next.trim() : parts.fallback;
    if (current === "") return null;
  }
  return null;
}

/**
 * The literal COLOUR a token paints in the project, alias chains followed, or `null` when the layer
 * cannot answer. The only thing a swatch may ever be painted with.
 */
export function resolveTokenColor(layer: TokenLayer, value: string): string | null {
  const resolved = resolveTokenValue(layer, value);
  return resolved !== null && varParts(resolved) === null && isColorValue(resolved)
    ? resolved
    : null;
}

/** The `{ "--name": "value" }` map at `key`, or an empty map (defensive: never throws). */
function variableMap(record: Record<string, unknown>, key: string): Map<string, string> {
  const out = new Map<string, string>();
  const raw = record[key];
  if (raw == null || typeof raw !== "object" || Array.isArray(raw)) return out;
  for (const [name, value] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof value !== "string" && typeof value !== "number") continue;
    const v = String(value).trim();
    const bare = bareName(name);
    if (!bare || !v) continue;
    out.set(`--${bare}`, v);
  }
  return out;
}

/** The two CSS bodies the iframe needs, both possibly empty. */
export interface ThemeStyles {
  /** The lines that go INSIDE `@theme { … }` — what DEFINES the Tailwind utilities. */
  theme: string;
  /** A standalone stylesheet (`:root { … } .dark { … }`) carrying the raw app variables. */
  root: string;
  /**
   * Does [`ThemeStyles.root`] actually carry a `.dark { … }` block — i.e. does the DESIGN SYSTEM
   * ITSELF define a dark variant (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`)?
   *
   * This is what decides whether the frame may put `class="dark"` on its `<html>`. The two answers
   * are not symmetric:
   *
   *   * TRUE — the layer declares `--background`, `--card`, `--foreground`… under `dark`. Marking
   *     the frame dark selects THE VARIANT THE DESIGN SYSTEM WROTE. Nothing is repainted over it.
   *   * FALSE — the layer is light-only. Marking the frame dark would select nothing at all: the
   *     rendering would be byte-identical, so the flag would be a lie with no effect. Worse, the
   *     one thing it WOULD change (`color-scheme: dark`, i.e. native controls and scrollbars) would
   *     put dark widgets on a light canvas. So the frame stays light, and the HOST says so — see
   *     [`themeHasDark`], which is what the catalogue and the detail read to caption the preview.
   */
  hasDark: boolean;
}

/** WHERE in the app a slice of the token layer came from (metadata; never emits CSS). */
export interface TokenSourceRef {
  readonly repository_id: string | null;
  readonly repo: string;
  readonly path: string;
  readonly variable_count: number;
}

/**
 * A candidate stylesheet the ingest did NOT merge, and WHY — the wire shape of
 * `nyx_core::design_tokens::SkippedSheet` (finding `01KYSCX562SWDXZBBAPX397X8F`).
 *
 * The other half of {@link TokenSourceRef}: the layer claims to be THE reference of the project's
 * design system, so "why is MY stylesheet not in there?" has to be as answerable as "where do these
 * tokens come from?". A layer polluted by a `.generated` mirror and a docs theme used to look exactly
 * like a healthy one.
 */
export interface SkippedSheetRef {
  readonly repo: string;
  readonly path: string;
  /**
   * The backend's reason CODE: `recipe_exclude` / `doc_tool_theme` / `foreign_namespace` /
   * `not_a_token_sheet` / `unreadable` / `peripheral_sheet` / `outranked` / `not_ranked`.
   */
  readonly reason: string;
}

/** The parsed token layer the `/design` tokens panel reads. */
export interface TokenLayer {
  readonly sources: readonly TokenSourceRef[];
  /** The light/default theme, `[--name, value]`, name-sorted. */
  readonly variables: readonly (readonly [string, string])[];
  /**
   * The variables that are a bare ALIAS of ANOTHER variable of the SAME layer — name → target name
   * (`--color-secondary` → `--secondary`, the Tailwind-v4 `@theme inline` idiom an app writes so a
   * utility points back at its one raw declaration).
   *
   * They are the SAME token said twice (finding `01KZ3JW5A63TJWX83V8ZKX62CN`: 36 of 103 rows). The
   * layer keeps them — they are what makes `bg-secondary` exist — but a READER is shown the raw
   * declaration alone: an alias row states no value of its own, and the one it seems to state is
   * only readable through {@link resolveTokenValue}.
   */
  readonly aliasOf: ReadonlyMap<string, string>;
  /** The dark-theme overrides, `[--name, value]`, name-sorted. */
  readonly dark: readonly (readonly [string, string])[];
  /**
   * The candidate sheets the ingest DISCARDED, with the backend's reason code — the bounded head of
   * them (see {@link TokenLayer.skippedCount}). Empty when the stored layer carries no provenance
   * report (a hand-authored theme, or a layer ingested before the report existed).
   */
  readonly skipped: readonly SkippedSheetRef[];
  /** How many sheets were discarded in TOTAL (`skipped` is the bounded head). */
  readonly skippedCount: number;
  /**
   * The distinct NAMES of the directories the walk refused to enter (`.generated`, `dist`, …) — the
   * bounded head of them (see {@link TokenLayer.prunedDirsCount}).
   */
  readonly prunedDirs: readonly string[];
  /**
   * How many directory names were pruned in TOTAL (`prunedDirs` is the bounded head). The twin of
   * {@link TokenLayer.skippedCount}: this list is capped too, and a truncation may never be
   * displayed as a total (finding `01KYSNRNY550NS778YCMC8CZGH`).
   */
  readonly prunedDirsCount: number;
  /**
   * Does the stored layer carry a provenance report AT ALL (a `skipped` / `pruned_dirs` key)? It is
   * the difference between "nothing was discarded" and "nobody recorded what was discarded" — the
   * panel must not present the second as the first.
   */
  readonly provenanceReported: boolean;
}

/**
 * Parse a stored `theme_tokens_json` into the layer the tokens panel reads. DEFENSIVE: a
 * blank/malformed/legacy blob yields an empty layer rather than throwing, so a hand-written theme
 * never breaks the catalogue.
 */
export function parseTokenLayer(themeTokensJson: string | null | undefined): TokenLayer {
  const empty: TokenLayer = {
    sources: [],
    variables: [],
    aliasOf: new Map(),
    dark: [],
    skipped: [],
    skippedCount: 0,
    prunedDirs: [],
    prunedDirsCount: 0,
    provenanceReported: false,
  };
  if (!themeTokensJson || !themeTokensJson.trim()) return empty;
  let parsed: unknown;
  try {
    parsed = JSON.parse(themeTokensJson);
  } catch {
    return empty;
  }
  if (parsed == null || typeof parsed !== "object" || Array.isArray(parsed)) return empty;
  const record = parsed as Record<string, unknown>;
  const sources: TokenSourceRef[] = Array.isArray(record.sources)
    ? (record.sources as unknown[]).flatMap((entry) => {
        if (entry == null || typeof entry !== "object" || Array.isArray(entry)) return [];
        const o = entry as Record<string, unknown>;
        if (typeof o.path !== "string") return [];
        return [
          {
            repository_id: typeof o.repository_id === "string" ? o.repository_id : null,
            repo: typeof o.repo === "string" ? o.repo : "",
            path: o.path,
            variable_count: typeof o.variable_count === "number" ? o.variable_count : 0,
          },
        ];
      })
    : [];
  // The ingest run's PROVENANCE report, when the stored layer carries one: what was NOT merged and
  // where the walk did not go. Read exactly as tolerantly as `sources` — a layer written by hand has
  // none of it, and that is not an error.
  const skipped: SkippedSheetRef[] = Array.isArray(record.skipped)
    ? (record.skipped as unknown[]).flatMap((entry) => {
        if (entry == null || typeof entry !== "object" || Array.isArray(entry)) return [];
        const o = entry as Record<string, unknown>;
        if (typeof o.path !== "string" || typeof o.reason !== "string") return [];
        return [
          {
            repo: typeof o.repo === "string" ? o.repo : "",
            path: o.path,
            reason: o.reason,
          },
        ];
      })
    : [];
  const prunedDirs: string[] = Array.isArray(record.pruned_dirs)
    ? (record.pruned_dirs as unknown[]).flatMap((dir) =>
        typeof dir === "string" && dir.trim() !== "" ? [dir] : [],
      )
    : [];
  // A stored TOTAL only ever RAISES the count: it is what says "the list you are holding is a head".
  // A blob without one (or with a nonsense one) falls back to the length, which is then exact.
  const skippedCount =
    typeof record.skipped_count === "number" && record.skipped_count >= skipped.length
      ? record.skipped_count
      : skipped.length;
  const prunedDirsCount =
    typeof record.pruned_dirs_count === "number" && record.pruned_dirs_count >= prunedDirs.length
      ? record.pruned_dirs_count
      : prunedDirs.length;
  const sorted = (m: Map<string, string>): (readonly [string, string])[] =>
    [...m.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  const variables = variableMap(record, "variables");
  // THE SAME TOKEN SAID TWICE: a variable whose whole value is `var(--other)`, `--other` being
  // declared right here. It adds no value, only a second name for one.
  const aliasOf = new Map<string, string>();
  for (const [name, value] of variables) {
    const target = varReference(value);
    if (target !== null && target !== name && variables.has(target)) aliasOf.set(name, target);
  }
  return {
    sources,
    variables: sorted(variables),
    aliasOf,
    dark: sorted(variableMap(record, "dark")),
    skipped,
    skippedCount,
    prunedDirs,
    prunedDirsCount,
    provenanceReported: Array.isArray(record.skipped) || Array.isArray(record.pruned_dirs),
  };
}

/**
 * Build the iframe's theme stylesheet from a `theme_tokens_json` string: the `@theme` body AND the
 * `:root` / `.dark` body. Returns empty bodies for null/blank/invalid input so the frame still
 * renders (Tailwind's default theme), never throwing on a malformed token blob.
 *
 * The INGESTED layer (`variables` / `dark`) is split the way Tailwind v4 expects:
 *   * a name already in a Tailwind namespace (`--color-*`, `--font-*`, `--radius*`, …) is a theme
 *     entry → emitted verbatim inside `@theme` (Tailwind re-emits it on `:root` itself);
 *   * any other name is a RAW app variable → emitted on `:root`, and — when its value is a colour —
 *     ALIASED into the theme as `--color-<name>: var(--<name>)`, so `bg-<name>` exists and points
 *     BACK at the single raw declaration instead of duplicating its value.
 * An alias is never emitted over a theme entry the app declared itself, so the app's own
 * `@theme inline { --color-primary: var(--primary) }` always wins.
 */
export function buildThemeStyles(themeTokensJson: string | null | undefined): ThemeStyles {
  const empty: ThemeStyles = { theme: "", root: "", hasDark: false };
  if (!themeTokensJson || !themeTokensJson.trim()) return empty;
  let tokens: unknown;
  try {
    tokens = JSON.parse(themeTokensJson);
  } catch {
    return empty;
  }
  if (tokens == null || typeof tokens !== "object" || Array.isArray(tokens)) return empty;
  const record = tokens as Record<string, unknown>;

  const themeLines: string[] = [];
  const rootLines: string[] = [];
  const darkLines: string[] = [];

  // --- the INGESTED layer (review #42) ---
  const variables = variableMap(record, "variables");
  const dark = variableMap(record, "dark");
  const themeNames = new Set<string>();
  for (const [name] of variables) {
    if (isThemeNamespaced(name)) themeNames.add(name);
  }
  for (const [name, value] of variables) {
    if (isThemeNamespaced(name)) {
      themeLines.push(`  ${name}: ${value};`);
    } else {
      rootLines.push(`  ${name}: ${value};`);
      const alias = `--color-${bareName(name)}`;
      if (isColorValue(value) && !themeNames.has(alias)) {
        themeNames.add(alias);
        // REFERENCED, not copied: the utility points at the single raw declaration.
        themeLines.push(`  ${alias}: var(${name});`);
      }
    }
  }
  // A dark override re-declares the RAW variable under `.dark`; the utilities alias `var(--x)`, so
  // they follow automatically — nothing to duplicate in `@theme`.
  for (const [name, value] of dark) {
    darkLines.push(`  ${name}: ${value};`);
  }

  // --- the escape hatch + the legacy namespaced maps ---
  if (typeof record.css === "string" && record.css.trim()) {
    themeLines.push(record.css.trim());
  } else {
    for (const [group, value] of Object.entries(record)) {
      if (value == null || RESERVED_KEYS.has(group)) continue;
      if (typeof value === "object" && !Array.isArray(value)) {
        const prefix = NAMESPACE[group] ?? kebab(group);
        emitGroup(prefix, value as Record<string, unknown>, themeLines);
      } else {
        // A top-level scalar like `{ "radius": "0.5rem" }` → `--radius`.
        const name = NAMESPACE[group] ?? kebab(group);
        const line = decl(name, value);
        if (line) themeLines.push(line);
      }
    }
  }

  const root = [
    rootLines.length ? `:root {\n${rootLines.join("\n")}\n}` : "",
    darkLines.length ? `.dark {\n${darkLines.join("\n")}\n}` : "",
  ]
    .filter(Boolean)
    .join("\n");

  return { theme: themeLines.join("\n"), root, hasDark: darkLines.length > 0 };
}

/**
 * Does this stored token layer define a DARK VARIANT of its own (finding
 * `01KYSVEZMJP1Z8V7SYJ8YC5H9W`)? A thin read over [`buildThemeStyles`], for the HOSTS: the frame
 * follows the app's appearance when the answer is yes, and when it is no the surface has to SAY that
 * what it shows is the light variant — never let a light rendering pass for the app's own theme.
 */
export function themeHasDark(themeTokensJson: string | null | undefined): boolean {
  return buildThemeStyles(themeTokensJson).hasDark;
}

/**
 * The `@theme` body alone — the historical entry point, kept so callers that only theme the frame
 * (and every existing test) read unchanged. Prefer [`buildThemeStyles`] when you also need the raw
 * `:root` variables the utilities point at.
 */
export function buildThemeCss(themeTokensJson: string | null | undefined): string {
  return buildThemeStyles(themeTokensJson).theme;
}
