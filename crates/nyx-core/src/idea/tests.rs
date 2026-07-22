//! Unit + integration tests for the IDEAS search/scoring layer (v2.6 Phase 1).

use super::*;
use crate::db;

// --- pure helpers ----------------------------------------------------------

#[test]
fn tokenize_lowercases_and_splits_on_non_alnum() {
    assert_eq!(tokenize("Login-Flow, v2!"), vec!["login", "flow", "v2"]);
    assert!(tokenize("   ,. ").is_empty());
}

#[test]
fn build_fts_query_quotes_prefixes_and_or_joins() {
    assert_eq!(
        build_fts_query("login flow").as_deref(),
        Some("\"login\"* OR \"flow\"*")
    );
    // Punctuation-only → no usable term → None (caller falls back to fuzzy).
    assert_eq!(build_fts_query("  ?? "), None);
    // A double-quote (like any non-alphanumeric char) is a token boundary, so the FTS parse
    // can never be broken by user punctuation — "a\"b" tokenizes into two safe prefix terms.
    assert_eq!(build_fts_query("a\"b").as_deref(), Some("\"a\"* OR \"b\"*"));
}

#[test]
fn trigram_similarity_is_high_for_typos_and_low_for_unrelated() {
    assert_eq!(trigram_similarity("login", "login"), 1.0);
    // A one-letter typo keeps most trigrams → high.
    assert!(
        trigram_similarity("authentication", "authentcation") > 0.7,
        "a one-letter typo stays close"
    );
    // Unrelated words → low.
    assert!(trigram_similarity("login", "elephant") < 0.2);
}

#[test]
fn fuzzy_score_is_term_granular_typo_tolerance() {
    // A typo'd query term still matches its intended idea term regardless of body length.
    let idea_terms = tokenize("Rework the authentication and login flow entirely");
    let q = tokenize("authentcation"); // missing the 'i'
    assert!(
        fuzzy_score(&q, &idea_terms) >= 0.3,
        "a single-letter typo clears the admit threshold"
    );
    // An unrelated query term scores ~0 against the idea.
    assert!(fuzzy_score(&tokenize("banana"), &idea_terms) < 0.3);
    // Empty inputs are safe.
    assert_eq!(fuzzy_score(&[], &idea_terms), 0.0);
}

#[test]
fn make_extract_windows_around_the_match_with_elisions() {
    let body = format!(
        "{}NEEDLE marker in the middle{}",
        "x".repeat(300),
        "y".repeat(300)
    );
    let ex = make_extract("t", &body, &tokenize("needle"));
    assert!(ex.contains("NEEDLE"), "the extract centers on the match");
    assert!(ex.starts_with('…') && ex.ends_with('…'), "cuts are marked");
    assert!(ex.chars().count() <= EXTRACT_LEN + 2, "bounded window");
    // A short body is returned whole (no elision).
    assert_eq!(
        make_extract("t", "short body", &tokenize("body")),
        "short body"
    );
}

// --- search_ideas integration (FTS + fuzzy + linked specs) -----------------

fn seed() -> (diesel::SqliteConnection, String) {
    let mut conn = db::open_in_memory();
    let (project, _ws) = db::create_project(&mut conn, "Proj", "/tmp/idea-search", None).unwrap();
    (conn, project.id)
}

/// The core done-criterion: `search_ideas` returns a top-N SCORED (extract + score + linked
/// Specs), combining an exact FTS match and a fuzzy typo match, ranking the exact one first,
/// and excluding unrelated ideas.
#[test]
fn search_combines_fts_and_fuzzy_scored_with_linked_specs() {
    let (mut conn, project_id) = seed();
    let (spec, _rev) = db::create_spec(
        &mut conn,
        &project_id,
        "Auth Spec",
        db::SpecSections::empty(),
    )
    .unwrap();

    let exact = db::create_idea(
        &mut conn,
        &project_id,
        "Authentication rework",
        "harden the login path against empty credentials",
        Some("auth"),
        db::IDEA_SOURCE_SESSION,
    )
    .unwrap();
    db::link_idea_spec(&mut conn, &exact.id, &spec.id, db::IDEA_KIND_ADDRESSES).unwrap();

    // A DIFFERENT idea that only matches via a TYPO of "authentication" (no FTS hit).
    let fuzzy = db::create_idea(
        &mut conn,
        &project_id,
        "Session token authentcation",
        "the authentcation token expiry is wrong",
        None,
        db::IDEA_SOURCE_SESSION,
    )
    .unwrap();
    // An unrelated idea that must NOT surface.
    db::create_idea(
        &mut conn,
        &project_id,
        "Dark mode toggle",
        "a theme switch",
        None,
        db::IDEA_SOURCE_SESSION,
    )
    .unwrap();

    let results = search_ideas(&mut conn, &project_id, "authentication", None, None, 10).unwrap();

    // The unrelated idea is excluded; both auth ideas surface.
    let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.contains(&exact.id.as_str()),
        "the exact FTS match surfaces"
    );
    assert!(
        ids.contains(&fuzzy.id.as_str()),
        "the typo variant surfaces via fuzzy"
    );
    assert!(!ids.iter().any(|id| results
        .iter()
        .find(|r| r.id == *id)
        .unwrap()
        .title
        .contains("Dark mode")));
    assert_eq!(results.len(), 2, "only the two relevant ideas");

    // The exact match ranks FIRST (a stronger, FTS-backed score) and its linked Spec rides along.
    assert_eq!(
        results[0].id, exact.id,
        "exact match ranks above the fuzzy one"
    );
    assert!(results[0].score > results[1].score, "scores are ordered");
    assert!(results[0].score > 0.0);
    assert_eq!(results[0].linked_specs.len(), 1);
    assert_eq!(results[0].linked_specs[0].spec_id, spec.id);
    assert_eq!(results[0].linked_specs[0].kind, db::IDEA_KIND_ADDRESSES);
    assert_eq!(results[0].linked_specs[0].title, "Auth Spec");
    assert!(
        !results[0].extract.is_empty(),
        "each result carries an extract"
    );
}

/// The theme/status filters NARROW the candidate set, and the FTS hit of another theme does
/// not leak past the filter.
#[test]
fn search_honors_theme_and_status_filters() {
    let (mut conn, project_id) = seed();
    let a = db::create_idea(
        &mut conn,
        &project_id,
        "Login speed",
        "make login faster",
        Some("auth"),
        db::IDEA_SOURCE_SESSION,
    )
    .unwrap();
    let b = db::create_idea(
        &mut conn,
        &project_id,
        "Login page copy",
        "reword the login page",
        Some("ui"),
        db::IDEA_SOURCE_SESSION,
    )
    .unwrap();

    // theme=auth → only idea `a`, even though both match "login".
    let auth = search_ideas(&mut conn, &project_id, "login", Some("auth"), None, 10).unwrap();
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0].id, a.id);

    // status=implemented → nothing until we close one; then only that one.
    assert!(search_ideas(
        &mut conn,
        &project_id,
        "login",
        None,
        Some(db::IDEA_STATUS_IMPLEMENTED),
        10
    )
    .unwrap()
    .is_empty());
    db::close_idea(&mut conn, &b.id, db::IDEA_STATUS_IMPLEMENTED, Some("done")).unwrap();
    let done = search_ideas(
        &mut conn,
        &project_id,
        "login",
        None,
        Some(db::IDEA_STATUS_IMPLEMENTED),
        10,
    )
    .unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].id, b.id);
}

/// The top-N is BOUNDED: `limit` is clamped and only the best N are returned.
#[test]
fn search_bounds_the_top_n() {
    let (mut conn, project_id) = seed();
    for i in 0..5 {
        db::create_idea(
            &mut conn,
            &project_id,
            &format!("search idea {i}"),
            "about searching",
            None,
            db::IDEA_SOURCE_SESSION,
        )
        .unwrap();
    }
    let top2 = search_ideas(&mut conn, &project_id, "search", None, None, 2).unwrap();
    assert_eq!(top2.len(), 2, "limit caps the result set");
    // A limit above the ceiling is clamped, not honored literally.
    let capped = search_ideas(&mut conn, &project_id, "search", None, None, 9_999).unwrap();
    assert!(capped.len() <= MAX_SEARCH_LIMIT);
}
