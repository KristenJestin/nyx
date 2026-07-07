import { act, render, waitFor } from "@testing-library/react";
import type { WTerm } from "@wterm/dom";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { mockIPC, emit } from "@/bridge/test-harness";
import { FakeTerminalCore } from "@/components/terminal/fake-core";
import { setCoreFactory, type CoreFactory } from "@/components/terminal/ghostty-core";
import { CommandOutputPanel } from "./command-output-panel";

/**
 * `<CommandOutputPanel>` read-only contract (T8), jsdom. Post-v2.2.5 the panel
 * renders with wterm + the ghostty core (xterm is gone). The WASM core cannot
 * load under jsdom, so we inject a {@link FakeTerminalCore} (records writes) via
 * the core-factory seam and assert on `core.written`. ANSI COLOR RENDERING is a
 * browser-mode concern (the DOM renderer), not asserted here.
 */

interface IpcSpy {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

function installIpc(history: Record<string, string> = {}): IpcSpy {
  const spy: IpcSpy = {
    calls: [],
    callsTo: (cmd) => spy.calls.filter((c) => c.cmd === cmd),
  };
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      spy.calls.push({ cmd, args: a });
      if (cmd === "command_output") return history[a.instanceId as string] ?? "";
      return null;
    },
    { shouldMockEvents: true },
  );
  return spy;
}

let lastCore: FakeTerminalCore | null = null;
let restoreFactory: CoreFactory | null = null;

function written(): string {
  return lastCore?.written ?? "";
}

describe("<CommandOutputPanel> (read-only command output, T8)", () => {
  beforeEach(() => {
    installIpc();
    lastCore = null;
    restoreFactory = setCoreFactory(async () => {
      lastCore = new FakeTerminalCore();
      return lastCore;
    });
  });

  afterEach(() => {
    if (restoreFactory) setCoreFactory(restoreFactory);
  });

  it("rehydrates the persisted history via command_output on open", async () => {
    const spy = installIpc({ inst1: "RESTORED_HISTORY_42\r\n" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst1" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());

    await waitFor(() => {
      const calls = spy.callsTo("command_output");
      expect(calls.length).toBeGreaterThan(0);
      expect(calls[0].args.instanceId).toBe("inst1");
    });
    await waitFor(() => expect(written()).toContain("RESTORED_HISTORY_42"));
  });

  it("renders streamed command://output for its instance", async () => {
    const spy = installIpc({ inst2: "" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst2" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());
    // Wait until the output subscription is wired (the fake bridge delivers only
    // to CURRENT subscribers, so an emit before subscribe would be dropped). The
    // rehydrate `command_output` call fires AFTER all three subscribes resolve.
    await waitFor(() => expect(spy.callsTo("command_output").length).toBeGreaterThan(0));

    const ansi = "\x1b[31mERR_TOKEN\x1b[0m\r\n";
    const bytes = Array.from(new TextEncoder().encode(ansi));
    await act(async () => {
      await emit("command://output", { instanceId: "inst2", bytes });
    });
    await waitFor(() => expect(written()).toContain("ERR_TOKEN"));
  });

  it("only writes output whose instanceId matches (filtered stream)", async () => {
    const spy = installIpc({ inst3: "" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst3" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());
    // Wait until the subscription is wired (rehydrate call fires after subscribe).
    await waitFor(() => expect(spy.callsTo("command_output").length).toBeGreaterThan(0));

    const mine = Array.from(new TextEncoder().encode("MINE_3\r\n"));
    const other = Array.from(new TextEncoder().encode("OTHER_9\r\n"));
    await act(async () => {
      await emit("command://output", { instanceId: "other9", bytes: other });
      await emit("command://output", { instanceId: "inst3", bytes: mine });
    });

    await waitFor(() => expect(written()).toContain("MINE_3"));
    expect(written()).not.toContain("OTHER_9");
  });

  it("resets the panel on a new run (running transition)", async () => {
    installIpc({ inst5: "OLD_RUN_OUTPUT\r\n" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst5" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(written()).toContain("OLD_RUN_OUTPUT"));

    // A running transition = a new run → RIS-reset the surface. The fake core
    // records the reset sequence; we assert the reset was written.
    await act(async () => {
      await emit("command://state", { instanceId: "inst5", state: "running", code: null });
    });
    await waitFor(() => expect(written()).toContain("\x1bc"));

    const fresh = Array.from(new TextEncoder().encode("NEW_RUN_OUTPUT\r\n"));
    await act(async () => {
      await emit("command://output", { instanceId: "inst5", bytes: fresh });
    });
    await waitFor(() => expect(written()).toContain("NEW_RUN_OUTPUT"));
  });

  it("ignores a running transition for a DIFFERENT instance (no spurious reset)", async () => {
    installIpc({ inst6: "KEEP_THIS_OUTPUT\r\n" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst6" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(written()).toContain("KEEP_THIS_OUTPUT"));

    await act(async () => {
      await emit("command://state", { instanceId: "other99", state: "running", code: null });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    // No reset sequence was written (the running was for another instance).
    expect(written()).not.toContain("\x1bc");
  });

  it("resets on command://output-cleared for its instance (clear_command_output)", async () => {
    installIpc({ inst7: "STALE_SCROLLBACK\r\n" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst7" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(written()).toContain("STALE_SCROLLBACK"));

    await act(async () => {
      await emit("command://output-cleared", { instanceId: "inst7" });
    });
    await waitFor(() => expect(written()).toContain("\x1bc"));

    const fresh = Array.from(new TextEncoder().encode("AFTER_CLEAR\r\n"));
    await act(async () => {
      await emit("command://output", { instanceId: "inst7", bytes: fresh });
    });
    await waitFor(() => expect(written()).toContain("AFTER_CLEAR"));
  });

  it("ignores command://output-cleared for a DIFFERENT instance (no spurious reset)", async () => {
    installIpc({ inst8: "KEEP_AFTER_OTHER_CLEAR\r\n" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst8" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(written()).toContain("KEEP_AFTER_OTHER_CLEAR"));

    await act(async () => {
      await emit("command://output-cleared", { instanceId: "other77" });
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(written()).not.toContain("\x1bc");
  });

  it("NEVER sends a keystroke/stdin/resize to the backend (read-only strict)", async () => {
    const spy = installIpc({ inst4: "HELLO\r\n" });
    let term: WTerm | null = null;
    render(<CommandOutputPanel instanceId="inst4" onInstance={(t) => (term = t)} />);
    await waitFor(() => expect(term).not.toBeNull());
    await waitFor(() => expect(spy.callsTo("command_output").length).toBeGreaterThan(0));

    // The panel builds its WTerm with NO `onData`, so there is no input path at
    // all. No write/stdin/resize command is ever invoked; the only IPC is the read.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(spy.callsTo("pty_write")).toHaveLength(0);
    expect(spy.callsTo("command_write")).toHaveLength(0);
    expect(spy.callsTo("pty_resize")).toHaveLength(0);
    expect(spy.callsTo("command_resize")).toHaveLength(0);
    for (const call of spy.calls) {
      expect(call.cmd).toBe("command_output");
    }
  });
});
