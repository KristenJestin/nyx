//! The GLOBAL SEARCH aggregator — the single backend of the `Mod+K` palette (Spec « Recherche
//! live globale », Phase 1, task `01KZ8GHEDYW75C7D8R54RAWE59`).
//!
//! ## What this module answers
//!
//! ONE ranked, BOUNDED list over the seven entity types of the product, ALL PROJECTS CONFOUNDED:
//! Specs (title + the prose of their current revision), tasks, ideas, workspaces, projects,
//! managed commands, agent sessions. The user has no single place to look things up today; this is
//! that place, and it is deliberately the only one — a second search path would be a second answer
//! to the same question.
//!
//! "Managed command" here means a command INSTANCE, not a template: what the palette lists is what
//! it can open, and a `managed_commands` row is a project-level definition with no pane behind it
//! (see [`crate::db::recent_search_commands`]). A template materialized in two workspaces is
//! therefore two rows, told apart by the workspace each one names.
//!
//! ## Typed targets, never a route
//!
//! A hit carries an IDENTITY (`kind` + `id` + the container ids), never a URL and never a route
//! fragment. Deciding where a kind lands is the renderer's business and changes with the routes;
//! encoding it here would freeze a routing table inside the backend and force a sidecar release to
//! move a page. So the backend answers "this is a Spec, in this project" and the renderer answers
//! "a Spec is at `/work/$specId`".
//!
//! ## Two shapes of read, one shape of answer
//!
//!   * VOLUMINOUS sources go through FTS5 (`specs_fts`, `spec_revisions_fts`, `tasks_fts`,
//!     `ideas_fts`): SQLite does the matching.
//!   * LOW-VOLUME sources (projects, workspaces, managed commands, agent sessions) have no index
//!     and need none: a BOUNDED recency read plus an exact / prefix / substring / fuzzy pass in
//!     Rust covers them, at a fraction of the cost of maintaining four more indexes.
//!
//! Both produce the same [`SearchHit`], and the EMPTY query produces it too — the suggestions are
//! not a second model, just the same list ordered by recency alone. One shape of answer means one
//! rendering path in the palette.
//!
//! ## Scoring, and why it is bucketed rather than blended
//!
//! `bm25` is comparable only within ONE FTS table (it depends on that table's own term
//! statistics), so each source is normalized to `[0,1]` against its own best/worst
//! ([`crate::idea::Bm25Norm`]) before anything is compared. Even then a normalized relevance
//! cannot arbitrate "the Spec literally named what you typed" against "a task whose title shares a
//! word": those are different KINDS of certainty. Hence the ordered buckets — exact title, then
//! prefix, then a match (FTS or fuzzy) — with the normalized score ordering INSIDE a bucket and
//! recency breaking the remaining ties. A pure score would let a long fuzzy tail outrank an exact
//! hit, which is the one outcome a palette may never produce.
//!
//! ## ONE list, ranked across the types
//!
//! That same comparison is what orders the WHOLE answer, types confounded ([`by_rank`]): the
//! palette draws one list and no section headings, so a managed command named exactly what was
//! typed sits above a Spec that merely shares a word with it. A fixed sequence of type sections did
//! the opposite — it put a mediocre Spec above an exact command, whatever the query — and the row's
//! own glyph already says which type it is.
//!
//! WHAT MAKES THIS SOUND IS THE BUCKET, NOT THE SCORE. The normalization above puts every source on
//! the same `[0,1]` scale, which is NOT the same as making the numbers mean the same thing: a `0.8`
//! from `specs_fts` and a `0.8` from the fuzzy pass over command names are two different questions
//! answered on a common axis. The buckets, on the other hand, are comparable BY CONSTRUCTION — an
//! exact label match is an exact label match whatever the table it came from. So the cross-type
//! order leans on the bucket FIRST and lets the score arbitrate only INSIDE one, where the
//! comparison is between rows of comparable certainty.
//!
//! The scoring HELPERS are shared with the per-project `idea_search`
//! ([`crate::idea::build_fts_query`], [`crate::idea::combine`], [`crate::idea::Bm25Norm`],
//! [`crate::idea::fuzzy_score_in_context`]): the same idea must not rank differently depending on
//! which surface asked.
//!
//! ## An entity is searched IN ITS CONTEXT
//!
//! A row is not just a name: it lives in a project, and often in a workspace. Both are part of what
//! the user types — `bui dep auth` means "the `build` of *depot*, in *auth*" — so both are part of
//! the match surface ([`literal_verdict`], [`context_terms`]), at [`crate::idea::CONTEXT_WEIGHT`],
//! i.e. strictly below the entity's own name. On the literal path that discount is structural: the
//! context can only ever produce a `Match`, never an `Exact` or a `Prefix`.
//!
//! This only works because the aggregation over query terms REWARDS COVERAGE instead of averaging
//! it away — see [`crate::idea::fuzzy_score_in_context`], which is where the old flat mean made
//! every added term lower the score. The two halves are one change: context in the surface without
//! coverage in the aggregation would still have thrown `bui dep auth` out.
//!
//! ## COVERAGE is the second comparable key, and it is one BECAUSE the score is not
//!
//! The literal pass above reads the query as ONE STRING. That is the whole truth of a one-word
//! query — the needle IS the word — and it is nearly none of it past that: `audio manager build` is
//! not equal to, does not prefix and is not contained in `build`, in `Music Manager` or in `audio
//! player`, so the command that answers all three words used to land in [`Bucket::Match`] beside
//! every Spec that merely says `audio`. Typing a second word DISARMED the buckets, which is exactly
//! backwards: a second word is more of an answer, not less of one.
//!
//! So the literal pass is now read TERM BY TERM as well ([`literal_verdict`]), and it answers two
//! things instead of one:
//!
//!   * the BUCKET — unchanged for one term, and reachable again past it: a row whose OWN NAME
//!     accounts for every word typed claims [`Bucket::Exact`] (its words ARE the query) or
//!     [`Bucket::Prefix`] (each word prefixes one of its words). The context still cannot buy those
//!     two, exactly as before — only the entity's own name can;
//!   * the COVERAGE — how many of the typed words the row accounts for AT ALL, name or context.
//!
//! Coverage then leads [`by_rank`] right after the bucket, ahead of the score, and the reason is the
//! one the module has been arguing since its first line: `score` is normalized PER SOURCE, so a
//! `0.8` from `specs_fts` and a `0.32` from a fuzzy pass over a command name are not two values of
//! one quantity. Worse, they are not even bounded alike — the four non-indexed sources have
//! `fts_norm = 0` BY CONSTRUCTION, so their ceiling is `FUZZY_WEIGHT` where an indexed source's is
//! four times that. At equal bucket the score therefore did not arbitrate, it decided by source, and
//! the row answering the whole question lost to rows answering a third of it.
//!
//! "How many of the words you typed does this row account for" has no such defect: it is the same
//! integer computed the same way in all seven sources, over the surface the palette DRAWS. It is
//! comparable by construction, like the bucket and unlike the score — which is why it goes where it
//! goes, and why the score keeps the job it can do honestly: separating rows of equal certainty AND
//! equal coverage.

use diesel::result::QueryResult;
use diesel::SqliteConnection;

use crate::db::{self, SearchCandidate};
use crate::idea::{self, Bm25Norm};

/// Maximum hits returned PER TYPE. Also the number of suggestions the empty query returns — the
/// palette shows one un-scrolled screen of them, and that screen is eight rows.
pub const CAP_TYPE: usize = 8;

/// Maximum hits returned overall, all types confounded. Seven groups of [`CAP_TYPE`] would be
/// fifty-six; forty is the point past which a palette stops being a shortcut and becomes a list to
/// read. The trim follows [`GROUP_ORDER`], so it is deterministic rather than "whatever fit".
pub const CAP_TOTAL: usize = 40;

/// Shortest query that reaches the database. One character matches nearly everything, so it is not
/// a search — it is a full-table scan with a ranking on top. Below this the answer is empty and NO
/// read is issued at all.
pub const MIN_QUERY_LEN: usize = 2;

/// How many rows a BOUNDED read pulls from a source with no FTS index before the Rust match pass.
/// Generous for the real volumetry of these four tables (dozens, at most a few hundred) while
/// keeping every scan O(bounded) — the palette can never degrade into an unbounded read.
const CANDIDATE_CAP: i64 = 1_000;

/// How many rows an FTS read pulls before the bucketing re-orders them. Larger than [`CAP_TYPE`]
/// on purpose: bm25 order is not the final order (an exact title match outranks a better-scored
/// partial one), so the ranking needs a head deep enough to re-sort, not just the eight rows it
/// will keep.
const FTS_READ_CAP: i64 = 50;

/// The seven entity types the palette spans.
///
/// The declaration order IS the trim order (see [`GROUP_ORDER`]) and the tie-break order of the
/// suggestions, hence the derived `Ord`: knowledge first (Specs, tasks, ideas), then the places
/// they live in (workspaces, projects), then what runs there (commands, sessions). It is NOT a
/// display order — the results are one list ranked by [`by_rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Spec,
    Task,
    Idea,
    Workspace,
    Project,
    Command,
    Session,
}

/// The FIXED order the groups are TRIMMED in: Specs → Tasks → Ideas → Workspaces → Projects →
/// Managed commands → Agent sessions. It decides WHICH hits survive a saturated answer (the
/// [`CAP_TOTAL`] cut walks it, so the four non-indexed sources are the ones that lose rows), and
/// nothing else.
///
/// It is NO LONGER A DISPLAY ORDER: the palette draws one list ranked by [`by_rank`], types
/// confounded. Keeping a fixed sequence of sections meant a type outranked another before the query
/// was even read.
pub const GROUP_ORDER: [SearchKind; 7] = [
    SearchKind::Spec,
    SearchKind::Task,
    SearchKind::Idea,
    SearchKind::Workspace,
    SearchKind::Project,
    SearchKind::Command,
    SearchKind::Session,
];

/// ONE result of the palette: a TYPED TARGET plus what to draw for it.
///
/// The target half is `kind` + `id` + the containers (`project_id`, and `workspace_id` /
/// `spec_id` / `terminal_id` when the kind has one). There is deliberately NO url, path or route
/// fragment here — see the module header.
///
/// The display half is `label` (the entity's name), `detail` (a short secondary line, when the
/// source has one), `extract` (the match window, when the source can produce one) and `human_key`
/// (a Spec's `KEY-number` — displayed, never matched).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SearchHit {
    pub kind: SearchKind,
    /// The entity to open. For a Spec matched through its PROSE this is the Spec, not the
    /// revision: a revision has no surface of its own.
    pub id: String,
    /// The project that OWNS the entity — the renderer activates it before navigating, because
    /// every surface of the app is read through the entered project.
    pub project_id: String,
    /// The NAME of that project, joined at the source.
    ///
    /// It travels WITH the hit rather than being resolved by the renderer from the rail's project
    /// tree, for two reasons that point the same way. The tree only holds what the rail loaded, so
    /// a hit from anywhere else drew an opaque id; and a name the BACKEND never reads is a name the
    /// backend cannot MATCH, which is what [`literal_verdict`] and [`context_terms`] now do with it.
    pub project_name: String,
    /// The workspace to enter, when the kind has one.
    pub workspace_id: Option<String>,
    /// The NAME of that workspace, `None` exactly where `workspace_id` is — the two fields are one
    /// pair.
    ///
    /// An entity lives in a project AND, for three of the seven kinds, in a workspace: the
    /// workspaces themselves, the managed-command INSTANCES and the agent sessions. Two rows from
    /// two workspaces of one project are otherwise the same row twice — the exact situation one
    /// template answering one row per instance creates.
    pub workspace_name: Option<String>,
    /// For a TASK: the Spec that contains it — v1 creates no task route, so a task opens the
    /// nearest existing surface. `None` for a task attached to no Spec.
    pub spec_id: Option<String>,
    /// For an agent SESSION: the terminal that hosts it. A session is reached through its
    /// terminal or not at all.
    pub terminal_id: Option<String>,
    pub label: String,
    pub detail: Option<String>,
    pub extract: Option<String>,
    /// The entity's HUMAN KEY when it has one — today a Spec's `KEY-number` (e.g. `NYX-42`),
    /// `None` for every other kind. DRAWN, NEVER MATCHED: it is a field of its own rather than a
    /// second `detail` precisely because `detail` feeds [`literal_verdict`] and
    /// [`searchable_terms`], so putting the key there would turn `NYX-42` into a query and move a
    /// ranking that was tuned without it. Searching a Spec BY its key is a separate capability.
    pub human_key: Option<String>,
    /// The blended relevance in `[0,1]`, higher = better (rounded to 4 decimals). `0` on a
    /// suggestion, which is ordered by recency and never scored.
    pub score: f64,
    /// The entity's recency (`updated_at`, or `last_seen_at` for a session) — the ordering of the
    /// suggestions and the last tie-break of the ranking.
    pub updated_at: i64,
}

/// The ANSWER of ONE search: the hits, plus the one fact about the install the hits themselves
/// cannot carry.
///
/// `hits` alone is not enough for the palette's EMPTY STATE. "This install holds no Spec, no task
/// and no idea" is a property of the DATABASE, while the suggestions are a list capped at
/// [`CAP_TYPE`] — so reading that fact off the eight most recently touched entities answers a
/// different question ("is there one in the top eight"), and answers it wrong on the ordinary
/// install whose top eight are sessions, commands and workspaces (a session's `last_seen_at` is
/// re-stamped at every agent start, so those saturate the head while the Specs sit still). The
/// information exists only HERE, at the source, and therefore travels as its own field.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SearchAnswer {
    /// The ranked results, or the recency-ordered suggestions of an empty query.
    pub hits: Vec<SearchHit>,
    /// `true` when the install carries NO searchable Spec, NO task and NO idea — the "nothing to
    /// search yet" of a fresh install, which the containers one just created (a project, its root
    /// workspace) do not lift.
    ///
    /// "Searchable" is the very definition the suggestions use: the exclusions of the recency reads
    /// (an archived Spec, a task whose Spec is archived) apply here too, because this flag is
    /// computed through those same reads.
    ///
    /// The one regime that does not measure it is the below-[`MIN_QUERY_LEN`] query, which issues
    /// NO read at all and answers `false`. That is the conservative value — it can only ever hide
    /// the empty state, never invent one — and that regime cannot render it anyway: a typed
    /// character is a query, and a query that found nothing says so instead.
    pub knowledge_empty: bool,
}

/// The two BOUNDS of one search. Both are clamped by the core, so a caller cannot widen them: the
/// point of a palette is that it can never answer with a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimits {
    /// Hits kept per type, clamped to `[1, CAP_TYPE]`.
    pub per_type: usize,
    /// Hits kept overall, clamped to `[1, CAP_TOTAL]`.
    pub total: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            per_type: CAP_TYPE,
            total: CAP_TOTAL,
        }
    }
}

impl SearchLimits {
    /// The default bounds narrowed to a caller-supplied OVERALL cap (the `limit` the `db_command`
    /// kind accepts). The per-type cap is untouched: a caller asking for fewer results wants a
    /// shorter list, not a differently-shaped one.
    pub fn with_total(total: usize) -> Self {
        Self {
            total,
            ..Self::default()
        }
    }

    /// Both bounds forced back inside their ceilings.
    fn clamped(self) -> Self {
        Self {
            per_type: self.per_type.clamp(1, CAP_TYPE),
            total: self.total.clamp(1, CAP_TOTAL),
        }
    }

    /// How many SUGGESTIONS an empty query returns: the per-type cap, never more than the overall
    /// one (a caller that asked for three results gets three suggestions, not eight).
    fn suggestions(self) -> usize {
        self.per_type.min(self.total)
    }
}

/// The ORDERED certainty of a match. Lower is better, and the order is the whole point: no
/// relevance score may promote a fuzzy tail above the entity whose name IS what the user typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bucket {
    /// The label is exactly the query.
    Exact,
    /// The label starts with the query.
    Prefix,
    /// Something matched — an FTS hit, a substring of the searchable text, or a fuzzy variant.
    Match,
}

/// The LITERAL verdict on one candidate: HOW CERTAIN the match is, and HOW MUCH of the query it
/// accounts for.
///
/// One struct rather than two returns because the two are computed from the same walk over the
/// query's terms, and because they are read together: [`by_rank`] compares the bucket, then the
/// coverage, then the score.
struct Literal {
    bucket: Bucket,
    /// How many of the query's terms landed literally SOMEWHERE on the match surface — the
    /// entity's own text or its containers, at any strength.
    ///
    /// `0` on a query of fewer than two terms, where the notion does not apply: see
    /// [`literal_verdict`].
    coverage: usize,
}

/// A hit that still remembers HOW CERTAIN its match was, and HOW MUCH of the query it answered.
///
/// Neither reaches the wire — [`SearchHit`] carries the score and nothing else — so the ordering
/// that needs them has to happen here. That is precisely why the cross-type sort lives in the
/// aggregator rather than in the palette: the renderer cannot re-derive a certainty it was never
/// told, and sorting on the score alone would be the blend the buckets exist to prevent.
struct Ranked {
    bucket: Bucket,
    /// [`Literal::coverage`], or `0` for a row admitted by the FTS index or the fuzzy pass with
    /// nothing literal to show — such a row accounts for no term it can be checked against, and
    /// ranks after any row that does.
    coverage: usize,
    hit: SearchHit,
}

/// A candidate on its way to becoming a hit: the row, plus what the FTS pass (if any) said about
/// it.
struct Scored {
    candidate: SearchCandidate,
    /// The bm25 of this candidate, normalized to `[0,1]` AGAINST ITS OWN SOURCE.
    fts_norm: f64,
    /// Whether an FTS index actually matched it. Distinct from `fts_norm > 0` on purpose: the
    /// WORST hit of a source normalizes to exactly `0.0` and must not be mistaken for a row the
    /// index never returned.
    from_fts: bool,
}

/// SEARCH every source of the product for `query`, ALL PROJECTS CONFOUNDED, and return one ranked
/// list bounded by `limits` — the single read behind the `Mod+K` palette.
///
/// The answer is a [`SearchAnswer`]: the hits, plus [`SearchAnswer::knowledge_empty`], the one
/// thing about the install a capped list cannot be asked.
///
/// Three regimes, one return type:
///
///   * an EMPTY query (or blanks only) returns the SUGGESTIONS — the most recently touched
///     entities of every type, ordered by recency alone. Nothing is scored, and no journal of
///     visited entities is read, because none exists: what these show is what was last TOUCHED,
///     which is often an agent's doing rather than the user's.
///   * a query of a single character returns an EMPTY list WITHOUT touching the database (see
///     [`MIN_QUERY_LEN`]).
///   * anything longer is matched, bucketed, scored and trimmed.
pub fn global_search(
    conn: &mut SqliteConnection,
    query: &str,
    limits: SearchLimits,
) -> QueryResult<SearchAnswer> {
    let limits = limits.clamped();
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return suggestions(conn, limits);
    }
    if trimmed.chars().count() < MIN_QUERY_LEN {
        // No read at all — see [`SearchAnswer::knowledge_empty`] for why this regime reports
        // `false` rather than measuring a fact it deliberately does not go and look up.
        return Ok(SearchAnswer {
            hits: Vec::new(),
            knowledge_empty: false,
        });
    }
    ranked(conn, trimmed, limits)
}

/// Does this install carry ANY searchable Spec, task or idea?
///
/// Asked through the very same three recency reads the suggestions are built on, bounded to ONE
/// row: what those reads exclude (an archived Spec, a task whose Spec is archived) IS the
/// definition of searchable, and a second piece of SQL spelling that out again would drift from
/// them without anything failing. It short-circuits on the first source that answers, so the
/// ordinary install pays a single `LIMIT 1` read.
fn knowledge_exists(conn: &mut SqliteConnection) -> QueryResult<bool> {
    Ok(!db::recent_search_specs(conn, 1)?.is_empty()
        || !db::recent_search_tasks(conn, 1)?.is_empty()
        || !db::recent_search_ideas(conn, 1)?.is_empty())
}

/// The empty-query answer: the most recently touched entities of the seven sources, merged and
/// cut to [`SearchLimits::suggestions`].
///
/// Each source is read through its OWN recency key — `updated_at` everywhere except
/// `agent_sessions`, which does not carry that column and is ordered by `last_seen_at`. There is
/// no single column to sort seven tables on, so the merge happens here, over the value each read
/// aliases to `updated_at`. Ties fall back to the group order and then to the id, so the list is
/// reproducible on identical fixtures instead of "whatever SQLite returned first".
fn suggestions(conn: &mut SqliteConnection, limits: SearchLimits) -> QueryResult<SearchAnswer> {
    let n = limits.suggestions();
    let cap = n as i64;
    let specs = db::recent_search_specs(conn, cap)?;
    let tasks = db::recent_search_tasks(conn, cap)?;
    let ideas = db::recent_search_ideas(conn, cap)?;
    // The install's KNOWLEDGE, read BEFORE the merge — and free, because each head IS its own
    // source's answer: an empty one means that table has nothing searchable. After the merge and
    // the truncation below, that same question is unanswerable (a Spec that fell out of the top
    // `n` is indistinguishable from a Spec that does not exist), which is the whole reason the
    // flag is computed at this point and shipped rather than inferred downstream.
    let knowledge_empty = specs.is_empty() && tasks.is_empty() && ideas.is_empty();
    let sources = [
        (SearchKind::Spec, specs),
        (SearchKind::Task, tasks),
        (SearchKind::Idea, ideas),
        (
            SearchKind::Workspace,
            db::recent_search_workspaces(conn, cap)?,
        ),
        (SearchKind::Project, db::recent_search_projects(conn, cap)?),
        (SearchKind::Command, db::recent_search_commands(conn, cap)?),
        (SearchKind::Session, db::recent_search_sessions(conn, cap)?),
    ];

    let mut hits: Vec<SearchHit> = Vec::new();
    for (kind, rows) in sources {
        for candidate in rows {
            hits.push(into_hit(kind, candidate, 0.0));
        }
    }
    hits.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.id.cmp(&b.id))
    });
    hits.truncate(n);
    Ok(SearchAnswer {
        hits,
        knowledge_empty,
    })
}

/// The scored answer: every source matched, bucketed, trimmed twice (per type, then overall in
/// [`GROUP_ORDER`]) and finally ordered as ONE list by [`by_rank`].
///
/// The two steps answer two different questions and are deliberately not merged. The TRIM decides
/// what survives a saturated answer, and it walks the group order so that a container-only match
/// (a session whose project name happens to contain the query) fills the tail of the forty instead
/// of evicting an indexed hit. The ORDER decides what the user reads first, and it knows nothing of
/// types.
fn ranked(
    conn: &mut SqliteConnection,
    query: &str,
    limits: SearchLimits,
) -> QueryResult<SearchAnswer> {
    // Measured on this path too, so the field means the same thing whatever the regime — a caller
    // never has to know WHICH question it asked to know what the flag answered.
    let knowledge_empty = !knowledge_exists(conn)?;
    let terms = idea::tokenize(query);
    let needle = query.to_lowercase();
    // A query made only of punctuation has no usable FTS term; the indexed sources then return
    // nothing and the fuzzy pass carries what it can, exactly as `idea_search` degrades.
    let expression = idea::build_fts_query(query);

    // --- the FTS sources ------------------------------------------------------------------
    //
    // A Spec answers from TWO indexes — its own title and the prose of its current revision — so
    // the two reads are normalized separately (bm25 is not comparable across tables) and then
    // folded on the Spec id, keeping the better of the two and the extract whichever produced it.
    let spec_rows = match &expression {
        Some(expression) => fold_sources(vec![
            db::fts_match_specs(conn, None, expression, FTS_READ_CAP)?,
            db::fts_match_spec_revisions(conn, None, expression, FTS_READ_CAP)?,
        ]),
        None => Vec::new(),
    };
    let task_rows = match &expression {
        Some(expression) => fold_sources(vec![db::fts_match_tasks(
            conn,
            None,
            expression,
            FTS_READ_CAP,
        )?]),
        None => Vec::new(),
    };
    let idea_rows = match &expression {
        Some(expression) => fold_sources(vec![db::fts_match_ideas(
            conn,
            None,
            expression,
            FTS_READ_CAP,
        )?]),
        None => Vec::new(),
    };

    // --- the sources with no index --------------------------------------------------------
    //
    // A bounded recency head, matched in Rust. `from_fts` is false for all of them, so each has
    // to earn its place through the exact / prefix / substring / fuzzy pass below.
    let workspace_rows = unscored(db::recent_search_workspaces(conn, CANDIDATE_CAP)?);
    let project_rows = unscored(db::recent_search_projects(conn, CANDIDATE_CAP)?);
    let command_rows = unscored(db::recent_search_commands(conn, CANDIDATE_CAP)?);
    let session_rows = unscored(db::recent_search_sessions(conn, CANDIDATE_CAP)?);

    let mut groups: Vec<(SearchKind, Vec<Ranked>)> = vec![
        (SearchKind::Spec, spec_rows),
        (SearchKind::Task, task_rows),
        (SearchKind::Idea, idea_rows),
        (SearchKind::Workspace, workspace_rows),
        (SearchKind::Project, project_rows),
        (SearchKind::Command, command_rows),
        (SearchKind::Session, session_rows),
    ]
    .into_iter()
    .map(|(kind, rows)| {
        (
            kind,
            rank_group(kind, rows, &terms, &needle, limits.per_type),
        )
    })
    .collect();

    // The OVERALL trim walks the groups in their fixed order, so which hits survive a saturated
    // answer is a property of the order, not of the iteration.
    let mut out: Vec<Ranked> = Vec::new();
    'trim: for kind in GROUP_ORDER {
        let Some(index) = groups.iter().position(|(k, _)| *k == kind) else {
            continue;
        };
        let (_, hits) = groups.remove(index);
        for hit in hits {
            if out.len() >= limits.total {
                break 'trim;
            }
            out.push(hit);
        }
    }
    // …and THEN the survivors become one list, ranked without regard to their type. Sorted after
    // the trim rather than before it so the two rules stay separable: the cap keeps favouring the
    // indexed sources, the reading order favours nothing but the match.
    out.sort_by(by_rank);
    Ok(SearchAnswer {
        hits: out.into_iter().map(|ranked| ranked.hit).collect(),
        knowledge_empty,
    })
}

/// THE ORDER OF THE PALETTE, and the only one: certainty first, then how much of the query the row
/// accounts for, then relevance, then recency, then the id — so it is TOTAL and two runs over
/// identical fixtures cannot differ.
///
/// It orders a group and it orders the whole answer, by the same call: what is true of two Specs
/// ("the one whose title IS the query comes first") is exactly as true of a Spec and a command, and
/// a second comparison for the cross-type case would be a second, quieter ranking rule.
///
/// The first TWO keys lead for the reason the module header gives: they are the terms of this
/// comparison that mean the same thing in every source. A bucket is a bucket whatever the table it
/// came from, and "three of the words you typed are accounted for" is the same statement about a
/// command and about a Spec. `score` is normalized per source AND ceilinged by source, so it
/// separates rows of equal certainty and equal coverage honestly and arbitrates across sources only
/// approximately — which, four keys deep, is all it is asked to do.
///
/// Coverage sits BELOW the bucket rather than beside it, and the two never contradict: a bucket
/// better than [`Bucket::Match`] is only reachable through a surface that accounts for every term
/// ([`literal_verdict`]), so a better bucket already implies full coverage.
fn by_rank(a: &Ranked, b: &Ranked) -> std::cmp::Ordering {
    a.bucket
        .cmp(&b.bucket)
        .then_with(|| b.coverage.cmp(&a.coverage))
        .then_with(|| {
            b.hit
                .score
                .partial_cmp(&a.hit.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| b.hit.updated_at.cmp(&a.hit.updated_at))
        .then_with(|| a.hit.id.cmp(&b.hit.id))
}

/// Normalize each source's bm25 against ITS OWN best/worst, then fold the sources on the entity
/// id — keeping the best normalized score, and the extract of whichever source produced one.
///
/// The per-source normalization is the reason this cannot be one flat pass: a raw bm25 of
/// `specs_fts` and one of `spec_revisions_fts` are numbers from two different scales.
fn fold_sources(sources: Vec<Vec<SearchCandidate>>) -> Vec<Scored> {
    let mut folded: Vec<Scored> = Vec::new();
    // Id → its index in `folded`. The fold used to LOOK UP by scanning `folded` for each incoming
    // row, which is quadratic in the number of candidates: two Spec reads of `FTS_READ_CAP` are a
    // hundred rows, so five thousand id comparisons on every keystroke. The map answers the same
    // question in one hash, and the OUTPUT IS UNCHANGED — insertion still appends in encounter
    // order, so the caller sees the same sequence it always did.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for rows in sources {
        let normalizer = Bm25Norm::of(rows.iter().map(|row| row.score));
        for candidate in rows {
            let fts_norm = normalizer.of_value(candidate.score);
            match seen.get(&candidate.id).copied() {
                Some(index) => {
                    let kept = &mut folded[index];
                    if kept.candidate.extract.is_none() {
                        kept.candidate.extract = candidate.extract.clone();
                    }
                    if fts_norm > kept.fts_norm {
                        kept.fts_norm = fts_norm;
                    }
                }
                None => {
                    seen.insert(candidate.id.clone(), folded.len());
                    folded.push(Scored {
                        candidate,
                        fts_norm,
                        from_fts: true,
                    });
                }
            }
        }
    }
    folded
}

/// Wrap the rows of a source that has no index: nothing was scored, and nothing is admitted for
/// free.
fn unscored(rows: Vec<SearchCandidate>) -> Vec<Scored> {
    rows.into_iter()
        .map(|candidate| Scored {
            candidate,
            fts_norm: 0.0,
            from_fts: false,
        })
        .collect()
}

/// Match, score, order and cut ONE group — keeping each survivor's [`Bucket`], which is what lets
/// [`ranked`] merge the seven groups into a single ordered list afterwards.
fn rank_group(
    kind: SearchKind,
    rows: Vec<Scored>,
    terms: &[String],
    needle: &str,
    per_type: usize,
) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = Vec::new();
    for row in rows {
        let label = row.candidate.label.to_lowercase();
        let fuzzy = idea::fuzzy_score_in_context(
            terms,
            &searchable_terms(&row.candidate),
            &context_terms(&row.candidate),
        );
        let literal = literal_verdict(needle, terms, &label, &row.candidate);
        // What earns a place: a literal match of any strength, an FTS hit, or a fuzzy variant
        // above the shared admit bar. Everything else is noise wearing a score.
        let (bucket, coverage) = match literal {
            Some(literal) => (literal.bucket, literal.coverage),
            None if row.from_fts => (Bucket::Match, 0),
            None if fuzzy >= idea::FUZZY_ADMIT_THRESHOLD => (Bucket::Match, 0),
            None => continue,
        };
        let score = idea::combine(row.fts_norm, fuzzy);
        ranked.push(Ranked {
            bucket,
            coverage,
            hit: into_hit(kind, row.candidate, score),
        });
    }
    // The very comparison the whole answer is ordered by — applied here to decide WHICH eight of a
    // type survive the per-type cap.
    ranked.sort_by(by_rank);
    ranked.truncate(per_type);
    ranked
}

/// The SECONDARY tier of the match surface, lowercased: the entity's other text (`detail`,
/// `extract`) and its containers (`project_name`, `workspace_name`).
///
/// `human_key` is deliberately ABSENT, so a Spec's `NYX-42` is drawn on its row without becoming a
/// query and without shifting an order that was settled before the key existed.
fn secondary_surface(candidate: &SearchCandidate) -> Vec<String> {
    [
        candidate.detail.as_deref(),
        candidate.extract.as_deref(),
        Some(candidate.project_name.as_str()),
        candidate.workspace_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::to_lowercase)
    .collect()
}

/// The LITERAL certainty of a candidate against the query read as ONE STRING — the rule as it has
/// always been, and the WHOLE rule for a query of fewer than two terms.
///
/// `label` is already lowercased, `needle` is the lowercased query, `secondary` comes from
/// [`secondary_surface`]. The context can produce a `Match` and never more: only the entity's own
/// name reaches the two certain buckets.
fn needle_bucket(needle: &str, label: &str, secondary: &[String]) -> Option<Bucket> {
    if label == needle {
        return Some(Bucket::Exact);
    }
    if label.starts_with(needle) {
        return Some(Bucket::Prefix);
    }
    if label.contains(needle) || secondary.iter().any(|text| text.contains(needle)) {
        return Some(Bucket::Match);
    }
    None
}

/// The LITERAL verdict on a candidate — its [`Bucket`] and its [`Literal::coverage`] — or `None`
/// when nothing literal matched at all (the caller then falls back to the FTS verdict or the fuzzy
/// pass).
///
/// The match surface is the entity's own text (`label` + `detail` + `extract`) PLUS ITS CONTEXT
/// (`project_name` + `workspace_name`), and nothing else — the same surface on both readings below,
/// and the same surface the palette DRAWS, so a row's coverage is a claim the user can check
/// against the row in front of them.
///
/// ## Two readings of the same query, and why the second exists
///
/// [`needle_bucket`] reads the query as one string. That is the entire truth of a ONE-WORD query,
/// and almost none of it beyond: `audio manager build` is neither equal to, nor a prefix of, nor
/// contained in `build`, `bun run build`, `Music Manager` or `audio player`, so the one row that
/// answered all three words fell into `Match` alongside every row that answered `audio` alone. Past
/// one word the buckets stopped saying anything, which is the defect this second reading repairs.
///
/// Term by term, then:
///
///   * `Exact` when the label's words ARE the query's terms, set for set — `pipeline build` finds
///     the command named `build pipeline`, which is the multi-word form of "the label is exactly
///     what you typed";
///   * `Prefix` when every term prefixes one of the label's words, the label simply having more to
///     say — `aud pla` finds `audio player`;
///   * `Match` when at least one term lands anywhere on the surface;
///   * `None` when none does.
///
/// THE CONTEXT STILL BUYS NOTHING ABOVE `Match`, on either reading. `Exact` and `Prefix` are
/// decided on the label alone, so a row that merely shares a project with what you typed can never
/// outrank the row literally named it. What the context does buy is COVERAGE — and coverage is
/// compared before the score, which is how the `build` of `Music Manager` / `audio player` finally
/// outranks a Spec that says `audio` and nothing else.
///
/// The two readings are combined by keeping the BETTER bucket, never a worse one: the string
/// reading catches what the terms cannot (a query whose punctuation is part of the label, a term
/// that lands mid-word) and the term reading catches what the string cannot. Neither can lower the
/// other's verdict.
///
/// ## The one-term regime is left strictly alone
///
/// Fewer than two terms returns [`needle_bucket`]'s answer verbatim, with a coverage of `0` — the
/// value every other row of that same search also carries, so the key cannot reorder anything. That
/// is deliberate and not a shortcut: a single word is not a SET to cover, and "did it land" is
/// precisely the question the string reading already answered. The defect being fixed is the
/// multi-term one; the regime that already worked keeps the ranking and the admission it had, to
/// the bit.
fn literal_verdict(
    needle: &str,
    terms: &[String],
    label: &str,
    candidate: &SearchCandidate,
) -> Option<Literal> {
    let secondary = secondary_surface(candidate);
    let whole = needle_bucket(needle, label, &secondary);
    if terms.len() < 2 {
        return whole.map(|bucket| Literal {
            bucket,
            coverage: 0,
        });
    }

    let words = idea::tokenize(label);
    let mut coverage = 0;
    let mut every_term_prefixes_a_word = true;
    for term in terms {
        let prefixes_a_word = words.iter().any(|word| word.starts_with(term.as_str()));
        every_term_prefixes_a_word &= prefixes_a_word;
        // A term that lands MID-WORD (`dio` in `audio`) is on the name, but it is not a prefix of
        // it: it counts for coverage and not for the two certain buckets.
        let landed = prefixes_a_word
            || label.contains(term.as_str())
            || secondary.iter().any(|text| text.contains(term.as_str()));
        if landed {
            coverage += 1;
        }
    }
    let complete = coverage == terms.len();
    let per_term = if complete && label_is_the_query_word_for_word(terms, &words) {
        Some(Bucket::Exact)
    } else if complete && every_term_prefixes_a_word {
        Some(Bucket::Prefix)
    } else if coverage > 0 {
        Some(Bucket::Match)
    } else {
        None
    };

    let bucket = match (whole, per_term) {
        (Some(whole), Some(per_term)) => whole.min(per_term),
        (Some(whole), None) => whole,
        (None, Some(per_term)) => per_term,
        (None, None) => return None,
    };
    Some(Literal { bucket, coverage })
}

/// Are the label's words EXACTLY the query's terms, order aside?
///
/// The multi-word form of `label == needle`. Order-free because the query is already a BAG of words
/// everywhere else — the FTS expression `OR`-joins its terms and the fuzzy pass scores each one
/// independently — so making the literal path the only order-sensitive reading would be the
/// surprise, not the rule. Duplicates are ignored on both sides: `build build` typed twice is one
/// word said twice.
fn label_is_the_query_word_for_word(terms: &[String], words: &[String]) -> bool {
    !words.is_empty()
        && words.iter().all(|word| terms.contains(word))
        && terms.iter().all(|term| words.contains(term))
}

/// Every word of a candidate's OWN searchable text — its name, its secondary line and its long
/// tail — as the term list the fuzzy pass compares a query against at full weight. `human_key` is
/// NOT part of it, for the same reason it is not part of [`literal_verdict`].
fn searchable_terms(candidate: &SearchCandidate) -> Vec<String> {
    let mut terms = idea::tokenize(&candidate.label);
    for text in [candidate.detail.as_deref(), candidate.extract.as_deref()]
        .into_iter()
        .flatten()
    {
        terms.extend(idea::tokenize(text));
    }
    terms
}

/// Every word of a candidate's CONTEXT — the project it belongs to, and the workspace when it has
/// one. Scored at [`idea::CONTEXT_WEIGHT`], i.e. below the entity's own name.
///
/// This is what makes a query like `bui dep auth` answerable at all: `dep` and `auth` have
/// nothing to match on a command called `build` running `bun run build`, and used to drag its score
/// under the admit bar. They match here instead — for less than the name is worth, but for
/// something — and the coverage aggregation in [`idea::fuzzy_score_in_context`] turns that into the
/// row the user meant.
///
/// WHAT IT WIDENS AND WHAT IT DOES NOT. The context never widens a READ — it ranks and admits what
/// the read already returned — but the seven sources do not read alike, so that single rule lands
/// in two very different places:
///
///   * the THREE INDEXED sources (Specs, tasks, ideas) hand `rank_group` what their FTS read
///     returned, so a Spec whose title and prose say nothing about the query is never a candidate,
///     however well its project name matches. Typing a project name does NOT summon its Specs;
///   * the FOUR NON-INDEXED ones (workspaces, projects, command instances, agent sessions) have no
///     index to be narrower than the context: their read is already the whole table bounded at
///     [`CANDIDATE_CAP`]. A match landing on `project_name` / `workspace_name` ALONE therefore does
///     produce a [`Bucket::Match`] ([`literal_verdict`]), and typing a project name does bring up
///     its workspaces, its command instances and its sessions, up to [`CAP_TYPE`] each. That is
///     deliberate rather than tolerated — those rows have no prose of their own, so their container
///     is most of what they can be searched BY — and it is what
///     `tests::the_context_is_searchable_on_every_kind_weighs_less_than_the_name_and_keeps_the_or`
///     requires.
///
/// What bounds the cost of the second bullet is [`GROUP_ORDER`]: the four non-indexed groups are
/// trimmed LAST, so container-only rows fill the tail of the forty instead of evicting an
/// indexed hit.
fn context_terms(candidate: &SearchCandidate) -> Vec<String> {
    let mut terms = idea::tokenize(&candidate.project_name);
    if let Some(workspace) = candidate.workspace_name.as_deref() {
        terms.extend(idea::tokenize(workspace));
    }
    terms
}

/// Turn a row into the wire hit, rounding the score the way `idea_search` rounds its own (a
/// relevance is never meaningful past four decimals, and an exact float leaks the blend's
/// arithmetic into every snapshot that prints it).
fn into_hit(kind: SearchKind, candidate: SearchCandidate, score: f64) -> SearchHit {
    SearchHit {
        kind,
        id: candidate.id,
        project_id: candidate.project_id,
        project_name: candidate.project_name,
        workspace_id: candidate.workspace_id,
        workspace_name: candidate.workspace_name,
        spec_id: candidate.spec_id,
        terminal_id: candidate.terminal_id,
        label: candidate.label,
        detail: candidate.detail,
        extract: candidate.extract,
        human_key: candidate.human_key,
        score: (score * 10_000.0).round() / 10_000.0,
        updated_at: candidate.updated_at,
    }
}

// Tests live in the module they cover.
#[cfg(test)]
mod tests;
