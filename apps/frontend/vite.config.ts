import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import Icons from "unplugin-icons/vite";
import { defineConfig } from "vite";
import tsconfigPaths from "vite-tsconfig-paths";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

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

  // Statically replace the gate flag at build time so it tree-shakes cleanly:
  // in a production build this becomes `"" !== "1"` → always true → the seam's
  // entire `useEffect` body is dead code Vite/esbuild drops from the bundle.
  define: {
    "import.meta.env.VITE_NYX_E2E": JSON.stringify(nyxE2e),
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
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
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
