//! The DESIGN-SYSTEM machinery (v2.7 — Prototype : machinerie, Phase 1) that sits ABOVE the
//! [`crate::db`] `design` / `design_components` CRUD authority.
//!
//! ## Why this module exists (the FIDÈLE distill, PDR-0010)
//!
//! The proto→dev distill is broken because the coder never receives the design-system + the
//! mapped components that would keep the rendu faithful to the app's standards. Fixing that
//! starts by keeping the component catalogue POPULATED and FRESH from the real front, natively
//! in nyx.
//!
//! ## The BUNDLE pivot (review #40) — discovery here, SYNTHESIS by the agent
//!
//! Phase 1 assumed "le corps du fichier = le rendu": one markup file → one component, its content
//! regex-extracted into a Handlebars template. That assumption is FALSE for a real front. An
//! Angular component is a multi-file BUNDLE — the `.component.ts` class (which declares the
//! `@Input()` / `input()` params), the template resolved through `templateUrl` → `.html`, the
//! sibling `.variants.ts` (CVA classes) and styles. A server-side regex cannot structurally
//! reconstruct that, and every attempt mutilated the source (imports emptied, TS braces mistaken
//! for mustaches) while cataloguing non-components (`.service.ts`, `.types.ts`, `.variants.ts`).
//!
//! So the unit is now the BUNDLE, and the work is SPLIT:
//!
//!   * **nyx does the DISCOVERY — deterministic, dependency-free** ([`discover_bundles`]): walk the
//!     repo, glob the candidate ENTRY files, DROP the non-components, group them per
//!     (directory, base name), and resolve each bundle's files — the entry, the template resolved
//!     from `templateUrl`, the sibling styles / `.variants.*`. Each bundle carries an
//!     [`ComponentBundle::input_hash`]: the md5 of its INPUTS (the source files), never of any
//!     output.
//!   * **an LLM agent does the SYNTHESIS**: it READS a bundle's files ([`read_bundle`]) and writes
//!     back a SELF-CONTAINED logic-less Handlebars template + params + states through the authoring
//!     tool (`design_refresh_component`) — the format `proto_author_component` already fills. That
//!     is uniform across stacks (React / Angular / Vue …) where a regex never could be.
//!   * **the loop stays INCREMENTAL** ([`inventory_project`] / [`mark_synthesized`]): the stored
//!     `visual_hash` records the INPUT hash a component was last SYNTHESIZED at, so a bundle whose
//!     sources did not change is NEVER re-synthesized (and the non-deterministic LLM output is
//!     never hashed). A component stays CATALOGUED (name / path / bundle / hash) even before any
//!     synthesis — its `template` is simply empty until the agent fills it.
//!
//! This is conform to PDR-0010, which freezes the Handlebars RENDU in the sidecar, NOT the method
//! used to POPULATE the library ("l'agent crée la lib" is written there).
//!
//! ## The RECIPE pivot (finding 01KYKXPHW103C0V5RBXQ29BBXQ) — the FRAMEWORK KNOWLEDGE left nyx too
//!
//! The discovery above was deterministic but not neutral: WHAT constitutes a component was hard-coded
//! here (globs per stack, "a `.ts` containing `@Component`", "the template is the `templateUrl`",
//! "the styles are the `.css` siblings"). That knowledge is always late — it already cost 177 of
//! the reference front's 179 components once — so it now lives in a per-project [`BundleRecipe`] the AGENT writes
//! after exploring the repo ONCE, and that nyx APPLIES deterministically at every scan
//! ([`recipe::BundleRecipe`] documents the split and the schema). Everything mechanical is unchanged:
//! the walk, the hash, the incremental loop, the élagage. [`default_recipe`] is the former behaviour,
//! derived from the detected stack, so a project that never writes a recipe scans exactly as before.
//!
//! Everything here is either PURE (the glob matcher, the stack defaults, the bundle grouping, the
//! `templateUrl` resolution — all unit-testable without a DB or disk) or a thin orchestration over
//! the `db` authority; this module never re-implements a CRUD rule.

use std::path::{Path, PathBuf};

use diesel::sqlite::SqliteConnection;
use diesel::QueryResult;

use crate::db::{self, DesignComponent};

pub mod recipe;

pub use recipe::{
    default_recipe, recipe_from_legacy_globs, BundleRecipe, EntryRule, PinnedBundle, PinnedFile,
    SiblingRule, RECIPE_VERSION,
};

/// The ABSOLUTE path of a repo-relative, `/`-separated `rel` under `root` (platform-stable — the
/// single path JOIN the whole module uses).
///
/// It only JOINS, and it is used for exactly ONE thing: LISTING a bundle's directory
/// ([`bundle_for_entry_reporting`]'s sibling pass), which yields NAMES. Every path that is then
/// READ goes through [`open_confined`], which is where the "a bundle never reaches outside the
/// scanned repo" invariant is ENFORCED — and enforced by the open itself, so no name this listing
/// produced can turn into an out-of-repo read afterwards.
fn abs_source_path(root: &Path, rel: &str) -> PathBuf {
    let mut abs = root.to_path_buf();
    for seg in rel.split('/') {
        abs.push(seg);
    }
    abs
}

/// Why a repo-relative path could NOT be opened as a confined bundle file — see [`open_confined`].
///
/// The two arms are the two `ScanError` KINDS the callers record, and they say different things: a
/// path that LEAVES the repo is a refusal (`outside_repo`), a path that merely could not be opened
/// (missing, unreadable, a directory where a file was expected) is the I/O failure it always was
/// (`read_file` / `read_input`). Both are fail-closed — an incomplete scan prunes nothing — but only
/// the first one is a containment statement.
#[derive(Debug)]
enum ConfinedOpenError {
    /// The path leaves the repo: a segment is a SYMLINK (which the walk never follows), or the path
    /// is not a normalized repo-relative one. Carries the sentence the agent reads.
    OutsideRepo(String),
    /// A plain I/O failure on a path that never left the repo.
    Io(std::io::Error),
}

impl ConfinedOpenError {
    /// The message, whichever arm it is — for the callers that only report one string.
    fn message(&self) -> String {
        match self {
            ConfinedOpenError::OutsideRepo(message) => message.clone(),
            ConfinedOpenError::Io(err) => err.to_string(),
        }
    }
}

/// The refusal sentence for a segment of `rel` that turned out to be a symlink.
fn symlink_refusal(rel: &str, segment: &str) -> String {
    format!(
        "'{rel}' traverses the SYMLINK '{segment}', which the repo walk never follows: a bundle \
         file must be a real file INSIDE the repo, so nyx refuses to read it"
    )
}

/// The `/`-separated SEGMENTS of a repo-relative path, or the reason it is not one.
///
/// `""`, `.` and `..` are refused BY NAME (a lexical escape, or a path not written the way the scan
/// resolves it) before any syscall is made.
fn confined_segments(rel: &str) -> Result<Vec<&str>, ConfinedOpenError> {
    let segments: Vec<&str> = rel.split('/').collect();
    for segment in &segments {
        if segment.is_empty() || *segment == "." || *segment == ".." {
            return Err(ConfinedOpenError::OutsideRepo(format!(
                "'{rel}' is not a normalized repo-relative path (segment '{segment}')"
            )));
        }
    }
    Ok(segments)
}

/// OPEN the repo-relative `rel` under `root`, CONFINED to the repo — the single door every read of
/// a bundle file goes through, and the place the "a bundle never reaches outside the scanned repo"
/// invariant is ENFORCED.
///
/// # Why an OPEN and not a check
///
/// [`abs_source_path`] merely joins, and the OS then FOLLOWS every symlink it meets. A
/// `src/leak.ts -> /etc/passwd` (or a `src/outside -> /tmp/secret/`) sitting INSIDE the repo
/// therefore used to let a [PINNED](PinnedBundle) bundle read — hand to the agent through
/// [`read_bundle`], and fold into the input hash — a file the DISCOVERY itself could never have
/// seen: [`walk_files`] types its entries with `entry.file_type()` (an `lstat`), so a symlink is
/// neither a file nor a directory to it and is simply skipped (audit 01KYM2RKGHY1WE9HTWEAJ2N2SZ).
///
/// The first fix CHECKED the path (an `lstat` per prefix + a `canonicalize` containment) and then
/// handed back a path the callers RE-OPENED. That is a TOCTOU race, and it was not theoretical:
/// nyx is multi-session, so an agent writing a repo WHILE an inventory walks it is the normal case,
/// not an attack. The audit (01KYM9KVQ0M1PA3AMA7MYCSE93) measured it with a writer alternating
/// `src/race.ts` between a real file and a symlink out of the repo — 2062 reads out of 30000 (~7 %)
/// returned OUT-OF-REPO BYTES through `read_bundle`'s own expression, and 222 scans out of 3000
/// produced exactly the input hash of the out-of-repo secret.
///
/// So the check and the open are now THE SAME ACT, and the descriptor — never a path — is what is
/// read:
///
///   * the `root` is opened once (following symlinks: the repo root is the caller's own anchor and
///     may legitimately live behind one — a symlinked worktree, `/tmp` on macOS);
///   * every segment BELOW it is then opened with `openat(2)` + `O_NOFOLLOW` relative to the
///     previous one (`O_DIRECTORY` on the intermediate ones, `O_CLOEXEC` throughout). A segment
///     that is (or becomes) a symlink makes the OPEN FAIL with `ELOOP` instead of being followed,
///     and a name can never be `.` / `..` / absolute, so the walk cannot leave the subtree.
///
/// There is therefore no window left between "it is inside the repo" and "these are its bytes":
/// the bytes come from the descriptor the check itself produced. A substituted symlink is an
/// `outside_repo` refusal, which makes the scan INCOMPLETE — fail-closed, nothing is pruned.
///
/// A path that simply DOES NOT EXIST is not a containment failure: it is [`ConfinedOpenError::Io`],
/// which the callers report exactly as they always reported a failed `metadata` / read.
///
/// A HARD LINK is accepted, deliberately: the walk sees it too (it is a real directory entry for a
/// real inode), so this is the same capability the discovery has, not a wider one.
#[cfg(unix)]
fn open_confined(root: &Path, rel: &str) -> Result<std::fs::File, ConfinedOpenError> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;

    let segments = confined_segments(rel)?;
    let mut dir: OwnedFd = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(root)
        .map_err(ConfinedOpenError::Io)?
        .into();
    let last = segments.len() - 1;
    for (index, segment) in segments.iter().enumerate() {
        // A NUL byte cannot reach the kernel — and cannot be a real file name either.
        let name = std::ffi::CString::new(*segment).map_err(|_| {
            ConfinedOpenError::OutsideRepo(format!(
                "'{rel}' is not a normalized repo-relative path (segment '{segment}')"
            ))
        })?;
        let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        if index != last {
            flags |= libc::O_DIRECTORY;
        }
        // SAFETY: `dir` is an open directory descriptor we own, `name` is a NUL-terminated C string
        // that outlives the call, and the returned fd is immediately wrapped in an `OwnedFd`.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            // WHICH failure this was decides the `ScanError` KIND and nothing else — the open
            // already failed, so not a byte can leak either way. The errno alone does not tell:
            // `O_NOFOLLOW` on a symlink is `ELOOP`, but COMBINED WITH `O_DIRECTORY` Linux reports
            // `ENOTDIR` (a symlink is not a directory), which is also what a plain file used as a
            // directory gives. So the entry is `lstat`ed THROUGH THE PARENT DESCRIPTOR to name what
            // it is — a classification, never a re-resolution.
            return Err(if entry_is_symlink(dir.as_raw_fd(), &name) {
                ConfinedOpenError::OutsideRepo(symlink_refusal(rel, segment))
            } else {
                ConfinedOpenError::Io(err)
            });
        }
        // SAFETY: `fd` was just returned by `openat` and is not owned by anything else.
        dir = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Ok(std::fs::File::from(dir))
}

/// Is the entry `name` of the ALREADY-OPEN directory `dir` a SYMLINK? (`fstatat` +
/// `AT_SYMLINK_NOFOLLOW`.)
///
/// Asked only to CLASSIFY an [`open_confined`] failure — the open is what refuses, this only picks
/// the sentence and the `ScanError` kind. It goes through the parent DESCRIPTOR, so it says
/// something about the entry the open just tried, not about a path resolved a second time.
#[cfg(unix)]
fn entry_is_symlink(dir: std::os::fd::RawFd, name: &std::ffi::CStr) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` is an open descriptor, `name` is NUL-terminated, and `stat` is only read when
    // the call reports success (which means the kernel initialized it).
    let rc = unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    rc == 0 && (unsafe { stat.assume_init() }.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

/// [`open_confined`] where `openat(2)` is not available.
///
/// The containment is the same RULE (no segment of the path may be a symlink, and what it resolves
/// to must stay under the root), but it is a CHECK followed by an open rather than one act: without
/// an `openat`-style primitive there is nothing to hold the path still in between. nyx ships on
/// Unix; this arm exists so the crate still builds elsewhere, and it says exactly what it is worth.
#[cfg(not(unix))]
fn open_confined(root: &Path, rel: &str) -> Result<std::fs::File, ConfinedOpenError> {
    let segments = confined_segments(rel)?;
    let mut abs = root.to_path_buf();
    for segment in segments {
        abs.push(segment);
        if let Ok(meta) = std::fs::symlink_metadata(&abs) {
            if meta.file_type().is_symlink() {
                return Err(ConfinedOpenError::OutsideRepo(symlink_refusal(
                    rel, segment,
                )));
            }
        }
    }
    if let Ok(resolved) = abs.canonicalize() {
        let confined = match root.canonicalize() {
            Ok(real_root) => resolved.starts_with(&real_root),
            Err(_) => true, // the root itself is gone — the open below reports that
        };
        if !confined {
            return Err(ConfinedOpenError::OutsideRepo(format!(
                "'{rel}' resolves OUTSIDE the repo root ({}), so nyx refuses to read it",
                resolved.display()
            )));
        }
    }
    std::fs::File::open(&abs).map_err(ConfinedOpenError::Io)
}

/// Is `rel` a path that LEAVES the repo? `Some(reason)` when it does — the CHECK-ONLY form of
/// [`open_confined`], for the one caller that decides whether a path may enter a bundle at all
/// ([`pinned_bundle`]) without reading it yet.
///
/// A path that could not be opened for any other reason is NOT a containment failure: it stays in
/// the bundle and its unreadability is reported where it is read (the input hash's `read_input`),
/// which is what keeps a pin naming a missing file VISIBLE instead of silently dropped.
fn confinement_refusal(root: &Path, rel: &str) -> Option<String> {
    match open_confined(root, rel) {
        Ok(_) => None,
        Err(ConfinedOpenError::OutsideRepo(message)) => Some(message),
        Err(ConfinedOpenError::Io(_)) => None,
    }
}

/// The DIRECTORY part of a repo-relative, `/`-separated path (`""` for a root-level file).
fn parent_slash_path(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, _file)) => dir.to_string(),
        None => String::new(),
    }
}

/// The FILE NAME part of a repo-relative, `/`-separated path.
fn file_name_of(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

// =============================================================================
// STACK detection + the candidate ENTRY globs
// =============================================================================

/// The front-end STACK of a project, which selects the DEFAULT inventory globs (PRD Impl
/// Decisions: "défaut selon stack ; nyx = React"). A project can always OVERRIDE the default
/// with its own globs (see [`resolve_component_globs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComponentStack {
    /// React / React-Native: `.tsx` / `.jsx` components. nyx's own stack — the default default.
    /// nyx = React (PRD Impl Decisions): an unclassified project inherits nyx's own default.
    #[default]
    React,
    /// Vue single-file components (`.vue`).
    Vue,
    /// Svelte single-file components (`.svelte`).
    Svelte,
    /// Angular components: the `.component.ts` CLASS is the entry (it declares the params and
    /// resolves the template through `templateUrl`); a template-only `.component.html` still
    /// forms a bundle of its own when no class sits next to it.
    Angular,
    /// Unknown stack: a broad net over the common component entries.
    Generic,
}

impl ComponentStack {
    /// The stack's wire NAME (what the agent-facing tools echo).
    pub fn label(self) -> &'static str {
        match self {
            ComponentStack::React => "react",
            ComponentStack::Vue => "vue",
            ComponentStack::Svelte => "svelte",
            ComponentStack::Angular => "angular",
            ComponentStack::Generic => "generic",
        }
    }
}

/// The DEFAULT candidate-ENTRY globs for a `stack` — the INCLUDE patterns matched against each
/// file's repo-relative, `/`-separated path (see [`glob_match`]). They select the files that may
/// OPEN a bundle; the bundle's other files (template, styles, variants) are then resolved around
/// the entry ([`bundle_for_entry`]), never globbed. Broad on purpose (`**` spans any depth); the
/// noisy directories are pruned by the walk ([`IGNORED_DIRS`]), the non-components dropped by
/// [`is_component_entry`], and a plain `.ts` that declares NO component by [`declares_component`].
///
/// ANGULAR takes ALL `.ts` files, deliberately: the modern Angular style guide (v20+, what the reference front
/// uses) names a component `button.ts` + `button.html`, NOT `button.component.ts` — a name-only net
/// would miss the entire front. The CLASS is then recognized by CONTENT (`@Component`), which is
/// convention-independent and exact; `**/*.component.html` stays as an entry for the legacy layout
/// where a template has no class file beside it.
pub fn default_component_globs(stack: ComponentStack) -> Vec<String> {
    match stack {
        ComponentStack::React => vec!["**/*.tsx".to_string(), "**/*.jsx".to_string()],
        ComponentStack::Vue => vec!["**/*.vue".to_string()],
        ComponentStack::Svelte => vec!["**/*.svelte".to_string()],
        ComponentStack::Angular => {
            vec!["**/*.ts".to_string(), "**/*.component.html".to_string()]
        }
        ComponentStack::Generic => vec![
            "**/*.tsx".to_string(),
            "**/*.jsx".to_string(),
            "**/*.vue".to_string(),
            "**/*.svelte".to_string(),
            "**/*.ts".to_string(),
            "**/*.component.html".to_string(),
        ],
    }
}

/// Resolve the EFFECTIVE inventory globs: a project's `project_override` when it supplies a
/// non-empty set (the "surchargeable par projet" seam), else the `stack` default. This is the
/// single place the default-vs-override precedence lives, so `inventory_components` and any
/// future persisted per-project config agree. An empty override is treated as "no override"
/// (fall back to the default) so a project can never silently inventory NOTHING by mis-config.
pub fn resolve_component_globs(
    stack: ComponentStack,
    project_override: Option<&[String]>,
) -> Vec<String> {
    match project_override {
        Some(globs) if !globs.is_empty() => globs.to_vec(),
        _ => default_component_globs(stack),
    }
}

/// DETECT the front-end [`ComponentStack`] of a repo rooted at `root` — the fix for review #32
/// (the inventory hard-coded React globs, so on an Angular repo it captured the wrong subsystem).
/// The signals, in priority order:
///
///   1. an `angular.json` at the repo root — an unambiguous Angular workspace marker; OR
///   2. the `package.json` dependencies (see [`stack_from_package_json`]): `@angular/core` →
///      Angular, `vue` → Vue, `svelte` → Svelte, `react` → React.
///
/// Falls back to [`ComponentStack::Generic`] (a broad net) when a repo carries NEITHER an
/// `angular.json` NOR a recognizable `package.json` dep — so an unclassifiable repo still scans a
/// sensible default rather than nothing. GREENFIELD-safe: a missing/unreadable `package.json`
/// simply yields `Generic` (never an error), so an app-neuve repo detects fine.
pub fn detect_stack(root: &Path) -> ComponentStack {
    // An `angular.json` at the root is the strongest, cheapest signal — check it first.
    if root.join("angular.json").is_file() {
        return ComponentStack::Angular;
    }
    if let Ok(content) = std::fs::read_to_string(root.join("package.json")) {
        if let Some(stack) = stack_from_package_json(&content) {
            return stack;
        }
    }
    ComponentStack::Generic
}

/// The [`ComponentStack`] implied by a `package.json`'s DEPENDENCIES — the pure, disk-free core of
/// [`detect_stack`] (unit-testable without a scratch tree). Scans `dependencies`,
/// `devDependencies` and `peerDependencies` for the marker package of each stack, in priority
/// order (Angular → Vue → Svelte → React) so a project pulling in more than one framework resolves
/// deterministically to its PRIMARY stack. Returns `None` when the manifest is unparsable or names
/// no recognized framework (the caller then falls back to [`ComponentStack::Generic`]).
pub fn stack_from_package_json(package_json: &str) -> Option<ComponentStack> {
    let value: serde_json::Value = serde_json::from_str(package_json).ok()?;
    let has_dep = |dep: &str| -> bool {
        ["dependencies", "devDependencies", "peerDependencies"]
            .iter()
            .any(|section| value.get(section).and_then(|s| s.get(dep)).is_some())
    };
    if has_dep("@angular/core") {
        Some(ComponentStack::Angular)
    } else if has_dep("vue") || has_dep("@vue/runtime-core") {
        Some(ComponentStack::Vue)
    } else if has_dep("svelte") {
        Some(ComponentStack::Svelte)
    } else if has_dep("react") {
        Some(ComponentStack::React)
    } else {
        None
    }
}

/// The DEFAULT directories never descended into during the inventory walk — dependency / build /
/// VCS noise that would drown the real component set (and be catastrophically slow). Pruned by
/// NAME at every level.
///
/// This is a DEFAULT, not a law: a project states its own through
/// [`recipe::BundleRecipe::ignored_dirs`], which starts from exactly this list
/// (audit 01KYM2W9H0QCV9EE3X9T96SMK8 — it was the last piece of hard-coded, INEXPRESSIBLE knowledge
/// left on the discovery path, and it had already had to be extended in reaction to one real repo).
pub const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    "coverage",
    ".cache",
    // GENERATED output that would otherwise flood the catalogue with duplicate/derived markup
    // (review #32): `.generated` (e.g. a `*-tests/.generated/**` mirror) and
    // `.angular` (the Angular CLI build cache). `coverage` (above) is likewise generated.
    ".generated",
    ".angular",
];

/// The directories the walk prunes NO MATTER WHAT THE RECIPE SAYS — the floor under
/// [`recipe::BundleRecipe::ignored_dirs`].
///
/// WHY A FLOOR AT ALL (audit 01KYM9MN6RF9KTA5M4ZW75HAXP). The recipe's pattern budget
/// ([`recipe::MAX_TOTAL_PATTERN_BYTES`]) bounds the matching work PER FILE; it says nothing about
/// HOW MANY FILES the walk visits, and that term dominates by two orders of magnitude. Measured on
/// a real Angular repo (release): the default recipe scans in 29,7 ms and catalogues 179
/// components; the same repo with `ignored_dirs: []` took 60,2 s (×2030) and catalogued 14 229
/// components — 68 700 of the files walked, and essentially all of the extra components, came from
/// `node_modules`. So the announced ceiling was not a ceiling.
///
/// WHY THESE TWO AND NO MORE. `node_modules` is the DEPENDENCY TREE and `.git` the VCS's own
/// internals: neither is ever the front a project asks nyx to inventory, and a component found
/// there is somebody else's. Every other name in [`IGNORED_DIRS`] stays REMOVABLE, which is the
/// whole point of audit 01KYM2W9H0QCV9EE3X9T96SMK8 (finding J): a front that legitimately lives
/// under `build/` / `out/` / `dist/` must stay reachable by writing the recipe that says so.
///
/// THE ESCAPE HATCH IS NOT CLOSED: [PINS](PinnedBundle) do not go through the walk at all
/// ([`pinned_bundle`] resolves the paths a pin NAMES), so a specific vendored file under
/// `node_modules/` can still be made a component — one path at a time, which is a statement, not a
/// 68 700-file descent.
///
/// The floor is applied in TWO places, deliberately: the walk unions it in ([`scan_bundles_with`]),
/// so no in-memory recipe can bypass it, and [`recipe::BundleRecipe::from_json`] folds it back into
/// the parsed document, so the recipe that is stored and ECHOED says what the scan actually does.
pub const MANDATORY_IGNORED_DIRS: &[&str] = &["node_modules", ".git"];

/// The directory NAMES a walk of a repo PRUNES under `recipe`: the recipe's own
/// [`recipe::BundleRecipe::ignored_dirs`] UNIONED with the floor no recipe can lift
/// ([`MANDATORY_IGNORED_DIRS`]).
///
/// THE ONE PLACE that union is built (finding `01KYSCX562SWDXZBBAPX397X8F`). It used to be inline in
/// [`scan_bundles_with`], and the TOKEN ingest — which sweeps the SAME arborescence, for the layer
/// that is supposed to be the design system's single reference — carried a hard-coded list of its
/// own. The two lists had drifted: the token one did not know `.generated`, so a real front got a
/// GENERATED mirror of its stylesheet and a VitePress docs theme (`--vp-*`) merged into its tokens,
/// while the component discovery of the very same tree pruned both. One tree, one pruning rule: both
/// paths now take it from here ([`crate::design_tokens`] included).
pub(crate) fn walk_pruned_dirs(recipe: &BundleRecipe) -> std::collections::BTreeSet<&str> {
    recipe
        .ignored_dirs
        .iter()
        .map(String::as_str)
        .chain(MANDATORY_IGNORED_DIRS.iter().copied())
        .collect()
}

/// A source file that MATCHED a glob but is NOT a component: a test / spec / story / type-decl
/// sidecar. Excluded from the catalogue so it stays the real component set. Keyed on the file
/// NAME (not the whole path).
///
/// LEGACY SHIM: the exclusions are now the recipe's ([`BundleRecipe::exclude`], path globs). This
/// answers the same question against the DEFAULT recipe, for the callers that only have a name.
pub fn is_excluded_source_file(file_name: &str) -> bool {
    default_recipe(ComponentStack::Generic).is_excluded(file_name)
}

/// The QUALIFIER of a source file name — the dotted segment BEFORE the extension
/// (`button.component.ts` → `component`, `chip.variants.ts` → `variants`, `Button.tsx` → `None`).
/// Lowercased. The single reading of the "family" convention every stack uses.
pub fn source_qualifier(file_name: &str) -> Option<String> {
    let (stem, _ext) = file_name.rsplit_once('.')?;
    let (_base, qualifier) = stem.rsplit_once('.')?;
    if qualifier.is_empty() {
        None
    } else {
        Some(qualifier.to_ascii_lowercase())
    }
}

/// Is `file_name` a candidate component ENTRY — the file that may OPEN a bundle? It must be
/// neither a test/spec/story/decl sidecar ([`is_excluded_source_file`]) nor a non-component family
/// member. This is the FILTER the review demands: `.service.ts`, `.types.ts`, `.variants.ts`,
/// specs/tests/stories never create a catalogue entry.
///
/// LEGACY SHIM over the DEFAULT recipe's `exclude` + `non_component_qualifiers`.
pub fn is_component_entry(file_name: &str) -> bool {
    let recipe = default_recipe(ComponentStack::Generic);
    !recipe.is_excluded(file_name) && recipe.opens_bundle(file_name)
}

/// The component / BUNDLE base name derived from a source file NAME: the stem before the
/// extension, with every trailing FAMILY qualifier dropped — so `Button.tsx`,
/// `Button.module.css`, `button.component.html` and `button.variants.ts` all name the same
/// component. Returns `None` for a name with no usable stem.
///
/// LEGACY SHIM over the DEFAULT recipe's `family_qualifiers`.
pub fn component_name_from_file(file_name: &str) -> Option<String> {
    default_recipe(ComponentStack::Generic).component_name(file_name)
}

/// The lowercased LAST extension of a file name (`Button.module.css` → `css`), or `None` when
/// the name has no extension.
pub fn file_extension_lower(file_name: &str) -> Option<String> {
    file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|e| !e.is_empty())
}

/// Is `ext` a MARKUP (template) extension? LEGACY SHIM over the DEFAULT recipe's sibling rules.
pub fn is_template_extension(ext: &str) -> bool {
    matches!(
        default_recipe(ComponentStack::Generic).sibling_role(&format!("x.{ext}")),
        Some(BundleRole::Template)
    )
}

/// Is `ext` a STYLE extension? LEGACY SHIM over the DEFAULT recipe's sibling rules.
pub fn is_style_extension(ext: &str) -> bool {
    matches!(
        default_recipe(ComponentStack::Generic).sibling_role(&format!("x.{ext}")),
        Some(BundleRole::Style)
    )
}

// =============================================================================
// BUNDLE discovery (review #40) — the deterministic half of the pivot
// =============================================================================

/// One I/O failure met by the discovery WALK — the fact the walk used to SWALLOW.
///
/// The walk is best-effort by design (a real tree can always hold something unreadable at the
/// instant it is looked at), but its result was used as a PROOF OF NON-EXISTENCE by the élagage: a
/// directory it could not open, a file it could not probe, an entry that vanished between the
/// enumeration and the read all became "this component is not there any more" and fed a DELETE that
/// destroyed a synthesized template. And these are ORDINARY events, not exotic ones — another agent
/// session writing a file through write-tmp + rename, a concurrent `git checkout` / `git stash`, a
/// network / bind-mounted FS returning EIO, an EMFILE.
///
/// AN ABSENCE IS NEVER A PROOF. So every failure is now COUNTED and NAMED, and a repo whose scan met
/// a single one prunes NOTHING (see [`prune_orphans`], `kept_because: "scan_incomplete"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScanError {
    /// The repo the failure happened in (its human label; `""` for a bare [`scan_bundles`] call,
    /// which has no repo context — [`inventory_project`] fills it in).
    pub repo: String,
    /// The repo-relative, `/`-separated path that could not be read (`"."` = the repo root itself).
    pub path: String,
    /// WHAT failed: `read_dir` (a directory could not be LISTED), `read_entry` (a directory entry
    /// could not be stat'ed), `read_file` (a candidate entry could not be PROBED, so it could not
    /// be recognized as a component), `read_input` (a bundle file could not be folded into the
    /// bundle's input hash, so the change detector is running on partial matter) or `outside_repo`
    /// (a declared path leaves the repo — see [`open_confined`] — so nyx REFUSED to read it).
    pub kind: &'static str,
    /// The OS error, as the platform reported it.
    pub message: String,
}

/// How many [`ScanError`]s are kept VERBATIM. The COUNT is always exact (it is what the fail-closed
/// rule keys on); the named sample is bounded so a pathologically unreadable tree cannot turn one
/// inventory result into an unbounded payload.
const MAX_REPORTED_SCAN_ERRORS: usize = 50;

/// The I/O failures of a scan: their exact COUNT (the fail-closed trigger — `0` means the scan read
/// everything it looked at) plus a bounded NAMED sample (the actionable detail).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ScanErrors {
    /// How many I/O failures the scan met IN TOTAL.
    pub count: usize,
    /// The first [`MAX_REPORTED_SCAN_ERRORS`] of them, named.
    pub sample: Vec<ScanError>,
}

impl ScanErrors {
    /// Is this scan CLEAN — did it read everything it looked at? Only a clean scan may justify a
    /// destruction.
    pub fn is_clean(&self) -> bool {
        self.count == 0
    }

    /// RECORD one failure: always counted, named while there is room in the sample.
    fn record(&mut self, error: ScanError) {
        self.count += 1;
        if self.sample.len() < MAX_REPORTED_SCAN_ERRORS {
            self.sample.push(error);
        }
    }

    /// FOLD another scan's failures into this one (the per-repo → per-project accumulation).
    fn absorb(&mut self, other: ScanErrors, repo_label: &str) {
        self.count += other.count;
        for mut error in other.sample {
            if self.sample.len() >= MAX_REPORTED_SCAN_ERRORS {
                break;
            }
            error.repo = repo_label.to_string();
            self.sample.push(error);
        }
    }
}

/// The result of scanning ONE repo root: the BUNDLES it discovered, and everything it FAILED to
/// read while doing so. The two are inseparable — a bundle list is only as trustworthy as the walk
/// that produced it, and the élagage needs to know which.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoScan {
    pub bundles: Vec<ComponentBundle>,
    pub errors: ScanErrors,
}

/// The repo-relative, `/`-separated path a [`ScanError`] names — `"."` when the failure is on the
/// repo root itself (or on something outside it, which the walk never produces).
fn scan_path(root: &Path, path: &Path) -> String {
    relative_slash_path(root, path).unwrap_or_else(|| ".".to_string())
}

/// The ROLE a file plays inside a component [`ComponentBundle`] — what the synthesizing agent
/// should expect to read in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleRole {
    /// The ENTRY: the class / single-file component that opened the bundle (declares the params).
    Entry,
    /// The MARKUP: the template, resolved from `templateUrl` or found as a sibling.
    Template,
    /// A STYLE sheet of the component.
    Style,
    /// A `.variants.*` sidecar (CVA / class-variance classes).
    Variants,
}

/// One FILE of a component bundle: its repo-relative path + the [`BundleRole`] it plays.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BundleFile {
    pub path: String,
    pub role: BundleRole,
}

/// One component BUNDLE discovered on disk — the UNIT of the inventory since review #40 (a
/// component is a set of files, not a file). Pure data: the name, the directory it lives in, its
/// ENTRY file, every file that composes it, and the md5 of its INPUTS (`input_hash`) — the
/// incremental key. The DB write is [`inventory_components`]; the CONTENT read (for the agent's
/// synthesis) is [`read_bundle`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ComponentBundle {
    /// The component name (the bundle base name, e.g. `button` for `button.component.ts`).
    pub name: String,
    /// The bundle's ROOT: the repo-relative directory of its entry (`""` at the repo root).
    pub root_path: String,
    /// The ENTRY file (repo-relative) — the component's identity + `source_path` in the catalogue.
    pub entry_path: String,
    /// Every file of the bundle, path-ordered (the entry included).
    pub files: Vec<BundleFile>,
    /// The md5 of the bundle's INPUTS (file names + contents, path-ordered). NEVER a hash of a
    /// synthesized output — an LLM synthesis is not deterministic, its inputs are.
    pub input_hash: String,
}

/// The `templateUrl` declared in a component CLASS source (Angular's `@Component({ templateUrl:
/// './x.component.html' })`), or `None` when the source declares none. LEGACY SHIM over
/// [`template_attribute_value`] with the default attribute.
pub fn template_url_of(source: &str) -> Option<String> {
    template_attribute_value(source, "templateUrl")
}

/// The value of the TEMPLATE ATTRIBUTE `attribute` declared in an entry source — Angular's
/// `@Component({ templateUrl: './x.component.html' })`, a house framework's
/// `defineWidget({ view: './x.view.html' })` — or `None` when the source declares none. The
/// attribute NAME comes from the project's [`BundleRecipe`]: nothing here knows what `templateUrl`
/// is.
///
/// PURE (no disk): the value is returned verbatim, the caller resolves it against the entry's
/// directory ([`join_relative`]). Deliberately a targeted scan, not a TS parser — the review forbids
/// a heavy Angular/TS dependency, and resolving ONE string literal needs none. Only the FIRST
/// occurrence of the attribute is considered (unchanged from the hard-coded version — scanning on
/// would silently CHANGE which template a component resolves to).
pub fn template_attribute_value(source: &str, attribute: &str) -> Option<String> {
    let idx = source.find(attribute)?;
    let rest = source[idx + attribute.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let quote = rest.chars().next()?;
    if !matches!(quote, '\'' | '"' | '`') {
        return None;
    }
    let after = &rest[quote.len_utf8()..];
    let end = after.find(quote)?;
    let url = after[..end].trim();
    if url.is_empty() {
        None
    } else {
        Some(url.to_string())
    }
}

/// RESOLVE a `/`-separated relative reference `rel` against the repo-relative directory `dir`,
/// normalizing `.` and `..` — the platform-stable `templateUrl` resolution. A leading `/` makes
/// `rel` repo-root-relative. Returns `None` when the reference ESCAPES the repo root (a `..` too
/// many) or resolves to nothing, so a bundle can never reach outside the scanned repo. PURE.
pub fn join_relative(dir: &str, rel: &str) -> Option<String> {
    let mut segments: Vec<&str> = if dir.is_empty() || rel.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for segment in rel.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?; // escaping the repo root → unresolvable
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// BUILD the bundle whose ENTRY is `entry_rel` (repo-relative) under `root`: the entry itself, the
/// template resolved from its `templateUrl` (when the entry is code), and every SIBLING of the
/// same base name that is a template / style / `.variants.*` sidecar. A plain sibling `.ts` LOGIC
/// file is deliberately NOT a bundle member: it holds no rendering, and folding it in would make a
/// logic-only edit re-trigger a synthesis for nothing.
///
/// Returns `None` when the entry does not exist or has no usable name. Deterministic: the files
/// are path-ordered and the [`ComponentBundle::input_hash`] is computed over them.
///
/// Every I/O failure it meets is REPORTED through [`bundle_for_entry_reporting`] — this convenience
/// wrapper is for the callers that resolve ONE known component's bundle ([`bundle_of_component`]),
/// where a `None` is an expected answer ("its source is gone") rather than a scan hole.
///
/// LEGACY SHIM: resolves with the legacy ladder recipe (the pre-recipe, extension-driven
/// behaviour). The recipe-aware entry point is [`bundle_for_entry_with`].
pub fn bundle_for_entry(root: &Path, entry_rel: &str) -> Option<ComponentBundle> {
    bundle_for_entry_with(root, entry_rel, &recipe::legacy_recipe_for_any_entry())
}

/// [`bundle_for_entry`] under an explicit [`BundleRecipe`] — the form every real caller uses, so a
/// bundle is assembled the SAME way whoever asks (the scan, the per-component read, the synthesis
/// hash check).
pub fn bundle_for_entry_with(
    root: &Path,
    entry_rel: &str,
    recipe: &BundleRecipe,
) -> Option<ComponentBundle> {
    bundle_for_entry_reporting(root, entry_rel, recipe, &mut ScanErrors::default())
}

/// [`bundle_for_entry`], REPORTING every I/O failure into `errors` instead of swallowing it: the
/// entry that vanished between the walk and the bundling (the window of an atomic save's
/// rename-over), the template-attribute probe that could not be read, the sibling directory that
/// could not be listed, and every bundle file the input hash could not fold in. The discovery uses
/// THIS one, so a partially-read repo can never justify an élagage.
///
/// The RECIPE decides everything framework-shaped here: which attribute of the entry names its
/// template (if any), what a same-base-name neighbour IS, which files are excluded, how the base
/// name is derived — and whether this entry is [PINNED](PinnedBundle), in which case its files are
/// taken verbatim and no rule is consulted at all.
pub fn bundle_for_entry_reporting(
    root: &Path,
    entry_rel: &str,
    recipe: &BundleRecipe,
    errors: &mut ScanErrors,
) -> Option<ComponentBundle> {
    if let Some(pin) = recipe.pin_for(entry_rel) {
        return pinned_bundle(root, pin, recipe, errors);
    }
    // CONFINED: the entry can come from the WALK (which never yields a symlink) but also from a
    // STORED `source_path`, so it is opened through the same gate as everything else. The
    // descriptor is then KEPT: what is stat'ed and what is probed below is this file, not a path
    // re-resolved twice (audit 01KYM9KVQ0M1PA3AMA7MYCSE93).
    let mut entry_file = match open_confined(root, entry_rel) {
        Ok(file) => file,
        Err(err) => {
            errors.record(ScanError {
                repo: String::new(),
                path: entry_rel.to_string(),
                kind: match err {
                    ConfinedOpenError::OutsideRepo(_) => "outside_repo",
                    ConfinedOpenError::Io(_) => "read_file",
                },
                message: err.message(),
            });
            return None;
        }
    };
    match entry_file.metadata() {
        Ok(meta) if meta.is_file() => {}
        // Not a file (a directory named like an entry) — a real answer, not a failure to read.
        Ok(_) => return None,
        Err(err) => {
            errors.record(ScanError {
                repo: String::new(),
                path: entry_rel.to_string(),
                kind: "read_file",
                message: err.to_string(),
            });
            return None;
        }
    }
    let entry_name = file_name_of(entry_rel);
    let name = recipe.component_name(entry_name)?;
    let dir_rel = parent_slash_path(entry_rel);

    let mut files: Vec<BundleFile> = vec![BundleFile {
        path: entry_rel.to_string(),
        role: BundleRole::Entry,
    }];
    let push = |files: &mut Vec<BundleFile>, path: String, role: BundleRole| {
        if !files.iter().any(|f| f.path == path) {
            files.push(BundleFile { path, role });
        }
    };

    // 1. The template resolved through the recipe's TEMPLATE ATTRIBUTE (Angular's `templateUrl`, a
    //    house framework's `view`) — the file that ACTUALLY holds the markup, which no sibling rule
    //    would find when it lives elsewhere.
    if let Some(attribute) = recipe.template_attribute_for(entry_rel, entry_name) {
        // BOUNDED (review — unbounded discovery reads): such a decorator sits in the file's HEAD, so
        // probing the same window as [`declares_marker`] is enough and a vendored megabyte cannot be
        // slurped whole on the `/design` button's path.
        match read_bounded(&mut entry_file, DECLARATION_PROBE_BYTES) {
            Ok((head, _bytes)) => {
                let source = String::from_utf8_lossy(&head);
                if let Some(url) = template_attribute_value(&source, attribute) {
                    if let Some(rel) = join_relative(&dir_rel, &url) {
                        // CONFINED like every other bundle file: a `templateUrl` pointing through an
                        // internal symlink out of the repo is refused, not silently followed. The
                        // "is it a file?" answer comes from the descriptor `open_confined` just
                        // produced — and the file is opened AGAIN, confined, when it is actually
                        // read (hash / `read_bundle`), so a swap in between cannot leak either.
                        match open_confined(root, &rel) {
                            Ok(template) => {
                                if template.metadata().map(|m| m.is_file()).unwrap_or(false) {
                                    push(&mut files, rel, BundleRole::Template);
                                }
                            }
                            // A `templateUrl` naming a file that is not there is not an error here
                            // (it never was): the entry simply has no template file beside it.
                            Err(ConfinedOpenError::Io(_)) => {}
                            Err(ConfinedOpenError::OutsideRepo(message)) => {
                                errors.record(ScanError {
                                    repo: String::new(),
                                    path: rel,
                                    kind: "outside_repo",
                                    message,
                                })
                            }
                        }
                    }
                }
            }
            // The entry could not be re-read: its template attribute — hence possibly the file that
            // holds ALL of its markup — is unknown. The bundle is INCOMPLETE, so say so.
            Err(err) => errors.record(ScanError {
                repo: String::new(),
                path: entry_rel.to_string(),
                kind: "read_file",
                message: err.to_string(),
            }),
        }
    }

    // 2. The SIBLINGS of the same base name: templates, styles, `.variants.*`. This LISTS a
    //    directory (it yields NAMES); every file it turns into a bundle file is opened through
    //    [`open_confined`] when it is read, so a name coming out of here can never become an
    //    out-of-repo read.
    {
        let dir_abs = if dir_rel.is_empty() {
            root.to_path_buf()
        } else {
            abs_source_path(root, &dir_rel)
        };
        let listing = std::fs::read_dir(&dir_abs).map_err(|err| {
            errors.record(ScanError {
                repo: String::new(),
                path: dir_rel.clone(),
                kind: "read_dir",
                message: err.to_string(),
            });
        });
        if let Ok(entries) = listing {
            let mut siblings: Vec<(String, BundleRole)> = Vec::new();
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let sibling_name = entry.file_name().to_string_lossy().to_string();
                if sibling_name == entry_name {
                    continue;
                }
                if recipe.component_name(&sibling_name).as_deref() != Some(name.as_str()) {
                    continue;
                }
                let path = if dir_rel.is_empty() {
                    sibling_name.clone()
                } else {
                    format!("{dir_rel}/{sibling_name}")
                };
                if recipe.is_excluded(&path) {
                    continue;
                }
                // A sibling NO rule claims is a plain logic file — not part of the rendering
                // surface, and deliberately not part of the input hash either.
                let Some(role) = recipe.sibling_role(&sibling_name) else {
                    continue;
                };
                siblings.push((path, role));
            }
            // Filesystem order is not stable — sort before folding so the bundle (and its hash) is
            // deterministic across runs and machines.
            siblings.sort_by(|a, b| a.0.cmp(&b.0));
            for (path, role) in siblings {
                push(&mut files, path, role);
            }
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let input_hash = bundle_input_hash(root, &dir_rel, &files, errors);
    Some(ComponentBundle {
        name,
        root_path: dir_rel,
        entry_path: entry_rel.to_string(),
        files,
        input_hash,
    })
}

/// BUILD the bundle of an explicitly [PINNED](PinnedBundle) component — the recipe's escape hatch.
///
/// No rule is consulted: the entry is the entry, the declared files are the bundle (roles as
/// declared), the name is the declared one or the entry's derived base name. Everything MECHANICAL
/// is identical to a discovered bundle — path ordering, the input hash, the incremental loop — so a
/// pinned component is a component like any other from the catalogue's point of view.
///
/// The NAME comes from the PROJECT's recipe, never from nyx (audit 01KYM2W7KH89X33091AQJ2PN5H).
/// It used to be derived through `legacy_recipe_for_any_entry()`, i.e. through nyx's hard-coded
/// `DEFAULT_FAMILY_QUALIFIERS` (`component` / `service` / `spec` / `stories` …): on a project whose
/// convention is `card.ui.mjs`, the SAME file was named `card` when a rule discovered it and
/// `card.ui` when a pin did, and a pinned `payment.service.js` was silently renamed `payment` —
/// colliding with the `payment` the rules had found. That is framework knowledge, and it had stayed
/// in nyx precisely on the escape hatch meant to carry the project's own.
///
/// A pinned file that cannot be read is NOT silently dropped: [`bundle_input_hash`] records it as a
/// `read_input` failure, which makes the scan INCOMPLETE and therefore prunes nothing (fail-closed).
/// A pin naming a file that does not exist is thus visible in `scan_errors`, not invisible.
/// Returns `None` (recording the failure) when the pinned ENTRY itself is not a readable file.
///
/// CONFINED: every path a pin names is OPENED through [`open_confined`], so a pin can no more reach
/// outside the scanned repo than the walk can — a path that leaves the repo through an internal
/// symlink is REFUSED (an `outside_repo` scan error, hence fail-closed), never read.
fn pinned_bundle(
    root: &Path,
    pin: &PinnedBundle,
    recipe: &BundleRecipe,
    errors: &mut ScanErrors,
) -> Option<ComponentBundle> {
    let entry_rel = recipe::normalized_repo_path(&pin.entry)?;
    let entry_file = match open_confined(root, &entry_rel) {
        Ok(file) => file,
        Err(err) => {
            errors.record(ScanError {
                repo: String::new(),
                path: entry_rel.clone(),
                kind: match err {
                    ConfinedOpenError::OutsideRepo(_) => "outside_repo",
                    ConfinedOpenError::Io(_) => "read_file",
                },
                message: err.message(),
            });
            return None;
        }
    };
    match entry_file.metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return None,
        Err(err) => {
            errors.record(ScanError {
                repo: String::new(),
                path: entry_rel.clone(),
                kind: "read_file",
                message: err.to_string(),
            });
            return None;
        }
    }
    let name = match pin.name.as_deref().map(str::trim) {
        Some(explicit) if !explicit.is_empty() => explicit.to_string(),
        // No explicit name: derive it exactly as a discovered bundle would, under THIS PROJECT's
        // `family_qualifiers` — a pin overrides the FILES, not the naming rule, and the naming rule
        // is the recipe's.
        _ => recipe.component_name(file_name_of(&entry_rel))?,
    };
    let mut files: Vec<BundleFile> = vec![BundleFile {
        path: entry_rel.clone(),
        role: BundleRole::Entry,
    }];
    for file in &pin.files {
        let Some(path) = recipe::normalized_repo_path(&file.path) else {
            continue; // validation refuses these on write; a hand-edited DB cannot smuggle one in
        };
        if files.iter().any(|f| f.path == path) {
            continue; // the entry re-declared, or a duplicate: the first role wins
        }
        // A file that leaves the repo NEVER enters the bundle: not into `files` (so `read_bundle`
        // cannot hand its content to the agent), not into the input hash (so an edit OUTSIDE the
        // repo cannot move a component's hash). It is an `outside_repo` error, so the scan is
        // INCOMPLETE and nothing is pruned on its account either.
        if let Some(message) = confinement_refusal(root, &path) {
            errors.record(ScanError {
                repo: String::new(),
                path: path.clone(),
                kind: "outside_repo",
                message,
            });
            continue;
        }
        files.push(BundleFile {
            path,
            role: file.role,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let root_path = parent_slash_path(&entry_rel);
    let input_hash = bundle_input_hash(root, &root_path, &files, errors);
    Some(ComponentBundle {
        name,
        root_path,
        entry_path: entry_rel,
        files,
        input_hash,
    })
}

/// How many bytes are read PER I/O when a bundle file is STREAMED into [`bundle_input_hash`]. It
/// bounds the hash's MEMORY, never its CONTENT: every byte of every file is folded in, one 64 KiB
/// window at a time (review — a constant-size edit past a truncated read was invisible, so
/// "re-synthesize only when the source changed" silently failed on any bundle file over the cap).
const HASH_CHUNK_BYTES: usize = 64 * 1024;

/// READ at most `max_bytes` of an ALREADY-OPEN file, returning `(head, real_len)` — the BOUNDED
/// read used where a RESPONSE (or a probe window) must stay bounded: the exposed [`read_bundle`],
/// the `@Component` probe, the `templateUrl` probe. NOT used by the hash (see
/// [`bundle_input_hash`]). The real length comes from the metadata, so a truncated read still
/// reports what the file actually weighs.
///
/// It takes the FILE, not a path: the file it reads must be the very one [`open_confined`] verified
/// (audit 01KYM9KVQ0M1PA3AMA7MYCSE93 — re-opening a checked path is a TOCTOU window a concurrent
/// writer walks straight through).
///
/// Returns the I/O error rather than an `Option` (review — a failure to read was indistinguishable
/// from a legitimate "nothing there", and the discovery turned the first into the second).
fn read_bounded(file: &mut std::fs::File, max_bytes: usize) -> std::io::Result<(Vec<u8>, u64)> {
    use std::io::Read;
    let real_len = file.metadata().map(|m| m.len()).unwrap_or_default();
    let mut head = Vec::with_capacity(max_bytes.min(real_len as usize + 1));
    file.by_ref()
        .take(max_bytes as u64)
        .read_to_end(&mut head)?;
    Ok((head, real_len))
}

/// STREAM an ALREADY-OPEN file into `hasher`: its declared LENGTH first (so the framing stays
/// unambiguous for content carrying the NUL delimiter), then every one of its bytes, read through a
/// reusable [`HASH_CHUNK_BYTES`] window — the peak allocation is that window whatever the file
/// weighs.
///
/// Like [`read_bounded`], it takes the FILE [`open_confined`] verified rather than a path to
/// re-open (audit 01KYM9KVQ0M1PA3AMA7MYCSE93).
///
/// Returns the I/O error when the file could not be read to the end. The DIGEST is unchanged by
/// that reporting: a file that cannot be opened folds in NOTHING (it contributes its name only,
/// exactly as a vanished file does) and a read that fails mid-file leaves the digest over what was
/// folded so far — a change detector, never a security claim. Only the caller's KNOWLEDGE changed:
/// it now learns the hash it just computed is over partial matter.
fn hash_file_streaming(hasher: &mut Md5, file: &mut std::fs::File) -> std::io::Result<()> {
    use std::io::Read;
    let declared_len = file.metadata().map(|m| m.len()).unwrap_or_default();
    hasher.update(&declared_len.to_le_bytes());
    let mut buf = vec![0u8; HASH_CHUNK_BYTES];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The path a bundle file is hashed UNDER: its path RELATIVE TO THE BUNDLE ROOT (`root_path`), or
/// its repo-relative path when it sits outside the bundle's own directory (an Angular `templateUrl`
/// pointing at a shared folder).
///
/// The bundle's LOCATION is provenance, not content: hashing the repo-relative path made a bundle
/// MOVED wholesale on disk look like a different bundle, which is precisely what the move
/// reconciliation ([`reconcile_moved_sources`]) has to recognize. Relative-to-the-bundle still folds
/// the file NAMES in, so adding / removing / renaming a bundle file flips the hash as before.
fn bundle_relative_path<'a>(root_path: &str, file_path: &'a str) -> &'a str {
    if root_path.is_empty() {
        return file_path;
    }
    file_path
        .strip_prefix(root_path)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(file_path)
}

/// The md5 of a bundle's INPUTS: each file's BUNDLE-relative name, real LENGTH and FULL CONTENT,
/// NUL-delimited, in path order. The file names are folded in (not just the bytes) so ADDING /
/// REMOVING / RENAMING a bundle file also flips the hash. An unreadable file contributes its name
/// only — a vanished style still moves the hash. This is the incremental key of the whole synthesis
/// loop: the INPUTS are hashed, never the (non-deterministic) LLM output.
///
/// Two deliberate properties:
///   * COMPLETE — every byte is folded in, STREAMED through [`hash_file_streaming`]; the memory,
///     not the content, is bounded ([`HASH_CHUNK_BYTES`]). An edit at ANY offset flips the hash,
///     including a constant-size one megabytes into a template.
///   * LOCATION-INDEPENDENT — see [`bundle_relative_path`]: a bundle simply MOVED keeps its hash,
///     which is what lets the inventory recognize a move by CONTENT instead of by name.
///   * HONEST — a file it could not fold in is RECORDED in `errors`. The digest is a change
///     detector, so an unreadable file silently produced a DIFFERENT hash: the component looked
///     "changed" (harmless) but the repo also looked fully read (not harmless — that is what
///     authorized the élagage).
fn bundle_input_hash(
    root: &Path,
    root_path: &str,
    files: &[BundleFile],
    errors: &mut ScanErrors,
) -> String {
    let mut hasher = Md5::new();
    for file in files {
        hasher.update(bundle_relative_path(root_path, &file.path).as_bytes());
        hasher.update(&[0]);
        // CONFINED (audits 01KYM2RKGHY1WE9HTWEAJ2N2SZ / 01KYM9KVQ0M1PA3AMA7MYCSE93): the bytes
        // folded in are the bytes of the DESCRIPTOR [`open_confined`] verified — it opens every
        // segment with `O_NOFOLLOW`, so the file cannot be swapped for a symlink between the check
        // and the read. An edit to a file OUTSIDE the repo therefore cannot move a component's
        // input hash, under concurrency included: the only two outcomes are the repo file's bytes
        // and a recorded failure (which folds in the file's NAME only, exactly as a vanished file
        // does, and makes the scan incomplete — so nothing is pruned on its account).
        match open_confined(root, &file.path) {
            Ok(mut opened) => {
                if let Err(err) = hash_file_streaming(&mut hasher, &mut opened) {
                    errors.record(ScanError {
                        repo: String::new(),
                        path: file.path.clone(),
                        kind: "read_input",
                        message: err.to_string(),
                    });
                }
            }
            Err(ConfinedOpenError::OutsideRepo(message)) => errors.record(ScanError {
                repo: String::new(),
                path: file.path.clone(),
                kind: "outside_repo",
                message,
            }),
            Err(ConfinedOpenError::Io(err)) => errors.record(ScanError {
                repo: String::new(),
                path: file.path.clone(),
                kind: "read_input",
                message: err.to_string(),
            }),
        }
        hasher.update(&[0]);
    }
    hasher.finish()
}

/// DISCOVER the component BUNDLES under `root` whose ENTRY matches any of `globs`: walk the tree
/// (pruning [`IGNORED_DIRS`]), keep the candidate entries ([`is_component_entry`] — no
/// `.service.ts` / `.types.ts` / `.variants.ts` / spec / story), GROUP them per (directory, base
/// name) so a `.component.ts` and its `.component.html` form ONE component, elect each group's
/// entry ([`entry_priority`]) and resolve its bundle ([`bundle_for_entry`]).
///
/// Deterministic (name- then path-ordered). GREENFIELD-safe: a `root` that does not exist (or an
/// empty / unreadable tree) yields an EMPTY vec — never an error, never a panic (the app-neuve
/// case: the agent creates the lib later).
///
/// The bundle list ALONE, for the callers that only want it. Everything that decides a DESTRUCTION
/// must use [`scan_bundles`] instead, which also hands back what the walk failed to read.
/// LEGACY SHIM: the glob set is turned into its equivalent recipe ([`recipe_from_legacy_globs`]).
pub fn discover_bundles(root: &Path, globs: &[String]) -> Vec<ComponentBundle> {
    scan_bundles(root, globs).bundles
}

/// [`discover_bundles`] under an explicit [`BundleRecipe`] — the recipe-aware entry point.
#[cfg_attr(not(test), allow(dead_code))]
pub fn discover_bundles_with(root: &Path, recipe: &BundleRecipe) -> Vec<ComponentBundle> {
    scan_bundles_with(root, recipe).bundles
}

/// [`discover_bundles`] WITH the scan's I/O failures — the honest form of the discovery.
///
/// Same walk, same grouping, same determinism; it just stops PRETENDING it read everything. A
/// directory it could not list, an entry it could not stat, a candidate `.ts` it could not probe
/// for `@Component`, a bundle file it could not fold into the input hash: each is recorded in
/// [`RepoScan::errors`]. The inventory then refuses to prune ANYTHING in a repo whose scan is not
/// clean — because "I did not see it" and "it is not there" are not the same statement, and only the
/// second could ever justify deleting a component and its synthesis.
pub fn scan_bundles(root: &Path, globs: &[String]) -> RepoScan {
    scan_bundles_with(root, &recipe_from_legacy_globs(globs))
}

/// [`scan_bundles`] under an explicit [`BundleRecipe`] — THE discovery, and the only place the
/// recipe and the walk meet.
///
/// nyx still does all the mechanics: the walk (pruning [`IGNORED_DIRS`]), the grouping per
/// (directory, base name), the entry election, the bundling, the hash, the ordering. The RECIPE
/// answers, and only answers, the framework question: which files may open a bundle, what content
/// they must carry to count, how their base name is derived, which qualifiers never open one, what
/// is excluded — plus the [PINS](PinnedBundle), which are added AFTER the rule-driven pass and win
/// over it.
pub fn scan_bundles_with(root: &Path, recipe: &BundleRecipe) -> RepoScan {
    let mut errors = ScanErrors::default();
    let mut rels = Vec::new();
    // The directories the WALK prunes are the recipe's (nyx's [`IGNORED_DIRS`] by default) — see
    // [`recipe::BundleRecipe::ignored_dirs`] — UNIONED with the floor no recipe can lift
    // ([`MANDATORY_IGNORED_DIRS`]: the dependency tree and the VCS internals). The union is built by
    // [`walk_pruned_dirs`] and applied to the WALK itself, so it holds for a recipe built in memory
    // as well as for one parsed from JSON (audit 01KYM9MN6RF9KTA5M4ZW75HAXP — `ignored_dirs: []`
    // walked 68 700 extra files and catalogued 14 229 components out of `node_modules`, ×2030 the
    // default scan) — and so the TOKEN ingest applies the very same set (finding
    // 01KYSCX562SWDXZBBAPX397X8F).
    let ignored_dirs = walk_pruned_dirs(recipe);
    walk_files(root, root, &ignored_dirs, &mut rels, &mut errors);
    rels.sort();

    // Group the candidate entries by (directory, base name) — the BUNDLE key. Each candidate keeps
    // the INDEX of the entry rule that elected it: the recipe's rule ORDER is the entry preference
    // (an Angular `.component.ts` CLASS over its `.component.html`, a single-file `.tsx` over a
    // loose `.ts`), and the path breaks the ties inside one rule.
    let mut groups: std::collections::BTreeMap<(String, String), Vec<(usize, String)>> =
        std::collections::BTreeMap::new();
    // Every path a PIN claims (its entry AND its files), normalized once. The rule-driven pass
    // SKIPS them: the pin loop below is the sole authority on a pinned component, and assembling it
    // twice cost double the I/O and double-counted its `scan_errors` — one missing pinned file
    // reported TWO failures for one cause, on the very counter the human reads to decide whether a
    // scan may prune (audit 01KYM2W82XBPJXW38GMXZDRJDP).
    let pinned_paths: std::collections::BTreeSet<String> = recipe
        .pins
        .iter()
        .flat_map(|pin| std::iter::once(&pin.entry).chain(pin.files.iter().map(|file| &file.path)))
        .filter_map(|path| recipe::normalized_repo_path(path))
        .collect();
    for rel in rels {
        let file_name = file_name_of(&rel);
        if pinned_paths.contains(&rel) {
            continue;
        }
        if recipe.is_excluded(&rel) || !recipe.opens_bundle(file_name) {
            continue;
        }
        let Some((rule_index, rule)) = recipe.entry_rule_for(&rel, file_name) else {
            continue;
        };
        let Some(name) = recipe.component_name(file_name) else {
            continue;
        };
        // A broad net (`**/*.ts`) is a component ONLY when the file CARRIES THE MARKER the recipe
        // demands (`@Component`, `defineWidget(`) — the convention-independent check that lets the
        // Angular net take every `.ts` (modern layout: `button.ts` + `button.html`) without
        // cataloguing every util / store / route file. A file that could not be READ is NOT a "no":
        // it is an unknown, recorded as such (an Angular front where ONE `.ts` is momentarily
        // unreadable used to lose that component's row and its template — one failed read sufficed).
        //
        // The probe opens the candidate through [`open_confined`] like every other read: the walk
        // never yields a symlink, but a concurrent writer can turn the file into one BETWEEN the
        // walk and this probe (audit 01KYM9KVQ0M1PA3AMA7MYCSE93) — and the answer here decides
        // whether the component exists at all.
        if let Some(marker) = rule.requires_content.as_deref() {
            let probed = open_confined(root, &rel).and_then(|mut file| {
                declares_marker(&mut file, marker).map_err(ConfinedOpenError::Io)
            });
            match probed {
                Ok(true) => {}
                Ok(false) => continue,
                Err(err) => {
                    errors.record(ScanError {
                        repo: String::new(),
                        path: rel.clone(),
                        kind: match err {
                            ConfinedOpenError::OutsideRepo(_) => "outside_repo",
                            ConfinedOpenError::Io(_) => "read_file",
                        },
                        message: err.message(),
                    });
                    continue;
                }
            }
        }
        groups
            .entry((parent_slash_path(&rel), name))
            .or_default()
            .push((rule_index, rel));
    }

    let mut out = Vec::new();
    for (_key, mut candidates) in groups {
        candidates.sort();
        if let Some(bundle) =
            bundle_for_entry_reporting(root, &candidates[0].1, recipe, &mut errors)
        {
            out.push(bundle);
        }
    }
    // The PINS — what no rule could express. The rule-driven pass above already skipped every path
    // a pin claims (`pinned_paths`), so this loop is the ONLY place a pinned component is
    // assembled; the `retain` below is the belt to that brace — it costs nothing and keeps the
    // "a file a pin claims never also stands as a component of its own" rule true even if a future
    // pin path normalized differently from the walk's.
    for pin in &recipe.pins {
        let Some(bundle) = pinned_bundle(root, pin, recipe, &mut errors) else {
            continue;
        };
        let claimed: std::collections::BTreeSet<&str> =
            bundle.files.iter().map(|f| f.path.as_str()).collect();
        out.retain(|b| !claimed.contains(b.entry_path.as_str()));
        out.push(bundle);
    }
    out.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.entry_path.cmp(&b.entry_path))
    });
    RepoScan {
        bundles: out,
        errors,
    }
}

/// Does an ALREADY-OPEN candidate CARRY the recipe's declaration marker (`@Component`,
/// `defineWidget(`)? A bounded, dependency-free CONTENT probe (the first
/// [`DECLARATION_PROBE_BYTES`] of the file), NOT a parser: the review forbids a heavy Angular/TS
/// dependency, and "is this class a component?" is answered exactly by the marker's presence.
///
/// An UNREADABLE file is an `Err`, NOT a `false` (review — the critical data-loss path): on a broad
/// net, where every `.ts` is a candidate and the marker alone tells components from utils, "I could
/// not read it" used to mean "it is not a component", which meant "its row has no source any more",
/// which meant a DELETE of a synthesized template. One failed read was enough.
fn declares_marker(file: &mut std::fs::File, marker: &str) -> std::io::Result<bool> {
    let (head, _bytes) = read_bounded(file, DECLARATION_PROBE_BYTES)?;
    Ok(String::from_utf8_lossy(&head).contains(marker))
}

/// How many bytes of a candidate `.ts` are probed for a component declaration — enough to cover
/// the imports + the decorator of any real component, bounded so a generated megabyte-sized file
/// cannot slow the walk down.
const DECLARATION_PROBE_BYTES: usize = 128 * 1024;

/// Recursively collect the repo-relative (`/`-separated) paths of every FILE under `dir`,
/// pruning the directory names in `ignored_dirs` (the RECIPE's — [`IGNORED_DIRS`] by default).
/// Still NON-FATAL — an unreadable subtree does not abort the walk, the rest of the repo is scanned
/// normally — but no longer SILENT: every failure is recorded in `errors` so the caller knows the
/// result is partial.
///
/// SYMLINKS ARE NOT FOLLOWED: entries are typed with `entry.file_type()` (an `lstat`), so a symlink
/// is neither a file nor a directory here and is simply skipped. That is the reference the rest of
/// the module holds itself to — see [`open_confined`], which enforces the same rule with
/// `O_NOFOLLOW` at the moment it opens.
///
/// The one failure that is NOT an error: a `root` that does not exist at all. That is the GREENFIELD
/// case (a project with no front yet), and the élagage already has a dedicated, stricter guard for
/// it (`repo_not_scanned`).
fn walk_files(
    root: &Path,
    dir: &Path,
    ignored_dirs: &std::collections::BTreeSet<&str>,
    out: &mut Vec<String>,
    errors: &mut ScanErrors,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            if !(dir == root && err.kind() == std::io::ErrorKind::NotFound) {
                errors.record(ScanError {
                    repo: String::new(),
                    path: scan_path(root, dir),
                    kind: "read_dir",
                    message: err.to_string(),
                });
            }
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                errors.record(ScanError {
                    repo: String::new(),
                    path: scan_path(root, dir),
                    kind: "read_entry",
                    message: err.to_string(),
                });
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(err) => {
                errors.record(ScanError {
                    repo: String::new(),
                    path: scan_path(root, &path),
                    kind: "read_entry",
                    message: err.to_string(),
                });
                continue;
            }
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if ignored_dirs.contains(name.as_ref()) {
                continue;
            }
            walk_files(root, &path, ignored_dirs, out, errors);
        } else if file_type.is_file() {
            if let Some(rel) = relative_slash_path(root, &path) {
                out.push(rel);
            }
        }
    }
}

/// The path of `full` RELATIVE to `root`, with OS separators normalized to `/`. `None` when
/// `full` is not under `root` (never expected from the walk). Used so glob matching and stored
/// `source_path` are platform-stable.
fn relative_slash_path(root: &Path, full: &Path) -> Option<String> {
    let rel = full.strip_prefix(root).ok()?;
    let s = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// INVENTORY the component BUNDLES of a repo into `project_id`: [`discover_bundles`] under `root`
/// with `globs`, [`assign_catalogue_names`] over the discovered set, then UPSERT each bundle's
/// DISCOVERY (name + provenance repo + entry path) into `design_components` — keyed on the bundle's
/// SOURCE, so a re-run reconciles in place and two homonymous bundles never collapse onto one row.
/// Returns the stored [`DesignComponent`]s, name-ordered.
///
/// DISCOVERY ONLY (review #40): it records WHAT exists, never a template — the SYNTHESIS is the
/// agent's job (`design_refresh_component`), and an already-synthesized component keeps its
/// `template` / `params_json` / `states_json` / hash untouched ([`db::upsert_component_discovery`]).
///
/// GREENFIELD: an app with no front yet (a `root` that is missing/empty, or nothing matches)
/// inventories NOTHING and returns an EMPTY vec WITHOUT error — the "amorçage greenfield sans
/// erreur" done-criterion.
#[cfg_attr(not(test), allow(dead_code))]
pub fn inventory_components(
    conn: &mut SqliteConnection,
    project_id: &str,
    source_repository_id: Option<&str>,
    root: &Path,
    globs: &[String],
) -> QueryResult<Vec<DesignComponent>> {
    let discovered: Vec<DiscoveredBundle> = discover_bundles(root, globs)
        .into_iter()
        .map(|bundle| DiscoveredBundle {
            repo_id: source_repository_id.map(str::to_string),
            repo_label: String::new(),
            bundle,
        })
        .collect();
    let existing = db::list_components(conn, project_id)?;
    for (discovery, name) in discovered
        .iter()
        .zip(assign_catalogue_names(&existing, &discovered))
    {
        // `None` = the row this discovery would have reconciled was removed by an EXPLICIT delete
        // in between: nothing to reconcile, and the next run will simply insert it. Skipped, never
        // fatal (audit 01KYHYW25GKJBRTZVTSXQAMP9F).
        db::upsert_component_discovery(
            conn,
            project_id,
            &name,
            discovery.repo_id.as_deref(),
            &discovery.bundle.entry_path,
        )?;
    }
    db::list_components(conn, project_id)
}

/// Match a repo-relative, `/`-separated `path` against a glob `pattern`. Supports the three
/// standard wildcards, dependency-free (no `glob`/`globset` crate):
///
/// * `?`   — exactly one CHARACTER (a whole UTF-8 code point), never a `/`.
/// * `*`   — any run of characters NOT crossing a `/` (stays inside one path segment).
/// * `**/` — zero or more WHOLE path SEGMENTS, `/` included. Zero is allowed, so `**/*.tsx`
///   matches a root-level `Button.tsx` as well as `src/ui/Button.tsx`.
/// * `**`  — when NOT followed by a `/` (a trailing `**`, or a `**` glued inside a segment): any
///   run of characters, `/` included.
///
/// Everything else is a literal byte.
///
/// # SEGMENT ANCHORING (review — non-standard, undocumented glob semantics)
///
/// `**/` is ANCHORED to segment boundaries, the semantics minimatch / bash `globstar` give it and
/// the one an agent writing a `globs` override expects. It used to absorb any run of BYTES, so
/// `**/Button.tsx` also matched `MyButton.tsx` and `**/components/**/*.ts` quietly swept a
/// `xcomponents/` folder into the catalogue — silently, on a value the agent PERSISTS.
///
/// Two documented consequences of matching FILE paths (a glob here is only ever applied to a file
/// the walk found, never to a directory):
///   * `src/**` does NOT match the bare `src` (globstar special-cases the directory itself; there
///     is no directory to match here). It does match `src/a.ts`, `src/a/b.ts`, …
///   * a `**` glued inside a segment (`a**b`) is NOT a globstar — it is the unanchored run above.
///
/// `?` consumes a whole CODE POINT (`?` matches `é`, `??` does not), so a non-ASCII file name
/// counts characters the way a human does. The matcher never slices a `str` — it compares bytes and
/// steps by UTF-8 lead-byte length — so any input, valid UTF-8 or not, is a plain `false`/`true`,
/// never a panic.
///
/// # Why a DP and not backtracking (review — catastrophic backtracking)
///
/// The first implementation recursed on every `**` / `*`, re-exploring the same (pattern position,
/// path position) pair exponentially often: a pattern with 9 `**` took 8.1 SECONDS on a 46-char
/// path, ×20 per extra `**` — and `design_inventory` lets an AGENT persist an arbitrary `globs`
/// override, so one unlucky pattern froze every later inventory (incl. the `/design` button). The
/// matcher is a bottom-up dynamic program over `(token index, path index)`: each pair is decided
/// ONCE, so the cost is a hard `O(tokens × path bytes)` — no backtracking to explode. Rolling two
/// rows keeps the memory at `O(path bytes)`.
///
/// That rewrite was ISO-SEMANTIC: it fixed a COST, not a result (a differential over 496 860
/// pattern/path pairs found ZERO divergence with the recursive matcher it replaced). The semantic
/// change is the segment anchoring above, which is deliberate and separate.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let tokens = glob_tokens(pattern.as_bytes());
    let s = path.as_bytes();
    let m = s.len();

    // `next[j]` = "tokens[i+1..] matches s[j..]", `row[j]` = "tokens[i..] matches s[j..]".
    // Seeded with the EMPTY pattern: it matches only the empty remainder of the path.
    let mut next: Vec<bool> = (0..=m).map(|j| j == m).collect();
    let mut row: Vec<bool> = vec![false; m + 1];
    // `some[j]` = "`**/` matches s[j..k] absorbing AT LEAST one whole segment, and the rest matches
    // s[k..]" — the auxiliary row that anchors the globstar without a second pass.
    let mut some: Vec<bool> = vec![false; m + 1];
    for token in tokens.iter().rev() {
        // `j` descends because a star's row depends on ITSELF one char further right.
        for j in (0..=m).rev() {
            row[j] = match token {
                // `**/` — absorb NOTHING, or whole segments: everything up to and including a `/`.
                GlobToken::AnySegments => {
                    some[j] = j < m && ((s[j] == b'/' && next[j + 1]) || some[j + 1]);
                    next[j] || some[j]
                }
                // `**` not followed by a `/` — absorb nothing, or one more char (a `/` included).
                GlobToken::DoubleStar => next[j] || (j < m && row[j + 1]),
                // `*` — absorb nothing, or one more NON-`/` char (never leaves the segment).
                GlobToken::Star => next[j] || (j < m && s[j] != b'/' && row[j + 1]),
                // `?` — exactly one CODE POINT, never a `/` and never a dangling continuation byte.
                GlobToken::AnyChar => match utf8_char_len(s, j) {
                    Some(len) if s[j] != b'/' => next[j + len],
                    _ => false,
                },
                GlobToken::Literal(c) => j < m && s[j] == *c && next[j + 1],
            };
        }
        std::mem::swap(&mut next, &mut row);
    }
    next[0]
}

/// The BYTE length of the UTF-8 code point starting at `s[j]`, or `None` when `j` is past the end,
/// sits on a continuation byte, or the sequence would run past the end. Never slices — a malformed
/// byte simply fails to match.
fn utf8_char_len(s: &[u8], j: usize) -> Option<usize> {
    let lead = *s.get(j)?;
    let len = match lead {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        // A continuation byte (or an invalid lead) is not the start of a character.
        _ => return None,
    };
    (j + len <= s.len()).then_some(len)
}

/// One unit of a compiled glob [`pattern`](glob_match) — the alphabet the matcher's DP runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobToken {
    /// `**/`: zero or more WHOLE path segments (each absorbed run ends on a `/`) — so `**/*.tsx`
    /// also matches a root-level `Button.tsx`, but `**/Button.tsx` does NOT match `MyButton.tsx`.
    AnySegments,
    /// `**` NOT followed by a `/` (trailing, or glued inside a segment): any run of chars, `/`
    /// INCLUDED, possibly empty.
    DoubleStar,
    /// `*`: any run of chars WITHIN one path segment (never crosses a `/`), possibly empty.
    Star,
    /// `?`: exactly one CODE POINT, not a `/`.
    AnyChar,
    /// Anything else: one literal byte.
    Literal(u8),
}

/// COMPILE a glob pattern into [`GlobToken`]s. A `**` FOLLOWED BY A `/` swallows that `/` and
/// becomes the segment-anchored [`GlobToken::AnySegments`] (so it can match ZERO directories); a
/// `**` that is trailing or glued inside a segment stays the unanchored [`GlobToken::DoubleStar`].
/// A longer run of `*` collapses onto the same tokens, which already absorb the leftovers.
fn glob_tokens(p: &[u8]) -> Vec<GlobToken> {
    let mut tokens = Vec::with_capacity(p.len());
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            b'*' if i + 1 < p.len() && p[i + 1] == b'*' => {
                i += 2;
                // Collapse a longer `***…` run onto the same token.
                while i < p.len() && p[i] == b'*' {
                    i += 1;
                }
                if i < p.len() && p[i] == b'/' {
                    i += 1;
                    tokens.push(GlobToken::AnySegments);
                } else {
                    tokens.push(GlobToken::DoubleStar);
                }
            }
            b'*' => {
                i += 1;
                tokens.push(GlobToken::Star);
            }
            b'?' => {
                i += 1;
                tokens.push(GlobToken::AnyChar);
            }
            c => {
                i += 1;
                tokens.push(GlobToken::Literal(c));
            }
        }
    }
    tokens
}

// =============================================================================
// INCREMENTAL synthesis loop — hash the INPUTS, never the output
// =============================================================================
//
// The catalogue must stay FRESH without a bulk re-synthesis (an LLM pass over 100+ bundles is
// neither free nor deterministic). So `design_components.visual_hash` stores the bundle's INPUT
// hash AS OF THE LAST SYNTHESIS ([`mark_synthesized`], called by the authoring tool). A bundle
// whose current [`ComponentBundle::input_hash`] equals the stored one is UP TO DATE and is never
// re-synthesized; a fresh bundle (no stored hash) or a changed one is PENDING.

/// A bundle AWAITING (re-)synthesis — the targeted work list the agent consumes. Never the whole
/// catalogue: only what actually changed (or was never synthesized).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingSynthesis {
    /// The component name (the key the authoring tool refreshes).
    pub name: String,
    /// The bundle's ENTRY path (repo-relative) — where to read it.
    pub entry_path: String,
    /// The bundle's CURRENT input hash (what a synthesis would be recorded at).
    pub input_hash: String,
    /// Why it is pending: `never_synthesized` (no stored hash / empty template) or
    /// `source_changed` (the bundle's inputs moved since the last synthesis).
    pub reason: &'static str,
}

/// A catalogue row whose recorded SOURCE is no longer there — the LIFE-CYCLE signal the inventory
/// used to swallow entirely (the `pending` loop only ever iterated DISCOVERED bundles, so a row
/// whose file vanished was never re-proposed, never reported, and still counted as synthesized).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OrphanComponent {
    /// The catalogue row's id (what the delete tool takes).
    pub id: String,
    /// Its catalogue name (what a composition references it by).
    pub name: String,
    /// The `source_path` it still points at — gone from disk, or no longer matched by the globs.
    pub source_path: String,
    /// Was it REALLY REMOVED by this run, or merely reported? `true` means the `DELETE` affected
    /// this row — never merely that the run intended to delete it. See [`prune_orphans`] for the
    /// guards that spare a row.
    pub pruned: bool,
    /// Why it was spared (`None` when it was pruned) — one of [`KEPT_REPO_NOT_SCANNED`],
    /// [`KEPT_SCAN_INCOMPLETE`], [`KEPT_EMPTY_SCAN`], [`KEPT_SYNTHESIZED`] or
    /// [`KEPT_CONTENT_APPEARED`].
    pub kept_because: Option<&'static str>,
    /// Does the row carry the PERSISTENT orphan flag (`design_components.orphaned_at`)? RAISED by
    /// this run for [`KEPT_SYNTHESIZED`] / [`KEPT_CONTENT_APPEARED`] — a scan that WAS in a position
    /// to judge found the source gone while the row held authored content. The other reasons mean
    /// "this run could not judge": they leave the flag untouched, and this field then reports
    /// whether a PREVIOUS, competent run had raised it.
    pub flagged_orphan: bool,
}

/// Spared: the row's provenance repo is not a member of the root workspace, or its root is missing
/// on disk. The run never looked at it — it cannot conclude anything about it.
pub const KEPT_REPO_NOT_SCANNED: &str = "repo_not_scanned";

/// Spared: the repo's scan met at least ONE I/O failure ([`ScanError`]). The walk read PART of the
/// tree, so "not rediscovered" may just mean "in the part that could not be read". FAIL-CLOSED — a
/// scan that did not read everything prunes nothing at all in that repo.
pub const KEPT_SCAN_INCOMPLETE: &str = "scan_incomplete";

/// Spared: the repo is there and fully readable but discovered NOTHING while the catalogue holds
/// rows for it. An empty scan is a mis-configuration signal (a wrong glob override, a half-checked
/// out repo), never an order to purge a whole front.
pub const KEPT_EMPTY_SCAN: &str = "empty_scan";

/// Spared: the row carries AUTHORED CONTENT ([`carries_synthesis`] — a non-empty template, a
/// recorded hash, a params shape or declared states) — work a scan cannot re-derive. It is FLAGGED
/// orphan (persistent) and kept; its removal is an EXPLICIT gesture (`design_delete_component`, or
/// the `/design` card's delete), never a side effect of a scan.
pub const KEPT_SYNTHESIZED: &str = "synthesized";

/// Spared BY THE `DELETE` ITSELF: the guards cleared this row for removal on the run's SNAPSHOT, but
/// a concurrent write (a `design_refresh_component` that answered OK to its agent, a
/// `proto_author_component`) put content on it in between, so the conditional
/// [`db::delete_component_if_unauthored`] affected ZERO rows.
///
/// The race the CRITICAL review 01KYHV8DJ7XZG9F5TCRG2Q17Q1 measured — a 300-component inventory and
/// a refresh landing 120 µs into its prune loop — resolved in favour of the content. Reported like
/// any other sparing, and FLAGGED orphan: the source really is gone, and the row really does hold
/// something now.
pub const KEPT_CONTENT_APPEARED: &str = "content_appeared";

/// The OUTCOME of a project-wide inventory run — what the trigger surfaces to the caller (MCP
/// result / UI toast) so a human sees the catalogue moved. `Serialize` for the IPC/MCP boundary.
///
/// Deliberately NOT carrying the full catalogue's CONTENT: a real front has 100+ components, and
/// the MCP tool bounds what it echoes (counters + a bounded bundle list, contents on demand).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectInventory {
    /// Every BUNDLE discovered on disk this run (paths + input hash only, no content).
    pub bundles: Vec<ComponentBundle>,
    /// How many bundles were DISCOVERED on disk this run (across every scanned repo).
    pub discovered: usize,
    /// The repo roots WALKED (the project's non-optional repos in the root workspace; the
    /// current-folder fallback when the project has none configured). 0 ⇒ no on-disk binding.
    pub repos_scanned: usize,
    /// The catalogue SIZE after the run (what `/design` lists).
    pub component_count: usize,
    /// How many catalogued components carry a SYNTHESIZED template (non-empty).
    pub synthesized_count: usize,
    /// The TARGETED work list: the bundles whose synthesis is missing or stale.
    pub pending: Vec<PendingSynthesis>,
    /// The rows whose recorded source vanished this run — pruned or spared, each saying which.
    /// A greenfield `[NEW]` (no `source_path`) is never an orphan: it has no file by construction.
    pub orphans: Vec<OrphanComponent>,
    /// The rows RE-ANCHORED onto a moved bundle this run, as `(name, new entry path)`. A move is
    /// recognized by CONTENT, so the row keeps its id, its name, its synthesis and every frozen
    /// composition reference.
    pub moved: Vec<(String, String)>,
    /// The greenfield `[NEW]` rows a DISCOVERED bundle MATERIALIZED this run, as
    /// `(name, entry path)` (review #41). Adoption by name is the intended way a `[NEW]` the front
    /// now implements turns `[DS]` in place instead of being duplicated — but it used to be the ONLY
    /// life-cycle event the inventory did not report, so a node silently left `new_components` and
    /// the coder silently stopped being asked to build it.
    pub materialized: Vec<(String, String)>,
    /// What the walk FAILED to read this run, across every repo. Non-empty ⇒ the discovery is
    /// PARTIAL, and every orphan of an affected repo was spared ([`KEPT_SCAN_INCOMPLETE`]).
    pub scan_errors: ScanErrors,
    /// Did this run actually WRITE to the catalogue (an insert / rename / re-anchor / prune / orphan
    /// flag)? The callers use it to emit a `changed` notification only when something really moved.
    pub catalogue_changed: bool,
}

/// One repo ROOT of a project, as the discovery walks it: its provenance id, its human label and
/// its absolute path. Shared with [`crate::design_tokens`], so the TOKEN ingest reads exactly the
/// same repos as the component inventory (one resolution rule, never two).
#[derive(Debug, Clone)]
pub(crate) struct RepoRoot {
    /// The provenance repository id — `None` for the synthetic current-folder FALLBACK, whose id is
    /// not a persisted row and would violate the FK.
    pub(crate) repo_id: Option<String>,
    /// The repo's NAME — only used as the LAST-RESORT discriminator when two repos of the same
    /// project expose the SAME relative path (see [`name_candidates`]).
    pub(crate) label: String,
    /// The absolute on-disk path of the repo in the root workspace.
    pub(crate) root: PathBuf,
}

/// The project's repo ROOTS, resolved exactly as the dev context does — through the ROOT
/// workspace's [`crate::repository::workspace_members`] (`root_path` + each repo's
/// `relative_path`, PDR-0013). GREENFIELD-safe: a project with no live workspace resolves to an
/// EMPTY list.
pub(crate) fn project_repo_roots(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<Vec<RepoRoot>> {
    let workspace = db::list_workspaces(conn, project_id)?
        .into_iter()
        .find(|w| w.archived_at.is_none());
    let members = match &workspace {
        Some(ws) => crate::repository::workspace_members(conn, ws)?,
        None => Vec::new(),
    };
    Ok(members
        .into_iter()
        .map(|member| {
            let repo_id = if member
                .repository
                .id
                .starts_with(crate::repository::FALLBACK_REPO_ID_PREFIX)
            {
                None
            } else {
                Some(member.repository.id.clone())
            };
            RepoRoot {
                repo_id,
                label: member.repository.name.clone(),
                root: PathBuf::from(member.absolute_path),
            }
        })
        .collect())
}

// =============================================================================
// CATALOGUE IDENTITY — one row per BUNDLE, one unambiguous NAME per row
// =============================================================================
//
// The discovery groups bundles per (directory, base name), so two components can legitimately share
// a base name (`data-table.ts` under `catalog/list/…` AND under `billing/invoice-detail/…` on
// one Angular front; `index.tsx` per folder in React; `header.ts` / `card.ts` per feature in Angular). The
// catalogue's `name` is UNIQUE per project — it is the key `proto_composition` compositions,
// `design_refresh_component` and the `/design` UI all resolve a component by — so a name collision
// used to make the two bundles UPSERT onto ONE row: one component silently lost, and the loser
// re-proposed as `pending` on EVERY run (an infinite re-synthesis loop for the agent).
//
// The fix separates the two notions:
//   * a discovered component's IDENTITY is its SOURCE (`source_repository_id` + `source_path`) —
//     that is what [`db::upsert_component_discovery`] keys on, so two homonyms are two rows;
//   * its NAME is only its LABEL — kept readable (the bare base name) whenever it is free, and
//     QUALIFIED with the shortest distinguishing directory suffix when it is not.
//
// A bundle already catalogued under its own source KEEPS its stored name, so discovering a homonym
// never renames (nor un-synthesizes) a component an existing composition references by name.

/// One DISCOVERED bundle together with the repo it came from — the unit the catalogue names and
/// upserts.
#[derive(Debug, Clone)]
pub struct DiscoveredBundle {
    /// The provenance repository id (`None` for a loose scan / the current-folder fallback).
    pub repo_id: Option<String>,
    /// The repo's human label — the last-resort discriminator (see [`name_candidates`]).
    pub repo_label: String,
    /// The bundle itself, carrying its BASE name (the preferred catalogue name).
    pub bundle: ComponentBundle,
}

/// The catalogue NAME candidates for a bundle, from the most READABLE to the most QUALIFIED:
/// the bare `base` name, then `base` prefixed by 1, 2, … directory segments of its entry path, then
/// (last resort) the repo label + the full directory. Directory segments that merely REPEAT the
/// base name are dropped (`…/data-table/data-table.ts` qualifies as `components/data-table`, not
/// `data-table/data-table`). PURE — no disk, no DB.
pub fn name_candidates(repo_label: &str, entry_path: &str, base: &str) -> Vec<String> {
    let dir = parent_slash_path(entry_path);
    let mut segments: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    while matches!(segments.last(), Some(last) if last.eq_ignore_ascii_case(base)) {
        segments.pop();
    }

    let mut out = vec![base.to_string()];
    for depth in 1..=segments.len() {
        out.push(format!(
            "{}/{base}",
            segments[segments.len() - depth..].join("/")
        ));
    }
    // Two REPOS can expose the very same relative path — the directory alone discriminates nothing
    // then, so the repo's own label opens one more level of qualification.
    if !repo_label.is_empty() {
        out.push(if dir.is_empty() {
            format!("{repo_label}/{base}")
        } else {
            format!("{repo_label}/{dir}/{base}")
        });
    }
    out.dedup();
    out
}

/// ASSIGN a UNIQUE catalogue NAME to every discovered bundle — the single place the catalogue's
/// identity rule lives. Returns one name per entry of `discovered`, in the SAME order.
///
/// Three passes:
///   1. a bundle already catalogued UNDER ITS OWN SOURCE keeps its STORED name. Its identity is the
///      source, so a homonym appearing elsewhere in the tree can never rename it — an existing
///      `proto_composition` reference, and its recorded synthesis, both survive untouched.
///   2. every OTHER catalogued row that CARRIES A SOURCE reserves its name too, even though no
///      discovered bundle claims it this run (its repo may simply not have been scanned). Its name
///      designates a LIVE component: letting a homonymous newcomer take it is exactly the hijack
///      review — adoption by name — reported, and it would also collide with
///      `UNIQUE(project_id, name)` on the insert. The newcomer takes a qualified name instead.
///   3. every remaining bundle takes the FIRST FREE [`name_candidates`] entry (bare base name
///      first, so the readable name is only given up when it is genuinely ambiguous). Iterated in a
///      deterministic (repo, entry path) order, so re-scanning the same tree always yields the same
///      names.
///
/// A name held by a GREENFIELD row (no `source_path`) stays FREE on purpose: that is how a `[NEW]`
/// the front now implements is MATERIALIZED in place instead of being duplicated (review #41).
/// A component simply MOVED on disk is re-anchored earlier, by CONTENT
/// ([`reconcile_moved_sources`]), so it reaches pass 1 already matching its new source.
///
/// Total: the numbered `base~2`, `base~3`… tail guarantees a free name even when every candidate is
/// taken (two repos with the same label AND the same relative path).
pub fn assign_catalogue_names(
    existing: &[DesignComponent],
    discovered: &[DiscoveredBundle],
) -> Vec<String> {
    let mut names: Vec<Option<String>> = vec![None; discovered.len()];
    let mut taken: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // Pass 1 — the source-anchored names. The OLDEST row wins when (pathologically) two rows share
    // one source, the SAME tie-break `db::find_component_by_source` applies, so the naming pass and
    // the upsert can never disagree about which row a bundle is (they used to order by NAME vs by
    // ID: the upsert would then try to rename a row onto a name the other row holds, violating
    // `UNIQUE(project_id, name)` and failing the WHOLE inventory on every run).
    for (index, discovery) in discovered.iter().enumerate() {
        let stored = existing
            .iter()
            .filter(|c| {
                c.source_path.as_deref() == Some(discovery.bundle.entry_path.as_str())
                    && c.source_repository_id.as_deref() == discovery.repo_id.as_deref()
            })
            .min_by(|a, b| a.id.cmp(&b.id));
        if let Some(row) = stored {
            if taken.insert(row.name.clone()) {
                names[index] = Some(row.name.clone());
            }
        }
    }

    // Pass 2 — RESERVE the name of every other SOURCED row (a live component not rediscovered this
    // run). A greenfield row's name is deliberately NOT reserved: it is adoptable.
    for row in existing.iter().filter(|c| c.source_path.is_some()) {
        taken.insert(row.name.clone());
    }

    // Pass 3 — the free ones, in a stable order.
    let mut order: Vec<usize> = (0..discovered.len())
        .filter(|index| names[*index].is_none())
        .collect();
    let sort_key = |d: &DiscoveredBundle| {
        (
            d.repo_label.clone(),
            d.repo_id.clone().unwrap_or_default(),
            d.bundle.entry_path.clone(),
        )
    };
    order.sort_by_key(|index| sort_key(&discovered[*index]));
    for index in order {
        let discovery = &discovered[index];
        let base = discovery.bundle.name.as_str();
        let chosen = name_candidates(&discovery.repo_label, &discovery.bundle.entry_path, base)
            .into_iter()
            .find(|candidate| !taken.contains(candidate))
            .unwrap_or_else(|| {
                let mut nth = 2usize;
                loop {
                    let candidate = format!("{base}~{nth}");
                    if !taken.contains(&candidate) {
                        return candidate;
                    }
                    nth += 1;
                }
            });
        taken.insert(chosen.clone());
        names[index] = Some(chosen);
    }

    names
        .into_iter()
        .zip(discovered)
        .map(|(name, discovery)| name.unwrap_or_else(|| discovery.bundle.name.clone()))
        .collect()
}

// =============================================================================
// LIFE CYCLE — what becomes of a row whose SOURCE is gone (reviews #44, #57, #58)
// =============================================================================
//
// A catalogue row's identity is its SOURCE, so the day that source disappears the row must go
// SOMEWHERE. It used to go nowhere: `pending` only ever iterated the DISCOVERED bundles, so a row
// whose file vanished stayed in the catalogue forever with a stale synthesis, was counted as
// synthesized, was never re-proposed and was never even reported — while a homonymous newcomer
// could quietly ADOPT it by name. The life cycle is now explicit and has exactly three outcomes:
//
//   1. MOVED — the source vanished AND an unclaimed bundle carries the row's EXACT content
//      ([`reconcile_moved_sources`]). The row is re-anchored: same id, same name, same synthesis,
//      so every frozen composition reference survives. Decided on CONTENT, never on the name, so it
//      works identically for a bare name and for a path-qualified one.
//   2. ORPHANED and PRUNED — the source is simply gone (deleted, or no longer matched by the
//      globs) AND the row was NEVER SYNTHESIZED. It is removed ([`prune_orphans`]) and REPORTED, so
//      the catalogue stops carrying entries no glob change could ever remove. This is the ONLY
//      automatic destruction left, and it is LOSSLESS: a re-scan rebuilds such a row for free.
//   3. ORPHANED and SPARED — either the run is not in a position to judge (its repo was not
//      scanned, its scan met an I/O failure, or that repo discovered nothing at all), or the row
//      CARRIES A SYNTHESIS. Reported, never removed; the synthesized case is additionally FLAGGED
//      (`design_components.orphaned_at`) so it stays visible in `/design` until someone decides.
//      The explicit delete (`design_delete_component` / the `/design` card action) is the escape
//      hatch — the review's rule: an ABSENCE is never a PROOF, so no scan ever destroys work.
//
// A GREENFIELD row (`source_path IS NULL`) is NEVER any of the three: it has no file by
// construction — it is a `[NEW]` waiting to be built. Its one life-cycle event is
// MATERIALIZATION (a discovered bundle takes its name and binds its provenance, review #41), which
// is reported in [`ProjectInventory::materialized`].

/// Is `component` a row this run could judge — i.e. was its provenance repo actually WALKED? A row
/// whose repo is not a member of the root workspace (or whose root is missing on disk) is invisible
/// to this run: it must never be pruned on the strength of a scan that never looked at it.
fn repo_was_scanned(roots: &[RepoRoot], component: &DesignComponent) -> bool {
    roots.iter().any(|r| {
        r.repo_id.as_deref() == component.source_repository_id.as_deref() && r.root.is_dir()
    })
}

/// RE-ANCHOR the rows whose source VANISHED onto the bundle that carries their CONTENT — the MOVE.
///
/// Matched by the stored `visual_hash` (the bundle's INPUT hash as of the last synthesis) against
/// the [`ComponentBundle::input_hash`] of a bundle NO row claims. Since that hash is
/// location-independent ([`bundle_relative_path`]), a bundle moved wholesale keeps it, so the match
/// is exact and content-based — it holds for a bare catalogue name (`card`) exactly as for a
/// qualified one (`y/card`), which the old name-keyed adoption did not (it duplicated the qualified
/// one and left the original invisible).
///
/// DELIBERATELY CONSERVATIVE — each side is used at most once and an ambiguous hash matches nothing:
///   * only a row whose repo was scanned is a candidate (else its source is not "gone", just
///     unlooked-at);
///   * only a row that WAS synthesized carries a hash to match on. An unsynthesized row holds
///     nothing worth re-anchoring: prune + insert is indistinguishable from a move for it;
///   * a hash claimed by SEVERAL rows, or matching SEVERAL free bundles (two byte-identical
///     copies), re-anchors NOTHING — a coin flip between two identities is worse than an insert.
///
/// # A bundle is FREE when nothing that has something to LOSE points at it (review #58)
///
/// A move is very often done in TWO STEPS by an agent — write the new file, check it, delete the old
/// one — and it only takes ONE inventory in between (another session's MCP call, the `/design`
/// button) for the new source to already own a row of its own. That row is EMPTY: born of the
/// discovery, never synthesized. Treating it as a "claim" made the move unrecognizable, and the
/// row that DID carry the synthesis became an ordinary orphan.
///
/// So a bundle claimed ONLY by rows that carry no synthesis is still a re-anchoring target: the
/// vanished, synthesized row takes it over and the empty placeholder is dropped. Nothing is lost —
/// the placeholder holds nothing — and the identity, the template and every frozen composition
/// reference of the real component survive. A bundle claimed by a SYNTHESIZED row is never taken.
///
/// Returns the `(name, new entry path)` of every re-anchored row.
fn reconcile_moved_sources(
    conn: &mut SqliteConnection,
    roots: &[RepoRoot],
    existing: &[DesignComponent],
    discovered: &[DiscoveredBundle],
) -> QueryResult<Vec<(String, String)>> {
    let points_at = |component: &DesignComponent, bundle: &DiscoveredBundle| {
        component.source_path.as_deref() == Some(bundle.bundle.entry_path.as_str())
            && component.source_repository_id.as_deref() == bundle.repo_id.as_deref()
    };
    let claimed_by_a_synthesis = |bundle: &DiscoveredBundle| {
        existing
            .iter()
            .any(|c| points_at(c, bundle) && carries_synthesis(c))
    };
    let rediscovered = |component: &DesignComponent| {
        discovered.iter().any(|d| {
            component.source_path.as_deref() == Some(d.bundle.entry_path.as_str())
                && component.source_repository_id.as_deref() == d.repo_id.as_deref()
        })
    };

    // The FREE bundles (no row WITH A SYNTHESIS points at them) indexed by input hash; an ambiguous
    // hash is dropped.
    let mut free_by_hash: std::collections::BTreeMap<&str, Option<&DiscoveredBundle>> =
        std::collections::BTreeMap::new();
    for discovery in discovered.iter().filter(|d| !claimed_by_a_synthesis(d)) {
        free_by_hash
            .entry(discovery.bundle.input_hash.as_str())
            .and_modify(|slot| *slot = None)
            .or_insert(Some(discovery));
    }
    // The VANISHED rows, likewise indexed; an ambiguous hash is dropped on this side too.
    let mut vanished_by_hash: std::collections::BTreeMap<&str, Option<&DesignComponent>> =
        std::collections::BTreeMap::new();
    for component in existing.iter().filter(|c| {
        c.source_path.is_some()
            && !rediscovered(c)
            && repo_was_scanned(roots, c)
            && c.visual_hash.is_some()
    }) {
        vanished_by_hash
            .entry(component.visual_hash.as_deref().unwrap_or_default())
            .and_modify(|slot| *slot = None)
            .or_insert(Some(component));
    }

    let mut moved = Vec::new();
    for (hash, row) in &vanished_by_hash {
        let (Some(component), Some(Some(discovery))) = (row, free_by_hash.get(hash)) else {
            continue;
        };
        // The EMPTY placeholder a previous inventory may have created for the new source (the
        // two-step move) makes room for the row that actually carries the synthesis. It has no
        // template, no hash and no history: dropping it loses nothing, and keeping it would leave
        // two rows on one source — which the naming pass and the discovery upsert would then have
        // to arbitrate.
        //
        // CONDITIONAL like every other automatic delete: `existing` is a SNAPSHOT, and a
        // `design_refresh_component` / `proto_author_component` committed since it was taken makes
        // the "placeholder" a row with something to lose. The `DELETE` re-tests that itself, so a
        // row that grew content survives — it simply stays where it is, and the naming pass then
        // arbitrates the two rows as it does for any homonym.
        for placeholder in existing
            .iter()
            .filter(|c| c.id != component.id && points_at(c, discovery))
        {
            db::delete_component_if_unauthored(conn, &placeholder.id)?;
        }
        // `existing` is a SNAPSHOT here too: an EXPLICIT delete may have removed the row we are
        // about to re-anchor. `None` means exactly that — nothing to re-anchor, nothing lost — so we
        // SKIP the line instead of failing the whole inventory, the treatment `prune_orphans`
        // already gives `ConditionalDelete::AlreadyGone` (audit 01KYHYW25GKJBRTZVTSXQAMP9F). It is
        // not reported as a move, because no move happened.
        let rebound = db::bind_component_source(
            conn,
            &component.id,
            discovery.repo_id.as_deref(),
            &discovery.bundle.entry_path,
        )?;
        if rebound.is_none() {
            continue;
        }
        moved.push((component.name.clone(), discovery.bundle.entry_path.clone()));
    }
    Ok(moved)
}

/// Does this row carry AUTHORED CONTENT — anything an agent or a human WROTE, that no scan could
/// re-derive? Four fields, and only them:
///   * a non-empty `template` and/or a recorded `visual_hash` — the two halves of one act (storing
///     the LLM's Handlebars, then recording the input state it was synthesized from);
///   * a non-empty `params_json` and/or `states_json` — the param SHAPE and the declared STATES a
///     `[NEW]` can be authored with WITHOUT a template (`proto_author_component` makes the template
///     optional, review 01KYHV9F1JXCCKYMYYZR6Y1JYD). Once such a row is MATERIALIZED
///     ([`db::bind_component_source`] gives it a source without giving it a template), the old
///     template-only test called it empty and the élagage DESTROYED its params and its states — and
///     `classify_elected_nodes`, resolving by name on `source_path.is_some()`, then re-listed the
///     frozen node as `[NEW]`, sending the coder back to build a component already built.
///
/// THE line the automatic élagage never crosses (both of them: [`prune_orphans`] and the empty
/// placeholder [`reconcile_moved_sources`] drops). A row WITHOUT authored content holds nothing a
/// re-scan could not reproduce for free — a DISCOVERED row has all four fields empty by
/// construction, so a corrected glob still prunes exactly what it always did — while a row WITH some
/// holds work no scan can re-derive: it is FLAGGED orphan and kept, and only an explicit gesture
/// removes it.
///
/// It is evaluated on a SNAPSHOT, so it can never be the last word: the automatic delete re-tests it
/// IN SQL, inside the `DELETE` itself ([`db::delete_component_if_unauthored`], whose
/// `UNAUTHORED_COMPONENT_SQL` is this predicate negated).
fn carries_synthesis(component: &DesignComponent) -> bool {
    let authored = |field: &Option<String>| field.as_deref().is_some_and(|v| !v.trim().is_empty());
    !component.template.trim().is_empty()
        || component.visual_hash.is_some()
        || authored(&component.params_json)
        || authored(&component.states_json)
}

/// PRUNE the rows whose recorded source is GONE — the élagage a glob change (or a deletion) must
/// produce, since nothing else can ever remove them (the catalogue used to keep 28 phantom rows a
/// corrected glob no longer matched, with no way to drop them).
///
/// A row is pruned when its source was NOT rediscovered this run, under FOUR guards, each of which
/// SPARES the row and reports why:
///   * GREENFIELD (`source_path IS NULL`) — never even considered: a `[NEW]` has no file;
///   * [`KEPT_REPO_NOT_SCANNED`] — its provenance repo is not a member of the root workspace, or its
///     root is missing on disk. The run never looked: it cannot conclude;
///   * [`KEPT_SCAN_INCOMPLETE`] — its repo's walk met at least one I/O failure. FAIL-CLOSED: a scan
///     that could not read everything it looked at proves NOTHING about what it did not read, so it
///     prunes nothing at all in that repo;
///   * [`KEPT_EMPTY_SCAN`] — that repo discovered NOTHING while the catalogue holds rows for it. An
///     empty scan is a mis-configuration signal (a wrong glob override, a half-checked-out repo),
///     never an order to purge a whole front;
///   * [`KEPT_SYNTHESIZED`] — the row carries AUTHORED CONTENT ([`carries_synthesis`]). It is
///     FLAGGED orphan (persistently, in `design_components.orphaned_at`) and kept;
///   * [`KEPT_CONTENT_APPEARED`] — the four guards above judged the row on a SNAPSHOT and cleared
///     it for deletion, but the `DELETE` itself, re-testing the condition IN SQL, found content on
///     the row and removed NOTHING. A concurrent write won the race and is not destroyed for it.
///
/// # The guards decide, the `DELETE` disposes (review 01KYHV8DJ7XZG9F5TCRG2Q17Q1, CRITICAL)
///
/// `existing` is a snapshot the caller read BEFORE walking every repo; the deletes happen after,
/// row by row, outside any transaction of the caller's. Between the two, another session's
/// `design_refresh_component` — atomic for itself, and answering OK to its agent — can store an LLM
/// synthesis on a row this snapshot says is empty. Adding a guard ABOVE the delete cannot fix that:
/// every guard here reads the same stale snapshot, and even a fresh re-read would only shrink the
/// window. So the removal is [`db::delete_component_if_unauthored`]: the "carries nothing" test is
/// part of the `DELETE` statement, inside a transaction. A row that grew content is SPARED (zero
/// rows affected) instead of destroyed, and `pruned` counts only the rows this run REALLY removed.
///
/// # Why the automatic élagage is now this conservative (the CRITICAL data-loss review)
///
/// The élagage contributes NOTHING to closing the name-hijack hole — that is closed one level down,
/// by [`db::upsert_component_discovery`]'s step 2, which refuses to re-point a row that already
/// carries a source. So the prune can be as conservative as the risk demands without re-opening
/// anything. And the risk was total: the only thing standing between a transient `EIO` / `EMFILE` /
/// concurrent `git checkout` and the destruction of an LLM synthesis was a walk that swallowed its
/// own read errors. What the prune removes now is exactly what a re-scan can rebuild for free: rows
/// that were DISCOVERED and never SYNTHESIZED. Everything else is reported, flagged, and left to an
/// explicit human/agent gesture (`design_delete_component` / the `/design` card's delete).
///
/// Callers run this AFTER [`reconcile_moved_sources`], so a component that merely MOVED is
/// re-anchored before it can ever look orphaned.
fn prune_orphans(
    conn: &mut SqliteConnection,
    roots: &[RepoRoot],
    existing: &[DesignComponent],
    discovered: &[DiscoveredBundle],
    incomplete_repos: &std::collections::BTreeSet<Option<String>>,
) -> QueryResult<Vec<OrphanComponent>> {
    let mut orphans = Vec::new();
    for component in existing {
        let Some(source_path) = component.source_path.as_deref() else {
            continue; // a greenfield `[NEW]`: no file by construction, never an orphan
        };
        let rediscovered = discovered.iter().any(|d| {
            d.bundle.entry_path == source_path
                && d.repo_id.as_deref() == component.source_repository_id.as_deref()
        });
        if rediscovered {
            continue;
        }
        let repo_discovered_something = discovered
            .iter()
            .any(|d| d.repo_id.as_deref() == component.source_repository_id.as_deref());
        let kept_because = if !repo_was_scanned(roots, component) {
            Some(KEPT_REPO_NOT_SCANNED)
        } else if incomplete_repos.contains(&component.source_repository_id) {
            Some(KEPT_SCAN_INCOMPLETE)
        } else if !repo_discovered_something {
            Some(KEPT_EMPTY_SCAN)
        } else if carries_synthesis(component) {
            Some(KEPT_SYNTHESIZED)
        } else {
            None
        };
        // The DELETE is CONDITIONAL: the guards above judged a snapshot, the statement re-tests
        // "carries nothing" itself. Its outcome — and not the guards' verdict — is what `pruned`
        // and `kept_because` report, so the counts describe what HAPPENED, never what was intended.
        let (pruned, kept_because) = match kept_because {
            Some(reason) => (false, Some(reason)),
            None => match db::delete_component_if_unauthored(conn, &component.id)? {
                db::ConditionalDelete::Deleted => (true, None),
                db::ConditionalDelete::KeptContentAppeared => (false, Some(KEPT_CONTENT_APPEARED)),
                // Someone else's EXPLICIT delete removed it first: this run neither pruned it nor
                // spared it, and the row is no longer in the catalogue to be reported about.
                db::ConditionalDelete::AlreadyGone => continue,
            },
        };
        // The PERSISTENT orphan flag is RAISED only by a run that WAS in a position to judge, on a
        // row that has something to lose — the last guard, and the row the DELETE spared for the
        // very same reason. `repo_not_scanned` / `scan_incomplete` / `empty_scan` mean "this run
        // could not tell": they leave the flag EXACTLY as a competent run left it (review
        // 01KYHVAB5ZQN6K56SSSPV55EJY — clearing it made a real orphan indistinguishable from a live
        // component in `/design`, durably so for a repo that left the workspace), and simply report
        // whether the row still carries it.
        let judged = matches!(kept_because, Some(KEPT_SYNTHESIZED | KEPT_CONTENT_APPEARED));
        let flagged_orphan = judged || (!pruned && component.orphaned_at.is_some());
        if !pruned {
            // The last scan did NOT see this source: forget the hash it once observed (so the
            // non-scanning catalogue page enumerates exactly what the scan discovered) and settle
            // the orphan flag — raised when this run could judge, untouched when it could not.
            let verdict = if judged {
                db::OrphanVerdict::Orphaned
            } else {
                db::OrphanVerdict::Undecidable
            };
            db::set_component_scan_state(conn, &component.id, None, verdict)?;
        }
        orphans.push(OrphanComponent {
            id: component.id.clone(),
            name: component.name.clone(),
            source_path: source_path.to_string(),
            pruned,
            kept_because,
            flagged_orphan,
        });
    }
    Ok(orphans)
}

// =============================================================================
// The project's EFFECTIVE recipe — one precedence rule, read in one place
// =============================================================================

/// The effective recipe came from the AGENT (`design.bundle_recipe_json`, v36).
pub const RECIPE_SOURCE_CUSTOM: &str = "custom";

/// The effective recipe was DERIVED from the v33 per-project glob override
/// (`design.inventory_globs_json`) — the retro-compatible bridge.
pub const RECIPE_SOURCE_GLOBS_OVERRIDE: &str = "globs_override";

/// The effective recipe is the DEFAULT of the repo's DETECTED stack — nyx's former hard-coded
/// behaviour, and what every project gets until an agent writes one.
pub const RECIPE_SOURCE_STACK_DEFAULT: &str = "stack_default";

/// A project's persisted recipe CONFIG, read ONCE per inventory: the agent's recipe (v36) and the
/// legacy glob override (v33). Resolving a repo's effective recipe from it is pure
/// ([`ProjectRecipe::for_root`]) — the one place the precedence lives, so the scan, the per-component
/// bundle read and the agent-facing tool can never disagree about what the rules are.
#[derive(Debug, Clone, Default)]
pub struct ProjectRecipe {
    /// The recipe the AGENT wrote, when it wrote one.
    pub custom: Option<BundleRecipe>,
    /// The v33 glob override, when set and no custom recipe supersedes it.
    pub legacy_globs: Option<Vec<String>>,
}

impl ProjectRecipe {
    /// LOAD the project's recipe config.
    ///
    /// A stored recipe that does not PARSE is a hard error
    /// ([`diesel::result::Error::DeserializationError`]), never a silent fall-back to the default:
    /// scanning a repo under rules that are not the ones the project chose would discover a
    /// different population, and the élagage acts on that population. Fail-closed. (The write path
    /// validates, so this can only come from a hand-edited DB.)
    pub fn load(conn: &mut SqliteConnection, project_id: &str) -> QueryResult<Self> {
        let custom = match db::design_bundle_recipe_json(conn, project_id)? {
            Some(json) => Some(BundleRecipe::from_json(&json).map_err(|message| {
                diesel::result::Error::DeserializationError(
                    format!(
                        "the bundle recipe stored for project {project_id} is unusable, so the \
                         inventory REFUSES to scan under different rules than the ones you chose: \
                         {message}. Fix it with the design_recipe tool (or reset it)."
                    )
                    .into(),
                )
            })?),
            None => None,
        };
        let legacy_globs = if custom.is_some() {
            None
        } else {
            db::design_inventory_globs(conn, project_id)?
        };
        Ok(Self {
            custom,
            legacy_globs,
        })
    }

    /// The EFFECTIVE recipe for a repo rooted at `root`, and WHERE it came from
    /// ([`RECIPE_SOURCE_CUSTOM`] / [`RECIPE_SOURCE_GLOBS_OVERRIDE`] /
    /// [`RECIPE_SOURCE_STACK_DEFAULT`]). The stack is detected PER REPO, so a project whose
    /// workspace mixes an Angular front and a React one still scans each with its own default —
    /// while an agent-written recipe, being a statement about the PROJECT's convention, applies to
    /// all of them.
    pub fn for_root(&self, root: &Path) -> (BundleRecipe, &'static str) {
        if let Some(recipe) = &self.custom {
            return (recipe.clone(), RECIPE_SOURCE_CUSTOM);
        }
        if let Some(globs) = &self.legacy_globs {
            return (
                recipe_from_legacy_globs(globs),
                RECIPE_SOURCE_GLOBS_OVERRIDE,
            );
        }
        (
            default_recipe(detect_stack(root)),
            RECIPE_SOURCE_STACK_DEFAULT,
        )
    }
}

/// One repo's view of the project's recipe — what the agent-facing read echoes so a human/agent can
/// see WHY a repo is being scanned the way it is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoRecipeView {
    /// The repo's human label.
    pub repo: String,
    /// The stack DETECTED in it (`angular` / `react` / …) — what its default recipe would be.
    pub stack: &'static str,
    /// Where its effective recipe came from ([`RECIPE_SOURCE_CUSTOM`] / …).
    pub source: &'static str,
    /// The recipe THIS repo is actually scanned with ([`ProjectRecipe::for_root`]).
    ///
    /// The read used to echo ONE recipe — the first repo's — for a project that may hold several
    /// repos on several stacks (audit 01KYM2W91TKCTEAK20ZDZJ75Z4). An agent following the manual's
    /// step 0-bis ("take that default as your template and edit it") then wrote the FIRST repo's
    /// default over the whole project and silently took the other repos' components out of the
    /// discovery. Every repo now shows what it is really scanned with.
    pub recipe: BundleRecipe,
}

/// The project's recipe AS THE AGENT READS IT: the effective recipe, where it came from, the
/// per-repo detection behind it, and what is worth SAYING about the combination.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectRecipeView {
    /// Where the effective recipe came from.
    pub source: &'static str,
    /// The EFFECTIVE, PROJECT-WIDE recipe: the agent's own when it wrote one, else the one a WRITE
    /// would have to carry to keep discovering what the defaults discover today.
    ///
    /// For a project whose repos run DIFFERENT stacks and that has written no recipe, that is the
    /// UNION of the per-repo defaults ([`merge_recipes`]) — not the first repo's, which is what
    /// made "read it, write it back" lose a repo. A single-repo project (and a project whose repos
    /// share a stack) gets exactly what it always got: the union of identical recipes is that
    /// recipe.
    pub recipe: BundleRecipe,
    /// Every repo of the project with its detected stack and its effective recipe.
    pub repos: Vec<RepoRecipeView>,
    /// What the agent must KNOW before writing (mixed stacks under one project-wide recipe…).
    /// Empty for the ordinary single-stack project.
    pub warnings: Vec<String>,
}

/// The UNION of several recipes: every rule / qualifier / exclusion of each, in order, deduplicated.
///
/// Used for ONE thing (audit 01KYM2W91TKCTEAK20ZDZJ75Z4): a recipe is a statement about the PROJECT
/// and applies to every repo, so the recipe a mixed-stack project must be SHOWN is the one that
/// covers all of its repos — otherwise the read hands the agent a document whose write silently
/// disables a repo. It is only ever built from the per-repo DEFAULTS, never from an agent's own
/// recipe (which already applies to everything).
///
/// Entry-rule ORDER is preserved (first repo's rules first): the election rule is "first matching
/// rule wins", so a repo's own rules keep the precedence they had among themselves.
fn merge_recipes(recipes: &[BundleRecipe]) -> BundleRecipe {
    let mut merged = BundleRecipe {
        version: RECIPE_VERSION,
        entries: Vec::new(),
        siblings: Vec::new(),
        family_qualifiers: Vec::new(),
        non_component_qualifiers: Vec::new(),
        exclude: Vec::new(),
        ignored_dirs: Vec::new(),
        pins: Vec::new(),
    };
    let push_new = |into: &mut Vec<String>, from: &[String]| {
        for value in from {
            if !into.iter().any(|kept| kept == value) {
                into.push(value.clone());
            }
        }
    };
    for recipe in recipes {
        for rule in &recipe.entries {
            if !merged.entries.contains(rule) {
                merged.entries.push(rule.clone());
            }
        }
        for rule in &recipe.siblings {
            if !merged.siblings.contains(rule) {
                merged.siblings.push(rule.clone());
            }
        }
        push_new(&mut merged.family_qualifiers, &recipe.family_qualifiers);
        push_new(
            &mut merged.non_component_qualifiers,
            &recipe.non_component_qualifiers,
        );
        push_new(&mut merged.exclude, &recipe.exclude);
        push_new(&mut merged.ignored_dirs, &recipe.ignored_dirs);
        for pin in &recipe.pins {
            if !merged.pins.contains(pin) {
                merged.pins.push(pin.clone());
            }
        }
    }
    merged
}

/// READ the project's effective recipe + the detection behind it — the `design_recipe` tool's read
/// shape, kept here so the tool never re-implements the precedence.
#[cfg_attr(not(test), allow(dead_code))]
pub fn project_recipe_view(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<ProjectRecipeView> {
    let config = ProjectRecipe::load(conn, project_id)?;
    let roots = project_repo_roots(conn, project_id)?;
    let repos: Vec<RepoRecipeView> = roots
        .iter()
        .map(|repo| {
            let (recipe, source) = config.for_root(&repo.root);
            RepoRecipeView {
                repo: repo.label.clone(),
                stack: detect_stack(&repo.root).label(),
                source,
                recipe,
            }
        })
        .collect();
    let (recipe, source) = match roots.first() {
        // A recipe is PROJECT-WIDE. When it is still the per-repo DEFAULT, the document to show is
        // the one that covers every repo — the union — so that writing back what was read cannot
        // narrow the project to its first repo's stack.
        Some(repo) => match config.for_root(&repo.root) {
            (_first, RECIPE_SOURCE_STACK_DEFAULT) if repos.len() > 1 => (
                merge_recipes(
                    &repos
                        .iter()
                        .map(|view| view.recipe.clone())
                        .collect::<Vec<_>>(),
                ),
                RECIPE_SOURCE_STACK_DEFAULT,
            ),
            resolved => resolved,
        },
        // No repo on disk yet (greenfield / no workspace): show what a project of nyx's own stack
        // would get, so the agent still sees a concrete starting point to edit.
        None => config.for_root(Path::new("")),
    };
    let mut stacks: Vec<&str> = repos.iter().map(|view| view.stack).collect();
    stacks.sort_unstable();
    stacks.dedup();
    let mut warnings = Vec::new();
    // The walk's FLOOR, said out loud (audit 01KYM9MN6RF9KTA5M4ZW75HAXP): a recipe may write
    // whatever it likes in `ignored_dirs`, but the dependency tree and the VCS internals are pruned
    // regardless — so the echoed document must not let the agent believe otherwise.
    let unlisted = recipe.unlisted_mandatory_ignored_dirs();
    if !unlisted.is_empty() {
        warnings.push(format!(
            "`ignored_dirs` does not list {}, but the walk prunes {} anyway — the dependency tree \
             and the VCS internals are never the front nyx inventories, and descending into them \
             cost a real repo 60,2 s and 14 229 phantom components. Everything else in the list \
             stays yours to remove. To catalogue a specific vendored file, PIN it: pins name paths \
             and do not go through the walk.",
            unlisted
                .iter()
                .map(|dir| format!("`{dir}`"))
                .collect::<Vec<_>>()
                .join(" / "),
            if unlisted.len() == 1 { "it" } else { "them" }
        ));
    }
    if stacks.len() > 1 {
        warnings.push(format!(
            "this project's repos run DIFFERENT stacks ({}) and a recipe is PROJECT-WIDE: the one \
             stored here is applied to EVERY repo, and its `entries` globs are matched against \
             each file's path RELATIVE TO ITS OWN REPO. The `recipe` above already covers all of \
             them (it is the union of their defaults while none is stored), so writing it back is \
             safe — but if you EDIT it, keep a rule for every stack, or the repos you drop rules \
             for stop being discovered. Each repo's own effective recipe is in `repos[].recipe`.",
            stacks.join(" + ")
        ));
    }
    Ok(ProjectRecipeView {
        source,
        recipe,
        repos,
        warnings,
    })
}

/// WRITE (or CLEAR) the project's bundle recipe. `None` clears it, so the project falls back to its
/// v33 glob override when it has one and to the detected stack's default otherwise. The recipe is
/// VALIDATED before it is stored — an unusable recipe must never reach the column, because the
/// inventory refuses to scan under one it cannot parse.
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_project_recipe(
    conn: &mut SqliteConnection,
    project_id: &str,
    recipe: Option<&BundleRecipe>,
) -> QueryResult<()> {
    let json = match recipe {
        Some(recipe) => {
            recipe
                .validate()
                .map_err(|message| diesel::result::Error::DeserializationError(message.into()))?;
            Some(recipe.to_json())
        }
        None => None,
    };
    db::set_design_bundle_recipe(conn, project_id, json.as_deref())?;
    Ok(())
}

// =============================================================================
// THE EXPLICIT REMOVAL — and what it really achieves (finding 01KYSCX4PNKTMX17YVG9XD8KBZ)
// =============================================================================
//
// A catalogued component's IDENTITY IS ITS SOURCE: as long as the file matches the project's recipe,
// the discovery finds it — that is the whole point of the model, and what makes a bundle that MOVES
// keep its synthesis. The consequence had never been said out loud: deleting the ROW of a component
// whose file is still there and still matches deletes NOTHING durable. The next `design_inventory`
// re-discovers it, `pending`, with a fresh id. "Keep only these N components" therefore does not hold
// through a rescan — the recipe has to be restricted — and the gesture said nothing about it.
//
// The two halves of the answer, both of them here so the MCP tool and the `/design` card agree:
//   * [`removal_effect`] — computed BEFORE the delete, it says whether the removal is EPHEMERAL and
//     WHY (the verdict of the discovery on that very path), so the response can name the problem AND
//     the correction instead of answering a bare `deleted: true`;
//   * [`remove_component`] — the removal itself, optionally carrying a PERSISTENT exclusion. That
//     exclusion is not a new mechanism: it is the recipe's own `exclude` list, the one the discovery
//     has always honoured ([`recipe::BundleRecipe::is_excluded`], applied to entries AND siblings).
//     No new column, no second exclusion path, no migration — and the recipe the agent reads back
//     says, in its own document, why that file is out.
//
// NOTHING HERE WEAKENS THE ANTI-LOSS GUARDS. The delete is still [`db::delete_component`], the
// UNCONDITIONAL one, reached only by an EXPLICIT gesture; the automatic élagage still goes through
// [`prune_orphans`] + [`db::delete_component_if_unauthored`] and still spares anything carrying a
// synthesis. Writing an exclusion destroys nothing at all — it only narrows what a FUTURE scan
// discovers, which is exactly what a human asking "and don't bring it back" is asking for.

/// Why the discovery would — or would not — elect ONE path as a component ENTRY. The verdict behind
/// "will the next scan bring this row back?", answered by the SAME rules
/// [`scan_bundles_with`] applies (pins, the walk's pruned directories, the recipe's exclusions, the
/// entry rules, the content marker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryVerdict {
    /// The scan ELECTS it: a rule claims it and, when that rule demands a marker, the file carries
    /// it. A row deleted while its source is in this state COMES BACK.
    Elected,
    /// The recipe PINS it — discovered whatever the rules say, so it comes back too.
    Pinned,
    /// A DIRECTORY of its path is pruned by the walk (the offending name is carried).
    PrunedDir(String),
    /// The recipe's `exclude` globs match it.
    Excluded,
    /// No entry rule claims it (or its name never opens a bundle / has no derivable base name).
    NoRule,
    /// A rule claims it, but the CONTENT marker that rule demands is not in the file.
    NoMarker,
    /// Nothing is there to elect (the file is gone, or the path leaves the repo).
    Missing,
    /// It is there and the rules claim it, but nyx could NOT READ it to check the marker. The scan
    /// would record an error and elect nothing — fail-closed — so the honest answer is UNKNOWN, and
    /// the caller is warned as if the row could come back.
    Unreadable,
}

impl EntryVerdict {
    /// The machine-readable verdict echoed to the agent / UI.
    pub fn label(&self) -> &'static str {
        match self {
            EntryVerdict::Elected => "elected",
            EntryVerdict::Pinned => "pinned",
            EntryVerdict::PrunedDir(_) => "pruned_dir",
            EntryVerdict::Excluded => "excluded",
            EntryVerdict::NoRule => "no_rule",
            EntryVerdict::NoMarker => "no_marker",
            EntryVerdict::Missing => "missing",
            EntryVerdict::Unreadable => "unreadable",
        }
    }

    /// Would a scan (re-)DISCOVER this path? `Unreadable` counts as YES: nyx could not decide, and a
    /// warning that turns out to be unnecessary costs nothing, while a silence that turns out to be
    /// wrong is the whole finding.
    pub fn discovers(&self) -> bool {
        matches!(
            self,
            EntryVerdict::Elected | EntryVerdict::Pinned | EntryVerdict::Unreadable
        )
    }
}

/// Would the discovery elect `rel` (repo-relative, `/`-separated) as a component ENTRY in the repo at
/// `root`, under `recipe`? The per-path form of [`scan_bundles_with`]'s election, in the same order
/// and through the same predicates — the walk's pruned directories ([`walk_pruned_dirs`]), the
/// recipe's `exclude`, `opens_bundle` / `component_name` / `entry_rule_for`, then the
/// `requires_content` probe opened through [`open_confined`] like every other read.
pub(crate) fn entry_verdict(root: &Path, rel: &str, recipe: &BundleRecipe) -> EntryVerdict {
    // A PIN does not go through the walk at all, so it is decided first (as in the scan).
    if recipe.pin_for(rel).is_some() {
        return EntryVerdict::Pinned;
    }
    let pruned = walk_pruned_dirs(recipe);
    let mut segments: Vec<&str> = rel.split('/').collect();
    // The last segment is the FILE — only DIRECTORIES are pruned by name.
    segments.pop();
    if let Some(dir) = segments.into_iter().find(|seg| pruned.contains(seg)) {
        return EntryVerdict::PrunedDir(dir.to_string());
    }
    if recipe.is_excluded(rel) {
        return EntryVerdict::Excluded;
    }
    let file_name = file_name_of(rel);
    if !recipe.opens_bundle(file_name) || recipe.component_name(file_name).is_none() {
        return EntryVerdict::NoRule;
    }
    let Some((_index, rule)) = recipe.entry_rule_for(rel, file_name) else {
        return EntryVerdict::NoRule;
    };
    let mut file = match open_confined(root, rel) {
        Ok(file) => file,
        Err(ConfinedOpenError::OutsideRepo(_)) => return EntryVerdict::Missing,
        Err(ConfinedOpenError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return EntryVerdict::Missing
        }
        Err(ConfinedOpenError::Io(_)) => return EntryVerdict::Unreadable,
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return EntryVerdict::Missing,
        Err(_) => return EntryVerdict::Unreadable,
    }
    match rule.requires_content.as_deref() {
        None => EntryVerdict::Elected,
        Some(marker) => match declares_marker(&mut file, marker) {
            Ok(true) => EntryVerdict::Elected,
            Ok(false) => EntryVerdict::NoMarker,
            Err(_) => EntryVerdict::Unreadable,
        },
    }
}

/// What removing ONE catalogue row really achieves — computed BEFORE the row is touched.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RemovalEffect {
    /// Is the removal EPHEMERAL: will the next `design_inventory` DISCOVER this source again (as a
    /// `pending` component, under a new id)? `false` for a greenfield `[NEW]` and for a source the
    /// recipe no longer takes.
    pub ephemeral: bool,
    /// The discovery's verdict on the source path ([`EntryVerdict::label`]), or `"greenfield"` for a
    /// row that has no on-disk source at all.
    pub verdict: &'static str,
    /// The repo the source resolved under — `None` for a greenfield `[NEW]` (or a source whose repo
    /// left the workspace).
    pub repo: Option<String>,
    /// The row's repo-relative source path — `None` for a greenfield `[NEW]`.
    pub source_path: Option<String>,
    /// Where the recipe that judged the source came from ([`RECIPE_SOURCE_CUSTOM`] /
    /// [`RECIPE_SOURCE_GLOBS_OVERRIDE`] / [`RECIPE_SOURCE_STACK_DEFAULT`]).
    pub recipe_source: Option<&'static str>,
}

/// The EFFECT of deleting `component`'s row, WITHOUT deleting anything: does its source still match
/// the project's recipe, and under which repo / which recipe. PURE with respect to the DB (reads
/// only), so a caller can ask before it decides.
pub fn removal_effect(
    conn: &mut SqliteConnection,
    project_id: &str,
    component: &DesignComponent,
) -> QueryResult<RemovalEffect> {
    let Some(source_path) = component.source_path.clone() else {
        // No source: nothing can re-discover it. This is the greenfield `[NEW]` the review-#44
        // escape hatch was written for, and it is DEFINITIVE — the tool must not cry wolf on it.
        return Ok(RemovalEffect {
            ephemeral: false,
            verdict: "greenfield",
            repo: None,
            source_path: None,
            recipe_source: None,
        });
    };
    let project_recipe = ProjectRecipe::load(conn, project_id)?;
    let roots = project_repo_roots(conn, project_id)?;
    // The SAME repo resolution the bundle read uses ([`bundle_of_component`]): the provenance repo
    // the discovery recorded decides, and only a component whose repo is not (or no longer) a member
    // falls back to whichever repo exposes the path — two repos of one project routinely expose the
    // same relative path.
    let resolved = component
        .source_repository_id
        .as_deref()
        .and_then(|repo_id| roots.iter().find(|r| r.repo_id.as_deref() == Some(repo_id)))
        .or_else(|| {
            roots.iter().find(|repo| {
                let (recipe, _source) = project_recipe.for_root(&repo.root);
                bundle_for_entry_with(&repo.root, &source_path, &recipe).is_some()
            })
        });
    let Some(repo) = resolved else {
        // The source's repo is not in the workspace and no other repo exposes the path: nothing is
        // scanning it, so nothing can bring the row back.
        return Ok(RemovalEffect {
            ephemeral: false,
            verdict: EntryVerdict::Missing.label(),
            repo: None,
            source_path: Some(source_path),
            recipe_source: None,
        });
    };
    let (recipe, recipe_source) = project_recipe.for_root(&repo.root);
    let verdict = entry_verdict(&repo.root, &source_path, &recipe);
    Ok(RemovalEffect {
        ephemeral: verdict.discovers(),
        verdict: verdict.label(),
        repo: Some(repo.label.clone()),
        source_path: Some(source_path),
        recipe_source: Some(recipe_source),
    })
}

/// The outcome of the EXPLICIT removal — see [`remove_component`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ComponentRemoval {
    /// Did the `DELETE` really affect the row?
    pub deleted: bool,
    /// What the removal achieves ([`removal_effect`], computed before the delete).
    pub effect: RemovalEffect,
    /// The `exclude` patterns this removal ADDED to the project's recipe (empty unless the caller
    /// asked for the exclusion, or when they were already there).
    pub excluded: Vec<String>,
    /// The project had NO recipe of its own and its effective one was MATERIALIZED to carry the
    /// exclusion (its `source` therefore moves to [`RECIPE_SOURCE_CUSTOM`]).
    pub recipe_materialized: bool,
    /// NOTHING was done, and why: the caller asked for an exclusion the recipe cannot hold. The
    /// delete stood down with it — they were one gesture.
    pub refused: Option<String>,
    /// What the caller MUST be told: the removal is ephemeral and here is what to do instead / the
    /// exclusion that was written and what it now means. Empty = the removal is complete and final.
    pub warnings: Vec<String>,
}

/// REMOVE `component` from the project's catalogue — the single authority behind the
/// `design_delete_component` MCP tool and the `/design` card's delete action.
///
/// It does three things the bare `db::delete_component` could not:
///
///  1. it DECIDES, before deleting, whether the gesture is EPHEMERAL ([`removal_effect`]);
///  2. with `exclude_source`, it writes a PERSISTENT exclusion into the project's recipe — the
///     bundle's own paths appended to `exclude`, the list the discovery already honours — so the next
///     scan does not bring the row back. The recipe is MATERIALIZED when the project still ran on a
///     default (an exclusion is a statement about THIS project), and the write is refused, with the
///     delete, when the recipe cannot hold it (`refused`);
///  3. it says both things out loud in `warnings[]`, naming the problem AND the correction.
///
/// ATOMIC: the exclusion and the delete are the two halves of ONE act ("this file is not a component
/// of this project"), so they run in a single transaction — a recipe carrying an exclusion for a row
/// that is still catalogued, or a deleted row whose exclusion was lost, are both incoherent states.
///
/// The DELETE itself is unchanged: [`db::delete_component`], the unconditional one, which only an
/// explicit gesture ever reaches. The automatic élagage keeps its own, conditional path
/// ([`db::delete_component_if_unauthored`]) and its "never destroy a synthesis" guard.
pub fn remove_component(
    conn: &mut SqliteConnection,
    project_id: &str,
    component: &DesignComponent,
    exclude_source: bool,
) -> QueryResult<ComponentRemoval> {
    use diesel::connection::Connection;
    conn.transaction(|c| {
        let effect = removal_effect(c, project_id, component)?;
        let name = component.name.clone();
        let mut excluded: Vec<String> = Vec::new();
        let mut recipe_materialized = false;
        let mut warnings: Vec<String> = Vec::new();

        // An exclusion that is asked for where it cannot work (a greenfield row, a PINNED entry) is
        // REFUSED with the delete — writing a pattern that changes nothing, and answering as if the
        // removal were now durable, is the very confusion this finding is about.
        if exclude_source {
            let refusal = match (effect.source_path.as_deref(), effect.verdict) {
                (None, _) => Some(format!(
                    "'{name}' has NO on-disk source (it is a greenfield [NEW] component), so there \
                     is nothing for the scan to exclude: deleting it is already definitive. Re-run \
                     without exclude_source."
                )),
                (Some(_), "pinned") => Some(format!(
                    "'{name}' was NOT deleted: its entry is PINNED in this project's recipe, and a \
                     pin names its files verbatim and bypasses every rule — including `exclude`. \
                     Remove the pin first (design_recipe with the pins you keep), then delete."
                )),
                // An `exclude` entry is a GLOB, so a source path carrying `*` / `?` cannot be turned
                // into a literal one: the pattern would silently match its neighbours and take THEM
                // out of the catalogue too. Refused rather than over-excluded.
                (Some(path), _) if path.contains(['*', '?']) => Some(format!(
                    "'{name}' was NOT deleted: its source path '{path}' contains a glob character \
                     (`*` or `?`), and an `exclude` entry IS a glob — excluding it literally is \
                     impossible, and the pattern would also match neighbouring files. Restrict the \
                     recipe's `entries` globs instead (design_recipe), or rename the file."
                )),
                _ => None,
            };
            if let Some(refused) = refusal {
                return Ok(ComponentRemoval {
                    deleted: false,
                    effect,
                    excluded,
                    recipe_materialized,
                    refused: Some(refused),
                    warnings,
                });
            }
        }
        // A source the scan does not take ANYWAY needs no exclusion: the recipe is a document a human
        // reads, and padding it with patterns that change nothing is how it becomes unreadable.
        if exclude_source && !effect.ephemeral {
            warnings.push(format!(
                "no exclusion was written: the discovery does not take '{}' anyway ({}), so removing \
                 '{name}' is already definitive. The project's recipe is left exactly as it was.",
                effect.source_path.as_deref().unwrap_or("its source"),
                effect.verdict,
            ));
        }
        let exclude_source = exclude_source && effect.ephemeral;

        if exclude_source {
            let source_path = effect.source_path.clone().unwrap_or_default();
            let view = project_recipe_view(c, project_id)?;
            let mut recipe = view.recipe.clone();
            recipe_materialized = view.source != RECIPE_SOURCE_CUSTOM;
            for pattern in exclusion_patterns_for(c, project_id, component, &recipe)? {
                if !recipe.exclude.iter().any(|kept| kept == &pattern) {
                    recipe.exclude.push(pattern.clone());
                    excluded.push(pattern);
                }
            }
            if let Err(message) = recipe.validate() {
                // The recipe REFUSES the exclusion (its `exclude` budget is full, most likely). Say
                // so, name the real lever, and do not delete: a delete without the exclusion is
                // exactly the ephemeral gesture the caller asked to avoid.
                return Ok(ComponentRemoval {
                    deleted: false,
                    effect,
                    excluded: Vec::new(),
                    recipe_materialized: false,
                    refused: Some(format!(
                        "'{name}' was NOT deleted: the exclusion could not be added to this \
                         project's recipe — {message}. Excluding sources ONE BY ONE does not scale: \
                         RESTRICT the recipe instead (design_recipe — narrow the `entries` globs to \
                         the directories that really hold your design system, or add ONE `exclude` \
                         glob covering the whole family), then delete."
                    )),
                    warnings: Vec::new(),
                });
            }
            if !excluded.is_empty() || recipe_materialized {
                set_project_recipe(c, project_id, Some(&recipe))?;
            }
            warnings.push(format!(
                "'{source_path}' is now in this project's recipe `exclude` list, so the next \
                 design_inventory will NOT re-discover '{name}'. That is a PERSISTED statement about \
                 the project, not a one-off: read it back (design_recipe) and drop the pattern if you \
                 ever want the component again.{}",
                if recipe_materialized {
                    " This project had no recipe of its own, so the effective one (the default nyx \
                     derives from the detected stack) was MATERIALIZED as the project's own to hold \
                     the exclusion: from now on it is YOURS and no longer follows nyx's defaults."
                } else {
                    ""
                }
            ));
        }

        // Re-judge AFTER the exclusion: what the caller is told must describe the state that will
        // really be scanned, not the one that was there when the call started.
        let final_effect = removal_effect(c, project_id, component)?;
        if final_effect.ephemeral {
            warnings.push(ephemeral_removal_warning(&name, &final_effect));
        }
        // WHO STILL REFERENCES IT (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`). A component's template may
        // REFERENCE this one (`data-nyx-use`), and that reference is resolved at RENDER time: once
        // the row is gone, every host carrying it stops rendering — a readable error, but one that
        // would surface three tools later, in a preview, with nothing linking it back to this
        // gesture. So the hosts are named HERE, before the delete has any chance to surprise anyone.
        if let Some(sentence) = still_referenced_warning(c, project_id, component)? {
            warnings.push(sentence);
        }
        let deleted = db::delete_component(c, &component.id)?;
        Ok(ComponentRemoval {
            deleted,
            effect: final_effect,
            excluded,
            recipe_materialized,
            refused: None,
            warnings,
        })
    })
}

/// How many HOSTS a "still referenced" warning names before it counts the rest — the same discipline
/// every other list an agent reads applies.
const MAX_NAMED_REFERENCING_HOSTS: usize = 8;

/// The sentence a removal owes the catalogue when OTHER components' templates still REFERENCE the
/// one being deleted (`data-nyx-use`), or `None` when nobody does.
///
/// It names the hosts, because the correction is in THEM: the render refuses an unresolved
/// reference, so each of them must either point at another component or inline the markup it was
/// borrowing. Naming the problem without naming who has it is exactly the silence this module keeps
/// closing.
fn still_referenced_warning(
    conn: &mut SqliteConnection,
    project_id: &str,
    component: &DesignComponent,
) -> QueryResult<Option<String>> {
    let hosts: Vec<String> = db::list_components(conn, project_id)?
        .into_iter()
        .filter(|row| row.id != component.id)
        .filter(|row| {
            crate::render::reference_sites(&row.template)
                .iter()
                .any(|site| site.component == component.name)
        })
        .map(|row| row.name)
        .collect();
    if hosts.is_empty() {
        return Ok(None);
    }
    let named = hosts
        .iter()
        .take(MAX_NAMED_REFERENCING_HOSTS)
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let rest = match hosts.len().saturating_sub(MAX_NAMED_REFERENCING_HOSTS) {
        0 => String::new(),
        more => format!(" (+{more} more)"),
    };
    Ok(Some(format!(
        "{} component(s) still REFERENCE '{}' in their template (data-nyx-use=\"{}\"): {named}{rest} \
         — a reference is resolved at RENDER time, so as of this delete each of them FAILS to render \
         with an explicit \"references a component the catalogue does NOT have\". Re-synthesize them \
         (design_refresh_component) pointing at another component, or inlining the markup they were \
         borrowing.",
        hosts.len(),
        component.name,
        component.name,
    )))
}

/// The `exclude` patterns that take `component`'s BUNDLE out of the discovery: every file of the
/// bundle that could itself OPEN a bundle under `recipe`, plus the recorded source path.
///
/// Why not the entry alone: an Angular bundle is `foo.component.ts` + `foo.component.html`, and
/// `**/*.component.html` is an ENTRY rule of the default recipe. Excluding only the class would leave
/// the template to open a bundle of the SAME name at the next scan — the delete would still have been
/// ephemeral, and this time silently, since the response would have claimed the exclusion was
/// written. So the exclusion covers every path of the bundle the recipe could elect.
fn exclusion_patterns_for(
    conn: &mut SqliteConnection,
    project_id: &str,
    component: &DesignComponent,
    recipe: &BundleRecipe,
) -> QueryResult<Vec<String>> {
    let mut patterns: Vec<String> = Vec::new();
    if let Some(source_path) = component.source_path.as_deref() {
        patterns.push(source_path.to_string());
    }
    if let Some((root, bundle)) = bundle_of_component(conn, project_id, component)? {
        for file in &bundle.files {
            if patterns.iter().any(|kept| kept == &file.path) {
                continue;
            }
            if matches!(
                entry_verdict(&root, &file.path, recipe),
                EntryVerdict::Elected | EntryVerdict::Pinned
            ) {
                patterns.push(file.path.clone());
            }
        }
    }
    patterns.sort();
    patterns.dedup();
    Ok(patterns)
}

/// THE sentence a removal that will not last must carry — the problem AND the correction, in the
/// pedagogical form the rest of the design tools use (finding 01KYSCX4PNKTMX17YVG9XD8KBZ: the gesture
/// used to answer a bare `deleted: true` for something a rescan undid).
fn ephemeral_removal_warning(name: &str, effect: &RemovalEffect) -> String {
    let path = effect.source_path.as_deref().unwrap_or("its source");
    let repo = effect
        .repo
        .as_deref()
        .map(|repo| format!(" of repo '{repo}'"))
        .unwrap_or_default();
    let unreadable = if effect.verdict == "unreadable" {
        " (nyx could not read the file to be sure, and a scan that cannot read decides nothing — \
         assume it comes back)"
    } else {
        ""
    };
    let pinned = if effect.verdict == "pinned" {
        " Its entry is PINNED in the recipe, so no exclusion can hide it: remove the pin \
         (design_recipe with the pins you keep) — that is the statement that put it there."
    } else {
        ""
    };
    format!(
        "THIS REMOVAL IS EPHEMERAL: '{name}' is out of the catalogue, but its source \
         '{path}'{repo} STILL matches this project's recipe{unreadable}, and a component's identity \
         IS its source — so the next design_inventory DISCOVERS IT AGAIN, as a pending component \
         with a new id. Deleting the row does not stop the scan from finding the file, so \"keep only \
         these N components\" cannot be obtained this way. To make it last, do ONE of these: (a) \
         RESTRICT the recipe so the file no longer matches — design_recipe, narrow the `entries` \
         globs to the directories that really hold your design system (the durable fix when a whole \
         family does not belong); or (b) re-run this delete with exclude_source: true, which appends \
         '{path}' to the recipe's `exclude` list — the same exclusion the discovery already \
         honours.{pinned}"
    )
}

/// INVENTORY the whole PROJECT: DISCOVER every component BUNDLE in the project's repos, RECONCILE
/// the catalogue with what is really there (moves re-anchored, orphans pruned), UPSERT each
/// discovery into `design_components` (name / provenance / entry path — never a template), and
/// compute the TARGETED work list of bundles awaiting (re-)synthesis. The runtime trigger's single
/// authority, shared by the `design_inventory` MCP tool and the `/design` UI action.
///
/// Each repo is scanned with the globs of its DETECTED stack ([`detect_stack`]) — or the project's
/// persisted OVERRIDE ([`db::design_inventory_globs`]) when set. NOTHING here writes a hash: the
/// stored hash is the input state of the LAST SYNTHESIS ([`mark_synthesized_at`]), so a run that
/// merely observes a change can never make the change look "already handled".
///
/// The pass ORDER is the life-cycle rule (see the section above): MOVE before PRUNE before NAME, so
/// a component that merely moved is never pruned, and a name a prune frees is available to a
/// newcomer in the SAME run.
///
/// GREENFIELD-safe end to end: a project with no workspace, no repos, or an empty/missing front
/// inventories NOTHING and returns an empty result WITHOUT error — and never prunes a thing.
#[cfg_attr(not(test), allow(dead_code))]
pub fn inventory_project(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> QueryResult<ProjectInventory> {
    // The project's effective RECIPE config (v36 recipe > v33 glob override > detected-stack
    // default), read ONCE. Its rules are what "a component" means in this project — the framework
    // knowledge nyx no longer holds.
    let project_recipe = ProjectRecipe::load(conn, project_id)?;
    let roots = project_repo_roots(conn, project_id)?;

    // Pass 1 — DISCOVER every repo's bundles. NOTHING is written yet: the catalogue NAMES depend on
    // the whole project-wide set (two repos can hold homonymous bundles too). Each repo's scan also
    // hands back what it FAILED to read: a repo with a single failure is marked INCOMPLETE, and
    // nothing of it will be pruned this run.
    let mut discovered: Vec<DiscoveredBundle> = Vec::new();
    let mut scan_errors = ScanErrors::default();
    let mut incomplete_repos: std::collections::BTreeSet<Option<String>> =
        std::collections::BTreeSet::new();
    for repo in &roots {
        let (recipe, _source) = project_recipe.for_root(&repo.root);
        let scan = scan_bundles_with(&repo.root, &recipe);
        if !scan.errors.is_clean() {
            incomplete_repos.insert(repo.repo_id.clone());
            scan_errors.absorb(scan.errors, &repo.label);
        }
        for bundle in scan.bundles {
            discovered.push(DiscoveredBundle {
                repo_id: repo.repo_id.clone(),
                repo_label: repo.label.clone(),
                bundle,
            });
        }
    }

    // The catalogue identity BEFORE the run — the reference `catalogue_changed` compares against, so
    // a no-op inventory reports no change (and its callers emit no notification).
    let before = db::list_components(conn, project_id)?;
    // The ORPHAN FLAG is part of the identity: raising or clearing it is a catalogue MOVE the UI
    // must re-pull for. The observed hash is NOT — a source EDIT moves the pending work list, not
    // the catalogue's identity, and re-notifying on every keystroke-sized change would be noise.
    #[allow(clippy::type_complexity)]
    let identity =
        |rows: &[DesignComponent]| -> Vec<(String, String, Option<String>, Option<String>, bool)> {
            rows.iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        c.name.clone(),
                        c.source_repository_id.clone(),
                        c.source_path.clone(),
                        c.orphaned_at.is_some(),
                    )
                })
                .collect()
        };
    let identity_before = identity(&before);

    // Pass 2 — MOVES: re-anchor by CONTENT the rows whose source vanished (before anything can
    // mistake them for orphans).
    let moved = reconcile_moved_sources(conn, &roots, &before, &discovered)?;

    // Pass 3 — ORPHANS: the rows whose source is gone are pruned (under guards) and reported.
    let after_moves = db::list_components(conn, project_id)?;
    let orphans = prune_orphans(conn, &roots, &after_moves, &discovered, &incomplete_repos)?;

    // Pass 4 — NAME each bundle unambiguously, then UPSERT its discovery keyed on its SOURCE
    // (idempotent; template/params/hash untouched). Two homonymous bundles are two rows.
    let existing = db::list_components(conn, project_id)?;
    let assigned = assign_catalogue_names(&existing, &discovered);
    let mut materialized: Vec<(String, String)> = Vec::new();
    for (discovery, name) in discovered.iter_mut().zip(assigned) {
        // A greenfield `[NEW]` holding this name is about to be MATERIALIZED in place by this
        // bundle (`upsert_component_discovery` step 2, review #41). That is the intent — a `[NEW]`
        // the front now implements must not be duplicated — but it is a state change the agent and
        // the human have to SEE: it is what takes a node out of `new_components`, i.e. what stops
        // the coder from being asked to build the component.
        if existing
            .iter()
            .any(|c| c.name == name && c.source_path.is_none())
        {
            materialized.push((name.clone(), discovery.bundle.entry_path.clone()));
        }
        let row = db::upsert_component_discovery(
            conn,
            project_id,
            &name,
            discovery.repo_id.as_deref(),
            &discovery.bundle.entry_path,
        )?;
        // The bundle is EXPOSED under its catalogue name — that is the key the agent passes back to
        // `design_inventory(component=…)` / `design_refresh_component`.
        discovery.bundle.name = name;
        // `None` = the row the discovery targeted was removed by an EXPLICIT delete between the
        // read and the write. There is nothing left to stamp: SKIP this bundle, exactly as
        // `prune_orphans` skips a row on `ConditionalDelete::AlreadyGone`, rather than aborting an
        // inventory whose earlier passes already committed (audit 01KYHYW25GKJBRTZVTSXQAMP9F). The
        // next run re-discovers the bundle and inserts it afresh; nothing is lost either way.
        let Some(row) = row else {
            continue;
        };
        // RECORD what this scan SAW of the bundle: its input hash, and that it is not an orphan.
        // This is what lets a later, NON-SCANNING catalogue page enumerate exactly this population
        // with exactly this pending predicate — the paging used to serve a different set.
        // REDISCOVERED: this run WAS in a position to judge and found the source — the one event
        // that legitimately LOWERS the orphan flag.
        db::set_component_scan_state(
            conn,
            &row.id,
            Some(discovery.bundle.input_hash.as_str()),
            db::OrphanVerdict::Alive,
        )?;
    }
    discovered.sort_by(|a, b| {
        a.bundle
            .name
            .cmp(&b.bundle.name)
            .then_with(|| a.bundle.entry_path.cmp(&b.bundle.entry_path))
    });

    // Pass 5 — the TARGETED work list: a bundle is PENDING when it was never synthesized (no
    // stored hash, or an empty template) or when its INPUTS moved since the last synthesis. The
    // stored row is matched by SOURCE, never by name, so two homonyms each track their OWN hash
    // (the name-keyed lookup left the loser 'source_changed' at every run, forever).
    let components = db::list_components(conn, project_id)?;
    let mut pending = Vec::new();
    for discovery in &discovered {
        let bundle = &discovery.bundle;
        let stored = components.iter().find(|c| {
            c.source_path.as_deref() == Some(bundle.entry_path.as_str())
                && c.source_repository_id.as_deref() == discovery.repo_id.as_deref()
        });
        let reason = match stored {
            Some(c) if c.template.trim().is_empty() || c.visual_hash.is_none() => {
                "never_synthesized"
            }
            Some(c) if c.visual_hash.as_deref() != Some(bundle.input_hash.as_str()) => {
                "source_changed"
            }
            Some(_) => continue,
            None => "never_synthesized",
        };
        pending.push(PendingSynthesis {
            name: bundle.name.clone(),
            entry_path: bundle.entry_path.clone(),
            input_hash: bundle.input_hash.clone(),
            reason,
        });
    }

    let synthesized_count = components
        .iter()
        .filter(|c| !c.template.trim().is_empty())
        .count();
    let catalogue_changed = identity(&components) != identity_before;
    let bundles: Vec<ComponentBundle> = discovered.into_iter().map(|d| d.bundle).collect();
    Ok(ProjectInventory {
        discovered: bundles.len(),
        repos_scanned: roots.len(),
        component_count: components.len(),
        synthesized_count,
        pending,
        orphans,
        moved,
        materialized,
        scan_errors,
        catalogue_changed,
        bundles,
    })
}

/// RESOLVE a catalogued component's bundle, returning the repo ROOT it resolved under alongside it.
/// `None` when the component has no source (a greenfield `[NEW]`) or its source vanished.
///
/// The provenance repo the discovery RECORDED (`source_repository_id`) decides — never "the first
/// repo where that relative path happens to exist". Two repos of one project routinely expose the
/// same relative path (`src/app/list.ts`), and scanning them in listing order hashed/read the WRONG
/// file for one of the two components. A component whose recorded repo is not (or is no longer) a
/// member of the root workspace falls back to the first repo where its `source_path` exists — the
/// loose-scan / current-folder-fallback case, where no repo id was ever persisted.
fn bundle_of_component(
    conn: &mut SqliteConnection,
    project_id: &str,
    component: &DesignComponent,
) -> QueryResult<Option<(PathBuf, ComponentBundle)>> {
    let Some(source_path) = component.source_path.as_deref() else {
        return Ok(None);
    };
    let roots = project_repo_roots(conn, project_id)?;
    // The SAME recipe the scan uses — a bundle must be assembled identically whoever asks, or the
    // hash the agent is handed on a READ would not be the hash the SCAN computed, and the
    // incremental loop would re-propose a component that never changed.
    let project_recipe = ProjectRecipe::load(conn, project_id)?;
    // 1. The repo the PROVENANCE names — authoritative when it is still a member.
    if let Some(repo_id) = component.source_repository_id.as_deref() {
        if let Some(repo) = roots.iter().find(|r| r.repo_id.as_deref() == Some(repo_id)) {
            let (recipe, _source) = project_recipe.for_root(&repo.root);
            return Ok(bundle_for_entry_with(&repo.root, source_path, &recipe)
                .map(|bundle| (repo.root.clone(), bundle)));
        }
    }
    // 2. No recorded repo (loose scan / fallback member), or it left the workspace.
    for repo in roots {
        let (recipe, _source) = project_recipe.for_root(&repo.root);
        if let Some(bundle) = bundle_for_entry_with(&repo.root, source_path, &recipe) {
            return Ok(Some((repo.root, bundle)));
        }
    }
    Ok(None)
}

/// One bundle FILE with its CONTENT — the matter the synthesizing agent reads. `content` is
/// truncated at `max_bytes` (a real template stays far below it; a pathological file cannot blow
/// the MCP response), `truncated` says so, and `bytes` always reports the file's REAL size.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BundleFileContent {
    pub path: String,
    pub role: BundleRole,
    pub bytes: usize,
    pub truncated: bool,
    pub content: String,
    /// The I/O failure that prevented reading this file (`None` = read fine). AN EMPTY FILE AND AN
    /// UNREADABLE ONE ARE NOT THE SAME THING (review 01KYHV1C2M1QNQ6A4Z1D00SC3B): the read used to
    /// `unwrap_or_default()`, handing an `EIO` / `EMFILE` / permission failure to the agent as
    /// `bytes: 0, truncated: false, content: ""` — so the agent synthesized on partial matter
    /// without knowing, and `design_refresh_component` then marked that synthesis up to date (the
    /// input hash folds the same "nothing" for the same file, so the two hashes agreed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_error: Option<String>,
}

/// A bundle READ on demand: its structure + the CONTENT of each file, alongside the catalogue row
/// it maps to. The agent's synthesis input (review #40: nyx exposes the matter, the agent writes
/// the Handlebars).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BundleContent {
    pub component: DesignComponent,
    /// The resolved bundle, or `None` for a component with no on-disk source (a `[NEW]`).
    pub bundle: Option<ComponentBundle>,
    pub files: Vec<BundleFileContent>,
    /// Is the stored synthesis UP TO DATE with the bundle's current inputs?
    pub up_to_date: bool,
    /// How many of `files` could NOT be read ([`BundleFileContent::read_error`]). The `mode:
    /// "bundle"` twin of the listing's `scan_error_count`: non-zero ⇒ the matter handed to the agent
    /// is PARTIAL and it must not synthesize from it as if it were whole.
    pub read_error_count: usize,
}

/// READ one component's BUNDLE with its file CONTENTS — the per-component, on-demand matter the
/// agent synthesizes from (never the whole catalogue inlined). `None` when `name` is not in the
/// project's catalogue. A component with no on-disk source (a greenfield `[NEW]`) resolves to a
/// `bundle: None` with no files — a valid state, not an error.
#[cfg_attr(not(test), allow(dead_code))]
pub fn read_bundle(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    max_bytes: usize,
) -> QueryResult<Option<BundleContent>> {
    let Some(component) = db::get_component_by_name(conn, project_id, name)? else {
        return Ok(None);
    };
    let resolved = bundle_of_component(conn, project_id, &component)?;
    let mut files = Vec::new();
    // Read from the root the bundle RESOLVED under (its provenance repo) — not from whichever repo
    // happens to expose the same relative path first.
    if let Some((root, bundle)) = &resolved {
        for file in &bundle.files {
            // "I could not read it" is NEVER folded into "there is nothing in it": the failure is
            // CARRIED to the agent (`read_error`) and COUNTED (`read_error_count`), so a partial
            // bundle is visibly partial rather than silently empty.
            //
            // CONFINED: this is the call that hands a file's CONTENT to the agent, so the bytes are
            // read FROM THE DESCRIPTOR [`open_confined`] verified — every segment opened with
            // `O_NOFOLLOW`, nothing re-resolved afterwards. A path that leaves the repo yields a
            // `read_error`, never a body; and since the check IS the open, that holds under
            // concurrency too — a writer swapping a segment for a symlink mid-read makes the open
            // FAIL (audit 01KYM9KVQ0M1PA3AMA7MYCSE93, which measured ~7 % of reads returning
            // out-of-repo bytes through the previous check-then-open form).
            let (head, real_len, read_error) =
                match open_confined(root, &file.path).and_then(|mut opened| {
                    read_bounded(&mut opened, max_bytes).map_err(ConfinedOpenError::Io)
                }) {
                    Ok((head, real_len)) => (head, real_len, None),
                    Err(err) => (Vec::new(), 0, Some(err.message())),
                };
            let bytes = real_len as usize;
            files.push(BundleFileContent {
                path: file.path.clone(),
                role: file.role,
                bytes,
                truncated: head.len() < bytes,
                content: String::from_utf8_lossy(&head).into_owned(),
                read_error,
            });
        }
    }
    let bundle = resolved.map(|(_root, bundle)| bundle);
    let up_to_date = match &bundle {
        Some(b) => {
            component.visual_hash.as_deref() == Some(b.input_hash.as_str())
                && !component.template.trim().is_empty()
        }
        None => !component.template.trim().is_empty(),
    };
    let read_error_count = files.iter().filter(|f| f.read_error.is_some()).count();
    Ok(Some(BundleContent {
        component,
        bundle,
        files,
        up_to_date,
        read_error_count,
    }))
}

/// The OUTCOME of recording a synthesis ([`mark_synthesized_at`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SynthesisRecord {
    /// The updated catalogue row when the hash WAS recorded; `None` when nothing was recorded.
    pub component: Option<DesignComponent>,
    /// The bundle's CURRENT input hash on disk — `None` for an unknown component or a greenfield
    /// `[NEW]` with no on-disk bundle.
    pub current_input_hash: Option<String>,
    /// The bundle's sources MOVED between the agent's READ and this WRITE: nothing was recorded, so
    /// the bundle stays on the `pending` work list and the change is re-proposed.
    pub source_moved: bool,
}

/// RECORD that a component was just SYNTHESIZED from its bundle: recompute the bundle's INPUT hash
/// and persist it ([`db::update_component_visual_hash`]), so the next inventory sees the component
/// as up to date and never re-synthesizes it for nothing.
///
/// `expected_input_hash` is the hash the agent RECEIVED when it read the bundle
/// (`design_inventory(component=…)`). When supplied, the hash is recorded ONLY if the bundle's
/// inputs are STILL at that state; otherwise nothing is written and
/// [`SynthesisRecord::source_moved`] says so — a source edited DURING the synthesis (a human, a
/// `git pull`, another agent) would otherwise be silently recorded as "already handled" and NEVER
/// re-proposed, quietly breaking "a re-synthesis happens only when the bundle's source changed".
/// Passing `None` keeps the historical behaviour (hash whatever is on disk at write time) — the
/// retro-compatible path for a caller that has no hash to check against.
///
/// This is the ONLY writer of the hash: hashing happens at SYNTHESIS time, over the INPUTS, so a
/// non-deterministic LLM output never enters the incremental key.
#[cfg_attr(not(test), allow(dead_code))]
pub fn mark_synthesized_at(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    expected_input_hash: Option<&str>,
) -> QueryResult<SynthesisRecord> {
    let Some(component) = db::get_component_by_name(conn, project_id, name)? else {
        return Ok(SynthesisRecord::default());
    };
    let Some((_root, bundle)) = bundle_of_component(conn, project_id, &component)? else {
        return Ok(SynthesisRecord::default());
    };
    if matches!(expected_input_hash, Some(expected) if expected != bundle.input_hash) {
        return Ok(SynthesisRecord {
            component: None,
            current_input_hash: Some(bundle.input_hash),
            source_moved: true,
        });
    }
    let updated = db::update_component_visual_hash(conn, &component.id, &bundle.input_hash)?;
    Ok(SynthesisRecord {
        component: updated,
        current_input_hash: Some(bundle.input_hash),
        source_moved: false,
    })
}

/// [`mark_synthesized_at`] with NO expected hash: record whatever the bundle's inputs are right
/// now. Returns the updated row, or `None` when the component is unknown / has no resolvable
/// bundle (a greenfield `[NEW]` keeps a NULL hash — there is no input to hash).
#[cfg_attr(not(test), allow(dead_code))]
pub fn mark_synthesized(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
) -> QueryResult<Option<DesignComponent>> {
    Ok(mark_synthesized_at(conn, project_id, name, None)?.component)
}

/// The MD5 (RFC 1321) hex digest of `data`, self-contained (no `md5` crate dependency — the
/// project deliberately avoids extra hash deps, cf. `db::hash_content` which reuses gix's sha1).
/// This honours PDR-0010 literally ("visual_hash = md5 de la partie visuelle") without a new
/// dependency. Correctness is pinned by known-answer vectors in the tests. MD5 is used purely as
/// a fast CHANGE DETECTOR (not for security), which is exactly its role here.
///
/// A one-shot convenience over [`Md5`]; the bundle hash STREAMS instead (see [`Md5::update`]).
pub fn md5_hex(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finish()
}

/// Per-round left-rotate amounts (RFC 1321).
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// `K[i] = floor(2^32 * abs(sin(i + 1)))` (RFC 1321).
const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// An INCREMENTAL MD5 (RFC 1321) — the primitive the bundle hash streams through.
///
/// The one-shot [`md5_hex`] used to COPY the whole message before padding it, so hashing a file
/// meant holding it (twice) in memory; the discovery capped the read at 32 KiB per file to bound
/// that, which made any edit PAST the cap invisible unless it also resized the file (review — a
/// constant-size edit beyond the bound silently broke "re-synthesize only on change"). Streaming
/// bounds the MEMORY (one 64-byte block + the caller's read buffer) instead of bounding the
/// CONTENT, so every byte of every bundle file is folded in at a constant, tiny footprint. The
/// 32 KiB cap survives only where it belongs: the EXPOSED read
/// ([`read_bundle`] / `mcp_tools_design::MAX_BUNDLE_FILE_BYTES`), which bounds a RESPONSE.
pub struct Md5 {
    /// The running `(a0, b0, c0, d0)` state.
    state: [u32; 4],
    /// The partial 64-byte block awaiting a full compression round.
    block: [u8; 64],
    /// How many bytes of `block` are filled.
    filled: usize,
    /// The TOTAL message length in bytes (padded in at [`Md5::finish`]).
    total: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    /// A fresh hasher seeded with the RFC 1321 initial state.
    pub fn new() -> Self {
        Self {
            state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
            block: [0u8; 64],
            filled: 0,
            total: 0,
        }
    }

    /// FOLD `data` into the digest. Any number of calls of any size produce the same digest as one
    /// call with the concatenation (pinned by a test against [`md5_hex`]).
    pub fn update(&mut self, data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        let mut rest = data;
        // Top up the partial block first, then compress whole blocks straight out of `rest`.
        if self.filled > 0 {
            let take = rest.len().min(64 - self.filled);
            self.block[self.filled..self.filled + take].copy_from_slice(&rest[..take]);
            self.filled += take;
            rest = &rest[take..];
            if self.filled < 64 {
                return; // `rest` is necessarily empty — keep the partial block as it stands
            }
            let block = self.block;
            self.compress(&block);
            self.filled = 0;
        }
        let mut chunks = rest.chunks_exact(64);
        for chunk in chunks.by_ref() {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }
        let tail = chunks.remainder();
        self.block[..tail.len()].copy_from_slice(tail);
        self.filled = tail.len();
    }

    /// PAD (0x80, zeros, then the 64-bit little-endian ORIGINAL bit length) and return the hex
    /// digest.
    pub fn finish(mut self) -> String {
        let bit_len = self.total.wrapping_mul(8);
        self.update_no_count(&[0x80]);
        while self.filled != 56 {
            self.update_no_count(&[0x00]);
        }
        self.update_no_count(&bit_len.to_le_bytes());
        let mut out = String::with_capacity(32);
        for word in self.state {
            for byte in word.to_le_bytes() {
                out.push_str(&format!("{byte:02x}"));
            }
        }
        out
    }

    /// [`Md5::update`] for the PADDING — the same buffering, without counting the bytes into the
    /// message length (the length is already frozen when padding starts).
    fn update_no_count(&mut self, data: &[u8]) {
        for byte in data {
            self.block[self.filled] = *byte;
            self.filled += 1;
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    /// The RFC 1321 compression function over ONE 64-byte block.
    fn compress(&mut self, chunk: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let o = i * 4;
            *word = u32::from_le_bytes([chunk[o], chunk[o + 1], chunk[o + 2], chunk[o + 3]]);
        }
        let (mut a, mut b, mut c, mut d) =
            (self.state[0], self.state[1], self.state[2], self.state[3]);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(MD5_K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(MD5_S[i]));
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

#[cfg(test)]
mod tests;
