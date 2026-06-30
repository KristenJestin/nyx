//! Generic agent-session ADAPTER contract + registry (PRD-5, Phase 1 — ADR-0010).
//!
//! nyx treats agent resume as a GENERIC problem: every terminal can host an agent
//! session with an EXTERNAL session id and an agent-specific resume command. This
//! module separates the COMMON nyx logic (the registry, the normalized event +
//! resume types, the capability/limits surface) from each agent's SPECIFICS (how it
//! detects its CLI, installs its integration, parses its events, builds its resume
//! command).
//!
//! # The contract ([`AgentAdapter`])
//!
//! Mirrors the PRD Impl Decisions / ADR-0010 contract:
//!   - [`AgentAdapter::kind`] — the `agent_kind` this adapter owns (the registry key,
//!     and what is stored in `agent_sessions.agent_kind`).
//!   - [`AgentAdapter::detect`] — is this agent's CLI available / is a command line
//!     one of its invocations? (best-effort heuristic; never errors).
//!   - [`AgentAdapter::install_integration`] — install the agent-side glue (e.g. a
//!     Claude plugin) so nyx receives session events. Phase-2 work for `claude_code`;
//!     here it returns [`InstallOutcome::NotImplemented`] so the contract is complete.
//!   - [`AgentAdapter::parse_event`] — normalize a raw agent payload into an
//!     [`AgentEvent`] (start / end), or `None` if it is not a recognizable event.
//!   - [`AgentAdapter::build_resume_command`] — build the shell command line that
//!     resumes an EXACT session (e.g. `claude --resume <id>`), or `None` if the agent
//!     cannot resume exactly.
//!   - capability flags [`AgentAdapter::supports_exact_resume`] /
//!     [`AgentAdapter::supports_end_event`] and [`AgentAdapter::known_risks`] — the
//!     per-adapter LIMITS, surfaced to the rest of nyx (warnings, resume decisions).
//!
//! # v1 scope
//!
//! Only [`ClaudeCodeAdapter`] is put into PRODUCTION. The other three kinds
//! (`codex`, `opencode`, `custom`) are REPRESENTABLE — the registry knows every
//! `agent_kind` and exposes a [`GenericAdapter`] placeholder for each so they are
//! addressable and their (unknown / unvalidated) limits are declared — but no
//! production capture/resume adapter ships for them in this PRD (Codex / OpenCode
//! are spikes of PRD-6). The Claude end-to-end plugin + resume land in later phases;
//! THIS task delivers the contract, the registry, and the normalized event/resume
//! types with a tested `claude_code` adapter.
//!
//! The contract surface is fully exercised by this module's tests but is not yet
//! WIRED into the bridge (that is the Phase-2/3 work — the live plugin, the resume
//! flow, the close warning). Until those consumers land, the public items would read
//! as dead in a non-test build; the module-level `allow(dead_code)` (mirroring how
//! `db.rs` stages phased helpers) keeps the non-test build warning-free while the
//! tests prove the contract.
#![cfg_attr(not(test), allow(dead_code))]

use crate::db;

/// A NORMALIZED agent session lifecycle event, produced by
/// [`AgentAdapter::parse_event`] from a raw agent payload. The common layer maps
/// these onto the `agent_sessions` state transitions (start → upsert active row;
/// end → mark ended). `resume_failed` is NOT an agent event — it is recorded by
/// nyx's own resume flow when a resume attempt fails — so it is not a variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// The agent reported a session START (Claude `SessionStart`, source
    /// startup|resume|clear). Carries the captured fields nyx persists.
    Start(SessionStart),
    /// The agent reported a clean session END (Claude `SessionEnd`). Carries the
    /// external id so the common layer can resolve which row to mark `ended`.
    End(SessionEnd),
}

/// The fields captured from an agent SessionStart payload. `external_session_id` and
/// `cwd` are the structuring fields; `transcript_path` / `workspace_id` are optional;
/// `metadata_json` is the adapter's JSON bag (e.g. `{"source":"startup"}`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionStart {
    pub external_session_id: String,
    pub cwd: String,
    pub transcript_path: Option<String>,
    pub workspace_id: Option<String>,
    pub metadata_json: Option<String>,
}

/// The fields captured from an agent SessionEnd payload. `reason` is the raw Claude
/// `SessionEnd` reason (`clear` | `resume` | `logout` | `prompt_input_exit` |
/// `bypass_permissions_disabled` | `other`); the common layer uses it to tell an
/// INTERNAL transition (a `/clear` or `/resume` immediately re-opens a session on the
/// SAME terminal) apart from a real end, so the icon/dot do not blink during the
/// transition (finding: the Claude icon "jumps" after `/clear`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionEnd {
    pub external_session_id: String,
    pub reason: Option<String>,
}

impl SessionEnd {
    /// `true` when this end is an INTERNAL transition rather than a real session end:
    /// a `/clear` or `/resume` makes Claude emit `SessionEnd { reason }` IMMEDIATELY
    /// followed by `SessionStart { source }` on the SAME terminal — the session never
    /// actually goes away, it is replaced in place. nyx must NOT mark the row `ended`
    /// (nor drop the runtime activity) for these, otherwise the active-session row blinks
    /// out for the gap between the two events and the sidebar icon falls back to the
    /// generic terminal glyph (and the dot disappears) for a frame — the "icône qui
    /// saute après /clear" bug. For every OTHER reason (logout, prompt_input_exit, a
    /// brutal kill that did fire, …) the session really ends and the row is marked.
    pub fn is_internal_transition(&self) -> bool {
        matches!(self.reason.as_deref(), Some("clear") | Some("resume"))
    }
}

/// Outcome of [`AgentAdapter::install_integration`]. The end-to-end install is
/// per-agent and lands in later phases; the contract still needs a typed result so
/// the registry is usable now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The integration was (idempotently) installed (or updated, e.g. the port
    /// changed). Constructed by [`ClaudeCodeAdapter::install_integration`] (Phase 2).
    Installed,
    /// The integration was already present and current; nothing was written.
    /// Constructed by [`ClaudeCodeAdapter::install_integration`] (Phase 2).
    AlreadyPresent,
    /// This adapter has no integration to install in this PRD (the default for the
    /// representable-but-not-production agents).
    NotImplemented,
    /// The install was attempted but FAILED (e.g. the `claude` CLI is not on PATH, or a
    /// CLI subcommand errored). Carries the user-facing message surfaced in the UI — no
    /// fake success (review #35).
    Failed(String),
}

/// A KNOWN RISK / LIMIT of an adapter — the per-adapter limits the PRD requires be
/// "exposable". Surfaced to the resume decision and the close-warning so nyx can be
/// honest about what an agent can and cannot do (e.g. Claude's `SessionEnd` does not
/// fire on a brutal kill).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRisk {
    /// Stable identifier for the risk (e.g. `"session_end_unreliable_on_kill"`).
    pub code: &'static str,
    /// Human-readable description.
    pub detail: &'static str,
}

/// The agent adapter contract. Every method has a sane default so a placeholder
/// adapter (the representable-only kinds) only needs [`AgentAdapter::kind`]; a real
/// adapter overrides the methods it supports.
pub trait AgentAdapter: Send + Sync {
    /// The `agent_kind` this adapter owns — the registry key and the value stored in
    /// `agent_sessions.agent_kind`. MUST be one of the `db::AGENT_KIND_*` constants.
    fn kind(&self) -> &'static str;

    /// Best-effort: does `command_line` look like an invocation of THIS agent's CLI?
    /// Pure heuristic over the spawned command; never errors. Default: `false`
    /// (a placeholder adapter detects nothing).
    fn detect(&self, _command_line: &str) -> bool {
        false
    }

    /// Describe this agent's bundled PLUGIN install — the GENERIC vehicle that wires
    /// the agent's session-capture hooks. The descriptor names the marketplace/plugin,
    /// the read-only bundled SOURCE dir (the copy source), the STABLE app-data dir the
    /// plugin is copied into + registered from (review #33), and the settings file (for
    /// legacy-key cleanup on uninstall). `resource_dir` is the Tauri resource base (so
    /// the bundled plugin resolves in a packaged build; `None` falls back to the dev
    /// source tree); `app_data_dir` is where the stable copy lands. Returns `None` when
    /// the agent ships no plugin or its paths cannot be resolved. The common layer
    /// (bridge) drives install/reconcile/uninstall off this descriptor + this adapter's
    /// [`AgentAdapter::plugin_cli`] via [`crate::plugin`] — no agent specifics leak into
    /// the common code (finding #25). Default: `None` (a placeholder adapter ships no
    /// plugin).
    fn plugin_install(
        &self,
        _resource_dir: Option<&std::path::Path>,
        _app_data_dir: Option<&std::path::Path>,
    ) -> Option<crate::plugin::PluginInstall> {
        None
    }

    /// The agent's plugin CLI driver (review #32) — the seam that shells out to the
    /// agent's `plugin` CLI to register/install/remove the marketplace. `None` for an
    /// agent with no CLI-driven install. Default: `None`.
    fn plugin_cli(&self) -> Option<Box<dyn crate::plugin::PluginCli>> {
        None
    }

    /// Install the agent-side integration (the bundled PLUGIN) that makes nyx receive
    /// session events. Idempotent. `resource_dir` lets a packaged build resolve the
    /// bundled plugin; `app_data_dir` is the stable copy target. Default:
    /// [`InstallOutcome::NotImplemented`] (no plugin). A `claude`-absent / CLI error is
    /// surfaced as [`InstallOutcome::Failed`] (no fake success — review #35).
    fn install_integration(
        &self,
        resource_dir: Option<&std::path::Path>,
        app_data_dir: Option<&std::path::Path>,
    ) -> InstallOutcome {
        let (Some(descriptor), Some(cli)) = (
            self.plugin_install(resource_dir, app_data_dir),
            self.plugin_cli(),
        ) else {
            return InstallOutcome::NotImplemented;
        };
        match crate::plugin::install_with(&descriptor, cli.as_ref()) {
            Ok(crate::plugin::PluginChange::Unchanged) => InstallOutcome::AlreadyPresent,
            Ok(_) => InstallOutcome::Installed,
            Err(e) => InstallOutcome::Failed(e.to_string()),
        }
    }

    /// Normalize a raw agent payload (the adapter knows its own shape) into an
    /// [`AgentEvent`], or `None` if it is not a recognizable session event. Default:
    /// `None` (a placeholder adapter parses nothing).
    fn parse_event(&self, _payload: &serde_json::Value) -> Option<AgentEvent> {
        None
    }

    /// Build the shell command line that resumes the EXACT session
    /// `external_session_id`, or `None` if this agent cannot resume exactly. Default:
    /// `None`.
    fn build_resume_command(&self, _external_session_id: &str) -> Option<String> {
        None
    }

    /// Can this agent resume a SPECIFIC past session by its exact id (vs. only "most
    /// recent")? Default: `false`.
    fn supports_exact_resume(&self) -> bool {
        false
    }

    /// Does this agent emit a RELIABLE end event nyx can trust to mark a session
    /// `ended`? Default: `false` (SQLite stays the authority; an absent/unreliable
    /// end event just means nyx relies on its own lifecycle, never on the agent).
    fn supports_end_event(&self) -> bool {
        false
    }

    /// The adapter's known risks / limits — the per-adapter limits surfaced to the
    /// rest of nyx. Default: none.
    fn known_risks(&self) -> Vec<KnownRisk> {
        Vec::new()
    }
}

/// A placeholder adapter for a REPRESENTABLE-but-not-production agent kind
/// (`codex`, `opencode`, `custom`). It is addressable through the registry and
/// declares its single limit — "no production adapter in v1" — but supports no
/// capture/resume. Codex / OpenCode are validated by the spikes of PRD-6 before any
/// production adapter ships.
pub struct GenericAdapter {
    kind: &'static str,
}

impl GenericAdapter {
    pub fn new(kind: &'static str) -> Self {
        GenericAdapter { kind }
    }
}

impl AgentAdapter for GenericAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn known_risks(&self) -> Vec<KnownRisk> {
        vec![KnownRisk {
            code: "no_production_adapter_v1",
            detail: "This agent kind is representable but has no production capture/resume \
                     adapter in this PRD (validated separately by the PRD-6 spikes).",
        }]
    }
}

/// The Claude Code adapter (the only v1 PRODUCTION adapter). This task delivers its
/// CONTRACT surface — the resume command, the capability flags, and its known risks —
/// plus the normalized parse of the Claude `SessionStart` / `SessionEnd` payload
/// shape (the canal frozen by ADR-0004). The end-to-end plugin install + the live
/// wiring land in Phase 2.
pub struct ClaudeCodeAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> &'static str {
        db::AGENT_KIND_CLAUDE_CODE
    }

    /// Describe the nyx Claude Code plugin install (PRD-5 phase 2, ADR-0004): the LOCAL
    /// marketplace `nyx` + the `nyx-claude-integration` plugin, registered via the
    /// `claude plugin` CLI from a STABLE app-data copy of the bundled plugin (review
    /// #32/#33) — NOT by hand-editing `~/.claude/settings.json` (the old approach caused
    /// the marketplace cache-miss). The ONE plugin now bundles BOTH the nyx MCP server
    /// (declared in `.mcp.json`, referenced from `plugin.json` — finding #44; connects
    /// as `plugin:nyx-claude-integration:nyx`) AND the SessionStart/SessionEnd hooks (the
    /// SessionEnd `mcp_tool` channel + the SessionStart/SessionEnd `command` curl
    /// fallback) in its bundled `hooks/hooks.json`, auto-loaded by Claude Code. The MCP
    /// port is templated into the copied `.mcp.json` at install time from
    /// [`crate::mcp::resolve_port`]. `resource_dir` lets a packaged build resolve the
    /// bundled SOURCE; `app_data_dir` is where the STABLE copy lands (resolved via Tauri's
    /// path API by the bridge). `settings_path` is used only to strip legacy hand-written
    /// keys on uninstall. Returns `None` when the bundled plugin dir, the stable dir or the
    /// settings path cannot be resolved.
    fn plugin_install(
        &self,
        resource_dir: Option<&std::path::Path>,
        app_data_dir: Option<&std::path::Path>,
    ) -> Option<crate::plugin::PluginInstall> {
        let source_dir = crate::plugin::resolve_bundled_plugin_dir(resource_dir)?;
        let install_dir = crate::plugin::resolve_stable_plugin_dir(app_data_dir)?;
        let settings_path = crate::plugin::claude_settings_path()?;
        Some(crate::plugin::PluginInstall {
            marketplace: crate::plugin::CLAUDE_MARKETPLACE.to_string(),
            plugin: crate::plugin::CLAUDE_PLUGIN_NAME.to_string(),
            source_dir,
            install_dir,
            settings_path,
            // The plugin now bundles the nyx MCP (finding #44); the live loopback port is
            // templated into the copied `.mcp.json` so the plugin-declared http MCP points
            // at nyx's actual port.
            mcp_port: crate::mcp::resolve_port(),
        })
    }

    /// The Claude Code plugin CLI driver: shells out to the `claude plugin` subcommands
    /// (verified against Claude Code 2.1.170). This is the ONE place the Claude CLI
    /// specifics live (review #35); the generic [`crate::plugin`] layer drives it
    /// through the [`crate::plugin::PluginCli`] trait.
    fn plugin_cli(&self) -> Option<Box<dyn crate::plugin::PluginCli>> {
        Some(Box::new(ClaudePluginCli::default()))
    }

    /// A command line is a Claude invocation when its first token is `claude` (or a
    /// path ending in `claude`/`claude.exe`). Heuristic, used only to tag a spawn.
    fn detect(&self, command_line: &str) -> bool {
        let Some(first) = command_line.split_whitespace().next() else {
            return false;
        };
        let name = first.rsplit(['/', '\\']).next().unwrap_or(first);
        let base = name.strip_suffix(".exe").unwrap_or(name);
        base == "claude"
    }

    /// Parse a Claude hook payload. `SessionStart` carries `session_id`, `cwd`,
    /// `transcript_path`, `source`, and `NYX_TERMINAL_ID`; `SessionEnd` carries
    /// `session_id` (+ `reason`). The event kind is read from `hook_event_name`
    /// (`SessionStart` / `SessionEnd`); `source` is stashed in `metadata_json` for
    /// the start (at least `source` per the PRD). Returns `None` for any payload
    /// that is not a recognizable Claude session event or lacks a `session_id`.
    fn parse_event(&self, payload: &serde_json::Value) -> Option<AgentEvent> {
        let event_name = payload.get("hook_event_name")?.as_str()?;
        let session_id = payload.get("session_id")?.as_str()?.to_string();
        if session_id.is_empty() {
            return None;
        }
        match event_name {
            "SessionStart" => {
                let cwd = payload
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let transcript_path = payload
                    .get("transcript_path")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // Stash at least `source` in the adapter metadata bag (PRD: for
                // claude_code we store at least source = startup|resume|clear).
                let metadata_json = payload
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(|src| serde_json::json!({ "source": src }).to_string());
                Some(AgentEvent::Start(SessionStart {
                    external_session_id: session_id,
                    cwd,
                    transcript_path,
                    // Correlation to a terminal/workspace is done by the bridge from
                    // NYX_TERMINAL_ID; the adapter does not resolve workspace_id.
                    workspace_id: None,
                    metadata_json,
                }))
            }
            "SessionEnd" => Some(AgentEvent::End(SessionEnd {
                external_session_id: session_id,
                reason: payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })),
            _ => None,
        }
    }

    /// `claude --resume <id>` — the EXACT-id resume (vs. the ambiguous `--continue`).
    fn build_resume_command(&self, external_session_id: &str) -> Option<String> {
        // The id is interpolated into a shell line that nyx injects verbatim into the
        // resumed terminal, and it arrives over the UNAUTHENTICATED loopback MCP — so it
        // is untrusted. Claude session ids are UUIDs; accept only a safe id charset
        // (`[A-Za-z0-9_-]`) and refuse anything else rather than risk a shell injection
        // (e.g. `x; rm -rf ~`) being parked and run at the next relaunch.
        if external_session_id.is_empty()
            || !external_session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
        Some(format!("claude --resume {external_session_id}"))
    }

    fn supports_exact_resume(&self) -> bool {
        true
    }

    /// Claude emits `SessionEnd`, but it is NOT reliable on a brutal kill (see the
    /// risk below) — so nyx never depends on it (SQLite is the authority). We still
    /// CONSUME it for the clean case, hence `true`.
    fn supports_end_event(&self) -> bool {
        true
    }

    fn known_risks(&self) -> Vec<KnownRisk> {
        vec![
            KnownRisk {
                code: "session_end_unreliable_on_kill",
                detail: "Claude's SessionEnd hook does not fire reliably on SIGKILL / parent \
                         terminal close / app kill; nyx must treat SQLite as the authority and \
                         never depend on SessionEnd for cleanup.",
            },
            KnownRisk {
                code: "resume_target_unix_or_wsl_only",
                detail: "Exact resume now targets a native Linux shell, WSL under Windows, AND \
                         native Windows PowerShell/cmd (finding #83: `claude --resume` is \
                         shell-agnostic and the CR injection from #76 executes it on PSReadLine). \
                         The earlier Windows-native exclusion is lifted; the residual risk is that \
                         native-Windows resume is the least-exercised path (validate in real use).",
            },
        ]
    }
}

/// The Claude Code plugin CLI driver: drives `claude plugin …` to register/install/
/// remove the nyx marketplace + plugin. The subcommands + flags are verified against
/// Claude Code 2.1.170:
/// - `claude plugin marketplace add <dir> --scope user` (idempotent; re-adding at a new
///   path updates the registry entry in place — the self-heal mechanism).
/// - `claude plugin install <plugin>@<marketplace> --scope user` (idempotent).
/// - `claude plugin uninstall <plugin>@<marketplace> --scope user -y` (`-y` required
///   when stdout is not a TTY; best-effort — already-absent exits 0 with a message).
/// - `claude plugin marketplace remove <marketplace> --scope user` (best-effort).
/// - `claude plugin marketplace update <marketplace>` (re-reads the source dir into the
///   marketplace cache — the first step of the forced re-cache, finding #47).
/// - `claude plugin marketplace list --json` → `[{name, source, path, installLocation}]`.
///
/// The binary is resolved off `PATH` (overridable via `NYX_CLAUDE_BIN` for tests/ops); a
/// missing binary surfaces as [`crate::plugin::PluginError::CliNotFound`] so the UI shows
/// an honest typed error rather than a fake success (review #35).
pub struct ClaudePluginCli {
    /// The `claude` binary to invoke. `NYX_CLAUDE_BIN` overrides (test seam / operator);
    /// otherwise the bare `claude` resolved off `PATH`.
    bin: std::ffi::OsString,
}

impl Default for ClaudePluginCli {
    fn default() -> Self {
        let bin = std::env::var_os("NYX_CLAUDE_BIN")
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| std::ffi::OsString::from("claude"));
        ClaudePluginCli { bin }
    }
}

/// Default wall-clock budget for ONE `claude plugin …` subcommand (PRD-5 task #4
/// hardening). `std::process::Command::output()` waits for the child to EXIT with NO
/// timeout, so a hung `claude` (a network round-trip that never returns, a wedged
/// child) would block the caller — and, under Electron, the napi worker thread —
/// FOREVER. We instead spawn the child and wait at most this long, killing it and
/// returning [`crate::plugin::PluginError::Timeout`] on expiry. 30s is generous for the
/// install/uninstall/list round-trips while bounding a true stall. Overridable via
/// `NYX_CLAUDE_TIMEOUT_SECS` (operator / test seam; `0` or unparseable → the default).
const CLAUDE_CLI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How often the bounded wait polls `try_wait` while the child runs. Small enough that
/// a fast subcommand returns promptly, large enough that the poll loop costs nothing.
const CLAUDE_CLI_POLL: std::time::Duration = std::time::Duration::from_millis(25);

impl ClaudePluginCli {
    /// Resolve the wall-clock timeout for one subcommand: `NYX_CLAUDE_TIMEOUT_SECS` when
    /// set to a positive integer, else [`CLAUDE_CLI_TIMEOUT`].
    fn timeout() -> std::time::Duration {
        std::env::var("NYX_CLAUDE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .map(std::time::Duration::from_secs)
            .unwrap_or(CLAUDE_CLI_TIMEOUT)
    }

    /// Run `claude <args>` with a WALL-CLOCK TIMEOUT (PRD-5 task #4), capturing status +
    /// output. Maps a missing binary (`NotFound`) to
    /// [`crate::plugin::PluginError::CliNotFound`]. `tolerate_failure` returns `Ok` even
    /// on a non-zero exit (best-effort remove/uninstall). A child that does not exit
    /// within the budget is KILLED and reported as
    /// [`crate::plugin::PluginError::Timeout`] — so a frozen `claude` can never freeze
    /// the app (the `.output()` trap this hardens).
    ///
    /// stdout/stderr are drained on dedicated threads BEFORE the wait so a chatty child
    /// that fills a pipe buffer cannot deadlock against our bounded wait (the classic
    /// `wait`-with-unread-pipes hang) — we never block on a pipe, only on the bounded
    /// `try_wait` poll.
    fn run(
        &self,
        args: &[&str],
        tolerate_failure: bool,
    ) -> Result<std::process::Output, crate::plugin::PluginError> {
        use std::io::Read as _;
        use std::process::Stdio;

        // Build via the centralized hardened-spawn helper so the `claude` CLI shell-out
        // never flashes a Windows console (the dogfood finding on integration install).
        let mut child = match crate::proc_util::command(&self.bin)
            .args(args)
            // Non-interactive: no TTY, so the CLI never prompts.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(crate::plugin::PluginError::CliNotFound);
            }
            Err(e) => return Err(crate::plugin::PluginError::Io(e)),
        };

        // Drain both pipes on threads so a full pipe buffer never wedges the child while
        // we wait (and so we still capture whatever it wrote before a timeout kill).
        let drain = |pipe: Option<std::process::ChildStdout>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                buf
            })
        };
        let drain_err = |pipe: Option<std::process::ChildStderr>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                buf
            })
        };
        let stdout_h = drain(child.stdout.take());
        let stderr_h = drain_err(child.stderr.take());

        // Bounded wait: poll `try_wait` until the child exits or the deadline passes.
        let timeout = Self::timeout();
        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        // The child is stuck: kill it (and reap) so no zombie is left,
                        // then report a typed Timeout IMMEDIATELY. We do NOT join the
                        // drain threads here: a surviving GRANDCHILD (e.g. a `.cmd`
                        // wrapper's child that inherited the stdout pipe) can hold the
                        // pipe open past our kill, so `read_to_end` would block — exactly
                        // the hang this hardening exists to avoid. The drain threads are
                        // left detached; they finish when the pipe finally closes and
                        // their small buffers are dropped. The caller returns at once.
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(crate::plugin::PluginError::Timeout {
                            command: args.join(" "),
                            after_secs: timeout.as_secs(),
                        });
                    }
                    std::thread::sleep(CLAUDE_CLI_POLL);
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(crate::plugin::PluginError::Io(e));
                }
            }
        };

        let stdout = stdout_h.join().unwrap_or_default();
        let stderr = stderr_h.join().unwrap_or_default();
        let out = std::process::Output {
            status,
            stdout,
            stderr,
        };

        if out.status.success() || tolerate_failure {
            Ok(out)
        } else {
            let mut msg = String::from_utf8_lossy(&out.stderr).into_owned();
            if msg.trim().is_empty() {
                msg = String::from_utf8_lossy(&out.stdout).into_owned();
            }
            Err(crate::plugin::PluginError::CliFailed {
                command: args.join(" "),
                output: msg,
            })
        }
    }
}

impl crate::plugin::PluginCli for ClaudePluginCli {
    fn marketplace_add(&self, dir: &std::path::Path) -> Result<(), crate::plugin::PluginError> {
        let dir = dir.to_string_lossy();
        self.run(
            &["plugin", "marketplace", "add", &dir, "--scope", "user"],
            false,
        )?;
        Ok(())
    }

    fn install(&self, install_id: &str) -> Result<(), crate::plugin::PluginError> {
        self.run(&["plugin", "install", install_id, "--scope", "user"], false)?;
        Ok(())
    }

    fn uninstall(&self, install_id: &str) -> Result<(), crate::plugin::PluginError> {
        // Best-effort: "not found in installed plugins" is acceptable (exits 0 anyway).
        self.run(
            &["plugin", "uninstall", install_id, "--scope", "user", "-y"],
            true,
        )?;
        Ok(())
    }

    fn marketplace_remove(&self, marketplace: &str) -> Result<(), crate::plugin::PluginError> {
        // Best-effort: "not found" is acceptable.
        self.run(
            &[
                "plugin",
                "marketplace",
                "remove",
                marketplace,
                "--scope",
                "user",
            ],
            true,
        )?;
        Ok(())
    }

    fn marketplace_update(&self, marketplace: &str) -> Result<(), crate::plugin::PluginError> {
        // Refresh Claude's marketplace cache from the source dir (finding #47). Verified
        // against 2.1.170: `claude plugin marketplace update <name>` re-reads the dir (a
        // plain `marketplace add` on an existing entry does not), so the subsequent
        // uninstall+reinstall pulls the NEW content rather than the stale marketplace cache.
        self.run(&["plugin", "marketplace", "update", marketplace], false)?;
        Ok(())
    }

    fn marketplace_list(
        &self,
    ) -> Result<Vec<crate::plugin::MarketplaceEntry>, crate::plugin::PluginError> {
        let out = self.run(&["plugin", "marketplace", "list", "--json"], false)?;
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
        let entries = parsed
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let name = e.get("name")?.as_str()?.to_string();
                        // Only `directory` sources carry a usable path; nyx ships those.
                        let path = e
                            .get("path")
                            .and_then(|p| p.as_str())
                            .map(std::path::PathBuf::from);
                        Some(crate::plugin::MarketplaceEntry { name, path })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(entries)
    }
}

/// The agent-adapter REGISTRY: resolves an `agent_kind` to its adapter. Owns one
/// adapter per representable kind so every `agent_kind` is addressable, with the
/// production [`ClaudeCodeAdapter`] for `claude_code` and a [`GenericAdapter`]
/// placeholder for the rest.
pub struct AgentRegistry {
    adapters: Vec<Box<dyn AgentAdapter>>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        AgentRegistry {
            adapters: vec![
                Box::new(ClaudeCodeAdapter),
                Box::new(GenericAdapter::new(db::AGENT_KIND_CODEX)),
                Box::new(GenericAdapter::new(db::AGENT_KIND_OPENCODE)),
                Box::new(GenericAdapter::new(db::AGENT_KIND_CUSTOM)),
            ],
        }
    }
}

impl AgentRegistry {
    /// The adapter owning `agent_kind`, or `None` if the kind is unknown.
    pub fn get(&self, agent_kind: &str) -> Option<&dyn AgentAdapter> {
        self.adapters
            .iter()
            .find(|a| a.kind() == agent_kind)
            .map(|a| a.as_ref())
    }

    /// Every `agent_kind` the registry knows — the representable set
    /// (`claude_code`, `codex`, `opencode`, `custom`).
    pub fn kinds(&self) -> Vec<&'static str> {
        self.adapters.iter().map(|a| a.kind()).collect()
    }

    /// Best-effort: the adapter whose [`AgentAdapter::detect`] matches `command_line`,
    /// if any. The production tag-a-spawn path (`claude` → `claude_code`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn detect(&self, command_line: &str) -> Option<&dyn AgentAdapter> {
        self.adapters
            .iter()
            .find(|a| a.detect(command_line))
            .map(|a| a.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All four agent kinds are REPRESENTABLE: the registry resolves each, and the
    /// set it reports is exactly the db vocabulary.
    #[test]
    fn all_four_agent_kinds_are_representable() {
        let reg = AgentRegistry::default();
        for kind in [
            db::AGENT_KIND_CLAUDE_CODE,
            db::AGENT_KIND_CODEX,
            db::AGENT_KIND_OPENCODE,
            db::AGENT_KIND_CUSTOM,
        ] {
            let adapter = reg.get(kind);
            assert!(
                adapter.is_some(),
                "{kind} must be representable in the registry"
            );
            assert_eq!(
                adapter.unwrap().kind(),
                kind,
                "registry keyed by agent_kind"
            );
        }
        let mut kinds = reg.kinds();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            vec!["claude_code", "codex", "custom", "opencode"],
            "registry exposes exactly the four representable kinds"
        );
    }

    /// An unknown kind resolves to None (the registry is closed over the vocabulary).
    #[test]
    fn unknown_kind_is_not_resolvable() {
        let reg = AgentRegistry::default();
        assert!(reg.get("not_an_agent").is_none());
    }

    /// PRD-5 task #4 hardening: `NYX_CLAUDE_TIMEOUT_SECS` overrides the wall-clock
    /// budget; a missing/zero/garbage value falls back to the 30s default.
    #[test]
    fn claude_cli_timeout_honors_env_override() {
        let _g = crate::CLAUDE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NYX_CLAUDE_TIMEOUT_SECS", "7");
        assert_eq!(
            ClaudePluginCli::timeout(),
            std::time::Duration::from_secs(7)
        );
        std::env::set_var("NYX_CLAUDE_TIMEOUT_SECS", "0"); // 0 → default (not "no wait")
        assert_eq!(ClaudePluginCli::timeout(), CLAUDE_CLI_TIMEOUT);
        std::env::set_var("NYX_CLAUDE_TIMEOUT_SECS", "garbage");
        assert_eq!(ClaudePluginCli::timeout(), CLAUDE_CLI_TIMEOUT);
        std::env::remove_var("NYX_CLAUDE_TIMEOUT_SECS");
        assert_eq!(ClaudePluginCli::timeout(), CLAUDE_CLI_TIMEOUT);
    }

    /// PRD-5 task #4 CORE GUARANTEE: a `claude` that FREEZES does not freeze the app —
    /// `run` kills the stuck child and returns `PluginError::Timeout` WELL within the
    /// would-be hang, instead of blocking forever like the old `.output()`.
    ///
    /// We point `NYX_CLAUDE_BIN` at a tiny script that sleeps far longer than the
    /// (overridden, 1s) timeout, ignoring its args, then assert the call returns a
    /// `Timeout` error in a bounded window. Cross-platform: a `.cmd` on Windows, a `sh`
    /// script elsewhere. On a host with no usable shell to run the script the spawn
    /// itself is what we still bound — but the common case exercises the real kill path.
    #[test]
    fn run_times_out_a_frozen_cli_instead_of_blocking_forever() {
        let _g = crate::CLAUDE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Write a platform-appropriate "hang ignoring args" script to a unique temp file.
        let dir = std::env::temp_dir().join(format!("nyx-claude-hang-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("mk temp dir");
        let script = if cfg!(windows) {
            let p = dir.join("hang.cmd");
            // `timeout`/`ping` hang for ~30s ignoring the plugin args appended after it.
            std::fs::write(&p, "@echo off\r\nping -n 30 127.0.0.1 >NUL\r\n").expect("write cmd");
            p
        } else {
            let p = dir.join("hang.sh");
            std::fs::write(&p, "#!/bin/sh\nsleep 30\n").expect("write sh");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            p
        };

        std::env::set_var("NYX_CLAUDE_BIN", &script);
        std::env::set_var("NYX_CLAUDE_TIMEOUT_SECS", "1");

        let cli = ClaudePluginCli::default();
        let started = std::time::Instant::now();
        let result = cli.run(&["plugin", "install", "x@y", "--scope", "user"], false);
        let elapsed = started.elapsed();

        std::env::remove_var("NYX_CLAUDE_BIN");
        std::env::remove_var("NYX_CLAUDE_TIMEOUT_SECS");
        let _ = std::fs::remove_dir_all(&dir);

        // It must have RETURNED (not hung) and within a bound far below the 30s sleep.
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "run did not return promptly on a frozen CLI: took {elapsed:?}"
        );
        match result {
            Err(crate::plugin::PluginError::Timeout { after_secs, .. }) => {
                assert_eq!(after_secs, 1, "Timeout carries the budget it bounded to");
            }
            // If the platform could not actually launch the script (no shell), the spawn
            // erred FAST — still not a hang, which is the property under test. A success
            // would mean the script did not hang (wrong fixture), so reject it.
            Err(crate::plugin::PluginError::CliNotFound)
            | Err(crate::plugin::PluginError::Io(_)) => {}
            other => panic!("expected a Timeout (or fast spawn error), got {other:?}"),
        }
    }

    /// Per-adapter LIMITS can be exposed: every adapter answers the capability flags
    /// and `known_risks`. claude_code declares exact-resume + its kill/Windows risks;
    /// the placeholder kinds declare the "no production adapter" limit.
    #[test]
    fn per_adapter_limits_are_exposable() {
        let reg = AgentRegistry::default();

        let claude = reg.get(db::AGENT_KIND_CLAUDE_CODE).unwrap();
        assert!(
            claude.supports_exact_resume(),
            "claude supports exact resume"
        );
        assert!(
            claude.supports_end_event(),
            "claude emits an end event (consumed)"
        );
        let codes: Vec<&str> = claude.known_risks().iter().map(|r| r.code).collect();
        assert!(
            codes.contains(&"session_end_unreliable_on_kill"),
            "claude exposes the SessionEnd-on-kill limit"
        );
        assert!(
            codes.contains(&"resume_target_unix_or_wsl_only"),
            "claude exposes the resume-target limit"
        );

        for kind in [
            db::AGENT_KIND_CODEX,
            db::AGENT_KIND_OPENCODE,
            db::AGENT_KIND_CUSTOM,
        ] {
            let a = reg.get(kind).unwrap();
            assert!(
                !a.supports_exact_resume(),
                "{kind} has no production resume in v1"
            );
            let risk_codes: Vec<&str> = a.known_risks().iter().map(|r| r.code).collect();
            assert!(
                risk_codes.contains(&"no_production_adapter_v1"),
                "{kind} exposes its 'no production adapter' limit"
            );
        }
    }

    /// claude_code builds the EXACT-id resume command (`claude --resume <id>`), and
    /// refuses an empty id. Placeholder adapters build nothing.
    #[test]
    fn claude_builds_exact_resume_command() {
        let reg = AgentRegistry::default();
        let claude = reg.get(db::AGENT_KIND_CLAUDE_CODE).unwrap();
        assert_eq!(
            claude.build_resume_command("abc-123").as_deref(),
            Some("claude --resume abc-123"),
            "exact resume uses the precise id, not --continue"
        );
        assert_eq!(
            claude.build_resume_command("").as_deref(),
            None,
            "empty id → no command"
        );

        // An id with shell metacharacters (untrusted via the loopback MCP) is REFUSED —
        // it would otherwise be injected verbatim into the resumed shell.
        for danger in ["x; rm -rf ~", "$(id)", "a b", "a`whoami`", "a|b", "a&&b"] {
            assert_eq!(
                claude.build_resume_command(danger).as_deref(),
                None,
                "unsafe id {danger:?} must not build a resume command"
            );
        }

        let codex = reg.get(db::AGENT_KIND_CODEX).unwrap();
        assert_eq!(
            codex.build_resume_command("abc").as_deref(),
            None,
            "placeholder builds nothing"
        );
    }

    /// claude_code normalizes a SessionStart payload into a Start event, stashing
    /// `source` in the metadata bag and capturing id / cwd / transcript_path.
    #[test]
    fn claude_parses_session_start() {
        let claude = ClaudeCodeAdapter;
        let payload = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": "sid-1",
            "cwd": "/work/proj",
            "transcript_path": "/home/u/.claude/projects/h/sid-1.jsonl",
            "source": "startup",
            "NYX_TERMINAL_ID": "term-xyz"
        });
        let ev = claude
            .parse_event(&payload)
            .expect("recognized SessionStart");
        match ev {
            AgentEvent::Start(s) => {
                assert_eq!(s.external_session_id, "sid-1");
                assert_eq!(s.cwd, "/work/proj");
                assert_eq!(
                    s.transcript_path.as_deref(),
                    Some("/home/u/.claude/projects/h/sid-1.jsonl")
                );
                assert_eq!(s.metadata_json.as_deref(), Some(r#"{"source":"startup"}"#));
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// claude_code normalizes a SessionEnd payload into an End event carrying the id AND
    /// the `reason` — `clear` is an INTERNAL transition (a `/clear` immediately re-opens a
    /// session), so the common layer keeps the row active rather than blinking the icon.
    #[test]
    fn claude_parses_session_end() {
        let claude = ClaudeCodeAdapter;
        let payload = serde_json::json!({
            "hook_event_name": "SessionEnd",
            "session_id": "sid-2",
            "reason": "clear"
        });
        match claude.parse_event(&payload) {
            Some(AgentEvent::End(e)) => {
                assert_eq!(e.external_session_id, "sid-2");
                assert_eq!(e.reason.as_deref(), Some("clear"));
                assert!(
                    e.is_internal_transition(),
                    "clear is an internal transition — the session is kept active"
                );
            }
            other => panic!("expected End, got {other:?}"),
        }
    }

    /// The end-reason classifier: only `clear`/`resume` are internal transitions (the
    /// session is replaced in place); every real end reason — and an absent reason — is
    /// a genuine end that vacates the active slot.
    #[test]
    fn session_end_reason_classifies_internal_transitions() {
        let mk = |reason: Option<&str>| SessionEnd {
            external_session_id: "x".to_string(),
            reason: reason.map(str::to_string),
        };
        assert!(mk(Some("clear")).is_internal_transition());
        assert!(mk(Some("resume")).is_internal_transition());
        for real in ["logout", "prompt_input_exit", "bypass_permissions_disabled", "other"] {
            assert!(
                !mk(Some(real)).is_internal_transition(),
                "{real} is a real end"
            );
        }
        assert!(
            !mk(None).is_internal_transition(),
            "an absent reason is treated as a real end (safe default)"
        );
    }

    /// A payload that is not a recognizable session event (or lacks a session_id)
    /// parses to None — the contract's error/unknown path.
    #[test]
    fn claude_rejects_unknown_or_incomplete_payloads() {
        let claude = ClaudeCodeAdapter;
        // Unknown hook.
        assert!(claude
            .parse_event(&serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "x"}))
            .is_none());
        // Missing session_id.
        assert!(claude
            .parse_event(&serde_json::json!({"hook_event_name": "SessionStart"}))
            .is_none());
        // Empty session_id.
        assert!(claude
            .parse_event(&serde_json::json!({"hook_event_name": "SessionStart", "session_id": ""}))
            .is_none());
        // Not even an object with the keys.
        assert!(claude.parse_event(&serde_json::json!("nope")).is_none());
    }

    /// `detect` tags a `claude` spawn to the claude_code adapter (via the registry),
    /// matching a bare name or a path, and never a non-claude command.
    #[test]
    fn detect_routes_claude_invocations() {
        let reg = AgentRegistry::default();
        for cmd in [
            "claude",
            "claude --resume x",
            "/usr/bin/claude",
            "C:\\bin\\claude.exe -p",
        ] {
            let a = reg.detect(cmd);
            assert!(a.is_some(), "{cmd} should detect an adapter");
            assert_eq!(
                a.unwrap().kind(),
                db::AGENT_KIND_CLAUDE_CODE,
                "{cmd} → claude_code"
            );
        }
        assert!(
            reg.detect("vim file.txt").is_none(),
            "non-claude detects nothing"
        );
        assert!(reg.detect("").is_none(), "empty command detects nothing");
    }

    /// install_integration has a typed result for every adapter. The placeholder
    /// kinds stay NotImplemented (they ship no plugin / no CLI); claude_code drives the
    /// real CLI-backed install (covered against a clean scratch config in the integration
    /// validation, not here — this unit only asserts the contract surface). The
    /// placeholder check needs no env redirection (they never touch a config / never
    /// shell out).
    #[test]
    fn install_integration_is_typed_for_placeholder_adapters() {
        let reg = AgentRegistry::default();
        for kind in [
            db::AGENT_KIND_CODEX,
            db::AGENT_KIND_OPENCODE,
            db::AGENT_KIND_CUSTOM,
        ] {
            assert_eq!(
                reg.get(kind).unwrap().install_integration(None, None),
                InstallOutcome::NotImplemented,
                "no production integration in this PRD for {kind}"
            );
            assert!(
                reg.get(kind).unwrap().plugin_install(None, None).is_none(),
                "{kind} ships no bundled plugin"
            );
            assert!(
                reg.get(kind).unwrap().plugin_cli().is_none(),
                "{kind} has no plugin CLI driver"
            );
        }
    }

    /// claude_code builds a complete CLI-driven plugin descriptor (review #32/#33): a
    /// stable install dir under the given app-data dir (NOT the volatile bundled source),
    /// the bundled source dir, and a settings path for legacy cleanup. Resolves entirely
    /// from injected env so it never touches the real `~/.claude` and never shells out.
    #[test]
    fn claude_plugin_install_targets_a_stable_dir() {
        // Resolves entirely from injected `NYX_CLAUDE_*` seams (process-global env), so it
        // takes the ONE crate-wide seam lock every env-mutating test shares (review
        // #42/#43) — NOT a private mutex — so a seam mutation in bridge/plugin/onboarding
        // can never interleave mid-resolution. Prior values restored on exit (no leak).
        let _guard = crate::CLAUDE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prev_plugin_dir = std::env::var_os("NYX_CLAUDE_PLUGIN_DIR");
        let prev_settings = std::env::var_os("NYX_CLAUDE_SETTINGS");
        let prev_stable = std::env::var_os("NYX_CLAUDE_STABLE_PLUGIN_DIR");

        let mut dir = std::env::temp_dir();
        dir.push(format!("nyx-agent-desc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A fake bundled plugin dir (just the manifest the resolver checks for).
        let plugin_dir = dir.join("claude-plugin");
        std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            plugin_dir.join(".claude-plugin").join("marketplace.json"),
            "{}",
        )
        .unwrap();
        let settings = dir.join("settings.json");
        let app_data = dir.join("appdata");

        std::env::set_var("NYX_CLAUDE_PLUGIN_DIR", &plugin_dir);
        std::env::set_var("NYX_CLAUDE_SETTINGS", &settings);
        std::env::remove_var("NYX_CLAUDE_STABLE_PLUGIN_DIR");

        let adapter = ClaudeCodeAdapter;
        let desc = adapter
            .plugin_install(None, Some(&app_data))
            .expect("descriptor resolves");
        assert_eq!(desc.marketplace, "nyx");
        assert_eq!(desc.plugin, "nyx-claude-integration");
        assert_eq!(desc.install_id(), "nyx-claude-integration@nyx");
        // The STABLE install dir lives under app-data, NOT the volatile bundled source.
        assert_eq!(desc.install_dir, app_data.join("claude-plugin"));
        assert_ne!(
            desc.install_dir, desc.source_dir,
            "registered path is stable, not the source"
        );
        assert_eq!(desc.source_dir, plugin_dir);
        // The adapter ships a real CLI driver.
        assert!(adapter.plugin_cli().is_some());

        restore_env("NYX_CLAUDE_PLUGIN_DIR", prev_plugin_dir);
        restore_env("NYX_CLAUDE_SETTINGS", prev_settings);
        restore_env("NYX_CLAUDE_STABLE_PLUGIN_DIR", prev_stable);
    }

    /// A `claude` binary that does not exist surfaces as a typed [`InstallOutcome::Failed`]
    /// — never a fake success (review #35). Points `NYX_CLAUDE_BIN` at a non-existent
    /// binary so the CLI driver fails fast with `CliNotFound`.
    #[test]
    fn claude_install_integration_fails_when_cli_absent() {
        // Drives the `NYX_CLAUDE_BIN`/`NYX_CLAUDE_PLUGIN_DIR`/`NYX_CLAUDE_SETTINGS` seams
        // (process-global env), so it takes the ONE crate-wide seam lock every env-mutating
        // test shares (review #42/#43). Prior values restored on exit (no leak).
        let _guard = crate::CLAUDE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prev_plugin_dir = std::env::var_os("NYX_CLAUDE_PLUGIN_DIR");
        let prev_settings = std::env::var_os("NYX_CLAUDE_SETTINGS");
        let prev_bin = std::env::var_os("NYX_CLAUDE_BIN");

        let mut dir = std::env::temp_dir();
        dir.push(format!("nyx-agent-nocli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plugin_dir = dir.join("claude-plugin");
        std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            plugin_dir.join(".claude-plugin").join("marketplace.json"),
            "{}",
        )
        .unwrap();

        std::env::set_var("NYX_CLAUDE_PLUGIN_DIR", &plugin_dir);
        std::env::set_var("NYX_CLAUDE_SETTINGS", dir.join("settings.json"));
        std::env::set_var("NYX_CLAUDE_BIN", dir.join("definitely-not-claude-binary"));

        let adapter = ClaudeCodeAdapter;
        let outcome = adapter.install_integration(None, Some(&dir.join("appdata")));
        match outcome {
            InstallOutcome::Failed(msg) => assert!(
                msg.contains("claude"),
                "absent CLI → actionable error: {msg}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }

        restore_env("NYX_CLAUDE_PLUGIN_DIR", prev_plugin_dir);
        restore_env("NYX_CLAUDE_SETTINGS", prev_settings);
        restore_env("NYX_CLAUDE_BIN", prev_bin);
    }

    /// Restore a previously-captured env var value: set it back if it was present, else
    /// remove it. Used by the seam-mutating tests so they never leak a `NYX_CLAUDE_*`
    /// override into other tests sharing the process (review #42/#43).
    fn restore_env(key: &str, prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
