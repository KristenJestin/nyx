import { act, render, renderHook, screen, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { CommandControls } from "./command-controls";
import { CommandStateDot } from "./command-state-dot";
import { useCommandState } from "./use-command-state";
import type { ExecState } from "@/components/sidebar/use-terminals";

const ALL_STATES: ExecState[] = ["idle", "running", "success", "error"];

/**
 * The colour token now lives on `<CommandStateDot>`'s shared `<CrossfadeFill>`
 * layer (an `aria-hidden` absolute child), not the `role="status"` host. Read the
 * fill classes off that layer; `data-state`/`data-animated` stay on the host.
 */
function fillClasses(host: HTMLElement): string {
  // During a colour cross-fade two stacked layers briefly coexist; aggregate them so a
  // token assertion sees the layer it cares about regardless of DOM order.
  return Array.from(host.querySelectorAll("[aria-hidden]"))
    .map((el) => el.className)
    .join(" ");
}

interface IpcSpy {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

function installIpc(returnState: Record<string, string> = {}): IpcSpy {
  const spy: IpcSpy = {
    calls: [],
    callsTo: (cmd) => spy.calls.filter((c) => c.cmd === cmd),
  };
  mockIPC(
    (cmd, args) => {
      spy.calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      return returnState[cmd] ?? null;
    },
    { shouldMockEvents: true },
  );
  return spy;
}

describe("<CommandStateDot> (4 states: colour + motion)", () => {
  it.each(ALL_STATES)("renders the %s state with its design-system token colour", (state) => {
    render(<CommandStateDot state={state} />);
    const dot = screen.getByRole("status", { name: new RegExp(`command status: ${state}`, "i") });
    expect(dot).toHaveAttribute("data-state", state);
  });

  it("colours each state from a token (no raw colours)", () => {
    const tokens: Record<ExecState, string> = {
      idle: "bg-muted-foreground/50",
      running: "bg-info",
      success: "bg-success",
      error: "bg-destructive",
    };
    for (const state of ALL_STATES) {
      const { unmount } = render(<CommandStateDot state={state} />);
      // The colour lives on the cross-fade layer, not the role="status" host.
      expect(fillClasses(screen.getByRole("status"))).toContain(tokens[state]);
      unmount();
    }
  });

  it("running is BLUE + ANIMATED; success is GREEN + STATIC (distinct on BOTH axes)", () => {
    const { rerender } = render(<CommandStateDot state="running" />);
    const running = screen.getByRole("status");
    // Blue token (on the fill layer) + a live Motion animation (data-animated marks the loop).
    expect(fillClasses(running)).toContain("bg-info");
    expect(running).toHaveAttribute("data-animated");

    rerender(<CommandStateDot state="success" />);
    const success = screen.getByRole("status");
    // Green token + NO animation → never confusable with running.
    expect(fillClasses(success)).toContain("bg-success");
    expect(success).not.toHaveAttribute("data-animated");
  });

  it("only running animates (idle/success/error are static)", () => {
    for (const state of ["idle", "success", "error"] as ExecState[]) {
      const { unmount } = render(<CommandStateDot state={state} />);
      expect(screen.getByRole("status")).not.toHaveAttribute("data-animated");
      unmount();
    }
  });
});

describe("useCommandState (driven by command://state, filtered by instanceId)", () => {
  beforeEach(() => {
    installIpc();
  });

  it("seeds from the initial state then follows command://state for its instance", async () => {
    const { result } = renderHook(() => useCommandState("inst-a", "idle"));
    expect(result.current).toBe("idle");

    await act(async () => {
      await emit("command://state", { instanceId: "inst-a", state: "running", code: null });
    });
    await waitFor(() => expect(result.current).toBe("running"));

    await act(async () => {
      await emit("command://state", { instanceId: "inst-a", state: "success", code: 0 });
    });
    await waitFor(() => expect(result.current).toBe("success"));

    await act(async () => {
      await emit("command://state", { instanceId: "inst-a", state: "error", code: 1 });
    });
    await waitFor(() => expect(result.current).toBe("error"));
  });

  it("ignores transitions for OTHER instances (filtered)", async () => {
    const { result } = renderHook(() => useCommandState("inst-b", "idle"));
    await act(async () => {
      await emit("command://state", { instanceId: "other", state: "running", code: null });
    });
    // Give the (ignored) event a chance to wrongly apply.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(result.current).toBe("idle");
  });
});

describe("<CommandControls> (start/stop/relaunch buttons)", () => {
  beforeEach(() => {
    installIpc();
  });

  it("invokes the matching lifecycle command for each button", async () => {
    const spy = installIpc({
      command_start: "running",
      command_stop: "idle",
      command_relaunch: "running",
    });
    // idle: start + relaunch enabled, stop disabled — exercise start + relaunch.
    const { rerender } = render(<CommandControls instanceId="i1" state="idle" />);
    screen.getByRole("button", { name: /start command/i }).click();
    await waitFor(() => {
      const c = spy.callsTo("command_start");
      expect(c).toHaveLength(1);
      expect(c[0].args.instanceId).toBe("i1");
    });
    screen.getByRole("button", { name: /relaunch command/i }).click();
    await waitFor(() => expect(spy.callsTo("command_relaunch")).toHaveLength(1));

    // running: stop is enabled — exercise it.
    rerender(<CommandControls instanceId="i1" state="running" />);
    screen.getByRole("button", { name: /stop command/i }).click();
    await waitFor(() => {
      const c = spy.callsTo("command_stop");
      expect(c).toHaveLength(1);
      expect(c[0].args.instanceId).toBe("i1");
    });
  });

  it("disables Stop when idle, Start when running; Relaunch always enabled", () => {
    const { rerender } = render(<CommandControls instanceId="i2" state="idle" />);
    expect(screen.getByRole("button", { name: /stop command/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /start command/i })).not.toBeDisabled();
    expect(screen.getByRole("button", { name: /relaunch command/i })).not.toBeDisabled();

    rerender(<CommandControls instanceId="i2" state="running" />);
    expect(screen.getByRole("button", { name: /start command/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /stop command/i })).not.toBeDisabled();
    expect(screen.getByRole("button", { name: /relaunch command/i })).not.toBeDisabled();
  });

  it("Start is enabled again after success/error (a finished run can be restarted)", () => {
    for (const state of ["success", "error"] as ExecState[]) {
      const { unmount } = render(<CommandControls instanceId="i3" state={state} />);
      expect(screen.getByRole("button", { name: /start command/i })).not.toBeDisabled();
      expect(screen.getByRole("button", { name: /stop command/i })).toBeDisabled();
      unmount();
    }
  });

  it("surfaces the returned last_state via onStateChange", async () => {
    installIpc({ command_start: "running" });
    const onStateChange = vi.fn();
    render(<CommandControls instanceId="i4" state="idle" onStateChange={onStateChange} />);
    screen.getByRole("button", { name: /start command/i }).click();
    await waitFor(() => expect(onStateChange).toHaveBeenCalledWith("running"));
  });

  it("SURFACES a rejected lifecycle invoke: calls onError and forces the dot to error", async () => {
    // The backend rejects command_start (the real Windows failure shape). The old
    // empty `catch {}` swallowed this entirely; now it must be visible.
    mockIPC(
      (cmd) => {
        if (cmd === "command_start") throw new Error("spawn failed: cwd not found");
        return null;
      },
      { shouldMockEvents: true },
    );
    const onError = vi.fn();
    render(<CommandControls instanceId="i5" state="idle" onError={onError} />);
    // Pre-failure the dot reflects idle.
    expect(screen.getByRole("status")).toHaveAttribute("data-state", "idle");
    screen.getByRole("button", { name: /start command/i }).click();
    await waitFor(() =>
      expect(onError).toHaveBeenCalledWith("start", expect.stringContaining("spawn failed")),
    );
    // The failure is visible on the dot (error), not a silent no-op.
    await waitFor(() => expect(screen.getByRole("status")).toHaveAttribute("data-state", "error"));
  });

  it("clears the error dot when a fresh live state arrives (a retry took hold)", async () => {
    mockIPC(
      (cmd) => {
        if (cmd === "command_start") throw new Error("boom");
        return null;
      },
      { shouldMockEvents: true },
    );
    const { rerender } = render(<CommandControls instanceId="i6" state="idle" />);
    screen.getByRole("button", { name: /start command/i }).click();
    await waitFor(() => expect(screen.getByRole("status")).toHaveAttribute("data-state", "error"));
    // A live `command://state` transition (running) flows in via props → error clears.
    rerender(<CommandControls instanceId="i6" state="running" />);
    expect(screen.getByRole("status")).toHaveAttribute("data-state", "running");
  });
});
