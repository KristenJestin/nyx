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
        // This tree carries no transition_task / sub_workflow leaf; the arms keep the match total.
        Action::TransitionTask { .. } => NodeResult::Ack,
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
