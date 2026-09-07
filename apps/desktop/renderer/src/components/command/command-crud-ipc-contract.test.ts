import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * THE WIRE CONTRACT OF THE COMMAND-TEMPLATE CRUD ROUTES, PINNED BETWEEN THE TWO HALVES.
 *
 * The sibling of `command-group-ipc-contract.test.ts`, on the other family of command routes: that
 * file pins `command_group_*` (who is in a folder), this one pins `command_create` /
 * `command_update` (what a command IS). The reason is identical and is worth restating rather than
 * cross-referenced away: both halves are STRINGS, written twice and compared by nothing. The
 * renderer emits `command_create { projectId, name, command, … }` from `use-commands.ts`; the
 * sidecar reads those exact keys by hand in `db_command.rs` (`arg_str("projectId")` …). A typo on
 * one side, or a rename on one side only, compiles, passes `tsc`, passes `cargo check`, and fails
 * at runtime as a field that silently never persists.
 *
 * WHAT MADE IT WORTH WRITING NOW: the NOTE FOR THE AGENT (`description`) is exactly the field that
 * failure mode would swallow in silence. A note is written once and read months later by an agent
 * in another session — nobody watches it save, and a `description` on one side against a `note` on
 * the other would look, from the app, like a user who forgot to type one.
 *
 * NOT AN EQUALITY, a CONTAINMENT — and that is the difference from the group file. The sidecar's
 * `command_create` also reads the five `source*` provenance keys that ONLY the package.json import
 * path sends. So the rule is: every key the renderer emits must be one the sidecar reads. A key the
 * sidecar reads and nobody sends is not a defect; a key the renderer sends and nobody reads is.
 */

const RENDERER = process.cwd();
const REPO = join(RENDERER, "..", "..", "..");
const DISPATCH = join(REPO, "crates", "nyx-sidecar", "src", "runtime", "db_command.rs");
const HOOK = join(RENDERER, "src", "components", "command", "use-commands.ts");

/** The CRUD routes this file pins — the two that carry an editable command surface. */
const ROUTES = ["command_create", "command_update"] as const;

const OPENERS = "{[(";
const CLOSERS = "}])";

/** The braced body that follows `from`, with its delimiters stripped. */
function bracedAfter(source: string, from: number): string {
  const open = source.indexOf("{", from);
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (OPENERS.includes(source[i])) depth += 1;
    else if (CLOSERS.includes(source[i])) {
      depth -= 1;
      if (depth === 0) return source.slice(open + 1, i);
    }
  }
  throw new Error(`unbalanced braces after index ${from}`);
}

/**
 * The top-level keys of an object literal body (`a: 1, b` → `["a", "b"]`).
 *
 * LINE COMMENTS ARE STRIPPED FIRST, which the group file's copy does not do because its literals
 * carry none. Here they do — the arguments that mean something subtle (a `null` that ERASES rather
 * than omits) are commented at the call site, where the reader needs it — and an unstripped `//`
 * glues itself to the key on the next line, which reads here as "the renderer stopped sending that
 * field". A parser that only recognises uncommented code would make commenting a call site a way to
 * turn this test green.
 */
function literalKeys(body: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let current = "";
  // `[^\n]` rather than `.` + `$`: the sources are checked out with CRLF on Windows, and JS's `.`
  // does not match a carriage return — an end-anchored strip silently did nothing there, which is
  // how this parser first read a commented key as absent.
  const uncommented = body.replace(/\/\/[^\n]*/g, "");
  for (const ch of uncommented) {
    if (OPENERS.includes(ch)) depth += 1;
    else if (CLOSERS.includes(ch)) depth -= 1;
    if (ch === "," && depth === 0) {
      parts.push(current);
      current = "";
    } else current += ch;
  }
  parts.push(current);
  return parts.map((part) => part.split(":")[0].trim()).filter((key) => /^[A-Za-z_]\w*$/.test(key));
}

/** The argument names the SIDECAR reads out of one dispatch arm. */
function sidecarArgs(route: string): Set<string> {
  const source = readFileSync(DISPATCH, "utf8");
  const armStarts = [...source.matchAll(/^ {12}"([a-z_]+)" => \{$/gm)];
  const index = armStarts.findIndex((arm) => arm[1] === route);
  if (index < 0) throw new Error(`no '${route}' arm in db_command.rs`);
  const from = armStarts[index].index ?? 0;
  const to = armStarts[index + 1]?.index ?? source.length;
  const body = source.slice(from, to);
  return new Set([...body.matchAll(/self\.arg_[a-z_]+\("([A-Za-z_]\w*)"/g)].map((m) => m[1]));
}

/** The argument names the RENDERER emits for one route, across every call site in the hook. */
function rendererArgs(route: string): Set<string> {
  const source = readFileSync(HOOK, "utf8");
  const emitted = new Set<string>();
  for (const call of source.matchAll(new RegExp(`"${route}",`, "g"))) {
    for (const key of literalKeys(bracedAfter(source, (call.index ?? 0) + call[0].length))) {
      emitted.add(key);
    }
  }
  return emitted;
}

describe("the command-template CRUD wire contract", () => {
  it.each(ROUTES)("every argument '%s' emits is one the sidecar reads", (route) => {
    const emitted = [...rendererArgs(route)].sort();
    const read = sidecarArgs(route);
    expect(emitted.length).toBeGreaterThan(0);
    expect(emitted.filter((key) => !read.has(key))).toEqual([]);
  });

  /**
   * THE NOTE IS ON BOTH HALVES OF BOTH ROUTES. Spelled out rather than left to the containment
   * above, which would stay green if the renderer simply stopped sending the field: a form that
   * silently drops what the user typed is precisely the failure this column is exposed to.
   */
  it.each(ROUTES)("'%s' carries the agent note on both sides", (route) => {
    expect(rendererArgs(route)).toContain("description");
    expect(sidecarArgs(route)).toContain("description");
  });

  /**
   * THE NOTE IS READ AS A TRISTATE ON THE UPDATE — same rule, same reason as
   * `command_group_labels`' name and colour. An EMPTY note is a value: clearing the textarea must
   * ERASE the stored note, and `arg_opt_str` (whose `null` means "absent") would turn that gesture
   * into a silent no-op, leaving the old note in every future session's context. On the CREATE
   * there is nothing to leave alone, so the ordinary optional read is correct there.
   */
  it("reads the note as a TRISTATE on the update, not as an optional string", () => {
    const source = readFileSync(DISPATCH, "utf8");
    const arm = source.slice(source.indexOf('"command_update" => {'));
    expect(arm).toContain('self.arg_tristate("description"');
  });
});
