use std::process::Command;

/// Run `git <args>` and return its TRIMMED stdout on success, or `None` when git is
/// unavailable, exits non-zero, or emits non-UTF-8. One shared best-effort idiom for
/// every git probe below, so the SHA, dirty bit, and rerun paths degrade identically.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
}

fn main() {
    // Emit the short git SHA at build time so the `probe` tool can report it.
    // Best-effort: if git is not available (detached CI, no .git), emit "unknown".
    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=NYX_BUILD_SHA={sha}");

    // PRD-4.1 task #6: bake whether the working tree was DIRTY at build time so the
    // `probe` tool can report `build_dirty` WITHOUT corrupting `build_sha` (no `-dirty`
    // suffix — the sha stays a clean, parseable value). `git status --porcelain` prints
    // one line per modified or untracked-but-not-ignored path across the repo; a
    // NON-EMPTY listing ⇒ the working tree carried uncommitted changes at build time
    // (this is repo-wide, matching `build_sha`, which is the repo HEAD). It reports
    // `true` ONLY on positive evidence from a SUCCESSFUL `git status`; if git is
    // unavailable OR errors (the same conditions that make `build_sha` "unknown" — e.g.
    // no .git, a locked index, or a refusal like "dubious ownership"), it degrades to
    // `false` rather than fabricating a dirty bit, since an "unknown" sha already flags
    // the build as non-reproducible. So `build_dirty` is only trustworthy when
    // `build_sha` is a real sha.
    let dirty = git(&["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false);
    println!("cargo:rustc-env=NYX_BUILD_DIRTY={dirty}");

    // Re-run when HEAD / the index / the reflog change (new commit, checkout, stage).
    // CRUCIAL in a git WORKTREE: there `.git` is a pointer FILE and the real per-worktree
    // git dir lives under `<main>/.git/worktrees/<name>/`, so the naive `../.git/HEAD`
    // path does NOT exist and the rerun would never fire (build_sha/build_dirty would
    // silently go stale). `git rev-parse --absolute-git-dir` resolves the true on-disk
    // git dir for a worktree, a submodule, or a plain repo alike. Best-effort: if git is
    // unavailable we fall back to the legacy relative path.
    match git(&["rev-parse", "--absolute-git-dir"]) {
        Some(git_dir) => {
            for file in ["HEAD", "index", "logs/HEAD"] {
                println!("cargo:rerun-if-changed={git_dir}/{file}");
            }
        }
        None => println!("cargo:rerun-if-changed=../.git/HEAD"),
    }
    tauri_build::build()
}
