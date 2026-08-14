import type { EngineNode } from "@/bridge";

import { describeLoopCondition, parseNodeConfig } from "./build-graph";

/**
 * Compose a NATURAL-LANGUAGE sentence explaining what a behavior-tree node DOES, from its typed config
 * (finding #35). The inspector shows this at the top INSTEAD of a raw key/value dump (the raw JSON is
 * kept behind a "Raw config" accordion for authors). It REUSES the readable-condition helpers from
 * `build-graph` (`describeLoopCondition` / `describeBranchCondition`) so the graph and the inspector
 * always word a condition identically — never a second, drifting formatter.
 *
 * The returned string marks CODE identifiers (blackboard keys, states, command ids) with backticks —
 * the inspector renders those spans as `<code>`. Returns `null` for a type with no natural-language
 * form (wait / produce / start / terminal / unknown) so the caller falls back gracefully to a generic
 * line + the raw-config accordion. Never throws on malformed config (every read is defensive).
 *
 * `children` are the node's ORDERED child rows — a `selector` needs their LABELS to name each branch's
 * destination (`spec_status = ready → revise a ready spec`).
 */
export function describeNode(
  type: string,
  config: string,
  children: readonly EngineNode[] = [],
): string | null {
  const cfg = parseNodeConfig(config);
  switch (type) {
    case "query":
      return describeQuery(cfg);
    case "selector":
      return describeSelector(config, children);
    case "loop":
      return describeLoop(config, cfg);
    case "sequence":
      return "Runs its children in order, one after another.";
    case "note":
      return "A no-op marker — it labels this branch of the graph (often a safe default) and changes nothing.";
    case "ask_human":
      return describeAskHuman(cfg);
    case "judge":
      return describeJudge(cfg);
    case "transition":
      return describeTransition(cfg);
    case "gate_check":
      return describeGate(cfg);
    case "spawn_subagent":
      return describeSpawn(cfg);
    case "fork_spec":
      return describeForkSpec(cfg);
    case "sub_workflow":
      return describeSubWorkflow(cfg);
    case "transition_task":
      return describeTransitionTask(cfg);
    default:
      return null;
  }
}

/** The readable phrase for a known nyx `query` fact, e.g. `spec_status` → "the Spec status". */
function queryFactPhrase(query: string, target: string | undefined): string {
  switch (query) {
    case "spec_status":
      return "the Spec status";
    case "spec_open_tasks":
      return "the Spec's open tasks";
    case "spec_can_transition":
      return `whether the Spec can transition${target ? ` to \`${target}\`` : ""}`;
    default:
      return query ? `the \`${query}\` fact` : "a nyx fact";
  }
}

/** `query` → "Reads the Spec status and writes it to `spec_status`." */
function describeQuery(cfg: Record<string, unknown>): string {
  const query = asString(cfg.query) ?? "";
  const write = asString(cfg.write);
  const phrase = queryFactPhrase(query, asString(cfg.target));
  return write ? `Reads ${phrase} and writes it to \`${write}\`.` : `Reads ${phrase}.`;
}

/** `selector` → the SHORT framing sentence ONLY. The conditions are deliberately NOT re-enumerated
 *  here: the inspector renders the branches below as an if-chain (`IF <cond>` ↳ child), so repeating
 *  them in prose was pure duplication. The count still says how wide the fork is. */
function describeSelector(_config: string, children: readonly EngineNode[]): string | null {
  if (children.length === 0) return null;
  const plural = children.length > 1 ? "branches" : "branch";
  return `Takes the first branch whose condition is true, in order — ${children.length} ${plural}:`;
}

/** `sub_workflow` → "Runs the `coder` flow as a CHILD run and waits for it…". The heaviest leaf there
 *  is: it opens a whole run of another flow, so the sentence names the flow, what is handed down
 *  (`inject`) and what comes back up (`retrieve`). */
function describeSubWorkflow(cfg: Record<string, unknown>): string {
  const scope = asString(cfg.scope);
  const explicit = asString(cfg.workflow_id);
  const target = scope
    ? `the \`${scope}\` flow`
    : explicit
      ? `workflow \`${explicit}\``
      : "another flow";
  /** The destination keys of an `inject`/`retrieve` list of `{from,to}` copies. */
  const copies = (key: string): string[] =>
    Array.isArray(cfg[key])
      ? (cfg[key] as unknown[])
          .map((c) =>
            c && typeof c === "object" ? asString((c as Record<string, unknown>).to) : null,
          )
          .filter((k): k is string => !!k)
      : [];
  const tick = (keys: string[]) => keys.map((k) => `\`${k}\``).join(", ");
  const seeded = copies("inject").length ? ` It starts it with ${tick(copies("inject"))}.` : "";
  const brought = copies("retrieve");
  const back = brought.length
    ? ` When the child finishes, ${tick(brought)} come back here.`
    : " It reads the child's own result when it finishes.";
  return `Runs ${target} as a CHILD run and waits for it.${seeded}${back}`;
}

/** `transition_task` → "Moves the task in `task_id` to done." */
function describeTransitionTask(cfg: Record<string, unknown>): string {
  const t = asString(cfg.transition) ?? "move";
  const key = asString(cfg.task_key) ?? "task_id";
  const gate = asString(cfg.gate_on);
  const gated = gate ? `, only if \`${gate}\` was approved` : "";
  return `Moves the task named by \`${key}\` to \`${t}\`${gated}.`;
}

/** `loop` → "Repeats its contents until `grill_status = complete`, 25 iterations max." */
function describeLoop(config: string, cfg: Record<string, unknown>): string {
  const cond = describeLoopCondition(config);
  const max = typeof cfg.max_iterations === "number" ? cfg.max_iterations : null;
  if (!cond) return "Runs its contents once."; // absent `until` = a single body pass.
  const maxClause = max != null ? `, ${max} iterations max` : "";
  return `Repeats its contents until \`${cond}\`${maxClause}.`;
}

/** `ask_human` → "Asks « … » and appends the answer to `answers`." (+ allowed answers when closed). */
function describeAskHuman(cfg: Record<string, unknown>): string {
  const prompt = asString(cfg.prompt) ?? "";
  const outputKey = asString(cfg.output_key);
  const append = cfg.append === true;
  const options = stringArray(cfg.options);
  let sentence = `Asks « ${prompt} »`;
  if (outputKey) {
    sentence += ` and ${append ? "appends the answer to" : "stores the answer in"} \`${outputKey}\`.`;
  } else {
    sentence += ` and ${append ? "appends" : "stores"} the answer.`;
  }
  if (options.length > 0) sentence += ` Allowed answers: ${options.join(", ")}.`;
  return sentence;
}

/** `judge` → "Asks the dispatcher « … » and stores the answer in `grill_status`." */
function describeJudge(cfg: Record<string, unknown>): string {
  const prompt = asString(cfg.prompt) ?? asString(cfg.criterion) ?? "…";
  const outputKey = asString(cfg.output_key) ?? "verdict";
  return `Asks the dispatcher « ${prompt} » and stores the answer in \`${outputKey}\`.`;
}

/** `transition` → "Moves the Spec to `ready`, only if `ready_ok` was approved." (drops the clause
 *  when ungated). */
function describeTransition(cfg: Record<string, unknown>): string {
  const target = asString(cfg.target_state) ?? "";
  const gateOn = asString(cfg.gate_on);
  const clause = gateOn ? `, only if \`${gateOn}\` was approved` : "";
  return `Moves the Spec to \`${target}\`${clause}.`;
}

/**
 * `gate_check` → what this gate actually runs. A gate arms itself one of two ways and the sentence
 * must not blur them: a `slot` names WHAT must be verified and leaves the WITH WHAT to the project
 * that binds it (the shape a SHARED graph uses — no built-in can name `bun test`), while
 * `command_ids` names the commands outright. A slot gate reads as unconfigured otherwise, which is
 * the opposite of true: it is configured, and it is the project that has yet to answer.
 */
function describeGate(cfg: Record<string, unknown>): string {
  // A gate is BLOCKING by nature (the engine defaults `blocking` true); only an explicit `false` is advisory.
  const blocking = cfg.blocking !== false;
  const slot = asString(cfg.slot);
  if (slot) {
    return `Runs whatever this project binds to the \`${slot}\` slot; ${
      blocking ? "blocks the run until it passes" : "advisory only"
    } (unbound, it verifies nothing and the run is refused).`;
  }
  const ids = stringArray(cfg.command_ids);
  if (ids.length === 0) return "No command configured: always passes green and checks nothing.";
  return `Runs \`${ids.join(", ")}\`; ${blocking ? "blocks the run until they pass" : "advisory only"}.`;
}

/** `spawn_subagent` → "Launches the `spec` effector in this session (inline mode, so it can address you)." */
function describeSpawn(cfg: Record<string, unknown>): string {
  const agentId = asString(cfg.agent_id) ?? "";
  const mode = cfg.inline === true
    ? "in this session (inline mode, so it can address you)"
    : "as a background sub-agent (native mode)";
  return `Launches the \`${agentId}\` effector ${mode}.`;
}

/** `fork_spec` → "Creates a new revision of the Spec, if `revise_ok` was approved." */
function describeForkSpec(cfg: Record<string, unknown>): string {
  const gateOn = asString(cfg.gate_on);
  const clause = gateOn ? `, if \`${gateOn}\` was approved` : "";
  return `Creates a new revision of the Spec${clause}.`;
}

/** A config value that is a string, else `undefined`. */
function asString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

/** A config value coerced to a `string[]` (non-strings dropped). */
function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}
