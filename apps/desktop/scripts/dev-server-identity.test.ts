/**
 * THE TWO GUARDS THE DEV LAUNCHER RUNS ON, exercised against real HTTP servers and real process
 * trees — no Electron, no Vite, and never on the port a worktree actually uses.
 *
 *   * WHICH PORT IS OURS and WHOSE SERVER ANSWERS (`scripts/dev-server-identity.cjs`). `dev.cjs`
 *     waited with `http.get(url, () => startElectron())`, which establishes only that SOMETHING
 *     answers; every worktree served on the same fixed 1420, so three launches ran this worktree's
 *     backend against another worktree's renderer without a line of output saying so. The port is
 *     now derived per worktree, which makes that collision rare rather than impossible — so both
 *     halves are checked: the derivation (stable, distinct, in range) and the three verdicts (ours,
 *     foreign, not-up-yet), each driven by a server that really answers that way.
 *   * NOT BECOMING THE SQUATTER ONE'S SELF (`scripts/dev.cjs`). Killing the direct child left the
 *     dev server alive — on Windows `shell: true` interposes a `cmd.exe` — so a stopped run kept
 *     holding the port and the next worktree met a FOREIGN server this script had created. The
 *     tree kill is driven here against a real grandchild.
 *
 * Run via `bun test` (the `smoke:dev-identity` npm script).
 */
import { afterEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";

/**
 * The launcher itself, for its process-tree kill. Requiring it spawns NOTHING: `dev.cjs` guards its
 * launch behind `require.main === module` precisely so this file can drive `run`/`killTree`/
 * `shutdown` without bringing up Vite, cargo and Electron inside the test process.
 */
const devScript = require("./dev.cjs");

const {
  DEV_IDENTITY_PATH,
  DEV_PORT_MAX,
  DEV_PORT_MIN,
  DEV_PORT_STRIDE,
  IDENTITY,
  VITE_EXIT,
  classifyIdentityResponse,
  decideOnViteExit,
  devHmrPort,
  devServerPort,
  devServerUrl,
  describePortHolder,
  findPortHolderPid,
  identityPayload,
  killTreeCommand,
  normalizeRoot,
  probeDevServerIdentity,
} = require("./dev-server-identity.cjs");

/** The root this "worktree" claims, and a different worktree's root on the same machine. */
const OUR_ROOT = "/repos/nyx-diagnostics/apps/desktop/renderer";
const OTHER_ROOT = "/repos/nyx-other-branch/apps/desktop/renderer";

const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(
    servers.splice(0).map((server) => {
      // A server that never answered still holds its socket; without this the close below waits on
      // it and the suite hangs where the probe under test has already given up.
      server.closeAllConnections?.();
      return new Promise<void>((done) => server.close(() => done()));
    }),
  );
});

/** Start a throwaway HTTP server on an EPHEMERAL port and return its base URL. */
async function serve(handler: (path: string) => { status: number; body: string }): Promise<string> {
  const server = createServer((req, res) => {
    const { status, body } = handler(req.url ?? "");
    res.statusCode = status;
    res.setHeader("content-type", "application/json");
    res.end(body);
  });
  servers.push(server);
  await new Promise<void>((ready) => server.listen(0, "127.0.0.1", () => ready()));
  return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
}

/**
 * A server that ACCEPTS the connection and never answers — the case a status/JSON check cannot see
 * and only the request timeout can. Without it `dev.cjs` polls such a port forever with no output.
 */
async function serveSilently(): Promise<string> {
  const server = createServer(() => {
    /* deliberately no response */
  });
  servers.push(server);
  await new Promise<void>((ready) => server.listen(0, "127.0.0.1", () => ready()));
  return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
}

/** A server that answers the identity route the way our `vite.config.ts` does, for `root`. */
function nyxDevServer(root: string) {
  return (path: string) =>
    path.startsWith(DEV_IDENTITY_PATH)
      ? { status: 200, body: JSON.stringify(identityPayload(root)) }
      : { status: 200, body: "<html>the app</html>" };
}

/** Probe `url` and resolve with the verdict. */
function probe(url: string, expectedRoot: string): Promise<{ verdict: string; detail: string }> {
  return new Promise((done) => probeDevServerIdentity(url, expectedRoot, done));
}

describe("the dev server we would launch Electron against", () => {
  test("is accepted when it serves this worktree's renderer root", async () => {
    const url = await serve(nyxDevServer(OUR_ROOT));
    expect((await probe(url, OUR_ROOT)).verdict).toBe(IDENTITY.OURS);
  });

  test("is refused when another worktree's dev server holds the port", async () => {
    const url = await serve(nyxDevServer(OTHER_ROOT));
    const result = await probe(url, OUR_ROOT);
    expect(result.verdict).toBe(IDENTITY.FOREIGN);
    // The message has to name the two roots, or the reader is told "foreign" and left to guess.
    expect(result.detail).toContain(normalizeRoot(OTHER_ROOT));
    expect(result.detail).toContain(normalizeRoot(OUR_ROOT));
  });

  test("is refused when the responder is not a nyx dev server at all", async () => {
    const url = await serve(() => ({
      status: 200,
      body: JSON.stringify({ app: "something-else" }),
    }));
    expect((await probe(url, OUR_ROOT)).verdict).toBe(IDENTITY.FOREIGN);
  });

  test("is refused when the responder serves HTML on every route, identity route included", async () => {
    // A dev server from a checkout that predates this middleware, or any SPA on the port: it
    // answers 200 with a page. That is not an identity, so it is foreign.
    const url = await serve(() => ({
      status: 200,
      body: "<!doctype html><title>some app</title>",
    }));
    expect((await probe(url, OUR_ROOT)).verdict).toBe(IDENTITY.FOREIGN);
  });

  test("is refused when the identity route does not exist", async () => {
    const url = await serve(() => ({ status: 404, body: "not found" }));
    expect((await probe(url, OUR_ROOT)).verdict).toBe(IDENTITY.FOREIGN);
  });

  test("is refused on a non-200, even when the body IS a well-formed identity", async () => {
    // The test above passes for the wrong reason: `"not found"` breaks `JSON.parse`, so the STATUS
    // check can be deleted and it stays green. A 404 carrying a perfectly good payload is what
    // actually exercises it — an error page from a proxy or a framework that happens to echo JSON.
    const url = await serve(() => ({
      status: 404,
      body: JSON.stringify(identityPayload(OUR_ROOT)),
    }));
    const result = await probe(url, OUR_ROOT);
    expect(result.verdict).toBe(IDENTITY.FOREIGN);
    expect(result.detail).toContain("404");
  });

  test("is refused when the payload names our root but does not say WHAT it is", async () => {
    // The `app` field carries its own weight: a JSON endpoint that happens to hold our port and
    // answers `{ root: … }` with the right path is still not our dev server — and without this
    // check it would be accepted as OURS.
    const url = await serve(() => ({
      status: 200,
      body: JSON.stringify({ root: normalizeRoot(OUR_ROOT) }),
    }));
    expect((await probe(url, OUR_ROOT)).verdict).toBe(IDENTITY.FOREIGN);
  });

  test("is UNREACHABLE when nothing answers — the one verdict that keeps polling", async () => {
    // A port nobody listens on: Vite may simply not be up yet, which must NOT abort the launch.
    // NOT "only": a responder that never answers reaches the same verdict, by the timeout below —
    // deliberately, because "keep polling" is the right thing to do with a port that says nothing.
    const result = await probe("http://127.0.0.1:1", OUR_ROOT);
    expect(result.verdict).toBe(IDENTITY.UNREACHABLE);
  });

  test("gives up on a responder that accepts and never answers, within a bounded delay", async () => {
    // Without the request timeout there is no verdict at all: `dev.cjs` waits on this socket for as
    // long as the process lives, printing nothing. The bound is what turns a hang into an answer.
    const url = await serveSilently();
    const started = Date.now();
    const result = await probe(url, OUR_ROOT);
    expect(result.verdict).toBe(IDENTITY.UNREACHABLE);
    expect(Date.now() - started).toBeLessThan(6_000);
  });
});

/**
 * WHAT `dev.cjs` DOES WITH A DEAD VITE — the decision that used to be taken without measuring
 * anything. It printed "likely already held by another worktree's dev server" on EVERY non-zero
 * exit, and aborted; measured, the common case is that the port is held by this worktree's OWN
 * `bun run dev`, the probe says so in milliseconds, and the abort killed `bundle.cjs` mid-write.
 */
describe("a Vite dev server that exited non-zero", () => {
  const decide = (url: string, exitCode: number): Promise<{ action: string; message: string }> =>
    new Promise((done) => decideOnViteExit(url, OUR_ROOT, exitCode, done));

  test("ATTACHES when the port already answers as ours", async () => {
    const url = await serve(nyxDevServer(OUR_ROOT));
    const decision = await decide(url, 1);
    expect(decision.action).toBe(VITE_EXIT.ATTACH);
    expect(decision.message).toContain("1");
    expect(decision.message).not.toContain("another dev server");
  });

  test("aborts and names another dev server ONLY on a measured FOREIGN verdict", async () => {
    const url = await serve(nyxDevServer(OTHER_ROOT));
    const decision = await decide(url, 1);
    expect(decision.action).toBe(VITE_EXIT.ABORT);
    expect(decision.message).toContain("another dev server");
    expect(decision.message).toContain(normalizeRoot(OTHER_ROOT));
  });

  test("aborts on a free port WITHOUT blaming a worktree — that is Vite failing to start", async () => {
    const decision = await decide("http://127.0.0.1:1", 7);
    expect(decision.action).toBe(VITE_EXIT.ABORT);
    // The exit code, which is the one thing the reader needs and the old message dropped…
    expect(decision.message).toContain("7");
    // …and no story about a worktree, because nothing measured one.
    expect(decision.message).not.toContain("another dev server");
    expect(decision.message).toContain("NOTHING answers");
  });
});

describe("root comparison", () => {
  test("ignores separator style and, on Windows, the drive-letter case", () => {
    const windowsRoot = "D:\\Projects\\_worktrees\\nyx\\apps\\desktop\\renderer";
    const asVitePrintsIt = "d:/Projects/_worktrees/nyx/apps/desktop/renderer/";
    const verdict = classifyIdentityResponse(windowsRoot, {
      statusCode: 200,
      body: JSON.stringify(identityPayload(asVitePrintsIt)),
    });
    // Same directory, two spellings — on win32 this is one root; elsewhere it is genuinely two
    // (a case-sensitive filesystem), and the check must not pretend otherwise.
    expect(verdict.verdict).toBe(process.platform === "win32" ? IDENTITY.OURS : IDENTITY.FOREIGN);
  });

  test("still separates two roots that differ by more than case", () => {
    const verdict = classifyIdentityResponse(OUR_ROOT, {
      statusCode: 200,
      body: JSON.stringify(identityPayload(OTHER_ROOT)),
    });
    expect(verdict.verdict).toBe(IDENTITY.FOREIGN);
  });
});

/**
 * THE PORT A WORKTREE OWNS. It used to be 1420 for everyone, deliberately: "two branches will still
 * not run in parallel, but the second fails loudly instead of borrowing the first one's UI". The
 * user reversed that, so the port is derived — and a derivation is only useful if it is STABLE for
 * one worktree and DISTINCT between worktrees. Both are checked here, plus the window it promises.
 */
describe("the dev-server port derived from a renderer root", () => {
  /** The two worktrees from the report that collided on 1420, plus four more on the same disk. */
  const WORKTREES = [
    "d:/projects/_worktrees/nyx-system-scope/apps/desktop/renderer",
    "d:/projects/_worktrees/nyx-global-search/apps/desktop/renderer",
    "d:/projects/_worktrees/nyx-review/apps/desktop/renderer",
    "d:/projects/_worktrees/nyx-terminal/apps/desktop/renderer",
    "d:/projects/_worktrees/nyx-docs/apps/desktop/renderer",
    "d:/projects/nyx/nyx-v2/apps/desktop/renderer",
  ];

  test("separates the two worktrees that were reported fighting over one port", () => {
    const [systemScope, globalSearch] = WORKTREES;
    expect(devServerPort(systemScope)).not.toBe(devServerPort(globalSearch));
  });

  test("gives six sibling worktrees six ports", () => {
    expect(new Set(WORKTREES.map((root) => devServerPort(root))).size).toBe(WORKTREES.length);
  });

  test("gives ONE port to ONE worktree, however the path is spelled", () => {
    // THE INVARIANT THAT KEEPS THE LAUNCHER AND THE CONFIG ON THE SAME PORT. `dev.cjs` derives from
    // `path.resolve(<electron app>, "renderer")`; `vite.config.ts` derives from its own module URL.
    // Same directory, different spellings — and if the hash were taken over the raw string, one
    // worktree would get two ports and `dev.cjs` would poll a port Vite never bound.
    const asResolved = resolve(__dirname, "..", "renderer");
    const spellings = [
      asResolved,
      `${asResolved}${sep}`, // trailing separator
      asResolved.replace(/\\/g, "/"), // forward slashes, the way Vite prints a root
      resolve(asResolved, "src", ".."), // a detour through a child directory
    ];
    if (process.platform === "win32") {
      // The drive-letter case differs between what Node resolves and what Vite reports. Folded on
      // win32 only: elsewhere these really are two directories and must NOT collapse.
      spellings.push(asResolved.charAt(0).toLowerCase() + asResolved.slice(1));
      spellings.push(asResolved.toLowerCase().replace(/\\/g, "/"));
    }
    expect(new Set(spellings.map((root) => devServerPort(root))).size).toBe(1);
    // …and it is the port THIS worktree's launcher actually computed at require time.
    expect(devServerPort(asResolved)).toBe(devServerPort(devScript.rendererRoot));
  });

  test("lands inside the documented window, HMR included", () => {
    for (let i = 0; i < 400; i++) {
      const port = devServerPort(`/repos/worktree-${i}/apps/desktop/renderer`);
      expect(port).toBeGreaterThanOrEqual(DEV_PORT_MIN);
      expect(port).toBeLessThanOrEqual(DEV_PORT_MAX);
      expect(devHmrPort(`/repos/worktree-${i}/apps/desktop/renderer`)).toBe(port + 1);
      expect(port + 1).toBeLessThanOrEqual(DEV_PORT_MAX);
    }
  });

  test("never puts one worktree's HMR socket on another worktree's dev-server port", () => {
    // The back door a naive `port + 1` would leave open: with a stride of 1, the worktree in the
    // next slot serves where its neighbour speaks HMR.
    const roots = Array.from({ length: 400 }, (_, i) => `/repos/worktree-${i}/apps/desktop/renderer`);
    const servers = new Set(roots.map((root) => devServerPort(root)));
    for (const root of roots) expect(servers.has(devHmrPort(root))).toBe(false);
    expect(DEV_PORT_STRIDE).toBeGreaterThanOrEqual(2);
  });

  test("hands `dev.cjs` a URL carrying that very port", () => {
    const root = WORKTREES[0];
    expect(devServerUrl(root)).toBe(`http://localhost:${devServerPort(root)}`);
  });
});

/**
 * WHO HOLDS THE PORT — the line the FOREIGN abort was missing. It named the renderer root being
 * served and stopped there, so the reader knew a stranger held the port and not which process to
 * kill; the stranger is usually a dev server a previous run failed to reap.
 */
describe("naming the process on the port", () => {
  test("finds the pid of a server this very process is running", async () => {
    const url = await serve(nyxDevServer(OUR_ROOT));
    const port = Number(new URL(url).port);
    const pid = findPortHolderPid(port);
    if (process.platform === "win32") {
      // `netstat` ships with every Windows; there is no excuse for a miss here.
      expect(pid).toBe(process.pid);
    } else {
      // `lsof` is not installed everywhere. A miss is tolerated; a WRONG pid never is.
      expect(pid === null || pid === process.pid).toBe(true);
    }
  });

  test("says nothing about a port nobody holds — but still says how to look", () => {
    const described = describePortHolder("http://127.0.0.1:1");
    expect(described).toContain("port 1");
    expect(described).not.toContain("PID");
  });

  test("hands over a ready-to-paste kill command for the pid it found", async () => {
    const url = await serve(nyxDevServer(OUR_ROOT));
    const described = describePortHolder(url);
    const pid = findPortHolderPid(Number(new URL(url).port));
    if (pid === null) return; // no `lsof`; the miss is covered by the test above
    expect(described).toContain(`PID ${pid}`);
    expect(described).toContain(killTreeCommand(pid));
  });

  test("declines to guess about a server that is not on this machine", () => {
    // A remote `NYX_DEV_SERVER_URL` (a tunnel, a shared dev box): there is no local pid to name,
    // and inventing one would be worse than saying nothing.
    expect(describePortHolder("http://dev.example.test:14486")).toBeNull();
    expect(describePortHolder("not a url at all")).toBeNull();
  });
});

/**
 * NOT BECOMING THE SQUATTER. `shutdown()` killed its direct children, and on Windows `shell: true`
 * makes the direct child a `cmd.exe`: the `bun`/`node` underneath survived and kept the dev port,
 * so the next worktree's launch met a FOREIGN server that this script had itself left behind
 * (measured twice — a `node` still listening after the command was stopped).
 */
describe("shutting the dev launcher down", () => {
  /** Whether `pid` still exists. Signal 0 checks for presence without delivering anything. */
  function isAlive(pid: number): boolean {
    try {
      process.kill(pid, 0);
      return true;
    } catch {
      return false;
    }
  }

  async function waitUntil(predicate: () => boolean, limitMs: number): Promise<boolean> {
    const deadline = Date.now() + limitMs;
    while (Date.now() < deadline) {
      if (predicate()) return true;
      await new Promise((r) => setTimeout(r, 100));
    }
    return predicate();
  }

  const scratch: string[] = [];
  function workspace(): string {
    const dir = mkdtempSync(join(tmpdir(), "nyx-dev-treekill-"));
    scratch.push(dir);
    return dir;
  }

  afterEach(() => {
    for (const dir of scratch.splice(0)) rmSync(dir, { recursive: true, force: true });
  });

  test(
    "kills the GRANDCHILD, not just the child it spawned",
    async () => {
      const dir = workspace();
      const pidFile = join(dir, "grandchild.pid");
      const grandchild = join(dir, "grandchild.cjs");
      const parent = join(dir, "parent.cjs");
      // The grandchild stands in for the Vite dev server: it announces its pid and then holds on.
      writeFileSync(
        grandchild,
        `require("node:fs").writeFileSync(${JSON.stringify(pidFile)}, String(process.pid));\n` +
          `setInterval(() => {}, 1000);\n`,
      );
      // The parent stands in for `bun run --filter … dev` (and, on Windows, for everything below
      // the `cmd.exe` that `shell: true` interposes).
      writeFileSync(
        parent,
        `require("node:child_process").spawn(process.execPath, [${JSON.stringify(grandchild)}], { stdio: "ignore" });\n` +
          `setInterval(() => {}, 1000);\n`,
      );

      const child = devScript.run(process.execPath, [parent]);
      expect(await waitUntil(() => existsSync(pidFile), 20_000)).toBe(true);
      const grandPid = Number(readFileSync(pidFile, "utf8").trim());
      expect(Number.isInteger(grandPid)).toBe(true);
      expect(isAlive(grandPid)).toBe(true);

      try {
        devScript.killTree(child);
        devScript.hardKillTree(child);
        expect(await waitUntil(() => !isAlive(grandPid), 20_000)).toBe(true);
      } finally {
        // Never leave the stand-in dev server behind, whatever the assertion did.
        if (isAlive(grandPid)) {
          try {
            process.kill(grandPid, "SIGKILL");
          } catch {}
        }
      }
    },
    60_000,
  );

  test("is a no-op on a child that is already dead, and on one that never spawned", async () => {
    const dir = workspace();
    const done = join(dir, "exits.cjs");
    writeFileSync(done, "process.exit(0);\n");
    const child = devScript.run(process.execPath, [done]);
    await new Promise<void>((exited) => child.on("exit", () => exited()));
    // Twice, because `shutdown` can be reached twice in the same tick and the second pass must not
    // throw its way out of the loop before the remaining children are reaped.
    expect(() => {
      devScript.killTree(child);
      devScript.killTree(child);
      devScript.hardKillTree(child);
      devScript.killTree(null);
      devScript.killTree({ pid: undefined });
    }).not.toThrow();
  }, 30_000);

  test("exits ONCE, with the FIRST code, however many callers reach it", () => {
    // `shutdown` ends the process, so it is driven in a child: Electron's `exit`, a Ctrl+C and an
    // abort verdict can all land in the same tick, and a second pass would `taskkill` pids the OS
    // may already have handed to someone else.
    const dir = workspace();
    const probe = join(dir, "shutdown-probe.cjs");
    writeFileSync(
      probe,
      `const dev = require(${JSON.stringify(join(__dirname, "dev.cjs"))});\n` +
        `const seen = [];\n` +
        `process.exit = (code) => seen.push(code);\n` +
        `dev.shutdown(3);\n` +
        `dev.shutdown(9);\n` +
        // Long enough to outlast the POSIX SIGTERM→SIGKILL grace period, after which the (stubbed)
        // exit has been called for the last time it ever will be.
        `setTimeout(() => process.stdout.write(JSON.stringify(seen)), 1500);\n`,
    );
    const result = spawnSync(process.execPath, [probe], { encoding: "utf8", timeout: 30_000 });
    expect(result.stdout.trim()).toBe("[3]");
  }, 40_000);
});
