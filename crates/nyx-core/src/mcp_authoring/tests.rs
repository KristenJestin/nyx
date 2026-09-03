//! Tests for the v2.8 graph-authoring MCP verbs (task `01KY71J2GF83Z2ATBRA5125FPV`): the
//! workflow_create/fork/assign + node CRUD surface, the tree it re-builds, and the coherence
//! REFUSALS (dangling parent, leaf-with-children, cycle, second root, non-object config).

use super::*;
use crate::db::Db;
use crate::mcp_engine::EngineRuntime;

/// A memory DB with one project. Returns `(db, project_id)`.
fn seeded() -> (Db, String) {
    let db = Db::in_memory();
    let project_id = db.with_conn(|c| {
        let (project, _ws) = db::create_project(c, "Proj", "/tmp/authoring-mcp", None).unwrap();
        project.id
    });
    (db, project_id)
}

fn id_of(out: &ToolOutcome, path: &str) -> String {
    out.result[path]["id"].as_str().unwrap().to_string()
}

/// Create a workflow (via the verb) and return its id.
fn make_workflow(db: &Db, name: &str, project_id: Option<&str>) -> String {
    let mut args = json!({ "name": name });
    if let Some(p) = project_id {
        args["project_id"] = json!(p);
    }
    let out = workflow_create(db, &args).expect("workflow_create ok");
    assert_eq!(out.effects, vec![ChangedTopic::Workflows]);
    id_of(&out, "workflow")
}

/// Add a node and return its id. Supplies a minimal VALID config for the types that now require one
/// (M1: spawn needs agent_id, judge a prompt, transition a target) so a helper add never trips the
/// config validation — a test that WANTS an invalid config calls `node_add` directly.
fn add_node(db: &Db, wf: &str, node_type: &str, parent: Option<&str>, label: &str) -> String {
    let mut args = json!({ "workflow_id": wf, "type": node_type, "label": label });
    if let Some(p) = parent {
        args["parent_id"] = json!(p);
    }
    match node_type {
        "spawn_subagent" => args["config"] = json!({ "agent_id": label }),
        "judge" => args["config"] = json!({ "prompt": "clear enough?" }),
        "transition" => args["config"] = json!({ "target_state": "in_review" }),
        _ => {}
    }
    let out = node_add(db, &args).expect("node_add ok");
    assert_eq!(out.effects, vec![ChangedTopic::Workflows]);
    id_of(&out, "node")
}

/// A root `sequence` whose first child is an `ask_human` writing `key` — the MINIMAL shape rule (4)
/// accepts for a sensitive node ([`crate::mcp_authoring::sensitive_node_gate`]): the question
/// necessarily runs first, and `key` is what the node's own `gate_on` then reads. Returns the root, so
/// a test about something ELSE (a config field, a move) can author a sensitive node without
/// re-spelling the gate each time. A test ABOUT the gate builds its shape by hand.
fn gated_root(db: &Db, wf: &str, key: &str) -> String {
    let root = add_node(db, wf, "sequence", None, "root");
    node_add(
        db,
        &json!({
            "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
            "label": "approve?", "config": { "prompt": "approve?", "output_key": key }
        }),
    )
    .expect("the gate question is added");
    root
}

// --- create / assign -------------------------------------------------------

/// DONE-CRITERION ("create/fork/assign posent projects.workflow_id"): `workflow_create` with a
/// project BINDS the workflow to it (`workflows.project_id`); without one it is a global default.
#[test]
fn create_binds_project_or_stays_global() {
    let (db, project) = seeded();

    let owned = workflow_create(
        &db,
        &json!({ "name": "owned", "scope": "ndev", "project_id": project }),
    )
    .unwrap();
    assert_eq!(owned.result["workflow"]["project_id"], json!(project));
    assert_eq!(owned.result["workflow"]["scope"], "ndev");

    let global = workflow_create(&db, &json!({ "name": "global" })).unwrap();
    assert_eq!(global.result["workflow"]["project_id"], Value::Null);
    assert_eq!(global.result["workflow"]["scope"], "custom");

    // An unknown project is refused.
    let bad = workflow_create(&db, &json!({ "name": "x", "project_id": "nope" }));
    assert_eq!(bad.unwrap_err().code, "invalid_id");
}

/// Lot 2 — the SUB-WORKFLOW-ONLY scopes `coder` / `prototyper` are AUTHORABLE (a `sub_workflow`
/// node needs them as a resolvable child target), and off-vocabulary scopes are still refused.
#[test]
fn create_accepts_the_sub_workflow_scopes_and_rejects_unknown() {
    let (db, project) = seeded();

    for scope in ["coder", "prototyper"] {
        let out = workflow_create(
            &db,
            &json!({ "name": scope, "scope": scope, "project_id": project }),
        )
        .unwrap_or_else(|_| panic!("workflow_create scope={scope} ok"));
        assert_eq!(out.result["workflow"]["scope"], scope);
    }

    // A scope outside the vocabulary is refused, and the message lists the new scopes.
    let err = workflow_create(&db, &json!({ "name": "x", "scope": "banane" })).unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(err.message.contains("coder"));
    assert!(err.message.contains("prototyper"));
}

/// DONE-CRITERION (assign): `workflow_assign` (re)binds a workflow to a project; unknown ids refused.
#[test]
fn assign_sets_project() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "global", None);

    let assigned =
        workflow_assign(&db, &json!({ "workflow_id": wf, "project_id": project })).unwrap();
    assert_eq!(assigned.effects, vec![ChangedTopic::Workflows]);
    assert_eq!(assigned.result["workflow"]["project_id"], json!(project));
    // Persisted.
    let row = db.with_conn(|c| db::get_workflow(c, &wf)).unwrap().unwrap();
    assert_eq!(row.project_id.as_deref(), Some(project.as_str()));

    assert_eq!(
        workflow_assign(
            &db,
            &json!({ "workflow_id": "nope", "project_id": project })
        )
        .unwrap_err()
        .code,
        "invalid_id"
    );
    assert_eq!(
        workflow_assign(&db, &json!({ "workflow_id": wf, "project_id": "nope" }))
            .unwrap_err()
            .code,
        "invalid_id"
    );
}

// --- node CRUD rebuilds the tree ------------------------------------------

/// DONE-CRITERION ("node CRUD reconstruit l'arbre"): add root + children, update, reorder, remove —
/// the tree reconstructs (via the interpreter's own `Tree::from_rows`) after every step, and the
/// first parent-less node wires the workflow's root pointer.
#[test]
fn node_crud_rebuilds_the_tree() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "flow", Some(&project));

    // First node (parent-less) → becomes the root.
    let root = add_node(&db, &wf, "sequence", None, "root");
    let wf_row = db.with_conn(|c| db::get_workflow(c, &wf)).unwrap().unwrap();
    assert_eq!(
        wf_row.root_node_id.as_deref(),
        Some(root.as_str()),
        "the first parent-less node wires the root pointer"
    );

    let a = add_node(&db, &wf, "spawn_subagent", Some(&root), "coder");
    let b = add_node(&db, &wf, "wait", Some(&root), "pause");

    // The tree reconstructs from the rows alone.
    let assert_rebuilds = |db: &Db| {
        let nodes = db.with_conn(|c| db::list_nodes(c, &wf)).unwrap();
        let root_id = db
            .with_conn(|c| db::get_workflow(c, &wf))
            .unwrap()
            .unwrap()
            .root_node_id;
        Tree::from_rows(&nodes, root_id.as_deref()).expect("tree rebuilds");
        nodes.len()
    };
    assert_eq!(assert_rebuilds(&db), 3);

    // update: change a node's config; still rebuilds.
    let upd = node_update(
        &db,
        &json!({ "node_id": a, "config": { "agent_id": "auditeur" } }),
    )
    .unwrap();
    assert_eq!(upd.effects, vec![ChangedTopic::Workflows]);
    let a_row = db.with_conn(|c| db::get_node(c, &a)).unwrap().unwrap();
    assert!(a_row.config.contains("auditeur"));

    // reorder: swap the two children's order; still rebuilds.
    node_reorder(&db, &json!({ "node_id": b, "order_index": 0 })).unwrap();
    node_reorder(&db, &json!({ "node_id": a, "order_index": 1 })).unwrap();
    let children = db.with_conn(|c| db::child_nodes(c, &root)).unwrap();
    assert_eq!(children[0].id, b, "b reordered to the front");

    // remove: delete a child; the tree rebuilds with 2 nodes.
    let rm = node_remove(&db, &json!({ "node_id": b })).unwrap();
    assert_eq!(rm.result["removed"], true);
    assert_eq!(assert_rebuilds(&db), 2);
}

/// A leaf can be a single-node root, but a leaf may NOT gain a child.
#[test]
fn refuses_leaf_with_children() {
    let (db, _p) = seeded();
    let wf = make_workflow(&db, "flow", None);
    let root = add_node(&db, &wf, "sequence", None, "root");
    let leaf = add_node(&db, &wf, "wait", Some(&root), "leaf");

    let refused = node_add(
        &db,
        &json!({ "workflow_id": wf, "type": "note", "parent_id": leaf }),
    );
    assert_eq!(refused.unwrap_err().code, "invalid_state");
    // Rolled back: still 2 nodes.
    assert_eq!(
        db.with_conn(|c| db::list_nodes(c, &wf)).unwrap().len(),
        2,
        "the incoherent add was rolled back"
    );
}

/// A second parent-less node (a second root) is refused.
#[test]
fn refuses_second_root() {
    let (db, _p) = seeded();
    let wf = make_workflow(&db, "flow", None);
    let _root = add_node(&db, &wf, "sequence", None, "root");

    let refused = node_add(&db, &json!({ "workflow_id": wf, "type": "sequence" }));
    assert_eq!(refused.unwrap_err().code, "invalid_state");
    assert_eq!(
        db.with_conn(|c| db::list_nodes(c, &wf)).unwrap().len(),
        1,
        "the second root was rolled back"
    );
}

/// A parent living in ANOTHER workflow (dangling for this one) is refused.
#[test]
fn refuses_foreign_parent() {
    let (db, _p) = seeded();
    let wf1 = make_workflow(&db, "one", None);
    let wf2 = make_workflow(&db, "two", None);
    let root2 = add_node(&db, &wf2, "sequence", None, "root2");

    let refused = node_add(
        &db,
        &json!({ "workflow_id": wf1, "type": "wait", "parent_id": root2 }),
    );
    assert_eq!(refused.unwrap_err().code, "invalid_state");
    // An entirely unknown parent → invalid_id.
    assert_eq!(
        node_add(
            &db,
            &json!({ "workflow_id": wf1, "type": "wait", "parent_id": "ghost" })
        )
        .unwrap_err()
        .code,
        "invalid_id"
    );
}

/// A reparent that would create a CYCLE (a node under its own descendant) is refused and rolled back.
#[test]
fn refuses_cycle_on_reparent() {
    let (db, _p) = seeded();
    let wf = make_workflow(&db, "flow", None);
    let root = add_node(&db, &wf, "sequence", None, "root");
    let a = add_node(&db, &wf, "sequence", Some(&root), "a");
    let b = add_node(&db, &wf, "sequence", Some(&a), "b");

    // Move A under B (its own child) → cycle.
    let refused = node_reorder(
        &db,
        &json!({ "node_id": a, "order_index": 0, "parent_id": b }),
    );
    assert_eq!(refused.unwrap_err().code, "invalid_state");
    // Rolled back: A's parent is still root.
    let a_row = db.with_conn(|c| db::get_node(c, &a)).unwrap().unwrap();
    assert_eq!(a_row.parent_id.as_deref(), Some(root.as_str()));
}

/// A `config` that is not a JSON object is refused up front (invalid_argument).
#[test]
fn refuses_non_object_config() {
    let (db, _p) = seeded();
    let wf = make_workflow(&db, "flow", None);
    let refused = node_add(
        &db,
        &json!({ "workflow_id": wf, "type": "note", "config": "not-an-object" }),
    );
    assert_eq!(refused.unwrap_err().code, "invalid_argument");
    assert!(
        db.with_conn(|c| db::list_nodes(c, &wf)).unwrap().is_empty(),
        "nothing persisted"
    );
}

// --- slot_bind: the per-project half of a SHARED graph's gate --------------

/// Register a managed command in `project` and return its INSTANCE id (what a slot binds — the thing
/// that exists per workspace, not the template).
fn command_instance(db: &Db, project: &str, name: &str) -> String {
    db.with_conn(|c| {
        let tpl = db::create_template(c, project, name, "true", None, Default::default())
            .expect("template");
        db::instance_ids_for_template(c, &tpl.id)
            .expect("instances")
            .into_iter()
            .next()
            .expect("one instance materialized")
    })
}

/// `slot_bind` is THE gesture that replaces forking a built-in to arm its gate: it writes PROJECT
/// data (which commands answer a slot) and touches no graph at all. It upserts per (project, slot),
/// it is per project, an empty list UNBINDS, and a command id nobody registered is REFUSED at the
/// write — a mistyped id would otherwise sit there looking armed and only surface at the gate.
#[test]
fn slot_bind_writes_project_data_and_forks_nothing() {
    let (db, project) = seeded();
    let build = command_instance(&db, &project, "build");
    let test = command_instance(&db, &project, "test");

    let out = slot_bind(
        &db,
        &json!({ "project_id": project, "slot": "build_check", "command_ids": [build, test] }),
    )
    .expect("slot_bind ok");
    assert_eq!(out.effects, vec![ChangedTopic::Workflows]);
    assert_eq!(out.result["slot"]["slot"], json!("build_check"));
    assert_eq!(
        db.with_conn(|c| db::command_slot_ids(c, &project, "build_check"))
            .unwrap(),
        vec![build.clone(), test.clone()],
        "the ids are stored IN ORDER — the gate runs them in sequence",
    );
    assert!(
        db.with_conn(|c| db::list_workflows(c, Some(&project)))
            .unwrap()
            .is_empty(),
        "arming a gate created NO workflow — nothing was forked, so nothing stopped upgrading",
    );

    // Re-binding REPLACES; the answer carries the whole set so the caller sees what is still unbound.
    let again = slot_bind(
        &db,
        &json!({ "project_id": project, "slot": "build_check", "command_ids": [test] }),
    )
    .expect("re-bind ok");
    assert_eq!(
        db.with_conn(|c| db::command_slot_ids(c, &project, "build_check"))
            .unwrap(),
        vec![test.clone()],
    );
    assert_eq!(
        again.result["slots"].as_array().expect("the project's slots").len(),
        1,
        "one row per (project, slot), whatever the number of binds",
    );

    // An EMPTY list UNBINDS — the gate goes back to refusing, which is the honest undo.
    slot_bind(
        &db,
        &json!({ "project_id": project, "slot": "build_check", "command_ids": [] }),
    )
    .expect("unbind ok");
    assert!(db
        .with_conn(|c| db::command_slot_ids(c, &project, "build_check"))
        .unwrap()
        .is_empty());

    // A command nobody registered is refused HERE, not three ticks later inside a PTY.
    let bad = slot_bind(
        &db,
        &json!({ "project_id": project, "slot": "build_check", "command_ids": ["not-an-instance"] }),
    )
    .unwrap_err();
    assert_eq!(bad.code, "invalid_id");
    assert!(
        bad.message.contains("not-an-instance") && bad.message.contains("list_commands"),
        "it names the bad id and where the good one comes from: {}",
        bad.message
    );

    // An unknown project, and a blank slot name, are refused too.
    assert_eq!(
        slot_bind(
            &db,
            &json!({ "project_id": "nope", "slot": "build_check", "command_ids": [] })
        )
        .unwrap_err()
        .code,
        "invalid_id"
    );
    assert_eq!(
        slot_bind(
            &db,
            &json!({ "project_id": project, "slot": "   ", "command_ids": [] })
        )
        .unwrap_err()
        .code,
        "invalid_argument"
    );
}

/// Register a managed command with a given COMMAND LINE (and optionally declared a long-running
/// service), returning its instance id — the two facts the non-terminating guard below reads.
fn command_instance_running(
    db: &Db,
    project: &str,
    name: &str,
    cmdline: &str,
    service: bool,
) -> String {
    db.with_conn(|c| {
        let tpl = db::create_template(c, project, name, cmdline, None, Default::default())
            .expect("template");
        if service {
            db::set_restart_on_startup(c, &tpl.id, true).expect("declare a service");
        }
        db::instance_ids_for_template(c, &tpl.id)
            .expect("instances")
            .into_iter()
            .next()
            .expect("one instance materialized")
    })
}

/// L1 — a command that OBVIOUSLY never returns is refused AT THE BINDING, where the refusal costs
/// nothing. Bound, it would hold the gate for the whole time budget on EVERY tick and never yield a
/// verdict, so the run stalls there for good — discovered 90 seconds at a time.
///
/// The three candidates are the ones the demo workspace really ships, and `list_commands` offers them
/// to a gate exactly like any other.
#[test]
fn slot_bind_refuses_a_command_that_never_returns() {
    let (db, project) = seeded();
    let watch_test = command_instance_running(&db, &project, "t", "bun test --watch", false);
    let watch_tsc =
        command_instance_running(&db, &project, "tc", "bun run typecheck --watch", false);
    let dev = command_instance_running(&db, &project, "dev", "bun run dev", true);

    for (id, expected) in [
        (&watch_test, "WATCH mode"),
        (&watch_tsc, "WATCH mode"),
        (&dev, "restart_on_startup"),
    ] {
        let refused = slot_bind(
            &db,
            &json!({ "project_id": project, "slot": "build_check", "command_ids": [id] }),
        )
        .unwrap_err();
        assert_eq!(refused.code, "invalid_argument");
        assert_eq!(
            refused
                .data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some("command_never_returns"),
        );
        assert!(
            refused.message.contains(expected),
            "the refusal says WHICH fact makes it non-terminating: {}",
            refused.message
        );
        assert!(
            refused.message.contains("one-shot"),
            "and names the way out: {}",
            refused.message
        );
    }
    assert!(
        db.with_conn(|c| db::command_slot_ids(c, &project, "build_check"))
            .unwrap()
            .is_empty(),
        "a refused binding writes nothing",
    );

    // The refusal rests on a FACT about the command, never on a word appearing somewhere inside it:
    // a one-shot check whose ARGUMENT merely mentions watching binds like any other.
    let one_shot = command_instance_running(
        &db,
        &project,
        "unit",
        "bun test src/watch-mode.test.ts",
        false,
    );
    slot_bind(
        &db,
        &json!({ "project_id": project, "slot": "build_check", "command_ids": [one_shot] }),
    )
    .expect("a terminating check binds");
    assert_eq!(
        db.with_conn(|c| db::command_slot_ids(c, &project, "build_check"))
            .unwrap(),
        vec![one_shot],
    );
}

/// A gate arms itself ONE way. Setting both `command_ids` and a `slot` is a question with two
/// answers: the runtime resolves the slot, so the ids beside it would read as the truth while running
/// nothing — the quietest possible way for a gate to check something other than what its config says.
/// Refused at the WRITE, where a human is still looking.
#[test]
fn a_gate_may_not_name_both_its_own_commands_and_a_slot() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let root = add_node(&db, &wf, "sequence", None, "root");

    let err = node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "gate_check", "label": "checks", "parent_id": root,
            "config": { "command_ids": ["inst"], "slot": "build_check", "blocking": true }
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("never both"),
        "the refusal says which of the two shapes to keep: {}",
        err.message
    );

    // Each ALONE is valid — including a slot beside the empty list serde leaves behind.
    let ids_only = node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "gate_check", "label": "own", "parent_id": root,
            "config": { "command_ids": ["inst"], "blocking": true }
        }),
    );
    assert!(ids_only.is_ok(), "a gate naming its own commands stays valid");
    let slot_only = node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "gate_check", "label": "slotted", "parent_id": root,
            "config": { "command_ids": [], "slot": "build_check", "blocking": true }
        }),
    );
    assert!(slot_only.is_ok(), "a slot gate with an empty id list is the ordinary shape");
}

// --- fork ------------------------------------------------------------------

/// DONE-CRITERION (fork): `workflow_fork` deep-copies the whole tree (new ids, re-wired parents,
/// re-pointed root, `forked_from` = source), optionally owned by a project; the source is untouched.
#[test]
fn fork_deep_copies_the_tree() {
    let (db, project) = seeded();
    let src = make_workflow(&db, "src", None);
    let root = add_node(&db, &src, "sequence", None, "root");
    let loop_n = add_node(&db, &src, "loop", Some(&root), "phase");
    let _coder = add_node(&db, &src, "spawn_subagent", Some(&loop_n), "coder");
    let _gate = add_node(&db, &src, "gate_check", Some(&loop_n), "checks");

    let forked = workflow_fork(
        &db,
        &json!({ "workflow_id": src, "name": "fork", "project_id": project }),
    )
    .unwrap();
    assert_eq!(forked.effects, vec![ChangedTopic::Workflows]);
    let fork_id = id_of(&forked, "workflow");
    assert_eq!(forked.result["workflow"]["forked_from"], json!(src));
    assert_eq!(forked.result["workflow"]["project_id"], json!(project));
    assert!(forked.result["workflow"]["root_node_id"].is_string());

    // Same node count, DISTINCT ids, and the fork rebuilds as a valid tree.
    let src_nodes = db.with_conn(|c| db::list_nodes(c, &src)).unwrap();
    let fork_nodes = db.with_conn(|c| db::list_nodes(c, &fork_id)).unwrap();
    assert_eq!(fork_nodes.len(), src_nodes.len());
    let src_ids: std::collections::HashSet<&str> =
        src_nodes.iter().map(|n| n.id.as_str()).collect();
    for fnode in &fork_nodes {
        assert!(!src_ids.contains(fnode.id.as_str()), "fork has fresh ids");
    }
    let fork_root = forked.result["workflow"]["root_node_id"]
        .as_str()
        .map(str::to_string);
    Tree::from_rows(&fork_nodes, fork_root.as_deref()).expect("fork rebuilds");

    // The source is untouched (still un-owned).
    let src_row = db
        .with_conn(|c| db::get_workflow(c, &src))
        .unwrap()
        .unwrap();
    assert_eq!(src_row.project_id, None);

    // Unknown source → invalid_id (project_id is required now, so pass it to reach the source check).
    assert_eq!(
        workflow_fork(&db, &json!({ "workflow_id": "nope", "project_id": project }))
            .unwrap_err()
            .code,
        "invalid_id"
    );

    // A fork now REQUIRES a project_id (no more rogue unowned forks — review S3).
    assert_eq!(
        workflow_fork(&db, &json!({ "workflow_id": src }))
            .unwrap_err()
            .code,
        "invalid_argument"
    );
}

// --- dispatch --------------------------------------------------------------

/// The sub-dispatcher routes every authoring tool (and only those).
#[test]
fn dispatch_routes_authoring_tools() {
    let (db, project) = seeded();
    let created = dispatch_authoring_tool(
        &db,
        WORKFLOW_CREATE_TOOL,
        &json!({ "name": "via dispatch", "project_id": project }),
    )
    .expect("routed")
    .expect("ok");
    assert_eq!(created.effects, vec![ChangedTopic::Workflows]);

    for tool in AUTHORING_TOOLS {
        assert!(
            dispatch_authoring_tool(&db, tool, &json!({})).is_some(),
            "{tool} must route"
        );
    }
    assert!(
        dispatch_authoring_tool(&db, "not_an_authoring_tool", &json!({})).is_none(),
        "a foreign tool falls through"
    );
}

// --- v2.8 review — the contract polish (m1) + the read (S1) / delete (S4) verbs ----------------

/// m1 — a node in a response carries `type` (the input param name), NOT the Diesel column `type_`.
#[test]
fn node_responses_use_type_not_the_diesel_field() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let out = node_add(
        &db,
        &json!({ "workflow_id": wf, "type": "spawn_subagent", "label": "coder", "config": { "agent_id": "coder" } }),
    )
    .unwrap();
    assert_eq!(out.result["node"]["type"], "spawn_subagent", "the contract echoes `type`");
    assert!(
        out.result["node"].get("type_").is_none(),
        "the Diesel column name must not leak into the tool contract"
    );
}

/// S1 — `workflow_list` returns a project's OWN workflows PLUS the global/built-in defaults; with no
/// project it returns just the globals. A pure READ (no `changed` effect).
#[test]
fn workflow_list_returns_project_plus_globals() {
    let (db, project) = seeded();
    let owned = make_workflow(&db, "owned", Some(&project));
    let global = make_workflow(&db, "global", None);

    let out = workflow_list(&db, &json!({ "project_id": project })).unwrap();
    assert!(out.effects.is_empty(), "a read has no changed effect");
    let ids: Vec<&str> = out.result["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&owned.as_str()) && ids.contains(&global.as_str()));

    let globals_only = workflow_list(&db, &json!({})).unwrap();
    let g_ids: Vec<&str> = globals_only.result["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap())
        .collect();
    assert!(g_ids.contains(&global.as_str()) && !g_ids.contains(&owned.as_str()));
}

/// S1 — `workflow_get` returns the workflow + its whole tree, the node ids carried as `type` (m1);
/// an unknown workflow is `invalid_id`.
#[test]
fn workflow_get_returns_the_tree_with_node_ids() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let root = add_node(&db, &wf, "sequence", None, "root");
    let child = add_node(&db, &wf, "spawn_subagent", Some(&root), "coder");

    let out = workflow_get(&db, &json!({ "workflow_id": wf })).unwrap();
    assert!(out.effects.is_empty());
    assert_eq!(out.result["workflow"]["id"], wf);
    let nodes = out.result["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(
        nodes.iter().any(|n| n["id"] == json!(child) && n["type"] == "spawn_subagent"),
        "the child is listed with its id + `type` (not type_)"
    );
    assert!(nodes.iter().all(|n| n.get("type_").is_none()));

    assert_eq!(
        workflow_get(&db, &json!({ "workflow_id": "nope" }))
            .unwrap_err()
            .code,
        "invalid_id"
    );
}

/// S1 — `run_list` returns a workflow's runs (newest first). A pure READ.
#[test]
fn run_list_returns_a_workflows_runs() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    db.with_conn(|c| db::create_workflow_run(c, &wf, None, None, "{}"))
        .unwrap();

    let out = run_list(&db, &json!({ "workflow_id": wf })).unwrap();
    assert!(out.effects.is_empty());
    assert_eq!(out.result["runs"].as_array().unwrap().len(), 1);
}

/// S4 (guarded by S2) — `workflow_delete` removes a PROJECT-OWNED workflow (cascading its nodes +
/// runs) but REFUSES a built-in default (`project_id IS NULL`) with `builtin_immutable`.
#[test]
fn workflow_delete_removes_project_owned_but_refuses_a_builtin() {
    let (db, project) = seeded();

    // Project-owned → deleted; a subsequent read is invalid_id.
    let owned = make_workflow(&db, "owned", Some(&project));
    add_node(&db, &owned, "note", None, "n");
    db.with_conn(|c| db::create_workflow_run(c, &owned, None, None, "{}"))
        .unwrap();
    let del = workflow_delete(&db, &json!({ "workflow_id": owned })).unwrap();
    assert_eq!(del.effects, vec![ChangedTopic::Workflows]);
    assert_eq!(del.result["deleted"], true);
    assert_eq!(
        workflow_get(&db, &json!({ "workflow_id": owned }))
            .unwrap_err()
            .code,
        "invalid_id",
        "the workflow (and its cascaded nodes + runs) is gone"
    );

    // A SEEDED built-in default (project_id NULL + a seeded scope) is immutable — refused. Created
    // via the DB layer, exactly as seeding does (the verb won't author an unowned ndev).
    let builtin = db
        .with_conn(|c| db::create_workflow(c, None, "ndev-default", db::WORKFLOW_SCOPE_NDEV, None))
        .unwrap()
        .id;
    let err = workflow_delete(&db, &json!({ "workflow_id": builtin })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("builtin_immutable")
    );
}

// --- v2.8 review — S2 immutability, M1 config validation, S3 fork guard, m2 message -----------

/// S2 — a SEEDED built-in default (unowned + a seeded scope) is immutable to authoring (node_add
/// refused), while a project-owned FORK of it is editable. An unowned `custom` template stays mutable.
#[test]
fn seeded_builtin_is_immutable_but_a_fork_and_a_template_are_editable() {
    let (db, project) = seeded();
    let builtin = db
        .with_conn(|c| db::create_workflow(c, None, "ndev", db::WORKFLOW_SCOPE_NDEV, None))
        .unwrap()
        .id;

    let err = node_add(&db, &json!({ "workflow_id": builtin, "type": "sequence" })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("builtin_immutable")
    );

    // Forking it into the project yields an EDITABLE copy.
    let fork = workflow_fork(&db, &json!({ "workflow_id": builtin, "project_id": project })).unwrap();
    let fork_id = id_of(&fork, "workflow");
    node_add(&db, &json!({ "workflow_id": fork_id, "type": "sequence", "label": "root" }))
        .expect("the fork is editable");

    // An unowned `custom` TEMPLATE is NOT a protected default — it stays mutable.
    let template = make_workflow(&db, "template", None);
    node_add(&db, &json!({ "workflow_id": template, "type": "sequence", "label": "root" }))
        .expect("an unowned custom template is mutable");
}

/// M1 — an inexecutable config is refused at WRITE time: spawn without agent_id, judge without
/// prompt, transition to a bogus state. A well-formed transition target is accepted.
#[test]
fn node_add_validates_the_config_per_type() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    assert_eq!(
        node_add(&db, &json!({ "workflow_id": wf, "type": "spawn_subagent" }))
            .unwrap_err()
            .code,
        "invalid_argument",
        "spawn needs agent_id"
    );
    assert_eq!(
        node_add(&db, &json!({ "workflow_id": wf, "type": "judge" }))
            .unwrap_err()
            .code,
        "invalid_argument",
        "judge needs a prompt"
    );
    assert_eq!(
        node_add(&db, &json!({
            "workflow_id": wf, "type": "transition", "config": { "target_state": "banane" }
        }))
        .unwrap_err()
        .code,
        "invalid_argument",
        "a bogus transition target is refused at authoring"
    );
    // A well-formed transition IS accepted (it becomes the tree's root here).
    node_add(&db, &json!({
        "workflow_id": wf, "type": "transition", "config": { "target_state": "in_review" }
    }))
    .expect("a valid transition target is accepted");
    // No partial state leaked from the refused adds.
    let got = workflow_get(&db, &json!({ "workflow_id": wf })).unwrap();
    assert_eq!(
        got.result["nodes"].as_array().unwrap().len(),
        1,
        "only the one valid node was written"
    );
}

/// Lot 2 — a `transition_task` node's `transition` is validated at WRITE time: a missing or
/// off-vocabulary verb is refused (only start/done/block/skip), while a valid one is accepted.
#[test]
fn node_add_validates_transition_task_transition() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    // Missing transition → refused.
    assert_eq!(
        node_add(&db, &json!({ "workflow_id": wf, "type": "transition_task", "config": {} }))
            .unwrap_err()
            .code,
        "invalid_argument",
        "transition_task needs a transition"
    );
    // A bogus verb → refused, and the message names the accepted set.
    let err = node_add(&db, &json!({
        "workflow_id": wf, "type": "transition_task", "config": { "transition": "banane" }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("start, done, block, skip"),
        "the error lists the task transitions: {}",
        err.message
    );
    // A valid verb IS accepted (it becomes the tree's root here); the optional fields need not be set.
    node_add(&db, &json!({
        "workflow_id": wf, "type": "transition_task", "config": { "transition": "done" }
    }))
    .expect("a valid transition_task is accepted");
}

/// A `create_workspace` node is AUTHORABLE like its fifteen neighbours: `known_config_fields`
/// lists exactly the three fields `engine::CreateWorkspaceConfig` decodes, so a `node_add` carrying
/// them is accepted, while an invented key is still refused and the refusal NAMES the accepted set.
/// Before this the type fell to the catch-all `_ => &[]` ("takes no config"), so every real config
/// was rejected as unknown and a FORK of /nspec could not touch the node at all.
///
/// `path_key` is GONE from that set (finding `01M0B5AG0XR4BG6DSGEFGKW6C4`) and is therefore refused
/// like any other invented key — which is the point: it named a value the effect took AS the final
/// folder, next to a `root_key` the effect derived a subfolder under, so one graph could mean two
/// things by the same kind of answer.
#[test]
fn node_add_accepts_the_create_workspace_config_fields() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    // An invented key is refused, and the message lists the three accepted fields.
    let err = node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "create_workspace", "config": { "root": "D:/wt" }
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("unknown config field 'root'")
            && err.message.contains("root_key, name_key, gate_on"),
        "the refusal names the accepted set: {}",
        err.message
    );

    // The RETIRED source is refused by that same rule — nothing quietly ignores it.
    assert!(
        node_add(
            &db,
            &json!({
                "workflow_id": wf,
                "type": "create_workspace",
                "config": { "path_key": "workspace_path", "root_key": "worktrees_root" }
            }),
        )
        .unwrap_err()
        .message
        .contains("unknown config field 'path_key'"),
        "the second, un-derived source cannot be authored back in"
    );

    // The keys the seeded /nspec node uses ARE accepted — under the human gate rule (4) demands of a
    // `create_workspace` like of any other sensitive node (the shape itself is fixed by
    // `a_create_workspace_needs_a_human_gate_on_the_branch_that_reaches_it`).
    let root = gated_root(&db, &wf, "workspace_free");
    node_add(
        &db,
        &json!({
            "workflow_id": wf,
            "type": "create_workspace",
            "parent_id": root, "order_index": 1,
            "config": { "root_key": "worktrees_root", "gate_on": "workspace_free" }
        }),
    )
    .expect("a create_workspace carrying root_key / gate_on is accepted");
}

/// …AND THE WRITE SIDE STILL REFUSES THE ONE CONFIG THAT CANNOT RUN. `CreateWorkspaceConfig`
/// defaults `root_key` to `None`, so a `create_workspace {}` hands the effects layer an empty string
/// and `create_run_workspace` answers invalid_argument on EVERY run — the node is inexecutable the
/// moment it is written. That is exactly what `validate_node_config` exists to catch (the M1/M2
/// lesson: `spawn` with no `agent_id`, `judge` with no prompt).
#[test]
fn node_add_refuses_a_create_workspace_with_no_root_key() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    // An EMPTY config: known fields, and nothing to build a folder from.
    let err = node_add(
        &db,
        &json!({ "workflow_id": wf, "type": "create_workspace", "config": {} }),
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("root_key"),
        "the refusal names the way out: {}",
        err.message
    );

    // A config carrying only the OPTIONAL fields is the same node: still nothing to create.
    assert_eq!(
        node_add(
            &db,
            &json!({
                "workflow_id": wf,
                "type": "create_workspace",
                "config": { "name_key": "workspace_name", "gate_on": "ready_ok" }
            }),
        )
        .unwrap_err()
        .code,
        "invalid_argument",
        "name_key/gate_on say WHAT to call it and WHEN — never WHERE"
    );

    // …and the ONE source resolves it — the shape the seeded /nspec graph uses. It lands in its OWN
    // workflow, under the human gate rule (4) demands of every sensitive node (a parent-less
    // `create_workspace` is refused as an UNGATED graph, which is that rule's business, not this
    // one's).
    let wf = make_workflow(&db, "cw-root", Some(&project));
    let root = gated_root(&db, &wf, "go_ahead");
    node_add(
        &db,
        &json!({
            "workflow_id": wf,
            "type": "create_workspace",
            "parent_id": root, "order_index": 1,
            "config": { "root_key": "worktrees_root", "gate_on": "go_ahead" }
        }),
    )
    .expect("a root_key alone resolves the folder");
}

/// A `create_workspace` IS A SENSITIVE NODE, and the shape rule (4) that says so is read on the
/// BRANCH THAT REACHES IT — never on the tree at large. It MKDIRS on the user's disk, inserts a
/// workspace row and rewrites `specs.workspace_id`; before this it passed both validators as a plain
/// child of a sequence, so every tick that walked it created a folder with nobody ever asked, while
/// `fork_spec` — a mutation that touches no disk — was gated on both counts.
///
/// The four cases below are the rule, and they are worth reading as a pair of pairs. The FIRST is the
/// shape the seeded /nspec ships: the question sits TWO levels up ("mark it ready?"), the branch its
/// answer selects is a whole sequence, and the node's own `gate_on` names a QUERY fact
/// (`workspace_free`, the prevalidation) rather than the answer — so the consent is carried by the
/// SELECTOR that routed the branch. Refusing to climb would have made that graph un-authorable; the
/// three refusals are what stops the climb from becoming a rubber stamp.
#[test]
fn a_create_workspace_needs_a_human_gate_on_the_branch_that_reaches_it() {
    let (db, project) = seeded();

    // Build the /nspec skeleton, parameterized by what the selector branch that leads to the
    // workspace sequence TESTS, and by where the question that writes `ready_ok` is put.
    //   `route_cond` — the cond of the branch selecting the workspace sequence.
    //   `ask_at_root` — true: the question is a preceding sibling of the selector (it necessarily
    //                   runs); false: it is buried in the OTHER branch (it may never run at all).
    let build = |name: &str, route_cond: Value, ask_at_root: bool| -> Result<(), RpcError> {
        let wf = make_workflow(&db, name, Some(&project));
        let root = add_node(&db, &wf, "sequence", None, "root");
        let ask = json!({
            "prompt": "Mark the Spec ready, and open a dev workspace for it?",
            "output_key": "ready_ok",
            // Closed, because a branch below compares the answer by equality (J1).
            "options": ["go", "no"],
        });
        if ask_at_root {
            node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
                    "label": "confirm ready", "config": ask
                }),
            )?;
        }
        let route = node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "selector", "parent_id": root, "order_index": 1,
                "label": "route the commitment",
                "config": { "branches": [
                    { "cond": route_cond, "child": 0 },
                    { "cond": { "always": true }, "child": 1 },
                ] }
            }),
        )?;
        let route = id_of(&route, "node");
        let workspace = id_of(
            &node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "sequence", "parent_id": route, "order_index": 0,
                    "label": "ready + a dev workspace"
                }),
            )?,
            "node",
        );
        // The else branch: either the clean N16 exit, or (when the question is banished off-branch)
        // the sequence that holds it.
        let other = id_of(
            &node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "sequence", "parent_id": route, "order_index": 1,
                    "label": "left as draft"
                }),
            )?,
            "node",
        );
        if !ask_at_root {
            node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "ask_human", "parent_id": other, "order_index": 0,
                    "label": "confirm ready", "config": ask
                }),
            )?;
        }
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "query", "parent_id": workspace, "order_index": 0,
                "label": "worktrees root",
                "config": { "query": "project_worktrees_root", "write": "worktrees_root" }
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "query", "parent_id": workspace, "order_index": 1,
                "label": "is the folder free?",
                "config": { "query": "workspace_target_free", "write": "workspace_free" }
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "create_workspace", "parent_id": workspace, "order_index": 2,
                "label": "dev workspace",
                "config": { "root_key": "worktrees_root", "gate_on": "workspace_free" }
            }),
        )?;
        Ok(())
    };

    // (a) ACCEPTED — the shipped shape. The question runs first (a preceding sibling of the selector,
    //     in a sequence), and the branch that routes to the workspace TESTS the key it wrote: the
    //     human's own answer is what puts the run on this branch at all.
    build("gated", json!({ "var": "ready_ok", "eq": "go" }), true)
        .expect("a consent read one selector higher is still a consent");

    // (b) REFUSED — THE HOLE ITSELF: `node_add(create_workspace, {root_key})` as a plain child of a
    //     sequence. Both validators used to wave it through, and every tick that walked it made a
    //     folder.
    let wf = make_workflow(&db, "ungated", Some(&project));
    let root = add_node(&db, &wf, "sequence", None, "root");
    let err = node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "create_workspace", "parent_id": root, "order_index": 0,
            "config": { "root_key": "worktrees_root" }
        }),
    )
    .unwrap_err();
    assert_eq!(
        err.code, "invalid_state",
        "an ungated create_workspace is refused"
    );
    assert!(
        err.message.contains("not HUMAN-GATED"),
        "the refusal says WHAT is missing: {}",
        err.message
    );

    // (c) REFUSED — AND THIS IS THE ONE THE CLIMB COULD HAVE FAKED. The question is exactly where (a)
    //     puts it, on this very branch, necessarily answered before the mkdir — but the branch is
    //     routed `always` and the node's `gate_on` reads a query fact, so NOTHING between the answer
    //     and the folder reads that answer. A human was asked something; nobody was asked about THIS.
    //     Chronology is not consent, and an ask_human anywhere above must not become a rubber stamp.
    let err = build("asked-but-not-read", json!({ "always": true }), true).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("not HUMAN-GATED"),
        "a question nothing on the branch reads is not a gate: {}",
        err.message
    );

    // (d) REFUSED — the answer IS read (the branch tests `ready_ok`), but the question that writes it
    //     sits in the SIBLING branch: a selector picks ONE child, so this run may never have been
    //     asked at all. The climb stays on the ancestor chain — the branch actually taken — and an
    //     ask_human off it gates nothing.
    let err = build(
        "asked-elsewhere",
        json!({ "var": "ready_ok", "eq": "go" }),
        false,
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("not HUMAN-GATED"),
        "a question in another branch is a question this run may never see: {}",
        err.message
    );
}

/// FINDING `01M0C048HM5CWJBX0JB6K46D73`, BYPASS 1 — A KEY A SECOND NODE CAN WRITE IS NOT A CONSENT.
///
/// The audit reproduced it end to end. The graph is three nodes long and nothing about it is exotic:
///
/// ```text
/// sequence[ ask_human{output_key: "approved"},
///           query{spec_can_transition(ready), write: "approved"},
///           transition{ready, gate_on: "approved"} ]
/// ```
///
/// `validate_tree` said Ok. The human answered "no" on a REAL human turn (so the `human_turn_required`
/// check was satisfied — it is the KEY that was hijacked, not the turn), the `query` overwrote the
/// answer with `true`, and the Spec went `draft → ready`. Condition (ii) compared a key NAME; the
/// value the runtime gate reads is whatever wrote it LAST.
///
/// Condition (iii) — the gated key has exactly ONE writer in the workflow — is what closes it, and the
/// test is built in the order that proves the guard is about THAT and not about the shape: question +
/// transition alone is ACCEPTED, and each writer added afterwards is what flips it to refused. The
/// FIVE writers of the vocabulary are exercised, plus a second `ask_human` (two questions, one key: no
/// question owns the gate any more).
#[test]
fn a_second_writer_on_the_gated_key_is_not_consent() {
    let (db, project) = seeded();

    // The ACCEPTED baseline, returned as `(workflow, root)` so a writer can be slid in at order 1 —
    // between the question and the gate, exactly where the audit put its `query`.
    let gated = |name: &str| -> (String, String) {
        let wf = make_workflow(&db, name, Some(&project));
        let root = add_node(&db, &wf, "sequence", None, "root");
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
                "label": "approve?",
                "config": { "prompt": "Mark the Spec ready?", "output_key": "approved" }
            }),
        )
        .expect("the question is added");
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "transition", "parent_id": root, "order_index": 2,
                "label": "ready",
                "config": { "target_state": "ready", "gate_on": "approved" }
            }),
        )
        .expect("ONE writer on the gated key — the canonical shape stays accepted");
        (wf, root)
    };

    // Every node type that can land a value on `approved`, each refused between the question and the
    // gate. The engine writes all of them through `write_result` / `set_board`, so they are one class.
    let writers = [
        (
            "query.write",
            "query",
            json!({ "query": "spec_can_transition", "target": "ready", "write": "approved" }),
        ),
        (
            "judge.output_key",
            "judge",
            json!({ "prompt": "is it ready?", "output_key": "approved" }),
        ),
        (
            "produce.output_key",
            "produce",
            json!({ "output_key": "approved" }),
        ),
        (
            "sub_workflow.output_key",
            "sub_workflow",
            json!({ "scope": "coder", "output_key": "approved" }),
        ),
        (
            // The WIDEST write in the vocabulary: a child board key folded FLAT onto the parent's.
            "sub_workflow.retrieve[].to",
            "sub_workflow",
            json!({
                "scope": "coder", "output_key": "child_result",
                "retrieve": [{ "from": "verdict", "to": "approved" }]
            }),
        ),
        (
            "a second ask_human",
            "ask_human",
            json!({ "prompt": "sure?", "output_key": "approved" }),
        ),
    ];
    for (what, node_type, config) in writers {
        let (wf, root) = gated(&format!("second-writer-{node_type}-{what}"));
        let err = node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": node_type, "parent_id": root, "order_index": 1,
                "label": what, "config": config
            }),
        )
        .unwrap_err();
        assert_eq!(
            err.code, "invalid_state",
            "{what} can replace the human's answer before the gate reads it"
        );
        assert!(
            err.message.contains("not HUMAN-GATED") && err.message.contains("NOTHING ELSE"),
            "the refusal names the second writer ({what}): {}",
            err.message
        );
    }

    // A writer on ANOTHER key changes nothing: the rule is about the gated key, not about queries.
    let (wf, root) = gated("second-writer-elsewhere");
    node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "query", "parent_id": root, "order_index": 1,
            "label": "can transition?",
            "config": { "query": "spec_can_transition", "target": "ready", "write": "can_ready" }
        }),
    )
    .expect("a query writing a DIFFERENT key leaves the consent intact");
}

/// FINDING `01M0C048HM5CWJBX0JB6K46D73`, BYPASS 2 — A BRANCH THAT DOES NOT TEST AN APPROVAL GATES
/// NOTHING, and `create_workspace` is where that bit hardest: its `gate_on` is OPTIONAL
/// (`unwrap_or(true)` at runtime = UNGATED), so the selector `cond` was the WHOLE guard — and the rule
/// asked of that cond only that it NAME a variable.
///
/// The audit reproduced it physically: `selector[ {var: "ready_ok", eq: "no"} → create_workspace ]`
/// validated, the human answered "no" on a real human turn, and nyx MKDIR'd on the disk, inserted the
/// workspace row and rewrote `specs.workspace_id`. Answering NO fired the action.
///
/// The polarity condition ([`crate::mcp_authoring::branch_affirmative_key`]) is what closes it: a
/// crediting branch must read `{var: K, eq: <an approval>}`, judged by the engine's OWN consent
/// vocabulary (`board_value_is_affirmative`). The four refusals below are the four ways a cond names a
/// key while deciding nothing — and `truthy: true` is the one worth naming twice: on the closed answer
/// set J1 forces, `"no"` is a TRUTHY string, so it is the `always` branch wearing a var's name.
#[test]
fn a_branch_that_does_not_test_an_approval_gates_nothing() {
    let (db, project) = seeded();

    // /nspec's shape reduced to its bones, parameterized by the cond of the branch that routes the
    // workspace sequence. The `create_workspace` carries NO `gate_on` — the branch IS the guard.
    let build = |name: &str, route_cond: Value| -> Result<(), RpcError> {
        let wf = make_workflow(&db, name, Some(&project));
        let root = add_node(&db, &wf, "sequence", None, "root");
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
                "label": "confirm ready",
                "config": {
                    "prompt": "Mark it ready and open a dev workspace?",
                    "output_key": "ready_ok",
                    "options": ["go", "no"],
                }
            }),
        )?;
        let route = id_of(
            &node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "selector", "parent_id": root, "order_index": 1,
                    "label": "route the commitment",
                    "config": { "branches": [
                        { "cond": route_cond, "child": 0 },
                        { "cond": { "always": true }, "child": 1 },
                    ] }
                }),
            )?,
            "node",
        );
        let workspace = id_of(
            &node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "sequence", "parent_id": route, "order_index": 0,
                    "label": "ready + a dev workspace"
                }),
            )?,
            "node",
        );
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "note", "parent_id": route, "order_index": 1,
                "label": "left as draft", "config": { "text": "left as a draft" }
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "query", "parent_id": workspace, "order_index": 0,
                "label": "worktrees root",
                "config": { "query": "project_worktrees_root", "write": "worktrees_root" }
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "create_workspace", "parent_id": workspace, "order_index": 1,
                "label": "dev workspace",
                // NO `gate_on`: nothing but the branch above stands between "no" and the mkdir.
                "config": { "root_key": "worktrees_root" }
            }),
        )?;
        Ok(())
    };

    // ACCEPTED — the shipped shape: only a human's `go` puts the run on this branch, and `go` is an
    // approval in the engine's own vocabulary.
    build("polarity-go", json!({ "var": "ready_ok", "eq": "go" }))
        .expect("a branch that tests the answer for an APPROVAL is the consent");
    // ACCEPTED — extra tests ANDed beside a qualifying `eq` only NARROW the branch: they can make it
    // fire less often, never on a value the `eq` already refused.
    build(
        "polarity-go-narrowed",
        json!({ "var": "ready_ok", "eq": "go", "ne": "no" }),
    )
    .expect("an extra AND beside an affirmative eq narrows the branch, it does not open it");

    for (name, cond, why) in [
        (
            "polarity-eq-no",
            json!({ "var": "ready_ok", "eq": "no" }),
            "THE REPRODUCED BYPASS: answering no would fire the mkdir",
        ),
        (
            "polarity-truthy",
            json!({ "var": "ready_ok", "truthy": true }),
            "on a closed answer set every answer is truthy — an `always` in disguise",
        ),
        (
            "polarity-ne",
            json!({ "var": "ready_ok", "ne": "no" }),
            "not-a-refusal is the complement of a consent, not a consent",
        ),
        (
            "polarity-bare",
            json!({ "var": "ready_ok" }),
            "a bare var is a truthiness test, so it is the truthy case again",
        ),
        (
            "polarity-falsy",
            json!({ "var": "ready_ok", "truthy": false }),
            "a branch taken when the key is FALSY is the refusal branch",
        ),
    ] {
        let err = build(name, cond).unwrap_err();
        assert_eq!(err.code, "invalid_state", "{why}");
        assert!(
            err.message.contains("not HUMAN-GATED"),
            "{why}: {}",
            err.message
        );
    }
}

/// FINDING `01M0C3F7V89HJFDGWJEBP12P0N` — THE SINGLE-WRITER CENSUS WAS MISSING THREE ENGINE WRITES.
///
/// Condition (iii) asks that the gated key have exactly ONE writer in the workflow, and it reads the
/// writers off [`crate::mcp_authoring::node_written_board_keys`], whose own doc-comment says "the list
/// is the contract". Three fixed keys the engine writes on a node's behalf were absent from it:
/// `last_gate_result` and `blocking_gate_red` (both written by the `gate_check` arm of `drive`) and
/// `__child_run_id__<node>` (written and cleared by the `sub_workflow` arm). So a graph could name one
/// of them as its consent key and satisfy (iii) with a second writer standing right beside it.
///
/// It was never a BYPASS — those three carry an object, an array and a UUID, and none is affirmative,
/// so a second writer could only REVOKE a consent, never fabricate one. That is why it is minor. It is
/// still a hole in a contract that is stated flatly, and the three forms below are the ones the audit
/// found ACCEPTED.
#[test]
fn the_fixed_keys_the_engine_writes_itself_count_as_writers() {
    let (db, project) = seeded();

    // `sequence[ ask_human{output_key: K}, <the second writer>, create_workspace{gate_on: K} ]` —
    // the exact three-node shape the single-writer condition exists for.
    let build = |name: &str,
                 key: &str,
                 writer_type: &str,
                 writer_config: Value|
     -> Result<(), RpcError> {
        let wf = make_workflow(&db, name, Some(&project));
        let root = add_node(&db, &wf, "sequence", None, "root");
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
                "label": "approve?",
                "config": { "prompt": "open a workspace?", "output_key": key }
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": writer_type, "parent_id": root, "order_index": 1,
                "label": "the second writer", "config": writer_config
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "create_workspace", "parent_id": root, "order_index": 2,
                "label": "dev workspace",
                "config": { "root_key": "worktrees_root", "gate_on": key }
            }),
        )?;
        Ok(())
    };

    // A `sub_workflow`'s child-run handle is keyed by the NODE, so the colliding key can only be
    // written once the node exists: author the graph in the order that lets the test name it.
    let child_key_form = |name: &str| -> Result<(), RpcError> {
        let wf = make_workflow(&db, name, Some(&project));
        let root = add_node(&db, &wf, "sequence", None, "root");
        // The sub_workflow FIRST, so its id (and therefore its child-run key) is known.
        let sub = id_of(
            &node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "sub_workflow", "parent_id": root, "order_index": 1,
                    "label": "coder", "config": { "scope": "coder" }
                }),
            )?,
            "node",
        );
        let key = crate::mcp_engine::subworkflow_child_key(&sub);
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
                "label": "approve?",
                "config": { "prompt": "open a workspace?", "output_key": key }
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "create_workspace", "parent_id": root, "order_index": 2,
                "label": "dev workspace",
                "config": { "root_key": "worktrees_root", "gate_on": key }
            }),
        )?;
        Ok(())
    };

    // THE THREE OMISSIONS — accepted before, refused now, each by condition (iii).
    for (name, key, writer_type, writer_config) in [
        (
            "census-last-gate-result",
            crate::mcp_engine::LAST_GATE_RESULT_KEY,
            "gate_check",
            json!({ "command_ids": ["inst"] }),
        ),
        (
            "census-blocking-gate-red",
            crate::mcp_engine::BLOCKING_GATE_RED_KEY,
            "gate_check",
            json!({ "command_ids": ["inst"], "blocking": true }),
        ),
    ] {
        let err = build(name, key, writer_type, writer_config).unwrap_err();
        assert_eq!(err.code, "invalid_state", "'{key}' has a second writer");
        assert!(
            err.message.contains("not HUMAN-GATED") && err.message.contains(key),
            "the refusal names the key the engine also writes: {}",
            err.message
        );
    }
    let err = child_key_form("census-child-run-id").unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("not HUMAN-GATED"),
        "a sub_workflow's own child-run key is a write like any other: {}",
        err.message
    );

    // THE CONTROLS — unchanged, so the refusals above are about the census and not about the shape.
    // A key that was ALREADY censused is still refused…
    for (name, key, writer_type, writer_config) in [
        (
            "census-control-gate-green",
            "gate_green",
            "gate_check",
            json!({ "command_ids": ["inst"] }),
        ),
        (
            "census-control-spawns",
            "spawns",
            "spawn_subagent",
            json!({ "agent_id": "coder" }),
        ),
    ] {
        let err = build(name, key, writer_type, writer_config).unwrap_err();
        assert_eq!(err.code, "invalid_state", "'{key}' was already censused");
        assert!(err.message.contains("not HUMAN-GATED"));
    }
    // …and a legitimate single-writer graph is still ACCEPTED — the second node writes elsewhere.
    build(
        "census-control-accepted",
        "ws_ok",
        "gate_check",
        json!({ "command_ids": ["inst"] }),
    )
    .expect("one question, one writer, one gate: still authorable");
}

/// FINDING `01M0C3F7V89HJFDGWJEBP12P0N`, done-criterion 4 — THE LIST MAY NOT RE-DIVERGE. Every FIXED
/// blackboard key the engine writes without a node's config naming it must be censused by
/// [`crate::mcp_authoring::node_written_board_keys`] for at least one node type. A fixed key added
/// later and not censused fails HERE, loudly, instead of quietly weakening condition (iii).
///
/// This is a census over CONSTANTS, not over strings retyped in a test: `LAST_GATE_RESULT_KEY`,
/// `BLOCKING_GATE_RED_KEY`, `LOOP_ITER_KEY` and `subworkflow_child_key` are read from the engine
/// itself, so renaming one on the engine side moves this test with it.
#[test]
fn every_fixed_engine_written_key_is_censused() {
    let node_id = "01a00000-0000-7000-8000-000000000001";
    // The census, taken over EVERY node type: what could this graph's nodes write, all told.
    let mut censused: HashSet<String> = HashSet::new();
    for node_type in db::NODE_TYPES {
        let node = db::Node {
            id: node_id.to_string(),
            workflow_id: "wf".to_string(),
            parent_id: None,
            order_index: 0,
            type_: node_type.to_string(),
            label: node_type.to_string(),
            config: "{}".to_string(),
            created_at: 0,
            updated_at: 0,
        };
        censused.extend(super::node_written_board_keys(&node));
    }

    for key in [
        crate::mcp_engine::LAST_GATE_RESULT_KEY.to_string(),
        crate::mcp_engine::BLOCKING_GATE_RED_KEY.to_string(),
        crate::engine::LOOP_ITER_KEY.to_string(),
        crate::mcp_engine::subworkflow_child_key(node_id),
        "gate_green".to_string(),
        "last_diff".to_string(),
        "spawns".to_string(),
    ] {
        assert!(
            censused.contains(&key),
            "the engine writes '{key}' on a node's behalf and no node type censuses it — rule (4)'s \
             single-writer condition would accept a gated key that a second node also fills. Add it \
             to `node_written_board_keys`. Censused today: {:?}",
            {
                let mut sorted: Vec<&String> = censused.iter().collect();
                sorted.sort();
                sorted
            }
        );
    }
}

/// EXHAUSTIVE — A NODE TYPE THAT TAKES A CONFIG MUST SAY WHICH FIELDS IT TAKES.
/// [`super::known_config_fields`] ends in a catch-all that MEANS "takes no config": a type that
/// forgets its arm falls there, and every field an author writes on it is then refused as unknown —
/// the node is authorable and inexecutable at once. It has happened twice already (`sub_workflow`,
/// then `create_workspace`), each time discovered on a real fork of a seeded graph.
///
/// So the census is taken over [`db::NODE_TYPES`] and the CONFIGLESS set is spelled out: `sequence`
/// composes its children and `wait` pauses, and neither reads a thing. An EIGHTEENTH type that lands
/// without its arm fails HERE, on the type it forgot.
#[test]
fn every_node_type_that_takes_a_config_declares_its_fields() {
    /// The only two node types whose behaviour reads NOTHING — the deliberate members of the
    /// catch-all, named so the assertion below can tell a design from an omission.
    const CONFIGLESS: [&str; 2] = [db::NODE_TYPE_SEQUENCE, db::NODE_TYPE_WAIT];
    for node_type in db::NODE_TYPES {
        let known = super::known_config_fields(node_type);
        if CONFIGLESS.contains(&node_type) {
            assert!(
                known.is_empty(),
                "'{node_type}' is declared CONFIGLESS here but now names fields ({known:?}) — move \
                 it out of the list rather than leaving the two readings to disagree"
            );
            continue;
        }
        assert!(
            !known.is_empty(),
            "the node type '{node_type}' declares NO config field, so `known_config_fields` fell to \
             its catch-all and every field an author writes on it is refused as unknown. Add its \
             arm — exactly the fields its `crate::engine` config struct decodes"
        );
    }
}

/// The SEVENTEENTH node type is AUTHORABLE, and it is authorable on its own terms.
///
/// Three things at once, because each was a real trap for one of its predecessors: its fields are
/// KNOWN (a `create_workspace` that fell to the catch-all was inexecutable), a config that names no
/// `category_key` is REFUSED (the `create_workspace` `root_key` reading — the one thing this node
/// does is say WHICH category, and saying it by omission says it to whatever a sibling last wrote),
/// and it needs NO human gate: a classification is metadata, correctable at any Spec status, so it is
/// deliberately NOT a member of [`crate::engine::SensitiveEffect`] and a graph may impose it without
/// asking (ADR-0006 — "un nœud du graphe ne peut pas être ignoré").
#[test]
fn set_category_is_authorable_needs_its_key_and_needs_no_human_gate() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "cat", Some(&project));
    let root = add_node(&db, &wf, "sequence", None, "root");

    // (a) A config-less node is refused, and the refusal NAMES the field.
    let err = node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "set_category", "parent_id": root, "order_index": 0,
            "label": "classify", "config": {}
        }),
    )
    .expect_err("a set_category with no category_key is refused");
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("category_key"),
        "the refusal must name the field, got: {}",
        err.message
    );

    // (b) A typo'd field is refused too — the N10a "no unknown field" complement.
    let err = node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "set_category", "parent_id": root, "order_index": 0,
            "label": "classify", "config": { "category": "bug" }
        }),
    )
    .expect_err("'category' is not 'category_key'");
    assert!(
        err.message.contains("unknown config field 'category'"),
        "got: {}",
        err.message
    );

    // (c) The real shape is accepted — with NO ask_human anywhere in this graph, which is the point:
    //     the classification is imposed, not consented to.
    node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "set_category", "parent_id": root, "order_index": 0,
            "label": "classify", "config": { "category_key": "category_choice" }
        }),
    )
    .expect("a set_category naming its key is authorable, and needs no human gate");

    // (d) …and the OPTIONAL gate is accepted as well, for a flow that does want a human's word.
    node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "set_category", "parent_id": root, "order_index": 1,
            "label": "classify (gated)",
            "config": { "category_key": "category_choice", "gate_on": "cat_ok" }
        }),
    )
    .expect("the optional gate is accepted too");
}

/// COLLISIONS — `set_category` WRITES NOTHING, so it can never be the second writer rule (4)'s
/// condition (iii) refuses.
///
/// Condition (iii) is what keeps a human's approval from being overwritten before the gate reads it:
/// a sensitive node's `gate_on` key must be written by ONE node, the question. Every type that writes
/// a board key is censused for it ([`super::node_written_board_keys`]), and a type censused as writing
/// nothing must genuinely write nothing — otherwise the census certifies a graph the runtime can
/// still have clobbered.
///
/// `set_category` READS its key and commits to the DOMAIN; it writes no board key at all, by design
/// (a board copy of the classification would be a second truth — see the node's own doc). So this
/// pins both halves: the census answers EMPTY for it, and a real graph where a `set_category` READS
/// the very key an `ask_human` writes and a sensitive `transition` gates on stays AUTHORABLE. The day
/// someone gives this node an `output_key`, that graph stops validating and this test says why.
#[test]
fn set_category_writes_no_board_key_and_so_collides_with_no_consent_gate() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "collision", Some(&project));
    let root = gated_root(&db, &wf, "cat_ok");
    node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "set_category", "parent_id": root, "order_index": 1,
            "label": "classify", "config": { "category_key": "cat_ok" }
        }),
    )
    .expect("the classification reads the answer");
    node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "transition", "parent_id": root, "order_index": 2,
            "label": "ready", "config": { "target_state": "ready", "gate_on": "cat_ok" }
        }),
    )
    .expect(
        "a READ of the gate key is not a WRITE of it — the engaging transition beside it is still \
         single-writer, and rule (4) must accept this graph",
    );

    // The census's own answer, read directly: EMPTY, whatever the node's config says.
    let nodes = db.with_conn(|c| db::list_nodes(c, &wf)).expect("nodes");
    let classify = nodes
        .iter()
        .find(|n| n.type_ == db::NODE_TYPE_SET_CATEGORY)
        .expect("the set_category node");
    assert!(
        super::node_written_board_keys(classify).is_empty(),
        "a `set_category` writes NO blackboard key — it commits to `specs.category_id`, and a graph \
         that routes on the classification reads it back through the `spec_category` query fact"
    );
}

/// FINDING `01M0C3FGCACXCZA1ZESRV73N0R` — THE DEAD GATES: shapes rule (4) certified HUMAN-GATED that
/// the RUNTIME can never satisfy. Safe (no consent is fabricated) and still wrong: the author leaves
/// with a valid graph whose sensitive node will never fire, or whose run dies at the selector.
///
/// THE CHOICE MADE, written rather than implied: each of the three is REFUSED at authoring, because
/// in each case the value the gate would read is provably not the answer.
///
///   (a) A DOTTED read — `gate_on: "ok.deep"`, or a branch on `{var: "ok.deep"}`. An `ask_human`'s
///       answer is a SCALAR (`mcp_engine::human_answer` refuses an object or an array outright) and
///       lands FLAT on its key, so `Blackboard::get("ok.deep")` descends into a scalar and resolves to
///       nothing, forever. It is the READ side of the trap `validate_board_write_key` already refuses
///       on the WRITE side, and it is refused only where it is claimed as a CONSENT — a `selector`
///       may still branch on `proto_result.status`, because a `sub_workflow` writes an OBJECT there
///       and `/nspec` ships exactly that.
///   (b) An APPENDING question — `ask_human {append: true}` writes `["yes"]`, and an array is neither
///       affirmative nor equal to any affirmative literal. An appending question is a transcript
///       (`/nspec`'s "steer"), not a decision.
///   (c) An `eq` LITERAL the runtime cannot match — `"YES"`, `" yes "`. `board_value_is_affirmative`
///       trims and case-folds (it reads a HUMAN's typed value); `Condition::matches` compares by
///       EXACT JSON equality. The authoring rule credited the branch and the run then matched NO
///       branch — `SelectorNoBranch`, run dead. The polarity test now goes through
///       `eq_literal_is_affirmative`, which is the same vocabulary narrowed to what `matches` can win.
#[test]
fn a_gate_the_runtime_can_never_satisfy_is_not_a_gate() {
    let (db, project) = seeded();

    // (a) + (b) — the `gate_on` arm: one question, one sensitive node, parameterized by the question's
    //     config and by what the node's `gate_on` reads.
    let gate_on_form = |name: &str, ask: Value, gate_on: &str| -> Result<(), RpcError> {
        let wf = make_workflow(&db, name, Some(&project));
        let root = add_node(&db, &wf, "sequence", None, "root");
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
                "label": "approve?", "config": ask
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "create_workspace", "parent_id": root, "order_index": 1,
                "label": "dev workspace",
                "config": { "root_key": "worktrees_root", "gate_on": gate_on }
            }),
        )?;
        Ok(())
    };

    // The BASELINE, so every refusal below is about the one thing it changes.
    gate_on_form(
        "dead-baseline",
        json!({ "prompt": "open it?", "output_key": "ok" }),
        "ok",
    )
    .expect("a plain question read by a plain gate_on is the shape that must keep working");

    // …and the same shape inside a LOOP body, the OTHER ordered composite condition (i) accepts (a
    // loop re-enters at cursor 0, so a left sibling ran in every pass). Listed with the acceptances
    // because tightening (ii) must not have narrowed WHERE the question may stand.
    let wf_loop = make_workflow(&db, "dead-baseline-loop", Some(&project));
    let body = add_node(&db, &wf_loop, "loop", None, "body");
    node_update(
        &db,
        &json!({ "node_id": body, "config": { "until": { "var": "done_flag", "truthy": true } } }),
    )
    .expect("the loop exits on a key no question writes");
    node_add(
        &db,
        &json!({
            "workflow_id": wf_loop, "type": "ask_human", "parent_id": body, "order_index": 0,
            "label": "approve?", "config": { "prompt": "open it?", "output_key": "ok" }
        }),
    )
    .expect("the question stands in the loop body");
    node_add(
        &db,
        &json!({
            "workflow_id": wf_loop, "type": "create_workspace", "parent_id": body, "order_index": 1,
            "label": "dev workspace",
            "config": { "root_key": "worktrees_root", "gate_on": "ok" }
        }),
    )
    .expect("a loop body is an ordered composite: the left sibling necessarily ran");

    let err = gate_on_form(
        "dead-dotted-gate-on",
        json!({ "prompt": "open it?", "output_key": "ok" }),
        "ok.deep",
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("not HUMAN-GATED") && err.message.contains("un-dotted"),
        "a dotted gate_on reads a path a human's answer never fills, and the refusal says so: {}",
        err.message
    );

    let err = gate_on_form(
        "dead-append",
        json!({ "prompt": "open it?", "output_key": "ok", "append": true }),
        "ok",
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("not HUMAN-GATED") && err.message.contains("append"),
        "an appending question fills its key with an ARRAY, and the refusal says so: {}",
        err.message
    );

    // (c) — the SELECTOR arm: the branch IS the consent, so the literal it compares must be one the
    //     runtime's exact equality can actually match.
    let branch_form = |name: &str, cond: Value| -> Result<(), RpcError> {
        let wf = make_workflow(&db, name, Some(&project));
        let root = add_node(&db, &wf, "sequence", None, "root");
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
                "label": "confirm ready",
                "config": {
                    "prompt": "mark it ready and open a workspace?",
                    "output_key": "ready_ok",
                    "options": ["go", "no"],
                }
            }),
        )?;
        let route = id_of(
            &node_add(
                &db,
                &json!({
                    "workflow_id": wf, "type": "selector", "parent_id": root, "order_index": 1,
                    "label": "route",
                    "config": { "branches": [
                        { "cond": cond, "child": 0 },
                        { "cond": { "always": true }, "child": 1 },
                    ] }
                }),
            )?,
            "node",
        );
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "create_workspace", "parent_id": route, "order_index": 0,
                "label": "dev workspace", "config": { "root_key": "worktrees_root" }
            }),
        )?;
        node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "note", "parent_id": route, "order_index": 1,
                "label": "left as draft", "config": { "text": "left as a draft" }
            }),
        )?;
        Ok(())
    };

    // ACCEPTED — the literals `Condition::matches` can win on, in the canonical form the vocabulary
    // lists. `eq: true` is the BOOL case, which needs no canonicalisation at all.
    for (name, cond, why) in [
        (
            "live-eq-yes",
            json!({ "var": "ready_ok", "eq": "yes" }),
            "yes",
        ),
        ("live-eq-go", json!({ "var": "ready_ok", "eq": "go" }), "go"),
        (
            "live-eq-true",
            json!({ "var": "ready_ok", "eq": true }),
            "true",
        ),
    ] {
        branch_form(name, cond).unwrap_or_else(|e| {
            panic!(
                "`eq: {why}` is matchable and must stay accepted: {}",
                e.message
            )
        });
    }

    // REFUSED — affirmative to the eye, unmatchable to `Condition::matches`.
    for (name, cond, why) in [
        (
            "dead-eq-uppercase",
            json!({ "var": "ready_ok", "eq": "YES" }),
            "the runtime compares by exact equality — a board holding \"yes\" never equals \"YES\"",
        ),
        (
            "dead-eq-padded",
            json!({ "var": "ready_ok", "eq": " yes " }),
            "…and never equals \" yes \" either: the trim is on the VALUE reader, not on the branch",
        ),
    ] {
        let err = branch_form(name, cond).unwrap_err();
        assert_eq!(err.code, "invalid_state", "{why}");
        assert!(
            err.message.contains("not HUMAN-GATED") && err.message.contains("exact equality"),
            "{why}: {}",
            err.message
        );
    }

    // REFUSED — a DOTTED branch var, for the same reason as (a): the answer is flat.
    let err = branch_form(
        "dead-dotted-branch",
        json!({ "var": "ready_ok.d", "eq": "yes" }),
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(err.message.contains("not HUMAN-GATED"), "{}", err.message);
}

/// Lot 2 — a `sub_workflow` node needs a RESOLVABLE target (a non-empty `scope` OR an explicit
/// `workflow_id`), and `inject`/`retrieve` must be arrays of well-formed `{from, to}` copies.
#[test]
fn node_add_validates_sub_workflow_target_and_board_copies() {
    let (db, project) = seeded();

    // No target (neither scope nor workflow_id) → refused.
    let wf = make_workflow(&db, "w1", Some(&project));
    assert_eq!(
        node_add(&db, &json!({ "workflow_id": wf, "type": "sub_workflow", "config": {} }))
            .unwrap_err()
            .code,
        "invalid_argument",
        "a sub_workflow needs a scope or a workflow_id"
    );

    // A mis-shaped inject (missing `to`) → refused.
    let wf2 = make_workflow(&db, "w2", Some(&project));
    let err = node_add(&db, &json!({
        "workflow_id": wf2, "type": "sub_workflow",
        "config": { "scope": "coder", "inject": [{ "from": "task_id" }] }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(err.message.contains("from, to"), "the error names the copy shape: {}", err.message);

    // A well-formed sub_workflow (scope + tidy inject/retrieve) IS accepted.
    let wf3 = make_workflow(&db, "w3", Some(&project));
    node_add(&db, &json!({
        "workflow_id": wf3, "type": "sub_workflow",
        "config": {
            "scope": "coder",
            "inject": [{ "from": "task_id", "to": "task_id" }],
            "retrieve": [{ "from": "child_status", "to": "child_status" }],
        }
    }))
    .expect("a well-formed sub_workflow is accepted");
}

/// B2 — a `query` node is refused at WRITE time unless it names a KNOWN nyx fact AND a `write` key: a
/// missing or typo'd fact would branch the graph on a permanent `null` at runtime, so it is caught at
/// authoring (the accepted set shared with the runtime resolver). A well-formed query is accepted.
#[test]
fn query_node_needs_a_known_fact_and_a_write_key() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    assert_eq!(
        node_add(&db, &json!({ "workflow_id": wf, "type": "query", "config": { "write": "s" } }))
            .unwrap_err()
            .code,
        "invalid_argument",
        "query needs a fact name"
    );
    assert_eq!(
        node_add(&db, &json!({
            "workflow_id": wf, "type": "query", "config": { "query": "spec_status" }
        }))
        .unwrap_err()
        .code,
        "invalid_argument",
        "query needs a write key"
    );
    let err = node_add(&db, &json!({
        "workflow_id": wf, "type": "query", "config": { "query": "banane", "write": "s" }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    // The refusal names EVERY fact the runtime resolves — an author who mistyped is handed the whole
    // set, not a list that has drifted behind the resolver (it once named ten facts of eleven, with
    // `project_worktrees_root` missing exactly when a graph needed it). Both sides read one array.
    for fact in crate::mcp_engine::QUERY_FACTS {
        assert!(
            err.message.contains(fact),
            "the refusal names '{fact}': {}",
            err.message
        );
    }
    // A well-formed query IS accepted (the tree's root here).
    node_add(&db, &json!({
        "workflow_id": wf, "type": "query",
        "config": { "query": "spec_status", "write": "status" }
    }))
    .expect("a valid query is accepted");
}

/// A fork must be able to say it is BEHIND, and that starts at the fork: `workflow_fork` stamps the
/// SOURCE's revision at the instant of the copy. Without it a fork is a copy of unknown age — the boot
/// seeder upgrades only the rows it seeded, so from the fork onward every later fix to the source stops
/// reaching this project, and until this stamp nothing could even tell. A source with NO revision (a
/// fork of a fork, a `custom` template) stamps nothing: there is no upstream to be behind.
#[test]
fn a_fork_records_the_revision_of_the_source_it_was_taken_from() {
    let (db, project) = seeded();
    let builtin = db
        .with_conn(|c| {
            let wf = db::create_workflow(c, None, "coder", db::WORKFLOW_SCOPE_CODER, None).unwrap();
            db::set_builtin_revision(c, &wf.id, 11).unwrap();
            wf.id
        });

    let fork_id = id_of(
        &workflow_fork(&db, &json!({ "workflow_id": builtin, "project_id": project })).unwrap(),
        "workflow",
    );
    let fork = db.with_conn(|c| db::get_workflow(c, &fork_id)).unwrap().unwrap();
    assert_eq!(
        fork.forked_from_revision,
        Some(11),
        "the fork records WHICH revision of the built-in it is a copy of",
    );
    assert_eq!(fork.builtin_revision, None, "…and is not itself a built-in");

    // A source carrying no revision stamps none: nothing upstream upgrades, so nothing can be missed.
    let template = make_workflow(&db, "template", None);
    let fork_of_template = id_of(
        &workflow_fork(&db, &json!({ "workflow_id": template, "project_id": project })).unwrap(),
        "workflow",
    );
    assert_eq!(
        db.with_conn(|c| db::get_workflow(c, &fork_of_template))
            .unwrap()
            .unwrap()
            .forked_from_revision,
        None,
    );
}

/// S3 — a project holds ONE workflow per SCOPED family: a second fork of the same scope is refused;
/// `custom` forks are freeform (many allowed).
#[test]
fn fork_refuses_a_second_scoped_workflow_but_allows_many_custom() {
    let (db, project) = seeded();
    let ndev = db
        .with_conn(|c| db::create_workflow(c, None, "ndev", db::WORKFLOW_SCOPE_NDEV, None))
        .unwrap()
        .id;

    // First ndev fork into the project: OK.
    workflow_fork(&db, &json!({ "workflow_id": ndev, "project_id": project })).expect("first ndev fork");
    // Second ndev fork: REFUSED (the project already owns one).
    let err = workflow_fork(&db, &json!({ "workflow_id": ndev, "project_id": project })).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("scope_already_owned")
    );

    // But a project can own MANY custom workflows.
    let custom = make_workflow(&db, "tmpl", None); // unowned custom template
    workflow_fork(&db, &json!({ "workflow_id": custom, "project_id": project })).expect("first custom");
    workflow_fork(&db, &json!({ "workflow_id": custom, "project_id": project })).expect("second custom ok");
}

/// m2 — the unknown-node-type error LISTS the accepted types (parity with the scope error).
#[test]
fn unknown_node_type_lists_the_accepted_types() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let err = node_add(&db, &json!({ "workflow_id": wf, "type": "parallel" })).unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("accepted:") && err.message.contains("spawn_subagent"),
        "the error lists the vocabulary: {}",
        err.message
    );
}

/// B3 (superseding M-a) — an ENGAGING transition (ready/done/cancel) needs a `gate_on` AND an
/// ask_human that runs before it AND WRITES THE KEY THAT `gate_on` READS; the ENTRY transition `start`
/// (in_progress) and `in_review` need neither. A question on another key is not a gate — the human
/// answers something, and the commit still hangs on a key nobody fills.
#[test]
fn engaging_transitions_require_a_gate_and_a_preceding_ask_human() {
    let (db, project) = seeded();

    // Engaging targets with NO gate_on → refused (missing gate_on).
    for target in ["done", "ready", "cancelled"] {
        let wf = make_workflow(&db, "w", Some(&project));
        let err = node_add(&db, &json!({
            "workflow_id": wf, "type": "transition", "config": { "target_state": target }
        }))
        .unwrap_err();
        assert_eq!(err.code, "invalid_argument", "engaging '{target}' without gate_on is refused");
        assert!(err.message.contains("gate_on"), "the error names gate_on: {}", err.message);
    }

    // in_review (non-sensitive) + in_progress (ENTRY) need no gate → accepted as roots.
    let wf_r = make_workflow(&db, "wr", Some(&project));
    node_add(&db, &json!({ "workflow_id": wf_r, "type": "transition", "config": { "target_state": "in_review" } }))
        .expect("in_review is imposable with no gate");
    let wf_s = make_workflow(&db, "ws", Some(&project));
    node_add(&db, &json!({ "workflow_id": wf_s, "type": "transition", "config": { "target_state": "in_progress" } }))
        .expect("start (entry) is imposable with no gate — consent from the launch");

    // Engaging WITH gate_on but NO preceding ask_human → refused by the tree guard.
    let wf4 = make_workflow(&db, "w4", Some(&project));
    let root4 = add_node(&db, &wf4, "sequence", None, "root");
    let err = node_add(&db, &json!({
        "workflow_id": wf4, "type": "transition", "parent_id": root4, "order_index": 0,
        "config": { "target_state": "done", "gate_on": "approved" }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_state", "engaging without a preceding ask_human is incoherent");

    // A preceding ask_human that writes a DIFFERENT key is NOT a gate: it writes the default key
    // (`human`) while the transition fires on `approved`, so the question the human answers decides
    // nothing about the commit — and the commit hangs on a key nobody fills. Chronology is not
    // consent (rule (4), condition (ii)).
    let wf4b = make_workflow(&db, "w4b", Some(&project));
    let root4b = add_node(&db, &wf4b, "sequence", None, "root");
    add_node(&db, &wf4b, "ask_human", Some(&root4b), "approve?");
    let err = node_add(&db, &json!({
        "workflow_id": wf4b, "type": "transition", "parent_id": root4b, "order_index": 1,
        "config": { "target_state": "done", "gate_on": "approved" }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_state", "a question the gate does not read gates nothing");

    // WITH a preceding ask_human in the same sequence, WRITING THE KEY THE GATE READS → accepted.
    let wf5 = make_workflow(&db, "w5", Some(&project));
    let root5 = gated_root(&db, &wf5, "approved");
    node_add(&db, &json!({
        "workflow_id": wf5, "type": "transition", "parent_id": root5, "order_index": 1,
        "config": { "target_state": "done", "gate_on": "approved" }
    }))
    .expect("engaging behind a preceding ask_human is accepted");
}

/// N13(b) — a `fork_spec` needs a `gate_on` AND a preceding ask_human writing the key it reads (a
/// fork is never implicit). No gate_on → invalid_argument; gate_on but no preceding ask_human → the
/// tree guard refuses; with one that writes that key → accepted.
#[test]
fn fork_spec_needs_a_gate_and_a_preceding_ask_human() {
    let (db, project) = seeded();

    // No gate_on → refused at config validation.
    let wf1 = make_workflow(&db, "w1", Some(&project));
    let err = node_add(&db, &json!({ "workflow_id": wf1, "type": "fork_spec" })).unwrap_err();
    assert_eq!(err.code, "invalid_argument", "fork_spec without gate_on is refused");
    assert!(err.message.contains("gate_on"), "the error names gate_on: {}", err.message);

    // gate_on but NO preceding ask_human → refused by the tree guard.
    let wf2 = make_workflow(&db, "w2", Some(&project));
    let root2 = add_node(&db, &wf2, "sequence", None, "root");
    let err = node_add(&db, &json!({
        "workflow_id": wf2, "type": "fork_spec", "parent_id": root2, "order_index": 0,
        "config": { "gate_on": "revise_ok" }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_state", "a fork without a preceding ask_human is incoherent");

    // WITH a preceding ask_human in the same sequence, writing the key `gate_on` reads → accepted.
    let wf3 = make_workflow(&db, "w3", Some(&project));
    let root3 = gated_root(&db, &wf3, "revise_ok");
    node_add(&db, &json!({
        "workflow_id": wf3, "type": "fork_spec", "parent_id": root3, "order_index": 1,
        "config": { "gate_on": "revise_ok" }
    }))
    .expect("fork_spec behind a preceding ask_human is accepted");
}

/// J1 — a question a BRANCH answers must declare that branch's answers. An `ask_human` with no
/// `options` whose written key a `selector` cond or a `loop` `until` compares is REFUSED, and the
/// refusal names BOTH nodes: the graph would otherwise accept any text and then route on it by
/// equality. A free-form ask nobody branches on stays perfectly legal (nspec's `steer`, the
/// prototyper's `election`), and declaring the options unblocks the very same edit.
#[test]
fn an_ask_human_a_branch_reads_must_declare_its_options() {
    let (db, project) = seeded();

    // A SELECTOR branching on the answer → the free-form ask is refused.
    let wf = make_workflow(&db, "sel", Some(&project));
    let root = add_node(&db, &wf, "sequence", None, "root");
    let ask = id_of(
        &node_add(&db, &json!({
            "workflow_id": wf, "type": "ask_human", "parent_id": root, "order_index": 0,
            "label": "review", "config": { "prompt": "approve?", "output_key": "decision" }
        }))
        .expect("a free-form ask nobody branches on yet is fine"),
        "node",
    );
    let branch = json!({
        "workflow_id": wf, "type": "selector", "parent_id": root, "order_index": 1,
        "label": "finish", "config": { "branches": [{ "cond": { "var": "decision", "eq": "approve" } }] }
    });
    let err = node_add(&db, &branch).unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains("review") && err.message.contains("finish"),
        "the refusal names the question AND the branch that decides on it: {}",
        err.message
    );
    assert!(
        err.message.contains(r#"decision == "approve""#),
        "and the test the answer would have been compared by: {}",
        err.message
    );

    // Declaring the closed set the branch assumes is what unblocks it.
    node_update(&db, &json!({
        "node_id": ask,
        "config": { "prompt": "approve?", "output_key": "decision", "options": ["approve", "cancel"] }
    }))
    .expect("declaring the options makes the question answerable");
    node_add(&db, &branch).expect("the branch is accepted once the answers it compares are declared");

    // A LOOP's `until` is the same read — ndoc's shape exactly.
    let wf2 = make_workflow(&db, "loop", Some(&project));
    let body = add_node(&db, &wf2, "loop", None, "syncing");
    add_node(&db, &wf2, "ask_human", Some(&body), "review");
    let err = node_update(&db, &json!({
        "node_id": body, "config": { "until": { "var": "human", "ne": "changes" } }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert!(
        err.message.contains(r#"human != "changes""#),
        "a loop's `until` reads the answer just as a selector does: {}",
        err.message
    );
}

/// L-d — the "one scoped workflow per project" invariant holds at EVERY entry (create/fork/assign),
/// not just fork; `custom` stays freeform.
#[test]
fn one_scoped_workflow_per_project_across_create_fork_assign() {
    let (db, project) = seeded();

    // create: first ndev OK, second ndev refused.
    workflow_create(&db, &json!({ "name": "a", "scope": "ndev", "project_id": project }))
        .expect("first ndev");
    let err = workflow_create(&db, &json!({ "name": "b", "scope": "ndev", "project_id": project }))
        .unwrap_err();
    assert_eq!(err.code, "invalid_state");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str),
        Some("scope_already_owned")
    );

    // fork: forking a built-in ndev into the same project is refused (it already owns one).
    let builtin = db
        .with_conn(|c| db::create_workflow(c, None, "ndev", db::WORKFLOW_SCOPE_NDEV, None))
        .unwrap()
        .id;
    assert_eq!(
        workflow_fork(&db, &json!({ "workflow_id": builtin, "project_id": project }))
            .unwrap_err()
            .code,
        "invalid_state"
    );

    // assign: re-homing ANOTHER project's ndev into this one is refused.
    let other = db
        .with_conn(|c| {
            let (p, _ws) = db::create_project(c, "Other", "/tmp/authoring-other", None).unwrap();
            p.id
        });
    let other_ndev = id_of(
        &workflow_create(&db, &json!({ "name": "o", "scope": "ndev", "project_id": other })).unwrap(),
        "workflow",
    );
    assert_eq!(
        workflow_assign(&db, &json!({ "workflow_id": other_ndev, "project_id": project }))
            .unwrap_err()
            .code,
        "invalid_state"
    );

    // custom is freeform: many allowed in the same project.
    workflow_create(&db, &json!({ "name": "c1", "scope": "custom", "project_id": project }))
        .expect("custom 1");
    workflow_create(&db, &json!({ "name": "c2", "scope": "custom", "project_id": project }))
        .expect("custom 2");
}

/// N4 — an UNOWNED FORK of a seeded scope (project NULL + forked_from set) is NOT a protected
/// built-in: the real seeded built-in stays immutable, but the stray fork is mutable/deletable so it
/// can be cleaned up (before the fix, `forked_from` was ignored and the fork was indestructible).
#[test]
fn an_unowned_fork_of_a_seeded_scope_is_mutable_and_deletable() {
    let (db, _project) = seeded();
    let builtin = db
        .with_conn(|c| db::create_workflow(c, None, "ndev", db::WORKFLOW_SCOPE_NDEV, None))
        .unwrap()
        .id;
    let rogue = db
        .with_conn(|c| {
            db::create_workflow(c, None, "ndev-fork", db::WORKFLOW_SCOPE_NDEV, Some(&builtin))
        })
        .unwrap()
        .id;

    // The REAL built-in is immutable.
    assert_eq!(
        node_add(&db, &json!({ "workflow_id": builtin, "type": "sequence" }))
            .unwrap_err()
            .code,
        "invalid_state"
    );
    // The stray fork is NOT protected → deletable.
    let del = workflow_delete(&db, &json!({ "workflow_id": rogue })).unwrap();
    assert_eq!(del.result["deleted"], true);
}

/// N5 — `run_delete` removes a run by id, INCLUDING a run on a built-in flow (which workflow_delete
/// cannot reach). An unknown id is `invalid_id`.
#[test]
fn run_delete_removes_a_run_even_on_a_builtin_flow() {
    let (db, _project) = seeded();
    let builtin = db
        .with_conn(|c| db::create_workflow(c, None, "ndev", db::WORKFLOW_SCOPE_NDEV, None))
        .unwrap()
        .id;
    let run = db
        .with_conn(|c| db::create_workflow_run(c, &builtin, None, None, "{}"))
        .unwrap()
        .id;

    let out = run_delete(&db, &EngineRuntime::new(), &json!({ "run_id": run })).unwrap();
    assert_eq!(out.effects, vec![ChangedTopic::Workflows]);
    assert_eq!(out.result["deleted"], true);
    assert!(
        db.with_conn(|c| db::get_workflow_run(c, &run))
            .unwrap()
            .is_none(),
        "the run row is gone"
    );
    assert_eq!(
        run_delete(&db, &EngineRuntime::new(), &json!({ "run_id": "nope" }))
            .unwrap_err()
            .code,
        "invalid_id"
    );
}

/// N10a — an UNKNOWN config field is REFUSED at authoring: a typo (`accumulate` instead of the real
/// `append`) must be an error, not silently ignored at runtime (where it changed the behaviour with
/// no signal). The complement to M1 (which checks the KNOWN fields).
#[test]
fn node_add_refuses_an_unknown_config_field() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    let err = node_add(&db, &json!({
        "workflow_id": wf, "type": "ask_human",
        "config": { "prompt": "?", "output_key": "log", "accumulate": true }
    }))
    .unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("accumulate") && err.message.contains("append"),
        "names the bad field AND lists the accepted (incl. the real `append`): {}",
        err.message
    );

    // The CORRECT field is accepted.
    node_add(&db, &json!({
        "workflow_id": wf, "type": "ask_human",
        "config": { "prompt": "?", "output_key": "log", "append": true }
    }))
    .expect("the real `append` field is accepted");

    // A spawn node's per-node `subject` (WHAT this spawn is about, when one role is spawned over
    // several objects) is a KNOWN field — authoring it must not read as a typo.
    let wf2 = make_workflow(&db, "wf-subject", Some(&project));
    node_add(&db, &json!({
        "workflow_id": wf2, "type": "spawn_subagent",
        "config": { "agent_id": "auditor", "subject": "out_of_scope" }
    }))
    .expect("a spawn's `subject` is accepted");
}

/// A `gate_check`'s `expect` is DECLARED at authoring: `green` / `red` are accepted, anything else
/// is refused with the two words spelled out, and ABSENCE is valid (it means `green`).
///
/// The refusal belongs here and not to the runtime for the usual M1/M2 reason: a lenient runtime
/// would fail to DECODE `"raed"` at the moment the gate should have run, parking the flow on a
/// decode error long after the typo was written. And the ABSENT case has to stay valid, because
/// every graph in the field is in it.
#[test]
fn node_add_declares_the_gate_expectation_and_refuses_any_other_word() {
    let (db, project) = seeded();
    // ONE workflow per node: a gate authored as a second root would be refused for INCOHERENCE
    // (an orphan), which says nothing about the config this test is here to grade.
    let mut n = 0;
    let mut author = |config: Value| {
        n += 1;
        let wf = make_workflow(&db, &format!("wf-expect-{n}"), Some(&project));
        node_add(
            &db,
            &json!({ "workflow_id": wf, "type": "gate_check", "config": config }),
        )
    };

    for expect in ["green", "red"] {
        author(json!({ "slot": "repro_check", "expect": expect }))
            .unwrap_or_else(|e| panic!("'{expect}' is a declarable expectation: {}", e.message));
    }

    // NO `expect` at all — the shape every pre-expectation graph has.
    author(json!({ "slot": "build_check" })).expect("an absent `expect` is valid and means green");

    let err = author(json!({ "slot": "repro_check", "expect": "raed" })).unwrap_err();
    assert_eq!(err.code, "invalid_argument");
    assert!(
        err.message.contains("green") && err.message.contains("red"),
        "the refusal spells the two words out: {}",
        err.message
    );

    // And `expect` is a KNOWN field — authoring it must not read as a typo.
    let unknown = author(json!({ "slot": "repro_check", "expects": "red" })).unwrap_err();
    assert!(
        unknown.message.contains("unknown config field 'expects'")
            && unknown.message.contains("expect"),
        "the near-miss is caught and the real field listed: {}",
        unknown.message
    );
}

/// H12 — the Spec-write GRANT is nyx's to write, and a node that named it as its own output key would
/// be handing itself the authorization. So naming it is an authoring ERROR — which is what makes "only
/// the engine writes it, and only from a human's answer" a property of the system rather than a habit.
/// The legitimate way to reach it, an `ask_human` declaring `grant_spec_write_on`, is accepted.
#[test]
fn no_node_may_write_the_spec_write_grant_itself() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let key = crate::engine::SPEC_WRITE_GRANT_KEY;

    for (node_type, config) in [
        ("ask_human", json!({ "prompt": "?", "output_key": key })),
        ("query", json!({ "query": "spec_status", "write": key })),
        ("judge", json!({ "prompt": "?", "output_key": key })),
    ] {
        let err = node_add(
            &db,
            &json!({ "workflow_id": wf, "type": node_type, "config": config }),
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_argument", "a '{node_type}' may not write the grant");
        assert!(
            err.message.contains("RESERVED") && err.message.contains(key),
            "the refusal names the reserved key: {}",
            err.message
        );
    }

    // The ONE legitimate route to the grant: a human question that declares which answer opens it.
    node_add(&db, &json!({
        "workflow_id": wf, "type": "ask_human",
        "config": { "prompt": "?", "output_key": "route", "options": ["code", "spec"],
                    "grant_spec_write_on": "spec" }
    }))
    .expect("an ask_human's `grant_spec_write_on` is how the grant is authored");

    // The SAME rule over a `sub_workflow`'s board copies — the widest write in the vocabulary, and the
    // one that lands on the PARENT board straight from a child run's own blackboard.
    for field in ["inject", "retrieve"] {
        let err = node_add(&db, &json!({
            "workflow_id": wf, "type": "sub_workflow",
            "config": { "scope": "coder", field: [{ "from": "anything", "to": key }] }
        }))
        .unwrap_err();
        assert!(
            err.message.contains("RESERVED") && err.message.contains(key),
            "a '{field}' copy may not land on the grant either: {}",
            err.message
        );
    }
}

/// A blackboard key may not carry a '.': the board is written FLAT (`Blackboard::set` inserts ONE
/// top-level key) and read by DOT-PATH (`get` splits on '.'), so `write: "spec.status"` was accepted,
/// written whole, and unreadable forever — every later read looked for a nested value nobody wrote.
/// Nothing said so, at authoring or at runtime: a selector branching on it simply missed every branch,
/// and a loop reading it burned its whole iteration budget — a round, and a sub-agent, per pass.
#[test]
fn a_node_may_not_write_a_dotted_blackboard_key() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    for (node_type, config) in [
        ("query", json!({ "query": "spec_status", "write": "spec.status" })),
        ("judge", json!({ "prompt": "?", "output_key": "audit.verdict" })),
        ("ask_human", json!({ "prompt": "?", "output_key": "human.decision" })),
        (
            "sub_workflow",
            json!({ "scope": "coder", "retrieve": [{ "from": "status", "to": "child.status" }] }),
        ),
    ] {
        let err = node_add(
            &db,
            &json!({ "workflow_id": wf, "type": node_type, "config": config }),
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_argument");
        assert!(
            err.message.contains("may not contain a '.'"),
            "a '{node_type}' is refused a dotted write target, and told why: {}",
            err.message
        );
    }

    // A dotted READ is untouched — that is what dot-paths are FOR: a `sub_workflow`'s result is an
    // object, and branching on `sub_result.status` is the ordinary shape.
    node_add(&db, &json!({
        "workflow_id": wf, "type": "selector",
        "config": { "branches": [
            { "cond": { "var": "sub_result.status", "eq": "done" }, "child": 0 },
        ] }
    }))
    .expect("reading a nested value by dot-path stays legal");
}

/// H9 — the graph vocabulary now carries the two things the coder's task close needed: the
/// `task_close_route` fact (HOW a task may be closed right now) and the `verify` transition_task verb
/// (the human-verified close the `done` guard-rail points at). The MCP `task_transition` tool had
/// accepted `verify` all along; the authoring layer refused it, so no graph could express it.
#[test]
fn the_task_close_vocabulary_is_authorable() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));

    node_add(&db, &json!({
        "workflow_id": wf, "type": "query",
        "config": { "query": "task_close_route", "write": "task_close", "task_key": "task_id" }
    }))
    .expect("the task-grained fact, parameterized by the board key naming its task");
    let wf2 = make_workflow(&db, "wf-verify", Some(&project));
    node_add(&db, &json!({
        "workflow_id": wf2, "type": "transition_task",
        "config": { "transition": "verify", "task_key": "task_id" }
    }))
    .expect("the human-verified close");

    let err = node_add(&db, &json!({
        "workflow_id": make_workflow(&db, "wf-bad", Some(&project)),
        "type": "transition_task", "config": { "transition": "banane" }
    }))
    .unwrap_err();
    assert!(
        err.message.contains("verify"),
        "and the refusal of a bogus verb lists it: {}",
        err.message
    );
}

// --- M4: a graph is not reshaped under a run that is WALKING it -------------

/// Put a WALKING run on `wf`: live (running) AND already into the tree (a cursor + a persisted
/// traversal) — the state a structural edit would destroy. Returns its id.
fn walking_run(db: &Db, wf: &str, cursor: &str) -> String {
    db.with_conn(|c| {
        let run = db::create_workflow_run(c, wf, None, Some(cursor), "{}").unwrap();
        db::update_workflow_run(
            c,
            &run.id,
            Some(cursor),
            "{}",
            db::RUN_STATUS_RUNNING,
            Some(r#"{"stack":[],"awaiting":null,"replay":false}"#),
        )
        .unwrap();
        run.id
    })
}

/// M4 — the STRUCTURAL mutations (which nodes exist, where they sit) are REFUSED while a run walks
/// the workflow: each would either strand that run's traversal (its progress lost at the next
/// run_start) or slide it onto the wrong sibling, and both used to happen without a word. The
/// refusal NAMES the run and the two ways out. A live run that has NEVER ticked blocks nothing — it
/// starts from the root of whatever tree it finds.
#[test]
fn a_structural_edit_is_refused_while_a_run_walks_the_workflow() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let root = add_node(&db, &wf, "sequence", None, "root");
    let leaf = add_node(&db, &wf, "note", Some(&root), "leaf");

    // A live run that has never ticked: nothing to lose, nothing blocked.
    let untouched = db
        .with_conn(|c| db::create_workflow_run(c, &wf, None, None, "{}"))
        .unwrap()
        .id;
    add_node(&db, &wf, "note", Some(&root), "still-fine");

    let run = walking_run(&db, &wf, &leaf);
    let refusals = [
        node_add(&db, &json!({ "workflow_id": wf, "type": "note", "parent_id": root })),
        node_remove(&db, &json!({ "node_id": leaf })),
        node_reorder(&db, &json!({ "node_id": leaf, "order_index": 5 })),
        workflow_delete(&db, &json!({ "workflow_id": wf })),
    ];
    for refused in refusals {
        let err = refused.expect_err("a structural edit is refused under a walking run");
        assert_eq!(err.code, "invalid_state");
        assert_eq!(
            err.data
                .as_ref()
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
            Some("workflow_has_live_run"),
        );
        assert!(
            err.message.contains(&run) && err.message.contains("run_delete"),
            "the refusal names the run AND the way out: {}",
            err.message
        );
    }
    // Nothing was committed (each verb rolls its transaction back).
    assert!(
        db.with_conn(|c| db::get_node(c, &leaf)).unwrap().is_some(),
        "the refused removal changed nothing"
    );

    // Once the run is gone, the very same edits go through — the guard is about the RUN, not the
    // workflow.
    run_delete(&db, &EngineRuntime::new(), &json!({ "run_id": run })).expect("drop the walking run");
    run_delete(&db, &EngineRuntime::new(), &json!({ "run_id": untouched })).expect("drop the idle run");
    node_remove(&db, &json!({ "node_id": leaf })).expect("node_remove once no run walks the tree");
    workflow_delete(&db, &json!({ "workflow_id": wf })).expect("workflow_delete too");
}

/// M4 — `node_update` is the ONE node mutation still allowed under a walking run: it moves no node
/// id, so the run keeps its whole progress. Refusing it would contradict nyx's own N26(a) advice
/// ("configure this gate's command_ids with node_update"), which run_start raises exactly when a run
/// is live. It is therefore applied AND announced: a `warnings` entry naming the run.
#[test]
fn node_update_is_allowed_under_a_walking_run_but_says_so() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let root = add_node(&db, &wf, "sequence", None, "root");
    let gate = node_add(
        &db,
        &json!({ "workflow_id": wf, "type": "gate_check", "parent_id": root,
                 "config": { "command_ids": [] } }),
    )
    .expect("gate node");
    let gate_id = id_of(&gate, "node");

    // No run walking yet → applied, and NOTHING to announce.
    let quiet = node_update(
        &db,
        &json!({ "node_id": gate_id, "config": { "command_ids": ["cmd-1"] } }),
    )
    .expect("node_update with no live run");
    assert_eq!(
        quiet.result["warnings"].as_array().expect("a warnings array").len(),
        0,
        "no run walks the workflow: no warning"
    );

    let run = walking_run(&db, &wf, &gate_id);
    let noisy = node_update(
        &db,
        &json!({ "node_id": gate_id, "config": { "command_ids": ["cmd-1", "cmd-2"] } }),
    )
    .expect("node_update is NOT refused under a walking run");
    let warnings = noisy.result["warnings"].as_array().expect("a warnings array");
    assert_eq!(warnings.len(), 1, "the walking run is announced: {warnings:?}");
    assert_eq!(warnings[0]["code"], "workflow_has_live_run");
    assert_eq!(warnings[0]["run_id"], json!(run));
    assert!(
        warnings[0]["message"].as_str().unwrap().contains(&run),
        "the sentence names the run that will execute the edit: {}",
        warnings[0]["message"]
    );

    // And the edit really landed (a warning is not a refusal).
    let stored = db.with_conn(|c| db::get_node(c, &gate_id)).unwrap().unwrap();
    assert!(stored.config.contains("cmd-2"), "the new config is persisted");
    // The run kept its progress — no node id moved.
    let row = db.with_conn(|c| db::get_workflow_run(c, &run)).unwrap().unwrap();
    assert_eq!(row.current_node_id.as_deref(), Some(gate_id.as_str()));
    assert!(row.traversal.is_some(), "its walk is untouched");
}

/// The PROVENANCE key is nyx's own record of WHICH NODE wrote each board key — the fact the runtime
/// consent check reads to tell a human's answer from a query's fact. A node able to name it as an
/// `output_key` (or a `write`, or a `retrieve.to`) would be writing its own answer to that question:
/// one node, one object, and every gate in the graph reads "a human filled this". So naming it is an
/// authoring ERROR, exactly like `__spec_write_granted__` and for the same reason — it is what makes
/// "only the engine writes it" a property rather than a habit.
#[test]
fn no_node_may_write_its_own_provenance() {
    let (db, project) = seeded();
    let wf = make_workflow(&db, "wf", Some(&project));
    let key = crate::engine::PROVENANCE_KEY;

    for (node_type, config) in [
        ("ask_human", json!({ "prompt": "?", "output_key": key })),
        ("query", json!({ "query": "spec_status", "write": key })),
        ("judge", json!({ "prompt": "?", "output_key": key })),
        ("produce", json!({ "output_key": key })),
    ] {
        let err = node_add(
            &db,
            &json!({ "workflow_id": wf, "type": node_type, "config": config }),
        )
        .unwrap_err();
        assert_eq!(
            err.code, "invalid_argument",
            "a '{node_type}' may not write the provenance record"
        );
        assert!(
            err.message.contains("RESERVED") && err.message.contains(key),
            "the refusal names the reserved key: {}",
            err.message
        );
    }

    // The SAME rule over a `sub_workflow`'s board copies — the widest write in the vocabulary, and the
    // one that lands on the PARENT board straight from a child run's own blackboard.
    for field in ["inject", "retrieve"] {
        let err = node_add(
            &db,
            &json!({
                "workflow_id": wf, "type": "sub_workflow",
                "config": { "scope": "coder", field: [{ "from": "anything", "to": key }] }
            }),
        )
        .unwrap_err();
        assert!(
            err.message.contains("RESERVED") && err.message.contains(key),
            "a '{field}' copy may not land on the provenance either: {}",
            err.message
        );
    }

    // A key that merely LOOKS like it — the guard is an equality on the reserved name, not a ban on
    // underscores, and refusing a graph's own `__notes__` would be a rule nobody asked for.
    node_add(
        &db,
        &json!({
            "workflow_id": wf, "type": "judge",
            "config": { "prompt": "?", "output_key": "written_by" }
        }),
    )
    .expect("an ordinary key that resembles the reserved one is not the reserved one");
}
