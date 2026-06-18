//! `package.json` import backend (PRD-3): discover scripts under a workspace and
//! turn a selection into a managed-command template, WITHOUT losing provenance.
//!
//! Two halves:
//!
//! 1. **Discovery** ([`discover_package_scripts`]): walk the workspace tree, find
//!    every `package.json` (root + sub-packages of a monorepo), skipping the heavy
//!    build/vendor directories (`node_modules`, `.git`, `dist`, …). Each retained
//!    file is CANONICALIZED and re-checked to be inside the workspace (a symlinked
//!    `package.json` pointing outside is refused). For each script we propose an
//!    EDITABLE name and a default RUNNER command (`pnpm dev`, `bun run dev`, …) —
//!    never the raw script body — picking the package manager from the
//!    `packageManager` field, else the nearest lockfile, else npm.
//!
//! 2. **Creation** ([`import_command`]): persist ONE selected (name, command,
//!    subfolder, source-metadata) row as a template via [`crate::db::create_template`],
//!    storing the four source fields (`source_package_json_path`,
//!    `source_script_name`, `source_script_command_snapshot`, `package_manager`).
//!    A name already taken in the project BLOCKS the import with a clear error (the
//!    UI keeps the proposed name editable until it is unique).
//!
//! Non-goals (deferred): no checkbox UI, no `package.json` file-watching. This is
//! the pure backend the import commands and the import UI (Phase 4) call.
//!
//! Consumer: the `command_import_scripts` / `command_import_create`
//! `#[tauri::command]`s (PRD-3 Phase 3) call [`discover_package_scripts`] /
//! [`import_command`]; the source-detach check in `command_update` reuses
//! [`PackageManager::run_script`] to recognize the canonical runner call.

use std::collections::HashMap;
use std::path::Path;

use diesel::sqlite::SqliteConnection;
use serde::Serialize;

use crate::db::{self, CommandSource, ManagedCommand};
use crate::pathnorm;

/// Directories never descended into during a scan, BEYOND the blanket dotdir rule
/// ([`is_excluded_dir`] also drops every `.`-prefixed directory). These are the
/// non-dot package vendors and build outputs whose nested `package.json` files are
/// noise (every dependency under `node_modules` ships one). Matched by exact
/// directory name (case-sensitive on Unix; this is the conventional spelling).
pub const SCAN_EXCLUSIONS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "target",
    "coverage",
    "out",
    "vendor",
];

/// How deep the scan descends from the workspace root WHEN no `workspaces` manifest
/// bounds it (the non-monorepo / unbounded case). A modest bound that covers root +
/// conventional monorepo packages (`packages/api`, `apps/web/sub`, …) while keeping a
/// pathological tree from being walked forever. When a root manifest DECLARES
/// workspaces (npm/yarn `workspaces`, `pnpm-workspace.yaml`), discovery is bounded by
/// those globs instead and this depth does not apply (see [`discover_package_scripts`]).
const MAX_SCAN_DEPTH: usize = 4;

/// Is `name` a directory the scan must never descend into? Drops (a) any hidden
/// dotdir (`.git`, `.agents`, `.next`, `.cache`, … — every `.`-prefixed name, which
/// covers tool/config caches without enumerating them), and (b) the explicit
/// vendor/build [`SCAN_EXCLUSIONS`]. This is the monorepo-noise filter that keeps the
/// scan from surfacing thousands of irrelevant nested manifests.
fn is_excluded_dir(name: &str) -> bool {
    name.starts_with('.') || SCAN_EXCLUSIONS.contains(&name)
}

/// Is `dir` the root of a git repository — i.e. does it directly contain a `.git`
/// entry? (PRD-4.1 #2, repo-of-repos.) A standard repo has a `.git` DIRECTORY; a git
/// worktree / submodule has a `.git` FILE (a gitdir pointer). Either form counts, so a
/// nested sub-repo the parent gitignores is still recognized as a scan candidate. We
/// only check for the `.git` entry's existence — we never descend INTO it (`.git` is a
/// dotdir, dropped by [`is_excluded_dir`]).
fn is_git_repo_dir(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// A detected package manager. Stored as the DB `package_manager` string
/// (npm/pnpm/yarn/bun — the v3 CHECK vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl PackageManager {
    /// The persisted `package_manager` string (DB CHECK vocabulary).
    pub fn as_db_str(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Yarn => "yarn",
            PackageManager::Bun => "bun",
        }
    }

    /// The RUNNER invocation for `script` under this manager — the default command
    /// an imported template runs. NOT the raw script body: importing `dev` yields
    /// `pnpm dev` / `bun run dev` / `yarn dev` / `npm run dev`, which is what a
    /// developer would type, independent of how the script is implemented.
    pub fn run_script(self, script: &str) -> String {
        match self {
            // pnpm and yarn accept the script name directly (`pnpm dev`).
            PackageManager::Pnpm => format!("pnpm {script}"),
            PackageManager::Yarn => format!("yarn {script}"),
            // npm and bun need the explicit `run` verb (`npm run dev`).
            PackageManager::Npm => format!("npm run {script}"),
            PackageManager::Bun => format!("bun run {script}"),
        }
    }
}

/// Parse the `packageManager` field value (`"pnpm@8.6.0"`, `"yarn@4"`, …) into a
/// [`PackageManager`]. Only the name before an optional `@version` matters. An
/// unrecognized name yields `None` (the caller falls back to lockfile/npm).
fn manager_from_field(value: &str) -> Option<PackageManager> {
    let name = value.split('@').next().unwrap_or("").trim();
    match name {
        "npm" => Some(PackageManager::Npm),
        "pnpm" => Some(PackageManager::Pnpm),
        "yarn" => Some(PackageManager::Yarn),
        "bun" => Some(PackageManager::Bun),
        _ => None,
    }
}

/// Map a lockfile NAME to its package manager. The lockfile is the second-priority
/// signal after the `packageManager` field.
fn manager_from_lockfile(name: &str) -> Option<PackageManager> {
    match name {
        "pnpm-lock.yaml" => Some(PackageManager::Pnpm),
        "bun.lock" | "bun.lockb" => Some(PackageManager::Bun),
        "yarn.lock" => Some(PackageManager::Yarn),
        "package-lock.json" => Some(PackageManager::Npm),
        _ => None,
    }
}

/// All lockfile names we recognize, in detection priority order (most specific
/// managers first; npm's lockfile last as the most common/least specific).
const LOCKFILE_NAMES: &[&str] = &[
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "yarn.lock",
    "package-lock.json",
];

// --- .gitignore matching (monorepo-aware discovery filter) ------------------
//
// A FOCUSED, dependency-free `.gitignore` matcher: enough to keep discovery from
// pulling in gitignored trees (vendored deps, build dirs, fixtures the repo ignores)
// WITHOUT pulling a heavy `ignore`/`globset` crate for a handful of patterns. It
// honors the common gitignore forms used to exclude directories:
//   - blank lines and `#` comments are ignored;
//   - a leading `!` negates (re-includes) a previously ignored path;
//   - a leading `/` anchors the pattern to the gitignore's own directory;
//   - a trailing `/` matches directories only (we only ever match dir names);
//   - `*` matches any run of non-separator chars within a single path segment;
//   - a pattern with no `/` (other than a trailing one) matches by BASENAME at any
//     depth (git's "match in any directory" rule);
//   - otherwise the pattern is matched against the path RELATIVE to the gitignore.
// Deliberately omitted (rare in the dir-exclusion case this serves): `**`,
// character classes, and escaped specials — they degrade to a literal match, which
// is safe (a miss only means we scan a dir we could have skipped, never the reverse).

/// One parsed `.gitignore` line: a glob plus its modifiers.
struct GitignoreRule {
    /// The pattern body with `!`, anchoring `/`, and trailing `/` stripped off.
    pattern: String,
    /// `!`-prefixed: a match RE-INCLUDES the path instead of ignoring it.
    negated: bool,
    /// Leading-`/` (or embedded-`/`) anchored to the gitignore dir; else basename-matchable.
    anchored: bool,
    /// Trailing-`/`: matches directories only (always true for our dir-only use).
    dir_only: bool,
}

/// A `.gitignore` file's rules, paired with the directory it governs (relative to the
/// workspace root, `""` = workspace root). Rules apply to paths under that directory.
struct Gitignore {
    /// The gitignore's directory, relative to the workspace root (`""` = root).
    base: String,
    rules: Vec<GitignoreRule>,
}

impl Gitignore {
    /// Parse a `.gitignore` body. `base` is the gitignore's directory relative to the
    /// workspace root. Empty/comment lines are dropped.
    fn parse(base: &str, text: &str) -> Self {
        let mut rules = Vec::new();
        for raw in text.lines() {
            let line = raw.trim_end();
            // Skip blanks and comments (a leading '#'; '\#' would escape it, ignored here).
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let mut p = line.trim();
            let negated = p.starts_with('!');
            if negated {
                p = &p[1..];
            }
            let dir_only = p.ends_with('/');
            let p = p.trim_end_matches('/');
            // Anchored if it begins with '/', or contains a '/' in the middle (git treats
            // a slash anywhere but the trailing one as anchoring to the gitignore dir).
            let inner = p.trim_start_matches('/');
            let anchored = p.starts_with('/') || inner.contains('/');
            if inner.is_empty() {
                continue;
            }
            rules.push(GitignoreRule {
                pattern: inner.to_string(),
                negated,
                anchored,
                dir_only,
            });
        }
        Gitignore {
            base: base.to_string(),
            rules,
        }
    }

    /// Decide whether `rel_to_ws` (a path RELATIVE to the workspace root, forward-slash
    /// separated, `dir`-known) is ignored by THIS gitignore. Returns `Some(true)` /
    /// `Some(false)` when a rule matches (later rules win, so `!`-negation can re-include),
    /// or `None` when no rule applies.
    fn verdict(&self, rel_to_ws: &str, is_dir: bool) -> Option<bool> {
        // Express the path relative to the gitignore's own directory. A path not under
        // this gitignore's directory yields no verdict.
        let rel = if self.base.is_empty() {
            rel_to_ws
        } else {
            rel_to_ws
                .strip_prefix(&self.base)
                .and_then(|s| s.strip_prefix('/'))?
        };
        if rel.is_empty() {
            return None;
        }
        let basename = rel.rsplit('/').next().unwrap_or(rel);
        let mut verdict = None;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            let hit = if rule.anchored {
                // Anchored: match against the full path relative to the gitignore dir,
                // OR a leading sub-path (so `a/b` ignores `a/b/c`).
                glob_match_path(&rule.pattern, rel)
            } else {
                // Unanchored: match the basename at any depth.
                glob_match_segment(&rule.pattern, basename)
            };
            if hit {
                verdict = Some(!rule.negated);
            }
        }
        verdict
    }
}

/// Match a glob `pat` (with `*` = any run of non-`/` chars) against a single path
/// SEGMENT `seg` (no separators). Anchored at both ends.
fn glob_match_segment(pat: &str, seg: &str) -> bool {
    glob_match_impl(pat.as_bytes(), seg.as_bytes(), false)
}

/// Match a glob `pat` against a RELATIVE PATH `path` (may contain `/`). `*` does not
/// cross `/`. A pattern matches when it equals the whole path OR a leading directory
/// prefix of it (so `a/b` matches `a/b/c`), mirroring git's directory-prefix rule.
fn glob_match_path(pat: &str, path: &str) -> bool {
    if glob_match_impl(pat.as_bytes(), path.as_bytes(), true) {
        return true;
    }
    // Leading-prefix: try matching `pat` against each directory prefix of `path`, so a
    // pattern like `a/b` ignores everything under it (`a/b/c`).
    let bytes = path.as_bytes();
    for (i, c) in path.char_indices() {
        if c == '/' && glob_match_impl(pat.as_bytes(), &bytes[..i], true) {
            return true;
        }
    }
    false
}

/// Backtracking glob matcher: `*` matches any run of chars (stopping at `/` when
/// `cross_sep` is false), `?` matches a single non-`/` char, everything else is
/// literal. Anchored at both ends.
fn glob_match_impl(pat: &[u8], text: &[u8], cross_sep: bool) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);
    while t < text.len() {
        if p < pat.len() && (pat[p] == text[t] || (pat[p] == b'?' && text[t] != b'/')) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star_p {
            // Backtrack: let the last `*` swallow one more char (unless it would cross
            // a separator and `cross_sep` is false).
            if !cross_sep && text[star_t] == b'/' {
                return false;
            }
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

// --- workspaces manifest globs (monorepo-aware discovery bound) -------------

/// Parse the workspace package GLOBS declared at the workspace root, if any. npm/yarn
/// declare them in `package.json` `workspaces` (an array of globs, OR an object with a
/// `packages` array); pnpm in `pnpm-workspace.yaml` (`packages:` YAML list). Returns
/// the glob list when a manifest declares workspaces, else `None` (→ bounded-depth
/// scan). The globs are package DIRECTORIES relative to the root (e.g. `packages/*`,
/// `apps/*`, `tools/cli`); a trailing `/*` (`/**`) means "each immediate child dir".
fn workspace_globs(root: &Path, root_json: Option<&serde_json::Value>) -> Option<Vec<String>> {
    // 1) npm/yarn `workspaces` in the root package.json.
    if let Some(json) = root_json {
        if let Some(ws) = json.get("workspaces") {
            let globs = match ws {
                serde_json::Value::Array(arr) => globs_from_json_array(arr),
                serde_json::Value::Object(obj) => obj
                    .get("packages")
                    .and_then(|p| p.as_array())
                    .map(|a| globs_from_json_array(a))
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            if !globs.is_empty() {
                return Some(globs);
            }
        }
    }
    // 2) pnpm-workspace.yaml `packages:` list.
    if let Ok(text) = std::fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let globs = pnpm_workspace_packages(&text);
        if !globs.is_empty() {
            return Some(globs);
        }
    }
    None
}

/// Collect the string entries of a JSON array as workspace globs (skipping negations
/// like `!packages/excluded`, which we treat as "not a positive include").
fn globs_from_json_array(arr: &[serde_json::Value]) -> Vec<String> {
    arr.iter()
        .filter_map(|v| v.as_str())
        .filter(|s| !s.starts_with('!'))
        .map(normalize_glob)
        .collect()
}

/// Minimal `pnpm-workspace.yaml` `packages:` extractor. Reads the `- "glob"` list items
/// under a top-level `packages:` key. Deliberately tiny (no YAML crate): handles the
/// conventional shape pnpm emits. Quotes are stripped; `!`-negations are skipped.
fn pnpm_workspace_packages(text: &str) -> Vec<String> {
    let mut globs = Vec::new();
    let mut in_packages = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        // Leaving the `packages:` block: a non-indented, non-list key.
        if in_packages && !line.starts_with(char::is_whitespace) && !trimmed.starts_with('-') {
            in_packages = false;
        }
        if trimmed.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if in_packages {
            if let Some(item) = trimmed.strip_prefix('-') {
                let g = item.trim().trim_matches(['"', '\'']).trim();
                if !g.is_empty() && !g.starts_with('!') {
                    globs.push(normalize_glob(g));
                }
            }
        }
    }
    globs
}

/// Normalize a workspace glob to forward slashes with no leading/trailing slash, so it
/// composes cleanly with the forward-slash relative subfolders the scan computes.
fn normalize_glob(g: &str) -> String {
    g.replace('\\', "/")
        .trim_matches('/')
        .to_string()
}

/// Does a package directory at relative path `subfolder` (forward-slash, `""` = root)
/// satisfy at least one workspace `glob`? A glob like `packages/*` matches a single
/// level under `packages/`; `packages/**` (or trailing `/**`) matches any depth; an
/// exact `tools/cli` matches just that dir. The root (`""`) is always included (the
/// root manifest's own scripts), independent of the globs.
fn matches_workspace_glob(subfolder: &str, globs: &[String]) -> bool {
    if subfolder.is_empty() {
        return true; // the root package.json is always a candidate
    }
    globs.iter().any(|g| workspace_glob_matches(g, subfolder))
}

/// Match ONE workspace glob against a relative package dir. Handles the package-glob
/// vocabulary npm/yarn/pnpm use: `*` within a segment, `**` across segments, and the
/// common trailing `/*` ("immediate children") / `/**` ("any descendant").
fn workspace_glob_matches(glob: &str, subfolder: &str) -> bool {
    // `a/**` should also match `a` itself per the common workspace convention, but the
    // root-vs-package split already covers `""`; for non-root, fall through to matching.
    let g = glob.as_bytes();
    let s = subfolder.as_bytes();
    ws_glob_impl(g, s)
}

/// Backtracking matcher supporting `*` (within a segment) and `**` (across segments).
fn ws_glob_impl(pat: &[u8], text: &[u8]) -> bool {
    // Iterative matcher with `**` support. `**` (optionally followed by `/`) matches any
    // number of path segments including zero.
    fn helper(pat: &[u8], text: &[u8]) -> bool {
        let (mut p, mut t) = (0usize, 0usize);
        while p < pat.len() {
            if pat[p] == b'*' && p + 1 < pat.len() && pat[p + 1] == b'*' {
                // `**`: consume it (and an optional following `/`), then try to match the
                // remainder at every position of `text`.
                let mut rest = p + 2;
                if rest < pat.len() && pat[rest] == b'/' {
                    rest += 1;
                }
                if rest >= pat.len() {
                    return true; // trailing `**` matches anything remaining
                }
                // Try matching the remainder of the pattern at t, then after each '/'.
                if helper(&pat[rest..], &text[t..]) {
                    return true;
                }
                for i in t..text.len() {
                    if text[i] == b'/' && helper(&pat[rest..], &text[i + 1..]) {
                        return true;
                    }
                }
                return false;
            } else if pat[p] == b'*' {
                // Single `*`: match any run of non-`/` chars in this segment.
                let mut rest = p + 1;
                // Collapse a redundant double handled above; here rest points past `*`.
                let _ = &mut rest;
                // Try every split of the current segment.
                let seg_end = text[t..].iter().position(|&c| c == b'/').map(|i| t + i).unwrap_or(text.len());
                for split in t..=seg_end {
                    if helper(&pat[p + 1..], &text[split..]) {
                        return true;
                    }
                }
                return false;
            } else if t < text.len() && (pat[p] == text[t]) {
                p += 1;
                t += 1;
            } else {
                return false;
            }
        }
        t == text.len()
    }
    helper(pat, text)
}

/// One discovered script, ready to surface in the import UI. Carries everything
/// the import needs: the EDITABLE proposed `name`, the default RUNNER `command`,
/// the `subfolder` (the package.json's location relative to the workspace), and
/// the source metadata to persist (`package_json_path`, `script_name`,
/// `script_command_snapshot`, `package_manager`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredScript {
    /// Proposed template name (editable in the UI). `<script>` when the script name
    /// is unique across the whole scan, else `<package-or-folder>:<script>`.
    pub proposed_name: String,
    /// The raw script name as it appears in `package.json` `scripts`.
    pub script_name: String,
    /// Default RUNNER command (`pnpm dev` / `npm run dev` / …), editable in the UI.
    pub default_command: String,
    /// Snapshot of the raw script body at discovery time (informative; not authority).
    pub script_command_snapshot: String,
    /// The package.json's directory RELATIVE to the workspace (`""` = root). Used as
    /// the template's `subfolder`.
    pub subfolder: String,
    /// Absolute, normalized path of the originating `package.json`.
    pub package_json_path: String,
    /// Detected package manager (DB string: npm/pnpm/yarn/bun).
    pub package_manager: String,
}

/// One `package.json` retained by the scan, with its parsed scripts + manager.
struct PackageFile {
    /// Absolute, normalized package.json path (canonicalized, proven in-workspace).
    abs_path: String,
    /// The package.json's directory, relative to the workspace (`""` = root).
    subfolder: String,
    /// A display label for name disambiguation: the package's `name` field if
    /// present, else its directory name (or the workspace's own name at the root).
    label: String,
    /// Detected package manager for this file.
    manager: PackageManager,
    /// `scripts` as (name, body) pairs, in file order.
    scripts: Vec<(String, String)>,
}

/// The outcome of a discovery scan: the discoverable scripts PLUS a discovery summary
/// the agent needs to distinguish "no manifest found" from "all already imported".
/// `manifests_found` counts the retained `package.json` files (root + sub-packages)
/// — `0` means there was nothing to import because no manifest exists (the
/// `reason:no_manifest` signal lives at the MCP layer, derived from this count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryResult {
    /// Discoverable scripts, with editable proposed names + runner commands.
    pub scripts: Vec<DiscoveredScript>,
    /// How many `package.json` manifests the (filtered) scan retained.
    pub manifests_found: usize,
}

/// Discover the package.json scripts under `workspace_path` (back-compat shim that
/// returns just the script list). See [`discover_scripts`] for the richer result that
/// also carries the manifest count.
pub fn discover_package_scripts(workspace_path: &str) -> Vec<DiscoveredScript> {
    discover_scripts(workspace_path).scripts
}

/// Discover the package.json scripts under `workspace_path`, grouped by location, each
/// with an editable proposed name and a default runner command, ALONGSIDE a discovery
/// summary ([`DiscoveryResult::manifests_found`]).
///
/// **Filtered, monorepo-aware walk.** From the (canonicalized) workspace root the scan
/// descends child directories, NEVER entering (a) hidden dotdirs (`.git`, `.agents`,
/// `.next`, … — any `.`-prefixed dir) or the explicit vendor/build [`SCAN_EXCLUSIONS`]
/// (`node_modules`, `dist`, `target`, …), nor (b) directories ignored by a `.gitignore`
/// in scope (the workspace's own `.gitignore` plus any nested ones), so a repo's
/// gitignored trees (vendored deps, fixtures, generated dirs) are not surfaced.
///
/// **Repo-of-repos exception (PRD-4.1 #2).** A directory that is itself a git repo (it
/// directly contains a `.git` entry — see [`is_git_repo_dir`]) is scanned EVEN WHEN the
/// parent's `.gitignore` ignores it: in the umbrella / repo-of-repos layout the parent
/// gitignores its nested sub-repos, but those are exactly the folders holding the real
/// `package.json` files. Only the gitignore skip is overridden for such a dir; the dotdir
/// and `node_modules` exclusions still apply, a gitignored NON-repo dir is still skipped,
/// and the scan never descends into the `.git` directory itself.
///
/// If the ROOT manifest declares workspaces (npm/yarn `package.json` `workspaces`, or
/// `pnpm-workspace.yaml`), discovery is BOUNDED to the root + directories matching those
/// globs (so only real workspace packages contribute). Otherwise the walk is bounded by
/// [`MAX_SCAN_DEPTH`]. Every retained `package.json` is canonicalized and re-checked to
/// be inside the workspace; a file that escapes (e.g. via a symlink) is dropped.
/// Unreadable / unparsable files are skipped, so a workspace with no readable
/// `package.json` yields an EMPTY result (never an error).
///
/// Proposed names: a script name unique across the whole result keeps its bare name
/// (`dev`); a name appearing in several packages is disambiguated as
/// `<package-or-folder>:<script>` (`api:dev`).
pub fn discover_scripts(workspace_path: &str) -> DiscoveryResult {
    let workspace_norm = pathnorm::normalize(workspace_path);
    // Canonicalize the workspace once so containment is checked against the
    // symlink-resolved root. If the workspace itself is inaccessible, there is
    // nothing to scan.
    let Ok(canon_ws) = std::fs::canonicalize(Path::new(workspace_path)) else {
        return DiscoveryResult {
            scripts: Vec::new(),
            manifests_found: 0,
        };
    };
    let canon_ws_norm = pathnorm::normalize(&canon_ws.to_string_lossy());

    // Read the ROOT package.json once (if any) to detect declared workspaces. The
    // presence of `workspaces` / `pnpm-workspace.yaml` switches discovery from
    // bounded-depth to glob-bounded (only real workspace packages contribute).
    let root_json: Option<serde_json::Value> = std::fs::read_to_string(canon_ws.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let globs = workspace_globs(&canon_ws, root_json.as_ref());

    let mut ctx = DiscoveryCtx {
        canon_ws_norm: &canon_ws_norm,
        workspace_norm: &workspace_norm,
        globs: globs.as_deref(),
        gitignores: Vec::new(),
        files: Vec::new(),
    };
    // Seed the root .gitignore (if any) before walking.
    ctx.push_gitignore(&canon_ws, "");
    collect_package_files(&canon_ws, "", 0, &mut ctx);
    let files = ctx.files;

    // Determine which script names are AMBIGUOUS (appear in more than one package),
    // so unique names stay bare and only collisions get the `<label>:` prefix.
    let mut script_counts: HashMap<&str, usize> = HashMap::new();
    for f in &files {
        for (name, _) in &f.scripts {
            *script_counts.entry(name.as_str()).or_insert(0) += 1;
        }
    }

    let mut out = Vec::new();
    for f in &files {
        for (script_name, body) in &f.scripts {
            let ambiguous = script_counts
                .get(script_name.as_str())
                .copied()
                .unwrap_or(0)
                > 1;
            let proposed_name = if ambiguous {
                format!("{}:{}", f.label, script_name)
            } else {
                script_name.clone()
            };
            out.push(DiscoveredScript {
                proposed_name,
                script_name: script_name.clone(),
                default_command: f.manager.run_script(script_name),
                script_command_snapshot: body.clone(),
                subfolder: f.subfolder.clone(),
                package_json_path: f.abs_path.clone(),
                package_manager: f.manager.as_db_str().to_string(),
            });
        }
    }
    DiscoveryResult {
        scripts: out,
        manifests_found: files.len(),
    }
}

/// Mutable state threaded through the recursive walk: the workspace anchors, the
/// optional workspace-globs bound, the stack of in-scope `.gitignore` files, and the
/// accumulating retained manifests.
struct DiscoveryCtx<'a> {
    /// Canonical workspace root (containment + relative-subfolder base).
    canon_ws_norm: &'a str,
    /// The stored workspace path (for the human-facing root label).
    workspace_norm: &'a str,
    /// When `Some`, discovery is bounded to the root + dirs matching these workspace
    /// globs; when `None`, the walk is bounded by [`MAX_SCAN_DEPTH`].
    globs: Option<&'a [String]>,
    /// In-scope `.gitignore` files (root first, then nested), checked newest-last.
    gitignores: Vec<Gitignore>,
    /// Retained, parsed manifests.
    files: Vec<PackageFile>,
}

impl DiscoveryCtx<'_> {
    /// Parse and push `dir`'s `.gitignore` (if present) onto the in-scope stack. `rel`
    /// is `dir`'s path relative to the workspace root (`""` = root).
    fn push_gitignore(&mut self, dir: &Path, rel: &str) -> bool {
        if let Ok(text) = std::fs::read_to_string(dir.join(".gitignore")) {
            self.gitignores.push(Gitignore::parse(rel, &text));
            true
        } else {
            false
        }
    }

    /// Is the path at relative `rel` (a dir when `is_dir`) ignored by any in-scope
    /// `.gitignore`? The DEEPEST gitignore with a verdict wins, and within one file the
    /// LAST matching rule wins (so `!`-negation can re-include). A negated final verdict
    /// (`false`) means "explicitly NOT ignored".
    fn is_gitignored(&self, rel: &str, is_dir: bool) -> bool {
        // Walk gitignores from deepest to shallowest; the first that yields a verdict
        // for this path decides (a deeper .gitignore overrides a shallower one).
        for gi in self.gitignores.iter().rev() {
            if let Some(v) = gi.verdict(rel, is_dir) {
                return v;
            }
        }
        false
    }
}

/// Recursively collect retained `package.json` files under `dir`, honoring the dotdir +
/// vendor exclusions, the `.gitignore` rules in scope, and EITHER the workspace globs
/// (when declared) OR the depth bound. `rel` is `dir`'s path relative to the workspace
/// root (`""` = root).
fn collect_package_files(dir: &Path, rel: &str, depth: usize, ctx: &mut DiscoveryCtx) {
    // Without a workspace-globs bound, stop at the depth limit. WITH globs, depth is
    // not the bound (the globs are) — but a sane ceiling still guards a pathological
    // tree, so cap at a generous multiple.
    let max_depth = if ctx.globs.is_some() {
        MAX_SCAN_DEPTH * 3
    } else {
        MAX_SCAN_DEPTH
    };
    if depth > max_depth {
        return;
    }

    // A package.json directly in this directory? Retain it when this dir is a discovery
    // CANDIDATE: the root always, else (with globs) only a glob-matching dir, else any
    // dir within the depth bound.
    let candidate = match ctx.globs {
        Some(globs) => matches_workspace_glob(rel, globs),
        None => true,
    };
    if candidate {
        let pkg = dir.join("package.json");
        if pkg.is_file() {
            if let Some(file) =
                read_package_file(&pkg, dir, ctx.canon_ws_norm, ctx.workspace_norm)
            {
                ctx.files.push(file);
            }
        }
    }

    // Descend into child directories, skipping the exclusions + gitignored dirs.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Track how many gitignores we push at this level so we can pop them on the way out.
    for entry in entries.flatten() {
        let path = entry.path();
        // Only descend into real directories (never follow a symlink into a
        // directory — that is a classic escape vector and avoids cycles).
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // (a) dotdirs + explicit vendor/build dirs. This drops `.git` itself (a dotdir),
        // so the scan never descends INTO a `.git` directory.
        if is_excluded_dir(name) {
            continue;
        }
        let child_rel = if rel.is_empty() {
            name.to_string()
        } else {
            format!("{rel}/{name}")
        };
        // (b) gitignored directories — UNLESS the directory is itself a git repo.
        //
        // Repo-of-repos (PRD-4.1 #2, the PalBank layout): an umbrella repo gitignores its
        // nested sub-repos, but those sub-repos are exactly the folders holding the real
        // `package.json` files. So a directory that CONTAINS a `.git` entry is a legitimate
        // scan candidate even when the parent's `.gitignore` ignores it — we override the
        // gitignore skip for it. All OTHER exclusions still hold: dotdirs (incl. `.agents`)
        // and `node_modules` were already dropped above (a), and a gitignored NON-repo dir
        // is still skipped here. We never scan inside `.git` itself (it is a dotdir, (a)).
        //
        // The override is LIMITED to the workspace's DIRECT children (`depth == 0` means we
        // are iterating the workspace ROOT's entries): the umbrella / repo-of-repos layout
        // puts its sub-repos at the TOP level, so a gitignored `.git`-dir there is a real
        // scan candidate; a gitignored `.git`-dir buried DEEPER (a vendored clone, a tool
        // cache, a checked-out dependency) stays excluded as the user intended — we do not
        // un-ignore arbitrarily nested repos (arbitration B).
        let gitignored = ctx.is_gitignored(&child_rel, true);
        let is_subrepo = depth == 0 && is_git_repo_dir(&path);
        if gitignored && !is_subrepo {
            continue;
        }
        // RE-ROOT the gitignore scope at a gitignored sub-repo (finding #10). When the
        // override above lets us into a nested repo the PARENT excluded, the umbrella's
        // `.gitignore` must NOT govern that repo's tree — only the sub-repo's OWN
        // `.gitignore` does, exactly as git treats an independent repo boundary. Otherwise
        // a broad umbrella rule (e.g. `sub-repo/`) would still match `sub-repo/packages/…`
        // and silently drop the sub-repo's nested packages. So for that descent we swap in
        // a fresh stack and restore it after. (A non-ignored nested dir keeps the inherited
        // stack — its rules legitimately apply.) The dotdir/`node_modules` exclusions in
        // (a) are gitignore-independent, so they keep protecting the sub-repo's tree.
        let saved = if gitignored && is_subrepo {
            Some(std::mem::take(&mut ctx.gitignores))
        } else {
            None
        };
        // Push this dir's own .gitignore (if any) before descending, pop after.
        let pushed = ctx.push_gitignore(&path, &child_rel);
        collect_package_files(&path, &child_rel, depth + 1, ctx);
        if pushed {
            ctx.gitignores.pop();
        }
        if let Some(saved) = saved {
            ctx.gitignores = saved;
        }
    }
}

/// Read + parse one `package.json` at `pkg` (whose containing dir is `dir`),
/// returning a [`PackageFile`] or `None` if it is unreadable, unparsable, escapes
/// the workspace after canonicalization, or has no usable scripts. `None` is the
/// "skip silently" signal (a bad file never aborts the whole scan).
fn read_package_file(
    pkg: &Path,
    dir: &Path,
    canon_ws_norm: &str,
    workspace_norm: &str,
) -> Option<PackageFile> {
    // Canonicalize the package.json and refuse anything that escapes the workspace
    // (a symlinked file pointing outside). The canonical, normalized path is what
    // we store as `source_package_json_path`.
    let canon = std::fs::canonicalize(pkg).ok()?;
    let abs_norm = pathnorm::normalize(&canon.to_string_lossy());
    if !pathnorm::is_ancestor_or_equal(canon_ws_norm, &abs_norm) {
        return None;
    }

    let text = std::fs::read_to_string(pkg).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    // Scripts (object of name -> command). A package.json with no scripts yields
    // nothing useful; skip it.
    let scripts_obj = json.get("scripts").and_then(|v| v.as_object())?;
    let scripts: Vec<(String, String)> = scripts_obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();
    if scripts.is_empty() {
        return None;
    }

    let manager = detect_manager(&json, dir, canon_ws_norm);

    // The package.json's directory relative to the workspace ("" = root), derived
    // from the canonical dir vs canonical workspace. Used as the template subfolder.
    let canon_dir = canon.parent().unwrap_or(dir);
    let canon_dir_norm = pathnorm::normalize(&canon_dir.to_string_lossy());
    let subfolder = relative_under(canon_ws_norm, &canon_dir_norm);

    // A display label for disambiguation: the package's `name`, else the folder
    // name, else the workspace's own basename at the root.
    let label = json
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if subfolder.is_empty() {
                basename(workspace_norm)
            } else {
                basename(&subfolder)
            }
        });

    Some(PackageFile {
        abs_path: abs_norm,
        subfolder,
        label,
        manager,
        scripts,
    })
}

/// Detect the package manager for a package.json: the `packageManager` field wins;
/// else the NEAREST lockfile walking from the package's dir UP to (and including)
/// the workspace root; else npm. `canon_ws_norm` bounds the upward search so it
/// never climbs above the workspace.
fn detect_manager(json: &serde_json::Value, dir: &Path, canon_ws_norm: &str) -> PackageManager {
    // 1) `packageManager` field (highest priority).
    if let Some(field) = json.get("packageManager").and_then(|v| v.as_str()) {
        if let Some(pm) = manager_from_field(field) {
            return pm;
        }
    }

    // 2) Nearest lockfile, searching from `dir` upward to the workspace root.
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        // Stop once we have left the workspace (the canonical dir is no longer
        // inside the canonical workspace root).
        let d_norm = pathnorm::normalize(&d.to_string_lossy());
        if !pathnorm::is_ancestor_or_equal(canon_ws_norm, &d_norm) {
            break;
        }
        for lock in LOCKFILE_NAMES {
            if d.join(lock).is_file() {
                if let Some(pm) = manager_from_lockfile(lock) {
                    return pm;
                }
            }
        }
        if d_norm == canon_ws_norm {
            break; // reached the workspace root; stop climbing.
        }
        cur = d.parent().map(Path::to_path_buf);
    }

    // 3) Fallback.
    PackageManager::Npm
}

/// Return `descendant` expressed RELATIVE to `ancestor` (both normalized canonical
/// strings), or `""` when they are equal. Assumes `ancestor` is an
/// ancestor-or-equal of `descendant` (the caller has checked). Uses the platform
/// separator so the result is a usable subfolder for [`crate::subfolder`].
fn relative_under(ancestor: &str, descendant: &str) -> String {
    if ancestor == descendant {
        return String::new();
    }
    let rest = descendant.strip_prefix(ancestor).unwrap_or(descendant);
    // Trim the leading separator left by the strip.
    rest.trim_start_matches(['/', '\\']).to_string()
}

/// The final path component of a normalized path (its basename), or the whole
/// string if it has no separator. Used as a fallback disambiguation label.
fn basename(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// Create a managed-command template from an import selection.
///
/// `name` is the FINAL (user-edited) template name, `command` the FINAL (editable)
/// command line, `subfolder` the package.json location, and `source` the provenance
/// to persist (`source_package_json_path`, `source_script_name`,
/// `source_script_command_snapshot`, `package_manager`, with `source_kind` =
/// `package_json`).
///
/// A `name` already taken by another template in the project is REFUSED with a
/// clear error (the caller keeps the name editable until unique). The DB's
/// `UNIQUE(project_id, name)` is the backstop, but we check first so the message is
/// human-facing, not a raw SQLite constraint string.
pub fn import_command(
    conn: &mut SqliteConnection,
    project_id: &str,
    name: &str,
    command: &str,
    subfolder: &str,
    source: CommandSource,
) -> Result<ManagedCommand, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a command name is required".to_string());
    }

    // Block a name already present in the project BEFORE inserting, so the error is
    // a clear "name already used" rather than a raw UNIQUE violation.
    let existing = db::list_templates(conn, project_id).map_err(|e| e.to_string())?;
    if existing.iter().any(|t| t.name == name) {
        return Err(format!(
            "the name '{name}' is already used by a command in this project — choose a unique name"
        ));
    }

    let subfolder = subfolder.trim();
    let subfolder_opt = if subfolder.is_empty() {
        None
    } else {
        Some(subfolder)
    };

    db::create_template(conn, project_id, name, command, subfolder_opt, source).map_err(|e| {
        // A racing duplicate (UNIQUE) still surfaces as a clear message.
        format!("could not create command '{name}': {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, open_in_memory, SOURCE_KIND_PACKAGE_JSON};
    use std::path::PathBuf;

    /// A throwaway temp directory tree, canonicalized, cleaned on drop.
    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(tag: &str) -> Self {
            let mut root = std::env::temp_dir();
            let uniq = format!(
                "nyx_pkgjson_{}_{}_{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            root.push(uniq);
            std::fs::create_dir_all(&root).expect("create temp root");
            let root = std::fs::canonicalize(&root).expect("canonicalize temp root");
            TempTree { root }
        }

        fn path(&self) -> String {
            self.root.to_string_lossy().into_owned()
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let p = self.root.join(rel);
            std::fs::create_dir_all(&p).expect("mkdir");
            p
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::write(&p, content).expect("write file");
            p
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn find<'a>(scripts: &'a [DiscoveredScript], proposed: &str) -> Option<&'a DiscoveredScript> {
        scripts.iter().find(|s| s.proposed_name == proposed)
    }

    // --- Package manager detection ---------------------------------------

    #[test]
    fn manager_from_package_manager_field_wins() {
        let tree = TempTree::new("pm_field");
        // A pnpm-lock would say pnpm, but the field says yarn → field wins.
        tree.write("pnpm-lock.yaml", "");
        tree.write(
            "package.json",
            r#"{ "packageManager": "yarn@4.1.0", "scripts": { "dev": "vite" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        let dev = find(&scripts, "dev").expect("dev script");
        assert_eq!(
            dev.package_manager, "yarn",
            "packageManager field is priority"
        );
        assert_eq!(dev.default_command, "yarn dev");
    }

    #[test]
    fn manager_from_nearest_lockfile() {
        for (lock, pm, cmd) in [
            ("pnpm-lock.yaml", "pnpm", "pnpm dev"),
            ("bun.lockb", "bun", "bun run dev"),
            ("yarn.lock", "yarn", "yarn dev"),
            ("package-lock.json", "npm", "npm run dev"),
        ] {
            let tree = TempTree::new(&format!("lock_{pm}"));
            tree.write(lock, "");
            tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
            let scripts = discover_package_scripts(&tree.path());
            let dev = find(&scripts, "dev").expect("dev script");
            assert_eq!(dev.package_manager, pm, "{lock} must detect {pm}");
            assert_eq!(dev.default_command, cmd);
        }
    }

    #[test]
    fn fallback_to_npm_without_field_or_lockfile() {
        let tree = TempTree::new("fallback");
        tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
        let scripts = discover_package_scripts(&tree.path());
        let dev = find(&scripts, "dev").expect("dev script");
        assert_eq!(dev.package_manager, "npm", "no signal => npm fallback");
        assert_eq!(dev.default_command, "npm run dev");
    }

    #[test]
    fn default_command_is_runner_not_raw_body() {
        let tree = TempTree::new("runner");
        tree.write("pnpm-lock.yaml", "");
        tree.write(
            "package.json",
            r#"{ "scripts": { "dev": "vite --host 0.0.0.0" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        let dev = find(&scripts, "dev").expect("dev");
        // The DEFAULT command is the runner, not the raw `vite --host ...`.
        assert_eq!(dev.default_command, "pnpm dev");
        // But the raw body is snapshotted for provenance/swap.
        assert_eq!(dev.script_command_snapshot, "vite --host 0.0.0.0");
    }

    // --- Discovery: root + subfolders, exclusions ------------------------

    #[test]
    fn discovers_root_and_subfolders() {
        let tree = TempTree::new("monorepo");
        tree.write(
            "package.json",
            r#"{ "name": "root", "scripts": { "build": "tsc" } }"#,
        );
        tree.write(
            "packages/api/package.json",
            r#"{ "name": "api", "scripts": { "start": "node ." } }"#,
        );
        tree.write(
            "apps/web/package.json",
            r#"{ "name": "web", "scripts": { "serve": "next" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        // All three packages' scripts were found.
        assert!(find(&scripts, "build").is_some(), "root build found");
        assert!(find(&scripts, "start").is_some(), "api start found");
        assert!(find(&scripts, "serve").is_some(), "web serve found");

        // Subfolders are the package locations relative to the workspace.
        let start = find(&scripts, "start").unwrap();
        assert_eq!(start.subfolder, "packages/api");
        let serve = find(&scripts, "serve").unwrap();
        assert_eq!(serve.subfolder, "apps/web");
        let build = find(&scripts, "build").unwrap();
        assert_eq!(build.subfolder, "", "root package.json has empty subfolder");
    }

    #[test]
    fn excluded_directories_are_not_scanned() {
        let tree = TempTree::new("exclusions");
        tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
        // A package.json buried in each excluded dir must NOT be discovered.
        for ex in SCAN_EXCLUSIONS {
            tree.write(
                &format!("{ex}/inner/package.json"),
                r#"{ "scripts": { "leak": "should-not-appear" } }"#,
            );
        }
        let scripts = discover_package_scripts(&tree.path());
        assert!(
            scripts.iter().all(|s| s.script_name != "leak"),
            "no script from an excluded directory may surface, got: {:?}",
            scripts.iter().map(|s| &s.script_name).collect::<Vec<_>>()
        );
        assert!(
            find(&scripts, "dev").is_some(),
            "the real root dev is found"
        );
    }

    #[test]
    fn empty_when_no_package_json() {
        let tree = TempTree::new("empty");
        tree.mkdir("src");
        let scripts = discover_package_scripts(&tree.path());
        assert!(
            scripts.is_empty(),
            "no package.json => empty list, no crash"
        );
    }

    #[test]
    fn unreadable_or_malformed_package_json_is_skipped_without_crash() {
        let tree = TempTree::new("malformed");
        // Malformed JSON at the root, valid one in a subfolder: the bad one is
        // skipped, the good one still surfaces (no crash, no error).
        tree.write("package.json", "{ this is not valid json ");
        tree.write("sub/package.json", r#"{ "scripts": { "ok": "echo hi" } }"#);
        let scripts = discover_package_scripts(&tree.path());
        assert_eq!(scripts.len(), 1, "only the valid package.json contributes");
        assert_eq!(scripts[0].script_name, "ok");
    }

    #[test]
    fn missing_workspace_path_yields_empty() {
        // A workspace path that does not exist must not crash; empty result.
        let scripts = discover_package_scripts("/this/path/definitely/does/not/exist/nyx_test_xyz");
        assert!(scripts.is_empty());
    }

    // --- Proposed names: unique vs collision -----------------------------

    #[test]
    fn unique_script_name_keeps_bare_name() {
        let tree = TempTree::new("unique_name");
        tree.write(
            "package.json",
            r#"{ "name": "solo", "scripts": { "dev": "vite", "build": "tsc" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        // Both names are unique across the (single) package → bare proposed names.
        assert!(find(&scripts, "dev").is_some());
        assert!(find(&scripts, "build").is_some());
    }

    #[test]
    fn colliding_script_names_get_package_prefixed_proposal() {
        let tree = TempTree::new("collide_name");
        tree.write(
            "packages/api/package.json",
            r#"{ "name": "api", "scripts": { "dev": "node ." } }"#,
        );
        tree.write(
            "packages/web/package.json",
            r#"{ "name": "web", "scripts": { "dev": "next" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        // `dev` appears in BOTH packages → each is disambiguated by the package name.
        assert!(
            find(&scripts, "api:dev").is_some(),
            "colliding dev must be proposed as api:dev, got: {:?}",
            scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
        assert!(find(&scripts, "web:dev").is_some());
        // No bare `dev` proposal remains (both collided).
        assert!(find(&scripts, "dev").is_none());
    }

    #[test]
    fn package_json_outside_workspace_via_symlink_is_refused() {
        // A symlinked subdirectory whose target (with a package.json) is OUTSIDE the
        // workspace must not contribute scripts. We never follow symlinked dirs, AND
        // the canonicalization containment check is the backstop.
        let tree = TempTree::new("symlink_escape");
        let ws = tree.mkdir("workspace");
        let ws_str = ws.to_string_lossy().into_owned();
        // An outside package.json.
        tree.write(
            "outside/package.json",
            r#"{ "scripts": { "leak": "should-not-appear" } }"#,
        );
        #[cfg(unix)]
        {
            let outside = tree.root.join("outside");
            std::os::unix::fs::symlink(&outside, ws.join("linked")).expect("symlink");
        }
        let scripts = discover_package_scripts(&ws_str);
        assert!(
            scripts.iter().all(|s| s.script_name != "leak"),
            "a package.json reached only via a symlink outside the workspace must be refused"
        );
    }

    // --- R-IMPORT #1: filtered, monorepo-aware discovery -----------------

    #[test]
    fn monorepo_without_root_manifest_finds_sub_package_scripts() {
        // The palbank shape: NO root package.json, several sub-packages under apps/ and
        // packages/. The recursive (but filtered) walk must still find their scripts.
        let tree = TempTree::new("no_root_manifest");
        tree.mkdir("apps");
        tree.write(
            "apps/web/package.json",
            r#"{ "name": "web", "scripts": { "dev": "next dev" } }"#,
        );
        tree.write(
            "apps/api/package.json",
            r#"{ "name": "api", "scripts": { "start": "node ." } }"#,
        );
        tree.write(
            "packages/ui/package.json",
            r#"{ "name": "ui", "scripts": { "build": "tsup" } }"#,
        );
        let result = discover_scripts(&tree.path());
        assert_eq!(
            result.manifests_found, 3,
            "all 3 sub-package manifests found despite no root package.json"
        );
        assert!(find(&result.scripts, "dev").is_some(), "web dev found");
        assert!(find(&result.scripts, "start").is_some(), "api start found");
        assert!(find(&result.scripts, "build").is_some(), "ui build found");
    }

    #[test]
    fn hidden_dotdirs_are_excluded_from_discovery() {
        // A package.json buried in ANY hidden dotdir (.agents, .config, …) must NOT be
        // discovered — the blanket dotdir rule, beyond the explicit SCAN_EXCLUSIONS.
        let tree = TempTree::new("dotdirs");
        tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
        for dot in [".agents", ".config", ".vscode", ".husky"] {
            tree.write(
                &format!("{dot}/package.json"),
                r#"{ "scripts": { "leak": "should-not-appear" } }"#,
            );
        }
        let scripts = discover_package_scripts(&tree.path());
        assert!(
            scripts.iter().all(|s| s.script_name != "leak"),
            "no script from a hidden dotdir may surface, got: {:?}",
            scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
        assert!(find(&scripts, "dev").is_some(), "the real root dev is found");
    }

    #[test]
    fn node_modules_is_excluded_from_discovery() {
        // node_modules (the heavy vendor dir) must never be descended into, even when
        // it holds thousands of dependency package.json files.
        let tree = TempTree::new("node_modules");
        tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
        tree.write(
            "node_modules/some-dep/package.json",
            r#"{ "scripts": { "leak": "should-not-appear" } }"#,
        );
        tree.write(
            "packages/api/node_modules/nested-dep/package.json",
            r#"{ "scripts": { "leak2": "should-not-appear" } }"#,
        );
        tree.write(
            "packages/api/package.json",
            r#"{ "name": "api", "scripts": { "start": "node ." } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        assert!(
            scripts.iter().all(|s| !s.script_name.starts_with("leak")),
            "no script from node_modules may surface, got: {:?}",
            scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
        assert!(find(&scripts, "start").is_some(), "the real api package is found");
    }

    #[test]
    fn gitignored_directories_are_excluded_from_discovery() {
        // A directory matched by the workspace .gitignore must not be scanned: the
        // dogfood case of a vendored / generated tree the repo ignores.
        let tree = TempTree::new("gitignored");
        tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
        tree.write(".gitignore", "vendored/\n/generated\nfixtures\n");
        // gitignored dirs holding manifests that must NOT surface.
        tree.write(
            "vendored/dep/package.json",
            r#"{ "scripts": { "leak_vendored": "x" } }"#,
        );
        tree.write(
            "generated/package.json",
            r#"{ "scripts": { "leak_generated": "x" } }"#,
        );
        tree.write(
            "packages/api/fixtures/package.json",
            r#"{ "scripts": { "leak_fixtures": "x" } }"#,
        );
        // a NON-ignored real package that must surface.
        tree.write(
            "packages/api/package.json",
            r#"{ "name": "api", "scripts": { "start": "node ." } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        assert!(
            scripts.iter().all(|s| !s.script_name.starts_with("leak_")),
            "no script from a gitignored dir may surface, got: {:?}",
            scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
        assert!(find(&scripts, "dev").is_some(), "root dev surfaces");
        assert!(find(&scripts, "start").is_some(), "non-ignored api surfaces");
    }

    #[test]
    fn gitignored_nested_git_repo_is_still_discovered_repo_of_repos() {
        // PRD-4.1 #2 (the PalBank repo-of-repos layout): an umbrella parent gitignores its
        // nested sub-repos, but those sub-repos hold the real package.json files. A
        // gitignored sub-DIR that itself contains `.git` must still be discovered, while
        // dotdirs (.agents), node_modules, and gitignored NON-repo dirs stay excluded.
        let tree = TempTree::new("repo_of_repos");
        // The umbrella gitignores its sub-repos AND a plain vendored dir.
        tree.write(".gitignore", "sub-repo/\nvendored/\n");
        // A nested GIT REPO (has .git) that the parent gitignores → MUST surface.
        tree.mkdir("sub-repo/.git"); // a real repo has a .git directory
        tree.write(
            "sub-repo/package.json",
            r#"{ "name": "sub", "scripts": { "build_subrepo": "tsc" } }"#,
        );
        // A nested git repo using the worktree/submodule `.git` FILE form → MUST surface too.
        tree.write("submodule/.git", "gitdir: ../.git/modules/submodule\n");
        tree.write(
            "submodule/package.json",
            r#"{ "name": "submod", "scripts": { "build_submodule": "tsc" } }"#,
        );
        // A gitignored NON-repo dir (no .git) → still excluded.
        tree.write(
            "vendored/package.json",
            r#"{ "scripts": { "leak_vendored": "x" } }"#,
        );
        // A dotdir (.agents) holding a manifest → still excluded (even though it has no .git).
        tree.write(
            ".agents/package.json",
            r#"{ "scripts": { "leak_agents": "x" } }"#,
        );
        // node_modules inside the discovered sub-repo → still excluded.
        tree.write(
            "sub-repo/node_modules/dep/package.json",
            r#"{ "scripts": { "leak_nm": "x" } }"#,
        );

        let result = discover_scripts(&tree.path());
        assert!(
            find(&result.scripts, "build_subrepo").is_some(),
            "the gitignored nested .git-DIR sub-repo is discovered, got: {:?}",
            result.scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
        assert!(
            find(&result.scripts, "build_submodule").is_some(),
            "the gitignored nested .git-FILE sub-repo is discovered too"
        );
        assert!(
            result.scripts.iter().all(|s| !s.script_name.starts_with("leak_")),
            "no script from .agents / node_modules / a gitignored non-repo may surface, got: {:?}",
            result.scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn gitignored_subrepo_rerooting_surfaces_nested_packages_but_honors_its_own_gitignore() {
        // PRD-4.1 #2, finding #10: when a gitignored sub-repo is scanned via the override,
        // the gitignore scope RE-ROOTS at the sub-repo. A broad umbrella rule (`sub-repo/`)
        // would otherwise still match `sub-repo/packages/…` and silently drop the sub-repo's
        // NESTED packages; after re-rooting only the sub-repo's OWN .gitignore governs its tree.
        let tree = TempTree::new("subrepo_reroot");
        // The umbrella gitignores the whole sub-repo (a rule that also matches everything under it).
        tree.write(".gitignore", "sub-repo/\n");
        tree.mkdir("sub-repo/.git");
        tree.write(
            "sub-repo/package.json",
            r#"{ "name": "sub", "scripts": { "build_root": "tsc" } }"#,
        );
        // A NESTED package inside the sub-repo: must surface now that scope re-roots (it would
        // be dropped if the umbrella's `sub-repo/` rule still applied inside the sub-repo).
        tree.write(
            "sub-repo/packages/inner/package.json",
            r#"{ "name": "inner", "scripts": { "build_nested": "tsc" } }"#,
        );
        // The sub-repo's OWN .gitignore must STILL be honored after re-rooting.
        tree.write("sub-repo/.gitignore", "private/\n");
        tree.write(
            "sub-repo/private/package.json",
            r#"{ "scripts": { "leak_private": "x" } }"#,
        );

        let result = discover_scripts(&tree.path());
        let names: Vec<&String> = result.scripts.iter().map(|s| &s.script_name).collect();
        assert!(
            find(&result.scripts, "build_root").is_some(),
            "the sub-repo root package surfaces, got: {names:?}"
        );
        assert!(
            find(&result.scripts, "build_nested").is_some(),
            "the sub-repo's NESTED package surfaces after re-rooting, got: {names:?}"
        );
        assert!(
            result.scripts.iter().all(|s| s.script_name != "leak_private"),
            "the sub-repo's OWN .gitignore (`private/`) is still honored, got: {names:?}"
        );
    }

    #[test]
    fn gitignored_git_repo_below_the_top_level_stays_excluded() {
        // Arbitration B: the repo-of-repos override only un-ignores the workspace's DIRECT
        // children. A gitignored `.git`-dir buried DEEPER (a vendored clone, a tool cache,
        // a checked-out dependency) stays excluded — we don't surface arbitrarily nested
        // repos the user deliberately gitignored.
        let tree = TempTree::new("deep_vendored_repo");
        tree.write(".gitignore", "vendored-clone/\n");
        // A normal (non-ignored) nested dir at the top level is scanned...
        tree.write("outer/package.json", r#"{ "scripts": { "build_outer": "tsc" } }"#);
        // ...but a gitignored dir that is ITSELF a git repo, nested at depth 2, is NOT.
        tree.mkdir("outer/vendored-clone/.git");
        tree.write(
            "outer/vendored-clone/package.json",
            r#"{ "scripts": { "leak_deep_clone": "x" } }"#,
        );

        let result = discover_scripts(&tree.path());
        let names: Vec<&String> = result.scripts.iter().map(|s| &s.script_name).collect();
        assert!(
            find(&result.scripts, "build_outer").is_some(),
            "the non-ignored nested dir is still scanned, got: {names:?}"
        );
        assert!(
            result.scripts.iter().all(|s| s.script_name != "leak_deep_clone"),
            "a gitignored .git-dir below the top level stays excluded, got: {names:?}"
        );
    }

    #[test]
    fn npm_workspaces_manifest_bounds_discovery_to_declared_globs() {
        // With a root `workspaces` declaration, ONLY the root + glob-matching packages
        // contribute — a stray package.json outside the declared globs is ignored even
        // though it is not in an excluded/ignored dir.
        let tree = TempTree::new("npm_workspaces");
        tree.write(
            "package.json",
            r#"{ "name": "root", "workspaces": ["apps/*", "packages/*"], "scripts": { "lint": "eslint" } }"#,
        );
        tree.write(
            "apps/web/package.json",
            r#"{ "name": "web", "scripts": { "dev": "next" } }"#,
        );
        tree.write(
            "packages/ui/package.json",
            r#"{ "name": "ui", "scripts": { "build": "tsup" } }"#,
        );
        // OUTSIDE the declared globs: must NOT be imported (a tool/script dir, an example).
        tree.write(
            "examples/demo/package.json",
            r#"{ "scripts": { "leak_example": "x" } }"#,
        );
        let result = discover_scripts(&tree.path());
        assert!(find(&result.scripts, "lint").is_some(), "root lint found");
        assert!(find(&result.scripts, "dev").is_some(), "apps/web dev found");
        assert!(find(&result.scripts, "build").is_some(), "packages/ui build found");
        assert!(
            result.scripts.iter().all(|s| s.script_name != "leak_example"),
            "a package outside the declared workspace globs must NOT be imported, got: {:?}",
            result.scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
        // root + apps/web + packages/ui = 3 manifests (examples/demo excluded).
        assert_eq!(result.manifests_found, 3);
    }

    #[test]
    fn npm_workspaces_object_form_packages_key() {
        // The object form `"workspaces": { "packages": [...] }` (yarn) is honored too.
        let tree = TempTree::new("npm_workspaces_obj");
        tree.write(
            "package.json",
            r#"{ "workspaces": { "packages": ["modules/*"] }, "scripts": { "ci": "turbo run" } }"#,
        );
        tree.write(
            "modules/core/package.json",
            r#"{ "name": "core", "scripts": { "test": "vitest" } }"#,
        );
        tree.write(
            "other/package.json",
            r#"{ "scripts": { "leak_other": "x" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        assert!(find(&scripts, "ci").is_some(), "root ci found");
        assert!(find(&scripts, "test").is_some(), "modules/core test found");
        assert!(
            scripts.iter().all(|s| s.script_name != "leak_other"),
            "a package outside the declared globs must NOT surface"
        );
    }

    #[test]
    fn pnpm_workspace_yaml_bounds_discovery() {
        // pnpm declares its workspace globs in pnpm-workspace.yaml, not package.json.
        let tree = TempTree::new("pnpm_workspace");
        tree.write(
            "package.json",
            r#"{ "name": "root", "scripts": { "release": "changeset" } }"#,
        );
        tree.write(
            "pnpm-workspace.yaml",
            "packages:\n  - 'apps/*'\n  - \"packages/*\"\n",
        );
        tree.write(
            "apps/web/package.json",
            r#"{ "name": "web", "scripts": { "dev": "next" } }"#,
        );
        tree.write(
            "packages/ui/package.json",
            r#"{ "name": "ui", "scripts": { "build": "tsup" } }"#,
        );
        tree.write(
            "scratch/package.json",
            r#"{ "scripts": { "leak_scratch": "x" } }"#,
        );
        let result = discover_scripts(&tree.path());
        assert!(find(&result.scripts, "release").is_some(), "root release found");
        assert!(find(&result.scripts, "dev").is_some(), "apps/web dev found");
        assert!(find(&result.scripts, "build").is_some(), "packages/ui build found");
        assert!(
            result.scripts.iter().all(|s| s.script_name != "leak_scratch"),
            "a package outside the pnpm workspace globs must NOT surface"
        );
        assert_eq!(result.manifests_found, 3);
    }

    #[test]
    fn unbounded_walk_respects_max_scan_depth() {
        // Without a workspaces manifest the walk is bounded by MAX_SCAN_DEPTH; a manifest
        // deeper than that is NOT discovered.
        let tree = TempTree::new("depth_bound");
        tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
        // Build a path deeper than MAX_SCAN_DEPTH (4): a/b/c/d/e/package.json (depth 5).
        tree.write(
            "a/b/c/d/e/package.json",
            r#"{ "scripts": { "too_deep": "x" } }"#,
        );
        // And one just within the bound (depth 2): a/b/package.json.
        tree.write(
            "a/b/package.json",
            r#"{ "name": "shallow", "scripts": { "shallow_ok": "x" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        assert!(
            scripts.iter().all(|s| s.script_name != "too_deep"),
            "a manifest deeper than MAX_SCAN_DEPTH must NOT surface in the unbounded walk"
        );
        assert!(find(&scripts, "shallow_ok").is_some(), "a within-bound manifest surfaces");
    }

    #[test]
    fn gitignore_negation_reincludes_a_sibling_dir() {
        // `!` negation re-includes a sibling that an earlier wildcard ignored — git's
        // last-rule-wins semantics, applied where the parent dir itself is NOT excluded
        // (git cannot re-include under an excluded parent, which we honor by pruning the
        // walk at the ignored parent; see node_modules/gitignored-dir tests).
        let tree = TempTree::new("gitignore_negate");
        tree.write("package.json", r#"{ "scripts": { "dev": "vite" } }"#);
        // Ignore every `tmp-*` dir at the root, but re-include `tmp-keep`.
        tree.write(".gitignore", "tmp-*\n!tmp-keep\n");
        tree.write(
            "tmp-drop/package.json",
            r#"{ "scripts": { "leak_drop": "x" } }"#,
        );
        tree.write(
            "tmp-keep/package.json",
            r#"{ "name": "keep", "scripts": { "kept": "x" } }"#,
        );
        let scripts = discover_package_scripts(&tree.path());
        assert!(
            scripts.iter().all(|s| s.script_name != "leak_drop"),
            "the ignored tmp-drop must NOT surface"
        );
        assert!(
            find(&scripts, "kept").is_some(),
            "the re-included tmp-keep must surface, got: {:?}",
            scripts.iter().map(|s| &s.proposed_name).collect::<Vec<_>>()
        );
    }

    // --- R-IMPORT #3: manifest count summary -----------------------------

    #[test]
    fn discovery_reports_zero_manifests_when_none_exist() {
        // The no-manifest case: an explicit manifests_found:0, distinct from a manifest
        // with no scripts (which still counts? no — no usable scripts => not retained).
        let tree = TempTree::new("no_manifest_summary");
        tree.mkdir("src");
        let result = discover_scripts(&tree.path());
        assert_eq!(result.manifests_found, 0, "no package.json => manifests_found 0");
        assert!(result.scripts.is_empty());
    }

    // --- import_command: storage + collision blocking --------------------

    fn pkg_source(path: &str, script: &str, snapshot: &str, pm: &str) -> CommandSource {
        CommandSource {
            source_kind: Some(SOURCE_KIND_PACKAGE_JSON.to_string()),
            source_package_json_path: Some(path.to_string()),
            source_script_name: Some(script.to_string()),
            source_script_command_snapshot: Some(snapshot.to_string()),
            package_manager: Some(pm.to_string()),
        }
    }

    #[test]
    fn import_stores_all_four_source_fields() {
        let mut conn = open_in_memory();
        let (project, _root) = db::create_project(&mut conn, "p", "/tmp/p", None).expect("project");

        let created = import_command(
            &mut conn,
            &project.id,
            "dev",
            "pnpm dev",
            "packages/api",
            pkg_source(
                "/tmp/p/packages/api/package.json",
                "dev",
                "vite --host",
                "pnpm",
            ),
        )
        .expect("import succeeds");

        assert_eq!(created.name, "dev");
        assert_eq!(created.command, "pnpm dev");
        assert_eq!(created.subfolder.as_deref(), Some("packages/api"));
        // The four source fields + source_kind are persisted.
        assert_eq!(created.source_kind.as_deref(), Some("package_json"));
        assert_eq!(
            created.source_package_json_path.as_deref(),
            Some("/tmp/p/packages/api/package.json")
        );
        assert_eq!(created.source_script_name.as_deref(), Some("dev"));
        assert_eq!(
            created.source_script_command_snapshot.as_deref(),
            Some("vite --host")
        );
        assert_eq!(created.package_manager.as_deref(), Some("pnpm"));
    }

    #[test]
    fn import_refuses_a_name_already_used_in_the_project() {
        let mut conn = open_in_memory();
        let (project, _root) = db::create_project(&mut conn, "p", "/tmp/p", None).expect("project");
        // A hand-authored command already named "dev".
        db::create_template(
            &mut conn,
            &project.id,
            "dev",
            "echo hi",
            None,
            Default::default(),
        )
        .expect("seed dev");

        // Importing another "dev" must be refused with a clear message, and create
        // nothing.
        let err = import_command(
            &mut conn,
            &project.id,
            "dev",
            "pnpm dev",
            "",
            pkg_source("/tmp/p/package.json", "dev", "vite", "pnpm"),
        )
        .expect_err("colliding import must be refused");
        assert!(
            err.contains("already used"),
            "the error must clearly state the name is taken, got: {err}"
        );
        // Still exactly one template named dev.
        let templates = db::list_templates(&mut conn, &project.id).unwrap();
        assert_eq!(templates.iter().filter(|t| t.name == "dev").count(), 1);
    }

    #[test]
    fn import_allows_edited_unique_name_and_custom_command() {
        let mut conn = open_in_memory();
        let (project, _root) = db::create_project(&mut conn, "p", "/tmp/p", None).expect("project");
        db::create_template(
            &mut conn,
            &project.id,
            "dev",
            "echo hi",
            None,
            Default::default(),
        )
        .expect("seed dev");

        // The user renames the collision to a unique name and edits the command.
        let created = import_command(
            &mut conn,
            &project.id,
            "api-dev",
            "pnpm --filter api dev",
            "packages/api",
            pkg_source("/tmp/p/packages/api/package.json", "dev", "vite", "pnpm"),
        )
        .expect("unique edited import succeeds");
        assert_eq!(created.name, "api-dev");
        assert_eq!(
            created.command, "pnpm --filter api dev",
            "the (edited) command is stored as given, not the default runner"
        );
        // Source script name still records the ORIGINAL script (dev), not the name.
        assert_eq!(created.source_script_name.as_deref(), Some("dev"));
    }

    #[test]
    fn import_rejects_empty_name() {
        let mut conn = open_in_memory();
        let (project, _root) = db::create_project(&mut conn, "p", "/tmp/p", None).expect("project");
        let err = import_command(
            &mut conn,
            &project.id,
            "  ",
            "pnpm dev",
            "",
            Default::default(),
        )
        .expect_err("empty name must be rejected");
        assert!(err.contains("name is required"));
    }
}
