//! Re-playable DEMO SEED for nyx v2 (dogfooding tool — NOT production code).
//!
//! Opens the v2 `nyx_core::db::Db` on a target SQLite file (running migrations, so a
//! fresh/empty file is created + brought fully up to schema) and populates a RICH, coherent
//! dataset through the SAME shared authorities the UI/MCP use — `nyx_core::repository`
//! (project/workspace + explicit per-project repo config), `nyx_core::db` (spec/task/review CRUD + the guarded
//! lifecycle transitions) and `nyx_core::review_comment` (anchor capture + re-mapping) — so
//! every invariant (state machines, human-verify guard, acyclic deps, anchor re-map/outdated)
//! is enforced exactly as in the app. No raw INSERTs.
//!
//! Usage:
//!   cargo run -p nyx-core --example seed_demo -- <db_path> [setup|remap]
//!
//! Env (paths of the demo git trees the shell wrapper creates):
//!   NYX_SEED_DEMO_ROOT   multi-repo root holding `api/` + `web/`   (default ~/nyx-demo)
//!   NYX_SEED_DEMO_ROOT2  a 2nd multi-repo worktree (feature)       (default ~/nyx-demo-feature)
//!   NYX_SEED_NYX_V2      the REAL nyx-v2 checkout (mono-repo)       (default this repo)
//!
//! Modes:
//!   setup  create projects/workspaces/repos + specs/tasks + review comments/tasks + commands
//!          + terminals, and anchor comments on the api anchor file at feature HEAD.
//!   remap  after the shell has ADVANCED the api repo (shift + drop of the anchored lines),
//!          re-map the anchored comments → one FOLLOWS the moved code, one flips OUTDATED.

use std::path::Path;

use nyx_core::db::{
    self, CommandSource, CommentAnchor, Db, NewComment, NewTaskInput, SpecSection, SpecSections,
    SpecTransition, TaskTransition, AUTHOR_AGENT, AUTHOR_HUMAN, DEPENDENCY_TYPE_BLOCKS, SIDE_NEW,
};
use nyx_core::{repository, review_comment};

fn home() -> String {
    std::env::var("HOME").expect("HOME set")
}
fn env_or(key: &str, default: String) -> String {
    std::env::var(key).unwrap_or(default)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db_path = args
        .get(1)
        .cloned()
        .expect("usage: seed_demo <db_path> [setup|remap]");
    let mode = args.get(2).cloned().unwrap_or_else(|| "setup".to_string());

    let db = Db::open(Path::new(&db_path)).expect("open+migrate target db");

    match mode.as_str() {
        "setup" => setup(&db),
        "remap" => remap(&db),
        other => panic!("unknown mode '{other}' (setup|remap)"),
    }
}

// ------------------------------------------------------------------------------------------
// SETUP — the full dataset.
// ------------------------------------------------------------------------------------------

fn setup(db: &Db) {
    let demo_root = env_or("NYX_SEED_DEMO_ROOT", format!("{}/nyx-demo", home()));
    let demo_root2 = env_or(
        "NYX_SEED_DEMO_ROOT2",
        format!("{}/nyx-demo-feature", home()),
    );
    let nyx_v2 = env_or("NYX_SEED_NYX_V2", "/home/kris/Projects/nyx-v2".to_string());

    // --- Project A: "Nyx Demo" over the multi-repo tree (api + web) ------------------------
    let (proj_a, root_ws_a) = db
        .with_conn(|c| repository::register_project(c, "Nyx Demo", &demo_root, None))
        .expect("register Nyx Demo");
    // A 2nd workspace (a feature worktree) — "plusieurs workspaces".
    let feat_ws = db
        .with_conn(|c| repository::register_workspace(c, &proj_a.id, "feature/login", &demo_root2))
        .expect("register feature workspace");
    // Finding #27: configure the project's repos EXPLICITLY (no disk auto-detection). Both
    // workspaces then inherit them at their own root_path + relative_path.
    let repos_a = db
        .with_conn(|c| {
            db::set_project_repositories(
                c,
                &proj_a.id,
                &[
                    db::RepoConfig {
                        relative_path: "api".into(),
                        name: Some("api".into()),
                    },
                    db::RepoConfig {
                        relative_path: "web".into(),
                        name: Some("web".into()),
                    },
                ],
            )
        })
        .expect("configure repos A");
    let api_repo = repos_a
        .iter()
        .find(|r| r.name == "api")
        .expect("api repo configured")
        .clone();
    let web_repo = repos_a
        .iter()
        .find(|r| r.name == "web")
        .expect("web repo configured")
        .clone();
    println!(
        "project A '{}' ws=[{}, {}] repos=[{}, {}]",
        proj_a.name, root_ws_a.id, feat_ws.id, api_repo.name, web_repo.name
    );

    // --- Project B: self-review over the REAL nyx-v2 checkout (mono-repo) ------------------
    let (proj_b, root_ws_b) = db
        .with_conn(|c| repository::register_project(c, "nyx-v2 (self review)", &nyx_v2, None))
        .expect("register nyx-v2 self-review");
    // Finding #27: a mono-repo is one explicit repo at relative_path ".".
    let repos_b = db
        .with_conn(|c| {
            db::set_project_repositories(
                c,
                &proj_b.id,
                &[db::RepoConfig {
                    relative_path: ".".into(),
                    name: Some("nyx-v2".into()),
                }],
            )
        })
        .expect("configure repos B");
    let nyxv2_repo = repos_b
        .first()
        .expect("nyx-v2 mono repo configured")
        .clone();
    println!(
        "project B '{}' ws={} repo={} (rel='{}')",
        proj_b.name, root_ws_b.id, nyxv2_repo.name, nyxv2_repo.relative_path
    );

    // --- Specs in EVERY state, anchored on workspaces --------------------------------------
    // 1) COMPLETED, multi-phase, with done tasks.
    let s_done = create_spec(
        db,
        &proj_a.id,
        &root_ws_a.id,
        "User login & sessions",
        "Users have no way to authenticate; sessions are not persisted across reloads.",
        "Add email+password auth with signed, http-only session cookies and a refresh flow.",
    );
    author_section(db, &s_done, SpecSection::UserStories,
        "- As a user I can sign in with email + password\n- As a user my session survives a reload\n- As a user I can sign out everywhere");
    author_section(
        db,
        &s_done,
        SpecSection::OutOfScope,
        "- SSO / OAuth providers (later)\n- Passwordless magic links\n- Hardware 2FA keys",
    );
    transition(db, &s_done, SpecTransition::Ready);
    transition(db, &s_done, SpecTransition::Start);
    transition(db, &s_done, SpecTransition::Review);
    transition(db, &s_done, SpecTransition::Done);
    set_phases(db, &s_done);

    // 2) IN_REVIEW — carries the api review (comments + promoted tasks).
    let s_review = create_spec(
        db,
        &proj_a.id,
        &root_ws_a.id,
        "Realtime notifications",
        "Users must refresh to see new activity; there is no push channel.",
        "Introduce a WebSocket gateway with per-user topics and an at-least-once delivery buffer.",
    );
    author_section(db, &s_review, SpecSection::ImplementationDecisions,
        "- Gateway is a separate process behind the API\n- Redis pub/sub as the fan-out bus\n- Client reconnects with exponential backoff + resume token");
    author_section(
        db,
        &s_review,
        SpecSection::OutOfScope,
        "- Email/SMS fallback delivery\n- Notification preferences UI",
    );
    transition(db, &s_review, SpecTransition::Ready);
    transition(db, &s_review, SpecTransition::Start);
    transition(db, &s_review, SpecTransition::Review);
    set_phases(db, &s_review);

    // 3) IN_PROGRESS — task variety + a human task + a dependency edge.
    let s_prog = create_spec(
        db,
        &proj_a.id,
        &feat_ws.id,
        "Billing & invoices",
        "There is no way to charge customers or produce invoices for accounting.",
        "Integrate a payment provider, model subscriptions, and generate PDF invoices per cycle.",
    );
    author_section(db, &s_prog, SpecSection::TestingDecisions,
        "- Contract tests against the provider sandbox\n- Golden-file tests for invoice PDFs\n- A nightly reconciliation job with alerting");
    transition(db, &s_prog, SpecTransition::Ready);
    transition(db, &s_prog, SpecTransition::Start);
    set_phases(db, &s_prog);

    // 4) READY — authored + validated, not yet started.
    let s_ready = create_spec(
        db,
        &proj_a.id,
        &root_ws_a.id,
        "API rate limiting",
        "A single client can exhaust the API for everyone; there is no throttling.",
        "Token-bucket limiter per API key at the gateway, with 429 + Retry-After and burst config.",
    );
    transition(db, &s_ready, SpecTransition::Ready);

    // 5) DRAFT — still being shaped; plus a REVISION (fork) to show history.
    let s_draft = create_spec(
        db,
        &proj_a.id,
        &root_ws_a.id,
        "Dark mode theming",
        "The UI is light-only; users in low light have asked for a dark theme.",
        "", // solution still empty on purpose → legitimately a draft
    );
    author_section(
        db,
        &s_draft,
        SpecSection::OutOfScope,
        "- Per-component theme overrides\n- User-uploaded custom palettes",
    );
    db.with_conn(|c| {
        db::fork_spec(
            c,
            &s_draft,
            Some("Split theming from the design-tokens work"),
        )
    })
    .expect("fork draft spec");

    // 6) Self-review spec on the REAL nyx-v2 diff (in_review), anchored on project B's workspace.
    let s_self = create_spec(
        db,
        &proj_b.id,
        &root_ws_b.id,
        "Review: v2 review subsystem",
        "The grafted working tree adds the whole review subsystem; it needs a self-review pass.",
        "Walk the diff: anchoring, re-mapping, the grill (comment → task), and the diff engine.",
    );
    transition(db, &s_self, SpecTransition::Ready);
    transition(db, &s_self, SpecTransition::Start);
    transition(db, &s_self, SpecTransition::Review);

    // --- Tasks: kinds (slice/human/review) × states (todo/in_progress/done/blocked/cancelled) --
    // COMPLETED spec — three done slice tasks across phases.
    let t_schema = task(
        db,
        &proj_a.id,
        &s_done,
        "Design the sessions schema",
        TaskKind::Slice,
        Some(1),
    );
    start(db, &t_schema);
    done(db, &t_schema);
    let t_cookie = task(
        db,
        &proj_a.id,
        &s_done,
        "Signed cookie middleware",
        TaskKind::Slice,
        Some(2),
    );
    start(db, &t_cookie);
    done(db, &t_cookie);
    let t_signout = task(
        db,
        &proj_a.id,
        &s_done,
        "Global sign-out",
        TaskKind::Slice,
        Some(3),
    );
    start(db, &t_signout);
    done(db, &t_signout);

    // IN_PROGRESS spec — the variety pack + a dependency.
    let t_provider = task(
        db,
        &proj_a.id,
        &s_prog,
        "Wire the payment provider SDK",
        TaskKind::Slice,
        Some(1),
    );
    start(db, &t_provider); // in_progress
    let t_subs = task(
        db,
        &proj_a.id,
        &s_prog,
        "Model subscriptions & plans",
        TaskKind::Slice,
        Some(1),
    );
    // t_subs depends on t_provider (blocks) — real acyclic edge.
    db.with_conn(|c| db::add_task_dependency(c, &t_subs, &t_provider, DEPENDENCY_TYPE_BLOCKS))
        .expect("dep subs->provider");
    let t_pdf = task_full(
        db,
        &proj_a.id,
        &s_prog,
        "Blocked: invoice PDF renderer",
        TaskKind::Slice,
        Some(2),
        Some("Renderer library evaluation stalled pending a licensing decision."),
        Some("A cycle's invoice renders to a byte-stable PDF matching the golden file."),
        None,
    );
    block(db, &t_pdf); // blocked
    let t_spike = task(
        db,
        &proj_a.id,
        &s_prog,
        "Spike: dunning emails (dropped)",
        TaskKind::Slice,
        None,
    );
    skip(db, &t_spike); // cancelled (skipped)
    let t_todo = task(
        db,
        &proj_a.id,
        &s_prog,
        "Reconciliation nightly job",
        TaskKind::Slice,
        Some(2),
    ); // stays todo
    let _ = t_todo;
    // A HUMAN task closed through the VERIFIED path (guard-rail).
    let t_human = task_full(
        db,
        &proj_a.id,
        &s_prog,
        "Rotate the payment API secret in prod",
        TaskKind::Human,
        Some(2),
        Some("A human must rotate the live secret and confirm the webhook still validates."),
        Some("New secret active; a test webhook is accepted; the old secret is revoked."),
        Some("manual: operator confirmed rotation + webhook check"),
    );
    start(db, &t_human);
    db.with_conn(|c| db::verify_human_task(c, &t_human))
        .expect("verify human task"); // done via verify
                                      // A REVIEW-kind finding task with a severity.
    let t_finding = task_full(
        db,
        &proj_a.id,
        &s_review,
        "Finding: gateway leaks a resume token in logs",
        TaskKind::Review,
        None,
        Some("The reconnect resume token is logged at info level on the gateway."),
        Some("The token is redacted in all log sinks; a test asserts no token in output."),
        None,
    );
    start(db, &t_finding);

    // ------------------------------------------------------------------------------------
    // REVIEW comments + tasks on the API repo (Project A root workspace) — the full matrix.
    // ------------------------------------------------------------------------------------
    let api_path = demo_member_path(db, &root_ws_a.id, &api_repo.id);
    let anchor_file = "src/anchor.rs";
    let anchor_abs = format!("{api_path}/{anchor_file}");
    let anchor_src = std::fs::read_to_string(&anchor_abs).unwrap_or_default();

    // GLOBAL comment (no anchor), human.
    let c_global = post(
        db,
        &root_ws_a.id,
        &api_repo.id,
        Some(&s_review),
        None,
        AUTHOR_HUMAN,
        "Ada (reviewer)",
        None,
        "Overall this reads well. A few line notes below — mostly around the anchor module.",
    );
    // An AGENT reply into that global thread.
    post(
        db,
        &root_ws_a.id,
        &api_repo.id,
        Some(&s_review),
        Some(&c_global),
        AUTHOR_AGENT,
        "nyx-agent",
        None,
        "Acked. I'll address the anchor-module notes and push a follow-up commit.",
    );

    // LINE comment anchored on the KEEP line (line 4 of feature anchor.rs) — will RE-MAP.
    let keep_anchor =
        review_comment::capture_anchor(anchor_file, SIDE_NEW, 4, 4, None, &anchor_src);
    let c_keep = post_anchored(
        db,
        &root_ws_a.id,
        &api_repo.id,
        Some(&s_review),
        AUTHOR_HUMAN,
        "Ada (reviewer)",
        keep_anchor,
        "compute_total ignores discounts — is that intended here?",
    );
    // A HUMAN reply on the KEEP thread.
    post(
        db,
        &root_ws_a.id,
        &api_repo.id,
        Some(&s_review),
        Some(&c_keep),
        AUTHOR_HUMAN,
        "Grace (author)",
        None,
        "Good catch — discounts are applied upstream, but a comment would help.",
    );

    // LINE comment anchored on the GONE line (line 5) — will go OUTDATED after the advance.
    let gone_anchor =
        review_comment::capture_anchor(anchor_file, SIDE_NEW, 5, 5, None, &anchor_src);
    let _c_gone = post_anchored(
        db,
        &root_ws_a.id,
        &api_repo.id,
        Some(&s_review),
        AUTHOR_HUMAN,
        "Ada (reviewer)",
        gone_anchor,
        "legacy_side_effect() looks dead — can we drop it?",
    );

    // RANGE comment anchored on a multi-line span in a bulk-rewritten module.
    let mod_src = std::fs::read_to_string(format!("{api_path}/src/mod_0.rs")).unwrap_or_default();
    let range_anchor =
        review_comment::capture_anchor("src/mod_0.rs", SIDE_NEW, 3, 6, None, &mod_src);
    let c_range = post_anchored(
        db,
        &root_ws_a.id,
        &api_repo.id,
        Some(&s_review),
        AUTHOR_HUMAN,
        "Ada (reviewer)",
        range_anchor,
        "This whole block is duplicated across mod_1..mod_11 — extract a helper?",
    );

    // A comment that gets RESOLVED (human-only transition) on the web repo.
    let web_src = std::fs::read_to_string(format!(
        "{}/index.js",
        demo_member_path(db, &root_ws_a.id, &web_repo.id)
    ))
    .unwrap_or_default();
    let web_anchor = review_comment::capture_anchor("index.js", SIDE_NEW, 1, 1, None, &web_src);
    let c_resolved = post_anchored(
        db,
        &root_ws_a.id,
        &web_repo.id,
        Some(&s_review),
        AUTHOR_HUMAN,
        "Ada (reviewer)",
        web_anchor,
        "nit: prefer a named const over the literal 10 here.",
    );
    db.with_conn(|c| db::resolve_review_comment(c, &c_resolved))
        .expect("resolve web comment");

    // --- The GRILL: promote comments → tasks in proposed / structured ---------
    // proposed (human lâche brut).
    let comment_range = db
        .with_conn(|c| db::get_review_comment(c, &c_range))
        .expect("get range comment")
        .expect("range comment exists");
    let rt_proposed = db
        .with_conn(|c| {
            db::promote_comment_to_task(
                c,
                &comment_range,
                "Extract the duplicated module block into a helper",
                Some(db::TASK_SEVERITY_MEDIUM),
            )
        })
        .expect("promote range");
    let _ = rt_proposed;

    // structured (agent structures via MCP) — a structured task is coder-ready (the grill collapsed
    // to two states in v25, review follow-up #31; there is no human-only validate step).
    let comment_keep = db
        .with_conn(|c| db::get_review_comment(c, &c_keep))
        .expect("get keep comment")
        .expect("keep comment exists");
    let rt = db
        .with_conn(|c| {
            db::promote_comment_to_task(
                c,
                &comment_keep,
                "Document discount handling in compute_total",
                None,
            )
        })
        .expect("promote keep");
    db.with_conn(|c| {
        db::structure_task(
            c,
            &rt.id,
            "Document discount handling around compute_total",
            Some("Add a doc-comment clarifying that discounts are applied upstream; add a unit test pinning the behaviour."),
            Some(Some(db::TASK_SEVERITY_LOW)),
        )
    })
    .expect("structure keep task");

    // A second structured task (from the global comment).
    let comment_global = db
        .with_conn(|c| db::get_review_comment(c, &c_global))
        .expect("get global comment")
        .expect("global comment exists");
    let rt2 = db
        .with_conn(|c| {
            db::promote_comment_to_task(
                c,
                &comment_global,
                "Sweep the anchor module for the noted issues",
                None,
            )
        })
        .expect("promote global");
    db.with_conn(|c| {
        db::structure_task(
            c,
            &rt2.id,
            "Address the anchor-module review notes",
            Some("Group the anchor.rs notes into one follow-up: discount comment + drop legacy_side_effect."),
            Some(Some(db::TASK_SEVERITY_INFO)),
        )
    })
    .expect("structure global task");

    // ------------------------------------------------------------------------------------
    // REVIEW on the REAL nyx-v2 diff (Project B) — anchored on real changed files (living).
    // ------------------------------------------------------------------------------------
    let nyxv2_path = demo_member_path(db, &root_ws_b.id, &nyxv2_repo.id);
    // A global comment on the self-review.
    let c_self_global = post(
        db,
        &root_ws_b.id,
        &nyxv2_repo.id,
        Some(&s_self),
        None,
        AUTHOR_HUMAN,
        "Kris",
        None,
        "Self-review of the grafted review subsystem. Anchoring + re-map look solid; notes inline.",
    );
    post(
        db,
        &root_ws_b.id,
        &nyxv2_repo.id,
        Some(&s_self),
        Some(&c_self_global),
        AUTHOR_AGENT,
        "nyx-agent",
        None,
        "I can take the re-map edge cases as a follow-up task if you promote this.",
    );
    // Anchor a line + a range comment on a real changed file that exists in the working tree.
    anchor_real_file(
        db,
        &root_ws_b.id,
        &nyxv2_repo.id,
        &s_self,
        &nyxv2_path,
        "crates/nyx-core/src/review_comment.rs",
        33,
        33,
        "CONTEXT_RADIUS of 3 — worth a comment on why 3 is enough to disambiguate.",
    );
    anchor_real_file(
        db,
        &root_ws_b.id,
        &nyxv2_repo.id,
        &s_self,
        &nyxv2_path,
        "crates/nyx-core/src/review_comment.rs",
        178,
        200,
        "relocate(): the disambiguation tie-break is subtle — add a test for the equal-score case.",
    );

    // ------------------------------------------------------------------------------------
    // Managed COMMANDS (a fake dev server + friends) and TERMINALS in project A.
    // ------------------------------------------------------------------------------------
    let dev = db
        .with_conn(|c| {
            db::create_template(
                c,
                &proj_a.id,
                "dev server",
                "bun run dev",
                Some("api"),
                CommandSource::default(),
            )
        })
        .expect("cmd dev");
    db.with_conn(|c| db::set_restart_on_startup(c, &dev.id, true))
        .expect("restart-on-startup");
    db.with_conn(|c| {
        db::create_template(
            c,
            &proj_a.id,
            "typecheck (watch)",
            "bun run typecheck --watch",
            None,
            CommandSource::default(),
        )
    })
    .expect("cmd typecheck");
    db.with_conn(|c| {
        db::create_template(
            c,
            &proj_a.id,
            "unit tests (watch)",
            "bun test --watch",
            Some("api"),
            CommandSource::default(),
        )
    })
    .expect("cmd tests");

    for (cwd, label, ws) in [
        (api_path.as_str(), "api shell", &root_ws_a.id),
        (demo_root.as_str(), "repo root", &root_ws_a.id),
        (demo_root2.as_str(), "feature shell", &feat_ws.id),
    ] {
        let t = db
            .with_conn(|c| db::create_terminal(c, cwd, Some(label.to_string())))
            .expect("create terminal");
        db.with_conn(|c| db::pin_terminal_workspace(c, &t.id, ws))
            .expect("pin terminal");
    }

    println!("SETUP complete.");
    print_counts(db, &proj_a.id, &proj_b.id, &root_ws_a.id, &api_repo.id);
}

// ------------------------------------------------------------------------------------------
// REMAP — after the shell advanced the api repo (shift KEEP, drop GONE).
// ------------------------------------------------------------------------------------------

fn remap(db: &Db) {
    // Resolve project A's api repo + root workspace from the DB.
    let projects = db.with_conn(db::list_projects).expect("list projects");
    let proj_a = projects
        .iter()
        .find(|p| p.name == "Nyx Demo")
        .expect("Nyx Demo project");
    let repos = db
        .with_conn(|c| db::list_repositories(c, &proj_a.id))
        .expect("list repos");
    let api = repos.iter().find(|r| r.name == "api").expect("api repo");
    let workspaces = db
        .with_conn(|c| db::list_workspaces(c, &proj_a.id))
        .expect("list workspaces");
    let root_ws = workspaces
        .iter()
        .find(|w| w.is_root)
        .expect("root workspace");
    let api_path = db
        .with_conn(|c| repository::member_absolute_path(c, &root_ws.id, &api.id))
        .expect("member path query")
        .expect("api member path");

    let written = db
        .with_conn(|c| review_comment::remap_repo_comments(c, &api_path, &root_ws.id, &api.id))
        .expect("remap");
    println!("REMAP: re-anchored/outdated {written} comment(s) on api ({api_path}).");

    // Report the resulting anchor states so the run is self-verifying.
    let comments = db
        .with_conn(|c| db::list_review_comments(c, &root_ws.id, &api.id, None))
        .expect("list comments");
    for cm in comments
        .iter()
        .filter(|c| c.file_path.as_deref() == Some("src/anchor.rs"))
    {
        println!(
            "  anchor.rs comment {} → line_start={:?} outdated={} : {}",
            &cm.id[..8],
            cm.line_start,
            cm.outdated,
            cm.body
        );
    }
}

// ------------------------------------------------------------------------------------------
// Helpers.
// ------------------------------------------------------------------------------------------

enum TaskKind {
    Slice,
    Human,
    Review,
}
impl TaskKind {
    fn as_str(&self) -> &'static str {
        match self {
            TaskKind::Slice => db::TASK_KIND_SLICE,
            TaskKind::Human => db::TASK_KIND_HUMAN,
            TaskKind::Review => db::TASK_KIND_REVIEW,
        }
    }
}

fn create_spec(
    db: &Db,
    project_id: &str,
    workspace_id: &str,
    title: &str,
    problem: &str,
    solution: &str,
) -> String {
    let sections = SpecSections {
        problem: problem.to_string(),
        solution: solution.to_string(),
        implementation_decisions: String::new(),
        testing_decisions: String::new(),
        user_stories: String::new(),
        out_of_scope: String::new(),
    };
    let (spec, _rev) = db
        .with_conn(|c| db::create_spec_with_workspace(c, project_id, workspace_id, title, sections))
        .unwrap_or_else(|e| panic!("create spec '{title}': {e:?}"));
    println!("spec '{}' -> {}", title, spec.id);
    spec.id
}

fn author_section(db: &Db, spec_id: &str, section: SpecSection, value: &str) {
    db.with_conn(|c| db::update_active_revision_section(c, spec_id, section, value))
        .expect("update section");
}

fn transition(db: &Db, spec_id: &str, t: SpecTransition) {
    // All lifecycle transitions are driven with user_confirmed=true (the seed IS the human).
    db.with_conn(|c| db::transition_spec(c, spec_id, t, true))
        .unwrap_or_else(|e| panic!("transition {t:?} on {spec_id}: {e:?}"));
}

fn set_phases(db: &Db, spec_id: &str) {
    let phases = r#"[{"number":1,"name":"Foundations"},{"number":2,"name":"Delivery"},{"number":3,"name":"Hardening"}]"#;
    db.with_conn(|c| db::set_spec_phases(c, spec_id, phases))
        .expect("set phases");
}

fn task(
    db: &Db,
    project_id: &str,
    spec_id: &str,
    title: &str,
    kind: TaskKind,
    phase: Option<i32>,
) -> String {
    task_full(
        db, project_id, spec_id, title, kind, phase, None, None, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn task_full(
    db: &Db,
    project_id: &str,
    spec_id: &str,
    title: &str,
    kind: TaskKind,
    phase: Option<i32>,
    description: Option<&str>,
    acceptance: Option<&str>,
    verification: Option<&str>,
) -> String {
    let severity = match kind {
        TaskKind::Review => Some(db::TASK_SEVERITY_HIGH.to_string()),
        _ => None,
    };
    // task_add now groups BY NAME (the plan materializes from the names, get-or-create). The
    // multi-phase demo specs pre-seed their plan via `set_phases` with these exact names, so
    // mapping the demo's phase number back to its name makes get-or-create REUSE the seeded
    // number (the plan and the tasks stay coherent, numbers unchanged).
    let phase_name = phase.map(|n| {
        match n {
            1 => "Foundations",
            2 => "Delivery",
            3 => "Hardening",
            _ => "Extra",
        }
        .to_string()
    });
    let input = NewTaskInput {
        kind: kind.as_str().to_string(),
        type_: db::TASK_TYPE_IMPLEMENTATION.to_string(),
        priority: db::TASK_PRIORITY_MED.to_string(),
        severity,
        description: description.map(str::to_string),
        acceptance_criteria: acceptance.map(str::to_string),
        verification_command: verification.map(str::to_string),
        spec_id: Some(spec_id.to_string()),
        parent_task_id: None,
        phase_name,
        from_comment_id: None,
        grill_state: None,
        round: None,
    };
    let t = db
        .with_conn(|c| db::create_task(c, project_id, title, input.clone()))
        .expect("create task");
    t.id
}

fn start(db: &Db, task_id: &str) {
    db.with_conn(|c| db::transition_task(c, task_id, TaskTransition::Start, None))
        .expect("task start");
}
fn done(db: &Db, task_id: &str) {
    db.with_conn(|c| db::transition_task(c, task_id, TaskTransition::Done, None))
        .expect("task done");
}
fn block(db: &Db, task_id: &str) {
    db.with_conn(|c| {
        db::transition_task(
            c,
            task_id,
            TaskTransition::Block,
            Some("blocked pending a decision"),
        )
    })
    .expect("task block");
}
fn skip(db: &Db, task_id: &str) {
    db.with_conn(|c| {
        db::transition_task(c, task_id, TaskTransition::Skip, Some("dropped from scope"))
    })
    .expect("task skip");
}

#[allow(clippy::too_many_arguments)]
fn post(
    db: &Db,
    workspace_id: &str,
    repository_id: &str,
    spec_id: Option<&str>,
    parent_id: Option<&str>,
    author_type: &str,
    author_name: &str,
    anchor: Option<CommentAnchor>,
    body: &str,
) -> String {
    let cm = db
        .with_conn(|c| {
            db::insert_review_comment(
                c,
                NewComment {
                    workspace_id: workspace_id.to_string(),
                    repository_id: repository_id.to_string(),
                    spec_id: spec_id.map(str::to_string),
                    parent_id: parent_id.map(str::to_string),
                    author_type: author_type.to_string(),
                    author_name: author_name.to_string(),
                    anchor,
                    body: body.to_string(),
                },
            )
        })
        .expect("insert review comment");
    cm.id
}

#[allow(clippy::too_many_arguments)]
fn post_anchored(
    db: &Db,
    workspace_id: &str,
    repository_id: &str,
    spec_id: Option<&str>,
    author_type: &str,
    author_name: &str,
    anchor: CommentAnchor,
    body: &str,
) -> String {
    post(
        db,
        workspace_id,
        repository_id,
        spec_id,
        None,
        author_type,
        author_name,
        Some(anchor),
        body,
    )
}

#[allow(clippy::too_many_arguments)]
fn anchor_real_file(
    db: &Db,
    workspace_id: &str,
    repository_id: &str,
    spec_id: &str,
    repo_abs: &str,
    rel: &str,
    line_start: i32,
    line_end: i32,
    body: &str,
) {
    let content = std::fs::read_to_string(format!("{repo_abs}/{rel}")).unwrap_or_default();
    let anchor =
        review_comment::capture_anchor(rel, SIDE_NEW, line_start, line_end, None, &content);
    post_anchored(
        db,
        workspace_id,
        repository_id,
        Some(spec_id),
        AUTHOR_HUMAN,
        "Kris",
        anchor,
        body,
    );
}

fn demo_member_path(db: &Db, workspace_id: &str, repository_id: &str) -> String {
    db.with_conn(|c| repository::member_absolute_path(c, workspace_id, repository_id))
        .expect("member path query")
        .expect("member path resolves")
}

fn print_counts(db: &Db, proj_a: &str, proj_b: &str, ws_a: &str, api_repo: &str) {
    let specs_a = db
        .with_conn(|c| db::list_specs(c, Some(proj_a)))
        .map(|v| v.len())
        .unwrap_or(0);
    let specs_b = db
        .with_conn(|c| db::list_specs(c, Some(proj_b)))
        .map(|v| v.len())
        .unwrap_or(0);
    let comments = db
        .with_conn(|c| db::list_review_comments(c, ws_a, api_repo, None))
        .map(|v| v.len())
        .unwrap_or(0);
    let tasks = db
        .with_conn(|c| db::list_review_tasks(c, ws_a, api_repo, None))
        .map(|v| v.len())
        .unwrap_or(0);
    println!(
        "counts: specs(A)={specs_a} specs(B)={specs_b} api_review_comments={comments} api_review_tasks={tasks}"
    );
}
