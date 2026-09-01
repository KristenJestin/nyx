#!/usr/bin/env node
/**
 * Dev orchestrator for `nyx-v2` (cross-platform, no shell-specific syntax).
 *
 * 1. Starts the frontend Vite dev server (`nyx-v2-renderer dev`) on the port DERIVED from THIS
 *    worktree's renderer root (scripts/dev-server-identity.cjs), so several worktrees can run side
 *    by side instead of fighting over one fixed 1420. Its non-zero exit is HANDLED rather than
 *    ignored (it used to be the one failure this script swallowed) — and handled by ASKING the
 *    identity probe of step 2 who holds the port, never by assuming. `strictPort: true` makes Vite
 *    die when the derived port is taken, and the server holding it is very often our OWN
 *    (`bun run dev` in one shell, `bun run dev:desktop` in another — same worktree, so the same
 *    derived port): that case ATTACHES. Anything else aborts, printing the exit code, what the
 *    probe measured, and which pid to kill.
 * 2. Waits for it to answer, and checks WHOSE server answered (scripts/dev-server-identity.cjs).
 *    A presence on the port is not an appartenance: three launches ran this worktree's backend
 *    against another worktree's UI before this check existed. Deriving the port makes that
 *    collision rare, not impossible — a stale server of ours, a proxy, an unrelated app — so the
 *    check stays.
 * 3. BUNDLES the Electron main/preload once with esbuild (scripts/bundle.cjs)
 *    — NOT a bare `tsc`. A `sandbox: true` preload runs in a restricted context whose
 *    `require` cannot resolve a sibling `../shared/ipc.js`; bare `tsc` would leave that
 *    bare `require` in `dist/preload/index.js`, the preload would die on load
 *    ('Unable to load preload script'), `window.nyxCore` would never be installed, and
 *    the front would fall back to the Tauri adapter and crash. Bundling inlines the
 *    shared allowlist so dev is sandbox-safe and IDENTICAL to prod (scripts/bundle.cjs).
 * 4. Runs a SEPARATE, non-blocking typecheck (`tsc --noEmit`) — types are checked but a
 *    type error never blocks the dev launch (the bundle is the build of record).
 * 5. Launches Electron with `NYX_DEV_SERVER_URL` pointing at the live Vite server
 *    so the main process loads the dev server instead of an on-disk build.
 * 6. On the way out — an abort, a Ctrl+C, or Electron quitting — reaps each child's whole PROCESS
 *    TREE, not just the child ([`killTree`]). `child.kill()` alone was measured to leave the dev
 *    server alive: on Windows `shell: true` puts a `cmd.exe` between us and `bun`, so killing the
 *    child killed the shell and left a `node` listening on the port — which the NEXT worktree's
 *    launch then met as a "FOREIGN server", created by this very script.
 *
 * Kept as a plain CJS node script (not a shell one-liner) so `bun run --filter
 * nyx-v2 dev` behaves identically on Windows and Linux.
 */
"use strict";
const { execFileSync, spawn } = require("node:child_process");
const path = require("node:path");
const {
  IDENTITY,
  VITE_EXIT,
  decideOnViteExit,
  describePortHolder,
  devServerUrl,
  probeDevServerIdentity,
} = require("./dev-server-identity.cjs");

const repoRoot = path.resolve(__dirname, "..", "..", "..");
const electronDir = path.resolve(__dirname, "..");
// The renderer root THIS worktree serves — the identity the dev server must claim before we point
// Electron at it, AND the input its port is derived from. An absolute path: no two worktrees can
// produce the same one.
const rendererRoot = path.resolve(electronDir, "renderer");

// `NYX_DEV_SERVER_URL` still overrides everything (a hand-started server, a tunnel). Otherwise the
// URL carries THIS worktree's derived port — the same one `vite.config.ts` binds, because both
// sides call the same function on the same directory.
const DEV_URL = process.env.NYX_DEV_SERVER_URL || devServerUrl(rendererRoot);

/**
 * How long a POSIX process group gets between the SIGTERM and the SIGKILL. Long enough for Vite to
 * close its socket, short enough that a Ctrl+C still feels immediate. Windows needs none: `taskkill
 * /F` is not negotiable and returns synchronously.
 */
const KILL_GRACE_MS = 300;

const children = [];

/**
 * Kill a child's WHOLE PROCESS TREE. NEVER THROWS: reaping a tree that is already dead is a no-op
 * by contract, not an error to surface.
 *
 * This is the fix for a leak measured twice — `bun run dev:desktop`, Ctrl+C, and a `node` still
 * listening on the dev port afterwards. The same semantics the Rust runtime already uses for a
 * managed command (`crates/nyx-core/src/command.rs`, `KillHandle::kill_tree`), reached
 * independently here because `dev.cjs` must stay a dependency-free node script.
 *
 * WINDOWS: `taskkill /PID <pid> /T /F`. `shell: true` (see [`run`]) makes the direct child a
 * `cmd.exe`; terminating it leaves the `bun` it launched and the `node` that `bun` launched holding
 * the port. `/T` takes the descendants with it, `/F` forces them.
 *
 * POSIX: the child leads its own process group (`detached`, see [`run`]), so its pid IS its pgid
 * and the NEGATIVE pgid signals every process in the group — starting with SIGTERM so the tree can
 * close its sockets; [`hardKillTree`] follows up for whatever ignored it.
 */
function killTree(child) {
  const pid = child && child.pid;
  if (!pid) return; // never spawned (ENOENT) — nothing to reap
  if (process.platform === "win32") {
    try {
      execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], {
        stdio: "ignore",
        timeout: 5_000,
        windowsHide: true,
      });
    } catch {
      // Non-zero exit = "process not found": the tree is already gone.
    }
    return;
  }
  try {
    process.kill(-pid, "SIGTERM");
  } catch {
    // ESRCH: the group is already gone.
  }
}

/** POSIX escalation after {@link KILL_GRACE_MS}. A no-op on Windows, where `/F` already forced it. */
function hardKillTree(child) {
  const pid = child && child.pid;
  if (!pid || process.platform === "win32") return;
  try {
    process.kill(-pid, "SIGKILL");
  } catch {
    // ESRCH: the group is already gone.
  }
}

/**
 * IDEMPOTENT, and it has to be: Electron's `exit`, a Ctrl+C and an abort verdict can all land
 * within the same tick, and a second pass over an already-reaped list would `taskkill` pids the OS
 * may have handed to someone else. The list is drained (`splice`) as it is reaped, so a re-entrant
 * call finds nothing and the flag makes it return before it can exit twice with two codes.
 */
let shuttingDown = false;
function shutdown(code) {
  if (shuttingDown) return;
  shuttingDown = true;
  const doomed = children.splice(0);
  if (process.platform === "win32") {
    // ONE `taskkill` for the whole list, not one per child. [`killTree`] is SYNCHRONOUS with a
    // five-second ceiling, and this runs inside the SIGINT/SIGTERM handler: called in a loop, a
    // `taskkill` slow to answer — a process in an uninterruptible wait, an antivirus filter driver
    // on the handle — stalls the event loop once per child, so Ctrl+C could freeze the shell for
    // four times the ceiling with nothing printed. `taskkill` takes repeated `/PID`, so the worst
    // case collapses to one ceiling for the whole tree.
    const pids = doomed.map((c) => c && c.pid).filter(Boolean);
    if (pids.length > 0) {
      try {
        execFileSync(
          "taskkill",
          [...pids.flatMap((pid) => ["/PID", String(pid)]), "/T", "/F"],
          { stdio: "ignore", timeout: 5_000, windowsHide: true },
        );
      } catch {
        // Non-zero exit = at least one pid was already gone; the rest were still killed.
      }
    }
    return process.exit(code);
  }
  for (const c of doomed) killTree(c);
  // POSIX only past here — the Windows branch returned above. SIGTERM went out, so give the groups
  // a moment to die on their own before the
  // SIGKILL. The timer is deliberately NOT unref'd — an unref'd one lets the loop drain and node
  // exits 0, silently dropping `code`.
  setTimeout(() => {
    for (const c of doomed) hardKillTree(c);
    process.exit(code);
  }, KILL_GRACE_MS);
}
process.on("SIGINT", () => shutdown(0));
process.on("SIGTERM", () => shutdown(0));

function run(cmd, args, opts) {
  const c = spawn(cmd, args, {
    // No stdin for the children. It costs nothing (none of them is interactive — Vite's keyboard
    // shortcuts are the only user, and the dev shell is driven from the app) and on POSIX it is
    // what makes `detached` safe: a background process group that READS the controlling terminal
    // takes a SIGTTIN and stops dead.
    stdio: ["ignore", "inherit", "inherit"],
    // Windows: `bun`/`cargo` may be `.cmd` shims, which node refuses to exec directly. The price is
    // a `cmd.exe` between us and the real process — which is exactly why [`killTree`] uses
    // `taskkill /T` rather than `child.kill()`.
    shell: process.platform === "win32",
    // POSIX: give the child its own process GROUP (`setsid`), so [`killTree`] has a group to signal
    // and reaps the grandchildren too. Without it `child.kill()` reaches the shell alone.
    detached: process.platform !== "win32",
    ...opts,
  });
  children.push(c);
  return c;
}

// 1. Vite dev server (filtered through the monorepo runner). A non-zero exit is not, on its own, a
// reason to abort: `strictPort: true` makes Vite die when THIS worktree's derived port is taken,
// and the server holding it may be OURS — the `bun run dev` this developer left running in another
// shell, which derives the same port from the same renderer root. So the exit is a QUESTION for the
// probe of step 2, not a verdict ([`decideOnViteExit`]).
function startVite() {
  const vite = run("bun", ["run", "--filter", "nyx-v2-renderer", "dev"], { cwd: repoRoot });
  vite.on("exit", (code) => {
    if (code === 0) return;
    decideOnViteExit(DEV_URL, rendererRoot, code, ({ action, verdict, message }) => {
      if (action === VITE_EXIT.ATTACH) {
        // The poll below is already asking the same server the same question, and its OURS branch
        // is what launches Electron. Nothing to do here but say what happened — and above all NOT
        // to shut down: an abort at this instant kills `bundle.cjs` mid-write and leaves a
        // truncated `dist/main` / `dist/preload` behind.
        return console.error(`[dev] ${message}`);
      }
      if (launching) {
        // …AND THE SAME PROTECTION ON THE ABORT VERDICTS, because the race is not only the ATTACH
        // one. The poll and this handler probe the SAME server a few hundred milliseconds apart:
        // the poll can answer OURS and start bundling while this probe times out
        // (`PROBE_TIMEOUT_MS`) on a loaded machine and comes back UNREACHABLE. Shutting down then
        // kills `bundle.cjs` mid-write — exactly the truncated `dist/main` / `dist/preload` the
        // ATTACH branch above exists to avoid. The launch has already been decided by a probe that
        // DID get an answer, so this one has nothing to add but a line.
        return console.error(`[dev] ${message} — the launch is already under way, not aborting it`);
      }
      console.error(`[dev] ${message} — aborting launch`);
      reportPortHolder(verdict);
      shutdown(code || 1);
    });
  });
}

/**
 * Name the process squatting on the port, on a FOREIGN verdict only.
 *
 * The abort used to say which renderer root the squatter served and stop there, leaving the reader
 * to find the process themselves — and the squatter is very often a dev server this script failed
 * to reap on a previous run, so the pid plus a ready-to-paste kill command IS the remedy. On the
 * other two verdicts there is nothing to name: OURS is not a problem and UNREACHABLE means the port
 * is free.
 */
function reportPortHolder(verdict) {
  if (verdict !== IDENTITY.FOREIGN) return;
  const holder = describePortHolder(DEV_URL);
  if (holder) console.error(`[dev] ${holder}`);
}

// 2. Poll the dev server until it responds AS OURS, then build main + launch Electron.
function waitForServer(url, attempts) {
  probeDevServerIdentity(url, rendererRoot, (result) => {
    if (result.verdict === IDENTITY.OURS) return startElectron();
    if (result.verdict === IDENTITY.FOREIGN) {
      // Never launch Electron on a server we did not recognise: this backend would drive that
      // UI, and nothing in the running app would say so.
      console.error(`[dev] a FOREIGN server is answering on ${url} — aborting launch`);
      console.error(`[dev] ${result.detail}`);
      reportPortHolder(result.verdict);
      return shutdown(1);
    }
    if (attempts <= 0) {
      console.error(`[dev] dev server ${url} never came up`);
      return shutdown(1);
    }
    setTimeout(() => waitForServer(url, attempts - 1), 300);
  });
}

/**
 * IDEMPOTENT. Two callers can now reach the same OURS verdict — the poll below and the Vite-exit
 * handler above, which race by design — and bundling twice into the same `dist/` is how a half-written
 * preload gets loaded.
 */
let launching = false;
function startElectron() {
  if (launching) return;
  launching = true;
  // BUNDLE main/preload once before launch — the SAME esbuild path prod
  // uses (scripts/bundle.cjs), so the preload is self-contained (shared/ipc inlined)
  // and sandbox-safe. A bare `tsc` would leave a `require("../shared/ipc.js")` the
  // sandboxed preload can't resolve, killing the bridge.
  const bundle = run(process.execPath, [path.join(__dirname, "bundle.cjs")], {
    cwd: electronDir,
  });
  bundle.on("exit", (code) => {
    if (code !== 0) {
      console.error("[dev] bundle failed — aborting launch");
      return shutdown(code || 1);
    }
    // BUILD + STAGE the nyx-sidecar binary (the runtime authority) before launch, so the
    // main process resolves it at `dist/sidecar/`. A build/stage failure aborts — the dev
    // shell has no runtime without it.
    const cargo = run("cargo", ["build", "-p", "nyx-sidecar"], { cwd: electronDir });
    cargo.on("exit", (cc) => {
      if (cc !== 0) {
        console.error("[dev] sidecar build failed — aborting launch");
        return shutdown(cc || 1);
      }
      const stage = run(process.execPath, [path.join(__dirname, "copy-sidecar.cjs")], {
        cwd: electronDir,
      });
      stage.on("exit", (sc) => {
        if (sc !== 0) {
          console.error("[dev] sidecar staging failed — aborting launch");
          return shutdown(sc || 1);
        }
        launchElectron();
      });
    });
  });
}

function launchElectron() {
  // Type-check in the background (non-blocking): types are validated but a type error
  // must NOT block bringing the dev shell up (the bundle is the build of record).
  const tsc = run(process.execPath, [require.resolve("typescript/bin/tsc"), "-p", "tsconfig.json", "--noEmit"], {
    cwd: electronDir,
  });
  tsc.on("exit", (c) => {
    if (c !== 0) console.error(`[dev] typecheck reported errors (exit ${c}) — dev shell left running`);
    else console.log("[dev] typecheck clean ✓");
  });

  const electronBin = require("electron");
  const el = run(electronBin, ["."], {
    cwd: electronDir,
    env: { ...process.env, NYX_DEV_SERVER_URL: DEV_URL },
  });
  el.on("exit", (c) => shutdown(c || 0));
}

/**
 * Only when RUN, never when REQUIRED. `dev-server-identity.test.ts` pulls [`run`], [`killTree`] and
 * [`shutdown`] in to drive a real grandchild tree and check it gets reaped; without this guard the
 * mere `require` would spawn Vite, cargo and Electron inside the test process.
 */
if (require.main === module) {
  startVite();
  waitForServer(DEV_URL, 100);
}

module.exports = { DEV_URL, children, hardKillTree, killTree, rendererRoot, run, shutdown };
