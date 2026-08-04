/**
 * The pure builder of the RENDERER iframe's `srcdoc` (v2.7 — Prototype : machinerie, Phase 3, task
 * `01KY4DFQ6NXKE9B7FC0KHRQH8T` — "Renderer iframe + shim + Tailwind local + thème").
 *
 * PDR-0010 freezes the renderer: the sidecar-compiled MARKUP is displayed in an **iframe under a
 * strict CSP**, styled by **Tailwind LOCAL** (embedded, nonce'd, NEVER a CDN — the CSS is generated
 * IN THE BROWSER by the bundled `@tailwindcss/browser` engine), themed from the project's `design`
 * tokens, and made interactive by the **nyx shim** (declarative states chip-bar + `data-nyx-*`
 * nav/flip). The Electron renderer compiles NOTHING — it only assembles this document string.
 *
 * This module is PURE (string → string) so every guarantee is unit-testable without a browser: the
 * CSP forbids the network (`default-src 'none'` / `connect-src 'none'` — no CDN, no fetch), every
 * `<script>` is gated by the per-render `nonce`, the Tailwind engine + the shim are INLINED (no
 * external URL), the theme tokens land in a `@theme` block, and the document states its own
 * `[hidden]` rule so the shim's STRUCTURAL hide cannot be overruled by Preflight or a display utility
 * (finding `01KYSCW89NRM1TSZ0TFXC8HD36`). The real Tailwind + shim sources are injected by the caller
 * ([`CompositionFrame`]) so the builder never hard-depends on them.
 */

import { axisDefaults, primaryAxisStates, type VarianceAxis } from "./axes";

/**
 * The message a sandboxed frame posts to its parent — see the shim.
 *
 * THE SHIM RE-TYPES THIS LITERAL, and it cannot do otherwise: `PROTO_SHIM_SOURCE` is a STRING
 * evaluated inside the frame, so it can import nothing. That is why the seam is TESTED instead
 * (`shim-seam.test.ts`, finding `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 5): the shim's source is read
 * for the names it really posts and really listens for, and confronted with these constants and the
 * lists below. Renaming a name on one side alone stops the channel dead — silently, because a
 * message that does not name the other side is simply ignored.
 */
export const DEPOT_FRAME_SOURCE = "depot-frame";
/**
 * …and the message the HOST posts INTO the frame (the pick mode, the highlighted node, the nodes
 * that already carry a comment). One name each way, so neither side ever acts on a message that
 * merely happens to look like one of ours.
 */
export const DEPOT_HOST_SOURCE = "depot-host";

/**
 * EVERY message type the FRAME posts out, and nothing else. Seven, and each one is a fact the host
 * acts on: `ready` unblocks the host, `resize` sizes the frame, `navigate` switches page, `select`
 * opens the params of a node, `pin` anchors a comment on one, `axes` / `state` echo an in-frame flip
 * back to the host that owns the values.
 */
export const FRAME_MESSAGE_TYPES = [
  "ready",
  "resize",
  "navigate",
  "select",
  "pin",
  "axes",
  "state",
] as const;

/**
 * EVERY message type the HOST posts IN, and nothing else. Three, and they carry no content of their
 * own: a mode, a node key, a set of node keys.
 */
export const HOST_MESSAGE_TYPES = ["pick-mode", "highlight-node", "pinned-nodes"] as const;

/**
 * The `window.__nyx<Name>` globals the srcdoc's bootstrap writes for the shim to read.
 *
 * The shim reads them by CONCATENATION (`window["__nyx" + name]`), which makes them invisible to
 * every refactor and to every grep for the whole identifier — renaming `__nyxAxes` here leaves the
 * shim reading a global nobody writes, i.e. a component with no axes at all, and no suite says so.
 * `shim-seam.test.ts` extracts the names the shim really asks for and confronts them with this list.
 */
export const FRAME_GLOBAL_PREFIX = "__nyx";
export const FRAME_GLOBALS = ["States", "ActiveState", "Axes", "AxisValues", "RootNode"] as const;

/**
 * The class the sidecar's LENIENT render puts on the placeholder of a node it could not draw
 * (`nyx_core::render::UNRENDERED_CLASS`), and the ONLY hook this builder has to draw that hole AS a
 * hole. Pinned to `contract/design-fixtures.json` (`unrendered_nodes.placeholder_class`) in both
 * directions: renaming either side used to leave every suite green while the placeholders lost their
 * box, their margin and their type size and melted into the page as plain text (finding
 * `01KZ3JTWB82DCG0C6JT06T4CAF`).
 */
export const UNRENDERED_CLASS = "nyx-unrendered";

/**
 * The four classes the PICKING chrome draws with — the ones the shim adds and removes on elements,
 * and this builder styles. They are the whole of the pin/select gesture's visible half: a class
 * renamed on one side alone leaves the gesture working and INVISIBLE.
 */
export const PICK_CLASSES = {
  /** The full-cover transparent layer pin mode mounts. */
  capture: "nyx-pick-capture",
  /** The element under the cursor in pin mode. */
  hover: "nyx-pick-hover",
  /** The node currently open in the params panel. */
  selected: "nyx-pick-selected",
  /** A node that already carries an open comment (pin mode only). */
  pinned: "nyx-pick-pinned",
} as const;

/** Inputs to [`buildFrameSrcdoc`]. */
export interface FrameSrcdocInput {
  /** The sidecar-compiled markup (`design_render`) — trusted HTML, injected verbatim. */
  markup: string;
  /** The LOCAL `@tailwindcss/browser` engine source (inlined, nonce'd — never a CDN). */
  tailwindSource: string;
  /** The nyx interaction shim source (inlined, nonce'd). */
  shimSource: string;
  /** The `@theme` body derived from the project's `design` tokens (may be empty). */
  themeCss: string;
  /**
   * The RAW app variables the theme utilities point at — a standalone `:root { … } .dark { … }`
   * stylesheet derived from the same `design` tokens (review #42). WITHOUT it, an ingested layer
   * that follows the shadcn/Tailwind-v4 idiom (`@theme inline { --color-primary: var(--primary) }`)
   * resolves `bg-primary` to an UNDEFINED `var(--primary)` — i.e. renders grey. May be empty.
   */
  rootCss?: string;
  /**
   * Select the DESIGN SYSTEM'S OWN DARK VARIANT (default `false`) — put `class="dark"` on the
   * frame's `<html>` so the `.dark { … }` block [`rootCss`] already carries actually applies
   * (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`).
   *
   * This is NOT repainting over the design system: `.dark` is a block the PROJECT declares (sandbox
   * ships 103 light variables and 48 dark ones — `--background` is `oklch(99.107% …)` light and
   * `oklch(18% …)` dark). The frame emitted BOTH blocks from the start and then selected only the
   * light one, because nothing ever marked the document dark — that is the whole of the "l'aperçu
   * est blanc dans une app sombre" report, reported three times. The `@theme` aliases point at
   * `var(--x)`, so every utility follows the selected block with nothing to duplicate.
   *
   * The caller decides, and it may only say `true` when BOTH are true: the app is in dark
   * appearance, AND the layer really declares dark overrides (`ThemeStyles.hasDark`). On a
   * light-only layer the flag would select nothing — the rendering would be identical — while
   * `color-scheme: dark` would still darken the native widgets on a light canvas. So the frame stays
   * honestly light there, and the HOST captions it.
   */
  dark?: boolean;
  /** The per-render CSP nonce — the ONLY thing that lets the two inline scripts run. */
  nonce: string;
  /**
   * The declared states, exposed to the shim as `window.__nyxStates` (the chip-bar). This is the
   * LEGACY one-axis shorthand: when [`axes`] is given it is derived from the FIRST axis instead.
   */
  states: string[];
  /**
   * The component's VARIANCE AXES (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — the model that replaces
   * the flat state list: named axes (`variant`, `size`, a two-valued `loading`), each value carrying
   * its CONCRETE classes. Handed to the shim as `window.__nyxAxes`, which applies the ACTIVE
   * value's classes of EVERY axis onto its targets — so a template needs ONE element, not one
   * complete rendering per combination.
   *
   * Absent (or empty) → the builder synthesizes ONE anonymous `state` axis out of [`states`], which
   * is exactly the previous behaviour: the legacy blob keeps driving `data-nyx-state-case`.
   */
  axes?: readonly VarianceAxis[];
  /**
   * The ACTIVE COMBINATION — one value per axis (`window.__nyxAxisValues`). Missing axes fall back
   * to their default value. This is the multi-axis generalization of [`activeState`], and the reason
   * a host can pilot `variant` and `size` independently instead of enumerating their product.
   */
  axisValues?: Readonly<Record<string, string>>;
  /**
   * The ACTIVE state — the one the shim applies (`data-nyx-state` on the body,
   * `[data-nyx-state-case]` filtered to it). Defaults to the first declared state.
   *
   * This is the whole point of the field (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`): the builder always
   * honoured it, but no host ever PASSED it, so every re-render fell back to `states[0]` and the
   * composant detail's state control was inert BY CONSTRUCTION. It is the ONE input that makes a
   * host-side state selection reach the rendering.
   */
  activeState?: string | null;
  /**
   * Emit the interaction CHROME (default `true`): the states CHIP-BAR host element AND the dashed
   * outlines that flag what is clickable (`data-nyx-page` nav in blue, `data-nyx-state` flip in
   * green). Both are affordances of a host one INTERACTS with — the proto-viewer and the composant
   * detail, which keep them. A host that only DISPLAYS one instance turns them off: the catalogue
   * vignette (findings `01KYPD8K75EC2AAY1KVAS2XZNX` for the chip-bar — a `badge` with 13 CVA axes
   * filled the whole 160 px tile — and `01KYPEG16E95WADT2VMCYBPCK5` for the outlines, whose dashes
   * framed the tile as debug noise; the shim mirrors the active state ON THE BODY, so the green
   * outline literally boxed the whole preview).
   *
   * It removes the CHROME ONLY: `states` / `activeState` still reach the shim, which still APPLIES
   * the active state (`data-nyx-state` on the body, `[data-nyx-state-case]` filtered to the
   * active one). Dropping `states` instead would be a different — and wrong — thing: with no active
   * state the shim shows EVERY state case at once, i.e. the very enumeration we remove.
   *
   * The two halves are separable: [`showChipBar`] overrides the chip-bar alone.
   */
  showChrome?: boolean;
  /**
   * The states CHIP-BAR alone (defaults to [`showChrome`]) — the half of the chrome that is a state
   * CONTROL rather than an affordance marker.
   *
   * It is separable because a host can want the interaction chrome WITHOUT a second state control
   * (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`): the composant detail already pilots the state from its
   * PARAMS panel — the value it persists per node — so the chip-bar there was a duplicate control,
   * unreconciled with the panel. The detail keeps the dashed `data-nyx-*` affordances
   * (`showChrome`) and drops the chips (`showChipBar: false`); the proto-viewer, which has no param
   * panel, keeps both; the vignette drops both.
   *
   * Like `showChrome`, this removes a CONTROL, never the state itself: `states` / `activeState`
   * still reach the shim, which still applies the active state.
   */
  showChipBar?: boolean;
  /**
   * The CSS-SLOT MARK alone (defaults to [`showChrome`]) — the dashed amber outline and the
   * `⚡ css slot` badge drawn on the element carrying `data-nyx-css`.
   *
   * It is separable for the same reason the chip-bar is (finding `01KYW32N1BVDVTGS0WPMG2PJW2`): a
   * scoped CSS exception is a property of a COMPONENT, and the surface that judges a component is
   * its DETAIL. On the prototype stage it fired on a page one reads as a page — an amber badge
   * stamped over the rendering of a component the reviewer is not reviewing here, saying nothing
   * about the page in front of them. The prototype passes `false`; the composant detail keeps it.
   *
   * Like the two flags above, it removes a MARK, never a behaviour: the slot's own stylesheet is in
   * the markup and still applies, so the page renders exactly as it will ship.
   */
  showCssSlotMark?: boolean;
  /**
   * The ROOT node's `node_key` (`window.__nyxRootNode`) — the region `#depot-root` itself stands for
   * in the shim's element → node index (finding `01KYW32N1BVDVTGS0WPMG2PJW2`).
   *
   * Every OTHER node names itself in the markup, through the comment markers the host asks the
   * sidecar to interleave ({@link markCompositionNodes}); the root cannot, because it has no parent
   * to hold its bracket. Absent → the frame indexes nothing and every designation resolves to
   * nothing, which is exactly what a host that never asked for the markers wants.
   */
  rootNode?: string | null;
  /**
   * CENTRE the instance in the frame box and clip it there (default `false` — a page flows from the
   * top-left, as a page must). This is the COMPONENT layout (finding `01KYPEG16E95WADT2VMCYBPCK5`,
   * extended to the detail by `01KYSA1N8SDT012R6E9XFXFJB0`): the body had no layout at all, so a
   * host showed its component glued to the top-left corner of its emptiness — 160 px on a catalogue
   * tile, a full pane on the composant detail. Centred, the instance also has to STAY inside the
   * box: the root shrinks (`min-width:0`) and overflows hidden, so an oversized component is cut
   * cleanly rather than scrolling the frame or stretching its host. `safe center` keeps a FULL-PAGE
   * component readable — it starts at the top-left instead of being cropped on both sides.
   *
   * BOTH component hosts set it; the proto-viewer, which renders PAGES, does not.
   */
  center?: boolean;
}

/** Escape a value for safe embedding inside an HTML attribute (double-quoted). */
function escapeAttr(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Neutralise a `</script` / `</style` sequence inside embedded script/style TEXT so it cannot break
 * out of its element. `<\/script` is byte-identical to `</script` inside a JS string/regex/comment
 * (the only places the sequence can legally appear in the minified engine), so this never changes
 * behaviour — it only prevents the HTML parser from closing the element early.
 */
function neutralizeClosers(text: string): string {
  return text.replace(/<\/(script|style)/gi, "<\\/$1");
}

/**
 * Assemble the iframe document. The CSP is the security spine: `default-src 'none'` + `connect-src
 * 'none'` mean the frame can reach NOTHING on the network (no CDN, ever — the "aucun réseau"
 * criterion); `script-src 'nonce-…'` runs ONLY our two inlined, nonce'd scripts (the agent ships
 * ZERO application JS); `style-src 'unsafe-inline'` lets the Tailwind engine inject its generated
 * `<style>` (a nonce would block the runtime-generated element, so styles use unsafe-inline while
 * scripts stay strict). `img-src data: blob:` / `font-src data:` keep assets local too.
 *
 * The THEME arrives as TWO blocks (review #42): the `@theme` body that DEFINES the utilities, and a
 * plain `:root` / `.dark` stylesheet carrying the RAW app variables those utilities point at. Both
 * are inline `<style>` elements of the document itself — still zero network, still no CDN. WHICH of
 * the two variants applies is [`FrameSrcdocInput.dark`]: `:root` and `.dark` have the SAME
 * specificity, so the emission order (`:root` first, `.dark` after — see `buildThemeStyles`) is what
 * makes the class on `<html>` decide, exactly as in the app the tokens were ingested from.
 */
export function buildFrameSrcdoc(input: FrameSrcdocInput): string {
  const { markup, tailwindSource, shimSource, themeCss, nonce, states } = input;
  const rootCss = input.rootCss ?? "";
  const n = escapeAttr(nonce);

  // THE AXES the shim will apply (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`). A host that knows the
  // model passes them; a host that only knows the legacy flat list gets ONE anonymous `state` axis
  // synthesized from it — same names, no classes, `data-nyx-state-case` as before.
  const axes: readonly VarianceAxis[] =
    input.axes && input.axes.length > 0
      ? input.axes
      : states.length > 0
        ? [
            {
              name: "state",
              values: states.map((name) => ({ name })),
              defaultValue: states[0]!,
              legacy: true,
            },
          ]
        : [];
  // The ACTIVE COMBINATION: the defaults, overridden by what the host selected, and — for the FIRST
  // axis — by the legacy single `activeState` when the host only knows that one.
  const axisValues: Record<string, string> = { ...axisDefaults(axes), ...input.axisValues };
  const primary = axes[0];
  if (primary && input.activeState != null && input.axisValues?.[primary.name] === undefined) {
    if (primary.values.some((v) => v.name === input.activeState)) {
      axisValues[primary.name] = input.activeState;
    }
  }
  // The LEGACY globals stay coherent with the axes: they ARE the first axis (the chip-bar of a
  // one-axis component, `data-nyx-state-case`, `data-nyx-state`).
  const legacyStates = axes.length > 0 ? primaryAxisStates(axes) : states;
  const active = primary ? (axisValues[primary.name] ?? null) : (input.activeState ?? null);
  // The chip-bar AND the dashed affordance outlines are CHROME: without the chip-bar host element
  // the shim's `buildChipBar` is a no-op (it returns on a missing `#depot-chip-bar`) while the state
  // APPLIER still runs, and without the outline rules the very same document renders the very same
  // markup, minus the debug dashes — see `showChrome`.
  const showChrome = input.showChrome ?? true;
  // The chip-bar follows the chrome unless the host asks for one without the other (the composant
  // detail: affordance outlines yes, second state control no — finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`).
  const showChipBar = input.showChipBar ?? showChrome;
  const center = input.center ?? false;
  const dark = input.dark ?? false;

  // The frame's own CANVAS. The background is the PROJECT'S — the `--background` token the `:root`
  // block carries (review #42) — so a preview sits on the design-system's own surface instead of a
  // hard-coded white plate inside a dark app. `--color-background` covers a layer that only declares
  // the Tailwind namespace; with NO token at all the chain ends on `transparent`, letting the themed
  // HOST container show through — a readable fallback, never a raw white rectangle.
  //
  // The TEXT follows the same chain, and its last resort is a LIGHT grey rather than the UA black
  // (finding `01KYSA1N8SDT012R6E9XFXFJB0`). That fallback only fires when the project declares no
  // foreground token at all, and in that case what shows through the transparent canvas is the app's
  // own container — the nyx window is dark (`index.html` hardcodes `.dark`), so black was illegible.
  // It used to be the VIGNETTE's private fix, because the vignette was the first host to drop its
  // hard `bg-white`; now that NO preview surface is a white plate any more, it is simply the right
  // default for every host — the detail's live preview and the proto-viewer included.
  //
  // …and the VARIANT of those tokens is the one the app is in (finding
  // `01KYSVEZMJP1Z8V7SYJ8YC5H9W`): when the host says `dark`, `<html class="dark">` below selects
  // the `.dark { … }` block the project's own layer declares, so `--background` resolves to the
  // design system's dark surface instead of its light one. `color-scheme` travels with it so the
  // native widgets of a rendered form (a checkbox, a scrollbar) follow the same variant — it is
  // stated HERE, next to the canvas it describes, and only when the variant really exists.
  const canvasCss = `html{height:100%${dark ? ";color-scheme:dark" : ""}}
body{margin:0;box-sizing:border-box;background-color:var(--background,var(--color-background,transparent));color:var(--foreground,var(--color-foreground,#e2e8f0))}`;

  // COMPONENT layout: ONE instance, centred in the frame box and clipped at its edge. `min-*:0` lets
  // the root shrink below its content, and the hidden overflow (body AND root) cuts an oversized
  // component cleanly instead of scrolling the frame or pushing the host out of shape. The
  // alignment is `safe center`: centred WHILE IT FITS, but start-aligned as soon as it does not —
  // a plain `center` clips an oversized component on BOTH sides, so a card would lose its avatar
  // and the start of its title and read as a rendering bug. Cut from the right, it reads as a crop.
  // That safety is what lets a FULL-PAGE component (a `dialog` bigger than the box) stay readable
  // here: it is never cropped at its start edge, it simply begins at the top-left as a page does.
  //
  // BOTH component hosts ask for it (finding `01KYSA1N8SDT012R6E9XFXFJB0`): the catalogue vignette
  // and the composant detail's live preview. Only the proto-viewer does not — it renders PAGES, and
  // a page flows from the top-left by definition.
  const centerCss = center
    ? `body{height:100%;display:flex;align-items:safe center;justify-content:safe center;padding:.75rem;overflow:hidden}
#depot-root{display:flex;align-items:safe center;justify-content:safe center;min-width:0;min-height:0;max-width:100%;max-height:100%;overflow:hidden}`
    : "";

  // Interaction affordances (proto-only, inside the frame): nav = blue, state flip = green. The
  // `:not(body)` is not cosmetic — the shim mirrors the ACTIVE state on the body itself and its
  // click delegation STOPS at the body, so that marker is never clickable: outlining it drew a green
  // dashed box around the entire page for an affordance that does not exist.
  //
  // …AND THE LINK WHOSE PAGE IS GONE, beside the blue one. `nyx_core::render` stamps
  // `data-nyx-page-missing` (the destination id that resolved to nothing) INSTEAD of the whole
  // navigation vocabulary, so the element is structurally un-navigable — and NOTHING styled that
  // attribute in this builder, which is the entire reason a dead link read as an ordinary, silently
  // inert element on the stage while the crate's own doc claimed a frame badged it. It is marked
  // like every other in-place marker of this frame: the hole's dashed box, the slot's amber badge —
  // a dashed outline plus a `::after` cartouche naming the state in the vocabulary the rest of the
  // product already uses (`NavigationStatus::UnknownPage.short()` = "page gone", the wording the
  // links strip under the stage shows for the same node). The badge is deliberately WEAK (no
  // `!important`, `position:relative` on the element itself) for the same reason the slot mark is:
  // markup that legitimately styles its own `::after` wins, and the outline remains.
  const outlineCss = showChrome
    ? `[data-nyx-page]{cursor:pointer;outline:1px dashed #2563eb;outline-offset:2px}
[data-nyx-page-missing]{position:relative;cursor:not-allowed;outline:1px dashed #dc2626;outline-offset:2px}
[data-nyx-page-missing]::after{content:"page gone";position:absolute;top:0;right:0;z-index:2147483000;pointer-events:none;font:700 9px/1.7 ui-monospace,SFMono-Regular,Menlo,monospace;letter-spacing:.04em;color:#fff;background:#dc2626;border-radius:0 0 0 4px;padding:0 4px}
[data-nyx-state]:not(body){cursor:pointer;outline:1px dashed #16a34a;outline-offset:2px}`
    : "";
  // ONE ROW PER AXIS (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`): the bar stacks its axes vertically and
  // each row WRAPS, so `variant × size × loading` reads as three short lines and never as a
  // horizontal scrollbar. A single anonymous axis renders one row with no label — the previous look.
  const chipBarCss = showChipBar
    ? `#depot-chip-bar{position:sticky;top:0;z-index:50;display:flex;flex-direction:column;gap:.25rem;padding:.5rem;background:rgba(0,0,0,.04);backdrop-filter:blur(4px)}
#depot-chip-bar:empty{display:none}
.depot-chip-row{display:flex;flex-wrap:wrap;align-items:center;gap:.375rem}
.depot-chip-axis{font:600 10px/1 ui-sans-serif,system-ui;text-transform:uppercase;letter-spacing:.06em;color:#64748b;min-width:3.5rem}
.depot-chip{font:500 12px/1 ui-sans-serif,system-ui;padding:.3rem .6rem;border-radius:9999px;border:1px solid #cbd5e1;background:#fff;color:#0f172a;cursor:pointer}
.depot-chip[aria-pressed="true"]{background:#0f172a;color:#fff;border-color:#0f172a}`
    : "";
  // THE CSS-SLOT MARK, READ FROM THE MARKUP (findings `01KYVMTWB71R0D0CT1SV88J8DX` /
  // `01KYVVEQZW2Y2VY7ZAPK5RECZS`). A component carrying a scoped CSS slot arrives with
  // `data-nyx-css="<scope>"` STAMPED BY THE SIDECAR on its root element(s), and its compiled
  // stylesheet appended as `<style data-nyx-css-slot="…">`. The frame therefore has nothing to
  // compute: it styles THE ATTRIBUTE THAT IS ALREADY THERE. Re-deriving which node is concerned —
  // from `css_slots[]` and the catalogue, say — would be a second, drifting answer to a question the
  // markup already answers, which is the twin this machinery keeps deleting.
  //
  // It is CHROME: a review affordance, exactly like the dashed nav/flip outlines, shown on the
  // surfaces one reviews (the proto stage, the composant detail) and dropped on the 160 px catalogue
  // vignettes and the rail miniatures, where a badge would BE the tile.
  //
  // A REFUSED slot still shows it: the sidecar stamps the attribute whether or not the stored CSS
  // still compiles, so the exception stays visible on the node even when nothing was applied — that
  // is the point of naming a refusal instead of dropping it.
  //
  // The badge is `::after` on the root, anchored by a `position:relative` on that same root. Both
  // are deliberately WEAK: no `!important`, and the slot's own stylesheet is appended AFTER this one
  // at equal specificity, so a slot that legitimately styles its root's `::after` (a `pseudo_element`
  // slot) WINS over the badge instead of fighting it. The mark then remains as the outline.
  //
  // It is separable from the rest of the chrome (finding `01KYW32N1BVDVTGS0WPMG2PJW2`): a slot
  // belongs to a COMPONENT, so its mark belongs on the surface that judges components. See
  // [`showCssSlotMark`].
  const showCssSlotMark = input.showCssSlotMark ?? showChrome;
  const slotMarkCss = showCssSlotMark
    ? `[data-nyx-css]{position:relative;outline:1px dashed #f59e0b;outline-offset:2px}
[data-nyx-css]::after{content:"⚡ css slot";position:absolute;top:0;right:0;z-index:2147483000;pointer-events:none;font:700 9px/1.7 ui-monospace,SFMono-Regular,Menlo,monospace;letter-spacing:.04em;color:#0a0a0a;background:#f59e0b;border-radius:0 0 0 4px;padding:0 4px}`
    : "";

  // DESIGNATING A NODE (finding `01KYW32N1BVDVTGS0WPMG2PJW2`) — the classes the shim puts on the
  // element under the cursor while the host is picking, plus the full-cover capture layer pin mode
  // mounts. Emitted UNCONDITIONALLY, and that costs a host nothing: the shim only ever adds these
  // classes after the host asks for a mode, so a frame nobody picks in renders identically.
  //
  // The hues are hard-coded, like every other affordance here: the frame is a sandboxed opaque
  // origin with no access to the app's token layer. Violet = the element about to be pinned (depot's
  // own pin hue), blue = the node currently open in the params panel, amber-dashed = a node that
  // already carries an open comment. `outline` never changes layout, so highlighting an element can
  // never move the page under the reviewer.
  const pickCss = `.${PICK_CLASSES.capture}{position:fixed;inset:0;z-index:2147483646;cursor:crosshair;background:transparent}
.${PICK_CLASSES.hover}{outline:2px solid #7c3aed!important;outline-offset:1px;background-image:repeating-linear-gradient(45deg,transparent,transparent 4px,rgba(124,58,237,.12) 4px,rgba(124,58,237,.12) 8px)!important}
.${PICK_CLASSES.selected}{outline:2px solid #2563eb!important;outline-offset:1px}
.${PICK_CLASSES.pinned}{outline:2px dashed #f59e0b!important;outline-offset:1px}`;
  const chromeCss = [outlineCss, slotMarkCss, chipBarCss, pickCss].filter(Boolean).join("\n");

  // THE HOLES, LEGIBLE IN PLACE. The sidecar's LENIENT render replaces a node it cannot draw with a
  // `<div class="nyx-unrendered">[UNRENDERED Button — no template yet]…</div>` at the exact spot that
  // node occupies (finding `01KYT78699XG6Q6P3VZC64W8H8`), the node's own children kept INSIDE it —
  // so a hole never swallows the subtree below it, and the nesting is visible as nesting.
  //
  // Styled UNCONDITIONALLY, unlike the chrome above: this is not an interaction affordance a vignette
  // may drop, it is the composition's own state. And drawn in `currentColor` with NO background of
  // its own — the frame's canvas belongs to the design system (finding `01KYSA1N8SDT012R6E9XFXFJB0`)
  // — so the box stays legible whatever (and whether) the project declares a token layer.
  const unrenderedCss = `.${UNRENDERED_CLASS}{display:block;margin:4px 0;padding:8px 10px;border:1px dashed currentColor;border-radius:8px;opacity:.7;font:400 11px/1.45 ui-sans-serif,system-ui,sans-serif}`;

  // THE STRUCTURAL HIDE, MADE UNCONDITIONAL (finding `01KYSCW89NRM1TSZ0TFXC8HD36`). The shim marks a
  // structurally absent element with the `hidden` ATTRIBUTE — `data-nyx-if`, `data-nyx-if-not`,
  // `data-nyx-state-case`. Whether that attribute HIDES anything is a CSS question, and leaving the
  // answer to whatever stylesheet happens to be in the frame is how the mechanism can go silently
  // inert: the browser's own `[hidden]{display:none}` is a USER-AGENT rule, so ANY author rule
  // outranks it — Tailwind's Preflight `img,svg,video,canvas,audio,iframe,embed,object{display:block}`,
  // a `flex`/`inline-flex`/`grid`/`block` utility, or a `<style>` the TEMPLATE ITSELF carries. The
  // rendering then keeps a spinner that must not exist, or shows all eight cases of an 8-case
  // template at once, with nothing to flag it as broken.
  //
  // So the frame states it ITSELF, as an AUTHOR rule carrying `!important`. `!important` — not
  // ordering — is what makes it hold, because being LAST is not something this builder can own: the
  // Tailwind engine `document.head.append`s its generated stylesheet from inside the frame, and above
  // all the MARKUP lives in the BODY, so a `<style>` a template ships comes after every head
  // stylesheet in document order. The declaration is legitimate in the strict sense — `hidden` means
  // HIDDEN, and no display rule should resurrect an element the model says does not exist — and it is
  // scoped to that one property on that one selector.
  //
  // It deliberately does NOT lean on Tailwind's own Preflight, which currently happens to ship an
  // equivalent `[hidden]` rule: that is a version-dependent detail of a dependency (and it is gone as
  // soon as a frame imports the utilities alone), while this guarantee belongs to the frame.
  //
  // The `:where(:not([hidden='until-found']))` guard mirrors Preflight so the platform's
  // `hidden="until-found"` (a searchable collapsed region) keeps its own semantics; `:where()` adds
  // no specificity, so the rule stays a plain attribute selector.
  const hiddenCss = `[hidden]:where(:not([hidden='until-found'])){display:none!important}`;

  const csp = [
    "default-src 'none'",
    `script-src 'nonce-${nonce}'`,
    "style-src 'unsafe-inline'",
    "img-src data: blob:",
    "font-src data:",
    "connect-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
  ].join("; ");

  // The AXES + the active combination, handed to the shim as globals BEFORE it runs — plus the two
  // LEGACY globals, kept in sync with the first axis so nothing that reads them regresses.
  // Written through {@link FRAME_GLOBALS} rather than as literals: the shim reads them by
  // CONCATENATION, so a name spelled only here is invisible to every refactor (finding
  // `01KZ3JTVM23C3MBATJZ3RYVPY6`, family 6).
  //
  // The result is passed through {@link neutralizeClosers} like every other script text below:
  // `JSON.stringify` does NOT escape `/`, so a state, an axis or a node key an agent named
  // `a</script>b` would close the element early — truncating the shim, i.e. no chip-bar, no
  // navigation and no node designation, silently and with nothing on screen to say so.
  const global = (name: (typeof FRAME_GLOBALS)[number], value: unknown): string =>
    `window.${FRAME_GLOBAL_PREFIX}${name}=${JSON.stringify(value)};`;
  const bootstrap =
    global("States", legacyStates) +
    global("ActiveState", active) +
    global(
      "Axes",
      axes.map((a) => ({
        name: a.name,
        label: a.label ?? a.name,
        values: a.values.map((v) => ({ name: v.name, classes: v.classes ?? "" })),
      })),
    ) +
    global("AxisValues", axisValues) +
    // The ROOT node's key — the one node of the tree that cannot name itself in the markup.
    global("RootNode", input.rootNode ?? null);

  return `<!doctype html>
<html lang="en"${dark ? ` class="dark"` : ""}>
<head>
<meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="${escapeAttr(csp)}" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<style type="text/tailwindcss" nonce="${n}">@import "tailwindcss";
@theme {
${neutralizeClosers(themeCss)}
}</style>
<style>
/* The project's RAW theme variables (review #42) — the single declarations the @theme entries
   above REFERENCE with var(--x). Without them bg-primary resolves to nothing (the grey rendu). */
${neutralizeClosers(rootCss)}
</style>
<style>
/* The frame canvas (themed from the project's own tokens), the vignette layout, the box a node the
   sidecar could not render is NAMED in, then the interaction CHROME — the vignette layout and the
   chrome only for the hosts that asked for them. */
${canvasCss}
${centerCss}
${unrenderedCss}
${chromeCss}
</style>
<script nonce="${n}">${neutralizeClosers(tailwindSource)}</script>
<style>
/* THE LAST WORD ON display: an element the model says does not exist is HIDDEN, whatever Preflight,
   a display utility or the template's own CSS claims (finding 01KYSCW89NRM1TSZ0TFXC8HD36). Injected
   after the Tailwind source; its authority comes from !important, see above. */
${hiddenCss}
</style>
</head>
<body>
${showChipBar ? `<div id="depot-chip-bar" role="toolbar" aria-label="States"></div>` : ""}
<div id="depot-root">${markup}</div>
<script nonce="${n}">${neutralizeClosers(bootstrap)}${neutralizeClosers(shimSource)}</script>
</body>
</html>`;
}
