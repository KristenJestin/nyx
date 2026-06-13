import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { describe, expect, it, vi } from "vitest";

import { TerminalItem } from "./terminal-item";
import type { TerminalInfo } from "./auto-label";
import type { TerminalRecord } from "./use-terminals";

function row(
  id: number,
  cwd: string,
  label: string | null = null,
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
  };
}

/** Mock the `terminal_info` command to return a fixed live reading. */
function mockTerminalInfo(info: TerminalInfo): void {
  mockIPC((cmd) => {
    if (cmd === "terminal_info") return info;
    return null;
  });
}

describe("<TerminalItem> auto-naming + rename", () => {
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
    await waitFor(() =>
      expect(screen.getByText("projetA · htop")).toBeInTheDocument(),
    );
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

  it("double-click → type → Enter persists a manual rename via onRename", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onRename = vi.fn();
    render(
      <ul>
        <TerminalItem
          record={row(7, "/home/x/projetA")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
          onRename={onRename}
        />
      </ul>,
    );

    // Enter edit mode and rename.
    fireEvent.doubleClick(screen.getByText("projetA"));
    const input = screen.getByLabelText(/rename terminal/i);
    fireEvent.change(input, { target: { value: "backend api" } });
    fireEvent.keyDown(input, { key: "Enter" });

    // The manual label is persisted (record id + new label).
    expect(onRename).toHaveBeenCalledWith("7", "backend api");
  });

  it("renaming to an empty value clears back to auto-naming (null)", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onRename = vi.fn();
    render(
      <ul>
        <TerminalItem
          record={row(7, "/home/x/projetA", "old-name")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
          onRename={onRename}
        />
      </ul>,
    );

    fireEvent.doubleClick(screen.getByText("old-name"));
    const input = screen.getByLabelText(/rename terminal/i);
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.keyDown(input, { key: "Enter" });

    // Whitespace-only → clear the override (null), restoring auto-naming.
    expect(onRename).toHaveBeenCalledWith("7", null);
  });

  it("Escape cancels the rename without persisting", () => {
    mockTerminalInfo({ cwd: "/home/x/projetA", foreground: "bash" });
    const onRename = vi.fn();
    render(
      <ul>
        <TerminalItem
          record={row(7, "/home/x/projetA")}
          index={0}
          active={false}
          ptyId={50}
          onSelect={vi.fn()}
          onClose={vi.fn()}
          onRename={onRename}
        />
      </ul>,
    );

    fireEvent.doubleClick(screen.getByText("projetA"));
    const input = screen.getByLabelText(/rename terminal/i);
    fireEvent.change(input, { target: { value: "discarded" } });
    fireEvent.keyDown(input, { key: "Escape" });

    expect(onRename).not.toHaveBeenCalled();
    // Back to the auto/cwd name, edit cancelled.
    expect(screen.getByText("projetA")).toBeInTheDocument();
  });
});
