/**
 * Wayland-native + HiDPI command-line flags for the Electron shell (PRD task #9).
 *
 * The target platform is Linux/Wayland; the POC measured **120 fps native Wayland**
 * (`xwayland:false`, hardware GPU) and **correct per-monitor HiDPI** with ONLY the
 * two Ozone flags below and — crucially — **no `--force-device-scale-factor`**. On
 * Wayland the compositor owns scaling; forcing a scale factor is exactly the
 * per-machine hack this task forbids (it breaks per-monitor DPI and double-scales).
 *
 * These switches must be appended on the COMMAND LINE **before** `app` is ready
 * (Chromium reads Ozone/feature switches at startup), so `applyWaylandFlags()` is
 * called at the very top of the main entrypoint, before `app.whenReady()`.
 *
 * ### The flags (and why)
 *
 *   - `ozone-platform-hint = <hint>` — lets Chromium pick the Ozone backend: `auto`
 *     selects Wayland when the session exposes it and **falls back to X11**
 *     otherwise. This is the documented, hack-free way to get native Wayland with a
 *     safe X11 fallback. Overridable per-launch via `NYX_OZONE=wayland|x11|auto`
 *     (default `auto`) for users on a broken compositor — NOT a per-machine value
 *     baked into the build.
 *   - `enable-features = WaylandWindowDecorations` — client-side decorations (CSD),
 *     required for a frameless window to be draggable/resizable under Wayland
 *     compositors that don't draw server-side decorations.
 *
 * ### Fallback / honesty
 *
 * `ozone-platform-hint=auto` means an X11 session (or `NYX_OZONE=x11`) transparently
 * runs under XWayland/X11 — no code path differs. The single known non-fatal startup
 * warning under Wayland (`--ozone-platform=wayland is not compatible with Vulkan`)
 * is benign: Chromium auto-falls back to OpenGL (POC §H) and still renders at 120fps.
 *
 * This is Linux's concern; these switches are inert / harmless on Windows and macOS
 * (Ozone is Linux-only), so we still apply them unconditionally for a single code
 * path — Chromium ignores an unknown Ozone hint off-Linux.
 */
import type { App } from "electron";

/** Allowed values for the `NYX_OZONE` override. */
type OzoneHint = "wayland" | "x11" | "auto";

/** Resolve the Ozone platform hint from `NYX_OZONE`, defaulting to `auto`. */
function ozoneHint(): OzoneHint {
  const raw = (process.env.NYX_OZONE ?? "").toLowerCase();
  if (raw === "wayland" || raw === "x11" || raw === "auto") return raw;
  return "auto";
}

/**
 * Append the Wayland-native + HiDPI switches. Call ONCE, synchronously, before
 * `app.whenReady()` — Chromium consumes these at process startup.
 *
 * Notably absent: any `--force-device-scale-factor`. Scaling is the compositor's
 * job; HiDPI is native (POC measured `devicePixelRatio = 1.6` correct, crisp).
 */
export function applyWaylandFlags(app: App): void {
  // Native Wayland with automatic X11 fallback (overridable via NYX_OZONE).
  app.commandLine.appendSwitch("ozone-platform-hint", ozoneHint());
  // Client-side decorations so the frameless window is usable under Wayland.
  app.commandLine.appendSwitch("enable-features", "WaylandWindowDecorations");
  // NO --force-device-scale-factor: per-monitor HiDPI is the compositor's job.
}
