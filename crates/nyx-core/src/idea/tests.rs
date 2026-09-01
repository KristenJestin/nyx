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
        fuzzy_score(&q, &idea_terms) >= FUZZY_ADMIT_THRESHOLD,
        "a single-letter typo clears FUZZY_ADMIT_THRESHOLD"
    );
    // An unrelated query term scores ~0 against the idea — under the SAME bar, so this guard keeps
    // pinning the property it names whatever the constant becomes.
    assert!(fuzzy_score(&tokenize("banana"), &idea_terms) < FUZZY_ADMIT_THRESHOLD);
    // Empty inputs are safe.
    assert_eq!(fuzzy_score(&[], &idea_terms), 0.0);
}

/// REVIEW-FINDING (« Recherche contextuelle : scoring par couverture ») : the aggregation over a
/// query's terms combines them as EVIDENCE, not as a flat mean — the mean made precision
/// self-defeating.
///
/// The four properties the ranking rests on, asserted on the pure helper so they hold for every
/// caller (`search_ideas` and the global palette alike).
#[test]
fn the_term_aggregation_rewards_coverage_and_never_sinks_on_a_missed_term() {
    let entity = tokenize("build pipeline");

    // 1. A ONE-TERM query keeps its order and its admission: the formula collapses to a UNIFORM
    //    scaling of the similarity, and the admit bar carries the same factor — so a one-word
    //    query admits exactly the set it admitted before.
    let one = tokenize("build");
    let similarity =
        trigram_similarity("build", "build").max(trigram_similarity("build", "pipeline"));
    assert_eq!(fuzzy_score(&one, &entity), CERTAINTY_CEILING * similarity);
    assert_eq!(
        fuzzy_score(&one, &entity) >= FUZZY_ADMIT_THRESHOLD,
        similarity >= 0.3,
        "the bar scales with the scale: the same one-word candidates are admitted"
    );

    // 2. COVERAGE is rewarded: a second term that also lands scores STRICTLY higher than the same
    //    term alone would… (`pipeline` matches too)
    let two = tokenize("build pipeline");
    assert!(
        fuzzy_score(&two, &entity) > fuzzy_score(&tokenize("buil"), &entity),
        "two words that both land beat one that lands partly"
    );
    // …and covering three beats covering two, even when the first term already matches PERFECTLY —
    // the case an evidence-combining rule would have saturated at 1 and tied.
    let target = tokenize("build pipeline nightly");
    assert!(
        fuzzy_score(&tokenize("build pipeline nightly"), &target)
            > fuzzy_score(&tokenize("build pipeline zzzz"), &target),
        "more of the query accounted for ranks strictly better, perfect first term or not"
    );

    // 3. A MISSED term costs EXACTLY NOTHING. The flat mean took `build` from 1.0 to 0.33 on a
    //    three-word query and dropped it under the admit bar; evidence that says nothing has a
    //    factor of 1, so four dead words leave the score where one live word put it.
    let noisy = fuzzy_score(&tokenize("build zzzz yyyy wwww vvvv"), &entity);
    assert_eq!(
        noisy,
        fuzzy_score(&one, &entity),
        "words that find nothing neither add nor subtract"
    );
    assert!(
        noisy >= FUZZY_ADMIT_THRESHOLD,
        "a precise query never argues its own candidate out of the answer"
    );

    // 4. The CONTEXT is worth less than the name, term for term: the same perfect match scores
    //    CONTEXT_WEIGHT when it lands on the container instead of on the entity.
    let query = tokenize("depot");
    let on_name = fuzzy_score_in_context(&query, &tokenize("depot"), &[]);
    let on_context = fuzzy_score_in_context(&query, &tokenize("build"), &tokenize("depot"));
    assert!(
        on_context < on_name,
        "a term landing on the context weighs less than one landing on the name: \
         {on_context} vs {on_name}"
    );
    assert_eq!(on_context, CONTEXT_WEIGHT * on_name);
}

/// Two floats are the same relevance. The properties below are EXACT in ℝ; in `f64` they carry the
/// usual ~1e-16 of rounding, and the wire rounds every score to four decimals anyway, so anything
/// under `1e-12` is the same number as far as any ordering that ships is concerned.
fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

/// REVIEW-FINDINGS `01M1BPT7TNT0ME5E61BVBXDN8X` (spec) + `01M1BPMTS0Y4FX70B4ZSDC9XCR` (standards),
/// the SAME defect seen twice: [`CERTAINTY_CEILING`] promised a scaling that "moves NO ordering",
/// and the promise held only for the FUZZY COMPONENT. The ranking sorts on [`combine`], where the
/// ceiling used to reach the fuzzy half and not the FTS half — so the declared `FTS_WEIGHT :
/// FUZZY_WEIGHT` of 60:40 was applied as 60:32 and the order COULD flip.
///
/// [`combine`] now scales its FTS side by the same ceiling. What this pins is the ALGEBRA rather
/// than a fixture, which is strictly stronger: a one-term blended score is a positive HOMOTHETY of
/// the plain weighted blend of the two RAW signals, and a positive homothety reorders nothing, on
/// any input at all.
#[test]
fn the_ceiling_is_a_homothety_of_the_whole_blend_on_a_one_term_query() {
    let entity = tokenize("build pipeline");
    let one = tokenize("buil");
    let similarity =
        trigram_similarity("buil", "build").max(trigram_similarity("buil", "pipeline"));
    assert!(
        similarity > 0.0 && similarity < 1.0,
        "a PARTIAL similarity, so a scaling of it is observable at all"
    );

    // The whole blend — both halves, not one — carries exactly one factor of CERTAINTY_CEILING.
    for fts_norm in [0.0, 0.1, 0.25, 0.5, 0.6, 1.0] {
        let plain = FTS_WEIGHT * fts_norm + FUZZY_WEIGHT * similarity;
        let blended = combine(fts_norm, fuzzy_score(&one, &entity));
        assert!(
            same(blended, CERTAINTY_CEILING * plain),
            "a one-term score is CERTAINTY_CEILING × the plain blend of the raw signals: \
             {blended} vs {}",
            CERTAINTY_CEILING * plain
        );
    }
    // …and the blend still honours the `[0,1]` the wire promises, at every term count.
    assert!(combine(1.0, fuzzy_score(&tokenize("build"), &entity)) <= 1.0);
    assert!(combine(1.0, fuzzy_score(&tokenize("build pipeline"), &entity)) <= 1.0);

    // THE PAIR THE REVIEW BUILT. Two candidates whose signals CONTRADICT: A is ranked well by bm25
    // but barely resembles the query, B is ranked badly and matches it perfectly. A one-term fuzzy
    // score IS `CERTAINTY_CEILING × similarity`, so that is what goes into `combine`.
    let (a_fts, a_sim) = (0.60_f64, 0.225_f64);
    let (b_fts, b_sim) = (0.10_f64, 1.0_f64);
    let declared = |f: f64, s: f64| FTS_WEIGHT * f + FUZZY_WEIGHT * s;
    assert!(
        declared(b_fts, b_sim) > declared(a_fts, a_sim),
        "the DECLARED weights rank B above A"
    );
    let a = combine(a_fts, CERTAINTY_CEILING * a_sim);
    let b = combine(b_fts, CERTAINTY_CEILING * b_sim);
    assert!(b > a, "and so does the blend that ships: {b} vs {a}");

    // The regression this guards, spelled out: capping ONE side reversed that very pair. If this
    // ever stops failing, `combine` has quietly gone back to weighting the two signals unequally.
    let one_sided = |f: f64, s: f64| FTS_WEIGHT * f + FUZZY_WEIGHT * CERTAINTY_CEILING * s;
    assert!(
        one_sided(a_fts, a_sim) > one_sided(b_fts, b_sim),
        "capping the fuzzy side alone put A first — that is the bug, kept here as the reference"
    );
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

    let results = search_ideas(&mut conn, Some(&project_id), "authentication", None, None, 10).unwrap();

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

/// REVIEW-FINDING `01M1BPT7TNT0ME5E61BVBXDN8X`: the one-term invariance, pinned END TO END on a
/// fixture where the two signals CONTRADICT each other — which is the only place the defect was
/// ever observable.
///
/// The claim under test is the one the ceiling's doc now makes: for a ONE-TERM query, `idea_search`
/// returns exactly the order the DECLARED weights (`FTS_WEIGHT : FUZZY_WEIGHT`) give on the RAW
/// signals — the normalized bm25 and the trigram similarity. The expected order is recomputed here
/// from the real FTS read rather than hard-coded, so the test cannot drift with `bm25`'s arithmetic
/// while still failing the moment the blend re-weights one signal against the other: under the
/// one-sided ceiling the head of this fixture scored `0.7714` where the declared weights say
/// `0.6514`. The ORDER-invariance itself is proved algebraically next to the constant
/// (`the_ceiling_is_a_homothety_of_the_whole_blend_on_a_one_term_query`, which carries the pair
/// that actually inverted) — a fixture can only ever sample it.
#[test]
fn a_one_term_idea_search_orders_exactly_as_the_declared_weights_do() {
    let (mut conn, project_id) = seed();

    // Three candidates the FTS read all returns, built so the two signals cannot agree:
    //   * two rows matched by PREFIX on a long word the query is only the head of — no exact word
    //     anywhere, so a mediocre trigram resemblance — in a short and in a padded document, which
    //     spreads them across bm25;
    //   * one row carrying the query word VERBATIM but drowned in a long body: a PERFECT fuzzy
    //     match that bm25 does not put first.
    // Which of the three bm25 ranks first is left to FTS5 rather than predicted here; what the
    // test requires is only that the bm25 favourite and the fuzzy favourite are not the same row,
    // and that is asserted below.
    let fixture = [
        (
            "authenticationreconciliationsubsystemdaemon",
            "rotate the pool".to_string(),
        ),
        (
            "authenticationreconciliationsubsystemworker",
            format!("rotate the pool {}", "filler prose ".repeat(60)),
        ),
        (
            "Session token authentication",
            format!(
                "the authentication expiry is wrong {}",
                "padding ".repeat(120)
            ),
        ),
    ];
    let seeded: Vec<(String, Vec<String>)> = fixture
        .iter()
        .map(|(title, body)| {
            let idea = db::create_idea(
                &mut conn,
                &project_id,
                title,
                body,
                None,
                db::IDEA_SOURCE_SESSION,
            )
            .unwrap();
            let mut terms = tokenize(title);
            terms.extend(tokenize(body));
            (idea.id, terms)
        })
        .collect();

    // The two RAW signals, read exactly the way `search_ideas` reads them.
    let query = "authentication";
    let query_terms = tokenize(query);
    let hits = db::fts_match_ideas(
        &mut conn,
        Some(&project_id),
        &build_fts_query(query).expect("the query has a usable term"),
        MAX_SEARCH_LIMIT as i64,
    )
    .unwrap();
    assert_eq!(
        hits.len(),
        3,
        "all three are FTS hits, so bm25 has a spread"
    );
    let normalizer = Bm25Norm::of(hits.iter().map(|hit| hit.score));
    let fts_norm: std::collections::HashMap<&str, f64> = hits
        .iter()
        .map(|hit| (hit.id.as_str(), normalizer.of_value(hit.score)))
        .collect();

    // The RAW pair per row: the normalized bm25, and the trigram similarity BEFORE any ceiling —
    // read straight off `best_similarity` rather than divided back out of `fuzzy_score`, so the
    // expectation is built from the signals themselves and not from the formula under test.
    assert_eq!(
        query_terms.len(),
        1,
        "the invariance under test is the ONE-TERM one"
    );
    let signals: Vec<(&str, f64, f64)> = seeded
        .iter()
        .map(|(id, terms)| {
            (
                id.as_str(),
                fts_norm[id.as_str()],
                best_similarity(&query_terms[0], terms),
            )
        })
        .collect();

    // THE FIXTURE REALLY CONTRADICTS ITSELF — asserted, not assumed, so it can never quietly
    // degenerate into a case where both signals agree and the test stops proving anything.
    let best_fts = signals
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("a best bm25");
    let best_fuzzy = signals
        .iter()
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .expect("a best similarity");
    assert_ne!(
        best_fts.0, best_fuzzy.0,
        "the bm25 favourite and the fuzzy favourite must be two DIFFERENT rows"
    );

    // The order the DECLARED weights give on those raw signals.
    let mut expected: Vec<(&str, f64)> = signals
        .iter()
        .map(|(id, fts, similarity)| (*id, FTS_WEIGHT * fts + FUZZY_WEIGHT * similarity))
        .collect();
    expected.sort_by(|a, b| b.1.total_cmp(&a.1));
    for pair in expected.windows(2) {
        assert!(
            pair[0].1 - pair[1].1 > 1e-3,
            "the fixture separates its rows well clear of the wire's 4-decimal rounding"
        );
    }

    let results = search_ideas(&mut conn, Some(&project_id), query, None, None, 10).unwrap();
    assert_eq!(
        results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        expected.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        "a one-term search ranks on the declared weights, not on a blend that quietly \
         discounted one of the two signals"
    );
    // And the SCORES are that same blend, scaled once by the ceiling — the homothety, observed on
    // the wire rather than on the helper.
    for (id, plain) in &expected {
        let hit = results.iter().find(|r| r.id == *id).expect("it is there");
        assert!(
            same(
                hit.score,
                (CERTAINTY_CEILING * plain * 10_000.0).round() / 10_000.0
            ),
            "{id}: {} vs {}",
            hit.score,
            CERTAINTY_CEILING * plain
        );
    }
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
    let auth = search_ideas(&mut conn, Some(&project_id), "login", Some("auth"), None, 10).unwrap();
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0].id, a.id);

    // status=implemented → nothing until we close one; then only that one.
    assert!(search_ideas(
        &mut conn,
        Some(&project_id),
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
        Some(&project_id),
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
    let top2 = search_ideas(&mut conn, Some(&project_id), "search", None, None, 2).unwrap();
    assert_eq!(top2.len(), 2, "limit caps the result set");
    // A limit above the ceiling is clamped, not honored literally.
    let capped = search_ideas(&mut conn, Some(&project_id), "search", None, None, 9_999).unwrap();
    assert!(capped.len() <= MAX_SEARCH_LIMIT);
}
