//! The BUNDLE RECIPE — the FRAMEWORK KNOWLEDGE, moved out of nyx and into a per-project value the
//! AGENT writes (v2.7 — Prototype : machinerie, review 01KYKX2A0JGWF8XNWWZ9YFF5MG, finding
//! 01KYKXPHW103C0V5RBXQ29BBXQ).
//!
//! # Why (the knowledge nyx encoded was always going to be late)
//!
//! "What constitutes a component" used to be HARD-CODED in [`super`]: globs per stack, "an entry is
//! a `.ts` containing `@Component`", "the template is what `templateUrl` points at", "the styles are
//! the `.css` siblings", "the variants are the `.variants.ts` sibling". That knowledge broke ONCE
//! already — nyx had Angular = `**/*.component.html`, the reference front follows the Angular v20 convention
//! (`x.ts` + `x.html`), and the scan found 2 bundles instead of 179 — and it would break again for
//! every stack nyx has not met yet (Vue SFC vs split Vue, Lit, Solid, a mixed-stack monorepo).
//!
//! # The split (three responsibilities, not one)
//!
//!   1. LISTING the repo's files under the exclusions — MECHANICAL, stays in nyx (the walk).
//!   2. DECIDING what constitutes a component — FRAMEWORK KNOWLEDGE, comes from the AGENT: THIS
//!      module. The agent explores the repo ONCE, understands the convention, writes the recipe;
//!      nyx APPLIES it deterministically at every scan.
//!   3. HASHING / storing / remembering what changed — MECHANICAL, stays in nyx (the incremental
//!      loop). One agent pass PER PROJECT, never one per scan.
//!
//! # Expressive without being a programming language
//!
//! A recipe is a flat, finite record of PREDICATES — no expressions, no conditionals, no ordering
//! operators beyond "the rules are tried in order", nothing an agent has to *execute* in its head:
//!
//!   * [`EntryRule`] — what OPENS a bundle: `globs` (+ optional `extensions` / `qualifiers`
//!     refinements), an optional CONTENT marker (`requires_content`, e.g. `@Component`) and an
//!     optional TEMPLATE ATTRIBUTE to read out of the entry (`templateUrl`, `view`, …). The rules
//!     are tried IN ORDER: the first one whose filters match ELECTS the file, and its
//!     `requires_content` then decides whether that file is really an entry (no fall-through — a
//!     rule that claims a file owns it).
//!   * [`SiblingRule`] — the ROLE a same-base-name neighbour plays (`template` / `style` /
//!     `variants`), keyed on its extension and/or its dotted qualifier. First match wins; no match
//!     means the file is not part of the bundle.
//!   * `family_qualifiers` — the dotted segments STRIPPED when deriving a bundle's base name, so
//!     `button.component.ts`, `button.component.html` and `button.variants.ts` name ONE component.
//!   * `non_component_qualifiers` — the dotted segments that make a file a bundle MEMBER but never
//!     a component of its own (`.service.ts`, `.types.ts`).
//!   * `exclude` — path globs never considered at all (specs, stories, `.d.ts`).
//!   * [`PinnedBundle`] — the ESCAPE HATCH: the files of THIS component, named explicitly, for what
//!     no rule can express. See [`BundleRecipe::pins`].
//!
//! # Retro-compatibility
//!
//! [`default_recipe`] reproduces, EXACTLY, what nyx did before this module existed for each detected
//! stack, and [`recipe_from_legacy_globs`] does the same for an arbitrary glob set (the v33 per-
//! project `inventory_globs_json` override, and every caller of the legacy `discover_bundles(root,
//! globs)` API). The behaviour is unchanged until an agent decides otherwise.

use serde::{Deserialize, Serialize};

use super::{file_extension_lower, glob_match, source_qualifier, BundleRole, ComponentStack};

/// The recipe SCHEMA version an agent writes and nyx reads. Bumped only by a shape change that an
/// older reader could misinterpret; a stored recipe carrying an unknown version is REFUSED rather
/// than half-understood.
pub const RECIPE_VERSION: u32 = 1;

// --- BOUNDS -----------------------------------------------------------------
//
// A recipe is agent-written and PERSISTED, so it is applied at every scan of every repo forever.
// The bounds below are not about safety of the matcher (the glob DP is O(tokens × path) — see
// `glob_match`), they are about keeping a mis-generated recipe from becoming a permanent cost:
// a thousand rules × a hundred thousand files is a scan nobody asked for.
//
// WHAT THEY GUARANTEE, exactly (audits 01KYM2TCTZNRF1WAZRYJ4550V6 + 01KYM9MN6RF9KTA5M4ZW75HAXP).
// The per-ITEM caps below never bounded the WHOLE: 32 rules × 32 globs × 512 bytes is ~524 000
// bytes of glob evaluated against EVERY file of EVERY repo at EVERY scan — measured at 54,7 s on a
// 2 000-file tree against 34 ms for the default React recipe, on the SYNCHRONOUS `design_inventory`
// / `/design` path. So the guarantee is carried by a TOTAL budget ([`MAX_TOTAL_PATTERN_BYTES`]).
//
// The cost of a scan is a PRODUCT — (bytes of pattern per file) × (files walked) — and this budget
// bounds ONLY THE FIRST FACTOR. That is exactly what it guarantees, and the previous wording
// ("within ~2 % of the ×1600 blow-up") claimed a CEILING ON A SCAN it never had: the second audit
// measured the worst accepted recipe at 60,2 s on a real repo, ×2030 the default, because the same
// recipe also said `"ignored_dirs": []` and the walk then descended into `node_modules`.
//
// So the SECOND factor is bounded by the walk ([`super::MANDATORY_IGNORED_DIRS`]: `node_modules`
// and `.git` are pruned whatever a recipe says; every other name in [`super::IGNORED_DIRS`] stays
// removable — finding J), and what this file guarantees is stated per FILE.
//
// RE-MEASURED on a real Angular monorepo (4 796 files outside `node_modules` / 73 505 with it, RELEASE),
// before → after the floor:
//
//   * default recipe (441 B of pattern) ............  23,7 ms →  24,2 ms   (179 components)
//   * default + `"ignored_dirs": []` ...............   1,25 s →  75,5 ms   (179 components)
//   * banal `**/*.ts` + `"ignored_dirs": []` .......   1,16 s →  19,0 ms   (14 229 → 522)
//   * WORST accepted (8 KiB of `?`) + `[]` .........  61,7 s  →   4,29 s
//
// The `?` is the token the matcher's DP can never short-circuit, so the last line is the ceiling
// this budget is responsible for: ~×20 the default's matching work per file, over whatever files
// the walk hands it. It is NOT a promise that any recipe scans in milliseconds: a recipe that
// empties `ignored_dirs` on a repo carrying a large BUILD CACHE still pays for walking it (nyx's
// own repo, whose `target/` holds a Cargo cache, takes 22,6 s under that same worst recipe against
// 16,8 ms under its default). That is the recipe asking for it — the default list prunes those
// directories, and the recipe READ warns about what the walk prunes regardless.
//
// Every announced bound is EXERCISED at limit / limit+1 by
// `every_announced_recipe_bound_is_applied_and_names_its_field` — the audit found seven bounds and
// zero tests, and one of them (`MAX_QUALIFIERS`) was contradicted by the code that applied it. The
// COST guarantee is exercised by `the_most_expensive_accepted_recipe_still_scans_a_real_tree_
// quickly`, which runs the worst accepted recipe over a tree the size of a real repo, `node_modules`
// included.

/// Max [`EntryRule`]s in one recipe.
const MAX_ENTRY_RULES: usize = 32;
/// Max [`SiblingRule`]s in one recipe.
const MAX_SIBLING_RULES: usize = 32;
/// Max globs in ONE rule (and max `exclude` globs in the recipe).
const MAX_GLOBS_PER_RULE: usize = 32;
/// Max bytes of ONE glob / extension / qualifier / marker string.
const MAX_PATTERN_BYTES: usize = 512;
/// Max explicitly [`PinnedBundle`]s in one recipe.
const MAX_PINS: usize = 512;
/// Max files named by ONE pin (its entry excluded).
const MAX_PIN_FILES: usize = 64;
/// Max qualifiers in `family_qualifiers` / `non_component_qualifiers`.
///
/// This is the bound that is APPLIED, not merely announced: it is handed to [`check_patterns`] as
/// that field's cap. It used to sit in a guard of its own in front of a `check_patterns` that
/// capped at 32, so the effective bound was 32, the "capped at 64" branch was unreachable below 65,
/// and an agent got two contradictory messages depending on how far past the limit it went
/// (audit 01KYM2SSV6CGGERA9RGNQ3JCRW).
const MAX_QUALIFIERS: usize = 64;
/// Max directory names in `ignored_dirs`.
const MAX_IGNORED_DIRS: usize = 64;

/// The TOTAL bytes of PATTERN a recipe may declare — every glob, extension, qualifier, content
/// marker, template attribute, exclusion and ignored-directory name added up.
///
/// This is the bound that says something about the COST OF MATCHING, so it is the one that had to
/// exist (audit 01KYM2TCTZNRF1WAZRYJ4550V6). 8 KiB is ~20× the largest DEFAULT recipe (the
/// `Generic` one weighs under 400 bytes) and far above any real convention, and it holds the worst
/// recipe `validate()` accepts to roughly ×20 the default's matching work PER FILE — measured, not
/// asserted (see the BOUNDS block above and
/// `the_most_expensive_accepted_recipe_still_scans_a_real_tree_quickly`). What it does NOT bound is
/// the NUMBER OF FILES walked; that is the walk's floor ([`super::MANDATORY_IGNORED_DIRS`]) plus
/// the recipe's own `ignored_dirs`.
///
/// Pin PATHS are deliberately out of the budget: they are compared, never glob-matched per file,
/// and they carry their own counts ([`MAX_PINS`] / [`MAX_PIN_FILES`]).
const MAX_TOTAL_PATTERN_BYTES: usize = 8 * 1024;

/// What a recipe REFUSED to be, said in a sentence an agent can act on. Not an error type with
/// variants: the whole point is the message.
pub type RecipeError = String;

/// One ENTRY rule: which files may OPEN a bundle, and how that bundle's TEMPLATE is reached.
///
/// The filters are ANDed and each one is optional except `globs`; the rules of a recipe are tried
/// IN ORDER and the FIRST whose filters match elects the file — its `requires_content` then decides
/// whether the file is an entry at all (there is no fall-through to a later rule, exactly as nyx's
/// hard-coded "a `.ts` is a component only if it declares one" never fell through).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryRule {
    /// The path globs (repo-relative, `/`-separated) a candidate entry must match. At least one.
    pub globs: Vec<String>,
    /// Optional: restrict the rule to these LAST extensions (lowercased, no dot — `ts`, `tsx`).
    /// Empty = any extension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Optional: restrict the rule to these dotted QUALIFIERS (`button.component.ts` → `component`).
    /// Empty = any (including none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<String>,
    /// Optional CONTENT marker the file's head must CONTAIN to be an entry (`@Component`,
    /// `defineWidget(`). This is what lets a broad net (`**/*.ts`) take a whole front without
    /// cataloguing every service, store and util: the marker is convention-INDEPENDENT and exact.
    /// A file that cannot be READ is never a "no" — it is a scan error (fail-closed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_content: Option<String>,
    /// Optional ATTRIBUTE of the entry file naming its template file (Angular's `templateUrl`, a
    /// house framework's `view`). Read as `<attribute><spaces>:<spaces>'<relative path>'` in the
    /// entry's head and resolved against the entry's directory. `None` = the entry carries its
    /// template INLINE (a `.vue` / `.svelte` / `.tsx`) or has none of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_attribute: Option<String>,
}

impl EntryRule {
    /// A rule matching `globs` and nothing else — the minimal form.
    fn globs(globs: &[&str]) -> Self {
        Self {
            globs: globs.iter().map(|g| (*g).to_string()).collect(),
            extensions: Vec::new(),
            qualifiers: Vec::new(),
            requires_content: None,
            template_attribute: None,
        }
    }

    /// This rule, restricted to `extensions`.
    fn with_extensions(mut self, extensions: &[&str]) -> Self {
        self.extensions = extensions.iter().map(|e| (*e).to_string()).collect();
        self
    }

    /// This rule, restricted to `qualifiers`.
    fn with_qualifiers(mut self, qualifiers: &[&str]) -> Self {
        self.qualifiers = qualifiers.iter().map(|q| (*q).to_string()).collect();
        self
    }

    /// This rule, requiring `marker` in the entry's head.
    fn requiring(mut self, marker: &str) -> Self {
        self.requires_content = Some(marker.to_string());
        self
    }

    /// This rule, resolving its template through the `attribute` of the entry file.
    fn templated_by(mut self, attribute: &str) -> Self {
        self.template_attribute = Some(attribute.to_string());
        self
    }

    /// Do this rule's FILTERS (globs + extensions + qualifiers) accept the file at `rel_path`?
    /// `requires_content` is NOT tested here — it needs a disk read and is applied by the scan.
    fn filters_match(&self, rel_path: &str, file_name: &str) -> bool {
        if !self.globs.iter().any(|g| glob_match(g, rel_path)) {
            return false;
        }
        self.accepts_shape(file_name)
    }

    /// Do the extension / qualifier filters accept `file_name`? (The glob-free half of
    /// [`Self::filters_match`] — see [`BundleRecipe::template_attribute_for`].)
    fn accepts_shape(&self, file_name: &str) -> bool {
        if !self.extensions.is_empty() {
            let ext = file_extension_lower(file_name).unwrap_or_default();
            if !self.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
                return false;
            }
        }
        if !self.qualifiers.is_empty() {
            let qualifier = source_qualifier(file_name).unwrap_or_default();
            if !self
                .qualifiers
                .iter()
                .any(|q| q.eq_ignore_ascii_case(&qualifier))
            {
                return false;
            }
        }
        true
    }
}

/// One SIBLING rule: the ROLE a file of the SAME base name, next to the entry, plays in the bundle.
/// Filters are ANDed, both optional (a rule with neither matches every sibling); the recipe's rules
/// are tried IN ORDER and the first match decides. A sibling no rule matches is NOT part of the
/// bundle — that is how a plain logic sibling (`button.helpers.ts`) stays out of the rendering
/// surface, and out of the input hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiblingRule {
    /// The sibling's LAST extension (lowercased, no dot). Empty = any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// The sibling's dotted QUALIFIER (`button.variants.ts` → `variants`). Empty = any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<String>,
    /// What the sibling IS to the component.
    pub role: BundleRole,
}

impl SiblingRule {
    /// A rule keyed on EXTENSIONS.
    fn by_extension(extensions: &[&str], role: BundleRole) -> Self {
        Self {
            extensions: extensions.iter().map(|e| (*e).to_string()).collect(),
            qualifiers: Vec::new(),
            role,
        }
    }

    /// A rule keyed on QUALIFIERS (whatever the extension).
    fn by_qualifier(qualifiers: &[&str], role: BundleRole) -> Self {
        Self {
            extensions: Vec::new(),
            qualifiers: qualifiers.iter().map(|q| (*q).to_string()).collect(),
            role,
        }
    }

    /// Does this rule accept the sibling named `file_name`?
    fn matches(&self, file_name: &str) -> bool {
        if !self.extensions.is_empty() {
            let ext = file_extension_lower(file_name).unwrap_or_default();
            if !self.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
                return false;
            }
        }
        if !self.qualifiers.is_empty() {
            let qualifier = source_qualifier(file_name).unwrap_or_default();
            if !self
                .qualifiers
                .iter()
                .any(|q| q.eq_ignore_ascii_case(&qualifier))
            {
                return false;
            }
        }
        true
    }
}

/// One FILE of an explicitly [pinned](BundleRecipe::pins) bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedFile {
    /// Repo-relative, `/`-separated path.
    pub path: String,
    /// What the file IS to the component.
    pub role: BundleRole,
}

/// A component whose files are named EXPLICITLY — the recipe's escape hatch.
///
/// There will always be a twisted case: a template three directories away, a component whose styles
/// live in a shared file, a legacy widget nothing generalizes. An assumed way OUT beats a recipe
/// that pretends to cover everything: a pin says "the files of THIS component are exactly these",
/// bypasses every rule (globs, markers, siblings, name derivation), and is discovered by the scan
/// like any other bundle — same catalogue row, same input hash, same incremental loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedBundle {
    /// The bundle's ENTRY (repo-relative) — the component's IDENTITY, exactly as for a discovered
    /// one. It does NOT have to match any entry rule, nor even to be a "component-looking" file.
    pub entry: String,
    /// The bundle's OTHER files with their roles. The entry itself is always in the bundle and need
    /// not be repeated here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<PinnedFile>,
    /// The base NAME to catalogue it under. `None` = derived from the entry file name like any
    /// other bundle. (The catalogue may still QUALIFY it with a directory prefix when the name
    /// collides with another component's — the uniqueness rule is unchanged.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A project's BUNDLE RECIPE — everything nyx needs to know about a front's CONVENTION, and nothing
/// about its framework.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleRecipe {
    /// The schema version ([`RECIPE_VERSION`]).
    #[serde(default = "current_version")]
    pub version: u32,
    /// What OPENS a bundle, in PREFERENCE order (the first rule matching a file both elects it and,
    /// when several files of one directory share a base name, wins the entry election).
    pub entries: Vec<EntryRule>,
    /// What a same-base-name neighbour IS, in match order.
    #[serde(default)]
    pub siblings: Vec<SiblingRule>,
    /// The dotted qualifiers STRIPPED when deriving a bundle's base name.
    #[serde(default)]
    pub family_qualifiers: Vec<String>,
    /// The dotted qualifiers that make a file a bundle MEMBER but never a component of its own.
    #[serde(default)]
    pub non_component_qualifiers: Vec<String>,
    /// Path globs NEVER considered — neither as an entry nor as a sibling. Matched against the
    /// repo-relative path AND against its lowercased form, so `**/*.spec.*` also excludes
    /// `Button.Spec.ts`.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// The directory NAMES the walk never descends into, pruned at EVERY level — dependency /
    /// build / VCS noise that would otherwise drown the real component set (and be catastrophically
    /// slow to walk).
    ///
    /// This was the last piece of hard-coded, INEXPRESSIBLE knowledge on the discovery path
    /// (audit 01KYM2W9H0QCV9EE3X9T96SMK8): `.generated` and `.angular` had to be ADDED to nyx in
    /// reaction to one real repo, and a front whose components legitimately live under a directory
    /// named `build` / `out` / `dist` was invisible whatever recipe the agent wrote. It is now a
    /// project's own statement, DEFAULTING to nyx's list ([`super::IGNORED_DIRS`]) — a recipe that
    /// does not mention it behaves exactly as before.
    ///
    /// WITH ONE FLOOR: [`super::MANDATORY_IGNORED_DIRS`] (`node_modules`, `.git`) is pruned whatever
    /// this list says, so `[]` means "walk everything BUT the dependency tree and the VCS
    /// internals" (audit 01KYM9MN6RF9KTA5M4ZW75HAXP — `[]` used to mean a ×2030 scan and 14 229
    /// components catalogued out of `node_modules`). The read of a recipe that omits them WARNS,
    /// so the echoed document never lets an agent believe otherwise, and a vendored file that
    /// really must be catalogued is [PINNED](PinnedBundle) — pins name paths and skip the walk.
    ///
    /// It prunes the WALK, which `exclude` cannot do: `exclude` filters paths the walk already
    /// produced, so it costs the descent into a directory even when it drops every result.
    #[serde(default = "default_ignored_dirs")]
    pub ignored_dirs: Vec<String>,
    /// The components whose files are named explicitly — see [`PinnedBundle`].
    #[serde(default)]
    pub pins: Vec<PinnedBundle>,
}

/// serde default for [`BundleRecipe::ignored_dirs`] — nyx's historical [`super::IGNORED_DIRS`], so a
/// recipe written (or stored) before this field existed prunes exactly what it always pruned.
fn default_ignored_dirs() -> Vec<String> {
    super::IGNORED_DIRS
        .iter()
        .map(|d| (*d).to_string())
        .collect()
}

/// serde default for [`BundleRecipe::version`] — a recipe written without one is a v1 recipe.
fn current_version() -> u32 {
    RECIPE_VERSION
}

impl BundleRecipe {
    /// PARSE + VALIDATE a recipe from its stored/agent-supplied JSON. Unknown fields are REFUSED
    /// (`deny_unknown_fields`): a typo in an agent-written recipe must be an error, never a rule
    /// silently ignored at every scan for the rest of the project's life.
    pub fn from_json(json: &str) -> Result<Self, RecipeError> {
        let recipe: BundleRecipe = serde_json::from_str(json).map_err(|e| {
            format!("the bundle recipe is not valid JSON for the recipe schema: {e}")
        })?;
        recipe.validate()?;
        Ok(recipe)
    }

    /// The directories of [`super::MANDATORY_IGNORED_DIRS`] this recipe does NOT list — the ones
    /// the walk will prune anyway (audit 01KYM9MN6RF9KTA5M4ZW75HAXP).
    ///
    /// The floor is deliberately NOT folded into the document: `ignored_dirs` is bounded by
    /// [`MAX_IGNORED_DIRS`] and its bytes count against [`MAX_TOTAL_PATTERN_BYTES`], so rewriting
    /// the recipe to carry it could make a recipe `validate()` accepted fail to re-parse. The
    /// document stays the agent's; the READER is told what the walk adds
    /// ([`super::project_recipe_view`]'s warnings).
    pub(crate) fn unlisted_mandatory_ignored_dirs(&self) -> Vec<&'static str> {
        super::MANDATORY_IGNORED_DIRS
            .iter()
            .copied()
            .filter(|mandatory| !self.ignored_dirs.iter().any(|dir| dir == mandatory))
            .collect()
    }

    /// This recipe as the JSON that is PERSISTED / echoed to the agent.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a BundleRecipe is always serializable")
    }

    /// REFUSE a recipe that could not be applied, or that would apply catastrophically. Every
    /// message names the field and what to do instead — this is the agent's only feedback loop.
    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.version != RECIPE_VERSION {
            return Err(format!(
                "unsupported bundle-recipe version {} (this nyx understands version {RECIPE_VERSION})",
                self.version
            ));
        }
        if self.entries.is_empty() {
            return Err(
                "`entries` is empty: a recipe with no ENTRY rule would inventory NOTHING. \
                        Give at least one rule (e.g. { \"globs\": [\"**/*.tsx\"] })."
                    .to_string(),
            );
        }
        if self.entries.len() > MAX_ENTRY_RULES {
            return Err(format!(
                "`entries` holds {} rules (max {MAX_ENTRY_RULES}): a convention that needs more \
                 than that is not a convention — pin the exceptions instead (`pins`)",
                self.entries.len()
            ));
        }
        if self.siblings.len() > MAX_SIBLING_RULES {
            return Err(format!(
                "`siblings` holds {} rules (max {MAX_SIBLING_RULES})",
                self.siblings.len()
            ));
        }
        for (index, rule) in self.entries.iter().enumerate() {
            if rule.globs.is_empty() {
                return Err(format!(
                    "entries[{index}] has no `globs`: an entry rule must say WHICH files it claims"
                ));
            }
            check_patterns(
                &format!("entries[{index}].globs"),
                &rule.globs,
                MAX_GLOBS_PER_RULE,
            )?;
            check_patterns(
                &format!("entries[{index}].extensions"),
                &rule.extensions,
                MAX_GLOBS_PER_RULE,
            )?;
            check_patterns(
                &format!("entries[{index}].qualifiers"),
                &rule.qualifiers,
                MAX_GLOBS_PER_RULE,
            )?;
            if let Some(marker) = &rule.requires_content {
                if marker.trim().is_empty() {
                    return Err(format!(
                        "entries[{index}].requires_content is blank: omit it, or give the exact \
                         text an entry file must contain (e.g. \"@Component\")"
                    ));
                }
                if marker.len() > MAX_PATTERN_BYTES {
                    return Err(format!(
                        "entries[{index}].requires_content is longer than {MAX_PATTERN_BYTES} bytes"
                    ));
                }
            }
            if let Some(attribute) = &rule.template_attribute {
                if attribute.trim().is_empty() {
                    return Err(format!(
                        "entries[{index}].template_attribute is blank: omit it (the entry then \
                         carries its template inline), or name the attribute (e.g. \"templateUrl\")"
                    ));
                }
                if attribute.len() > MAX_PATTERN_BYTES {
                    return Err(format!(
                        "entries[{index}].template_attribute is longer than {MAX_PATTERN_BYTES} bytes"
                    ));
                }
            }
        }
        for (index, rule) in self.siblings.iter().enumerate() {
            check_patterns(
                &format!("siblings[{index}].extensions"),
                &rule.extensions,
                MAX_GLOBS_PER_RULE,
            )?;
            check_patterns(
                &format!("siblings[{index}].qualifiers"),
                &rule.qualifiers,
                MAX_GLOBS_PER_RULE,
            )?;
        }
        check_patterns("exclude", &self.exclude, MAX_GLOBS_PER_RULE)?;
        // ONE bound per field, applied and announced by the SAME number — the qualifier lists used
        // to carry a 64 of their own in front of a check that capped them at 32.
        check_patterns("family_qualifiers", &self.family_qualifiers, MAX_QUALIFIERS)?;
        check_patterns(
            "non_component_qualifiers",
            &self.non_component_qualifiers,
            MAX_QUALIFIERS,
        )?;
        check_patterns("ignored_dirs", &self.ignored_dirs, MAX_IGNORED_DIRS)?;
        for (index, dir) in self.ignored_dirs.iter().enumerate() {
            if dir.contains('/') || dir.contains('\\') {
                return Err(format!(
                    "`ignored_dirs[{index}]` '{dir}' is a PATH: this list holds directory NAMES, \
                     pruned at every level of the walk (`node_modules`, `dist`). To drop one \
                     specific path, use `exclude` (a path glob)."
                ));
            }
        }
        let pattern_bytes = self.total_pattern_bytes();
        if pattern_bytes > MAX_TOTAL_PATTERN_BYTES {
            return Err(format!(
                "the recipe declares {pattern_bytes} bytes of patterns in total (max \
                 {MAX_TOTAL_PATTERN_BYTES}): every one of them is matched against EVERY file of \
                 EVERY repo at EVERY scan, so this budget is what keeps an inventory from taking \
                 minutes instead of milliseconds. A real convention needs a handful of short globs \
                 — narrow them, and name the exceptions in `pins` (whose paths are not part of \
                 this budget)."
            ));
        }
        if self.pins.len() > MAX_PINS {
            return Err(format!(
                "`pins` holds {} entries (max {MAX_PINS})",
                self.pins.len()
            ));
        }
        for (index, pin) in self.pins.iter().enumerate() {
            check_pin_path(&format!("pins[{index}].entry"), &pin.entry)?;
            if pin.files.len() > MAX_PIN_FILES {
                return Err(format!(
                    "pins[{index}] names {} files (max {MAX_PIN_FILES})",
                    pin.files.len()
                ));
            }
            for (file_index, file) in pin.files.iter().enumerate() {
                check_pin_path(
                    &format!("pins[{index}].files[{file_index}].path"),
                    &file.path,
                )?;
            }
            if let Some(name) = &pin.name {
                if name.trim().is_empty() {
                    return Err(format!(
                        "pins[{index}].name is blank: omit it to derive the name from the entry file"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The TOTAL bytes of PATTERN this recipe declares — what [`MAX_TOTAL_PATTERN_BYTES`] bounds.
    ///
    /// It sums everything that is MATCHED PER FILE at scan time: the entry rules' globs /
    /// extensions / qualifiers / content markers / template attributes, the sibling rules' filters,
    /// `exclude`, both qualifier lists and `ignored_dirs`. Pin paths are NOT in it: they are
    /// compared, never matched, and are bounded by count instead.
    pub fn total_pattern_bytes(&self) -> usize {
        let sum = |values: &[String]| values.iter().map(String::len).sum::<usize>();
        let mut total = 0usize;
        for rule in &self.entries {
            total += sum(&rule.globs) + sum(&rule.extensions) + sum(&rule.qualifiers);
            total += rule.requires_content.as_ref().map_or(0, String::len);
            total += rule.template_attribute.as_ref().map_or(0, String::len);
        }
        for rule in &self.siblings {
            total += sum(&rule.extensions) + sum(&rule.qualifiers);
        }
        total
            + sum(&self.exclude)
            + sum(&self.family_qualifiers)
            + sum(&self.non_component_qualifiers)
            + sum(&self.ignored_dirs)
    }

    /// The FIRST entry rule whose filters accept the file at `rel_path` — the rule that ELECTS it,
    /// with its index (the entry-election preference: a lower index wins). `None` = this file is not
    /// a candidate entry at all.
    pub(crate) fn entry_rule_for(
        &self,
        rel_path: &str,
        file_name: &str,
    ) -> Option<(usize, &EntryRule)> {
        self.entries
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.filters_match(rel_path, file_name))
    }

    /// The TEMPLATE ATTRIBUTE to read out of the entry at `rel_path`.
    ///
    /// Normally the electing rule's. A component ALREADY CATALOGUED whose path no longer matches any
    /// glob (the agent narrowed the recipe after the fact) still has to resolve its bundle the same
    /// way — otherwise its input hash would move for a reason that has nothing to do with its
    /// sources — so the fallback is the first rule whose SHAPE filters admit the file, globs
    /// ignored. Still recipe-driven: nothing here knows what `templateUrl` is.
    pub(crate) fn template_attribute_for(&self, rel_path: &str, file_name: &str) -> Option<&str> {
        if let Some((_, rule)) = self.entry_rule_for(rel_path, file_name) {
            return rule.template_attribute.as_deref();
        }
        self.entries
            .iter()
            .find(|rule| rule.accepts_shape(file_name) && rule.template_attribute.is_some())
            .and_then(|rule| rule.template_attribute.as_deref())
    }

    /// The ROLE a same-base-name neighbour named `file_name` plays, or `None` when no sibling rule
    /// claims it (it is then NOT part of the bundle).
    pub(crate) fn sibling_role(&self, file_name: &str) -> Option<BundleRole> {
        self.siblings
            .iter()
            .find(|rule| rule.matches(file_name))
            .map(|rule| rule.role)
    }

    /// Is the file at `rel_path` EXCLUDED — never an entry, never a sibling? Matched against the
    /// path as it is AND against its lowercased form, so a pattern written in lower case still
    /// catches `Button.Spec.tsx` (the hard-coded predicate this replaces lowercased the name).
    pub(crate) fn is_excluded(&self, rel_path: &str) -> bool {
        if self.exclude.is_empty() {
            return false;
        }
        let lowered = rel_path.to_ascii_lowercase();
        self.exclude
            .iter()
            .any(|pattern| glob_match(pattern, rel_path) || glob_match(pattern, &lowered))
    }

    /// May `file_name` open a bundle of its OWN? False for a file whose dotted qualifier marks it a
    /// mere family member (`user.service.ts`, `button.variants.ts`).
    pub(crate) fn opens_bundle(&self, file_name: &str) -> bool {
        match source_qualifier(file_name) {
            Some(qualifier) => !self
                .non_component_qualifiers
                .iter()
                .any(|q| q.eq_ignore_ascii_case(&qualifier)),
            None => true,
        }
    }

    /// The bundle BASE NAME derived from a file NAME: its stem, with every trailing FAMILY
    /// qualifier dropped — so `button.component.ts`, `button.component.html` and
    /// `button.variants.ts` all name the same component. `None` for a name with no usable stem.
    pub(crate) fn component_name(&self, file_name: &str) -> Option<String> {
        // Strip the LAST extension only (a compound like `.component.html` keeps `.component`,
        // which the qualifier loop below then strips).
        let mut base = match file_name.rsplit_once('.') {
            Some((stem, _ext)) => stem,
            None => file_name,
        };
        while let Some((head, qualifier)) = base.rsplit_once('.') {
            if !self
                .family_qualifiers
                .iter()
                .any(|q| q.eq_ignore_ascii_case(qualifier))
            {
                break;
            }
            base = head;
        }
        let base = base.trim();
        if base.is_empty() {
            None
        } else {
            Some(base.to_string())
        }
    }

    /// The PIN whose entry is `rel_path`, if any — checked BEFORE any rule, since a pin's whole
    /// purpose is to bypass them.
    pub(crate) fn pin_for(&self, rel_path: &str) -> Option<&PinnedBundle> {
        self.pins.iter().find(|pin| {
            normalized_repo_path(&pin.entry).as_deref() == normalized_repo_path(rel_path).as_deref()
        })
    }
}

/// NORMALIZE a repo-relative path (`./a/../b/c.ts` → `b/c.ts`), or `None` when it is empty or
/// escapes the repo root with `..`.
///
/// This is a LEXICAL normalization and nothing more: it says what the path MEANS, not what it
/// RESOLVES to on disk. The "a pin can no more reach outside the scanned repo than the walk can"
/// invariant is NOT enforced here — it used to be claimed here, and it was false: a
/// `src/leak.ts -> /etc/passwd` symlink inside the repo passes this function untouched (audit
/// 01KYM2RKGHY1WE9HTWEAJ2N2SZ). The invariant is enforced at OPEN time by
/// [`super::open_confined`], which opens every segment with `O_NOFOLLOW` — so a symlinked segment
/// is refused exactly as the walk refuses it, and refused by the very act that reads the bytes.
pub(crate) fn normalized_repo_path(path: &str) -> Option<String> {
    super::join_relative("", path)
}

/// Bound a list of agent-written patterns: COUNT (against the field's own `max_count` — the cap is
/// passed in, so the number a message announces is always the number that was applied) and
/// INDIVIDUAL size.
fn check_patterns(field: &str, patterns: &[String], max_count: usize) -> Result<(), RecipeError> {
    if patterns.len() > max_count {
        return Err(format!(
            "`{field}` holds {} entries (max {max_count})",
            patterns.len()
        ));
    }
    for pattern in patterns {
        if pattern.trim().is_empty() {
            return Err(format!("`{field}` holds a BLANK entry — remove it"));
        }
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(format!(
                "`{field}` holds an entry longer than {MAX_PATTERN_BYTES} bytes"
            ));
        }
    }
    Ok(())
}

/// Does `path` start with a Windows DRIVE (`C:\…`, `C:/…`)? Absolute, whatever the separator.
fn has_drive_prefix(path: &str) -> bool {
    let mut chars = path.chars();
    matches!((chars.next(), chars.next()), (Some(letter), Some(':')) if letter.is_ascii_alphabetic())
}

/// Bound + CANONICALITY of one pinned path, so the value the recipe STORES is exactly the value the
/// scan RESOLVES (audit 01KYM2TWYT43Z9DP91QWQVKA6Y).
///
/// `validate()` promised to refuse anything that is not "a usable repo-relative path" and did not:
/// an ABSOLUTE `/etc/passwd` was accepted, persisted verbatim, and only re-read at scan time as
/// `etc/passwd` UNDER the repo root — a different file from the one the agent wrote, chosen
/// silently. Same for `~/.ssh/id_rsa`, `C:\Windows\win.ini`, `//etc//passwd` and
/// `....//....//etc/passwd`. None of them ESCAPED the repo (`..` was, and is, refused); they were
/// REINTERPRETED, which is worse than a refusal because nothing said so. And because `pin_for`
/// compares NORMALIZED forms while the stored value was raw, re-reading the recipe did not describe
/// what the scan would do.
fn check_pin_path(field: &str, path: &str) -> Result<(), RecipeError> {
    if path.starts_with('/')
        || path.starts_with('~')
        || path.contains('\\')
        || has_drive_prefix(path)
    {
        return Err(format!(
            "`{field}` '{path}' is an ABSOLUTE path (or a home / drive / backslash one). A pin \
             names a file INSIDE the repo: give it relative to the repo root, `/`-separated \
             (e.g. \"src/legacy/gauge.js\"). nyx will NOT re-interpret it under the root for you — \
             that used to turn '/etc/passwd' into 'etc/passwd' without a word."
        ));
    }
    let Some(normalized) = normalized_repo_path(path) else {
        return Err(format!(
            "`{field}` '{path}' is not a usable repo-relative path (it is empty, or it escapes the \
             repo root with `..`)"
        ));
    };
    if normalized != path {
        return Err(format!(
            "`{field}` '{path}' is not written the way the scan resolves it ('{normalized}'). \
             Write it exactly as it appears under the repo root, so the recipe you read back says \
             what the scan actually does."
        ));
    }
    Ok(())
}

// =============================================================================
// The DEFAULTS — nyx's former hard-coded knowledge, expressed AS DATA
// =============================================================================

/// Extensions that carry MARKUP (a component's template) in the DEFAULT recipes.
const DEFAULT_TEMPLATE_EXTENSIONS: &[&str] = &["html", "htm"];

/// Extensions that carry STYLES in the DEFAULT recipes.
const DEFAULT_STYLE_EXTENSIONS: &[&str] = &["css", "scss", "sass", "less", "pcss", "styl"];

/// Qualifiers that mark a `.variants.*` sidecar (CVA / class-variance classes).
const DEFAULT_VARIANT_QUALIFIERS: &[&str] = &["variants", "variant"];

/// The code extensions whose net is broad enough to catch services / stores / routes, so an entry
/// must DECLARE itself to count (nyx's former `requires_declaration`).
const DEFAULT_DECLARING_EXTENSIONS: &[&str] = &["ts", "js", "mts", "cts", "mjs", "cjs"];

/// The content marker of an Angular component class — the default `requires_content`.
const DEFAULT_DECLARATION_MARKER: &str = "@Component";

/// The default TEMPLATE ATTRIBUTE (Angular's `@Component({ templateUrl: './x.html' })`).
const DEFAULT_TEMPLATE_ATTRIBUTE: &str = "templateUrl";

/// Qualifiers stripped when deriving a bundle's base name (nyx's former `FAMILY_QUALIFIERS`).
const DEFAULT_FAMILY_QUALIFIERS: &[&str] = &[
    "component",
    "module",
    "variants",
    "variant",
    "styles",
    "style",
    "types",
    "service",
    "spec",
    "test",
    "stories",
    "story",
];

/// Qualifiers that never open a bundle of their own (nyx's former `NON_COMPONENT_QUALIFIERS`).
const DEFAULT_NON_COMPONENT_QUALIFIERS: &[&str] = &[
    "service",
    "types",
    "type",
    "variants",
    "variant",
    "styles",
    "style",
    "module",
    "directive",
    "pipe",
    "guard",
    "resolver",
    "interceptor",
    "model",
    "models",
    "routes",
    "config",
    "constants",
    "mock",
    "mocks",
];

/// The default EXCLUSIONS — the test / spec / story / type-declaration sidecars nyx's former
/// `is_excluded_source_file` dropped by name. Expressed as PATH globs, which is strictly more
/// expressive (a whole directory can be excluded) and matched case-insensitively.
const DEFAULT_EXCLUDE: &[&str] = &[
    "**/*.d.ts",
    "**/*.test.*",
    "**/*.spec.*",
    "**/*.stories.*",
    "**/*.story.*",
];

/// The default SIBLING rules: a `.variants.*` sidecar (whatever its extension), then markup, then
/// styles. Order matters — `button.variants.css` is a VARIANTS sidecar, not a style sheet, exactly
/// as the hard-coded chain decided.
fn default_siblings() -> Vec<SiblingRule> {
    vec![
        SiblingRule::by_qualifier(DEFAULT_VARIANT_QUALIFIERS, BundleRole::Variants),
        SiblingRule::by_extension(DEFAULT_TEMPLATE_EXTENSIONS, BundleRole::Template),
        SiblingRule::by_extension(DEFAULT_STYLE_EXTENSIONS, BundleRole::Style),
    ]
}

/// A recipe carrying the default siblings / qualifiers / exclusions and the given ENTRY rules.
fn recipe_with(entries: Vec<EntryRule>) -> BundleRecipe {
    BundleRecipe {
        version: RECIPE_VERSION,
        entries,
        siblings: default_siblings(),
        family_qualifiers: DEFAULT_FAMILY_QUALIFIERS
            .iter()
            .map(|q| (*q).to_string())
            .collect(),
        non_component_qualifiers: DEFAULT_NON_COMPONENT_QUALIFIERS
            .iter()
            .map(|q| (*q).to_string())
            .collect(),
        exclude: DEFAULT_EXCLUDE.iter().map(|g| (*g).to_string()).collect(),
        ignored_dirs: default_ignored_dirs(),
        pins: Vec::new(),
    }
}

/// The DEFAULT recipe of a detected [`ComponentStack`] — nyx's former hard-coded behaviour, now
/// merely the recipe a project starts from. STRICTLY iso-behaviour: the entry rules are ordered so
/// that the "first rule wins, then path order" election reproduces the old hard-coded
/// `entry_priority` ladder exactly for these glob sets.
///
/// ANGULAR takes ALL `.ts` files, deliberately: the modern Angular style guide (v20+, what the reference front
/// uses) names a component `button.ts` + `button.html`, NOT `button.component.ts` — a name-only net
/// would miss the entire front. The CLASS is then recognized by CONTENT (`@Component`), which is
/// convention-independent and exact; `**/*.component.html` stays as an entry for the legacy layout
/// where a template has no class file beside it.
///
/// The `templateUrl` attribute is declared on the CODE rules of every stack, not just Angular's,
/// because that is what nyx did before: it probed every code entry for a `templateUrl`, whatever the
/// stack. It costs a React project nothing (no `.tsx` declares one) and keeps the hash identical.
pub fn default_recipe(stack: ComponentStack) -> BundleRecipe {
    let templated =
        |globs: &[&str]| EntryRule::globs(globs).templated_by(DEFAULT_TEMPLATE_ATTRIBUTE);
    let declaring = |globs: &[&str]| {
        EntryRule::globs(globs)
            .requiring(DEFAULT_DECLARATION_MARKER)
            .templated_by(DEFAULT_TEMPLATE_ATTRIBUTE)
    };
    recipe_with(match stack {
        ComponentStack::React => vec![templated(&["**/*.tsx", "**/*.jsx"])],
        ComponentStack::Vue => vec![EntryRule::globs(&["**/*.vue"])],
        ComponentStack::Svelte => vec![EntryRule::globs(&["**/*.svelte"])],
        ComponentStack::Angular => vec![
            declaring(&["**/*.ts"]),
            EntryRule::globs(&["**/*.component.html"]),
        ],
        ComponentStack::Generic => vec![
            templated(&["**/*.tsx", "**/*.jsx"]),
            EntryRule::globs(&["**/*.vue", "**/*.svelte"]),
            declaring(&["**/*.ts"]),
            EntryRule::globs(&["**/*.component.html"]),
        ],
    })
}

/// The recipe equivalent to a bare GLOB SET — the retro-compatibility bridge for the v33 per-project
/// `inventory_globs_json` override and for every caller of the legacy `discover_bundles(root,
/// globs)` API.
///
/// It is nyx's former EXTENSION-driven ladder, expressed as rules and restricted to `globs`:
/// `.component.ts` first, then the single-file components, then the other code files (which must
/// DECLARE themselves), then markup, then anything else the globs let through. That order is
/// literally the old `entry_priority` function, so a glob override elects exactly the same entry it
/// always did — including for glob sets that mix `.ts` and `.tsx`.
pub fn recipe_from_legacy_globs(globs: &[String]) -> BundleRecipe {
    let owned: Vec<&str> = globs.iter().map(|g| g.as_str()).collect();
    let rule = || EntryRule::globs(&owned);
    recipe_with(vec![
        // priority 0 — an Angular `.component.ts` CLASS
        rule()
            .with_extensions(&["ts"])
            .with_qualifiers(&["component"])
            .requiring(DEFAULT_DECLARATION_MARKER)
            .templated_by(DEFAULT_TEMPLATE_ATTRIBUTE),
        // priority 1 — a React single-file component
        rule()
            .with_extensions(&["tsx", "jsx"])
            .templated_by(DEFAULT_TEMPLATE_ATTRIBUTE),
        // priority 2 — a Vue / Svelte single-file component
        rule().with_extensions(&["vue", "svelte"]),
        // priority 3 — any other CODE file, which must declare itself
        rule()
            .with_extensions(DEFAULT_DECLARING_EXTENSIONS)
            .requiring(DEFAULT_DECLARATION_MARKER)
            .templated_by(DEFAULT_TEMPLATE_ATTRIBUTE),
        // priority 4 — a template with no class beside it
        rule().with_extensions(DEFAULT_TEMPLATE_EXTENSIONS),
        // priority 5 — anything else an override glob lets through (a `.md`, a `.hbs`…). It is not
        // a code file, so it needs no declaration: the four rules above cover, between them, every
        // extension nyx ever treated as code.
        rule(),
    ])
}

/// The recipe that resolves ANY entry the way nyx did before recipes existed — the legacy ladder
/// over "every path". Used by the entry-point that has no glob context at all
/// (`bundle_for_entry`), and by nothing else.
pub(crate) fn legacy_recipe_for_any_entry() -> BundleRecipe {
    recipe_from_legacy_globs(&["**".to_string()])
}
