//! The IDEAS **search + scoring** layer (v2.6 — Idées entretenues, Phase 1, task
//! `01KXZ9K207NGKHS45DN1Y8ZAPJ`) — the maintenance-grade surfacing that sits ABOVE the
//! [`crate::db`] `ideas` CRUD + FTS5 index.
//!
//! ## Why this module exists
//!
//! Depot's idées entity drowned in a flat, unbounded list nobody re-read. v2.6's answer is
//! `idea_search`: instead of dumping the backlog, the agent SURFACES the relevant slice —
//! a top-N SCORED over two signals combined:
//!
//!   * **FTS5** ([`crate::db::fts_match_ideas`], `bm25` relevance) — precise word / prefix
//!     matching on (title, body). Fast and exact, but blind to typos and spelling variants.
//!   * **a fuzzy trigram pass** ([`fuzzy_score`]) — a dependency-free Dice coefficient over
//!     character trigrams, run in Rust over a BOUNDED candidate set, so a typo or a variant
//!     ("authentification" vs "authentication", "athentication") still surfaces its idea.
//!
//! The two are normalized and blended ([`combine`]) into one score; each result carries a
//! title, an EXTRACT (a window around the match), the score, and the idea's LINKED Specs.
//! Everything here is either pure (unit-testable without a DB) or a thin orchestration over
//! the `db` authority — this module never re-implements a CRUD rule.

use diesel::result::QueryResult;
use diesel::SqliteConnection;

use crate::db::{self, Idea, LinkedSpec};

/// Default number of scored results `idea_search` returns when the caller gives no `limit`.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;
/// Hard ceiling on a single search's top-N, so a search can never re-create the unbounded
/// dump Depot drowned in (the caller's `limit` is clamped to this).
pub const MAX_SEARCH_LIMIT: usize = 50;
/// How many project ideas the fuzzy pass scans at most (the bounded candidate set). Generous
/// for a real backlog (dozens–hundreds) while keeping the pass O(bounded); FTS hits BEYOND
/// this cap are still folded in explicitly, so a match is never lost to the bound.
pub const FUZZY_CANDIDATE_CAP: i64 = 1_000;
/// The FTS weight in the blended score (fuzzy gets the rest). FTS is the stronger signal
/// (exact/prefix word match), the fuzzy pass the typo-tolerance safety net.
const FTS_WEIGHT: f64 = 0.6;
const FUZZY_WEIGHT: f64 = 1.0 - FTS_WEIGHT;
/// Minimum fuzzy similarity for a NON-FTS-matched idea to still be surfaced. Below this a
/// fuzzy hit is noise (prefix cases are already caught by the FTS prefix query, so this only
/// needs to admit genuine typos/variants of similar-length words). Tuned so a one-letter typo
/// clears it while unrelated words do not.
const FUZZY_ADMIT_THRESHOLD: f64 = 0.3;
/// Target length of a result EXTRACT (characters), and the radius of the window centered on
/// the first matched term.
const EXTRACT_LEN: usize = 200;

/// One scored `idea_search` hit: the idea's identity + a match EXTRACT + the blended `score`,
/// plus its LINKED Specs (each with kind + the Spec's title/status/number). `Serialize` for
/// the MCP boundary.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IdeaSearchResult {
    pub id: String,
    pub title: String,
    /// A short window of the idea's text around the first matched term (or a body prefix).
    pub extract: String,
    /// The blended FTS+fuzzy relevance, higher = better (rounded to 4 decimals).
    pub score: f64,
    pub status: String,
    pub theme: Option<String>,
    /// The Specs this idea is linked to (resolution / context).
    pub linked_specs: Vec<LinkedSpec>,
}

/// Lowercase alphanumeric WORD tokens of `s` (the shared tokenizer for the FTS query build,
/// the fuzzy term comparison, and the extract term search). Splits on every non-alphanumeric
/// char (Unicode-aware), drops empties. Deterministic, pure.
pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Build a SAFE FTS5 MATCH expression from a raw user `query`, or `None` when the query has
/// no usable term (empty / punctuation-only) — the caller then skips FTS and relies on the
/// fuzzy pass. Each token becomes a quoted prefix term (`"tok"*`) — quoting makes it a string
/// literal (so FTS5 syntax chars in the user text can never break the parse) and the trailing
/// `*` gives prefix matching. Tokens are OR-joined (match ANY term). Pure.
pub fn build_fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = tokenize(query)
        .into_iter()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

/// The character-trigram SET of `s` (lowercased, padded so short words still yield trigrams).
/// A trigram is 3 consecutive chars of `  s ` (two leading spaces + trailing space), the
/// standard padding that lets a 1–2 char word compare meaningfully. Pure helper for
/// [`trigram_similarity`].
fn trigrams(s: &str) -> std::collections::HashSet<[char; 3]> {
    let padded: Vec<char> = [' ', ' ']
        .into_iter()
        .chain(s.to_lowercase().chars())
        .chain(std::iter::once(' '))
        .collect();
    let mut set = std::collections::HashSet::new();
    for w in padded.windows(3) {
        set.insert([w[0], w[1], w[2]]);
    }
    set
}

/// The trigram DICE coefficient of two strings — `2·|A∩B| / (|A|+|B|)` over their trigram
/// sets — in `[0, 1]` (1 = identical trigrams). The typo-tolerance metric: a one-letter
/// change to a word keeps most trigrams, so the similarity stays high. Pure.
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    2.0 * inter / (ta.len() + tb.len()) as f64
}

/// The FUZZY similarity of a query to an idea, in `[0, 1]`: for each query term, the BEST
/// trigram similarity against any of the idea's terms, averaged over the query terms. Term-
/// granular on purpose — a typo'd query word matches its intended idea word regardless of how
/// long the body is (a whole-body trigram compare would drown a single word). Pure.
pub fn fuzzy_score(query_terms: &[String], idea_terms: &[String]) -> f64 {
    if query_terms.is_empty() || idea_terms.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    for qt in query_terms {
        let best = idea_terms
            .iter()
            .map(|it| trigram_similarity(qt, it))
            .fold(0.0_f64, f64::max);
        total += best;
    }
    total / query_terms.len() as f64
}

/// Blend an FTS component (`fts_norm`, already normalized to `[0,1]`, `0` = no FTS hit) with a
/// fuzzy component into the final relevance. Pure.
fn combine(fts_norm: f64, fuzzy: f64) -> f64 {
    FTS_WEIGHT * fts_norm + FUZZY_WEIGHT * fuzzy
}

/// A short EXTRACT of an idea's text for a result: a window centered on the FIRST occurrence
/// of any `query_term` in the body (else the title), or the body's head when nothing matches.
/// Trimmed, with `…` elisions marking a cut. Pure.
pub fn make_extract(title: &str, body: &str, query_terms: &[String]) -> String {
    let hay = if !body.trim().is_empty() { body } else { title };
    let hay_lower = hay.to_lowercase();
    // Earliest byte position of any query term in the haystack.
    let hit = query_terms
        .iter()
        .filter_map(|t| hay_lower.find(t.as_str()))
        .min();
    let chars: Vec<char> = hay.chars().collect();
    if chars.len() <= EXTRACT_LEN {
        return hay.trim().to_string();
    }
    // Convert the byte hit to a char index (byte_pos indexes the lowercased copy — slice THAT
    // to stay on a valid char boundary; the index into the original `chars` is approximate,
    // which is fine for a cosmetic extract).
    let center = match hit {
        Some(byte_pos) => hay_lower[..byte_pos].chars().count().min(chars.len()),
        None => 0,
    };
    let radius = EXTRACT_LEN / 2;
    let start = center.saturating_sub(radius);
    let end = (start + EXTRACT_LEN).min(chars.len());
    let start = end.saturating_sub(EXTRACT_LEN); // re-anchor if we hit the tail
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(chars[start..end].iter().collect::<String>().trim());
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// SEARCH the ideas of one project — or of EVERY project — for `query`, returning a top-N SCORED
/// over FTS5 + a fuzzy trigram pass (best first). `theme` / `status` NARROW the candidate set (a
/// `None` filter is "any"); `limit` is clamped to `[1, MAX_SEARCH_LIMIT]` (default
/// [`DEFAULT_SEARCH_LIMIT`]). Each result carries the idea's title, a match extract, the blended
/// score, and its linked Specs. An empty/termless query degrades to the most-recent matching
/// ideas (score 0).
///
/// `project_id` is the SCOPE (`None` = every project, the same spelling [`db::list_ideas`] takes):
/// a search that stayed inside the entered project while the page listed all of them would be a
/// silent trap — the query spans exactly what the list spans.
///
/// Orchestration only: reads through the `db` authority (`fts_match_ideas` + a bounded
/// `list_ideas` candidate set, unioned so an FTS hit past the cap is never lost), then scores
/// with the pure helpers above.
pub fn search_ideas(
    conn: &mut SqliteConnection,
    project_id: Option<&str>,
    query: &str,
    theme: Option<&str>,
    status: Option<&str>,
    limit: usize,
) -> QueryResult<Vec<IdeaSearchResult>> {
    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
    let query_terms = tokenize(query);

    // The bounded fuzzy candidate set (filtered by the theme/status the caller asked for). A
    // search `status` narrows to exactly that status (`None` = every status — search spans the
    // whole backlog, terminal ideas included, so a resolved idea is still findable).
    let status_filter = match status {
        Some(s) => db::IdeaStatusFilter::One(s),
        None => db::IdeaStatusFilter::All,
    };
    let mut candidates: Vec<Idea> = db::list_ideas(
        conn,
        project_id,
        status_filter,
        theme,
        FUZZY_CANDIDATE_CAP,
        0,
    )?;
    let mut seen: std::collections::HashSet<String> =
        candidates.iter().map(|i| i.id.clone()).collect();

    // FTS pass (only when the query has a usable term). Map idea_id → bm25.
    let mut fts_scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    if let Some(fts_query) = build_fts_query(query) {
        let hits = db::fts_match_ideas(conn, project_id, &fts_query, MAX_SEARCH_LIMIT as i64)?;
        for hit in hits {
            fts_scores.insert(hit.id.clone(), hit.score);
            // Fold in an FTS hit that the bounded/filter candidate scan missed — but respect
            // the caller's theme/status filter (an FTS hit of another theme is not wanted).
            if !seen.contains(&hit.id) {
                if let Some(idea) = db::get_idea(conn, &hit.id)? {
                    let theme_ok = match theme {
                        Some(t) => idea.theme.as_deref() == Some(t),
                        None => true,
                    };
                    let status_ok = match status {
                        Some(s) => idea.status == s,
                        None => true,
                    };
                    if theme_ok && status_ok {
                        seen.insert(idea.id.clone());
                        candidates.push(idea);
                    }
                }
            }
        }
    }

    // Normalize bm25 across the FTS hits to [0,1] (best match = 1). bm25 is smaller-is-better,
    // so the most-negative is best. A single hit (or a zero spread) normalizes to 1.
    let (fts_best, fts_worst) = fts_scores
        .values()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
            (lo.min(v), hi.max(v))
        });
    let fts_norm = |id: &str| -> f64 {
        match fts_scores.get(id) {
            None => 0.0,
            Some(&v) => {
                let spread = fts_worst - fts_best;
                if spread <= f64::EPSILON {
                    1.0
                } else {
                    (fts_worst - v) / spread
                }
            }
        }
    };

    // Score every candidate; keep the FTS hits + the fuzzy matches above the admit threshold.
    let mut scored: Vec<(f64, Idea)> = Vec::new();
    for idea in candidates {
        let is_fts_hit = fts_scores.contains_key(&idea.id);
        let idea_terms = {
            let mut t = tokenize(&idea.title);
            t.extend(tokenize(&idea.body));
            t
        };
        let fuzzy = fuzzy_score(&query_terms, &idea_terms);
        // With no query terms (empty search) surface recent candidates as score-0 hits.
        let keep = query_terms.is_empty() || is_fts_hit || fuzzy >= FUZZY_ADMIT_THRESHOLD;
        if !keep {
            continue;
        }
        let score = combine(fts_norm(&idea.id), fuzzy);
        scored.push((score, idea));
    }

    // Best first; ties broken by recency (candidates already came newest-first, so a stable
    // sort by score desc preserves that for equal scores).
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    let mut results = Vec::with_capacity(scored.len());
    for (score, idea) in scored {
        let linked_specs = db::linked_specs_for_idea(conn, &idea.id)?;
        results.push(IdeaSearchResult {
            id: idea.id,
            title: idea.title.clone(),
            extract: make_extract(&idea.title, &idea.body, &query_terms),
            score: (score * 10_000.0).round() / 10_000.0,
            status: idea.status,
            theme: idea.theme,
            linked_specs,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests;
