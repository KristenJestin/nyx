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

// Mark the shell on <html> so the shell-specific CSS applies. Under Electron the
// frameless-window DRAG region is a CSS concern (`-webkit-app-region: drag`, carried by the
// `drag-region` utility on `<ChromeBar>`): we tag the document `data-shell="electron"`
// (detected via the allowlisted `window.nyxCore` preload bridge) so globals.css can add the
// shell-specific safety net that cuts every interactive element back OUT of a drag region.
if (typeof window !== "undefined" && (window as { nyxCore?: unknown }).nyxCore) {
  document.documentElement.setAttribute("data-shell", "electron");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
