import { Component, type ErrorInfo, type ReactNode } from "react";

/**
 * `<ErrorBoundary>` — a minimal, reusable React error boundary. The renderer previously had NONE, so a
 * throw anywhere in a routed page (e.g. the third-party `@pierre/diffs` viewer tearing down when the
 * active review repo switches to an empty diff) propagated to the React ROOT and unmounted the ENTIRE
 * tree — blanking the whole app (rail + page alike). This contains a subtree crash so the surrounding
 * chrome survives and the affected region shows a small fallback instead.
 *
 * `resetKeys` clears the caught error when any key changes (referential/`Object.is` compare) — pass the
 * identity of the data the subtree renders (e.g. the active diff), so once that data moves on the
 * boundary re-mounts its children and recovers, rather than staying stuck on the fallback.
 */
interface ErrorBoundaryProps {
  readonly children: ReactNode;
  readonly fallback: ReactNode;
  readonly resetKeys?: readonly unknown[];
}

interface ErrorBoundaryState {
  readonly error: Error | null;
}

/**
 * The last render error any `<ErrorBoundary>` caught, mirrored onto `window` so a driver (the CDP
 * review gate, a browser-mode repro) can read the ACTUAL message + stack + React component stack of
 * a swallowed crash. `wdio` does not pipe the renderer console into its log, so the boundary's
 * `console.error` is invisible to the gate; this seam makes the crash inspectable from the outside
 * (`browser.execute(() => window.__nyxLastTreeError)`). Diagnostic only — never drives the UI.
 */
export interface NyxCaughtError {
  readonly message: string;
  readonly stack: string;
  readonly componentStack: string;
  readonly at: number;
}

declare global {
  interface Window {
    __nyxLastTreeError?: NyxCaughtError;
  }
}

function sameKeys(a?: readonly unknown[], b?: readonly unknown[]): boolean {
  if (a === b) return true;
  if (a == null || b == null || a.length !== b.length) return false;
  return a.every((v, i) => Object.is(v, b[i]));
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // Keep the crash observable (the boundary swallows it for the UI, not for the console).
    console.error("ErrorBoundary caught a render error:", error, info.componentStack);
    // Mirror it onto `window` so an OUT-OF-PROCESS driver can read the real stack even when the
    // renderer console is not piped into its log (the CDP review gate). Diagnostic only.
    if (typeof window !== "undefined") {
      window.__nyxLastTreeError = {
        message: error?.message ?? String(error),
        stack: error?.stack ?? "",
        componentStack: info.componentStack ?? "",
        at: Date.now(),
      };
    }
  }

  componentDidUpdate(prev: ErrorBoundaryProps): void {
    if (this.state.error !== null && !sameKeys(prev.resetKeys, this.props.resetKeys)) {
      this.setState({ error: null });
    }
  }

  render(): ReactNode {
    return this.state.error !== null ? this.props.fallback : this.props.children;
  }
}
