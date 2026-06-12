import "@/globals.css";

import { useCallback } from "react";
import type { Terminal as XTerm } from "@xterm/xterm";

import { Terminal } from "@/components/terminal/terminal";

/**
 * E2E test seam exposed on `window.__nyx`.
 *
 * xterm renders into a WebGL canvas, so the terminal's text is NOT in the DOM
 * and a WebDriver (tauri-driver / WebKitWebDriver) cannot read it by querying
 * elements. To let the end-to-end suite assert on real shell output we expose
 * the live xterm instance plus a `readBuffer()` that flattens the xterm buffer
 * (screen + scrollback) to a string. This is inert in normal use — nothing
 * reads it unless a driver calls `executeScript` — and adds no behavior.
 */
declare global {
  interface Window {
    __nyx?: {
      term: XTerm | null;
      readBuffer: () => string;
    };
  }
}

function App() {
  // Stable callback so the Terminal does not see a changing prop each render.
  const handleInstance = useCallback((instance: XTerm | null) => {
    const readBuffer = (): string => {
      if (!instance) return "";
      const buf = instance.buffer.active;
      let out = "";
      for (let i = 0; i < buf.length; i++) {
        const line = buf.getLine(i);
        if (line) out += line.translateToString(true) + "\n";
      }
      return out;
    };
    window.__nyx = { term: instance, readBuffer };
  }, []);

  return (
    <main className="h-screen w-screen overflow-hidden bg-background">
      <Terminal onInstance={handleInstance} />
    </main>
  );
}

export default App;
