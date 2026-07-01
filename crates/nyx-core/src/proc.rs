//! Linux `/proc` introspection: a terminal's LIVE working directory and the
//! name of its foreground program — without OSC7 (which lands in PRD 2).
//!
//! Two facts, two anchors:
//! - **cwd** is `readlink /proc/<shell_pid>/cwd`. The kernel tracks the shell's
//!   real cwd, so this reflects every `cd` the user typed, instantly.
//! - **foreground program** is `tcgetpgrp(master)` → `/proc/<pgid>/comm`. The
//!   controlling-terminal foreground process group leader is the running
//!   program (htop/vim) at the prompt it is the shell itself. `portable-pty`
//!   exposes the `tcgetpgrp` as `MasterPty::process_group_leader`.
//!
//! The pure parsers ([`parse_comm`], [`clean_cwd`]) are split out so the parsing
//! logic is unit-tested independently of a live pid; the IO wrappers
//! ([`read_cwd`], [`read_foreground_comm`]) are the thin `/proc` layer on top.
//!
//! Cost control: each lookup is two cheap syscalls (a readlink + a small read).
//! Callers MUST poll this on a bounded cadence (~1s), never per output byte —
//! see [`crate::bridge`] for the debounced command and the documented contract.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

/// Normalize the target of `readlink /proc/<pid>/cwd`.
///
/// The kernel can append a ` (deleted)` suffix when the directory was removed
/// out from under the process; we strip it so the path stays usable. An empty
/// result is treated as "unknown" (`None`).
pub fn clean_cwd(raw: &str) -> Option<String> {
    let trimmed = raw.strip_suffix(" (deleted)").unwrap_or(raw).trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse the contents of `/proc/<pid>/comm`.
///
/// `comm` is the program's command name (truncated by the kernel to 15 bytes +
/// a trailing newline). We strip the newline and reject an empty name. The
/// 15-byte truncation is inherent to `comm`; callers that need the full name
/// would read `/proc/<pid>/cmdline` instead (not needed for auto-naming v1).
pub fn parse_comm(raw: &str) -> Option<String> {
    let name = raw.trim_end_matches(['\n', '\r']);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Read the live cwd of `pid` via `readlink /proc/<pid>/cwd`.
///
/// Returns `None` if the process is gone, we lack permission, or the link is
/// empty. The returned path is the directory the process is CURRENTLY in (every
/// `cd` is reflected immediately by the kernel).
pub fn read_cwd(pid: u32) -> Option<String> {
    let link = PathBuf::from(format!("/proc/{pid}/cwd"));
    let target = std::fs::read_link(&link).ok()?;
    clean_cwd(&target.to_string_lossy())
}

/// Read the foreground program name from `/proc/<pgid>/comm`.
///
/// `pgid` is the foreground process group leader (from `tcgetpgrp` on the PTY
/// master). The group leader's `comm` is the running program's name — `htop`
/// when htop runs, the shell name (`bash`/`zsh`) at the prompt.
pub fn read_foreground_comm(pgid: i32) -> Option<String> {
    if pgid <= 0 {
        return None;
    }
    let path = format!("/proc/{pgid}/comm");
    let raw = std::fs::read_to_string(&path).ok()?;
    parse_comm(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Pure parser tests (no live pid required) -------------------------

    #[test]
    fn clean_cwd_strips_deleted_suffix_and_trailing_ws() {
        assert_eq!(clean_cwd("/home/kris/work"), Some("/home/kris/work".into()));
        assert_eq!(
            clean_cwd("/tmp/gone (deleted)"),
            Some("/tmp/gone".into()),
            "the kernel's ' (deleted)' marker must be stripped"
        );
        assert_eq!(clean_cwd("/x\n"), Some("/x".into()));
        assert_eq!(clean_cwd(""), None);
        assert_eq!(clean_cwd("   "), None);
    }

    #[test]
    fn parse_comm_strips_newline_and_rejects_empty() {
        assert_eq!(parse_comm("htop\n"), Some("htop".into()));
        assert_eq!(parse_comm("bash\n"), Some("bash".into()));
        // No trailing newline (defensive): still parsed.
        assert_eq!(parse_comm("zsh"), Some("zsh".into()));
        assert_eq!(parse_comm("\n"), None);
        assert_eq!(parse_comm(""), None);
    }

    // --- Real IO tests against the test process itself --------------------
    //
    // The test binary is a live Linux process, so we can read OUR OWN /proc
    // entries: a true, deterministic exercise of the IO path with a guaranteed
    // pid, no shell spawning required at this layer.

    #[test]
    fn read_cwd_of_self_matches_current_dir() {
        let me = std::process::id();
        let got = read_cwd(me).expect("reading our own /proc/<pid>/cwd must succeed");
        let expected = std::env::current_dir().unwrap();
        // Compare canonicalized paths to absorb symlinks in the prefix.
        let got_canon = std::fs::canonicalize(&got).unwrap();
        let exp_canon = std::fs::canonicalize(&expected).unwrap();
        assert_eq!(
            got_canon, exp_canon,
            "live cwd from /proc must equal the process current_dir"
        );
    }

    #[test]
    fn read_cwd_reflects_a_chdir() {
        // Reading our own cwd before and after a chdir must change — this is the
        // exact "after a cd, the live cwd is the new folder" property, exercised
        // through the real readlink path. (current_dir is process-global; we
        // restore it so we don't disturb sibling tests.)
        let original = std::env::current_dir().unwrap();
        let me = std::process::id();
        let tmp = std::env::temp_dir();
        std::env::set_current_dir(&tmp).unwrap();
        let after = read_cwd(me).expect("read_cwd after chdir");
        std::env::set_current_dir(&original).unwrap();

        let after_canon = std::fs::canonicalize(&after).unwrap();
        let tmp_canon = std::fs::canonicalize(&tmp).unwrap();
        assert_eq!(
            after_canon, tmp_canon,
            "live cwd must reflect a chdir (the cd-then-observe property)"
        );
    }

    #[test]
    fn read_foreground_comm_of_self_is_nonempty() {
        // Our own process group leader's comm is readable and non-empty. This
        // exercises the /proc/<pgid>/comm read path with a guaranteed-live pgid.
        let my_pgid = unsafe { libc::getpgrp() } as i32;
        let comm =
            read_foreground_comm(my_pgid).expect("reading our own process-group comm must succeed");
        assert!(!comm.is_empty(), "comm must be a non-empty program name");
    }

    #[test]
    fn read_foreground_comm_rejects_nonpositive_pgid() {
        assert_eq!(read_foreground_comm(0), None);
        assert_eq!(read_foreground_comm(-1), None);
    }
}
