import { RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { createAppRouter } from "@/router";

import type { DesignComponentRow } from "./types";

/**
 * THE END-TO-END OF A **MULTI-AXIS** COMPONENT (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`), in a real
 * Chromium.
 *
 * The report was four symptoms of ONE defect: booleans inert, a single pilotable axis, a variant
 * that was ambiguous (param or state?), and a template that had to enumerate every case (8 buttons
 * for a `button`, 48 for `variant × size × loading`). The model is now NAMED AXES whose values carry
 * their own concrete classes, applied by the SHIM.
 *
 * This test pins what that buys, on the exact shape a real design system has:
 *
 *   ONE `<button>` in the template — not 8, not 48 — declared with `variant × size × loading`;
 *   the panel's PER-AXIS controls → `axisValues` → the frame's `srcdoc` → `window.__nyxAxes`
 *   → the shim's applier → the classes ACTUALLY on the element, and the spinner ACTUALLY in the DOM.
 *
 * Only a browser can close it: the last links are JavaScript running inside the frame, and jsdom
 * executes no iframe script. The live preview is `sandbox="allow-scripts"` (an opaque origin — its
 * `contentDocument` is unreachable BY DESIGN), so what is measured is a MIRROR of the very `srcdoc`
 * the page built, mounted sandbox-free; the document's own strict CSP still applies, so the shim
 * runs exactly as it does in the real preview.
 */

/** A `button` synthesized THE NEW WAY: ONE element, three axes, classes on the values. */
const AXES_BUTTON: DesignComponentRow = {
  id: "c1",
  project_id: "p1",
  name: "Button",
  source_repository_id: null,
  source_path: "src/ui/Button.tsx",
  visual_hash: null,
  template:
    `<button id="btn" class="inline-flex items-center rounded-md font-medium">` +
    `<i id="spinner" data-nyx-if="loading=on">…</i>` +
    `<span id="label" data-nyx-if-not="loading=on">{{label}}</span>` +
    `</button>`,
  params_json: JSON.stringify([
    { name: "label", type: "text", required: true, preview: "Enregistrer" },
  ]),
  states_json: JSON.stringify({
    axes: [
      {
        name: "variant",
        values: [
          { name: "primary", classes: "bg-primary text-primary-foreground" },
          { name: "destructive", classes: "bg-destructive text-destructive-foreground" },
        ],
      },
      {
        name: "size",
        values: [
          { name: "md", classes: "h-9 px-4 text-sm" },
          { name: "lg", classes: "h-11 px-6 text-base" },
        ],
      },
      {
        name: "loading",
        values: [{ name: "off" }, { name: "on", classes: "pointer-events-none opacity-70" }],
      },
    ],
  }),
  created_at: 0,
  updated_at: 0,
  scanned_input_hash: null,
  orphaned_at: null,
};

/** The sidecar's `design_render`, in miniature: a logic-less substitution of the composition params. */
function fakeRender(compositionJson: string): { markup: string } {
  const { params } = JSON.parse(compositionJson) as { params: Record<string, unknown> };
  return {
    markup: AXES_BUTTON.template.replace(/\{\{(\w+)\}\}/g, (_m, k: string) =>
      String(params[k] ?? ""),
    ),
  };
}

function installBackend() {
  (nyxBridge as FakeNyxBridge).onAnyCommand((cmd, args) => {
    switch (cmd) {
      case "list_projects":
        return [
          {
            id: "p1",
            name: "Project",
            key: "NYX",
            collapsed: false,
            created_at: 0,
            updated_at: 0,
            resume_agent_sessions: false,
          },
        ];
      case "list_workspaces":
        return [];
      case "design_get":
        return null;
      case "design_component_get":
        return AXES_BUTTON;
      case "design_render":
        return fakeRender((args as { compositionJson: string }).compositionJson);
      default:
        return undefined;
    }
  });
}

/** What the shim ACTUALLY did inside a mirrored frame: the button's classes + the spinner. */
async function rendered(
  srcdoc: string,
): Promise<{ classes: string[]; spinner: boolean; label: boolean }> {
  const mirror = document.createElement("iframe");
  mirror.style.width = "480px";
  mirror.style.height = "220px";
  mirror.style.border = "0";
  mirror.srcdoc = srcdoc;
  document.body.appendChild(mirror);
  let out = { classes: [] as string[], spinner: false, label: false };
  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const btn = doc?.getElementById("btn");
      expect(btn).toBeTruthy();
      // The shim ran: it reflected the whole combination on the body.
      expect(doc?.body.getAttribute("data-nyx-axis-variant")).toBeTruthy();
      out = {
        classes: [...btn!.classList],
        spinner: doc!.getElementById("spinner")!.hasAttribute("hidden") === false,
        label: doc!.getElementById("label")!.hasAttribute("hidden") === false,
      };
    },
    { timeout: 10000, interval: 100 },
  );
  mirror.remove();
  return out;
}

const srcdocOf = () => screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "";

/** Pick a value on ONE axis, in the panel's control for that axis. */
function pick(axis: string, value: string) {
  fireEvent.click(within(screen.getByRole("group", { name: axis })).getByText(value));
}

afterEach(() => {
  (nyxBridge as FakeNyxBridge).reset();
  document.body.replaceChildren();
  localStorage.clear();
});

it("pilots a TWO-AXIS component axis by axis, and a boolean really changes the rendering", async () => {
  installBackend();
  render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

  await screen.findByTitle("Button live preview");
  // ONE CONTROL PER AXIS — not a single "state" dropdown standing in for all three.
  expect(screen.getByRole("group", { name: "variant" })).toBeTruthy();
  expect(screen.getByRole("group", { name: "size" })).toBeTruthy();
  expect(screen.getByRole("group", { name: "loading" })).toBeTruthy();
  // Nothing is inert: every value carries classes or a structural case.
  expect(screen.queryByTestId("state-cases-missing")).toBeNull();

  // 1. The aperçu opens on the DEFAULT COMBINATION (the first value of each axis).
  let view = await rendered(srcdocOf());
  expect(view.classes).toContain("bg-primary");
  expect(view.classes).toContain("h-9");
  expect(view.classes).toContain("rounded-md"); // the template's own classes are never touched
  expect(view.spinner).toBe(false);
  expect(view.label).toBe(true);

  // 2. Flip the SIZE axis alone. `variant` must not move — that is the whole promise of an axis.
  pick("size", "lg");
  await vi.waitFor(() => expect(srcdocOf()).toContain(`"size":"lg"`));
  view = await rendered(srcdocOf());
  expect(view.classes).toContain("h-11");
  expect(view.classes).not.toContain("h-9");
  expect(view.classes).toContain("bg-primary");

  // 3. Flip the VARIANT axis. `size` stays at lg — the combination is a product of INDEPENDENT axes,
  //    and the template carries ONE element for all four (variant × size) combinations.
  pick("variant", "destructive");
  await vi.waitFor(() => expect(srcdocOf()).toContain(`"variant":"destructive"`));
  view = await rendered(srcdocOf());
  expect(view.classes).toContain("bg-destructive");
  expect(view.classes).not.toContain("bg-primary");
  expect(view.classes).toContain("h-11");

  // 4. THE BOOLEAN — an axis with two values. It changes the rendering in BOTH ways at once: the
  //    classes of `on`, and the STRUCTURE (`data-nyx-if`) that classes cannot express.
  pick("loading", "on");
  await vi.waitFor(() => expect(srcdocOf()).toContain(`"loading":"on"`));
  view = await rendered(srcdocOf());
  expect(view.spinner).toBe(true);
  expect(view.label).toBe(false);
  expect(view.classes).toContain("opacity-70");
  // …and the two other axes are exactly where they were left.
  expect(view.classes).toContain("bg-destructive");
  expect(view.classes).toContain("h-11");
});
