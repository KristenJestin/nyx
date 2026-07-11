import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import Icons from "unplugin-icons/vite";
import { defineConfig } from "vite";
import tsconfigPaths from "vite-tsconfig-paths";

// Optional explicit dev-server host (for an over-the-network HMR setup). Read from the
// environment; unset → bind localhost only (the normal local Electron dev run).
// @ts-expect-error process is a nodejs global
const host = process.env.NYX_DEV_HOST;

// The e2e build flag. Vite does NOT expose shell `VITE_*` env vars to
// `import.meta.env` (only vars from `.env` files), so we read it from the build
// environment here and inject it via `define`. This is what gates the
// `window.__nyx` e2e seam (src/components/sidebar/terminal-manager.tsx): the e2e
// build runs with `VITE_NYX_E2E=1` (see the `build:e2e` npm script), a real
// production `bun run build` leaves it unset → the seam is compiled out.
// @ts-expect-error process is a nodejs global
const nyxE2e = process.env.VITE_NYX_E2E === "1" ? "1" : "";

// https://vite.dev/config/
export default defineConfig(async () => ({
  // Relative asset base so the Electron shell can load the build over `file://`
  // (`loadFile`): with the Vite default `base: "/"`, `index.html` references
  // `/assets/...`, which under `file://` resolves to the filesystem root and the
  // bundle never loads → black window. `"./"` makes every asset URL relative.
  base: "./",

  plugins: [
    react(),
    tailwindcss(),
    tsconfigPaths(),
    // Provider brand logos (finding #55): build-time, tree-shaken, OFFLINE (the SVG
    // bodies are bundled from the local `@iconify-json/simple-icons` set, no network).
    // `~icons/<collection>/<slug>` imports become real React components (jsx compiler).
    // We keep lucide for the rest of the UI — this is only the agent-provider logos.
    Icons({ compiler: "jsx", jsx: "react" }),
  ],

  // The @pierre/diffs syntax-highlight worker (the `?worker` import in
  // `components/review/diffs-worker.ts`) code-splits its Shiki grammars + the inlined Oniguruma
  // WASM into on-demand chunks, so it MUST be emitted as an ES-module worker: Vite's default
  // `iife` worker format rejects code-splitting ("UMD and IIFE output formats are not supported
  // for code-splitting builds"). Every chunk (grammars, wasm) is bundled LOCALLY — the review
  // viewer stays fully offline at runtime (no CDN fetch), which the local app requires (PDR-0028).
  worker: { format: "es" },

  // Statically replace the gate flag at build time so it tree-shakes cleanly:
  // in a production build this becomes `"" !== "1"` → always true → the seam's
  // entire `useEffect` body is dead code Vite/esbuild drops from the bundle.
  define: {
    "import.meta.env.VITE_NYX_E2E": JSON.stringify(nyxE2e),
  },

  // Dev-server options for the Electron dev run (`scripts/dev.cjs` waits on this fixed
  // port, then points the Electron main at it).
  //
  // 1. don't clear the screen so build/runtime errors stay visible.
  clearScreen: false,
  // 2. a fixed port the Electron dev launcher expects; fail if it is taken.
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
  },
}));
