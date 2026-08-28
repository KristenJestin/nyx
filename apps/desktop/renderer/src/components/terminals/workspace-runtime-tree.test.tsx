import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { emit, mockIPC } from "@/bridge/test-harness";
import { describe, expect, it } from "vitest";

import type { AgentActivity } from "@/components/sidebar/use-agent-sessions";
import { TerminalStatsProvider } from "@/components/sidebar/use-terminal-stats";
import { TerminalRenameProvider } from "@/components/sidebar/use-terminal-rename";
import { RuntimeSelectionProvider, type RuntimeSelection } from "./runtime-selection";
import { ConsumedNotificationsProvider } from "./use-consumed-notifications";
import { TerminalCloseProvider } from "./use-terminal-close";
import { WorkspaceRuntimeTree } from "./workspace-runtime-tree";
import type {
  WorkspaceRuntimeSession,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "@/components/runtime/use-workspace-runtime";

function terminal(overrides: Partial<WorkspaceRuntimeTerminal> = {}): WorkspaceRuntimeTerminal {
  return {
    id: "t1",
    cwd: "/repo",
    label: "t1",
    workspace_id: "ws1",
    status: "alive",
    exec_state: "idle",
    exec_state_unread: false,
    sessions: [],
    ...overrides,
  };
}

function session(state: string): WorkspaceRuntimeSession {
  return {
    id: "sess1",
    terminal_id: "t1",
    workspace_id: "ws1",
    agent_kind: "claude_code",
    external_session_id: "ext1",
    state,
  };
}

function snapshot(overrides: Partial<WorkspaceRuntimeSnapshot> = {}): WorkspaceRuntimeSnapshot {
  return {
    workspace: { id: "ws1", name: "root", path: "/repo" },
    terminals: [],
    agent_sessions: [],
    services: [],
    ...overrides,
  };
}

function renderTree(
  snap: WorkspaceRuntimeSnapshot,
  initialSelection: RuntimeSelection | null = null,
) {
  return render(
    <RuntimeSelectionProvider initialSelection={initialSelection}>
      <ConsumedNotificationsProvider>
        <WorkspaceRuntimeTree snapshot={snap} />
      </ConsumedNotificationsProvider>
    </RuntimeSelectionProvider>,
  );
}

/** The presentational state a row's dot renders (`data-state`), or null when no dot. */
function dotStateFor(label: string): string | null {
  const row = screen.getByText(label).closest("[data-selected]") as HTMLElement | null;
  return row?.querySelector("[data-state]")?.getAttribute("data-state") ?? null;
}

describe("WorkspaceRuntimeTree", () => {
  it("has exactly TWO sections (Terminals, Commands) — no 3rd Agents category", () => {
    renderTree(snapshot());
    expect(screen.getByText("Terminals")).toBeTruthy();
    expect(screen.getByText("Commands")).toBeTruthy();
    expect(screen.queryByText("Agents")).toBeNull();
  });

  it("renders a terminal row labelled by its label (else cwd)", () => {
    renderTree(
      snapshot({
        terminals: [
          {
            id: "t1",
            cwd: "/repo",
            label: "build",
            workspace_id: "ws1",
            status: "alive",
            exec_state: "running",
            exec_state_unread: false,
            sessions: [],
          },
        ],
      }),
    );
    expect(screen.getByText("build")).toBeTruthy();
  });

  it("surfaces a live agent ON its host terminal (provider glyph, no meta text), not a 3rd category", () => {
    renderTree(
      snapshot({
        terminals: [
          {
            id: "t1",
            cwd: "/repo",
            label: "agent-term",
            workspace_id: "ws1",
            status: "alive",
            exec_state: "running",
            exec_state_unread: false,
            sessions: [session("working")],
          },
        ],
      }),
    );
    // The agent lives on its terminal (glyph = provider logo); the row is name + dot only,
    // so the provider label no longer rides as trailing meta text.
    expect(screen.getByText("agent-term")).toBeTruthy();
    expect(screen.queryByText("Claude Code")).toBeNull();
    // Still no separate Agents section.
    expect(screen.queryByText("Agents")).toBeNull();
  });

  it("renders a service row by its name, with no trailing command meta", () => {
    renderTree(
      snapshot({
        services: [
          {
            id: "s1",
            command_id: "c1",
            workspace_id: "ws1",
            name: "dev-server",
            command: "npm run dev",
            run_state: "running",
            exit_code: null,
            unread: false,
          },
        ],
      }),
    );
    expect(screen.getByText("dev-server")).toBeTruthy();
    // The command line is no longer shown as row meta (it surfaces in the pane instead).
    expect(screen.queryByText("npm run dev")).toBeNull();
  });

  it("shows empty hints when a section has no rows", () => {
    renderTree(snapshot());
    expect(screen.getByText("No open terminals.")).toBeTruthy();
    expect(screen.getByText("No managed commands.")).toBeTruthy();
  });
});

describe("WorkspaceRuntimeTree — notification dots (finding 53)", () => {
  it("shows the settled success dot on an UNREAD terminal that is NOT selected", () => {
    renderTree(
      snapshot({
        terminals: [
          terminal({ id: "t1", label: "built", exec_state: "success", exec_state_unread: true }),
        ],
      }),
    );
    expect(dotStateFor("built")).toBe("success");
  });

  it("does NOT show the settled dot on the currently-SELECTED terminal", () => {
    renderTree(
      snapshot({
        terminals: [
          terminal({ id: "t1", label: "built", exec_state: "error", exec_state_unread: true }),
        ],
      }),
      { kind: "terminal", id: "t1" },
    );
    // The viewed terminal never shows its notification dot (you are on it → seen).
    expect(dotStateFor("built")).toBeNull();
  });

  it("KEEPS the running dot on the selected terminal (running is not a consumable notification)", () => {
    renderTree(
      snapshot({
        terminals: [terminal({ id: "t1", label: "dev", exec_state: "running" })],
      }),
      { kind: "terminal", id: "t1" },
    );
    expect(dotStateFor("dev")).toBe("running");
  });

  it("CONSUMES the terminal notification on select — marks read + clears the dot", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      return undefined;
    });
    renderTree(
      snapshot({
        terminals: [
          terminal({ id: "t1", label: "built", exec_state: "success", exec_state_unread: true }),
        ],
      }),
    );
    // The dot is visible before the visit.
    expect(dotStateFor("built")).toBe("success");
    // Selecting the terminal marks it read (backend `terminal_exec_mark_read`) and hides the dot.
    fireEvent.click(screen.getByText("built"));
    await waitFor(() => {
      const markRead = calls.find((c) => c.cmd === "terminal_exec_mark_read");
      expect(markRead?.args.id).toBe("t1");
    });
    // The row is now the selected one, so the notification dot leaves (exit-animates then unmounts).
    await waitFor(() => expect(dotStateFor("built")).toBeNull());
  });

  it("CONSUMES a service notification on select via command_acknowledge", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      return undefined;
    });
    renderTree(
      snapshot({
        services: [
          {
            id: "s1",
            command_id: "c1",
            workspace_id: "ws1",
            name: "build",
            command: "npm run build",
            run_state: "error",
            exit_code: 1,
            unread: true,
          },
        ],
      }),
    );
    expect(dotStateFor("build")).toBe("error");
    fireEvent.click(screen.getByText("build"));
    await waitFor(() => {
      const ack = calls.find((c) => c.cmd === "command_acknowledge");
      expect(ack?.args.instanceId).toBe("s1");
    });
    await waitFor(() => expect(dotStateFor("build")).toBeNull());
  });

  it("keeps a consumed dot GONE after leaving the terminal (visited → stays read this session)", async () => {
    mockIPC(() => undefined);
    // Two unread-success terminals; the snapshot's `exec_state_unread` stays true (mark-read does
    // not re-pull it), so only the page-local consumed record can keep a visited dot hidden.
    renderTree(
      snapshot({
        terminals: [
          terminal({ id: "t1", label: "one", exec_state: "success", exec_state_unread: true }),
          terminal({ id: "t2", label: "two", exec_state: "success", exec_state_unread: true }),
        ],
      }),
    );
    expect(dotStateFor("one")).toBe("success");
    // Visit t1 (consumes it), then leave it by visiting t2.
    fireEvent.click(screen.getByText("one"));
    await waitFor(() => expect(dotStateFor("one")).toBeNull());
    fireEvent.click(screen.getByText("two"));
    // t1 is no longer selected, but its notification stays consumed (does not resurface).
    await waitFor(() => expect(dotStateFor("two")).toBeNull());
    expect(dotStateFor("one")).toBeNull();
  });
});

describe("WorkspaceRuntimeTree — per-row rename + close (v2.5.6)", () => {
  function renderWithActions(
    snap: WorkspaceRuntimeSnapshot,
    {
      onClose,
      onRename,
    }: { onClose: (id: string) => void; onRename: (id: string, label: string | null) => void },
  ) {
    return render(
      <RuntimeSelectionProvider>
        <ConsumedNotificationsProvider>
          <TerminalRenameProvider rename={onRename}>
            <TerminalCloseProvider close={onClose}>
              <WorkspaceRuntimeTree snapshot={snap} />
            </TerminalCloseProvider>
          </TerminalRenameProvider>
        </ConsumedNotificationsProvider>
      </RuntimeSelectionProvider>,
    );
  }

  it("a per-row close button closes the RIGHT terminal by id (not just the selected one)", () => {
    const closed: string[] = [];
    renderWithActions(
      snapshot({
        terminals: [terminal({ id: "t1", label: "one" }), terminal({ id: "t2", label: "two" })],
      }),
      { onClose: (id) => closed.push(id), onRename: () => {} },
    );
    // Close the SECOND row: its "×" targets t2 by id, independent of any selection.
    fireEvent.click(screen.getByRole("button", { name: "Close terminal two" }));
    expect(closed).toEqual(["t2"]);
  });

  it("inline rename pins a MANUAL label via the rename callback (pencil → type → Enter)", () => {
    const renames: Array<[string, string | null]> = [];
    renderWithActions(snapshot({ terminals: [terminal({ id: "t1", label: "old" })] }), {
      onClose: () => {},
      onRename: (id, label) => renames.push([id, label]),
    });
    fireEvent.click(screen.getByRole("button", { name: "Rename terminal old" }));
    const input = screen.getByRole("textbox", { name: "Rename terminal old" });
    fireEvent.change(input, { target: { value: "new-name" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(renames).toEqual([["t1", "new-name"]]);
  });

  it("committing an EMPTY rename clears the label back to auto-naming (null)", () => {
    const renames: Array<[string, string | null]> = [];
    renderWithActions(snapshot({ terminals: [terminal({ id: "t1", label: "old" })] }), {
      onClose: () => {},
      onRename: (id, label) => renames.push([id, label]),
    });
    fireEvent.click(screen.getByRole("button", { name: "Rename terminal old" }));
    const input = screen.getByRole("textbox", { name: "Rename terminal old" });
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(renames).toEqual([["t1", null]]);
  });

  it("Escape cancels the inline rename (no callback, name restored)", () => {
    const renames: unknown[] = [];
    renderWithActions(snapshot({ terminals: [terminal({ id: "t1", label: "old" })] }), {
      onClose: () => {},
      onRename: (...args) => renames.push(args),
    });
    fireEvent.click(screen.getByRole("button", { name: "Rename terminal old" }));
    const input = screen.getByRole("textbox", { name: "Rename terminal old" });
    fireEvent.change(input, { target: { value: "x" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(renames).toEqual([]);
    expect(screen.getByText("old")).toBeTruthy();
  });

  it("a double-click on the name opens the inline editor", () => {
    renderWithActions(snapshot({ terminals: [terminal({ id: "t1", label: "old" })] }), {
      onClose: () => {},
      onRename: () => {},
    });
    fireEvent.doubleClick(screen.getByText("old"));
    expect(screen.getByRole("textbox", { name: "Rename terminal old" })).toBeTruthy();
  });

  it("the inline editor is h-5 (matches the label line-box) so entering rename keeps the row height (finding #25)", () => {
    // jsdom has no layout to measure a pixel height, so we guard the ROOT of the +4px jump: the
    // editor Input was h-6 (24px) while the label's line-box is ~20px (text-sm). Under the shared
    // py-1.5 wrapper that grew the row on edit; h-5 (20px) matches the label so the height holds.
    renderWithActions(snapshot({ terminals: [terminal({ id: "t1", label: "old" })] }), {
      onClose: () => {},
      onRename: () => {},
    });
    fireEvent.click(screen.getByRole("button", { name: "Rename terminal old" }));
    const input = screen.getByRole("textbox", { name: "Rename terminal old" });
    expect(input.className).toContain("h-5");
    expect(input.className).not.toContain("h-6");
  });
});

describe("WorkspaceRuntimeTree — agent activity on the LINE (review T2)", () => {
  function agent(overrides: Partial<AgentActivity> = {}): AgentActivity {
    return {
      activity: "idle",
      readyUnread: false,
      errorUnread: false,
      pluginOutdated: false,
      ...overrides,
    };
  }

  function renderWithActivity(
    snap: WorkspaceRuntimeSnapshot,
    byId: Record<string, AgentActivity>,
    selection: RuntimeSelection | null = null,
  ) {
    return render(
      <RuntimeSelectionProvider initialSelection={selection}>
        <ConsumedNotificationsProvider>
          <WorkspaceRuntimeTree snapshot={snap} agentActivity={(id) => byId[id]} />
        </ConsumedNotificationsProvider>
      </RuntimeSelectionProvider>,
    );
  }

  const agentTerminal = () =>
    terminal({ id: "t1", label: "agent", exec_state: "idle", sessions: [session("active")] });

  it("shows the agent WORKING dot (running/blue) on its host LINE", () => {
    renderWithActivity(snapshot({ terminals: [agentTerminal()] }), {
      t1: agent({ activity: "working" }),
    });
    expect(dotStateFor("agent")).toBe("running");
  });

  it("shows the agent WAITING dot (yellow) on its host LINE — not just the workspace rollup", () => {
    renderWithActivity(snapshot({ terminals: [agentTerminal()] }), {
      t1: agent({ activity: "waiting" }),
    });
    expect(dotStateFor("agent")).toBe("waiting");
  });

  it("shows the agent READY dot (success/green) on an UNREAD, unselected agent LINE", () => {
    renderWithActivity(snapshot({ terminals: [agentTerminal()] }), {
      t1: agent({ activity: "idle", readyUnread: true }),
    });
    expect(dotStateFor("agent")).toBe("success");
  });

  it("shows the agent turn-ERROR dot (red) with priority over ready", () => {
    renderWithActivity(snapshot({ terminals: [agentTerminal()] }), {
      t1: agent({ activity: "idle", readyUnread: true, errorUnread: true }),
    });
    expect(dotStateFor("agent")).toBe("error");
  });

  it("agent activity PRIMES over the terminal mode (busy/exec_state) on an agent line", () => {
    // The host terminal has a settled unread SUCCESS exec_state AND is busy — but because it
    // hosts an agent whose activity is `waiting`, the LINE shows `waiting`, not the terminal mode.
    const t = terminal({
      id: "t1",
      label: "agent",
      exec_state: "success",
      exec_state_unread: true,
      busy: true,
      sessions: [session("active")],
    });
    renderWithActivity(snapshot({ terminals: [t] }), { t1: agent({ activity: "waiting" }) });
    expect(dotStateFor("agent")).toBe("waiting");
  });

  it("falls back to TERMINAL mode when the line has NO agent session (busy → running, T1)", () => {
    // No session on the terminal → terminal mode: the OS busy bit lights `running` even with an
    // (irrelevant) activity entry present for its id.
    const t = terminal({ id: "t1", label: "plain", exec_state: "idle", busy: true, sessions: [] });
    renderWithActivity(snapshot({ terminals: [t] }), { t1: agent({ activity: "idle" }) });
    expect(dotStateFor("plain")).toBe("running");
  });

  it("idle agent line (busy, NO live activity entry) shows NO dot — busy/exec_state IGNORED (bug fix)", () => {
    // THE BUG: a fresh/idle Claude session sits at the prompt as the foreground process (OS `busy`
    // ON) and may carry a stale settled exec_state, yet has NO activity entry. It must show an
    // EMPTY dot — a line that carries an agent is in AGENT mode, which ignores the busy bit AND the
    // exec_state — not the busy-derived `running` (nor a phantom green). This is v1 parity.
    const t = terminal({
      id: "t1",
      label: "idle-agent",
      exec_state: "success",
      exec_state_unread: true,
      busy: true,
      sessions: [session("active")],
    });
    renderWithActivity(snapshot({ terminals: [t] }), {});
    expect(dotStateFor("idle-agent")).toBeNull();
  });

  it("NON-REGRESSION: a NON-agent terminal that is busy STILL shows the running dot (T1)", () => {
    // A plain PTY (no session, no detected agent) running a foreground command must still light the
    // busy `running` dot — only an agent line ignores busy.
    const t = terminal({
      id: "t1",
      label: "plain-busy",
      exec_state: "idle",
      busy: true,
      sessions: [],
    });
    renderWithActivity(snapshot({ terminals: [t] }), {});
    expect(dotStateFor("plain-busy")).toBe("running");
  });

  it("CONSUMES the agent ready notification on select via agent_mark_ready_read", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      return undefined;
    });
    renderWithActivity(snapshot({ terminals: [agentTerminal()] }), {
      t1: agent({ activity: "idle", readyUnread: true }),
    });
    expect(dotStateFor("agent")).toBe("success");
    // Selecting the agent line clears its ready dot via the agent seam (NOT terminal_exec_mark_read).
    fireEvent.click(screen.getByText("agent"));
    await waitFor(() => {
      const markRead = calls.find((c) => c.cmd === "agent_mark_ready_read");
      expect(markRead?.args.terminalId).toBe("t1");
    });
    // The viewed agent line suppresses its settled dot (you are on it → seen).
    await waitFor(() => expect(dotStateFor("agent")).toBeNull());
    // It did NOT fire the terminal exec mark-read (agent mode owns its own clear).
    expect(calls.some((c) => c.cmd === "terminal_exec_mark_read")).toBe(false);
  });
});

describe("WorkspaceRuntimeTree — per-terminal CPU%/RAM is NON-inline (tooltip, FEEDBACK #28)", () => {
  // The firm constraint of the stats task: the live CPU%/RAM reading surfaces ONLY on hover as
  // the row's tooltip, so it never competes with the terminal name for row width (v1 put stats on
  // the item line and they masked/truncated the name). These tests exercise the actual placement
  // wiring — `terminal://stats` reading → `useTerminalStatsFor` → RuntimeRow `tooltip=` → the
  // `<Tooltip>` around the select-button — which the pure formatter tests do not reach.

  // Render the tree UNDER the real stats provider so a `terminal://stats` event folds through the
  // same context seam the row reads (no direct context injection — the whole chain runs).
  function renderTreeWithStats(snap: WorkspaceRuntimeSnapshot) {
    return render(
      <RuntimeSelectionProvider>
        <ConsumedNotificationsProvider>
          <TerminalStatsProvider>
            <WorkspaceRuntimeTree snapshot={snap} />
          </TerminalStatsProvider>
        </ConsumedNotificationsProvider>
      </RuntimeSelectionProvider>,
    );
  }

  // Push one live reading for a terminal through the `terminal://stats` channel (snake_case wire
  // shape) and let React fold it into the shared map.
  async function emitStats(terminalId: string, cpuPct: number, memBytes: number) {
    await act(async () => {
      await emit("terminal://stats", {
        terminal_id: terminalId,
        cpu_pct: cpuPct,
        mem_bytes: memBytes,
      });
    });
  }

  it("keeps CPU%/RAM OUT of the row line — it is not rendered inline even after a reading arrives", async () => {
    mockIPC(() => undefined);
    renderTreeWithStats(snapshot({ terminals: [terminal({ id: "t1", label: "build" })] }));
    // 42.4% CPU → "42%", 340 MiB RAM → "340 MB": the tooltip string is "CPU 42% · RAM 340 MB".
    await emitStats("t1", 42.4, 340 * 1024 * 1024);

    // The name is on the row line, fully visible in its OWN text node…
    const name = await screen.findByText("build");
    const row = name.closest("[data-selected]") as HTMLElement;
    expect(within(row).getByText("build")).toBeTruthy();
    // …and the stats never ride on that row line (non-inline): the CPU/RAM string is NOT inside the
    // row element at rest — it lives in the hover tooltip (a portal), so it cannot mask/truncate
    // the name. The name node itself is just the label, not the label + stats.
    expect(within(row).queryByText(/CPU/)).toBeNull();
    expect(name.textContent).toBe("build");
  });

  it("surfaces the CPU%/RAM reading as the row's HOVER tooltip (name still its own element)", async () => {
    mockIPC(() => undefined);
    renderTreeWithStats(snapshot({ terminals: [terminal({ id: "t1", label: "build" })] }));
    await emitStats("t1", 42.4, 340 * 1024 * 1024);

    // Before hover the stats are nowhere in the document — purely on-demand.
    expect(screen.queryByText(/CPU 42% · RAM 340 MB/)).toBeNull();

    // Hover the select-button (the tooltip trigger wrapping the name) → the reading opens as a
    // tooltip, rendered in the portal OUTSIDE the row, so the name is never covered. Base UI's
    // hover opens on the pointer coming to REST (`restMs`), so a `mouseMove` primes the rest timer.
    const trigger = screen.getByText("build").closest("button") as HTMLButtonElement;
    fireEvent.pointerEnter(trigger);
    fireEvent.mouseEnter(trigger);
    fireEvent.mouseMove(trigger);
    const tip = await waitFor(() => screen.getByText(/CPU 42% · RAM 340 MB/), { timeout: 2000 });

    // The tooltip text is a DISTINCT node from the name span (stats are non-inline, the name is
    // rendered separately and stays whole).
    expect(tip).not.toBe(screen.getByText("build"));
    expect(tip.textContent).toBe("CPU 42% · RAM 340 MB");
    expect(screen.getByText("build").textContent).toBe("build");
  });

  it("shows NO tooltip until a reading arrives (a terminal with no live stats is untouched)", () => {
    mockIPC(() => undefined);
    renderTreeWithStats(snapshot({ terminals: [terminal({ id: "t1", label: "quiet" })] }));
    // No `terminal://stats` emitted → `statsTooltip` is undefined → the row has no tooltip wrapper.
    const trigger = screen.getByText("quiet").closest("button") as HTMLButtonElement;
    fireEvent.pointerEnter(trigger);
    fireEvent.mouseEnter(trigger);
    expect(screen.queryByText(/CPU/)).toBeNull();
    expect(screen.getByText("quiet")).toBeTruthy();
  });
});
