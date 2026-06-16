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

/// Directories never descended into during a scan: package vendors and build
/// outputs. Scanning them is wasteful and can surface thousands of irrelevant
/// nested `package.json` files (every dependency ships one). Matched by exact
/// directory name (case-sensitive on Unix; this is the conventional spelling).
pub const SCAN_EXCLUSIONS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "target",
    ".next",
    ".turbo",
    ".cache",
    "coverage",
];

/// How deep the scan descends from the workspace root. A generous bound that
/// covers root + monorepo packages (`packages/api`, `apps/web/sub`, …) while
/// keeping a pathological tree from being walked forever.
const MAX_SCAN_DEPTH: usize = 8;

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

/// Discover the package.json scripts under `workspace_path`, grouped by location,
/// each with an editable proposed name and a default runner command.
///
/// Walks the workspace tree (root + subfolders) up to [`MAX_SCAN_DEPTH`], skipping
/// the [`SCAN_EXCLUSIONS`] directories. Every retained `package.json` is
/// canonicalized and re-checked to be inside the workspace; a file that escapes
/// (e.g. via a symlink) is dropped. Unreadable / unparsable files are skipped, so a
/// workspace with no readable package.json yields an EMPTY list (never an error).
///
/// Proposed names: a script name unique across the whole result keeps its bare name
/// (`dev`); a name appearing in several packages is disambiguated as
/// `<package-or-folder>:<script>` (`api:dev`).
pub fn discover_package_scripts(workspace_path: &str) -> Vec<DiscoveredScript> {
    let workspace_norm = pathnorm::normalize(workspace_path);
    // Canonicalize the workspace once so containment is checked against the
    // symlink-resolved root. If the workspace itself is inaccessible, there is
    // nothing to scan.
    let Ok(canon_ws) = std::fs::canonicalize(Path::new(workspace_path)) else {
        return Vec::new();
    };
    let canon_ws_norm = pathnorm::normalize(&canon_ws.to_string_lossy());

    let mut files: Vec<PackageFile> = Vec::new();
    collect_package_files(&canon_ws, &canon_ws_norm, &workspace_norm, 0, &mut files);

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
    out
}

/// Recursively collect retained `package.json` files under `dir`, honoring the
/// exclusion list and depth bound. `canon_ws_norm` is the canonical workspace root
/// (for containment), `workspace_norm` the stored workspace path (for the
/// human-facing root label / subfolder base).
fn collect_package_files(
    dir: &Path,
    canon_ws_norm: &str,
    workspace_norm: &str,
    depth: usize,
    out: &mut Vec<PackageFile>,
) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }

    // A package.json directly in this directory?
    let pkg = dir.join("package.json");
    if pkg.is_file() {
        if let Some(file) = read_package_file(&pkg, dir, canon_ws_norm, workspace_norm) {
            out.push(file);
        }
    }

    // Descend into child directories, skipping the exclusions.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
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
        if SCAN_EXCLUSIONS.contains(&name) {
            continue;
        }
        collect_package_files(&path, canon_ws_norm, workspace_norm, depth + 1, out);
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
