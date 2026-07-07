import { Store } from "@tanstack/react-store";

import { WtermInstance, type WtermInstanceOptions } from "./wterm-instance";

/**
 * THE NO-HOST TERMINAL STORE (v2.2.5).
 *
 * Live terminal instances are held in a FLAT, IMPERATIVE, MODULE-LEVEL `Map`,
 * OUTSIDE React and OUTSIDE the router's lifecycle. This is what makes terminals
 * survive navigation WITHOUT a persistent host mounted off the `<Outlet/>`: the
 * router mounts/unmounts routes freely, but the instances never live in that tree
 * — a route only ATTACHES an instance's persistent DOM node into its pane and
 * DETACHES it on unmount. The instance (and its ghostty WASM screen state) stays
 * put in this Map. A spike validated this holds losslessly across 50 cycles.
 *
 * Only METADATA is reactive: the ids of live instances and their titles live in a
 * `@tanstack/react-store` `Store`, so React views can subscribe to "which
 * terminals exist / their titles" without the heavy instances ever entering the
 * render tree. The instances themselves are read imperatively by id.
 */

/** Reactive metadata mirror (NOT the instances). */
export interface TerminalStoreState {
  /** Live record ids, in insertion order — the set of instances the store holds. */
  readonly ids: readonly string[];
  /** Per-record live title (from OSC 0/2), for views that show it. */
  readonly titles: Readonly<Record<string, string>>;
}

/** The imperative Map of instances — module-level, outside React. */
const instances = new Map<string, WtermInstance>();

/** The reactive metadata slice (ids + titles). */
export const terminalStore = new Store<TerminalStoreState>({ ids: [], titles: {} });

function publishIds(): void {
  terminalStore.setState((s) => ({ ...s, ids: Array.from(instances.keys()) }));
}

function setTitle(recordId: string, title: string): void {
  terminalStore.setState((s) => ({ ...s, titles: { ...s.titles, [recordId]: title } }));
}

/**
 * Ensure a live instance exists for `recordId`, creating + `init()`-ing it once
 * if absent (idempotent — a second call with the same id returns the existing
 * instance, so a remount/reattach never respawns the shell). The instance owns
 * its persistent DOM node from construction; the caller attaches it to a pane
 * with {@link attachTerminal}.
 */
export function ensureTerminal(options: WtermInstanceOptions): WtermInstance {
  const existing = instances.get(options.recordId);
  if (existing) return existing;

  const instance = new WtermInstance({
    ...options,
    onTitle: (title) => {
      setTitle(options.recordId, title);
      options.onTitle?.(title);
    },
  });
  instances.set(options.recordId, instance);
  publishIds();
  // Fire the async init; the shell spawns once. Errors are swallowed so a WASM
  // load failure never breaks the store (the pane just shows an empty terminal).
  void instance.init().catch(() => {});
  return instance;
}

/** Read a live instance by id (or `undefined` if none). */
export function getTerminal(recordId: string): WtermInstance | undefined {
  return instances.get(recordId);
}

/**
 * ATTACH an instance's persistent DOM node into a pane container and start the
 * app-driven resize observer. If the node was previously attached elsewhere,
 * `appendChild` MOVES it (it is never cloned/rebuilt), so the wterm DOM — and the
 * focusable input that is its child — survives intact. Focus is restored on the
 * next frame (an element cannot take focus until it is laid out). Returns whether
 * an instance existed to attach.
 */
export function attachTerminal(recordId: string, container: HTMLElement): boolean {
  const instance = instances.get(recordId);
  if (!instance) return false;
  if (instance.el.parentElement !== container) container.appendChild(instance.el);
  instance.observeResize();
  return true;
}

/**
 * DETACH an instance's DOM node from its pane — the navigation path. We stop the
 * app-driven resize observer (so no stale observer lingers on the now-removed
 * parent) and remove the node from its parent; we DO NOT `destroy()`, so the
 * ghostty core keeps the full screen/alt-screen/cursor state. A later
 * {@link attachTerminal} re-arms the observer (via `observeResize`) and reattaches
 * the SAME node losslessly.
 */
export function detachTerminal(recordId: string): void {
  const instance = instances.get(recordId);
  if (!instance) return;
  instance.stopObserving(); // cut the observer before the node leaves its parent.
  instance.el.remove(); // detach the node; the instance + its state stay in the Map.
}

/**
 * REAL close of a terminal: dispose the instance (PTY closed, wterm destroyed,
 * node removed) and drop it from the store. This is the ONLY path that calls
 * `destroy()` — never navigation.
 */
export function destroyTerminal(recordId: string): void {
  const instance = instances.get(recordId);
  if (!instance) return;
  instance.dispose();
  instances.delete(recordId);
  terminalStore.setState((s) => {
    const titles = { ...s.titles };
    delete titles[recordId];
    return { ids: Array.from(instances.keys()), titles };
  });
}

/**
 * Prune instances whose record id is NO LONGER in the live set (e.g. a terminal
 * closed elsewhere / by an agent over MCP). Disposes each removed instance so a
 * churn of create/close never leaks. Called by the pane host with the current
 * live record ids.
 */
export function reconcileTerminals(liveIds: readonly string[]): void {
  const live = new Set(liveIds);
  for (const id of Array.from(instances.keys())) {
    if (!live.has(id)) destroyTerminal(id);
  }
}

/** TEST-ONLY: drop every instance (used by unit tests to reset module state). */
export function __resetTerminalStore(): void {
  for (const id of Array.from(instances.keys())) destroyTerminal(id);
}
