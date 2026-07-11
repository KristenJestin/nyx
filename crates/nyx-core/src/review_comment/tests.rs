//! Tests for the v2.4 review COMMENT model + re-mapping (task
//! `01KX0F6HEBPK34CV49EREHK3Y9`).
//!
//! The done_criterion is: "un commentaire ancré (fichier/côté/ligne ou plage +
//! blob/snippet/contexte) ou global est stocké (review_comments auto-threadé) ; il se
//! re-mappe après un nouveau commit ou passe outdated ; résolution réservée à l'humain ;
//! auteur humain ou agent."
//!
//! Each test names the clause it covers. The re-mapping tests drive a REAL git repo (via
//! the same `command("git")` the engine uses) and skip gracefully when git is absent.

use super::*;
use crate::db::{
    self, open_in_memory, NewComment, AUTHOR_AGENT, AUTHOR_HUMAN, COMMENT_STATUS_OPEN,
    COMMENT_STATUS_RESOLVED, SIDE_NEW,
};
use uuid::Uuid;

// --- git scratch helpers (mirror git_diff/tests.rs) ------------------------

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("nyx-rc-{tag}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git_available() -> bool {
    crate::proc_util::command("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_in(dir: &std::path::Path, args: &[&str]) {
    let status = crate::proc_util::command("git")
        .args([
            "-c",
            "user.email=test@nyx",
            "-c",
            "user.name=nyx-test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// Init a repo with one committed `src.rs` and return its dir.
fn init_repo(content: &str) -> std::path::PathBuf {
    let dir = scratch_dir("repo");
    git_in(&dir, &["init", "-q"]);
    git_in(&dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    std::fs::write(dir.join("src.rs"), content).expect("seed");
    git_in(&dir, &["add", "src.rs"]);
    git_in(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

/// The current HEAD blob SHA of `path` in `dir`.
fn head_blob(dir: &std::path::Path, path: &str) -> String {
    crate::git_diff::head_blob_sha(dir.to_str().unwrap(), path).expect("head blob sha")
}

/// Commit a new content for `src.rs`.
fn recommit(dir: &std::path::Path, content: &str) {
    std::fs::write(dir.join("src.rs"), content).expect("rewrite");
    git_in(dir, &["add", "src.rs"]);
    git_in(dir, &["commit", "-q", "-m", "change"]);
}

// --- storage / auto-threading ---------------------------------------------

/// An ANCHORED comment (file/side/line + blob/snippet/context) and a GLOBAL comment are both
/// stored in the single `review_comments` table; a reply auto-threads under its root via
/// `parent_id`. Covers "un commentaire ancré (…) ou global est stocké (review_comments
/// auto-threadé)".
#[test]
fn anchored_and_global_stored_and_auto_threaded() {
    let mut conn = open_in_memory();

    // Anchored root (file / side / line-range + blob + snippet + context).
    let anchor = capture_anchor("src.rs", SIDE_NEW, 2, 3, Some("blob123"), "a\nb\nc\nd\n");
    assert_eq!(anchor.line_start, 2);
    assert_eq!(anchor.line_end, 3);
    assert_eq!(anchor.snippet.as_deref(), Some("b"));
    assert!(anchor.context.is_some(), "context captured for re-mapping");

    let root = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            author_type: AUTHOR_HUMAN.into(),
            author_name: "Kris".into(),
            anchor: Some(anchor),
            body: "look here".into(),
            ..Default::default()
        },
    )
    .expect("insert anchored root");
    assert!(root.parent_id.is_none(), "a root has no parent");
    assert_eq!(root.file_path.as_deref(), Some("src.rs"));
    assert_eq!(root.side.as_deref(), Some(SIDE_NEW));
    assert!(root.blob.is_some() && root.snippet.is_some() && root.context.is_some());
    assert!(!root.outdated && root.status == COMMENT_STATUS_OPEN);

    // A reply auto-threads under the root (parent_id set) — no separate thread/message table.
    let reply = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            parent_id: Some(root.id.clone()),
            author_type: AUTHOR_HUMAN.into(),
            author_name: "Kris".into(),
            body: "agreed".into(),
            ..Default::default()
        },
    )
    .expect("insert reply");
    assert_eq!(reply.parent_id.as_deref(), Some(root.id.as_str()));

    // A GLOBAL comment (no anchor).
    let global = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            author_type: AUTHOR_HUMAN.into(),
            author_name: "Kris".into(),
            body: "overall LGTM".into(),
            ..Default::default()
        },
    )
    .expect("insert global");
    assert!(global.file_path.is_none(), "a global comment has no anchor");

    // Listed as one auto-threaded feed, oldest-first, roots + reply + global together.
    let all = db::list_review_comments(&mut conn, "ws", "repo", None).expect("list");
    assert_eq!(all.len(), 3);
    let roots: Vec<_> = all.iter().filter(|c| c.parent_id.is_none()).collect();
    let replies: Vec<_> = all.iter().filter(|c| c.parent_id.is_some()).collect();
    assert_eq!(roots.len(), 2, "the anchored root + the global root");
    assert_eq!(replies.len(), 1, "the one reply threads under its root");
}

/// A comment's author may be `human` OR `agent` — both persist to the same table. Covers
/// "auteur humain ou agent".
#[test]
fn author_human_or_agent() {
    let mut conn = open_in_memory();
    let human = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            author_type: AUTHOR_HUMAN.into(),
            author_name: "Kris".into(),
            body: "human note".into(),
            ..Default::default()
        },
    )
    .expect("human comment");
    let agent = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            parent_id: Some(human.id.clone()),
            author_type: AUTHOR_AGENT.into(),
            author_name: "nyx-agent".into(),
            body: "agent reply via MCP".into(),
            ..Default::default()
        },
    )
    .expect("agent reply");
    assert_eq!(human.author_type, AUTHOR_HUMAN);
    assert_eq!(agent.author_type, AUTHOR_AGENT);
    assert_eq!(agent.parent_id.as_deref(), Some(human.id.as_str()));
}

// --- resolution reserved to the human -------------------------------------

/// Resolving stamps `resolved_by_type = human`; re-opening clears it. The resolve/reopen
/// write path is only reachable from the human façade (the MCP agent tool set has no
/// resolve) — the DB layer always records the human. Covers "résolution réservée à
/// l'humain".
#[test]
fn resolution_is_human_only() {
    let mut conn = open_in_memory();
    let c = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            author_type: AUTHOR_AGENT.into(),
            author_name: "nyx-agent".into(),
            body: "agent-authored, human resolves".into(),
            ..Default::default()
        },
    )
    .expect("insert");
    assert_eq!(c.status, COMMENT_STATUS_OPEN);
    assert!(c.resolved_by_type.is_none());

    let resolved = db::resolve_review_comment(&mut conn, &c.id)
        .expect("resolve")
        .expect("row present");
    assert_eq!(resolved.status, COMMENT_STATUS_RESOLVED);
    assert_eq!(
        resolved.resolved_by_type.as_deref(),
        Some(AUTHOR_HUMAN),
        "only the human resolves; the DB always records the human"
    );

    let reopened = db::reopen_review_comment(&mut conn, &c.id)
        .expect("reopen")
        .expect("row present");
    assert_eq!(reopened.status, COMMENT_STATUS_OPEN);
    assert!(reopened.resolved_by_type.is_none());
}

// --- re-mapping across commits + outdated ---------------------------------

/// After a new commit shifts the anchored line down (lines inserted above it), the re-mapping
/// RELOCATES the anchor: its `line_start` follows the code and `blob` refreshes, `outdated`
/// stays false. Covers "il se re-mappe après un nouveau commit".
#[test]
fn remap_follows_the_code_after_a_commit() {
    if !git_available() {
        eprintln!("skipping remap test: git unavailable");
        return;
    }
    let mut conn = open_in_memory();
    // Original file: the anchored line "let target = compute();" is at line 3.
    let dir = init_repo("fn main() {\n    setup();\n    let target = compute();\n    done();\n}\n");
    let repo = dir.to_str().unwrap();
    let blob0 = head_blob(&dir, "src.rs");

    let anchor = capture_anchor(
        "src.rs",
        SIDE_NEW,
        3,
        3,
        Some(&blob0),
        &String::from_utf8(std::fs::read(dir.join("src.rs")).unwrap()).unwrap(),
    );
    assert_eq!(
        anchor.snippet.as_deref(),
        Some("    let target = compute();")
    );
    let c = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            author_type: AUTHOR_HUMAN.into(),
            author_name: "Kris".into(),
            anchor: Some(anchor),
            body: "why compute here?".into(),
            ..Default::default()
        },
    )
    .expect("insert anchored");
    assert_eq!(c.line_start, Some(3));

    // New commit: insert TWO lines above the anchored line → it moves from 3 to 5.
    recommit(
        &dir,
        "fn main() {\n    setup();\n    extra_a();\n    extra_b();\n    let target = compute();\n    done();\n}\n",
    );
    let blob1 = head_blob(&dir, "src.rs");
    assert_ne!(blob0, blob1, "the blob changed");

    let written = remap_repo_comments(&mut conn, repo, "ws", "repo").expect("remap");
    assert_eq!(written, 1, "the one anchored comment was re-anchored");

    let after = db::get_review_comment(&mut conn, &c.id)
        .expect("get")
        .expect("present");
    assert!(!after.outdated, "the anchor was relocated, not outdated");
    assert_eq!(
        after.line_start,
        Some(5),
        "the anchor followed the code down to its new line"
    );
    assert_eq!(
        after.blob.as_deref(),
        Some(blob1.as_str()),
        "blob refreshed"
    );
    assert_eq!(
        after.snippet.as_deref(),
        Some("    let target = compute();")
    );
}

/// When the anchored line is DELETED by a new commit and cannot be relocated, the comment
/// flips `outdated = true` (it keeps its last-known line for display). Covers "… ou passe
/// outdated".
#[test]
fn remap_flips_outdated_when_snippet_gone() {
    if !git_available() {
        eprintln!("skipping outdated test: git unavailable");
        return;
    }
    let mut conn = open_in_memory();
    let dir = init_repo("fn main() {\n    setup();\n    let target = compute();\n    done();\n}\n");
    let repo = dir.to_str().unwrap();
    let blob0 = head_blob(&dir, "src.rs");
    let anchor = capture_anchor(
        "src.rs",
        SIDE_NEW,
        3,
        3,
        Some(&blob0),
        &String::from_utf8(std::fs::read(dir.join("src.rs")).unwrap()).unwrap(),
    );
    let c = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            author_type: AUTHOR_HUMAN.into(),
            author_name: "Kris".into(),
            anchor: Some(anchor),
            body: "why compute here?".into(),
            ..Default::default()
        },
    )
    .expect("insert");

    // New commit: DELETE the anchored line entirely (the snippet no longer exists).
    recommit(&dir, "fn main() {\n    setup();\n    done();\n}\n");

    let written = remap_repo_comments(&mut conn, repo, "ws", "repo").expect("remap");
    assert_eq!(written, 1);
    let after = db::get_review_comment(&mut conn, &c.id)
        .expect("get")
        .expect("present");
    assert!(after.outdated, "the snippet is gone → outdated");
}

/// A GLOBAL comment (no anchor) is never touched by the re-mapping (no blob to follow, never
/// outdated). Guards the "global" clause against the re-mapping path.
#[test]
fn remap_leaves_global_comments_untouched() {
    if !git_available() {
        eprintln!("skipping global-remap test: git unavailable");
        return;
    }
    let mut conn = open_in_memory();
    let dir = init_repo("a\nb\n");
    let repo = dir.to_str().unwrap();
    let g = db::insert_review_comment(
        &mut conn,
        NewComment {
            workspace_id: "ws".into(),
            repository_id: "repo".into(),
            author_type: AUTHOR_HUMAN.into(),
            author_name: "Kris".into(),
            body: "overall".into(),
            ..Default::default()
        },
    )
    .expect("insert global");
    recommit(&dir, "a\nx\nb\n");
    let written = remap_repo_comments(&mut conn, repo, "ws", "repo").expect("remap");
    assert_eq!(written, 0, "the global comment is not re-anchored");
    let after = db::get_review_comment(&mut conn, &g.id)
        .expect("get")
        .expect("present");
    assert!(!after.outdated);
}
