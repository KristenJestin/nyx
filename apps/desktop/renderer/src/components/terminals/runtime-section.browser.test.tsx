import { render } from "@testing-library/react";
import { page, userEvent } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities and the REAL tokens: this spec measures pixels, so the stylesheet has
// to be the app's.
import "@/globals.css";

import { RuntimeSection, RuntimeSectionList } from "./runtime-section";

/**
 * THE BAND FOLDS BY ANIMATING, NOT BY DISAPPEARING.
 *
 * jsdom can say the rows left the document; it cannot say HOW. And "how" is the whole of what
 * `<CollapsibleSection>` + `sectionVariants` buy: a band that snapped shut would pass every unit
 * assertion in `workspace-runtime-tree.test.tsx` while making the sidebar jump under the pointer —
 * which is precisely what the shared section motion exists to prevent, and what the workspace
 * accordion one level up already does.
 *
 * So this spec measures the ONE thing only a real browser can answer: between "open" and "closed"
 * the body passes through INTERMEDIATE heights. No expected duration, no expected easing, no
 * expected height — a retuned spring stays green, a fold that stops animating goes red.
 */

let host: HTMLElement;

beforeEach(async () => {
  await page.viewport(900, 600);
  host = document.createElement("div");
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.width = "280px";
  host.className = "bg-background";
  document.body.appendChild(host);
});

afterEach(() => {
  host.remove();
});

/** The animated wrapper `<CollapsibleSection>` renders around the band's rows. */
function body(): HTMLElement | null {
  return host.querySelector("[data-band-rows]")?.parentElement ?? null;
}

/** Sample the body's height once per animation frame for `frames` frames. */
async function sampleHeights(frames: number): Promise<number[]> {
  const heights: number[] = [];
  for (let i = 0; i < frames; i += 1) {
    await new Promise((resolve) => requestAnimationFrame(resolve));
    const el = body();
    heights.push(el ? el.getBoundingClientRect().height : 0);
  }
  return heights;
}

it("collapses through intermediate heights rather than snapping shut", async () => {
  function Band({ open, onToggle }: { open: boolean; onToggle: () => void }) {
    return (
      <RuntimeSection
        title="Commands"
        count={3}
        open={open}
        onToggle={onToggle}
        toggleLabel="Toggle commands"
      >
        <div data-band-rows>
          <RuntimeSectionList
            rows={["a", "b", "c"].map((id) => (
              <div key={id} className="h-8 px-2 text-sm">
                {id}
              </div>
            ))}
            empty="none"
          />
        </div>
      </RuntimeSection>
    );
  }

  let open = true;
  const view = render(<Band open={open} onToggle={() => {}} />, { container: host });
  const rerender = (next: boolean) => {
    open = next;
    view.rerender(<Band open={open} onToggle={() => {}} />);
  };

  const expanded = body()?.getBoundingClientRect().height ?? 0;
  expect(expanded, "the open band has a height to lose").toBeGreaterThan(0);

  rerender(false);
  const heights = await sampleHeights(12);

  // At least one frame sits STRICTLY between the two resting states — the definition of an
  // animation, and the one thing a snap cannot produce.
  expect(
    heights.some((h) => h > 0 && h < expanded),
    `the collapse passed through no intermediate height: ${heights.join(", ")}`,
  ).toBe(true);
  // And it is monotonic enough to be a collapse rather than a flicker: it ends lower than it began.
  expect(heights.at(-1) ?? 0).toBeLessThan(expanded);
});

it("the toggle is a real button that reports its state", async () => {
  let open = true;
  function Band() {
    return (
      <RuntimeSection
        title="Commands"
        count={1}
        open={open}
        onToggle={() => {
          open = !open;
        }}
        toggleLabel="Toggle commands"
      >
        <div data-band-rows className="h-8" />
      </RuntimeSection>
    );
  }
  render(<Band />, { container: host });

  const toggle = page.getByRole("button", { name: "Toggle commands" });
  await expect.element(toggle).toHaveAttribute("aria-expanded", "true");
  await userEvent.click(toggle);
  expect(open, "the click reached the caller's handler").toBe(false);
});
