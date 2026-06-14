import { resolve } from "node:path";
import { platform } from "node:process";

import react from "@vitejs/plugin-react";
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
  plugins: [react(), tsconfigPaths()],
  test: {
    projects: [
      {
        // Inherit the root plugins (react + path aliases) for this project.
        extends: true,
        test: {
          name: "unit",
          environment: "jsdom",
          globals: true,
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
