import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@/bridge/test-harness";
import { DragDropProvider } from "@dnd-kit/react";
import { describe, expect, it, vi } from "vitest";

import { TerminalItem } from "./terminal-item";
import { SortableTerminalItem } from "./sortable-terminal-item";
import { TerminalRenameProvider } from "./use-terminal-rename";
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
});

describe("<TerminalItem> shell suffix declutter (FEEDBACK #29, 2nd pass)", () => {
  it("a non-agent BASH row shows NO `· bash` suffix and renders the FULL name", async () => {
    // The screenshot bug: a plain shell row showed a muted `· bash` the user rejected, and
    // that `shrink-0` suffix ate the width so `palbank` was pre-truncated to `pal…`. With the
    // suffix dropped for a bare login shell, the name keeps the full row width.
    mockTerminalInfo({ cwd: "/home/x/palbank", foreground: "bash" });
    render(
      <ul>
        <TerminalItem
          record={row(1, "/home/x/palbank")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>,
    );
    // The full name is rendered (not pre-truncated — the DOM text is the whole word; the
    // visual ellipsis is CSS-only).
    await waitFor(() => expect(screen.getByText("palbank")).toBeInTheDocument());
    // No muted `· bash` suffix anywhere on the row.
    expect(screen.queryByText(/· bash/)).not.toBeInTheDocument();
    expect(screen.queryByText("bash")).not.toBeInTheDocument();
  });

  it("suppresses the suffix for OTHER bare login shells too (zsh)", async () => {
    mockTerminalInfo({ cwd: "/home/x/palbank", foreground: "-zsh" });
    render(
      <ul>
        <TerminalItem
          record={row(1, "/home/x/palbank")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
        />
      </ul>,
    );
    await waitFor(() => expect(screen.getByText("palbank")).toBeInTheDocument());
    expect(screen.queryByText(/· zsh/)).not.toBeInTheDocument();
  });

  it("STILL shows the suffix for a real foreground program (vim) — genuinely useful", async () => {
    // The decluttering only targets bare shells; a real program suffix stays (it tells the
    // user what is running) so the row keeps the muted `· vim` beside the name. The auto
    // label itself also reflects the program (`projetA · vim`), so we target the muted suffix
    // span by its class to assert the suffix specifically renders.
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "vim" });
    const { container } = render(
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
    await waitFor(() => {
      const suffix = container.querySelector("span.text-muted-foreground.text-xs");
      expect(suffix?.textContent).toBe("· vim");
    });
  });
});

describe("<TerminalItem> rename (FEEDBACK #30)", () => {
  it("double-clicking the name opens an inline editor; Enter commits a MANUAL label (trimmed)", async () => {
    // The auto label would be "projetA"; renaming pins a manual one that wins.
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onRename = vi.fn();
    render(
      <TerminalRenameProvider rename={onRename}>
        <ul>
          <TerminalItem
            record={row(7, "/home/x/projetA")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </TerminalRenameProvider>,
    );

    // Double-click the auto name → an inline editor appears.
    fireEvent.doubleClick(screen.getByText("projetA"));
    const input = screen.getByLabelText(/rename terminal/i) as HTMLInputElement;
    expect(input).toBeInTheDocument();

    // Type a new name (with surrounding whitespace) and commit with Enter.
    fireEvent.change(input, { target: { value: "  my-shell  " } });
    fireEvent.keyDown(input, { key: "Enter" });

    // The rename callback fires with the TRIMMED manual label.
    expect(onRename).toHaveBeenCalledWith("7", "my-shell");
    // The editor closes (back to a plain name span).
    await waitFor(() => expect(screen.queryByLabelText(/rename terminal/i)).toBeNull());
  });

  it("the kebab 'Rename' action opens the same inline editor", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    render(
      <TerminalRenameProvider rename={vi.fn()}>
        <ul>
          <TerminalItem
            record={row(7, "/home/x/projetA")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </TerminalRenameProvider>,
    );

    // Open the row's actions kebab, then click "Rename".
    fireEvent.click(screen.getByLabelText(/terminal actions for/i));
    fireEvent.click(screen.getByRole("menuitem", { name: /rename terminal/i }));
    expect(screen.getByLabelText(/rename terminal/i)).toBeInTheDocument();
  });

  it("an empty name CLEARS the label back to auto-naming (null)", () => {
    // The terminal already has a manual label; clearing it returns to the auto name.
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onRename = vi.fn();
    render(
      <TerminalRenameProvider rename={onRename}>
        <ul>
          <TerminalItem
            record={row(7, "/home/x/projetA", "pinned")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </TerminalRenameProvider>,
    );

    fireEvent.doubleClick(screen.getByText("pinned"));
    const input = screen.getByLabelText(/rename terminal/i) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.keyDown(input, { key: "Enter" });
    // Empty → null clears the manual label (auto-naming resumes).
    expect(onRename).toHaveBeenCalledWith("7", null);
  });

  it("Escape cancels the edit WITHOUT renaming", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onRename = vi.fn();
    render(
      <TerminalRenameProvider rename={onRename}>
        <ul>
          <TerminalItem
            record={row(7, "/home/x/projetA")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </TerminalRenameProvider>,
    );

    fireEvent.doubleClick(screen.getByText("projetA"));
    const input = screen.getByLabelText(/rename terminal/i) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "discard-me" } });
    fireEvent.keyDown(input, { key: "Escape" });
    // No rename, editor closed, the auto name remains.
    expect(onRename).not.toHaveBeenCalled();
    expect(screen.queryByLabelText(/rename terminal/i)).toBeNull();
    expect(screen.getByText("projetA")).toBeInTheDocument();
  });

  it("a MANUAL label is shown and is NOT clobbered by an auto-name poll update", async () => {
    // The crux of #30: a manual label wins at DISPLAY time and the live terminal_info
    // poll (which feeds the auto label) never overwrites it — the auto name is only a
    // fallback under a blank label, never persisted into the record.
    let foreground = "bash";
    mockIPC((cmd) => {
      if (cmd === "terminal_info") return { cwd: "/home/x/projetA", foreground };
      return null;
    });
    render(
      <TerminalRenameProvider rename={vi.fn()}>
        <ul>
          <TerminalItem
            record={row(7, "/home/x/projetA", "renamed")}
            index={0}
            active={false}
            ptyId={50}
            onSelect={vi.fn()}
            onClose={vi.fn()}
          />
        </ul>
      </TerminalRenameProvider>,
    );

    // The manual label renders immediately.
    expect(screen.getByText("renamed")).toBeInTheDocument();
    // Now a program comes to the foreground — the auto label would become
    // "projetA · htop", but it must NEVER replace the manual label.
    foreground = "htop";
    await new Promise((r) => setTimeout(r, 30));
    expect(screen.getByText("renamed")).toBeInTheDocument();
    expect(screen.queryByText("projetA · htop")).not.toBeInTheDocument();
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

  it("shows the settled badge while UNREAD and HIDES it once READ — even when inactive again (PRD-2.1 user story #3)", async () => {
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
    // The badge now leaves via an EXIT animation (the prior round cut it instantly), so it
    // is no longer immediately gone — wait for the node to finish animating out.
    await waitFor(() =>
      expect(
        screen.queryByRole("status", { name: /terminal status: success/i }),
      ).not.toBeInTheDocument(),
    );
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
