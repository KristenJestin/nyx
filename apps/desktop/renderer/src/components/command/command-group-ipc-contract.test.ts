import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * THE WIRE CONTRACT OF THE `command_group_*` ROUTES, PINNED BETWEEN THE TWO HALVES.
 *
 * Both halves are STRINGS, written twice and compared by nothing: the renderer emits
 * `command_group_add { groupId, commandId }` from `use-command-groups.ts`, the sidecar reads those
 * exact keys by hand in `db_command.rs` (`arg_str("groupId")` …). A typo on one side, or a rename
 * on one side only, compiles, passes `tsc`, passes `cargo check`, and fails at runtime with a
 * silently refused edit.
 *
 * Nothing else covers it (review finding 01M0ZN7TJT9ZR8QY8WPEWZ7KNS):
 *  - `command-groups.test.tsx` injects an in-memory controller, so `useCommandGroups` — the only
 *    place the route names are written on this side — is never exercised;
 *  - `contract/contract.test.ts` pins the MessagePack envelope, not the db-command names;
 *  - `crates/nyx-sidecar/tests/db_command.rs` round-trips a real sidecar, but on other routes.
 *
 * This matters for THIS feature in particular: finding #90 (01M0ABR6V501D0SPR2BYBVZC4A) requires an
 * MCP operation and a hand gesture to produce the same state, in both directions. The MCP half is
 * already pinned (`command-groups.test.tsx` reads the `action` enum straight out of `mcp.rs`); the
 * IPC half — the hand's half — is this file.
 *
 * Both lists are DERIVED from the sources, never restated here, so an eighth route or a renamed
 * argument turns this red instead of drifting.
 */

const RENDERER = process.cwd();
const REPO = join(RENDERER, "..", "..", "..");
const DISPATCH = join(REPO, "crates", "nyx-sidecar", "src", "runtime", "db_command.rs");
const COMMAND_DIR = join(RENDERER, "src", "components", "command");
const HOOK = join(COMMAND_DIR, "use-command-groups.ts");
/**
 * THE RENDERER'S HALF IS TWO FILES, and the split is the design rather than an accident.
 *
 * `use-command-groups.ts` is PROJECT-scoped — it reads and edits `managed_command_groups`, which
 * hold command TEMPLATES. `use-command-group-run.ts` is WORKSPACE-scoped — starting a group drives
 * `command_instances`, which only exist per workspace, which is why those three controls live in
 * the SIDEBAR and not in the project-settings modal (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
 * 01M11XVEFEQ524XRT9ARPQFGSV). Both are scanned, so neither half can add a route in silence.
 */
const HOOKS = [HOOK, join(COMMAND_DIR, "use-command-group-run.ts")];
const CONTRACT = join(RENDERER, "src", "bridge", "contract.ts");

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

/** The top-level keys of an object literal body (`a: 1, b` → `["a", "b"]`). */
function literalKeys(body: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let current = "";
  for (const ch of body) {
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

/** Route → argument names, as the SIDECAR reads them out of its dispatch arms. */
function sidecarRoutes(): Map<string, Set<string>> {
  const source = readFileSync(DISPATCH, "utf8");
  const armStarts = [...source.matchAll(/^ {12}"([a-z_]+)" => \{$/gm)];
  const routes = new Map<string, Set<string>>();
  armStarts.forEach((arm, index) => {
    const name = arm[1];
    if (!name.startsWith("command_group_")) return;
    const from = arm.index ?? 0;
    const to = armStarts[index + 1]?.index ?? source.length;
    const body = source.slice(from, to);
    const args = new Set(
      [...body.matchAll(/self\.arg_[a-z_]+\("([A-Za-z_]\w*)"/g)].map((m) => m[1]),
    );
    routes.set(name, args);
  });
  return routes;
}

/** Route → argument names, as the RENDERER emits them from its command-group hooks. */
function rendererRoutes(): Map<string, Set<string>> {
  const routes = new Map<string, Set<string>>();
  for (const path of HOOKS) {
    const source = readFileSync(path, "utf8");
    for (const call of source.matchAll(/"(command_group_[a-z_]+)",/g)) {
      const name = call[1];
      const keys = literalKeys(bracedAfter(source, (call.index ?? 0) + call[0].length));
      const known = routes.get(name) ?? new Set<string>();
      for (const key of keys) known.add(key);
      routes.set(name, known);
    }
  }
  return routes;
}

/** The route names the TS union advertises — the third place they are written. */
function contractRoutes(): string[] {
  const source = readFileSync(CONTRACT, "utf8");
  return [...source.matchAll(/^ {2}\| "(command_group_[a-z_]+)"$/gm)].map((m) => m[1]).sort();
}

describe("the command_group_* wire contract", () => {
  const sidecar = sidecarRoutes();
  const renderer = rendererRoutes();

  /**
   * THE COUNT IS DERIVED, NOT WRITTEN DOWN. It used to be pinned at seven, so the pass that added
   * the shape and the three group-wide lifecycle routes turned this red for the RIGHT reason and
   * the WRONG one at once — "a route moved on one side only" is what this file is for, "there are
   * eleven of them now" is bookkeeping. The three sources are compared to each other instead: any
   * two agreeing and the third not is still a failure, and adding a twelfth route to all three
   * costs no edit here.
   */
  it("the three declarations of the routes agree, name for name", () => {
    expect(sidecar.size).toBeGreaterThan(0);
    expect([...renderer.keys()].sort()).toEqual([...sidecar.keys()].sort());
    expect(contractRoutes()).toEqual([...sidecar.keys()].sort());
  });

  it.each([...sidecar.keys()].sort())(
    "'%s' carries the same argument names on both sides",
    (route) => {
      const emitted = [...(renderer.get(route) ?? new Set<string>())].sort();
      const read = [...(sidecar.get(route) ?? new Set<string>())].sort();
      expect(emitted.length).toBeGreaterThan(0);
      expect(emitted).toEqual(read);
    },
  );

  /**
   * THE THREE-STATE LABELS, kept honest. `command_group_labels` is the one route where an ABSENT
   * key and an explicit `null` mean different things (leave the label alone vs CLEAR it), which is
   * why the sidecar reads it with `arg_tristate` and not `arg_opt_str`. Downgrading it would make
   * "clear the name" silently a no-op, with nothing else to catch it.
   */
  it("reads the labels as a TRISTATE, not as an optional string", () => {
    const source = readFileSync(DISPATCH, "utf8");
    const arm = source.slice(source.indexOf('"command_group_labels" => {'));
    expect(arm).toContain('self.arg_tristate("name"');
    expect(arm).toContain('self.arg_tristate("color"');
    // …and the renderer sends BOTH keys explicitly (one per edit), never a merged patch.
    expect(readFileSync(HOOK, "utf8")).toContain(
      'mutate("command_group_labels", { groupId, name })',
    );
    expect(readFileSync(HOOK, "utf8")).toContain(
      'mutate("command_group_labels", { groupId, color })',
    );
  });
});
