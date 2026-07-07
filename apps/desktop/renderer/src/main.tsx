// wterm's stylesheet carries the terminal layout rules (`.wterm`, `.term-grid`,
// `.term-row`) the DOM renderer relies on. Imported once here so every terminal
// surface (`<Terminal>` and the read-only `<CommandOutputPanel>`) is styled even
// before its instance mounts. (v2.2.5: xterm — and its WebGL canvas-positioning
// workarounds — are gone; wterm renders to the DOM, so there is no canvas layer
// to position.) The instance modules also import this so the style is present in
// any entrypoint, but keeping it here mirrors the original global-load intent.
import "@wterm/dom/css";

import React from "react";
import ReactDOM from "react-dom/client";
import App from "./app";

// Mark the shell on <html> so the shell-specific CSS applies. Under Electron the
// frameless-window DRAG region is a CSS concern (`-webkit-app-region: drag`): we tag the
// document `data-shell="electron"` (detected via the allowlisted `window.nyxCore` preload
// bridge) and a CSS rule in globals.css maps the drag-region attribute the chrome markup
// carries to the Electron drag region.
if (typeof window !== "undefined" && (window as { nyxCore?: unknown }).nyxCore) {
  document.documentElement.setAttribute("data-shell", "electron");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
