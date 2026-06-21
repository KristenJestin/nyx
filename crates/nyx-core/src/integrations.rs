//! Provider-agnostic INTEGRATIONS cores (PRD-5 review #58) — the install / uninstall /
//! status logic that backs the Settings → Integrations UI, extracted out of the Tauri
//! adapter so BOTH shells drive the EXACT same `nyx_core` logic over the same
//! [`crate::plugin`] + [`crate::onboarding`] seams.
//!
//! Under Tauri these cores were inline in `apps/tauri/src-tauri/src/bridge.rs`
//! (`do_integration_install` / `do_integration_remove` / `integration_status_list`); the
//! Electron core-host had NO path to them, so `integration_install` / `integration_remove`
//! returned "not available over this transport" and the Settings Install/Uninstall button
//! was a dead end (the criterion of PRD-5 task #17 was not verifiable end-to-end). This
//! module is the shared home for that logic: the napi dispatcher
//! ([`crate::core_db` in nyx-napi]) calls [`install`] / [`remove`] / [`status_list`] so the
//! real renderer → preload → main → core-host → nyx-core round-trip reaches the SAME
//! `claude` plugin install/uninstall the Tauri command body reached.
//!
//! The cores are `AppHandle`-free and path-injectable (every external file is an explicit
//! argument), so they are unit-testable against temp paths + a fake [`crate::plugin::PluginCli`]
//! with no process-global state — the same testability the Tauri inline cores had.

use std::path::Path;

use serde::Serialize;

use crate::onboarding::{self, IntegrationState, OnboardingTarget, INTEGRATIONS_FILE};
use crate::plugin::{self, PluginCli, PluginInstall};

/// Status of one integration, returned to the front-end. The nyx Claude integration is ONE
/// bundled plugin that provides BOTH the MCP server and the session-capture hooks, so there
/// is a SINGLE `installed` flag (no split MCP/plugin state to desync). Serialized
/// camelCase so the wire shape is identical to the Tauri `IntegrationStatus` the renderer
/// already consumes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationStatus {
    /// Provider key (e.g. `"claude_code"`).
    pub provider: &'static str,
    /// Human-readable display name.
    pub label: &'static str,
    /// Whether the nyx integration (the ONE bundled plugin: MCP + hooks) is installed for
    /// this provider. For `claude_code` this is derived from Claude Code's REAL config —
    /// `enabledPlugins["nyx-claude-integration@nyx"] == true` — not nyx's stored flag, so a
    /// plugin removed directly in Claude Code reads as uninstalled at the next refresh.
    pub installed: bool,
    /// `true` when the provider is fully functional in v1; `false` = coming soon.
    pub available: bool,
}

/// The integration status list (parity with the Tauri `integration_status_list`): the 4
/// registry providers with their available / coming-soon flags. `claude_code`'s single
/// install flag is derived from Claude Code's REAL config ([`plugin::claude_plugin_enabled`]),
/// so the list reflects reality, not nyx's own cached flag.
pub fn status_list() -> Vec<IntegrationStatus> {
    vec![
        claude_status(),
        IntegrationStatus {
            provider: "codex",
            label: "Codex",
            installed: false,
            available: false,
        },
        IntegrationStatus {
            provider: "opencode",
            label: "OpenCode",
            installed: false,
            available: false,
        },
        // `custom` is reserved for a future user-defined MCP server flow (no semantics in
        // v1 → coming soon, like codex/opencode). Listed so the UI shows all 4 providers.
        IntegrationStatus {
            provider: "custom",
            label: "Custom",
            installed: false,
            available: false,
        },
    ]
}

/// The `claude_code` integration status from Claude Code's REAL config (the authoritative
/// signal — `enabledPlugins["nyx-claude-integration@nyx"] == true` in
/// `~/.claude/settings.json`, honoring the `NYX_CLAUDE_SETTINGS` test seam).
fn claude_status() -> IntegrationStatus {
    IntegrationStatus {
        provider: "claude_code",
        label: "Claude Code",
        installed: plugin::claude_plugin_enabled(),
        available: true,
    }
}

/// Same as [`claude_status`] but reads the REAL Claude `settings.json` at an EXPLICIT path
/// instead of resolving the `NYX_CLAUDE_SETTINGS` seam. The install/remove cores report the
/// post-mutation status from the very file the plugin CLI just wrote — keeping them testable
/// against temp paths with no process-global env.
fn claude_status_at(settings_path: &Path) -> IntegrationStatus {
    IntegrationStatus {
        provider: "claude_code",
        label: "Claude Code",
        installed: plugin::plugin_enabled_in_settings(
            settings_path,
            &plugin::claude_plugin_install_id(),
        ),
        available: true,
    }
}

/// The Claude `settings.json` path to read the post-mutation plugin status from: the plugin
/// descriptor's `settings_path` when one was resolved (the same file the CLI install/uninstall
/// writes `enabledPlugins` into), else the resolved real seam.
fn claude_settings_path_for(plugin_install: Option<&PluginInstall>) -> std::path::PathBuf {
    plugin_install
        .map(|p| p.settings_path.clone())
        .or_else(plugin::claude_settings_path)
        .unwrap_or_default()
}

/// Core of `integration_install` (parity with the Tauri `do_integration_install`). Installs
/// the ONE nyx Claude integration — the bundled plugin that provides BOTH the MCP server and
/// the session-capture hooks. There is no separate MCP write: the plugin's `.mcp.json`
/// declares the MCP, so to avoid a double-declaration we also strip any legacy standalone
/// `mcpServers.nyx` left by the old flow. A missing `claude` CLI surfaces as a typed error
/// (no fake success). Unit-testable against temp paths.
pub fn install(
    provider: &str,
    target: &OnboardingTarget,
    plugin_install: Option<&PluginInstall>,
    plugin_cli: Option<&dyn PluginCli>,
    state_path: &Path,
) -> Result<IntegrationStatus, String> {
    if provider != "claude_code" {
        return Err(format!("provider '{provider}' is not supported in v1"));
    }
    let descriptor = plugin_install.ok_or_else(|| {
        "Could not resolve the bundled nyx plugin (no plugin dir / app data dir)".to_string()
    })?;
    let cli =
        plugin_cli.ok_or_else(|| "Could not resolve the Claude plugin CLI driver".to_string())?;
    // Install the ONE plugin (copy + port-template + register via the CLI). The plugin
    // bundles the MCP, so this is the whole integration.
    plugin::install_with(descriptor, cli).map_err(|e| e.to_string())?;
    // Drop any legacy standalone MCP so it is not declared twice (the plugin now owns it).
    let _ = onboarding::remove_legacy_mcp_server(&target.config_path);

    // Mark nyx's own (non-authoritative) install cache flag, kept for back-compat — the real
    // status is read from Claude's config below. Best-effort: this flag is NOT the status
    // authority, and the plugin is already installed, so a save failure must NOT fail the
    // install.
    let mut state = IntegrationState::load(state_path);
    state.set_installed("claude_code", true);
    if let Err(e) = state.save(state_path) {
        eprintln!("integration_install: persisting install cache flag failed (non-fatal): {e}");
    }
    Ok(claude_status_at(&claude_settings_path_for(plugin_install)))
}

/// Core of `integration_remove` (parity with the Tauri `do_integration_remove`). The mirror
/// of install: uninstalls the ONE nyx plugin (CLI uninstall + marketplace remove) AND cleans
/// every legacy residue so nothing nyx lingers — the legacy standalone `mcpServers.nyx` plus
/// the legacy hand-written settings keys. Best-effort: a `None` descriptor / CLI still clears
/// the legacy MCP + the state flag. Unit-testable against temp paths.
pub fn remove(
    provider: &str,
    target: &OnboardingTarget,
    plugin_install: Option<&PluginInstall>,
    plugin_cli: Option<&dyn PluginCli>,
    state_path: &Path,
) -> Result<IntegrationStatus, String> {
    if provider != "claude_code" {
        return Err(format!("provider '{provider}' is not supported in v1"));
    }
    // Uninstall the plugin + remove the marketplace + strip legacy settings keys. Best-effort,
    // but don't swallow the error SILENTLY: the returned status is read from Claude's real
    // config (so it stays honest), yet a failed CLI uninstall should be surfaced in the log.
    if let (Some(descriptor), Some(cli)) = (plugin_install, plugin_cli) {
        if let Err(e) = plugin::remove_with(descriptor, cli) {
            eprintln!("integration_remove: plugin uninstall failed (best-effort): {e}");
        }
    }
    // Strip the legacy standalone MCP server entry (residue from the old separate-MCP flow).
    let _ = onboarding::remove_legacy_mcp_server(&target.config_path);

    // Non-authoritative cache flag (see install) — best-effort, never fatal.
    let mut state = IntegrationState::load(state_path);
    state.set_installed("claude_code", false);
    if let Err(e) = state.save(state_path) {
        eprintln!("integration_remove: persisting install cache flag failed (non-fatal): {e}");
    }
    Ok(claude_status_at(&claude_settings_path_for(plugin_install)))
}

/// The integrations-state file path under `data_dir` (`<data_dir>/integrations.json`) — the
/// non-authoritative install cache the install/remove cores persist. Shared so every caller
/// resolves the SAME file the Tauri shell does.
pub fn state_path_in(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(INTEGRATIONS_FILE)
}

#[cfg(test)]
mod tests {
    //! Tests run EXCLUSIVELY against temp dirs + a FAKE CLI — they never touch the user's real
    //! `~/.claude` and never shell out to the real `claude` binary. They prove the cores route
    //! to the plugin install/uninstall + flip the persisted state flag + return a status read
    //! from the real (temp) settings file the CLI wrote.

    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("nyx-integrations-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake plugin CLI that records calls and writes `enabledPlugins[<id>]` into the
    /// settings file on install / clears it on uninstall — so `claude_status_at` reading the
    /// SAME file reflects the CLI's effect (the parity the real `claude` CLI provides).
    struct FakeCli {
        settings_path: PathBuf,
        install_id: String,
        calls: RefCell<Vec<String>>,
    }

    impl FakeCli {
        fn new(settings_path: PathBuf) -> Self {
            FakeCli {
                settings_path,
                install_id: plugin::claude_plugin_install_id(),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn set_enabled(&self, enabled: bool) {
            let mut root = onboarding::read_config_pub(&self.settings_path)
                .unwrap_or_else(|_| serde_json::json!({}));
            let obj = root.as_object_mut().unwrap();
            let map = obj
                .entry("enabledPlugins")
                .or_insert_with(|| serde_json::json!({}));
            map.as_object_mut()
                .unwrap()
                .insert(self.install_id.clone(), serde_json::Value::Bool(enabled));
            onboarding::write_config_pub(&self.settings_path, &root).unwrap();
        }
    }

    impl PluginCli for FakeCli {
        fn marketplace_add(&self, _dir: &Path) -> Result<(), plugin::PluginError> {
            self.calls.borrow_mut().push("marketplace_add".into());
            Ok(())
        }
        fn install(&self, _install_id: &str) -> Result<(), plugin::PluginError> {
            self.calls.borrow_mut().push("install".into());
            self.set_enabled(true);
            Ok(())
        }
        fn uninstall(&self, _install_id: &str) -> Result<(), plugin::PluginError> {
            self.calls.borrow_mut().push("uninstall".into());
            self.set_enabled(false);
            Ok(())
        }
        fn marketplace_remove(&self, _marketplace: &str) -> Result<(), plugin::PluginError> {
            self.calls.borrow_mut().push("marketplace_remove".into());
            Ok(())
        }
        fn marketplace_list(&self) -> Result<Vec<plugin::MarketplaceEntry>, plugin::PluginError> {
            Ok(Vec::new())
        }
        fn marketplace_update(&self, _marketplace: &str) -> Result<(), plugin::PluginError> {
            Ok(())
        }
        fn plugin_update(&self, _install_id: &str) -> Result<(), plugin::PluginError> {
            Ok(())
        }
    }

    /// Build a `PluginInstall` descriptor over temp dirs with a minimal bundled plugin tree so
    /// `install_with` can copy + register it without touching the real bundle.
    fn temp_descriptor(root: &Path) -> PluginInstall {
        let source_dir = root.join("bundled");
        std::fs::create_dir_all(source_dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            source_dir.join(".claude-plugin").join("marketplace.json"),
            "{\"name\":\"nyx\"}",
        )
        .unwrap();
        PluginInstall {
            marketplace: plugin::CLAUDE_MARKETPLACE.to_string(),
            plugin: plugin::CLAUDE_PLUGIN_NAME.to_string(),
            source_dir,
            install_dir: root.join("stable"),
            settings_path: root.join("settings.json"),
            mcp_port: 4517,
        }
    }

    #[test]
    fn install_then_remove_round_trips_status_and_flag() {
        let root = temp_dir("rt");
        let descriptor = temp_descriptor(&root);
        let cli = FakeCli::new(descriptor.settings_path.clone());
        let target = OnboardingTarget::new("Claude Code", root.join("claude.json"));
        let state_path = root.join("integrations.json");

        // Install: the status reads back installed=true from the settings file the CLI wrote.
        let status = install(
            "claude_code",
            &target,
            Some(&descriptor),
            Some(&cli),
            &state_path,
        )
        .expect("install ok");
        assert_eq!(status.provider, "claude_code");
        assert!(
            status.installed,
            "post-install status must read installed=true"
        );
        assert!(IntegrationState::load(&state_path).is_installed("claude_code"));
        assert!(cli.calls.borrow().iter().any(|c| c == "install"));

        // Remove: the status reads back installed=false; the cache flag flips too.
        let status = remove(
            "claude_code",
            &target,
            Some(&descriptor),
            Some(&cli),
            &state_path,
        )
        .expect("remove ok");
        assert!(
            !status.installed,
            "post-remove status must read installed=false"
        );
        assert!(!IntegrationState::load(&state_path).is_installed("claude_code"));
        assert!(cli.calls.borrow().iter().any(|c| c == "uninstall"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsupported_provider_is_a_readable_error() {
        let root = temp_dir("unsup");
        let target = OnboardingTarget::new("Claude Code", root.join("claude.json"));
        let state_path = root.join("integrations.json");
        for p in ["codex", "opencode", "custom"] {
            let err = install(p, &target, None, None, &state_path).unwrap_err();
            assert!(err.contains("not supported"), "install({p}) → {err}");
            let err = remove(p, &target, None, None, &state_path).unwrap_err();
            assert!(err.contains("not supported"), "remove({p}) → {err}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn status_list_advertises_four_providers() {
        let list = status_list();
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].provider, "claude_code");
        assert!(list[0].available, "claude_code is available in v1");
        for s in &list[1..] {
            assert!(!s.available, "{} is coming soon", s.provider);
        }
    }
}
