import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { Tabs, TabCount } from "./tabs";
import { tabHeightTransition, tabPanelTransition, tabPanelVariants } from "./tabs-motion";

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
