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
 *   - POSTs the JSON-RPC `tools/call name=agent_session_event` to
 *     `http://127.0.0.1:${NYX_MCP_PORT||8765}/mcp` over native node `http`.
 *
 * SLASH-COMMAND MANUAL INJECTION (v2.5.8 / PDR-0020): the bundled `/nspec` `/ndev` `/ndoc`
 * command stubs each carry a sentinel marker (`NYX-AGENT-CONTEXT role=<role>`). When a
 * `UserPromptSubmit` prompt carries that marker, this hook derives the role from it, POSTs a
 * SECOND `tools/call name=agent_context` (`{ role, cwd }`) to the SAME sidecar, and relays the
 * rendered role manual back to Claude as `UserPromptSubmit` `additionalContext` — the exact
 * mirror of the SessionStart context relay below. ZERO argument parsing: the marker only names
 * the role; the human's free text (id + instruction) stays untouched for the agent to read.
 * `agent_context` resolves the Spec from `cwd`, so no id is needed. Fully best-effort — a
 * missing marker, a sidecar that is down, or an unresolved Spec simply injects nothing.
 *
 * GUARANTEES: best-effort, ~1s timeout, NEVER throws, ALWAYS exits 0. A hook that crashes
 * (or whose server is down) must not disturb the user's Claude Code session.
 */
"use strict";

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

const EVENT = process.argv[2] || ""; // hook name (informational; stdin is authoritative)
const PORT = process.env.NYX_MCP_PORT || "8765";
const TERMINAL_ID = process.env.NYX_TERMINAL_ID || "";
const TIMEOUT_MS = 1000;

/**
 * The sentinel a `/nspec` `/ndev` `/ndoc` command stub embeds in its expanded prompt body
 * (`<!-- NYX-AGENT-CONTEXT role=<role> -->`). A `UserPromptSubmit` carrying it means the user
 * ran a nyx slash command → we render that role's manual and inject it (see `roleFromPrompt`).
 * Only the three TRIGGERABLE roles ship as stubs; coder/auditor/prototype are pull-only (the
 * dev orchestrator fetches them via `agent_context`), so an unknown role in the marker is
 * ignored rather than trusted.
 */
const AGENT_ROLE_MARKER = /NYX-AGENT-CONTEXT\s+role=([a-z][a-z-]*)/i;
const TRIGGERABLE_ROLES = new Set(["spec", "dev", "doc"]);

/**
 * Derive the agent role from a submitted prompt, or "" when it carries no nyx sentinel (a
 * normal prompt) or names a role we do not inject. Pure + defensive: a non-string / missing
 * prompt yields "". ZERO argument parsing — only the marker's role token is read; the human's
 * free text is never inspected.
 */
function roleFromPrompt(prompt) {
  if (typeof prompt !== "string" || prompt.length === 0) return "";
  const m = prompt.match(AGENT_ROLE_MARKER);
  if (!m) return "";
  const role = m[1].toLowerCase();
  return TRIGGERABLE_ROLES.has(role) ? role : "";
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

/** Best-effort POST of the JSON-RPC body; resolves on any outcome (never rejects). */
function post(body) {
  return new Promise((resolve) => {
    let settled = false;
    const done = (value) => {
      if (settled) return;
      settled = true;
      resolve(typeof value === "string" ? value : "");
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
          res.on("end", () => done(respBody));
          res.on("error", () => done());
        },
      );
    } catch (_e) {
      done();
      return;
    }
    req.on("error", done); // ECONNREFUSED (server down) etc. — best-effort, swallow.
    req.setTimeout(TIMEOUT_MS, () => {
      try {
        req.destroy();
      } catch (_e) {
        /* ignore */
      }
      done();
    });
    try {
      req.write(body);
      req.end();
    } catch (_e) {
      done();
    }
  });
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

  const responseBody = await post(body);

  // SessionStart: relay nyx's resolved context as `additionalContext` so the agent
  // situates itself inside nyx (project/workspace/terminal) with no prompting (#22). The
  // core returns it in `structuredContent.context`; we just forward it. Best-effort and
  // NEVER throws — a missing/garbled response simply means no context is injected (the
  // session still works). Only SessionStart carries `context`, so we gate on it.
  if (args.hook_event_name === "SessionStart") {
    try {
      const ctx = JSON.parse(responseBody)?.result?.structuredContent?.context;
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

  // UserPromptSubmit — the `/nspec` `/ndev` `/ndoc` slash-command manual injection (v2.5.8 /
  // PDR-0020). When the submitted prompt carries the stub's `NYX-AGENT-CONTEXT role=<role>`
  // sentinel, POST a SECOND `agent_context` call ({ role, cwd }) and relay the rendered manual
  // as `additionalContext` — the mirror of the SessionStart relay above. `agent_context`
  // resolves the Spec from `cwd`, so no id is parsed here. Best-effort and NEVER throws: a
  // sidecar that is down, an unresolved Spec, or a garbled response simply injects nothing (the
  // agent still has the stub's own fallback instruction to pull the manual itself).
  if (args.hook_event_name === "UserPromptSubmit") {
    const role = roleFromPrompt(args.prompt);
    if (role) {
      try {
        const cwd = typeof args.cwd === "string" ? args.cwd : "";
        const ctxBody = JSON.stringify({
          jsonrpc: "2.0",
          id: 2,
          method: "tools/call",
          params: { name: "agent_context", arguments: { role, cwd } },
        });
        const ctxResp = await post(ctxBody);
        const manual = JSON.parse(ctxResp)?.result?.structuredContent?.context;
        if (manual && typeof manual === "string") {
          process.stdout.write(
            JSON.stringify({
              hookSpecificOutput: {
                hookEventName: "UserPromptSubmit",
                additionalContext: manual,
              },
            }),
          );
        }
      } catch (_e) {
        // No manual injected on a parse/shape/network error — harmless (best-effort).
      }
    }
  }
}

// Top-level guard: under no circumstances do we throw or exit non-zero.
main()
  .catch(() => {})
  .finally(() => {
    process.exit(0);
  });
