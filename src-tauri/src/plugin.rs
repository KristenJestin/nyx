//! Generic agent-plugin install vehicle (PRD-5 phase 2; ADR-0004 / ADR-0010).
//!
//! This module is the **provider-agnostic** layer that installs an agent's
//! session-capture glue as a real, on-disk **plugin** — decoupled from the MCP
//! server install ([`crate::onboarding`]). It carries NO Claude specifics in its
//! logic: an adapter (e.g. [`crate::agent::ClaudeCodeAdapter`]) constructs a
//! [`PluginInstall`] describing *which* marketplace/plugin/source dir/stable dir to
//! wire, and this layer performs the install/reconcile/uninstall generically by
//! driving the agent's own plugin CLI. Adding a future provider = construct a
//! different [`PluginInstall`] + supply a [`PluginCli`]; the copy, reconcile and IO
//! here are reused unchanged (finding #25).
//!
//! ## How an install registers the plugin (verified against Claude Code 2.1.170)
//!
//! The prior approach hand-wrote `~/.claude/settings.json`
//! (`extraKnownMarketplaces.nyx` + `enabledPlugins[...]`). That is BROKEN: it does
//! **not** populate Claude Code's real marketplace registry
//! (`~/.claude/plugins/known_marketplaces.json` + the per-marketplace cache), so a
//! real session fails with `Failed to load marketplace "nyx": cache-miss` and the
//! hooks never load (review 01KVD320). The registry is owned by the **`claude plugin`
//! CLI**, so nyx now drives that CLI instead (review #32):
//!
//! 1. **Copy** the bundled plugin (a Tauri resource dir) into a **stable**,
//!    app-controlled directory ([`PluginInstall::install_dir`], under the Tauri app
//!    data dir) — never the volatile `target/debug/resources` or the packaged
//!    resource dir, which churn across rebuilds/updates and were the root of the
//!    stale-path cache-miss (review #33). The copy is automatic on the install click;
//!    ZERO manual user step.
//! 2. `claude plugin marketplace add <stable-dir> --scope user` — registers the
//!    marketplace in `known_marketplaces.json` (a `directory` source needs no git
//!    cache, so there is no cache-miss).
//! 3. `claude plugin install <plugin>@<marketplace> --scope user` — installs the
//!    plugin (idempotent, non-interactive). The CLI owns `settings.json` /
//!    `enabledPlugins` / `installed_plugins.json`; nyx no longer hand-edits them.
//!
//! The hooks themselves (the `command`/curl session-capture channel — `mcp_tool` hooks
//! are unsupported on `SessionEnd`, finding #54, so both events are command-only) live
//! in the bundled `hooks/hooks.json`, auto-loaded by Claude Code from the plugin's
//! standard hooks path.
//!
//! ## Properties
//! - **Idempotent**: both CLI subcommands are no-ops when already current (they exit 0
//!   with a friendly "already installed" message); a moved stable dir is repaired by a
//!   re-`add` at the new path ([`PluginChange::Updated`]).
//! - **Self-healing**: a stale/dead-path nyx entry (the exact broken state a real user
//!   hit) is repaired by re-copying the content + re-`add`ing at the live stable path —
//!   `marketplace add` overwrites the stale `known_marketplaces.json` entry in place
//!   (review #34).
//! - **Conservative reconcile**: [`reconcile`] refreshes an ALREADY-installed plugin
//!   (re-copies content if it changed, re-registers if the path drifted) but **never
//!   installs a plugin that is absent** — mirroring the MCP server reconcile.
//! - **Legacy cleanup**: uninstall strips any leftover hand-written nyx keys
//!   (`extraKnownMarketplaces.nyx`, `enabledPlugins[...]`) from the old approach.
//!
//! ## Safety / testability
//! The bundled-plugin source dir, the stable install dir and the settings file are all
//! **injectable** (`NYX_CLAUDE_PLUGIN_DIR` / explicit `install_dir` / `NYX_CLAUDE_SETTINGS`),
//! and the CLI is abstracted behind [`PluginCli`] so tests drive a fake recorder
//! instead of the real `claude` binary — the suite never touches the user's real
//! `~/.claude` and never shells out. The legacy-settings strip ([`strip_plugin`]) is a
//! pure JSON op, unit-tested directly.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::onboarding::{read_config_pub, write_config_pub};

/// A provider-agnostic description of a bundled plugin to install/reconcile/uninstall
/// via the agent's plugin CLI. Constructed by the per-agent adapter; the logic in this
/// module is generic over it.
#[derive(Debug, Clone)]
pub struct PluginInstall {
    /// The local marketplace name nyx registers (the `@<name>` half of the
    /// `<plugin>@<marketplace>` install id). Stable per provider.
    pub marketplace: String,
    /// The plugin name as declared in the bundled `plugin.json` (the install id is
    /// `<plugin>@<marketplace>`). Stable per provider.
    pub plugin: String,
    /// Absolute path to the **bundled** plugin directory on disk (the Tauri resource
    /// dir holding `.claude-plugin/marketplace.json`). The COPY SOURCE — read-only,
    /// resolved per-build (dev/packaged); it may move across rebuilds/updates, which is
    /// exactly why it is never the registered path.
    pub source_dir: PathBuf,
    /// Absolute path to the **stable**, app-controlled directory the bundled plugin is
    /// copied INTO and registered FROM (under the Tauri app data dir). Stable across
    /// rebuilds/updates — this is what `claude plugin marketplace add` points at.
    pub install_dir: PathBuf,
    /// The agent's user-scope settings JSON file (Claude Code: `~/.claude/settings.json`).
    /// Used ONLY to strip legacy hand-written nyx keys on uninstall; the CLI owns the
    /// install-time writes. Injectable for tests.
    pub settings_path: PathBuf,
    /// The loopback port the bundled MCP server runs on (`mcp::resolve_port()`). Templated
    /// into the COPIED `.mcp.json` at install/reconcile time (the bundled source keeps the
    /// [`MCP_PORT_PLACEHOLDER`] token instead of a hard-coded port — finding #44), so the
    /// plugin-declared http MCP points at nyx's actual live port.
    pub mcp_port: u16,
}

impl PluginInstall {
    /// The plugin install id for the CLI: `"<plugin>@<marketplace>"`.
    pub fn install_id(&self) -> String {
        format!("{}@{}", self.plugin, self.marketplace)
    }
}

/// The placeholder token the bundled `.mcp.json` carries in place of the MCP port (finding
/// #44). It is substituted with the resolved [`PluginInstall::mcp_port`] when the bundled
/// plugin is COPIED into the stable dir, so the bundled source stays port-agnostic and the
/// copy always reflects nyx's live port. Kept distinct + greppable so the templating is
/// unambiguous.
pub const MCP_PORT_PLACEHOLDER: &str = "__NYX_MCP_PORT__";

/// The bundled MCP descriptor file inside the plugin (`<plugin>/.mcp.json`), referenced
/// from `plugin.json` (`"mcpServers": "./.mcp.json"`). This is the ONE file the port is
/// templated into during the copy.
pub const MCP_DESCRIPTOR_FILE: &str = ".mcp.json";

/// What an [`install`] / [`reconcile`] run changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginChange {
    /// The plugin was absent and was registered (content copied + marketplace added +
    /// plugin installed).
    Added,
    /// The plugin was present but stale (the bundled content changed, or the registered
    /// path had drifted) and was refreshed in place (re-copied + re-registered).
    Updated,
    /// The plugin was already exactly current; nothing was copied or re-registered.
    Unchanged,
}

/// Outcome of a boot-time [`reconcile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The plugin was absent → nothing was written (NEVER install silently).
    SkippedAbsent,
    /// The plugin was present and already current → no copy / no re-register.
    Unchanged,
    /// The plugin was present and was refreshed (content re-copied / path re-registered).
    Updated,
}

/// Errors a plugin install/reconcile/uninstall can fail with. Surfaced to the UI so a
/// missing `claude` binary is an honest, typed error (no fake success — review #35).
#[derive(Debug)]
pub enum PluginError {
    /// The agent's plugin CLI (`claude`) is not on `PATH`. The single most likely
    /// failure; surfaced verbatim to the user so they install / fix `claude`.
    CliNotFound,
    /// A CLI subcommand failed (non-zero exit). Carries the subcommand + captured
    /// stderr/stdout for the UI / logs.
    CliFailed { command: String, output: String },
    /// An IO error while copying the bundled plugin into the stable dir (or reading
    /// the legacy settings file).
    Io(std::io::Error),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::CliNotFound => write!(
                f,
                "the 'claude' CLI was not found on PATH — install Claude Code (or add it to PATH) \
                 to register the nyx session plugin"
            ),
            PluginError::CliFailed { command, output } => {
                write!(f, "`claude {command}` failed: {}", output.trim())
            }
            PluginError::Io(e) => write!(f, "could not copy the bundled nyx plugin: {e}"),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<std::io::Error> for PluginError {
    fn from(e: std::io::Error) -> Self {
        PluginError::Io(e)
    }
}

/// A marketplace entry as reported by `claude plugin marketplace list --json`. Only the
/// fields nyx needs to detect real state + drift: the name and the registered path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketplaceEntry {
    pub name: String,
    /// The registered directory path (for a `directory` source). `None` for non-directory
    /// sources (github/url) — nyx only ships directory sources.
    pub path: Option<PathBuf>,
}

/// The agent's plugin CLI, abstracted so the generic logic drives it without knowing it
/// is `claude`, and so tests inject a fake recorder instead of shelling out. The real
/// impl ([`crate::agent::ClaudePluginCli`]) shells out to the `claude` binary; the
/// Claude specifics (binary name, flags verified against 2.1.170) live there, keeping
/// this module provider-agnostic (review #35).
pub trait PluginCli {
    /// `claude plugin marketplace add <dir> --scope user`. Idempotent; re-adding at a
    /// new path updates the registry entry in place (self-heal).
    fn marketplace_add(&self, dir: &Path) -> Result<(), PluginError>;

    /// `claude plugin install <plugin>@<marketplace> --scope user`. Idempotent.
    fn install(&self, install_id: &str) -> Result<(), PluginError>;

    /// `claude plugin uninstall <plugin>@<marketplace> --scope user -y`. Best-effort;
    /// already-absent is not an error.
    fn uninstall(&self, install_id: &str) -> Result<(), PluginError>;

    /// `claude plugin marketplace remove <marketplace> --scope user`. Best-effort.
    fn marketplace_remove(&self, marketplace: &str) -> Result<(), PluginError>;

    /// `claude plugin marketplace list --json` → the parsed entries. Used to detect the
    /// REAL installed state + path drift (the registry, not nyx's own settings guess).
    fn marketplace_list(&self) -> Result<Vec<MarketplaceEntry>, PluginError>;

    /// `claude plugin marketplace update <marketplace>` — re-read the marketplace SOURCE
    /// dir into Claude's marketplace cache (finding #47). A plain `marketplace add` on an
    /// already-registered entry does NOT re-read the directory, so without this a bundled
    /// plugin version bump never reaches Claude's cache. Idempotent.
    fn marketplace_update(&self, marketplace: &str) -> Result<(), PluginError>;

    /// `claude plugin update <plugin>@<marketplace> --scope user` — move the INSTALLED
    /// plugin to the version now in the (refreshed) marketplace cache (finding #47). The
    /// installed plugin is pinned to a versioned cache dir; `marketplace update` alone does
    /// not move it, so this completes the propagation. Idempotent (no-op when already at
    /// the latest version).
    fn plugin_update(&self, install_id: &str) -> Result<(), PluginError>;
}

/// Recursively copy `src` → `dst`, mirroring the tree (creating `dst`). Used to land the
/// bundled plugin in the stable app-data dir. The bundled `.mcp.json` is PORT-TEMPLATED on
/// the way through (finding #44): its [`MCP_PORT_PLACEHOLDER`] token is replaced with
/// `port` so the COPIED descriptor points at nyx's live loopback port while the bundled
/// source stays port-agnostic. Returns `true` when the copy actually changed the
/// destination (the dst was absent or differed from the desired, port-substituted bytes),
/// `false` when it was already identical — so reconcile detects a content/port change
/// cheaply and an unchanged port is idempotent (no churn).
fn copy_tree(src: &Path, dst: &Path, port: u16) -> std::io::Result<bool> {
    let mut changed = false;
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            if copy_tree(&from, &to, port)? {
                changed = true;
            }
        } else {
            // The .mcp.json descriptor is templated with the live port; every other file
            // is copied byte-for-byte. We compare the DESIRED (post-template) bytes against
            // what is already on disk, so a re-copy with the same port is a no-op.
            let raw = std::fs::read(&from)?;
            let new_bytes = if entry.file_name() == MCP_DESCRIPTOR_FILE {
                template_mcp_port(&raw, port)
            } else {
                raw
            };
            let same = std::fs::read(&to).map(|old| old == new_bytes).unwrap_or(false);
            if !same {
                std::fs::write(&to, &new_bytes)?;
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// Substitute every [`MCP_PORT_PLACEHOLDER`] occurrence in the bundled `.mcp.json` bytes
/// with `port` (finding #44). Operates on bytes (the descriptor is small UTF-8 JSON) so a
/// non-UTF-8 file is passed through untouched rather than panicking. Pure → unit-tested.
fn template_mcp_port(raw: &[u8], port: u16) -> Vec<u8> {
    match std::str::from_utf8(raw) {
        Ok(s) => s.replace(MCP_PORT_PLACEHOLDER, &port.to_string()).into_bytes(),
        Err(_) => raw.to_vec(),
    }
}

/// True when `marketplace` is registered (present in the CLI's marketplace list) AND its
/// registered path equals `expected_dir` (no drift). Both conditions must hold for the
/// install to be "current": a stale/dead path counts as NOT current so reconcile heals it.
fn is_registered_at(entries: &[MarketplaceEntry], marketplace: &str, expected_dir: &Path) -> bool {
    entries
        .iter()
        .find(|e| e.name == marketplace)
        .and_then(|e| e.path.as_deref())
        .map(|p| paths_eq(p, expected_dir))
        .unwrap_or(false)
}

/// Whether `marketplace` is registered at all (regardless of path) — used to distinguish
/// "absent" (reconcile no-op) from "present-but-stale" (reconcile heals).
fn is_registered(entries: &[MarketplaceEntry], marketplace: &str) -> bool {
    entries.iter().any(|e| e.name == marketplace)
}

/// Compare two paths, canonicalizing when possible (so `.../a` and `.../a/` or symlinked
/// equivalents match), falling back to a literal compare when a path does not yet exist.
fn paths_eq(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Install (idempotently) the plugin: copy the bundled content into the stable dir, then
/// drive the CLI to register the marketplace + install the plugin. Returns
/// [`PluginChange`] so the caller can distinguish a fresh `Added` from an idempotent
/// `Unchanged` / in-place `Updated`. The stable copy is the SAME mechanism whether the
/// source moved (dev rebuild / packaged update) or not — the registered path never churns
/// (review #33).
pub fn install_with(install: &PluginInstall, cli: &dyn PluginCli) -> Result<PluginChange, PluginError> {
    let entries = cli.marketplace_list()?;
    let already_current =
        is_registered_at(&entries, &install.marketplace, &install.install_dir);

    // Copy the bundled plugin into the stable, app-controlled dir (zero user step),
    // templating the live MCP port into the copied `.mcp.json` (finding #44). The copy
    // reports whether the content (or port) changed so we can return Updated vs Unchanged.
    let content_changed = copy_tree(&install.source_dir, &install.install_dir, install.mcp_port)?;

    // Register the marketplace + install the plugin. Both are idempotent, so re-running
    // when already current is safe; re-adding at the stable path heals a drifted entry.
    cli.marketplace_add(&install.install_dir)?;
    cli.install(&install.install_id())?;

    // When an ALREADY-current install's stable content changed (a bundled plugin version
    // bump from an app update, or a port re-template), propagate it through Claude's caches
    // (finding #47): refresh the marketplace cache + move the installed plugin to the new
    // version. A plain `marketplace add` does NOT re-read the dir, so without this the bump
    // never lands. A FRESH add needs no propagation — `install` already pulled the current
    // version into the cache.
    if already_current && content_changed {
        propagate_update(install, cli)?;
    }

    Ok(if already_current && !content_changed {
        PluginChange::Unchanged
    } else if already_current {
        PluginChange::Updated
    } else {
        PluginChange::Added
    })
}

/// Propagate a bundled-plugin change through Claude's caches (finding #47), verified
/// against Claude Code 2.1.170: `marketplace update <name>` re-reads the marketplace
/// SOURCE dir into Claude's cache (a plain `marketplace add` on an existing entry does
/// NOT), then `plugin update <plugin>@<marketplace>` moves the installed plugin off its
/// pinned versioned cache dir onto the new version. Both are idempotent, so calling this
/// when nothing actually changed is harmless — but callers gate it on a real content
/// change to avoid needless CLI churn.
fn propagate_update(install: &PluginInstall, cli: &dyn PluginCli) -> Result<(), PluginError> {
    cli.marketplace_update(&install.marketplace)?;
    cli.plugin_update(&install.install_id())?;
    Ok(())
}

/// Uninstall the plugin (the mirror of [`install_with`]): drive the CLI to uninstall the
/// plugin + remove the marketplace, then strip any leftover hand-written nyx keys in the
/// legacy settings file (from the old hand-edit approach — review #34). Best-effort: an
/// already-absent CLI removal is not fatal. Returns whether anything was removed/cleaned.
pub fn remove_with(install: &PluginInstall, cli: &dyn PluginCli) -> Result<bool, PluginError> {
    // CLI removals are best-effort (already-absent is fine — the CLI exits 0 with a
    // "not found" message, which our adapter maps to Ok).
    let _ = cli.uninstall(&install.install_id());
    let _ = cli.marketplace_remove(&install.marketplace);
    // Strip legacy hand-written keys the OLD approach left in settings.json.
    let legacy_cleaned = strip_legacy_settings(install)?;
    Ok(legacy_cleaned)
}

/// Boot-time reconciliation for a single plugin, mirroring the conservative MCP-server
/// reconcile:
/// - plugin **absent** from the real registry → **no-op** (we never install on boot
///   without an explicit click).
/// - plugin **present** → refresh: re-copy the bundled content (it may have changed
///   across an app update — including a version bump or a port change) and re-register at
///   the stable path (healing a drifted / dead path — review #34). When the content
///   changed, ALSO propagate the bump through Claude's caches (`marketplace update` +
///   `plugin update`, finding #47) so a new bundled version actually reaches the installed
///   plugin. Writes / runs the CLI only when something changed (idempotent otherwise).
pub fn reconcile_with(
    install: &PluginInstall,
    cli: &dyn PluginCli,
) -> Result<ReconcileOutcome, PluginError> {
    let entries = cli.marketplace_list()?;
    if !is_registered(&entries, &install.marketplace) {
        // Absent → no-op: we do NOT silently install the plugin at boot.
        return Ok(ReconcileOutcome::SkippedAbsent);
    }
    let path_ok = is_registered_at(&entries, &install.marketplace, &install.install_dir);
    // Present → ensure the stable content is current and the registered path matches.
    let content_changed = copy_tree(&install.source_dir, &install.install_dir, install.mcp_port)?;
    if path_ok && !content_changed {
        return Ok(ReconcileOutcome::Unchanged);
    }
    // Re-register (heals a drifted path) and re-install (idempotent).
    cli.marketplace_add(&install.install_dir)?;
    cli.install(&install.install_id())?;
    // A content change = a version bump or port re-template: propagate it through Claude's
    // marketplace + installed-plugin caches (finding #47). A pure path-drift heal (content
    // unchanged) skips this — nothing new to propagate.
    if content_changed {
        propagate_update(install, cli)?;
    }
    Ok(ReconcileOutcome::Updated)
}

/// Remove the legacy hand-written nyx keys (`extraKnownMarketplaces.<marketplace>` and
/// `enabledPlugins["<plugin>@<marketplace>"]`) from the settings file, if present. PURE
/// of CLI; the CLI owns the live install, this only cleans up residue from the OLD
/// hand-edit approach. Returns whether anything was stripped.
fn strip_legacy_settings(install: &PluginInstall) -> std::io::Result<bool> {
    // A missing settings file → nothing to clean.
    if !install.settings_path.exists() {
        return Ok(false);
    }
    let mut root = read_config_pub(&install.settings_path)?;
    let removed = strip_plugin(&mut root, install);
    if removed {
        write_config_pub(&install.settings_path, &root)?;
    }
    Ok(removed)
}

/// Remove the legacy nyx marketplace + enabledPlugins keys from `root`. **Pure** (no IO),
/// unit-tested directly. Leaves every other marketplace / enabled plugin / top-level key
/// intact (non-destructive).
pub fn strip_plugin(root: &mut Value, install: &PluginInstall) -> bool {
    let Some(obj) = root.as_object_mut() else { return false };
    let mut removed = false;
    if let Some(m) = obj.get_mut("extraKnownMarketplaces").and_then(Value::as_object_mut) {
        if m.remove(&install.marketplace).is_some() {
            removed = true;
        }
    }
    if let Some(e) = obj.get_mut("enabledPlugins").and_then(Value::as_object_mut) {
        if e.remove(&install.install_id()).is_some() {
            removed = true;
        }
    }
    removed
}

/// Resolve the bundled plugin **source** directory on disk, in BOTH a dev run and a
/// packaged build (finding #26). This is the read-only COPY SOURCE — nyx copies it into
/// the stable install dir and registers THAT, never this path. Resolution order:
/// 1. `NYX_CLAUDE_PLUGIN_DIR` — explicit override (tests, and any operator who ships the
///    plugin elsewhere).
/// 2. The Tauri **resource** dir (`<resource>/resources/claude-plugin`) — the packaged
///    build, where the plugin ships as a bundled resource.
/// 3. The source-tree path (`<CARGO_MANIFEST_DIR>/resources/claude-plugin`) — the dev
///    run (`cargo`/`tauri dev`), where no resource dir is materialized.
///
/// Returns the first candidate whose `.claude-plugin` manifest exists, so a half-set
/// resource dir falls through to the dev path.
pub fn resolve_bundled_plugin_dir(resource_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("NYX_CLAUDE_PLUGIN_DIR") {
        if !p.is_empty() {
            candidates.push(PathBuf::from(p));
        }
    }
    if let Some(res) = resource_dir {
        candidates.push(res.join("resources").join("claude-plugin"));
    }
    // Dev fallback: the source tree next to this crate.
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join("claude-plugin"));

    candidates
        .into_iter()
        .find(|dir| dir.join(".claude-plugin").join("marketplace.json").exists())
}

/// Resolve the STABLE install directory the bundled plugin is copied into and registered
/// from (review #33). `NYX_CLAUDE_STABLE_PLUGIN_DIR` wins (test seam / operator override);
/// otherwise `<app_data_dir>/claude-plugin`. `app_data_dir` is the Tauri-resolved app
/// data dir, passed by the caller (the bridge resolves it from the `AppHandle`). Returns
/// `None` only when no override is set and no app data dir is given.
pub fn resolve_stable_plugin_dir(app_data_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NYX_CLAUDE_STABLE_PLUGIN_DIR") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    app_data_dir.map(|d| d.join("claude-plugin"))
}

/// Resolve the agent's user-scope **settings** file path (Claude Code:
/// `~/.claude/settings.json`) — used ONLY for legacy-key cleanup on uninstall.
/// `NYX_CLAUDE_SETTINGS` wins (override / test seam); otherwise `~/.claude/settings.json`
/// from `$HOME` / `$USERPROFILE`. `None` when no home dir resolves.
pub fn claude_settings_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NYX_CLAUDE_SETTINGS") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    crate::onboarding::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Build the Claude Code [`PluginInstall`] from a resolved bundled plugin dir + the
/// resolved settings path. Centralizes the Claude-specific names (`nyx` marketplace,
/// `nyx-claude-integration` plugin) in ONE place — the rest of this module is generic.
pub const CLAUDE_MARKETPLACE: &str = "nyx";
pub const CLAUDE_PLUGIN_NAME: &str = "nyx-claude-integration";

/// The install id (`"<plugin>@<marketplace>"`) Claude Code stores in
/// `settings.json.enabledPlugins` when the nyx plugin is enabled — the REAL "is the
/// plugin enabled" signal (review #40). Centralized here next to the Claude names.
pub fn claude_plugin_install_id() -> String {
    format!("{CLAUDE_PLUGIN_NAME}@{CLAUDE_MARKETPLACE}")
}

/// Whether the nyx Claude Code plugin is ENABLED in Claude Code's REAL config — the
/// authoritative install signal for the plugin component (review #40). Reads
/// `~/.claude/settings.json` (injectable via `NYX_CLAUDE_SETTINGS`) and returns `true`
/// only when `enabledPlugins["nyx-claude-integration@nyx"] == true`. This is the seam
/// the CLI install writes through and the uninstall (`/plugin` / `claude plugin
/// uninstall`) clears, so it tracks reality — NOT nyx's own stored `integrations.json`
/// flag, which goes stale when the user uninstalls the plugin directly in Claude Code.
///
/// **Marketplace presence is NOT the signal**: `extraKnownMarketplaces.nyx` survives a
/// plugin uninstall, so only the `enabledPlugins` flag is checked. A missing settings
/// file / unresolvable path / absent or non-`true` entry all read as `false`.
pub fn claude_plugin_enabled() -> bool {
    match claude_settings_path() {
        Some(path) => plugin_enabled_in_settings(&path, &claude_plugin_install_id()),
        None => false,
    }
}

/// Pure check: is `install_id` enabled (`true`) in the `enabledPlugins` map of the
/// settings file at `path`? A missing file, a parse error, an absent key or a non-`true`
/// value all read as `false`. Factored out (no `claude_settings_path` resolution) so it
/// is unit-testable against a temp settings file without env juggling.
pub fn plugin_enabled_in_settings(path: &Path, install_id: &str) -> bool {
    let Ok(root) = read_config_pub(path) else {
        return false;
    };
    root.get("enabledPlugins")
        .and_then(Value::as_object)
        .and_then(|m| m.get(install_id))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    //! Tests run EXCLUSIVELY against temp dirs and a FAKE CLI — they never read or write
    //! the user's real `~/.claude`, and never shell out to the real `claude` binary. The
    //! pure legacy-strip logic is tested directly; the install/reconcile/uninstall flows
    //! drive a recording [`FakeCli`] over temp source + stable dirs.

    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("nyx-plugin-{}-{}-{}", std::process::id(), tag, n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a minimal bundled plugin tree (the manifest, a hooks file + the bundled
    /// `.mcp.json` carrying the port PLACEHOLDER) under `dir` and return it, so the
    /// resolver/copy see a realistic source — including the port-templating target.
    fn write_bundled(dir: &Path) -> PathBuf {
        let plugin = dir.join("claude-plugin");
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(plugin.join("hooks")).unwrap();
        std::fs::write(plugin.join(".claude-plugin").join("marketplace.json"), r#"{"name":"nyx"}"#).unwrap();
        std::fs::write(plugin.join("hooks").join("hooks.json"), r#"{"hooks":{}}"#).unwrap();
        std::fs::write(
            plugin.join(MCP_DESCRIPTOR_FILE),
            format!(r#"{{"nyx":{{"type":"http","url":"http://127.0.0.1:{MCP_PORT_PLACEHOLDER}/mcp"}}}}"#),
        )
        .unwrap();
        plugin
    }

    fn install_for(source: &Path, stable: &Path, settings: &Path) -> PluginInstall {
        install_for_port(source, stable, settings, 8765)
    }

    fn install_for_port(source: &Path, stable: &Path, settings: &Path, mcp_port: u16) -> PluginInstall {
        PluginInstall {
            marketplace: CLAUDE_MARKETPLACE.to_string(),
            plugin: CLAUDE_PLUGIN_NAME.to_string(),
            source_dir: source.to_path_buf(),
            install_dir: stable.to_path_buf(),
            settings_path: settings.to_path_buf(),
            mcp_port,
        }
    }

    /// A recording fake of the agent plugin CLI: tracks every call and models the real
    /// registry (a single optional `nyx` marketplace path) so reconcile/idempotency can
    /// be asserted without the `claude` binary.
    #[derive(Default)]
    struct FakeCli {
        /// The currently registered marketplace path, if any (mirrors known_marketplaces).
        registered: RefCell<Option<PathBuf>>,
        /// Whether the plugin is installed.
        plugin_installed: RefCell<bool>,
        /// Ordered log of subcommands, for assertions.
        calls: RefCell<Vec<String>>,
        /// When true, every call returns `CliNotFound` (claude absent simulation).
        absent: bool,
    }

    impl PluginCli for FakeCli {
        fn marketplace_add(&self, dir: &Path) -> Result<(), PluginError> {
            if self.absent {
                return Err(PluginError::CliNotFound);
            }
            self.calls.borrow_mut().push(format!("add {}", dir.display()));
            *self.registered.borrow_mut() = Some(dir.to_path_buf());
            Ok(())
        }
        fn install(&self, install_id: &str) -> Result<(), PluginError> {
            if self.absent {
                return Err(PluginError::CliNotFound);
            }
            self.calls.borrow_mut().push(format!("install {install_id}"));
            *self.plugin_installed.borrow_mut() = true;
            Ok(())
        }
        fn uninstall(&self, install_id: &str) -> Result<(), PluginError> {
            self.calls.borrow_mut().push(format!("uninstall {install_id}"));
            *self.plugin_installed.borrow_mut() = false;
            Ok(())
        }
        fn marketplace_remove(&self, marketplace: &str) -> Result<(), PluginError> {
            self.calls.borrow_mut().push(format!("remove {marketplace}"));
            *self.registered.borrow_mut() = None;
            Ok(())
        }
        fn marketplace_update(&self, marketplace: &str) -> Result<(), PluginError> {
            if self.absent {
                return Err(PluginError::CliNotFound);
            }
            self.calls.borrow_mut().push(format!("mkt-update {marketplace}"));
            Ok(())
        }
        fn plugin_update(&self, install_id: &str) -> Result<(), PluginError> {
            if self.absent {
                return Err(PluginError::CliNotFound);
            }
            self.calls.borrow_mut().push(format!("plugin-update {install_id}"));
            Ok(())
        }
        fn marketplace_list(&self) -> Result<Vec<MarketplaceEntry>, PluginError> {
            if self.absent {
                return Err(PluginError::CliNotFound);
            }
            Ok(self
                .registered
                .borrow()
                .clone()
                .map(|p| vec![MarketplaceEntry { name: CLAUDE_MARKETPLACE.to_string(), path: Some(p) }])
                .unwrap_or_default())
        }
    }

    // --- install (copy + CLI driving) -------------------------------------

    #[test]
    fn install_copies_to_stable_dir_and_drives_cli() {
        let dir = temp_dir("install");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();

        // First install → Added: content lands in the STABLE dir, the CLI is driven.
        assert_eq!(install_with(&inst, &cli).unwrap(), PluginChange::Added);
        assert!(stable.join(".claude-plugin").join("marketplace.json").exists(), "bundled content copied to stable dir");
        let calls = cli.calls.borrow().clone();
        assert!(calls.iter().any(|c| c == &format!("add {}", stable.display())), "registered the STABLE path, not the source: {calls:?}");
        assert!(calls.iter().any(|c| c == "install nyx-claude-integration@nyx"));
        // The registered path is the stable dir, NEVER the volatile source dir.
        assert_eq!(cli.registered.borrow().as_deref(), Some(stable.as_path()));
        assert_ne!(cli.registered.borrow().as_deref(), Some(source.as_path()));
    }

    #[test]
    fn install_is_idempotent_when_already_current() {
        let dir = temp_dir("idem");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();

        assert_eq!(install_with(&inst, &cli).unwrap(), PluginChange::Added);
        // Re-install with unchanged content → Unchanged (registry already at stable path).
        assert_eq!(install_with(&inst, &cli).unwrap(), PluginChange::Unchanged);
    }

    #[test]
    fn install_reports_updated_when_bundled_content_changes() {
        let dir = temp_dir("update");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        assert_eq!(install_with(&inst, &cli).unwrap(), PluginChange::Added);

        // Bundled hooks change (an app update ships a new hooks.json) → Updated + re-copied.
        std::fs::write(source.join("hooks").join("hooks.json"), r#"{"hooks":{"SessionStart":[]}}"#).unwrap();
        assert_eq!(install_with(&inst, &cli).unwrap(), PluginChange::Updated);
        let copied = std::fs::read_to_string(stable.join("hooks").join("hooks.json")).unwrap();
        assert!(copied.contains("SessionStart"), "stable copy refreshed with new content");
    }

    // --- bundled MCP port templating (finding #44) -------------------------

    /// The bundled `.mcp.json` port PLACEHOLDER is substituted with the live port in the
    /// COPIED descriptor, while the bundled SOURCE keeps the placeholder (port-agnostic).
    #[test]
    fn install_templates_mcp_port_into_copied_descriptor() {
        let dir = temp_dir("mcp-port");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for_port(&source, &stable, &settings, 9931);
        let cli = FakeCli::default();
        install_with(&inst, &cli).unwrap();

        // Copied descriptor carries the real port; the placeholder is gone.
        let copied = std::fs::read_to_string(stable.join(MCP_DESCRIPTOR_FILE)).unwrap();
        assert!(copied.contains("127.0.0.1:9931/mcp"), "live port templated in: {copied}");
        assert!(!copied.contains(MCP_PORT_PLACEHOLDER), "placeholder substituted out");
        // The bundled SOURCE is untouched — still the placeholder (port-agnostic).
        let src = std::fs::read_to_string(source.join(MCP_DESCRIPTOR_FILE)).unwrap();
        assert!(src.contains(MCP_PORT_PLACEHOLDER), "bundled source stays port-agnostic");
    }

    /// Re-installing with the SAME port is idempotent (the templated descriptor matches),
    /// while a PORT CHANGE re-templates the copied descriptor and reports Updated +
    /// propagates the change through Claude's caches (finding #44/#47).
    #[test]
    fn install_re_templates_descriptor_on_port_change() {
        let dir = temp_dir("mcp-port-change");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let cli = FakeCli::default();

        install_with(&install_for_port(&source, &stable, &settings, 8765), &cli).unwrap();
        // Same port → Unchanged, no re-write, no update propagation.
        assert_eq!(
            install_with(&install_for_port(&source, &stable, &settings, 8765), &cli).unwrap(),
            PluginChange::Unchanged
        );

        // Port changes (e.g. NYX_MCP_PORT override) → re-template + Updated + propagate.
        assert_eq!(
            install_with(&install_for_port(&source, &stable, &settings, 7000), &cli).unwrap(),
            PluginChange::Updated
        );
        let copied = std::fs::read_to_string(stable.join(MCP_DESCRIPTOR_FILE)).unwrap();
        assert!(copied.contains("127.0.0.1:7000/mcp"), "descriptor re-templated to new port");
        let calls = cli.calls.borrow().clone();
        assert!(calls.iter().any(|c| c == "mkt-update nyx"), "propagated marketplace update: {calls:?}");
        assert!(calls.iter().any(|c| c == "plugin-update nyx-claude-integration@nyx"), "propagated plugin update: {calls:?}");
    }

    /// `template_mcp_port` is pure: substitutes every placeholder, leaves other text, and
    /// passes non-UTF-8 through untouched (no panic).
    #[test]
    fn template_mcp_port_is_pure_and_robust() {
        let out = template_mcp_port(format!("a{MCP_PORT_PLACEHOLDER}b{MCP_PORT_PLACEHOLDER}").as_bytes(), 42);
        assert_eq!(String::from_utf8(out).unwrap(), "a42b42");
        // Non-UTF-8 bytes pass through unchanged.
        let raw = [0xff, 0xfe, 0x00];
        assert_eq!(template_mcp_port(&raw, 1), raw.to_vec());
    }

    // --- update propagation through Claude's caches (finding #47) ----------

    /// A bundled-content change (a plugin VERSION bump from an app update) makes install
    /// run the full propagation sequence — `marketplace update` THEN `plugin update` — so
    /// Claude's marketplace cache is refreshed and the installed plugin moves to the new
    /// version. Verified against 2.1.170: `marketplace add` alone does not re-read the dir.
    #[test]
    fn install_propagates_version_bump_through_caches() {
        let dir = temp_dir("propagate");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        install_with(&inst, &cli).unwrap();
        // No propagation on the FIRST install (Added, not a refresh of an existing entry).
        assert!(!cli.calls.borrow().iter().any(|c| c.starts_with("mkt-update")), "no churn on fresh add");

        // App update ships a new plugin.json version → content change → propagate.
        std::fs::write(
            source.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"nyx-claude-integration","version":"0.5.0"}"#,
        )
        .unwrap();
        assert_eq!(install_with(&inst, &cli).unwrap(), PluginChange::Updated);
        let calls = cli.calls.borrow().clone();
        let mkt = calls.iter().position(|c| c == "mkt-update nyx").expect("marketplace update ran");
        let plug = calls.iter().position(|c| c == "plugin-update nyx-claude-integration@nyx").expect("plugin update ran");
        assert!(mkt < plug, "marketplace cache refreshed BEFORE the installed plugin is bumped: {calls:?}");
    }

    /// Reconcile propagates a version bump too (an app update lands while the plugin is
    /// already installed): present + content changed → re-register + propagate. Idempotent
    /// when nothing changed (no propagation churn).
    #[test]
    fn reconcile_propagates_version_bump_and_is_idempotent() {
        let dir = temp_dir("recon-propagate");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        install_with(&inst, &cli).unwrap();

        // Unchanged → reconcile is a no-op (no propagation churn).
        assert_eq!(reconcile_with(&inst, &cli).unwrap(), ReconcileOutcome::Unchanged);
        let before = cli.calls.borrow().len();

        // App update bumps the bundled version → reconcile re-copies + propagates.
        std::fs::write(
            source.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"nyx-claude-integration","version":"0.6.0"}"#,
        )
        .unwrap();
        assert_eq!(reconcile_with(&inst, &cli).unwrap(), ReconcileOutcome::Updated);
        let calls = cli.calls.borrow().clone();
        assert!(calls.len() > before, "reconcile drove CLI on the bump");
        assert!(calls.iter().any(|c| c == "mkt-update nyx"), "marketplace cache refreshed: {calls:?}");
        assert!(calls.iter().any(|c| c == "plugin-update nyx-claude-integration@nyx"), "plugin bumped: {calls:?}");
    }

    /// The registered path is STABLE across a source-dir move (dev rebuild / packaged
    /// update): the source dir moves but the install dir — and thus the registered path —
    /// does not (review #33).
    #[test]
    fn registered_path_is_stable_across_source_dir_move() {
        let dir = temp_dir("stable-path");
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let cli = FakeCli::default();

        // Install from a "dev" source dir.
        let dev_source = write_bundled(&dir.join("dev"));
        let inst_dev = install_for(&dev_source, &stable, &settings);
        install_with(&inst_dev, &cli).unwrap();
        let registered_after_dev = cli.registered.borrow().clone();

        // Re-install from a DIFFERENT (packaged) source dir → SAME stable registered path.
        let pkg_source = write_bundled(&dir.join("pkg"));
        let inst_pkg = install_for(&pkg_source, &stable, &settings);
        install_with(&inst_pkg, &cli).unwrap();
        assert_eq!(cli.registered.borrow().clone(), registered_after_dev, "registered path unchanged across a source move");
        assert_eq!(cli.registered.borrow().as_deref(), Some(stable.as_path()));
    }

    // --- self-heal of a dead/drifted path (review #34) --------------------

    #[test]
    fn install_self_heals_a_dead_path_entry() {
        let dir = temp_dir("selfheal");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        // Simulate the user's broken state: nyx registered at a DEAD path, no stable copy.
        *cli.registered.borrow_mut() = Some(dir.join("DEAD-stale-path"));

        // Install heals it: re-copies content + re-registers at the LIVE stable path.
        let change = install_with(&inst, &cli).unwrap();
        assert!(matches!(change, PluginChange::Added | PluginChange::Updated));
        assert_eq!(cli.registered.borrow().as_deref(), Some(stable.as_path()), "dead path healed to the stable path");
        assert!(stable.join(".claude-plugin").join("marketplace.json").exists());
    }

    // --- reconcile (conservative; absent = no-op) -------------------------

    #[test]
    fn reconcile_absent_is_noop() {
        let dir = temp_dir("recon-absent");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default(); // nothing registered

        assert_eq!(reconcile_with(&inst, &cli).unwrap(), ReconcileOutcome::SkippedAbsent);
        assert!(cli.calls.borrow().is_empty(), "absent → no CLI writes (never install on boot)");
        assert!(!stable.exists(), "absent → no copy");
    }

    #[test]
    fn reconcile_present_unchanged_is_noop() {
        let dir = temp_dir("recon-unchanged");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        install_with(&inst, &cli).unwrap();
        let calls_before = cli.calls.borrow().len();

        assert_eq!(reconcile_with(&inst, &cli).unwrap(), ReconcileOutcome::Unchanged);
        assert_eq!(cli.calls.borrow().len(), calls_before, "no re-register when already current");
    }

    #[test]
    fn reconcile_present_heals_drifted_path() {
        let dir = temp_dir("recon-drift");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        // Present but registered at a STALE path (dev→packaged drift, no stable copy yet).
        *cli.registered.borrow_mut() = Some(dir.join("stale-volatile-path"));

        assert_eq!(reconcile_with(&inst, &cli).unwrap(), ReconcileOutcome::Updated);
        assert_eq!(cli.registered.borrow().as_deref(), Some(stable.as_path()), "drifted path healed to stable");
        assert!(stable.join(".claude-plugin").join("marketplace.json").exists(), "content copied on heal");
    }

    // --- uninstall (CLI + legacy settings cleanup, review #34) ------------

    #[test]
    fn remove_drives_cli_and_strips_legacy_settings() {
        let dir = temp_dir("remove");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        // Leftover hand-written nyx keys from the OLD approach, plus a sibling to preserve.
        std::fs::write(
            &settings,
            r#"{"autoConnectIde":true,"enabledPlugins":{"nyx-claude-integration@nyx":true,"warp@claude-code-warp":true},"extraKnownMarketplaces":{"nyx":{"source":{"source":"directory","path":"/old/volatile"}},"claude-code-warp":{"source":{"source":"github","repo":"warpdotdev/claude-code-warp"}}}}"#,
        )
        .unwrap();
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        install_with(&inst, &cli).unwrap();

        assert!(remove_with(&inst, &cli).unwrap(), "legacy keys were stripped");
        // CLI uninstall + marketplace remove were both driven.
        let calls = cli.calls.borrow().clone();
        assert!(calls.iter().any(|c| c == "uninstall nyx-claude-integration@nyx"), "{calls:?}");
        assert!(calls.iter().any(|c| c == "remove nyx"), "{calls:?}");
        // Legacy nyx keys gone; the sibling marketplace + plugin survive.
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(v["extraKnownMarketplaces"].get("nyx").is_none(), "legacy nyx marketplace stripped");
        assert!(v["enabledPlugins"].get("nyx-claude-integration@nyx").is_none(), "legacy nyx enabledPlugins stripped");
        assert_eq!(v["extraKnownMarketplaces"]["claude-code-warp"]["source"]["repo"], "warpdotdev/claude-code-warp");
        assert_eq!(v["enabledPlugins"]["warp@claude-code-warp"], true);
        assert_eq!(v["autoConnectIde"], true);
    }

    #[test]
    fn remove_with_no_legacy_settings_is_clean() {
        let dir = temp_dir("remove-clean");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json"); // does not exist
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli::default();
        // No legacy file → nothing to strip, but CLI removals still run (best-effort).
        assert!(!remove_with(&inst, &cli).unwrap(), "no legacy keys → returns false");
        let calls = cli.calls.borrow().clone();
        assert!(calls.iter().any(|c| c == "uninstall nyx-claude-integration@nyx"));
    }

    // --- graceful when claude is absent (review #35) ----------------------

    #[test]
    fn install_surfaces_cli_not_found() {
        let dir = temp_dir("absent");
        let source = write_bundled(&dir);
        let stable = dir.join("stable");
        let settings = dir.join("settings.json");
        let inst = install_for(&source, &stable, &settings);
        let cli = FakeCli { absent: true, ..Default::default() };

        let err = install_with(&inst, &cli).unwrap_err();
        assert!(matches!(err, PluginError::CliNotFound), "claude absent → typed CliNotFound");
        // The display message is user-actionable (surfaced in the UI).
        assert!(err.to_string().contains("claude"), "error mentions the missing CLI");
    }

    // --- pure legacy-strip ------------------------------------------------

    #[test]
    fn strip_plugin_removes_only_nyx() {
        let dir = temp_dir("strip");
        let inst = install_for(&dir.join("src"), &dir.join("stable"), &dir.join("settings.json"));
        let mut root: Value = serde_json::from_str(
            r#"{"extraKnownMarketplaces":{"nyx":{},"claude-code-warp":{}},"enabledPlugins":{"nyx-claude-integration@nyx":true,"warp@claude-code-warp":true}}"#,
        )
        .unwrap();
        assert!(strip_plugin(&mut root, &inst), "nyx present → stripped");
        assert!(root["extraKnownMarketplaces"].get("nyx").is_none());
        assert!(root["enabledPlugins"].get("nyx-claude-integration@nyx").is_none());
        assert!(root["extraKnownMarketplaces"].get("claude-code-warp").is_some(), "sibling preserved");
        assert_eq!(root["enabledPlugins"]["warp@claude-code-warp"], true);
        // A second strip is a no-op.
        assert!(!strip_plugin(&mut root, &inst));
    }

    // --- stable-dir resolution (review #33) -------------------------------

    #[test]
    fn resolve_stable_dir_prefers_override_then_app_data() {
        // Mutates the process-global `NYX_CLAUDE_STABLE_PLUGIN_DIR` seam (which the
        // agent.rs install tests also touch), so it takes the ONE crate-wide seam lock
        // (review #42/#43), not a private mutex. Prior value restored on exit.
        let _g = crate::CLAUDE_ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("NYX_CLAUDE_STABLE_PLUGIN_DIR");

        std::env::remove_var("NYX_CLAUDE_STABLE_PLUGIN_DIR");
        // Default: <app_data>/claude-plugin.
        let app_data = Path::new("/home/u/.local/share/nyx");
        assert_eq!(
            resolve_stable_plugin_dir(Some(app_data)),
            Some(app_data.join("claude-plugin"))
        );
        // Override wins.
        std::env::set_var("NYX_CLAUDE_STABLE_PLUGIN_DIR", "/tmp/forced");
        assert_eq!(resolve_stable_plugin_dir(Some(app_data)), Some(PathBuf::from("/tmp/forced")));
        std::env::remove_var("NYX_CLAUDE_STABLE_PLUGIN_DIR");
        // No override + no app data → None.
        assert_eq!(resolve_stable_plugin_dir(None), None);

        if let Some(v) = prev {
            std::env::set_var("NYX_CLAUDE_STABLE_PLUGIN_DIR", v);
        }
    }

    // --- bundled-dir resolution (finding #26) -----------------------------

    #[test]
    fn resolve_bundled_dir_finds_dev_source_tree() {
        // Mutates the process-global `NYX_CLAUDE_PLUGIN_DIR` seam (which the agent.rs
        // install tests also touch), so it takes the ONE crate-wide seam lock
        // (review #42/#43), not a private mutex. Prior value restored on exit.
        let _g = crate::CLAUDE_ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("NYX_CLAUDE_PLUGIN_DIR");
        std::env::remove_var("NYX_CLAUDE_PLUGIN_DIR");

        let resolved = resolve_bundled_plugin_dir(None).expect("dev source-tree plugin resolves");
        assert!(resolved.join(".claude-plugin").join("marketplace.json").exists());
        assert!(resolved.ends_with("resources/claude-plugin"));

        if let Some(v) = prev {
            std::env::set_var("NYX_CLAUDE_PLUGIN_DIR", v);
        }
    }

    /// GUARD (finding #51): every file the bundled plugin needs must be listed in
    /// `tauri.conf.json` `bundle.resources`, or the packaged build copies an incomplete
    /// plugin into `target/.../resources/claude-plugin/`. The original bug: `.mcp.json`
    /// was absent from the list, so the resource dir shipped without it, `plugin.json`'s
    /// `"mcpServers": "./.mcp.json"` pointed at a missing file, and Claude loaded the
    /// plugin with `MCP servers (0)`. This test fails if ANY of the four plugin files is
    /// dropped from the bundle manifest — AND that each listed file actually exists in the
    /// source tree (so the manifest can't reference a path that never ships).
    #[test]
    fn tauri_conf_bundles_every_plugin_file() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
        let conf: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).expect("read tauri.conf.json"))
                .expect("parse tauri.conf.json");
        let resources = conf["bundle"]["resources"]
            .as_array()
            .expect("bundle.resources is an array");
        let listed: Vec<&str> = resources.iter().filter_map(|v| v.as_str()).collect();

        // The four files that together make a loadable plugin (manifest + marketplace +
        // hooks + the MCP descriptor referenced by plugin.json).
        let required = [
            "resources/claude-plugin/.claude-plugin/marketplace.json",
            "resources/claude-plugin/.claude-plugin/plugin.json",
            "resources/claude-plugin/hooks/hooks.json",
            "resources/claude-plugin/.mcp.json",
        ];
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for f in required {
            assert!(
                listed.contains(&f),
                "tauri.conf.json bundle.resources is missing the plugin file `{f}` — the \
                 packaged build would ship an incomplete plugin. Listed: {listed:?}"
            );
            assert!(
                crate_root.join(f).exists(),
                "bundle.resources lists `{f}` but it does not exist in the source tree"
            );
        }
    }

    // --- REAL plugin-enabled status from settings.json (review #40) --------

    /// The install id Claude Code stores in `enabledPlugins` is `<plugin>@<marketplace>`.
    #[test]
    fn claude_plugin_install_id_is_plugin_at_marketplace() {
        assert_eq!(claude_plugin_install_id(), "nyx-claude-integration@nyx");
    }

    /// `enabledPlugins["nyx-claude-integration@nyx"] == true` → enabled. The REAL signal
    /// the CLI install writes (review #40), read against a temp settings file.
    #[test]
    fn plugin_enabled_when_enabledplugins_true() {
        let dir = temp_dir("enabled-true");
        let settings = dir.join("settings.json");
        std::fs::write(
            &settings,
            r#"{"enabledPlugins":{"nyx-claude-integration@nyx":true,"warp@claude-code-warp":true}}"#,
        )
        .unwrap();
        assert!(plugin_enabled_in_settings(&settings, &claude_plugin_install_id()));
    }

    /// Key ABSENT from `enabledPlugins` → not enabled. This is the exact state after a
    /// user uninstalls the plugin directly in Claude Code (the bug in review #40): the
    /// `enabledPlugins` entry disappears even though the marketplace may linger.
    #[test]
    fn plugin_not_enabled_when_key_absent() {
        let dir = temp_dir("enabled-absent");
        let settings = dir.join("settings.json");
        // The marketplace can survive a plugin uninstall, but the enabledPlugins entry is
        // gone — and marketplace presence is NOT the signal.
        std::fs::write(
            &settings,
            r#"{"enabledPlugins":{"warp@claude-code-warp":true},"extraKnownMarketplaces":{"nyx":{}}}"#,
        )
        .unwrap();
        assert!(!plugin_enabled_in_settings(&settings, &claude_plugin_install_id()));
    }

    /// `enabledPlugins["…"] == false` (explicitly disabled) → not enabled.
    #[test]
    fn plugin_not_enabled_when_value_false() {
        let dir = temp_dir("enabled-false");
        let settings = dir.join("settings.json");
        std::fs::write(&settings, r#"{"enabledPlugins":{"nyx-claude-integration@nyx":false}}"#).unwrap();
        assert!(!plugin_enabled_in_settings(&settings, &claude_plugin_install_id()));
    }

    /// A missing settings file → not enabled (no panic), the same as no `enabledPlugins`.
    #[test]
    fn plugin_not_enabled_when_settings_missing() {
        let dir = temp_dir("enabled-missing");
        let settings = dir.join("does-not-exist.json");
        assert!(!settings.exists());
        assert!(!plugin_enabled_in_settings(&settings, &claude_plugin_install_id()));
    }

    /// `claude_plugin_enabled` resolves the `NYX_CLAUDE_SETTINGS` seam and reads the real
    /// flag — present→true / absent→false — WITHOUT ever touching the user's `~/.claude`.
    #[test]
    fn claude_plugin_enabled_honors_the_settings_seam() {
        // Genuinely exercises the `NYX_CLAUDE_SETTINGS` env RESOLUTION, so it mutates the
        // process-global seam. It takes the ONE crate-wide lock every seam-mutating test
        // shares (review #42/#43) — NOT a private mutex — so it never interleaves with a
        // seam mutation in another module. Prior value restored on exit (no leak).
        let _g = crate::CLAUDE_ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("NYX_CLAUDE_SETTINGS");

        let dir = temp_dir("seam-enabled");
        let settings = dir.join("settings.json");
        std::env::set_var("NYX_CLAUDE_SETTINGS", &settings);

        // Present + true → enabled.
        std::fs::write(&settings, r#"{"enabledPlugins":{"nyx-claude-integration@nyx":true}}"#).unwrap();
        assert!(claude_plugin_enabled(), "enabledPlugins true → enabled via the seam");

        // Absent → not enabled (the direct-uninstall case).
        std::fs::write(&settings, r#"{"enabledPlugins":{}}"#).unwrap();
        assert!(!claude_plugin_enabled(), "enabledPlugins entry gone → not enabled via the seam");

        match prev {
            Some(v) => std::env::set_var("NYX_CLAUDE_SETTINGS", v),
            None => std::env::remove_var("NYX_CLAUDE_SETTINGS"),
        }
    }
}
