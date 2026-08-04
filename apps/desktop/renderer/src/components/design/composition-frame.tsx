import { useEffect, useMemo, useRef, useState } from "react";

import { cn } from "@/lib/utils";

import { type VarianceAxis } from "./axes";
import { buildFrameSrcdoc, DEPOT_FRAME_SOURCE, DEPOT_HOST_SOURCE } from "./frame-srcdoc";
import { PROTO_SHIM_SOURCE } from "./proto-shim";
import { TAILWIND_BROWSER_SOURCE } from "./tailwind-runtime";
import { buildThemeStyles } from "./theme-css";

/**
 * `CompositionFrame` — the RENDERER of the proto (v2.7 — Prototype : machinerie, Phase 3, task
 * `01KY4DFQ6NXKE9B7FC0KHRQH8T`). It displays sidecar-compiled MARKUP inside a SANDBOXED iframe
 * under a strict CSP, styled by the LOCAL Tailwind engine (inlined, nonce'd — never a CDN), themed
 * from the project's `design` tokens, and made interactive by the nyx shim (declarative states
 * chip-bar + `data-nyx-*` nav/flip). The Electron renderer compiles NOTHING — it assembles the
 * `srcdoc` string ([`buildFrameSrcdoc`]) and hands it to the browser.
 *
 * The iframe is `sandbox="allow-scripts"` (NO `allow-same-origin` → an opaque origin that can
 * reach nothing of the host), so the shim talks back only via `postMessage`: a `navigate` message
 * (a `data-nyx-page` click) reaches [`onNavigate`], and a `resize` message auto-sizes the frame
 * when `autoHeight` is set (the live preview of the composant detail).
 */
export interface CompositionFrameProps {
  /** The sidecar-compiled markup (`design_render`). */
  markup: string;
  /**
   * The project's `design.theme_tokens_json` (feeds `@theme`); `null`/absent → default theme.
   *
   * Its DARK half is used too (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`): when nyx is in dark
   * appearance and the layer declares dark overrides, the frame selects the design system's OWN
   * dark variant instead of rendering its light one inside a dark app. A light-only layer keeps
   * rendering light — see [`themeHasDark`], which is what a host reads to SAY so.
   */
  themeTokensJson?: string | null;
  /** The declared states surfaced as the flippable chip-bar (`window.__nyxStates`). */
  states?: readonly string[];
  /**
   * The ACTIVE state — a CONTROLLED input: the frame renders THIS state (the shim applies it on the
   * body and filters `[data-nyx-state-case]` to it). Defaults to the first declared state.
   *
   * Passing it is what makes a host-side state control actually change the rendering (finding
   * `01KYPJ1SCHGZ2STSXZ4VCR1K33`): the builder read the field from the start, but no host ever fed
   * it, so every re-render silently fell back to `states[0]`.
   */
  activeState?: string | null;
  /**
   * The state changed INSIDE the frame — a `data-nyx-state` flip (or a chip, where the chip-bar is
   * shown). The frame is a VIEW of `activeState`, so a host that owns that value MUST feed the
   * change back (the detail writes it into its `state` param): otherwise the frame's own state and
   * the host's diverge until the next srcdoc rebuild silently discards the flip.
   */
  onStateChange?: (state: string | null) => void;
  /**
   * The component's VARIANCE AXES (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — named axes whose values
   * carry their CONCRETE classes. The shim applies the active value of EVERY axis onto its targets,
   * so a host pilots `variant` and `size` INDEPENDENTLY instead of enumerating their product.
   * Absent → the legacy [`states`] list is read as one anonymous `state` axis.
   */
  axes?: readonly VarianceAxis[];
  /** The ACTIVE COMBINATION (one value per axis) — a CONTROLLED input, exactly like [`activeState`]. */
  axisValues?: Readonly<Record<string, string>>;
  /**
   * The combination changed INSIDE the frame (a chip, a `data-nyx-set` click). Same contract as
   * [`onStateChange`], for the whole combination: the frame is a VIEW, the host owns the values.
   */
  onAxisChange?: (values: Record<string, string>) => void;
  /**
   * Show the interaction CHROME (default `true`): the states chip-bar AND the dashed outlines that
   * flag the clickable `data-nyx-page` / `data-nyx-state` elements. A full-page host — the
   * composant detail and the proto-viewer — keeps it: interacting with the proto there is the point.
   * The catalogue VIGNETTE turns it OFF (findings `01KYPD8K75EC2AAY1KVAS2XZNX` for the chip-bar,
   * `01KYPEG16E95WADT2VMCYBPCK5` for the outlines): a 160 px tile DISPLAYS one instance, it is not
   * an interaction surface, so the affordances are only noise there. Turning it off hides the CHROME
   * only — the declared `states` still reach the shim, so the frame still renders the ACTIVE state
   * (and only it).
   */
  showChrome?: boolean;
  /**
   * The states CHIP-BAR alone (defaults to `showChrome`). A host that pilots the state ITSELF turns
   * it off while keeping the rest of the chrome: the composant detail selects the state from its
   * PARAMS panel — the value it persists per node — so a chip-bar there is a SECOND, unreconciled
   * control over the same thing (finding `01KYPJ1SCHGZ2STSXZ4VCR1K33`). The proto-viewer, which has
   * no param panel, keeps the chips: there the frame IS the only place to flip a state.
   */
  showChipBar?: boolean;
  /**
   * The CSS-SLOT MARK alone (defaults to `showChrome`) — the amber dashed outline + `⚡ css slot`
   * badge on the element carrying the exception. The PROTOTYPE stage turns it off (finding
   * `01KYW32N1BVDVTGS0WPMG2PJW2`): a slot belongs to a component, and the surface that judges a
   * component is its detail — on a page it only stamps a badge over a rendering nobody is reviewing
   * there. The composant detail keeps it.
   */
  showCssSlotMark?: boolean;
  /**
   * DESIGNATING A NODE — how a click in the frame is read (default `off`, finding
   * `01KYW32N1BVDVTGS0WPMG2PJW2`):
   *
   *  - `select` — a click that lands on nothing the page declares (`data-nyx-page`, `data-nyx-set`,
   *    `data-nyx-state` keep priority) reports the `node_key` it landed in. THE RENDER IS THE
   *    COMPOSITION: that is how the params panel opens on a component one points at.
   *  - `pin` — a full-cover capture layer swallows every click and reports the node under the cursor
   *    with its FRAME-RELATIVE coordinates, so the host can anchor a comment popup there. The
   *    prototype's own nav/flip never fires while pinning.
   *
   * It needs markup the SIDECAR rendered: the render is what stamps each node's `node_key` on the
   * root element(s) it draws (`data-nyx-node`). Over markup from anywhere else every designation
   * resolves to nothing and only the declared affordances remain.
   */
  pickMode?: "off" | "select" | "pin";
  /** The node currently OPEN in the host's panel — ringed in the frame so the selection is legible. */
  selectedNodeKey?: string | null;
  /** The nodes that already carry an OPEN comment — ringed while `pin` mode is on, as depot does. */
  pinnedNodeKeys?: readonly string[];
  /** A node DESIGNATED by a plain click in `select` mode. */
  onNodeSelect?: (nodeKey: string) => void;
  /** A node PINNED in `pin` mode, with the frame-relative point the click landed on. */
  onNodePin?: (pin: { nodeKey: string | null; x: number; y: number }) => void;
  /**
   * CENTRE the instance inside the frame box and clip it there (default `false`: a page flows from
   * the top-left). This is the layout of every COMPONENT host (finding `01KYPEG16E95WADT2VMCYBPCK5`
   * for the catalogue tile, `01KYSA1N8SDT012R6E9XFXFJB0` for the composant detail, where the fix had
   * been left out of scope): a component preview shows its instance in the MIDDLE of the box, never
   * glued to a corner, and an oversized one is cut at the edge instead of stretching its host.
   *
   * It needs a DEFINITE box to centre in, so a host that sets it sizes the frame itself (`h-full`)
   * rather than combining it with [`autoHeight`], which makes the box exactly as tall as the content
   * — i.e. re-glues the instance to the top. The proto-viewer renders PAGES and never sets it.
   */
  center?: boolean;
  /** Called when a `data-nyx-page` element is clicked inside the frame (the blue nav). */
  onNavigate?: (page: string) => void;
  /** Auto-size the iframe to its content height (for a scroll-free live preview). */
  autoHeight?: boolean;
  className?: string;
  title?: string;
}

/** A CSP nonce: a fresh, opaque token per srcdoc so only our two inlined scripts run. */
function makeNonce(): string {
  return globalThis.crypto?.randomUUID?.().replace(/-/g, "") ?? Math.random().toString(36).slice(2);
}

/** Is the HOST app itself in dark appearance right now? (`.dark` on `<html>` — nyx's own switch.) */
function readHostDark(): boolean {
  return typeof document !== "undefined" && document.documentElement.classList.contains("dark");
}

/**
 * The app's CURRENT appearance, live (finding `01KYSVEZMJP1Z8V7SYJ8YC5H9W`).
 *
 * The preview has to follow the app it sits in, so this is read from the ONE place that holds the
 * answer — the `dark` class on `<html>`, which is what every nyx surface already themes from — and
 * OBSERVED rather than sampled: a frame whose srcdoc was built while the app was dark must not keep
 * rendering dark after a flip. Same pattern (and same authority) as the review diff's own theme
 * read. Today nyx hardcodes `.dark` in `index.html`, so this is constant — which is precisely why it
 * must not be hardcoded a SECOND time here: the day the app gains the switch, the preview follows
 * with nothing to change.
 */
function useHostDark(): boolean {
  const [dark, setDark] = useState(readHostDark);
  useEffect(() => {
    if (typeof document === "undefined") return;
    const el = document.documentElement;
    const observer = new MutationObserver(() => setDark(readHostDark()));
    observer.observe(el, { attributes: true, attributeFilter: ["class"] });
    setDark(readHostDark());
    return () => observer.disconnect();
  }, []);
  return dark;
}

export function CompositionFrame({
  markup,
  themeTokensJson,
  states,
  activeState,
  onStateChange,
  axes,
  axisValues,
  onAxisChange,
  showChrome = true,
  showChipBar,
  showCssSlotMark,
  pickMode = "off",
  selectedNodeKey = null,
  pinnedNodeKeys,
  onNodeSelect,
  onNodePin,
  center = false,
  onNavigate,
  autoHeight,
  className,
  title = "Prototype preview",
}: CompositionFrameProps) {
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [autoHeightPx, setAutoHeightPx] = useState<number | null>(null);
  const hostDark = useHostDark();
  /**
   * The frame RELOADS on every srcdoc change, and a reloaded frame knows nothing of the mode it was
   * in. This counts the `ready` messages so the effects below re-post the whole host state each time
   * the document comes back — the alternative (posting once, on a state change) leaves the pick mode
   * silently off after any re-render, which reads as a dead button.
   */
  const [readyTick, setReadyTick] = useState(0);

  const stateList = useMemo(() => (states ? [...states] : []), [states]);

  // Assemble the srcdoc. A fresh nonce per (re)assembly — memoized on the inputs so identical
  // markup/theme/states does not needlessly reload the frame.
  const srcdoc = useMemo(() => {
    // BOTH halves of the theme (review #42): the `@theme` body that DEFINES the utilities, and the
    // `:root` stylesheet of raw app variables they point at. Injecting only the first left
    // `bg-primary` resolving to an undefined `var(--primary)` — the grey rendu.
    const theme = buildThemeStyles(themeTokensJson);
    return buildFrameSrcdoc({
      markup,
      tailwindSource: TAILWIND_BROWSER_SOURCE,
      shimSource: PROTO_SHIM_SOURCE,
      themeCss: theme.theme,
      rootCss: theme.root,
      // THE VARIANT THE APP IS IN, when the design system defines it (finding
      // `01KYSVEZMJP1Z8V7SYJ8YC5H9W`). The conjunction is the whole decision: a dark app AND a layer
      // that really declares `.dark` overrides. Either half missing and the frame stays light —
      // which for a light-only layer is not a fallback but the ONLY honest rendering, since there is
      // no dark variant to select; the hosts caption that case rather than let it pass unsaid.
      dark: hostDark && theme.hasDark,
      nonce: makeNonce(),
      states: stateList,
      activeState: activeState ?? null,
      axes,
      axisValues,
      showChrome,
      showChipBar,
      showCssSlotMark,
      center,
    });
  }, [
    markup,
    themeTokensJson,
    hostDark,
    stateList,
    activeState,
    axes,
    axisValues,
    showChrome,
    showChipBar,
    showCssSlotMark,
    center,
  ]);

  // The frame talks back ONLY through postMessage; accept messages from THIS frame's window only.
  useEffect(() => {
    function onMessage(ev: MessageEvent) {
      if (ev.source !== iframeRef.current?.contentWindow) return;
      const data = ev.data as {
        source?: string;
        type?: string;
        page?: string;
        height?: number;
        state?: string | null;
        values?: Record<string, string>;
        nodeKey?: string | null;
        x?: number;
        y?: number;
      };
      if (!data || data.source !== DEPOT_FRAME_SOURCE) return;
      if (data.type === "navigate" && typeof data.page === "string") {
        onNavigate?.(data.page);
      } else if (data.type === "ready") {
        setReadyTick((t) => t + 1);
      } else if (data.type === "select" && typeof data.nodeKey === "string") {
        onNodeSelect?.(data.nodeKey);
      } else if (data.type === "pin" && typeof data.x === "number" && typeof data.y === "number") {
        // A pin with NO node is still a pin: the click landed on markup no node claims (the chrome,
        // a stylesheet the render appended). The host says so rather than swallowing the gesture.
        onNodePin?.({ nodeKey: data.nodeKey ?? null, x: data.x, y: data.y });
      } else if (data.type === "resize" && autoHeight && typeof data.height === "number") {
        setAutoHeightPx(Math.max(0, Math.ceil(data.height)));
      } else if (data.type === "axes" && data.values != null && typeof data.values === "object") {
        // The WHOLE combination, echoed on every apply — the multi-axis twin of the `state`
        // message below. Same contract: the host is expected to be idempotent.
        onAxisChange?.(data.values);
      } else if (data.type === "state" && (typeof data.state === "string" || data.state === null)) {
        // The shim posts this on EVERY apply — at boot (echoing `activeState`) and on an in-frame
        // flip. The host is expected to be idempotent (the detail's `setParam` no-ops on an equal
        // value), so the echo costs nothing while a real flip stays in sync.
        onStateChange?.(data.state);
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [onNavigate, onStateChange, onAxisChange, onNodeSelect, onNodePin, autoHeight]);

  /**
   * …and the HOST → FRAME half. `postMessage` on the frame's own `contentWindow` is the only channel
   * that exists here: the sandbox has no `allow-same-origin`, so the host cannot touch the frame's
   * DOM, and the frame cannot touch the host's. Nothing about picking or pinning loosens the
   * document's CSP — the shim was already inlined under the per-render nonce, and this adds no
   * script, no style element and no network.
   */
  // Serialized so the effect depends on the CONTENT of the list, not on the identity of an array the
  // host rebuilds on every render — which would re-post on every keystroke it happens to hold.
  const pinnedSerialized = JSON.stringify(pinnedNodeKeys ?? []);
  useEffect(() => {
    const win = iframeRef.current?.contentWindow;
    if (win == null) return;
    win.postMessage({ source: DEPOT_HOST_SOURCE, type: "pick-mode", mode: pickMode }, "*");
    win.postMessage(
      { source: DEPOT_HOST_SOURCE, type: "highlight-node", nodeKey: selectedNodeKey },
      "*",
    );
    win.postMessage(
      {
        source: DEPOT_HOST_SOURCE,
        type: "pinned-nodes",
        nodeKeys: JSON.parse(pinnedSerialized) as readonly string[],
      },
      "*",
    );
  }, [pickMode, selectedNodeKey, pinnedSerialized, readyTick]);

  return (
    <iframe
      ref={iframeRef}
      title={title}
      // Scripts run (Tailwind engine + shim) but the frame is a cross-origin opaque sandbox: it
      // cannot touch the host, cookies, or storage. The network is closed by the document CSP.
      sandbox="allow-scripts"
      srcDoc={srcdoc}
      // NO background of its own: the document paints its canvas from the project's `--background`
      // token, and falls back to TRANSPARENT so the host container's themed surface shows through
      // (finding `01KYPEG16E95WADT2VMCYBPCK5` — a hard `bg-white` here put a raw white plate under
      // every preview, whatever the design system said).
      className={cn("w-full border-0 bg-transparent", className)}
      style={autoHeight && autoHeightPx != null ? { height: `${autoHeightPx}px` } : undefined}
    />
  );
}
