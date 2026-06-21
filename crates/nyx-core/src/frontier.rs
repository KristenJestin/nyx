//! The four explicit frontiers between `nyx-core` and any shell.
//!
//! `nyx-core` is shell-agnostic: it never names a Tauri or Electron type. Instead a
//! shell adapts to the core across these four seams. Three are Rust traits the shell
//! implements ([`EventSink`], [`AppPaths`], [`Lifecycle`]); the fourth — the service
//! / state container — is a pattern the shell owns (it holds the long-lived state the
//! core defines and hands the core typed access; the [`crate::mcp::ToolDispatcher`]
//! trait is its abstract call-in seam).
//!
//! None of these traits mention a shell type, so the SAME core drives the Tauri
//! adapter (`app.emit` / `app.path()`), the Electron core-host (an IPC channel /
//! resolved paths over napi) and the in-process test fakes interchangeably.

use std::path::PathBuf;

/// Frontier 1 — **events out**. The core pushes UI / observer notifications OUT
/// through a sink the shell implements; the core never reaches into a shell event
/// bus directly.
///
/// The umbrella over the already-extracted [`crate::command::RunnerSink`] (the
/// managed-command-runner slice) and the interactive-terminal pump's emissions
/// (`pty://output`, `pty://exit`) and the coarse "X changed" invalidations the UI
/// re-fetches on. A shell maps each call to its transport:
/// * Tauri adapter → `app.emit(<channel>, payload)`.
/// * Electron core-host → a structured message on the host↔main IPC channel.
/// * tests → a recording fake.
///
/// Bytes are passed borrowed (`&[u8]`) so the sink decides whether to copy; ordering
/// of `pty_output` for a given `terminal_id` is the shell transport's responsibility
/// to preserve (the core emits in order).
pub trait EventSink: Send + Sync + 'static {
    /// A chunk of interactive-terminal output for `terminal_id` (channel
    /// `pty://output`). Ordered per terminal.
    fn pty_output(&self, terminal_id: &str, bytes: &[u8]);
    /// The interactive terminal `terminal_id` exited with `code` (channel
    /// `pty://exit`).
    fn pty_exit(&self, terminal_id: &str, code: Option<i32>);
    /// A coarse-grained "this collection changed, re-fetch it" invalidation. `topic`
    /// is one of the stable change channels (`terminals`, `workspaces`, `commands`,
    /// `agent-sessions`); the shell maps it to its concrete event name.
    fn changed(&self, topic: ChangedTopic);
}

/// The stable set of "X changed → re-fetch" invalidation topics carried by
/// [`EventSink::changed`]. A closed enum (not a free string) so a new topic is a
/// compile error in every shell adapter rather than a silently-dropped event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedTopic {
    Terminals,
    Workspaces,
    Commands,
    AgentSessions,
}

/// Frontier 2 — **paths in**. The two filesystem locations the core needs, resolved
/// by the shell instead of the core calling a Tauri `app.path()`.
///
/// * `data_dir` — the per-user writable dir that holds `nyx.db`, `integrations.json`
///   and any other state. Honors the `NYX_DATA_DIR` override at the shell layer
///   (the e2e suite pins it); the core just consumes the resolved path.
/// * `resource_dir` — the read-only bundled-resources dir (the packaged
///   `claude-plugin`), if the shell has one (`None` in a bare dev/test run).
pub trait AppPaths: Send + Sync + 'static {
    /// The writable per-user data directory (created by the shell if missing).
    fn data_dir(&self) -> PathBuf;
    /// The read-only bundled-resource directory, if any.
    fn resource_dir(&self) -> Option<PathBuf>;
}

/// Frontier 4 — the **boot / shutdown lifecycle**. Names the ordered steps a shell
/// runs to bring the core up and take it down. The work of each step is a Tauri-free
/// function in its owning module ([`crate::db::Db::open`] + migrate, the
/// [`crate::command`] restore/normalize/snapshot helpers, [`crate::agent_resume`]
/// resume scan, [`crate::mcp::McpServer::start`]); this trait is the documented
/// sequence the shell drives, so the Tauri adapter and the Electron core-host boot
/// the SAME core in the SAME order.
///
/// It is intentionally a thin marker over those free functions rather than a fat
/// god-object: the state container (frontier 3) is shell-owned, so the concrete boot
/// closure lives in the shell where it can `manage` the resulting services.
pub trait Lifecycle {
    /// Open + migrate the DB, restore the last shutdown's running commands, normalize
    /// any terminal stuck at `running`, park agent-session resumes, and start the MCP
    /// server. Returns once the core is ready to serve.
    fn boot(&mut self) -> anyhow::Result<()>;
    /// Snapshot which command instances are running (so the next boot can relaunch
    /// exactly those) and tree-kill the live processes so nothing is orphaned past
    /// exit. Idempotent / latched: safe to call on both close-request and destroy.
    fn shutdown(&mut self);
}
