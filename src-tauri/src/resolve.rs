//! Auto-attach resolver: map a terminal's live cwd to a KNOWN workspace, and
//! decide the resulting binding under the hybrid auto/manual rule.
//!
//! Two layers, both pure (no IO, no DB), so they unit-test in isolation:
//!
//! 1. [`CwdProvider`] — the platform-agnostic source of a terminal's current
//!    working directory. Linux uses `/proc`; Windows/macOS use OSC 7. Both feed
//!    a NORMALIZED cwd into the same resolver, so nothing above this layer is
//!    platform-specific. If no reliable cwd is available the provider yields
//!    `None` and the resolver makes NO change (never guesses).
//!
//! 2. [`resolve_workspace`] / [`decide_attachment`] — the matching + decision.
//!    Matching is ONLY against already-known workspaces (auto-attach creates
//!    nothing). The LONGEST canonical ancestor wins for nested workspaces.
//!    A terminal in `manual` mode is never moved by the cwd; in `auto` mode it
//!    follows the resolved match, and stays put when there is no match.
//!
//! The bridge wires the live `/proc`/OSC7 cwd + the workspace list from the DB
//! into these functions, then persists the decided attachment via the Task-#1
//! `attach_terminal`/`detach_terminal` DB functions.

use crate::pathnorm;

/// The platform-agnostic source of a terminal's live cwd. Linux reads `/proc`;
/// other platforms ride OSC 7. The variant carries the RAW cwd (pre-normalize);
/// [`CwdProvider::normalized_cwd`] applies [`pathnorm::normalize`] uniformly so
/// every provider's output is compared in the same canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdProvider {
    /// Linux `/proc/<pid>/cwd` reading (raw path string). `None` = the read
    /// failed (process gone / permission) ⇒ no reliable cwd. Constructed only on
    /// Linux builds (the bridge picks the provider per platform), so it reads as
    /// dead code on Windows/macOS — allowed.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Proc(Option<String>),
    /// A cwd parsed from an OSC 7 `file://` payload (raw, host-stripped path).
    /// `None` = no OSC 7 seen yet ⇒ no reliable cwd. The portable source for
    /// Windows/macOS (and a possible fallback elsewhere). Constructed only on
    /// non-Linux builds (Linux uses `Proc`), so it reads as dead code on Linux —
    /// allowed (mirrors `Proc`'s non-Linux allow above).
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Osc7(Option<String>),
    /// No cwd source available on this platform/terminal. Explicit degradation:
    /// the resolver must not guess.
    None,
}

impl CwdProvider {
    /// The normalized cwd this provider currently reports, or `None` when no
    /// reliable cwd is available (the explicit "do not guess" signal).
    pub fn normalized_cwd(&self) -> Option<String> {
        let raw = match self {
            CwdProvider::Proc(c) | CwdProvider::Osc7(c) => c.as_deref(),
            CwdProvider::None => None,
        }?;
        let norm = pathnorm::normalize(raw);
        if norm.is_empty() {
            None
        } else {
            Some(norm)
        }
    }
}

/// A minimal view of a known workspace for matching: its id and canonical path.
/// (The bridge maps `db::Workspace` rows into these.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMatch {
    pub id: String,
    /// The workspace's stored CANONICAL path (already normalized at insert).
    pub path: String,
}

/// Find the KNOWN workspace whose canonical path is the longest ancestor-or-equal
/// of `cwd`. Returns the matching workspace id, or `None` when no known workspace
/// contains the cwd (no match ⇒ NOTHING is created, the caller leaves the
/// attachment unchanged). `cwd` MUST already be normalized.
///
/// "Longest ancestor wins": for nested workspaces `/p` and `/p/feat`, a cwd of
/// `/p/feat/src` resolves to `/p/feat` (the deeper, more specific match), never
/// the shallower `/p`. Ties on depth are broken by id for determinism (they
/// cannot share a path within a project; across projects an arbitrary-but-stable
/// pick is acceptable and never invents data).
pub fn resolve_workspace<'a>(
    cwd: &str,
    workspaces: impl IntoIterator<Item = &'a WorkspaceMatch>,
) -> Option<String> {
    let mut best: Option<&WorkspaceMatch> = None;
    let mut best_depth = 0usize;
    for ws in workspaces {
        if pathnorm::is_ancestor_or_equal(&ws.path, cwd) {
            let d = pathnorm::depth(&ws.path);
            let take = match best {
                None => true,
                Some(_) if d > best_depth => true,
                // Deterministic tiebreak on equal depth.
                Some(b) if d == best_depth && ws.id < b.id => true,
                _ => false,
            };
            if take {
                best = Some(ws);
                best_depth = d;
            }
        }
    }
    best.map(|w| w.id.clone())
}

/// The current binding of a terminal, as the decision input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentBinding {
    /// The workspace the terminal is attached to now (`None` = unattached).
    pub workspace_id: Option<String>,
    /// Whether the binding follows the cwd (`auto`) or is pinned (`manual`).
    pub mode: BindingMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingMode {
    Auto,
    Manual,
}

/// What auto-attach decides to do with a terminal's binding. The bridge turns
/// this into the right DB call (`attach_terminal(.., auto)` / no-op).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// Attach (in auto mode) to this workspace id — it changed from current.
    AttachAuto(String),
    /// Leave the binding exactly as it is (no reliable cwd, no match, manual
    /// pin, or already attached to the resolved workspace).
    Unchanged,
}

/// Decide a terminal's new attachment from its current binding, the resolved cwd
/// (already normalized; `None` = no reliable cwd), and the set of known
/// workspaces. The HYBRID rule, in full:
///
/// - `manual` mode ⇒ always [`Attachment::Unchanged`] (a `cd` never moves a
///   pinned terminal until it is unpinned).
/// - no reliable cwd ⇒ [`Attachment::Unchanged`] (do NOT guess; keep current).
/// - `auto` mode with a cwd:
///   - matches a known workspace different from the current one ⇒
///     [`Attachment::AttachAuto`] of that workspace.
///   - matches the workspace already attached ⇒ [`Attachment::Unchanged`].
///   - no known workspace matches ⇒ [`Attachment::Unchanged`] (creates nothing,
///     keeps the current attachment — which may be `None`).
pub fn decide_attachment<'a>(
    current: &CurrentBinding,
    cwd: Option<&str>,
    workspaces: impl IntoIterator<Item = &'a WorkspaceMatch>,
) -> Attachment {
    // A pinned terminal is immovable by cwd.
    if current.mode == BindingMode::Manual {
        return Attachment::Unchanged;
    }
    // No reliable cwd ⇒ never guess.
    let Some(cwd) = cwd else {
        return Attachment::Unchanged;
    };
    match resolve_workspace(cwd, workspaces) {
        // A different workspace matched ⇒ move (in auto mode).
        Some(ws) if current.workspace_id.as_deref() != Some(ws.as_str()) => {
            Attachment::AttachAuto(ws)
        }
        // Already attached to the matched workspace, or no match ⇒ keep current.
        _ => Attachment::Unchanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, path: &str) -> WorkspaceMatch {
        WorkspaceMatch {
            id: id.to_string(),
            // Store the canonical form, as the DB would.
            path: pathnorm::normalize(path),
        }
    }

    fn norm(p: &str) -> String {
        pathnorm::normalize(p)
    }

    // Platform-appropriate path literals so the canonical forms line up.
    #[cfg(windows)]
    const P_ROOT: &str = "C:\\proj";
    #[cfg(windows)]
    const P_FEAT: &str = "C:\\proj\\feat";
    #[cfg(windows)]
    const P_CWD_IN_FEAT: &str = "C:\\proj\\feat\\src";
    #[cfg(windows)]
    const P_CWD_IN_ROOT: &str = "C:\\proj\\docs";
    #[cfg(windows)]
    const P_OUTSIDE: &str = "D:\\elsewhere";

    #[cfg(not(windows))]
    const P_ROOT: &str = "/proj";
    #[cfg(not(windows))]
    const P_FEAT: &str = "/proj/feat";
    #[cfg(not(windows))]
    const P_CWD_IN_FEAT: &str = "/proj/feat/src";
    #[cfg(not(windows))]
    const P_CWD_IN_ROOT: &str = "/proj/docs";
    #[cfg(not(windows))]
    const P_OUTSIDE: &str = "/elsewhere";

    #[test]
    fn no_reliable_cwd_yields_none_and_changes_nothing() {
        assert_eq!(CwdProvider::None.normalized_cwd(), None);
        assert_eq!(CwdProvider::Proc(None).normalized_cwd(), None);
        assert_eq!(CwdProvider::Osc7(None).normalized_cwd(), None);

        let current = CurrentBinding {
            workspace_id: Some("w-root".into()),
            mode: BindingMode::Auto,
        };
        let known = [ws("w-root", P_ROOT)];
        let decision = decide_attachment(&current, None, &known);
        assert_eq!(
            decision,
            Attachment::Unchanged,
            "no cwd ⇒ keep the current attachment (no guessing)"
        );
    }

    #[test]
    fn provider_normalizes_raw_cwd() {
        // The provider applies pathnorm so /proc and OSC7 outputs are comparable.
        let p = CwdProvider::Osc7(Some(P_CWD_IN_FEAT.to_string()));
        assert_eq!(p.normalized_cwd(), Some(norm(P_CWD_IN_FEAT)));
    }

    #[test]
    fn matches_only_known_workspaces_no_match_creates_nothing() {
        let known = [ws("w-root", P_ROOT)];
        // A cwd OUTSIDE every known workspace ⇒ no match.
        assert_eq!(resolve_workspace(&norm(P_OUTSIDE), &known), None);
        let current = CurrentBinding {
            workspace_id: None,
            mode: BindingMode::Auto,
        };
        assert_eq!(
            decide_attachment(&current, Some(&norm(P_OUTSIDE)), &known),
            Attachment::Unchanged,
            "no known workspace contains the cwd ⇒ nothing attached, nothing created"
        );
    }

    #[test]
    fn longest_canonical_ancestor_wins_for_nested_workspaces() {
        // Both /proj and /proj/feat are known; a cwd inside feat must pick feat.
        let known = [ws("w-root", P_ROOT), ws("w-feat", P_FEAT)];
        assert_eq!(
            resolve_workspace(&norm(P_CWD_IN_FEAT), &known),
            Some("w-feat".to_string()),
            "the deeper (longer) ancestor wins"
        );
        // A cwd under the root but NOT under feat picks the root.
        assert_eq!(
            resolve_workspace(&norm(P_CWD_IN_ROOT), &known),
            Some("w-root".to_string())
        );
    }

    #[test]
    fn naive_string_prefix_does_not_match_sibling_with_shared_prefix() {
        // GUARD (ZE0 done-criterion): matching MUST use an ancestor RELATION, not a
        // naive string prefix. A cwd in `/foo/bar-baz` must NOT resolve to a
        // workspace at `/foo/bar` — `bar-baz` shares the textual prefix `bar` but is
        // a SIBLING directory, not a descendant. A `starts_with` implementation
        // would wrongly match here and fail this test.
        #[cfg(windows)]
        let (ws_path, sibling_cwd) = ("C:\\foo\\bar", "C:\\foo\\bar-baz\\src");
        #[cfg(not(windows))]
        let (ws_path, sibling_cwd) = ("/foo/bar", "/foo/bar-baz/src");

        let known = [ws("w-bar", ws_path)];
        assert_eq!(
            resolve_workspace(&norm(sibling_cwd), &known),
            None,
            "a sibling sharing only a textual prefix (`bar-baz` vs `bar`) must NOT match"
        );

        // The genuine descendant DOES match (proves the guard is not vacuously
        // rejecting everything).
        #[cfg(windows)]
        let real_child = "C:\\foo\\bar\\baz";
        #[cfg(not(windows))]
        let real_child = "/foo/bar/baz";
        assert_eq!(
            resolve_workspace(&norm(real_child), &known),
            Some("w-bar".to_string()),
            "a true descendant of the workspace path must match"
        );
    }

    #[test]
    fn auto_attach_never_invents_a_workspace_for_an_unmatched_cwd() {
        // GUARD (ZE0 done-criterion): auto-attach creates NOTHING. The resolver only
        // ever returns the id of an ALREADY-KNOWN workspace or `None`; it has no path
        // by which to mint a new workspace/project. With an EMPTY known-set, every
        // cwd resolves to `None`, and the decision keeps the (unattached) binding —
        // no invented attachment, no created workspace.
        let empty: [WorkspaceMatch; 0] = [];
        assert_eq!(
            resolve_workspace(&norm(P_CWD_IN_FEAT), &empty),
            None,
            "with no known workspaces nothing can match (nothing is created)"
        );
        let current = CurrentBinding {
            workspace_id: None,
            mode: BindingMode::Auto,
        };
        assert_eq!(
            decide_attachment(&current, Some(&norm(P_CWD_IN_FEAT)), &empty),
            Attachment::Unchanged,
            "an unmatched cwd against an empty known-set invents no attachment"
        );
    }

    #[test]
    fn auto_mode_follows_resolved_cwd() {
        let known = [ws("w-root", P_ROOT), ws("w-feat", P_FEAT)];
        // Currently attached to root, in auto; cd into feat ⇒ attach to feat.
        let current = CurrentBinding {
            workspace_id: Some("w-root".into()),
            mode: BindingMode::Auto,
        };
        assert_eq!(
            decide_attachment(&current, Some(&norm(P_CWD_IN_FEAT)), &known),
            Attachment::AttachAuto("w-feat".to_string())
        );
    }

    #[test]
    fn auto_mode_no_change_when_already_on_the_match() {
        let known = [ws("w-feat", P_FEAT)];
        let current = CurrentBinding {
            workspace_id: Some("w-feat".into()),
            mode: BindingMode::Auto,
        };
        assert_eq!(
            decide_attachment(&current, Some(&norm(P_CWD_IN_FEAT)), &known),
            Attachment::Unchanged,
            "already attached to the resolved workspace ⇒ no redundant move"
        );
    }

    #[test]
    fn manual_pin_is_immovable_then_auto_resumes_after_unpin() {
        let known = [ws("w-root", P_ROOT), ws("w-feat", P_FEAT)];
        // Pinned to root (manual); a cd into feat must NOT move it.
        let pinned = CurrentBinding {
            workspace_id: Some("w-root".into()),
            mode: BindingMode::Manual,
        };
        assert_eq!(
            decide_attachment(&pinned, Some(&norm(P_CWD_IN_FEAT)), &known),
            Attachment::Unchanged,
            "a manual pin is not moved by a cd elsewhere"
        );
        // After unpin (mode flips to auto, workspace kept), the SAME cwd now
        // resolves to feat ⇒ auto mode resumes and moves it.
        let unpinned = CurrentBinding {
            workspace_id: Some("w-root".into()),
            mode: BindingMode::Auto,
        };
        assert_eq!(
            decide_attachment(&unpinned, Some(&norm(P_CWD_IN_FEAT)), &known),
            Attachment::AttachAuto("w-feat".to_string()),
            "after unpin, auto mode follows the cwd again"
        );
    }
}
