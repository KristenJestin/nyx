//! THE command line that does X on THIS platform — the one vehicle every test uses to arm a
//! managed command.
//!
//! ## Why this module exists
//!
//! A test that arms a gate is not testing `sh`. It is testing what nyx does with a command that
//! SUCCEEDS, one that FAILS with a given code, one that PRINTS something, one that floods, one that
//! never returns, one that reads an environment variable. The command line is the VEHICLE; the
//! assertion is the subject. Every one of those vehicles was nonetheless written as POSIX — `true`,
//! `false`, `test -f gate_ok`, `sh -c 'echo hello; exit 0'`.
//!
//! nyx runs a managed command as `<platform shell> <flag> <cmdline>`
//! ([`crate::command::command_invocation`], over [`crate::pty::resolve_shell`]). On Windows that
//! shell is PowerShell, which knows neither `sh` nor `true` nor `test`. So the line never resolved,
//! and the tests split into two equally useless halves: the ones expecting GREEN went red (the 18
//! frozen in `BASELINE-REDS.md`), and the ones expecting RED stayed "green" **for the wrong
//! reason** — `The term 'sh' is not recognized` is a non-zero exit like any other, and it even
//! carries the whole command line in its error text, which is how an assertion as specific as
//! "the output tail still holds `TAIL_SENTINEL`" passed while nothing had run.
//!
//! ## The measurement trap this module also closes
//!
//! Launched from **Git Bash**, `cargo test` inherits a `PATH` carrying Git's `usr\bin`, where
//! `sh.exe`, `true.exe`, `false.exe` and `test.exe` all live — so the platform shell (still
//! PowerShell) resolves them and the whole family turns green. Launched from **PowerShell**, that
//! directory is absent and the family is red. Same commit, same machine, two baselines. Asking
//! [`crate::pty::resolve_shell`] which interpreter will really get the line removes the question:
//! the line is written for that interpreter and needs nothing else on `PATH`.
//!
//! **This is not a workaround for a product limitation.** nyx already picks the platform shell and
//! its flag correctly, and nothing in production ever writes `sh` or `true`. Enriching the `PATH`
//! nyx hands a managed command would be the actual regression: it is exactly the trap
//! `command.rs`'s N31 tests guard — green inside nyx, red by hand.
//!
//! ## What it does NOT do
//!
//! It never changes a verdict. `exit 2` stays `exit 2` on all three shells; a gate that was red
//! stays red, for the reason it claims. Only the spelling moves.

use crate::command::shell_stem;
use crate::pty::resolve_shell;

/// The FAMILY of the shell that will really interpret the line.
///
/// Keyed on [`crate::pty::resolve_shell`] — the SAME resolution [`crate::command::CommandPty::spawn`]
/// performs — so a test never guesses which interpreter gets its line. Three families, because
/// [`crate::command::command_invocation`] already knows exactly three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shell {
    /// `bash`/`sh`/`zsh`/… — `-c`.
    Posix,
    /// `pwsh`/`powershell` — `-Command`. The Windows default: [`crate::pty::resolve_shell`] reaches
    /// [`Shell::Cmd`] only on a machine carrying neither PowerShell edition.
    PowerShell,
    /// `cmd.exe` — `/C`. The Windows last resort.
    Cmd,
}

impl Shell {
    /// The family of the shell THIS machine's managed commands run in.
    pub(crate) fn of_platform() -> Self {
        match shell_stem(&resolve_shell()).as_str() {
            "pwsh" | "powershell" => Shell::PowerShell,
            "cmd" => Shell::Cmd,
            _ => Shell::Posix,
        }
    }

    /// What joins two statements on one line.
    fn separator(self) -> &'static str {
        match self {
            // `&` chains unconditionally in cmd, and takes NO leading space: in `echo a & b` the
            // space belongs to the echoed text, so the ledger would carry a trailing blank.
            Shell::Cmd => "&",
            _ => "; ",
        }
    }
}

/// A command line under construction, in the dialect of [`Shell::of_platform`].
///
/// Built statement by statement and CLOSED by [`Cmdline::exits`], which returns the line. The
/// closing exit is mandatory by construction: a gate's whole answer is its exit code, and a builder
/// that could be forgotten open would produce a line whose verdict is whatever the last statement
/// happened to leave behind.
pub(crate) struct Cmdline {
    shell: Shell,
    parts: Vec<String>,
}

impl Cmdline {
    pub(crate) fn new() -> Self {
        Self {
            shell: Shell::of_platform(),
            parts: Vec::new(),
        }
    }

    fn push(mut self, part: String) -> Self {
        self.parts.push(part);
        self
    }

    /// Print `text` — a literal, on its own line.
    pub(crate) fn prints(self, text: &str) -> Self {
        let part = match self.shell {
            Shell::Posix => format!("echo '{text}'"),
            Shell::PowerShell => format!("Write-Output '{text}'"),
            // cmd's `echo` has no quoting: quotes around the text would be PRINTED.
            Shell::Cmd => format!("echo {text}"),
        };
        self.push(part)
    }

    /// Print `prefix`, then the VALUE of the environment variable `var` (empty when it is unset),
    /// then `suffix`.
    ///
    /// The empty-when-unset half is the point: it is what proves an env entry nyx REFUSED to pass
    /// (a malformed one) reached the command as an absence rather than as a literal. cmd is the one
    /// shell that needs a branch for it — an undefined `%VAR%` is left standing as written.
    pub(crate) fn prints_env(self, prefix: &str, var: &str, suffix: &str) -> Self {
        let part = match self.shell {
            Shell::Posix => format!("echo \"{prefix}${{{var}}}{suffix}\""),
            Shell::PowerShell => format!("Write-Output ('{prefix}' + $env:{var} + '{suffix}')"),
            Shell::Cmd => format!(
                "if not defined {var} (echo {prefix}{suffix}) else (echo {prefix}%{var}%{suffix})"
            ),
        };
        self.push(part)
    }

    /// BLOCK until `name` appears as a file in the command's cwd — the rendezvous that lets a test
    /// decide WHEN a running command produces its next output, instead of racing a sleep.
    ///
    /// Bounded (~60 s) so a test that never drops the file cannot leave a process behind. Relative
    /// on purpose: the name resolves against the command's own working directory, which is the one
    /// the test controls.
    pub(crate) fn waits_for_file(self, name: &str) -> Self {
        let part = match self.shell {
            Shell::Posix => format!(
                "i=0; while [ ! -f '{name}' ] && [ $i -lt 1200 ]; do sleep 0.05; i=$((i+1)); done"
            ),
            Shell::PowerShell => format!(
                "$i=0; while (-not (Test-Path -LiteralPath '{name}') -and $i -lt 1200) \
                 {{ Start-Sleep -Milliseconds 50; $i++ }}"
            ),
            // cmd has no sleep and no inline while: a bounded FOR whose body pings for ~1 s only
            // while the file is still absent, so it costs nothing once it appears.
            Shell::Cmd => {
                format!("for /L %i in (1,1,60) do @(if not exist {name} ping -n 2 127.0.0.1 >nul)")
            }
        };
        self.push(part)
    }

    /// Sleep `seconds` — a command that outlives the budget measuring it.
    pub(crate) fn sleeps(self, seconds: u64) -> Self {
        let part = match self.shell {
            Shell::Posix => format!("sleep {seconds}"),
            Shell::PowerShell => format!("Start-Sleep -Seconds {seconds}"),
            // cmd has no sleep; `ping` to the loopback is the idiom, and it sends its FIRST packet
            // immediately, so N+1 pings span N seconds.
            Shell::Cmd => format!("ping -n {} 127.0.0.1 >nul", seconds + 1),
        };
        self.push(part)
    }

    /// Close the line: exit with `code`.
    ///
    /// `exit <n>` is the one statement all three shells spell identically — it is shell SYNTAX, not
    /// a program name, which is precisely what `true` and `false` are not.
    pub(crate) fn exits(self, code: i32) -> String {
        let separator = self.shell.separator();
        let mut parts = self.parts;
        parts.push(format!("exit {code}"));
        parts.join(separator)
    }
}

/// A command that succeeds and prints nothing — the always-green gate.
pub(crate) fn green() -> String {
    Cmdline::new().exits(0)
}

/// A command that fails and prints nothing — the always-red gate.
pub(crate) fn red() -> String {
    Cmdline::new().exits(1)
}

/// A command that prints `text` and then exits with `code`.
pub(crate) fn prints_then_exits(text: &str, code: i32) -> String {
    Cmdline::new().prints(text).exits(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform's family is the one [`crate::command::command_invocation`] will route to —
    /// asserted through that function rather than restated, so the two can never drift.
    #[test]
    fn the_platform_family_matches_the_invocation_flag_nyx_would_use() {
        let shell = resolve_shell();
        let (_prog, args) = crate::command::command_invocation(&shell, "exit 0");
        let expected_flag = match Shell::of_platform() {
            Shell::Posix => "-c",
            Shell::PowerShell => "-Command",
            Shell::Cmd => "/C",
        };
        assert_eq!(
            args[0], expected_flag,
            "the family this module writes for must be the family nyx invokes ({shell})"
        );
    }

    /// The verdict-only lines are shell SYNTAX and identical everywhere — the property that lets
    /// `green()` / `red()` carry no branch at all.
    #[test]
    fn a_bare_verdict_is_the_same_line_on_every_shell() {
        assert_eq!(green(), "exit 0");
        assert_eq!(red(), "exit 1");
    }

    /// Statements are joined by the separator the platform's shell parses — and cmd's `&` carries
    /// no leading space, or the space would land inside the text `echo` prints.
    #[test]
    fn statements_are_joined_by_the_separator_the_platform_shell_parses() {
        let line = prints_then_exits("boom", 1);
        match Shell::of_platform() {
            Shell::Cmd => assert_eq!(line, "echo boom&exit 1"),
            Shell::PowerShell => assert_eq!(line, "Write-Output 'boom'; exit 1"),
            Shell::Posix => assert_eq!(line, "echo 'boom'; exit 1"),
        }
    }
}
