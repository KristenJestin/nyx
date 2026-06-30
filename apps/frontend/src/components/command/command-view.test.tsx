import { act, render, screen, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import { CommandView } from "./command-view";

interface IpcSpy {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

function installIpc(): IpcSpy {
  const spy: IpcSpy = {
    calls: [],
    callsTo: (cmd) => spy.calls.filter((c) => c.cmd === cmd),
  };
  mockIPC(
    (cmd, args) => {
      spy.calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      if (cmd === "command_output") return "PANEL_HISTORY\r\n";
      return null;
    },
    { shouldMockEvents: true },
  );
  return spy;
}

describe("<CommandView> (composed: panel + dot + 3 buttons, no stdin)", () => {
  beforeEach(() => {
    installIpc();
  });

  it("renders the read-only panel, the state dot, and the three buttons", async () => {
    const spy = installIpc();
    render(<CommandView instanceId="i1" name="dev" initialState="idle" />);
    // The dot + three lifecycle buttons (T9) are mounted.
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /command status: idle/i })).toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: /start command/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /stop command/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /relaunch command/i })).toBeInTheDocument();
    // The panel rehydrated via command_output (T8 wiring is composed in).
    await waitFor(() => expect(spy.callsTo("command_output").length).toBeGreaterThan(0));
  });

  it("the dot follows command://state for this instance", async () => {
    installIpc();
    render(<CommandView instanceId="i2" name="dev" initialState="idle" />);
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /command status: idle/i })).toBeInTheDocument(),
    );
    await act(async () => {
      await emit("command://state", { instanceId: "i2", state: "running", code: null });
    });
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /command status: running/i })).toBeInTheDocument(),
    );
  });

  it("gates the buttons by the LIVE state: running disables Start, enables Stop; back to idle re-enables Start", async () => {
    // The view drives gating from `useCommandState` (seeded then followed by
    // command://state). With the camelCase payload fix the dot is no longer frozen,
    // so the buttons must re-gate as the live state flows: this is the end-to-end
    // proof that gating follows the live state in the MAIN VIEW (finding 7).
    installIpc();
    render(<CommandView instanceId="iv" name="dev" initialState="idle" />);
    const start = () => screen.getByRole("button", { name: /start command/i });
    const stop = () => screen.getByRole("button", { name: /stop command/i });
    const relaunch = () => screen.getByRole("button", { name: /relaunch command/i });

    // Seeded idle: Start + Relaunch enabled, Stop disabled.
    await waitFor(() => expect(start()).toBeInTheDocument());
    expect(start()).not.toBeDisabled();
    expect(relaunch()).not.toBeDisabled();
    expect(stop()).toBeDisabled();

    // Live → running: Start disabled, Stop + Relaunch enabled.
    await act(async () => {
      await emit("command://state", { instanceId: "iv", state: "running", code: null });
    });
    await waitFor(() => expect(start()).toBeDisabled());
    expect(stop()).not.toBeDisabled();
    expect(relaunch()).not.toBeDisabled();

    // Live → error (a finished run): Start + Relaunch enabled again, Stop disabled.
    await act(async () => {
      await emit("command://state", { instanceId: "iv", state: "error", code: 1 });
    });
    await waitFor(() => expect(start()).not.toBeDisabled());
    expect(stop()).toBeDisabled();
    expect(relaunch()).not.toBeDisabled();
  });

  it("surfaces a refused lifecycle command inline (no more silent no-op on Start)", async () => {
    // The backend rejects command_start — the real Windows failure the finding is
    // about. The view must render the error inline instead of doing nothing.
    mockIPC(
      (cmd) => {
        if (cmd === "command_start") throw new Error("cwd does not exist");
        if (cmd === "command_output") return "";
        return null;
      },
      { shouldMockEvents: true },
    );
    render(<CommandView instanceId="i4" name="dev" initialState="idle" />);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /start command/i })).toBeInTheDocument(),
    );
    screen.getByRole("button", { name: /start command/i }).click();
    // The failure is now visible: an inline alert carrying the backend message.
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/failed to start command/i);
    expect(alert).toHaveTextContent(/cwd does not exist/i);
  });

  it("renders the info bar (command + resolved cwd + source) when threaded context", async () => {
    installIpc();
    render(
      <CommandView
        instanceId="ib1"
        name="dev"
        initialState="idle"
        command="bun run start"
        cwd="/p/frontend"
        sourceScriptName="start"
        sourcePackageJsonPath="frontend/package.json"
      />,
    );
    // The command line, resolved run directory, and package.json source reference
    // all appear in the compact info bar under the controls.
    await waitFor(() => expect(screen.getByText("bun run start")).toBeInTheDocument());
    expect(screen.getByText("/p/frontend")).toBeInTheDocument();
    expect(screen.getByText("package.json:scripts.start")).toBeInTheDocument();
    // The redundant state-text label was dropped (the dot carries the live state);
    // the info bar shows no "idle" word. The dot itself keeps the accessible status.
    expect(screen.queryByText("idle")).toBeNull();
    expect(screen.getByRole("status", { name: /command status: idle/i })).toBeInTheDocument();
  });

  it("omits the source field for a hand-authored command (no package.json link)", async () => {
    installIpc();
    render(
      <CommandView instanceId="ib2" name="build" initialState="idle" command="make" cwd="/p" />,
    );
    await waitFor(() => expect(screen.getByText("make")).toBeInTheDocument());
    expect(screen.queryByText(/package\.json:scripts\./)).toBeNull();
  });

  it("shows the last run's exit code in the info bar once a run ends", async () => {
    installIpc();
    render(
      <CommandView
        instanceId="ib3"
        name="dev"
        initialState="idle"
        command="bun run dev"
        cwd="/p"
      />,
    );
    await waitFor(() => expect(screen.getByText("bun run dev")).toBeInTheDocument());
    // No exit code before any run ends this session.
    expect(screen.queryByText(/^exit /)).toBeNull();
    // A run finishes with code 1 → the info bar shows "exit 1" (the error STATE is
    // the dot's job; the info bar no longer repeats a state-text word).
    await act(async () => {
      await emit("command://state", { instanceId: "ib3", state: "error", code: 1 });
    });
    await waitFor(() => expect(screen.getByText("exit 1")).toBeInTheDocument());
    // The state word is NOT repeated in the info bar (only the dot carries it).
    expect(screen.getByRole("status", { name: /command status: error/i })).toBeInTheDocument();
  });

  it("the last run's exit code PERSISTS through an acknowledge (idle) — survives the dot reset", async () => {
    // Finding 1 ↔ 3 interaction: the exit code is LAST-RUN info, not the dot's live
    // state. An acknowledge emits an `idle` command://state (no code); the info bar
    // must KEEP showing the prior run's "exit 1" while the dot goes back to idle.
    installIpc();
    render(
      <CommandView
        instanceId="ib5"
        name="dev"
        initialState="idle"
        command="bun run dev"
        cwd="/p"
      />,
    );
    await waitFor(() => expect(screen.getByText("bun run dev")).toBeInTheDocument());
    await act(async () => {
      await emit("command://state", { instanceId: "ib5", state: "error", code: 1 });
    });
    await waitFor(() => expect(screen.getByText("exit 1")).toBeInTheDocument());
    // Acknowledge → idle (the dot clears), but the exit code stays put.
    await act(async () => {
      await emit("command://state", { instanceId: "ib5", state: "idle", code: null });
    });
    await waitFor(() =>
      expect(screen.getByRole("status", { name: /command status: idle/i })).toBeInTheDocument(),
    );
    expect(screen.getByText("exit 1")).toBeInTheDocument();
  });

  it("does NOT render the info bar when the command/cwd context is absent", async () => {
    installIpc();
    render(<CommandView instanceId="ib4" name="dev" initialState="idle" />);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /start command/i })).toBeInTheDocument(),
    );
    // The header (dot + name + buttons) is unchanged; no info-bar fields appear.
    expect(screen.queryByText(/package\.json:scripts\./)).toBeNull();
  });

  it("never sends stdin: the only command IPC are the lifecycle + rehydration", async () => {
    const spy = installIpc();
    render(<CommandView instanceId="i3" name="dev" initialState="idle" />);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /start command/i })).toBeInTheDocument(),
    );
    // No write/resize command appears (the panel is read-only; the view has no
    // stdin surface). Let any wiring settle.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(spy.callsTo("pty_write")).toHaveLength(0);
    expect(spy.callsTo("command_write")).toHaveLength(0);
    expect(spy.callsTo("pty_resize")).toHaveLength(0);
  });
});
