import { describe, expect, it } from "vitest";

import type { GlobalSearchHit, GlobalSearchKind } from "@/bridge/contract";
import type { WorkspaceRuntimeService } from "@/components/runtime/use-workspace-runtime";

import { resolveSearchTarget } from "./search-target";

/** A hit of `kind`, with only the container fields that kind actually carries. */
function hit(kind: GlobalSearchKind, over: Partial<GlobalSearchHit> = {}): GlobalSearchHit {
  return {
    kind,
    id: `${kind}-1`,
    project_id: "p1",
    project_name: "Alpha",
    workspace_id: null,
    workspace_name: null,
    spec_id: null,
    terminal_id: null,
    label: kind,
    detail: null,
    extract: null,
    human_key: null,
    score: 0.5,
    updated_at: 1,
    ...over,
  };
}

describe("resolveSearchTarget — the typed target → route mapping (renderer-owned)", () => {
  it("opens a SPEC on its detail route", () => {
    expect(resolveSearchTarget(hit("spec", { id: "s1" }), { terminalAlive: false })).toEqual({
      projectId: "p1",
      to: "/work/$specId",
      params: { specId: "s1" },
    });
  });

  it("opens a TASK on ITS SPEC (default tab — no ?task=, no ?tab=)", () => {
    const target = resolveSearchTarget(hit("task", { id: "t1", spec_id: "s9" }), {
      terminalAlive: false,
    });
    expect(target).toEqual({ projectId: "p1", to: "/work/$specId", params: { specId: "s9" } });
  });

  it("falls back to the project's `/` for a task attached to NO Spec (tasks.spec_id is nullable)", () => {
    // A standalone / review task hangs off no Spec, so there is no `/work/$specId` to open. It
    // must still land somewhere real: `/` of ITS OWN project, never an error or a blank page.
    const target = resolveSearchTarget(hit("task", { id: "t2", spec_id: null }), {
      terminalAlive: false,
    });
    expect(target).toEqual({ projectId: "p1", to: "/", search: {} });
  });

  it("opens an IDEA on /knowledge", () => {
    expect(resolveSearchTarget(hit("idea"), { terminalAlive: false })).toEqual({
      projectId: "p1",
      to: "/knowledge",
    });
  });

  it("opens a WORKSPACE and a PROJECT on `/` (no ?workspace=, no new route)", () => {
    expect(
      resolveSearchTarget(hit("workspace", { workspace_id: "w1" }), { terminalAlive: false }),
    ).toEqual({ projectId: "p1", to: "/", search: {} });
    expect(resolveSearchTarget(hit("project", { id: "p1" }), { terminalAlive: false })).toEqual({
      projectId: "p1",
      to: "/",
      search: {},
    });
  });

  it("opens a managed COMMAND on the existing `?command=` deep-link", () => {
    expect(resolveSearchTarget(hit("command", { id: "c1" }), { terminalAlive: false })).toEqual({
      projectId: "p1",
      to: "/",
      search: { command: "c1" },
    });
  });

  // REVIEW-FINDING (« Une ligne par instance de commande gérée, portant l'id d'instance ») : the
  // `?command=` id is compared, in `<TerminalsBody>`, against `WorkspaceRuntimeService.id` — which
  // IS `command_instances.id`. While the backend answered the TEMPLATE id
  // (`managed_commands.id`), the comparison never matched, the deep-link was abandoned in silence
  // and the jump opened no pane. This pins the pairing on the REAL runtime type: the resolved
  // search param must name a service that exists in the project's runtime.
  it("resolves a COMMAND to an id that names a live WorkspaceRuntimeService (instance, not template)", () => {
    const services: WorkspaceRuntimeService[] = [
      {
        id: "instance-root",
        command_id: "template-build",
        workspace_id: "w-root",
        name: "build",
        command: "bun run build",
        run_state: "idle",
        exit_code: null,
        unread: false,
      },
      {
        id: "instance-auth",
        command_id: "template-build",
        workspace_id: "w-auth",
        name: "build",
        command: "bun run build",
        run_state: "idle",
        exit_code: null,
        unread: false,
      },
    ];
    // Two rows of the SAME template: each resolves to its own instance, so the two jumps land on
    // two different panes rather than on one guessed workspace.
    for (const service of services) {
      const target = resolveSearchTarget(
        hit("command", { id: service.id, workspace_id: service.workspace_id }),
        { terminalAlive: false },
      );
      expect(target).toEqual({ projectId: "p1", to: "/", search: { command: service.id } });
      const deepLinked = "search" in target ? target.search.command : undefined;
      expect(services.some((s) => s.id === deepLinked)).toBe(true);
    }
    // The template id — what the read used to answer — matches NO service: the silent dead jump.
    const stale = resolveSearchTarget(hit("command", { id: "template-build" }), {
      terminalAlive: false,
    });
    const staleId = "search" in stale ? stale.search.command : undefined;
    expect(services.some((s) => s.id === staleId)).toBe(false);
  });

  it("opens an agent SESSION on its terminal's `?terminal=` deep-link", () => {
    const target = resolveSearchTarget(hit("session", { id: "a1", terminal_id: "term-7" }), {
      terminalAlive: true,
    });
    expect(target).toEqual({ projectId: "p1", to: "/", search: { terminal: "term-7" } });
  });

  it("drops the deep-link when the session's terminal is no longer alive", () => {
    // `?terminal=` at a dead id makes the page wait forever AND suppresses its last-terminal
    // restore — an empty deck. The fallback is a real landing on the right project's `/`.
    const target = resolveSearchTarget(hit("session", { id: "a1", terminal_id: "term-7" }), {
      terminalAlive: false,
    });
    expect(target).toEqual({ projectId: "p1", to: "/", search: {} });
  });

  it("keeps the OWNER project on both fallbacks", () => {
    const task = resolveSearchTarget(hit("task", { project_id: "p9", spec_id: null }), {
      terminalAlive: false,
    });
    const session = resolveSearchTarget(hit("session", { project_id: "p9", terminal_id: "gone" }), {
      terminalAlive: false,
    });
    expect(task.projectId).toBe("p9");
    expect(session.projectId).toBe("p9");
  });

  it("never produces a url or href — the target is a route, params and search only", () => {
    for (const kind of [
      "spec",
      "task",
      "idea",
      "workspace",
      "project",
      "command",
      "session",
    ] as const) {
      const target = resolveSearchTarget(hit(kind, { spec_id: "s1", terminal_id: "t1" }), {
        terminalAlive: true,
      });
      expect(
        Object.keys(target).every((k) => ["projectId", "to", "params", "search"].includes(k)),
      ).toBe(true);
      expect(target.to).toMatch(/^(\/|\/knowledge|\/work\/\$specId)$/);
    }
  });
});
