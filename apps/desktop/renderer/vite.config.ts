import { createRequire } from "node:module";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import Icons from "unplugin-icons/vite";
import { defineConfig, type Plugin } from "vite";
import tsconfigPaths from "vite-tsconfig-paths";

// The port derivation, the identity route and the identity payload come from the SAME module
// `scripts/dev.cjs` reads, so the launcher and the server can never disagree about any of them. It
// is plain CJS (dev.cjs is a `require`-only node script), hence `createRequire` rather than import.
const requireCjs = createRequire(import.meta.url);
const { DEV_IDENTITY_PATH, devHmrPort, devServerPort, identityPayload } = requireCjs(
  "../scripts/dev-server-identity.cjs",
);

// The renderer root this dev server serves — THIS file's own directory, which is what the port is
// derived from. `scripts/dev.cjs` derives it from `path.resolve(<electron app>, "renderer")`: the
// same directory, spelled differently (backslashes, and on Windows a different drive-letter case).
// `dev-server-identity.cjs` normalizes before hashing, which is what makes the two spellings land
// on ONE port instead of two. Taken from `import.meta.url` rather than `process.cwd()` so the port
// does not depend on where the command was typed.
const rendererRoot = dirname(fileURLToPath(import.meta.url));

/**
 * Publish WHICH renderer root this dev server serves, so `scripts/dev.cjs` can tell our server
 * from another worktree's before pointing Electron at it. Deriving the port per worktree made that
 * collision rare, not impossible — a stale server of ours, a proxy, an unrelated app — and a
 * response on the port still says only that SOMETHING is listening. Three launches silently ran one
 * worktree's backend against another's UI before this route existed; that is why it stays.
 *
 * Dev only (`configureServer`): a production build carries no trace of this.
 */
const devServerIdentity: Plugin = {
  name: "nyx-dev-server-identity",
  configureServer(server) {
    server.middlewares.use(DEV_IDENTITY_PATH, (_req, res) => {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(identityPayload(server.config.root)));
    });
  },
};

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
    devServerIdentity,
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

  // Dev-server options for the Electron dev run (`scripts/dev.cjs` derives the same port from the
  // same renderer root, waits on it, then points the Electron main at it).
  //
  // 1. don't clear the screen so build/runtime errors stay visible.
  clearScreen: false,
  // 2. the port the Electron dev launcher expects — DERIVED from this renderer root by the ONE
  //    definition in `scripts/dev-server-identity.cjs`, which is also where `dev.cjs` reads it.
  //    It used to be a fixed 1420 for every worktree; the user reversed that call so worktrees can
  //    run in parallel. `strictPort` stays: a derived port that is nonetheless taken must be a LOUD
  //    exit rather than a silent slide to the next port (which would strand `dev.cjs` polling a
  //    port nobody bound), and `dev.cjs` asks its identity probe who holds it before aborting.
  // 3. the HMR port is derived too, and only used on the explicit remote-HMR path. It was a
  //    hard-coded 1421, which would have collided across worktrees through the back door once the
  //    server ports were separated. The default branch leaves `hmr` undefined so Vite derives its
  //    own from the server port.
  server: {
    port: devServerPort(rendererRoot),
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: devHmrPort(rendererRoot),
        }
      : undefined,
  },
}));
