import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * THE GUARD-RAIL THAT BITES (finding `01KYVT6HVA260DTAEREQA4DFJN`).
 *
 * The same defect was found TWICE, at two places, by two coders: a `String(e)` in the node-params
 * panel, and an `e instanceof Error` test in `useDesignRender` while the bridge rejects with a
 * `BridgeError` OBJECT — so every refusal of `design_render` reached the stage as `[object Object]`.
 * Each was fixed WHERE IT WAS SEEN. Nobody looked anywhere else, and the pattern was in a dozen more
 * files.
 *
 * A third fix in a third place would have been worth nothing. What is worth something is a test that
 * FAILS the day a fourth appears — so this one reads the renderer's own source and refuses the
 * patterns that produce `[object Object]`, wherever they are. It is a lint the suite carries,
 * because the rule it enforces is a product rule: A GESTURE MUST NEVER LIE, and a backend refusal
 * rendered as `[object Object]` is a lie by negligence.
 *
 * MUTATION-CHECKED: re-introduce `String(e)` or `e instanceof Error` in any surface and this test
 * names the file and the line. Empty the `catch` around a bridge call and it names that too.
 */

const SRC = resolve(__dirname, "..");

/** The ONE module allowed to know how a failure is read — everything else calls `errorText`. */
const THE_ONE_PATH = "bridge/error-text.ts";

/** Every `.ts`/`.tsx` of the renderer that is PRODUCT code (tests may construct errors freely). */
function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      sourceFiles(path, out);
      continue;
    }
    if (!/\.tsx?$/.test(entry.name)) continue;
    if (/\.(test|spec)\.tsx?$/.test(entry.name)) continue;
    if (entry.name.endsWith(".d.ts")) continue;
    out.push(path);
  }
  return out;
}

/** `file:line` for every line of `files` matching `pattern`, ignoring the ones inside a comment. */
function offenders(pattern: RegExp, files: readonly string[]): string[] {
  const hits: string[] = [];
  for (const file of files) {
    const rel = relative(SRC, file);
    if (rel === THE_ONE_PATH) continue;
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((line, i) => {
      // A COMMENT may name the defect — this file, and a dozen others, explain it at length.
      const code = line.replace(/\/\/.*$/, "").replace(/^\s*\*.*$/, "");
      if (pattern.test(code)) hits.push(`${rel}:${i + 1}: ${line.trim()}`);
    });
  }
  return hits;
}

const FILES = sourceFiles(SRC);

describe("every reading of a failure goes through errorText", () => {
  /** There IS product source to scan — a broken walk must not pass by finding nothing. */
  it("scans the renderer's own sources", () => {
    expect(FILES.length).toBeGreaterThan(100);
    expect(FILES.map((f) => relative(SRC, f))).toContain(THE_ONE_PATH);
  });

  /**
   * `String(e)` on a caught value IS the defect: the bridge rejects with an OBJECT, and `String` of
   * an object is `[object Object]`. There is no case where it is the right call — a value that
   * genuinely is a string is returned by `errorText` unchanged.
   */
  it("no surface stringifies a caught value", () => {
    expect(offenders(/\bString\(\s*(e|err|error|ex|cause|reason|failure)\s*\)/, FILES)).toEqual([]);
  });

  /**
   * `e instanceof Error` is the SECOND form of the same defect: it is FALSE for a `BridgeError`, so
   * the branch that reads the backend's sentence is never taken. `errorText` tests the bridge shape
   * FIRST, which is the whole point of there being one function.
   */
  it("no surface branches on `instanceof Error` to read a message", () => {
    expect(offenders(/\binstanceof Error\b/, FILES)).toEqual([]);
  });

  /** `(err as Error).message` is the same bet in a cast — a `BridgeError` is not an `Error`. */
  it("no surface casts a caught value to Error", () => {
    expect(offenders(/\bas Error\b/, FILES)).toEqual([]);
  });

  /**
   * AND THE SWALLOWED REFUSAL, which is worse than `[object Object]` — it leaves no trace at all.
   * A `catch` with an EMPTY body (no statement, no comment) around a block that calls the bridge is
   * a backend refusal disappearing on screen. A best-effort catch that documents WHY it ignores the
   * failure (a layout fit, a teardown race) is a decision, and it stays legible as one.
   */
  it("no bridge call is wrapped in a catch that swallows the refusal in silence", () => {
    const hits: string[] = [];
    for (const file of FILES) {
      const text = readFileSync(file, "utf8");
      const rel = relative(SRC, file);
      for (const match of text.matchAll(/\btry\s*\{/g)) {
        const start = match.index ?? 0;
        const region = text.slice(start, start + 4000);
        const catchAt = region.search(/\}\s*catch\s*(\([^)]*\)\s*)?\{/);
        if (catchAt === -1) continue;
        const tryBody = region.slice(0, catchAt);
        if (!/nyxBridge|\.invoke\(/.test(tryBody)) continue;
        const afterCatch = region.slice(catchAt).replace(/^\}\s*catch\s*(\([^)]*\)\s*)?\{/, "");
        const body = afterCatch.slice(0, afterCatch.indexOf("}"));
        if (body.trim().length === 0) {
          hits.push(`${rel}: a bridge call caught into an empty block near offset ${start}`);
        }
      }
    }
    expect(hits).toEqual([]);
  });
});
