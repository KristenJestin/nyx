#!/usr/bin/env node
/**
 * nyx agent-session + activity hook — CROSS-PLATFORM (node, zero external deps).
 *
 * Drives BOTH channels of the nyx `agent_session_event` MCP tool:
 *   - SESSION lifecycle: `SessionStart` / `SessionEnd` (the persisted agent-session row
 *     → the sidebar icon + resume candidate).
 *   - per-turn ACTIVITY (the RUNTIME live dot — running / response-ready / waiting; never
 *     persisted): `UserPromptSubmit`, `PreToolUse` / `PostToolUse` / `PostToolUseFailure`
 *     (the in-flight tool counter that keeps a long tool "working" with no timer),
 *     `SubagentStart` / `SubagentStop`, `Notification`, and `Stop` / `StopFailure`.
 * The script is GENERIC over the event name (argv[2] + the stdin `hook_event_name`); it
 * forwards the FULL hook stdin to the core (so per-event discriminators like `tool_name`
 * on Pre/PostToolUse and `notification_type` on Notification reach the core untouched),
 * which decides what each hook means — so adding a hook is just a `hooks.json` entry.
 *
 * WHY THIS EXISTS (review 01KVJW7DHX34WJC6YAPH1DV0TF / icone Claude morte sur Windows):
 * The bundled `hooks.json` used to run a POSIX shell one-liner (`jq`/`curl`/`$(cat)`) to
 * POST the `agent_session_event` MCP call. On Windows there is no bash/jq/curl, so the
 * command fell into its `else` branch (a bare `probe`) and `agent_session_event` was NEVER
 * called → the nyx agent session was never recorded → the Claude provider icon never lit
 * up in the sidebar. It worked on Linux only. Claude Code IS node, so node is ALWAYS
 * present — this script replaces the shell with native node `http`, giving identical
 * behaviour on Windows, macOS and Linux.
 *
 * WHAT IT DOES (parity with the old `jq` merge):
 *   - event kind comes from argv[2] (`SessionStart` | `SessionEnd` | `UserPromptSubmit` |
 *     `Stop` | `SubagentStop` | `Notification`) — informational; the authoritative
 *     `hook_event_name` is already inside the hook's stdin JSON, which the core reads. We
 *     still accept argv so each hook is self-describing (and as a fallback if Claude ever
 *     omits `hook_event_name` from stdin — e.g. the activity hooks carry no `session_id`).
 *   - reads the hook's input JSON from STDIN (Claude pipes it in: `hook_event_name`,
 *     `session_id`, `cwd`, `transcript_path`, `source`, ...).
 *   - merges `{ NYX_TERMINAL_ID: process.env.NYX_TERMINAL_ID }` into that object — EXACTLY
 *     what the old `jq '. + {NYX_TERMINAL_ID: $tid}'` did. The core's `agent_session_event`
 *     requires `NYX_TERMINAL_ID` (env) + `hook_event_name` + `session_id` (stdin).
 *   - POSTs the JSON-RPC `tools/call name=agent_session_event` to the sidecar of the nyx that
 *     installed this plugin, over native node `http` (see `resolvePort`).
 *
 * SLASH-COMMAND RUN OPENING + MANUAL INJECTION (v2.5.8 / PDR-0020, then v2.8 G6): the `/nspec`
 * `/ndev` `/ndoc` command stubs are the triggers. When a `UserPromptSubmit` prompt INVOKES one of
 * them, this hook OPENS THE RUN ITSELF — `tools/call name=run_start` (`{ scope, cwd }`), then
 * `tools/call name=agent_context` (`{ role, scope, run_id }`) — and relays ONE `additionalContext`
 * carrying the `run_id`, the `run_start` warnings and the dispatcher manual, so the session's FIRST
 * act is `nyx_next(run_id)`. WHY THE HOOK AND NOT THE SESSION: an agent that fetched its own manual
 * and opened its own run spent two calls before the first tick, resolved anchors the hook already
 * receives (`cwd`), and left "who opens the run" to whichever agent got there first.
 * IT READS THE COMMAND NAME AND, AT MOST, ONE TOKEN: the Spec REFERENCE, when the first argument IS
 * a whole one (`specIdFromPrompt` — it recognises a shape and forwards the token RAW; nyx resolves
 * it). The three stubs offer to name a Spec, and a `cwd` cannot honour that in a project holding
 * several active Specs — a location resolves none of them, and for `/ndev` and `/ndoc` the run
 * never opens. Everything else in the prompt is the human's instruction and stays untouched for
 * the agent to read. Fully best-effort — a prompt that is not a nyx command, a sidecar that is
 * down, a REFUSED `run_start` (no run ⇒ no MANUAL: one without a `run_id` would send the session
 * hunting for one) or a refused `agent_context` injects no manual, but each of those says WHY on
 * stderr (see `trace`) instead of failing mute. And when the refusal is about the REFERENCE the
 * human typed, nyx's own sentence is injected in the manual's place
 * (`refusedReferenceBlock`) — stderr is not read by the model, so swallowing it would make a
 * refused `/ndev 3 fois plus rapide` look like nyx saying nothing.
 *
 * GUARANTEES: best-effort, BOUNDED (~1s per call, and `RUN_OPEN_TIMEOUT_MS` for the two calls
 * that open a run — they do real server-side work before answering), NEVER throws, ALWAYS exits
 * 0. A hook that crashes (or whose server is down) must not disturb the user's Claude session.
 */
"use strict";

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

const EVENT = process.argv[2] || ""; // hook name (informational; stdin is authoritative)
const TERMINAL_ID = process.env.NYX_TERMINAL_ID || "";
const TIMEOUT_MS = 1000;
/**
 * The bound for the two calls that OPEN a run (`run_start`, then `agent_context`), as opposed to
 * the fire-and-forget `agent_session_event` POST that keeps the 1s above.
 *
 * WHY IT IS NOT 1s. `run_start` OPENS THE RUN SERVER-SIDE BEFORE IT ANSWERS, and it has grown real
 * work: a machine-wide scan for stranded runs (plus queries per stranded run), a RECURSIVE walk of
 * the graphs the run will drive following every `sub_workflow` hop, and the consent / query
 * warnings. The MCP surface serves it on a pool of 4 workers, one of which a `gate_check` can hold
 * for up to 90s. So the POST genuinely exceeds 1000ms on a busy machine — and the timeout does NOT
 * cancel the run: the row is created (or resumed) and only the ANSWER is lost. The session then
 * gets no `run_id`, nothing is injected, and the user's `/ndev` starts with no manual over a run
 * that exists. The idempotency (at most one live run per (workflow, spec)) means the next
 * invocation RESUMES that run rather than piling up another — losing the answer costs a turn, not
 * a run — but a turn is exactly what the hook took the call away from the agent to save.
 *
 * Still BOUNDED, and bounded by what a human will sit through: this hook runs on the prompt the
 * user just submitted, so the wait is visible. Claude Code's own hook budget is far larger.
 */
const RUN_OPEN_TIMEOUT_MS = 8000;
/** nyx's default MCP port — the last resort, and only ever right when ONE nyx runs (see below). */
const DEFAULT_PORT = "8765";

/**
 * The port of THE nyx that installed this plugin.
 *
 * `NYX_MCP_PORT` first: nyx exports it into every shell it spawns, straight from the bind, so it
 * is authoritative. It is absent when the terminal was spawned before the port was bound, or when
 * the session did not start from a nyx terminal at all.
 *
 * Then the port nyx TEMPLATED into this plugin's OWN `.mcp.json` when it installed it — the very
 * file the MCP client reads to reach the sidecar. Taking it from there is what keeps the hook and
 * the tools pointed at the SAME nyx, for one small read.
 *
 * The literal default comes last, and it is a trap on a machine running more than one nyx: they
 * all default to 8765, so a hook that reaches for it POSTs into whichever one bound the port
 * first — a DIFFERENT application, which answers `unknown tool` to calls meant for this one, and
 * does so silently. That is the case reading the sibling `.mcp.json` exists to avoid.
 */
function resolvePort() {
  const injected = (process.env.NYX_MCP_PORT || "").trim();
  if (injected) return injected;
  try {
    const root = process.env.CLAUDE_PLUGIN_ROOT;
    if (!root) return DEFAULT_PORT;
    const parsed = JSON.parse(fs.readFileSync(path.join(root, ".mcp.json"), "utf8"));
    for (const server of Object.values(parsed || {})) {
      const url = server && typeof server.url === "string" ? server.url : "";
      const m = url.match(/:(\d+)\//);
      if (m) return m[1];
    }
  } catch (_e) {
    /* no plugin root, unreadable or malformed file → the literal default */
  }
  return DEFAULT_PORT;
}

const PORT = resolvePort();

/**
 * The nyx slash commands, the run each one OPENS (`run_start {scope}`) and the manual injected
 * with it (`agent_context {role}`).
 *
 * The TRIGGER is the command NAME because that is ALL a `UserPromptSubmit` hook ever sees:
 * Claude Code hands the hook the prompt the human TYPED (`prompt: "/nspec KEY-3 …"`) and
 * expands the stub body only afterwards, so the `<!-- NYX-AGENT-CONTEXT role=… scope=… -->`
 * sentinel a stub declares NEVER reaches this hook — keying on it injected nothing, ever.
 * The stub's sentinel stays the DECLARATION of what its command triggers; the push-injection
 * e2e asserts this table agrees with it, so a drift on either side fails there.
 */
const NYX_COMMANDS = {
  nspec: { role: "dev", scope: "nspec" },
  ndev: { role: "dev", scope: "ndev" },
  ndoc: { role: "dev", scope: "ndoc" },
};

/** The leading slash-command invocation of a prompt (Claude Code allows leading blanks). */
const SLASH_COMMAND = /^\s*\/([a-z][a-z0-9:_-]*)(?=\s|$)/i;

/**
 * The slash command a prompt INVOKES (`"/nspec KEY-3 go"` → `"nspec"`), or "" when the
 * prompt is not a command invocation. Pure + defensive: a non-string / missing prompt yields
 * "". ZERO argument parsing HERE — only the command name is read. The FIRST token is read by
 * `specIdFromPrompt` alone, and only as a SHAPE; everything after it is the human's free text and
 * is never inspected by anything in this file.
 */
function commandFromPrompt(prompt) {
  if (typeof prompt !== "string" || prompt.length === 0) return "";
  const m = prompt.match(SLASH_COMMAND);
  return m ? m[1].toLowerCase() : "";
}

/**
 * THE SHAPES OF A SPEC REFERENCE — the four forms `spec_ref::parse` (Rust) accepts, expressed as
 * the SHAPE test a recogniser can make. Each is anchored, so a token is either a whole reference or
 * not one at all.
 *
 * They are pinned against the Rust grammar by `contract/spec-ref-corpus.txt`, which both sides
 * replay: `spec_ref.rs` feeds every token to `parse`, and its
 * `the_shared_corpus_pins_the_hook_recogniser_to_this_grammar` feeds the same tokens to THIS
 * file's `specIdFromPrompt`, under node. A form added on one side and not the other goes red.
 */
const SPEC_REF = {
  /** The CANONICAL id nyx mints (`8-4-4-4-12` hex), tried first so it is never re-read as a key. */
  uuid: /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i,
  /** The human key `KEY-N`: a project-key head (letter then alphanumerics), one `-`, all digits. */
  key: /^[a-z][a-z0-9]*-[0-9]+$/i,
  /** `#N` and the bare `N` — one form, with and without the sigil every render prints. */
  number: /^#?[0-9]+$/,
};

/**
 * The Spec reference the human NAMED, when the command's first argument is one (`"/ndev 3 fix the
 * probe"` → `"3"`; `"/ndev 019ff517-… fix the probe"` → `"019ff517-…"`), else "".
 *
 * WHY the hook reads this ONE token when its rule is to parse nothing: the three nyx commands
 * advertise a leading Spec reference, and the `cwd` alone cannot honour it — a real project holds
 * several active Specs, so resolving one by location is ambiguous, and for `/ndev` and `/ndoc` the
 * run then never opens. The typed reference is the only anchor that disambiguates. The hook opens
 * the run BEFORE the agent says anything, so a form it cannot SEE is a form that never reaches the
 * core on this path: recognising only the canonical id — the one shape a human never sees, since
 * every render speaks in `#3` — is what sent a `/nspec 3` into an unanchored run.
 *
 * IT RECOGNISES A SHAPE AND FORWARDS THE TOKEN RAW, byte for byte as typed. It resolves NOTHING:
 * translating `#3` into an id, and refusing a reference that designates no Spec, belong to
 * `spec_ref.rs` and to it alone — a hook that resolved would be a second truth to keep aligned with
 * the first, which is how this regex and the core drifted apart to begin with. It follows that the
 * two do not answer identically on every token, BY DESIGN: the core also bounds a reference by
 * VALUE (a number outside an `i64` is read as prose), and a shape-only recogniser has no business
 * restating a bound it cannot act on. `contract/spec-ref-corpus.txt` sorts the tokens accordingly.
 *
 * The original discipline is untouched: only the FIRST token, only when it is a WHOLE reference.
 * Anything else — a slug, a sentence, a partial id — leaves this empty and the run resolves from
 * the `cwd` exactly as before, so a human writing prose can never have a word of it mistaken for an
 * anchor.
 */
function specIdFromPrompt(prompt) {
  if (typeof prompt !== "string") return "";
  const rest = prompt.replace(SLASH_COMMAND, "").trim();
  if (!rest) return "";
  const first = rest.split(/\s+/, 1)[0];
  const recognised =
    SPEC_REF.uuid.test(first) || SPEC_REF.key.test(first) || SPEC_REF.number.test(first);
  return recognised ? first : "";
}

/**
 * The reason of the LAST trace, so the caller can report to nyx the same sentence it wrote to
 * stderr. One variable rather than a return value threaded through three helpers: every failure
 * already funnels through `trace`, and duplicating the wording is how the two ends drift apart.
 */
let lastTraced = "";

/**
 * The SERVER's own sentence when `run_start` REFUSED, kept apart from [`lastTraced`] because it is
 * the only failure whose text is worth showing the MODEL: nyx wrote it for a reader (it names the
 * accepted forms and what the project actually holds), and it is the one the session can act on.
 * Empty for every other failure mode — a sidecar that is down, a timeout, a malformed body — where
 * there is no server sentence to relay.
 */
let lastRunStartRefusal = "";

/**
 * Say WHY no manual was injected. A hook must never break the prompt, so this branch stays
 * best-effort and exits 0 — but "best effort" is not a licence to be undiagnosable: four different
 * situations (the prompt is not a nyx command, the sidecar is unreachable, `run_start` refused,
 * `agent_context` refused) used to be one silence, which is how a dead injection survived a whole
 * release. stderr is written FIRST because it is the only sink that still works when the sidecar
 * itself is down, and Claude Code shows it in debug mode without ever feeding it to the model. The
 * reason is also kept so a nyx command's failure can be FILED in nyx (see `report`) — stderr alone
 * is read by nobody, which is precisely how the dead injection went unnoticed. Silent on the happy
 * path and on any non-command prompt.
 */
function trace(reason) {
  lastTraced = String(reason);
  try {
    process.stderr.write(`nyx-hook UserPromptSubmit: no manual injected — ${reason}\n`);
  } catch (_e) {
    /* a closed stderr must never break the hook */
  }
}

/**
 * FILE in nyx that a nyx command's injection failed, carrying the reason already traced. Rides
 * `agent_session_event`, the channel this hook already POSTs on every prompt — the core recognises
 * `nyx_injection_failed`, records it in the domain-event outbox and does NOT touch the live dot,
 * because this is a report about the turn and not a beat of it.
 *
 * Only for a NYX command: a foreign slash command is a non-event, and filing it would turn an
 * audit trail into noise. Best-effort like everything here — a report that cannot be sent (the
 * sidecar being down is one of the reasons we report) changes nothing else.
 */
async function report(command, reason) {
  try {
    await post(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 4,
        method: "tools/call",
        params: {
          name: "agent_session_event",
          arguments: {
            NYX_TERMINAL_ID: TERMINAL_ID,
            hook_event_name: "UserPromptSubmit",
            nyx_injection_failed: reason,
            nyx_injection_command: command,
          },
        },
      }),
    );
  } catch (_e) {
    /* a report is the last thing that may break a prompt */
  }
}

/**
 * The rendered manual out of an `agent_context` response, or "" after tracing what went wrong.
 * Every failure mode gets its OWN wording so the trace names the cause instead of the symptom.
 */
function manualFromResponse(resp) {
  if (!resp || resp.error) {
    trace(
      `agent_context unreachable on 127.0.0.1:${PORT} — ${(resp && resp.error) || "no response"} (is the nyx sidecar running?)`,
    );
    return "";
  }
  if (resp.status !== 200) {
    trace(`agent_context answered HTTP ${resp.status}`);
    return "";
  }
  let parsed;
  try {
    parsed = JSON.parse(resp.body);
  } catch (_e) {
    trace("agent_context answered a body that is not JSON");
    return "";
  }
  if (parsed && parsed.error) {
    trace(`agent_context refused: ${parsed.error.message || JSON.stringify(parsed.error)}`);
    return "";
  }
  const manual = parsed?.result?.structuredContent?.context;
  if (typeof manual !== "string" || manual.length === 0) {
    trace("agent_context answered without result.structuredContent.context");
    return "";
  }
  return manual;
}

/**
 * The opened run (`{ runId, warnings }`) out of a `run_start` response, or `null` after tracing
 * what went wrong — failure mode for failure mode, like [`manualFromResponse`].
 *
 * A REFUSAL is the mode that matters here (`/ndev` on a location anchoring no Spec, or several):
 * it must NOT degrade into injecting a MANUAL with no run. The session's first act is
 * `nyx_next(run_id)`, so a context carrying no id sends it hunting for one — the very pre-tick
 * scramble the hook took over. No run ⇒ no manual, and the SERVER's own sentence is what the trace
 * carries, because it is the one that names which anchor was missing. (The block `main` injects in
 * its place carries no run id at all — see [`refusedReferenceBlock`].)
 *
 * THE REFUSAL SENTENCE IS ALSO KEPT ([`lastRunStartRefusal`]), because since the recogniser widened
 * to `KEY-N` / `#N` / `N` a refusal is no longer only a bad workspace: a first token that merely
 * LOOKS like a reference (`feature-42`, `3`) is now forwarded, refused, and used to cost the
 * session its whole nyx context in silence — stderr is not read by the model. `main` turns it into
 * an injected instruction instead (see there).
 */
function runFromResponse(resp) {
  if (!resp || resp.error) {
    // NO ANSWER is not the same as NO RUN: `run_start` opens the run BEFORE it answers, so a
    // timeout leaves a run this session never learns the id of. Say that rather than only blaming
    // the sidecar — the recovery (invoke the command again; the run is RESUMED, never duplicated)
    // is not the same one a genuinely-down sidecar calls for.
    trace(
      `run_start unreachable on 127.0.0.1:${PORT} — ${(resp && resp.error) || "no response"} ` +
        `(is the nyx sidecar running? on a TIMEOUT the run may have opened anyway: invoking the ` +
        `command again RESUMES it rather than opening a second one)`,
    );
    return null;
  }
  if (resp.status !== 200) {
    trace(`run_start answered HTTP ${resp.status}`);
    return null;
  }
  let parsed;
  try {
    parsed = JSON.parse(resp.body);
  } catch (_e) {
    trace("run_start answered a body that is not JSON");
    return null;
  }
  if (parsed && parsed.error) {
    lastRunStartRefusal = parsed.error.message || JSON.stringify(parsed.error);
    trace(`run_start refused: ${lastRunStartRefusal}`);
    return null;
  }
  const opened = parsed?.result?.structuredContent;
  const runId = opened && opened.run_id;
  if (typeof runId !== "string" || runId.length === 0) {
    trace("run_start answered without result.structuredContent.run_id");
    return null;
  }
  return { runId, warnings: Array.isArray(opened.warnings) ? opened.warnings : [] };
}

/**
 * The header the injection LEADS with: the `run_id` the session ticks, and the `run_start`
 * warnings.
 *
 * The id is stated twice — as a field and inside the call that consumes it — because it is the one
 * value EVERY subsequent call takes and the session never obtains it any other way.
 *
 * The WARNINGS ride here rather than being dropped: `run_start` returns them written for a human to
 * act on (a run destroyed because its graph changed, a gate that verifies nothing, a location so
 * ambiguous the run started bound to no Spec). Opening the run for the session and swallowing them
 * would hide exactly what taking the call away from the agent must not cost the human.
 */
function runBlock(run) {
  let block = `=== NYX RUN (opened by nyx for this command — you need no run_start) ===\nrun_id: ${run.runId}\nTick it: nyx_next(run_id="${run.runId}").\n`;
  if (run.warnings.length > 0) {
    block += "WARNINGS from run_start — RELAY each one to the user, in one line, before you tick:\n";
    for (const w of run.warnings) {
      const code = w && typeof w.code === "string" ? w.code : "warning";
      const message = w && typeof w.message === "string" ? w.message : JSON.stringify(w);
      block += `- [${code}] ${message}\n`;
    }
  }
  return `${block}\n`;
}

/**
 * WHAT IS INJECTED WHEN THE REFERENCE THE HUMAN TYPED WAS REFUSED — no run, no manual, one
 * instruction.
 *
 * THE FAILURE THIS CLOSES IS ONE THE WIDENED RECOGNISER CREATED. While `specIdFromPrompt` knew only
 * the canonical id, a first token it did not recognise left `spec_id` unset and the run opened off
 * the `cwd`. Now `KEY-N` / `#N` / `N` are shapes, so a branch name (`feature-42`), a ticket
 * (`JIRA-123`) or a plain number (`/ndev 3 fois plus rapide`) is forwarded, `run_start` refuses it —
 * rightly, opening an unanchored run on a named Spec is what ordered a duplicate — and the whole
 * injection used to be dropped. The session then got NO run and NO manual, and the reason went to
 * stderr, which Claude Code never feeds to the model: from inside the session, nyx had simply said
 * nothing.
 *
 * So the refusal is relayed instead of swallowed. It carries NO `run_id`, deliberately: there is no
 * run, and inventing a header with an empty one is the "session hunting for an id" this hook exists
 * to prevent. What it carries is the token as typed, nyx's own sentence (it names the accepted forms
 * and what the project actually holds) and the SAME instruction the three command stubs give — stop,
 * ask, never author a second Spec — so the two texts an agent reads in one breath agree.
 *
 * IT DOES NOT CLAIM TO KNOW WHICH CONDITION FAILED, and it used to. `lastRunStartRefusal` is set on
 * EVERY `run_start` refusal, and this branch fires whenever one arrives beside a forwarded token —
 * so a location anchoring several Specs, a project with no `ndev` workflow, or a cross-project
 * disagreement all reached a header asserting "THE SPEC REFERENCE YOU TYPED WAS REFUSED" and an
 * instruction to go and ask which Spec was meant. The hook cannot tell those apart (the verdict is
 * the core's, and matching on its prose would be a second copy of it), so it states what it DOES
 * know — the run was refused, here is nyx's sentence — and makes the "ask which Spec" move
 * conditional on that sentence being about the reference. The refusal itself is the discriminator,
 * and it is right there to be read.
 */
function refusedReferenceBlock(command, specId, refusal) {
  return (
    `=== NYX REFUSED TO OPEN THE RUN (no run was opened) ===\n` +
    `You invoked /${command} with the Spec reference "${specId}". nyx refused:\n` +
    `${refusal}\n` +
    `\n` +
    `There is NO run and NO dispatcher manual for this turn — do not call nyx_next, and do not go ` +
    `looking for a run to tick.\n` +
    `READ the refusal above: it names the condition that failed. If it is about the reference ` +
    `"${specId}" (nyx could not resolve it to a Spec), STOP and ASK the human which Spec they ` +
    `meant, quoting the refusal. If it is about something else — a workspace anchoring several ` +
    `Specs, a missing workflow — relay THAT to the human instead of asking about the reference. ` +
    `Either way: never substitute a Spec of your own choosing, and never author a new one to get ` +
    `past a refusal — that is how a duplicate of the very Spec they named gets ordered. Once the ` +
    `human answers, re-invoke /${command}.\n\n`
  );
}

/**
 * The version of the nyx plugin THIS session loaded (#18b — the stale-plugin badge). A
 * Claude session loads the plugin's hooks ONCE at start and keeps them until it restarts,
 * so an already-running session keeps the OLD hooks with no signal after a plugin update.
 * We report the loaded version at runtime via `CLAUDE_PLUGIN_ROOT/.claude-plugin/plugin.json`
 * so the core can compare it to the version nyx BUNDLES and flag a periphery "plugin périmé"
 * badge on that terminal. BEST-EFFORT: any failure (no env, unreadable file, malformed
 * JSON, missing/blank version) returns `""` so the field is simply OMITTED from the POST —
 * the hook NEVER throws and a missing version is treated as "unknown" (NOT stale) downstream.
 */
function readPluginVersion() {
  try {
    const root = process.env.CLAUDE_PLUGIN_ROOT;
    if (!root) return "";
    const manifest = path.join(root, ".claude-plugin", "plugin.json");
    const raw = fs.readFileSync(manifest, "utf8");
    const parsed = JSON.parse(raw);
    const version = parsed && typeof parsed.version === "string" ? parsed.version.trim() : "";
    return version;
  } catch (_e) {
    return ""; // unreadable/malformed/missing → unknown version, omit the field.
  }
}

/** Read all of stdin (the hook input JSON) as a string. Resolves "" if no stdin. */
function readStdin() {
  return new Promise((resolve) => {
    // If stdin is a TTY (run by hand without a pipe) there is no hook payload.
    if (process.stdin.isTTY) {
      resolve("");
      return;
    }
    let buf = "";
    let settled = false;
    const done = () => {
      if (settled) return;
      settled = true;
      resolve(buf);
    };
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (d) => {
      buf += d;
    });
    process.stdin.on("end", done);
    process.stdin.on("error", done);
    // Safety net: never block forever waiting on stdin.
    setTimeout(done, TIMEOUT_MS).unref();
  });
}

/**
 * Best-effort POST of the JSON-RPC body; resolves on any outcome (never rejects) with either
 * `{ status, body }` (the sidecar answered) or `{ error }` (it did not: down, timeout, socket
 * error). The two must stay apart — a caller that cannot tell them apart cannot say WHY it
 * injected nothing.
 */
function post(body, timeoutMs = TIMEOUT_MS) {
  return new Promise((resolve) => {
    let settled = false;
    const done = (value) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    let req;
    try {
      req = http.request(
        {
          host: "127.0.0.1",
          port: PORT,
          path: "/mcp",
          method: "POST",
          headers: {
            "content-type": "application/json",
            "content-length": Buffer.byteLength(body),
          },
        },
        (res) => {
          // Capture the response body so a SessionStart can relay nyx's context (#22).
          let respBody = "";
          res.setEncoding("utf8");
          res.on("data", (d) => {
            respBody += d;
          });
          res.on("end", () => done({ status: res.statusCode, body: respBody }));
          res.on("error", (e) => done({ error: reason(e) }));
        },
      );
    } catch (_e) {
      done({ error: reason(_e) });
      return;
    }
    req.on("error", (e) => done({ error: reason(e) })); // ECONNREFUSED (server down) etc.
    req.setTimeout(timeoutMs, () => {
      try {
        req.destroy();
      } catch (_e) {
        /* ignore */
      }
      done({ error: `no answer within ${timeoutMs}ms` });
    });
    try {
      req.write(body);
      req.end();
    } catch (e) {
      done({ error: reason(e) });
    }
  });
}

/** A short, loggable cause out of a node error (`ECONNREFUSED`, a message, or a fallback). */
function reason(e) {
  return (e && (e.code || e.message)) || "request failed";
}

async function main() {
  let input = {};
  try {
    const raw = await readStdin();
    if (raw && raw.trim()) {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        input = parsed;
      }
    }
  } catch (_e) {
    // Malformed/empty stdin → fall through with whatever we have. The core will reject a
    // payload missing hook_event_name/session_id; that's fine (best-effort, never throws).
    input = input || {};
  }

  // Parity with the old `jq '. + {NYX_TERMINAL_ID: $tid}'`: merge the terminal id into the
  // hook input. NYX_TERMINAL_ID wins (it is the authoritative env value).
  const args = Object.assign({}, input, { NYX_TERMINAL_ID: TERMINAL_ID });
  // Belt-and-suspenders: if Claude ever omits hook_event_name from stdin, fall back to argv
  // so SessionStart/SessionEnd are still distinguishable by the core.
  if (!args.hook_event_name && EVENT) {
    args.hook_event_name = EVENT;
  }
  // #18b — report the LOADED plugin version so the core can flag a stale (out-of-date)
  // plugin per session. Best-effort: omitted when unknown (never sent as blank), so a
  // missing version reads as "unknown ⇒ not stale" rather than "stale".
  const pluginVersion = readPluginVersion();
  if (pluginVersion) {
    args.plugin_version = pluginVersion;
  }

  const body = JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "tools/call",
    params: { name: "agent_session_event", arguments: args },
  });

  const response = await post(body);

  // SessionStart: relay nyx's resolved context as `additionalContext` so the agent
  // situates itself inside nyx (project/workspace/terminal) with no prompting (#22). The
  // core returns it in `structuredContent.context`; we just forward it. Best-effort and
  // NEVER throws — a missing/garbled response simply means no context is injected (the
  // session still works). Only SessionStart carries `context`, so we gate on it.
  if (args.hook_event_name === "SessionStart") {
    try {
      const ctx = JSON.parse(response.body || "")?.result?.structuredContent?.context;
      if (ctx && typeof ctx === "string") {
        process.stdout.write(
          JSON.stringify({
            hookSpecificOutput: {
              hookEventName: "SessionStart",
              additionalContext: ctx,
            },
          }),
        );
      }
    } catch (_e) {
      // No context injected on a parse/shape error — harmless.
    }
  }

  // UserPromptSubmit — the `/nspec` `/ndev` `/ndoc` run opening + manual injection (v2.5.8 /
  // PDR-0020, then v2.8 G6). When the submitted prompt INVOKES one of those commands, OPEN the run
  // (`run_start { scope, cwd }`) and fetch the dispatcher manual FOR THAT RUN
  // (`agent_context { role, scope, run_id }`), then relay BOTH in ONE `additionalContext` — the
  // mirror of the SessionStart relay above. The hook anchors the run on the `cwd` Claude Code hands
  // it, so nothing here parses the human's text and no agent resolves an anchor a second time.
  // Best-effort and NEVER throws: a sidecar that is down or a refused `agent_context` injects
  // nothing — but it TRACES why on stderr. A `run_start` REFUSED on the reference the human typed
  // injects nyx's own sentence instead of nothing (`refusedReferenceBlock`).
  if (args.hook_event_name === "UserPromptSubmit") {
    const command = commandFromPrompt(args.prompt);
    const trigger = command ? NYX_COMMANDS[command] : null;
    if (trigger) {
      let injected = false;
      try {
        const cwd = typeof args.cwd === "string" ? args.cwd : "";
        // The Spec the human NAMED, when they named one. It is passed BESIDE the `cwd`, not instead
        // of it: the id resolves WHICH Spec, the cwd still anchors the run's dev workspace.
        const specId = specIdFromPrompt(args.prompt);
        const runBody = JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          method: "tools/call",
          params: {
            name: "run_start",
            // `scope` is the flow this command runs (nspec | ndev | ndoc); `cwd` is the anchor the
            // run resolves its project and dev workspace from; `spec_id` — when the human typed one
            // — is what lets `/ndev` and `/ndoc` open at all in a project holding several active
            // Specs, where a location alone is ambiguous and refused.
            arguments: specId
              ? { scope: trigger.scope, cwd, spec_id: specId }
              : { scope: trigger.scope, cwd },
          },
        });
        const run = runFromResponse(await post(runBody, RUN_OPEN_TIMEOUT_MS));
        if (run) {
          const ctxBody = JSON.stringify({
            jsonrpc: "2.0",
            id: 3,
            method: "tools/call",
            params: {
              name: "agent_context",
              // The RUN is the anchor now: it carries the Spec it drives and the workspace it
              // opened against, so the manual renders off it and no anchor is resolved twice.
              arguments: { role: trigger.role, scope: trigger.scope, run_id: run.runId },
            },
          });
          const manual = manualFromResponse(await post(ctxBody, RUN_OPEN_TIMEOUT_MS));
          if (manual) {
            process.stdout.write(
              JSON.stringify({
                hookSpecificOutput: {
                  hookEventName: "UserPromptSubmit",
                  additionalContext: `${runBlock(run)}${manual}`,
                },
              }),
            );
            injected = true;
          }
        } else if (specId && lastRunStartRefusal) {
          // `run_start` REFUSED a command that NAMED a Spec — relay the refusal instead of going
          // silent. No run means no manual, but it does not mean nothing to say: this is the one
          // failure mode whose sentence nyx wrote for a reader, and the one the session can act on.
          // The condition is deliberately BROAD (a token was forwarded and the server said no), and
          // the block does NOT claim the reference is what failed — the hook cannot know that, so
          // it hands the server's sentence over and lets it discriminate. See
          // [`refusedReferenceBlock`], for that and for why it carries no `run_id`.
          process.stdout.write(
            JSON.stringify({
              hookSpecificOutput: {
                hookEventName: "UserPromptSubmit",
                additionalContext: refusedReferenceBlock(command, specId, lastRunStartRefusal),
              },
            }),
          );
          // NOT `injected = true`: no manual was injected, and the failure is still FILED in nyx
          // below with the sentence already traced. What changed is that the session is no longer
          // the only party left in the dark.
        }
      } catch (e) {
        trace(`unexpected hook error: ${reason(e)}`);
      }
      // A nyx command that got NOTHING is filed in nyx, with the sentence already traced. The
      // happy path files nothing at all.
      if (!injected) {
        await report(command, lastTraced || "no manual injected, with no reason traced");
      }
    } else if (command) {
      // A slash command that is not ours. Traced (not silent) because this is EXACTLY the shape
      // the dead injection took: `/nspec` reaching the hook as something the table does not know
      // must be visible in one debug line, not inferred from an absence.
      const known = Object.keys(NYX_COMMANDS)
        .map((c) => `/${c}`)
        .join(" ");
      trace(`/${command} is not a nyx command (${known})`);
    }
  }
}

// THE RECOGNISER IS EXPORTED so the shared corpus can be replayed against THIS function rather than
// against a copy of it (`contract/spec-ref-corpus.txt`, read from Rust by `spec_ref.rs`'s
// `the_shared_corpus_pins_the_hook_recogniser_to_this_grammar`). A second spelling of the grammar —
// even one living in a test — is the very drift the corpus exists to close.
//
// AND NOTHING ELSE. This file is a surface a third-party agent runtime loads, so every name on it is
// a contract someone can come to depend on; `commandFromPrompt` rode out here with no reader at all,
// which is how an internal helper becomes an unintended anchor point. One reader, one export — add a
// name when a reader arrives, not before.
module.exports = { specIdFromPrompt };

// Top-level guard: under no circumstances do we throw or exit non-zero. Claude Code runs this file
// AS a script (`node <path> <Event>`, see `hooks.json`), so the hook behaves exactly as before;
// requiring it — which only a test does — loads the functions without running the hook.
if (require.main === module) {
  main()
    .catch(() => {})
    .finally(() => {
      process.exit(0);
    });
}
