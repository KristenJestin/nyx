import { render } from "@testing-library/react";
import { page } from "vitest/browser";
import { afterEach, beforeEach, expect, it } from "vitest";

// The REAL Tailwind utilities: the panes are positioned by `paneRectStyle` but SIZED by the
// stylesheet's `h-full`/`w-full` chain, so without this the boxes would be the markup's own.
import "@/globals.css";

import { CommandPaneHost } from "./command-pane-host";
import type { CommandGroup } from "./use-command-groups";
import {
  layoutFitsFloor,
  MIN_PANE_HEIGHT_PX,
  MIN_PANE_WIDTH_PX,
} from "@/components/panes/pane-geometry";
import { commandGroupTree } from "./command-group-tree";
import type { WorkspaceRuntimeService } from "@/components/runtime/use-workspace-runtime";

/**
 * A COMMANDS GROUP, MEASURED IN REAL CHROMIUM (review 01M0ABR6V501D0SPR2BYBVZC4A, major).
 *
 * jsdom has no layout engine, so "the three commands really are side by side, each with a real box
 * over the 180×120px floor, and they come back identical after a relaunch with the disposition the
 * user set" can only be stated here.
 */

const HOST = { width: 900, height: 600 };
let host: HTMLElement;

beforeEach(async () => {
  await page.viewport(1200, 800);
  host = document.createElement("div");
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.width = `${HOST.width}px`;
  host.style.height = `${HOST.height}px`;
  document.body.appendChild(host);
});

afterEach(() => {
  host.remove();
});

function service(commandId: string): WorkspaceRuntimeService {
  return {
    id: `${commandId}-inst`,
    command_id: commandId,
    workspace_id: "ws1",
    name: commandId,
    command: `run ${commandId}`,
    subfolder: null,
    description: null,
    run_state: "idle",
    exit_code: null,
    unread: false,
  };
}

/** The group as the DB serves it back: identity, labels, and the ORDERED members. */
function group(commandIds: string[]): CommandGroup {
  return {
    id: "g1",
    project_id: "p1",
    name: "checks",
    color: 4,
    layout_shape: "auto",
    command_ids: commandIds,
  };
}

/** Every pane's measured box, keyed by the command it shows. */
function boxes(): Record<string, { left: number; top: number; width: number; height: number }> {
  const out: Record<string, { left: number; top: number; width: number; height: number }> = {};
  for (const pane of host.querySelectorAll<HTMLElement>("[data-command-pane-id]")) {
    const box = pane.getBoundingClientRect();
    out[pane.dataset.commandId as string] = {
      left: box.left,
      top: box.top,
      width: box.width,
      height: box.height,
    };
  }
  return out;
}

it("a group of THREE commands lays out side by side and comes back IDENTICAL after a relaunch", () => {
  const services = [service("c1"), service("c2"), service("c3")];
  // THE DISPOSITION THE USER SET: c2 first, so it holds the full-height left column — not the
  // automatic order the group was born with.
  const adjusted = ["c2", "c1", "c3"];

  const first = render(<CommandPaneHost group={group(adjusted)} services={services} />, {
    container: host,
  });

  const before = boxes();
  expect(Object.keys(before).sort()).toEqual(["c1", "c2", "c3"]);
  // The ADJUSTED order really is what is painted: c2 spans the full height on the left.
  expect(before.c2.left).toBeCloseTo(0, 0);
  expect(before.c2.width).toBeCloseTo(HOST.width / 2, 0);
  expect(before.c2.height).toBeCloseTo(HOST.height, 0);
  expect(before.c1.left).toBeCloseTo(HOST.width / 2, 0);
  expect(before.c1.height).toBeCloseTo(HOST.height / 2, 0);
  expect(before.c3.top).toBeCloseTo(HOST.height / 2, 0);
  // …and the panes TILE the host: no gap, no overlap.
  expect(before.c2.width + before.c1.width).toBeCloseTo(HOST.width, 0);
  expect(before.c1.height + before.c3.height).toBeCloseTo(HOST.height, 0);

  // THE FLOOR is the terminals' floor, held by every pane.
  for (const box of Object.values(before)) {
    expect(box.width).toBeGreaterThanOrEqual(MIN_PANE_WIDTH_PX);
    expect(box.height).toBeGreaterThanOrEqual(MIN_PANE_HEIGHT_PX);
  }
  expect(layoutFitsFloor(commandGroupTree(adjusted)!, HOST)).toBe(true);

  // A RELAUNCH: the view goes and comes back on what the DB serves — the same ordered members.
  first.unmount();
  host.remove();
  host = document.createElement("div");
  host.style.position = "fixed";
  host.style.top = "0";
  host.style.left = "0";
  host.style.width = `${HOST.width}px`;
  host.style.height = `${HOST.height}px`;
  document.body.appendChild(host);
  render(<CommandPaneHost group={group(adjusted)} services={services} />, { container: host });

  expect(boxes()).toEqual(before);
});

it("offers NO gesture to re-arrange: no split control, no separator, no drag", () => {
  render(
    <CommandPaneHost
      group={group(["c1", "c2", "c3"])}
      services={[service("c1"), service("c2"), service("c3")]}
    />,
    { container: host },
  );
  expect(host.querySelectorAll("[data-separator-id]")).toHaveLength(0);
  expect(host.querySelectorAll('[role="separator"]')).toHaveLength(0);
  expect(host.querySelectorAll('[aria-label="Split pane right"]')).toHaveLength(0);
  expect(host.querySelectorAll('[aria-label="Split pane down"]')).toHaveLength(0);
  expect(host.querySelectorAll('[aria-label="Close pane"]')).toHaveLength(0);

  // A POINTER DRAG across the panes changes nothing: there is no handler to move a boundary.
  const before = boxes();
  const first = host.querySelector("[data-command-pane-id]") as HTMLElement;
  const box = first.getBoundingClientRect();
  const at = (type: string, x: number) =>
    first.dispatchEvent(
      new PointerEvent(type, {
        pointerId: 1,
        pointerType: "mouse",
        isPrimary: true,
        buttons: type === "pointerup" ? 0 : 1,
        clientX: x,
        clientY: box.top + box.height / 2,
        bubbles: true,
        cancelable: true,
      }),
    );
  at("pointerdown", box.right - 2);
  at("pointermove", box.right + 120);
  at("pointerup", box.right + 120);
  expect(boxes()).toEqual(before);
});

/* ─────────── THE SEAM, MEASURED (review 01M0AT7KJV4G3C6GQNB36AV3N4) ───────────
 *
 * The arbitrage: two adjacent COMMAND panes must be told apart exactly like two terminals, while
 * no pointer gesture may re-arrange them. "Told apart" is a question about PIXELS, so it is asked
 * of a real engine, in the app's real theme, with nothing hovered — the same method the terminals'
 * resting hairline was held to (`pane-tree-host.browser.test.tsx`, finding F3).
 */

/** An sRGB sample, 0..255 per channel, alpha 0..1. */
interface Sample {
  r: number;
  g: number;
  b: number;
  a: number;
}

/**
 * A CSS colour resolved to sRGB by the ENGINE ITSELF, optionally composited over an opaque
 * `under`. Chromium hands back Tailwind's `/85` modifier as an `oklch(… / a)` string, which no
 * regex can read — a 1×1 canvas both parses and composites it the way the compositor does.
 */
function sample(color: string, under?: string): Sample {
  const canvas = document.createElement("canvas");
  canvas.width = 1;
  canvas.height = 1;
  const ctx = canvas.getContext("2d") as CanvasRenderingContext2D;
  ctx.clearRect(0, 0, 1, 1);
  if (under != null) {
    ctx.fillStyle = under;
    ctx.fillRect(0, 0, 1, 1);
  }
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
  return { r: r ?? 0, g: g ?? 0, b: b ?? 0, a: (a ?? 0) / 255 };
}

/** WCAG relative luminance of an OPAQUE sRGB sample. */
function luminance(c: Sample): number {
  const channel = (v: number) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b);
}

/** WCAG contrast ratio between two opaque samples (1 … 21). */
function contrast(a: Sample, b: Sample): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

it("the seam between two command panes is VISIBLE with no interaction, and inert", () => {
  // THE APP'S REAL THEME: `index.html` ships `<html class="dark">`, vitest's page does not.
  document.documentElement.classList.add("dark");
  try {
    render(
      <CommandPaneHost group={group(["c1", "c2"])} services={[service("c1"), service("c2")]} />,
      { container: host },
    );
    const seam = host.querySelector<HTMLElement>("[data-command-seam]") as HTMLElement;
    expect(seam, "two panes meet, so a seam is drawn").not.toBeNull();

    // (1) IT HAS A REAL BOX, on the boundary, spanning the split.
    const box = seam.getBoundingClientRect();
    expect(box.width, "the seam has a painted width").toBeGreaterThan(0);
    expect(box.height, "the seam spans the split").toBeCloseTo(HOST.height, 0);
    expect(box.left + box.width / 2, "it sits ON the boundary").toBeCloseTo(HOST.width / 2, 0);

    // (2) IT IS PAINTED — nothing was hovered, focused or dragged.
    const style = getComputedStyle(seam);
    const seamColor = style.backgroundColor;
    expect(sample(seamColor).a, "the seam is not transparent").toBeGreaterThan(0);
    expect(Number(style.opacity), "the seam is not faded out").toBe(1);

    // (3) IT READS against the surface behind it — the same WCAG 1.4.11 non-text floor (3:1) the
    //     terminals' resting hairline was held to, measured the same way (composited, not assumed).
    const backdrop = getComputedStyle(document.body).backgroundColor;
    const composited = sample(seamColor, backdrop);
    expect(contrast(composited, sample(backdrop)), "the seam reads at rest").toBeGreaterThanOrEqual(
      3,
    );

    // (4) THE SAME TREATMENT AS THE TERMINALS' RESTING TRAIT — not a colour of its own. Both are
    //     `ring/85`, so a probe painted with the terminals' own class resolves to the SAME string.
    const probe = document.createElement("div");
    probe.className = "before:bg-ring/85";
    probe.style.cssText = "position:fixed;top:-9999px;width:1px;height:1px";
    document.body.appendChild(probe);
    const terminalsTrait = getComputedStyle(probe, "::before").backgroundColor;
    probe.remove();
    expect(seamColor, "the commands seam paints the terminals' resting trait").toBe(terminalsTrait);

    // (5) AND IT IS INERT: out of the hit test, out of the a11y tree, with no resize cursor.
    expect(style.pointerEvents).toBe("none");
    expect(seam.getAttribute("aria-hidden")).toBe("true");
    expect(seam.getAttribute("role")).toBeNull();
    expect(seam.getAttribute("tabindex")).toBeNull();
    expect(style.cursor).not.toContain("resize");
    // The element under the pointer AT the boundary is the pane, never the seam.
    const atSeam = document.elementFromPoint(HOST.width / 2, HOST.height / 2);
    expect(atSeam?.closest("[data-command-seam]"), "the seam cannot be grabbed").toBeNull();
  } finally {
    document.documentElement.classList.remove("dark");
  }
});
