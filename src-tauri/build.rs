fn main() {
    // Emit the short git SHA at build time so the `probe` tool can report it.
    // Best-effort: if git is not available (detached CI, no .git), emit "unknown".
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() { Some(o.stdout) } else { None })
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=NYX_BUILD_SHA={sha}");
    // Re-run whenever HEAD changes (new commit or checkout).
    println!("cargo:rerun-if-changed=../.git/HEAD");
    tauri_build::build()
}
