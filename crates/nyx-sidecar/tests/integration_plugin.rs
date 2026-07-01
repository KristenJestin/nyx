//! EASG1 integration test: the Claude plugin (MCP register + hooks) installed via the
//! Settings -> Integrations seam, over the REAL `nyx-sidecar` binary + the framed pipe.
//!
//! The renderer's Settings dialog invokes `integration_install` / `integration_remove` /
//! `integration_list` through the bridge; here we drive the SAME `db_command` requests over
//! the pipe against the real sidecar and assert the install REALLY:
//!   * copies the bundled plugin (MCP `.mcp.json` + the SessionStart/UserPromptSubmit hooks)
//!     into the stable app-data dir, and
//!   * drives the `claude plugin` CLI (a FAKE `claude` here, so no real `~/.claude` is
//!     touched) so the plugin is registered + enabled, the status read back from the real
//!     (temp) settings flag.
//!
//! All seams are injected via env so the test touches NOTHING on the real machine:
//!   * `NYX_CLAUDE_PLUGIN_DIR` -> the bundled plugin SOURCE (the source tree),
//!   * `NYX_CLAUDE_STABLE_PLUGIN_DIR` -> a temp INSTALL dir,
//!   * `NYX_CLAUDE_BIN` -> a fake `claude` shell script that emulates the handful of
//!     `claude plugin` subcommands by flipping the `enabledPlugins` flag in the temp
//!     settings file,
//!   * `NYX_CLAUDE_SETTINGS` -> the temp settings file the status is read back from.
//!
//! Gated `#[cfg(unix)]` (the fake CLI is a `/bin/sh` script).
#![cfg(unix)]

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use nyx_sidecar::pipe::codec::{encode_frame, FrameDecoder};
use nyx_sidecar::pipe::wire::{
    decode_message, encode_message, Event, Message, Request, Response, PROTOCOL_VERSION,
};

fn sidecar_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_nyx-sidecar"))
}

/// The bundled plugin SOURCE (the source tree next to nyx-core), so the install copies a real
/// plugin (the `.mcp.json` + the SessionStart/UserPromptSubmit hooks).
fn bundled_plugin_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("nyx-core")
        .join("resources")
        .join("claude-plugin")
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nyx-sidecar-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a fake `claude` shell script that emulates the `claude plugin …` subcommands the
/// install/uninstall drive, by flipping the `enabledPlugins` flag in `$NYX_CLAUDE_SETTINGS`
/// (the exact effect the real CLI has — so the status read back is honest). Returns the path.
fn write_fake_claude(dir: &std::path::Path, settings: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("fake-claude.sh");
    // The install id is `<plugin>@<marketplace>` = `nyx-v2-claude-integration@nyx-v2` (v2-
    // namespaced so it coexists with v1's `nyx-claude-integration@nyx`). `marketplace list
    // --json` -> `[]`; `install` -> set the flag true; `uninstall` -> false; everything else
    // -> a no-op success.
    let body = format!(
        r#"#!/bin/sh
SETTINGS="{settings}"
INSTALL_ID="nyx-v2-claude-integration@nyx-v2"
set_enabled() {{
  node -e '
    const fs=require("fs"); const p=process.env.S; const v=process.argv[1]==="1";
    let root={{}}; try{{root=JSON.parse(fs.readFileSync(p,"utf8"))}}catch(e){{}}
    root.enabledPlugins=root.enabledPlugins||{{}};
    root.enabledPlugins["'"$INSTALL_ID"'"]=v;
    fs.mkdirSync(require("path").dirname(p),{{recursive:true}});
    fs.writeFileSync(p,JSON.stringify(root));
  ' "$1" 2>/dev/null || S="$SETTINGS" node -e 'const fs=require("fs");const p=process.env.S;let r={{}};try{{r=JSON.parse(fs.readFileSync(p,"utf8"))}}catch(e){{}};r.enabledPlugins=r.enabledPlugins||{{}};r.enabledPlugins[process.env.IID]=(process.argv[1]==="1");fs.mkdirSync(require("path").dirname(p),{{recursive:true}});fs.writeFileSync(p,JSON.stringify(r));' "$1"
}}
export S="$SETTINGS"
export IID="$INSTALL_ID"
sub="$2"
case "$1" in
  plugin)
    case "$sub" in
      marketplace)
        if [ "$3" = "list" ]; then printf '[]'; fi
        exit 0 ;;
      install) set_enabled 1; exit 0 ;;
      uninstall) set_enabled 0; exit 0 ;;
      *) exit 0 ;;
    esac ;;
  *) exit 0 ;;
esac
"#,
        settings = settings.display(),
    );
    std::fs::write(&script, body).unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

struct Harness {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Message>,
    next_id: u64,
    data_dir: std::path::PathBuf,
    seam_dir: std::path::PathBuf,
}

impl Harness {
    fn spawn() -> Self {
        let data_dir = temp_dir("intplugin-data");
        let seam_dir = temp_dir("intplugin-seam");
        let install_dir = seam_dir.join("stable-plugin");
        let settings = seam_dir.join("claude-settings.json");
        let fake_claude = write_fake_claude(&seam_dir, &settings);

        let mut child = Command::new(sidecar_bin())
            .env("NYX_DATA_DIR", &data_dir)
            .env("SHELL", "/bin/sh")
            // Plugin SOURCE + INSTALL dir + CLI + settings seams (no real ~/.claude touched).
            .env("NYX_CLAUDE_PLUGIN_DIR", bundled_plugin_dir())
            .env("NYX_CLAUDE_STABLE_PLUGIN_DIR", &install_dir)
            .env("NYX_CLAUDE_BIN", &fake_claude)
            .env("NYX_CLAUDE_SETTINGS", &settings)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn nyx-sidecar");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("intplugin-reader".into())
            .spawn(move || {
                let mut dec = FrameDecoder::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    loop {
                        match dec.next_frame() {
                            Ok(Some(p)) => {
                                if let Ok(m) = decode_message(&p) {
                                    if tx.send(m).is_err() {
                                        return;
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(_) => return,
                        }
                    }
                    match stdout.read(&mut buf) {
                        Ok(0) => return,
                        Ok(n) => dec.push(&buf[..n]),
                        Err(_) => return,
                    }
                }
            })
            .unwrap();
        let mut h = Harness {
            child,
            stdin,
            rx,
            next_id: 100,
            data_dir,
            seam_dir,
        };
        match h.recv(Duration::from_secs(5)) {
            Some(Message::Event(Event::Log { .. })) => {}
            other => panic!("expected ready event, got {other:?}"),
        }
        h.handshake();
        h
    }

    fn send(&mut self, req: Request) {
        let frame = encode_frame(&encode_message(&Message::Request(req))).unwrap();
        self.stdin.write_all(&frame).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn recv_until<T>(&mut self, deadline: Duration, mut f: impl FnMut(&Message) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            let remaining = match deadline.checked_sub(start.elapsed()) {
                Some(r) => r,
                None => panic!("timed out"),
            };
            match self.recv(remaining) {
                Some(m) => {
                    if let Some(v) = f(&m) {
                        return v;
                    }
                }
                None => panic!("timed out waiting for a matching message"),
            }
        }
    }

    fn handshake(&mut self) {
        self.send(Request::Handshake {
            id: 1,
            protocol_version: PROTOCOL_VERSION,
        });
        self.recv_until(Duration::from_secs(5), |m| {
            matches!(m, Message::Response(Response::HandshakeOk { .. })).then_some(())
        });
    }

    fn db_command(
        &mut self,
        command: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(Request::DbCommand {
            id,
            command: command.to_string(),
            args_json: args.to_string(),
        });
        self.recv_until(Duration::from_secs(10), |m| match m {
            Message::Response(Response::DbResult {
                id: rid,
                result_json,
            }) if *rid == id => Some(Ok(serde_json::from_str(result_json).unwrap())),
            Message::Response(Response::Error {
                id: rid, message, ..
            }) if *rid == id => Some(Err(message.clone())),
            _ => None,
        })
    }

    fn shutdown(mut self) {
        self.send(Request::Shutdown { id: 9999 });
        self.recv_until(Duration::from_secs(5), |m| {
            matches!(m, Message::Response(Response::ShutdownAck { .. })).then_some(())
        });
        let start = Instant::now();
        loop {
            if self.child.try_wait().expect("try_wait").is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                let _ = self.child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(&self.data_dir).ok();
        std::fs::remove_dir_all(&self.seam_dir).ok();
    }
}

/// Settings -> Integrations installs the plugin (MCP + hooks) for Claude Code, then the
/// status reads back installed, then uninstall reverses it — all over the SAME `db_command`
/// path the renderer's Settings dialog drives.
#[test]
fn settings_integrations_installs_and_removes_the_claude_plugin() {
    let mut h = Harness::spawn();

    // The 4-provider list (the Settings dialog rows), claude_code available.
    let list = h
        .db_command("integration_list", serde_json::json!({}))
        .unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 4, "the 4-provider list (got {arr:?})");
    let claude = arr
        .iter()
        .find(|r| r.get("provider").and_then(|p| p.as_str()) == Some("claude_code"))
        .expect("claude_code row present");
    assert_eq!(
        claude.get("available").and_then(|v| v.as_bool()),
        Some(true),
        "claude_code is available"
    );

    // INSTALL: copies the bundled plugin (MCP + hooks) into the stable dir AND drives the CLI
    // to register + enable it; the status is read back from the real (temp) settings flag.
    let installed = h
        .db_command(
            "integration_install",
            serde_json::json!({ "provider": "claude_code" }),
        )
        .expect("integration_install succeeds over the pipe");
    assert_eq!(
        installed.get("provider").and_then(|v| v.as_str()),
        Some("claude_code")
    );
    assert_eq!(
        installed.get("installed").and_then(|v| v.as_bool()),
        Some(true),
        "post-install status reads installed=true (got {installed:?})"
    );

    // The bundled plugin was COPIED into the stable install dir: the MCP descriptor + the
    // session hooks are present (the MCP-register + hooks artifacts the plugin ships).
    let install_dir = h.seam_dir.join("stable-plugin");
    assert!(
        install_dir.join(".mcp.json").exists(),
        "the MCP descriptor was copied into the install dir"
    );
    assert!(
        install_dir.join("hooks").join("hooks.json").exists(),
        "the SessionStart/UserPromptSubmit hook config was copied into the install dir"
    );
    assert!(
        install_dir
            .join("hooks")
            .join("nyx-session-hook.cjs")
            .exists(),
        "the session-capture hook script was copied into the install dir"
    );

    // REMOVE: reverses the install (the status flips back to installed=false).
    let removed = h
        .db_command(
            "integration_remove",
            serde_json::json!({ "provider": "claude_code" }),
        )
        .expect("integration_remove succeeds over the pipe");
    assert_eq!(
        removed.get("installed").and_then(|v| v.as_bool()),
        Some(false),
        "post-remove status reads installed=false (got {removed:?})"
    );

    // An UNSUPPORTED provider surfaces the core's "not supported" error THROUGH the pipe (not
    // the allowlist fallthrough), proving the routing reached nyx-core.
    let err = h
        .db_command(
            "integration_install",
            serde_json::json!({ "provider": "codex" }),
        )
        .unwrap_err();
    assert!(
        err.to_lowercase().contains("not supported"),
        "unsupported provider yields the core's 'not supported' error, got {err}"
    );
    assert!(
        !err.contains("not available over"),
        "integration_* must route to the integrations core, not the allowlist fallthrough"
    );

    h.shutdown();
}
