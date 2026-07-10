import { Store } from "@tanstack/react-store";

import { XtermInstance, type XtermInstanceOptions } from "./xterm-instance";

/**
 * THE MODULE-LEVEL TERMINAL STORE.
 *
 * Live xterm instances are held in a FLAT, IMPERATIVE, MODULE-LEVEL `Map`,
 * OUTSIDE React and OUTSIDE the router's lifecycle. This is what makes terminals
 * survive NAVIGATION (not just a selection switch): the router mounts/unmounts
 * the Terminals route freely, but the instances never live in that tree — a route
 * only ATTACHES an instance's persistent DOM node into its pane and DETACHES it on
 * unmount. The instance (and its xterm buffer / screen state) stays put in this
 * Map, so a running command keeps streaming into the buffer and is reattached
 * losslessly on return. The instance is DISPOSED (PTY closed, xterm freed) ONLY
 * when the record is truly closed — never on navigation.
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
const instances = new Map<string, XtermInstance>();

/** The reactive metadata slice (ids + titles). */
export const terminalStore = new Store<TerminalStoreState>({ ids: [], titles: {} });

function publishIds(): void {
  terminalStore.setState((s) => ({ ...s, ids: Array.from(instances.keys()) }));
}

function setTitle(recordId: string, title: string): void {
  terminalStore.setState((s) => ({ ...s, titles: { ...s.titles, [recordId]: title } }));
}

/**
 * Ensure a live instance exists for `options.recordId`, creating it once if
 * absent (idempotent — a second call with the same id returns the EXISTING
 * instance, so a remount/reattach never respawns the shell). The instance owns
 * its persistent DOM node + PTY wiring from construction; the caller attaches it
 * to a pane with {@link attachTerminal}.
 */
export function ensureTerminal(options: XtermInstanceOptions): XtermInstance {
  const existing = instances.get(options.recordId);
  if (existing) return existing;

  const instance = new XtermInstance({
    ...options,
    onTitle: (title) => {
      setTitle(options.recordId, title);
      options.onTitle?.(title);
    },
  });
  instances.set(options.recordId, instance);
  publishIds();
  return instance;
}

/** Read a live instance by id (or `undefined` if none). */
export function getTerminal(recordId: string): XtermInstance | undefined {
  return instances.get(recordId);
}

/**
 * ATTACH an instance's persistent DOM node into a pane container (MOVES the node,
 * never clones), then arm the app-driven resize/geometry pipeline. Returns
 * whether an instance existed to attach.
 */
export function attachTerminal(recordId: string, container: HTMLElement): boolean {
  const instance = instances.get(recordId);
  if (!instance) return false;
  instance.attach(container);
  return true;
}

/**
 * DETACH an instance's DOM node from its pane — the navigation path. Removes the
 * node and stops observing, but leaves the instance ALIVE in the Map (PTY open,
 * buffer intact). A later {@link attachTerminal} reattaches the SAME node
 * losslessly. NEVER disposes.
 */
export function detachTerminal(recordId: string): void {
  const instance = instances.get(recordId);
  if (!instance) return;
  instance.detach();
}

/**
 * REAL close of a terminal: dispose the instance (PTY closed, xterm freed, node
 * removed) and drop it from the store. This is the ONLY path that calls
 * `dispose()` — never navigation.
 */
export function disposeTerminal(recordId: string): void {
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
 * churn of create/close never leaks. Called by the deck with the current live
 * record ids.
 */
export function reconcileTerminals(liveIds: readonly string[]): void {
  const live = new Set(liveIds);
  for (const id of Array.from(instances.keys())) {
    if (!live.has(id)) disposeTerminal(id);
  }
}

/** TEST-ONLY: drop every instance (used by unit tests to reset module state). */
export function __resetTerminalStore(): void {
  for (const id of Array.from(instances.keys())) disposeTerminal(id);
}
