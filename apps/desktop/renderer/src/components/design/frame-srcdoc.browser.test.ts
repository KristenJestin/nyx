import { afterEach, expect, it, vi } from "vitest";

import { type VarianceAxis } from "./axes";
import { buildFrameSrcdoc } from "./frame-srcdoc";
import { PROTO_SHIM_SOURCE } from "./proto-shim";

/**
 * BROWSER-MODE proof that the frame's `[hidden]` rule is what it claims to be — the LAST WORD on
 * `display` (finding `01KYSCW89NRM1TSZ0TFXC8HD36`).
 *
 * The structural mechanism (`data-nyx-if`, `data-nyx-if-not`, `data-nyx-state-case`) marks an absent
 * element with the `hidden` attribute, and the browser's `[hidden]{display:none}` is a USER-AGENT
 * rule — the weakest thing in the cascade. Anything an author ships beats it, and the frame is FULL of
 * author CSS it does not own: Tailwind's Preflight (`svg{display:block}`), the display utilities, and
 * the template's own `<style>`, which lives in the BODY and therefore comes after every stylesheet in
 * the head.
 *
 * This spec measures that claim WITHOUT the real Tailwind engine, on purpose: Tailwind's Preflight
 * currently ships an equivalent `[hidden]` rule of its own, so running the engine would prove a
 * DEPENDENCY's guarantee instead of ours. Here the only author CSS is the template's — pointed
 * straight at the hidden elements with ID selectors, i.e. more specific than `[hidden]` and last in
 * document order. Nothing but the frame's `!important` can win that, in either direction.
 *
 * The frame is mounted WITHOUT `sandbox` so the spec can read `contentDocument`; the document's own
 * strict CSP is unchanged (`composition-frame.browser.test.tsx` covers the real, sandboxed host).
 */

const LOADING_AXIS: readonly VarianceAxis[] = [
  {
    name: "loading",
    values: [{ name: "off" }, { name: "on" }],
    defaultValue: "off",
  },
];

/**
 * A template that fights the hide: its own stylesheet pins a `display` on each conditional element,
 * with an ID selector (more specific than `[hidden]`) from the body (later than the head).
 */
const MARKUP = `<style>
    #spinner { display: block }
    #badge { display: inline-flex }
    #label { display: inline }
  </style>
  <svg id="spinner" data-nyx-if="loading=on" viewBox="0 0 24 24" width="16" height="16"
    aria-hidden="true"><circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" /></svg>
  <span id="badge" data-nyx-if="loading=on">1</span>
  <span id="label" data-nyx-if-not="loading=on">Enregistrer</span>`;

/** The real srcdoc for a combination, with a NO-OP engine in place of Tailwind (see the header). */
function srcdocFor(loading: string): string {
  return buildFrameSrcdoc({
    markup: MARKUP,
    tailwindSource: `/* no Tailwind engine in this spec: the guarantee is the frame's own */`,
    shimSource: PROTO_SHIM_SOURCE,
    themeCss: "",
    nonce: "NONCEFORTEST",
    states: [],
    axes: LOADING_AXIS,
    axisValues: { loading },
    showChrome: false,
  });
}

/** Mount a srcdoc frame and resolve once the shim has applied the combination. */
async function mount(loading: string): Promise<HTMLIFrameElement> {
  const iframe = document.createElement("iframe");
  iframe.style.width = "320px";
  iframe.style.height = "120px";
  iframe.srcdoc = srcdocFor(loading);
  document.body.appendChild(iframe);
  await vi.waitFor(
    () => {
      expect(iframe.contentDocument?.body.getAttribute("data-nyx-axis-loading")).toBe(loading);
    },
    { timeout: 8000, interval: 50 },
  );
  return iframe;
}

/** The COMPUTED display of an element inside a mounted frame — the only answer that matters. */
function displayOf(iframe: HTMLIFrameElement, id: string): string {
  const el = iframe.contentDocument?.getElementById(id);
  if (!el) return "";
  return iframe.contentWindow?.getComputedStyle(el).getPropertyValue("display") ?? "";
}

afterEach(() => {
  document.body.replaceChildren();
});

it("hides a structural element against the template's own display rules", async () => {
  const iframe = await mount("off");

  // `loading=off`: neither the svg (Preflight's family) nor the badge (a display utility's family)
  // exists — and the template's `#spinner{display:block}` / `#badge{display:inline-flex}` lose.
  expect(displayOf(iframe, "spinner")).toBe("none");
  expect(displayOf(iframe, "badge")).toBe("none");
  // The element whose condition HOLDS keeps the display the template gave it.
  expect(displayOf(iframe, "label")).toBe("inline");
});

it("gives each element its own display back when the condition holds", async () => {
  const iframe = await mount("on");

  // The guarantee is a hide, not a pin: showing again yields the template's display, not a leftover.
  expect(displayOf(iframe, "spinner")).toBe("block");
  expect(displayOf(iframe, "badge")).toBe("inline-flex");
  expect(displayOf(iframe, "label")).toBe("none");
});
