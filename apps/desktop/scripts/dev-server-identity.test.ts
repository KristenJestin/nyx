/**
 * The dev-server IDENTITY check (`scripts/dev-server-identity.cjs`), exercised against real HTTP
 * servers — no Electron, no Vite, no port 1420.
 *
 * The gap these close: `dev.cjs` waited with `http.get(url, () => startElectron())`, which
 * establishes that SOMETHING answers on the port. Every nyx worktree serves on the same fixed
 * 1420, so three launches ran this worktree's backend against another worktree's renderer without
 * a single line of output saying so. The check has to separate three cases — ours, foreign,
 * not-up-yet — and each one is driven here by a server that actually answers that way.
 *
 * Run via `bun test` (the `smoke:dev-identity` npm script).
 */
import { afterEach, describe, expect, test } from "bun:test";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";

const {
  DEV_IDENTITY_PATH,
  IDENTITY,
  VITE_EXIT,
  classifyIdentityResponse,
  decideOnViteExit,
  identityPayload,
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
    // The `app` field carries its own weight: a JSON endpoint on 1420 answering `{ root: … }` with
    // the right path is not our dev server, and without this check it would be accepted as OURS.
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
