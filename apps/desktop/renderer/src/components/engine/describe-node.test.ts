import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import type { EngineNode } from "@/bridge";

import { describeNode } from "./describe-node";
import { metaFor, NODE_META } from "./node-meta";

/** A raw config JSON string (what `EngineNode.config` carries on the wire). */
const cfg = (config: Record<string, unknown>) => JSON.stringify(config);

/** A minimal child row (only `label` + `type_` matter to the describer). */
function child(label: string, type = "note"): EngineNode {
  return {
    id: label,
    workflow_id: "wf",
    parent_id: "sel",
    order_index: 0,
    type_: type,
    label,
    config: "{}",
    created_at: 0,
    updated_at: 0,
  };
}

describe("describeNode", () => {
  it("query → reads a nyx fact and writes it to a blackboard key", () => {
    expect(describeNode("query", cfg({ query: "spec_status", write: "spec_status" }))).toBe(
      "Reads the Spec status and writes it to `spec_status`.",
    );
    // A parameterized fact names its transition target.
    expect(
      describeNode("query", cfg({ query: "spec_can_transition", target: "ready", write: "can_ready" })),
    ).toBe("Reads whether the Spec can transition to `ready` and writes it to `can_ready`.");
    // An unknown fact falls back to the raw fact name (no crash).
    expect(describeNode("query", cfg({ query: "custom_fact", write: "x" }))).toBe(
      "Reads the `custom_fact` fact and writes it to `x`.",
    );
  });

  it("selector → a SHORT framing sentence (the conditions live in the if-chain below, not here)", () => {
    const config = cfg({
      branches: [
        { cond: { var: "spec_status", eq: "ready" }, child: 0 },
        { cond: { var: "spec_status", eq: "draft" }, child: 1 },
        { cond: { always: true }, child: 2 },
      ],
    });
    const children = [
      child("revise a ready spec"),
      child("resume a draft"),
      child("create new"),
    ];
    // N37-review: the sentence FRAMES the fork only — each condition is rendered as the if-chain
    // BELOW it (`IF <cond>` ↳ child), so re-enumerating them in prose was pure duplication.
    expect(describeNode("selector", config, children)).toBe(
      "Takes the first branch whose condition is true, in order — 3 branches:",
    );
    expect(describeNode("selector", config, [child("only one")])).toBe(
      "Takes the first branch whose condition is true, in order — 1 branch:",
    );
  });

  it("selector → falls back to null when it has no children to name", () => {
    expect(describeNode("selector", cfg({ branches: [] }), [])).toBeNull();
  });

  it("loop → repeats until its condition, with the iteration cap", () => {
    expect(
      describeNode("loop", cfg({ until: { var: "grill_status", eq: "complete" }, max_iterations: 25 })),
    ).toBe("Repeats its contents until `grill_status = complete`, 25 iterations max.");
    // No cap → no cap clause.
    expect(describeNode("loop", cfg({ until: { var: "done", truthy: true } }))).toBe(
      "Repeats its contents until `done is set`.",
    );
    // No `until` → a single body pass (a safe non-looping default).
    expect(describeNode("loop", cfg({}))).toBe("Runs its contents once.");
  });

  it("ask_human → asks the prompt and routes the answer (appends vs stores + allowed answers)", () => {
    expect(
      describeNode(
        "ask_human",
        cfg({ prompt: "Answer the author's question…", output_key: "answers", append: true }),
      ),
    ).toBe("Asks « Answer the author's question… » and appends the answer to `answers`.");
    // Non-append → "stores in".
    expect(
      describeNode("ask_human", cfg({ prompt: "Approve the merge?", output_key: "merge_ok" })),
    ).toBe("Asks « Approve the merge? » and stores the answer in `merge_ok`.");
    // A closed option set is surfaced as the allowed answers.
    expect(
      describeNode(
        "ask_human",
        cfg({ prompt: "Ready?", output_key: "ready_ok", options: ["yes", "no"] }),
      ),
    ).toBe("Asks « Ready? » and stores the answer in `ready_ok`. Allowed answers: yes, no.");
  });

  it("judge → asks the dispatcher and stores the verdict under its output key", () => {
    expect(describeNode("judge", cfg({ prompt: "Is the phase complete?", output_key: "grill_status" }))).toBe(
      "Asks the dispatcher « Is the phase complete? » and stores the answer in `grill_status`.",
    );
    // Default output key is `verdict`.
    expect(describeNode("judge", cfg({ prompt: "Good?" }))).toBe(
      "Asks the dispatcher « Good? » and stores the answer in `verdict`.",
    );
  });

  it("transition → moves the Spec, with the gate clause only when gated", () => {
    expect(describeNode("transition", cfg({ target_state: "ready", gate_on: "ready_ok" }))).toBe(
      "Moves the Spec to `ready`, only if `ready_ok` was approved.",
    );
    // Ungated → no "only if…" clause.
    expect(describeNode("transition", cfg({ target_state: "in_review" }))).toBe(
      "Moves the Spec to `in_review`.",
    );
  });

  it("gate_check → the empty-gate signal, else runs the commands (blocking vs advisory)", () => {
    // N26(a): an empty gate is surfaced as always-green, checks-nothing.
    expect(describeNode("gate_check", cfg({ command_ids: [] }))).toBe(
      "No command configured: always passes green and checks nothing.",
    );
    expect(describeNode("gate_check", cfg({ command_ids: ["typecheck", "test"], blocking: true }))).toBe(
      "Runs `typecheck, test`; blocks the run until they pass.",
    );
    expect(describeNode("gate_check", cfg({ command_ids: ["lint"], blocking: false }))).toBe(
      "Runs `lint`; advisory only.",
    );
    // A gate is blocking BY DEFAULT (the engine defaults `blocking` true).
    expect(describeNode("gate_check", cfg({ command_ids: ["build"] }))).toBe(
      "Runs `build`; blocks the run until they pass.",
    );
  });

  it("gate_check with a slot → names what the project must bind, never 'no command configured'", () => {
    // The seeded coder's gate: it declares WHAT must be verified, the project answers WITH WHAT
    // (slot_bind). Reading that as "no command configured: always passes green" is the opposite of
    // true — the gate is configured, and unbound it REFUSES the run rather than waving it through.
    expect(describeNode("gate_check", cfg({ slot: "build_check", blocking: true }))).toBe(
      "Runs whatever this project binds to the `build_check` slot; blocks the run until it passes " +
        "(unbound, it verifies nothing and the run is refused).",
    );
    expect(describeNode("gate_check", cfg({ slot: "lint_check", blocking: false }))).toBe(
      "Runs whatever this project binds to the `lint_check` slot; advisory only " +
        "(unbound, it verifies nothing and the run is refused).",
    );
    // The empty `command_ids` serde leaves beside a slot must not win over the slot.
    expect(describeNode("gate_check", cfg({ slot: "build_check", command_ids: [] }))).toBe(
      "Runs whatever this project binds to the `build_check` slot; blocks the run until it passes " +
        "(unbound, it verifies nothing and the run is refused).",
    );
  });

  it("gate_check with `expect: red` → says the check must FAIL, never that it must pass", () => {
    // THE TWO REPRODUCTION GATES OF THE SEEDED CODER differ by this one field, on the SAME slot:
    // the first blocks the run until the check FAILS, the second until the same check passes. That
    // difference IS the red-before-green proof, and describing both as "blocks the run until it
    // passes" told the reader the exact opposite of what the first node enforces — and made the two
    // indistinguishable in the inspector.
    expect(
      describeNode("gate_check", cfg({ slot: "repro_check", expect: "red", blocking: true })),
    ).toBe(
      "Runs whatever this project binds to the `repro_check` slot; blocks the run until it FAILS — " +
        "a reproduction: a check that passes here has reproduced nothing " +
        "(unbound, it verifies nothing and the run is refused).",
    );
    expect(
      describeNode("gate_check", cfg({ slot: "repro_check", expect: "green", blocking: true })),
    ).toBe(
      "Runs whatever this project binds to the `repro_check` slot; blocks the run until it passes " +
        "(unbound, it verifies nothing and the run is refused).",
    );
    // ABSENT means green — the runtime default, and the shape of every gate that is not a
    // reproduction. Same sentence as before this field existed.
    expect(describeNode("gate_check", cfg({ slot: "repro_check" }))).toBe(
      "Runs whatever this project binds to the `repro_check` slot; blocks the run until it passes " +
        "(unbound, it verifies nothing and the run is refused).",
    );
    // …and the same reading on a gate that names its own commands.
    expect(describeNode("gate_check", cfg({ command_ids: ["repro"], expect: "red" }))).toBe(
      "Runs `repro`; blocks the run until they FAIL — a reproduction: a check that passes here has " +
        "reproduced nothing.",
    );
    // A non-blocking red still says which outcome it expects: advisory is about the CONSEQUENCE,
    // never about the declaration.
    expect(
      describeNode("gate_check", cfg({ command_ids: ["repro"], expect: "red", blocking: false })),
    ).toBe("Runs `repro`; expects a FAILURE (a reproduction), advisory only.");
  });

  it("spawn_subagent → launches the effector, inline vs native mode", () => {
    expect(describeNode("spawn_subagent", cfg({ agent_id: "spec", inline: true }))).toBe(
      "Launches the `spec` effector in this session (inline mode, so it can address you).",
    );
    expect(describeNode("spawn_subagent", cfg({ agent_id: "coder" }))).toBe(
      "Launches the `coder` effector as a background sub-agent (native mode).",
    );
  });

  it("fork_spec → creates a new Spec revision, gated on an approval", () => {
    expect(describeNode("fork_spec", cfg({ gate_on: "revise_ok" }))).toBe(
      "Creates a new revision of the Spec, if `revise_ok` was approved.",
    );
    expect(describeNode("fork_spec", cfg({}))).toBe("Creates a new revision of the Spec.");
  });

  it("sequence → a plain in-order run", () => {
    expect(describeNode("sequence", "{}")).toBe("Runs its children in order, one after another.");
  });

  it("note → a no-op marker that labels a branch and changes nothing", () => {
    expect(describeNode("note", cfg({ text: "safe default" }))).toBe(
      "A no-op marker — it labels this branch of the graph (often a safe default) and changes nothing.",
    );
  });

  it("undescribed types fall back to null (wait / produce / unknown)", () => {
    for (const type of ["wait", "produce", "dragon"]) {
      expect(describeNode(type, cfg({ any: "thing" }))).toBeNull();
    }
  });

  it("never crashes on malformed config", () => {
    expect(describeNode("query", "not json")).toBe("Reads a nyx fact.");
    expect(describeNode("transition", "")).toBe("Moves the Spec to ``.");
    expect(describeNode("gate_check", "{")).toBe(
      "No command configured: always passes green and checks nothing.",
    );
    expect(describeNode("selector", "not json", [])).toBeNull();
  });
});

/**
 * The lot-2 node types (`sub_workflow`, `transition_task`) reached the ENGINE while the visualiser was
 * explicitly left out of that batch — so they fell through `metaFor`'s `note` fallback and the graph
 * drew the node that OPENS A CHILD RUN as an inert marker. These pin that the renderer knows them.
 */
describe("lot-2 node types are described (never the note fallback)", () => {
  it("sub_workflow names the flow it runs, what it seeds and what comes back", () => {
    const s = describeNode(
      "sub_workflow",
      JSON.stringify({
        scope: "coder",
        inject: [{ from: "phase", to: "phase" }],
        retrieve: [{ from: "child_status", to: "child_status" }],
      }),
    );
    expect(s).toContain("`coder`");
    expect(s).toContain("CHILD run");
    expect(s).toContain("`phase`");
    expect(s).toContain("`child_status`");
  });

  it("sub_workflow still reads without inject/retrieve", () => {
    const s = describeNode("sub_workflow", JSON.stringify({ scope: "prototyper" }));
    expect(s).toContain("`prototyper`");
    expect(s).not.toBeNull();
  });

  it("transition_task names the task key and the move, gated or not", () => {
    expect(
      describeNode("transition_task", JSON.stringify({ transition: "done", task_key: "task_id" })),
    ).toBe("Moves the task named by `task_id` to `done`.");
    expect(
      describeNode(
        "transition_task",
        JSON.stringify({ transition: "start", task_key: "t", gate_on: "ok" }),
      ),
    ).toContain("only if `ok` was approved");
  });
});

/**
 * `create_workspace` is the SIXTEENTH node type, and it arrived by exactly the road the lot-2
 * block above describes: wired end to end in the engine and shipped in the seeded /nspec graph, while
 * the visualiser knew nothing of it — so `metaFor` fell back to `note` and the node that creates a
 * folder, registers a workspace and ANCHORS the Spec painted as an inert grey marker. Same precedent,
 * same answer: the renderer knows the type, and says where the folder lands.
 */
describe("create_workspace is described (never the note fallback)", () => {
  it("names the root the folder is derived under, plus the gate", () => {
    expect(
      describeNode(
        "create_workspace",
        JSON.stringify({ root_key: "worktrees_root", gate_on: "workspace_free" }),
      ),
    ).toBe(
      "Creates the Spec's dev workspace under the root in `worktrees_root`, in a subfolder named " +
        "after the Spec, and anchors the Spec to it, only if `workspace_free` was approved.",
    );
  });

  it("says UNDER the root, never AT it, and drops the clause when ungated", () => {
    // The wording is load-bearing (finding 01M0B5AG0XR4BG6DSGEFGKW6C4): the node used to take a
    // second `path_key` source AS the folder, and a sentence that says "at the path in …" is the
    // reading that put a workspace on the user's whole worktrees root.
    const described = describeNode(
      "create_workspace",
      JSON.stringify({ root_key: "worktrees_root", name_key: "workspace_name" }),
    );
    expect(described).toBe(
      "Creates the Spec's dev workspace under the root in `worktrees_root`, in a subfolder named " +
        "after `workspace_name`, and anchors the Spec to it.",
    );
    expect(described).not.toContain("at the path");
  });

  it("says so when the node names no folder at all, rather than reading as configured", () => {
    expect(describeNode("create_workspace", "{}")).toContain("nowhere");
    expect(describeNode("create_workspace", "not json")).toContain("nowhere");
    // The retired source names nothing any more — it must not read as a configured folder.
    expect(
      describeNode("create_workspace", JSON.stringify({ path_key: "workspace_path" })),
    ).toContain("nowhere");
  });
});

/**
 * `set_category` is the SEVENTEENTH node type, and it is the one nyx uses to IMPOSE a Spec's genre of
 * work (PDR-0017 / ADR-0006). Its whole decision is WHICH BOARD KEY holds the label — the category is
 * a per-run answer, so a graph never names one literally — and the sentence has to say that, or a
 * reader takes the node for a constant classification.
 */
describe("set_category is described (never the note fallback)", () => {
  it("names the board key the label comes from, plus the gate", () => {
    expect(
      describeNode("set_category", cfg({ category_key: "category_choice", gate_on: "cat_ok" })),
    ).toBe(
      "Classifies the Spec with the category named in `category_choice`, only if `cat_ok` was approved.",
    );
  });

  it("drops the clause when ungated — the default shape, since the engine imposes it", () => {
    expect(describeNode("set_category", cfg({ category_key: "category_choice" }))).toBe(
      "Classifies the Spec with the category named in `category_choice`.",
    );
  });

  it("says so when the node names no key at all, rather than reading as configured", () => {
    expect(describeNode("set_category", "{}")).toContain("names no category key");
    expect(describeNode("set_category", "not json")).toContain("names no category key");
    // A LITERAL category is not a thing this node takes — authoring refuses the field, and the
    // sentence must not quietly report it as the configured source.
    expect(describeNode("set_category", cfg({ category: "bug" }))).toContain(
      "names no category key",
    );
  });
});

/**
 * THE RENDERER'S VOCABULARY IS THE ENGINE'S, AND THE TWO ARE MADE TO FAIL TOGETHER.
 *
 * `metaFor` must resolve every DB node type to its OWN metadata — the `note` fallback exists for a
 * FORWARD-compatible type the renderer has not met yet, never for one that already ships in the
 * seeded graphs. `sub_workflow`, `transition_task` and `create_workspace` each reached the engine and
 * the seeded graphs while the visualiser knew nothing of them, so the node that opens a whole child
 * run, the one that moves a task and the one that creates a folder all painted as inert grey markers.
 *
 * The list used to be RETYPED here, which closed the case for the sixteen types of the day and for
 * nothing after: an EIGHTEENTH type added in Rust left this file green. So the list is now READ from
 * `db::NODE_TYPES` in `crates/nyx-core/src/db.rs` — the same "derive it, never restate it" the
 * command-group IPC contract uses to pin the renderer against the sidecar. Add a node type on the
 * Rust side and this turns red until the renderer knows it.
 */
describe("the renderer knows every node type the engine ships", () => {
  /** The `db::NODE_TYPES` entries, read out of the Rust constant that IS the vocabulary. */
  const dbNodeTypes = ((): string[] => {
    const source = readFileSync(
      join(process.cwd(), "..", "..", "..", "crates", "nyx-core", "src", "db.rs"),
      "utf8",
    );
    const declaration = /pub const NODE_TYPES: \[&str; (\d+)\] = \[([^\]]*)\]/.exec(source);
    expect(declaration, "db.rs no longer declares NODE_TYPES the way this guard reads it").not.toBe(
      null,
    );
    const [, count, body] = declaration!;
    // Each entry is a `NODE_TYPE_*` constant; its VALUE is the string the DB stores, so resolve each
    // one through its own `pub const NODE_TYPE_X: &str = "…";` rather than lower-casing the name.
    const types = [...body.matchAll(/NODE_TYPE_[A-Z_]+/g)].map((m) => {
      const value = new RegExp(`pub const ${m[0]}: &str = "([a-z_]+)";`).exec(source);
      expect(value, `db.rs declares ${m[0]} without a readable string value`).not.toBe(null);
      return value![1];
    });
    expect(types.length, "the NODE_TYPES array length must match its declared size").toBe(
      Number(count),
    );
    return types;
  })();

  it("resolves each DB type to its own metadata, never the note fallback", () => {
    for (const type of dbNodeTypes) {
      if (type === "note") continue;
      expect(metaFor(type), `${type} must not read as an inert note`).not.toBe(NODE_META.note);
    }
    // …and an unknown type STILL paints, on the muted note fallback (forward compatibility).
    expect(metaFor("dragon")).toBe(NODE_META.note);
  });

  it("explains each DB type in plain language, save the ones that have nothing to say", () => {
    // `wait` pauses and `produce` produces whatever the node asks for: neither carries a config to
    // put in a sentence, so `describeNode` answers `null` and the inspector falls back to its generic
    // line + the raw-config accordion. Every OTHER type must have a describer, or the panel says
    // "No description for this node" about a node that decides something.
    const NO_NATURAL_LANGUAGE_FORM = ["wait", "produce"];
    for (const type of dbNodeTypes) {
      if (NO_NATURAL_LANGUAGE_FORM.includes(type)) {
        expect(describeNode(type, "{}"), `${type} is declared as having no sentence`).toBe(null);
        continue;
      }
      expect(
        describeNode(type, "{}", [child("a")]),
        `${type} has no describer — the inspector would show "No description" for a node that decides something`,
      ).not.toBe(null);
    }
  });
});
