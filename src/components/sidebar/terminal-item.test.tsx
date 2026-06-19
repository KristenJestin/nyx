import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { DragDropProvider } from "@dnd-kit/react";
import { describe, expect, it, vi } from "vitest";

import { TerminalItem } from "./terminal-item";
import { SortableTerminalItem } from "./sortable-terminal-item";
import type { TerminalInfo } from "./auto-label";
import type { TerminalRecord } from "./use-terminals";

function row(
  id: number,
  cwd: string,
  label: string | null = null,
  exec_state: TerminalRecord["exec_state"] = "idle",
  exec_state_unread = false,
  busy = false,
): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    exec_state,
    exec_state_unread,
    busy,
  };
}

/** Mock the `terminal_info` command to return a fixed live reading. */
function mockTerminalInfo(info: TerminalInfo): void {
  mockIPC((cmd) => {
    if (cmd === "terminal_info") return info;
    return null;
  });
}

describe("<TerminalItem> auto-naming", () => {
  it("auto-names from the cwd basename when only the shell runs", async () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    render(
      <ul>
        <TerminalItem
          record={row(1, "/home/x/projetA")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>,
    );
    await waitFor(() => expect(screen.getByText("projetA")).toBeInTheDocument());
  });

  it("reflects the foreground program in the auto label (htop)", async () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "htop" });
    render(
      <ul>
        <TerminalItem
          record={row(1, "/home/x/projetA")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>,
    );
    await waitFor(() => expect(screen.getByText("projetA · htop")).toBeInTheDocument());
  });

  it("a MANUAL label wins over the live auto label", async () => {
    // Even though terminal_info would yield "projetA · htop", the manual label
    // is shown.
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "htop" });
    render(
      <ul>
        <TerminalItem
          record={row(1, "/home/x/projetA", "my-shell")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>,
    );
    // The manual name renders…
    expect(screen.getByText("my-shell")).toBeInTheDocument();
    // …and stays put even after a poll cycle could have applied the auto label.
    await new Promise((r) => setTimeout(r, 30));
    expect(screen.getByText("my-shell")).toBeInTheDocument();
    expect(screen.queryByText("projetA · htop")).not.toBeInTheDocument();
  });

  it("has NO inline rename: double-clicking the name shows no edit input (finding 01KV3CNPD…)", () => {
    // The double-click-to-edit inline rename was removed entirely. Double-clicking
    // the name must NOT reveal a text input — rows are click-select + hover-close
    // only now.
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    render(
      <ul>
        <TerminalItem
          record={row(7, "/home/x/projetA")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>,
    );

    fireEvent.doubleClick(screen.getByText("projetA"));
    expect(screen.queryByLabelText(/rename terminal/i)).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
  });
});

describe("<SortableTerminalItem> whole-item drag (dnd-kit)", () => {
  it("a plain CLICK on the name selects the terminal (drag does not swallow the click)", () => {
    // The WHOLE row is the drag affordance, but a click (no pointer movement past
    // the sensor distance) must still fire onSelect → focus-on-activate. The row
    // (li) owns the click, so a tap anywhere — name included — selects.
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onSelect = vi.fn();
    render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(7, "/home/x/projetA")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={onSelect}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    // Click the name → selection fires with the record id.
    fireEvent.click(screen.getByText("projetA"));
    expect(onSelect).toHaveBeenCalledWith("7");
  });

  it("CLICK-ANYWHERE: clicking the ROW itself (below the text — no dead zone) selects", () => {
    // Finding 01KV3CND2…: a click anywhere on the row (not just the name) selects,
    // because the row element (li) owns the click.
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onSelect = vi.fn();
    render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(7, "/home/x/projetA")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={onSelect}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    // Click the row LISTITEM (the whole-row affordance), not the inner name.
    fireEvent.click(screen.getByRole("listitem"));
    expect(onSelect).toHaveBeenCalledWith("7");
  });

  it("there is NO separate grip handle — the whole row is draggable", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(7, "/home/x/projetA")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    // No grip handle is rendered (whole-item drag).
    expect(screen.queryByLabelText(/reorder terminal/i)).toBeNull();
  });

  it("renders the shared magenta ActiveRail ONLY on the active row (selection channel)", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    // Two rows: only the active one carries aria-current (a single shared layoutId
    // bar glides between them — finding 01KV304Y7WA5YPZBAFJ7V4ANHX).
    const { rerender } = render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(1, "/a")}
            index={0}
            active
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
          <SortableTerminalItem
            record={row(2, "/b")}
            index={1}
            active={false}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    // The active row carries aria-current; the inactive one does not.
    const actives = screen
      .getAllByRole("listitem")
      .filter((li) => li.getAttribute("aria-current") === "true");
    expect(actives).toHaveLength(1);
    // Move selection to row 2 → still exactly one active row.
    rerender(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(1, "/a")}
            index={0}
            active={false}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
          <SortableTerminalItem
            record={row(2, "/b")}
            index={1}
            active
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    expect(
      screen.getAllByRole("listitem").filter((li) => li.getAttribute("aria-current") === "true"),
    ).toHaveLength(1);
  });

  it("shows the settled badge while UNREAD and HIDES it once READ — even when inactive again (PRD-2.1 user story #3)", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    // UNREAD success on an inactive terminal → badge present (the persisted flag,
    // not selection, drives visibility).
    const { rerender } = render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(1, "/a", null, "success", true)}
            index={0}
            active={false}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    expect(screen.getByRole("status", { name: /terminal status: success/i })).toBeInTheDocument();
    // After the user viewed it the record is READ (`exec_state_unread = false`).
    // Re-deselecting (active=false) must NOT re-show the badge — the bug the
    // persisted-flag refactor fixes (a purely `active`-driven badge would re-appear).
    rerender(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(1, "/a", null, "success", false)}
            index={0}
            active={false}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    expect(screen.queryByRole("status", { name: /terminal status: success/i })).toBeNull();
  });

  it("the RUNNING dot is driven by the OS `busy` flag, NOT by exec_state (PRD task #1)", async () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    // busy=true with exec_state='idle' (no OSC 133) → the running badge appears
    // after the anti-flicker delay. Proves the dot comes from the OS signal.
    render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(1, "/a", null, "idle", false, true)}
            index={0}
            active={false}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /terminal status: running/i })).toBeInTheDocument(),
    );
  });

  it("a legacy exec_state='running' (OSC 133) with busy=false shows NO running dot", async () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    // exec_state says 'running' (the OLD OSC-133 source) but the OS busy flag is
    // false: the dot must NOT show running — busy is the sole running authority now.
    render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(1, "/a", null, "running", false, false)}
            index={0}
            active={false}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </DragDropProvider>,
    );
    // Give the debounce window time to elapse; the running badge must never appear.
    await new Promise((r) => setTimeout(r, 150));
    expect(screen.queryByRole("status", { name: /terminal status: running/i })).toBeNull();
  });

  it("clicking the close (x) closes WITHOUT selecting (stopPropagation kept)", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onSelect = vi.fn();
    const onClose = vi.fn();
    render(
      <DragDropProvider>
        <ul>
          <SortableTerminalItem
            record={row(7, "/home/x/projetA")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={onSelect}
            onClose={onClose}
          />
        </ul>
      </DragDropProvider>,
    );
    fireEvent.click(screen.getByLabelText(/close terminal/i));
    expect(onClose).toHaveBeenCalledWith("7");
    expect(onSelect).not.toHaveBeenCalled();
  });
});
