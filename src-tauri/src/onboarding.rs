//! Claude Code MCP onboarding (PRD-4 #5, ADR-0003 D10/D11).
//!
//! nyx writes/updates the **user-scope** MCP config of supported agent clients so
//! they find nyx without fragile manual setup. PRD-4 implements **Claude Code** at
//! minimum; other clients can be *detected* but are not auto-configured until their
//! format is validated (ADR-0003 D10).
//!
//! Claude Code's user-scope config lives in a single JSON file at
//! `~/.claude.json` with a top-level `mcpServers` object that maps a server name to
//! its transport config. For nyx's HTTP loopback transport the entry is
//! `{ "type": "http", "url": "http://127.0.0.1:<port>/mcp" }` — **localhost direct**,
//! never the portless `nyx.localhost` URL (ADR-0003 D11). This mirrors what
//! `claude mcp add --transport http --scope user nyx <url>` writes.
//!
//! ## Properties this module guarantees
//! - **Idempotent** (done-criterion): re-running onboarding with the same port
//!   leaves the file byte-for-byte stable and never duplicates the `nyx` entry —
//!   the `mcpServers` map is keyed by name, and we upsert by [`SERVER_NAME`].
//! - **Clean port-change update** (testingDecisions): if the resolved port changes,
//!   the existing `nyx` entry's `url` is rewritten in place to the new port; no
//!   stale duplicate is left behind.
//! - **Preserves the rest of the file**: every other top-level key and every other
//!   `mcpServers` entry is round-tripped untouched — we parse, mutate only nyx's
//!   slice, and re-serialize.
//!
//! ## Boot reconciliation (task #1)
//! nyx no longer silently auto-installs itself into every client config at boot.
//! Instead, [`reconcile_installed_providers`] runs at boot and operates only on
//! providers the user has explicitly installed (tracked in
//! `<app_data_dir>/integrations.json` via [`IntegrationState`]):
//! - **Present in provider config** → keep it up to date (`url`/`port` only).
//! - **Absent from provider config** → do nothing (never create silently).
//! - **Not marked installed** → skip entirely (not our entry to manage).
//!
//! The install/update/remove UI actions (Settings → Integrations) write the
//! `integrations.json` state and immediately call the relevant provider mutation.
//!
//! ## Safety / testability
//! The config path is **injectable**: [`OnboardingTarget::claude_code`] resolves the
//! real `~/.claude.json`, but [`OnboardingTarget::new`] takes an arbitrary path so
//! tests (and any caller that must not touch the user's real config) point at a
//! temp file. The pure JSON merge logic ([`merge_nyx_server`]) is a free function
//! over `serde_json::Value` with no IO, unit-tested directly.
//! [`IntegrationState`] IO is similarly injectable via [`reconcile_providers`].

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
fn home_dir() -> Option<PathBuf> {
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

/// Public re-export of [`write_config`] for callers that hold a config path directly.
pub fn write_config_pub(path: &Path, root: &Value) -> std::io::Result<()> {
    write_config(path, root)
}

/// Read a client config file into a JSON object. A missing file → an empty object
/// (first onboarding). A present-but-non-object root is replaced by an empty object
/// rather than failing, so a corrupt scalar can't wedge onboarding.
fn read_config(path: &Path) -> std::io::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let parsed: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
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

    match servers.get(SERVER_NAME) {
        // Exact match already present → idempotent no-op.
        Some(existing) if existing == &desired => OnboardConfigChange::Unchanged,
        // Present but differs (e.g. the port changed, or a hand-edited entry) →
        // rewrite in place. Capture the prior url for the log, if any.
        Some(existing) => {
            let previous_url = existing
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            servers.insert(SERVER_NAME.to_string(), desired);
            OnboardConfigChange::Updated { previous_url }
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

    /// Build the [`OnboardingTarget`] for this provider (injectable config path
    /// is the real user-scope config). Returns `None` if the path cannot be
    /// resolved (e.g. no home dir).
    pub fn target(&self) -> Option<OnboardingTarget> {
        match self {
            Provider::ClaudeCode => OnboardingTarget::claude_code(),
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

    /// Whether the given provider key is marked as installed.
    pub fn is_installed(&self, key: &str) -> bool {
        self.providers.get(key).copied().unwrap_or(false)
    }

    /// Mark a provider as installed (persists separately via [`save`]).
    pub fn set_installed(&mut self, key: &str, installed: bool) {
        self.providers.insert(key.to_string(), installed);
    }
}

// ---------------------------------------------------------------------------
// Boot reconciliation (task #1)
// ---------------------------------------------------------------------------

/// Boot-time reconciliation: update the `nyx` entry in every provider config
/// that the user has already installed, keyed on `integrations.json` state.
///
/// **Never creates a new entry** for a provider not already installed — this is
/// the fundamental invariant that replaces the old auto-onboard behavior:
/// - Provider **installed** and `nyx` entry **present** in its config → update
///   `url`/`port` in place (port may have changed since last boot).
/// - Provider **installed** but `nyx` entry **absent** from its config → no-op
///   (the user may have manually removed it; we do not re-create silently).
/// - Provider **not installed** → skip entirely.
///
/// This is called from `lib.rs` after the MCP server is bound and its port is
/// known. Best-effort: any IO error on a per-provider update is logged and
/// skipped; it never causes a boot failure.
///
/// For testability the state path and the per-provider config paths are
/// injectable — see [`reconcile_providers`] below.
pub fn reconcile_installed_providers(port: u16, state_path: &Path) {
    let state = IntegrationState::load(state_path);
    let targets: Vec<(&'static str, Option<OnboardingTarget>)> = Provider::all()
        .iter()
        .filter(|p| state.is_installed(p.key()))
        .map(|p| (p.key(), p.target()))
        .collect();
    reconcile_providers_with_targets(port, &targets);
    for client in detect_unsupported_clients() {
        eprintln!("nyx: detected MCP client '{client}' (not auto-configured in v1; add manually)");
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
}
