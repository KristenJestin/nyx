//! The v2.4 review COMMENT layer (PRD `01KX0EEP5H1MJW4T3QCZEWPHFK`, Phase 4, task
//! `01KX0F6HEBPK34CV49EREHK3Y9`, PDR-0012 / PDR-0015) — the anchoring, RE-MAPPING and
//! OUTDATED logic that sits above the [`crate::db`] `review_comments` CRUD.
//!
//! ## Why this module exists
//!
//! A review comment is a durable annotation on a diff that is itself a VUE DÉRIVÉE
//! (PDR-0015): the underlying Git blob moves as the branch advances. So an ANCHORED comment
//! cannot store just `(file, line)` — it must RE-ANCHOR itself when a new commit/checkout
//! changes the file. This module carries that policy:
//!
//!   * [`capture_anchor`] snapshots the where-on-the-diff (`file/side/line[/range]`) PLUS the
//!     material re-mapping needs later (the `blob` SHA the anchor was taken on, the anchored
//!     `snippet` line text, and a few lines of surrounding `context`).
//!   * [`remap_repo_comments`] re-anchors every anchored, still-`open` comment of a
//!     `(workspace, repository)` after the repo advances: it relocates the snippet inside the
//!     FRESH blob (via the phase-1 [`crate::git_diff`] blob-by-SHA reader), following the code
//!     when it moved, or flipping the comment to `outdated` when the snippet can no longer be
//!     located reliably.
//!
//! GLOBAL comments (no `file_path`) and REPLIES (which inherit their root's anchor) are left
//! untouched — only an anchored ROOT re-maps. Resolution stays HUMAN-ONLY (the re-mapping
//! never touches `status`), and a `resolved` comment is skipped (its thread is closed).

use diesel::result::QueryResult;
use diesel::SqliteConnection;

use crate::db::{self, AnchorUpdate, CommentAnchor, ReviewComment};

/// How many lines of surrounding CONTEXT (before + after the anchored line) a re-mapping
/// captures/uses to disambiguate an otherwise-ambiguous snippet. Small on purpose: enough to
/// pin a moved line uniquely, not so much that a nearby edit needlessly outdates the anchor.
pub const CONTEXT_RADIUS: usize = 3;

/// Snapshot the anchor of a NEW anchored comment from the file's CURRENT content on the
/// chosen `side`. Records the 1-based `line_start`/`line_end`, the `blob` SHA the anchor was
/// taken on, the anchored line text (`snippet` = the FIRST line of the range), and the
/// `context` (up to [`CONTEXT_RADIUS`] lines on each side of the range). The `content` is the
/// blob's UTF-8 text (the caller reads it from the diff it is annotating). A `line_start`
/// outside the content still stores the anchor (its snippet is empty) so a subsequent
/// re-mapping can flag it — it never panics.
pub fn capture_anchor(
    file_path: &str,
    side: &str,
    line_start: i32,
    line_end: i32,
    blob: Option<&str>,
    content: &str,
) -> CommentAnchor {
    let lines: Vec<&str> = content.lines().collect();
    let start = line_start.max(1);
    let end = line_end.max(start);
    let snippet = line_at(&lines, start);
    let context = context_window(&lines, start, end);
    CommentAnchor {
        file_path: file_path.to_string(),
        side: side.to_string(),
        line_start: start,
        line_end: end,
        blob: blob.map(|b| b.to_string()),
        snippet: Some(snippet),
        context: Some(context),
    }
}

/// RE-MAP every anchored, open comment of a `(workspace, repository)` after the repo at
/// `repo_path` advanced. For each comment:
///   * GLOBAL (no `file_path`), a REPLY (`parent_id` set), a RESOLVED, or an already-OUTDATED
///     comment is skipped (nothing to re-anchor).
///   * else the CURRENT HEAD blob SHA of its `file_path` is looked up; if it equals the stored
///     `blob`, the anchor is unchanged (no write); otherwise the snippet is relocated inside
///     the fresh blob — on success the line numbers + blob + snippet/context are updated (the
///     anchor FOLLOWS the code), on failure the comment flips `outdated = true`.
///
/// Returns the number of comments whose row was WRITTEN (re-anchored or newly outdated), so a
/// caller/test can assert work happened. Persists each change via
/// [`crate::db::update_review_comment_anchor`].
pub fn remap_repo_comments(
    conn: &mut SqliteConnection,
    repo_path: &str,
    workspace_id: &str,
    repository_id: &str,
) -> QueryResult<usize> {
    // Re-anchoring is spec-agnostic — the repo's HEAD moved under EVERY anchored comment it
    // hosts, regardless of which Spec each belongs to — so read the whole repo feed (`None`).
    let comments = db::list_review_comments(conn, workspace_id, repository_id, None)?;
    let mut written = 0usize;
    for comment in comments {
        if let Some(update) = remap_one(repo_path, &comment) {
            db::update_review_comment_anchor(conn, &comment.id, update)?;
            written += 1;
        }
    }
    Ok(written)
}

/// Decide the re-mapping of ONE comment as the [`AnchorUpdate`] to persist, or `None` when the
/// comment needs NO write (global, reply, resolved, already outdated, or its blob is unchanged).
/// Pure except for the two Git blob reads, which go through the phase-1 engine (gix primary,
/// git CLI fallback).
fn remap_one(repo_path: &str, comment: &ReviewComment) -> Option<AnchorUpdate> {
    // Only an anchored ROOT that is still open + not already outdated re-maps.
    if comment.parent_id.is_some() || comment.status != db::COMMENT_STATUS_OPEN || comment.outdated
    {
        return None;
    }
    let file_path = comment.file_path.as_deref()?; // global → skip
    let snippet = comment.snippet.clone().unwrap_or_default();

    // WHERE is this file NOW? Its fresh HEAD blob SHA. A file that vanished from HEAD (deleted /
    // renamed away) can no longer host the anchor → outdated.
    let Some(fresh_blob) = crate::git_diff::head_blob_sha(repo_path, file_path) else {
        return Some(outdated_from(comment));
    };
    // Blob unchanged since capture → the anchor still points at the same content. No write.
    if comment.blob.as_deref() == Some(fresh_blob.as_str()) {
        return None;
    }

    // The blob moved: read the fresh content and try to relocate the snippet.
    let Some(bytes) = crate::git_diff::read_blob_by_sha(repo_path, &fresh_blob) else {
        return Some(outdated_from(comment));
    };
    let content = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = content.lines().collect();
    let span = (comment.line_end.unwrap_or(comment.line_start.unwrap_or(1))
        - comment.line_start.unwrap_or(1))
    .max(0);

    match relocate(
        &lines,
        &snippet,
        comment.context.as_deref(),
        comment.line_start,
    ) {
        Some(new_line) => {
            let new_start = new_line;
            let new_end = new_start + span;
            Some(AnchorUpdate {
                line_start: Some(new_start),
                line_end: Some(new_end),
                blob: Some(fresh_blob),
                // Refresh snippet + context from the fresh content so the NEXT re-map compares
                // against the up-to-date text (the anchor rides forward with the code).
                snippet: Some(line_at(&lines, new_start)),
                context: Some(context_window(&lines, new_start, new_end)),
                outdated: false,
            })
        }
        // Could not relocate reliably (gone, or ambiguous) → outdated, keep last-known anchor.
        None => Some(outdated_from(comment)),
    }
}

/// The OUTDATED [`AnchorUpdate`] that keeps the comment's LAST-known line numbers / snippet /
/// context (so the UI can still show roughly where it was) but flips `outdated = true`.
fn outdated_from(comment: &ReviewComment) -> AnchorUpdate {
    AnchorUpdate {
        line_start: Some(comment.line_start.unwrap_or(1)),
        line_end: Some(
            comment
                .line_end
                .unwrap_or_else(|| comment.line_start.unwrap_or(1)),
        ),
        blob: comment.blob.clone(),
        snippet: comment.snippet.clone(),
        context: comment.context.clone(),
        outdated: true,
    }
}

/// Relocate `snippet` inside `lines`, returning its NEW 1-based line, or `None` when it cannot
/// be pinned reliably. The policy, in order of confidence:
///   1. An empty snippet cannot be relocated → `None` (outdated).
///   2. If the snippet occurs EXACTLY ONCE, that is the new line (unambiguous move).
///   3. If it occurs SEVERAL times, disambiguate with the captured `context`: pick the
///      occurrence whose surrounding lines best match the context; ties or no match near the
///      old position → `None` (outdated) rather than guess wrong.
///   4. Not found at all → `None` (outdated).
fn relocate(
    lines: &[&str],
    snippet: &str,
    context: Option<&str>,
    old_line: Option<i32>,
) -> Option<i32> {
    let needle = snippet.trim_end_matches(['\r', '\n']);
    if needle.is_empty() {
        return None;
    }
    // All 0-based indices whose line equals the snippet.
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_end_matches(['\r', '\n']) == needle)
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [] => None,
        [only] => Some(*only as i32 + 1),
        many => disambiguate(lines, many, context, old_line),
    }
}

/// Pick, among SEVERAL snippet matches, the one whose surrounding lines best match the
/// captured `context`. Scores each candidate by how many context lines it shares in its own
/// window; the winner must be a STRICT best (no tie) AND share at least one context line —
/// otherwise `None` (outdated), never a coin-flip. With no context to compare, falls back to
/// the match CLOSEST to the old line, still requiring a strict winner.
fn disambiguate(
    lines: &[&str],
    matches: &[usize],
    context: Option<&str>,
    old_line: Option<i32>,
) -> Option<i32> {
    let ctx_lines: Vec<String> = context
        .unwrap_or("")
        .lines()
        .map(|l| l.trim_end_matches(['\r', '\n']).to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if !ctx_lines.is_empty() {
        let mut best: Option<(usize, usize)> = None; // (score, index)
        let mut tie = false;
        for &m in matches {
            let score = context_overlap(lines, m, &ctx_lines);
            match best {
                None => best = Some((score, m)),
                Some((bs, _)) if score > bs => {
                    best = Some((score, m));
                    tie = false;
                }
                Some((bs, _)) if score == bs => tie = true,
                _ => {}
            }
        }
        if let Some((score, idx)) = best {
            if score > 0 && !tie {
                return Some(idx as i32 + 1);
            }
        }
        return None;
    }

    // No context: pick the match closest to the old position, but only if it is a strict
    // winner (a unique closest) — otherwise refuse rather than guess.
    let old = old_line.unwrap_or(1);
    let mut best: Option<(i32, usize)> = None; // (distance, index)
    let mut tie = false;
    for &m in matches {
        let dist = (m as i32 + 1 - old).abs();
        match best {
            None => best = Some((dist, m)),
            Some((bd, _)) if dist < bd => {
                best = Some((dist, m));
                tie = false;
            }
            Some((bd, _)) if dist == bd => tie = true,
            _ => {}
        }
    }
    match best {
        Some((_, idx)) if !tie => Some(idx as i32 + 1),
        _ => None,
    }
}

/// How many of `ctx_lines` appear in the [`CONTEXT_RADIUS`] window around index `m`.
fn context_overlap(lines: &[&str], m: usize, ctx_lines: &[String]) -> usize {
    let lo = m.saturating_sub(CONTEXT_RADIUS);
    let hi = (m + CONTEXT_RADIUS + 1).min(lines.len());
    let window: Vec<String> = lines[lo..hi]
        .iter()
        .map(|l| l.trim_end_matches(['\r', '\n']).to_string())
        .collect();
    ctx_lines
        .iter()
        .filter(|c| window.iter().any(|w| w == *c))
        .count()
}

/// The 1-based `line` of `lines`, or `""` when out of range.
fn line_at(lines: &[&str], line: i32) -> String {
    if line < 1 {
        return String::new();
    }
    lines
        .get(line as usize - 1)
        .map(|l| l.trim_end_matches(['\r', '\n']).to_string())
        .unwrap_or_default()
}

/// The CONTEXT window: up to [`CONTEXT_RADIUS`] lines BEFORE `start` and AFTER `end`
/// (1-based, inclusive), joined by newlines — the disambiguation material a re-mapping keeps.
fn context_window(lines: &[&str], start: i32, end: i32) -> String {
    let start = start.max(1) as usize;
    let end = end.max(start as i32) as usize;
    let lo = start.saturating_sub(CONTEXT_RADIUS).max(1);
    let hi = (end + CONTEXT_RADIUS).min(lines.len());
    let mut out = Vec::new();
    for i in lo..=hi {
        if let Some(l) = lines.get(i - 1) {
            out.push(l.trim_end_matches(['\r', '\n']).to_string());
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests;
