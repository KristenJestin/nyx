// xterm's own stylesheet MUST load before any terminal mounts: it carries the
// LOAD-BEARING positioning rules (`.xterm-screen { position: relative }` and
// `.xterm-screen canvas { position: absolute; left: 0; top: 0 }`). Without it the
// WebGL/canvas render layers fall into normal flow and stack vertically — the
// viewport goes mostly black with stray glyphs at the top and the real content
// pushed down, "repairing" only when a full repaint (e.g. a selection drag)
// fires. Imported here once so EVERY xterm surface (`<Terminal>` and the
// read-only `<CommandOutputPanel>`) gets it. See FEEDBACK.md #3.
import "@xterm/xterm/css/xterm.css";

import React from "react";
import ReactDOM from "react-dom/client";
import App from "./app";

// Mark the shell on <html> so shell-specific CSS can apply. Under Electron the
// frameless-window DRAG region is a CSS concern (`-webkit-app-region: drag`), unlike
// Tauri/WRY which interprets the `data-tauri-drag-region` attribute natively. We tag
// the document `data-shell="electron"` (detected via the allowlisted `window.nyxCore`
// preload bridge) and a CSS rule in globals.css maps the SAME drag-region attribute
// the chrome markup already carries to an Electron drag region — so the chrome
// component stays shell-agnostic. Tauri runs leave the attribute untagged and WRY
// handles it.
if (typeof window !== "undefined" && (window as { nyxCore?: unknown }).nyxCore) {
  document.documentElement.setAttribute("data-shell", "electron");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
