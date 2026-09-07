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
 * SLASH-COMMAND MANUAL INJECTION (v2.5.8 / PDR-0020, then PDR-0031): the `/nspec` `/ndev` `/ndoc`
 * command stubs are the triggers. When a `UserPromptSubmit` prompt INVOKES one of them, this hook
 * fetches the command's MANUAL — `tools/call name=agent_context` (`{ role, cwd, spec_id? }`) — and
 * relays it as `additionalContext`, so the session opens on the manual instead of spending its first
 * turns hunting for one. There is no run to open: PDR-0031 retired the orchestration engine, and the
 * manual is now the whole of what nyx hands the session. The ROLE comes from the command
 * (`nspec → spec`, `ndev → dev`, `ndoc → doc`); the ANCHOR is the `cwd` Claude Code hands the hook,
 * plus the Spec reference when the human typed one.
 * IT READS THE COMMAND NAME AND, AT MOST, ONE TOKEN: the Spec REFERENCE, when the first argument IS
 * a whole one (`specIdFromPrompt` — it recognises a shape and forwards the token RAW; nyx resolves
 * it). The three stubs offer to name a Spec, and a `cwd` cannot honour that in a project holding
 * several active Specs — a location resolves none of them, and for `/ndev` and `/ndoc` nothing
 * anchors. Everything else in the prompt is the human's instruction and stays untouched for the
 * agent to read. Fully best-effort — a prompt that is not a nyx command, a sidecar that is down, or
 * a REFUSED `agent_context` injects no manual, but each of those says WHY on stderr (see `trace`)
 * instead of failing mute. And when the refusal is about the REFERENCE the human typed, nyx's own
 * sentence is injected in the manual's place (`refusedReferenceBlock`) — stderr is not read by the
 * model, so swallowing it would make a refused `/ndev 3 fois plus rapide` look like nyx saying
 * nothing.
 *
 * SUB-AGENT CHARTER (`SubagentStart`, PDR-0031 / `docs/product/agent-manuals.md` §4): a sub-agent
 * spawned by an orchestrator inherits NOTHING of nyx — not the workspace, not the Spec, not the
 * managed commands. So the same `agent_session_event` POST this file already makes on every hook
 * carries back a `context` for that event too, and it is relayed as `additionalContext`. The
 * MISSION stays the orchestrator's; the charter is only what nyx knows and the sub-agent cannot.
 * NOTE THE SHAPE: `SubagentStart` takes the SAME `hookSpecificOutput` wrapper as `SessionStart` and
 * `UserPromptSubmit` just above. This file used to claim the opposite — a BARE
 * `{ "additionalContext": … }`, with the wrapper said to be dropped silently. MEASURED 2026-09-07
 * against Claude Code v2.1.246 on Windows, in a real nyx terminal, with this plugin installed: the
 * bare shape is the one that is dropped. The observable is the sub-agent's own transcript
 * (`~/.claude/projects/<proj>/<session>/subagents/agent-*.jsonl`). A hook that RAN leaves a
 * `hook_success` attachment carrying its stdout — the bare `SubagentStart` had one, so it fired and
 * succeeded — but text that was actually INJECTED leaves a SECOND attachment, of type
 * `hook_additional_context`, whose `content` IS that text. The bare run left none, and the live
 * sub-agent, asked to quote the first line of its nyx context, said it had received none. In the
 * SAME session the wrapped `SessionStart` and `UserPromptSubmit` each left both attachments, and the
 * manuals they carried were quoted back verbatim by the model. So: a `hook_additional_context`
 * attachment appears if and only if the text reached the model, and only the wrapper produces one.
 *
 * GUARANTEES: best-effort, BOUNDED (~1s per call, and `MANUAL_TIMEOUT_MS` for the manual fetch —
 * it does real server-side work before answering), NEVER throws, ALWAYS exits 0. A hook that
 * crashes (or whose server is down) must not disturb the user's Claude session.
 */
"use strict";

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

const EVENT = process.argv[2] || ""; // hook name (informational; stdin is authoritative)
const TERMINAL_ID = process.env.NYX_TERMINAL_ID || "";
const TIMEOUT_MS = 1000;
/**
 * The bound for the MANUAL fetch (`agent_context`), as opposed to the fire-and-forget
 * `agent_session_event` POST that keeps the 1s above.
 *
 * WHY IT IS NOT 1s. `agent_context` RESOLVES BEFORE IT ANSWERS: the anchor (`cwd` → workspace →
 * Spec, or the reference the human typed), the Spec and its revision, the whole task list with its
 * dependency edges, and the workspace's managed commands with their notes. The MCP surface serves it
 * on a small worker pool it shares with the terminal tools. So the POST genuinely exceeds 1000ms on
 * a busy machine, and a manual that arrives late is worth more than no manual at all: losing it
 * costs the session its whole nyx context for the turn.
 *
 * Still BOUNDED, and bounded by what a human will sit through: this hook runs on the prompt the
 * user just submitted, so the wait is visible. Claude Code's own hook budget is far larger.
 */
const MANUAL_TIMEOUT_MS = 8000;
/** nyx's default MCP port — the last resort, and only ever right when ONE nyx runs (see below). */
const DEFAULT_PORT = "8766";

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
 * all default to 8766, so a hook that reaches for it POSTs into whichever one bound the port
 * first — a DIFFERENT application, which answers `unknown tool` to calls meant for this one, and
 * does so silently. That is the case reading the sibling `.mcp.json` exists to avoid.
 *
 * That sibling read must match the port out of the descriptor's OWN url shape. nyx templates it as
 * `http://127.0.0.1:${NYX_MCP_PORT:-8766}/mcp` (an env expansion the MCP client resolves, with the
 * bound port as the literal fallback), so the fallback digits are what to take — falling back to a
 * bare `:PORT/` for the older frozen form.
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
      // The templated form `${NYX_MCP_PORT:-8766}` first (this file is read raw, unexpanded), then
      // the older frozen `:PORT/` literal.
      const m = url.match(/\$\{NYX_MCP_PORT:-(\d+)\}/) || url.match(/:(\d+)\//);
      if (m) return m[1];
    }
  } catch (_e) {
    /* no plugin root, unreadable or malformed file → the literal default */
  }
  return DEFAULT_PORT;
}

const PORT = resolvePort();

/**
 * The nyx slash commands and the manual each one injects (`agent_context {role}`).
 *
 * ONE ROLE PER COMMAND, and they are three (PDR-0031): `/nspec` writes the Spec, `/ndev` builds it,
 * `/ndoc` reconciles the docs with what it shipped. The old table sent all three to `role: "dev"`
 * with a `scope` beside it, because the scope named the graph the engine was about to walk; with the
 * engine gone the scope names nothing, and the role is the whole of what selects a manual.
 *
 * The TRIGGER is the command NAME because that is ALL a `UserPromptSubmit` hook ever sees:
 * Claude Code hands the hook the prompt the human TYPED (`prompt: "/nspec KEY-3 …"`) and
 * expands the stub body only afterwards, so the `<!-- NYX-AGENT-CONTEXT role=… -->`
 * sentinel a stub declares NEVER reaches this hook — keying on it injected nothing, ever.
 * The stub's sentinel stays the DECLARATION of what its command triggers; the push-injection
 * e2e asserts this table agrees with it, so a drift on either side fails there.
 */
const NYX_COMMANDS = {
  nspec: { role: "spec" },
  ndev: { role: "dev" },
  ndoc: { role: "doc" },
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
 * The SERVER's own sentence when `agent_context` REFUSED, kept apart from [`lastTraced`] because it
 * is the only failure whose text is worth showing the MODEL: nyx wrote it for a reader (it names the
 * accepted forms and what the project actually holds), and it is the one the session can act on.
 * Empty for every other failure mode — a sidecar that is down, a timeout, a malformed body — where
 * there is no server sentence to relay.
 *
 * IT MOVED HERE FROM `run_start` (PDR-0031). The ambiguous-reference refusal used to be the engine's:
 * the run refused to open on a token that resolved to no Spec, or to several. `agent_context` now
 * carries that verdict, because it is the call that resolves the anchor.
 */
let lastContextRefusal = "";

/**
 * Say WHY no manual was injected. A hook must never break the prompt, so this branch stays
 * best-effort and exits 0 — but "best effort" is not a licence to be undiagnosable: three different
 * situations (the prompt is not a nyx command, the sidecar is unreachable, `agent_context` refused)
 * used to be one silence, which is how a dead injection survived a whole release. stderr is written FIRST because it is the only sink that still works when the sidecar
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
    lastContextRefusal = parsed.error.message || JSON.stringify(parsed.error);
    trace(`agent_context refused: ${lastContextRefusal}`);
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
 * WHAT IS INJECTED WHEN THE REFERENCE THE HUMAN TYPED WAS REFUSED — no manual, one instruction.
 *
 * THE FAILURE THIS CLOSES IS ONE THE WIDENED RECOGNISER CREATED. While `specIdFromPrompt` knew only
 * the canonical id, a first token it did not recognise left `spec_id` unset and the manual rendered
 * off the `cwd`. Now `KEY-N` / `#N` / `N` are shapes, so a branch name (`feature-42`), a ticket
 * (`JIRA-123`) or a plain number (`/ndev 3 fois plus rapide`) is forwarded, `agent_context` refuses
 * it — rightly, rendering a manual for a Spec nobody named is what ordered a duplicate — and the
 * whole injection used to be dropped. The session then got NO manual, and the reason went to stderr,
 * which Claude Code never feeds to the model: from inside the session, nyx had simply said nothing.
 *
 * So the refusal is relayed instead of swallowed. What it carries is the token as typed, nyx's own
 * sentence (it names the accepted forms and what the project actually holds) and the SAME
 * instruction the three command stubs give — stop, ask, never author a second Spec — so the two
 * texts an agent reads in one breath agree.
 *
 * IT DOES NOT CLAIM TO KNOW WHICH CONDITION FAILED, and it used to. `lastContextRefusal` is set on
 * EVERY `agent_context` refusal, and this branch fires whenever one arrives beside a forwarded
 * token — so a location anchoring several Specs, a Spec in another project, or a malformed argument
 * all reached a header asserting "THE SPEC REFERENCE YOU TYPED WAS REFUSED" and an instruction to go
 * and ask which Spec was meant. The hook cannot tell those apart (the verdict is the core's, and
 * matching on its prose would be a second copy of it), so it states what it DOES know — the manual
 * was refused, here is nyx's sentence — and makes the "ask which Spec" move conditional on that
 * sentence being about the reference. The refusal itself is the discriminator, and it is right there
 * to be read.
 */
function refusedReferenceBlock(command, specId, refusal) {
  return (
    `=== NYX REFUSED TO RENDER THE MANUAL (no manual for this turn) ===\n` +
    `You invoked /${command} with the Spec reference "${specId}". nyx refused:\n` +
    `${refusal}\n` +
    `\n` +
    `There is NO manual for this turn, so nothing below tells you how to run /${command}.\n` +
    `READ the refusal above: it names the condition that failed. If it is about the reference ` +
    `"${specId}" (nyx could not resolve it to a Spec, or it resolved to several), STOP and ASK the ` +
    `human which Spec they meant, quoting the refusal. If it is about something else — a workspace ` +
    `nyx does not know, a Spec in another project — relay THAT to the human instead of asking about ` +
    `the reference. Either way: never substitute a Spec of your own choosing, and never author a ` +
    `new one to get past a refusal — that is how a duplicate of the very Spec they named gets ` +
    `ordered. Once the human answers, re-invoke /${command}.\n\n`
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
 *
 * IT CARRIES `x-nyx-terminal-id`, and that is not decoration. The sidecar reads that header to
 * learn WHICH TERMINAL is calling (`caller_terminal_id`), and the extension-tool dispatch uses it
 * to auto-map the Spec a call worked on onto that terminal's agent session
 * (`link_terminal_active_session_spec`). Without it, the manual fetch this hook makes at
 * `UserPromptSubmit` — and the `agent_session_event` the `SubagentStart` charter rides on, which
 * resolves the parent terminal's workspace from that very header — arrived with no caller, the
 * auto-map was skipped, and the terminal's dev overlay never learnt which Spec was being worked:
 * `/nspec` mapped (its `spec_create` goes through `.mcp.json`, which DOES carry the header) and
 * `/ndev` never did.
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
            // Only when it is really known: an EMPTY header is not "no terminal", it is a header
            // the server has to decide what to do with. Omitting it keeps "the hook ran outside a
            // nyx terminal" the same fact on the wire as before.
            ...(TERMINAL_ID ? { "x-nyx-terminal-id": TERMINAL_ID } : {}),
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

  // SubagentStart — the sub-agent CHARTER (PDR-0031, `docs/product/agent-manuals.md` §4). A
  // sub-agent the orchestrator spawned inherits its MISSION from the orchestrator and nothing else:
  // not the workspace, not the Spec, not the managed commands and their notes. nyx knows all three
  // from the parent terminal (`NYX_TERMINAL_ID` → workspace → anchored Spec → commands), so the
  // `agent_session_event` POST already made above answers with the charter in
  // `structuredContent.context` and we relay it here.
  //
  // THE SHAPE IS THE SAME AS THE OTHER TWO: the `hookSpecificOutput` wrapper, tagged
  // `SubagentStart`. This branch used to write a BARE `{ "additionalContext": … }` and the comment
  // here asserted the wrapper would be dropped silently. It is the other way round, and the way we
  // know is a measurement, not a reading of the docs — 2026-09-07, Claude Code v2.1.246 on Windows,
  // a real sub-agent spawned from a real nyx terminal with this plugin installed. The observable is
  // the `hook_additional_context` attachment in the sub-agent's transcript
  // (`~/.claude/projects/<proj>/<session>/subagents/agent-*.jsonl`): it is written when, and only
  // when, the text reached the model. The bare run left a `hook_success` attachment carrying this
  // stdout — the hook fired, and succeeded — and NO `hook_additional_context`; the sub-agent,
  // asked, confirmed it had no nyx context. The wrapped `SessionStart` and `UserPromptSubmit` of
  // that same session each left both attachments, and their manuals came back quoted verbatim. So
  // the wrapper is what carries; do not "restore" the bare shape on the strength of a comment.
  // Best-effort and NEVER throws, like every other branch here.
  if (args.hook_event_name === "SubagentStart") {
    try {
      const charter = JSON.parse(response.body || "")?.result?.structuredContent?.context;
      if (charter && typeof charter === "string") {
        process.stdout.write(
          JSON.stringify({
            hookSpecificOutput: {
              hookEventName: "SubagentStart",
              additionalContext: charter,
            },
          }),
        );
      }
    } catch (_e) {
      // No charter injected on a parse/shape error — the sub-agent still runs on its mission.
    }
  }

  // UserPromptSubmit — the `/nspec` `/ndev` `/ndoc` manual injection (v2.5.8 / PDR-0020, then
  // PDR-0031). When the submitted prompt INVOKES one of those commands, fetch that command's manual
  // (`agent_context { role, cwd, spec_id? }`) and relay it as `additionalContext` — the mirror of
  // the SessionStart relay above. The hook anchors on the `cwd` Claude Code hands it, so nothing
  // here parses the human's text and no agent resolves an anchor a second time. Best-effort and
  // NEVER throws: a sidecar that is down injects nothing — but it TRACES why on stderr. An
  // `agent_context` REFUSED on the reference the human typed injects nyx's own sentence instead of
  // nothing (`refusedReferenceBlock`).
  if (args.hook_event_name === "UserPromptSubmit") {
    const command = commandFromPrompt(args.prompt);
    const trigger = command ? NYX_COMMANDS[command] : null;
    if (trigger) {
      let injected = false;
      try {
        const cwd = typeof args.cwd === "string" ? args.cwd : "";
        // The Spec the human NAMED, when they named one. It is passed BESIDE the `cwd`, not instead
        // of it: the id resolves WHICH Spec, the cwd still anchors the workspace the manual renders
        // the commands of.
        const specId = specIdFromPrompt(args.prompt);
        const ctxBody = JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          method: "tools/call",
          params: {
            name: "agent_context",
            // `role` is the manual this command reads (spec | dev | doc); `cwd` is the anchor nyx
            // resolves the project and workspace from; `spec_id` — when the human typed one — is
            // what lets `/ndev` and `/ndoc` render at all in a project holding several active Specs,
            // where a location alone is ambiguous and refused.
            arguments: specId ? { role: trigger.role, cwd, spec_id: specId } : { role: trigger.role, cwd },
          },
        });
        const manual = manualFromResponse(await post(ctxBody, MANUAL_TIMEOUT_MS));
        if (manual) {
          process.stdout.write(
            JSON.stringify({
              hookSpecificOutput: {
                hookEventName: "UserPromptSubmit",
                additionalContext: manual,
              },
            }),
          );
          injected = true;
        } else if (specId && lastContextRefusal) {
          // `agent_context` REFUSED a command that NAMED a Spec — relay the refusal instead of going
          // silent. No manual does not mean nothing to say: this is the one failure mode whose
          // sentence nyx wrote for a reader, and the one the session can act on. The condition is
          // deliberately BROAD (a token was forwarded and the server said no), and the block does
          // NOT claim the reference is what failed — the hook cannot know that, so it hands the
          // server's sentence over and lets it discriminate. See [`refusedReferenceBlock`].
          process.stdout.write(
            JSON.stringify({
              hookSpecificOutput: {
                hookEventName: "UserPromptSubmit",
                additionalContext: refusedReferenceBlock(command, specId, lastContextRefusal),
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
