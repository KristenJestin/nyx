import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { Tabs, TabCount } from "./tabs";
import {
  tabHeightTransition,
  tabIndicatorTransition,
  tabPanelTransition,
  tabPanelVariants,
} from "./tabs-motion";

describe("tabPanelTransition (reduced-motion aware)", () => {
  it("returns a short eased tween when motion is allowed", () => {
    const t = tabPanelTransition(false) as Record<string, unknown>;
    expect(t.duration).toBe(0.16);
    expect(Array.isArray(t.ease)).toBe(true);
  });

  it("returns an instant (duration 0) transition when reduced motion is set", () => {
    const t = tabPanelTransition(true) as Record<string, unknown>;
    expect(t.duration).toBe(0);
    expect(t.ease).toBeUndefined();
  });

  it("treats null (unknown preference) as motion allowed", () => {
    const t = tabPanelTransition(null) as Record<string, unknown>;
    expect(t.duration).toBe(0.16);
  });
});

describe("tabHeightTransition (reduced-motion aware modal-height animation)", () => {
  it("returns a spring when motion is allowed", () => {
    const t = tabHeightTransition(false) as Record<string, unknown>;
    expect(t.type).toBe("spring");
    expect(typeof t.stiffness).toBe("number");
  });

  it("returns an instant (duration 0) transition when reduced motion is set", () => {
    const t = tabHeightTransition(true) as Record<string, unknown>;
    expect(t.duration).toBe(0);
    expect(t.type).toBeUndefined();
  });

  it("treats null (unknown preference) as motion allowed", () => {
    const t = tabHeightTransition(null) as Record<string, unknown>;
    expect(t.type).toBe("spring");
  });
});

describe("tabIndicatorTransition (reduced-motion aware active-underline slide)", () => {
  it("returns the ActiveRail spring when motion is allowed", () => {
    const t = tabIndicatorTransition(false) as Record<string, unknown>;
    // Same spring shape as base-layout's ActiveRail: a duration-based bouncy spring.
    expect(t.type).toBe("spring");
    expect(t.duration).toBe(0.4);
    expect(t.bounce).toBe(0.25);
  });

  it("returns an instant (duration 0) transition when reduced motion is set", () => {
    const t = tabIndicatorTransition(true) as Record<string, unknown>;
    expect(t.duration).toBe(0);
    expect(t.type).toBeUndefined();
  });

  it("treats null (unknown preference) as motion allowed", () => {
    const t = tabIndicatorTransition(null) as Record<string, unknown>;
    expect(t.type).toBe("spring");
  });
});

describe("<Tabs.List> active-tab underline (shared-layout indicator)", () => {
  it("renders the pink underline inside the active tab only, and moves it on switch", async () => {
    function Harness() {
      const [tab, setTab] = useState("a");
      return (
        <Tabs.Root value={tab} onValueChange={(v) => setTab(v as string)}>
          <Tabs.List>
            <Tabs.Tab value="a">A</Tabs.Tab>
            <Tabs.Tab value="b">B</Tabs.Tab>
          </Tabs.List>
        </Tabs.Root>
      );
    }
    render(<Harness />);
    const tabA = screen.getByRole("tab", { name: "A" });
    const tabB = screen.getByRole("tab", { name: "B" });

    // The underline (bg-primary bar) lives inside the ACTIVE tab (A), not the other.
    expect(tabA.querySelector(".bg-primary")).not.toBeNull();
    expect(tabB.querySelector(".bg-primary")).toBeNull();

    // Switch to B → the underline is now inside B (Motion FLIPs it across).
    fireEvent.click(tabB);
    await waitFor(() => expect(tabB.querySelector(".bg-primary")).not.toBeNull());
    expect(tabA.querySelector(".bg-primary")).toBeNull();
  });

  it("clips the horizontal overshoot of the indicator bounce (no transient scrollbar), keeping the underline visible below the rail", () => {
    render(
      <Tabs.Root value="a" onValueChange={() => {}}>
        <Tabs.List>
          <Tabs.Tab value="a">A</Tabs.Tab>
          <Tabs.Tab value="b">B</Tabs.Tab>
        </Tabs.List>
      </Tabs.Root>,
    );
    const list = screen.getByRole("tablist");
    // `overflow-x-clip` contains the spring's horizontal OVERSHOOT so it never widens the
    // content box → no ~0.5s horizontal scrollbar when the underline slides to the last tab.
    expect(list.className).toContain("overflow-x-clip");
    // It clips ONLY the x-axis: y stays visible (implicit `overflow-y: visible`), so the
    // underline pinned at `bottom-[-1px]` is not cut off — the fix does not hide the rail.
    expect(list.className).not.toContain("overflow-hidden");
    expect(screen.getByRole("tab", { name: "A" }).querySelector(".bottom-\\[-1px\\]")).not.toBeNull();
  });
});

describe("<Tabs.List fullWidth> (opt-in even/justified distribution)", () => {
  it("makes every trigger flex-1 and keeps the underline on the active tab", () => {
    render(
      <Tabs.Root value="a" onValueChange={() => {}}>
        <Tabs.List fullWidth>
          <Tabs.Tab value="a">A</Tabs.Tab>
          <Tabs.Tab value="b">B</Tabs.Tab>
        </Tabs.List>
      </Tabs.Root>,
    );
    const tabA = screen.getByRole("tab", { name: "A" });
    const tabB = screen.getByRole("tab", { name: "B" });
    // Both triggers flex to an equal share of the rail (even distribution).
    expect(tabA.className).toContain("flex-1");
    expect(tabB.className).toContain("flex-1");
    // The shared-layout underline still tracks the (now-wider) ACTIVE trigger only.
    expect(tabA.querySelector(".bg-primary")).not.toBeNull();
    expect(tabB.querySelector(".bg-primary")).toBeNull();
  });

  it("leaves triggers compact (no flex-1) by default — other consumers unchanged", () => {
    render(
      <Tabs.Root value="a" onValueChange={() => {}}>
        <Tabs.List>
          <Tabs.Tab value="a">A</Tabs.Tab>
          <Tabs.Tab value="b">B</Tabs.Tab>
        </Tabs.List>
      </Tabs.Root>,
    );
    expect(screen.getByRole("tab", { name: "A" }).className).not.toContain("flex-1");
    expect(screen.getByRole("tab", { name: "B" }).className).not.toContain("flex-1");
  });
});

describe("<Tabs.AnimatedHeight> (smooth modal height on tab switch)", () => {
  it("renders its content (height-auto fallback when ResizeObserver is absent)", () => {
    render(
      <Tabs.AnimatedHeight deps="a">
        <p>Measured content</p>
      </Tabs.AnimatedHeight>,
    );
    // The wrapper never clips its child before/without a measure — content shows.
    expect(screen.getByText("Measured content")).toBeInTheDocument();
  });
});

describe("tabPanelVariants (cross-fade + directional slide)", () => {
  it("enters fading from a small +x and exits fading to a small -x", () => {
    expect(tabPanelVariants.initial).toMatchObject({ opacity: 0 });
    expect(tabPanelVariants.enter).toMatchObject({ opacity: 1, x: 0 });
    expect(tabPanelVariants.exit).toMatchObject({ opacity: 0 });
    // A directional slide: enter from the right (+x), exit to the left (-x).
    expect((tabPanelVariants.initial as { x: number }).x).toBeGreaterThan(0);
    expect((tabPanelVariants.exit as { x: number }).x).toBeLessThan(0);
  });
});

describe("<TabCount> (shared count pill)", () => {
  it("renders its children in a muted pill", () => {
    render(<TabCount>7</TabCount>);
    const pill = screen.getByText("7");
    expect(pill).toBeInTheDocument();
    expect(pill.className).toContain("rounded-full");
    expect(pill.className).toContain("bg-muted");
  });
});

describe("<Tabs.AnimatedPanel> (Motion-animated tab switch)", () => {
  function Harness() {
    const [tab, setTab] = useState("a");
    return (
      <Tabs.Root value={tab} onValueChange={(v) => setTab(v as string)}>
        <Tabs.List>
          <Tabs.Tab value="a">A</Tabs.Tab>
          <Tabs.Tab value="b">B</Tabs.Tab>
        </Tabs.List>
        <Tabs.AnimatedPanel value="a" activeValue={tab}>
          <p>Panel A body</p>
        </Tabs.AnimatedPanel>
        <Tabs.AnimatedPanel value="b" activeValue={tab}>
          <p>Panel B body</p>
        </Tabs.AnimatedPanel>
      </Tabs.Root>
    );
  }

  it("shows only the active panel's content and swaps it on tab switch", async () => {
    // Force reduced motion so the presence swap resolves synchronously in jsdom.
    const mql = (q: string): MediaQueryList =>
      ({
        matches: q.includes("reduce"),
        media: q,
        onchange: null,
        addEventListener: () => {},
        removeEventListener: () => {},
        addListener: () => {},
        removeListener: () => {},
        dispatchEvent: () => false,
      }) as unknown as MediaQueryList;
    vi.stubGlobal("matchMedia", mql);

    render(<Harness />);
    // Active tab "a": only A's content is mounted (the inactive panel's motion
    // child is not rendered).
    expect(screen.getByText("Panel A body")).toBeInTheDocument();
    expect(screen.queryByText("Panel B body")).toBeNull();

    // Switch to B → B's content enters, A's content leaves.
    fireEvent.click(screen.getByRole("tab", { name: "B" }));
    await waitFor(() => expect(screen.getByText("Panel B body")).toBeInTheDocument());
    await waitFor(() => expect(screen.queryByText("Panel A body")).toBeNull());

    vi.unstubAllGlobals();
  });
});
