import { useEffect, useState } from "react";
import { act, render } from "@testing-library/react";
import { Terminal as XTerm } from "@xterm/xterm";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useScrollbackPersist } from "./scrollback-persist";

/**
 * Harness: mount an xterm instance in jsdom, wire `useScrollbackPersist` to it,
 * and surface the instance so the test can `write()` to it directly. The
 * `persist` sink is a spy; the serialize addon is faked to a deterministic
 * snapshot so we assert on the DEBOUNCE/FLUSH behaviour, not on xterm's exact
 * serialization (covered by the addon itself + Browser Mode).
 */
function Harness({
  recordId,
  persist,
  serialize,
  debounceMs,
  onTerm,
}: {
  recordId: string;
  persist: (id: string, s: string) => void;
  serialize: () => string;
  debounceMs: number;
  onTerm: (t: XTerm) => void;
}) {
  const term = useTerm();
  useEffect(() => {
    if (term) onTerm(term);
  }, [term, onTerm]);
  useScrollbackPersist(term, recordId, {
    persist,
    serializeAddonFactory: () => ({
      serialize,
      // xterm's loadAddon calls activate(term); a no-op satisfies the addon shape.
      activate: () => {},
      dispose: () => {},
    }),
    debounceMs,
  });
  return null;
}

/** Create + open a real xterm in a detached div, disposed on unmount. */
function useTerm(): XTerm | null {
  const [term, setTerm] = useStateTerm();
  useEffect(() => {
    const el = document.createElement("div");
    document.body.appendChild(el);
    const t = new XTerm({ scrollback: 1000 });
    t.open(el);
    setTerm(t);
    return () => {
      t.dispose();
      el.remove();
    };
  }, [setTerm]);
  return term;
}

function useStateTerm(): [XTerm | null, (t: XTerm | null) => void] {
  const [t, setT] = useState<XTerm | null>(null);
  return [t, setT];
}

describe("useScrollbackPersist (xterm-wired)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
  });

  it("persists the serialized scrollback after output then a quiet delay", () => {
    const persist = vi.fn();
    let term: XTerm | null = null;

    render(
      <Harness
        recordId={"42"}
        persist={persist}
        serialize={() => "serialized-history"}
        debounceMs={500}
        onTerm={(t) => {
          term = t;
        }}
      />,
    );

    expect(term).not.toBeNull();
    // Produce output â†’ schedules a debounced snapshot.
    act(() => {
      term!.write("hello world\r\n");
    });
    // Within the window: nothing persisted yet.
    expect(persist).not.toHaveBeenCalled();

    // After the quiet delay: exactly one persist with the serialized blob.
    act(() => {
      vi.advanceTimersByTime(500);
    });
    expect(persist).toHaveBeenCalledTimes(1);
    expect(persist).toHaveBeenCalledWith("42", "serialized-history");
  });

  it("debounces a burst of writes into ONE persist (not per write)", () => {
    const persist = vi.fn();
    let term: XTerm | null = null;
    render(
      <Harness
        recordId={"1"}
        persist={persist}
        serialize={() => "snap"}
        debounceMs={300}
        onTerm={(t) => {
          term = t;
        }}
      />,
    );

    act(() => {
      for (let i = 0; i < 40; i++) term!.write(`line ${i}\r\n`);
    });
    act(() => {
      vi.advanceTimersByTime(300);
    });
    // A flood of 40 writes collapsed into a single debounced persist.
    expect(persist).toHaveBeenCalledTimes(1);
  });

  it("flushes a final snapshot on unmount (tab close)", () => {
    const persist = vi.fn();
    let term: XTerm | null = null;
    const { unmount } = render(
      <Harness
        recordId={"7"}
        persist={persist}
        serialize={() => "on-tab-close"}
        debounceMs={10_000}
        onTerm={(t) => {
          term = t;
        }}
      />,
    );

    act(() => {
      term!.write("some output\r\n");
    });
    // Debounce is long; no write yet.
    expect(persist).not.toHaveBeenCalled();

    // Unmount (tab close) flushes immediately, even though the debounce had not
    // elapsed.
    act(() => {
      unmount();
    });
    expect(persist).toHaveBeenCalledTimes(1);
    expect(persist).toHaveBeenCalledWith("7", "on-tab-close");
  });

  it("flushes on the app beforeunload event (app close path)", () => {
    const persist = vi.fn();
    let term: XTerm | null = null;
    render(
      <Harness
        recordId={"5"}
        persist={persist}
        serialize={() => "on-app-close"}
        debounceMs={10_000}
        onTerm={(t) => {
          term = t;
        }}
      />,
    );
    act(() => {
      term!.write("x\r\n");
    });
    expect(persist).not.toHaveBeenCalled();

    // The app window is closing â†’ beforeunload â†’ flush.
    act(() => {
      window.dispatchEvent(new Event("beforeunload"));
    });
    expect(persist).toHaveBeenCalledWith("5", "on-app-close");
  });
});
