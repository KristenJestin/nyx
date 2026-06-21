//! Shell-integration INJECTION for the OSC 133 exec-state pipeline (PRD-2.1,
//! task #5). The PARSER lives in [`crate::osc133`]; THIS module produces the
//! minimal per-shell glue that makes a spawned shell EMIT the `133;C` / `133;D`
//! command-lifecycle sequences the parser consumes.
//!
//! # Strategy (ADR-0002, documented in `osc133.rs`)
//!
//! nyx OWNS the shell it spawns ([`crate::pty`] via `resolve_shell`), so it
//! injects the integration ITSELF rather than relying on the user having
//! configured shell integration. The injection is **non-destructive**: it sources
//! the user's real rc/profile FIRST, then APPENDS the hooks — it never edits the
//! user's `~/.bashrc` / `~/.zshrc` / PowerShell profile on disk. The glue lives in
//! a per-spawn TEMP file that nyx writes; the shell is pointed at it via spawn
//! ARGS / ENV only.
//!
//! Per shell (the validated shapes from the gate, ADR-0002):
//! - **bash** — spawn `bash --rcfile <tmp> -i`. The temp rc sources the user's
//!   `~/.bashrc` first, then appends a `PROMPT_COMMAND` (precmd → `133;D;$?` then
//!   `133;A`) and a `DEBUG` trap (preexec → `133;C`). VS Code's exact bash shape.
//! - **zsh** — point `ZDOTDIR` at a temp dir whose `.zshrc` sources the user's
//!   real `.zshrc` then registers `precmd`/`preexec` hook functions (zsh's
//!   first-class hooks — cleaner than a DEBUG trap). `precmd` reads `$?`.
//! - **PowerShell** (pwsh 7 AND Windows PowerShell 5.1) — spawn
//!   `<pwsh> -NoExit -Command ". '<tmp.ps1>'"`. The snippet WRAPS the existing
//!   `prompt` function: it emits `133;D;<code>` (code from `$?`→0 else
//!   `$LASTEXITCODE`) + `133;A` at the top, calls the original prompt, then emits
//!   `133;B`. `133;C` is emitted from a `PSReadLine` command-accepted handler.
//!
//! # Fallback / explicit degradation
//!
//! Any other resolved shell (`sh`, `cmd.exe`, fish, nushell, …) is
//! [`Shell::Unsupported`] → nyx injects NOTHING. With no `133;C`/`133;D` in the
//! stream the exec-state machine never transitions, so the terminal stays
//! `idle`-only: no false `running`, no fake success/error (honest degradation,
//! per the PRD). Likewise, if WRITING the temp snippet fails, [`build`] returns an
//! empty plan and the shell is spawned PLAIN — the integration is bypassed rather
//! than failing the spawn (criterion: "can be disabled/bypassed internally if it
//! causes a startup failure").
//!
//! # Purity / testability
//!
//! Shell CLASSIFICATION ([`classify`]) and SNIPPET GENERATION (`*_snippet`) are
//! pure string functions, unit-tested without spawning a shell. [`build`] is the
//! only IO (it writes the temp file); it is exercised by the integration tests and
//! falls back to a plain spawn on any IO error.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Env var to force-disable shell-integration injection at runtime (any value).
/// A safety hatch independent of the per-spawn IO fallback: `NYX_SHELL_INTEGRATION=0`
/// (or `off`/`false`) makes [`build`] return an empty plan so every shell is
/// spawned plain. Honors the "can be disabled internally" criterion explicitly.
const DISABLE_ENV: &str = "NYX_SHELL_INTEGRATION";

/// The shells nyx can instrument for OSC 133, classified from the resolved shell
/// path/name. Anything not bash/zsh/PowerShell is [`Shell::Unsupported`] and gets
/// NO injection (idle-only degradation).
// `PowerShell` ends with "Shell" (the enum name) but it is the product's real
// name; `Pwsh` would be less clear and `Zsh`/`Bash` already share the "shell"
// concept without the suffix. Suppress the lint for this deliberate naming.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    /// pwsh 7+ (`pwsh`) and Windows PowerShell 5.1 (`powershell`) — same
    /// `prompt`-function mechanism.
    PowerShell,
    /// `sh`, `cmd.exe`, fish, nushell, an unknown shell, … → inject nothing.
    Unsupported,
}

/// Classify a resolved shell PATH or bare name into a [`Shell`]. Matches on the
/// lowercased file stem (so `/usr/bin/bash`, `bash`, `C:\…\pwsh.exe`,
/// `powershell.exe` all classify correctly), with `.exe` stripped. A login-shell
/// `-bash` (leading dash, as `$SHELL` is sometimes spelled) is handled too.
///
/// IMPORTANT: `sh` is deliberately NOT bash — many systems symlink `/bin/sh` to
/// dash, which has no DEBUG trap; treating it as bash would inject hooks that
/// silently do nothing or error. `sh` is Unsupported (idle-only), per the PRD.
pub fn classify(shell: &str) -> Shell {
    // Take the final path component (handle both / and \ separators), trim a
    // leading login-shell dash, drop a trailing `.exe`, lowercase.
    let base = shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .trim_start_matches('-');
    let stem = base
        .strip_suffix(".exe")
        .unwrap_or(base)
        .to_ascii_lowercase();
    match stem.as_str() {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "pwsh" | "powershell" => Shell::PowerShell,
        _ => Shell::Unsupported,
    }
}

/// A spawn-time integration plan: extra ARGS to append after the program, extra
/// ENV vars to set, and any temp file PATHS created (returned so a caller could
/// schedule cleanup; nyx leaves them in the OS temp dir, which the OS reaps). An
/// EMPTY plan (`is_empty()`) means "spawn the shell plain" — the Unsupported /
/// disabled / IO-failure path.
#[derive(Debug, Clone, Default)]
pub struct IntegrationPlan {
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    /// Temp file/dir paths created for this plan. Retained so a caller could
    /// schedule cleanup; nyx leaves them in the OS temp dir (the OS reaps them).
    /// Read by the integration tests; `allow(dead_code)` outside tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub temp_paths: Vec<PathBuf>,
}

impl IntegrationPlan {
    /// True when no injection applies (plain spawn): no args AND no env. Used by
    /// the tests and a future consumer; `allow(dead_code)` outside tests (the
    /// spawn path consumes `args`/`env` directly rather than branching on this).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.args.is_empty() && self.env.is_empty()
    }
}

/// Monotonic suffix so concurrent spawns never collide on a temp snippet name
/// (the PID alone is shared across a batch spawned in one process tick).
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Build the [`IntegrationPlan`] for a resolved `shell`. Writes the per-shell
/// snippet to a unique temp file and returns the args/env that point the shell at
/// it. Returns an EMPTY plan (plain spawn) when:
///   - the shell is [`Shell::Unsupported`],
///   - `NYX_SHELL_INTEGRATION` is set to `0`/`off`/`false` (explicit disable), or
///   - writing the temp snippet fails (bypass-on-failure — never fail the spawn).
///
/// This is the only function here that does IO; classification + snippet text are
/// pure helpers it composes.
pub fn build(shell: &str) -> IntegrationPlan {
    if integration_disabled() {
        return IntegrationPlan::default();
    }
    match classify(shell) {
        Shell::Bash => build_bash().unwrap_or_default(),
        Shell::Zsh => build_zsh().unwrap_or_default(),
        Shell::PowerShell => build_powershell().unwrap_or_default(),
        Shell::Unsupported => IntegrationPlan::default(),
    }
}

/// Whether injection is force-disabled via [`DISABLE_ENV`]. `0`/`off`/`false`
/// (case-insensitive) disables; unset or any other value enables.
fn integration_disabled() -> bool {
    match std::env::var(DISABLE_ENV) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "0" || v == "off" || v == "false"
        }
        Err(_) => false,
    }
}

/// A fresh, process-unique temp path with `prefix` and `ext`, in the OS temp dir.
fn temp_path(prefix: &str, ext: &str) -> PathBuf {
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("{prefix}-{pid}-{seq}{ext}"));
    p
}

// --- bash ----------------------------------------------------------------

/// The bash rc snippet: source the user's `~/.bashrc` FIRST (non-destructive),
/// then append the OSC 133 hooks. `PROMPT_COMMAND` runs BEFORE each prompt —
/// it captures the just-finished command's `$?` and emits `133;D;<code>` then
/// `133;A`. A `DEBUG` trap fires before each command runs (preexec) and emits
/// `133;C`, gated by a flag so it only fires for an actual command, not for the
/// PROMPT_COMMAND itself. Mirrors VS Code's bash integration.
fn bash_snippet() -> String {
    // `\033` = ESC, `\007` = BEL. Single-quoted heredoc-style literal so the shell
    // expands `$?` at runtime, not now.
    r#"# nyx OSC 133 shell integration (auto-injected; your ~/.bashrc is sourced below).
if [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc"; fi

__nyx_osc133_preexec() {
  # Only emit pre-exec once per command, and never for the prompt command itself.
  if [ -n "$__nyx_osc133_active" ]; then return; fi
  __nyx_osc133_active=1
  printf '\033]133;C\007'
}
__nyx_osc133_precmd() {
  local code=$?
  printf '\033]133;D;%s\007' "$code"
  __nyx_osc133_active=
  printf '\033]133;A\007'
}
# preexec via the DEBUG trap; precmd via PROMPT_COMMAND (prepended, non-destructive).
trap '__nyx_osc133_preexec' DEBUG
case "$PROMPT_COMMAND" in
  *__nyx_osc133_precmd*) ;;
  *) PROMPT_COMMAND="__nyx_osc133_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac
"#
    .to_string()
}

fn build_bash() -> std::io::Result<IntegrationPlan> {
    let path = temp_path("nyx-bash-rc", ".sh");
    std::fs::write(&path, bash_snippet())?;
    Ok(IntegrationPlan {
        // `--rcfile <tmp>` overrides ~/.bashrc as the startup file (our snippet
        // sources the real one first); `-i` keeps it interactive.
        args: vec![
            OsString::from("--rcfile"),
            path.clone().into_os_string(),
            OsString::from("-i"),
        ],
        env: Vec::new(),
        temp_paths: vec![path],
    })
}

// --- zsh -----------------------------------------------------------------

/// The zsh `.zshrc` for a temp `ZDOTDIR`: source the user's real `.zshrc` first
/// (from their original `ZDOTDIR` or `$HOME`), then register `precmd`/`preexec`
/// hook functions. zsh has first-class hook arrays, so no DEBUG trap is needed.
/// `precmd` runs before each prompt and reads `$?`; `preexec` runs just before a
/// command executes.
fn zsh_snippet() -> String {
    r#"# nyx OSC 133 shell integration (auto-injected; your real .zshrc is sourced below).
# Source the user's real zshrc first (non-destructive). Their original ZDOTDIR was
# captured into NYX_REAL_ZDOTDIR; fall back to $HOME.
__nyx_real_zdotdir="${NYX_REAL_ZDOTDIR:-$HOME}"
if [ -f "$__nyx_real_zdotdir/.zshrc" ]; then source "$__nyx_real_zdotdir/.zshrc"; fi

autoload -Uz add-zsh-hook 2>/dev/null
__nyx_osc133_preexec() { printf '\033]133;C\007'; }
__nyx_osc133_precmd()  { printf '\033]133;D;%s\007' "$?"; printf '\033]133;A\007'; }
if (( $+functions[add-zsh-hook] )); then
  add-zsh-hook preexec __nyx_osc133_preexec
  add-zsh-hook precmd  __nyx_osc133_precmd
else
  precmd_functions+=(__nyx_osc133_precmd)
  preexec_functions+=(__nyx_osc133_preexec)
fi
"#
    .to_string()
}

fn build_zsh() -> std::io::Result<IntegrationPlan> {
    // zsh reads `$ZDOTDIR/.zshrc`; we point ZDOTDIR at a fresh temp DIR holding our
    // .zshrc, and pass the user's original ZDOTDIR through so the snippet can source
    // their real config.
    let dir = temp_path("nyx-zsh-zdotdir", "");
    std::fs::create_dir_all(&dir)?;
    let zshrc = dir.join(".zshrc");
    std::fs::write(&zshrc, zsh_snippet())?;

    let real_zdotdir = std::env::var_os("ZDOTDIR")
        .or_else(|| std::env::var_os("HOME"))
        .unwrap_or_default();

    Ok(IntegrationPlan {
        args: Vec::new(),
        env: vec![
            (OsString::from("ZDOTDIR"), dir.clone().into_os_string()),
            (OsString::from("NYX_REAL_ZDOTDIR"), real_zdotdir),
        ],
        temp_paths: vec![zshrc, dir],
    })
}

// --- PowerShell ----------------------------------------------------------

/// The PowerShell snippet (dot-sourced via `-Command`): wrap the existing
/// `prompt` function so it emits `133;D;<code>` (code from `$?`→0 else
/// `$LASTEXITCODE` else 1) + `133;A` at the top, calls the ORIGINAL prompt, then
/// emits `133;B`. A `PSReadLine` command-accepted hook (best-effort — guarded so a
/// missing PSReadLine never errors) emits `133;C`. Edition-identical for pwsh 7
/// and Windows PowerShell 5.1.
fn powershell_snippet() -> String {
    // `$([char]27)` = ESC, `$([char]7)` = BEL. The user's profile has already run
    // by the time `-Command` executes (PowerShell loads profiles before `-Command`),
    // so wrapping `prompt` here is non-destructive — it preserves their prompt.
    r#"# nyx OSC 133 shell integration (auto-injected; your profile already loaded).
$global:__nyxEsc = [char]27
$global:__nyxBel = [char]7
if (-not (Test-Path function:\__nyxOriginalPrompt)) {
  Copy-Item function:\prompt function:\__nyxOriginalPrompt -ErrorAction SilentlyContinue
}
function global:prompt {
  $code = if ($?) { 0 } elseif ($null -ne $LASTEXITCODE) { $LASTEXITCODE } else { 1 }
  $out = "$($__nyxEsc)]133;D;$code$($__nyxBel)$($__nyxEsc)]133;A$($__nyxBel)"
  if (Test-Path function:\__nyxOriginalPrompt) {
    $out += (& __nyxOriginalPrompt)
  } else {
    $out += "PS $($executionContext.SessionState.Path.CurrentLocation)> "
  }
  $out += "$($__nyxEsc)]133;B$($__nyxBel)"
  return $out
}
# Pre-exec (133;C) on command accept, best-effort (no-op if PSReadLine is absent).
try {
  Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    [Console]::Write("$($__nyxEsc)]133;C$($__nyxBel)")
  } -ErrorAction SilentlyContinue
} catch {}
"#
    .to_string()
}

fn build_powershell() -> std::io::Result<IntegrationPlan> {
    let path = temp_path("nyx-pwsh", ".ps1");
    std::fs::write(&path, powershell_snippet())?;
    // `-NoExit` keeps the session interactive after the dot-source; `-Command`
    // dot-sources the snippet (the user's profile has already loaded). Single-quote
    // the path for PowerShell and escape any embedded single quotes.
    let quoted = format!("'{}'", path.to_string_lossy().replace('\'', "''"));
    Ok(IntegrationPlan {
        args: vec![
            OsString::from("-NoExit"),
            OsString::from("-Command"),
            OsString::from(format!(". {quoted}")),
        ],
        env: Vec::new(),
        temp_paths: vec![path],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- classify (pure) -------------------------------------------------

    #[test]
    fn classifies_supported_shells_by_basename() {
        assert_eq!(classify("bash"), Shell::Bash);
        assert_eq!(classify("/bin/bash"), Shell::Bash);
        assert_eq!(classify("/usr/local/bin/bash"), Shell::Bash);
        assert_eq!(classify("-bash"), Shell::Bash); // login shell spelling
        assert_eq!(classify("zsh"), Shell::Zsh);
        assert_eq!(classify("/usr/bin/zsh"), Shell::Zsh);
        // PowerShell, both editions, with/without .exe and Windows paths.
        assert_eq!(classify("pwsh"), Shell::PowerShell);
        assert_eq!(classify("pwsh.exe"), Shell::PowerShell);
        assert_eq!(classify("powershell.exe"), Shell::PowerShell);
        assert_eq!(
            classify("C:\\Program Files\\PowerShell\\7\\pwsh.exe"),
            Shell::PowerShell
        );
        assert_eq!(
            classify("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            Shell::PowerShell
        );
    }

    #[test]
    fn classifies_unsupported_shells_as_unsupported() {
        // sh is NOT bash (dash has no DEBUG trap); cmd/fish/nu unsupported.
        assert_eq!(classify("sh"), Shell::Unsupported);
        assert_eq!(classify("/bin/sh"), Shell::Unsupported);
        assert_eq!(classify("cmd.exe"), Shell::Unsupported);
        assert_eq!(
            classify("C:\\Windows\\System32\\cmd.exe"),
            Shell::Unsupported
        );
        assert_eq!(classify("fish"), Shell::Unsupported);
        assert_eq!(classify("nu"), Shell::Unsupported);
        assert_eq!(classify(""), Shell::Unsupported);
    }

    // --- snippet content (pure) ------------------------------------------

    #[test]
    fn bash_snippet_sources_user_rc_and_emits_c_and_d() {
        let s = bash_snippet();
        assert!(
            s.contains(". \"$HOME/.bashrc\""),
            "sources user bashrc first"
        );
        assert!(s.contains(r"\033]133;C\007"), "emits pre-exec 133;C");
        assert!(
            s.contains(r"\033]133;D;%s\007"),
            "emits command-end 133;D with code"
        );
        assert!(
            s.contains("PROMPT_COMMAND="),
            "registers precmd via PROMPT_COMMAND"
        );
        assert!(
            s.contains("trap '__nyx_osc133_preexec' DEBUG"),
            "preexec via DEBUG trap"
        );
    }

    #[test]
    fn zsh_snippet_sources_user_rc_and_registers_hooks() {
        let s = zsh_snippet();
        assert!(
            s.contains("source \"$__nyx_real_zdotdir/.zshrc\""),
            "sources real zshrc"
        );
        assert!(s.contains(r"\033]133;C\007"), "preexec 133;C");
        assert!(
            s.contains(r"\033]133;D;%s\007"),
            "precmd 133;D with $? code"
        );
        assert!(s.contains("add-zsh-hook"), "uses zsh hooks");
    }

    #[test]
    fn powershell_snippet_wraps_prompt_and_recovers_exit_code() {
        let s = powershell_snippet();
        assert!(
            s.contains("__nyxOriginalPrompt"),
            "preserves the original prompt"
        );
        assert!(
            s.contains("if ($?) { 0 } elseif ($null -ne $LASTEXITCODE)"),
            "recovers exit code"
        );
        assert!(s.contains("133;D;$code"), "emits command-end with code");
        assert!(s.contains("133;A"), "emits prompt-start");
        assert!(s.contains("133;B"), "emits command-start at prompt end");
        assert!(s.contains("133;C"), "emits pre-exec on accept-line");
    }

    // --- build (IO + dispatch) -------------------------------------------

    #[test]
    fn build_unsupported_is_empty_plan() {
        assert!(build("sh").is_empty(), "sh injects nothing (idle-only)");
        assert!(build("cmd.exe").is_empty(), "cmd injects nothing");
        assert!(build("fish").is_empty());
    }

    #[test]
    fn build_bash_plan_points_at_a_real_rcfile() {
        let plan = build("bash");
        assert!(!plan.is_empty(), "bash gets an integration plan");
        // `--rcfile <path> -i`
        assert_eq!(plan.args[0], OsString::from("--rcfile"));
        let rc = PathBuf::from(&plan.args[1]);
        assert!(rc.exists(), "the rcfile was actually written");
        let body = std::fs::read_to_string(&rc).unwrap();
        assert!(body.contains("133;C") && body.contains("133;D"));
        assert!(plan.args.contains(&OsString::from("-i")));
        // cleanup
        for p in &plan.temp_paths {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn build_zsh_plan_sets_zdotdir_to_a_real_dir_with_zshrc() {
        let plan = build("zsh");
        assert!(!plan.is_empty(), "zsh gets an integration plan");
        let zdotdir = plan
            .env
            .iter()
            .find(|(k, _)| k == &OsString::from("ZDOTDIR"))
            .map(|(_, v)| PathBuf::from(v))
            .expect("ZDOTDIR set");
        assert!(zdotdir.join(".zshrc").exists(), "ZDOTDIR/.zshrc written");
        assert!(
            plan.env
                .iter()
                .any(|(k, _)| k == &OsString::from("NYX_REAL_ZDOTDIR")),
            "passes the user's original ZDOTDIR through"
        );
        for p in &plan.temp_paths {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir_all(&zdotdir);
    }

    #[test]
    fn build_powershell_plan_dot_sources_a_real_ps1() {
        let plan = build("pwsh.exe");
        assert!(!plan.is_empty(), "powershell gets an integration plan");
        assert_eq!(plan.args[0], OsString::from("-NoExit"));
        assert_eq!(plan.args[1], OsString::from("-Command"));
        let cmd = plan.args[2].to_string_lossy().into_owned();
        assert!(cmd.starts_with(". '"), "dot-sources the snippet path");
        // The referenced .ps1 exists.
        let ps1 = &plan.temp_paths[0];
        assert!(ps1.exists());
        let body = std::fs::read_to_string(ps1).unwrap();
        assert!(body.contains("133;D") && body.contains("__nyxOriginalPrompt"));
        for p in &plan.temp_paths {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn disable_env_forces_empty_plan() {
        // The explicit internal disable switch: even a supported shell gets a plain
        // spawn when NYX_SHELL_INTEGRATION is off.
        std::env::set_var(DISABLE_ENV, "0");
        assert!(build("bash").is_empty(), "disabled → plain spawn");
        std::env::set_var(DISABLE_ENV, "off");
        assert!(build("zsh").is_empty());
        std::env::remove_var(DISABLE_ENV);
    }
}
