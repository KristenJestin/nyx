import { act, render, waitFor } from "@testing-library/react";
import { emit, mockIPC } from "@/bridge/test-harness";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal as XTerm } from "@xterm/xterm";
import { StrictMode, useMemo } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { chooseSpawnDimensions, usePty } from "./use-pty";

const SPAWNED_ID = 7;

interface IpcRecorder {
  calls: { cmd: string; args: Record<string, unknown> }[];
  callsTo: (cmd: string) => { cmd: string; args: Record<string, unknown> }[];
}

function installIpc(): IpcRecorder {
  const calls: IpcRecorder["calls"] = [];
  mockIPC(
    (cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      if (cmd === "pty_spawn") return SPAWNED_ID;
      return null;
    },
    { shouldMockEvents: true },
  );
  return { calls, callsTo: (cmd) => calls.filter((c) => c.cmd === cmd) };
}

/**
 * Harness that creates ONE stable xterm instance (memoized) and feeds it to
 * `usePty`. Because the instance is stable from the very first render, the
 * spawn effect's mount is what React.StrictMode double-invokes (setup → cleanup
 * → setup) — unlike `<Terminal>`, where the instance arrives via a later state
 * update. This is the configuration that actually stresses the dedupe guard.
 */
function StablePtyHarness({
  onReady,
  recordId,
  onOutput,
}: {
  /** Surfaces the xterm instance and the hook's resyncSize to the test. */
  onReady?: (term: XTerm, resyncSize: () => void) => void;
  /** Persistent terminal record id to thread through to `pty_spawn`. */
  recordId?: string;
  /** Forwarded to `usePty` so the test can assert it fires after output. */
  onOutput?: () => void;
} = {}) {
  const term = useMemo(() => new XTerm({ cols: 80, rows: 24 }), []);
  const fit = useMemo(() => new FitAddon(), []);
  const resyncSize = usePty(term, fit, { recordId, onOutput });
  // Re-publish on every render so the test always holds the live callback.
  onReady?.(term, resyncSize);
  return null;
}

describe("usePty (hook, stable instance)", () => {
  let ipc: IpcRecorder;

  beforeEach(() => {
    ipc = installIpc();
  });
  afterEach(() => {
    // clearMocks runs in vitest.setup.ts afterEach.
  });

  // What this `toHaveLength(1)` actually discriminates (mutation-verified — be
  // precise here, the obvious story is WRONG):
  //
  //  - The naive classic pattern (`useEffect(() => { spawn(); return () =>
  //    close() }, [term])`, fresh session + immediate teardown, no dedupe) does
  //    NOT turn this red on its own: because `usePty`'s `start()` is async (it
  //    awaits two `listen()` calls before `invoke('pty_spawn')`), the throwaway
  //    cleanup sets `torndown=true` first and the discarded session bails before
  //    spawning. Result: still ONE spawn, test stays GREEN.
  //  - The single load-bearing guard is the `torndown` bail in `start()`
  //    (usePty.ts). Removing the effect-level reuse/`spawnIssued` dedupe alone
  //    keeps ONE spawn (the bail catches the throwaway). Removing the `torndown`
  //    bail alone also keeps ONE spawn (the reuse dedupe stops the 2nd start()).
  //    Only removing BOTH makes this assertion go RED (two `pty_spawn`).
  //
  // So this test is NOT vacuous — it catches a fully-naive regression (no dedupe
  // AND no async bail) — but it does not pin down a single mechanism. See
  // usePty.ts for the full explanation of the three cooperating guards.
  it("issues exactly one pty_spawn even though StrictMode double-invokes the effect", async () => {
    render(
      <StrictMode>
        <StablePtyHarness />
      </StrictMode>,
    );

    await waitFor(() => expect(ipc.callsTo("pty_spawn").length).toBeGreaterThan(0));
    // Let any erroneous second spawn / deferred teardown settle.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });

    expect(ipc.callsTo("pty_spawn")).toHaveLength(1);
    // The StrictMode throwaway cleanup must not close the surviving PTY.
    expect(ipc.callsTo("pty_close")).toHaveLength(0);
  });

  // Pins the finding's fix (option b): resyncSize() pushes the terminal's
  // CURRENT cols/rows to the PTY out-of-band from xterm's onResize event. This
  // is the path that rescues a font-driven authoritative fit that lands while
  // the onResize handler is not yet wired. We assert resyncSize emits a
  // pty_resize carrying the live dims + the spawned id — i.e. the PTY is told
  // its size independently of any onResize firing.
  it("resyncSize pushes the terminal's current size to the PTY via pty_resize", async () => {
    let term!: XTerm;
    let resync!: () => void;
    render(
      <StablePtyHarness
        onReady={(t, r) => {
          term = t;
          resync = r;
        }}
      />,
    );

    // Wait for the spawn so the session id is known (resync fires immediately
    // rather than deferring).
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    // Simulate the authoritative post-font fit having changed the geometry,
    // then resync — without emitting xterm's onResize.
    act(() => {
      term.resize(100, 30);
    });
    const before = ipc.callsTo("pty_resize").length;
    act(() => {
      resync();
    });

    await waitFor(() => {
      const resizes = ipc.callsTo("pty_resize");
      expect(resizes.length).toBeGreaterThan(before);
      const last = resizes[resizes.length - 1];
      expect(last.args.id).toBe(SPAWNED_ID);
      expect(last.args.cols).toBe(100);
      expect(last.args.rows).toBe(30);
    });
  });

  // PRD-2.1 task #3: the persistent terminal record id must reach `pty_spawn` as
  // the `terminalId` arg (Tauri maps it to the Rust `terminal_id` param) so the
  // backend can associate the live pty_id with the durable record for exec-state.
  // This is plumbing only — it must NOT perturb the single-spawn dedupe.
  it("forwards recordId to pty_spawn as terminalId", async () => {
    render(<StablePtyHarness recordId="term-rec-42" />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    const spawn = ipc.callsTo("pty_spawn")[0];
    expect(spawn.args.terminalId).toBe("term-rec-42");
  });

  // A record-less terminal (the socle / standalone harness) passes no record id,
  // so `pty_spawn` receives `terminalId: undefined` and the backend records no
  // mapping. Guards that the plumbing degrades cleanly.
  it("omits terminalId when no recordId is bound", async () => {
    render(<StablePtyHarness />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));
    const spawn = ipc.callsTo("pty_spawn")[0];
    expect(spawn.args.terminalId).toBeUndefined();
  });

  // FEEDBACK #24 — a terminal mounted `display:none` (an inactive/background pane
  // at boot) has a 0×0 element and `proposeDimensions()` then derives a DEGENERATE
  // ~8-col size from `parseInt("100%")`. Spawning there makes a resumed `claude`
  // hard-wrap its whole turn to 8 columns (frozen in the scrollback). The spawn
  // must IGNORE the fit when there is no real box and use the sane fallback.
  describe("chooseSpawnDimensions (spawn-size gate)", () => {
    it("uses the fit when the element has a real box", () => {
      const el = { clientWidth: 1700, clientHeight: 900 };
      const dims = chooseSpawnDimensions(el, () => ({ cols: 200, rows: 50 }), {
        cols: 80,
        rows: 24,
      });
      expect(dims).toEqual({ cols: 200, rows: 50 });
    });

    it("ignores the degenerate fit and falls back when the pane is hidden (0×0)", () => {
      const el = { clientWidth: 0, clientHeight: 0 };
      // proposeDimensions would return the bogus ~8 cols here — it must NOT be used,
      // and must NOT even be called (the pane has no box to measure).
      let called = false;
      const dims = chooseSpawnDimensions(
        el,
        () => {
          called = true;
          return { cols: 8, rows: 24 };
        },
        { cols: 80, rows: 24 },
      );
      expect(dims).toEqual({ cols: 80, rows: 24 });
      expect(called).toBe(false);
    });

    it("falls back when the element is absent (not yet opened)", () => {
      const dims = chooseSpawnDimensions(null, () => ({ cols: 8, rows: 24 }), {
        cols: 80,
        rows: 24,
      });
      expect(dims).toEqual({ cols: 80, rows: 24 });
    });

    it("falls back per-axis when a visible fit returns undefined", () => {
      const el = { clientWidth: 1700, clientHeight: 900 };
      const dims = chooseSpawnDimensions(el, () => undefined, { cols: 80, rows: 24 });
      expect(dims).toEqual({ cols: 80, rows: 24 });
    });
  });

  it("closes the PTY on a real unmount", async () => {
    const { unmount } = render(<StablePtyHarness />);
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    await act(async () => {
      unmount();
      // Deferred teardown runs on a microtask; flush it.
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });

    await waitFor(() => expect(ipc.callsTo("pty_close")).toHaveLength(1));
    expect(ipc.callsTo("pty_close")[0].args.id).toBe(SPAWNED_ID);
  });

  // FEEDBACK #25 — onOutput must fire AFTER a pty://output chunk is parsed into the
  // xterm buffer (it is invoked from term.write's completion callback). This is the
  // ping `<Terminal>` debounces into a repaint-if-at-bottom to defeat the resume
  // "stale canvas" race. We emit one output event tagged with the spawned id and
  // assert the callback is invoked.
  it("fires onOutput after an output chunk is written to the buffer", async () => {
    let outputs = 0;
    render(<StablePtyHarness onOutput={() => outputs++} />);
    // Wait for the spawn so the session can route output to our id.
    await waitFor(() => expect(ipc.callsTo("pty_spawn")).toHaveLength(1));

    await act(async () => {
      await emit("pty://output", {
        id: SPAWNED_ID,
        bytes: Array.from(new TextEncoder().encode("hi")),
      });
      // term.write's completion callback fires on a later tick; let it flush.
      await new Promise((r) => setTimeout(r, 20));
    });

    await waitFor(() => expect(outputs).toBeGreaterThan(0));
  });
});
