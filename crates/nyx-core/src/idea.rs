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
//!
//! ## This module is ALSO the crate's scoring core
//!
//! Since the global `Mod+K` palette, `idea.rs` carries a SECOND role that its name does not
//! advertise: the scoring built here for ideas is the scoring [`crate::search`] runs on for all
//! SEVEN entity kinds. That mutualisation is deliberate — the same idea must not rank differently
//! depending on which surface asked — but it means the reader of this file has to know what a
//! local edit reaches.
//!
//! Shared with [`crate::search`], and load-bearing for it:
//!
//!   * [`tokenize`] / [`build_fts_query`] — the query surface (also read by [`crate::db`]);
//!   * [`fuzzy_score_in_context`] / [`CONTEXT_WEIGHT`] — the per-term aggregation. `CONTEXT_WEIGHT`
//!     has NO consumer here at all: the per-project `idea_search` passes no context;
//!   * [`CERTAINTY_CEILING`] / [`FUZZY_ADMIT_THRESHOLD`] — what one term can be worth, and the bar
//!     a candidate with no FTS hit has to clear;
//!   * [`combine`] / [`Bm25Norm`] — the blend, and the per-source bm25 rescaling it consumes.
//!
//! **Touching any of them moves THREE surfaces**: the Knowledge page and the MCP `idea_search`
//! tool (both through [`search_ideas`]), AND the global palette. They live here rather than in a
//! neutral `scoring` module because the shared set does not stop at the scoring — `tokenize` and
//! `build_fts_query` are also the query half of the `ideas` FTS read, and `fuzzy_score` /
//! `trigram_similarity` are `pub` API of this path. Extracting would move a public path and pull
//! the tests away from the code they cover, for a gain that is purely internal layering.

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
///
/// DECLARED and APPLIED are the same ratio: [`combine`] puts both signals under the same
/// [`CERTAINTY_CEILING`], so a one-term score is exactly
/// `CERTAINTY_CEILING · (FTS_WEIGHT · fts_norm + FUZZY_WEIGHT · similarity)`. Capping one side
/// alone would make these two numbers a fiction.
const FTS_WEIGHT: f64 = 0.6;
const FUZZY_WEIGHT: f64 = 1.0 - FTS_WEIGHT;
/// Minimum fuzzy similarity for a NON-FTS-matched idea to still be surfaced. Below this a
/// fuzzy hit is noise (prefix cases are already caught by the FTS prefix query, so this only
/// needs to admit genuine typos/variants of similar-length words). Tuned so a one-letter typo
/// clears it while unrelated words do not. Shared with [`crate::search`], whose non-indexed
/// sources admit a fuzzy match on exactly the same bar.
///
/// Written as `0.3 × CERTAINTY_CEILING` rather than as a bare `0.3` because that is what it means:
/// the bar has always been "a similarity of 0.3", and [`fuzzy_score_in_context`] now expresses a
/// similarity of `s` as `CERTAINTY_CEILING · s`. Scaling the bar with the scale keeps a ONE-WORD
/// query admitting exactly the set it admitted before the aggregation changed; a multi-word one is
/// more permissive, which is the defect being fixed.
pub(crate) const FUZZY_ADMIT_THRESHOLD: f64 = 0.3 * CERTAINTY_CEILING;
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

/// How much a query term that lands on an entity's CONTEXT (its project, its workspace) is worth
/// compared with the same term landing on the entity's own name. Half: the context is real evidence
/// — `dep` in `bui dep auth` genuinely says "the one in depot" — but a row whose NAME is what you
/// typed must never be outranked by a row that merely shares a container.
///
/// Only [`crate::search`] has a context to weigh; the per-project `idea_search` passes none (it is
/// already scoped to one project, so the project name would be a constant on every candidate).
pub(crate) const CONTEXT_WEIGHT: f64 = 0.5;

/// The most ONE query term can be worth on its own in [`fuzzy_score_in_context`].
///
/// A word is EVIDENCE, not a verdict: however perfectly `build` matches, it is one word of what the
/// user typed, and the certainty it alone can carry has to stop short of total — otherwise the
/// remaining words would have no room left to say anything, and `build dep auth` would score the
/// `build` of every project identically at `1`. The ceiling is what makes the second and third word
/// able to matter at all.
///
/// ## What it moves, and what it provably does not
///
/// On a ONE-TERM query — and only there — the aggregation collapses to `CERTAINTY_CEILING · s`,
/// and [`combine`] carries the SAME factor on the FTS side, so the whole blended score collapses
/// to `CERTAINTY_CEILING · (FTS_WEIGHT · fts_norm + FUZZY_WEIGHT · s)`: a positive HOMOTHETY of the
/// plain weighted blend of the two raw signals. It therefore moves no ordering — not just of the
/// fuzzy component, of the FINAL score the ranking sorts on — and [`FUZZY_ADMIT_THRESHOLD`] is
/// expressed through the same factor, so the admitted set is unchanged too. Pinned by
/// `tests::the_ceiling_is_a_homothety_of_the_whole_blend_on_a_one_term_query`.
///
/// BEYOND one term the opposite is true: `1 - Π(1 - C·sᵢ)` is not a homothety of the `sᵢ`, so this
/// constant IS a ranking lever. `(1.0, 0.0)` loses to `(0.9, 0.9)` at `C = 0.8` and beats it at
/// `C = 1.0`. That is not a side effect — it is the reason the ceiling exists (see
/// [`fuzzy_score_in_context`]). Retune it only with that in view.
const CERTAINTY_CEILING: f64 = 0.8;

/// The best trigram similarity of ONE query term against a term list, `0.0` against an empty one.
fn best_similarity(query_term: &str, terms: &[String]) -> f64 {
    terms
        .iter()
        .map(|term| trigram_similarity(query_term, term))
        .fold(0.0_f64, f64::max)
}

/// The FUZZY similarity of a query to an entity, in `[0, 1]` — [`fuzzy_score_in_context`] with no
/// context to weigh. The per-project `idea_search` scores through this one. Pure.
pub fn fuzzy_score(query_terms: &[String], idea_terms: &[String]) -> f64 {
    fuzzy_score_in_context(query_terms, idea_terms, &[])
}

/// The FUZZY similarity of a query to an entity IN ITS CONTEXT, in `[0, 1]`.
///
/// ## Per term: the name, or the context at a discount
///
/// Each query term scores the BEST trigram similarity it finds among the entity's OWN terms, or —
/// when the context says it better — [`CONTEXT_WEIGHT`] times its best similarity among the context
/// terms, whichever is larger. Term-granular on purpose: a typo'd query word matches its intended
/// word regardless of how long the body is (a whole-body trigram compare would drown a single
/// word).
///
/// ## Across terms: COVERAGE, not an average
///
/// This used to be the plain MEAN over the query terms, and the mean made the search PUNISH
/// PRECISION. `bui dep auth` scored `build` at (0.8 + 0 + 0) / 3 = 0.27 — under
/// [`FUZZY_ADMIT_THRESHOLD`] — so the very row the user had just described precisely was thrown
/// out, while the vaguer `bui` alone kept it. Every word added to narrow the search made the answer
/// worse. Any average does that: a term is a divisor before it is a contribution.
///
/// Terms are therefore combined as INDEPENDENT EVIDENCE instead — each worth at most
/// [`CERTAINTY_CEILING`], the score being the certainty that at least one piece is right:
///
/// ```text
/// score = 1 - Π (1 - CERTAINTY_CEILING · sᵢ)
/// ```
///
/// Evidence never subtracts. The properties that buys, each of them load-bearing:
///
///   * a word that finds NOTHING costs exactly nothing — its factor is `1`. `bui dep auth` scores
///     `build` no worse than `bui` does, whatever the other two words fail to match;
///   * every word that DOES land raises the score strictly, so more COVERAGE strictly outranks
///     less: name + project + workspace beats name + project beats name alone. That is the whole
///     point of putting the containers in the surface;
///   * it holds even when the name matches PERFECTLY, because [`CERTAINTY_CEILING`] keeps one term
///     short of total certainty. Without it `build dep auth` would score the `build` of every
///     project at `1` and leave the ranking to the recency tie-break;
///   * a ONE-word query keeps its ORDER and its ADMISSION exactly: the formula collapses to
///     `CERTAINTY_CEILING · s`, and — because [`combine`] scales the FTS side by that same ceiling
///     — so does the BLENDED score, which is the number the ranking actually sorts on. The
///     scaling is uniform on the whole blend, not on one of its two halves.
///     [`FUZZY_ADMIT_THRESHOLD`] carries the same factor, so the admitted set does not move either.
///
/// Pure.
pub(crate) fn fuzzy_score_in_context(
    query_terms: &[String],
    entity_terms: &[String],
    context_terms: &[String],
) -> f64 {
    if query_terms.is_empty() || (entity_terms.is_empty() && context_terms.is_empty()) {
        return 0.0;
    }
    let mut doubt = 1.0;
    for query_term in query_terms {
        let name = best_similarity(query_term, entity_terms);
        let context = CONTEXT_WEIGHT * best_similarity(query_term, context_terms);
        doubt *= 1.0 - CERTAINTY_CEILING * name.max(context);
    }
    1.0 - doubt
}

/// Blend an FTS component (`fts_norm`, already normalized to `[0,1]`, `0` = no FTS hit) with a
/// fuzzy component into the final relevance, in `[0,1]`. Pure.
///
/// ## Both signals enter under the same ceiling
///
/// The fuzzy side already carries [`CERTAINTY_CEILING`] per term
/// ([`fuzzy_score_in_context`]); the FTS side is scaled by it HERE, and the ceiling means the same
/// thing on both: `fts_norm = 1` says only "the best hit of ITS OWN batch" — a lone hit normalizes
/// to `1` for want of a spread ([`Bm25Norm::of_value`]) — which is a rank, not a certainty.
///
/// Capping one side only silently RETUNES the blend, and that is a real defect rather than a
/// cosmetic one: on a one-term query the weights would read `60 : 40` and act `60 : 32`, a fifth
/// of the fuzzy signal's declared influence quietly removed. It flips candidates whose two signals
/// disagree — `(fts_norm 0.60, s 0.225)` against `(0.10, 1.0)` reverses. Under the ceiling on BOTH
/// sides the DECLARED weights are the APPLIED weights, and a one-term score is exactly
/// `CERTAINTY_CEILING ×` the plain weighted blend of the raw signals.
///
/// `pub(crate)` because the GLOBAL palette ([`crate::search`]) blends the very same two signals:
/// duplicating the weights there would let the per-project and the global path rank the same idea
/// differently for no reason anyone could name.
pub(crate) fn combine(fts_norm: f64, fuzzy: f64) -> f64 {
    CERTAINTY_CEILING * FTS_WEIGHT * fts_norm + FUZZY_WEIGHT * fuzzy
}

/// The bm25 NORMALIZER of one FTS source: the `[0,1]` rescaling that makes a raw `bm25` usable.
///
/// bm25 is smaller-is-better, unbounded, and — this is the load-bearing part — comparable only
/// WITHIN a single FTS table: its value depends on that table's own term statistics, so the "3.2"
/// of `ideas_fts` and the "3.2" of `specs_fts` mean nothing to each other. Every consumer
/// therefore rescales PER SOURCE against that source's own best and worst hit, and only then puts
/// the results side by side.
///
/// Built from the scores of ONE source ([`Bm25Norm::of`]); [`Bm25Norm::of_value`] maps a raw score
/// to `1.0` for the best hit and `0.0` for the worst. A single hit — or a batch with no spread —
/// normalizes to `1.0` rather than to an arbitrary point of a zero-width range.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bm25Norm {
    best: f64,
    worst: f64,
}

impl Bm25Norm {
    /// Read the bounds of one source's raw bm25 scores.
    pub(crate) fn of(scores: impl IntoIterator<Item = f64>) -> Self {
        let (best, worst) = scores
            .into_iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            });
        Self { best, worst }
    }

    /// Rescale ONE raw bm25 of this source into `[0,1]` (higher = better).
    pub(crate) fn of_value(&self, value: f64) -> f64 {
        let spread = self.worst - self.best;
        if spread <= f64::EPSILON {
            1.0
        } else {
            (self.worst - value) / spread
        }
    }
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
        // `project_id` IS already the scope, `None` meaning "every project" — the cross-project
        // Idées table and the global palette both need it to reach the read untouched.
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

    // Normalize bm25 across the FTS hits to [0,1] (best match = 1), through the shared
    // per-source normalizer the global palette uses too.
    let normalizer = Bm25Norm::of(fts_scores.values().copied());
    let fts_norm = |id: &str| -> f64 {
        match fts_scores.get(id) {
            None => 0.0,
            Some(&v) => normalizer.of_value(v),
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
