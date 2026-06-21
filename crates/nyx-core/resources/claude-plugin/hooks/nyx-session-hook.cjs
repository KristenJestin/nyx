#!/usr/bin/env node
/**
 * nyx SessionStart/SessionEnd hook — CROSS-PLATFORM (node, zero external deps).
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
 *   - event kind comes from argv[2] (`SessionStart` | `SessionEnd`) — informational; the
 *     authoritative `hook_event_name` is already inside the hook's stdin JSON, which the
 *     core reads. We still accept argv so the two hooks are self-describing.
 *   - reads the hook's input JSON from STDIN (Claude pipes it in: `hook_event_name`,
 *     `session_id`, `cwd`, `transcript_path`, `source`, ...).
 *   - merges `{ NYX_TERMINAL_ID: process.env.NYX_TERMINAL_ID }` into that object — EXACTLY
 *     what the old `jq '. + {NYX_TERMINAL_ID: $tid}'` did. The core's `agent_session_event`
 *     requires `NYX_TERMINAL_ID` (env) + `hook_event_name` + `session_id` (stdin).
 *   - POSTs the JSON-RPC `tools/call name=agent_session_event` to
 *     `http://127.0.0.1:${NYX_MCP_PORT||8765}/mcp` over native node `http`.
 *
 * GUARANTEES: best-effort, ~1s timeout, NEVER throws, ALWAYS exits 0. A hook that crashes
 * (or whose server is down) must not disturb the user's Claude Code session.
 */
"use strict";

const http = require("node:http");

const EVENT = process.argv[2] || ""; // "SessionStart" | "SessionEnd" (informational)
const PORT = process.env.NYX_MCP_PORT || "8765";
const TERMINAL_ID = process.env.NYX_TERMINAL_ID || "";
const TIMEOUT_MS = 1000;

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
    const done = () => {
      if (settled) return;
      settled = true;
      resolve();
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
          // Drain the response so the socket closes cleanly; we don't need the body.
          res.on("data", () => {});
          res.on("end", done);
          res.on("error", done);
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

  const body = JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "tools/call",
    params: { name: "agent_session_event", arguments: args },
  });

  await post(body);
}

// Top-level guard: under no circumstances do we throw or exit non-zero.
main()
  .catch(() => {})
  .finally(() => {
    process.exit(0);
  });
