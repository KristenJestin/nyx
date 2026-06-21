#!/usr/bin/env node
/**
 * Smoke for the WAYLAND-NATIVE + HiDPI flags (task #9). Runs under Electron and
 * asserts `applyWaylandFlags` registers EXACTLY the POC-validated switches on
 * `app.commandLine`, with NO per-machine scale hack:
 *
 *   - `ozone-platform-hint` = `auto` (default) / the `NYX_OZONE` value;
 *   - `enable-features` includes `WaylandWindowDecorations`;
 *   - `force-device-scale-factor` is NOT set (scaling = compositor → native HiDPI).
 *
 * Flag REGISTRATION is platform-independent (Chromium parses these on every OS, and
 * ignores the Ozone hint off-Linux), so this is verifiable on this Windows host. The
 * runtime outcomes the task also names — `xwayland:false` and 120 fps on a 120 Hz
 * Wayland screen — are LINUX-ONLY and are NOT exercisable here; they were measured
 * in the POC (annex §H) and are documented in `src/main/wayland.ts`.
 *
 * Run under Electron: `electron scripts/smoke-wayland-flags.cjs`.
 */
"use strict";
const { app } = require("electron");
const { applyWaylandFlags } = require("../dist/main/wayland.js");

function fail(msg) {
  console.error("[smoke-wayland-flags] FAIL:", msg);
  process.exit(1);
}

// Apply with the default (auto) hint.
applyWaylandFlags(app);

const ozone = app.commandLine.getSwitchValue("ozone-platform-hint");
const features = app.commandLine.getSwitchValue("enable-features");
const expectedOzone = (process.env.NYX_OZONE || "auto").toLowerCase();

if (ozone !== expectedOzone) fail(`ozone-platform-hint = "${ozone}", expected "${expectedOzone}"`);
console.log(`[smoke-wayland-flags] ozone-platform-hint = ${ozone} ✓`);

if (!features.includes("WaylandWindowDecorations"))
  fail(`enable-features = "${features}" missing WaylandWindowDecorations`);
console.log(`[smoke-wayland-flags] enable-features includes WaylandWindowDecorations ✓`);

if (app.commandLine.hasSwitch("force-device-scale-factor"))
  fail("force-device-scale-factor IS set — that is the per-machine HiDPI hack this task forbids");
console.log("[smoke-wayland-flags] no force-device-scale-factor (native HiDPI) ✓");

console.log("[smoke-wayland-flags] OK — Wayland/HiDPI flags registered (Linux runtime not exercisable here).");
process.exit(0);
