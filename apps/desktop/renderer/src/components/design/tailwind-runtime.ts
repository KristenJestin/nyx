/**
 * The LOCAL Tailwind engine, inlined as a string (v2.7 — Prototype : machinerie, Phase 3,
 * PDR-0010: "Tailwind LOCAL (embarqué, nonce CSP, jamais CDN) ; CSS généré browser").
 *
 * `@tailwindcss/browser` is the standalone Tailwind v4 engine that scans the DOM + the
 * `<style type="text/tailwindcss">` block and GENERATES the CSS in the browser. We import its
 * bundled build as RAW TEXT (Vite `?raw`) so it can be inlined into the iframe `srcdoc` behind a
 * CSP nonce — the frame therefore fetches NOTHING (no CDN, no network), which is exactly what the
 * strict CSP in `frame-srcdoc.ts` enforces. Kept behind this one module so the pure srcdoc builder
 * stays free of the raw import (it receives the source as a parameter).
 */
import tailwindBrowserSource from "@tailwindcss/browser?raw";

/** The bundled `@tailwindcss/browser` engine source, inlined (never loaded over the network). */
export const TAILWIND_BROWSER_SOURCE: string = tailwindBrowserSource;
