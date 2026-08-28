#!/usr/bin/env node
/**
 * WHO is answering on the dev-server port — the one fact `scripts/dev.cjs` used to assume.
 *
 * The port is FIXED (1420) and every nyx worktree uses it, so `http.get(url, () => startElectron())`
 * proved a PRESENCE, never an APPARTENANCE: three successive launches loaded ANOTHER worktree's
 * renderer against this worktree's backend, silently. This module is the answer to "is that server
 * mine?", kept out of `dev.cjs` so it can be exercised against a real HTTP server without launching
 * Electron (`dev-server-identity.test.ts`).
 *
 * The discriminator is the renderer ROOT the server was configured with — an absolute path, so two
 * worktrees can never serve the same one. `vite.config.ts` publishes it on {@link DEV_IDENTITY_PATH}
 * from THIS module's helpers, which is also why the port lives here rather than in two places.
 *
 * Plain CJS with no dependencies: `dev.cjs` is a `require`-only node script, and `vite.config.ts`
 * pulls the same constants through `createRequire`, so there is ONE definition of the port, the
 * identity route, and the payload shape.
 */
"use strict";
const http = require("node:http");

/**
 * The fixed dev-server port. `vite.config.ts` binds it with `strictPort: true` (so a taken port is
 * a LOUD failure rather than a silent slide to 1421) and `dev.cjs` waits on it. Deliberately NOT
 * derived per worktree — a user decision: two branches will still not run in parallel, but the
 * second now fails loudly instead of borrowing the first one's UI.
 */
const DEV_SERVER_PORT = 1420;

/** The dev-server URL `dev.cjs` waits on and hands to Electron as `NYX_DEV_SERVER_URL`. */
const DEV_SERVER_URL = `http://localhost:${DEV_SERVER_PORT}`;

/**
 * The route `vite.config.ts` answers the identity probe on. Under a `__`-prefix so it can never
 * collide with an app route, and outside `/@` so Vite's own internal middlewares ignore it.
 */
const DEV_IDENTITY_PATH = "/__nyx_dev_identity";

/**
 * What the payload claims to be. A server that does not say this is not our dev server, whatever
 * else it is — an unrelated app on 1420, a stale `vite preview`, a proxy.
 */
const DEV_IDENTITY_APP = "nyx-v2-renderer";

/** How long to wait for an answer before calling the port unreachable. */
const PROBE_TIMEOUT_MS = 2000;

/** The three answers a probe can give. */
const IDENTITY = {
  /** Our dev server, serving OUR renderer root. */
  OURS: "ours",
  /** Something answered, and it is not our dev server. Never launch Electron against it. */
  FOREIGN: "foreign",
  /** Nothing answered yet — Vite may still be starting. Keep polling. */
  UNREACHABLE: "unreachable",
};

/**
 * Put an absolute path in the ONE form both sides compare on: forward slashes, no trailing
 * separator, and case-folded on Windows (where `D:\x` and `d:/X` are the same directory, and the
 * drive-letter case in particular differs between what Node resolves and what Vite reports).
 */
function normalizeRoot(root) {
  const slashed = String(root).replace(/\\/g, "/").replace(/\/+$/, "");
  return process.platform === "win32" ? slashed.toLowerCase() : slashed;
}

/** The body `vite.config.ts` serves on {@link DEV_IDENTITY_PATH} for a given renderer root. */
function identityPayload(root) {
  return { app: DEV_IDENTITY_APP, root: normalizeRoot(root) };
}

/**
 * Read one probe RESPONSE and say whose server it came from. Pure — no I/O — so every verdict
 * below is reachable from a test.
 *
 * @param {string} expectedRoot the renderer root THIS worktree would serve
 * @param {{ statusCode: number, body: string }} response
 * @returns {{ verdict: string, detail: string }}
 */
function classifyIdentityResponse(expectedRoot, response) {
  const expected = normalizeRoot(expectedRoot);
  if (response.statusCode !== 200) {
    return {
      verdict: IDENTITY.FOREIGN,
      detail: `it answered ${response.statusCode} on ${DEV_IDENTITY_PATH}; a nyx dev server answers 200 there`,
    };
  }
  let payload;
  try {
    payload = JSON.parse(response.body);
  } catch {
    return {
      verdict: IDENTITY.FOREIGN,
      detail: `it answered ${DEV_IDENTITY_PATH} with something that is not JSON`,
    };
  }
  if (payload === null || typeof payload !== "object" || payload.app !== DEV_IDENTITY_APP) {
    return {
      verdict: IDENTITY.FOREIGN,
      detail: `it does not identify as ${DEV_IDENTITY_APP}`,
    };
  }
  if (normalizeRoot(payload.root ?? "") !== expected) {
    return {
      verdict: IDENTITY.FOREIGN,
      detail: `it serves ${payload.root} — this worktree is ${expected}`,
    };
  }
  return { verdict: IDENTITY.OURS, detail: `serving ${expected}` };
}

/**
 * Ask the server at `url` who it is, and call back with the verdict.
 *
 * `accept: application/json` matters: Vite's HTML fallback answers `index.html` for an unknown
 * route that accepts HTML, which would turn "this server has no identity route" into a 200 page.
 * With this header an unknown route is a 404 — a FOREIGN verdict, which is what a server without
 * our middleware is.
 *
 * @param {string} url the dev-server base URL
 * @param {string} expectedRoot the renderer root THIS worktree would serve
 * @param {(result: { verdict: string, detail: string }) => void} done
 */
function probeDevServerIdentity(url, expectedRoot, done) {
  let settled = false;
  const settle = (result) => {
    if (settled) return;
    settled = true;
    done(result);
  };
  const target = new URL(DEV_IDENTITY_PATH, url);
  const request = http.get(target, { headers: { accept: "application/json" } }, (res) => {
    let body = "";
    res.setEncoding("utf8");
    res.on("data", (chunk) => {
      body += chunk;
    });
    res.on("end", () => settle(classifyIdentityResponse(expectedRoot, { statusCode: res.statusCode, body })));
    res.on("error", () => settle({ verdict: IDENTITY.UNREACHABLE, detail: `the response from ${url} broke off` }));
  });
  // SETTLES ITSELF. `request.destroy()` with no error emits no `error` event, so destroying alone
  // left the callback un-called and `dev.cjs` polling a silent socket forever with no output — the
  // exact hang this timeout exists to end. Found by the test that drives a server which accepts and
  // never answers.
  request.setTimeout(PROBE_TIMEOUT_MS, () => {
    request.destroy();
    settle({
      verdict: IDENTITY.UNREACHABLE,
      detail: `${url} accepted the connection but did not answer within ${PROBE_TIMEOUT_MS}ms`,
    });
  });
  request.on("error", () => settle({ verdict: IDENTITY.UNREACHABLE, detail: `nothing is answering on ${url}` }));
}

/** What [`decideOnViteExit`] tells `dev.cjs` to do. */
const VITE_EXIT = {
  /** The port already answers as OURS: use that server instead of aborting. */
  ATTACH: "attach",
  /** Nothing usable is there — stop, and say what was measured. */
  ABORT: "abort",
};

/**
 * WHAT A NON-ZERO VITE EXIT MEANS — measured, not guessed.
 *
 * `dev.cjs` used to print "`<url>` is likely already held by another worktree's dev server" and abort
 * on ANY non-zero exit of the Vite child. Two things were wrong with that, and both were measured:
 *
 *   * THE SENTENCE WAS A GUESS. A broken `vite.config.ts`, a plugin throwing, a missing dependency —
 *     every one of them exits non-zero and got told it was another worktree. The script had a probe
 *     that could have answered, sitting three lines below, and never asked it.
 *   * THE ABORT WAS WRONG IN THE COMMON CASE. `bun run dev` then `bun run dev:desktop` from the SAME
 *     worktree: the second Vite dies on `strictPort` (the port is taken — by our own first server),
 *     the probe answers OURS in milliseconds and `startElectron()` is already under way, and the
 *     abort then killed `bundle.cjs` mid-write, leaving a truncated `dist/main` and `dist/preload`.
 *
 * So: probe FIRST, decide from the verdict, and let the message carry the exit code and the probe's
 * own detail rather than a story. "Another worktree" is named only after a FOREIGN verdict.
 *
 * @param {string} url the dev-server base URL
 * @param {string} expectedRoot the renderer root THIS worktree would serve
 * @param {number|null} exitCode the code Vite exited with
 * @param {(decision: { action: string, message: string }) => void} done
 */
function decideOnViteExit(url, expectedRoot, exitCode, done) {
  const died = `the Vite dev server exited (code ${exitCode})`;
  probeDevServerIdentity(url, expectedRoot, (result) => {
    if (result.verdict === IDENTITY.OURS) {
      return done({
        action: VITE_EXIT.ATTACH,
        message: `${died}, but ${url} already answers as OURS (${result.detail}) — attaching to the dev server that is already running`,
      });
    }
    if (result.verdict === IDENTITY.FOREIGN) {
      return done({
        action: VITE_EXIT.ABORT,
        message: `${died} and ${url} is held by another dev server: ${result.detail}`,
      });
    }
    return done({
      action: VITE_EXIT.ABORT,
      message: `${died} and NOTHING answers on ${url} — the port is free, so this is Vite itself failing to start (a broken config, a plugin error, a missing dependency). Read its output above`,
    });
  });
}

module.exports = {
  DEV_SERVER_PORT,
  DEV_SERVER_URL,
  DEV_IDENTITY_PATH,
  DEV_IDENTITY_APP,
  IDENTITY,
  VITE_EXIT,
  normalizeRoot,
  identityPayload,
  classifyIdentityResponse,
  probeDevServerIdentity,
  decideOnViteExit,
};
