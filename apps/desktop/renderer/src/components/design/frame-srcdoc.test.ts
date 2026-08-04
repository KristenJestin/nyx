import { describe, expect, test } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import { buildFrameSrcdoc, PICK_CLASSES, UNRENDERED_CLASS } from "./frame-srcdoc";

const base = {
  markup: `<button data-nyx-page="catalogue">Go</button>`,
  tailwindSource: "/*TAILWIND-ENGINE*/window.__tw=1;",
  shimSource: "/*SHIM*/window.__shim=1;",
  themeCss: "  --color-primary: oklch(0.5 0.2 250);",
  nonce: "NONCE123",
  states: ["idle", "loading"],
  activeState: "idle",
};

describe("buildFrameSrcdoc", () => {
  test("emits a strict CSP that forbids the network (no CDN, no fetch)", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).toContain("Content-Security-Policy");
    // The CSP lives in a double-quoted attribute; single quotes are not escaped, so the intent
    // reads back directly.
    const csp = html.match(/Content-Security-Policy" content="([^"]*)"/)?.[1] ?? "";
    expect(csp).toContain("default-src 'none'");
    expect(csp).toContain("connect-src 'none'");
    expect(csp).toContain("script-src 'nonce-NONCE123'");
    expect(csp).toContain("style-src 'unsafe-inline'");
  });

  test("inlines the LOCAL Tailwind engine + shim — no external/CDN URL anywhere", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).toContain("/*TAILWIND-ENGINE*/");
    expect(html).toContain("/*SHIM*/");
    // Not a single network origin in the whole document (no CDN, no http(s), no protocol-relative).
    expect(html).not.toMatch(/https?:\/\//);
    expect(html).not.toMatch(/src=["']\/\//);
    expect(html).not.toMatch(/cdn/i);
  });

  test("gates every script with the render nonce (agent ships zero un-nonced JS)", () => {
    const html = buildFrameSrcdoc(base);
    const scripts = [...html.matchAll(/<script([^>]*)>/g)].map((m) => m[1]);
    expect(scripts.length).toBeGreaterThanOrEqual(2);
    for (const attrs of scripts) {
      expect(attrs).toContain('nonce="NONCE123"');
    }
  });

  test("themes from the design tokens via an @theme block", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).toContain('<style type="text/tailwindcss"');
    expect(html).toContain('@import "tailwindcss"');
    expect(html).toContain("@theme {");
    expect(html).toContain("--color-primary: oklch(0.5 0.2 250);");
  });

  /**
   * DONE-CRITERION (review #42): the frame must also carry the RAW app variables the `@theme`
   * entries point at. `@theme inline { --color-primary: var(--primary) }` — the shadcn/Tailwind-v4
   * idiom every real front uses — resolves to NOTHING without the `:root { --primary: … }`
   * declaration, which is exactly why every preview rendered grey.
   */
  test("loads the raw token stylesheet (:root/.dark) alongside the @theme block", () => {
    const html = buildFrameSrcdoc({
      ...base,
      themeCss: "  --color-primary: var(--primary);",
      rootCss: ":root {\n  --primary: oklch(0.55 0.21 264);\n}\n.dark {\n  --primary: #223;\n}",
    });
    expect(html).toContain("--primary: oklch(0.55 0.21 264);");
    expect(html).toContain(".dark {");
    // Still LOCAL: the tokens are an inline <style> of the document, not a fetched stylesheet.
    expect(html).not.toMatch(/https?:\/\//);
    expect(html).not.toContain("<link");
    // The two blocks coexist: the utility definition AND the declaration it references.
    const themeAt = html.indexOf('<style type="text/tailwindcss"');
    const rootAt = html.indexOf("--primary: oklch(0.55 0.21 264);");
    expect(themeAt).toBeGreaterThanOrEqual(0);
    expect(rootAt).toBeGreaterThan(themeAt);
  });

  test("an absent rootCss keeps the document valid (no theme is still a theme)", () => {
    const html = buildFrameSrcdoc({ ...base, rootCss: undefined });
    expect(html).toContain("@theme {");
    expect(html).toContain("<body>");
  });

  /**
   * DONE-CRITERION (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`): the frame emitted BOTH variants of the
   * project's own tokens from the start, and then selected only the LIGHT one — nothing ever marked
   * the document dark. That, and not a hard-coded white, is why a preview stayed white inside a dark
   * app. `dark` is the SELECTOR of the variant the design system itself declares.
   */
  test("dark: selects the project's OWN .dark variant (class on <html> + color-scheme)", () => {
    const rootCss =
      ":root {\n  --background: oklch(0.99 0 0);\n}\n.dark {\n  --background: oklch(0.18 0 0);\n}";
    const dark = buildFrameSrcdoc({ ...base, rootCss, dark: true });
    expect(dark).toContain(`<html lang="en" class="dark">`);
    expect(dark).toContain("color-scheme:dark");
    // The variant is SELECTED, never repainted: both blocks are still the project's, verbatim, and
    // the `.dark` one comes last so — at equal specificity — it is the one that wins.
    expect(dark.indexOf(".dark {")).toBeGreaterThan(dark.indexOf(":root {"));
    expect(dark).toContain("--background: oklch(0.18 0 0);");
    // …and no colour of nyx's own is ever painted over the design system.
    expect(dark).not.toContain("background-color:#");

    // Default / explicit light: the document is NOT marked, so `:root` applies — which is the only
    // honest rendering for a layer that declares no dark variant.
    for (const html of [
      buildFrameSrcdoc({ ...base, rootCss }),
      buildFrameSrcdoc({ ...base, rootCss, dark: false }),
    ]) {
      expect(html).toContain(`<html lang="en">`);
      expect(html).not.toContain(`class="dark"`);
      expect(html).not.toContain("color-scheme:dark");
    }
  });

  test("exposes the declared states + active state to the shim, and injects the markup", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).toContain(`window.__nyxStates=["idle","loading"]`);
    expect(html).toContain(`window.__nyxActiveState="idle"`);
    expect(html).toContain(`data-nyx-page="catalogue"`);
    expect(html).toContain(`id="depot-chip-bar"`);
  });

  /**
   * Findings `01KYPD8K75EC2AAY1KVAS2XZNX` (chip-bar) then `01KYPEG16E95WADT2VMCYBPCK5` (outlines) —
   * the CHROME is the affordance layer of a host one INTERACTS with, and a 160 px catalogue vignette
   * only DISPLAYS one instance. `showChrome: false` removes the chip-bar HOST ELEMENT (the shim's
   * builder then no-ops) AND the dashed `data-nyx-*` outlines, while `__nyxStates` /
   * `__nyxActiveState` still reach the shim, so the APPLIER still selects the active state case.
   * Dropping the states instead would show EVERY case at once — the opposite of the intent.
   */
  test("showChrome: false drops the chip-bar host AND the affordance outlines", () => {
    const html = buildFrameSrcdoc({ ...base, showChrome: false });
    expect(html).not.toContain(`id="depot-chip-bar"`);
    expect(html).not.toContain(`aria-label="States"`);
    // Not one dashed OUTLINE rule: the debug dashes that framed the tile are gone entirely. (The
    // dashed box a node the sidecar could not render is named in is NOT chrome — it is the
    // composition's own state, so it is emitted whatever the host asks for; see `.nyx-unrendered`.)
    expect(html).not.toContain("[data-nyx-page]{");
    expect(html).not.toContain("[data-nyx-state]:not(body){");
    expect(html).not.toContain("outline:1px dashed");
    expect(html).not.toContain(".depot-chip{");
    // The state model is untouched: the shim still knows the axes and which one is active.
    expect(html).toContain(`window.__nyxStates=["idle","loading"]`);
    expect(html).toContain(`window.__nyxActiveState="idle"`);
    // …and the markup + the theme chain are unaffected.
    expect(html).toContain(`data-nyx-page="catalogue"`);
    expect(html).toContain("@theme {");
    expect(html).toContain(`id="depot-root"`);
  });

  test("the CHROME is emitted by DEFAULT (the detail + proto-viewer keep flip AND outlines)", () => {
    for (const html of [buildFrameSrcdoc(base), buildFrameSrcdoc({ ...base, showChrome: true })]) {
      expect(html).toContain(`id="depot-chip-bar"`);
      // The nav (blue) and state-flip (green) affordances: where one clicks, one sees what is
      // clickable.
      expect(html).toContain("[data-nyx-page]{cursor:pointer;outline:1px dashed #2563eb");
      expect(html).toContain(
        "[data-nyx-state]:not(body){cursor:pointer;outline:1px dashed #16a34a",
      );
    }
  });

  /**
   * A DEAD PAGE LINK IS BADGED, beside the blue rule that badges a live one.
   *
   * `nyx_core::render` stamps `data-nyx-page-missing` (and CLEARS the whole navigation vocabulary
   * with it, so the element cannot navigate) on a node whose `navigate_to` resolves to no page. The
   * crate's own documentation stated that a frame badges it — and NOTHING in this builder styled
   * that attribute, so a dead link rendered as an ordinary element, silently inert. It is marked the
   * way every other in-place marker of this frame is: a dashed outline plus a `::after` cartouche,
   * in the vocabulary the rest of the product already uses for that status ("page gone").
   */
  test("a link whose page is gone is BADGED in place", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).toContain("[data-nyx-page-missing]{position:relative;cursor:not-allowed;");
    expect(html).toContain("outline:1px dashed #dc2626");
    expect(html).toContain('[data-nyx-page-missing]::after{content:"page gone"');
    // …and it is CHROME, like the blue rule beside it: a 40 px vignette gets no badge.
    expect(buildFrameSrcdoc({ ...base, showChrome: false })).not.toContain("data-nyx-page-missing");
  });

  /**
   * The shim mirrors the ACTIVE state on `document.body` (`data-nyx-state`) and its click
   * delegation STOPS at the body — that marker is never clickable. An unscoped `[data-nyx-state]`
   * rule therefore drew a green dashed box around the WHOLE page for an affordance that does not
   * exist (finding `01KYPEG16E95WADT2VMCYBPCK5`).
   */
  test("the state-flip outline never applies to the body (the shim's own state marker)", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).toContain("[data-nyx-state]:not(body){");
    expect(html).not.toMatch(/\[data-nyx-state\]\{/);
  });

  /**
   * Finding `01KYPEG16E95WADT2VMCYBPCK5` — the COMPONENT LAYOUT (the vignette's, then the composant
   * detail's, finding `01KYSA1N8SDT012R6E9XFXFJB0`). The body had no layout at all, so the instance
   * sat in the top-left corner of the host's emptiness. `center: true` centres it on both axes with
   * a padding, and clamps it: the root can shrink (`min-width:0`) and both body and root hide their
   * overflow, so a component wider than the box is cut, never scrolled/stretched.
   */
  test("center: true centres the instance and clips it inside the frame box", () => {
    const html = buildFrameSrcdoc({ ...base, center: true });
    expect(html).toContain(
      "body{height:100%;display:flex;align-items:safe center;justify-content:safe center;padding:.75rem;overflow:hidden}",
    );
    expect(html).toContain(
      "#depot-root{display:flex;align-items:safe center;justify-content:safe center;",
    );
    expect(html).toContain(
      "min-width:0;min-height:0;max-width:100%;max-height:100%;overflow:hidden}",
    );
  });

  test("a page host is NOT centred by default (a page flows from the top-left)", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).not.toContain("justify-content:safe center");
    expect(html).not.toContain("#depot-root{");
    // …and it still has the canvas rule: centring is layout, theming is not.
    expect(html).toContain("background-color:var(--background");
  });

  /**
   * The CANVAS comes from the project's own tokens (finding `01KYPEG16E95WADT2VMCYBPCK5`): the
   * `--background` declaration the `:root` block carries (review #42), the `--color-background`
   * Tailwind namespace as a second chance, and TRANSPARENT as the last resort — the host container's
   * themed surface shows through instead of a raw white plate in a dark app.
   *
   * The TEXT fallback is a light grey for EVERY host, not just a centred one (finding
   * `01KYSA1N8SDT012R6E9XFXFJB0`): now that no preview container is a white plate any more, the
   * transparent no-token canvas always sits on the app's dark surface, where the UA black was
   * illegible. A project that declares a foreground token is untouched — it wins the `var()` chain.
   */
  test("paints the body from the project's background token, never a hard white", () => {
    const html = buildFrameSrcdoc(base);
    expect(html).toContain(
      "background-color:var(--background,var(--color-background,transparent))",
    );
    expect(html).toContain("color:var(--foreground,var(--color-foreground,#e2e8f0))");
    expect(html).not.toContain("var(--color-foreground,inherit)");
    expect(html).not.toMatch(/body\s*\{[^}]*background(-color)?\s*:\s*(#fff|white)/i);
  });

  /**
   * THE DOCUMENT GUARANTEES ITS OWN `hidden` (finding `01KYSCW89NRM1TSZ0TFXC8HD36`). The structural
   * mechanism (`data-nyx-if`, `data-nyx-if-not`, `data-nyx-state-case`) marks an element with the
   * `hidden` attribute — but `[hidden]{display:none}` is a USER-AGENT rule, so any author rule beats
   * it: Preflight's `svg{display:block}`, an `inline-flex` utility, a `<style>` in the template. The
   * frame therefore carries the rule ITSELF, with `!important`, in EVERY configuration (this is not
   * chrome and not a host option — it is the mechanism being true). The `display:none` it computes is
   * pinned in a real browser by `frame-srcdoc.browser.test.ts`.
   */
  test("states the [hidden] guarantee itself, with !important, in every configuration", () => {
    for (const variant of [
      base,
      { ...base, showChrome: false },
      { ...base, showChipBar: false },
      { ...base, center: true },
      { ...base, states: [] as string[] },
    ]) {
      const html = buildFrameSrcdoc(variant);
      expect(html).toContain(
        "[hidden]:where(:not([hidden='until-found'])){display:none!important}",
      );
    }
    // …in the HEAD, after the Tailwind source (a `<style>` of its own, never inside the markup).
    const html = buildFrameSrcdoc(base);
    const ruleAt = html.indexOf("[hidden]:where(");
    expect(ruleAt).toBeGreaterThan(html.indexOf("/*TAILWIND-ENGINE*/"));
    expect(ruleAt).toBeLessThan(html.indexOf("<body>"));
  });

  test("neutralises a </script> breakout inside embedded engine/shim text", () => {
    const html = buildFrameSrcdoc({
      ...base,
      tailwindSource: `var s = "</script><img src=x>";`,
    });
    expect(html).not.toContain("</script><img");
    expect(html).toContain("<\\/script>");
  });

  test("defaults the active state to the first declared state", () => {
    const html = buildFrameSrcdoc({ ...base, activeState: undefined });
    expect(html).toContain(`window.__nyxActiveState="idle"`);
  });

  /**
   * …but a GIVEN active state must survive to the shim — the whole point of the field (finding
   * `01KYPJ1SCHGZ2STSXZ4VCR1K33`). The composant detail's state control was inert because nothing
   * ever passed one, so every rebuild silently reverted to `states[0]`.
   */
  test("carries the GIVEN active state, not the first declared one", () => {
    const html = buildFrameSrcdoc({ ...base, activeState: "loading" });
    expect(html).toContain(`window.__nyxActiveState="loading"`);
    expect(html).not.toContain(`window.__nyxActiveState="idle"`);
    // The declared axes are unchanged — only which one is applied.
    expect(html).toContain(`window.__nyxStates=["idle","loading"]`);
  });

  /**
   * The CHIP-BAR alone (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`). The composant detail pilots the state
   * from its PARAMS panel — the value it persists per node — so a chip-bar there is a SECOND control
   * over the same thing. It drops the chips while KEEPING the rest of the chrome: the dashed
   * `data-nyx-*` affordances (it is a live, clickable preview) and, of course, the state model.
   */
  test("showChipBar: false drops the chips only — outlines and state model stay", () => {
    const html = buildFrameSrcdoc({ ...base, showChipBar: false });
    expect(html).not.toContain(`id="depot-chip-bar"`);
    expect(html).not.toContain(`aria-label="States"`);
    expect(html).not.toContain(".depot-chip{");
    // The interaction affordances are untouched — this host is still interactive.
    expect(html).toContain("[data-nyx-page]{cursor:pointer;outline:1px dashed #2563eb");
    expect(html).toContain("[data-nyx-state]:not(body){cursor:pointer;outline:1px dashed #16a34a");
    // …and so is the state the shim must apply.
    expect(html).toContain(`window.__nyxStates=["idle","loading"]`);
    expect(html).toContain(`window.__nyxActiveState="idle"`);
  });

  test("the chip-bar follows showChrome when showChipBar is not given", () => {
    expect(buildFrameSrcdoc({ ...base, showChrome: false })).not.toContain(`id="depot-chip-bar"`);
    expect(buildFrameSrcdoc({ ...base, showChrome: true })).toContain(`id="depot-chip-bar"`);
    // …and an explicit `true` re-arms the chips even with the rest of the chrome off.
    const chipsOnly = buildFrameSrcdoc({ ...base, showChrome: false, showChipBar: true });
    expect(chipsOnly).toContain(`id="depot-chip-bar"`);
    expect(chipsOnly).not.toContain("[data-nyx-page]{");
  });

  /**
   * THE AXES REACH THE SHIM (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`). The builder is the ONE place
   * that turns the component's declared variance into the frame's globals, and it must do it for
   * both shapes of the model — named axes AND the legacy flat list — without the two ever
   * disagreeing about what the first axis is.
   */
  describe("the variance axes", () => {
    const AXES = [
      {
        name: "variant",
        values: [
          { name: "primary", classes: "bg-primary" },
          { name: "destructive", classes: "bg-destructive" },
        ],
        defaultValue: "primary",
      },
      {
        name: "size",
        values: [{ name: "md" }, { name: "lg", classes: "h-11" }],
        defaultValue: "md",
      },
    ];

    test("emits every axis with its values' concrete classes, and the active combination", () => {
      const html = buildFrameSrcdoc({
        ...base,
        states: [],
        activeState: null,
        axes: AXES,
        axisValues: { variant: "destructive", size: "lg" },
      });
      expect(html).toContain(`"name":"variant"`);
      expect(html).toContain(`"classes":"bg-destructive"`);
      expect(html).toContain(`window.__nyxAxisValues={"variant":"destructive","size":"lg"}`);
      // The LEGACY globals stay coherent: they are the FIRST axis, nothing else.
      expect(html).toContain(`window.__nyxStates=["primary","destructive"]`);
      expect(html).toContain(`window.__nyxActiveState="destructive"`);
    });

    test("an omitted axis falls back to its DEFAULT value — never to nothing", () => {
      const html = buildFrameSrcdoc({ ...base, states: [], axes: AXES, axisValues: {} });
      expect(html).toContain(`window.__nyxAxisValues={"variant":"primary","size":"md"}`);
    });

    /** RETRO-COMPAT: no `axes` at all → ONE anonymous `state` axis built from the flat list. */
    test("the legacy flat list becomes one anonymous class-less axis", () => {
      const html = buildFrameSrcdoc({ ...base, activeState: "loading" });
      expect(html).toContain(
        `window.__nyxAxes=[{"name":"state","label":"state","values":[{"name":"idle","classes":""},{"name":"loading","classes":""}]}]`,
      );
      expect(html).toContain(`window.__nyxAxisValues={"state":"loading"}`);
      expect(html).toContain(`window.__nyxActiveState="loading"`);
    });
  });

  /**
   * THE SCOPED CSS SLOT, MARKED ON THE NODE (findings `01KYVMTWB71R0D0CT1SV88J8DX` /
   * `01KYVVEQZW2Y2VY7ZAPK5RECZS`).
   *
   * The sidecar STAMPS `data-nyx-css="<scope>"` on the component's root element and APPENDS the
   * compiled stylesheet as `<style data-nyx-css-slot="…">`. The frame's whole job here is to make
   * that visible: it styles the attribute the markup already carries and derives NOTHING — no second
   * list of which node is concerned, no re-reading of `css_slots[]` to place a badge.
   */
  describe("the css-slot mark", () => {
    const SCOPE = "c-spinner-8f2a";
    const slotted = {
      ...base,
      markup:
        `<div data-nyx-css="${SCOPE}"><i class="dot"></i></div>` +
        `<style data-nyx-css-slot="${SCOPE}">[data-nyx-css~="${SCOPE}"] .dot{animation:${SCOPE}-pulse 1.4s infinite}</style>`,
    };

    test("marks the node off the attribute the MARKUP carries, not off a list beside it", () => {
      const html = buildFrameSrcdoc(slotted);
      // The rule keys on the stamped attribute…
      expect(html).toContain("[data-nyx-css]{position:relative;outline:1px dashed #f59e0b");
      expect(html).toContain(`[data-nyx-css]::after{content:"⚡ css slot"`);
      // …and the attribute itself reaches the document untouched, on the element the sidecar chose.
      expect(html).toContain(`<div data-nyx-css="${SCOPE}">`);
      // The scope id appears NOWHERE in the frame's own head: the builder never learns which node
      // it is — that is what "read from the markup" means here.
      const head = html.slice(0, html.indexOf("<body>"));
      expect(head).not.toContain(SCOPE);
    });

    test("carries the slot's own stylesheet into the document, verbatim", () => {
      const html = buildFrameSrcdoc(slotted);
      expect(html).toContain(`<style data-nyx-css-slot="${SCOPE}">`);
      expect(html).toContain(`[data-nyx-css~="${SCOPE}"] .dot{animation:${SCOPE}-pulse`);
    });

    /**
     * AND THE CSP IS NOT TOUCHED FOR IT. The slot's stylesheet is an inline `<style>` of the
     * document, which `style-src 'unsafe-inline'` already covers; a slot must never become a reason
     * to open a network source — `external_resource` is one of the nineteen refusals for exactly
     * that reason.
     */
    test("needs no relaxation of the CSP whatsoever", () => {
      const csp =
        buildFrameSrcdoc(slotted).match(/Content-Security-Policy" content="([^"]*)"/)?.[1] ?? "";
      expect(csp).toContain("default-src 'none'");
      expect(csp).toContain("connect-src 'none'");
      expect(csp).toContain("style-src 'unsafe-inline'");
      expect(csp).not.toContain("http");
      expect(csp).not.toContain("*");
    });

    /**
     * It is CHROME — a review affordance, like the dashed nav/flip outlines. The 160 px catalogue
     * vignette and the rail miniatures drop it (a badge would BE the tile); the surfaces one
     * reviews on keep it.
     */
    test("is chrome: dropped where the chrome is, kept where review happens", () => {
      const vignette = buildFrameSrcdoc({ ...slotted, showChrome: false });
      expect(vignette).not.toContain("[data-nyx-css]::after");
      // …while the markup's own attribute and stylesheet still travel: only the badge is dropped,
      // so a vignette still RENDERS the exception, it just does not caption it.
      expect(vignette).toContain(`data-nyx-css="${SCOPE}"`);
      expect(vignette).toContain(`<style data-nyx-css-slot="${SCOPE}">`);
      // The composant detail keeps the affordances without the chips — and the mark with them.
      expect(buildFrameSrcdoc({ ...slotted, showChipBar: false })).toContain(
        "[data-nyx-css]::after",
      );
    });

    /** A page carrying no exception is the page it was: nothing of this reaches its markup. */
    test("a markup carrying no slot is unchanged by any of this", () => {
      expect(buildFrameSrcdoc(base)).not.toContain("data-nyx-css=");
    });

    /**
     * …AND IT IS SEPARABLE FROM THE REST OF THE CHROME (finding `01KYW32N1BVDVTGS0WPMG2PJW2`). A
     * slot belongs to a COMPONENT, so its mark belongs on the surface that judges components. The
     * PROTOTYPE stage drops it while keeping every other affordance: it renders a PAGE, and an amber
     * badge stamped inside that page is a verdict on something nobody is reviewing there.
     */
    test("the mark alone can be dropped, keeping the nav/flip affordances", () => {
      const stage = buildFrameSrcdoc({ ...slotted, showCssSlotMark: false });
      expect(stage).not.toContain("[data-nyx-css]::after");
      expect(stage).not.toContain("⚡ css slot");
      // The rest of the chrome is untouched…
      expect(stage).toContain("[data-nyx-page]{cursor:pointer;outline:1px dashed #2563eb");
      expect(stage).toContain('id="depot-chip-bar"');
      // …and so is the rendering: the attribute and the slot's own stylesheet still travel, so the
      // page renders exactly as it will ship. Only the caption is gone.
      expect(stage).toContain(`data-nyx-css="${SCOPE}"`);
      expect(stage).toContain(`<style data-nyx-css-slot="${SCOPE}">`);
    });
  });

  /**
   * DESIGNATING A NODE (finding `01KYW32N1BVDVTGS0WPMG2PJW2`) — the frame carries the classes the
   * shim puts on the element under the cursor, and the ROOT node's key, which is the one node the
   * markup cannot name for itself.
   */
  /**
   * THE HOLES ARE DRAWN AS HOLES (finding `01KZ3JTWB82DCG0C6JT06T4CAF`, point 2 — family 8 of
   * `01KZ3JTVM23C3MBATJZ3RYVPY6`).
   *
   * The sidecar's LENIENT render substitutes a `<div class="…">[UNRENDERED Button — no template
   * yet]…</div>` for a node it cannot draw, and this rule is the ONLY thing that makes that div read
   * as a hole rather than as a line of body text. The two halves were joined by nothing: renaming
   * the class on EITHER side left the whole suite green while every placeholder lost its dashed box,
   * its margin and its type size — the only signal INSIDE the surface, silently gone.
   *
   * `contract/design-fixtures.json` (`unrendered_nodes`) is the join, and Rust asserts the same line
   * in `render.rs::tests::the_shared_fixture_pins_the_unrendered_reasons_and_both_resolutions`.
   */
  describe("the placeholder of an unrenderable node", () => {
    const fixture = designFixtures.unrendered_nodes;

    test("styles exactly the class the sidecar writes, with the declarations that make it a box", () => {
      expect(UNRENDERED_CLASS, fixture.placeholder_class_note).toBe(fixture.placeholder_class);
      const html = buildFrameSrcdoc(base);
      const rule = new RegExp(`\\.${fixture.placeholder_class}\\{([^}]*)\\}`).exec(html)?.[1];
      expect(rule, "the frame carries a rule for the placeholder class").toBeTruthy();
      // Pinned by NAME, not by value: the hues and the metrics are the frame's business, their
      // PRESENCE is the guarantee — an encadré, the space that detaches it, a type size of its own.
      for (const declaration of fixture.placeholder_style_declarations) {
        expect(rule, `${declaration}: ${fixture.placeholder_style_note}`).toContain(
          `${declaration}:`,
        );
      }
    });

    test("styles it UNCONDITIONALLY — a vignette drops the chrome, never the composition's state", () => {
      // `showChrome: false` is the 160 px catalogue tile. It has no chip-bar and no affordance
      // outlines, and it must still SAY that a node could not be drawn.
      for (const html of [
        buildFrameSrcdoc({ ...base, showChrome: false }),
        buildFrameSrcdoc({ ...base, showChrome: false, showChipBar: false, center: true }),
      ]) {
        expect(html).toContain(`.${fixture.placeholder_class}{`);
      }
    });

    test("names the attributes a surface reads off a placeholder", () => {
      // They are the render's, not this file's: a surface that keyed off another spelling would
      // read nothing at all.
      expect(Object.values(fixture.placeholder_attributes)).toEqual([
        "data-unrendered-node",
        "data-unrendered-component",
        "data-unrendered-reason",
      ]);
    });
  });

  describe("the picking chrome", () => {
    test("ships the pick classes unconditionally — they cost a frame nobody picks in nothing", () => {
      const html = buildFrameSrcdoc(base);
      // The four classes are the shim's too — it ADDS and REMOVES them on elements — so they are
      // named through the shared constants and their seam is pinned in `shim-seam.test.ts`.
      expect(html).toContain(`.${PICK_CLASSES.capture}{position:fixed;inset:0`);
      expect(html).toContain(`.${PICK_CLASSES.hover}{outline:2px solid #7c3aed`);
      expect(html).toContain(`.${PICK_CLASSES.selected}{outline:2px solid #2563eb`);
      expect(html).toContain(`.${PICK_CLASSES.pinned}{outline:2px dashed #f59e0b`);
    });

    test("hands the shim the ROOT node's key, and null when the host names none", () => {
      expect(buildFrameSrcdoc({ ...base, rootNode: "Root" })).toContain(
        'window.__nyxRootNode="Root"',
      );
      expect(buildFrameSrcdoc(base)).toContain("window.__nyxRootNode=null");
    });

    /** Picking loosens NOTHING: same CSP, same two nonce'd scripts, no network. */
    test("changes no line of the CSP", () => {
      const plain = buildFrameSrcdoc(base);
      const picking = buildFrameSrcdoc({ ...base, rootNode: "Root" });
      const csp = (html: string) =>
        /content="([^"]*default-src[^"]*)"/.exec(html)?.[1] ?? "MISSING";
      expect(csp(picking)).toBe(csp(plain));
      expect(csp(picking)).toContain("default-src 'none'");
      expect(csp(picking)).toContain("connect-src 'none'");
    });
  });
});
