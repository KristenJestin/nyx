import { render } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { CompositionFrame } from "./composition-frame";

/**
 * BROWSER-MODE renderer test (real Chromium via Playwright, headless). Only a real browser can show
 * the END-TO-END of the PDR-0010 renderer: the sandboxed iframe loads under the STRICT CSP, the
 * INLINED (local, nonce'd) Tailwind engine + the nyx shim actually EXECUTE (no CDN, no network, no
 * eval), lay the markup out, and the shim posts its `resize` back to the host — which the component
 * applies as the frame height. jsdom cannot run an iframe's scripts, so this integration is only
 * observable here; the pure srcdoc/shim guarantees are covered by the unit tests.
 */

afterEach(() => {
  document.body.replaceChildren();
});

it("renders the markup in a sandboxed local-Tailwind iframe and auto-sizes from the shim", async () => {
  const markup = `<div class="p-6 text-lg"><h1 class="font-bold">Hello proto</h1>
    <p class="mt-2 text-slate-600">Rendered locally, no network.</p></div>`;

  const { container } = render(
    <CompositionFrame markup={markup} states={["idle", "loading"]} autoHeight />,
  );

  const iframe = container.querySelector("iframe");
  expect(iframe).not.toBeNull();
  // The sandbox is scripts-only (opaque origin): no same-origin escape to the host.
  expect(iframe?.getAttribute("sandbox")).toBe("allow-scripts");

  // The shim executed inside the real frame (proving the nonce'd inline scripts ran under the CSP)
  // and posted its measured content height, which the component applied — a non-trivial height
  // means the inlined engine + markup actually laid out locally.
  await vi.waitFor(
    () => {
      const h = parseInt(iframe?.style.height ?? "0", 10);
      expect(h).toBeGreaterThan(20);
    },
    { timeout: 8000, interval: 100 },
  );
});

/**
 * The TOKEN WIRING, exercised through the COMPONENT (review #42, finding
 * `01KYJ7AP3A1B7JJZZVSMFSEJFP`). `theme-tokens.browser.test.ts` proves the rendering chain over a
 * srcdoc it assembles ITSELF; here the document under test is the one `<CompositionFrame>` derived
 * from its `themeTokensJson` prop — the link that turns every preview grey when it breaks.
 *
 * The component's own frame is `sandbox="allow-scripts"` (an opaque origin: `contentDocument` is
 * unreachable BY DESIGN, and that guarantee is not weakened here), so the computed colour is read
 * from a MIRROR of the very document it built, mounted sandbox-free — the document's own strict CSP
 * still applies, so what is measured is the real styling pipeline of the real srcdoc.
 */
const INGESTED_LAYER = JSON.stringify({
  sources: [{ repository_id: "r1", repo: "web", path: "src/styles.css", variable_count: 2 }],
  // The RAW app identity + the app's own `@theme inline` alias onto it: the alias is USELESS unless
  // the component also injects the `:root` declaration it references (`rootCss: theme.root`).
  variables: { "--primary": "rgb(255, 0, 85)", "--color-primary": "var(--primary)" },
  dark: {},
});

/** Mount a MIRROR of the srcdoc a `<CompositionFrame>` built, at a given tile size, and measure it. */
function mirrorOf(srcdoc: string, width: number, height: number): HTMLIFrameElement {
  const mirror = document.createElement("iframe");
  mirror.style.width = `${width}px`;
  mirror.style.height = `${height}px`;
  mirror.style.border = "0";
  mirror.srcdoc = srcdoc;
  document.body.appendChild(mirror);
  return mirror;
}

/**
 * The CATALOGUE VIGNETTE contract (finding `01KYPD8K75EC2AAY1KVAS2XZNX`), proven where the shim
 * actually runs. `showChrome={false}` must remove the CHROME and nothing else: no chip is built
 * (the tile shows ONE instance, not the component's 13 variant axes), yet the state APPLIER still
 * runs — the active state is reflected on the body and only its `[data-nyx-state-case]` shows.
 * Simply dropping `states` would have done the opposite: with no active state the shim reveals
 * EVERY case at once. Same mirror technique as above (the real frame is an opaque sandbox).
 */
it("showChrome={false} builds no chip yet still applies the active state", async () => {
  const markup = `<div class="p-4">
      <span data-nyx-state-case="default" id="case-default">DEFAULT</span>
      <span data-nyx-state-case="destructive" id="case-destructive">DESTRUCTIVE</span>
    </div>`;

  const { container } = render(
    <CompositionFrame
      markup={markup}
      states={["default", "destructive"]}
      showChrome={false}
      title="vignette"
    />,
  );

  const srcdoc = container.querySelector("iframe")?.getAttribute("srcdoc") ?? "";
  const mirror = mirrorOf(srcdoc, 320, 160);

  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      // The shim ran (it applied the state on the body)…
      expect(doc?.body.dataset.nyxState).toBe("default");
      // …without a single chip: no chip-bar host, no `.depot-chip`.
      expect(doc?.getElementById("depot-chip-bar")).toBeNull();
      expect(doc?.querySelectorAll(".depot-chip").length).toBe(0);
      // …and the state cases are FILTERED to the active one — one instance, not an enumeration.
      expect(doc?.getElementById("case-default")?.hasAttribute("hidden")).toBe(false);
      expect(doc?.getElementById("case-destructive")?.hasAttribute("hidden")).toBe(true);
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * THE STATE IS CONTROLLED BY THE HOST (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`). The composant detail
 * selects a state in its params panel and hands it down as `activeState`; until this chaining
 * existed the frame ALWAYS fell back to `states[0]`, so the control changed nothing at all. Proven
 * where the shim really runs: given a non-default `activeState`, the rendered document shows THAT
 * state's case and hides the first one — the rendering genuinely differs.
 *
 * The chip-bar is off here on purpose: this is the detail's configuration (one control, the panel),
 * so the assertion also shows the state still applies with no chip in the document.
 */
it("renders the state it is GIVEN, not the first declared one", async () => {
  const markup = `<div class="p-4">
      <span data-nyx-state-case="default" id="case-default">DEFAULT</span>
      <span data-nyx-state-case="destructive" id="case-destructive">DESTRUCTIVE</span>
    </div>`;

  const { container } = render(
    <CompositionFrame
      markup={markup}
      states={["default", "destructive"]}
      activeState="destructive"
      showChipBar={false}
      title="detail preview"
    />,
  );

  const mirror = mirrorOf(
    container.querySelector("iframe")?.getAttribute("srcdoc") ?? "",
    320,
    160,
  );

  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      // The SELECTED state is the applied one…
      expect(doc?.body.dataset.nyxState).toBe("destructive");
      // …and it is the one that shows: the first declared state is hidden, not the other way round.
      expect(doc?.getElementById("case-destructive")?.hasAttribute("hidden")).toBe(false);
      expect(doc?.getElementById("case-default")?.hasAttribute("hidden")).toBe(true);
      // One control only: the chip-bar is absent while the dashed affordances stay (showChrome).
      expect(doc?.getElementById("depot-chip-bar")).toBeNull();
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * The VIGNETTE PRESENTATION (finding `01KYPEG16E95WADT2VMCYBPCK5`), measured in a real browser —
 * the only place layout and computed colours exist. The tile used to show its component glued to the
 * TOP-LEFT of 160 px of emptiness, on a hard-coded WHITE plate, framed by the proto-viewer's DEBUG
 * DASHES (the shim marks the active state on the body, so the green outline boxed the whole
 * preview). All three are pinned here at once: centred, on the project's own `--background` token,
 * with no outline anywhere.
 */
const VIGNETTE_TOKENS = JSON.stringify({
  variables: { "--background": "rgb(11, 15, 25)", "--foreground": "rgb(226, 232, 240)" },
  dark: {},
});

it("the vignette centres the instance on the project's background, with no debug outline", async () => {
  // The probe carries BOTH `data-nyx-*` markers — i.e. it is exactly what the proto-viewer would
  // outline in blue and green.
  const markup = `<button id="probe" data-nyx-page="home" data-nyx-state="loading"
      class="px-4 py-2">Enregistrer</button>`;

  const { container } = render(
    <CompositionFrame
      markup={markup}
      themeTokensJson={VIGNETTE_TOKENS}
      states={["default"]}
      showChrome={false}
      center
      title="vignette"
    />,
  );

  const mirror = mirrorOf(
    container.querySelector("iframe")?.getAttribute("srcdoc") ?? "",
    260,
    160,
  );

  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const win = mirror.contentWindow;
      const probe = doc?.getElementById("probe");
      expect(probe).toBeTruthy();
      // The shim ran (so what follows is the FINAL state of the document, chrome included).
      expect(doc?.body.dataset.nyxState).toBe("default");

      // 1. The CANVAS is the project's background token — not white, not grey.
      expect(win?.getComputedStyle(doc!.body).backgroundColor).toBe("rgb(11, 15, 25)");
      // …and the default text colour follows the same layer, so the instance stays readable.
      expect(win?.getComputedStyle(doc!.body).color).toBe("rgb(226, 232, 240)");

      // 2. CENTRED on both axes (the tile is 260 × 160).
      const rect = probe!.getBoundingClientRect();
      expect(Math.abs(rect.left + rect.width / 2 - 130)).toBeLessThanOrEqual(2);
      expect(Math.abs(rect.top + rect.height / 2 - 80)).toBeLessThanOrEqual(2);

      // 3. NO affordance outline, on the element that carries both markers NOR on the body.
      expect(win?.getComputedStyle(probe!).outlineStyle).toBe("none");
      expect(win?.getComputedStyle(doc!.body).outlineStyle).toBe("none");
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * "Jamais casser la carte": an instance WIDER than the tile is clipped inside it — the frame never
 * scrolls and the preview box keeps the tile's exact size.
 */
it("clips an oversized instance inside the tile instead of stretching it", async () => {
  const markup = `<div id="wide" style="min-width:600px;height:40px">A very wide component</div>`;

  const { container } = render(
    <CompositionFrame markup={markup} showChrome={false} center title="wide vignette" />,
  );

  const mirror = mirrorOf(
    container.querySelector("iframe")?.getAttribute("srcdoc") ?? "",
    260,
    160,
  );

  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      expect(doc?.getElementById("wide")).toBeTruthy();
      const root = doc?.getElementById("depot-root")?.getBoundingClientRect();
      // The instance's 600 px do not widen the root beyond the tile's content box (260 − 2 × 12).
      expect(root?.width ?? 0).toBeGreaterThan(0);
      expect(root?.width ?? 0).toBeLessThanOrEqual(236.5);
      // …and the tile does not scroll: the viewport is still exactly the frame's width.
      expect(doc?.documentElement.clientWidth).toBe(260);
      expect(doc?.body.getBoundingClientRect().width).toBeLessThanOrEqual(260);
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * THE STRUCTURAL HIDE IS A GUARANTEE, NOT A HINT (finding `01KYSCW89NRM1TSZ0TFXC8HD36`).
 *
 * `data-nyx-if` / `data-nyx-if-not` / `data-nyx-state-case` are the mechanism that makes an element
 * EXIST for some axis values only — the spinner of `loading=on`. It used to be silently inert on two
 * whole families, and only a real browser can tell (`hidden` is a *styling* effect):
 *
 *   1. an **`<svg>`** — `hidden` is an `HTMLElement` IDL attribute, so `svgEl.hidden = true` only set
 *      a JS expando and NEVER reflected the attribute: the spinner stayed visible for ever, which is
 *      exactly the reported symptom and why wrapping it in a `<span>` "fixed" it;
 *   2. an element carrying a **display UTILITY** (`inline-flex`, `grid`, `block`…) or living under
 *      Tailwind's Preflight (`svg { display: block }`) — an AUTHOR rule, which beats the browser's
 *      own `[hidden] { display: none }`.
 *
 * Both halves are asserted through `getComputedStyle`, in both directions (hidden when the condition
 * fails, VISIBLE when it holds), because "the attribute is there" was precisely the false comfort.
 */
const LOADING_AXIS = [
  {
    name: "loading",
    values: [{ name: "off" }, { name: "on", classes: "opacity-70" }],
    defaultValue: "off",
  },
] as const;

/**
 * A button whose spinner is an `<svg>` (the Preflight family), next to a badge carrying a display
 * UTILITY (the other family). The badge sits OUTSIDE the flex button on purpose: a flex ITEM is
 * blockified by the browser (`inline-flex` computes to `flex`), which would blur what is being
 * measured — here `inline-flex` stays `inline-flex` when the element is shown.
 */
const SPINNER_MARKUP = `<div id="wrap">
    <button id="btn" class="inline-flex items-center gap-2 px-4 py-2">
      <svg id="spinner" data-nyx-if="loading=on" viewBox="0 0 24 24" width="16" height="16"
        aria-hidden="true"><circle cx="12" cy="12" r="10" fill="none" stroke="currentColor"
        stroke-width="3" /></svg>
      <span id="label" data-nyx-if-not="loading=on">Enregistrer</span>
    </button>
    <span id="badge" data-nyx-if="loading=on" class="inline-flex">1</span>
  </div>`;

/** The srcdoc `<CompositionFrame>` builds for a given active combination, mirrored and measured. */
function frameFor(axisValues: Record<string, string>): HTMLIFrameElement {
  const { container } = render(
    <CompositionFrame
      markup={SPINNER_MARKUP}
      axes={LOADING_AXIS}
      axisValues={axisValues}
      showChrome={false}
      title={`loading=${axisValues.loading}`}
    />,
  );
  return mirrorOf(container.querySelector("iframe")?.getAttribute("srcdoc") ?? "", 320, 120);
}

it("really hides a structural element — an svg (Preflight) and a display utility", async () => {
  const off = frameFor({ loading: "off" });

  await vi.waitFor(
    () => {
      const doc = off.contentDocument;
      const win = off.contentWindow;
      // The shim ran to completion (so what follows is the document's FINAL styling).
      expect(doc?.body.getAttribute("data-nyx-axis-loading")).toBe("off");
      const display = (id: string) =>
        win!.getComputedStyle(doc!.getElementById(id)!).getPropertyValue("display");
      // The svg spinner: Preflight says `display: block`, the condition says it does not exist.
      expect(display("spinner")).toBe("none");
      // The badge carries `inline-flex` — a utility, i.e. another author rule.
      expect(display("badge")).toBe("none");
      // …and the element whose condition HOLDS is untouched.
      expect(display("label")).not.toBe("none");
    },
    { timeout: 10000, interval: 100 },
  );

  const on = frameFor({ loading: "on" });

  await vi.waitFor(
    () => {
      const doc = on.contentDocument;
      const win = on.contentWindow;
      expect(doc?.body.getAttribute("data-nyx-axis-loading")).toBe("on");
      const display = (id: string) =>
        win!.getComputedStyle(doc!.getElementById(id)!).getPropertyValue("display");
      // Hiding is REVERSIBLE and gives each element back its own display — never a leaked `none`.
      expect(display("spinner")).toBe("block");
      expect(display("badge")).toBe("inline-flex");
      expect(display("label")).toBe("none");
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * The COROLLARY the finding insists on: a template that enumerates its cases must show exactly ONE.
 * With the hide broken by an author rule, an 8-case `button` displayed all eight at once — a
 * "rendering" nothing flagged as wrong. The cases carry `inline-flex` on purpose: that utility is
 * what used to win over the hidden attribute.
 */
const EIGHT_CASES = [
  "default",
  "secondary",
  "destructive",
  "outline",
  "ghost",
  "link",
  "success",
  "warning",
];

it("shows exactly ONE of 8 state cases even when they carry a display utility", async () => {
  const markup = `<div class="flex flex-col">${EIGHT_CASES.map(
    (name) =>
      `<span id="case-${name}" data-nyx-state-case="${name}" class="inline-flex px-2">${name}</span>`,
  ).join("")}</div>`;

  const { container } = render(
    <CompositionFrame
      markup={markup}
      states={EIGHT_CASES}
      activeState="ghost"
      showChrome={false}
      title="8 cases"
    />,
  );
  const mirror = mirrorOf(
    container.querySelector("iframe")?.getAttribute("srcdoc") ?? "",
    320,
    240,
  );

  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const win = mirror.contentWindow;
      expect(doc?.body.dataset.nyxState).toBe("ghost");
      const shown = EIGHT_CASES.filter(
        (name) =>
          win!
            .getComputedStyle(doc!.getElementById(`case-${name}`)!)
            .getPropertyValue("display") !== "none",
      );
      expect(shown).toEqual(["ghost"]);
    },
    { timeout: 10000, interval: 100 },
  );
});

it("themes the document it builds from the project's tokens (bg-primary is not grey)", async () => {
  const { container } = render(
    <CompositionFrame
      markup={`<div id="probe" class="bg-primary p-6">Primary</div>`}
      themeTokensJson={INGESTED_LAYER}
      title="themed preview"
    />,
  );

  const mounted = container.querySelector("iframe");
  // The real preview keeps its opaque sandbox — the mirror below is the spec's own frame.
  expect(mounted?.getAttribute("sandbox")).toBe("allow-scripts");
  const srcdoc = mounted?.getAttribute("srcdoc") ?? "";
  expect(srcdoc).not.toBe("");

  const mirror = document.createElement("iframe");
  mirror.style.width = "480px";
  mirror.style.height = "240px";
  mirror.srcdoc = srcdoc;
  document.body.appendChild(mirror);

  // The LOCAL engine generates `bg-primary` from the `@theme` alias, and it resolves through the
  // `:root` declaration the COMPONENT injected — the whole chain, in a real browser.
  await vi.waitFor(
    () => {
      const probe = mirror.contentDocument?.getElementById("probe");
      expect(probe).toBeTruthy();
      expect(
        probe ? (mirror.contentWindow?.getComputedStyle(probe).backgroundColor ?? "") : "",
      ).toBe("rgb(255, 0, 85)");
    },
    { timeout: 10000, interval: 100 },
  );
});

/**
 * THE SCOPED CSS SLOT, IN A REAL BROWSER (findings `01KYVMTWB71R0D0CT1SV88J8DX` /
 * `01KYVVEQZW2Y2VY7ZAPK5RECZS`).
 *
 * Two things only a browser can prove:
 *
 *  1. THE SLOT APPLIES. The sidecar appends `<style data-nyx-css-slot="…">` to the markup, and the
 *     document's own CSP (`style-src 'unsafe-inline'`, `default-src 'none'`) must let it style the
 *     component WITHOUT being relaxed for it — a slot that needed a looser CSP would be a design
 *     failure, which is why `external_resource` is a refusal.
 *  2. THE MARK IS DRAWN OFF THE MARKUP. The frame styles `[data-nyx-css]`, the attribute the sidecar
 *     stamped on the component's root — no list, no scope id, nothing re-derived host-side. The
 *     `::after` badge really renders, on that element and on no other.
 */
it("applies a scoped CSS slot under the strict CSP and marks the node it is stamped on", async () => {
  const SCOPE = "c-spinner-8f2a";
  const markup =
    `<div id="slotted" data-nyx-css="${SCOPE}" class="p-6">Spinner</div>` +
    `<div id="plain" class="p-6">Plain</div>` +
    `<style data-nyx-css-slot="${SCOPE}">[data-nyx-css~="${SCOPE}"]{background-color:rgb(9, 88, 217)}</style>`;

  const { container } = render(<CompositionFrame markup={markup} title="slotted preview" />);
  const mirror = mirrorOf(
    container.querySelector("iframe")?.getAttribute("srcdoc") ?? "",
    480,
    240,
  );

  await vi.waitFor(
    () => {
      const doc = mirror.contentDocument;
      const win = mirror.contentWindow;
      const slotted = doc?.getElementById("slotted");
      const plain = doc?.getElementById("plain");
      expect(slotted && plain).toBeTruthy();
      // 1. The slot's own stylesheet applied — under the frame's unchanged CSP.
      expect(win?.getComputedStyle(slotted!).backgroundColor).toBe("rgb(9, 88, 217)");
      // 2. The MARK is on the stamped element: the badge renders and the node is outlined…
      const badge = win?.getComputedStyle(slotted!, "::after");
      expect(badge?.content ?? "").toContain("css slot");
      expect(win?.getComputedStyle(slotted!).outlineStyle).toBe("dashed");
      // …and NOT on the element the sidecar did not stamp — the attribute is the whole criterion.
      expect(win?.getComputedStyle(plain!).outlineStyle).toBe("none");
      expect(win?.getComputedStyle(plain!, "::after").content ?? "").not.toContain("css slot");
    },
    { timeout: 10000, interval: 100 },
  );
});
