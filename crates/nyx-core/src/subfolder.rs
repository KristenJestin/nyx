//! Resolution + validation of a managed command's optional run SUBFOLDER (PRD-3).
//!
//! A managed command runs at `workspace.path` by default, but may carry an
//! OPTIONAL `subfolder` — a path RELATIVE to the workspace where the command
//! actually runs (e.g. a `package.json` import in a monorepo package). This module
//! turns `(workspace.path, subfolder)` into the concrete `cwd` the runner passes to
//! [`crate::command::CommandPty::spawn`], rejecting anything unsafe BEFORE a spawn:
//!
//! - **absent / empty** subfolder → cwd is `workspace.path` unchanged;
//! - **relative, valid, existing** subfolder → cwd is the join, canonicalized;
//! - **escape** (a leading `/` absolute path, a `..` that climbs above the
//!   workspace, or a symlink whose target lands outside the workspace) → rejected;
//! - **non-existent** subfolder → a clear error (the runner never spawns into a
//!   missing directory).
//!
//! The escape guard is TWO-LAYERED, mirroring the path-validation style of PRD-2:
//! 1. a LEXICAL pass via [`crate::pathnorm`] rejects an absolute subfolder and any
//!    `..` that would climb out of the workspace WITHOUT touching the filesystem
//!    (so a traversal attempt is refused even if the target does not exist);
//! 2. a FILESYSTEM pass `canonicalize`s both the workspace and the resolved
//!    subfolder and re-checks ancestry, so a SYMLINK inside the subfolder pointing
//!    outside the workspace is caught too (a lexical check alone cannot see it).
//!
//! No directory is ever created here (that is explicitly a non-goal): existence is
//! REQUIRED, not provisioned.
//!
//! Consumer: the `command_start` / `command_relaunch` `#[tauri::command]`s (PRD-3
//! Phase 3) call [`resolve_run_dir`] via the bridge's `resolve_command_and_cwd`
//! helper to compute the spawn cwd.

use std::path::{Path, PathBuf};

use crate::pathnorm;

/// The outcome of resolving a command's run directory: the absolute `cwd` the
/// spawn should use. Returned as an owned `String` (the spawn API takes `&str`).
pub type ResolvedCwd = String;

/// Resolve and validate a command's run directory from its `workspace_path` and an
/// OPTIONAL `subfolder`.
///
/// `workspace_path` is the workspace's stored path (already normalized by PRD-2,
/// and expected to EXIST — it is the folder the workspace points at). `subfolder`
/// is the template's optional relative run path (`None`/empty = run at the
/// workspace root).
///
/// Returns the absolute directory to run in, or an `Err(String)` describing why the
/// subfolder was refused — surfaced to the user BEFORE any process is spawned:
/// - an ABSOLUTE subfolder, or one whose `..` climbs above the workspace
///   (path-traversal), is refused;
/// - a subfolder that does not exist (or is not a directory) is refused;
/// - a subfolder whose canonical (symlink-resolved) target falls OUTSIDE the
///   workspace is refused.
pub fn resolve_run_dir(
    workspace_path: &str,
    subfolder: Option<&str>,
) -> Result<ResolvedCwd, String> {
    // No subfolder (absent or whitespace-only): run at the workspace root verbatim.
    let raw = match subfolder.map(str::trim) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(workspace_path.to_string()),
    };

    // --- Layer 1: lexical guard (no IO) ----------------------------------
    //
    // Reject an ABSOLUTE subfolder outright: an absolute path is never "relative to
    // the workspace" and would escape it by definition. We test absoluteness on the
    // RAW input (not a normalized-in-isolation form), because normalizing a relative
    // path alone resolves away leading `..` (`normalize("..") == "."`), which would
    // hide a traversal — the join below is where `..` must take effect.
    if is_absolute_raw(raw) {
        return Err(format!(
            "subfolder '{raw}' must be relative to the workspace, not an absolute path"
        ));
    }

    // Join the RAW subfolder onto the workspace, THEN normalize the whole: this is
    // what lets a `..` climb against the workspace's own tail. If the joined-
    // normalized path is no longer a descendant-or-equal of the workspace, the
    // subfolder escaped. This catches `..`/`../..` traversal with no filesystem
    // access, so a non-existent escaping target is still refused.
    let workspace_norm = pathnorm::normalize(workspace_path);
    let joined = join_norm(&workspace_norm, raw);
    if !pathnorm::is_ancestor_or_equal(&workspace_norm, &joined) {
        return Err(format!(
            "subfolder '{raw}' escapes the workspace (path traversal is not allowed)"
        ));
    }

    // --- Layer 2: filesystem guard (existence + symlink escape) ----------
    //
    // The lexical path is safe; now require it to EXIST and re-verify it cannot
    // escape via a SYMLINK. We canonicalize both sides (resolving symlinks) and
    // re-check ancestry on the canonical, normalized forms. `canonicalize` fails if
    // the path does not exist, which gives us the "missing subfolder" rejection for
    // free, BEFORE any spawn.
    let joined_path = Path::new(&joined);
    let canon_sub = std::fs::canonicalize(joined_path).map_err(|e| {
        format!("subfolder '{raw}' does not exist or is not accessible under the workspace: {e}")
    })?;
    if !canon_sub.is_dir() {
        return Err(format!("subfolder '{raw}' is not a directory"));
    }

    // The workspace itself must canonicalize too (it is the folder it points at);
    // if it cannot, we cannot prove containment, so refuse rather than guess.
    let canon_ws = std::fs::canonicalize(Path::new(workspace_path)).map_err(|e| {
        format!("workspace path '{workspace_path}' is not accessible to resolve the subfolder: {e}")
    })?;

    // Normalize the canonical forms into the SAME canonical string space the
    // ancestor check compares on (so `\\?\` prefixes / case folding on Windows are
    // handled), then re-verify containment after symlink resolution.
    let canon_ws_norm = pathnorm::normalize(&canon_ws.to_string_lossy());
    let canon_sub_norm = pathnorm::normalize(&canon_sub.to_string_lossy());
    if !pathnorm::is_ancestor_or_equal(&canon_ws_norm, &canon_sub_norm) {
        return Err(format!(
            "subfolder '{raw}' resolves (via a symlink) outside the workspace and is not allowed"
        ));
    }

    // Use the canonical, symlink-resolved directory as the spawn cwd: it is the
    // real folder, proven inside the workspace. On Windows `canonicalize` returns
    // an extended-length `\\?\C:\…` (verbatim) path; strip that prefix so the cwd we
    // hand `portable-pty`/the shell is a plain path. A verbatim cwd is rejected by
    // `cmd.exe` (and surprises some tooling), which on the real Windows run made an
    // otherwise-valid subfolder command fail to spawn.
    Ok(strip_verbatim_prefix(&canon_sub.to_string_lossy()))
}

/// Resolve a command's run directory for DISPLAY (the info bar's "working
/// directory"), best-effort and INFALLIBLE: it never errors and never blocks the
/// listing on a missing/escaping subfolder.
///
/// - When [`resolve_run_dir`] succeeds (a valid, existing, in-bounds subfolder),
///   return its canonical cwd — exactly what a spawn would use, so the bar shows the
///   real run directory.
/// - Otherwise (absent IS handled by the success path; a missing or unsafe subfolder
///   would make the spawn-time resolver error) fall back to the LEXICAL join of the
///   normalized workspace path + the raw subfolder. The display still reflects where
///   the command is configured to run, even if that folder does not exist yet —
///   surfacing the configured target is more useful here than hiding it.
///
/// This is a read-only helper for the instance listing; the SPAWN path keeps using
/// the strict, fallible [`resolve_run_dir`] so an invalid subfolder is refused
/// BEFORE any process starts.
pub fn resolve_run_dir_lossy(workspace_path: &str, subfolder: Option<&str>) -> String {
    if let Ok(cwd) = resolve_run_dir(workspace_path, subfolder) {
        return cwd;
    }
    // Spawn-time resolution refused it (missing dir, etc.). Show the configured
    // target lexically: workspace + (trimmed, non-empty) subfolder, normalized.
    let workspace_norm = pathnorm::normalize(workspace_path);
    match subfolder.map(str::trim) {
        Some(s) if !s.is_empty() => join_norm(&workspace_norm, s),
        _ => workspace_norm,
    }
}

/// Strip a leading Windows extended-length / verbatim prefix (`\\?\` and the UNC
/// form `\\?\UNC\server\share`) from a path string, returning a plain path. A no-op
/// on non-Windows (and on any path without the prefix). `canonicalize` yields these
/// verbatim paths on Windows, but a verbatim cwd is not accepted everywhere (notably
/// `cmd.exe`), so the spawn cwd must be de-verbatimized.
fn strip_verbatim_prefix(path: &str) -> String {
    #[cfg(windows)]
    {
        // `\\?\UNC\server\share\…` → `\\server\share\…`
        if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        // `\\?\C:\…` → `C:\…`
        if let Some(rest) = path.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
        path.to_string()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// True if a RAW subfolder string is ABSOLUTE (rooted), and therefore not a legal
/// workspace-relative subfolder. On Unix that is a leading `/`; on Windows a drive
/// root (`c:\…`/`c:/…`), a UNC root (`\\server\share`), or a leading separator
/// (drive-relative-from-root). Operates on the raw input so the check is independent
/// of normalization (which would otherwise resolve a relative path's leading `..`).
fn is_absolute_raw(raw: &str) -> bool {
    let raw = raw.trim();
    #[cfg(windows)]
    {
        let bytes = raw.as_bytes();
        // Drive-rooted `c:…` (we always anchor a drive spec to its root).
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return true;
        }
        // UNC or a leading separator (either slash kind).
        raw.starts_with('\\') || raw.starts_with('/')
    }
    #[cfg(not(windows))]
    {
        raw.starts_with('/')
    }
}

/// Join a normalized `base` with a normalized RELATIVE `rel`, returning the
/// normalized result. Uses [`PathBuf`] to join with the platform separator, then
/// re-normalizes so the result is in the same canonical string space the ancestor
/// check uses.
fn join_norm(base: &str, rel: &str) -> String {
    let joined: PathBuf = Path::new(base).join(rel);
    pathnorm::normalize(&joined.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a unique temp directory tree for a test, returned as its canonical
    /// path string. Cleaned up by the caller via [`TempTree`].
    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(tag: &str) -> Self {
            let mut root = std::env::temp_dir();
            // Unique per test + process + nanos so parallel/serial runs never clash.
            let uniq = format!(
                "nyx_subfolder_{}_{}_{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            root.push(uniq);
            std::fs::create_dir_all(&root).expect("create temp root");
            // Canonicalize so the stored "workspace path" matches what
            // `canonicalize` yields inside the resolver (temp dirs are often
            // symlinks, e.g. /tmp → /private/tmp on macOS).
            let root = std::fs::canonicalize(&root).expect("canonicalize temp root");
            TempTree { root }
        }

        fn path(&self) -> String {
            self.root.to_string_lossy().into_owned()
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let p = self.root.join(rel);
            std::fs::create_dir_all(&p).expect("mkdir sub");
            p
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn absent_subfolder_is_workspace_path() {
        let tree = TempTree::new("absent");
        let ws = tree.path();
        // None and empty/whitespace all collapse to the workspace path verbatim.
        assert_eq!(resolve_run_dir(&ws, None).unwrap(), ws);
        assert_eq!(resolve_run_dir(&ws, Some("")).unwrap(), ws);
        assert_eq!(resolve_run_dir(&ws, Some("   ")).unwrap(), ws);
    }

    #[test]
    fn valid_relative_existing_subfolder_resolves_to_join() {
        let tree = TempTree::new("valid");
        let ws = tree.path();
        let sub = tree.mkdir("packages/api");
        let canon_sub = std::fs::canonicalize(&sub).unwrap();

        let got = resolve_run_dir(&ws, Some("packages/api")).expect("valid subfolder resolves");
        assert_eq!(
            std::fs::canonicalize(&got).unwrap(),
            canon_sub,
            "a valid existing relative subfolder must resolve to the joined directory"
        );
        // And the resolved cwd is a real, inside-the-workspace directory.
        assert!(Path::new(&got).is_dir());
    }

    #[test]
    fn nested_dot_segments_that_stay_inside_are_allowed() {
        let tree = TempTree::new("dots_inside");
        let ws = tree.path();
        let sub = tree.mkdir("a/b");
        let canon_sub = std::fs::canonicalize(&sub).unwrap();
        // `a/x/../b` stays inside the workspace (climbs back to `a` then into `b`).
        tree.mkdir("a/x");
        let got = resolve_run_dir(&ws, Some("a/x/../b")).expect("in-bounds .. allowed");
        assert_eq!(std::fs::canonicalize(&got).unwrap(), canon_sub);
    }

    #[test]
    fn parent_escape_is_rejected_even_if_target_exists() {
        let tree = TempTree::new("escape_parent");
        // The workspace is a CHILD dir; `..` climbs to its parent (which exists),
        // but that is outside the workspace and must be refused — proving the
        // lexical guard fires regardless of the target existing.
        let ws_dir = tree.mkdir("project");
        let ws = ws_dir.to_string_lossy().into_owned();
        let err = resolve_run_dir(&ws, Some("..")).expect_err("`..` must be rejected");
        assert!(
            err.contains("escapes the workspace") || err.contains("traversal"),
            "the error must explain the traversal refusal, got: {err}"
        );

        // A deeper climb that lands on a real sibling is still refused.
        tree.mkdir("sibling");
        let err2 = resolve_run_dir(&ws, Some("../sibling"))
            .expect_err("`../sibling` must be rejected even though it exists");
        assert!(err2.contains("escapes the workspace") || err2.contains("traversal"));
    }

    #[test]
    fn absolute_subfolder_is_rejected() {
        let tree = TempTree::new("absolute");
        let ws = tree.path();
        #[cfg(not(windows))]
        let abs = "/etc";
        #[cfg(windows)]
        let abs = "C:\\Windows";
        let err = resolve_run_dir(&ws, Some(abs)).expect_err("absolute subfolder must be rejected");
        assert!(
            err.contains("absolute") || err.contains("relative"),
            "the error must explain the absolute-path refusal, got: {err}"
        );
    }

    #[test]
    fn nonexistent_subfolder_is_a_clear_error() {
        let tree = TempTree::new("missing");
        let ws = tree.path();
        let err = resolve_run_dir(&ws, Some("does/not/exist"))
            .expect_err("a missing subfolder must be refused before spawn");
        assert!(
            err.contains("does not exist") || err.contains("not accessible"),
            "the error must clearly state the subfolder is missing, got: {err}"
        );
    }

    #[test]
    fn a_file_subfolder_is_rejected_as_not_a_directory() {
        let tree = TempTree::new("file");
        let ws = tree.path();
        let file = tree.root.join("not_a_dir.txt");
        std::fs::write(&file, b"x").unwrap();
        let err = resolve_run_dir(&ws, Some("not_a_dir.txt"))
            .expect_err("a file (not a dir) subfolder must be refused");
        assert!(
            err.contains("not a directory"),
            "the error must say the target is not a directory, got: {err}"
        );
    }

    /// A symlink INSIDE the workspace whose target is OUTSIDE must be refused — the
    /// lexical guard cannot see it (the link name has no `..`), so layer 2's
    /// canonicalize-then-recheck is what catches it. Unix-only (symlink API).
    #[test]
    #[cfg(unix)]
    fn symlink_escape_is_rejected() {
        let tree = TempTree::new("symlink");
        let ws = tree.mkdir("workspace");
        let ws_str = ws.to_string_lossy().into_owned();
        // An outside directory and a symlink inside the workspace pointing at it.
        let outside = tree.mkdir("outside_secret");
        let link = ws.join("escape");
        std::os::unix::fs::symlink(&outside, &link).expect("create escaping symlink");

        let err = resolve_run_dir(&ws_str, Some("escape"))
            .expect_err("a symlink that escapes the workspace must be refused");
        assert!(
            err.contains("symlink") || err.contains("outside"),
            "the error must explain the symlink escape, got: {err}"
        );
    }

    /// The resolved spawn cwd must NOT carry a `\\?\` verbatim prefix on Windows:
    /// `canonicalize` returns one, but a verbatim cwd is rejected by `cmd.exe` (and
    /// is the determinable cause of a subfolder command failing to spawn on the real
    /// Windows run). The resolver must hand back a plain path. Windows-only.
    #[test]
    #[cfg(windows)]
    fn resolved_subfolder_cwd_has_no_verbatim_prefix() {
        let tree = TempTree::new("no_verbatim");
        let ws = tree.path();
        tree.mkdir("packages/api");
        let got = resolve_run_dir(&ws, Some("packages/api")).expect("valid subfolder resolves");
        assert!(
            !got.starts_with(r"\\?\"),
            "the spawn cwd must not be an extended-length \\\\?\\ path, got: {got}"
        );
        // It still points at the real, existing directory.
        assert!(
            Path::new(&got).is_dir(),
            "resolved cwd must be a real dir, got: {got}"
        );
    }

    #[test]
    #[cfg(windows)]
    fn strip_verbatim_prefix_dewraps_drive_and_unc() {
        assert_eq!(strip_verbatim_prefix(r"\\?\C:\foo\bar"), r"C:\foo\bar");
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\server\share\dir"),
            r"\\server\share\dir"
        );
        // A plain path is returned unchanged.
        assert_eq!(strip_verbatim_prefix(r"C:\foo"), r"C:\foo");
    }

    /// A symlink inside the workspace pointing to another dir INSIDE the workspace
    /// is allowed (containment holds after resolution). Unix-only.
    #[test]
    #[cfg(unix)]
    fn symlink_inside_workspace_is_allowed() {
        let tree = TempTree::new("symlink_ok");
        let ws = tree.mkdir("workspace");
        let ws_str = ws.to_string_lossy().into_owned();
        let real = tree.mkdir("workspace/real_target");
        let link = ws.join("alias");
        std::os::unix::fs::symlink(&real, &link).expect("create in-bounds symlink");

        let got = resolve_run_dir(&ws_str, Some("alias")).expect("in-bounds symlink allowed");
        assert_eq!(
            std::fs::canonicalize(&got).unwrap(),
            std::fs::canonicalize(&real).unwrap()
        );
    }
}
