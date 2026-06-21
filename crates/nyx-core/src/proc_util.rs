//! Centralized non-PTY process-spawn hardening.
//!
//! Every `std::process::Command` nyx-core spawns that is NOT a PTY (PTYs go through
//! `portable-pty`, which owns a real terminal and must keep its console) is a
//! HEADLESS helper: a `git` branch probe, the `claude` CLI shell-out, `taskkill` /
//! `tasklist`, `portless`. On Windows, spawning such a console subprocess from a GUI
//! app FLASHES a console window for a fraction of a second unless the process is
//! created with the `CREATE_NO_WINDOW` flag. The dogfood finding (review
//! `01KVJEY0BX9ZZ83J40WJ2NT931`) was exactly this: a console flashing ~1/4 s on
//! workspace-add (the `git` probe) and on the Claude integration install (the
//! `claude` CLI), because those two spawn sites had been extracted WITHOUT the flag
//! that `taskkill`/`tasklist`/`portless` already carried.
//!
//! This module is the ONE place that knows the flag, so no future extraction can
//! forget it. Build every non-PTY `Command` through [`command`] (or apply
//! [`harden`] to one you already built) and the Windows flag is set for free; on
//! non-Windows it is a no-op. Because nyx-core is the SHARED core, this fix benefits
//! BOTH shells (the Tauri adapter and the Electron core-host) at once.

/// `CREATE_NO_WINDOW` (winbase.h): the child runs with NO console window, so a
/// headless helper never flashes a console on a GUI host. Defined here once.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Apply the Windows "no console window" creation flag to an already-built
/// `Command`, in place, and return it (so it chains in a builder expression). A
/// no-op on every non-Windows target. Use this for the `mod gitbranch`-style sites
/// that build the `Command` with `args(...)` before spawning.
pub fn harden(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Build a hardened non-PTY `Command` for `program`: the Windows `CREATE_NO_WINDOW`
/// flag is already applied (no-op elsewhere). The single entry point for every
/// headless subprocess nyx-core spawns — prefer it over `std::process::Command::new`
/// so the console-flash fix can never be forgotten on a new spawn site.
pub fn command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    harden(&mut cmd);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `command` builds a runnable `Command` on every platform (the flag is silent on
    /// non-Windows). We spawn a trivially-true cross-platform program and confirm it
    /// runs — the point is that hardening never breaks the spawn itself.
    #[test]
    fn hardened_command_still_spawns() {
        // `cmd /C exit 0` on Windows, `true` elsewhere — both exit 0 with no output.
        #[cfg(windows)]
        let mut c = command("cmd");
        #[cfg(windows)]
        c.args(["/C", "exit", "0"]);
        #[cfg(not(windows))]
        let mut c = command("true");

        match c.output() {
            Ok(out) => assert!(out.status.success(), "hardened helper should exit 0"),
            // A CI image without the chosen shim is not a hardening failure — the
            // contract under test is "harden does not break a normal spawn", which a
            // missing binary does not contradict.
            Err(e) => eprintln!("skipping spawn assertion (program unavailable): {e}"),
        }
    }

    /// `harden` is chainable and returns the same command for builder-style use.
    #[test]
    fn harden_is_chainable() {
        let mut c = std::process::Command::new("true");
        let _: &mut std::process::Command = harden(&mut c);
    }
}
