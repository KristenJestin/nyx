//! Unit tests for the behavior-tree interpreter (task 01KY71J03W9GST9050G96HCM53): every node type
//! executes and advances correctly, `loop`/`selector` conditions drive the flow, the blackboard is
//! updated, and the tree reconstructs from real DB rows.

use super::*;
use serde_json::json;

/// Build a `db::Node` row directly (its fields are public), so the tree tests need no live DB.
fn node(id: &str, parent: Option<&str>, order: i32, ty: &str, config: &str) -> db::Node {
    db::Node {
        id: id.to_string(),
        workflow_id: "wf".to_string(),
        parent_id: parent.map(|p| p.to_string()),
        order_index: order,
        type_: ty.to_string(),
        label: id.to_string(),
        config: config.to_string(),
        created_at: 0,
        updated_at: 0,
    }
}

/// Drive an engine to completion, feeding each action a scripted result chosen by `respond`.
/// Returns the ordered actions the engine yielded.
fn drive(engine: &mut Engine, mut respond: impl FnMut(&Action) -> NodeResult) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut next_result: Option<NodeResult> = None;
    loop {
        match engine.tick(next_result.take()).expect("tick ok") {
            Step::Action(action) => {
                next_result = Some(respond(&action));
                actions.push(action);
            }
            Step::Done => return actions,
        }
    }
}

// ---------------------------------------------------------------------------
// sequence
// ---------------------------------------------------------------------------

#[test]
fn sequence_runs_leaves_in_order() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node("a", Some("root"), 0, db::NODE_TYPE_NOTE, r#"{"text":"a"}"#),
        node("b", Some("root"), 1, db::NODE_TYPE_NOTE, r#"{"text":"b"}"#),
        node("c", Some("root"), 2, db::NODE_TYPE_NOTE, r#"{"text":"c"}"#),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);

    // The cursor advances a, b, c in order; each yields a Note with its text.
    let actions = drive(&mut engine, |_| NodeResult::Ack);
    assert_eq!(
        actions,
        vec![
            Action::Note {
                node_id: "a".into(),
                text: "a".into(),
                clear_gate_block: false,
                revoke_spec_write: false,
            },
            Action::Note {
                node_id: "b".into(),
                text: "b".into(),
                clear_gate_block: false,
                revoke_spec_write: false,
            },
            Action::Note {
                node_id: "c".into(),
                text: "c".into(),
                clear_gate_block: false,
                revoke_spec_write: false,
            },
        ]
    );
    assert!(engine.is_done());
    assert_eq!(engine.trace(), &["a", "b", "c"]);
    assert_eq!(engine.current_node_id(), None, "no cursor once done");
}

/// A sequence advances its cursor: `current_node_id` names the parked leaf while an action is pending.
#[test]
fn cursor_tracks_the_parked_leaf() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node("w", Some("root"), 0, db::NODE_TYPE_WAIT, "{}"),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);
    assert_eq!(
        engine.current_node_id(),
        None,
        "no cursor before the first tick"
    );
    let step = engine.tick(None).unwrap();
    assert_eq!(
        step,
        Step::Action(Action::Wait {
            node_id: "w".into()
        })
    );
    assert_eq!(
        engine.current_node_id(),
        Some("w"),
        "parked on the wait leaf"
    );
}

// ---------------------------------------------------------------------------
// loop
// ---------------------------------------------------------------------------

#[test]
fn loop_repeats_body_until_verdict() {
    // loop{ until verdict == "clair" }  body = [ judge ]
    let rows = vec![
        node(
            "loop",
            None,
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"verdict","eq":"clair"}}"#,
        ),
        node(
            "judge",
            Some("loop"),
            0,
            db::NODE_TYPE_JUDGE,
            r#"{"prompt":"assez clair ?"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("loop")).unwrap();
    let mut engine = Engine::start(tree);

    // First judge → "pas_clair" (loop re-runs); second judge → "clair" (loop exits).
    let mut verdicts = vec!["clair", "pas_clair"]; // popped from the end
    let actions = drive(&mut engine, |action| {
        assert!(matches!(action, Action::Judge { .. }));
        NodeResult::Verdict(json!(verdicts.pop().unwrap()))
    });

    assert_eq!(actions.len(), 2, "the body ran twice (repeat-until)");
    assert_eq!(engine.trace(), &["judge", "judge"]);
    assert_eq!(
        engine.blackboard().get("verdict"),
        Some(&json!("clair")),
        "the exit verdict is on the blackboard"
    );
    assert_eq!(
        engine.blackboard().get("loop_iter.loop"),
        Some(&json!(1)),
        "the loop recorded its final iteration index UNDER ITS OWN NODE ID (0-based: pass 0 then \
         pass 1) — a flat `loop_iter` is a single counter for every loop in the tree"
    );
}

/// NESTED loops each keep their OWN count. A single flat `loop_iter` made the inner loop overwrite it
/// on every pass and the outer one overwrite it again when its body finished, so the number described
/// whichever loop completed a pass last — and nothing reading it could tell which.
#[test]
fn nested_loops_each_record_their_own_iteration_count() {
    let rows = vec![
        node(
            "outer",
            None,
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"outer_done","eq":"yes"}}"#,
        ),
        node(
            "inner",
            Some("outer"),
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"verdict","eq":"clair"}}"#,
        ),
        node(
            "judge",
            Some("inner"),
            0,
            db::NODE_TYPE_JUDGE,
            r#"{"prompt":"assez clair ?"}"#,
        ),
        node(
            "close",
            Some("outer"),
            1,
            db::NODE_TYPE_PRODUCE,
            r#"{"output_key":"outer_done"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("outer")).unwrap();
    let mut engine = Engine::start(tree);

    // The inner loop needs THREE judge passes to clear; the outer one exits on the first `close`.
    let mut verdicts = vec!["clair", "pas_clair", "pas_clair"]; // popped from the end
    drive(&mut engine, |action| match action {
        Action::Judge { .. } => NodeResult::Verdict(json!(verdicts.pop().unwrap())),
        _ => NodeResult::Artifact(json!("yes")),
    });

    assert_eq!(
        engine.blackboard().get("loop_iter.inner"),
        Some(&json!(2)),
        "the inner loop's own count: three passes, 0-based"
    );
    assert_eq!(
        engine.blackboard().get("loop_iter.outer"),
        Some(&json!(0)),
        "the outer loop's own count, UNTOUCHED by the inner loop's three passes"
    );
}

/// A judge whose `output_key` redirects the verdict to a custom blackboard key still drives the loop.
#[test]
fn loop_reads_custom_judge_output_key() {
    let rows = vec![
        node(
            "loop",
            None,
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"clear","eq":true}}"#,
        ),
        node(
            "judge",
            Some("loop"),
            0,
            db::NODE_TYPE_JUDGE,
            r#"{"prompt":"clear?","output_key":"clear"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("loop")).unwrap();
    let mut engine = Engine::start(tree);
    let mut answers = vec![true, false]; // popped from the end
    let actions = drive(&mut engine, |_| {
        NodeResult::Verdict(json!(answers.pop().unwrap()))
    });
    assert_eq!(actions.len(), 2);
    assert_eq!(engine.blackboard().get("clear"), Some(&json!(true)));
}

#[test]
fn loop_max_iterations_guards_a_never_true_until() {
    let rows = vec![
        node(
            "loop",
            None,
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"verdict","eq":"never"},"max_iterations":3}"#,
        ),
        node("judge", Some("loop"), 0, db::NODE_TYPE_JUDGE, "{}"),
    ];
    let tree = Tree::from_rows(&rows, Some("loop")).unwrap();
    let mut engine = Engine::start(tree);

    // Always answer something the `until` never accepts → the guard must trip.
    let mut err = None;
    let mut result = None;
    for _ in 0..20 {
        match engine.tick(result.take()) {
            Ok(Step::Action(_)) => result = Some(NodeResult::Verdict(json!("no"))),
            Ok(Step::Done) => panic!("a never-true loop must not complete"),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert_eq!(
        err,
        Some(EngineError::LoopLimit {
            node_id: "loop".into(),
            max: 3,
            condition: "verdict == \"never\"".into(),
            observed: "verdict = \"no\"".into(),
        })
    );
}

/// H3(2) — the backstop's message is the ONLY thing a user gets when a run dies on it, so it must
/// DIAGNOSE: name the condition that failed to become true, and the value last observed for it.
/// `loop <uuid> exceeded 20 iterations without exiting` named neither — nothing to act on.
#[test]
fn the_loop_limit_message_names_the_condition_and_what_it_last_read() {
    let rows = vec![
        node(
            "rounds",
            None,
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"design_locked","eq":true},"max_iterations":2}"#,
        ),
        node("query", Some("rounds"), 0, db::NODE_TYPE_JUDGE, "{}"),
    ];
    let tree = Tree::from_rows(&rows, Some("rounds")).unwrap();
    let mut engine = Engine::start(tree);
    engine.set_board("design_locked", json!(false));

    let mut err = None;
    let mut result = None;
    for _ in 0..10 {
        match engine.tick(result.take()) {
            Ok(Step::Action(_)) => result = Some(NodeResult::Ack),
            Ok(Step::Done) => panic!("a never-true loop must not complete"),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    let message = err.expect("the backstop trips").to_string();
    assert!(
        message.contains("design_locked == true"),
        "the message names WHICH condition never held: {message}"
    );
    assert!(
        message.contains("design_locked = false"),
        "…and what it last observed for it: {message}"
    );
}

/// H3(1) — a loop whose exit reads a FROZEN key fails on its FIRST failed exit check, not its
/// twentieth. The body runs ONCE (repeat-until: the exit is checked after the body, so a first pass
/// is owed), and the run dies there instead of paying the body's cost 20 times over.
#[test]
fn a_loop_gated_on_a_frozen_key_dies_on_the_first_round() {
    let rows = vec![
        node(
            "rounds",
            None,
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"design_locked","eq":true},"max_iterations":20}"#,
        ),
        node("spawn", Some("rounds"), 0, db::NODE_TYPE_SPAWN_SUBAGENT, "{}"),
    ];
    let tree = Tree::from_rows(&rows, Some("rounds")).unwrap();
    let mut engine = Engine::start(tree);
    engine.set_board("design_locked", json!(false));
    engine.freeze_key("design_locked", "the run carries no Spec");

    let mut spawns = 0;
    let mut err = None;
    let mut result = None;
    for _ in 0..30 {
        match engine.tick(result.take()) {
            Ok(Step::Action(_)) => {
                spawns += 1;
                result = Some(NodeResult::Diff("…".into()));
            }
            Ok(Step::Done) => panic!("an unreachable exit must not complete the loop"),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    assert_eq!(spawns, 1, "the body ran ONCE — the trap is not paid 20 times over");
    let message = err.expect("the loop refuses to spin").to_string();
    assert!(
        message.contains("design_locked == true") && message.contains("design_locked = false"),
        "the failure diagnoses the condition and its value: {message}"
    );
    assert!(
        message.contains("no Spec"),
        "…and WHY it can never change: {message}"
    );
}

/// …and freezing a key the loop does NOT read changes nothing: the backstop stays the only guard for
/// a loop whose exit can still legitimately turn.
#[test]
fn freezing_an_unrelated_key_does_not_kill_a_live_loop() {
    let rows = vec![
        node(
            "loop",
            None,
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"clear","eq":true}}"#,
        ),
        node(
            "judge",
            Some("loop"),
            0,
            db::NODE_TYPE_JUDGE,
            r#"{"prompt":"clear?","output_key":"clear"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("loop")).unwrap();
    let mut engine = Engine::start(tree);
    engine.freeze_key("spec_status", "the run carries no Spec");
    let mut answers = vec![true, false]; // popped from the end
    let actions = drive(&mut engine, |_| {
        NodeResult::Verdict(json!(answers.pop().unwrap()))
    });
    assert_eq!(actions.len(), 2, "the loop still ran its two rounds and exited");
}

// ---------------------------------------------------------------------------
// selector
// ---------------------------------------------------------------------------

/// A gate_check writes `gate_green`; the following selector routes on it — the exact red/green branch
/// of the P1 loop, now driven by the general interpreter.
#[test]
fn selector_routes_on_the_gate_verdict() {
    // seq[ gate_check, selector( green→note"advance" / red→note"retry" ) ]
    let make_tree = || {
        let rows = vec![
            node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
            node(
                "gate",
                Some("root"),
                0,
                db::NODE_TYPE_GATE_CHECK,
                r#"{"command_ids":["typecheck"],"blocking":true}"#,
            ),
            node(
                "sel",
                Some("root"),
                1,
                db::NODE_TYPE_SELECTOR,
                r#"{"branches":[{"cond":{"var":"gate_green","eq":true},"child":0},{"cond":{"var":"gate_green","eq":false},"child":1}]}"#,
            ),
            node(
                "advance",
                Some("sel"),
                0,
                db::NODE_TYPE_NOTE,
                r#"{"text":"advance"}"#,
            ),
            node(
                "retry",
                Some("sel"),
                1,
                db::NODE_TYPE_NOTE,
                r#"{"text":"retry"}"#,
            ),
        ];
        Tree::from_rows(&rows, Some("root")).unwrap()
    };

    // GREEN gate → the selector descends into the "advance" branch.
    let mut green = Engine::start(make_tree());
    let green_actions = drive(&mut green, |action| match action {
        Action::GateCheck { .. } => NodeResult::Gate(true),
        _ => NodeResult::Ack,
    });
    assert_eq!(
        green_actions.last().unwrap(),
        &Action::Note {
            node_id: "advance".into(),
            text: "advance".into(),
            clear_gate_block: false,
            revoke_spec_write: false,
        }
    );
    assert_eq!(green.blackboard().get("gate_green"), Some(&json!(true)));
    assert_eq!(
        green.trace(),
        &["gate", "advance"],
        "only the green branch ran"
    );

    // RED gate → the selector descends into the "retry" branch instead.
    let mut red = Engine::start(make_tree());
    let red_actions = drive(&mut red, |action| match action {
        Action::GateCheck { .. } => NodeResult::Gate(false),
        _ => NodeResult::Ack,
    });
    assert_eq!(
        red_actions.last().unwrap(),
        &Action::Note {
            node_id: "retry".into(),
            text: "retry".into(),
            clear_gate_block: false,
            revoke_spec_write: false,
        }
    );
    assert_eq!(red.trace(), &["gate", "retry"], "only the red branch ran");
}

#[test]
fn selector_falls_through_to_the_default_branch() {
    // branch 0 guarded on an absent var (fails) → the default (cond null) branch is taken.
    let rows = vec![
        node(
            "sel",
            None,
            0,
            db::NODE_TYPE_SELECTOR,
            r#"{"branches":[{"cond":{"var":"verdict","eq":"yes"},"child":0},{"cond":null,"child":1}]}"#,
        ),
        node(
            "guarded",
            Some("sel"),
            0,
            db::NODE_TYPE_NOTE,
            r#"{"text":"guarded"}"#,
        ),
        node(
            "default",
            Some("sel"),
            1,
            db::NODE_TYPE_NOTE,
            r#"{"text":"default"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("sel")).unwrap();
    let mut engine = Engine::start(tree);
    let actions = drive(&mut engine, |_| NodeResult::Ack);
    assert_eq!(
        actions,
        vec![Action::Note {
            node_id: "default".into(),
            text: "default".into(),
            clear_gate_block: false,
            revoke_spec_write: false,
        }]
    );
}

/// J1 — a selector that matches NO branch FAILS the walk instead of completing as a silent no-op.
/// That no-op is how a run reported success for a flow that routed nothing: ndoc's terminal fork read
/// a free-text answer, matched neither of its conditions, and the walk carried straight on to `done`.
/// The failure names the selector and, for every branch, what the board actually held against it.
#[test]
fn a_selector_that_matches_nothing_fails_instead_of_walking_on() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "sel",
            Some("root"),
            0,
            db::NODE_TYPE_SELECTOR,
            r#"{"branches":[{"cond":{"var":"decision","eq":"approve"},"child":0},{"cond":{"var":"decision","eq":"cancel"},"child":1}]}"#,
        ),
        node("synced", Some("sel"), 0, db::NODE_TYPE_NOTE, r#"{"text":"synced"}"#),
        node("abandoned", Some("sel"), 1, db::NODE_TYPE_NOTE, r#"{"text":"abandoned"}"#),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut board = Blackboard::new();
    board.set(
        "decision",
        json!("changes — it draws the parser's line in the wrong place"),
    );
    let mut engine = Engine::new(tree, board);

    let err = engine.tick(None).expect_err("the selector can route nothing");
    let EngineError::SelectorNoBranch {
        node_id,
        label,
        tried,
    } = &err
    else {
        panic!("expected SelectorNoBranch, got {err:?}");
    };
    assert_eq!(node_id, "sel");
    assert_eq!(label, "sel");
    assert!(
        tried.contains(r#"decision == "approve""#) && tried.contains(r#"decision == "cancel""#),
        "the failure lists every branch it tried: {tried}"
    );
    assert!(
        tried.contains("draws the parser's line"),
        "and what the board actually held against them: {tried}"
    );
}

/// A branch with no explicit `child` defaults to its own position among the branches.
#[test]
fn selector_branch_child_defaults_to_position() {
    let rows = vec![
        node(
            "sel",
            None,
            0,
            db::NODE_TYPE_SELECTOR,
            // Two branches, no `child` index: branch 0 fails, branch 1 (default) → child at position 1.
            r#"{"branches":[{"cond":{"var":"x","eq":1}},{"cond":null}]}"#,
        ),
        node(
            "zero",
            Some("sel"),
            0,
            db::NODE_TYPE_NOTE,
            r#"{"text":"zero"}"#,
        ),
        node(
            "one",
            Some("sel"),
            1,
            db::NODE_TYPE_NOTE,
            r#"{"text":"one"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("sel")).unwrap();
    let mut engine = Engine::start(tree);
    let actions = drive(&mut engine, |_| NodeResult::Ack);
    assert_eq!(actions[0].node_id(), "one");
}

// ---------------------------------------------------------------------------
// every leaf type
// ---------------------------------------------------------------------------

#[test]
fn every_leaf_type_yields_its_action_and_updates_the_blackboard() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "sp",
            Some("root"),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            r#"{"agent_id":"coder"}"#,
        ),
        node(
            "gt",
            Some("root"),
            1,
            db::NODE_TYPE_GATE_CHECK,
            r#"{"command_ids":["tc"],"blocking":true}"#,
        ),
        node(
            "jg",
            Some("root"),
            2,
            db::NODE_TYPE_JUDGE,
            r#"{"prompt":"q"}"#,
        ),
        node(
            "tr",
            Some("root"),
            3,
            db::NODE_TYPE_TRANSITION,
            r#"{"target_state":"in_review"}"#,
        ),
        node("wt", Some("root"), 4, db::NODE_TYPE_WAIT, "{}"),
        node(
            "nt",
            Some("root"),
            5,
            db::NODE_TYPE_NOTE,
            r#"{"text":"hi"}"#,
        ),
        node(
            "ah",
            Some("root"),
            6,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"ok?"}"#,
        ),
        node("pr", Some("root"), 7, db::NODE_TYPE_PRODUCE, "{}"),
        node(
            "qr",
            Some("root"),
            8,
            db::NODE_TYPE_QUERY,
            r#"{"query":"spec_status","write":"spec_status"}"#,
        ),
        node(
            "fk",
            Some("root"),
            9,
            db::NODE_TYPE_FORK_SPEC,
            r#"{"gate_on":"revise_ok"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);

    let actions = drive(&mut engine, |action| match action {
        Action::SpawnSubagent { agent_id, .. } => {
            assert_eq!(agent_id, "coder");
            NodeResult::Diff("--- a\n+++ b\n".into())
        }
        Action::GateCheck {
            command_ids,
            blocking,
            ..
        } => {
            assert_eq!(command_ids, &vec!["tc".to_string()]);
            assert!(*blocking);
            NodeResult::Gate(true)
        }
        Action::Judge { prompt, .. } => {
            assert_eq!(prompt, "q");
            NodeResult::Verdict(json!("ok"))
        }
        Action::Transition { target_state, .. } => {
            assert_eq!(target_state, "in_review");
            NodeResult::Ack
        }
        Action::Wait { .. } => NodeResult::Ack,
        Action::Note { text, .. } => {
            assert_eq!(text, "hi");
            NodeResult::Ack
        }
        Action::AskHuman { prompt, .. } => {
            assert_eq!(prompt, "ok?");
            NodeResult::Human(json!({ "approved": true }))
        }
        Action::Produce { .. } => NodeResult::Artifact(json!({ "kind": "finding" })),
        Action::Query { query, write, .. } => {
            assert_eq!(query, "spec_status");
            assert_eq!(write, "spec_status");
            NodeResult::Ack
        }
        Action::ForkSpec { gate_on, .. } => {
            assert_eq!(gate_on.as_deref(), Some("revise_ok"));
            NodeResult::Ack
        }
        // This tree carries no transition_task / create_workspace / sub_workflow leaf; the arms keep
        // the match total.
        Action::TransitionTask { .. } => NodeResult::Ack,
        Action::CreateWorkspace { .. } => NodeResult::Ack,
        Action::SubWorkflow { .. } => NodeResult::SubResult(json!({ "status": "done" })),
    });

    // Every leaf yielded exactly one action, in order.
    assert_eq!(actions.len(), 10);
    assert_eq!(
        engine.trace(),
        &["sp", "gt", "jg", "tr", "wt", "nt", "ah", "pr", "qr", "fk"]
    );
    assert!(engine.is_done());

    // The type-specific results landed on the blackboard.
    let bb = engine.blackboard();
    assert_eq!(bb.get("spawns"), Some(&json!(1)));
    assert_eq!(bb.get("last_diff"), Some(&json!("--- a\n+++ b\n")));
    assert_eq!(bb.get("gate_green"), Some(&json!(true)));
    assert_eq!(bb.get("verdict"), Some(&json!("ok")));
    assert_eq!(bb.get("human"), Some(&json!({ "approved": true })));
    assert_eq!(bb.get("artifact"), Some(&json!({ "kind": "finding" })));
}

// ---------------------------------------------------------------------------
// nested composites — the ndev-shaped loop (mirrors the P1 spike, generalised)
// ---------------------------------------------------------------------------

/// The dev-loop shape: `seq[ loop( until gate_green ){ spawn(coder) → gate } , transition ]`. A RED
/// gate re-loops (re-spawns); the transition fires ONLY after the gate goes green — the same
/// "never advance while red" property the P1 spike proved, now emergent from the graph.
#[test]
fn ndev_shaped_loop_advances_only_after_green_gate() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "phase",
            Some("root"),
            0,
            db::NODE_TYPE_LOOP,
            r#"{"until":{"var":"gate_green","eq":true}}"#,
        ),
        node(
            "spawn",
            Some("phase"),
            0,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            r#"{"agent_id":"coder"}"#,
        ),
        node(
            "gate",
            Some("phase"),
            1,
            db::NODE_TYPE_GATE_CHECK,
            r#"{"command_ids":["typecheck"],"blocking":true}"#,
        ),
        node(
            "advance",
            Some("root"),
            1,
            db::NODE_TYPE_TRANSITION,
            r#"{"target_state":"in_review"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);

    // Gate is red on the first pass, green on the second. A Transition must never appear before green.
    let mut gate_results = vec![true, false]; // popped from the end: first false, then true
    let mut saw_green = false;
    let actions = drive(&mut engine, |action| match action {
        Action::SpawnSubagent { .. } => NodeResult::Diff("d".into()),
        Action::GateCheck { .. } => {
            let green = gate_results.pop().unwrap();
            if green {
                saw_green = true;
            }
            NodeResult::Gate(green)
        }
        Action::Transition { .. } => {
            assert!(
                saw_green,
                "the transition fired only after the gate went green"
            );
            NodeResult::Ack
        }
        _ => NodeResult::Ack,
    });

    // spawn → gate(red) → spawn → gate(green) → transition.
    assert_eq!(
        engine.trace(),
        &["spawn", "gate", "spawn", "gate", "advance"]
    );
    assert_eq!(
        engine.blackboard().get("spawns"),
        Some(&json!(2)),
        "the red gate forced a re-spawn"
    );
    assert_eq!(engine.blackboard().get("gate_green"), Some(&json!(true)));
    assert!(matches!(actions.last().unwrap(), Action::Transition { .. }));
}

// ---------------------------------------------------------------------------
// tree reconstruction (bridge to the db CRUD) + condition unit tests + errors
// ---------------------------------------------------------------------------

/// The tree reconstructs from REAL `nodes` rows written through the [`db`] CRUD (task T2), then the
/// engine walks it — proving the "arbre reconstructible" bridge end-to-end.
#[test]
fn tree_reconstructs_from_db_rows_and_walks() {
    let mut conn = db::open_in_memory();
    let wf = db::create_workflow(&mut conn, None, "ndev", db::WORKFLOW_SCOPE_NDEV, None).unwrap();
    let root = db::create_node(
        &mut conn,
        &wf.id,
        None,
        0,
        db::NODE_TYPE_SEQUENCE,
        "root",
        "{}",
    )
    .unwrap();
    db::create_node(
        &mut conn,
        &wf.id,
        Some(&root.id),
        0,
        db::NODE_TYPE_NOTE,
        "hello",
        r#"{"text":"hello"}"#,
    )
    .unwrap();
    db::create_node(
        &mut conn,
        &wf.id,
        Some(&root.id),
        1,
        db::NODE_TYPE_WAIT,
        "pause",
        "{}",
    )
    .unwrap();
    db::set_workflow_root(&mut conn, &wf.id, Some(&root.id)).unwrap();

    // Reconstruct straight from the persisted rows + the workflow's root pointer.
    let rows = db::list_nodes(&mut conn, &wf.id).unwrap();
    let stored = db::get_workflow(&mut conn, &wf.id).unwrap().unwrap();
    let tree = Tree::from_rows(&rows, stored.root_node_id.as_deref()).unwrap();
    assert_eq!(tree.len(), 3);
    assert_eq!(tree.root_id(), root.id);

    let mut engine = Engine::start(tree);
    let actions = drive(&mut engine, |_| NodeResult::Ack);
    assert_eq!(actions.len(), 2, "the note then the wait");
    assert!(matches!(actions[0], Action::Note { .. }));
    assert!(matches!(actions[1], Action::Wait { .. }));
    assert!(engine.is_done());
}

/// `Tree::from_rows` resolves the root from the single `parent_id IS NULL` row when no `root_id` is
/// given.
#[test]
fn tree_root_inferred_from_parentless_row() {
    let rows = vec![node(
        "only-root",
        None,
        0,
        db::NODE_TYPE_NOTE,
        r#"{"text":"x"}"#,
    )];
    let tree = Tree::from_rows(&rows, None).unwrap();
    assert_eq!(tree.root_id(), "only-root");
}

#[test]
fn tree_reconstruction_rejects_malformed_graphs() {
    // Unknown node type.
    let bad_type = vec![node("r", None, 0, "frobnicate", "{}")];
    assert_eq!(
        Tree::from_rows(&bad_type, None).unwrap_err(),
        EngineError::UnknownNodeType("frobnicate".into())
    );

    // A child pointing at a parent that is not in the rows.
    let dangling = vec![node("c", Some("ghost"), 0, db::NODE_TYPE_NOTE, "{}")];
    assert_eq!(
        Tree::from_rows(&dangling, None).unwrap_err(),
        EngineError::DanglingChild {
            node_id: "c".into(),
            parent_id: "ghost".into()
        }
    );

    // No parentless row and no explicit root.
    let rootless = vec![
        node("a", Some("b"), 0, db::NODE_TYPE_NOTE, "{}"),
        node("b", Some("a"), 0, db::NODE_TYPE_NOTE, "{}"),
    ];
    assert_eq!(
        Tree::from_rows(&rootless, None).unwrap_err(),
        EngineError::MissingRoot(None)
    );

    // Config that is not a JSON object.
    let bad_cfg = vec![node("r", None, 0, db::NODE_TYPE_NOTE, "[1,2,3]")];
    assert!(matches!(
        Tree::from_rows(&bad_cfg, None),
        Err(EngineError::BadConfig { .. })
    ));

    // A CYCLE reachable from an EXPLICIT root: the parentless-row check cannot see it (the root is
    // named), so the tree walk would descend forever. It is refused at reconstruction instead.
    let cyclic = vec![
        node("r", Some("c"), 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node("c", Some("r"), 0, db::NODE_TYPE_SEQUENCE, "{}"),
    ];
    assert!(
        matches!(
            Tree::from_rows(&cyclic, Some("r")),
            Err(EngineError::Cycle { .. })
        ),
        "a root-reachable cycle is refused, not walked"
    );
    // Same for a node that is its OWN parent.
    let self_parent = vec![node("r", Some("r"), 0, db::NODE_TYPE_SEQUENCE, "{}")];
    assert!(matches!(
        Tree::from_rows(&self_parent, Some("r")),
        Err(EngineError::Cycle { .. })
    ));
}

#[test]
fn condition_matches_eq_ne_truthy_and_default() {
    let mut bb = Blackboard::new();
    bb.set("verdict", json!("clair"));
    bb.set("count", json!(0));
    bb.set("flag", json!(true));

    let eq: Condition = serde_json::from_value(json!({"var":"verdict","eq":"clair"})).unwrap();
    assert!(eq.matches(&bb));
    let eq_no: Condition = serde_json::from_value(json!({"var":"verdict","eq":"flou"})).unwrap();
    assert!(!eq_no.matches(&bb));

    let ne: Condition = serde_json::from_value(json!({"var":"verdict","ne":"flou"})).unwrap();
    assert!(ne.matches(&bb));

    // truthy: a zero number is falsy, a `true` flag is truthy, an absent var is falsy.
    let zero_truthy: Condition =
        serde_json::from_value(json!({"var":"count","truthy":true})).unwrap();
    assert!(!zero_truthy.matches(&bb));
    let flag_truthy: Condition =
        serde_json::from_value(json!({"var":"flag","truthy":true})).unwrap();
    assert!(flag_truthy.matches(&bb));
    let absent: Condition = serde_json::from_value(json!({"var":"missing"})).unwrap();
    assert!(!absent.matches(&bb), "a bare absent var is falsy");

    // `truthy: false` is the NEGATIVE test, not an ignored field: an authored
    // `{"var":"flag","truthy":false}` means "flag is NOT set", so it must NOT match a `true` flag.
    let flag_falsy: Condition =
        serde_json::from_value(json!({"var":"flag","truthy":false})).unwrap();
    assert!(
        !flag_falsy.matches(&bb),
        "truthy:false must not match a truthy value"
    );
    let zero_falsy: Condition =
        serde_json::from_value(json!({"var":"count","truthy":false})).unwrap();
    assert!(zero_falsy.matches(&bb), "truthy:false matches a zero");
    let absent_falsy: Condition =
        serde_json::from_value(json!({"var":"missing","truthy":false})).unwrap();
    assert!(
        absent_falsy.matches(&bb),
        "truthy:false matches an absent var"
    );

    // always wins; an empty condition is false.
    let always: Condition = serde_json::from_value(json!({"always":true})).unwrap();
    assert!(always.matches(&bb));
    let empty: Condition = serde_json::from_value(json!({})).unwrap();
    assert!(!empty.matches(&bb));
}

#[test]
fn blackboard_json_round_trips_and_dot_path_reads() {
    let mut bb = Blackboard::from_json_str(r#"{"a":{"b":2}}"#);
    assert_eq!(bb.get("a.b"), Some(&json!(2)));
    assert_eq!(bb.get("a.missing"), None);
    bb.set("c", json!("x"));
    let round = Blackboard::from_json_str(&bb.to_json_string());
    assert_eq!(round, bb);
    // A non-object string degrades to empty.
    assert_eq!(Blackboard::from_json_str("not json"), Blackboard::new());
}

/// A lone leaf as the whole tree walks in one action then completes (the root-is-a-leaf path).
#[test]
fn single_leaf_root_completes() {
    let rows = vec![node(
        "only",
        None,
        0,
        db::NODE_TYPE_NOTE,
        r#"{"text":"solo"}"#,
    )];
    let tree = Tree::from_rows(&rows, Some("only")).unwrap();
    let mut engine = Engine::start(tree);
    let step = engine.tick(None).unwrap();
    assert_eq!(
        step,
        Step::Action(Action::Note {
            node_id: "only".into(),
            text: "solo".into(),
            clear_gate_block: false,
            revoke_spec_write: false,
        })
    );
    assert_eq!(engine.tick(Some(NodeResult::Ack)).unwrap(), Step::Done);
    assert!(engine.is_done());
}

/// Feeding a result when nothing is awaiting is a protocol error.
#[test]
fn tick_with_unexpected_result_errors() {
    let rows = vec![node("r", None, 0, db::NODE_TYPE_SEQUENCE, "{}")];
    let tree = Tree::from_rows(&rows, Some("r")).unwrap();
    let mut engine = Engine::new(tree, Blackboard::new());
    assert_eq!(
        engine.tick(Some(NodeResult::Ack)),
        Err(EngineError::UnexpectedResult)
    );
}

/// `NodeType` round-trips through its DB string, and composites are flagged.
#[test]
fn node_type_maps_to_and_from_db_strings() {
    for ty in db::NODE_TYPES {
        let nt = NodeType::from_db(ty).expect("known type");
        assert_eq!(nt.as_db(), ty);
    }
    assert!(NodeType::from_db("nope").is_none());
    assert!(NodeType::Sequence.is_composite());
    assert!(NodeType::Loop.is_composite());
    assert!(NodeType::Selector.is_composite());
    assert!(!NodeType::Judge.is_composite());
}

// ---------------------------------------------------------------------------
// N9 — output_key (ask_human/produce), blackboard append, prompt interpolation
// ---------------------------------------------------------------------------

/// N9(a) — `ask_human` and `produce` honor `output_key` (like `judge`), instead of the fixed
/// `human` / `artifact` keys — so a graph routes their results where its conditions read them.
#[test]
fn ask_human_and_produce_honor_output_key() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "q",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"?","output_key":"decision"}"#,
        ),
        node(
            "p",
            Some("root"),
            1,
            db::NODE_TYPE_PRODUCE,
            r#"{"output_key":"spec_section"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);
    drive(&mut engine, |a| match a {
        Action::AskHuman { .. } => NodeResult::Human(json!("yes")),
        Action::Produce { .. } => NodeResult::Artifact(json!({ "title": "S" })),
        _ => NodeResult::Ack,
    });
    let board = engine.blackboard();
    assert_eq!(board.get("decision"), Some(&json!("yes")));
    assert!(board.get("human").is_none(), "not the fixed default key");
    assert_eq!(board.get("spec_section"), Some(&json!({ "title": "S" })));
    assert!(board.get("artifact").is_none());
}

/// N9(b) — `append` ACCUMULATES results into a JSON array instead of overwriting, so a loop keeps its
/// whole history (the grill's Q&A) rather than clobbering it each turn.
#[test]
fn append_accumulates_into_an_array() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "q1",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"?","output_key":"log","append":true}"#,
        ),
        node(
            "q2",
            Some("root"),
            1,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"?","output_key":"log","append":true}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);
    let mut answers = vec![json!("second"), json!("first")];
    drive(&mut engine, |a| match a {
        Action::AskHuman { .. } => NodeResult::Human(answers.pop().unwrap()),
        _ => NodeResult::Ack,
    });
    assert_eq!(
        engine.blackboard().get("log"),
        Some(&json!(["first", "second"])),
        "both decisions accumulated (not overwritten)"
    );
}

/// N9(c) — a prompt/text interpolates `{{path}}` from the blackboard, so a looped ask_human/judge
/// asks a question that MOVES with the run's state. Absent keys drop to empty; no-placeholder
/// templates are unchanged (backward-compatible).
#[test]
fn prompts_interpolate_from_the_blackboard() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "q",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"Round {{round}}: {{topic}} — clear? {{missing}}"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);
    engine.set_board("round", json!(2));
    engine.set_board("topic", json!("auth"));
    match engine.tick(None).unwrap() {
        Step::Action(Action::AskHuman { prompt, .. }) => {
            assert_eq!(prompt, "Round 2: auth — clear? ");
        }
        other => panic!("expected ask_human, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// ask_human_commitment / answer_decides — the two readings, both evaluated
// ---------------------------------------------------------------------------

/// The `/nspec` `go` shape, reduced to its minimum: an `ask_human` whose answer ROUTES (through a
/// `selector` branch comparing it to an affirmative literal) to a `create_workspace` whose OWN
/// `gate_on` names a key a `query` writes — the PREVALIDATION, not the answer.
///
/// This is the graph `answer_decides` used to misread. Its early return fired on the `gate_on`
/// (`workspace_free`), compared it to the answer key (`ready_ok`), found no match, and reported that
/// NOTHING a human answers governs the folder — abandoning the climb to the selector that is the
/// actual consent. There is deliberately NO engaging `transition` beside the `create_workspace` here:
/// in the shipped `/nspec` the human turn survives only because such a neighbour gates on `ready_ok`,
/// and that neighbour is a coincidence of that graph, not a property of the shape.
#[test]
fn an_answer_routing_to_a_workspace_gated_on_a_query_fact_still_commits_it() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "confirm",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"open a workspace?","output_key":"ready_ok","options":["go","no"]}"#,
        ),
        node(
            "route",
            Some("root"),
            1,
            db::NODE_TYPE_SELECTOR,
            r#"{"branches":[{"cond":{"var":"ready_ok","eq":"go"},"child":0},
                            {"cond":{"always":true},"child":1}]}"#,
        ),
        node("go", Some("route"), 0, db::NODE_TYPE_SEQUENCE, "{}"),
        // The PREVALIDATION: a `query` fills `workspace_free`. No human ever writes it.
        node(
            "free?",
            Some("go"),
            0,
            db::NODE_TYPE_QUERY,
            r#"{"query":"workspace_target_free","write":"workspace_free"}"#,
        ),
        node(
            "workspace",
            Some("go"),
            1,
            db::NODE_TYPE_CREATE_WORKSPACE,
            r#"{"root_key":"worktrees_root","gate_on":"workspace_free"}"#,
        ),
        node(
            "left",
            Some("route"),
            1,
            db::NODE_TYPE_NOTE,
            r#"{"text":"nothing done"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    assert_eq!(
        tree.ask_human_commitment("confirm"),
        Some(HumanCommitment::CreateWorkspace),
        "the answer routes to a create_workspace: it commits a folder on the user's disk, whatever \
         key that node's own gate_on happens to name",
    );
}

/// The OTHER arm of the same OR, locked in its own right: a sensitive node whose `gate_on` IS the
/// answer key, reached with NO selector anywhere. Nothing routes here — the `gate_on` alone is the
/// consent — so a "fix" that dropped the `gate_on` reading in favour of the climb turns this red.
#[test]
fn an_answer_a_sensitive_nodes_gate_on_names_commits_it_with_no_selector_above() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "confirm",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"ready?","output_key":"ready_ok","options":["yes","no"]}"#,
        ),
        node(
            "ready",
            Some("root"),
            1,
            db::NODE_TYPE_TRANSITION,
            r#"{"target_state":"ready","gate_on":"ready_ok"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    assert_eq!(
        tree.ask_human_commitment("confirm"),
        Some(HumanCommitment::SpecTransition),
        "the gate_on names the answer key: the reading that has always worked must keep working",
    );
}

/// The climb stays ON THE ANCESTOR CHAIN, and the widening does not relax that: a `selector` that
/// routes a SIBLING branch on the answer decides nothing about a sensitive node sitting in ANOTHER
/// branch, whose own `gate_on` names an unrelated key. Without this, "evaluate both readings" would
/// slide into "any affirmative selector anywhere in the graph credits any sensitive node".
#[test]
fn a_selector_on_a_sibling_branch_does_not_decide_a_foreign_sensitive_node() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "confirm",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"?","output_key":"ready_ok","options":["yes","no"]}"#,
        ),
        node(
            "route",
            Some("root"),
            1,
            db::NODE_TYPE_SELECTOR,
            r#"{"branches":[{"cond":{"var":"ready_ok","eq":"yes"},"child":0},
                            {"cond":{"always":true},"child":1}]}"#,
        ),
        node(
            "noted",
            Some("route"),
            0,
            db::NODE_TYPE_NOTE,
            r#"{"text":"ok"}"#,
        ),
        node(
            "else",
            Some("route"),
            1,
            db::NODE_TYPE_NOTE,
            r#"{"text":"no"}"#,
        ),
        // A SIBLING of the selector, not a descendant of its approving branch.
        node(
            "fork",
            Some("root"),
            2,
            db::NODE_TYPE_FORK_SPEC,
            r#"{"gate_on":"some_query_fact"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    assert_eq!(
        tree.ask_human_commitment("confirm"),
        None,
        "the fork sits outside the branch the answer routes to, and its gate_on is not the answer",
    );
}

// ---------------------------------------------------------------------------
// blackboard PROVENANCE — who wrote which key
// ---------------------------------------------------------------------------

/// THE FOUR WRITE PATHS, each covered. `set` / `set_scoped` / `increment` / `append` are the whole of
/// what a node can do to the board, and a provenance blind on ONE of them is not a weaker guarantee
/// but a false one: a key filled through the uncovered path would keep whatever author it had before,
/// which is a consent check reading a name that no longer applies.
#[test]
fn every_blackboard_write_path_records_who_wrote_the_key() {
    let human = NodeOrigin::new("ask-1", NodeType::AskHuman);
    let fact = NodeOrigin::new("q-1", NodeType::Query);
    let mut board = Blackboard::new();

    board.written_by(&human).set("answer", json!("go"));
    board
        .written_by(&fact)
        .set_scoped("loop_iter", "loop-1", json!(3));
    board.written_by(&fact).increment("spawns");
    board.written_by(&human).append("log", json!("first"));

    assert_eq!(board.origin_of("answer"), Some(human.clone()), "set");
    assert_eq!(
        board.origin_of("loop_iter"),
        Some(fact.clone()),
        "set_scoped"
    );
    assert_eq!(board.origin_of("spawns"), Some(fact), "increment");
    assert_eq!(board.origin_of("log"), Some(human.clone()), "append");

    // The values themselves are untouched by the bookkeeping.
    assert_eq!(board.get("answer"), Some(&json!("go")));
    assert_eq!(board.get("loop_iter.loop-1"), Some(&json!(3)));
    assert_eq!(board.get("spawns"), Some(&json!(1)));
    assert_eq!(board.get("log"), Some(&json!(["first"])));

    // A dot-path resolves its ROOT's origin: `answer.decision` is written by whoever wrote `answer`.
    assert_eq!(board.origin_of("answer.decision"), Some(human));
    // Only an `ask_human` is a human's answer — the one distinction anything downstream acts on.
    assert!(board.origin_of("answer").unwrap().is_human_answer());
    assert!(!board.origin_of("spawns").unwrap().is_human_answer());
}

/// AN UNATTRIBUTED WRITE DROPS THE OLD AUTHOR, it does not inherit it. This is the runtime twin of the
/// authoring rule's single-writer condition (iii), whose reproduced bypass is three nodes long:
/// `ask_human` fills `approved`, something else overwrites it, and the gate still reads "a human said
/// yes". If an unattributed overwrite left the previous provenance standing, the record would say
/// `ask_human` about a value the human never gave — a FALSE provenance, which is worse than none,
/// because the check believes it.
#[test]
fn an_unattributed_write_drops_the_previous_author_instead_of_inheriting_it() {
    let human = NodeOrigin::new("ask-1", NodeType::AskHuman);
    let mut board = Blackboard::new();
    board.written_by(&human).set("approved", json!("yes"));
    assert_eq!(board.origin_of("approved"), Some(human.clone()));

    board.set("approved", json!(true));
    assert_eq!(
        board.origin_of("approved"),
        None,
        "the human's name must not survive onto a value the human did not write"
    );

    // The same on each of the other three paths.
    board.written_by(&human).set("n", json!(0));
    board.increment("n");
    assert_eq!(board.origin_of("n"), None, "increment");

    board.written_by(&human).set("acc", json!(["a"]));
    board.append("acc", json!("b"));
    assert_eq!(board.origin_of("acc"), None, "append");

    board.written_by(&human).set("scoped", json!({}));
    board.set_scoped("scoped", "k", json!(1));
    assert_eq!(board.origin_of("scoped"), None, "set_scoped");
}

/// BOUNDED, and bounded by the number of KEYS rather than by the number of ticks — the board is
/// persisted on every tick, so a provenance that grew per write would be a per-tick cost that
/// compounds for the life of a run. One entry per top-level key, overwritten in place; and a key that
/// is REMOVED takes its entry with it, so a finished child run or a revoked grant leaves no record of
/// an author for a key that is no longer there.
#[test]
fn provenance_is_one_entry_per_key_and_leaves_with_the_key() {
    let by = NodeOrigin::new("j-1", NodeType::Judge);
    let mut board = Blackboard::new();
    for i in 0..500 {
        board.written_by(&by).set("verdict", json!(i));
    }
    let record = board
        .as_map()
        .get(PROVENANCE_KEY)
        .and_then(Value::as_object)
        .expect("the provenance record");
    assert_eq!(record.len(), 1, "500 writes of one key leave ONE entry");

    board.remove("verdict");
    assert_eq!(board.origin_of("verdict"), None);
    assert!(
        board.as_map().get(PROVENANCE_KEY).is_none(),
        "the empty record is dropped entirely, so a board with no attributed write is \
         indistinguishable from one written before provenance existed"
    );
}

/// THE RECORD IS NOT WRITABLE BY WHAT IT RECORDS. A write naming [`PROVENANCE_KEY`] does not add a
/// key beside the others — it would REPLACE the whole record with an object of the writer's choosing,
/// which is a forged answer to the one question the runtime consent check asks. The four write paths
/// (and [`Blackboard::remove`], the denial half) refuse it, so the record survives every attempt
/// naming the real author.
///
/// MEASURED before the refusal existed, through the public API alone: `origin_of("ready_ok")` went
/// from `NodeOrigin { node_id: "q-1", node_type: Query }` to
/// `NodeOrigin { node_id: "totally-a-human", node_type: AskHuman }`, and `is_human_answer()` from
/// `false` to `true`.
#[test]
fn a_write_naming_the_provenance_key_cannot_forge_an_author() {
    let real = NodeOrigin::new("q-1", NodeType::Query);
    let forged = json!({"ready_ok": {"node": "totally-a-human", "type": "ask_human"}});

    // Every one of the four attributed paths, plus their unattributed twins, plus the removal.
    let attempts: Vec<(&str, fn(&mut Blackboard, &NodeOrigin, &Value))> = vec![
        ("written_by().set", |b, by, v| {
            b.written_by(by).set(PROVENANCE_KEY, v.clone())
        }),
        ("written_by().set_scoped", |b, by, _| {
            b.written_by(by).set_scoped(
                PROVENANCE_KEY,
                "ready_ok",
                json!({"node":"x","type":"ask_human"}),
            )
        }),
        ("written_by().increment", |b, by, _| {
            assert_eq!(
                b.written_by(by).increment(PROVENANCE_KEY),
                0,
                "a refused increment must report the count the board holds, not the one it wanted",
            )
        }),
        ("written_by().append", |b, by, v| {
            b.written_by(by).append(PROVENANCE_KEY, v.clone())
        }),
        ("set", |b, _, v| b.set(PROVENANCE_KEY, v.clone())),
        ("set_scoped", |b, _, _| {
            b.set_scoped(PROVENANCE_KEY, "ready_ok", json!(1))
        }),
        ("increment", |b, _, _| {
            b.increment(PROVENANCE_KEY);
        }),
        ("append", |b, _, v| b.append(PROVENANCE_KEY, v.clone())),
        ("remove", |b, _, _| b.remove(PROVENANCE_KEY)),
    ];

    for (path, attempt) in attempts {
        let mut board = Blackboard::new();
        board.written_by(&real).set("ready_ok", json!("yes"));
        attempt(&mut board, &real, &forged);

        assert_eq!(
            board.origin_of("ready_ok"),
            Some(real.clone()),
            "`{path}` rewrote the board's record of who authored `ready_ok`",
        );
        assert!(
            !board
                .origin_of("ready_ok")
                .expect("the real author")
                .is_human_answer(),
            "`{path}` turned a query's fact into a human's answer — the one claim a consent gate acts on",
        );
        assert_eq!(
            board.get(PROVENANCE_KEY),
            Some(&json!({"ready_ok": {"node": "q-1", "type": "query"}})),
            "`{path}` left something other than the record the engine minted",
        );
    }
}

/// The same refusal on the path a GRAPH actually reaches it by: a node's `output_key`
/// (`Executor::write_result` → `written_by(&by).set(&key, value)`). The authoring guard refuses this
/// config, but only for graphs authored THROUGH it — a seeded, forked or hand-inserted one arrives
/// here unchecked, and that is the population the runtime consent check exists for.
#[test]
fn a_node_whose_output_key_names_the_provenance_key_writes_nothing() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "ask",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"?","output_key":"ready_ok"}"#,
        ),
        node(
            "forger",
            Some("root"),
            1,
            db::NODE_TYPE_JUDGE,
            r#"{"output_key":"__written_by__"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);
    drive(&mut engine, |a| match a {
        Action::AskHuman { .. } => NodeResult::Human(json!("yes")),
        Action::Judge { .. } => {
            NodeResult::Verdict(json!({"ready_ok": {"node": "forger", "type": "ask_human"}}))
        }
        _ => NodeResult::Ack,
    });

    let board = engine.blackboard();
    assert_eq!(
        board.origin_of("ready_ok"),
        Some(NodeOrigin::new("ask", NodeType::AskHuman)),
        "the judge's verdict replaced the board's record of who answered the question",
    );
    assert_eq!(
        board.get(PROVENANCE_KEY),
        Some(&json!({"ready_ok": {"node": "ask", "type": "ask_human"}})),
        "the forged object landed on the board",
    );
}

/// RETRO-COMPATIBILITY, in the only shape that matters at deployment: a board PERSISTED before the
/// provenance key existed. It parses, every value reads back, and every origin is `None` — "unknown",
/// which is a first-class answer and not an error. A run in flight must not so much as notice.
#[test]
fn a_legacy_board_without_provenance_reads_normally_with_unknown_origins() {
    let legacy = r#"{"ready_ok":"yes","spawns":3,"loop_iter":{"l1":2}}"#;
    let board = Blackboard::from_json_str(legacy);

    assert_eq!(board.get("ready_ok"), Some(&json!("yes")));
    assert_eq!(board.get("spawns"), Some(&json!(3)));
    assert_eq!(board.get("loop_iter.l1"), Some(&json!(2)));
    assert_eq!(board.origin_of("ready_ok"), None);
    assert_eq!(board.origin_of("spawns"), None);
    assert_eq!(board.origin_of("nothing_here"), None);
}

/// The record SURVIVES the round trip through `workflow_runs.blackboard` — a TEXT column holding
/// [`Blackboard::to_json_string`], so provenance rides it with NO schema change and no migration. A
/// record that did not persist would answer "unknown" on the very tick that matters: the question is
/// asked on a LATER tick than the write.
#[test]
fn provenance_survives_the_json_round_trip_that_persists_the_board() {
    let human = NodeOrigin::new("ask-1", NodeType::AskHuman);
    let mut board = Blackboard::new();
    board.written_by(&human).set("ready_ok", json!("yes"));

    let restored = Blackboard::from_json_str(&board.to_json_string());
    assert_eq!(restored.origin_of("ready_ok"), Some(human));
    assert_eq!(restored.get("ready_ok"), Some(&json!("yes")));
}

/// The engine MINTS the provenance itself, from the node it just executed — the property that makes
/// the record a fact rather than a claim. A run of one `ask_human` and one `spawn_subagent` leaves the
/// board naming each writer, with the answer marked as a human's and the diff not.
#[test]
fn a_walk_records_the_node_type_behind_each_key_it_writes() {
    let rows = vec![
        node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
        node(
            "ask",
            Some("root"),
            0,
            db::NODE_TYPE_ASK_HUMAN,
            r#"{"prompt":"?","output_key":"ready_ok"}"#,
        ),
        node(
            "spawn",
            Some("root"),
            1,
            db::NODE_TYPE_SPAWN_SUBAGENT,
            r#"{"agent_id":"coder"}"#,
        ),
    ];
    let tree = Tree::from_rows(&rows, Some("root")).unwrap();
    let mut engine = Engine::start(tree);
    drive(&mut engine, |a| match a {
        Action::AskHuman { .. } => NodeResult::Human(json!("yes")),
        Action::SpawnSubagent { .. } => NodeResult::Diff("d".to_string()),
        _ => NodeResult::Ack,
    });

    let board = engine.blackboard();
    assert_eq!(
        board.origin_of("ready_ok"),
        Some(NodeOrigin::new("ask", NodeType::AskHuman)),
        "the answer names the question that asked it",
    );
    assert_eq!(
        board.origin_of("last_diff"),
        Some(NodeOrigin::new("spawn", NodeType::SpawnSubagent)),
        "the diff names the spawn",
    );
    assert_eq!(
        board.origin_of("spawns"),
        Some(NodeOrigin::new("spawn", NodeType::SpawnSubagent)),
        "the `increment` path is minted by the engine too",
    );
}

/// `consent_keys` reads a `selector` UNANIMOUSLY: a sensitive node a branch routes to on an
/// affirmative, but that ANOTHER branch of the same selector also reaches (an `always` fall-through,
/// a `ne`, an `eq` on a refusal), is a node the run may have reached WITHOUT that answer. So the
/// selector contributes nothing, and only the node's own `gate_on` remains.
///
/// This is where it parts company with [`Tree::ask_human_commitment`]'s reading, which credits ANY
/// route: over-crediting there costs one human turn, over-crediting here fires an effect nobody
/// approved. Same graph, two questions, two different conservative sides.
#[test]
fn a_sensitive_node_reachable_on_a_second_branch_takes_no_consent_from_that_selector() {
    let branches = |extra: &str| {
        format!(r#"{{"branches":[{{"cond":{{"var":"ready_ok","eq":"yes"}},"child":0}}{extra}]}}"#)
    };
    let rows = |selector_config: String| {
        vec![
            node("root", None, 0, db::NODE_TYPE_SEQUENCE, "{}"),
            node(
                "ask",
                Some("root"),
                0,
                db::NODE_TYPE_ASK_HUMAN,
                r#"{"prompt":"?","output_key":"ready_ok","options":["yes","no"]}"#,
            ),
            node(
                "route",
                Some("root"),
                1,
                db::NODE_TYPE_SELECTOR,
                &selector_config,
            ),
            node(
                "fork",
                Some("route"),
                0,
                db::NODE_TYPE_FORK_SPEC,
                r#"{"gate_on":"can_fork"}"#,
            ),
            node(
                "else",
                Some("route"),
                1,
                db::NODE_TYPE_NOTE,
                r#"{"text":"no"}"#,
            ),
        ]
    };

    // ONE branch names the fork, and it tests the answer affirmatively: the routing IS the consent.
    let unanimous = Tree::from_rows(
        &rows(branches(r#",{"cond":{"always":true},"child":1}"#)),
        Some("root"),
    )
    .unwrap();
    assert_eq!(
        unanimous.consent_keys("fork"),
        vec!["can_fork".to_string(), "ready_ok".to_string()],
    );

    // A SECOND branch also names the fork, and this one routes there on ANY value: the run can be
    // standing on the fork with `ready_ok` never affirmative, so nothing about that answer is
    // accountable for it.
    let ambiguous = Tree::from_rows(
        &rows(branches(r#",{"cond":{"always":true},"child":0}"#)),
        Some("root"),
    )
    .unwrap();
    assert_eq!(
        ambiguous.consent_keys("fork"),
        vec!["can_fork".to_string()],
        "the selector decides nothing when a second route reaches the same child",
    );

    // The OTHER shape of the same ambiguity, and the one the `always` case does not cover: the
    // second route to the fork tests a key AFFIRMATIVELY too — a DIFFERENT one. Both branches
    // "consent", so neither answer is accountable: the run standing on the fork proves that ONE of
    // them was affirmative and says nothing about which. Crediting either would let an approval of
    // `other_ok` authorize an effect the graph shows `ready_ok` deciding.
    let two_keys = Tree::from_rows(
        &rows(branches(
            r#",{"cond":{"var":"other_ok","eq":"yes"},"child":0}"#,
        )),
        Some("root"),
    )
    .unwrap();
    assert_eq!(
        two_keys.consent_keys("fork"),
        vec!["can_fork".to_string()],
        "two routes to the same child testing two different keys agree on nothing — the selector \
         contributes no consent key at all",
    );
}
