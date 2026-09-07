import { render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import { RuntimeSelectionProvider } from "@/components/terminals/runtime-selection";
import { ConsumedNotificationsProvider } from "@/components/terminals/use-consumed-notifications";
import { WorkspaceRuntimeTree } from "@/components/terminals/workspace-runtime-tree";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import type {
  HomeProject,
  HomeService,
  HomeSession,
  HomeSnapshot,
  HomeSpec,
  HomeTerminal,
} from "./types";

/**
 * THE HOME'S CONTRACT, proven on the real page in both scopes.
 *
 * What is asserted here is as much what the page does NOT render as what it does: no answer
 * option, no state badge doubling the dot, no dot on a row at rest, no filler text, no link out of
 * the rail — plus the four empty states, the loading screen and the error screen. The elected
 * design is a subtraction as much as a layout, and a subtraction is only kept by a test.
 */

// --- Fixtures ---------------------------------------------------------------------------

const NYX: HomeProject = {
  id: "p1",
  name: "nyx",
  kind: "user",
  accent: null,
  icon: "sparkles",
  image_path: null,
};
const PALBANK: HomeProject = {
  id: "p2",
  name: "palbank",
  kind: "user",
  accent: "#2f6df6",
  icon: null,
  image_path: null,
};
/** A project nobody dressed — no accent, no icon, no image: the INITIAL on the neutral tile. */
const KEYROAD: HomeProject = {
  id: "p3",
  name: "keyroad",
  kind: "user",
  accent: null,
  icon: null,
  image_path: null,
};

function session(overrides: Partial<HomeSession> & { session_id: string }): HomeSession {
  return {
    terminal_id: `t-${overrides.session_id}`,
    agent_kind: "claude_code",
    state: "idle",
    secondary: null,
    label: null,
    cwd: "D:/Projects/nyx/nyx-v2",
    started_at: 0,
    workspace: { id: "ws1", name: "nyx-v2", branch: "v2" },
    project: NYX,
    ...overrides,
  };
}

function service(overrides: Partial<HomeService> & { instance_id: string }): HomeService {
  return {
    name: "renderer dev",
    run_state: "running",
    exit_code: null,
    unread: false,
    workspace: { id: "ws1", name: "nyx-v2", branch: "v2" },
    project: NYX,
    ...overrides,
  };
}

function spec(overrides: Partial<HomeSpec> & { spec_id: string }): HomeSpec {
  return {
    key: "NYX-42",
    title: "Project home & cross-project home",
    status: "in_progress",
    updated_at: 0,
    tasks_done: 5,
    tasks_total: 11,
    phase: { number: 2, total: 4, name: "Recap blocks" },
    project: NYX,
    ...overrides,
  };
}

function terminal(overrides: Partial<HomeTerminal> & { terminal_id: string }): HomeTerminal {
  return {
    label: null,
    cwd: "D:/Projects/nyx/nyx-v2",
    exec_state: "idle",
    exec_state_unread: false,
    workspace: { id: "ws1", name: "nyx-v2", branch: "v2" },
    project: NYX,
    ...overrides,
  };
}

const EMPTY: HomeSnapshot = { sessions: [], services: [], specs: [], terminals: [] };

/**
 * THE FIVE STATES, in the order the backend returns them (`waiting > error > ready > working >
 * idle`, oldest first within a rank). The page must render them in exactly this order and must not
 * re-sort — which is also how the GLOBAL sort is asserted: `palbank`'s waiting row sits above
 * `nyx`'s working one, so a calm project never floats above a project that is waiting.
 */
const FLOW: HomeSession[] = [
  session({
    session_id: "s-waiting",
    state: "waiting",
    secondary: "Proceed as is, or fold phases 2 and 3 together?",
    label: "renderer",
    project: PALBANK,
  }),
  session({ session_id: "s-error", state: "error", label: "e2e" }),
  session({ session_id: "s-ready", state: "ready", label: "docs" }),
  session({
    session_id: "s-working",
    state: "working",
    secondary: "Bash · cargo test -p nyx-core",
    label: "core",
  }),
  session({ session_id: "s-idle", state: "idle", label: "nyx-v2" }),
];

function projectRecord(id: string, name: string): ProjectRecord {
  return { id, name, collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
}

function workspaceRecord(projectId: string): WorkspaceRecord {
  return {
    id: `ws-${projectId}`,
    project_id: projectId,
    name: "root",
    path: `/${projectId}`,
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

interface BackendOptions {
  snapshot?: HomeSnapshot;
  /** Reject `home_snapshot` — the error screen. */
  fail?: boolean;
  /** Never resolve `home_snapshot` — the loading screen. */
  hang?: boolean;
}

function installBackend({ snapshot = EMPTY, fail = false, hang = false }: BackendOptions = {}) {
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [projectRecord("p1", "nyx"), projectRecord("p2", "palbank")];
        case "list_workspaces":
          return [workspaceRecord((a.projectId as string) ?? "p1")];
        // The rail's aggregate dot reads this per workspace; an EMPTY runtime keeps it quiet.
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: (a.workspaceId as string) ?? "ws-p1", name: "root", path: "/" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        case "home_snapshot":
          if (fail) throw new Error("nope");
          if (hang) return new Promise(() => {});
          return snapshot;
        case "system_scope_detail":
          // The Système singleton is ABSENT on this fixture — the documented `null` answer of a
          // database with no scope. The rail then renders no tile, which is what these cases are
          // about; the catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`.
          return null;
        default:
          return [];
      }
    },
    { shouldMockEvents: true },
  );
}

function mountHome(path = "/home") {
  const router = createAppRouter([path]);
  return { ...render(<RouterProvider router={router} />), router };
}

/** The page's own region — everything the rail is NOT. */
async function homePage(): Promise<HTMLElement> {
  return await screen.findByTestId("home-page");
}

/** A session row by id. */
function row(id: string): HTMLElement {
  return screen.getByTestId(`home-session-${id}`);
}

/** The STATE DOT of a row, or `null` when the row draws none (a quiet state draws none). */
function rowDot(el: HTMLElement): Element | null {
  return el.querySelector("span[data-state]");
}

// --- The tests --------------------------------------------------------------------------

describe("HomePage — the project scope", () => {
  it("reads the ENTERED project's scope (the id goes to `home_snapshot`)", async () => {
    const scopes: unknown[] = [];
    mockIPC(
      (cmd, args) => {
        if (cmd === "home_snapshot") {
          scopes.push((args ?? {}) as Record<string, unknown>);
          return EMPTY;
        }
        if (cmd === "list_projects") return [projectRecord("p1", "nyx")];
        if (cmd === "list_workspaces") return [workspaceRecord("p1")];
        // The rail's per-project dot reads this for every workspace; an empty array is not a
        // snapshot, and the shell's error boundary swallowed the whole screen when it got one.
        if (cmd === "workspace_runtime_snapshot") {
          return {
            workspace: { id: "ws-p1", name: "root", path: "/" },
            terminals: [],
            services: [],
          };
        }
        // The Système singleton is ABSENT here — the documented `null` of a database with no scope.
        // The catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`: the rail
        // would dereference it and the shell error boundary would swallow the screen.
        if (cmd === "system_scope_detail") return null;
        return [];
      },
      { shouldMockEvents: true },
    );
    mountHome();
    await waitFor(() => expect(scopes).toContainEqual({ projectId: "p1" }));
    // …and the page draws itself in that scope.
    expect((await homePage()).getAttribute("data-scope")).toBe("project");
    // EVERY read of the home is this project's — and there is NO OTHER. The page never asks for
    // another project's home, and nothing else on the screen makes a cross-project read either:
    // the rail's `All projects` tile used to keep a global rollup live here for its own dot, and
    // that dot is gone (`terminals/rail-dots.test.tsx` holds the line on why). So a bare `{}` read
    // appearing in this list would now mean the page itself had gone global.
    expect(scopes).toEqual([{ projectId: "p1" }]);
  });

  it("renders the four panels and, on an empty home, the four empty states", async () => {
    installBackend();
    mountHome();
    const page = await homePage();
    await within(page).findByText("No agent session");
    for (const title of ["Agent sessions", "Recent specs", "Services", "Terminals"]) {
      expect(within(page).getByRole("heading", { name: title })).toBeTruthy();
    }
    expect(within(page).getByText("No spec yet")).toBeTruthy();
    expect(within(page).getByText("No service running")).toBeTruthy();
    expect(within(page).getByText("No terminal open")).toBeTruthy();
  });

  it("shows the LOADING screen before the first read resolves — never an empty state", async () => {
    installBackend({ hang: true });
    mountHome();
    expect(await screen.findByTestId("home-loading")).toBeTruthy();
    expect(screen.queryByText("No agent session")).toBeNull();
  });

  it("shows the four EMPTY states — not an endless spinner — when there is no project at all", async () => {
    // A fresh install: the project tree answers with nothing, so no scope is ever read.
    mockIPC(
      (cmd) => {
        if (cmd === "list_projects") return [];
        if (cmd === "home_snapshot") throw new Error("the project home must not read a scope here");
        // The Système singleton is ABSENT here — the documented `null` of a database with no scope.
        // The catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`: the rail
        // would dereference it and the shell error boundary would swallow the screen.
        if (cmd === "system_scope_detail") return null;
        return [];
      },
      { shouldMockEvents: true },
    );
    mountHome();
    const page = await homePage();
    expect(await within(page).findByText("No agent session")).toBeTruthy();
    expect(within(page).queryByTestId("home-loading")).toBeNull();
  });

  it("shows the ERROR screen when the read is rejected", async () => {
    installBackend({ fail: true });
    mountHome();
    expect(await screen.findByTestId("home-error")).toBeTruthy();
    expect(screen.queryByText("No agent session")).toBeNull();
  });
});

describe("HomePage — the row anatomy", () => {
  beforeEach(async () => {
    installBackend({
      snapshot: {
        ...EMPTY,
        sessions: FLOW,
        services: [
          service({ instance_id: "i1" }),
          service({ instance_id: "i2", name: "typecheck", run_state: "error", exit_code: 2 }),
        ],
        specs: [spec({ spec_id: "sp1" }), spec({ spec_id: "sp2", key: "NYX-41", phase: null })],
        terminals: [terminal({ terminal_id: "t-bare" })],
      },
    });
    mountHome();
    await screen.findByTestId("home-session-s-waiting");
  });

  it("keeps ONE structure across the five states — only the dot and the rank change", async () => {
    await homePage();
    for (const [id, label] of [
      ["s-waiting", "renderer"],
      ["s-error", "e2e"],
      ["s-ready", "docs"],
      ["s-working", "core"],
      ["s-idle", "nyx-v2"],
    ] as const) {
      const el = row(id);
      // Same element, same name slot, same age slot, on every state.
      expect(el.tagName).toBe("BUTTON");
      expect(el.textContent).toContain(label);
    }
    // The LIVE and settled states wear their dot…
    expect(rowDot(row("s-waiting"))?.getAttribute("data-state")).toBe("waiting");
    expect(rowDot(row("s-error"))?.getAttribute("data-state")).toBe("error");
    expect(rowDot(row("s-ready"))?.getAttribute("data-state")).toBe("success");
    expect(rowDot(row("s-working"))?.getAttribute("data-state")).toBe("running");
    // …and a row AT REST wears none: `isQuietState` hides it, on the home as everywhere else.
    expect(rowDot(row("s-idle"))).toBeNull();
  });

  it("renders the flow in the BACKEND's order and never re-sorts or groups by project", async () => {
    const page = await homePage();
    const ids = Array.from(page.querySelectorAll("[data-testid^='home-session-s']")).map((el) =>
      el.getAttribute("data-testid"),
    );
    expect(ids).toEqual([
      "home-session-s-waiting",
      "home-session-s-error",
      "home-session-s-ready",
      "home-session-s-working",
      "home-session-s-idle",
    ]);
  });

  it("renders the information slot only when there IS one — no filler on a quiet row", async () => {
    expect(
      within(row("s-waiting")).getByText("Proceed as is, or fold phases 2 and 3 together?"),
    ).toBeTruthy();
    expect(within(row("s-working")).getByText("Bash · cargo test -p nyx-core")).toBeTruthy();
    // A ready / error / idle row has nothing to say and says nothing — not a dash, not a
    // placeholder, not an empty element.
    for (const id of ["s-ready", "s-error", "s-idle"]) {
      expect(within(row(id)).queryByTestId("home-row-secondary")).toBeNull();
    }
  });

  it("names a terminal EXACTLY as the MOUNTED sidebar does, not as a function it also calls", async () => {
    await homePage();
    // A manual label wins…
    expect(within(row("s-idle")).getByTestId("home-row-name").textContent).toBe("nyx-v2");
    // …and with NO manual label the name is the cwd BASENAME on both surfaces.
    //
    // The comparison is made against `<WorkspaceRuntimeTree>` — the sidebar the shell actually
    // mounts — and NOT against `displayName()` of `components/sidebar/terminal-item.tsx`, which
    // belongs to the removed `<TerminalManager>`. Asserting against a function BOTH sides call is
    // how this test stayed green while the two screens named one session two ways
    // (review 01M15AWP3W1Y2PD3YHB406ERYT): it compared the home to itself. Rendering the real
    // tree from the SAME label/cwd pair is the only form of the assertion that can fail.
    const homeName = within(screen.getByTestId("home-terminal-t-bare")).getByTestId(
      "home-row-name",
    ).textContent;
    expect(homeName).toBe("nyx-v2");

    const sidebar = render(
      <RuntimeSelectionProvider initialSelection={null}>
        <ConsumedNotificationsProvider>
          <WorkspaceRuntimeTree
            snapshot={{
              // Deliberately NOT named `nyx-v2`: the match below must come from the row, not
              // from a workspace label that happens to read the same.
              workspace: { id: "ws1", name: "workspace", path: "D:/Projects/nyx/nyx-v2" },
              terminals: [
                {
                  id: "t-bare",
                  label: null,
                  cwd: "D:/Projects/nyx/nyx-v2",
                  workspace_id: "ws1",
                  status: "alive",
                  exec_state: "idle",
                  exec_state_unread: false,
                  sessions: [],
                },
              ],
              agent_sessions: [],
              services: [],
            }}
          />
        </ConsumedNotificationsProvider>
      </RuntimeSelectionProvider>,
    );
    // The sidebar row's select band IS the name.
    expect(within(sidebar.container).getByRole("button", { name: homeName! })).toBeTruthy();
    // And the raw cwd — what the tree wrote for an unlabelled terminal before the fix — is gone.
    expect(within(sidebar.container).queryByText("D:/Projects/nyx/nyx-v2")).toBeNull();
    sidebar.unmount();
  });

  it("draws a spec card's advancement as [bar][counter][phase], and nothing when there is no phase", async () => {
    const card = screen.getByTestId("home-spec-sp1");
    expect(within(card).getByText("NYX-42")).toBeTruthy();
    expect(within(card).getByText("5/11")).toBeTruthy();
    expect(within(card).getByText("phase 2/4 · Recap blocks")).toBeTruthy();
    // A Spec with no phase plan ends on its counter — the empty track is the LAST one.
    const noPhase = screen.getByTestId("home-spec-sp2");
    expect(within(noPhase).getByText("5/11")).toBeTruthy();
    expect(within(noPhase).queryByText(/^phase /)).toBeNull();
  });

  /**
   * NO VERB, on any family. The session row used to reveal `open ›` / `answer ›` at its right edge
   * on hover while the two other families never had one, so the anatomy was not the same from one
   * line to the next (user feedback, review `01M1C8AHV63Y0D54BJCSN46737`). The word was rendered
   * at ALL times — only its opacity was animated — so querying at rest is what catches it coming
   * back, whether it is revealed on hover or on focus.
   */
  it("reveals NO action verb on any family — the row IS the control, and its hover already says so", async () => {
    const page = await homePage();
    const verb = /^\s*(open|answer)\s*›?\s*$/i;
    const families = [
      ...["s-waiting", "s-error", "s-ready", "s-working", "s-idle"].map(row),
      within(page).getByTestId("home-service-i1"),
      within(page).getByTestId("home-service-i2"),
      within(page).getByTestId("home-terminal-t-bare"),
      within(page).getByTestId("home-spec-sp1"),
    ];
    for (const line of families) expect(within(line).queryAllByText(verb)).toHaveLength(0);
    // And the SLOT went with it: a session row now ENDS on its age, which is what lands every age
    // of the panel on the same right edge.
    for (const id of ["s-waiting", "s-error", "s-ready", "s-working", "s-idle"]) {
      const line = row(id);
      expect(line.lastElementChild).toBe(within(line).getByTestId("home-row-age"));
    }
    // The trailing slots that carry FACTS are untouched — they were never verbs.
    expect(within(within(page).getByTestId("home-service-i2")).getByText("exit 2")).toBeTruthy();
  });

  it("puts the exit CODE on a failed service — a fact the dot cannot carry, never a state word", async () => {
    const failed = screen.getByTestId("home-service-i2");
    expect(within(failed).getByText("exit 2")).toBeTruthy();
    expect(rowDot(failed)?.getAttribute("data-state")).toBe("error");
    // The word itself is nowhere on the row.
    expect(within(failed).queryByText("error")).toBeNull();
  });
});

/**
 * THE CHECKOUT PILL, on a REAL branch name. `codex/review-downloads-dev-2026-06-12` is what a row
 * wore on the user's screen: the pill grew to whatever it held and ate the line it annotates, and
 * the two values ran into each other (review `01M1ENS135VBDJNDJ6R2S87ZEM`). The slot itself does
 * not move — it is the LENGTH it is allowed to take that is bounded here.
 */
describe("HomePage — the checkout pill", () => {
  const LONG = "codex/review-downloads-dev-2026-06-12";

  beforeEach(async () => {
    installBackend({
      snapshot: {
        ...EMPTY,
        sessions: [
          session({
            session_id: "s-long",
            label: "depot",
            workspace: { id: "ws9", name: "main", branch: LONG },
          }),
          session({
            session_id: "s-nobranch",
            label: "scratch",
            workspace: { id: "ws8", name: "main", branch: null },
          }),
        ],
      },
    });
    mountHome();
    await screen.findByTestId("home-session-s-long");
  });

  it("keeps the WHOLE checkout readable however long the branch is", async () => {
    const pill = within(row("s-long")).getByTestId("home-row-checkout");
    // Capped: a branch of any length can no longer stretch the pill across the row…
    expect(pill.className).toContain("max-w-[12rem]");
    // …and what the cap cuts is never lost — the full value is one hover away.
    expect(pill.getAttribute("title")).toBe(`main ${LONG}`);
  });

  it("cuts the BRANCH and never the workspace — one run of text, one ellipsis", async () => {
    const pill = within(row("s-long")).getByTestId("home-row-checkout");
    // ONE truncating box, holding workspace then branch. Two SIBLING boxes under the cap would
    // each ellipse on their own, and a flex line shrinks both — the short half would lose
    // characters while the branch it stands next to still ran on.
    const truncating = pill.querySelectorAll(".truncate");
    expect(truncating).toHaveLength(1);
    expect(truncating[0].textContent).toBe(`main${LONG}`);
    // The branch carries the weight the elected round gives it (`.ws b`), which is what stops the
    // two values reading as one string.
    expect(within(pill).getByText(LONG).className).toContain("font-medium");
  });

  it("says only the workspace when there is no branch — no separator left hanging", async () => {
    const pill = within(row("s-nobranch")).getByTestId("home-row-checkout");
    expect(pill.getAttribute("title")).toBe("main");
    expect(pill.textContent).toBe("main");
  });
});

describe("HomePage — what it deliberately does NOT render", () => {
  beforeEach(async () => {
    installBackend({
      snapshot: { ...EMPTY, sessions: FLOW, specs: [spec({ spec_id: "sp1" })] },
    });
    mountHome();
    await screen.findByTestId("home-session-s-waiting");
  });

  it("offers NO answer option on a blocked row — nyx does not hold them", async () => {
    await homePage();
    const blocked = row("s-waiting");
    // The row IS the only control: nothing nested inside it can be clicked, so there is no
    // "pick option 1 / option 2" affordance anywhere on the line.
    expect(blocked.querySelectorAll("button, a, [role='option'], input")).toHaveLength(0);
  });

  it("puts NO state badge on a row — the dot says the state, alone", async () => {
    await homePage();
    for (const [id, word] of [
      ["s-waiting", "waiting"],
      ["s-error", "error"],
      ["s-ready", "ready"],
      ["s-working", "working"],
      ["s-idle", "idle"],
    ] as const) {
      expect(within(row(id)).queryByText(word, { exact: false })).toBeNull();
    }
  });

  it("says a state's WORD at most ONCE, at the head of its run — never on a row, never twice", async () => {
    const page = await homePage();
    // The elected design's legend over the ONE continuous flow (prototype round v8→v11: `le mot
    // idle en tête de son groupe`). It is a legend, not a container: the rows are siblings of it,
    // the list is not split, and no state gets two heads.
    for (const state of ["waiting", "error", "ready", "working", "idle"] as const) {
      expect(within(page).getAllByTestId(`home-state-group-${state}`)).toHaveLength(1);
    }
  });

  it("carries NO link out of the rail — only the entities it lists", async () => {
    const page = await homePage();
    const hrefs = within(page)
      .queryAllByRole("link")
      .map((a) => a.getAttribute("href"));
    // The only anchors are the Spec cards, each pointing at its own Spec. No "see all projects",
    // no shortcut to Terminals / Work / Knowledge: the rail already routes there.
    expect(hrefs.every((h) => h?.startsWith("/work/"))).toBe(true);
  });
});

describe("HomePage — the information slot reads the LIVE activity", () => {
  /** Install the page with a live `agent_activity_snapshot` for `t-s1`. */
  function withActivity(activity: Record<string, unknown>, snapshotSecondary: string | null) {
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "list_projects":
            return [projectRecord("p1", "nyx")];
          case "list_workspaces":
            return [workspaceRecord("p1")];
          case "workspace_runtime_snapshot":
            return {
              workspace: { id: (a.workspaceId as string) ?? "ws-p1", name: "root", path: "/" },
              terminals: [],
              agent_sessions: [],
              services: [],
            };
          case "agent_activity_snapshot":
            return [{ terminal_id: "t-s1", ready_unread: false, ...activity }];
          case "home_snapshot":
            return {
              ...EMPTY,
              sessions: [
                session({ session_id: "s1", state: "waiting", secondary: snapshotSecondary }),
              ],
            };
          case "system_scope_detail":
            // The Système singleton is ABSENT on this fixture — the documented `null` answer of a
            // database with no scope. The rail then renders no tile, which is what these cases are
            // about; the catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`.
            return null;
          default:
            return [];
        }
      },
      { shouldMockEvents: true },
    );
    mountHome();
  }

  it("prefers the BLOCK over the tool while the agent is waiting", async () => {
    withActivity(
      { activity: "waiting", waiting_on: "Fold phases 2 and 3?", tool: "Bash" },
      "a stale line",
    );
    const el = await screen.findByTestId("home-session-s1");
    await waitFor(() =>
      expect(within(el).getByTestId("home-row-secondary").textContent).toBe("Fold phases 2 and 3?"),
    );
  });

  it("falls back to the tool in flight when nothing blocks", async () => {
    withActivity({ activity: "working", waiting_on: null, tool: "Edit · contract.test.ts" }, null);
    const el = await screen.findByTestId("home-session-s1");
    await waitFor(() =>
      expect(within(el).getByTestId("home-row-secondary").textContent).toBe(
        "Edit · contract.test.ts",
      ),
    );
  });

  it("renders NOTHING when a blocked row carries no readable question — never a filler", async () => {
    // A permission gate: the agent IS blocked, and the hook payload held no text. The rule stops
    // there — it must not fall through to a tool the agent is not running.
    withActivity({ activity: "waiting", waiting_on: null, tool: "Bash" }, "a stale line");
    const el = await screen.findByTestId("home-session-s1");
    await waitFor(() => expect(within(el).queryByTestId("home-row-secondary")).toBeNull());
  });
});

describe("HomePage — the cross-project scope", () => {
  beforeEach(() => {
    installBackend({
      snapshot: {
        ...EMPTY,
        sessions: [
          ...FLOW,
          // A session on a LOOSE terminal: no workspace, no project.
          session({ session_id: "s-loose", state: "working", workspace: null, project: null }),
          // A project with no chosen identity — the INITIAL on the neutral tile.
          session({ session_id: "s-plain", state: "idle", project: KEYROAD }),
        ],
        specs: [spec({ spec_id: "sp1" })],
      },
    });
  });

  it("is the SAME page with the SAME four columns", async () => {
    mountHome("/home?scope=all");
    const page = await homePage();
    expect(page.getAttribute("data-scope")).toBe("all");
    for (const title of ["Agent sessions", "Recent specs", "Services", "Terminals"]) {
      expect(within(page).getByRole("heading", { name: title })).toBeTruthy();
    }
    expect(within(page).getByRole("heading", { name: "All projects" })).toBeTruthy();
  });

  it("reads the GLOBAL scope — no `projectId` goes to `home_snapshot`", async () => {
    const scopes: unknown[] = [];
    mockIPC(
      (cmd, args) => {
        if (cmd === "home_snapshot") {
          scopes.push((args ?? {}) as Record<string, unknown>);
          return EMPTY;
        }
        if (cmd === "list_projects") return [projectRecord("p1", "nyx")];
        if (cmd === "list_workspaces") return [workspaceRecord("p1")];
        // The Système singleton is ABSENT here — the documented `null` of a database with no scope.
        // The catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`: the rail
        // would dereference it and the shell error boundary would swallow the screen.
        if (cmd === "system_scope_detail") return null;
        return [];
      },
      { shouldMockEvents: true },
    );
    mountHome("/home?scope=all");
    await waitFor(() => expect(scopes).toContainEqual({}));
    expect(scopes).not.toContainEqual({ projectId: "p1" });
  });

  it("marks each row with its project — named in the title only, never in a column", async () => {
    mountHome("/home?scope=all");
    await homePage();
    const mark = within(row("s-waiting")).getByTestId("home-project-mark");
    expect(mark.getAttribute("title")).toBe("palbank");
    expect(mark.getAttribute("aria-label")).toBe("Project palbank");
    // The name is NOT written on the row: no column, no text.
    expect(within(row("s-waiting")).queryByText("palbank")).toBeNull();
  });

  it("falls back to the INITIAL for a project with no identity", async () => {
    mountHome("/home?scope=all");
    await homePage();
    const mark = within(row("s-plain")).getByTestId("home-project-mark");
    expect(mark.getAttribute("title")).toBe("keyroad");
    expect(within(mark).getByText("K")).toBeTruthy();
  });

  it("gives a session with NO project no mark at all — and keeps it in the flow", async () => {
    mountHome("/home?scope=all");
    const page = await homePage();
    expect(within(row("s-loose")).queryByTestId("home-project-mark")).toBeNull();
    // Still a row, at its rank, in the one list.
    const ids = Array.from(page.querySelectorAll("[data-testid^='home-session-s']")).map((el) =>
      el.getAttribute("data-testid"),
    );
    expect(ids).toContain("home-session-s-loose");
  });

  it("keeps ONE global sort — a calm project never floats above a project that is waiting", async () => {
    mountHome("/home?scope=all");
    const page = await homePage();
    const ids = Array.from(page.querySelectorAll("[data-testid^='home-session-s']")).map((el) =>
      el.getAttribute("data-testid"),
    );
    // `palbank`'s blocked row leads, and `nyx`'s rows follow at their own ranks — interleaved by
    // ATTENTION, never bucketed by project. A grouping would have put the two `nyx` idle rows
    // beside their working sibling and pushed `palbank` into a block of its own.
    expect(ids).toEqual([
      "home-session-s-waiting",
      "home-session-s-error",
      "home-session-s-ready",
      "home-session-s-working",
      "home-session-s-idle",
      "home-session-s-loose",
      "home-session-s-plain",
    ]);
  });

  it("marks a SERVICE and a bare TERMINAL with their project too — same corner, same rule", async () => {
    mockIPC(
      (cmd, args) => {
        const a = (args ?? {}) as Record<string, unknown>;
        switch (cmd) {
          case "list_projects":
            return [projectRecord("p1", "nyx")];
          case "list_workspaces":
            return [workspaceRecord("p1")];
          case "workspace_runtime_snapshot":
            return {
              workspace: { id: (a.workspaceId as string) ?? "ws-p1", name: "root", path: "/" },
              terminals: [],
              agent_sessions: [],
              services: [],
            };
          case "home_snapshot":
            return {
              ...EMPTY,
              services: [service({ instance_id: "i1", project: PALBANK })],
              terminals: [terminal({ terminal_id: "t1", project: KEYROAD })],
            };
          case "system_scope_detail":
            // The Système singleton is ABSENT on this fixture — the documented `null` answer of a
            // database with no scope. The rail then renders no tile, which is what these cases are
            // about; the catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`.
            return null;
          default:
            return [];
        }
      },
      { shouldMockEvents: true },
    );
    mountHome("/home?scope=all");
    const svc = await screen.findByTestId("home-service-i1");
    expect(within(svc).getByTestId("home-project-mark").getAttribute("title")).toBe("palbank");
    const term = screen.getByTestId("home-terminal-t1");
    expect(within(term).getByTestId("home-project-mark").getAttribute("title")).toBe("keyroad");
  });

  it("puts NO mark on a spec card — its key already names the project", async () => {
    mountHome("/home?scope=all");
    await homePage();
    expect(
      within(screen.getByTestId("home-spec-sp1")).queryByTestId("home-project-mark"),
    ).toBeNull();
  });

  it("does not mark a row on the PROJECT home — the scope already says whose it is", async () => {
    mountHome("/home");
    const page = await homePage();
    expect(within(page).queryAllByTestId("home-project-mark")).toHaveLength(0);
  });
});
