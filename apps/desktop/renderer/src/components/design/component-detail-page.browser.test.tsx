import { RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { nyxBridge } from "@/bridge";
import type { FakeNyxBridge } from "@/bridge/fake";
import { createAppRouter } from "@/router";

import type { DesignComponentRow } from "./types";

/**
 * THE END-TO-END OF A VARIANT (finding `01KYPSG1HM2GPJMNH2SP3DDADH`), in a real Chromium.
 *
 * The report was "les dropdowns d'état/variante sont cosmétiques : changer une variante ne change
 * rien au rendu — c'est le modèle". The mechanism existed all along: the shim shows the element
 * whose `data-nyx-state-case` equals the ACTIVE state and hides the others, so a LOGIC-LESS
 * template renders a whole variant axis by carrying one case per state. What was missing was the
 * MATTER — the manual never named the attribute, so every synthesis carried a single hard-coded
 * variant. This test pins the WHOLE chain on a component synthesized the way the manual now asks:
 *
 *   the state param of the detail's panel → `activeState` → the frame's `srcdoc`
 *   → `window.__nyxActiveState` → the shim's applier → which case is VISIBLE.
 *
 * Only a browser can close it: the last two links are JavaScript running inside the frame, and jsdom
 * executes no iframe script. The live preview is `sandbox="allow-scripts"` (an opaque origin — its
 * `contentDocument` is unreachable BY DESIGN, and that is not weakened here), so what is measured is
 * a MIRROR of the very `srcdoc` the page built, mounted sandbox-free; the document's own strict CSP
 * still applies, so the shim runs exactly as it does in the real preview.
 */

/** A `button` synthesized ACCORDING TO THE CONTRACT: one case per declared state, CVA classes inlined. */
const STATE_CASES: DesignComponentRow = {
  id: "c1",
  project_id: "p1",
  name: "Button",
  source_repository_id: null,
  source_path: "src/ui/Button.tsx",
  visual_hash: null,
  template: [
    `<button id="case-default" data-nyx-state-case="default" class="rounded-md bg-primary px-4 py-2 text-primary-foreground">{{label}}</button>`,
    `<button id="case-destructive" data-nyx-state-case="destructive" class="rounded-md bg-destructive px-4 py-2 text-destructive-foreground">{{label}}</button>`,
    `<button id="case-outline" data-nyx-state-case="outline" class="rounded-md border border-input px-4 py-2">{{label}}</button>`,
  ].join(""),
  params_json: JSON.stringify([
    { name: "label", type: "text", required: true, preview: "Enregistrer" },
  ]),
  states_json: JSON.stringify(["default", "destructive", "outline"]),
  created_at: 0,
  updated_at: 0,
  scanned_input_hash: null,
  orphaned_at: null,
};

/** The sidecar's `design_render`, in miniature: a logic-less substitution of the composition params. */
function fakeRender(compositionJson: string): { markup: string } {
  const { params } = JSON.parse(compositionJson) as { params: Record<string, unknown> };
  return {
    markup: STATE_CASES.template.replace(/\{\{(\w+)\}\}/g, (_m, k: string) =>
      String(params[k] ?? ""),
    ),
  };
}

/**
 * The backend, installed on the bridge the app ACTUALLY holds. Outside Electron (`window.nyxCore`
 * absent — a plain browser tab) `@/bridge` already resolves to an in-memory [`FakeNyxBridge`], so
 * the catch-all goes straight onto it: no module mock, and the page under test talks to the same
 * object it would in production, one adapter down.
 */
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
        return STATE_CASES;
      case "design_render":
        return fakeRender((args as { compositionJson: string }).compositionJson);
      default:
        return undefined;
    }
  });
}

/** Mount a MIRROR of the `srcdoc` the page built (the real frame is an opaque sandbox) and run it. */
function mirrorOf(srcdoc: string): HTMLIFrameElement {
  const mirror = document.createElement("iframe");
  mirror.style.width = "480px";
  mirror.style.height = "220px";
  mirror.style.border = "0";
  mirror.srcdoc = srcdoc;
  document.body.appendChild(mirror);
  return mirror;
}

/** Which state case the shim left VISIBLE inside a mirrored frame (there is exactly one). */
async function visibleCase(srcdoc: string): Promise<string> {
  const mirror = mirrorOf(srcdoc);
  let shown = "";
  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const cases = [...(doc?.querySelectorAll("[data-nyx-state-case]") ?? [])];
      expect(cases.length).toBe(3);
      // The shim ran: it reflected the active state on the body and filtered the cases.
      expect(doc?.body.dataset.nyxState).toBeTruthy();
      const visible = cases.filter((el) => !el.hasAttribute("hidden"));
      expect(visible.length).toBe(1);
      shown = visible[0]!.getAttribute("data-nyx-state-case") ?? "";
    },
    { timeout: 10000, interval: 100 },
  );
  mirror.remove();
  return shown;
}

afterEach(() => {
  (nyxBridge as FakeNyxBridge).reset();
  document.body.replaceChildren();
  localStorage.clear();
});

it("selecting a state on the DETAIL page changes which variant the preview renders", async () => {
  installBackend();
  render(<RouterProvider router={createAppRouter(["/design/c1"])} />);

  const frame = await screen.findByTitle("Button live preview");
  // The synthesis honours the contract, so the page flags NO missing case.
  expect(screen.queryByTestId("state-cases-missing")).toBeNull();

  // 1. The aperçu opens on the FIRST declared state — one case visible, the two others hidden.
  expect(await visibleCase(frame.getAttribute("srcdoc") ?? "")).toBe("default");

  // 2. Pick another variant in the params panel — the ONE state control of this page.
  fireEvent.click(screen.getByRole("button", { name: "destructive" }));
  await vi.waitFor(() => {
    expect(screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "").toContain(
      `window.__nyxActiveState="destructive"`,
    );
  });

  // 3. …and the RENDERING really moved: the destructive case is the visible one now.
  expect(
    await visibleCase(screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? ""),
  ).toBe("destructive");

  // 4. A third one, to show it is a real axis and not a one-shot toggle.
  fireEvent.click(screen.getByRole("button", { name: "outline" }));
  await vi.waitFor(() => {
    expect(screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? "").toContain(
      `window.__nyxActiveState="outline"`,
    );
  });
  expect(
    await visibleCase(screen.getByTitle("Button live preview").getAttribute("srcdoc") ?? ""),
  ).toBe("outline");
});
