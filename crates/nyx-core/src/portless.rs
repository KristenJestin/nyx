//! Optional `nyx.localhost` surface via the `portless` CLI (PRD-4 #6, ADR-0003 D11).
//!
//! This is a **separate human / integration surface**, NOT the MCP transport. The
//! MCP transport stays registered on **localhost direct** (`http://127.0.0.1:<port>/mcp`,
//! see [`crate::onboarding`]); portless only adds a readable `https://nyx.localhost`
//! alias in front of the same port. The two never couple (ADR-0003 D11).
//!
//! ## Behavior (ADR-0003 D11 / PRD-4 #6)
//! - A setting **disabled by default** ([`PortlessState::default`] is `Disabled`).
//! - **Enable**: verify the `portless` binary is present, run
//!   `portless alias nyx <port> --force`, and surface `https://nyx.localhost`.
//! - **Disable**: run `portless alias --remove nyx`.
//! - **No** auto-install, **no** LAN, **no** funnel/ngrok. If `portless` is absent,
//!   enabling fails with a **clear error** ([`PortlessError::NotInstalled`]) — nyx
//!   never tries to install it.
//!
//! ## Testability / safety
//! The `portless` invocation is behind the [`CommandRunner`] trait, so production
//! shells out to the real binary ([`SystemRunner`]) while tests inject a
//! [`fake::FakeRunner`] that records the argv and returns canned exit
//! codes/output. The real binary is NEVER invoked under test (the testingDecisions
//! safety requirement) and no real proxy is needed.

/// The portless alias name nyx registers. Stable so enable/disable target the same
/// alias.
pub const ALIAS_NAME: &str = "nyx";

/// The human-facing URL the portless alias exposes once enabled.
pub const PORTLESS_URL: &str = "https://nyx.localhost";

/// The on/off state of the portless option. **Disabled by default** (ADR-0003 D11,
/// PRD-4 #6): the `#[default]` marks `Disabled` so the option is strictly opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PortlessState {
    #[default]
    Disabled,
    Enabled,
}

/// Result of one portless invocation: the process exit status + captured output.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    /// `true` iff the process exited `0`.
    pub success: bool,
    /// Combined stderr/stdout the runner captured, for error surfacing.
    pub message: String,
}

/// A clear, typed failure for the portless option (done-criterion: "absence de
/// portless = erreur claire").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortlessError {
    /// The `portless` binary is not on `PATH`. nyx does NOT auto-install it; the
    /// caller surfaces this verbatim so the user installs portless themselves.
    NotInstalled,
    /// `portless` was found but the command exited non-zero. Carries its output.
    CommandFailed { command: String, message: String },
}

impl std::fmt::Display for PortlessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortlessError::NotInstalled => write!(
                f,
                "portless is not installed (the `portless` command was not found on PATH). \
                 Install portless to expose {PORTLESS_URL}; nyx does not auto-install it."
            ),
            PortlessError::CommandFailed { command, message } => {
                write!(f, "`{command}` failed: {message}")
            }
        }
    }
}

impl std::error::Error for PortlessError {}

/// Abstraction over running the `portless` CLI, so tests inject a fake and never
/// touch the real binary. Production is [`SystemRunner`].
pub trait CommandRunner: Send + Sync {
    /// Whether the `portless` binary is resolvable (on `PATH`). Checked before an
    /// enable so a missing binary is a clear error, not an opaque spawn failure.
    fn is_available(&self) -> bool;
    /// Run `portless <args...>` and return its outcome. Errors only on a spawn
    /// failure (e.g. binary vanished between the availability check and the run);
    /// a non-zero exit is reported via [`CommandOutcome::success`] == `false`.
    fn run(&self, args: &[&str]) -> std::io::Result<CommandOutcome>;
}

/// Manages the portless alias through an injected [`CommandRunner`]. Holds no global
/// state beyond the runner; the on/off setting itself is persisted by the caller
/// (the bridge), so this type is a pure command orchestrator.
pub struct PortlessManager<R: CommandRunner> {
    runner: R,
}

impl<R: CommandRunner> PortlessManager<R> {
    /// Build a manager over `runner`. Production passes [`SystemRunner`]; tests pass
    /// a fake.
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Enable the option: verify `portless` is present, then run
    /// `portless alias nyx <port> --force`. On success returns [`PORTLESS_URL`] (the
    /// surfaced `https://nyx.localhost`). A missing binary → [`PortlessError::NotInstalled`]
    /// (no auto-install); a non-zero exit → [`PortlessError::CommandFailed`].
    pub fn enable(&self, port: u16) -> Result<&'static str, PortlessError> {
        if !self.runner.is_available() {
            return Err(PortlessError::NotInstalled);
        }
        let port = port.to_string();
        // `--force` so re-enabling overwrites any stale alias idempotently.
        let args = ["alias", ALIAS_NAME, port.as_str(), "--force"];
        let outcome = self.run_checked(&args)?;
        let _ = outcome; // success implied by run_checked
        Ok(PORTLESS_URL)
    }

    /// Disable the option: run `portless alias --remove nyx`. A missing binary is a
    /// clear error here too (you can't remove an alias without the CLI), surfaced so
    /// the caller can decide; a non-zero exit → [`PortlessError::CommandFailed`].
    pub fn disable(&self) -> Result<(), PortlessError> {
        if !self.runner.is_available() {
            return Err(PortlessError::NotInstalled);
        }
        let args = ["alias", "--remove", ALIAS_NAME];
        self.run_checked(&args)?;
        Ok(())
    }

    /// Run a portless subcommand, mapping a spawn failure / non-zero exit onto a
    /// clear [`PortlessError`]. Keeps the `command` string in the error for the UI.
    fn run_checked(&self, args: &[&str]) -> Result<CommandOutcome, PortlessError> {
        let command = format!("portless {}", args.join(" "));
        match self.runner.run(args) {
            Ok(outcome) if outcome.success => Ok(outcome),
            Ok(outcome) => Err(PortlessError::CommandFailed {
                command,
                message: if outcome.message.is_empty() {
                    "non-zero exit".to_string()
                } else {
                    outcome.message
                },
            }),
            // A spawn error after the availability check (binary removed mid-flight,
            // permission, …) is still a "command failed" with the OS message.
            Err(e) => Err(PortlessError::CommandFailed {
                command,
                message: e.to_string(),
            }),
        }
    }
}

/// Production [`CommandRunner`]: shells out to the real `portless` binary. Availability
/// is a `portless --version` probe (works cross-platform; no `which`/`where` dep).
/// On Windows the spawn uses `CREATE_NO_WINDOW` so the helper never flashes a console.
pub struct SystemRunner;

impl SystemRunner {
    /// Base command, hiding the console window on Windows (same flag the command
    /// runner uses for `taskkill`).
    fn base() -> std::process::Command {
        // Hardened spawn via the centralized helper (CREATE_NO_WINDOW on Windows) so
        // the `portless` helper never flashes a console.
        let mut cmd = crate::proc_util::command("portless");
        cmd.stdin(std::process::Stdio::null());
        cmd
    }
}

impl CommandRunner for SystemRunner {
    fn is_available(&self) -> bool {
        Self::base()
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run(&self, args: &[&str]) -> std::io::Result<CommandOutcome> {
        let out = Self::base().args(args).output()?;
        let mut message = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if message.is_empty() {
            message = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Ok(CommandOutcome {
            success: out.status.success(),
            message,
        })
    }
}

/// Persist/read the portless on/off setting as a tiny JSON file. The path is
/// **injectable** (the caller passes nyx's data dir), so tests use a temp file and
/// production uses the app data dir — same injectability discipline as onboarding.
/// Missing / unreadable file → [`PortlessState::Disabled`] (the safe default).
pub mod settings {
    use super::PortlessState;
    use std::path::Path;

    /// File name under the data dir holding the toggle.
    pub const SETTINGS_FILE: &str = "portless.json";

    /// Read the persisted state from `path`. Absent or malformed → `Disabled`.
    pub fn read(path: &Path) -> PortlessState {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let enabled = serde_json::from_str::<serde_json::Value>(&raw)
                    .ok()
                    .and_then(|v| v.get("enabled").and_then(serde_json::Value::as_bool))
                    .unwrap_or(false);
                if enabled {
                    PortlessState::Enabled
                } else {
                    PortlessState::Disabled
                }
            }
            Err(_) => PortlessState::Disabled,
        }
    }

    /// Persist `state` to `path`, creating the parent dir if needed.
    pub fn write(path: &Path, state: PortlessState) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let enabled = matches!(state, PortlessState::Enabled);
        let body = format!("{{\n  \"enabled\": {enabled}\n}}\n");
        std::fs::write(path, body)
    }
}

#[cfg(test)]
pub mod fake {
    //! A fake [`CommandRunner`] for tests: records every argv and replays canned
    //! outcomes, so the suite exercises enable/disable/error WITHOUT the real binary.

    use super::*;
    use std::sync::Mutex;

    /// Records the argv of every `run` call and replays a configured availability +
    /// outcome. The recorded calls let a test assert the EXACT portless command line.
    pub struct FakeRunner {
        available: bool,
        /// Canned outcome for `run`; `None` simulates a spawn error.
        outcome: Option<CommandOutcome>,
        /// argv of every `run`, in order.
        pub calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        /// A runner where `portless` is present and every command succeeds.
        pub fn available() -> Self {
            Self {
                available: true,
                outcome: Some(CommandOutcome {
                    success: true,
                    message: String::new(),
                }),
                calls: Mutex::new(Vec::new()),
            }
        }

        /// A runner where `portless` is ABSENT (the NotInstalled path).
        pub fn missing() -> Self {
            Self {
                available: false,
                outcome: None,
                calls: Mutex::new(Vec::new()),
            }
        }

        /// A runner that is present but whose commands FAIL (non-zero) with `message`.
        pub fn failing(message: &str) -> Self {
            Self {
                available: true,
                outcome: Some(CommandOutcome {
                    success: false,
                    message: message.to_string(),
                }),
                calls: Mutex::new(Vec::new()),
            }
        }

        /// The argv of the n-th recorded `run`, joined as a command line for asserts.
        pub fn nth_call(&self, n: usize) -> Option<String> {
            self.calls.lock().unwrap().get(n).map(|c| c.join(" "))
        }
    }

    impl CommandRunner for FakeRunner {
        fn is_available(&self) -> bool {
            self.available
        }

        fn run(&self, args: &[&str]) -> std::io::Result<CommandOutcome> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            match &self.outcome {
                Some(o) => Ok(o.clone()),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "portless not found",
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests use [`fake::FakeRunner`] EXCLUSIVELY — the real `portless` binary is
    //! never invoked (testingDecisions safety). They cover enable, disable, and the
    //! missing-binary error, asserting the exact argv against ADR-0003 D11.

    use super::*;

    #[test]
    fn default_state_is_disabled() {
        assert_eq!(PortlessState::default(), PortlessState::Disabled);
    }

    #[test]
    fn enable_surfaces_url_and_runs_exact_argv() {
        // Keep a handle to the fake (via the borrow adapter) so we can inspect the
        // recorded argv after the call.
        let owned = fake::FakeRunner::available();
        let url = {
            let m = PortlessManager::new(BorrowRunner(&owned));
            m.enable(8765)
                .expect("enable succeeds with portless present")
        };
        // Done-criterion: enable surfaces nyx.localhost...
        assert_eq!(url, "https://nyx.localhost");
        assert_eq!(PORTLESS_URL, "https://nyx.localhost");
        // ...and runs the EXACT command line per ADR-0003 D11:
        // `portless alias nyx 8765 --force`.
        assert_eq!(owned.nth_call(0).as_deref(), Some("alias nyx 8765 --force"));
        assert_eq!(
            owned.calls.lock().unwrap().len(),
            1,
            "exactly one portless call"
        );
    }

    #[test]
    fn disable_runs_alias_remove_nyx() {
        let owned = fake::FakeRunner::available();
        {
            let m = PortlessManager::new(BorrowRunner(&owned));
            m.disable().expect("disable succeeds with portless present");
        }
        // Done-criterion: removes the expected alias.
        assert_eq!(owned.nth_call(0).as_deref(), Some("alias --remove nyx"));
    }

    #[test]
    fn enable_when_portless_missing_is_clear_error_and_no_spawn() {
        let owned = fake::FakeRunner::missing();
        let err = {
            let m = PortlessManager::new(BorrowRunner(&owned));
            m.enable(8765).expect_err("missing portless must error")
        };
        // Done-criterion: absence of portless = clear error, and no auto-install /
        // no spawn attempt (we never reached `run`).
        assert_eq!(err, PortlessError::NotInstalled);
        assert!(err.to_string().contains("portless is not installed"));
        assert_eq!(
            owned.calls.lock().unwrap().len(),
            0,
            "no command spawned when absent"
        );
    }

    #[test]
    fn disable_when_portless_missing_is_clear_error() {
        let mgr = PortlessManager::new(fake::FakeRunner::missing());
        let err = mgr
            .disable()
            .expect_err("missing portless must error on disable too");
        assert_eq!(err, PortlessError::NotInstalled);
    }

    #[test]
    fn enable_when_command_fails_reports_command_and_message() {
        let mgr = PortlessManager::new(fake::FakeRunner::failing("alias already bound"));
        let err = mgr.enable(8765).expect_err("non-zero exit must error");
        match err {
            PortlessError::CommandFailed { command, message } => {
                assert_eq!(command, "portless alias nyx 8765 --force");
                assert_eq!(message, "alias already bound");
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn settings_default_and_round_trip() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "nyx-portless-{}-{}.json",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::remove_file(&path).ok();
        // Absent file → Disabled (safe default).
        assert_eq!(settings::read(&path), PortlessState::Disabled);
        // Round-trips enabled, then back to disabled.
        settings::write(&path, PortlessState::Enabled).unwrap();
        assert_eq!(settings::read(&path), PortlessState::Enabled);
        settings::write(&path, PortlessState::Disabled).unwrap();
        assert_eq!(settings::read(&path), PortlessState::Disabled);
        std::fs::remove_file(&path).ok();
    }

    /// A thin [`CommandRunner`] adapter that BORROWS a fake so a test can inspect the
    /// fake's recorded calls after the manager runs (the manager takes its runner by
    /// value, so we hand it a reference wrapper instead).
    struct BorrowRunner<'a>(&'a fake::FakeRunner);
    impl CommandRunner for BorrowRunner<'_> {
        fn is_available(&self) -> bool {
            self.0.is_available()
        }
        fn run(&self, args: &[&str]) -> std::io::Result<CommandOutcome> {
            self.0.run(args)
        }
    }
}
