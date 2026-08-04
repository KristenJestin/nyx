//! The DESIGN TOKENS layer (v2.7 — Prototype : machinerie, review #42) — the project's theme
//! variables ingested BY REFERENCE from the real app, above the [`crate::db`] `design` authority.
//!
//! ## Why this module exists (the "aperçu gris")
//!
//! On a real Angular front, the style lives entirely in two places: the Tailwind CLASSES on the
//! markup (`.variants.ts` CVA + the template's `[class]`) and the THEME TOKENS the classes resolve
//! against (`--primary`, `--background`, …, declared in the app's `globals.css` / `styles.css` /
//! `@theme` block). nyx read NEITHER: `design.theme_tokens_json` existed but nothing ever
//! populated it, so the preview iframe got an EMPTY `@theme` — `bg-primary` resolved to nothing and
//! every rendu came out GREY. The classes half is the BUNDLE pivot's job (the agent sees the
//! `.variants.ts` and inlines the concrete classes); THIS module is the tokens half.
//!
//! ## Referenced, never copied (edit-once-apply-everywhere)
//!
//! A token layer is a PROJECT-level singleton — ONE row (`design.theme_tokens_json`), for the whole
//! catalogue. A component NEVER carries a copy of `primary`: its template carries the CLASS
//! (`bg-primary`), and the single token layer gives that class its colour. So `primary` has ONE
//! identity: change it once (or re-ingest it from the app) and every component, every composition
//! and every preview follows. The layer also RECORDS WHERE IT CAME FROM (`sources[]`: repo +
//! repo-relative path), so the reference back to the app's own stylesheet stays explicit and
//! re-syncable — the app's file remains the authority, nyx holds a re-derivable projection of it.
//!
//! ## ONE tree, ONE set of rules (finding 01KYSCX562SWDXZBBAPX397X8F)
//!
//! This module sweeps the SAME arborescence as the component discovery, so it applies the SAME
//! exclusions, through the same code: the project's recipe decides which directories are never
//! entered ([`crate::design::walk_pruned_dirs`] — its `ignored_dirs` plus the floor no recipe can
//! lift) and which paths are excluded (`recipe.exclude`, matched by the recipe's own matcher). It
//! used to carry a hard-coded list of its own that did not know `.generated`, and a real front got a
//! GENERATED mirror of its stylesheet plus a VitePress docs theme (`--vp-*`) merged into the layer
//! that is supposed to BE its design system — while the discovery of that same tree pruned both.
//! The ranking says the rest ([`SheetTier`]): a documentation / example / gallery sheet can never
//! outrank the application's own, and a sheet whose variables ARE a third-party namespace is not a
//! candidate at all. Every run reports what it took AND what it refused
//! ([`TokenIngest::sources`] / [`TokenIngest::skipped`] / [`TokenIngest::pruned_dirs`]) — and the
//! LAYER ITSELF carries that report ([`TokenLayer::provenance`]), because the only surface that
//! shows it may not run an ingest to obtain it (finding 01KYSK7GS9C3ZRJYW00ZA0H6GF). INCLUDING a run
//! that retained no sheet at all (finding 01KYSNVW31C0E8YQPET0505HW2): that one stores no token, so
//! everything renders grey and the report is the ONLY diagnostic there is — the report is written,
//! and only the report ([`blob_with_provenance`]), which is what keeps a hand-authored theme safe.
//!
//! ## Best-effort, dependency-free extraction
//!
//! No CSS parser is pulled in (PRD constraint): [`extract_css_variables`] is a small hand-rolled
//! scanner that strips comments, tracks the block nesting, and keeps the CUSTOM-PROPERTY
//! DECLARATIONS found in a GLOBAL scope (`:root` / `html` / `body` / `*` / `:host` / a bare
//! `@theme` or `@layer` block), separating the DARK ones (`.dark`, `[data-theme="dark"]`,
//! `prefers-color-scheme: dark`). Declarations scoped to a component selector are IGNORED — they
//! are not the design system. Everything the scanner cannot understand is skipped, never guessed:
//! a partial layer that renders beats a clever one that throws.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use diesel::{QueryResult, SqliteConnection};
use serde_json::{json, Value};

use crate::db;
use crate::design::BundleRecipe;

/// How many bytes of ONE candidate stylesheet are read. A design-token sheet is a few KiB; this
/// only stops a pathological generated CSS bundle from being slurped into memory.
const MAX_STYLESHEET_BYTES: usize = 512 * 1024;

/// How deep the candidate walk descends from a repo root. Token sheets live near the top
/// (`src/styles.css`, `src/app/globals.css`, `src/styles/tokens.css`); deeper is noise.
const MAX_WALK_DEPTH: usize = 6;

/// How many candidate stylesheets one repo contributes at most (the best-RANKED ones — see
/// [`ranking_key`]). The ones beyond it are reported as `not_ranked`, never silently dropped.
const MAX_CANDIDATES_PER_REPO: usize = 64;

/// How many stylesheets are MERGED into the project's token layer, best-RANKED first. A design
/// system routinely splits `tokens.css` + `theme.css`; beyond a handful it is app CSS, not tokens.
/// The ones that do not make the cut are reported as `outranked`.
const MAX_MERGED_SOURCES: usize = 4;

/// The minimum number of GLOBAL custom properties a file must declare to count as a TOKEN sheet.
/// One stray `--gap` in a page stylesheet is not a design system.
const MIN_TOKEN_VARS: usize = 3;

/// How many DISCARDED candidates the run REPORTS (the total is always reported alongside). The point
/// is to show WHERE the tokens did not come from; a full listing of every stylesheet of a monorepo
/// would be noise, and the response has a budget — and so has the PERSISTED layer, which carries the
/// very same bounded head ([`TokenProvenance`]).
const MAX_SKIPPED_REPORTED: usize = 24;

/// How many PRUNED DIRECTORY NAMES the run reports (the total is always reported alongside). It
/// exists because the report is now STORED: a token blob is a reference, not a journal, and nothing
/// (a hand-edited blob included) may grow it without bound.
///
/// IT CAN BITE, and that is why the total travels with it. The names are the recipe's `ignored_dirs`
/// plus the floor ([`crate::design::MANDATORY_IGNORED_DIRS`], two) plus [`TOKENS_ONLY_SKIPPED_DIRS`]
/// (five), and `ignored_dirs` is a field THE AGENT WRITES, capped at 64 entries by the recipe itself
/// (`design::recipe::MAX_IGNORED_DIRS`): 64 + 2 + 5 is over this ceiling, so a recipe that fills its
/// quota on a repo that really carries those directories loses names here. The former justification
/// — "a vocabulary of a couple of dozen entries, so this never bites on a real project" — was simply
/// false (finding `01KYSNRNY550NS778YCMC8CZGH`). A truncation may never become an UNDER-DECLARATION,
/// so [`TokenProvenance::pruned_dirs_count`] carries the real total exactly as `skipped_count` does
/// for the discarded sheets, and the panel says "… et N autre(s)".
const MAX_PRUNED_DIRS_REPORTED: usize = 64;

/// Directory names the token walk prunes IN ADDITION to the project's own
/// ([`crate::design::walk_pruned_dirs`], which is the recipe's `ignored_dirs` + the floor).
///
/// WHY AN ADDITION AT ALL (finding 01KYSCX562SWDXZBBAPX397X8F). The exclusions the discovery applies
/// are now applied here through the SAME path — that is the finding. These five names are what the
/// token walk pruned BEFORE it and the component discovery does not: dropping them would have
/// RE-OPENED a pollution the walk already avoided (a `storybook-static` build, a Python `.venv`'s
/// bundled CSS), and adding them to [`crate::design::IGNORED_DIRS`] instead would change which
/// COMPONENTS every project discovers — a population change, in a fix about tokens. They are a floor
/// UNDER the recipe, never a way to contradict it: a project that legitimately keeps its design
/// system in one of these names is the one case that stays unreachable here, and the run REPORTS the
/// names it pruned ([`TokenIngest::pruned_dirs`]) instead of leaving the absence unexplained.
const TOKENS_ONLY_SKIPPED_DIRS: &[&str] =
    &["vendor", ".venv", "venv", "storybook-static", "__pycache__"];

/// Directory names that mark ANOTHER TOOL's own theme — a documentation site, a component gallery, a
/// generated static site. A stylesheet under one of these is NOT the application's design system and
/// is never merged into the layer, whatever its file name is called.
///
/// The tester's case (finding 01KYSCX562SWDXZBBAPX397X8F): the ingest merged the VitePress docs
/// theme of `docs/.vitepress/`, so the project's design-system layer — the layer that is supposed to
/// be THE single reference — carried `--vp-*` and `--vp-custom-block-*` variables no component of the
/// app has ever heard of.
const DOC_TOOL_DIRS: &[&str] = &[
    ".vitepress",
    ".vuepress",
    ".docusaurus",
    ".storybook",
    ".astro",
    "_site",
];

/// Directory names that make a stylesheet PERIPHERAL: a documentation / example / demo tree. Such a
/// sheet is a legitimate stylesheet — it is just not the app's, so it can never OUTRANK an
/// application one (see [`SheetTier`]). It is only used when the project has no application
/// stylesheet at all (a docs-only repo), and the report then says where the layer came from.
const PERIPHERAL_DIRS: &[&str] = &[
    "docs",
    "doc",
    "documentation",
    "website",
    "site",
    "example",
    "examples",
    "demo",
    "demos",
    "playground",
    "sandbox",
    "fixtures",
    "stories",
    "storybook",
];

/// Custom-property NAMESPACES that belong to a third-party tool's own theme. A sheet whose global
/// custom properties are MOSTLY one of these declares that tool's theme, not the project's design
/// system — and no file name / directory heuristic can catch it (the VitePress theme of the finding
/// sat in a perfectly ordinary `custom.css`). Content decides.
const FOREIGN_TOKEN_PREFIXES: &[&str] = &[
    "--vp-",         // VitePress (incl. `--vp-custom-block-*`)
    "--vt-",         // VitePress' twin in the Vitest UI
    "--ifm-",        // Docusaurus (Infima)
    "--docusaurus-", // Docusaurus' own
    "--sb-",         // Storybook
    "--nextra-",     // Nextra
    "--rspress-",    // Rspress
    "--docz-",       // Docz
];

/// The share of a sheet's global custom properties that must carry ONE [`FOREIGN_TOKEN_PREFIXES`]
/// namespace for the sheet to BE that tool's theme. Half is deliberately generous: an app stylesheet
/// that happens to define a couple of `--vp-*` overrides for its own docs embed stays a candidate.
const FOREIGN_NAMESPACE_SHARE: f64 = 0.5;

// =============================================================================
// THE CSS SCANNER (pure, dependency-free)
// =============================================================================

/// Which THEME a custom-property declaration belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// The default/light theme (`:root`, `html`, `@theme`, `@layer base :root`, …).
    Root,
    /// The dark theme (`.dark`, `[data-theme="dark"]`, `@media (prefers-color-scheme: dark)`).
    Dark,
}

/// ONE custom-property declaration the scanner kept, with its leading `--`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssVariable {
    /// The property name INCLUDING its `--` prefix (`--primary`), so identity is unambiguous.
    pub name: String,
    /// The declared value, verbatim and trimmed (`oklch(0.21 0.006 285.885)`, `var(--brand)`).
    pub value: String,
    /// Which theme the declaration was found in.
    pub scope: TokenScope,
}

/// What a `{`-introducing prelude means for the declarations inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    /// A global scope (`:root`, `html`, `*`, …) — its declarations ARE design tokens.
    Global,
    /// A dark-theme scope.
    Dark,
    /// A component/page scope — its declarations are NOT design tokens.
    Local,
    /// A block whose declarations are never tokens (`@keyframes`, `@font-face`, `@property`).
    Ignored,
    /// A grouping at-rule (`@theme`, `@layer`, `@media`, `@supports`, `@container`): it does not
    /// itself decide the scope, so the declarations directly inside it inherit the enclosing one
    /// (and at the top level that is GLOBAL — a bare `@theme { --color-primary: … }` is tokens).
    Transparent,
}

/// The index just PAST the string literal that starts at `at` (a quote byte), handling `\` escapes.
fn skip_string(b: &[u8], at: usize) -> usize {
    let quote = b[at];
    let mut i = at + 1;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// Drop every `/* … */` comment, leaving string literals intact. Byte-level but UTF-8 safe: every
/// delimiter it keys on is ASCII, which can never occur inside a multi-byte sequence, and any other
/// byte is copied through verbatim.
fn strip_css_comments(css: &str) -> String {
    let b = css.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            out.push(b' ');
        } else if b[i] == b'"' || b[i] == b'\'' {
            let end = skip_string(b, i);
            out.extend_from_slice(&b[i..end]);
            i = end;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What ONE simple selector (`html`, `.dark`, `[data-theme="dark"]`) means for the scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimpleKind {
    /// Selects the document itself (`:root`, `html`, `body`, `*`, `:host`).
    Global,
    /// The DARK-THEME marker (`.dark`, `[data-theme="dark"]`).
    DarkMarker,
    /// Anything else — a component/page selector.
    Other,
}

/// Split ONE compound selector (`html.dark[data-x="y"]`) into its SIMPLE selectors
/// (`html`, `.dark`, `[data-x="y"]`). Bracketed attribute selectors are kept whole, so a `.` or `:`
/// inside a value never splits one. Best-effort like the rest of the scanner: anything exotic ends
/// up as an opaque simple selector, which classifies as `Other` (i.e. NOT the design system).
fn simple_selectors(compound: &str) -> Vec<&str> {
    let b = compound.as_bytes();
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'[' => {
                if i > start {
                    out.push(&compound[start..i]);
                }
                start = i;
                i += 1;
                while i < b.len() && b[i] != b']' {
                    // A `]` inside a quoted value does not close the attribute selector.
                    if b[i] == b'"' || b[i] == b'\'' {
                        i = skip_string(b, i);
                        continue;
                    }
                    i += 1;
                }
                i = (i + 1).min(b.len());
                out.push(&compound[start..i]);
                start = i;
            }
            b'.' | b'#' | b':' => {
                if i > start {
                    out.push(&compound[start..i]);
                }
                start = i;
                i += 1;
                // `::before` — the double colon introduces ONE simple selector.
                if i < b.len() && b[i] == b':' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    if start < b.len() {
        out.push(&compound[start..]);
    }
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// Classify ONE simple selector. The DARK marker is matched on the WHOLE selector, never as a
/// substring (review #42): `.dark` is the theme, `.dark-panel` / `.btn-darken` are components, and
/// an attribute marker must carry EXACTLY the `dark` value (`[data-theme="darkroom"]` is not it).
fn classify_simple(simple: &str) -> SimpleKind {
    let s = simple.trim();
    if matches!(s, ":root" | ":host" | "html" | "body" | "*") {
        return SimpleKind::Global;
    }
    if s == ".dark" {
        return SimpleKind::DarkMarker;
    }
    if let Some(inner) = s.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        // `[data-theme="dark"]`, `[data-mode='dark']`, `[data-theme=dark]` — EXACT match only, so
        // the `~=` / `^=` / `*=` operators (and any other value) are not the dark root.
        if let Some((attr, value)) = inner.split_once('=') {
            let exact = !attr.ends_with(['~', '|', '^', '$', '*']);
            let value = value.trim().trim_matches(['"', '\'']).trim();
            if exact && value == "dark" {
                return SimpleKind::DarkMarker;
            }
        }
    }
    SimpleKind::Other
}

/// What ONE selector of a comma-separated list means for the declarations it introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectorKind {
    /// The declarations land on the document (light/default theme).
    Global,
    /// The declarations land on the document, under the DARK theme.
    Dark,
    /// The declarations land on a component/page element — NOT the design system.
    Local,
}

/// Classify ONE selector (`html.dark body`, `.dark .invoice-card`, `:root`). EVERY compound of the
/// chain must be a document scope or a dark marker: as soon as one names a component the whole
/// selector is `Local` — `.dark .invoice-card` declares on a CARD, not on the theme. A dark marker
/// anywhere in an otherwise global chain makes it `Dark` (`.dark`, `html.dark`, `html.dark body`).
fn classify_selector(selector: &str) -> SelectorKind {
    let compounds: Vec<&str> = selector
        .split(|c: char| c.is_whitespace() || c == '>' || c == '+' || c == '~')
        .filter(|s| !s.is_empty())
        .collect();
    if compounds.is_empty() {
        return SelectorKind::Local;
    }
    let mut saw_dark = false;
    for compound in compounds {
        let simples = simple_selectors(compound);
        if simples.is_empty() {
            return SelectorKind::Local;
        }
        let mut compound_dark = false;
        for simple in simples {
            match classify_simple(simple) {
                SimpleKind::Global => {}
                SimpleKind::DarkMarker => compound_dark = true,
                SimpleKind::Other => return SelectorKind::Local,
            }
        }
        saw_dark |= compound_dark;
    }
    if saw_dark {
        SelectorKind::Dark
    } else {
        SelectorKind::Global
    }
}

/// Does this `@media` prelude select the DARK colour scheme? `@media (prefers-color-scheme: dark)`
/// and its `and`-combinations do; a NEGATED query (`@media not all and (prefers-color-scheme:
/// dark)`) selects the LIGHT one and must NOT be mistaken for it.
fn is_dark_media_query(lower: &str) -> bool {
    const FEATURE: &str = "prefers-color-scheme:dark";
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(at) = compact.find(FEATURE) else {
        return false;
    };
    // `prefers-color-scheme: darkroom` is not the dark scheme (the value must END there).
    if compact[at + FEATURE.len()..]
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return false;
    }
    !lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .any(|word| word == "not")
}

/// Classify the text preceding a `{` into the scope its declarations live in.
fn classify_prelude(prelude: &str) -> BlockKind {
    let trimmed = prelude.trim();
    let lower = trimmed.to_lowercase();
    if lower.starts_with('@') {
        // At-rules that can never hold design tokens.
        for ignored in [
            "@keyframes",
            "@font-face",
            "@property",
            "@counter-style",
            "@page",
        ] {
            if lower.starts_with(ignored) {
                return BlockKind::Ignored;
            }
        }
        if is_dark_media_query(&lower) {
            return BlockKind::Dark;
        }
        // `@theme`, `@theme inline`, `@layer base`, `@media (min-width: …)`, `@supports …`:
        // grouping only — the enclosing scope decides.
        return BlockKind::Transparent;
    }
    if trimmed.is_empty() {
        return BlockKind::Local;
    }
    // A selector LIST applies its declarations to EVERY selector it names, so one GLOBAL member is
    // enough to make them design tokens: `:root, .dark-panel { --primary }` really does declare
    // `--primary` on the document (light identity), and the component member changes nothing.
    let mut saw_global = false;
    let mut saw_dark = false;
    for part in lower.split(',') {
        match classify_selector(part.trim()) {
            SelectorKind::Global => saw_global = true,
            SelectorKind::Dark => saw_dark = true,
            SelectorKind::Local => {}
        }
    }
    match (saw_global, saw_dark) {
        (true, _) => BlockKind::Global,
        (false, true) => BlockKind::Dark,
        (false, false) => BlockKind::Local,
    }
}

/// The scope a declaration at the CURRENT nesting sits in, or `None` when it must be skipped.
/// `Transparent` levels delegate to their parent; the top level is global (a declaration can only
/// be reached through at least one block, and a bare `@theme` block is the design system).
fn effective_scope(stack: &[BlockKind]) -> Option<TokenScope> {
    let mut scope = TokenScope::Root;
    for kind in stack {
        match kind {
            BlockKind::Ignored | BlockKind::Local => return None,
            BlockKind::Dark => scope = TokenScope::Dark,
            BlockKind::Global | BlockKind::Transparent => {}
        }
    }
    Some(scope)
}

/// The declaration VALUE starting at `from`, and the index of its terminator (`;` or `}` at paren
/// depth 0, or end of input). Parens and string literals are respected, so
/// `calc(var(--radius) - 4px)` and `"a;b"` survive intact.
fn read_value(css: &str, from: usize) -> (String, usize) {
    let b = css.as_bytes();
    let mut i = from;
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = skip_string(b, i),
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b';' | b'}' if depth == 0 => break,
            _ => i += 1,
        }
    }
    (css[from..i].trim().to_string(), i)
}

/// Extract the GLOBAL custom-property declarations of a stylesheet, best-effort and dependency-free
/// (PRD: no CSS parser dependency). Comments are stripped, blocks are tracked, and only the
/// declarations whose enclosing scope is global (or dark-global) are kept — in SOURCE ORDER, so a
/// later re-declaration of the same name naturally wins when the caller folds them into a map.
///
/// PURE: no disk, no DB. A malformed sheet yields whatever was understandable, never a panic.
pub fn extract_css_variables(css: &str) -> Vec<CssVariable> {
    let source = strip_css_comments(css);
    let b = source.as_bytes();
    let mut out: Vec<CssVariable> = Vec::new();
    let mut stack: Vec<BlockKind> = Vec::new();
    let mut prelude_start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = skip_string(b, i),
            b'{' => {
                stack.push(classify_prelude(&source[prelude_start..i]));
                i += 1;
                prelude_start = i;
            }
            b'}' => {
                stack.pop();
                i += 1;
                prelude_start = i;
            }
            b';' => {
                i += 1;
                prelude_start = i;
            }
            // A custom property STARTS a declaration only when nothing precedes it in this
            // statement — that is what tells `--primary: …` apart from the `.btn--primary` of a
            // selector (a `var(--x)` inside a value never reaches here: `read_value` ate it).
            b'-' if i + 1 < b.len()
                && b[i + 1] == b'-'
                && !stack.is_empty()
                && source[prelude_start..i].trim().is_empty() =>
            {
                let mut j = i;
                while j < b.len() && !matches!(b[j], b':' | b';' | b'{' | b'}') {
                    j += 1;
                }
                if j >= b.len() || b[j] != b':' {
                    i = j;
                    continue;
                }
                let name = source[i..j].trim().to_string();
                let (value, end) = read_value(&source, j + 1);
                if let Some(scope) = effective_scope(&stack) {
                    if name.len() > 2 && !value.is_empty() {
                        out.push(CssVariable { name, value, scope });
                    }
                }
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

// =============================================================================
// THE PERSISTED TOKEN LAYER
// =============================================================================

/// WHERE a slice of the token layer came from — the REFERENCE back to the app's own stylesheet.
/// Kept so the layer stays re-syncable (and auditable) instead of being an opaque copy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TokenSource {
    /// The provenance repository id — `None` for the current-folder fallback root.
    pub repository_id: Option<String>,
    /// The repo's human label (what the `/design` UI shows).
    pub repo: String,
    /// The stylesheet's path RELATIVE to the repo root, `/`-separated.
    pub path: String,
    /// How many global custom properties this sheet contributed.
    pub variable_count: usize,
}

/// A candidate the ingest did NOT merge, and WHY — the other half of [`TokenLayer::sources`].
///
/// The layer is supposed to be THE reference of the design system, so "where did these tokens come
/// from?" must be answerable from the LAYER alone — and so must "why is MY stylesheet not in
/// there?" (finding 01KYSCX562SWDXZBBAPX397X8F: the run reported only what it took, so a layer
/// polluted by a `.generated` mirror and a docs theme looked exactly like a healthy one).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkippedSheet {
    /// The repo's human label.
    pub repo: String,
    /// The stylesheet's repo-relative, `/`-separated path.
    pub path: String,
    /// WHY it was not merged — one of [`SKIP_REASONS`]:
    /// * `recipe_exclude` — the project's recipe `exclude` globs match it (the discovery's own
    ///   exclusion, now applied here too);
    /// * `doc_tool_theme` — it lives in another tool's theme directory ([`DOC_TOOL_DIRS`]);
    /// * `foreign_namespace` — its variables are mostly a third-party namespace (`--vp-*`, …);
    /// * `not_a_token_sheet` — fewer than [`MIN_TOKEN_VARS`] GLOBAL custom properties;
    /// * `unreadable` — the file could not be read (best-effort: skipped, never fatal);
    /// * `peripheral_sheet` — a docs / example / demo sheet, while the project HAS an application
    ///   stylesheet ([`SheetTier`]);
    /// * `outranked` — a real candidate that lost to the better-ranked ones
    ///   ([`MAX_MERGED_SOURCES`]);
    /// * `not_ranked` — beyond [`MAX_CANDIDATES_PER_REPO`], so it was never even opened.
    pub reason: &'static str,
}

/// The CLOSED vocabulary of discard reasons (see [`SkippedSheet::reason`]). It is a vocabulary and
/// not free text because the codes are read back OUT of the stored blob: a reason is a term the
/// `/design` panel translates and the `design_tokens` tool answers with, so a hand-edited blob may
/// not smuggle an arbitrary string into it.
const SKIP_REASONS: &[&str] = &[
    "recipe_exclude",
    "doc_tool_theme",
    "foreign_namespace",
    "not_a_token_sheet",
    "unreadable",
    "peripheral_sheet",
    "outranked",
    "not_ranked",
];

/// The reason code `code` names, or `None` when it is not one of ours — how a stored report is read
/// back into `&'static str` codes without trusting the blob.
fn skip_reason(code: &str) -> Option<&'static str> {
    SKIP_REASONS.iter().copied().find(|reason| *reason == code)
}

/// WHAT THE INGEST REFUSED — persisted WITH the layer, not merely returned by the run.
///
/// A token layer is a claim: "this IS the project's design system". The report that keeps the claim
/// honest (finding 01KYSCX562SWDXZBBAPX397X8F) used to exist only inside the `design_tokens` INGEST
/// RESPONSE, so it lasted exactly one tool call: [`to_json`](TokenLayer::to_json) emitted
/// `{ source, sources, variables, dark }` and nothing else. The `/design` tokens panel may NOT
/// trigger an ingest — the agent is the only pilot of that step (finding
/// 01KYKX7HBP4ZSSS6ZDM31T6A5E) — so the stored blob is the ONLY place it can read this from, and it
/// could only display "non enregistrés". Storing the report is what makes the signal outlive the run
/// (finding 01KYSK7GS9C3ZRJYW00ZA0H6GF).
///
/// BOUNDED exactly as the response is ([`MAX_SKIPPED_REPORTED`] / [`MAX_PRUNED_DIRS_REPORTED`]):
/// a token blob is a reference, not a journal.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct TokenProvenance {
    /// The discarded sheets with their reason code, most informative first — the bounded HEAD of
    /// `skipped_count` of them.
    pub skipped: Vec<SkippedSheet>,
    /// How many sheets were discarded in TOTAL (`skipped` is the bounded head), so a truncated list
    /// never reads as a short one.
    pub skipped_count: usize,
    /// The distinct NAMES of the directories the walk refused to enter — the bounded HEAD of
    /// `pruned_dirs_count` of them. `.generated` in this list is the proof the generated mirror of a
    /// stylesheet was not swept.
    pub pruned_dirs: Vec<String>,
    /// How many directory names were pruned in TOTAL (`pruned_dirs` is the bounded head), so a
    /// truncated list never reads as a short one — the exact twin of `skipped_count`
    /// (finding `01KYSNRNY550NS778YCMC8CZGH`, see [`MAX_PRUNED_DIRS_REPORTED`]).
    pub pruned_dirs_count: usize,
}

impl TokenProvenance {
    /// The report, BOUNDED. `total` / `pruned_total` are how many sheets and directory names were
    /// REALLY discarded — never the length of the head that fits — so applying either cap can never
    /// turn into under-reporting.
    fn bounded(
        mut skipped: Vec<SkippedSheet>,
        total: usize,
        mut pruned_dirs: Vec<String>,
        pruned_total: usize,
    ) -> Self {
        let skipped_count = total.max(skipped.len());
        let pruned_dirs_count = pruned_total.max(pruned_dirs.len());
        skipped.truncate(MAX_SKIPPED_REPORTED);
        pruned_dirs.truncate(MAX_PRUNED_DIRS_REPORTED);
        Self {
            skipped,
            skipped_count,
            pruned_dirs,
            pruned_dirs_count,
        }
    }
}

/// The project's token layer, as persisted in `design.theme_tokens_json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenLayer {
    /// The app stylesheets the layer was derived from, best-ranked first (may be empty when the
    /// layer was authored by hand rather than ingested).
    pub sources: Vec<TokenSource>,
    /// The default/light theme variables, keyed by their full `--name`.
    pub variables: BTreeMap<String, String>,
    /// The dark-theme overrides, keyed by their full `--name`.
    pub dark: BTreeMap<String, String>,
    /// What the ingest that produced this layer REFUSED ([`TokenProvenance`]) — `None` when the
    /// stored blob carries no report at all (a hand-authored theme, or a layer written before the
    /// report was persisted).
    ///
    /// `None` and `Some(empty)` are DIFFERENT claims: "nobody recorded what was discarded" is not
    /// "nothing was discarded", and showing the second in place of the first is precisely how a
    /// polluted layer passed for a healthy one. The panel renders them differently, so the
    /// distinction is carried, never flattened.
    pub provenance: Option<TokenProvenance>,
}

impl TokenLayer {
    /// Read a stored `theme_tokens_json` back into a layer. TOLERANT: a blank / malformed / legacy
    /// blob (the untyped `{ "colors": … }` Phase-1 shape) yields an EMPTY layer rather than an
    /// error, so a hand-written theme never breaks the ingest or the editor.
    pub fn parse(tokens_json: Option<&str>) -> Self {
        let Some(raw) = tokens_json.map(str::trim).filter(|s| !s.is_empty()) else {
            return Self::default();
        };
        let Ok(Value::Object(root)) = serde_json::from_str::<Value>(raw) else {
            return Self::default();
        };
        let map_of = |key: &str| -> BTreeMap<String, String> {
            match root.get(key) {
                Some(Value::Object(m)) => m
                    .iter()
                    .filter_map(|(k, v)| match v {
                        Value::String(s) => Some((k.clone(), s.clone())),
                        Value::Number(n) => Some((k.clone(), n.to_string())),
                        _ => None,
                    })
                    .collect(),
                _ => BTreeMap::new(),
            }
        };
        let sources = match root.get("sources") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    let obj = item.as_object()?;
                    Some(TokenSource {
                        repository_id: obj
                            .get("repository_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        repo: obj
                            .get("repo")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        path: obj.get("path").and_then(Value::as_str)?.to_string(),
                        variable_count: obj
                            .get("variable_count")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as usize,
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        // THE PROVENANCE REPORT, when the blob carries one. Read exactly as tolerantly as `sources`:
        // an entry without a path, or with a reason code that is not one of ours, is DROPPED — but a
        // blob whose report is entirely junk still yields `Some(empty)` rather than `None`, because
        // the KEY being there is what says "a run recorded this". A blob with neither key (a
        // hand-authored theme, a legacy layer) yields `None`, which is a different statement.
        let reported = root.get("skipped").is_some_and(Value::is_array)
            || root.get("pruned_dirs").is_some_and(Value::is_array);
        let provenance = reported.then(|| {
            let skipped: Vec<SkippedSheet> = match root.get("skipped") {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|item| {
                        let obj = item.as_object()?;
                        Some(SkippedSheet {
                            repo: obj
                                .get("repo")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            path: obj.get("path").and_then(Value::as_str)?.to_string(),
                            reason: skip_reason(obj.get("reason").and_then(Value::as_str)?)?,
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let pruned_dirs: Vec<String> = match root.get("pruned_dirs") {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|dir| !dir.trim().is_empty())
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            let total = root
                .get("skipped_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let pruned_total = root
                .get("pruned_dirs_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            TokenProvenance::bounded(skipped, total, pruned_dirs, pruned_total)
        });
        Self {
            sources,
            variables: map_of("variables"),
            dark: map_of("dark"),
            provenance,
        }
    }

    /// Serialize the layer to the `design.theme_tokens_json` shape the renderer consumes:
    /// `{ source, sources, variables, dark }`, plus
    /// `{ skipped, skipped_count, pruned_dirs, pruned_dirs_count }` when the layer carries a
    /// provenance report. `source` is the PRIMARY reference (the best-scoring sheet), echoed for
    /// readers that only want "where does `primary` live in the app?".
    ///
    /// EVERY key here is one the renderer must know about: its theme builder FLATTENS unknown
    /// top-level keys into CSS custom properties, so a key it does not reserve comes back out as
    /// `--pruned-dirs-0` inside `@theme`. The four report keys are spelled exactly as
    /// `theme-css.ts` reserves them (`RESERVED_KEYS`) — they are metadata, never CSS. The two lists
    /// are pinned together by the shared fixture `contract/design-fixtures.json`
    /// (finding `01KYSNQWQD1324E0NA0172M66J`): this list of keys is data both suites read, so a key
    /// added here fails the Rust test until the fixture follows, and the fixture fails the TS test
    /// until `RESERVED_KEYS` follows.
    ///
    /// The report keys are OMITTED (not emitted empty) when the layer has none, because "nobody
    /// recorded what was discarded" must stay distinguishable from "nothing was discarded".
    pub fn to_json(&self) -> String {
        let mut value = json!({
            "source": self.sources.first(),
            "sources": self.sources,
            "variables": Value::Object(
                self.variables
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            ),
            "dark": Value::Object(
                self.dark
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            ),
        });
        if let (Some(object), Some(provenance)) = (value.as_object_mut(), self.provenance.as_ref())
        {
            insert_report(object, provenance);
        }
        serde_json::to_string(&value).expect("serialize the token layer (plain strings)")
    }

    /// Is there nothing to render from?
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty() && self.dark.is_empty()
    }
}

/// Write the FOUR report keys onto a token-blob object — THE one place they are spelled.
///
/// Both writers go through it ([`TokenLayer::to_json`] and [`blob_with_provenance`]), so the key
/// list the shared fixture pins (`contract/design-fixtures.json` ⇄ `theme-css.ts`'s `RESERVED_KEYS`)
/// cannot be satisfied by one writer and missed by the other.
fn insert_report(object: &mut serde_json::Map<String, Value>, provenance: &TokenProvenance) {
    object.insert("skipped".to_string(), json!(provenance.skipped));
    object.insert("skipped_count".to_string(), json!(provenance.skipped_count));
    object.insert("pruned_dirs".to_string(), json!(provenance.pruned_dirs));
    object.insert(
        "pruned_dirs_count".to_string(),
        json!(provenance.pruned_dirs_count),
    );
}

/// RECORD a run's report into the blob ALREADY STORED, touching nothing else it carries.
///
/// This is how an ingest that retained NO sheet can still say WHY without becoming collateral damage
/// on a theme somebody typed by hand (finding `01KYSNVW31C0E8YQPET0505HW2`). It works on the stored
/// OBJECT, key by key, and NOT on a re-serialization of [`TokenLayer::parse`]'s result: `variables`,
/// `dark` and `sources` travel through verbatim, and so do the shapes the layer type deliberately
/// does NOT model — the Phase-1 `{ "colors": … }` map and the `{ "css": … }` escape hatch, which
/// `parse` degrades to an empty layer (that tolerance is the point) and which re-serializing would
/// therefore have ERASED. Only the four report keys are replaced.
///
/// A blob that is not a JSON object (absent, blank, malformed) carries nothing to preserve, so the
/// report stands alone. `parse` reads that back as an empty layer WITH a report — which is exactly
/// the claim to make: nothing is stored, and here is what the run refused.
fn blob_with_provenance(previous: &str, provenance: &TokenProvenance) -> String {
    let mut object = match serde_json::from_str::<Value>(previous) {
        Ok(Value::Object(stored)) => stored,
        _ => serde_json::Map::new(),
    };
    insert_report(&mut object, provenance);
    serde_json::to_string(&Value::Object(object)).expect("serialize the token blob (plain values)")
}

// =============================================================================
// DISCOVERY — find the app's own theme stylesheet(s)
// =============================================================================

/// WHAT a candidate stylesheet is to the PROJECT — compared BEFORE any score.
///
/// A score alone could not express "a docs sheet must not win": `docs/.vitepress/theme/theme.css` and
/// `src/theme.css` have the SAME file name, so they score alike, and the merge then took the top four
/// whatever they were (finding 01KYSCX562SWDXZBBAPX397X8F — the layer ended up holding the app's
/// tokens, a `.generated` mirror of them AND a docs site's). The tier is the ordering that says it:
/// an [`Application`](SheetTier::Application) sheet always outranks a
/// [`Peripheral`](SheetTier::Peripheral) one, and a peripheral sheet is only ever used when the
/// project has no application stylesheet at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SheetTier {
    /// The application's own stylesheet — what the design system is.
    Application,
    /// A documentation / example / demo / gallery stylesheet ([`PERIPHERAL_DIRS`]).
    Peripheral,
}

/// A stylesheet the walk found, with the tier + score that rank it against the others AND the
/// variables the scoring pass already extracted from it — so the merge never re-reads and re-scans
/// the same sheet (each candidate is read and parsed EXACTLY once per ingest).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    /// Repo-relative, `/`-separated.
    rel_path: String,
    absolute: PathBuf,
    tier: SheetTier,
    score: i64,
    /// The global custom properties of the sheet, in source order (empty before the scoring pass).
    variables: Vec<CssVariable>,
}

/// The RANKING key: tier first, then score, then path — deterministic, and "a docs sheet never wins"
/// is a property of the ORDER rather than of a tuned penalty.
fn ranking_key(candidate: &Candidate) -> (SheetTier, i64, String) {
    (candidate.tier, -candidate.score, candidate.rel_path.clone())
}

/// How INFORMATIVE a discard reason is — the order the bounded report keeps. A sheet dropped by a
/// RULE (an exclusion, a foreign theme) answers "why are my tokens not there?"; a page stylesheet
/// with two custom properties is noise.
fn reason_rank(reason: &str) -> u8 {
    match reason {
        "recipe_exclude" => 0,
        "doc_tool_theme" => 1,
        "foreign_namespace" => 2,
        "peripheral_sheet" => 3,
        "outranked" => 4,
        "unreadable" => 5,
        "not_ranked" => 6,
        _ => 7, // `not_a_token_sheet`
    }
}

/// Does any DIRECTORY segment of `rel_path` (the file name excluded) match `names`? Case-insensitive,
/// exact segment match — never a substring, so a `src/documentation-viewer/` component directory is
/// not mistaken for a docs tree.
fn has_dir_segment(rel_path: &str, names: &[&str]) -> Option<String> {
    let mut segments: Vec<&str> = rel_path.split('/').collect();
    segments.pop();
    segments
        .into_iter()
        .find(|segment| {
            let lower = segment.to_ascii_lowercase();
            names.iter().any(|name| name.eq_ignore_ascii_case(&lower))
        })
        .map(str::to_string)
}

/// The dominant THIRD-PARTY namespace of a sheet's variables, when it holds at least
/// [`FOREIGN_NAMESPACE_SHARE`] of them — i.e. "this sheet IS that tool's theme".
fn foreign_namespace(vars: &[CssVariable]) -> Option<&'static str> {
    let names: BTreeSet<&str> = vars.iter().map(|var| var.name.as_str()).collect();
    if names.is_empty() {
        return None;
    }
    let threshold = (names.len() as f64 * FOREIGN_NAMESPACE_SHARE).ceil() as usize;
    FOREIGN_TOKEN_PREFIXES
        .iter()
        .copied()
        .find(|prefix| names.iter().filter(|name| name.starts_with(prefix)).count() >= threshold)
}

/// How strongly a FILE NAME suggests a design-token sheet. Names are checked lowercased.
fn filename_score(name: &str) -> i64 {
    match name {
        "tokens.css" | "design-tokens.css" | "theme.css" | "variables.css" | "tokens.scss"
        | "theme.scss" | "variables.scss" => 100,
        "globals.css" | "global.css" | "globals.scss" | "global.scss" => 90,
        "styles.css" | "style.css" | "styles.scss" | "style.scss" => 80,
        "app.css" | "index.css" | "main.css" | "app.scss" | "index.scss" | "main.scss" => 55,
        _ => 5,
    }
}

/// Is this a stylesheet the walk should even consider reading?
fn is_stylesheet(name: &str) -> bool {
    let lower = name.to_lowercase();
    (lower.ends_with(".css") || lower.ends_with(".scss")) && !lower.ends_with(".min.css")
}

/// Read at most [`MAX_STYLESHEET_BYTES`] of a file as UTF-8 (lossy). The read itself is BOUNDED
/// (`File` + [`std::io::Read::take`], like `read_bounded` in [`crate::design`]): a pathological
/// generated bundle is never slurped WHOLE into memory only to be truncated afterwards — which is
/// exactly what the cap promises. `None` on any I/O failure — discovery is BEST-EFFORT: an
/// unreadable candidate is skipped, never fatal.
fn read_capped(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes: Vec<u8> = Vec::new();
    file.take(MAX_STYLESHEET_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// What ONE repo's sweep found: the ranked candidates, the sheets it REFUSED with the reason, and the
/// directory NAMES it did not descend into.
#[derive(Debug, Default)]
struct RepoSweep {
    candidates: Vec<Candidate>,
    /// `(repo-relative path, reason)` — the repo LABEL is added by the caller, which knows it.
    discarded: Vec<(String, &'static str)>,
    /// The distinct NAMES of the directories the walk refused to enter.
    pruned_dirs: BTreeSet<String>,
}

/// Walk `root` (bounded depth) and rank every stylesheet by how much it looks like the app's TOKEN
/// sheet: its TIER (application vs peripheral), then its file name, its depth, and — for the plausible
/// ones — the markers in its content (`@theme`, a global custom property, the canonical token names).
/// Deterministic: the result is sorted by [`ranking_key`], so two runs agree.
///
/// THE EXCLUSIONS ARE THE PROJECT'S OWN (finding 01KYSCX562SWDXZBBAPX397X8F). The walk prunes exactly
/// what the component discovery prunes — [`crate::design::walk_pruned_dirs`]: the recipe's
/// `ignored_dirs` UNIONED with the floor no recipe can lift — plus [`TOKENS_ONLY_SKIPPED_DIRS`]; and a
/// sheet the recipe EXCLUDES (`recipe.exclude`, the same globs, the same matcher) is not a candidate.
/// It used to carry a hard-coded list of its own that did not know `.generated`, so ONE tree was swept
/// by two paths under two sets of rules and the layer meant to be the design system's single reference
/// silently merged a generated mirror of it.
fn sweep_repo(root: &Path, recipe: &BundleRecipe) -> RepoSweep {
    let mut sweep = RepoSweep::default();
    // THE shared pruning set — the recipe's statement, floor included — plus the token-only names.
    let pruned = crate::design::walk_pruned_dirs(recipe);
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() {
                if pruned.contains(name.as_str())
                    || TOKENS_ONLY_SKIPPED_DIRS.contains(&name.as_str())
                {
                    sweep.pruned_dirs.insert(name);
                    continue;
                }
                if depth + 1 > MAX_WALK_DEPTH {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else if file_type.is_file() && is_stylesheet(&name) {
                let path = entry.path();
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let rel_path = rel.to_string_lossy().replace('\\', "/");
                // The recipe's OWN exclusions, through the recipe's own matcher.
                if recipe.is_excluded(&rel_path) {
                    sweep.discarded.push((rel_path, "recipe_exclude"));
                    continue;
                }
                // Another tool's theme (a docs site, a gallery): never the app's design system.
                if has_dir_segment(&rel_path, DOC_TOOL_DIRS).is_some() {
                    sweep.discarded.push((rel_path, "doc_tool_theme"));
                    continue;
                }
                let tier = if has_dir_segment(&rel_path, PERIPHERAL_DIRS).is_some() {
                    SheetTier::Peripheral
                } else {
                    SheetTier::Application
                };
                let mut score = filename_score(&name.to_lowercase());
                score -= 2 * rel_path.matches('/').count() as i64;
                sweep.candidates.push(Candidate {
                    rel_path,
                    absolute: path,
                    tier,
                    score,
                    variables: Vec::new(),
                });
            }
        }
    }
    // Rank by tier/name/depth first, keep the plausible head, then refine with CONTENT markers — so a
    // huge repo only ever opens a bounded number of files. The extraction done here is KEPT on the
    // candidate: the merge downstream reuses it instead of reading and parsing the sheet a second
    // time (review #42 — every retained sheet used to be scanned twice per ingest).
    sweep.candidates.sort_by_key(ranking_key);
    for beyond in sweep
        .candidates
        .split_off(sweep.candidates.len().min(MAX_CANDIDATES_PER_REPO))
    {
        sweep.discarded.push((beyond.rel_path, "not_ranked"));
    }
    let discarded = &mut sweep.discarded;
    sweep.candidates.retain_mut(|candidate| {
        let Some(css) = read_capped(&candidate.absolute) else {
            discarded.push((candidate.rel_path.clone(), "unreadable"));
            return false;
        };
        let vars = extract_css_variables(&css);
        if vars.len() < MIN_TOKEN_VARS {
            discarded.push((candidate.rel_path.clone(), "not_a_token_sheet"));
            return false;
        }
        // A sheet whose variables ARE a third-party tool's namespace is that tool's theme, wherever
        // it sits and whatever it is named.
        if foreign_namespace(&vars).is_some() {
            discarded.push((candidate.rel_path.clone(), "foreign_namespace"));
            return false;
        }
        candidate.score += (vars.len() as i64).min(40);
        if css.contains("@theme") {
            candidate.score += 60;
        }
        for canonical in ["--primary", "--background", "--foreground", "--border"] {
            if vars.iter().any(|v| v.name == canonical) {
                candidate.score += 15;
            }
        }
        candidate.variables = vars;
        true
    });
    sweep.candidates.sort_by_key(ranking_key);
    sweep
}

// =============================================================================
// INGEST / EDIT — the two ways the layer is written
// =============================================================================

/// What an ingest run did — surfaced to the MCP tool and the `/design` UI so a human sees WHERE the
/// tokens came from (and that they came from the app at all).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TokenIngest {
    /// How many repo roots were walked (0 ⇒ the project has no on-disk binding yet).
    pub repos_scanned: usize,
    /// How many plausible token stylesheets the walk ranked, across every repo.
    pub candidates: usize,
    /// The sheets actually MERGED into the layer, best-ranked first — the reference to the app.
    pub sources: Vec<TokenSource>,
    /// The candidate sheets the run did NOT merge, with the reason — bounded to
    /// [`MAX_SKIPPED_REPORTED`], most informative first (see [`SkippedSheet`]). The other half of
    /// `sources`: the layer claims to be THE design-system reference, so where it did NOT come from
    /// has to be as visible as where it did (finding 01KYSCX562SWDXZBBAPX397X8F).
    ///
    /// This and the two fields below are the run's ECHO of [`TokenLayer::provenance`], which the run
    /// also PERSISTS into the blob — the `/design` panel cannot re-run an ingest to obtain them
    /// (finding 01KYSK7GS9C3ZRJYW00ZA0H6GF).
    pub skipped: Vec<SkippedSheet>,
    /// How many sheets were discarded in TOTAL (`skipped` is the bounded head of them).
    pub skipped_count: usize,
    /// The distinct NAMES of the directories the walk refused to enter — the project's own
    /// `ignored_dirs` + the floor + [`TOKENS_ONLY_SKIPPED_DIRS`], as APPLIED, bounded by
    /// [`MAX_PRUNED_DIRS_REPORTED`]. `.generated` in this list is the proof the generated mirror of a
    /// stylesheet was not swept.
    pub pruned_dirs: Vec<String>,
    /// How many directory names were pruned in TOTAL (`pruned_dirs` is the bounded head of them).
    pub pruned_dirs_count: usize,
    /// The size of the resulting light-theme map.
    pub variable_count: usize,
    /// The size of the resulting dark-theme map.
    pub dark_count: usize,
    /// Did the run actually change the stored TOKENS? (A re-ingest of an unchanged app is a no-op.)
    ///
    /// It answers for the `variables` / `dark` / `sources` of the layer and for NOTHING else: a run
    /// that retained no sheet writes no token — it only records WHY — and saying `changed` there
    /// would tell a reader the theme moved when it did not. That half is `provenance_changed`.
    pub changed: bool,
    /// Did the run's REPORT change what is stored — the sheets refused, the directories not entered?
    ///
    /// A run that retains nothing still records this (finding `01KYSNVW31C0E8YQPET0505HW2`): zero
    /// token stored means every design-system class renders grey, and the report is then the ONLY
    /// diagnostic there is. Callers notify on `changed || provenance_changed`: the `/design` panel
    /// cannot re-run an ingest to obtain the report, so a run that wrote nothing else must still
    /// wake it up.
    pub provenance_changed: bool,
    /// The stored layer (the exact `design.theme_tokens_json` value).
    pub tokens_json: String,
}

/// INGEST a project's design tokens FROM ITS APP: walk the project's repos exactly as the component
/// inventory does, rank the stylesheets that look like the design system, and merge the global
/// custom properties of the best few into the project's single token layer.
///
/// Referenced, not copied: the layer records the source sheets (`sources[]`) and lives in ONE place
/// for the whole project, so `primary` keeps ONE identity across every component and preview.
///
/// SAFE by construction: a run that retains NOTHING (greenfield project, no repos, every sheet
/// refused) leaves the stored TOKENS untouched — a failed detection can never erase a theme someone
/// edited by hand. It does record WHY it retained nothing (finding `01KYSNVW31C0E8YQPET0505HW2`):
/// that is the case where zero token is stored, every design-system class renders grey and the
/// report is the only diagnostic left, so the report is written — and ONLY the report.
pub fn ingest_project_tokens(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<TokenIngest> {
    let roots = crate::design::project_repo_roots(conn, project_id)?;
    // The project's RECIPE — the same document, read the same way, as the component inventory
    // (`design::inventory_project`): its `ignored_dirs` and its `exclude` are what the sweep applies.
    let project_recipe = crate::design::ProjectRecipe::load(conn, project_id)?;
    let mut ranked: Vec<(SheetTier, i64, TokenSource, Vec<CssVariable>)> = Vec::new();
    let mut skipped: Vec<SkippedSheet> = Vec::new();
    let mut pruned_dirs: BTreeSet<String> = BTreeSet::new();
    for repo in &roots {
        let (recipe, _source) = project_recipe.for_root(&repo.root);
        let sweep = sweep_repo(&repo.root, &recipe);
        pruned_dirs.extend(sweep.pruned_dirs);
        for (path, reason) in sweep.discarded {
            skipped.push(SkippedSheet {
                repo: repo.label.clone(),
                path,
                reason,
            });
        }
        // Each candidate arrives with the variables its SCORING pass already extracted — the sheet
        // is read and parsed exactly once per ingest.
        for candidate in sweep.candidates {
            let variable_count = candidate.variables.len();
            ranked.push((
                candidate.tier,
                candidate.score,
                TokenSource {
                    repository_id: repo.repo_id.clone(),
                    repo: repo.label.clone(),
                    path: candidate.rel_path,
                    variable_count,
                },
                candidate.variables,
            ));
        }
    }
    let candidates = ranked.len();
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.path.cmp(&b.2.path))
    });
    // TIER BEFORE SCORE (finding 01KYSCX562SWDXZBBAPX397X8F): as soon as the project has ONE
    // application stylesheet, a docs / example / demo sheet cannot enter the layer — it is not the
    // design system, and merging it is what put `--vp-*` variables in there. A docs-ONLY project keeps
    // its sheets (better a layer that renders than a grey one), and the report says where it came from.
    if ranked
        .iter()
        .any(|(tier, ..)| *tier == SheetTier::Application)
    {
        let (application, peripheral): (Vec<_>, Vec<_>) = ranked
            .into_iter()
            .partition(|(tier, ..)| *tier == SheetTier::Application);
        for (_tier, _score, source, _vars) in peripheral {
            skipped.push(SkippedSheet {
                repo: source.repo,
                path: source.path,
                reason: "peripheral_sheet",
            });
        }
        ranked = application;
    }
    for (_tier, _score, source, _vars) in ranked.split_off(ranked.len().min(MAX_MERGED_SOURCES)) {
        skipped.push(SkippedSheet {
            repo: source.repo,
            path: source.path,
            reason: "outranked",
        });
    }
    // The REPORT: most informative reason first, then repo/path — deterministic, and bounded. It is
    // built ONCE and used twice — echoed in the response AND stored in the layer — so what the
    // `/design` panel reads back can never disagree with what the run answered.
    skipped.sort_by(|a, b| {
        reason_rank(a.reason)
            .cmp(&reason_rank(b.reason))
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.path.cmp(&b.path))
    });
    let discarded = skipped.len();
    let pruned: Vec<String> = pruned_dirs.into_iter().collect();
    let pruned_total = pruned.len();
    let provenance = TokenProvenance::bounded(skipped, discarded, pruned, pruned_total);

    // Merge BEST-FIRST-WINS: a name the top sheet declares is never overwritten by a lesser one,
    // and within ONE sheet the LAST declaration wins (plain CSS cascade).
    let mut layer = TokenLayer::default();
    let mut owned: BTreeSet<String> = BTreeSet::new();
    for (_tier, _score, source, vars) in &ranked {
        let mut sheet_light: BTreeMap<String, String> = BTreeMap::new();
        let mut sheet_dark: BTreeMap<String, String> = BTreeMap::new();
        for var in vars {
            match var.scope {
                TokenScope::Root => {
                    sheet_light.insert(var.name.clone(), var.value.clone());
                }
                TokenScope::Dark => {
                    sheet_dark.insert(var.name.clone(), var.value.clone());
                }
            }
        }
        for (name, value) in sheet_light {
            if owned.insert(format!("light:{name}")) {
                layer.variables.insert(name, value);
            }
        }
        for (name, value) in sheet_dark {
            if owned.insert(format!("dark:{name}")) {
                layer.dark.insert(name, value);
            }
        }
        layer.sources.push(source.clone());
    }
    // The layer CARRIES what the run refused, so the report survives the tool call that produced it.
    layer.provenance = Some(provenance.clone());

    let previous = db::get_design(conn, project_id)?
        .and_then(|d| d.theme_tokens_json)
        .unwrap_or_default();
    // Is this run's report NEWS? Compared against the report the stored blob already carries, so a
    // re-ingest of an unchanged tree writes nothing and wakes nobody up.
    let stored = TokenLayer::parse(Some(previous.as_str()));
    let provenance_changed = stored.provenance.as_ref() != Some(&provenance);
    if layer.is_empty() {
        // RETAINED NOTHING — and this is the case where the report is the ONLY diagnostic there is
        // (finding `01KYSNVW31C0E8YQPET0505HW2`). Zero token stored means every design-system class
        // renders grey, so WHICH sheets were refused, for WHICH reason, and which directories were
        // never entered is all a human has to go on. This branch used to return BEFORE any write, so
        // that report died with the tool call — the very defect that was fixed for a non-empty layer
        // (`01KYSK7GS9C3ZRJYW00ZA0H6GF`) coming back out through the worst possible door, since the
        // `/design` panel may not run an ingest to obtain it and could only say "non enregistrés".
        //
        // WHAT IS WRITTEN IS THE REPORT AND NOTHING ELSE. A failed detection is still never
        // collateral damage on a theme someone typed by hand: [`blob_with_provenance`] replaces the
        // four report keys ON the stored object, so `variables`, `dark` and `sources` — and the
        // hand-authored shapes `parse` does not even model — are carried over untouched. Hence
        // `changed: false`: no token moved, and only `provenance_changed` claims anything.
        let tokens_json = blob_with_provenance(&previous, &provenance);
        if tokens_json != previous {
            db::set_design_theme_tokens(conn, project_id, Some(&tokens_json))?;
        }
        return Ok(TokenIngest {
            repos_scanned: roots.len(),
            candidates,
            sources: stored.sources,
            skipped: provenance.skipped,
            skipped_count: provenance.skipped_count,
            pruned_dirs: provenance.pruned_dirs,
            pruned_dirs_count: provenance.pruned_dirs_count,
            variable_count: stored.variables.len(),
            dark_count: stored.dark.len(),
            changed: false,
            provenance_changed,
            tokens_json,
        });
    }
    let tokens_json = layer.to_json();
    // The TOKENS moved — the layer's own content, never its report (`provenance_changed` is that
    // half). The WRITE is keyed on the BLOB, so a run whose only news is what it refused persists it
    // all the same, exactly as the empty branch above does.
    let changed = layer.variables != stored.variables
        || layer.dark != stored.dark
        || layer.sources != stored.sources;
    if tokens_json != previous {
        db::set_design_theme_tokens(conn, project_id, Some(&tokens_json))?;
    }
    Ok(TokenIngest {
        repos_scanned: roots.len(),
        candidates,
        sources: layer.sources.clone(),
        skipped: provenance.skipped,
        skipped_count: provenance.skipped_count,
        pruned_dirs: provenance.pruned_dirs,
        pruned_dirs_count: provenance.pruned_dirs_count,
        variable_count: layer.variables.len(),
        dark_count: layer.dark.len(),
        changed,
        provenance_changed,
        tokens_json,
    })
}

/// EDIT the project's token layer in place: `set` upserts `--name → value` pairs, `remove` drops
/// names. Both operate on the LIGHT map unless `dark` is set. The layer's `sources[]` reference is
/// PRESERVED — and so is its provenance report ([`TokenLayer::provenance`]) — so an edit does not
/// sever the link back to the app's stylesheet nor erase what the ingest refused: it records that the
/// project's `primary` now diverges from it (the next ingest re-derives from the app again).
///
/// Returns the stored layer. Names are normalised to carry their `--` prefix, so a caller may pass
/// either `primary` or `--primary`.
pub fn edit_project_tokens(
    conn: &mut SqliteConnection,
    project_id: &str,
    set: &BTreeMap<String, String>,
    remove: &[String],
    dark: bool,
) -> QueryResult<TokenLayer> {
    let stored = db::get_design(conn, project_id)?.and_then(|d| d.theme_tokens_json);
    let mut layer = TokenLayer::parse(stored.as_deref());
    let target = if dark {
        &mut layer.dark
    } else {
        &mut layer.variables
    };
    for (name, value) in set {
        let name = normalize_token_name(name);
        let value = value.trim();
        if name.len() > 2 && !value.is_empty() {
            target.insert(name, value.to_string());
        }
    }
    for name in remove {
        target.remove(&normalize_token_name(name));
    }
    db::set_design_theme_tokens(conn, project_id, Some(&layer.to_json()))?;
    Ok(layer)
}

/// `--primary` from either `primary` or `--primary` (and trimmed) — one spelling in storage.
pub fn normalize_token_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.starts_with("--") {
        trimmed.to_string()
    } else {
        format!("--{}", trimmed.trim_start_matches('-'))
    }
}

/// READ the project's token layer (the `/design` panel + the `design_tokens` MCP read).
pub fn project_token_layer(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<TokenLayer> {
    let stored = db::get_design(conn, project_id)?.and_then(|d| d.theme_tokens_json);
    Ok(TokenLayer::parse(stored.as_deref()))
}

#[cfg(test)]
mod tests;
