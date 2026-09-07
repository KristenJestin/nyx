import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { mockIPC } from "@/bridge/test-harness";
import { describe, expect, it, vi } from "vitest";

import type { GlobalSearchAnswer, GlobalSearchHit, GlobalSearchKind } from "@/bridge/contract";
import { ActiveProjectProvider, useActiveProject } from "@/components/terminals/active-project";
import {
  PendingTerminalProvider,
  usePendingTerminal,
} from "@/components/terminals/pending-terminal";
import {
  RevealWorkspaceProvider,
  useRevealWorkspace,
} from "@/components/terminals/reveal-workspace";

import { SearchPalette } from "./search-palette";

/**
 * A hit, with the CONTEXT the backend now joins onto it: `project_name` is always there,
 * `workspace_name` exactly where `workspace_id` is. Nothing in these tests hands the palette a
 * `project_id → name` map any more — the rail's tree is no longer the source of that name.
 */
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
    label: `${kind} label`,
    detail: null,
    extract: null,
    human_key: null,
    score: 0.5,
    updated_at: Date.now(),
    ...over,
  };
}

/**
 * THE TITLE OF A LINE — entity or action alike: the first child of the row's text column, which is
 * itself the second child, right after the fixed-width glyph. That ONE accessor reaches both
 * species is half of what "an action is a line like the others" means; the class it carries is the
 * other half, and both are asserted below.
 */
function lineTitle(row: HTMLElement): HTMLElement {
  return row.children[1]!.firstElementChild as HTMLElement;
}

/** Surfaces the entered project so a test can assert the activation the navigation depends on. */
function ActiveProjectProbe() {
  const { activeProjectId, scope } = useActiveProject();
  return (
    <div data-testid="active-project" data-scope={scope.kind}>
      {activeProjectId ?? "none"}
    </div>
  );
}

/**
 * Surfaces the PENDING WORKSPACE REVEAL — the ephemeral signal a jump raises so the target's band
 * is open on arrival. It stands in for the Terminals page's workspace bands, which are the real
 * consumers (`terminals/workspace-accordion.tsx`); what those DO with the signal is asserted in
 * that component's own suite.
 */
function RevealProbe() {
  const { revealWorkspaceId } = useRevealWorkspace();
  return <div data-testid="reveal-workspace">{revealWorkspaceId ?? "none"}</div>;
}

/**
 * Surfaces the PENDING TERMINAL request — the one-shot signal the `Open a new terminal in …` action
 * raises. It stands in for `<TerminalsBody>`, the real consumer, which honours it through its own
 * `newTerminalInWorkspace` (create + attach + SELECT — the "and enter it" half). What the page DOES
 * with the signal is asserted in `terminals/terminals-page.test.tsx`; what the PALETTE owes is to
 * raise it for the right workspace, which is what this probe reads.
 */
function PendingTerminalProbe() {
  const { pendingTerminal } = usePendingTerminal();
  return <div data-testid="pending-terminal">{pendingTerminal?.workspaceId ?? "none"}</div>;
}

/**
 * Mount the palette inside a REAL router (memory history) with stub pages on the app's own paths,
 * so an activation is asserted on the router's actual location — not on a mocked `navigate`.
 */
function renderPalette(
  backend: (cmd: string, args?: Record<string, unknown>) => unknown,
  initialEntry = "/",
  {
    onAddWorkspace,
    systemProjectId,
  }: {
    /** The rail's add-workspace flow, as `<BaseLayout>` hands it down. Omitted ⇒ inert. */
    onAddWorkspace?: (projectId: string) => void;
    /** The SYSTÈME singleton's project id, as `<BaseLayout>` hands it down. Omitted ⇒ none. */
    systemProjectId?: string | null;
  } = {},
) {
  mockIPC(backend);
  const rootRoute = createRootRoute({
    component: () => (
      <ActiveProjectProvider>
        <RevealWorkspaceProvider>
          <PendingTerminalProvider>
            <SearchPalette onAddWorkspace={onAddWorkspace} systemProjectId={systemProjectId} />
            <ActiveProjectProbe />
            <RevealProbe />
            <PendingTerminalProbe />
            <Outlet />
          </PendingTerminalProvider>
        </RevealWorkspaceProvider>
      </ActiveProjectProvider>
    ),
  });
  const mk = (path: string, text: string) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => <div>{text}</div>,
      validateSearch: (search: Record<string, unknown>) => search,
    });
  const routeTree = rootRoute.addChildren([
    mk("/", "TERMINALS"),
    mk("/work/$specId", "SPEC"),
    mk("/knowledge", "KNOWLEDGE"),
    mk("/system", "SYSTEM"),
  ]);
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [initialEntry] }),
  });
  return { ...render(<RouterProvider router={router} />), router };
}

/**
 * The wire ANSWER of a `global_search`: the hits plus `knowledge_empty` — the backend's verdict on
 * whether the install holds any Spec, task or idea at all. The palette reads THAT for its empty
 * state; it is never inferred from the (capped) hits.
 */
function answer(hits: GlobalSearchHit[], knowledgeEmpty = false): GlobalSearchAnswer {
  return { hits, knowledge_empty: knowledgeEmpty };
}

/** Serve one fixed answer to every `global_search`, and a terminal set to `list_terminals`. */
function backendWith(
  hits: GlobalSearchHit[],
  terminals: { id: string; status: "alive" | "closed" }[] = [],
  knowledgeEmpty = false,
) {
  return (cmd: string) => {
    if (cmd === "global_search") return answer(hits, knowledgeEmpty);
    if (cmd === "list_terminals") return terminals;
    return undefined;
  };
}

/** Serve the SUGGESTIONS pull (empty query) and a typed query with two distinct answers. */
function backendServing(
  suggestions: GlobalSearchHit[],
  results: GlobalSearchHit[],
  knowledgeEmpty = false,
) {
  return (cmd: string, args?: Record<string, unknown>) =>
    cmd === "global_search"
      ? answer((args?.query as string) === "" ? suggestions : results, knowledgeEmpty)
      : undefined;
}

/**
 * An answer a test settles BY HAND — the only way to observe the palette WHILE a request is in
 * flight, which is the whole subject of the stale-while-revalidate assertions below.
 */
function deferredAnswer() {
  let resolve!: (value: GlobalSearchAnswer) => void;
  const promise = new Promise<GlobalSearchAnswer>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/** Press the palette chord on `document` — jsdom is non-mac, so `Mod` is Ctrl. */
function pressChord() {
  fireEvent.keyDown(document, { key: "k", ctrlKey: true, bubbles: true });
}

/** Focus a stand-in for the xterm textarea, and hand back its teardown. */
function focusTerminalStandIn(): HTMLTextAreaElement {
  const textarea = document.createElement("textarea");
  textarea.setAttribute("data-testid", "xterm-textarea");
  document.body.appendChild(textarea);
  textarea.focus();
  return textarea;
}

async function openPalette() {
  // The router mounts its root component ASYNCHRONOUSLY, and the palette (with its chord binding)
  // lives in it — so wait for the shell before pressing anything.
  await screen.findByTestId("active-project");
  pressChord();
  return await screen.findByRole("combobox");
}

describe("<SearchPalette> — opening, keyboard, focus", () => {
  it("opens on Mod+K and puts the keyboard in the query field", async () => {
    renderPalette(backendWith([hit("spec")]));
    const input = await openPalette();
    await waitFor(() => expect(document.activeElement).toBe(input));
  });

  it("is absent until the chord fires", () => {
    renderPalette(backendWith([]));
    expect(screen.queryByRole("combobox")).toBeNull();
  });

  it("returns the focus to the terminal that had it, on Escape", async () => {
    const terminal = focusTerminalStandIn();
    renderPalette(backendWith([hit("spec")]));
    const input = await openPalette();
    await waitFor(() => expect(document.activeElement).toBe(input));
    fireEvent.keyDown(input, { key: "Escape", bubbles: true });
    await waitFor(() => expect(document.activeElement).toBe(terminal));
    terminal.remove();
  });

  it("closes on a scrim click by the SAME path as Escape, focus restoration included", async () => {
    const terminal = focusTerminalStandIn();
    const { container } = renderPalette(backendWith([hit("spec")]));
    const input = await openPalette();
    await waitFor(() => expect(document.activeElement).toBe(input));
    // The scrim is the dialog backdrop, portalled next to the popup.
    const backdrop = document.querySelector(
      "[data-portal-layer] [role='presentation'], .fixed.inset-0",
    );
    expect(backdrop).not.toBeNull();
    fireEvent.mouseDown(backdrop!);
    fireEvent.mouseUp(backdrop!);
    fireEvent.click(backdrop!);
    await waitFor(() => expect(document.activeElement).toBe(terminal));
    expect(container).toBeTruthy();
    terminal.remove();
  });
});

describe("<SearchPalette> — the five states", () => {
  it("EMPTY: the Suggestions section, capped at 8, with the relative recency", async () => {
    const now = Date.now();
    const many = Array.from({ length: 12 }, (_, i) =>
      hit("spec", { id: `s${i}`, label: `Spec ${i}`, updated_at: now - i * 60 * 60_000 }),
    );
    renderPalette(backendWith(many));
    await openPalette();
    expect(await screen.findByText("Suggestions")).toBeTruthy();
    const list = screen.getByRole("listbox");
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(8));
    // `now` under a minute, then `Nh` under a day — the fixed bounds.
    expect(within(list).getByText("now")).toBeTruthy();
    expect(within(list).getByText("1h")).toBeTruthy();
  });

  it("RESULTS: a typed query replaces the suggestions", async () => {
    renderPalette(
      backendServing(
        [hit("spec", { id: "s0", label: "Recently touched spec" })],
        [hit("spec", { id: "s1", label: "Auth spec" })],
      ),
    );
    const input = await openPalette();
    await screen.findByText("Suggestions");
    fireEvent.change(input, { target: { value: "auth" } });
    expect(await screen.findByText("Auth spec")).toBeTruthy();
    // The suggestions are the EMPTY state's, and only that state's.
    expect(screen.queryByText("Suggestions")).toBeNull();
  });

  // REVIEW-FINDING (« Le loading en gros au milieu c'est non par contre, en gros tu me fous le
  // loading à droite de l'input, ou à la place de l'icon recherche comme ça on le voit, et le
  // contenu en dessous ne change pas tant que tu n'as pas les nouvelles données ») — both halves,
  // asserted WHILE the request is out, which is the only moment either is observable.
  //
  // The wait used to replace the panel with a centred spinner. At a 180 ms debounce restarted on
  // every keystroke, that is one collapse-and-refill per letter — the palette flickered through a
  // three-word query. It is drawn in the query FIELD now, and the body keeps the last answer.
  it("LOADING is drawn in the FIELD, and the panel keeps its content until the new answer lands", async () => {
    const pending = deferredAnswer();
    renderPalette((cmd, args) => {
      if (cmd !== "global_search") return undefined;
      if ((args?.query as string) === "")
        return answer([hit("spec", { id: "s0", label: "Recently touched spec" })]);
      return pending.promise;
    });
    const input = await openPalette();
    await screen.findByText("Suggestions");

    fireEvent.change(input, { target: { value: "auth" } });

    // WHERE the wait is drawn: in the header that holds the input, never inside the results panel.
    const announced = await screen.findByText("Searching…");
    const list = screen.getByRole("listbox");
    expect(list.contains(announced)).toBe(false);
    expect(input.parentElement?.contains(announced)).toBe(true);
    // …and it is PERCEIVABLE without the animation: a polite live region, plus the field's own
    // busy flag. A spinning glyph alone says nothing to a screen reader.
    expect(announced.getAttribute("role")).toBe("status");
    expect(announced.getAttribute("aria-live")).toBe("polite");
    expect(input.getAttribute("aria-busy")).toBe("true");

    // WHAT THE BODY SHOWS MEANWHILE: exactly what it showed before the keystroke.
    expect(screen.getByText("Suggestions")).toBeTruthy();
    expect(within(list).getAllByRole("option")).toHaveLength(1);
    expect(screen.getByText("Recently touched spec")).toBeTruthy();

    // …and it is replaced in ONE go, when the data arrives.
    pending.resolve(answer([hit("spec", { id: "s1", label: "Auth spec" })]));
    expect(await screen.findByText("Auth spec")).toBeTruthy();
    expect(screen.queryByText("Suggestions")).toBeNull();
    expect(screen.queryByText("Searching…")).toBeNull();
    expect(input.getAttribute("aria-busy")).toBe("false");
  });

  // The one moment there is nothing to keep. The answer is to draw NOTHING under the field — not a
  // section heading over an empty list, and above all not the centred spinner this finding removed.
  it("THE FIRST LOAD has nothing to preserve and shows no spinner in the panel", async () => {
    const pending = deferredAnswer();
    renderPalette((cmd) => (cmd === "global_search" ? pending.promise : undefined));
    await openPalette();

    const announced = await screen.findByText("Searching…");
    const list = screen.getByRole("listbox");
    expect(list.contains(announced)).toBe(false);
    expect(list.textContent).toBe("");
    expect(within(list).queryAllByRole("option")).toHaveLength(0);

    pending.resolve(answer([hit("spec", { id: "s0", label: "Recently touched spec" })]));
    expect(await screen.findByText("Suggestions")).toBeTruthy();
  });

  it("NO RESULTS: a dry dead end — no suggestions fall back in", async () => {
    renderPalette(backendServing([hit("spec")], []));
    const input = await openPalette();
    await screen.findByText("Suggestions");
    fireEvent.change(input, { target: { value: "nothing" } });
    expect(await screen.findByText(/No results for/)).toBeTruthy();
    expect(screen.queryByText("Suggestions")).toBeNull();
    expect(screen.queryAllByRole("option")).toHaveLength(0);
  });

  // REVIEW-FINDING (« ça marche pas pendant le loading j'ai quand même la page vide, faut que tu
  // debounce je sais pas ? » — the field read `sc`, the body read `No results for s`) : the FIRST
  // LETTER used to settle a `no-results` answer, because the sub-floor non-search shared the dead
  // end's rendering. It is not a query that found nothing, it is a query that was never asked; the
  // body keeps the suggestions it came from, and the next letter has a real answer to preserve.
  it("BELOW THE FLOOR: the first letter keeps the suggestions instead of a dead end", async () => {
    const pending = deferredAnswer();
    renderPalette((cmd, args) => {
      if (cmd !== "global_search") return undefined;
      if ((args?.query as string) === "")
        return answer([hit("spec", { id: "s0", label: "Recently touched spec" })]);
      return pending.promise;
    });
    const input = await openPalette();
    await screen.findByText("Suggestions");

    // ONE letter — under the two-character floor, so nothing is asked of the backend.
    fireEvent.change(input, { target: { value: "s" } });
    await waitFor(() => expect(input.getAttribute("aria-busy")).toBe("false"));
    expect(screen.queryByText(/No results for/)).toBeNull();
    expect(screen.getByText("Suggestions")).toBeTruthy();
    expect(screen.getByText("Recently touched spec")).toBeTruthy();

    // A SECOND letter reaches the floor: a request goes out, and what it preserves meanwhile is
    // the suggestions — not the false verdict the first letter used to leave behind.
    fireEvent.change(input, { target: { value: "sc" } });
    await screen.findByText("Searching…");
    expect(screen.queryByText(/No results for/)).toBeNull();
    expect(screen.getByText("Recently touched spec")).toBeTruthy();

    // …and when the answer lands empty, THAT is a dead end — named on the query that answered.
    pending.resolve(answer([]));
    expect(await screen.findByText(/No results for “sc”/)).toBeTruthy();

    // ERASING BACK to one letter does not re-open a second dead end either: the last real answer
    // stands, still named on `sc`.
    fireEvent.change(input, { target: { value: "s" } });
    await waitFor(() => expect(input.getAttribute("aria-busy")).toBe("false"));
    expect(screen.getByText(/No results for “sc”/)).toBeTruthy();

    // Clearing the field entirely is the one thing that DOES return to the suggestions.
    fireEvent.change(input, { target: { value: "" } });
    expect(await screen.findByText("Suggestions")).toBeTruthy();
    expect(screen.queryByText(/No results for/)).toBeNull();
  });

  it("ERROR: a rejected search says so", async () => {
    renderPalette((cmd, args) => {
      if (cmd !== "global_search") return undefined;
      if ((args?.query as string) === "") return answer([hit("spec")]);
      throw new Error("backend down");
    });
    const input = await openPalette();
    await screen.findByText("Suggestions");
    fireEvent.change(input, { target: { value: "boom" } });
    expect(await screen.findByText(/Search failed/)).toBeTruthy();
  });

  it("EMPTY INSTALL: the BACKEND's `knowledge_empty` ⇒ an EmptyState instead of the Suggestions", async () => {
    // A fresh install still has the project and workspace one just created — listing THOSE as
    // suggestions answers nothing, so the palette says there is nothing to search yet. What decides
    // is the backend's flag, not the composition of the list: those same two rows come back below
    // with the flag off, and must render as suggestions.
    renderPalette(
      backendWith(
        [
          hit("project", { id: "p1", label: "Alpha" }),
          hit("workspace", { id: "w1", label: "root" }),
        ],
        [],
        true,
      ),
    );
    await openPalette();
    expect(await screen.findByText(/Nothing to search yet/)).toBeTruthy();
    expect(screen.queryByText("Suggestions")).toBeNull();
  });

  // REVIEW-FINDING (« EmptyState "base vierge" déclenché sur une base NON vide ») : the empty state
  // used to be inferred from the SUGGESTIONS — a list capped at 8, whose head is routinely all
  // sessions, commands and workspaces on a machine where agents run (`last_seen_at` is re-stamped
  // at every start). An install holding dozens of Specs then read "Nothing to search yet".
  it("EMPTY INSTALL is NOT inferred: 8 container suggestions + `knowledge_empty` false ⇒ Suggestions", async () => {
    renderPalette(
      backendWith(
        [
          ...Array.from({ length: 4 }, (_, i) =>
            hit("session", { id: `a${i}`, label: `agent ${i}`, terminal_id: `term-${i}` }),
          ),
          ...Array.from({ length: 2 }, (_, i) =>
            hit("command", { id: `c${i}`, label: `cmd ${i}` }),
          ),
          hit("workspace", { id: "w1", label: "root" }),
          hit("project", { id: "p1", label: "Alpha" }),
        ],
        [],
        false,
      ),
    );
    await openPalette();
    expect(await screen.findByText("Suggestions")).toBeTruthy();
    expect(screen.queryByText(/Nothing to search yet/)).toBeNull();
    // The eight rows carry no Spec, no task and no idea — and are drawn all the same.
    const list = screen.getByRole("listbox");
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(8));
  });
});

describe("<SearchPalette> — results, ordering and navigation", () => {
  const everyKind = [
    hit("session", { id: "a1", label: "agent one", terminal_id: "term-1" }),
    hit("command", { id: "c1", label: "dev server" }),
    hit("project", { id: "p1", label: "Alpha" }),
    hit("workspace", { id: "w1", label: "root ws" }),
    hit("idea", { id: "i1", label: "an idea" }),
    hit("task", { id: "t1", label: "a task", spec_id: "s9" }),
    hit("spec", { id: "s1", label: "a spec" }),
  ];

  // REVIEW-FINDING (« j'écris check, pourquoi c'est le bloc spec qui sort en premier ? pendant une
  // recherche je vois pas pourquoi une certaine section devrait être plus mise en avant qu'une
  // autre, pour moi ça doit être mélangé ») : the results are ONE list in the aggregator's rank
  // order. `everyKind` is the exact fixture the old code re-ordered — it arrives with the session
  // first and the Spec last, and the fixed sequence of sections turned it upside down.
  it("draws ONE list in the backend's order, with no section heading", async () => {
    renderPalette(backendServing([], everyKind));
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "everything" } });
    await screen.findByText("a spec");
    const list = screen.getByRole("listbox");

    // No heading of any kind is drawn between the rows — `Suggestions` is the empty state's, and
    // this is not it.
    expect([...list.querySelectorAll("span.font-mono.uppercase")]).toHaveLength(0);
    // …and the seven kinds are drawn EXACTLY as answered, session first, Spec last.
    //
    // UPDATED for the ACTION REGISTRY (review 01M1E5Z93P6X0VXFPV725BP2FY): the ENTITY order is
    // unchanged — that is what this test is about, and it is still the aggregator's, untouched. The
    // two extra lines are the affordances `search-actions` hangs off the two kinds that have one,
    // each IMMEDIATELY AFTER the entity it derives from, which is the placement rule stated as an
    // assertion. They are not results and cost no cap; the seven hits are all still here, in order.
    const drawn = [...list.querySelectorAll("[role='option']")];
    expect(drawn.map((row) => row.getAttribute("data-testid"))).toEqual([
      "search-row-session-a1",
      "search-row-command-c1",
      "search-row-project-p1",
      "search-action-project:new-workspace-p1",
      "search-row-workspace-w1",
      "search-action-workspace:new-terminal-w1",
      "search-row-idea-i1",
      "search-row-task-t1",
      "search-row-spec-s1",
    ]);
  });

  // The order is the AGGREGATOR's, and it is a cross-type one: the certainty bucket first (an exact
  // label match, whatever its type), the score only inside a bucket. The renderer cannot recompute
  // it — the bucket is not a field of the wire hit — so what it owes is to draw it untouched. Here
  // the command matched exactly and the Spec only partially; the old grouping drew the Spec first
  // whatever the query, and `↵` opened the wrong row.
  it("keeps an exact command above a partial Spec — the type no longer decides", async () => {
    const { router } = renderPalette(
      backendServing(
        [],
        [
          hit("command", { id: "c1", label: "deploy", score: 0.32 }),
          hit("spec", { id: "s1", label: "rollout deploy plan", score: 0.8 }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "deploy" } });
    const command = await screen.findByTestId("search-row-command-c1");
    const rows = [...screen.getByRole("listbox").querySelectorAll("[role='option']")];
    expect(rows[0]).toBe(command);
    // …and the preselection follows the list, so `↵` opens the command.
    await waitFor(() => expect(command.getAttribute("aria-selected")).toBe("true"));
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(router.state.location.search).toEqual({ command: "c1" }));
  });

  it("names the owning project on every row (the search is multi-project)", async () => {
    renderPalette(
      backendServing(
        [],
        [
          hit("spec", { id: "s1", label: "Auth", project_id: "p1", project_name: "Alpha" }),
          hit("spec", { id: "s2", label: "Auth", project_id: "p2", project_name: "Beta" }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "auth" } });
    await screen.findByTestId("search-row-spec-s1");
    // The name comes from the HIT, not from a map handed down by the shell — `<SearchPalette>`
    // takes no props at all now.
    expect(within(screen.getByTestId("search-row-spec-s1")).getByText("Alpha")).toBeTruthy();
    expect(within(screen.getByTestId("search-row-spec-s2")).getByText("Beta")).toBeTruthy();
  });

  // REVIEW-FINDING (« Contexte projet + workspace porté par le hit et visible sur la ligne ») : an
  // entity lives in a project AND, for three kinds, in a workspace. Two instances of one `build`
  // command in two workspaces of the SAME project share their kind, their label, their command line
  // and their project — the workspace is the only thing that departs them, so it has to be on the
  // row. A kind that has no workspace draws no empty slot for one.
  it("draws project / workspace on a kind that has both, and the project alone on a kind that has not", async () => {
    renderPalette(
      backendServing(
        [],
        [
          hit("command", {
            id: "instance-root",
            label: "build",
            detail: "bun run build",
            project_name: "depot",
            workspace_id: "w-root",
            workspace_name: "root",
          }),
          hit("command", {
            id: "instance-auth",
            label: "build",
            detail: "bun run build",
            project_name: "depot",
            workspace_id: "w-auth",
            workspace_name: "auth",
          }),
          hit("idea", { id: "i1", label: "build faster", project_name: "depot" }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "build" } });
    await screen.findByTestId("search-row-command-instance-root");

    // The two command rows are IDENTICAL but for their workspace — and it is drawn. The block
    // reads DETERMINANT then PROJECT, so the workspace leads and the project closes the line. (The
    // `D` in front of the project name is its letter TILE, asserted on its own below.)
    const root = screen.getByTestId("search-row-context-command-instance-root");
    const auth = screen.getByTestId("search-row-context-command-instance-auth");
    expect(root.textContent).toBe("root·Ddepot");
    expect(auth.textContent).toBe("auth·Ddepot");
    expect(root.textContent).not.toBe(auth.textContent);

    // An IDEA lives in a project and in nothing narrower: the context is the project, full stop.
    expect(screen.getByTestId("search-row-context-idea-i1").textContent).toBe("Ddepot");
  });

  // REVIEW-FINDING (« tu penses on peut inverser ? genre projet tout à droite, ensuite le
  // déterminant genre le workspace, ou l'id de spec etc ? histoire qu'il ait tous la même
  // gueule ») : with the project FIRST, its name ended wherever the determinant behind it began,
  // so it appeared to float from line to line and nothing lined up down the right edge. The
  // project now CLOSES the block, whatever precedes it — and the determinant, the part that
  // varies, varies to its left.
  it("closes every context with the PROJECT, whatever determinant precedes it", async () => {
    renderPalette(
      backendServing(
        [],
        [
          // A workspace determinant…
          hit("command", {
            id: "c1",
            label: "build",
            project_name: "Music Manager",
            workspace_id: "w1",
            workspace_name: "root",
          }),
          // …a human KEY as determinant…
          hit("spec", { id: "s1", label: "build pipeline", human_key: "MUSICMAN-23" }),
          // …and no determinant at all.
          hit("idea", { id: "i1", label: "build faster" }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "build" } });
    await screen.findByTestId("search-row-command-c1");

    // THE ORDER, asserted on the DOM sequence rather than on a string: the last two children of
    // every block are the letter tile and the project's name, in that order — the tile stays welded
    // to the name it stands for — and whatever determinant exists comes BEFORE them.
    const cases: [string, string, string | null][] = [
      ["command-c1", "Music Manager", "root"],
      ["spec-s1", "Alpha", "MUSICMAN-23"],
      ["idea-i1", "Alpha", null],
    ];
    for (const [id, project, determinant] of cases) {
      const block = screen.getByTestId(`search-row-context-${id}`);
      const children = [...block.children];
      expect(children.at(-1)!.textContent).toBe(project);
      expect(children.at(-2)!).toBe(screen.getByTestId(`search-row-tile-${id}`));
      if (determinant === null) {
        // Nothing at all precedes the tile — no placeholder, no reserved slot, no dangling `·`.
        expect(children).toHaveLength(2);
      } else {
        expect(children[0]!.textContent).toBe(determinant);
        // …followed by the ONE separator every determinant uses, so a workspace line and a key
        // line have the same shape.
        expect(children[1]!.textContent).toBe("·");
        expect(children[1]!.getAttribute("aria-hidden")).toBe("true");
      }
    }
  });

  // REVIEW-FINDING (« par contre j'ai toujours pas les icons de projet ? ») : the marker the user
  // was naming is the rail's LETTER TILE (`D` / `K` / `M` / `S` at the far left), not the small dot
  // a previous pass put here. The dot was uniform, so it could not tell `depot` from `Music
  // Manager` — which is the whole of the request. The tile can, and it comes from the rail's own
  // `projectInitial()` so there is ONE derivation of a project's letter, not two. It is still the
  // placeholder the row's FIXME names, waiting on the real project image icon from another PRD.
  it("draws the project's LETTER TILE in the context block, one letter per project", async () => {
    renderPalette(
      backendServing(
        [],
        [
          hit("spec", { id: "s1", label: "Auth", project_name: "depot", human_key: "NYX-12" }),
          hit("command", {
            id: "c1",
            label: "build",
            project_name: "Music Manager",
            workspace_id: "w1",
            workspace_name: "root",
          }),
          // A project whose name is blank (or all whitespace): `projectInitial` falls back to `?`
          // rather than rendering an empty square.
          hit("idea", { id: "i1", label: "build faster", project_name: "   " }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "build" } });
    await screen.findByTestId("search-row-spec-s1");

    // TWO PROJECTS, TWO LETTERS — the point of the whole change: `depot` and `Music Manager` are
    // told apart at a glance, without reading the name.
    const specTile = screen.getByTestId("search-row-tile-spec-s1");
    const commandTile = screen.getByTestId("search-row-tile-command-c1");
    expect(specTile.textContent).toBe("D");
    expect(commandTile.textContent).toBe("M");
    expect(specTile.textContent).not.toBe(commandTile.textContent);

    // The rail's SHAPE, at a row's scale: a rounded square on the `muted` surface, letter centred.
    for (const tile of [specTile, commandTile]) {
      const cls = tile.getAttribute("class")!;
      expect(cls).toMatch(/\brounded\b/);
      expect(cls).toMatch(/\bbg-muted\b/);
      expect(cls).toMatch(/\bitems-center\b/);
      expect(cls).toMatch(/\bjustify-center\b/);
      // DECORATIVE: the project's name follows it in words on the same line.
      expect(tile.getAttribute("aria-hidden")).toBe("true");
    }

    // THE EMPTY-NAME FALLBACK — `projectInitial`'s own, not a second rule written here.
    expect(screen.getByTestId("search-row-tile-idea-i1").textContent).toBe("?");

    // THE MAGENTA DOT IS GONE — no `bg-primary` marker survives anywhere in a context block.
    for (const id of ["spec-s1", "command-c1", "idea-i1"]) {
      expect(
        screen.getByTestId(`search-row-context-${id}`).querySelector("span.bg-primary"),
      ).toBeNull();
    }

    // The tile sits INSIDE the context block, welded to the project name it stands for — which is
    // now the LAST thing on the line, behind the determinant.
    expect(screen.getByTestId("search-row-context-command-c1").textContent).toBe(
      "root·MMusic Manager",
    );
  });

  // REVIEW-FINDING (« c'est compliqué de voir le projet / le workspace sur lequel on bosse ») : the
  // pair was `2xs`, MONO and flat `muted-foreground` — the type of a technical identifier, which
  // the eye SKIPS when it scans. It is the second information of the line and, since the results
  // became one mixed list, the only thing saying where a line comes from.
  //
  // jsdom lays nothing out and computes no cascade, so the claim is asserted where it is actually
  // decided — the CLASSES each half carries, all of them design-system tokens (`text-xs`,
  // `text-sm`, `text-2xs`, `font-medium`, `text-foreground`, `text-muted-foreground`). What is
  // guarded is the ORDER of the four voices, not a particular value: title > project > workspace >
  // key/secondary. A theme is never asserted here: every one of these is a token that resolves per
  // theme, which is precisely why no literal colour may enter this block.
  it("draws the project / workspace pair at scanning strength, below the title", async () => {
    renderPalette(
      backendServing(
        [],
        [
          hit("command", {
            id: "c1",
            label: "build",
            project_name: "Music Manager",
            workspace_id: "w1",
            workspace_name: "root",
          }),
          hit("spec", { id: "s1", label: "Auth", project_name: "depot", human_key: "NYX-12" }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "build" } });
    await screen.findByTestId("search-row-command-c1");

    const context = screen.getByTestId("search-row-context-command-c1");
    // THE BLOCK sets the size for the names and NO LONGER sets a face: `font-mono` on the wrapper
    // was what made both names read as identifiers.
    expect(context.className).toMatch(/\btext-xs\b/);
    expect(context.className).not.toMatch(/\bfont-mono\b/);

    // THE PROJECT — near-`foreground`, `medium`. It is what tells two homonymous entities apart,
    // alongside the letter tile.
    const project = within(context).getByText("Music Manager");
    expect(project.className).toMatch(/\bfont-medium\b/);
    expect(project.className).toMatch(/\btext-foreground\/85\b/);

    // THE WORKSPACE — one step under: full `muted-foreground`, no added weight, so the pair reads
    // in an order instead of as two equal halves. It used to sit at `/80`.
    const workspace = within(context).getByText("root");
    expect(workspace.className).toMatch(/\btext-muted-foreground\b/);
    expect(workspace.className).not.toMatch(/\bfont-medium\b/);

    // THE TITLE STAYS DOMINANT: `sm` at FULL foreground, one size above the pair and the only
    // undimmed text of the row.
    const title = lineTitle(screen.getByTestId("search-row-command-c1"));
    expect(title.textContent).toBe("build");
    expect(title.className).toMatch(/\btext-sm\b/);
    expect(title.className).toMatch(/\btext-foreground\b/);

    // THE HUMAN KEY keeps the quiet mono identifier voice — raising it too would give the block
    // two competing halves and put a technical string on the same level as the project's name.
    const keyBlock = screen.getByTestId("search-row-context-spec-s1");
    const humanKey = within(keyBlock).getByText("NYX-12");
    expect(humanKey.className).toMatch(/\bfont-mono\b/);
    expect(humanKey.className).toMatch(/\btext-2xs\b/);
  });

  // REVIEW-FINDING (« Clé humaine KEY-numéro sur les lignes Spec de la palette ») : the key is the
  // identity the user reads on every other Spec surface, and the palette is the one place where two
  // homonymous Specs from two projects sit side by side. It rides its own `human_key` field rather
  // than `detail` because `detail` is SEARCHABLE backend-side — the key is drawn, never matched.
  // REVIEW-FINDING (« pour les spec avoir l'id avant c'est trop bizarre, ça crée un genre de zigzag
  // sur le texte intéressant ») : the key is variable-width (`NYX-12` against `MUSICMAN-1`), so in
  // front of the title it started every line at a different abscissa. It now rides the context
  // block at the right edge, and the title is the FIRST thing after the fixed-width glyph.
  it("draws a Spec's human key in the CONTEXT block, leaving every title aligned", async () => {
    renderPalette(
      backendServing(
        [hit("spec", { id: "s0", label: "Recently touched", human_key: "NYX-7" })],
        [
          hit("spec", { id: "s1", label: "Auth", project_id: "p1", human_key: "NYX-12" }),
          hit("spec", {
            id: "s2",
            label: "Auth",
            project_id: "p2",
            project_name: "Beta",
            human_key: "MUSICMAN-1",
          }),
          hit("task", { id: "t1", label: "Auth task", spec_id: "s9" }),
        ],
      ),
    );
    const input = await openPalette();
    // SUGGESTIONS — the empty query draws the same row, key included, in the same place.
    const suggested = await screen.findByTestId("search-row-spec-s0");
    expect(
      within(screen.getByTestId("search-row-context-spec-s0")).getByText("NYX-7"),
    ).toBeTruthy();
    expect(suggested).toBeTruthy();

    // RESULTS — two Specs sharing a title, told apart by keys of DIFFERENT lengths.
    fireEvent.change(input, { target: { value: "auth" } });
    await screen.findByTestId("search-row-spec-s1");
    const short = screen.getByTestId("search-row-context-spec-s1");
    const long = screen.getByTestId("search-row-context-spec-s2");
    expect(within(short).getByText("NYX-12")).toBeTruthy();
    expect(within(long).getByText("MUSICMAN-1")).toBeTruthy();

    // ALIGNMENT — jsdom lays nothing out, so the claim is asserted where it is actually decided:
    // the title is the first child of the row's text column on BOTH Specs, whatever the key's
    // length, and the key is no longer among that column's descendants.
    for (const id of ["s1", "s2"]) {
      const column = screen.getByTestId(`search-row-spec-${id}`).children[1];
      expect(column.firstElementChild?.textContent).toBe("Auth");
      expect(column.textContent).not.toMatch(/-\d+$/);
    }

    // A kind the schema gives no key draws none — no placeholder, and no reserved slot. (The
    // leading `A` is the project's letter tile, which every context block carries.)
    expect(screen.getByTestId("search-row-context-task-t1").textContent).toBe("AAlpha");
  });

  // REVIEW-FINDING (« les terminaux qui sont des agents, il faut afficher l'icône de l'agent en
  // question ») : a session IS its agent. The mapping is the sidebar's own registry
  // (`sidebar/agent-providers`) — one truth, consumed here, never copied — and `agent_kind` travels
  // as the hit's `detail`. The provider label on the glyph's wrapper is the same proxy
  // `terminal-item.agent-icon.test.tsx` asserts the sidebar swap through.
  it("draws the PROVIDER's logo on an agent session, and falls back to the terminal glyph", async () => {
    renderPalette(
      backendServing(
        [],
        [
          hit("session", { id: "known", label: "claude run", detail: "claude_code" }),
          hit("session", { id: "later", label: "codex run", detail: "codex" }),
          hit("session", { id: "none", label: "bare run", detail: null }),
          hit("command", { id: "c1", label: "claude", detail: "claude_code" }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "run" } });
    const known = await screen.findByTestId("search-row-session-known");
    expect(within(known).getByTitle("Claude Code")).toBeTruthy();

    // An agent_kind the registry does not know yet, and a session with none at all: the generic
    // glyph stands, unlabelled, in the same box — no gap, no shifted row.
    for (const id of ["later", "none"]) {
      const row = screen.getByTestId(`search-row-session-${id}`);
      expect(within(row).queryByTitle(/Claude/)).toBeNull();
      expect(row.children[0].className).toBe(known.children[0].className);
    }
    // …and the swap is the SESSION kind's alone: a command whose `detail` happens to read like an
    // agent kind keeps its own glyph.
    expect(
      within(screen.getByTestId("search-row-command-c1")).queryByTitle("Claude Code"),
    ).toBeNull();
  });

  // REVIEW-FINDING (« si j'ai le logo pas besoin de préciser claude_code ») : the logo already
  // answers "which agent", and the row has exactly ONE secondary line — spending it on the raw
  // `agent_kind` costs the cwd, which is the only other thing a session has to say. The removal is
  // conditioned on the LOGO, not on the kind: without one, that name is the sole trace of what
  // occupies the session, so it stays.
  it("drops the agent kind from a session's secondary line where the LOGO is drawn, keeps it on the fallback", async () => {
    renderPalette(
      backendServing(
        [],
        [
          hit("session", {
            id: "known",
            label: "claude run",
            detail: "claude_code",
            extract: "d:\\projects\\depot",
          }),
          hit("session", {
            id: "later",
            label: "codex run",
            detail: "codex",
            extract: "d:\\projects\\depot",
          }),
          hit("session", { id: "bare", label: "bare run", detail: "claude_code", extract: null }),
          hit("command", { id: "c1", label: "claude run", detail: "bun run build" }),
        ],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "run" } });
    await screen.findByTestId("search-row-session-known");

    // The text COLUMN of the row (child 1: glyph, column, context, …) is where the title and the
    // secondary line live — asserted there so the context block's own text cannot mask the claim.
    const column = (id: string) => screen.getByTestId(id).children[1].textContent!;

    // LOGO DRAWN ⇒ the path, and only the path.
    expect(column("search-row-session-known")).toBe("claude rund:\\projects\\depot");
    expect(column("search-row-session-known")).not.toMatch(/claude_code/);

    // FALLBACK (an `agent_kind` the registry does not know) ⇒ the name SURVIVES, beside the path.
    expect(column("search-row-session-later")).toBe("codex runcodex · d:\\projects\\depot");

    // A logo with nothing else to say draws NO secondary line at all — not an empty one.
    expect(column("search-row-session-bare")).toBe("bare run");

    // The other six kinds are untouched: a command still draws its `detail`.
    expect(column("search-row-command-c1")).toBe("claude runbun run build");
  });

  it("preselects the first row as soon as there are results, and opens it on Enter", async () => {
    const { router } = renderPalette(
      backendServing([], [hit("spec", { id: "s1", label: "Auth", project_id: "p2" })]),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "auth" } });
    const row = await screen.findByTestId("search-row-spec-s1");
    await waitFor(() => expect(row.getAttribute("aria-selected")).toBe("true"));
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/s1"));
    // The OWNING project is entered — before the navigation, and it persists.
    expect(screen.getByTestId("active-project").textContent).toBe("p2");
  });

  // REVIEW-FINDING (« le border primary non ») : the active row carried a vertical `primary` bar at
  // its left edge — a strong accent on an already dense line. The state is now the fill alone, at
  // the app's own selected-line strength, so it stays unmistakable while the keyboard walks.
  it("marks the active row by its FILL alone — no primary bar down the left edge", async () => {
    renderPalette(
      backendServing([], [hit("spec", { id: "s1" }), hit("task", { id: "t1", spec_id: "s9" })]),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "both" } });
    const active = await screen.findByTestId("search-row-spec-s1");
    const inactive = screen.getByTestId("search-row-task-t1");
    await waitFor(() => expect(active.getAttribute("aria-selected")).toBe("true"));

    // No lateral accent on either row — the bar is gone, and so is the transparent slot it sat in.
    for (const row of [active, inactive]) expect(row.className).not.toMatch(/border-l/);
    // …and the two are still told apart, by the background.
    expect(active.className).toMatch(/(^|\s)bg-muted(\s|$)/);
    expect(inactive.className).not.toMatch(/bg-muted/);
  });

  it("preselects NOTHING while there is no query: ↵ does not navigate until ↓ is pressed", async () => {
    const { router } = renderPalette(backendWith([hit("spec", { id: "s1", label: "Recent" })]));
    const input = await openPalette();
    await screen.findByTestId("search-row-spec-s1");
    expect(screen.getByTestId("search-row-spec-s1").getAttribute("aria-selected")).toBe("false");
    fireEvent.keyDown(input, { key: "Enter" });
    await Promise.resolve();
    expect(router.state.location.pathname).toBe("/");
    // …and `↓` is what arms it.
    fireEvent.keyDown(input, { key: "ArrowDown" });
    await waitFor(() =>
      expect(screen.getByTestId("search-row-spec-s1").getAttribute("aria-selected")).toBe("true"),
    );
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/s1"));
  });

  // REVIEW-FINDING (« Le survol souris arme une ligne en état vide ») : the no-preselection rule is
  // about what `↵` may do, not only about what the palette looks like at t=0. The palette opens at
  // `top-[12vh]`, centred — under the cursor of whoever just clicked in a pane — so a mouse that
  // merely drifts must not hand `↵` a target the keyboard never chose.
  it("EMPTY: a HOVER arms nothing — ↵ stays inert until ↓, while a click still opens", async () => {
    const { router } = renderPalette(backendWith([hit("spec", { id: "s1", label: "Recent" })]));
    const input = await openPalette();
    const row = await screen.findByTestId("search-row-spec-s1");

    fireEvent.mouseMove(row);
    expect(row.getAttribute("aria-selected")).toBe("false");
    fireEvent.keyDown(input, { key: "Enter" });
    await Promise.resolve();
    expect(router.state.location.pathname).toBe("/");
    // The entered project has NOT moved — the durable side effect the rule exists to prevent.
    expect(screen.getByTestId("active-project").textContent).toBe("none");

    // A CLICK names its row, and still activates.
    fireEvent.click(row);
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/s1"));
    expect(screen.getByTestId("active-project").textContent).toBe("p1");
  });

  it("RESULTS: a HOVER still arms the row it lands on — ↵ opens THAT one", async () => {
    const { router } = renderPalette(
      backendServing([], [hit("spec", { id: "s1" }), hit("task", { id: "t1", spec_id: "s9" })]),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "both" } });
    const task = await screen.findByTestId("search-row-task-t1");
    // The FIRST row is preselected here, so opening the second proves the hover moved the selection.
    fireEvent.mouseMove(task);
    await waitFor(() => expect(task.getAttribute("aria-selected")).toBe("true"));
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/s9"));
  });

  it("walks the list with ↑ ↓ across the groups", async () => {
    renderPalette(
      backendServing([], [hit("spec", { id: "s1" }), hit("task", { id: "t1", spec_id: "s9" })]),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "both" } });
    await screen.findByTestId("search-row-task-t1");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    await waitFor(() =>
      expect(screen.getByTestId("search-row-task-t1").getAttribute("aria-selected")).toBe("true"),
    );
    fireEvent.keyDown(input, { key: "ArrowUp" });
    await waitFor(() =>
      expect(screen.getByTestId("search-row-spec-s1").getAttribute("aria-selected")).toBe("true"),
    );
  });

  it("opens a managed command on `?command=`, and an agent session on its live terminal", async () => {
    const { router } = renderPalette(
      backendWith(
        [
          hit("command", { id: "c1", label: "dev" }),
          hit("session", { id: "a1", label: "agent", terminal_id: "term-1" }),
        ],
        [{ id: "term-1", status: "alive" }],
      ),
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "dev" } });
    fireEvent.click(await screen.findByTestId("search-row-command-c1"));
    await waitFor(() => expect(router.state.location.search).toEqual({ command: "c1" }));

    pressChord();
    const reopened = await screen.findByRole("combobox");
    fireEvent.change(reopened, { target: { value: "dev" } });
    fireEvent.click(await screen.findByTestId("search-row-session-a1"));
    await waitFor(() => expect(router.state.location.search).toEqual({ terminal: "term-1" }));
  });

  it("lands a session whose terminal is gone on `/` — no stale deep-link, no error", async () => {
    const { router } = renderPalette(
      backendWith(
        [hit("session", { id: "a1", label: "agent", terminal_id: "dead" })],
        [{ id: "term-1", status: "alive" }],
      ),
      "/knowledge",
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "agent" } });
    fireEvent.click(await screen.findByTestId("search-row-session-a1"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(router.state.location.search).toEqual({});
    expect(screen.getByTestId("active-project").textContent).toBe("p1");
  });

  it("opens an idea on /knowledge, and a workspace or a project on / — with THAT project entered", async () => {
    const { router } = renderPalette(
      backendWith([
        hit("idea", { id: "i1" }),
        hit("workspace", { id: "w1", project_id: "p2" }),
        hit("project", { id: "p2", project_id: "p2", label: "Beta" }),
      ]),
      "/work/s0",
    );
    await openPalette();
    fireEvent.click(await screen.findByTestId("search-row-idea-i1"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/knowledge"));

    pressChord();
    fireEvent.click(await screen.findByTestId("search-row-workspace-w1"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(screen.getByTestId("active-project").textContent).toBe("p2");

    pressChord();
    fireEvent.click(await screen.findByTestId("search-row-project-p2"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // Entering ANOTHER project is durable — that is the behaviour, and the reason `↵` is inert
    // while nothing is selected.
    expect(screen.getByTestId("active-project").textContent).toBe("p2");
  });

  it("opens a task on ITS Spec, and a Spec-less task on the project's `/`", async () => {
    const { router } = renderPalette(
      backendWith([
        hit("task", { id: "t1", label: "with spec", spec_id: "s9" }),
        hit("task", { id: "t2", label: "no spec", spec_id: null }),
      ]),
    );
    await openPalette();
    fireEvent.click(await screen.findByTestId("search-row-task-t1"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/s9"));

    pressChord();
    fireEvent.click(await screen.findByTestId("search-row-task-t2"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
  });
});

// REVIEW-FINDING (« j'ai demandé à aller sur le workspace scratch de la commande machin, mais le
// workspace est fermé donc c'est bizarre. donc en gros que j'arrive sur un terminal, un agent ou
// une commande, il faut que le truc ouvre le workspace en question (main compris) si il est pas
// ouvert ») : landing on the right page is not landing on the entity. A workspace band opens by
// RANK on the Terminals page, so a jump into anything but the first workspace arrived on a closed
// band. The palette raises an EPHEMERAL reveal beside the project activation; the band consumes it
// (`terminals/workspace-accordion.test.tsx` asserts that half).
describe("<SearchPalette> — the workspace reveal a jump raises", () => {
  /** Serve the hits AND record every command name the palette invoked. */
  function recordingBackend(hits: GlobalSearchHit[], invoked: string[]) {
    return (cmd: string) => {
      invoked.push(cmd);
      if (cmd === "global_search") return answer(hits);
      if (cmd === "list_terminals") return [{ id: "term-1", status: "alive" }];
      return undefined;
    };
  }

  it("signals the owning workspace on a COMMAND instance and on an agent SESSION", async () => {
    const invoked: string[] = [];
    const { router } = renderPalette(
      recordingBackend(
        [
          hit("command", {
            id: "c1",
            label: "build",
            workspace_id: "ws-scratch",
            workspace_name: "scratch",
          }),
          hit("session", {
            id: "a1",
            label: "agent",
            terminal_id: "term-1",
            workspace_id: "ws-feat",
            workspace_name: "feat",
          }),
        ],
        invoked,
      ),
      "/knowledge",
    );
    await openPalette();
    expect(screen.getByTestId("reveal-workspace").textContent).toBe("none");

    // A COMMAND INSTANCE — the very case the user reported.
    fireEvent.click(await screen.findByTestId("search-row-command-c1"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(screen.getByTestId("reveal-workspace").textContent).toBe("ws-scratch");

    // AN AGENT SESSION (and with it the terminal it is reached through) — the second of the three
    // kinds, and the one that also resolves a deep-link.
    pressChord();
    fireEvent.click(await screen.findByTestId("search-row-session-a1"));
    await waitFor(() => expect(screen.getByTestId("reveal-workspace").textContent).toBe("ws-feat"));

    // NOTHING WAS WRITTEN. The reveal is a signal, not a preference: no `set_workspace_collapsed`,
    // no `set_project_collapsed`, nothing that would outlive the arrival.
    expect(invoked).not.toContain("set_workspace_collapsed");
    expect(invoked).not.toContain("set_project_collapsed");
  });

  it("raises NO signal for a kind that lives in no workspace, and clears a previous one", async () => {
    const { router } = renderPalette(
      backendWith([
        hit("command", { id: "c1", label: "build", workspace_id: "ws-scratch" }),
        hit("spec", { id: "s1", label: "build pipeline" }),
      ]),
    );
    await openPalette();
    fireEvent.click(await screen.findByTestId("search-row-command-c1"));
    await waitFor(() =>
      expect(screen.getByTestId("reveal-workspace").textContent).toBe("ws-scratch"),
    );

    // A Spec lives in a project and in nothing narrower: its jump carries no workspace, so it
    // raises none — AND it takes the previous, now meaningless one down with it rather than
    // leaving a stale id to open some unrelated band later.
    pressChord();
    fireEvent.click(await screen.findByTestId("search-row-spec-s1"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/work/s1"));
    expect(screen.getByTestId("reveal-workspace").textContent).toBe("none");
  });
});

/**
 * REVIEW-FINDING (« tu crées une liste à côté sur toutes les actions possibles et ça les remonte
 * sur le projet / workspace correspondant ») : the palette stops being only a way to GO somewhere.
 * A workspace that surfaces offers `Open a new terminal in …`; a project offers `Add a workspace
 * to …`. Which affordance rides with which KIND is declared in `./search-actions`, and adding a
 * third is one more entry there — which is what these tests pin: the placement, the bounds and the
 * two handovers, never a hard-coded pair of rows.
 */
describe("<SearchPalette> — the action registry", () => {
  const workspaceHit = hit("workspace", {
    id: "ws-audio",
    label: "audio player",
    project_id: "p9",
    project_name: "Music Manager",
    workspace_id: "ws-audio",
    workspace_name: "audio player",
  });
  const projectHit = hit("project", { id: "p9", label: "Music Manager", project_id: "p9" });

  it("hangs the WORKSPACE action under its workspace and requests the terminal it names", async () => {
    const { router } = renderPalette(backendServing([], [workspaceHit]), "/knowledge");
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "audio" } });
    await screen.findByTestId("search-row-workspace-ws-audio");

    // THE AFFORDANCE — a verb-first label, so it reads as something one DOES rather than as a
    // second entity. It does NOT name its target any more: the context block at the right edge of
    // the same line says it (asserted in the composition test at the end of this suite).
    const action = screen.getByTestId("search-action-workspace:new-terminal-ws-audio");
    expect(lineTitle(action).textContent).toBe("Open a new terminal");
    // …drawn IMMEDIATELY AFTER the entity it derives from.
    const drawn = [...screen.getByRole("listbox").querySelectorAll("[role='option']")];
    expect(drawn.map((row) => row.getAttribute("data-testid"))).toEqual([
      "search-row-workspace-ws-audio",
      "search-action-workspace:new-terminal-ws-audio",
    ]);
    // …and it is an OPTION of the same listbox, walked and armed like every other line.
    expect(action.getAttribute("role")).toBe("option");

    // ACTIVATING IT raises the one-shot request for THIS workspace (the Terminals page turns that
    // into `newTerminalInWorkspace`, which creates AND selects — see `terminals-page.test.tsx`)…
    expect(screen.getByTestId("pending-terminal").textContent).toBe("none");
    fireEvent.click(action);
    await waitFor(() =>
      expect(screen.getByTestId("pending-terminal").textContent).toBe("ws-audio"),
    );

    // …and LANDS exactly where activating the workspace itself would: its project entered, its band
    // revealed, the Terminals page. That is what makes the created terminal appear where one looks.
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(screen.getByTestId("active-project").textContent).toBe("p9");
    expect(screen.getByTestId("reveal-workspace").textContent).toBe("ws-audio");
  });

  it("hangs the PROJECT action under its project and runs the rail's own add-workspace flow", async () => {
    const onAddWorkspace = vi.fn();
    const { router } = renderPalette(backendServing([], [projectHit]), "/knowledge", {
      onAddWorkspace,
    });
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "music" } });
    await screen.findByTestId("search-row-project-p9");

    const action = screen.getByTestId("search-action-project:new-workspace-p9");
    expect(lineTitle(action).textContent).toBe("Add a workspace");

    // The palette CREATES NOTHING: it calls back into the flow `<BaseLayout>` handed down — the
    // folder picker → name dialog → `create_workspace` the rail already drives.
    fireEvent.click(action);
    await waitFor(() => expect(onAddWorkspace).toHaveBeenCalledWith("p9"));
    expect(onAddWorkspace).toHaveBeenCalledTimes(1);
    // …and it lands in the project it is about, so the new workspace appears where one is looking.
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(screen.getByTestId("active-project").textContent).toBe("p9");
  });

  it("walks and activates an action row with the keyboard, like every other line", async () => {
    const { router } = renderPalette(backendServing([], [workspaceHit]), "/knowledge");
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "audio" } });
    const entity = await screen.findByTestId("search-row-workspace-ws-audio");
    const action = screen.getByTestId("search-action-workspace:new-terminal-ws-audio");

    // The first row is preselected as always; `↓` steps ONTO the action — one sequence, one index.
    await waitFor(() => expect(entity.getAttribute("aria-selected")).toBe("true"));
    fireEvent.keyDown(input, { key: "ArrowDown" });
    await waitFor(() => expect(action.getAttribute("aria-selected")).toBe("true"));
    expect(entity.getAttribute("aria-selected")).toBe("false");
    // `aria-activedescendant` follows it too, so assistive tech is told which line `↵` would run.
    expect(input.getAttribute("aria-activedescendant")).toBe(action.getAttribute("id"));

    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() =>
      expect(screen.getByTestId("pending-terminal").textContent).toBe("ws-audio"),
    );
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
  });

  // THE BOUNDS. An action is an affordance DERIVED from a result, not a result: it must not spend
  // the answer's room, and it must not exist where nothing was searched.
  it("adds no action to the SUGGESTIONS — nothing was searched there", async () => {
    renderPalette(backendServing([workspaceHit, projectHit], []));
    await openPalette();
    await screen.findByTestId("search-row-workspace-ws-audio");
    // Both kinds that HAVE an action are on screen, and neither drew one.
    expect(screen.getByTestId("search-row-project-p9")).toBeTruthy();
    expect(
      document.querySelectorAll("[role='option'][data-testid^='search-action-']"),
    ).toHaveLength(0);
    // Which also means the walk is the entities alone: two rows, two options.
    expect(screen.getByRole("listbox").querySelectorAll("[role='option']")).toHaveLength(2);
  });

  it("spends NEITHER cap: every entity the backend answered is still drawn", async () => {
    // A saturated answer at the aggregator's own bounds — 8 workspaces (the per-type cap) inside a
    // 40-hit reply (the overall one). If the actions were counted in, entities would be evicted.
    const workspaces = Array.from({ length: 8 }, (_, i) =>
      hit("workspace", {
        id: `ws-${i}`,
        label: `ws ${i}`,
        workspace_id: `ws-${i}`,
        workspace_name: `ws ${i}`,
      }),
    );
    const specs = Array.from({ length: 32 }, (_, i) =>
      hit("spec", { id: `s-${i}`, label: `spec ${i}` }),
    );
    renderPalette(backendServing([], [...workspaces, ...specs]));
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "ws" } });
    await screen.findByTestId("search-row-workspace-ws-0");

    // The 40 entities are ALL there — the 8 workspaces included, none pushed out…
    for (let i = 0; i < 8; i++) {
      expect(screen.getByTestId(`search-row-workspace-ws-${i}`)).toBeTruthy();
    }
    for (let i = 0; i < 32; i++) {
      expect(screen.getByTestId(`search-row-spec-s-${i}`)).toBeTruthy();
    }
    // …and the 8 action lines sit ON TOP of them, which is exactly what "they cost no cap" means.
    // `role='option'` narrows the selector to the LINES: an action line now carries a context block
    // and a letter tile of its own, whose test ids share the `search-action-` prefix.
    expect(
      document.querySelectorAll("[role='option'][data-testid^='search-action-']"),
    ).toHaveLength(8);
    expect(screen.getByRole("listbox").querySelectorAll("[role='option']")).toHaveLength(48);
  });

  // THE FOCUS HANDOVER (the finding's « le cas du dialogue »). Closing the palette normally returns
  // the keyboard to the terminal one came from — which is wrong when the close exists precisely to
  // make room for a dialog, or for a terminal that is about to focus itself. An action therefore
  // closes with `finalFocus={false}`: the focus moves NOWHERE and the incoming surface takes it
  // unopposed. Escape and a plain hit activation are untouched, which the two halves below pin
  // side by side.
  it("hands the focus over on an action, and still restores it on a plain activation", async () => {
    const terminal = focusTerminalStandIn();
    const { unmount } = renderPalette(backendServing([], [workspaceHit]), "/knowledge");
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "audio" } });
    const action = await screen.findByTestId("search-action-workspace:new-terminal-ws-audio");
    fireEvent.click(action);
    // The palette gave the keyboard to NOBODY — it did not drag it back to the terminal, which is
    // what would have fought the surface the action opens.
    await waitFor(() => expect(screen.queryByRole("combobox")).toBeNull());
    expect(document.activeElement).not.toBe(terminal);
    unmount();

    // The SAME fixture, activated on the ENTITY instead: the restoration is untouched.
    terminal.focus();
    renderPalette(backendServing([], [workspaceHit]), "/knowledge");
    const input2 = await openPalette();
    fireEvent.change(input2, { target: { value: "audio" } });
    fireEvent.click(await screen.findByTestId("search-row-workspace-ws-audio"));
    await waitFor(() => expect(document.activeElement).toBe(terminal));
    terminal.remove();
  });

  // REVIEW-FINDING (« pourquoi ça a pas la même gueule que le reste ? on a pas le projet, le
  // workspace, le texte est muted ») : an action line used to be `xs` / `muted-foreground` with no
  // context at all, sitting between two full-strength entities — so it read as a caption of the row
  // above rather than as something one may run. A DERIVED line is not a LESSER line: what says
  // "action" is the `+` glyph and the verb, never a typographic downgrade.
  //
  // jsdom lays nothing out and computes no cascade, so the claim is asserted where it is decided:
  // the CLASSES the two labels carry, and the context block's own content.
  it("composes an ACTION line exactly like an ENTITY line: same type, same context", async () => {
    renderPalette(backendServing([], [workspaceHit, projectHit]), "/knowledge");
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "music" } });
    await screen.findByTestId("search-row-workspace-ws-audio");

    const entity = screen.getByTestId("search-row-workspace-ws-audio");
    const action = screen.getByTestId("search-action-workspace:new-terminal-ws-audio");

    // SAME SIZE, SAME INTENSITY — the entity's title class, token for token, on the action's label.
    // `not.toMatch` pins the two values the downgrade used to carry, so a revert cannot pass.
    expect(lineTitle(action).className).toBe(lineTitle(entity).className);
    expect(lineTitle(action).className).toMatch(/\btext-sm\b/);
    expect(lineTitle(action).className).toMatch(/\btext-foreground\b/);
    expect(lineTitle(action).className).not.toMatch(/\btext-xs\b/);
    expect(lineTitle(action).className).not.toMatch(/\btext-muted-foreground\b/);
    // …and the glyph keeps the entity's box, so both titles start at the same abscissa.
    expect(action.children[0]!.className).toBe(entity.children[0]!.className);

    // SAME CONTEXT — the action inherits the block of the entity it derives from, names and all:
    // `audio player · [M] Music Manager` on both lines.
    const entityContext = screen.getByTestId("search-row-context-workspace-ws-audio");
    const actionContext = screen.getByTestId(
      "search-action-context-workspace:new-terminal-ws-audio",
    );
    expect(actionContext.textContent).toBe(entityContext.textContent);
    expect(actionContext.textContent).toBe("audio player·MMusic Manager");
    expect(actionContext.className).toBe(entityContext.className);
    // The project half keeps its `medium` full-intensity voice on an action line too.
    expect(within(actionContext).getByText("Music Manager").className).toMatch(/\bfont-medium\b/);

    // NO REDUNDANCY — the label says the verb, the context says the target, and neither says both.
    expect(lineTitle(action).textContent).toBe("Open a new terminal");
    expect(lineTitle(action).textContent).not.toMatch(/audio player/);

    // AN ACTION ON A PROJECT carries the project alone — there is no determinant to inherit.
    const projectAction = screen.getByTestId("search-action-project:new-workspace-p9");
    expect(lineTitle(projectAction).textContent).toBe("Add a workspace");
    expect(screen.getByTestId("search-action-context-project:new-workspace-p9").textContent).toBe(
      screen.getByTestId("search-row-context-project-p9").textContent,
    );
  });
});

/**
 * THE SYSTÈME SIDE-DOOR, driven through the real palette. `global_search` filters no project, so a
 * hit can belong to the singleton — and the singleton is absent from `list_projects`
 * (`kind = 'user'`), so posing it as the ENTERED project made the rail's restore effect "recover"
 * onto a stranger's page. Such a hit lands on the Système SCOPE's own screen instead, the exact
 * correction `use-home-focus` makes for the cross-project home's rows.
 *
 * `<BaseLayout>` hands the singleton's id down (it already holds that read for the rail's tile);
 * the mapping itself is unit-tested in `search-target.test.ts`. What is asserted here is what the
 * PALETTE does with it: the route, the scope, and what it does NOT touch.
 */
describe("<SearchPalette> — a hit that belongs to the Système singleton", () => {
  const SYS = "p-sys";

  it("lands a Système command on `/system?command=` and enters the Système scope", async () => {
    const { router } = renderPalette(
      backendWith([hit("command", { id: "sys-inst", project_id: SYS, project_name: "System" })]),
      "/",
      { systemProjectId: SYS },
    );
    const input = await openPalette();
    // The project the shell entered first, captured before the activation.
    const enteredBefore = screen.getByTestId("active-project").textContent;

    fireEvent.change(input, { target: { value: "docker" } });
    fireEvent.click(await screen.findByTestId("search-row-command-sys-inst"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/system"));
    expect(router.state.location.search).toEqual({ command: "sys-inst" });
    await waitFor(() =>
      expect(screen.getByTestId("active-project").getAttribute("data-scope")).toBe("system"),
    );
    // THE ENTERED PROJECT IS NOT TOUCHED. The singleton has no id to put there, and the project is
    // where the rail puts the user back — writing it here is precisely the old bug.
    expect(screen.getByTestId("active-project").textContent).toBe(enteredBefore);
  });

  it("takes a PROJECT hit back out of the Système scope, into the project one", async () => {
    const { router } = renderPalette(
      backendWith([hit("project", { id: "p2", project_id: "p2", label: "Beta" })]),
      "/system",
      { systemProjectId: SYS },
    );
    await openPalette();
    fireEvent.click(await screen.findByTestId("search-row-project-p2"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    // Activating a hit ENTERS what holds it — so a project's hit leaves the host scope, exactly as
    // a rail tile would. Landing on `/` while the rail still said "Système" would show the
    // project's Terminals under an empty section-nav.
    await waitFor(() =>
      expect(screen.getByTestId("active-project").getAttribute("data-scope")).toBe("project"),
    );
    expect(screen.getByTestId("active-project").textContent).toBe("p2");
  });

  it("does nothing of the sort on a database with NO singleton", async () => {
    // `system_scope_detail` answers `null` there, so the shell hands down no id and no hit can be
    // mistaken for the host's — whatever project it names.
    const { router } = renderPalette(
      backendWith([hit("command", { id: "sys-inst", project_id: SYS })]),
      "/",
    );
    const input = await openPalette();
    fireEvent.change(input, { target: { value: "docker" } });
    fireEvent.click(await screen.findByTestId("search-row-command-sys-inst"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(router.state.location.search).toEqual({ command: "sys-inst" });
    expect(screen.getByTestId("active-project").textContent).toBe(SYS);
  });
});
