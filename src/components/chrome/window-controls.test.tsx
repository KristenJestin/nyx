import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useWindowControlsVisible, WindowControls, windowControlsVisible } from "./window-controls";

/** Record the window IPC calls the controls trigger so we can assert wiring. */
interface IpcRecorder {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

/**
 * The `@tauri-apps/api/window` `Window` methods (`minimize`, `toggleMaximize`,
 * `close`) ultimately route through the `plugin:window|<op>` IPC command. We
 * mock the IPC and record the command names so we can assert the buttons are
 * really wired to the window API (not just rendered).
 */
function installIpc(): IpcRecorder {
  const calls: IpcRecorder["calls"] = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
    // isMaximized is queried by toggleMaximize; report "not maximized".
    if (typeof cmd === "string" && cmd.includes("is_maximized")) return false;
    return null;
  });
  // `getCurrentWindow()` reads `__TAURI_INTERNALS__.metadata.currentWindow`,
  // which the real runtime injects but `mockIPC` does not. Seed it so the
  // window controls resolve a `Window("main")` in jsdom.
  const internals = (
    globalThis as unknown as {
      __TAURI_INTERNALS__: Record<string, unknown>;
    }
  ).__TAURI_INTERNALS__;
  internals.metadata = { currentWindow: { label: "main" }, windows: [] };
  return { calls, callsTo: (cmd) => calls.filter((c) => c.cmd.includes(cmd)) };
}

describe("windowControlsVisible (toggle logic)", () => {
  it("is visible by default (undefined / empty env)", () => {
    expect(windowControlsVisible(undefined)).toBe(true);
    expect(windowControlsVisible("")).toBe(true);
  });

  it("is hidden only when the env value is exactly '0'", () => {
    expect(windowControlsVisible("0")).toBe(false);
  });

  it("is visible for any non-'0' value", () => {
    expect(windowControlsVisible("1")).toBe(true);
    expect(windowControlsVisible("true")).toBe(true);
    expect(windowControlsVisible("yes")).toBe(true);
  });
});

/** Tiny probe component that renders the hook's resolved value as text. */
function VisibleProbe() {
  const visible = useWindowControlsVisible();
  return <span>{visible ? "visible" : "hidden"}</span>;
}

describe("useWindowControlsVisible (runtime env resolution)", () => {
  it("defaults to visible while the command is in flight and when it returns true", async () => {
    mockIPC((cmd) => {
      if (typeof cmd === "string" && cmd.includes("window_controls_visible")) {
        return true;
      }
      return null;
    });
    render(<VisibleProbe />);
    // Default before the async call resolves is already "visible".
    expect(screen.getByText("visible")).toBeInTheDocument();
    // And stays visible once the command resolves true.
    await waitFor(() => expect(screen.getByText("visible")).toBeInTheDocument());
  });

  it("hides when the command resolves false (NYX_WINDOW_CONTROLS=0)", async () => {
    mockIPC((cmd) => {
      if (typeof cmd === "string" && cmd.includes("window_controls_visible")) {
        return false;
      }
      return null;
    });
    render(<VisibleProbe />);
    await waitFor(() => expect(screen.getByText("hidden")).toBeInTheDocument());
  });

  it("stays visible (permissive default) when the command errors", async () => {
    mockIPC((cmd) => {
      if (typeof cmd === "string" && cmd.includes("window_controls_visible")) {
        throw new Error("backend unavailable");
      }
      return null;
    });
    render(<VisibleProbe />);
    // Give the rejected promise a tick to settle; value must stay visible.
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.getByText("visible")).toBeInTheDocument();
  });
});

describe("<WindowControls>", () => {
  let ipc: IpcRecorder;

  beforeEach(() => {
    ipc = installIpc();
  });
  afterEach(() => {
    // clearMocks runs in vitest.setup.ts afterEach.
  });

  it("renders minimize, maximize and close buttons", () => {
    render(<WindowControls />);
    expect(screen.getByRole("button", { name: /minimize/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /maximize|restore/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /close/i })).toBeInTheDocument();
  });

  it("minimize button calls the window minimize op", async () => {
    render(<WindowControls />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /minimize/i }));
      await Promise.resolve();
    });
    expect(ipc.callsTo("minimize").length).toBeGreaterThan(0);
  });

  it("maximize button calls the window toggle-maximize op", async () => {
    render(<WindowControls />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /maximize|restore/i }));
      await Promise.resolve();
    });
    expect(ipc.callsTo("maximize").length).toBeGreaterThan(0);
  });

  it("close button calls the window close op", async () => {
    render(<WindowControls />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /close/i }));
      await Promise.resolve();
    });
    expect(ipc.callsTo("close").length).toBeGreaterThan(0);
  });
});
