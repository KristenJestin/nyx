import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";

// The open panel headers now render `<OverlayPanelActions>`, which calls `useNavigate`. This suite
// mounts `<SpecOverlay>` with NO router, so stub `useNavigate` to a no-op (navigation targets are
// asserted in `overlay-panel-actions.test.tsx`); all other router exports stay real.
vi.mock("@tanstack/react-router", async (importActual) => ({
  ...(await importActual<typeof import("@tanstack/react-router")>()),
  useNavigate: () => () => Promise.resolve(),
}));

import type { Spec, SpecDetail, SpecRevision, Task } from "../specs/types";
import * as overlayMotion from "./overlay-motion";
import {
  OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID,
  overlayArrivalTransition,
  overlayArrivalVariants,
  overlayMotionTransition,
  overlayPanelVariants,
  overlaySurfaceVariants,
  overlayTabVariants,
} from "./overlay-motion";
import { SpecOverlay } from "./spec-overlay";

/** A `nyx_core::db::Spec` fixture (the shape `terminal_specs` returns per Spec). */
function spec(overrides: Partial<Spec>): Spec {
  return {
    id: "s1",
    project_id: "p1",
    workspace_id: null,
    title: "Untitled spec",
    slug: null,
    number: 1,
    priority: "med",
    status: "in_progress",
    mode: "stepped",
    phases: "[]",
    current_revision_number: 1,
    position: 0,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    archived_at: null,
    ...overrides,
  };
}

/** A `tasks` row fixture. */
function task(overrides: Partial<Task>): Task {
  return {
    id: "t1",
    project_id: "p1",
    spec_id: "s1",
    parent_task_id: null,
    kind: "spec",
    executor: "agent",
    type_: "",
    status: "todo",
    priority: "med",
    severity: null,
    title: "A task",
    description: null,
    acceptance_criteria: null,
    verification_command: null,
    phase: null,
    position: 0,
    blocked_reason: null,
    skip_reason: null,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    from_comment_id: null,
    grill_state: null,
    round: null,
    ...overrides,
  };
}

/** A `spec_detail` read-model fixture; `revision` overrides let a test set section bodies. */
function detail(s: Spec, tasks: Task[], revision: Partial<SpecRevision> = {}): SpecDetail {
  return {
    spec: s,
    revision: {
      id: "r1",
      spec_id: s.id,
      revision_number: 1,
      revision_status: "active",
      title: s.title,
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
      ...revision,
    },
    tasks,
    dependencies: [],
    workspace_context: null,
  };
}

const ACTIVE_SESSION = { id: "sess1", agent_kind: "claude_code", state: "active" };

describe("<SpecOverlay> — rail A (dev advancement)", () => {
  it("renders NOTHING when the terminal has no active session (non-regression)", async () => {
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: null, specs: [] };
      throw new Error(`unexpected ${cmd}`);
    });
    const { container } = render(<SpecOverlay terminalId="term-x" />);
    // Give the resolver a tick; the overlay must stay empty (AnimatePresence adds no DOM wrapper).
    await waitFor(() => expect(container).toBeEmptyDOMElement());
  });

  it("shows a SIMPLE closed rail tab (no Spec name) for a Spec in dev, and opens a FIDÈLE tasks panel on click", async () => {
    const devSpec = spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" });
    const tasks = [
      task({ id: "t1", title: "Wire the login form", status: "done", phase: 1 }),
      task({ id: "t2", title: "Blocked backend piece", status: "todo", phase: 1 }),
    ];
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [devSpec] };
      if (cmd === "spec_detail") return detail(devSpec, tasks);
      throw new Error(`unexpected ${cmd}`);
    });

    render(<SpecOverlay terminalId="term-1" />);

    // The closed tab carries an accessible label but NOT the Spec name (rail = SIMPLE onglet).
    const tab = await screen.findByRole("button", { name: /open auth revamp progress/i });
    expect(tab).toBeInTheDocument();
    expect(tab).not.toHaveTextContent("Auth revamp");
    // The languette is the shared animated OverlayTab (data-overlay-tab), living inside the animated
    // surface (data-overlay-surface) — the enter/leave + languette motion wrappers (tasks 4 + 5).
    expect(tab).toHaveAttribute("data-overlay-tab");
    expect(tab.closest("[data-overlay-surface]")).not.toBeNull();

    // Click opens the panel; the task rows come from the REAL `<TasksTab>` (fidèle rendering).
    fireEvent.click(tab);
    expect(await screen.findByText("Wire the login form")).toBeInTheDocument();
    expect(screen.getByText("Blocked backend piece")).toBeInTheDocument();
    // The panel header carries the status badge.
    expect(screen.getByText("In progress")).toBeInTheDocument();

    // A chevron closes it again.
    fireEvent.click(screen.getByRole("button", { name: /close progress panel/i }));
    await waitFor(() => expect(screen.queryByText("Wire the login form")).not.toBeInTheDocument());
  });

  it("re-pulls the Spec set live on `specs://changed` (no polling)", async () => {
    let currentSpecs: Spec[] = [];
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: currentSpecs };
      if (cmd === "spec_detail") return detail(spec({ id: "s1", status: "in_progress" }), []);
      throw new Error(`unexpected ${cmd}`);
    });

    render(<SpecOverlay terminalId="term-1" />);
    // No mapped Spec yet ⇒ no tab.
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /progress/i })).not.toBeInTheDocument(),
    );

    // A dev Spec gets mapped; the push makes the overlay appear WITHOUT any polling.
    currentSpecs = [spec({ id: "s1", number: 3, title: "New work", status: "in_progress" })];
    await emit("specs://changed");
    expect(
      await screen.findByRole("button", { name: /open new work progress/i }),
    ).toBeInTheDocument();
  });
});

describe("<SpecOverlay> — tiroir B (creation live)", () => {
  it("shows a SOBER closed tab (name + status point) for a Spec being authored, and opens the live drawer on click", async () => {
    const draft = spec({ id: "s1", number: 4, title: "Payments spec", status: "draft" });
    const built = detail(draft, [task({ id: "t1", title: "Design the ledger", status: "todo" })], {
      problem: "Users cannot pay.",
    });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [draft] };
      if (cmd === "spec_detail") return built;
      throw new Error(`unexpected ${cmd}`);
    });

    render(<SpecOverlay terminalId="term-1" />);

    // A sober tab labelled with the Spec name (the drawer onglet DOES carry the name, unlike A).
    const tab = await screen.findByRole("button", { name: /open payments spec live spec/i });
    expect(tab).toHaveTextContent("Payments spec");
    // No progress gauge on the drawer tab (it is a dev-only affordance).
    expect(screen.queryByRole("button", { name: /progress/i })).not.toBeInTheDocument();

    // Click opens the live drawer: a REAL status badge (Draft), a real section, and streamed tasks.
    fireEvent.click(tab);
    // Real status via the global `statusMeta` — a draft reads "Draft", NOT the old "Creating" tag.
    expect(await screen.findByText("Draft")).toBeInTheDocument();
    expect(screen.queryByText("Creating")).not.toBeInTheDocument();
    expect(screen.getByText("Users cannot pay.")).toBeInTheDocument();
    expect(screen.getByText("Design the ledger")).toBeInTheDocument();
    // The SpecTab banner (revision / "agent-written · read-only") is SUPPRESSED in the drawer.
    expect(screen.queryByText(/agent-written/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/revision v/i)).not.toBeInTheDocument();
    // Footer is a status line only (no CTA / button beyond the close chevron), keyed off real status.
    expect(screen.getByText(/Draft · .* · 1 task/)).toBeInTheDocument();
  });

  it("colours the drawer tab status dot from the GLOBAL statusMeta (badge colour), keeping the live pulse for a draft", async () => {
    // A `draft` authored while the session is LIVE: the dot takes the statusMeta(draft) colour
    // (`bg-muted-foreground/50`, the same status source as the panel badge), NOT the old bespoke
    // `var(--product-claude)` accent — and it still breathes (`animate-pulse`, vivacité signal).
    const draft = spec({ id: "s1", number: 4, title: "Draft spec", status: "draft" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [draft] };
      if (cmd === "spec_detail") return detail(draft, []);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open draft spec live spec/i });
    // The dot is labelled with the global status label and carries the statusMeta `dot` token.
    const dot = tab.querySelector('[aria-label="Draft"]');
    expect(dot).not.toBeNull();
    expect(dot?.className).toMatch(/bg-muted-foreground\/50/);
    expect(dot?.className).not.toMatch(/product-claude/);
    // The live-authoring pulse is retained on top of the statusMeta colour.
    expect(dot?.className).toMatch(/animate-pulse/);
  });

  it("colours a READY drawer tab dot with the statusMeta ready colour (bg-info), not the old bespoke green", async () => {
    // `ready` used to force a bespoke `bg-success`; it now follows statusMeta(ready) = `bg-info`
    // (the same tone as the panel's Ready badge), and does not pulse (not a live draft).
    const ready = spec({ id: "s1", number: 5, title: "Ready spec", status: "ready" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [ready] };
      if (cmd === "spec_detail") return detail(ready, []);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open ready spec live spec/i });
    const dot = tab.querySelector('[aria-label="Ready"]');
    expect(dot).not.toBeNull();
    expect(dot?.className).toMatch(/bg-info/);
    expect(dot?.className).not.toMatch(/bg-success/);
    expect(dot?.className).not.toMatch(/animate-pulse/);
  });

  it("fills the sections LIVE as the agent writes them (re-pull on `specs://changed`)", async () => {
    const draft = spec({ id: "s1", number: 5, title: "Live draft", status: "draft" });
    let currentDetail = detail(draft, []);
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [draft] };
      if (cmd === "spec_detail") return currentDetail;
      throw new Error(`unexpected ${cmd}`);
    });

    render(<SpecOverlay terminalId="term-1" />);
    fireEvent.click(await screen.findByRole("button", { name: /open live draft live spec/i }));
    // No section content yet.
    await waitFor(() =>
      expect(screen.getByText(/This revision has no content yet/i)).toBeInTheDocument(),
    );

    // The agent writes the Solution section; the push refreshes the drawer without polling.
    currentDetail = detail(draft, [], { solution: "Introduce a payment gateway." });
    await emit("specs://changed");
    expect(await screen.findByText("Introduce a payment gateway.")).toBeInTheDocument();
  });
});

describe("<SpecOverlay> — multi-Spec stacking (tiroir only)", () => {
  const drafts = [
    spec({ id: "s1", number: 1, title: "Ingest spec", status: "draft" }),
    spec({ id: "s2", number: 2, title: "Transform spec", status: "ready" }),
    spec({ id: "s3", number: 3, title: "Export spec", status: "draft" }),
  ];

  function mountMulti() {
    mockIPC((cmd, args) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: drafts };
      if (cmd === "spec_detail") {
        const s = drafts.find((d) => d.id === (args?.specId as string)) ?? drafts[0];
        return detail(s, [task({ id: `${s.id}-t`, title: `${s.title} task`, status: "todo" })]);
      }
      throw new Error(`unexpected ${cmd}`);
    });
    return render(<SpecOverlay terminalId="term-1" />);
  }

  it("stacks N sober tabs (fixed size, cap+scroll) — clicking one opens its panel while the others stay tabs", async () => {
    mountMulti();

    // N Specs ⇒ N sober tabs.
    const t1 = await screen.findByRole("button", { name: /open ingest spec live spec/i });
    const t2 = screen.getByRole("button", { name: /open transform spec live spec/i });
    const t3 = screen.getByRole("button", { name: /open export spec live spec/i });
    // Fixed tile size on each tab (uniform stack).
    expect(t1.className).toMatch(/\bh-36\b/);
    // The stack container caps + scrolls (never overflows the pane).
    const stack = t1.parentElement as HTMLElement;
    expect(stack.className).toMatch(/overflow-y-auto/);
    expect(stack.className).toMatch(/max-h-full/);

    // Click the 2nd tab: ITS live drawer opens; the OTHER two stay as tabs beside it (and so does
    // the 2nd — its languette is now the ACTIVE fused handle, not dropped).
    fireEvent.click(t2);
    expect(
      await screen.findByRole("region", { name: /transform spec live spec/i }, { timeout: 3000 }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /open ingest spec live spec/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /open export spec live spec/i })).toBeInTheDocument();
    // Only ONE panel open at a time in the STABLE state — switching lets the previous panel EXIT
    // while the next ENTERS (overlap, no `mode="wait"`), so once the export region has SETTLED the
    // transform region is gone. (A transient two-panel overlap mid-switch is intended — see the
    // "never shows an empty shell" test.)
    fireEvent.click(t3);
    expect(
      await screen.findByRole("region", { name: /export spec live spec/i }, { timeout: 3000 }),
    ).toBeInTheDocument();
    await waitFor(
      () =>
        expect(
          screen.queryByRole("region", { name: /transform spec live spec/i }),
        ).not.toBeInTheDocument(),
      { timeout: 3000 },
    );
  });

  it("never shows an empty shell during a Spec switch — the new panel MOUNTS while the old EXITS (no mode=wait empty frame)", async () => {
    mountMulti();

    // Open the first Spec's panel.
    fireEvent.click(await screen.findByRole("button", { name: /open ingest spec live spec/i }));
    await screen.findByRole("region", { name: /ingest spec live spec/i });
    expect(document.querySelectorAll("[data-overlay-panel]")).toHaveLength(1);

    // Switch to the third Spec. With `mode="wait"` the new panel would only mount AFTER the old one
    // finished exiting — leaving the 380px shell EMPTY (zero panels) for that gap. Without it, the
    // new panel is in the DOM IMMEDIATELY (enter overlaps exit), so a panel is ALWAYS present.
    fireEvent.click(screen.getByRole("button", { name: /open export spec live spec/i }));
    // Synchronously after the switch: the NEW Spec's panel is already mounted (overlap, not deferred)
    // and at no instant is the shell panel-less.
    expect(document.querySelector('[data-overlay-panel-spec="s3"]')).not.toBeNull();
    expect(document.querySelectorAll("[data-overlay-panel]").length).toBeGreaterThanOrEqual(1);

    // Once it settles, exactly ONE panel remains and it is the new (export) Spec.
    await waitFor(
      () => {
        const panels = document.querySelectorAll("[data-overlay-panel]");
        expect(panels).toHaveLength(1);
        expect(panels[0].getAttribute("data-overlay-panel-spec")).toBe("s3");
      },
      { timeout: 3000 },
    );
  });

  it("keeps the rail (dev) a SINGLE simple tab with no name and no stacking, even with several dev Specs", async () => {
    const devs = [
      spec({ id: "d1", number: 8, title: "Dev one", status: "in_progress" }),
      spec({ id: "d2", number: 9, title: "Dev two", status: "in_review" }),
    ];
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: devs };
      if (cmd === "spec_detail") return detail(devs[0], []);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);

    // Exactly ONE progress tab (no stacking on the rail), and it carries NO Spec name.
    const tabs = await screen.findAllByRole("button", { name: /progress/i });
    expect(tabs).toHaveLength(1);
    expect(tabs[0]).not.toHaveTextContent("Dev one");
    expect(tabs[0]).not.toHaveTextContent("Dev two");
  });
});

describe("<SpecOverlay> — model A (languette-poignée fusionnée + switch animé)", () => {
  it("keeps the open Spec's languette mounted as an ACTIVE FUSED handle (shared surface, no shared border); a re-click closes", async () => {
    const draft = spec({ id: "s1", number: 4, title: "Lonely spec", status: "draft" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [draft] };
      if (cmd === "spec_detail") return detail(draft, [task({ id: "t1", title: "Only task" })]);
      throw new Error(`unexpected ${cmd}`);
    });

    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open lonely spec live spec/i });
    // CLOSED: the languette is dimmed (bg-muted) with its full border — not fused.
    expect(tab.className).toMatch(/bg-muted/);
    expect(tab.className).not.toMatch(/border-r-0/);
    expect(tab).toHaveAttribute("aria-pressed", "false");

    // Open: the languette STAYS mounted (it is NOT dropped) and FUSES with the panel — it shares the
    // panel's surface (bg-card/95) and drops the border on the shared (right) edge (border-r-0).
    fireEvent.click(tab);
    await screen.findByRole("region", { name: /lonely spec live spec/i });
    const activeTab = screen.getByRole("button", { name: /open lonely spec live spec/i });
    expect(activeTab).toBeInTheDocument();
    expect(activeTab).toHaveAttribute("aria-pressed", "true");
    expect(activeTab.className).toMatch(/bg-card\/95/);
    expect(activeTab.className).toMatch(/border-r-0/);

    // Re-clicking the ACTIVE languette closes its panel (it is the close handle).
    fireEvent.click(activeTab);
    await waitFor(() =>
      expect(
        screen.queryByRole("region", { name: /lonely spec live spec/i }),
      ).not.toBeInTheDocument(),
    );
  });

  it("dims the INACTIVE languettes (bg-muted, no shared-edge fusion) while another Spec is open", async () => {
    const drafts = [
      spec({ id: "s1", number: 1, title: "Alpha spec", status: "draft" }),
      spec({ id: "s2", number: 2, title: "Beta spec", status: "draft" }),
    ];
    mockIPC((cmd, args) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: drafts };
      if (cmd === "spec_detail") {
        const s = drafts.find((d) => d.id === (args?.specId as string)) ?? drafts[0];
        return detail(s, []);
      }
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const alpha = await screen.findByRole("button", { name: /open alpha spec live spec/i });
    fireEvent.click(alpha);
    await screen.findByRole("region", { name: /alpha spec live spec/i });

    // Alpha is the ACTIVE fused languette; Beta stays a dimmed, fully-bordered closed languette.
    const activeAlpha = screen.getByRole("button", { name: /open alpha spec live spec/i });
    const inactiveBeta = screen.getByRole("button", { name: /open beta spec live spec/i });
    expect(activeAlpha.className).toMatch(/bg-card\/95/);
    expect(activeAlpha.className).toMatch(/border-r-0/);
    expect(inactiveBeta.className).toMatch(/bg-muted/);
    expect(inactiveBeta.className).not.toMatch(/border-r-0/);
    expect(inactiveBeta.className).not.toMatch(/bg-card\/95/);
  });

  it("marks the ACTIVE languette with a coloured accent bar + vivid bold text; INACTIVE ones stay bare + greyed", async () => {
    const drafts = [
      spec({ id: "s1", number: 1, title: "Alpha spec", status: "draft" }),
      spec({ id: "s2", number: 2, title: "Beta spec", status: "draft" }),
    ];
    mockIPC((cmd, args) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: drafts };
      if (cmd === "spec_detail") {
        const s = drafts.find((d) => d.id === (args?.specId as string)) ?? drafts[0];
        return detail(s, []);
      }
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const alpha = await screen.findByRole("button", { name: /open alpha spec live spec/i });
    const beta = screen.getByRole("button", { name: /open beta spec live spec/i });

    // CLOSED: no accent bar on any languette, and greyed text.
    expect(alpha.querySelector("[data-overlay-active-indicator]")).toBeNull();
    expect(beta.querySelector("[data-overlay-active-indicator]")).toBeNull();
    expect(alpha.className).toMatch(/text-foreground\/70/);

    // Open Alpha: it gains a `bg-primary` accent bar carrying the shared layoutId + bold vivid text;
    // Beta (inactive) has NO accent bar.
    fireEvent.click(alpha);
    await screen.findByRole("region", { name: /alpha spec live spec/i });
    const activeAlpha = screen.getByRole("button", { name: /open alpha spec live spec/i });
    const inactiveBeta = screen.getByRole("button", { name: /open beta spec live spec/i });
    const bar = activeAlpha.querySelector("[data-overlay-active-indicator]");
    expect(bar).not.toBeNull();
    expect(bar?.className).toMatch(/bg-primary/);
    expect(bar?.getAttribute("data-layout-id")).toBe(OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID);
    expect(activeAlpha.className).toMatch(/font-semibold/);
    expect(activeAlpha.className).toMatch(/text-foreground(?!\/)/);
    expect(inactiveBeta.querySelector("[data-overlay-active-indicator]")).toBeNull();

    // Exactly ONE accent bar exists in the whole overlay (the shared, travelling indicator).
    expect(document.querySelectorAll("[data-overlay-active-indicator]")).toHaveLength(1);
  });

  it("travels the SINGLE accent bar to the newly-active languette when the Spec switches", async () => {
    const drafts = [
      spec({ id: "s1", number: 1, title: "Alpha spec", status: "draft" }),
      spec({ id: "s2", number: 2, title: "Beta spec", status: "draft" }),
    ];
    mockIPC((cmd, args) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: drafts };
      if (cmd === "spec_detail") {
        const s = drafts.find((d) => d.id === (args?.specId as string)) ?? drafts[0];
        return detail(s, []);
      }
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    fireEvent.click(await screen.findByRole("button", { name: /open alpha spec live spec/i }));
    await screen.findByRole("region", { name: /alpha spec live spec/i });
    // The bar lives on Alpha.
    let alpha = screen.getByRole("button", { name: /open alpha spec live spec/i });
    expect(alpha.querySelector("[data-overlay-active-indicator]")).not.toBeNull();

    // Switch to Beta: still exactly ONE bar, now on Beta and gone from Alpha (it moved, not duplicated).
    fireEvent.click(screen.getByRole("button", { name: /open beta spec live spec/i }));
    await screen.findByRole("region", { name: /beta spec live spec/i }, { timeout: 3000 });
    await waitFor(() => {
      const beta = screen.getByRole("button", { name: /open beta spec live spec/i });
      expect(beta.querySelector("[data-overlay-active-indicator]")).not.toBeNull();
      alpha = screen.getByRole("button", { name: /open alpha spec live spec/i });
      expect(alpha.querySelector("[data-overlay-active-indicator]")).toBeNull();
      expect(document.querySelectorAll("[data-overlay-active-indicator]")).toHaveLength(1);
    });
  });

  it("highlights a hovered languette IN PLACE — a hover colour class, never a translate", async () => {
    const draft = spec({ id: "s1", number: 4, title: "Hover spec", status: "draft" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [draft] };
      if (cmd === "spec_detail") return detail(draft, []);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open hover spec live spec/i });
    // Hover is a background LIGHTEN (no displacement): a `hover:bg-*` class, and NO translate class.
    expect(tab.className).toMatch(/hover:bg-/);
    expect(tab.className).not.toMatch(/translate/);
  });

  it("re-keys the open panel by spec.id so SWITCHING Specs replays the full animation (remount, not in-place)", async () => {
    const drafts = [
      spec({ id: "s1", number: 1, title: "Ingest spec", status: "draft" }),
      spec({ id: "s2", number: 2, title: "Transform spec", status: "ready" }),
    ];
    mockIPC((cmd, args) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: drafts };
      if (cmd === "spec_detail") {
        const s = drafts.find((d) => d.id === (args?.specId as string)) ?? drafts[0];
        return detail(s, []);
      }
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);

    // Open ingest: the panel wrapper is identified/keyed by its spec.id.
    fireEvent.click(await screen.findByRole("button", { name: /open ingest spec live spec/i }));
    await screen.findByRole("region", { name: /ingest spec live spec/i });
    const panel1 = document.querySelector("[data-overlay-panel]");
    expect(panel1?.getAttribute("data-overlay-panel-spec")).toBe("s1");

    // Switch to transform: keyed by spec.id ⇒ the old panel EXITS and a NEW node ENTERS (a stable
    // key would keep the SAME node + only swap content). Assert the panel is a fresh node for s2.
    fireEvent.click(screen.getByRole("button", { name: /open transform spec live spec/i }));
    await screen.findByRole("region", { name: /transform spec live spec/i }, { timeout: 3000 });
    await waitFor(
      () => {
        const panel2 = document.querySelector("[data-overlay-panel]");
        expect(panel2?.getAttribute("data-overlay-panel-spec")).toBe("s2");
        expect(panel2).not.toBe(panel1);
      },
      { timeout: 3000 },
    );
  });

  it("re-clicking the RAIL gauge languette (active) closes its panel", async () => {
    const devSpec = spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [devSpec] };
      if (cmd === "spec_detail") return detail(devSpec, [task({ id: "t1", title: "Wire it" })]);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open auth revamp progress/i });
    fireEvent.click(tab); // open
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).not.toBeNull());
    // The rail languette is the coherent fused handle: active ⇒ shares the panel surface.
    expect(tab.className).toMatch(/bg-card\/95/);
    expect(tab.className).toMatch(/border-r-0/);
    fireEvent.click(tab); // re-click the active languette closes it
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).toBeNull());
  });

  it("animates the RAIL panel open/close via an AnimatePresence wrapper (no brutal tab↔panneau swap)", async () => {
    const devSpec = spec({ id: "s1", number: 7, title: "Auth revamp", status: "in_progress" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [devSpec] };
      if (cmd === "spec_detail") return detail(devSpec, [task({ id: "t1", title: "Wire it" })]);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open auth revamp progress/i });
    // Closed: the panel is not mounted, so there is nothing to pop in — only the languette shows.
    expect(document.querySelector("[data-overlay-panel]")).toBeNull();
    // Open: the panel mounts inside its animated wrapper (slides in, not a raw swap).
    fireEvent.click(tab);
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).not.toBeNull());
    // The languette stays mounted underneath (so its one-shot arrival ring is NOT re-triggered).
    expect(tab).toBeInTheDocument();
    // Close: the panel runs its exit animation and then unmounts (the reverse transition).
    fireEvent.click(screen.getByRole("button", { name: /close progress panel/i }));
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).toBeNull());
  });

  it("animates the TIROIR panel open/close via an AnimatePresence wrapper (no brutal openSpec swap)", async () => {
    const draft = spec({ id: "s1", number: 4, title: "Payments spec", status: "draft" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [draft] };
      if (cmd === "spec_detail") return detail(draft, []);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open payments spec live spec/i });
    expect(document.querySelector("[data-overlay-panel]")).toBeNull();
    fireEvent.click(tab);
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: /close live spec panel/i }));
    await waitFor(() => expect(document.querySelector("[data-overlay-panel]")).toBeNull());
  });

  it("flashes a one-shot arrival ring on a newly-mounted languette when motion is allowed", async () => {
    // The default matchMedia stub (setup) reports motion ALLOWED, so the arrival ring mounts on the
    // freshly-mapped Spec's languette (task 5: a pulse/highlight when a NEW tab arrives).
    const draft = spec({ id: "s1", number: 6, title: "Ring spec", status: "draft" });
    mockIPC((cmd) => {
      if (cmd === "terminal_specs") return { session: ACTIVE_SESSION, specs: [draft] };
      if (cmd === "spec_detail") return detail(draft, []);
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SpecOverlay terminalId="term-1" />);
    const tab = await screen.findByRole("button", { name: /open ring spec live spec/i });
    expect(tab.querySelector("[data-overlay-arrival]")).not.toBeNull();
  });
});

describe("overlay-motion (variants + reduced-motion resolvers) — pure", () => {
  it("SURFACE slides in from the right + fades, and reverses on exit (apparition/disparition)", () => {
    expect(overlaySurfaceVariants.initial).toMatchObject({ opacity: 0 });
    expect(overlaySurfaceVariants.animate).toMatchObject({ opacity: 1, x: 0 });
    expect(overlaySurfaceVariants).toHaveProperty("exit");
    // A non-zero x offset on initial + exit = the slide-in / slide-out travel.
    expect((overlaySurfaceVariants.initial as { x: number }).x).toBeGreaterThan(0);
    expect((overlaySurfaceVariants.exit as { x: number }).x).toBeGreaterThan(0);
  });

  it("PANEL is OPACITY-ONLY (no horizontal slide) — movement is width-driven; zIndex keeps the entering panel on top", () => {
    // Crossfade: opacity ramps 0→1→0 across initial/animate/exit.
    expect((overlayPanelVariants.initial as { opacity: number }).opacity).toBe(0);
    expect(overlayPanelVariants.animate).toMatchObject({ opacity: 1 });
    expect((overlayPanelVariants.exit as { opacity: number }).opacity).toBe(0);
    // NO horizontal slide on the panel content: the open/close movement is driven by the width-
    // animated shell (0↔380), so the content stays flush to the left edge and the fused languette
    // stays collée (regression fix — a content `x` slide decoupled it and read as a pop on close).
    expect((overlayPanelVariants.initial as { x?: number }).x).toBeUndefined();
    expect((overlayPanelVariants.animate as { x?: number }).x).toBeUndefined();
    expect((overlayPanelVariants.exit as { x?: number }).x).toBeUndefined();
    // zIndex keeps the ENTERING panel above the EXITING one so the switch crossfade never shows a
    // hole / empty frame (initial/animate on top, exit below).
    expect((overlayPanelVariants.initial as { zIndex: number }).zIndex).toBeGreaterThan(
      (overlayPanelVariants.exit as { zIndex: number }).zIndex,
    );
    expect((overlayPanelVariants.animate as { zIndex: number }).zIndex).toBeGreaterThan(
      (overlayPanelVariants.exit as { zIndex: number }).zIndex,
    );
  });

  it("LANGUETTE attaches from the right (enter/exit x offset); the hover-translate constant is GONE", () => {
    expect(overlayTabVariants.animate).toMatchObject({ opacity: 1, x: 0 });
    expect((overlayTabVariants.initial as { x: number }).x).toBeGreaterThan(0);
    // Hover no longer displaces the languette: the old `OVERLAY_TAB_HOVER = { x: -3 }` translate is
    // removed (hover is now a pure colour highlight — no `x` prop, no layout shift).
    expect((overlayMotion as Record<string, unknown>).OVERLAY_TAB_HOVER).toBeUndefined();
  });

  it("exposes a single SHARED layoutId for the active languette's travelling accent bar", () => {
    // A non-empty shared id: the active indicator carries it on WHICHEVER languette is open, so
    // Motion FLIPs the one bar between languettes on a switch instead of popping a fresh one.
    expect(typeof OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID).toBe("string");
    expect(OVERLAY_ACTIVE_INDICATOR_LAYOUT_ID.length).toBeGreaterThan(0);
  });

  it("ARRIVAL ring fades from visible to transparent (a one-shot highlight)", () => {
    expect((overlayArrivalVariants.initial as { opacity: number }).opacity).toBeGreaterThan(0);
    expect(overlayArrivalVariants.animate).toMatchObject({ opacity: 0 });
  });

  it("collapses the surface/languette spring to instant under reduced motion, a spring otherwise", () => {
    expect(overlayMotionTransition(true)).toMatchObject({ duration: 0 });
    const t = overlayMotionTransition(false) as Record<string, unknown>;
    expect(t.type).toBe("spring");
    // `null` (unknown preference) is treated as motion allowed.
    expect((overlayMotionTransition(null) as Record<string, unknown>).type).toBe("spring");
  });

  it("collapses the arrival ring transition to instant under reduced motion", () => {
    expect(overlayArrivalTransition(true)).toMatchObject({ duration: 0 });
    expect(overlayArrivalTransition(false)).not.toMatchObject({ duration: 0 });
  });
});
