#!/usr/bin/env node
/**
 * GATE PHASE 7 orchestrator (task #22, the FINAL gate).
 *
 * Chains the validations that are EXERCISABLE on the CURRENT platform and skips
 * the Linux-only steps cleanly (a skip is reported, not a failure) — so the same
 * command gives a meaningful, honest result on Windows AND on the Linux/Wayland
 * target. It does NOT, and cannot, conclude the gate by itself: the echo/FPS
 * thresholds, the AppImage, the native Wayland/HiDPI smoke, the E2E run and the
 * live 100 MiB lossless flood are only measurable on the target machine. The
 * authoritative runbook is `apps/electron/GATE-PHASE7.md`.
 *
 * Run with system Node from apps/electron: `node scripts/gate-phase7.cjs`
 *   (or `bun run gate:phase7`).
 *
 * Exit 0 iff every step that RAN passed (skipped steps never fail the run).
 */
"use strict";
const { spawnSync } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");

const appDir = path.resolve(__dirname, "..");
const electronBin = require("electron"); // path to the electron binary
const isLinux = process.platform === "linux";

// A step runs a command; `linuxOnly` steps SKIP (exit 0, reported) off Linux.
// `runner`: "node" → system node; "electron" → the electron binary.
const STEPS = [
  { name: "typecheck", runner: "bun", args: ["run", "typecheck"] },
  { name: "smoke:window", runner: "electron", args: ["scripts/smoke-window.cjs"] },
  { name: "smoke:single-instance", runner: "node", args: ["scripts/smoke-single-instance.cjs"] },
  { name: "smoke:core-host", runner: "electron", args: ["scripts/smoke-core-host.cjs"] },
  { name: "smoke:wayland-flags", runner: "electron", args: ["scripts/smoke-wayland-flags.cjs"] },
  { name: "smoke:flow-control", runner: "node", args: ["scripts/smoke-flow-control.cjs"] },
  { name: "smoke:lifecycle", runner: "electron", args: ["scripts/smoke-lifecycle.cjs"] },
  { name: "smoke:prod-load", runner: "electron", args: ["scripts/smoke-prod-load.cjs"] },
  { name: "smoke:bridge-e2e", runner: "electron", args: ["scripts/smoke-bridge-e2e.cjs"] },
  // Real-launch guard (review 01KVJEY…): the `bun run … dev` path — real `electron .`
  // main + real preload — installs window.nyxCore, picks the Electron adapter, and a
  // bridge call round-trips. This is the smoke the `tsc` preload regression escaped.
  { name: "smoke:dev-launch", runner: "node", args: ["scripts/smoke-dev-launch.cjs"] },
  // …and the prod load path (real built front) for the same class of regression.
  { name: "smoke:dev-launch:prod", runner: "electron", args: ["scripts/smoke-dev-launch.cjs", "--prod"] },
  // Packaging smoke: only if a packaged dir already exists (build is slow; the
  // runbook drives `bun run package` explicitly). Skipped cleanly otherwise.
  {
    name: "smoke:package",
    runner: "node",
    args: ["scripts/smoke-package.cjs"],
    skipIf: () => !hasPackagedDir(),
    skipReason: "no release/*-unpacked dir — run `bun run package` first",
  },
  // Linux-gated: the live shell flood + RSS bound (yes|head -c, /proc RSS).
  {
    name: "smoke:flow-control-live (100 MiB lossless + 60s flood)",
    runner: "electron",
    args: ["scripts/smoke-flow-control-live.cjs"],
    linuxOnly: true,
    skipReason: "live flood is Linux-gated; mechanism proven by smoke:flow-control",
  },
  // Linux-gated: the echo/FPS gate asserts thresholds tied to a 120 Hz Wayland
  // screen. On Linux we ASSERT (--mode=gate); off-target it would FAIL by design,
  // so we SKIP it here and point at the runbook (do not run sanity as a "pass").
  {
    name: "gate:echo (echo medians + ≥110 fps@120Hz)",
    runner: "electron",
    args: ["scripts/gate-echo-harness.cjs", "--mode=gate"],
    linuxOnly: true,
    skipReason:
      "echo/FPS thresholds need the target Wayland 120 Hz screen — see GATE-PHASE7.md step (b)",
  },
];

function hasPackagedDir() {
  const releaseDir = path.join(appDir, "release");
  if (!fs.existsSync(releaseDir)) return false;
  return fs
    .readdirSync(releaseDir)
    .some(
      (d) =>
        /unpacked$/.test(d) && fs.statSync(path.join(releaseDir, d)).isDirectory(),
    );
}

function resolveCmd(step) {
  if (step.runner === "electron") return { cmd: electronBin, args: step.args };
  if (step.runner === "node") return { cmd: process.execPath, args: step.args };
  // bun: resolve from PATH (spawnSync with shell handles the .cmd shim on Windows).
  return { cmd: "bun", args: step.args, shell: process.platform === "win32" };
}

console.log(
  `\n=== GATE PHASE 7 orchestrator — platform=${process.platform} ===\n` +
    `Authoritative runbook: apps/electron/GATE-PHASE7.md\n` +
    (isLinux
      ? "Running ALL exercisable steps (Linux target: echo/FPS + live flood ASSERT).\n"
      : "Running the WINDOWS-exercisable half; Linux-only steps are SKIPPED (not failed).\n"),
);

const results = [];
for (const step of STEPS) {
  const skipLinux = step.linuxOnly && !isLinux;
  const skipCond = !skipLinux && step.skipIf && step.skipIf();
  if (skipLinux || skipCond) {
    const reason = skipLinux ? step.skipReason : step.skipReason || "precondition not met";
    console.log(`\n--- SKIP  ${step.name}  (${reason})`);
    results.push({ name: step.name, status: "SKIP", reason });
    continue;
  }
  console.log(`\n--- RUN   ${step.name}`);
  const { cmd, args, shell } = resolveCmd(step);
  const r = spawnSync(cmd, args, {
    cwd: appDir,
    stdio: "inherit",
    shell: shell || false,
    env: process.env,
  });
  const ok = r.status === 0;
  results.push({ name: step.name, status: ok ? "PASS" : "FAIL", code: r.status });
  if (!ok) console.log(`--- FAIL  ${step.name} (exit ${r.status})`);
}

// --- Summary -----------------------------------------------------------------
console.log(`\n=== GATE PHASE 7 — summary (platform=${process.platform}) ===`);
for (const r of results) {
  const tag = r.status === "PASS" ? "PASS" : r.status === "SKIP" ? "SKIP" : "FAIL";
  console.log(`  [${tag}] ${r.name}${r.reason ? ` — ${r.reason}` : ""}`);
}
const failed = results.filter((r) => r.status === "FAIL");
const skipped = results.filter((r) => r.status === "SKIP");
const passed = results.filter((r) => r.status === "PASS");
console.log(
  `\n  ${passed.length} passed, ${skipped.length} skipped, ${failed.length} failed.`,
);

if (!isLinux) {
  console.log(
    "\nNOTE: this is the WINDOWS half of the gate. The Linux-only steps above are\n" +
      "the phase-7 target session — derive them on the Linux/Wayland 120 Hz machine\n" +
      "via apps/electron/GATE-PHASE7.md. The frontend `browser` suite and the\n" +
      "`nyx-core` POSIX/path tests have a documented Windows baseline in that runbook.",
  );
}

process.exit(failed.length > 0 ? 1 : 0);
