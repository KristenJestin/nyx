//! Path canonicalization for project/workspace storage AND comparison (PRD-2).
//!
//! Two facts force this to be a PURE, LEXICAL normalizer rather than
//! `std::fs::canonicalize`:
//! - **Comparison vs. existence.** Auto-attach matches a terminal's live cwd
//!   against stored workspace paths. `fs::canonicalize` requires the path to
//!   exist and (on Windows) returns a `\\?\` verbatim prefix, so it is unusable
//!   for comparing a workspace folder that may be on a different mount, deleted,
//!   or never created locally. We need a STRING canonical form that two
//!   spellings of the same directory collapse to, independent of the filesystem.
//! - **UNIQUE(project_id, path)** is enforced by SQLite on the stored string, so
//!   the backend must store the SAME canonical string a later `cd` resolves to,
//!   or the unique constraint and ancestor matching silently miss.
//!
//! What we do (lexical, deterministic, no IO):
//! - Trim surrounding whitespace.
//! - On Windows: strip a leading `\\?\` (and `\\?\UNC\`) verbatim prefix, fold
//!   ASCII case to LOWER (NTFS is case-insensitive — `C:\Foo` and `c:\foo` are
//!   the same dir), and treat `/` and `\` as equivalent separators (PowerShell
//!   and OSC7 both emit `/`-style paths).
//! - Collapse runs of separators, drop `.` components, and resolve `..`
//!   lexically against the preceding component (never above the root).
//! - Emit a single canonical separator (`\` on Windows, `/` elsewhere) and strip
//!   any trailing separator (except a bare root).
//!
//! The matching layer ([`is_ancestor_or_equal`]) then compares these canonical
//! strings on COMPONENT boundaries, so `/home/work` is an ancestor of
//! `/home/work/sub` but not of `/home/work2`.

/// The canonical in-storage separator for the current platform.
#[cfg(windows)]
const SEP: char = '\\';
#[cfg(not(windows))]
const SEP: char = '/';

/// True if `c` is any path separator we accept on input (`/` always; `\` too on
/// Windows). On Unix a backslash is a legal filename char, so it is NOT a
/// separator there.
fn is_sep(c: char) -> bool {
    #[cfg(windows)]
    {
        c == '/' || c == '\\'
    }
    #[cfg(not(windows))]
    {
        c == '/'
    }
}

/// Normalize a path to its canonical comparison/storage form. Pure and
/// IO-free — see the module docs for why we do not use `fs::canonicalize`.
///
/// The result is the form stored in `workspaces.path` and the form the
/// auto-attach resolver compares a live cwd against; both sides MUST route
/// through here so a `cd` and a stored workspace collapse to the same string.
pub fn normalize(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    #[cfg(windows)]
    {
        normalize_windows(trimmed)
    }
    #[cfg(not(windows))]
    {
        normalize_unix(trimmed)
    }
}

/// Split `s` into components on any accepted separator, dropping empty pieces
/// (collapses `//` and a trailing separator) and `.` components, resolving `..`
/// lexically against the previous popped component when one exists above the
/// root boundary `root_components`.
fn collapse_components(s: &str, root_components: usize, out: &mut Vec<String>) {
    for comp in s.split(is_sep) {
        match comp {
            "" | "." => {}
            ".." => {
                // Pop the last component, but never above the fixed root prefix
                // (drive / leading-slash). A `..` at the root is dropped.
                if out.len() > root_components {
                    out.pop();
                }
            }
            other => out.push(other.to_string()),
        }
    }
}

#[cfg(not(windows))]
fn normalize_unix(s: &str) -> String {
    let absolute = s.starts_with('/');
    let mut out: Vec<String> = Vec::new();
    collapse_components(s, 0, &mut out);
    let body = out.join("/");
    if absolute {
        format!("/{body}")
    } else if body.is_empty() {
        // A non-absolute path that collapsed to nothing (e.g. "." or "a/..").
        ".".to_string()
    } else {
        body
    }
}

#[cfg(windows)]
fn normalize_windows(s: &str) -> String {
    // Fold ASCII case (NTFS is case-insensitive) and unify separators by working
    // off a lowercased copy. Non-ASCII is left as-is (we only case-fold ASCII,
    // which covers drive letters and the vast majority of dev paths).
    let lower = s.to_ascii_lowercase();

    // Strip a verbatim/extended-length prefix: `\\?\` and `\\?\unc\`. The result
    // is owned because the UNC variant must synthesize a leading `\\`.
    let stripped_owned = strip_verbatim_prefix(&lower);
    let stripped = stripped_owned.as_str();

    // Detect the kind of absolute root so `..` cannot escape it and so the root
    // is re-emitted verbatim.
    if let Some((root, tail)) = unc_share_rest(stripped) {
        // UNC: \\server\share\... — keep `\\server\share` as the root prefix.
        let mut out: Vec<String> = Vec::new();
        collapse_components(tail, 0, &mut out);
        let body = out.join("\\");
        if body.is_empty() {
            root
        } else {
            format!("{root}\\{body}")
        }
    } else if let Some((drive, tail)) = drive_rooted(stripped) {
        // Drive-absolute: `c:\...`. The drive is the fixed root.
        let mut out: Vec<String> = Vec::new();
        collapse_components(tail, 0, &mut out);
        let body = out.join("\\");
        if body.is_empty() {
            format!("{drive}:\\")
        } else {
            format!("{drive}:\\{body}")
        }
    } else {
        // Relative or rootless: collapse and join. Preserve a leading separator
        // (drive-relative-from-root like `\foo`) as a single root slash.
        let rooted = stripped.starts_with(is_sep);
        let mut out: Vec<String> = Vec::new();
        collapse_components(stripped, 0, &mut out);
        let body = out.join("\\");
        if rooted {
            format!("{SEP}{body}")
        } else if body.is_empty() {
            ".".to_string()
        } else {
            body
        }
    }
}

/// Strip a leading `\\?\` or `\\?\unc\` (already lowercased) verbatim prefix,
/// returning an owned canonical-ish string. `\\?\unc\server\share` becomes
/// `\\server\share` (a synthetic plain-UNC form) so it normalizes downstream
/// like any other UNC path; `\\?\c:\…` becomes `c:\…`. Anything else is returned
/// unchanged.
#[cfg(windows)]
fn strip_verbatim_prefix(s: &str) -> String {
    let bytes = s.as_bytes();
    let is_sep_b = |b: u8| b == b'\\' || b == b'/';
    // `\\?\` (separators may be `/` or `\`).
    if bytes.len() >= 4
        && is_sep_b(bytes[0])
        && is_sep_b(bytes[1])
        && bytes[2] == b'?'
        && is_sep_b(bytes[3])
    {
        let after = &s[4..];
        let ab = after.as_bytes();
        // `\\?\unc\server\share` → `\\server\share` (re-add the double sep so the
        // UNC detector recognizes it).
        if ab.len() >= 4 && &ab[0..3] == b"unc" && is_sep_b(ab[3]) {
            return format!("\\\\{}", &after[4..]);
        }
        return after.to_string();
    }
    s.to_string()
}

/// If `s` is a UNC path (`\\server\share\...`), return its root prefix
/// (`\\server\share`) and the tail after it.
#[cfg(windows)]
fn unc_share_rest(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    let is_sep_b = |b: u8| b == b'\\' || b == b'/';
    if bytes.len() < 2 || !is_sep_b(bytes[0]) || !is_sep_b(bytes[1]) {
        return None;
    }
    let after = &s[2..];
    // server is up to the next separator; share is the one after.
    let mut parts = after.splitn(3, is_sep);
    let server = parts.next().unwrap_or("");
    let share = parts.next().unwrap_or("");
    if server.is_empty() || share.is_empty() {
        return None;
    }
    let tail = parts.next().unwrap_or("");
    Some((format!("\\\\{server}\\{share}"), tail))
}

/// If `s` is drive-rooted (`c:\...` or `c:/...`), return the lowercase drive
/// letter and the tail after `c:`. A bare `c:` (drive-relative, no root sep) is
/// treated as drive-rooted for our purposes (we always anchor to the drive root,
/// which is the right canonical form for an absolute workspace path).
#[cfg(windows)]
fn drive_rooted(s: &str) -> Option<(char, &str)> {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = bytes[0] as char;
        Some((drive, &s[2..]))
    } else {
        None
    }
}

/// True if `ancestor` is the same directory as `descendant` or a parent of it,
/// comparing CANONICAL paths on component boundaries. Both inputs must already
/// be [`normalize`]d. This is the matching primitive auto-attach uses: a live
/// cwd matches a workspace when the workspace path is an ancestor-or-equal of
/// the cwd; the LONGEST such match wins (see the resolver).
///
/// Component-boundary safety: `/home/work` matches `/home/work` and
/// `/home/work/sub`, but NOT `/home/work2` (which a naive `starts_with` would
/// wrongly accept).
pub fn is_ancestor_or_equal(ancestor: &str, descendant: &str) -> bool {
    if ancestor.is_empty() || descendant.is_empty() {
        return false;
    }
    if ancestor == descendant {
        return true;
    }
    // `descendant` must start with `ancestor` followed by a separator. Handle the
    // root case where `ancestor` itself ends in a separator (e.g. `c:\` or `/`).
    let sep = SEP;
    if let Some(rest) = descendant.strip_prefix(ancestor) {
        if ancestor.ends_with(sep) {
            // ancestor is a root like `/` or `c:\`: any non-empty rest is a child.
            return !rest.is_empty();
        }
        return rest.starts_with(sep);
    }
    false
}

/// The component DEPTH of a canonical path — used to pick the LONGEST (deepest)
/// ancestor when several workspaces match a cwd. More components = more specific.
/// Pure string measure over the canonical form.
pub fn depth(path: &str) -> usize {
    path.split(is_sep).filter(|c| !c.is_empty()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_normalize_to_empty() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   "), "");
    }

    #[cfg(not(windows))]
    mod unix {
        use super::*;

        #[test]
        fn collapses_redundant_separators_and_dots() {
            assert_eq!(normalize("/home//kris/./work"), "/home/kris/work");
            assert_eq!(normalize("/home/kris/work/"), "/home/kris/work");
            assert_eq!(normalize("/home/kris/work///"), "/home/kris/work");
        }

        #[test]
        fn resolves_parent_components_lexically() {
            assert_eq!(normalize("/home/kris/work/.."), "/home/kris");
            assert_eq!(normalize("/home/kris/../work"), "/home/work");
            // `..` cannot escape the root.
            assert_eq!(normalize("/.."), "/");
            assert_eq!(normalize("/../.."), "/");
        }

        #[test]
        fn root_is_preserved() {
            assert_eq!(normalize("/"), "/");
            assert_eq!(normalize("///"), "/");
        }

        #[test]
        fn case_is_significant_on_unix() {
            // Unix filesystems are case-sensitive: do NOT fold case.
            assert_ne!(normalize("/Home/Work"), normalize("/home/work"));
        }

        #[test]
        fn ancestor_matching_is_component_aware() {
            assert!(is_ancestor_or_equal("/home/work", "/home/work"));
            assert!(is_ancestor_or_equal("/home/work", "/home/work/sub/deep"));
            assert!(is_ancestor_or_equal("/", "/anything"));
            // The classic false-positive a naive starts_with would accept:
            assert!(!is_ancestor_or_equal("/home/work", "/home/work2"));
            assert!(!is_ancestor_or_equal("/home/work/sub", "/home/work"));
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        #[test]
        fn folds_case_and_unifies_separators() {
            // NTFS is case-insensitive and OSC7/PowerShell emit `/`-paths.
            assert_eq!(normalize("C:\\Users\\Kris\\Work"), "c:\\users\\kris\\work");
            assert_eq!(normalize("C:/Users/Kris/Work"), "c:\\users\\kris\\work");
            assert_eq!(
                normalize("C:\\Users\\Kris\\Work"),
                normalize("c:/users/kris/work"),
                "case + separator variants of one dir must collapse to one string"
            );
        }

        #[test]
        fn collapses_redundant_separators_and_dots() {
            assert_eq!(normalize("C:\\a\\\\b\\.\\c"), "c:\\a\\b\\c");
            assert_eq!(normalize("C:\\a\\b\\"), "c:\\a\\b");
        }

        #[test]
        fn resolves_parent_components_lexically() {
            assert_eq!(normalize("C:\\a\\b\\.."), "c:\\a");
            assert_eq!(normalize("C:\\a\\..\\b"), "c:\\b");
            // `..` cannot escape the drive root.
            assert_eq!(normalize("C:\\.."), "c:\\");
        }

        #[test]
        fn drive_root_is_preserved() {
            assert_eq!(normalize("C:\\"), "c:\\");
            assert_eq!(normalize("C:"), "c:\\");
        }

        #[test]
        fn strips_verbatim_prefix() {
            assert_eq!(normalize("\\\\?\\C:\\Users\\Kris"), "c:\\users\\kris");
        }

        #[test]
        fn unc_paths_normalize() {
            assert_eq!(
                normalize("\\\\Server\\Share\\Project\\Sub"),
                "\\\\server\\share\\project\\sub"
            );
            assert_eq!(
                normalize("//Server/Share/Project"),
                "\\\\server\\share\\project"
            );
        }

        #[test]
        fn ancestor_matching_is_component_aware() {
            assert!(is_ancestor_or_equal("c:\\work", "c:\\work"));
            assert!(is_ancestor_or_equal("c:\\work", "c:\\work\\sub\\deep"));
            assert!(is_ancestor_or_equal("c:\\", "c:\\anything"));
            assert!(!is_ancestor_or_equal("c:\\work", "c:\\work2"));
            assert!(!is_ancestor_or_equal("c:\\work\\sub", "c:\\work"));
        }
    }

    #[test]
    fn depth_counts_components() {
        #[cfg(windows)]
        {
            assert_eq!(depth("c:\\a\\b\\c"), 4); // c:, a, b, c
            assert_eq!(depth("c:\\"), 1);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(depth("/a/b/c"), 3);
            assert_eq!(depth("/"), 0);
        }
    }
}
