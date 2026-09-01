//! Tests for the GLOBAL SEARCH aggregator (Spec « Recherche live globale », Phase 1).
//!
//! Everything here runs against MULTI-PROJECT fixtures, because "global" is the property under
//! test: a palette that only ever sees the entered project is the navigation the Spec exists to
//! replace. Timestamps are STAMPED explicitly rather than left to the clock — several entities are
//! created inside the same millisecond, and an ordering test that depends on which one won that
//! race is not a test.

use super::*;
use crate::db;

/// The SHARED fixture of the three product rules the renderer repeats: the group order, the
/// per-type cap and the query floor. Read by this suite AND by
/// `apps/desktop/renderer/src/components/search/search-rules-contract.test.ts` — see
/// `contract/README.md`. Nothing generates it, and neither side builds the other.
const SEARCH_FIXTURES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contract/search-fixtures.json"
));

/// The whole product, twice: two projects, each carrying one entity of every searchable type, all
/// naming the same word so a single query reaches all seven sources. The recency stamps are
/// explicit and DISTINCT so the suggestion order is a fact, not a coincidence.
struct Fixture {
    alpha: Seeded,
    beta: Seeded,
}

/// One project's seven entities.
struct Seeded {
    id: String,
    workspace_id: String,
    spec_id: String,
    task_id: String,
    idea_id: String,
    /// The `managed_commands` TEMPLATE — the definition, never what the palette answers.
    command_id: String,
    /// The `command_instances` row that template materializes in the root workspace: the entity
    /// the `command` kind actually returns, and the id the `?command=` deep-link matches.
    command_instance_id: String,
    session_id: String,
    terminal_id: String,
}

/// Stamp a row's recency key so an ordering assertion is deterministic. `column` is the recency
/// key of that table — `updated_at` everywhere except `agent_sessions`.
fn stamp(conn: &mut SqliteConnection, table: &str, column: &str, id: &str, at: i64) {
    use diesel::sql_types::{BigInt, Text};
    use diesel::RunQueryDsl;
    diesel::sql_query(format!("UPDATE {table} SET {column} = ? WHERE id = ?"))
        .bind::<BigInt, _>(at)
        .bind::<Text, _>(id)
        .execute(conn)
        .expect("stamp the recency key");
}

/// Rename a row WITHOUT touching its recency key — the only way to test the ordering BUCKETS
/// (exact before prefix) independently of recency, since every rename accessor bumps `updated_at`
/// to the wall clock and would hand the renamed row both advantages at once.
fn rename(conn: &mut SqliteConnection, table: &str, id: &str, name: &str) {
    use diesel::sql_types::Text;
    use diesel::RunQueryDsl;
    diesel::sql_query(format!("UPDATE {table} SET name = ? WHERE id = ?"))
        .bind::<Text, _>(name)
        .bind::<Text, _>(id)
        .execute(conn)
        .expect("rename without stamping");
}

/// Create a managed command in `project`, stamp its recency at `at`, and return the id of the
/// INSTANCE it materializes in `workspace` — the row the `command` kind actually answers (a
/// template id matches nothing the palette can open).
fn command_instance(
    conn: &mut SqliteConnection,
    project: &str,
    workspace: &str,
    name: &str,
    at: i64,
) -> String {
    let template = db::create_template(
        conn,
        project,
        name,
        "bun run x",
        None,
        db::CommandSource::default(),
    )
    .expect("create the managed command");
    stamp(conn, "managed_commands", "updated_at", &template.id, at);
    db::list_instances_for_workspace(conn, workspace)
        .expect("list the workspace's command instances")
        .into_iter()
        .find(|instance| instance.command_id == template.id)
        .expect("the template is materialized in the workspace")
        .id
}

/// SOFT-DELETE a Spec — the archival every searchable read of a Spec excludes on. `db` exposes no
/// accessor for it (the product archives through its own surfaces), so the column is written here.
fn archive_spec(conn: &mut SqliteConnection, id: &str) {
    use diesel::sql_types::Text;
    use diesel::RunQueryDsl;
    diesel::sql_query("UPDATE specs SET archived_at = 1 WHERE id = ?")
        .bind::<Text, _>(id)
        .execute(conn)
        .expect("archive the Spec");
}

/// Seed ONE project with one entity of each type, every label carrying `word`.
fn seed_project(conn: &mut SqliteConnection, name: &str, word: &str, base: i64) -> Seeded {
    let (project, root) = db::create_project(
        conn,
        &format!("{word} {name}"),
        &format!("/tmp/gs/{name}"),
        None,
    )
    .expect("create the project and its root workspace");

    let (spec, _rev) = db::create_spec(
        conn,
        &project.id,
        &format!("{word} spec"),
        db::SpecSections {
            problem: format!("the {word} problem"),
            ..db::SpecSections::empty()
        },
    )
    .expect("create the Spec");

    let task = db::create_task(
        conn,
        &project.id,
        &format!("{word} task"),
        db::NewTaskInput {
            spec_id: Some(spec.id.clone()),
            ..db::NewTaskInput::slice()
        },
    )
    .expect("create the task");

    let idea = db::create_idea(
        conn,
        &project.id,
        &format!("{word} idea"),
        "a body",
        None,
        "manual",
    )
    .expect("create the idea");

    let command = db::create_template(
        conn,
        &project.id,
        &format!("{word} command"),
        &format!("bun run {word}"),
        None,
        db::CommandSource::default(),
    )
    .expect("create the managed command");

    let terminal = db::create_terminal(
        conn,
        &format!("/tmp/gs/{name}/work"),
        Some(format!("{word} terminal")),
    )
    .expect("create the terminal");

    let session = db::record_session_start(
        conn,
        &terminal.id,
        "claude_code",
        db::SessionCapture {
            workspace_id: Some(root.id.clone()),
            external_session_id: format!("ext-{name}"),
            cwd: format!("/tmp/gs/{name}/work"),
            transcript_path: None,
            metadata_json: None,
        },
    )
    .expect("record the agent session");

    // The workspace is renamed so its own label carries the word too (the root workspace is
    // created with the project's own name).
    db::rename_workspace(conn, &root.id, &format!("{word} workspace")).expect("rename the root");

    // Distinct, explicit recency stamps: one per source, oldest first in GROUP_ORDER.
    stamp(conn, "projects", "updated_at", &project.id, base + 1);
    stamp(conn, "specs", "updated_at", &spec.id, base + 2);
    stamp(conn, "tasks", "updated_at", &task.id, base + 3);
    stamp(conn, "ideas", "updated_at", &idea.id, base + 4);
    stamp(conn, "workspaces", "updated_at", &root.id, base + 5);
    stamp(
        conn,
        "managed_commands",
        "updated_at",
        &command.id,
        base + 6,
    );
    stamp(
        conn,
        "agent_sessions",
        "last_seen_at",
        &session.id,
        base + 7,
    );

    // `create_template` materializes ONE instance per existing workspace — here, the root. That
    // instance, not the template, is what the `command` kind answers.
    let command_instance_id = db::list_instances_for_workspace(conn, &root.id)
        .expect("list the root workspace's command instances")
        .into_iter()
        .find(|instance| instance.command_id == command.id)
        .expect("the template is materialized in the root workspace")
        .id;

    Seeded {
        id: project.id,
        workspace_id: root.id,
        spec_id: spec.id,
        task_id: task.id,
        idea_id: idea.id,
        command_id: command.id,
        command_instance_id,
        session_id: session.id,
        terminal_id: terminal.id,
    }
}

/// Two projects, both fully seeded, the second more recent than the first.
fn fixture(conn: &mut SqliteConnection) -> Fixture {
    Fixture {
        alpha: seed_project(conn, "alpha", "pelican", 1_000_000),
        beta: seed_project(conn, "beta", "pelican", 2_000_000),
    }
}

/// The hits of one kind, in order.
fn of_kind(hits: &[SearchHit], kind: SearchKind) -> Vec<&SearchHit> {
    hits.iter().filter(|hit| hit.kind == kind).collect()
}

/// An answer as a READABLE list — kind, label, context, score — for the failure message of an
/// ordering assertion. A `Vec<SearchHit>` printed raw is unreadable, and an ordering test that
/// fails without saying what the order WAS costs a debugging session.
fn labels(hits: &[SearchHit]) -> Vec<String> {
    hits.iter()
        .map(|hit| {
            format!(
                "{:?} {:?} in {:?}/{:?} @{}",
                hit.kind, hit.label, hit.project_name, hit.workspace_name, hit.score
            )
        })
        .collect()
}

/// The HITS half of an answer — what every ranking, ordering and cap assertion here is about. The
/// other half (`knowledge_empty`) is a fact about the install, and has its own test.
fn hits_of(conn: &mut SqliteConnection, query: &str, limits: SearchLimits) -> Vec<SearchHit> {
    global_search(conn, query, limits)
        .expect("the search answers")
        .hits
}

/// DONE-CRITERION ("Chacun des sept kinds produit un hit navigable sur des fixtures
/// multi-projets"): one query reaches all seven sources, in BOTH projects, and every hit carries
/// what the renderer needs to get there — the owning project, plus the container the kind is
/// reached through.
#[test]
fn each_of_the_seven_kinds_produces_a_navigable_hit_across_projects() {
    let mut conn = db::open_in_memory();
    let fixture = fixture(&mut conn);

    let hits = hits_of(&mut conn, "pelican", SearchLimits::default());

    for kind in GROUP_ORDER {
        let found = of_kind(&hits, kind);
        assert_eq!(
            found.len(),
            2,
            "{kind:?} answers once per project, got {found:#?}"
        );
        let projects: Vec<&str> = found.iter().map(|hit| hit.project_id.as_str()).collect();
        assert!(
            projects.contains(&fixture.alpha.id.as_str())
                && projects.contains(&fixture.beta.id.as_str()),
            "{kind:?} spans BOTH projects, got {projects:?}"
        );
        for hit in &found {
            assert!(
                !hit.project_id.is_empty(),
                "{kind:?} carries the project to activate"
            );
            assert!(!hit.label.is_empty(), "{kind:?} carries a label to draw");
        }
    }

    // Each kind points at the entity the palette opens — and, when the kind has no surface of its
    // own, at the container it is reached through.
    let spec = of_kind(&hits, SearchKind::Spec);
    assert!(spec.iter().any(|hit| hit.id == fixture.alpha.spec_id));
    let task = of_kind(&hits, SearchKind::Task);
    let alpha_task = task
        .iter()
        .find(|hit| hit.id == fixture.alpha.task_id)
        .expect("the alpha task answers");
    assert_eq!(
        alpha_task.spec_id.as_deref(),
        Some(fixture.alpha.spec_id.as_str()),
        "a task carries its Spec — v1 opens the Spec, there is no task route"
    );
    assert!(of_kind(&hits, SearchKind::Idea)
        .iter()
        .any(|hit| hit.id == fixture.alpha.idea_id));
    let workspace = of_kind(&hits, SearchKind::Workspace);
    let alpha_workspace = workspace
        .iter()
        .find(|hit| hit.id == fixture.alpha.workspace_id)
        .expect("the alpha workspace answers");
    assert_eq!(
        alpha_workspace.workspace_id.as_deref(),
        Some(fixture.alpha.workspace_id.as_str())
    );
    assert!(of_kind(&hits, SearchKind::Project)
        .iter()
        .any(|hit| hit.id == fixture.alpha.id));
    // REVIEW-FINDING (« Une ligne par instance de commande gérée, portant l'id d'instance ») : the
    // navigable entity is the INSTANCE. The id asserted here moved from `command_id` (the template)
    // to `command_instance_id` because a template id matches no `WorkspaceRuntimeService` and the
    // `?command=` jump opened nothing.
    let alpha_command = of_kind(&hits, SearchKind::Command)
        .into_iter()
        .find(|hit| hit.id == fixture.alpha.command_instance_id)
        .expect("the alpha command answers with its INSTANCE id");
    assert_eq!(
        alpha_command.workspace_id.as_deref(),
        Some(fixture.alpha.workspace_id.as_str()),
        "a command instance carries the workspace it actually runs in"
    );
    assert!(
        of_kind(&hits, SearchKind::Command)
            .iter()
            .all(|hit| hit.id != fixture.alpha.command_id),
        "the TEMPLATE id is never answered — it opens nothing"
    );
    let alpha_session = of_kind(&hits, SearchKind::Session)
        .into_iter()
        .find(|hit| hit.id == fixture.alpha.session_id)
        .expect("the alpha session answers");
    assert_eq!(
        alpha_session.terminal_id.as_deref(),
        Some(fixture.alpha.terminal_id.as_str()),
        "a session is reached through its terminal"
    );
}

/// DONE-CRITERION ("Une session sans workspace n'apparaît jamais ; une session avec workspace
/// porte le `project_id` dérivé de son workspace"). `agent_sessions` carries no `project_id` of
/// its own: without a workspace there is nothing to derive one from, hence no project to activate,
/// hence no navigable target — so the row is excluded at the source rather than surfaced dead.
#[test]
fn a_session_without_a_workspace_is_excluded_and_one_with_it_derives_its_project() {
    let mut conn = db::open_in_memory();
    let fixture = fixture(&mut conn);

    // A second session on a fresh terminal, deliberately UNBOUND to any workspace.
    let orphan_terminal = db::create_terminal(
        &mut conn,
        "/tmp/gs/orphan",
        Some("pelican orphan".to_string()),
    )
    .expect("terminal");
    let orphan = db::record_session_start(
        &mut conn,
        &orphan_terminal.id,
        "claude_code",
        db::SessionCapture {
            workspace_id: None,
            external_session_id: "ext-orphan".into(),
            cwd: "/tmp/gs/orphan".into(),
            transcript_path: None,
            metadata_json: None,
        },
    )
    .expect("record the orphan session");

    let hits = hits_of(&mut conn, "pelican", SearchLimits::default());
    let sessions = of_kind(&hits, SearchKind::Session);
    assert!(
        sessions.iter().all(|hit| hit.id != orphan.id),
        "a session with no workspace has no project to activate and never answers"
    );

    let alpha_session = sessions
        .iter()
        .find(|hit| hit.id == fixture.alpha.session_id)
        .expect("the bound session answers");
    assert_eq!(
        alpha_session.project_id, fixture.alpha.id,
        "the project is DERIVED through workspace_id → workspaces.project_id"
    );
    assert_eq!(
        alpha_session.workspace_id.as_deref(),
        Some(fixture.alpha.workspace_id.as_str())
    );

    // The same exclusion holds for the empty-query suggestions — one path, one rule.
    let suggestions = hits_of(&mut conn, "", SearchLimits::default());
    assert!(suggestions.iter().all(|hit| hit.id != orphan.id));
}

/// DONE-CRITERION ("L'ordre des groupes, l'ordre intra-groupe et les deux caps sont déterministes
/// et reproductibles sur les mêmes fixtures"), read through the review finding that turned the
/// results into ONE list (`01M1C3V7Z2P3KZPDY8YT4M06S3`): the group order is no longer a display
/// order, so what has to be deterministic is the single ranked list — and the two caps, one of
/// which still walks [`GROUP_ORDER`] to decide what survives.
#[test]
fn the_single_list_order_the_intra_type_order_and_both_caps_are_deterministic() {
    let mut conn = db::open_in_memory();
    let fixture = fixture(&mut conn);

    let hits = hits_of(&mut conn, "pelican", SearchLimits::default());

    // ONE LIST, ranked without regard to the type. Every label of this fixture STARTS with the
    // query, so all fourteen hits share the `Prefix` bucket and the reading order is the score's,
    // descending — where the old fixed sequence of sections would have drawn seven blocks.
    let scores: Vec<f64> = hits.iter().map(|hit| hit.score).collect();
    let mut descending = scores.clone();
    descending.sort_unstable_by(|a, b| b.partial_cmp(a).expect("no NaN in a normalized score"));
    assert_eq!(
        scores, descending,
        "inside one bucket the whole answer descends by relevance, types confounded"
    );

    // REPRODUCIBLE: the same fixtures give byte-identical results, twice.
    let again = hits_of(&mut conn, "pelican", SearchLimits::default());
    assert_eq!(hits, again, "the same fixtures rank the same way");

    // INTRA-TYPE: the more recent of two equally-matching entities comes first. The `beta`
    // project was stamped later than `alpha` throughout the fixture.
    let projects = of_kind(&hits, SearchKind::Project);
    assert_eq!(
        projects
            .iter()
            .map(|hit| hit.id.as_str())
            .collect::<Vec<_>>(),
        vec![fixture.beta.id.as_str(), fixture.alpha.id.as_str()],
        "recency breaks the tie between two identical matches"
    );

    // CAP PER TYPE: ten more matching projects, still eight project hits.
    for n in 0..10 {
        db::create_project(
            &mut conn,
            &format!("pelican {n}"),
            &format!("/tmp/gs/p{n}"),
            None,
        )
        .expect("project");
    }
    let capped = hits_of(&mut conn, "pelican", SearchLimits::default());
    assert_eq!(
        of_kind(&capped, SearchKind::Project).len(),
        CAP_TYPE,
        "no type ever returns more than CAP_TYPE"
    );

    // CAP OVERALL: the total is bounded, and the trim still follows the group order — so WHICH
    // hits survive a saturated answer is a property of that order, not of the iteration. Compared
    // as a multiset, because which of the three the user reads first is now the ranking's answer
    // and no longer the trim's.
    let narrowed = hits_of(&mut conn, "pelican", SearchLimits::with_total(3));
    let mut surviving: Vec<SearchKind> = narrowed.iter().map(|hit| hit.kind).collect();
    surviving.sort();
    assert_eq!(
        surviving,
        vec![SearchKind::Spec, SearchKind::Spec, SearchKind::Task],
        "the overall trim consumes the groups in order — the two Specs, then a task"
    );
    // A caller cannot widen the ceilings.
    let absurd = hits_of(
        &mut conn,
        "pelican",
        SearchLimits {
            per_type: 999,
            total: 999,
        },
    );
    assert!(absurd.len() <= CAP_TOTAL);
    assert!(of_kind(&absurd, SearchKind::Project).len() <= CAP_TYPE);
}

/// The ORDER OF THE BUCKETS, on its own fixture: exact label, then prefix, then everything a
/// relevance score can say. This is the ranking rule a palette may never get wrong — a fuzzy tail
/// promoted above the entity literally named what the user typed makes the first row untrustworthy,
/// and an untrustworthy first row means the user reads the list instead of pressing `↵`.
///
/// The trap the fixture sets: the exact match is the OLDEST and the fuzzy one the NEWEST, so
/// recency — the tie-break — actively pushes the wrong way. Only the buckets can produce the
/// expected order.
#[test]
fn an_exact_label_outranks_a_prefix_which_outranks_a_fuzzy_variant() {
    let mut conn = db::open_in_memory();
    let exact = seed_project(&mut conn, "one", "pelican", 1_000_000);
    let prefix = seed_project(&mut conn, "two", "pelican", 2_000_000);
    let fuzzy = seed_project(&mut conn, "three", "pelican", 3_000_000);

    // Renamed WITHOUT stamping, so the recency order set above survives: `exact` stays the oldest.
    rename(&mut conn, "projects", &exact.id, "pelican");
    rename(&mut conn, "projects", &prefix.id, "pelican two");
    rename(&mut conn, "projects", &fuzzy.id, "pelicen three");

    let hits = hits_of(&mut conn, "pelican", SearchLimits::default());
    let projects: Vec<&str> = of_kind(&hits, SearchKind::Project)
        .iter()
        .map(|hit| hit.id.as_str())
        .collect();
    assert_eq!(
        projects,
        vec![exact.id.as_str(), prefix.id.as_str(), fuzzy.id.as_str()],
        "exact > prefix > fuzzy, even though recency ranks them in the exact opposite order"
    );
}

/// REVIEW-FINDING (« pendant une recherche je vois pas pourquoi une certaine section devrait être
/// plus mise en avant qu'une autre, pour moi ça doit être mélangé ») : the buckets order the answer
/// ACROSS the types, not only inside one.
///
/// The fixture is the exact case the old fixed sequence got wrong, and it stacks BOTH former
/// advantages on the losing row: the Spec was drawn in the first section AND is the more recent of
/// the two, while the command instance merely happens to be named exactly what was typed. Under
/// `Specs → … → Managed commands` the Spec came first whatever the query; under one ranked list an
/// `Exact` outranks a `Match` whatever the type carrying it.
///
/// This is also the reason the bucket, and not the score, leads [`by_rank`]: the two rows are
/// scored by two different machineries (a normalized bm25 over `specs_fts`, a fuzzy pass over a
/// command name), so their numbers share a scale without sharing a meaning. Their BUCKETS are
/// comparable by construction, and that is what the assertion below rests on.
#[test]
fn an_exact_match_of_one_type_outranks_a_partial_match_of_another() {
    let mut conn = db::open_in_memory();
    let (project, root) =
        db::create_project(&mut conn, "pipelines", "/tmp/gs/pipelines", None).expect("project");

    // A Spec that MENTIONS the query without being named it — `Bucket::Match`.
    let (spec, _rev) = db::create_spec(
        &mut conn,
        &project.id,
        "rollout deploy plan",
        db::SpecSections {
            problem: "shipping is manual".to_string(),
            ..db::SpecSections::empty()
        },
    )
    .expect("spec");

    // A managed command named EXACTLY the query — `Bucket::Exact`, through the instance the
    // template materializes in the root workspace.
    let command = db::create_template(
        &mut conn,
        &project.id,
        "deploy",
        "bun run deploy",
        None,
        db::CommandSource::default(),
    )
    .expect("command");
    let instance = db::list_instances_for_workspace(&mut conn, &root.id)
        .expect("instances")
        .into_iter()
        .find(|instance| instance.command_id == command.id)
        .expect("the template is materialized in the root workspace")
        .id;

    // The trap: RECENCY pushes the wrong way too — the Spec is the newer row.
    stamp(
        &mut conn,
        "managed_commands",
        "updated_at",
        &command.id,
        1_000,
    );
    stamp(&mut conn, "specs", "updated_at", &spec.id, 2_000);

    let hits = hits_of(&mut conn, "deploy", SearchLimits::default());
    let order: Vec<&str> = hits.iter().map(|hit| hit.id.as_str()).collect();
    assert!(
        order.contains(&instance.as_str()) && order.contains(&spec.id.as_str()),
        "both rows are answered: {order:?}"
    );
    assert_eq!(
        order.first(),
        Some(&instance.as_str()),
        "the command literally named `deploy` leads — the Spec's section no longer does it for it"
    );
    let spec_at = order
        .iter()
        .position(|id| *id == spec.id)
        .expect("the Spec is in the answer");
    let command_at = order
        .iter()
        .position(|id| *id == instance)
        .expect("the command instance is in the answer");
    assert!(
        command_at < spec_at,
        "Exact before Match, across types: {order:?}"
    );
}

/// DONE-CRITERION ("Aucune URL ni fragment de route n'apparaît dans le type de retour"). Asserted
/// on the SERIALIZED shape — the thing the renderer actually receives — because a route could only
/// arrive there through a field, and the field set is what this pins.
#[test]
fn no_url_or_route_fragment_appears_in_the_returned_shape() {
    let mut conn = db::open_in_memory();
    fixture(&mut conn);
    let answer = global_search(&mut conn, "pelican", SearchLimits::default()).expect("search");
    let hits = answer.hits;
    assert!(!hits.is_empty());

    // The ENVELOPE first: the answer is the hits plus the one fact about the install they cannot
    // carry, and nothing else.
    let envelope = serde_json::to_value(SearchAnswer {
        hits: hits.clone(),
        knowledge_empty: answer.knowledge_empty,
    })
    .expect("serialize the answer");
    let mut envelope_keys: Vec<&str> = envelope
        .as_object()
        .expect("the answer is an object")
        .keys()
        .map(String::as_str)
        .collect();
    envelope_keys.sort_unstable();
    assert_eq!(envelope_keys, vec!["hits", "knowledge_empty"]);

    let json = serde_json::to_value(&hits).expect("serialize the hits");
    let rows = json.as_array().expect("an array of hits");
    for row in rows {
        let object = row.as_object().expect("a hit is an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "detail",
                "extract",
                "human_key",
                "id",
                "kind",
                "label",
                "project_id",
                // REVIEW-FINDING (« Contexte projet + workspace porté par le hit ») : the two
                // container NAMES joined at the source. They are new keys, and the shape assertion
                // grew by exactly them — the renderer no longer rebuilds the project name from the
                // rail's tree, and the workspace name did not exist anywhere.
                "project_name",
                "score",
                "spec_id",
                "terminal_id",
                "updated_at",
                "workspace_id",
                "workspace_name",
            ],
            "the hit shape is identity + context + display, and nothing else"
        );
        for forbidden in ["url", "href", "path", "route", "to", "target"] {
            assert!(
                !object.contains_key(forbidden),
                "`{forbidden}` would make the backend own the routing table"
            );
        }
    }
}

/// DONE-CRITERION ("Le scoring des idées n'est pas dupliqué : chemin projet et chemin global
/// partagent les helpers"). Pinned by RESULT, not by inspection: the global path's score for an
/// idea is exactly what the shared helpers produce — `combine` over the shared bm25 normalization
/// and the shared fuzzy pass — so a second, divergent scoring for the global path fails here.
#[test]
fn the_idea_scoring_is_the_shared_one_not_a_second_implementation() {
    let mut conn = db::open_in_memory();
    let (project, _root) =
        db::create_project(&mut conn, "solo", "/tmp/gs/solo", None).expect("project");
    let idea = db::create_idea(
        &mut conn,
        &project.id,
        "authentication passkeys",
        "a body",
        None,
        "manual",
    )
    .expect("idea");

    let hits = hits_of(&mut conn, "passkeys", SearchLimits::default());
    let hit = hits
        .iter()
        .find(|hit| hit.id == idea.id)
        .expect("the idea answers");

    // A single FTS hit normalizes to 1.0 (no spread), and the fuzzy component is the shared
    // term-granular pass over the idea's own words.
    let terms = crate::idea::tokenize("passkeys");
    let idea_terms = crate::idea::tokenize("authentication passkeys");
    let expected = crate::idea::combine(1.0, crate::idea::fuzzy_score(&terms, &idea_terms));
    assert_eq!(
        hit.score,
        (expected * 10_000.0).round() / 10_000.0,
        "the global path scores an idea with the SAME helpers the per-project search uses"
    );

    // And the per-project search still answers over the very same read.
    let scoped = crate::idea::search_ideas(&mut conn, Some(&project.id), "passkeys", None, None, 10)
        .expect("the per-project search still works");
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, idea.id);
}

/// DONE-CRITERION ("Une palette ouverte sans saisie rend un top N par `updated_at` traversant les
/// projets, ordonné de façon déterministe sur fixtures ; une requête d'un caractère rend une
/// réponse vide").
#[test]
fn an_empty_query_suggests_by_recency_across_projects_and_one_character_answers_nothing() {
    let mut conn = db::open_in_memory();
    let fixture = fixture(&mut conn);

    let suggestions = hits_of(&mut conn, "", SearchLimits::default());
    assert_eq!(
        suggestions.len(),
        CAP_TYPE,
        "the empty query fills exactly one un-scrolled screen"
    );

    // Ordered by recency alone — the fixture stamped `beta` entirely above `alpha`, and inside a
    // project the sources ascend in GROUP_ORDER, so the newest seven are all `beta`'s.
    let recencies: Vec<i64> = suggestions.iter().map(|hit| hit.updated_at).collect();
    let mut sorted = recencies.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(recencies, sorted, "suggestions descend by recency");
    assert_eq!(
        suggestions
            .iter()
            .filter(|hit| hit.project_id == fixture.beta.id)
            .count(),
        7,
        "all seven of the more recent project's entities come first"
    );
    assert!(
        suggestions
            .iter()
            .any(|hit| hit.project_id == fixture.alpha.id),
        "and the list crosses into the other project — it is not scoped to one"
    );
    // Every type is representable, and the session's recency came from `last_seen_at` (the column
    // `agent_sessions` actually has).
    let session = suggestions
        .iter()
        .find(|hit| hit.kind == SearchKind::Session)
        .expect("a session is suggestible");
    assert_eq!(
        session.updated_at, 2_000_007,
        "a session's recency is its last_seen_at"
    );
    // Nothing is scored: a suggestion is an ordering, not a relevance.
    assert!(suggestions.iter().all(|hit| hit.score == 0.0));

    // Blanks only are still "no query".
    assert_eq!(
        hits_of(&mut conn, "   ", SearchLimits::default()),
        suggestions
    );
    // Reproducible on the same fixtures.
    assert_eq!(hits_of(&mut conn, "", SearchLimits::default()), suggestions);
    // The caller's overall cap narrows the suggestions too.
    assert_eq!(hits_of(&mut conn, "", SearchLimits::with_total(3)).len(), 3);

    // A ONE-character query is neither: it is an empty answer, and it is NOT the suggestions.
    let single = hits_of(&mut conn, "p", SearchLimits::default());
    assert!(
        single.is_empty(),
        "one character is not a search — it would match nearly everything"
    );
    assert_eq!(
        hits_of(&mut conn, " p ", SearchLimits::default()),
        Vec::<SearchHit>::new(),
        "the length is measured on the TRIMMED query"
    );
}

/// A query whose every character is punctuation has no usable FTS term. The indexed sources then
/// have nothing to match, and the answer degrades to whatever the literal/fuzzy pass finds rather
/// than erroring inside FTS5 — the same degradation `idea_search` takes.
#[test]
fn a_termless_query_degrades_instead_of_breaking() {
    let mut conn = db::open_in_memory();
    fixture(&mut conn);
    let hits = hits_of(&mut conn, "?? ??", SearchLimits::default());
    assert!(hits.len() <= CAP_TOTAL);
}

/// REVIEW-FINDING (« Une ligne par instance de commande gérée, portant l'id d'instance ») : ONE
/// template materialized in TWO workspaces answers TWO rows, each carrying its own
/// `command_instances.id` and its own `workspace_id`.
///
/// Both halves are load-bearing. The id is the bug: `?command=` is compared against
/// `WorkspaceRuntimeService.id`, which IS `command_instances.id`, so a template id matched nothing
/// and the jump opened no pane at all. The multiplicity is the product answer to the ambiguity that
/// creates: rather than the palette guessing which `build` was meant, it draws both and lets the
/// workspace on the row decide — no sub-menu, no drill-down.
///
/// The cap is asserted in the same place because it is what the multiplicity could have broken:
/// nine instances of one template must still yield eight rows, exactly like nine distinct entities.
#[test]
fn one_template_in_two_workspaces_answers_two_rows_each_carrying_its_instance_and_workspace() {
    let mut conn = db::open_in_memory();
    let (project, root) =
        db::create_project(&mut conn, "quetzal", "/tmp/gs/quetzal", None).expect("project");
    let template = db::create_template(
        &mut conn,
        &project.id,
        "quetzal build",
        "bun run build",
        None,
        db::CommandSource::default(),
    )
    .expect("create the template");
    // A SECOND workspace materializes a second instance of the same template.
    let second = db::create_workspace(&mut conn, &project.id, "feature", "/tmp/gs/quetzal/feature")
        .expect("create the second workspace");

    let instances: Vec<String> = [root.id.as_str(), second.id.as_str()]
        .into_iter()
        .map(|workspace_id| {
            db::list_instances_for_workspace(&mut conn, workspace_id)
                .expect("list the instances")
                .into_iter()
                .find(|instance| instance.command_id == template.id)
                .expect("the template is materialized in this workspace")
                .id
        })
        .collect();

    let hits = hits_of(&mut conn, "quetzal build", SearchLimits::default());
    let commands = of_kind(&hits, SearchKind::Command);
    assert_eq!(
        commands.len(),
        2,
        "one template, two workspaces, two rows — got {:?}",
        commands
            .iter()
            .map(|hit| (&hit.id, &hit.workspace_id))
            .collect::<Vec<_>>()
    );

    // Each row IS an instance, and names the workspace it runs in.
    let mut answered: Vec<&str> = commands.iter().map(|hit| hit.id.as_str()).collect();
    answered.sort_unstable();
    let mut expected: Vec<&str> = instances.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(
        answered, expected,
        "the ids answered are the INSTANCE ids — a template id matches no runtime service"
    );
    let mut workspaces: Vec<&str> = commands
        .iter()
        .filter_map(|hit| hit.workspace_id.as_deref())
        .collect();
    workspaces.sort_unstable();
    let mut both = vec![root.id.as_str(), second.id.as_str()];
    both.sort_unstable();
    assert_eq!(
        workspaces, both,
        "the two rows are told apart by the workspace each one carries"
    );
    for hit in &commands {
        assert_eq!(hit.project_id, project.id);
        assert_eq!(
            hit.label, "quetzal build",
            "both rows draw the template name"
        );
    }

    // The SUGGESTIONS path draws the same rows — one read, one shape.
    let suggestions = hits_of(&mut conn, "", SearchLimits::default());
    assert_eq!(of_kind(&suggestions, SearchKind::Command).len(), 2);

    // THE CAP still holds when ONE template produces many rows: seven more workspaces bring the
    // template to nine instances, and the group is still trimmed to CAP_TYPE.
    for n in 0..7 {
        db::create_workspace(
            &mut conn,
            &project.id,
            &format!("ws{n}"),
            &format!("/tmp/gs/quetzal/ws{n}"),
        )
        .expect("create another workspace");
    }
    let capped = hits_of(&mut conn, "quetzal build", SearchLimits::default());
    assert_eq!(
        of_kind(&capped, SearchKind::Command).len(),
        CAP_TYPE,
        "nine instances of one template are still capped at CAP_TYPE rows"
    );

    // An ARCHIVED workspace drops its instance: the Terminals page never lists that workspace's
    // runtime, so the row would deep-link at a service that is not there — the dead jump again.
    db::archive_workspace(&mut conn, &second.id).expect("archive the second workspace");
    let after = hits_of(&mut conn, "quetzal build", SearchLimits::default());
    assert!(
        of_kind(&after, SearchKind::Command)
            .iter()
            .all(|hit| hit.workspace_id.as_deref() != Some(second.id.as_str())),
        "an archived workspace's instance is not navigable and does not answer"
    );
}

/// REVIEW-FINDING (« Contexte projet + workspace porté par le hit ») : every hit carries the NAME
/// of its project, and the name of its workspace exactly where it carries a `workspace_id` — joined
/// at the source, on both the ranked path and the suggestions path, for all seven kinds.
///
/// The pair rule is asserted as a PAIR (`workspace_name.is_some() == workspace_id.is_some()`)
/// rather than kind by kind, because that is the whole contract: an id without its name is an
/// opaque row, a name without its id is context pointing nowhere.
#[test]
fn every_hit_names_its_project_and_names_its_workspace_wherever_it_has_one() {
    let mut conn = db::open_in_memory();
    let fixture = fixture(&mut conn);

    for (query, path) in [("pelican", "ranked"), ("", "suggestions")] {
        let hits = hits_of(&mut conn, query, SearchLimits::default());
        assert!(!hits.is_empty(), "the {path} path answers");
        for hit in &hits {
            let expected = if hit.project_id == fixture.alpha.id {
                "pelican alpha"
            } else {
                "pelican beta"
            };
            assert_eq!(
                hit.project_name, expected,
                "{:?} names its project on the {path} path",
                hit.kind
            );
            assert_eq!(
                hit.workspace_name.is_some(),
                hit.workspace_id.is_some(),
                "{:?} carries the workspace NAME exactly where it carries the id — got {:?} / {:?}",
                hit.kind,
                hit.workspace_id,
                hit.workspace_name
            );
            if let Some(name) = &hit.workspace_name {
                assert_eq!(name, "pelican workspace");
            }
        }
    }

    // The three kinds that LIVE in a workspace carry one; the ideas and the projects do not.
    let hits = hits_of(&mut conn, "pelican", SearchLimits::default());
    for kind in [
        SearchKind::Workspace,
        SearchKind::Command,
        SearchKind::Session,
    ] {
        assert!(
            of_kind(&hits, kind)
                .iter()
                .all(|hit| hit.workspace_name.is_some()),
            "{kind:?} always lives in a workspace"
        );
    }
    for kind in [SearchKind::Idea, SearchKind::Project] {
        assert!(
            of_kind(&hits, kind)
                .iter()
                .all(|hit| hit.workspace_name.is_none()),
            "{kind:?} lives in a project and in nothing narrower"
        );
    }

    // TWO WORKSPACES OF ONE PROJECT, the case the project name alone cannot depart: the two rows
    // share every other field and are told apart by the workspace name.
    let second = db::create_workspace(
        &mut conn,
        &fixture.alpha.id,
        "pelican feature",
        "/tmp/gs/alpha/feature",
    )
    .expect("a second workspace");
    let hits = hits_of(&mut conn, "pelican command", SearchLimits::default());
    let alpha_commands: Vec<&SearchHit> = of_kind(&hits, SearchKind::Command)
        .into_iter()
        .filter(|hit| hit.project_id == fixture.alpha.id)
        .collect();
    assert_eq!(alpha_commands.len(), 2, "one template, two workspaces");
    let mut names: Vec<&str> = alpha_commands
        .iter()
        .filter_map(|hit| hit.workspace_name.as_deref())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["pelican feature", "pelican workspace"],
        "the two rows share the project and are departed by the workspace"
    );
    assert!(
        alpha_commands
            .iter()
            .any(|hit| hit.workspace_id.as_deref() == Some(second.id.as_str())),
        "the new workspace's instance is the second row"
    );
}

/// REVIEW-FINDING (« Recherche contextuelle : projet et workspace cherchables, scoring par
/// couverture ») — THE ACCEPTANCE CASE, verbatim from the user: typing `bui dep auth` must bring up
/// the `build` command OF THE PROJECT `depot`, IN THE WORKSPACE `auth`.
///
/// It was impossible for two cumulative reasons, and this test pins both.
///
///   * `dep` and `auth` had NOTHING to match: the surface was `label` + `detail` + `extract`, and a
///     command called `build` running `bun run build` says neither word. The container names are in
///     the surface now, at a lesser weight.
///   * the aggregation was a FLAT MEAN over the query's terms, so `bui` at ~0.8 plus two zeroes
///     scored 0.27 — under the admit bar. The row the user had just described precisely was thrown
///     out, while the vaguer `bui` alone kept it. The mean is now rank-weighted.
///
/// The three `build`s in the fixture differ ONLY by their context, so the ranking they produce is
/// the coverage rule and nothing else.
#[test]
fn bui_dep_auth_finds_the_build_of_depot_in_auth_and_ranks_it_by_coverage() {
    let mut conn = db::open_in_memory();
    let (depot, depot_main) =
        db::create_project(&mut conn, "depot", "/tmp/gs/depot", Some("main")).expect("depot");
    let depot_auth = db::create_workspace(&mut conn, &depot.id, "auth", "/tmp/gs/depot/auth")
        .expect("the auth workspace");
    db::create_template(
        &mut conn,
        &depot.id,
        "build",
        "bun run build",
        None,
        db::CommandSource::default(),
    )
    .expect("depot's build");
    // A THIRD `build`, identical in every way but its containers — the control.
    let (orchard, _orchard_root) =
        db::create_project(&mut conn, "orchard", "/tmp/gs/orchard", Some("trunk"))
            .expect("orchard");
    db::create_template(
        &mut conn,
        &orchard.id,
        "build",
        "bun run build",
        None,
        db::CommandSource::default(),
    )
    .expect("orchard's build");

    let hits = hits_of(&mut conn, "bui dep auth", SearchLimits::default());
    let commands = of_kind(&hits, SearchKind::Command);
    assert_eq!(
        commands.len(),
        3,
        "all three builds are ADMITTED — the added words no longer argue them out of the answer"
    );

    // THE ANSWER: the depot/auth build is first, and it is the one the user described.
    let first = commands[0];
    assert_eq!(first.label, "build");
    assert_eq!(first.project_name, "depot");
    assert_eq!(first.workspace_name.as_deref(), Some("auth"));
    assert_eq!(first.workspace_id.as_deref(), Some(depot_auth.id.as_str()));

    // …and the order below it is COVERAGE, strictly: name + project + workspace, then name +
    // project, then name alone.
    assert_eq!(
        commands
            .iter()
            .map(|hit| (hit.project_name.as_str(), hit.workspace_name.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("depot", Some("auth")),
            ("depot", Some("main")),
            ("orchard", Some("trunk")),
        ],
        "more of the query accounted for ranks strictly better"
    );
    assert!(
        commands[0].score > commands[1].score && commands[1].score > commands[2].score,
        "the ranking is the SCORE's doing, not a tie broken by recency: {:?}",
        commands.iter().map(|hit| hit.score).collect::<Vec<_>>()
    );
    let _ = depot_main;

    // THE SAME QUERY REDUCED TO THE NAME cannot tell them apart — which is what the two added words
    // bought, and the measure of "the flat mean no longer penalises precision".
    let narrow = hits_of(&mut conn, "bui", SearchLimits::default());
    let narrow_commands = of_kind(&narrow, SearchKind::Command);
    assert_eq!(narrow_commands.len(), 3);
    assert!(
        narrow_commands
            .iter()
            .all(|hit| hit.score == narrow_commands[0].score),
        "with the name alone the three builds are indistinguishable"
    );
    let target = commands
        .iter()
        .find(|hit| hit.workspace_name.as_deref() == Some("auth"))
        .expect("the target");
    let same_target = narrow_commands
        .iter()
        .find(|hit| hit.id == target.id)
        .expect("the same row under the narrower query");
    assert!(
        target.score > same_target.score,
        "the precise query scores the row it describes HIGHER than the vague one — the exact \
         inversion the flat mean produced ({} vs {})",
        target.score,
        same_target.score
    );

    // AN EXACT NAME TERM does not flatten the ranking either: `build dep auth` still departs the
    // three, where an evidence-combining rule would have saturated at 1 and tied them.
    let exact = hits_of(&mut conn, "build dep auth", SearchLimits::default());
    let exact_commands = of_kind(&exact, SearchKind::Command);
    assert_eq!(
        exact_commands
            .iter()
            .map(|hit| hit.workspace_name.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("auth"), Some("main"), Some("trunk")]
    );
    assert!(exact_commands[0].score > exact_commands[1].score);
    assert!(exact_commands[1].score > exact_commands[2].score);
}

/// REVIEW-FINDING (« La couverture des termes doit primer ») — THE ACCEPTANCE CASE, verbatim from
/// the user: « j'ai un workspace qui s'appelle audio player et une commande qui s'appelle build et
/// pourtant c'est affiché dans les derniers ». Query: `audio manager build`.
///
/// The fixture is that install, reduced to what produced the complaint: ONE row accounts for all
/// three words (the `build` of `Music Manager`, in `audio player`), and five Specs account for
/// `audio` alone. It came tenth, and this test is what says it comes first.
///
/// The two cumulative causes it pins, both measured on the answer itself:
///
///   * the WHOLE-NEEDLE literal pass could not see the row. `audio manager build` is not equal to,
///     not a prefix of and not a substring of `build`, `bun run build`, `music manager` or `audio
///     player` — nor of anything else here — so every row of this fixture shared one bucket and the
///     score alone decided;
///   * and the score is CEILINGED BY SOURCE. The command has no index, so its `fts_norm` is `0` by
///     construction and its blend cannot pass `FUZZY_WEIGHT`, where a Spec's reaches four times
///     that. The row that answered the whole question was structurally under rows answering a third
///     of it.
///
/// Both are asserted rather than described: the answer is re-sorted BY SCORE — which IS the former
/// order on this fixture, one bucket for everyone — and the head of that re-sort is a one-word Spec,
/// not the command.
#[test]
fn audio_manager_build_puts_the_command_that_covers_the_whole_query_first() {
    let mut conn = db::open_in_memory();

    // The row the user described: a command named `build`, in the project `Music Manager`, in the
    // workspace `audio player`. Its three words are the user's three words — one on its own name,
    // two on its containers.
    let (music, audio_player) = db::create_project(
        &mut conn,
        "Music Manager",
        "/tmp/gs/music",
        Some("audio player"),
    )
    .expect("the Music Manager project and its audio player workspace");
    let template = db::create_template(
        &mut conn,
        &music.id,
        "build",
        "bun run build",
        None,
        db::CommandSource::default(),
    )
    .expect("the build command");
    let build = db::list_instances_for_workspace(&mut conn, &audio_player.id)
        .expect("the instances of the audio player workspace")
        .into_iter()
        .find(|instance| instance.command_id == template.id)
        .expect("the template is materialized in the workspace")
        .id;

    // The rows that beat it: Specs of ANOTHER project, each saying `audio` and nothing else of the
    // query. Indexed, so their blend is not capped the way the command's is.
    let (lab, _mixing) =
        db::create_project(&mut conn, "Sound Lab", "/tmp/gs/lab", Some("mixing")).expect("the lab");
    let mut lab_specs = Vec::new();
    for title in [
        "audio latency",
        "audio routing",
        "audio mixdown",
        "audio metering",
        "audio clipping",
    ] {
        let (spec, _rev) = db::create_spec(
            &mut conn,
            &lab.id,
            title,
            db::SpecSections {
                problem: "the audio path".to_string(),
                ..db::SpecSections::empty()
            },
        )
        .expect("an audio Spec");
        lab_specs.push(spec.id);
    }

    // THE TRAP, as everywhere in this suite: recency pushes the wrong way. Every Spec is newer than
    // the command, so the last tie-break cannot be what produces the expected order.
    stamp(
        &mut conn,
        "managed_commands",
        "updated_at",
        &template.id,
        1_000,
    );
    for (n, spec) in lab_specs.iter().enumerate() {
        stamp(&mut conn, "specs", "updated_at", spec, 2_000 + n as i64);
    }

    let hits = hits_of(&mut conn, "audio manager build", SearchLimits::default());
    let at = |id: &str| {
        hits.iter()
            .position(|hit| hit.id == id)
            .unwrap_or_else(|| panic!("{id} is in the answer: {:?}", labels(&hits)))
    };

    // THE ANSWER: the one row that accounts for all three words leads.
    assert_eq!(
        hits.first().map(|hit| hit.id.as_str()),
        Some(build.as_str()),
        "the build of Music Manager / audio player leads: {:?}",
        labels(&hits)
    );

    // THE ORDER IT REPLACES, rebuilt from the very hits the palette returned. No row of this
    // fixture can reach `Exact` or `Prefix` through the whole needle, so under the previous rule
    // they all shared `Match` and the score alone ranked them: sorting by score IS that order.
    let mut previously = hits.clone();
    previously.sort_by(|a, b| b.score.partial_cmp(&a.score).expect("no NaN in a score"));
    assert_eq!(
        previously.first().map(|hit| hit.kind),
        Some(SearchKind::Spec),
        "the order the score alone produces is headed by a Spec: {:?}",
        labels(&previously)
    );
    assert!(
        previously
            .iter()
            .position(|hit| hit.id == build)
            .expect("the command is in the answer")
            > 0,
        "…and the command is not at its head — the regression this test guards"
    );

    // WHY it lost, stated as the measurement rather than as prose: the winning row is the LOWEST
    // scored of the three compared here. Its ceiling is `FUZZY_WEIGHT` because it has no index; a
    // Spec's is four times that. At equal bucket the score decided by SOURCE, and coverage is what
    // now decides before it.
    let command_hit = &hits[at(&build)];
    let best_spec = previously
        .iter()
        .find(|hit| hit.kind == SearchKind::Spec)
        .expect("a Spec answers");
    assert!(
        command_hit.score < best_spec.score,
        "the row that leads is still the worse-SCORED one — the ceiling did not move, the ordering \
         key did: {} vs {}",
        command_hit.score,
        best_spec.score
    );

    // COVERAGE 3 > 2 > 1, THREE SOURCES CONFOUNDED. The workspace `audio player` accounts for two
    // words (`audio` on its own name, `manager` on its project) and no more; a lab Spec accounts
    // for `audio` alone. Neither the type nor the score explains this order — only how much of the
    // query each row answers.
    assert!(
        at(&build) < at(&audio_player.id),
        "a command covering three words beats a workspace covering two: {:?}",
        labels(&hits)
    );
    assert!(
        at(&audio_player.id) < at(&lab_specs[0]),
        "…and that workspace beats a Spec covering one, its higher score notwithstanding: {:?}",
        labels(&hits)
    );
    assert!(
        hits[at(&audio_player.id)].score < hits[at(&lab_specs[0])].score,
        "the two-word workspace is the worse-scored of that pair too — coverage, not relevance, \
         is what ordered them"
    );

    // The five one-word Specs are all still ANSWERED: rewarding coverage does not turn the OR into
    // an AND, it only decides who reads first.
    for spec in &lab_specs {
        let _ = at(spec);
    }
}

/// REVIEW-FINDING (« La couverture des termes doit primer ») : the two CERTAIN buckets are
/// reachable again once the query has more than one word — and the context still cannot buy them.
///
/// A palette that only ever answers `Match` past the first space has no certainty signal left, and
/// the ranking falls back on a score that is not comparable between sources. The rule is read term
/// by term now: a row whose OWN NAME is the words you typed is `Exact`, one whose name each of your
/// words prefixes is `Prefix`, and a row that needs its project or its workspace to account for a
/// word is `Match` however completely it covers the query.
#[test]
fn a_multi_term_query_reaches_the_certain_buckets_only_through_the_name() {
    let mut conn = db::open_in_memory();
    // Two commands of one CONTEXT-FREE project, so nothing but their own names can depart them.
    let (narwhal, trunk) =
        db::create_project(&mut conn, "narwhal", "/tmp/gs/narwhal", Some("trunk"))
            .expect("the narwhal project");
    // Its words ARE the query, order aside: the multi-word form of `Exact`.
    let named = command_instance(&mut conn, &narwhal.id, &trunk.id, "pipeline build", 1_000);
    // Each word of the query prefixes one of its words: `Prefix`.
    let prefixed = command_instance(
        &mut conn,
        &narwhal.id,
        &trunk.id,
        "buildable pipelines",
        1_001,
    );
    // A row of another project, covering BOTH words through its project NAME alone.
    let (containers, containers_root) =
        db::create_project(&mut conn, "build pipeline", "/tmp/gs/bp", Some("root"))
            .expect("a project literally named the query");
    let contextual = command_instance(
        &mut conn,
        &containers.id,
        &containers_root.id,
        "deploy",
        1_002,
    );

    // THE TRAP: an indexed Spec whose SCORE beats every command here (its blend is not capped at
    // `FUZZY_WEIGHT`) and whose name accounts for both words — but one of them only mid-word
    // (`build` inside `rebuild`), which is a landing and not a prefix. It stays a `Match`.
    let (spec, _rev) = db::create_spec(
        &mut conn,
        &narwhal.id,
        "pipelines to rebuild",
        db::SpecSections {
            problem: "the nightly rebuild of the pipelines".to_string(),
            ..db::SpecSections::empty()
        },
    )
    .expect("the Spec");
    stamp(&mut conn, "specs", "updated_at", &spec.id, 9_000);

    let hits = hits_of(&mut conn, "build pipeline", SearchLimits::default());
    let at = |id: &str| {
        hits.iter()
            .position(|hit| hit.id == id)
            .unwrap_or_else(|| panic!("{id} is in the answer: {:?}", labels(&hits)))
    };

    // EXACT, then PREFIX, then MATCH — on a query of two words, where every row used to be a
    // `Match` because `build pipeline` is not a substring of `pipeline build`.
    assert!(
        at(&named) < at(&prefixed) && at(&prefixed) < at(&spec.id),
        "the name that IS the query leads, then the one each word prefixes, then the rest: {:?}",
        labels(&hits)
    );
    // …and the SCORE says the opposite, which is the whole point of a bucket: the Spec is the
    // best-scored row of the three and reads last of them.
    assert!(
        hits[at(&spec.id)].score > hits[at(&named)].score
            && hits[at(&spec.id)].score > hits[at(&prefixed)].score,
        "the buckets ordered these three against the score, not with it: {:?}",
        labels(&hits)
    );

    // THE CONTEXT BUYS NO CERTAINTY. `deploy` accounts for BOTH words — as completely as
    // `pipeline build` does — and still reads below the two rows whose own name accounts for them.
    // Coverage is a tie-break UNDER the bucket, never a way around it.
    assert!(
        at(&contextual) > at(&prefixed),
        "a row that needs its project to cover the query stays below the names that cover it: {:?}",
        labels(&hits)
    );
    // The project literally named the query is the `Exact` of its own source and reads at the top
    // with `pipeline build` — an exact name is an exact name whatever the type carrying it.
    assert!(
        at(&containers.id) < at(&prefixed),
        "the project named `build pipeline` is Exact too: {:?}",
        labels(&hits)
    );
}

/// REVIEW-FINDING (« La couverture des termes doit primer ») : the ONE-TERM regime is left strictly
/// alone, pinned as an IDENTITY rather than as a fixture — which is strictly stronger, since an
/// identity holds on every input and a fixture only on its own.
///
/// The two halves of the guarantee:
///
///   * on a query of fewer than two terms, [`literal_verdict`] returns EXACTLY what the whole-needle
///     rule returns — the rule as it was before the per-term reading existed — so no row's bucket
///     moves;
///   * and it returns a coverage of `0`, which every OTHER row of that same search also carries, so
///     the key [`by_rank`] gained is a constant there and can reorder nothing.
///
/// The scoring is untouched by this pass (nothing in `idea.rs` moved), so bucket + coverage + score
/// unchanged is the whole ranking unchanged.
///
/// The same loop pins the invariant `by_rank` leans on past one term: a bucket better than `Match`
/// is only reachable through a surface that accounts for EVERY term, so the two ordering keys can
/// never contradict each other. Below two terms there is nothing to contradict — the coverage is
/// the same `0` on every row.
#[test]
fn the_one_term_verdict_is_the_whole_needle_rule_and_a_better_bucket_implies_full_coverage() {
    let candidate = SearchCandidate {
        id: "c1".to_string(),
        project_id: "p1".to_string(),
        project_name: "Music Manager".to_string(),
        workspace_id: Some("w1".to_string()),
        workspace_name: Some("audio player".to_string()),
        spec_id: None,
        terminal_id: None,
        label: "build pipeline".to_string(),
        detail: Some("bun run build".to_string()),
        extract: None,
        human_key: None,
        updated_at: 1,
        score: 0.0,
    };
    let label = candidate.label.to_lowercase();
    let secondary = secondary_surface(&candidate);

    for query in [
        "build pipeline",
        "build",
        "buil",
        "pipeline",
        "ipelin",
        "audio",
        "manager",
        "zzzz",
        "!!",
        "build pipeline extra",
        "pipeline build",
        "buil pipe",
        "audio manager build",
        "audio zzzz",
    ] {
        let needle = query.to_lowercase();
        let terms = crate::idea::tokenize(query);
        let verdict = literal_verdict(&needle, &terms, &label, &candidate);
        let whole = needle_bucket(&needle, &label, &secondary);

        if terms.len() < 2 {
            assert_eq!(
                verdict.as_ref().map(|literal| literal.bucket),
                whole,
                "`{query}` has fewer than two terms: the verdict IS the whole-needle rule"
            );
            assert_eq!(
                verdict.as_ref().map(|literal| literal.coverage),
                whole.map(|_| 0),
                "…and its coverage is the constant 0, so the key orders nothing"
            );
        }

        if let Some(literal) = &verdict {
            if terms.len() >= 2 && literal.bucket < Bucket::Match {
                assert_eq!(
                    literal.coverage,
                    terms.len(),
                    "`{query}` reached {:?}, which is only reachable with every term accounted \
                     for — the two ordering keys may never disagree",
                    literal.bucket
                );
            }
            // A verdict is never WORSE than the whole-needle rule's: the per-term reading only
            // ever adds certainty, it cannot take any away.
            if let Some(whole) = whole {
                assert!(literal.bucket <= whole, "`{query}` lost certainty");
            }
        } else {
            assert!(whole.is_none(), "`{query}` dropped a whole-needle match");
        }
    }
}

/// REVIEW-FINDING (« Recherche contextuelle ») : the context is in the match surface FOR THE SEVEN
/// KINDS, it weighs LESS than the entity's own name, and a SINGLE term still finds — the FTS terms
/// are still OR-joined, not AND-joined.
#[test]
fn the_context_is_searchable_on_every_kind_weighs_less_than_the_name_and_keeps_the_or() {
    let mut conn = db::open_in_memory();
    // Two identical projects: one entity of every kind, all named `walrus …`. The only difference
    // is the project NAME — `narwhal` on one side, nothing like it on the other.
    let inside = seed_project(&mut conn, "inside", "walrus", 1_000_000);
    let outside = seed_project(&mut conn, "outside", "walrus", 2_000_000);
    // Renamed WITHOUT stamping, so `outside` keeps the recency ADVANTAGE: only the context can
    // produce the expected order.
    rename(&mut conn, "projects", &inside.id, "walrus narwhal");
    rename(&mut conn, "projects", &outside.id, "walrus otter");

    // ALL SEVEN KINDS: the hit whose project says `narwhal` outscores its twin that does not.
    let hits = hits_of(&mut conn, "walrus narwhal", SearchLimits::default());
    for kind in GROUP_ORDER {
        let group = of_kind(&hits, kind);
        let with = group
            .iter()
            .find(|hit| hit.project_id == inside.id)
            .unwrap_or_else(|| panic!("{kind:?} answers inside the narwhal project"));
        let without = group
            .iter()
            .find(|hit| hit.project_id == outside.id)
            .unwrap_or_else(|| panic!("{kind:?} answers outside it too"));
        assert!(
            with.score > without.score,
            "{kind:?} benefits from its context: {} vs {}",
            with.score,
            without.score
        );
    }

    // THE CONTEXT WEIGHS LESS THAN THE NAME. `walrus workspace` (the workspace of the narwhal
    // project) is named for the query's first word; the workspace of the other project is too. Give
    // ONE of them the second word in its own NAME and it must beat the one that only has it in its
    // project — same word, one on the name, one on the context.
    rename(&mut conn, "workspaces", &outside.workspace_id, "narwhal ws");
    let hits = hits_of(&mut conn, "narwhal", SearchLimits::default());
    let workspaces = of_kind(&hits, SearchKind::Workspace);
    let named = workspaces
        .iter()
        .find(|hit| hit.id == outside.workspace_id)
        .expect("the workspace NAMED narwhal answers");
    let contextual = workspaces
        .iter()
        .find(|hit| hit.id == inside.workspace_id)
        .expect("the workspace merely IN the narwhal project answers too");
    assert!(
        named.score > contextual.score,
        "a term landing on the name beats the same term landing on the context: {} vs {}",
        named.score,
        contextual.score
    );
    assert_eq!(
        workspaces[0].id, outside.workspace_id,
        "…and that is the order the palette draws"
    );

    // A SINGLE TERM STILL FINDS, and the FTS terms stay OR-joined: a two-word query where only ONE
    // word is indexed anywhere must still answer, which an AND would make impossible.
    assert!(
        crate::idea::build_fts_query("walrus zzzzz")
            .expect("a usable expression")
            .contains(" OR "),
        "the FTS terms are OR-joined — one term must be enough to find"
    );
    let one_word = hits_of(&mut conn, "walrus", SearchLimits::default());
    assert!(
        !of_kind(&one_word, SearchKind::Spec).is_empty(),
        "one term finds the indexed sources"
    );
    let half_matching = hits_of(&mut conn, "walrus zzzzz", SearchLimits::default());
    assert!(
        !of_kind(&half_matching, SearchKind::Spec).is_empty(),
        "a second word that matches NOTHING does not empty the answer — an AND would"
    );
    assert!(
        !of_kind(&half_matching, SearchKind::Idea).is_empty()
            && !of_kind(&half_matching, SearchKind::Task).is_empty(),
        "the same holds for the other two indexed sources"
    );
}

/// REVIEW-FINDING (« Clé humaine KEY-numéro sur les lignes Spec de la palette ») : a Spec hit
/// carries its human key `KEY-number` on BOTH paths — the ranked results and the empty-query
/// suggestions, which draw the same row — and the key is DISPLAYED WITHOUT BEING SEARCHABLE.
///
/// The second half is the point of the field existing at all. `detail` is both drawn and MATCHED
/// (`literal_verdict` reads it, `searchable_terms` tokenizes it), so putting `ZEPHYR-1` there would
/// have made it a query and shifted an order settled without it. `human_key` is drawn only, and
/// this test is what says so: typing the key back finds the PROJECT it names and no Spec.
///
/// The fixture is its own so the key shares no prefix with anything indexed — on the shared
/// `pelican` fixture a key derived from the project name would be a fuzzy neighbour of the labels
/// and the assertion would be measuring the trigram pass, not the match surface.
#[test]
fn a_spec_carries_its_human_key_on_both_paths_and_the_key_is_never_matched() {
    let mut conn = db::open_in_memory();
    let (project, _root) =
        db::create_project(&mut conn, "Zephyr", "/tmp/gs/zephyr", None).expect("project");
    assert_eq!(
        project.key, "ZEPHYR",
        "the fixture's key is a known literal"
    );
    let (spec, _rev) = db::create_spec(
        &mut conn,
        &project.id,
        "quokka roadmap",
        db::SpecSections {
            problem: "the quokka problem".into(),
            ..db::SpecSections::empty()
        },
    )
    .expect("create the Spec");
    assert_eq!(spec.number, 1, "the first Spec of a project is number 1");

    // The RESULTS path.
    let hits = hits_of(&mut conn, "quokka", SearchLimits::default());
    let specs = of_kind(&hits, SearchKind::Spec);
    assert_eq!(specs.len(), 1, "one Spec matched");
    assert_eq!(
        specs[0].human_key.as_deref(),
        Some("ZEPHYR-1"),
        "the ranked Spec row carries KEY-number"
    );

    // The SUGGESTIONS path — the same row, ordered by recency alone.
    let suggestions = hits_of(&mut conn, "", SearchLimits::default());
    let suggested = of_kind(&suggestions, SearchKind::Spec);
    assert_eq!(suggested.len(), 1, "the Spec is suggested");
    assert_eq!(
        suggested[0].human_key.as_deref(),
        Some("ZEPHYR-1"),
        "the suggested Spec row carries the same key"
    );

    // No other kind claims a human key — a Spec is the only entity the schema gives one.
    for hit in suggestions.iter().filter(|h| h.kind != SearchKind::Spec) {
        assert!(hit.human_key.is_none(), "{:?} has no human key", hit.kind);
    }

    // The key is NOT a query. It names the project, so the project may answer; the Spec may not.
    let by_key = hits_of(&mut conn, "ZEPHYR-1", SearchLimits::default());
    assert!(
        of_kind(&by_key, SearchKind::Spec).is_empty(),
        "the key is drawn, never matched — searching a Spec BY its key is a separate capability, \
         got {:?}",
        by_key
            .iter()
            .map(|h| (h.kind, &h.label))
            .collect::<Vec<_>>()
    );

    // REVIEW-FINDING (« Recherche contextuelle ») : the CONTEXT joining the match surface did not
    // smuggle the key back in through the side door. A Spec's key is built from its project's
    // `key`, and the project's NAME is now searchable — but the key itself still is not, and
    // neither does the project name summon a Spec whose own text says nothing about the query.
    let by_project_name = hits_of(&mut conn, "zephyr", SearchLimits::default());
    assert!(
        of_kind(&by_project_name, SearchKind::Spec).is_empty(),
        "the context ranks what the reads found; it does not turn a project name into a Spec \
         listing, got {:?}",
        by_project_name
            .iter()
            .map(|h| (h.kind, &h.label))
            .collect::<Vec<_>>()
    );
    assert!(
        !of_kind(&by_project_name, SearchKind::Project).is_empty(),
        "…while the project the name belongs to answers, as it always did"
    );
}

/// REVIEW-FINDING (« EmptyState "base vierge" déclenché sur une base NON vide ») : the palette's
/// "nothing to search yet" is keyed on `knowledge_empty`, and this test is the reason that field
/// exists at all.
///
/// The two fixtures are the two answers a capped list confuses:
///
///   * an install that really holds NO Spec, no task and no idea — just the project and the root
///     workspace one created — where the flag is `true` even though the suggestions are not empty;
///   * an install that DOES hold a Spec, whose eight most recently touched entities are commands
///     — the ordinary case, since a session's `last_seen_at` and a command's `updated_at` move
///     while the Specs sit still — where the flag is `false` even though not one suggestion is a
///     Spec, a task or an idea.
///
/// The second fixture is the false positive itself: read off the suggestions, it says "empty
/// install" on a base with a Spec in it.
#[test]
fn the_knowledge_flag_answers_the_install_and_never_the_head_of_the_suggestions() {
    // --- FIXTURE 1: a fresh install — containers only ----------------------------------------
    let mut conn = db::open_in_memory();
    let (fresh, fresh_root) =
        db::create_project(&mut conn, "fresh", "/tmp/gs/fresh", None).expect("project");

    let answer = global_search(&mut conn, "", SearchLimits::default()).expect("suggestions");
    assert!(
        answer.knowledge_empty,
        "no Spec, no task, no idea — this install has nothing to search yet"
    );
    assert!(
        !answer.hits.is_empty(),
        "the containers still answer: the flag is what says the install is empty, never the \
         length of the list"
    );
    assert!(
        answer
            .hits
            .iter()
            .all(|hit| matches!(hit.kind, SearchKind::Project | SearchKind::Workspace)),
        "…and what they answer is exactly the project and its root workspace"
    );
    // The RANKED path reports the same fact — the flag means one thing whatever the regime.
    assert!(
        global_search(&mut conn, "fresh", SearchLimits::default())
            .expect("ranked")
            .knowledge_empty
    );

    // --- FIXTURE 2: a Spec exists, but the top eight are all commands ------------------------
    let (spec, _rev) = db::create_spec(
        &mut conn,
        &fresh.id,
        "an existing Spec",
        db::SpecSections {
            problem: "a problem".into(),
            ..db::SpecSections::empty()
        },
    )
    .expect("create the Spec");
    // Explicit stamps: the Spec (and its containers) OLD, then CAP_TYPE commands, each newer.
    stamp(&mut conn, "projects", "updated_at", &fresh.id, 1_000);
    stamp(&mut conn, "workspaces", "updated_at", &fresh_root.id, 1_000);
    stamp(&mut conn, "specs", "updated_at", &spec.id, 1_000);
    for n in 0..CAP_TYPE {
        let command = db::create_template(
            &mut conn,
            &fresh.id,
            &format!("command {n}"),
            "bun run something",
            None,
            db::CommandSource::default(),
        )
        .expect("create the managed command");
        stamp(
            &mut conn,
            "managed_commands",
            "updated_at",
            &command.id,
            2_000 + n as i64,
        );
    }

    let answer = global_search(&mut conn, "", SearchLimits::default()).expect("suggestions");
    assert_eq!(
        answer.hits.len(),
        CAP_TYPE,
        "one full screen of suggestions"
    );
    assert!(
        answer
            .hits
            .iter()
            .all(|hit| hit.kind == SearchKind::Command),
        "the top eight hold NO Spec, no task and no idea — got {:?}",
        answer.hits.iter().map(|hit| hit.kind).collect::<Vec<_>>()
    );
    assert!(
        !answer.knowledge_empty,
        "the install HOLDS a Spec: the flag reports the database, not what survived the cap"
    );
    // Same fact on the ranked path, where the query matches none of the knowledge either.
    assert!(
        !global_search(&mut conn, "command", SearchLimits::default())
            .expect("ranked")
            .knowledge_empty
    );

    // An ARCHIVED Spec is not searchable, and does not lift the flag: "searchable" here is the
    // definition the recency reads apply, because the flag is computed through those same reads.
    let mut conn = db::open_in_memory();
    let (archived_project, _root) =
        db::create_project(&mut conn, "archived", "/tmp/gs/archived", None).expect("project");
    let (archived_spec, _rev) = db::create_spec(
        &mut conn,
        &archived_project.id,
        "an archived Spec",
        db::SpecSections::empty(),
    )
    .expect("create the Spec");
    archive_spec(&mut conn, &archived_spec.id);
    assert!(
        global_search(&mut conn, "", SearchLimits::default())
            .expect("suggestions")
            .knowledge_empty,
        "an archived Spec answers no search, so it does not make the install searchable"
    );
}

/// REVIEW-FINDING (« Trois constantes produit dupliquées Rust/TS sans parité tenue ») : the group
/// ORDER, the per-type CAP and the query FLOOR are product rules that exist twice — here and in the
/// palette (`search-meta.ts`, `search-palette.tsx`, `use-global-search.ts`) — and nothing made the
/// two halves fail together. A cap raised on this side alone would have shipped a backend answering
/// more rows than the palette draws, with every suite green.
///
/// `contract/search-fixtures.json` is now the statement of those three rules, replayed here and by
/// `search-rules-contract.test.ts` on the renderer side (which also reads THIS file's declarations,
/// so an edit made here alone reddens the TypeScript suite).
///
/// The order is compared on the SERIALIZED kind names — what actually crosses the wire and what the
/// renderer's union spells — rather than on the variant identifiers.
#[test]
fn the_shared_fixture_pins_the_three_product_rules_the_renderer_repeats() {
    let fixture: serde_json::Value =
        serde_json::from_str(SEARCH_FIXTURES).expect("contract/search-fixtures.json is valid JSON");

    let order: Vec<serde_json::Value> = GROUP_ORDER
        .into_iter()
        .map(|kind| serde_json::to_value(kind).expect("a kind serializes to its wire name"))
        .collect();
    assert_eq!(
        fixture["group_order"],
        serde_json::Value::Array(order),
        "GROUP_ORDER and the shared fixture must agree — the trim consumes the fixture's order, \
         and the palette's own copy names the very same seven kinds"
    );
    assert_eq!(
        fixture["cap_type"].as_u64(),
        Some(CAP_TYPE as u64),
        "CAP_TYPE is the palette's SUGGESTION_CAP: one un-scrolled screen, agreed on both sides"
    );
    assert_eq!(
        fixture["min_query_len"].as_u64(),
        Some(MIN_QUERY_LEN as u64),
        "MIN_QUERY_LEN is the floor BOTH halves enforce — the renderer to issue no read, the core \
         to answer nothing"
    );
}
