#!/usr/bin/env node
/**
 * WHICH port this worktree owns, and WHO is answering on it — the two facts `scripts/dev.cjs`
 * used to assume.
 *
 * Every worktree once served on a FIXED 1420, so `http.get(url, () => startElectron())` proved a
 * PRESENCE, never an APPARTENANCE: three successive launches loaded ANOTHER worktree's renderer
 * against this worktree's backend, silently. That history is why the identity probe below exists,
 * and it is why the probe STAYS now that the ports are separated: a stale server from THIS
 * worktree, a proxy, or an unrelated app can still answer on the derived port, and none of those
 * are "our dev server". A presence is still not an appartenance.
 *
 * What DID change is the port. It was fixed on purpose — "two branches will still not run in
 * parallel, but the second fails loudly instead of borrowing the first one's UI" — and the user
 * reversed that call: worktrees are to run side by side. So the port is now DERIVED from the
 * renderer root ({@link devServerPort}): same worktree, same port; different worktrees, all but
 * certainly different ports.
 *
 * The discriminator for identity is still the renderer ROOT the server was configured with — an
 * absolute path, so two worktrees can never serve the same one. `vite.config.ts` publishes it on
 * {@link DEV_IDENTITY_PATH} from THIS module's helpers and binds the port from THIS module's
 * function, so there is ONE definition of both. This module is kept out of `dev.cjs` so it can be
 * exercised against real HTTP servers without launching Electron (`dev-server-identity.test.ts`).
 *
 * Plain CJS with no third-party dependency: `dev.cjs` is a `require`-only node script, and
 * `vite.config.ts` pulls the same helpers through `createRequire`.
 */
"use strict";
const { execFileSync } = require("node:child_process");
const { createHash } = require("node:crypto");
const http = require("node:http");

/**
 * THE DERIVED-PORT WINDOW — `[DEV_PORT_MIN, DEV_PORT_MAX]` = 14200–14711.
 *
 * {@link DEV_PORT_SLOTS} slots of {@link DEV_PORT_STRIDE} consecutive ports. A worktree takes the
 * EVEN port of its slot for the dev server and the odd one for HMR, so no worktree's HMR port can
 * ever land on another worktree's dev-server port — the collision that a naive `port + 1` would
 * reintroduce through the back door.
 *
 * Why this window and not another:
 *   * ABOVE 1024 — nothing privileged to bind.
 *   * BELOW 49152 — the Windows dynamic range, where Hyper-V/WSL/Docker carve reserved blocks at
 *     boot and a bind dies on `EACCES` that no retry fixes. `vitest.config.ts` pins its browser API
 *     to 5310 for exactly that reason; same reasoning, same conclusion here.
 *   * CLEAR OF EVERY PORT THIS REPO ALREADY USES: 5310 (vitest browser API), 5173/4173 (Vite's own
 *     dev/preview defaults), 8766 (`nyx_core::mcp` default), 8779 (the e2e MCP port).
 *   * 142xx keeps the retired 1420 legible in the new numbers, for anyone reading an old log.
 */
const DEV_PORT_BASE = 14200;
/** How many worktrees can be told apart before two of them hash to one slot. */
const DEV_PORT_SLOTS = 256;
/** Ports reserved per worktree: the dev server, then its HMR socket. */
const DEV_PORT_STRIDE = 2;
/** Lowest port the derivation can return. */
const DEV_PORT_MIN = DEV_PORT_BASE;
/** Highest port the derivation can hand out, HMR included. */
const DEV_PORT_MAX = DEV_PORT_BASE + DEV_PORT_SLOTS * DEV_PORT_STRIDE - 1;

/**
 * The dev-server port THIS worktree owns — a pure function of its renderer root, within
 * `[DEV_PORT_MIN, DEV_PORT_MAX]`. `vite.config.ts` binds it with `strictPort: true` and `dev.cjs`
 * waits on it, both calling THIS function, so the launcher can never wait on a port Vite did not
 * bind.
 *
 * THE HASH IS TAKEN OVER {@link normalizeRoot}'s OUTPUT, not the raw string, and that is the whole
 * point: `dev.cjs` resolves the root through `path.resolve` (`D:\…\renderer`) while `vite.config.ts`
 * reads it from its own module URL (`d:/…/renderer`), and on Windows even the drive-letter case
 * differs between the two. Hashing the raw form would give one worktree two ports depending on who
 * asked, and the launcher would poll a port nobody bound.
 *
 * Collisions are possible — {@link DEV_PORT_SLOTS} slots, not infinitely many — and they are NOT
 * silent: `strictPort: true` makes the second worktree's Vite die on the taken port, and the
 * identity probe below names what holds it.
 *
 * @param {string} rendererRoot absolute path of the renderer root this dev server serves
 * @returns {number}
 */
function devServerPort(rendererRoot) {
  const digest = createHash("sha256").update(normalizeRoot(rendererRoot)).digest();
  return DEV_PORT_BASE + (digest.readUInt32BE(0) % DEV_PORT_SLOTS) * DEV_PORT_STRIDE;
}

/**
 * The HMR port that goes with {@link devServerPort} — the odd half of this worktree's slot.
 *
 * Only the explicit remote-HMR branch of `vite.config.ts` (`NYX_DEV_HOST`) needs it; the normal
 * local run leaves `hmr` undefined and lets Vite derive its own from the server port. It used to be
 * a hard-coded 1421 there, which would have put every worktree's remote HMR socket back on one port
 * after the dev-server ports were separated.
 *
 * @param {string} rendererRoot absolute path of the renderer root this dev server serves
 * @returns {number}
 */
function devHmrPort(rendererRoot) {
  return devServerPort(rendererRoot) + 1;
}

/**
 * The dev-server URL `dev.cjs` waits on and hands to Electron as `NYX_DEV_SERVER_URL` — unless the
 * environment already sets that variable, which still wins (a hand-started server, a tunnel).
 *
 * @param {string} rendererRoot absolute path of the renderer root this dev server serves
 * @returns {string}
 */
function devServerUrl(rendererRoot) {
  return `http://localhost:${devServerPort(rendererRoot)}`;
}

/**
 * The route `vite.config.ts` answers the identity probe on. Under a `__`-prefix so it can never
 * collide with an app route, and outside `/@` so Vite's own internal middlewares ignore it.
 */
const DEV_IDENTITY_PATH = "/__nyx_dev_identity";

/**
 * What the payload claims to be. A server that does not say this is not our dev server, whatever
 * else it is — an unrelated app that happened to take the port, a stale `vite preview`, a proxy.
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

/**
 * The OS process holding `port`, or `null` when nothing could be established.
 *
 * BEST EFFORT, NEVER THROWS. This is a diagnostic on an abort path: a lookup that fails must make
 * the message say less, never make the launch die differently.
 *
 * Windows goes through `netstat -ano` rather than `Get-NetTCPConnection` — no PowerShell start-up
 * cost, and it is present on every host. The STATE column ("LISTENING") is translated on a
 * localized Windows, so it is deliberately not matched on; the addresses are not translated, so a
 * wildcard foreign address is what identifies a listening row. A row for an established connection
 * to the same port reports the same owning pid anyway, hence the fallback.
 *
 * @param {number} port
 * @returns {number|null}
 */
function findPortHolderPid(port) {
  const options = {
    encoding: "utf8",
    timeout: 5_000,
    windowsHide: true,
    maxBuffer: 16 * 1024 * 1024,
    stdio: ["ignore", "pipe", "ignore"],
  };
  try {
    if (process.platform !== "win32") {
      // `-t` prints bare pids, one per line; `-sTCP:LISTEN` keeps clients off the list.
      const first = execFileSync("lsof", ["-nP", `-iTCP:${port}`, "-sTCP:LISTEN", "-t"], options)
        .split(/\r?\n/)
        .find((line) => line.trim().length > 0);
      const pid = Number(first);
      return Number.isInteger(pid) && pid > 0 ? pid : null;
    }
    const listening = [];
    const anyRow = [];
    for (const line of execFileSync("netstat", ["-a", "-n", "-o"], options).split(/\r?\n/)) {
      const parts = line.trim().split(/\s+/);
      if (parts.length < 5 || parts[0].toUpperCase() !== "TCP") continue;
      if (!parts[1].endsWith(`:${port}`)) continue;
      const pid = Number(parts[parts.length - 1]);
      if (!Number.isInteger(pid) || pid <= 0) continue;
      if (parts[2] === "0.0.0.0:0" || parts[2] === "[::]:0") listening.push(pid);
      else anyRow.push(pid);
    }
    return listening[0] ?? anyRow[0] ?? null;
  } catch {
    // The tool is missing (`lsof` is not installed everywhere) or it exited non-zero because
    // nothing matched. Either way there is no pid to report.
    return null;
  }
}

/** The copy-pasteable command that kills `pid` AND its descendants on this platform. */
function killTreeCommand(pid) {
  return process.platform === "win32" ? `taskkill /PID ${pid} /T /F` : `kill -9 ${pid}`;
}

/** The copy-pasteable command that shows who is listening on `port` on this platform. */
function portLookupCommand(port) {
  return process.platform === "win32"
    ? `Get-NetTCPConnection -LocalPort ${port} -State Listen`
    : `lsof -nP -iTCP:${port} -sTCP:LISTEN`;
}

/**
 * One line naming WHO holds the port behind `url`, and how to free it — or `null` when there is
 * nothing local to look up (a remote `NYX_DEV_SERVER_URL`, an unparseable URL).
 *
 * The gap it closes: the FOREIGN abort said WHICH renderer root the squatter served and left the
 * reader to find the process themselves. The squatter is very often a dev server this same script
 * failed to reap on its way out, so "which pid, and the command to kill it" is the whole remedy.
 *
 * @param {string} url the dev-server base URL that answered FOREIGN
 * @returns {string|null}
 */
function describePortHolder(url) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  const host = parsed.hostname.toLowerCase();
  if (host !== "localhost" && host !== "127.0.0.1" && host !== "::1" && host !== "[::1]") return null;
  const port = Number(parsed.port || (parsed.protocol === "https:" ? 443 : 80));
  const pid = findPortHolderPid(port);
  return pid === null
    ? `nothing could be read about the process holding port ${port} — look it up with: ${portLookupCommand(port)}`
    : `the process holding port ${port} is PID ${pid} — free it with: ${killTreeCommand(pid)}`;
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
 * @param {(decision: { action: string, verdict: string, message: string }) => void} done
 *   `verdict` is the probe's own verdict, carried through so the caller can add the "who holds the
 *   port" line ([`describePortHolder`]) on FOREIGN alone — on the other two it would be noise.
 */
function decideOnViteExit(url, expectedRoot, exitCode, done) {
  const died = `the Vite dev server exited (code ${exitCode})`;
  probeDevServerIdentity(url, expectedRoot, (result) => {
    if (result.verdict === IDENTITY.OURS) {
      return done({
        action: VITE_EXIT.ATTACH,
        verdict: result.verdict,
        message: `${died}, but ${url} already answers as OURS (${result.detail}) — attaching to the dev server that is already running`,
      });
    }
    if (result.verdict === IDENTITY.FOREIGN) {
      return done({
        action: VITE_EXIT.ABORT,
        verdict: result.verdict,
        message: `${died} and ${url} is held by another dev server: ${result.detail}`,
      });
    }
    return done({
      action: VITE_EXIT.ABORT,
      verdict: result.verdict,
      message: `${died} and NOTHING answers on ${url} — the port is free, so this is Vite itself failing to start (a broken config, a plugin error, a missing dependency). Read its output above`,
    });
  });
}

module.exports = {
  DEV_PORT_BASE,
  DEV_PORT_SLOTS,
  DEV_PORT_STRIDE,
  DEV_PORT_MIN,
  DEV_PORT_MAX,
  DEV_IDENTITY_PATH,
  DEV_IDENTITY_APP,
  IDENTITY,
  VITE_EXIT,
  devServerPort,
  devHmrPort,
  devServerUrl,
  normalizeRoot,
  identityPayload,
  classifyIdentityResponse,
  probeDevServerIdentity,
  findPortHolderPid,
  killTreeCommand,
  portLookupCommand,
  describePortHolder,
  decideOnViteExit,
};
