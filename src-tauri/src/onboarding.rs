//! Claude Code integration config helpers (PRD-4 #5 / PRD-5, ADR-0003 D10/D11).
//!
//! ## The ONE bundled plugin (MCP + hooks) — finding #44/#45/#46
//! The nyx Claude integration is now a SINGLE bundled plugin that provides BOTH the MCP
//! server (declared in the plugin's `.mcp.json`, port-templated at copy time — connects as
//! `plugin:nyx-claude-integration:nyx`) AND the SessionStart/SessionEnd session-capture
//! hooks. nyx no longer writes a SEPARATE standalone `mcpServers.nyx` entry into
//! `~/.claude.json` — that double-declaration desynced. The plugin install/reconcile/
//! uninstall lives in [`crate::plugin`]; THIS module now only:
//! - resolves the Claude config path ([`OnboardingTarget`] / `NYX_CLAUDE_CONFIG`);
//! - reads the legacy standalone-MCP signal ([`claude_mcp_installed`] /
//!   [`mcp_server_present`]) and **strips** that residue on install/uninstall
//!   ([`remove_legacy_mcp_server`]), so a migrated user keeps a single MCP declaration;
//! - persists the non-authoritative install cache ([`IntegrationState`]) and drives the
//!   conservative boot reconcile of the plugin ([`reconcile_installed_providers`]).
//!
//! ## Legacy standalone-MCP merge helpers (retained, tested, NOT wired in production)
//! The pure `mcpServers.nyx` merge/onboard helpers (`mcp_url`, `merge_nyx_server`,
//! `OnboardingTarget::onboard`, `reconcile_providers_with_targets`,
//! `OnboardConfigChange`) are the OLD separate-MCP install path. They are no longer wired
//! into any production flow (the plugin owns the MCP now), but are kept + unit-tested
//! because they document the exact `{ "type": "http", "url": "http://127.0.0.1:<port>/mcp" }`
//! entry shape the bundled `.mcp.json` mirrors, and the strip path round-trips the same
//! config. The module-level `#![cfg_attr(not(test), allow(dead_code))]` keeps the
//! non-test build warning-free while the tests prove the merge logic (same convention as
//! `agent.rs` / `db.rs` for tested-but-phased helpers).
//!
//! ## Boot reconciliation (keyed on REAL state — review #40/#41)
//! [`reconcile_installed_providers`] keys PURELY off the client's REAL plugin registry:
//! plugin **present** → refresh (re-copy/re-template/propagate a version bump, finding
//! #47); plugin **absent** → no-op (never install silently on boot). The same real-state
//! rule drives the install STATUS the UI shows (`enabledPlugins`, finding #46), so a plugin
//! the user removed DIRECTLY in Claude Code reads as uninstalled instead of nyx's stored
//! flag lying. `<app_data_dir>/integrations.json` ([`IntegrationState`]) is a
//! NON-authoritative cache the UI writes on its own clicks; never the source of truth.
//!
//! ## Safety / testability
//! The config path is **injectable**: [`OnboardingTarget::claude_code`] resolves the real
//! `~/.claude.json`, but [`OnboardingTarget::new`] takes an arbitrary path so tests point
//! at a temp file — the suite never mutates the user's real config.

// The legacy standalone-MCP merge/onboard helpers below are retained + unit-tested but no
// longer wired into any production flow (the bundled plugin owns the MCP — finding #45);
// this keeps the non-test build warning-free, matching the agent.rs / db.rs convention.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// The MCP server name nyx registers under in a client's config. Stable: the
/// idempotent upsert keys on it, so a re-run finds and updates the same entry.
pub const SERVER_NAME: &str = "nyx";

/// The localhost-direct MCP URL nyx onboards clients onto (ADR-0003 D10/D11).
/// **Never** the portless `https://nyx.localhost` URL — portless is a separate
/// human/integration surface, not the MCP transport.
pub fn mcp_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

/// Outcome of one onboarding run, for logging / UI surfacing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardConfigChange {
    /// No `nyx` entry existed; one was added.
    Added,
    /// A `nyx` entry existed but pointed at a different URL; it was rewritten
    /// (e.g. the port changed). Carries the previous URL for the log.
    Updated { previous_url: String },
    /// A `nyx` entry already pointed at the exact URL; nothing was written
    /// (idempotent no-op).
    Unchanged,
}

/// A supported onboarding client + its (injectable) config-file path.
///
/// The path is parameterized so production resolves the real user-scope file while
/// tests resolve a temp file — nyx never mutates the user's real config under test.
pub struct OnboardingTarget {
    /// Human-readable client name, for logs/UI.
    pub client: &'static str,
    /// The user-scope config JSON file this client reads.
    pub config_path: PathBuf,
}

impl OnboardingTarget {
    /// Build a target for an explicit config path. Used by tests (temp file) and by
    /// any caller that injects the path; never assumes the real `~/.claude.json`.
    pub fn new(client: &'static str, config_path: impl Into<PathBuf>) -> Self {
        Self { client, config_path: config_path.into() }
    }

    /// The Claude Code user-scope target: `~/.claude.json`. Honors the
    /// `NYX_CLAUDE_CONFIG` override first (so an operator/integration can redirect
    /// it without touching `$HOME`), then `$HOME` / `$USERPROFILE`. Returns `None`
    /// when no home directory can be resolved (the caller then skips silently).
    pub fn claude_code() -> Option<Self> {
        let path = claude_config_path()?;
        Some(Self::new("Claude Code", path))
    }

    /// Install or update nyx's user-scope MCP entry in this client's config so it
    /// points at the localhost-direct `http://127.0.0.1:<port>/mcp`. Idempotent: a
    /// re-run with the same port is a no-op; a changed port rewrites the entry. The
    /// parent directory is created if missing; a missing file is treated as empty.
    pub fn onboard(&self, port: u16) -> std::io::Result<OnboardConfigChange> {
        let url = mcp_url(port);
        let mut root = read_config(&self.config_path)?;
        let change = merge_nyx_server(&mut root, &url);
        // Only touch disk when something actually changed — keeps the file stable
        // byte-for-byte across idempotent re-runs.
        if change != OnboardConfigChange::Unchanged {
            write_config(&self.config_path, &root)?;
        }
        Ok(change)
    }

}

/// Resolve the Claude Code user-scope config path. `NYX_CLAUDE_CONFIG` wins (an
/// explicit override, also the seam the e2e/manual flow can pin); otherwise
/// `~/.claude.json` from `$HOME` (Unix/macOS) or `$USERPROFILE` (Windows).
fn claude_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NYX_CLAUDE_CONFIG") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    home_dir().map(|h| h.join(".claude.json"))
}

/// The user's home directory, from `$HOME` (Unix/macOS) or `$USERPROFILE`
/// (Windows). std-only — avoids adding a `dirs`/`home` dependency for one lookup.
/// `pub(crate)` so `plugin.rs` reuses the SAME resolver (no second copy to drift).
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()))
        .map(PathBuf::from)
}

/// Public re-export of [`read_config`] for callers that hold a config path directly
/// (e.g. the `integration_remove` Tauri command in `bridge.rs`).
pub fn read_config_pub(path: &Path) -> std::io::Result<Value> {
    read_config(path)
}

/// Whether nyx's MCP server is present in Claude Code's REAL config — the authoritative
/// install signal for the MCP component (review #41), symmetric to the plugin's
/// `enabledPlugins` check. Reads `~/.claude.json` (injectable via `NYX_CLAUDE_CONFIG`)
/// and returns `true` only when `mcpServers.nyx` exists — exactly the slice
/// [`merge_nyx_server`] writes and [`reconcile_providers_with_targets`] checks. This
/// tracks reality (a user removing the server in Claude flips it to `false`), NOT nyx's
/// own stored `integrations.json` flag. A missing file / unresolvable path / absent
/// entry all read as `false`.
pub fn claude_mcp_installed() -> bool {
    match claude_config_path() {
        Some(path) => mcp_server_present(&path),
        None => false,
    }
}

/// Pure check: is the `nyx` MCP server entry present in the config file at `path`? A
/// missing file, a parse error or an absent `mcpServers.nyx` all read as `false`.
/// Factored out (no path resolution) so it is unit-testable against a temp config.
pub fn mcp_server_present(path: &Path) -> bool {
    match read_config(path) {
        Ok(root) => root
            .get("mcpServers")
            .and_then(|s| s.get(SERVER_NAME))
            .is_some(),
        Err(_) => false,
    }
}

/// Remove the LEGACY standalone `mcpServers.nyx` entry from the config file at `path`, if
/// present (finding #45). The nyx MCP is now bundled IN the plugin (`.mcp.json`), so the
/// separate `~/.claude.json` declaration is no longer written on install — and any residue
/// from the old separate-MCP flow is stripped on uninstall so no double-declaration
/// lingers. A missing file / absent entry is a no-op. Returns whether anything was
/// removed. Best-effort (a read/parse error is swallowed as "nothing to remove").
pub fn remove_legacy_mcp_server(path: &Path) -> std::io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut root = match read_config(path) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let removed = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .map(|servers| servers.remove(SERVER_NAME).is_some())
        .unwrap_or(false);
    if removed {
        write_config(path, &root)?;
    }
    Ok(removed)
}

/// Public re-export of [`write_config`] for callers that hold a config path directly.
pub fn write_config_pub(path: &Path, root: &Value) -> std::io::Result<()> {
    write_config(path, root)
}

/// Read a client config file into a JSON object. A MISSING file → an empty object
/// (first onboarding). A present-but-MALFORMED file → an `Err` (NOT an empty object):
/// returning `{}` here would make a subsequent merge+write OVERWRITE and DESTROY the
/// user's entire config on a transient corruption or a concurrent writer (e.g. Claude
/// Code mid-write). A present-but-non-object root (a stray scalar/array) is still
/// coerced to an empty object rather than failing, so it can't wedge onboarding.
fn read_config(path: &Path) -> std::io::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let parsed: Value = serde_json::from_str(&raw).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("config at {} is not valid JSON: {e}", path.display()),
                )
            })?;
            Ok(if parsed.is_object() { parsed } else { json!({}) })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

/// Serialize and atomically-ish write the config back: write a sibling `.tmp` then
/// rename over the target, so a crash mid-write never leaves a truncated config.
/// Pretty-printed to match how Claude Code stores the file and stay diff-friendly.
fn write_config(path: &Path, root: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut body = serde_json::to_string_pretty(root).unwrap_or_else(|_| "{}".to_string());
    body.push('\n');
    let tmp = path.with_extension("json.nyx-tmp");
    std::fs::write(&tmp, body.as_bytes())?;
    // `rename` over an existing file is atomic on Unix; on Windows it can fail if the
    // destination exists, so fall back to a direct write there.
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            let res = std::fs::write(path, body.as_bytes());
            let _ = std::fs::remove_file(&tmp);
            res
        }
    }
}

/// Upsert nyx's MCP server entry into `root.mcpServers` for `url`, returning what
/// changed. **Pure** (no IO) so the idempotency + port-change logic is unit-tested
/// directly. Keys the entry on [`SERVER_NAME`], so a re-run finds and updates the
/// SAME slot instead of appending a duplicate (idempotency done-criterion). Only
/// nyx's slice is touched — other `mcpServers` entries and other top-level keys are
/// left exactly as they were.
pub fn merge_nyx_server(root: &mut Value, url: &str) -> OnboardConfigChange {
    // Ensure `root` is an object and `mcpServers` is an object within it, without
    // disturbing any sibling keys.
    let obj = root.as_object_mut().expect("read_config guarantees an object root");
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    let servers = servers.as_object_mut().expect("just ensured object");

    let desired = json!({ "type": "http", "url": url });

    // Classify the existing nyx slot under an immutable borrow, then mutate — keeps the
    // borrow checker happy and lets us PRESERVE any extra keys the user/another tool put
    // inside nyx's own entry (headers, auth, env, a stdio `command` form). We only ever
    // touch `url` (and ensure `type: http`); everything else in the entry survives.
    let existing_is_object = match servers.get(SERVER_NAME) {
        None => None,
        Some(v) => Some(v.is_object()),
    };
    match existing_is_object {
        // Present as an object → patch in place, preserving sibling keys. Decide
        // Unchanged-vs-Updated on the `url` (and `type` presence) alone so extra keys
        // neither force a needless rewrite nor get dropped.
        Some(true) => {
            let entry = servers
                .get_mut(SERVER_NAME)
                .and_then(Value::as_object_mut)
                .expect("just classified as object");
            let previous_url = entry
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let type_ok = entry.get("type").and_then(Value::as_str) == Some("http");
            entry.insert("url".to_string(), json!(url));
            entry.insert("type".to_string(), json!("http"));
            if previous_url == url && type_ok {
                OnboardConfigChange::Unchanged
            } else {
                OnboardConfigChange::Updated { previous_url }
            }
        }
        // Present but NOT an object (corrupt / hand-broken) → replace wholesale (nothing
        // worth preserving), reporting an unknown previous url.
        Some(false) => {
            servers.insert(SERVER_NAME.to_string(), desired);
            OnboardConfigChange::Updated {
                previous_url: String::new(),
            }
        }
        // Absent → add.
        None => {
            servers.insert(SERVER_NAME.to_string(), desired);
            OnboardConfigChange::Added
        }
    }
}

// ---------------------------------------------------------------------------
// Install-state persistence (task #1)
// ---------------------------------------------------------------------------

/// File name for the integration install-state store inside `app_data_dir`.
pub const INTEGRATIONS_FILE: &str = "integrations.json";

/// Provider identifiers supported by nyx's Integrations UI.
///
/// Only `claude_code` is fully functional in v1; `codex` and `opencode` are
/// advertised as "coming soon" in the UI. `custom` is reserved for a future
/// user-defined MCP server flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Provider {
    ClaudeCode,
}

impl Provider {
    /// Stable string key used in `integrations.json`. **Never change these**
    /// — they are persisted to disk and read back on every boot.
    pub fn key(&self) -> &'static str {
        match self {
            Provider::ClaudeCode => "claude_code",
        }
    }

    /// All providers nyx tracks install state for.
    fn all() -> &'static [Provider] {
        &[Provider::ClaudeCode]
    }
}

/// Per-provider install state, persisted to `<app_data_dir>/integrations.json`.
///
/// `installed: true` means the user explicitly installed the provider via the
/// Integrations UI and nyx owns the `nyx` entry in that provider's config.
/// `installed: false` (or absent) means nyx will never touch that provider's
/// config at boot.
#[derive(Debug, Clone, Default)]
pub struct IntegrationState {
    /// Provider key → installed flag.
    pub providers: HashMap<String, bool>,
}

impl IntegrationState {
    /// Load from the given path. A missing file → empty state (nothing installed).
    /// Parse errors are silently treated as empty state — a corrupt file never
    /// blocks boot.
    pub fn load(path: &Path) -> Self {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };
        let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
        let mut providers = HashMap::new();
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if let Some(b) = val.as_bool() {
                    providers.insert(k.clone(), b);
                }
            }
        }
        Self { providers }
    }

    /// Persist to the given path (pretty-printed, atomic-ish via `.tmp` + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut obj = Map::new();
        for (k, v) in &self.providers {
            obj.insert(k.clone(), Value::Bool(*v));
        }
        write_config(path, &Value::Object(obj))
    }

    /// Whether the given (component) key is marked as installed in the NON-authoritative
    /// cache. The key may be a bare provider (`"claude_code"`, the MCP server) or a
    /// component-qualified key (`"claude_code:plugin"`) — the store is a generic
    /// `key → bool` map so MCP and plugin install state are tracked INDEPENDENTLY
    /// (finding #23).
    ///
    /// Since review #40/#41 this cache is **no longer the source of truth** for install
    /// status or boot reconcile — both key off Claude Code's REAL config. The cache read
    /// is retained for the tests that assert the UI's own click is recorded; production
    /// status reads reality, so this has no non-test caller.
    #[allow(dead_code)]
    pub fn is_installed(&self, key: &str) -> bool {
        self.providers.get(key).copied().unwrap_or(false)
    }

    /// Mark a (component) key as installed (persists separately via [`save`]).
    pub fn set_installed(&mut self, key: &str, installed: bool) {
        self.providers.insert(key.to_string(), installed);
    }
}

// (The old per-component plugin state key — `plugin_state_key` / `PLUGIN_COMPONENT_SUFFIX`
// — was dropped with the split MCP/plugin install: there is now ONE integration unit, so
// `IntegrationState` tracks a single bare provider key, finding #45/#46.)

// ---------------------------------------------------------------------------
// Boot reconciliation (task #1)
// ---------------------------------------------------------------------------

/// One provider's resolved plugin-reconcile input: its [`crate::plugin::PluginInstall`]
/// descriptor + its CLI driver, or `None` when the bundled plugin / stable dir / CLI
/// could not be resolved. Built by the provider's adapter (no agent specifics leak into
/// the generic reconcile loop — finding #25).
pub type PluginReconcileEntry = Option<(crate::plugin::PluginInstall, Box<dyn crate::plugin::PluginCli>)>;

/// Boot-time reconciliation for the ONE bundled nyx plugin per provider (finding #45/#47).
///
/// The nyx Claude integration is now a SINGLE plugin that bundles BOTH the MCP server (its
/// `.mcp.json`, port-templated at copy time) AND the session-capture hooks — so the boot
/// reconcile only has to keep that plugin current; there is no separate standalone MCP
/// entry to refresh anymore (the old `mcpServers.nyx` write is gone).
///
/// **Never installs on boot.** The plugin reconcile is conservative:
/// - plugin **absent** from the real registry → no-op (the user removed it; we never
///   re-install silently);
/// - plugin **present** → re-copy the bundled content (re-templating the live MCP port,
///   picking up a bundled version bump) + re-register at the stable path, and on a content
///   change propagate the bump through Claude's caches (`marketplace update` + `plugin
///   update`, finding #47). Idempotent when nothing changed.
///
/// The legacy standalone `mcpServers.nyx` residue is NOT touched at boot — boot stays
/// purely conservative (a user mid-migration who still relies on the standalone MCP keeps
/// it working). It is stripped only on an explicit install/uninstall click (the bridge
/// cores call [`remove_legacy_mcp_server`]), where the bundled MCP replaces it.
///
/// `resolve_plugin` supplies each provider's [`crate::plugin::PluginInstall`] descriptor +
/// its CLI driver (built by its adapter, so no agent specifics leak into this generic loop
/// — finding #25). Called from `lib.rs` after the MCP server is bound. Best-effort: any IO
/// error is logged and skipped, never a boot failure. `_state_path` is retained for
/// signature stability (the stored flag is a non-authoritative cache the UI writes).
pub fn reconcile_installed_providers(
    _port: u16,
    _state_path: &Path,
    resolve_plugin: impl Fn(&str) -> PluginReconcileEntry,
) {
    // Plugin reconcile: every provider — absent from the real registry → no-op (never
    // install on boot); present → re-copy (re-template port / pick up a version bump) +
    // re-register + propagate the bump (finding #47).
    let plugins: Vec<(&'static str, PluginReconcileEntry)> = Provider::all()
        .iter()
        .map(|p| (p.key(), resolve_plugin(p.key())))
        .collect();
    reconcile_installed_plugins(&plugins);

    for client in detect_unsupported_clients() {
        eprintln!("nyx: detected MCP client '{client}' (not auto-configured in v1; add manually)");
    }
}

/// Testable core of the boot PLUGIN reconcile: for a pre-built list of
/// `(provider_key, Option<(PluginInstall, PluginCli)>)`, reconcile each installed
/// provider's plugin via [`crate::plugin::reconcile_with`]. Conservative (finding #24):
/// plugin **absent** from the real registry → no-op; **present** → re-copy the bundled
/// content + re-register at the stable path, healing a drifted/dead path (review #34). A
/// `None` (unresolvable bundled plugin / stable dir / CLI) is skipped silently.
/// Best-effort: a CLI error (e.g. `claude` not on PATH) is logged and skipped, never a
/// boot failure.
pub fn reconcile_installed_plugins(plugins: &[(&str, PluginReconcileEntry)]) {
    for (key, maybe) in plugins {
        let Some((install, cli)) = maybe else {
            eprintln!("nyx: skipping plugin reconcile for '{key}' (plugin path / CLI not resolvable)");
            continue;
        };
        match crate::plugin::reconcile_with(install, cli.as_ref()) {
            Ok(crate::plugin::ReconcileOutcome::SkippedAbsent) => {
                eprintln!("nyx: plugin reconcile skipped for {key} (plugin absent; not re-installing)");
            }
            Ok(crate::plugin::ReconcileOutcome::Unchanged) => {
                eprintln!("nyx: {key} plugin unchanged");
            }
            Ok(crate::plugin::ReconcileOutcome::Updated) => {
                eprintln!("nyx: refreshed {key} plugin (re-copied + re-registered at the stable path)");
            }
            Err(e) => eprintln!("nyx: could not reconcile {key} plugin: {e}"),
        }
    }
}

/// Testable core: reconcile a pre-built list of `(provider_key, target)` pairs.
/// In tests, each target points at a temp file instead of the real user config.
/// The reconciliation semantics are:
/// - Target resolves (`Some`) AND `nyx` entry present → update url only.
/// - Target resolves (`Some`) BUT `nyx` entry absent → no-op (do NOT add).
/// - Target is `None` (no home dir / unresolvable) → skip silently.
pub fn reconcile_providers_with_targets(
    port: u16,
    targets: &[(&str, Option<OnboardingTarget>)],
) {
    let url = mcp_url(port);
    for (key, maybe_target) in targets {
        let Some(target) = maybe_target else {
            eprintln!("nyx: skipping reconcile for '{key}' (config path not resolvable)");
            continue;
        };
        // Read the existing config — a missing file means no entry to update.
        let mut root = match read_config(&target.config_path) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("nyx: could not read {} config: {e}", target.client);
                continue;
            }
        };
        // Check whether the `nyx` entry is already present.
        let already_present = root
            .get("mcpServers")
            .and_then(|s| s.get(SERVER_NAME))
            .is_some();
        if !already_present {
            // Absent → no-op: we do NOT silently recreate the entry.
            eprintln!(
                "nyx: reconcile skipped for {} (nyx entry absent; not re-creating)",
                target.client
            );
            continue;
        }
        // Present → update url/port in place.
        let change = merge_nyx_server(&mut root, &url);
        match change {
            OnboardConfigChange::Unchanged => {
                eprintln!("nyx: {} MCP url unchanged ({})", target.client, url);
            }
            OnboardConfigChange::Updated { ref previous_url } => {
                if let Err(e) = write_config(&target.config_path, &root) {
                    eprintln!("nyx: could not update {} config: {e}", target.client);
                } else {
                    eprintln!(
                        "nyx: updated {} MCP url {previous_url} -> {url} (port change)",
                        target.client
                    );
                }
            }
            // merge_nyx_server returns Added only when absent, which we guarded above.
            OnboardConfigChange::Added => {}
        }
    }
}

/// Detect installed-but-not-yet-supported MCP clients so the UI/log can *signal*
/// them without auto-configuring (ADR-0003 D10: "detected but not configured if the
/// format is not yet validated"). Returns the client names whose user-scope config
/// is present on disk but which nyx does not auto-onboard in PRD-4.
///
/// Conservative: nyx only auto-onboards Claude Code in v1, so any other known client
/// found on disk is reported, not written.
pub fn detect_unsupported_clients() -> Vec<&'static str> {
    let mut found = Vec::new();
    // Cursor stores user MCP config at `~/.cursor/mcp.json`; its entry shape is not
    // validated in PRD-4, so we only report it.
    if let Some(home) = home_dir() {
        if home.join(".cursor").join("mcp.json").exists() {
            found.push("Cursor");
        }
        // Windsurf: `~/.codeium/windsurf/mcp_config.json`.
        if home.join(".codeium").join("windsurf").join("mcp_config.json").exists() {
            found.push("Windsurf");
        }
    }
    found
}

#[cfg(test)]
mod tests {
    //! Tests run EXCLUSIVELY against temp files via [`OnboardingTarget::new`] — they
    //! never read or write the user's real `~/.claude.json` (the
    //! testingDecisions safety requirement). The pure merge logic is also tested
    //! directly with no IO.

    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A process-unique temp config path (no `tempfile` dep). Each test gets its own
    /// file under the OS temp dir so the suite never collides or touches real config.
    fn temp_config(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("nyx-onboard-{}-{}-{}", std::process::id(), tag, n));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(".claude.json")
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    // --- pure merge logic -------------------------------------------------

    #[test]
    fn merge_adds_nyx_http_localhost_entry() {
        let mut root = json!({});
        let change = merge_nyx_server(&mut root, &mcp_url(8765));
        assert_eq!(change, OnboardConfigChange::Added);
        let entry = &root["mcpServers"]["nyx"];
        assert_eq!(entry["type"], "http");
        // Done-criterion: the URL is localhost direct, never portless.
        assert_eq!(entry["url"], "http://127.0.0.1:8765/mcp");
        assert!(!entry["url"].as_str().unwrap().contains("nyx.localhost"));
    }

    #[test]
    fn merge_is_idempotent_no_duplicate() {
        let mut root = json!({});
        merge_nyx_server(&mut root, &mcp_url(8765));
        let after_first = root.clone();
        let change = merge_nyx_server(&mut root, &mcp_url(8765));
        assert_eq!(change, OnboardConfigChange::Unchanged);
        // Byte-for-byte stable across a re-run, and exactly ONE nyx entry.
        assert_eq!(root, after_first);
        assert_eq!(root["mcpServers"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn merge_updates_url_in_place_on_port_change() {
        let mut root = json!({});
        merge_nyx_server(&mut root, &mcp_url(8765));
        let change = merge_nyx_server(&mut root, &mcp_url(9999));
        assert_eq!(
            change,
            OnboardConfigChange::Updated { previous_url: "http://127.0.0.1:8765/mcp".into() }
        );
        // Updated IN PLACE: still exactly one entry, now on the new port.
        assert_eq!(root["mcpServers"].as_object().unwrap().len(), 1);
        assert_eq!(root["mcpServers"]["nyx"]["url"], "http://127.0.0.1:9999/mcp");
    }

    #[test]
    fn merge_preserves_other_servers_and_top_level_keys() {
        let mut root = json!({
            "numStartups": 12,
            "mcpServers": {
                "other": { "type": "http", "url": "http://127.0.0.1:1234/mcp" }
            }
        });
        merge_nyx_server(&mut root, &mcp_url(8765));
        // Sibling top-level key untouched.
        assert_eq!(root["numStartups"], 12);
        // Sibling MCP server untouched, nyx added alongside it.
        assert_eq!(root["mcpServers"]["other"]["url"], "http://127.0.0.1:1234/mcp");
        assert_eq!(root["mcpServers"]["nyx"]["url"], "http://127.0.0.1:8765/mcp");
        assert_eq!(root["mcpServers"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn merge_coerces_non_object_mcpservers() {
        // A malformed `mcpServers` (string) is replaced by a fresh object rather
        // than panicking — onboarding stays robust.
        let mut root = json!({ "mcpServers": "oops" });
        let change = merge_nyx_server(&mut root, &mcp_url(8765));
        assert_eq!(change, OnboardConfigChange::Added);
        assert_eq!(root["mcpServers"]["nyx"]["type"], "http");
    }

    // --- file IO (temp files only) ---------------------------------------

    #[test]
    fn onboard_creates_file_when_missing() {
        let path = temp_config("missing");
        std::fs::remove_file(&path).ok(); // ensure absent
        let target = OnboardingTarget::new("Claude Code", &path);
        let change = target.onboard(8765).unwrap();
        assert_eq!(change, OnboardConfigChange::Added);
        let v = read_json(&path);
        assert_eq!(v["mcpServers"]["nyx"]["url"], "http://127.0.0.1:8765/mcp");
    }

    #[test]
    fn onboard_rerun_does_not_duplicate_and_is_stable() {
        let path = temp_config("rerun");
        let target = OnboardingTarget::new("Claude Code", &path);
        assert_eq!(target.onboard(8765).unwrap(), OnboardConfigChange::Added);
        let bytes_after_first = std::fs::read(&path).unwrap();
        // Re-run: no-op, and the file on disk is byte-for-byte identical.
        assert_eq!(target.onboard(8765).unwrap(), OnboardConfigChange::Unchanged);
        assert_eq!(std::fs::read(&path).unwrap(), bytes_after_first);
        assert_eq!(read_json(&path)["mcpServers"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn onboard_updates_cleanly_when_port_changes() {
        let path = temp_config("portchange");
        let target = OnboardingTarget::new("Claude Code", &path);
        target.onboard(8765).unwrap();
        let change = target.onboard(9100).unwrap();
        assert_eq!(
            change,
            OnboardConfigChange::Updated { previous_url: "http://127.0.0.1:8765/mcp".into() }
        );
        let v = read_json(&path);
        // Clean update: one entry, new port, no stale duplicate.
        assert_eq!(v["mcpServers"].as_object().unwrap().len(), 1);
        assert_eq!(v["mcpServers"]["nyx"]["url"], "http://127.0.0.1:9100/mcp");
    }

    #[test]
    fn onboard_preserves_unrelated_config_on_disk() {
        let path = temp_config("preserve");
        std::fs::write(
            &path,
            r#"{"numStartups":7,"mcpServers":{"other":{"type":"http","url":"http://127.0.0.1:1/mcp"}}}"#,
        )
        .unwrap();
        OnboardingTarget::new("Claude Code", &path).onboard(8765).unwrap();
        let v = read_json(&path);
        assert_eq!(v["numStartups"], 7);
        assert_eq!(v["mcpServers"]["other"]["url"], "http://127.0.0.1:1/mcp");
        assert_eq!(v["mcpServers"]["nyx"]["url"], "http://127.0.0.1:8765/mcp");
    }

    #[test]
    fn mcp_url_is_localhost_direct_never_portless() {
        let url = mcp_url(8765);
        assert_eq!(url, "http://127.0.0.1:8765/mcp");
        assert!(!url.contains("nyx.localhost"));
        assert!(url.starts_with("http://127.0.0.1:"));
    }

    // --- integration-state persistence ------------------------------------

    #[test]
    fn integration_state_default_is_not_installed() {
        let state = IntegrationState::default();
        assert!(!state.is_installed("claude_code"));
    }

    #[test]
    fn integration_state_round_trips_to_disk() {
        let path = temp_config("state-rt");
        let state_path = path.with_file_name("integrations.json");
        let mut state = IntegrationState::default();
        state.set_installed("claude_code", true);
        state.save(&state_path).unwrap();
        let loaded = IntegrationState::load(&state_path);
        assert!(loaded.is_installed("claude_code"));
    }

    #[test]
    fn integration_state_load_missing_file_is_empty() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("nyx-state-missing-{}", std::process::id()));
        let state_path = dir.join("integrations.json");
        // File does not exist → empty state, not an error.
        let state = IntegrationState::load(&state_path);
        assert!(!state.is_installed("claude_code"));
    }

    // --- boot reconciliation (task #1) ------------------------------------

    /// Reconcile when the nyx entry IS already present in the provider config:
    /// the url must be updated to the new port, but the entry must NOT be
    /// duplicated and other keys must be untouched.
    #[test]
    fn reconcile_present_entry_updates_url_in_place() {
        let config_path = temp_config("recon-present");
        // Pre-seed the provider config with a nyx entry on the old port.
        std::fs::write(
            &config_path,
            r#"{"numStartups":3,"mcpServers":{"nyx":{"type":"http","url":"http://127.0.0.1:8765/mcp"}}}"#,
        )
        .unwrap();

        let targets: Vec<(&str, Option<OnboardingTarget>)> =
            vec![("claude_code", Some(OnboardingTarget::new("Claude Code", &config_path)))];
        reconcile_providers_with_targets(9100, &targets);

        let v = read_json(&config_path);
        // Updated in place: still exactly one entry, now on the new port.
        assert_eq!(v["mcpServers"].as_object().unwrap().len(), 1);
        assert_eq!(v["mcpServers"]["nyx"]["url"], "http://127.0.0.1:9100/mcp");
        // Sibling top-level key untouched.
        assert_eq!(v["numStartups"], 3);
    }

    /// Reconcile when the nyx entry is ABSENT from the provider config:
    /// nothing must be created — the file must remain byte-for-byte identical.
    #[test]
    fn reconcile_absent_entry_is_noop() {
        let config_path = temp_config("recon-absent");
        let original = r#"{"numStartups":5,"mcpServers":{"other":{"type":"http","url":"http://127.0.0.1:1/mcp"}}}"#;
        std::fs::write(&config_path, original).unwrap();

        let targets: Vec<(&str, Option<OnboardingTarget>)> =
            vec![("claude_code", Some(OnboardingTarget::new("Claude Code", &config_path)))];
        reconcile_providers_with_targets(9100, &targets);

        // File unchanged: nyx entry was NOT created.
        let v = read_json(&config_path);
        assert!(v["mcpServers"].get("nyx").is_none(), "nyx must not be added silently");
        assert_eq!(v["numStartups"], 5);
        assert_eq!(v["mcpServers"]["other"]["url"], "http://127.0.0.1:1/mcp");
        // Exactly one entry (the pre-existing 'other').
        assert_eq!(v["mcpServers"].as_object().unwrap().len(), 1);
    }

    // --- REAL MCP install status from ~/.claude.json (review #41) ---------

    /// `mcpServers.nyx` present → installed. The REAL signal `merge_nyx_server` writes,
    /// read against a temp config (symmetric to the plugin's enabledPlugins check).
    #[test]
    fn mcp_present_when_nyx_server_entry_exists() {
        let path = temp_config("mcp-present");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"nyx":{"type":"http","url":"http://127.0.0.1:8765/mcp"},"other":{}}}"#,
        )
        .unwrap();
        assert!(mcp_server_present(&path));
    }

    /// `mcpServers.nyx` ABSENT → not installed — even when OTHER servers are present. This
    /// is the state after a user removes nyx's MCP server directly in Claude Code.
    #[test]
    fn mcp_absent_when_nyx_server_entry_missing() {
        let path = temp_config("mcp-absent");
        std::fs::write(&path, r#"{"mcpServers":{"other":{"type":"http","url":"http://x/mcp"}}}"#).unwrap();
        assert!(!mcp_server_present(&path));
        // And a config with no mcpServers at all.
        let path2 = temp_config("mcp-none");
        std::fs::write(&path2, r#"{"numStartups":3}"#).unwrap();
        assert!(!mcp_server_present(&path2));
    }

    /// A missing config file → not installed (no panic).
    #[test]
    fn mcp_absent_when_config_missing() {
        let path = temp_config("mcp-missing");
        std::fs::remove_file(&path).ok();
        assert!(!path.exists());
        assert!(!mcp_server_present(&path));
    }

    // --- legacy standalone-MCP strip (finding #45) ------------------------

    /// `remove_legacy_mcp_server` strips ONLY the `mcpServers.nyx` entry — leaving sibling
    /// servers and other top-level keys intact — and reports whether it removed anything.
    /// A second strip is a no-op (idempotent); a missing/empty config is a clean no-op.
    #[test]
    fn remove_legacy_mcp_server_strips_only_nyx() {
        let path = temp_config("strip-legacy-mcp");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"nyx":{"type":"http","url":"http://127.0.0.1:8765/mcp"},"other":{"type":"http","url":"http://x/mcp"}},"numStartups":3}"#,
        )
        .unwrap();
        assert!(remove_legacy_mcp_server(&path).unwrap(), "nyx present → stripped");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["mcpServers"].get("nyx").is_none(), "nyx MCP entry gone");
        assert!(v["mcpServers"].get("other").is_some(), "sibling server preserved");
        assert_eq!(v["numStartups"], 3, "unrelated top-level key preserved");
        // Idempotent: a second strip removes nothing.
        assert!(!remove_legacy_mcp_server(&path).unwrap(), "already-stripped → no-op");

        // A missing file is a clean no-op.
        let missing = temp_config("strip-missing");
        std::fs::remove_file(&missing).ok();
        assert!(!remove_legacy_mcp_server(&missing).unwrap(), "missing config → nothing to strip");
    }

    /// `claude_mcp_installed` resolves the `NYX_CLAUDE_CONFIG` seam and reads the real
    /// presence — present→true / absent→false — WITHOUT touching the user's `~/.claude.json`.
    #[test]
    fn claude_mcp_installed_honors_the_config_seam() {
        // Genuinely exercises the `NYX_CLAUDE_CONFIG` env RESOLUTION, so it mutates the
        // process-global seam. It takes the ONE crate-wide lock every seam-mutating test
        // shares (review #42/#43) — NOT a private mutex — so it never interleaves with a
        // seam mutation in another module. Prior value restored on exit (no leak).
        let _g = crate::CLAUDE_ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var_os("NYX_CLAUDE_CONFIG");

        let path = temp_config("seam-mcp");
        std::env::set_var("NYX_CLAUDE_CONFIG", &path);

        // Present → installed.
        std::fs::write(&path, r#"{"mcpServers":{"nyx":{"type":"http","url":"http://127.0.0.1:8765/mcp"}}}"#).unwrap();
        assert!(claude_mcp_installed(), "mcpServers.nyx present → installed via the seam");

        // Absent → not installed (the direct-removal case).
        std::fs::write(&path, r#"{"mcpServers":{}}"#).unwrap();
        assert!(!claude_mcp_installed(), "mcpServers.nyx gone → not installed via the seam");

        match prev {
            Some(v) => std::env::set_var("NYX_CLAUDE_CONFIG", v),
            None => std::env::remove_var("NYX_CLAUDE_CONFIG"),
        }
    }

    /// Reconcile with already-current url: file must not be touched (no write).
    #[test]
    fn reconcile_present_unchanged_url_is_noop() {
        let config_path = temp_config("recon-unchanged");
        let content = r#"{"mcpServers":{"nyx":{"type":"http","url":"http://127.0.0.1:8765/mcp"}}}"#;
        std::fs::write(&config_path, content).unwrap();
        let mtime_before = std::fs::metadata(&config_path).unwrap().modified().unwrap();

        let targets: Vec<(&str, Option<OnboardingTarget>)> =
            vec![("claude_code", Some(OnboardingTarget::new("Claude Code", &config_path)))];
        reconcile_providers_with_targets(8765, &targets);

        // File not rewritten: mtime must be unchanged.
        let mtime_after = std::fs::metadata(&config_path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "file must not be rewritten when url is already current"
        );
    }

    // --- boot PLUGIN reconcile orchestration (PRD-5 #24 / review #34) ------

    /// A recording fake of the plugin CLI for the boot-reconcile orchestration tests:
    /// models a single optional `nyx` marketplace registration in memory so reconcile's
    /// absent-vs-present branches are observable without the real `claude` binary.
    #[derive(Default)]
    struct FakeReconcileCli {
        registered: std::cell::RefCell<Option<std::path::PathBuf>>,
    }

    impl crate::plugin::PluginCli for FakeReconcileCli {
        fn marketplace_add(&self, dir: &Path) -> Result<(), crate::plugin::PluginError> {
            *self.registered.borrow_mut() = Some(dir.to_path_buf());
            Ok(())
        }
        fn install(&self, _id: &str) -> Result<(), crate::plugin::PluginError> {
            Ok(())
        }
        fn uninstall(&self, _id: &str) -> Result<(), crate::plugin::PluginError> {
            Ok(())
        }
        fn marketplace_remove(&self, _m: &str) -> Result<(), crate::plugin::PluginError> {
            *self.registered.borrow_mut() = None;
            Ok(())
        }
        fn marketplace_update(&self, _m: &str) -> Result<(), crate::plugin::PluginError> {
            Ok(())
        }
        fn plugin_update(&self, _id: &str) -> Result<(), crate::plugin::PluginError> {
            Ok(())
        }
        fn marketplace_list(&self) -> Result<Vec<crate::plugin::MarketplaceEntry>, crate::plugin::PluginError> {
            Ok(self
                .registered
                .borrow()
                .clone()
                .map(|p| vec![crate::plugin::MarketplaceEntry { name: "nyx".to_string(), path: Some(p) }])
                .unwrap_or_default())
        }
    }

    fn plugin_install_for(source: &Path, stable: &Path, settings: &Path) -> crate::plugin::PluginInstall {
        std::fs::create_dir_all(source.join(".claude-plugin")).unwrap();
        std::fs::write(source.join(".claude-plugin").join("marketplace.json"), "{}").unwrap();
        crate::plugin::PluginInstall {
            marketplace: crate::plugin::CLAUDE_MARKETPLACE.to_string(),
            plugin: crate::plugin::CLAUDE_PLUGIN_NAME.to_string(),
            source_dir: source.to_path_buf(),
            install_dir: stable.to_path_buf(),
            settings_path: settings.to_path_buf(),
            mcp_port: 8765,
        }
    }

    /// Boot plugin reconcile when the plugin is ABSENT from the real registry: NEVER
    /// install on boot — no copy, no CLI write.
    #[test]
    fn reconcile_plugin_absent_is_noop() {
        let base = temp_config("recon-plugin-absent").with_file_name("recon-absent");
        std::fs::create_dir_all(&base).unwrap();
        let inst = plugin_install_for(&base.join("src"), &base.join("stable"), &base.join("settings.json"));
        let cli = FakeReconcileCli::default(); // nothing registered

        reconcile_installed_plugins(&[("claude_code", Some((inst, Box::new(cli))))]);

        // Stable dir never created: the plugin was NOT installed silently.
        assert!(!base.join("stable").exists(), "absent → no copy on boot");
    }

    /// Boot plugin reconcile when the plugin IS present but registered at a STALE path
    /// (dev→packaged drift / the user's dead-path state): re-copy + re-register at the
    /// stable path, healing the entry (review #34).
    #[test]
    fn reconcile_plugin_present_heals_drifted_path() {
        let base = temp_config("recon-plugin-heal").with_file_name("recon-heal");
        std::fs::create_dir_all(&base).unwrap();
        let stable = base.join("stable");
        let inst = plugin_install_for(&base.join("src"), &stable, &base.join("settings.json"));
        let cli = FakeReconcileCli::default();
        // Present but at a STALE/dead path (no stable copy yet).
        *cli.registered.borrow_mut() = Some(base.join("DEAD-stale-path"));
        let cli: Box<dyn crate::plugin::PluginCli> = Box::new(cli);

        reconcile_installed_plugins(&[("claude_code", Some((inst, cli)))]);

        // Healed: content copied to the stable dir.
        assert!(stable.join(".claude-plugin").join("marketplace.json").exists(), "content re-copied to the stable dir on heal");
    }
}
