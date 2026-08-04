//! nyx-core — the shell-agnostic core of nyx.
//!
//! This crate owns every piece of nyx's behaviour that does NOT depend on the
//! windowing shell: the interactive PTY ([`pty`]) and managed-command runtime
//! ([`command`]), the OSC 7 / OSC 133 parsers ([`osc7`], [`osc133`]) and shell
//! integration ([`shellinteg`]), the SQLite persistence layer ([`db`] + the diesel
//! [`schema`] and the embedded `migrations/`), the MCP server + tool-dispatch
//! contract ([`mcp`]), the agent-session model ([`agent`], [`agent_resume`]), the
//! agent-plugin install vehicle ([`plugin`], [`onboarding`]) and the path / package
//! helpers ([`pathnorm`], [`resolve`], [`subfolder`], [`pkgjson`], [`portless`],
//! [`proc`]).
//!
//! **No Tauri or Electron type ever crosses this crate's public API.** A shell
//! (the Tauri adapter in `apps/tauri/src-tauri`, the Electron core-host via
//! `crates/nyx-napi`) adapts to the core through four explicit, documented
//! frontiers — see [`frontier`]:
//!
//! 1. [`frontier::EventSink`] — the core pushes UI/observer events (pty output,
//!    pty exit, command state/output/ack, "X changed" invalidations) OUT through a
//!    sink the shell implements (Tauri: `app.emit`; Electron: an IPC channel). The
//!    pre-existing [`command::RunnerSink`] is the managed-command-runner slice of
//!    this contract; `EventSink` is the umbrella the shell implements.
//! 2. [`frontier::AppPaths`] — the core asks the shell for the two paths it needs
//!    (`data_dir`, `resource_dir`) instead of calling a Tauri `app.path()`.
//! 3. The **service / state container** — the long-lived runtime state (the PTY
//!    manager, the command runner, the per-terminal caches, the OSC pending maps,
//!    the resume parks). The core defines the state types; the shell OWNS the
//!    container and hands the core typed access. The [`mcp::ToolDispatcher`] trait
//!    is the abstract seam the MCP server calls into — the shell's concrete
//!    dispatcher resolves services from its own container.
//! 4. The **`boot` / `shutdown` lifecycle** — [`frontier::Lifecycle`] names the
//!    ordered boot steps (open DB + migrate, restore commands, normalize terminals,
//!    resume agent sessions, start the MCP server) and the shutdown step (snapshot
//!    running commands + reap). The free functions that implement each step live in
//!    their owning module ([`db::Db::open`], [`command`] restore/snapshot, …) and
//!    are Tauri-free; the shell sequences them.

#![cfg_attr(not(test), allow(dead_code))]

pub mod agent;
pub mod agent_activity;
pub mod agent_liveness;
pub mod agent_resume;
pub mod agent_unload;
pub mod ansi;
pub mod command;
pub mod css_slot;
pub mod db;
pub mod design;
mod design_axes;
pub mod design_tokens;
pub mod frontier;
pub mod git_diff;
pub mod git_views;
pub mod git_watch;
pub mod idea;
pub mod integrations;
pub mod mcp;
pub mod mcp_context;
pub mod mcp_runtime;
pub mod mcp_tools_annexe;
pub mod mcp_tools_core;
pub mod mcp_tools_design;
pub mod mcp_tools_idea;
pub mod mcp_tools_proto;
pub mod mcp_tools_spec;
pub mod onboarding;
pub mod osc133;
pub mod osc7;
pub mod pathnorm;
pub mod pkgjson;
pub mod plugin;
pub mod portless;
#[cfg(target_os = "linux")]
pub mod proc;
/// Per-terminal CPU%/RAM over a process tree (FEEDBACK #28). Cross-platform via
/// `sysinfo` (Linux/macOS/Windows) — NOT `/proc`, unlike [`proc`].
pub mod proc_stats;
pub mod proc_util;
pub mod pty;
pub mod render;
pub mod repository;
pub mod resolve;
pub mod review_comment;
pub mod schema;
pub mod shellinteg;
pub mod spec_activity;
pub mod subfolder;
pub mod usecase;
pub mod vt_screen;
pub mod workspace_git;
pub mod workspace_runtime;
pub mod workspace_timeline;

/// GATE e2e (v2.5.11 Phase 3, PRD 01KXR1AGWK08H9P800YHXPBCHS) — the full review LOOP, in
/// process, over the real nyx-core surface. Test-only, so it compiles solely under `--lib`.
#[cfg(test)]
mod review_loop_gate;

/// ONE process-global lock for every test that mutates a `NYX_CLAUDE_*` env seam
/// (`NYX_CLAUDE_CONFIG` / `NYX_CLAUDE_SETTINGS` / `NYX_CLAUDE_BIN` /
/// `NYX_CLAUDE_PLUGIN_DIR` / `NYX_CLAUDE_STABLE_PLUGIN_DIR`). These vars are
/// PROCESS-GLOBAL, but `cargo test --lib` runs `#[test]`s in parallel threads in ONE
/// process, so per-module mutexes give NO mutual exclusion across modules: one test's
/// `set_var`/`remove_var` would interleave between another module's `set_var` and its
/// read, flipping the seam mid-resolution and producing non-deterministic reds. Every
/// env-seam-resolving test in `agent`/`plugin`/`onboarding` takes THIS single lock
/// (not a private one), serializing all seam mutation across the whole crate. Poison
/// is recovered: each holder restores/removes the env on exit, so a panicking test
/// never leaves a stale seam.
#[cfg(test)]
pub(crate) static CLAUDE_ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
