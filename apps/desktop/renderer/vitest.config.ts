import { resolve } from "node:path";
import { platform } from "node:process";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import Icons from "unplugin-icons/vite";
import tsconfigPaths from "vite-tsconfig-paths";
import { playwright } from "@vitest/browser-playwright";
import { defineConfig } from "vitest/config";

/**
 * Where the Browser Mode visual-regression BASELINE lives.
 *
 * It is a GITIGNORED dir at the repo root (NOT under `src/`, NOT `/tmp`), so the
 * binary baseline never lands in the source tree / git diff, yet persists across
 * reboots for local dev. On a fresh checkout / CI the dir is absent → the test
 * regenerates the baseline on first run and detects no regression (consistent
 * with PRD 0 having no CI). See `.gitignore` for the caveat.
 */
const SCREENSHOT_DIR = resolve(__dirname, ".vitest-screenshots");

/**
 * Vitest 4 multi-project config.
 *
 * - `unit`    — jsdom, fast logic/component tests (DEFAULT; no browser needed).
 * - `browser` — real Chromium via Playwright (headless), the only place WebGL
 *   actually paints. Holds the render + visual-regression tests. Browser specs
 *   use the `*.browser.test.tsx` suffix so the two suites never overlap.
 *
 * Run the unit suite alone with `bun run test:unit` (no Chromium required) and
 * the browser suite with `bun run test:browser`. `bun run test` runs both.
 */
export default defineConfig({
  // `Icons` resolves `~icons/<set>/<slug>` brand-logo imports (finding #55) the same way
  // the app build does, so component tests importing a terminal row don't fail on the
  // unresolved virtual module. Same compiler/jsx as `vite.config.ts`.
  // `tailwindcss()` compiles the REAL utility CSS from `globals.css` — matching the app build —
  // so a BROWSER-mode spec that imports `@/globals.css` gets the true Tailwind layout (grid
  // tracks, `min-w-[clamp(...)]`, `overflow-*`, `h-app`). Without it the utilities never emit and
  // a layout/overflow regression cannot be reproduced in the real browser. Only CSS that pulls in
  // `@import "tailwindcss"` (i.e. `globals.css`) is affected, so the other browser specs (which
  // never import it) render exactly as before.
  plugins: [react(), tailwindcss(), tsconfigPaths(), Icons({ compiler: "jsx", jsx: "react" })],
  test: {
    projects: [
      {
        // Inherit the root plugins (react + path aliases) for this project.
        extends: true,
        test: {
          name: "unit",
          environment: "jsdom",
          globals: true,
          // A test may take longer than vitest's 5 s default WITHOUT being wrong: the suite runs
          // one fork per core and a starved worker spends most of a `waitFor` not scheduled at
          // all (finding `01KYW0SA15GX1CZ4QFVY9VD75G`, see `vitest.setup.ts`). This bound has to
          // sit above the async-utils one, or a test dies on the clock before its own assertion
          // gets to speak, and the failure names the timer instead of the defect.
          testTimeout: 20_000,
          include: ["src/**/*.{test,spec}.{ts,tsx}"],
          // Browser-only specs run in the `browser` project, not in jsdom.
          exclude: ["src/**/*.browser.{test,spec}.{ts,tsx}"],
          setupFiles: ["./vitest.setup.ts"],
        },
      },
      {
        extends: true,
        test: {
          name: "browser",
          include: ["src/**/*.browser.{test,spec}.{ts,tsx}"],
          browser: {
            enabled: true,
            headless: true,
            provider: playwright(),
            // A single Chromium instance is enough for the render/visual check.
            instances: [{ browser: "chromium" }],
            expect: {
              toMatchScreenshot: {
                // Route the reference baseline OUT of the source tree into the
                // gitignored root dir. We drop `testFileDirectory` from the path
                // (it would put PNGs under `src/`) and key only on the test file
                // name + arg, so the baseline is `.vitest-screenshots/<file>/<arg>-<browser>-<platform>.png`.
                resolveScreenshotPath: ({ arg, ext, testFileName, browserName }) =>
                  resolve(SCREENSHOT_DIR, testFileName, `${arg}-${browserName}-${platform}${ext}`),
              },
            },
          },
        },
      },
    ],
  },
});
