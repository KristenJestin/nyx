/**
 * THE ONE `electron` STUB every main-process spec shares — because `mock.module` is NOT scoped.
 *
 * bun's `mock.module` replaces a module for the WHOLE test process, last registration wins; it is
 * not isolated per file. Three specs in this folder need `electron` gone — `identity.test.ts`,
 * `sidecar.test.ts`, `window-ipc.test.ts` — and while each declared its own shape inline, the
 * narrowest one could land last and win. Running them together (`bun test src/main`) then broke
 * mid-suite on a module the winning stub had never heard of:
 *
 *     SyntaxError: Export named 'shell' not found in module '…/electron/index.js'
 *
 * The fix is not ordering — the collision was symmetric — it is SHAPE. Every spec registers THIS
 * SAME object, so whichever registration lands last changes nothing:
 *
 *     import { electronStub } from "./test-electron-stub";
 *     mock.module("electron", () => electronStub);
 *
 * A new spec that needs another slice of Electron ADDS IT HERE. Registering a narrower `electron`
 * next door re-opens the exact hole this file closes, and it re-opens it silently — in the file
 * that did nothing wrong.
 *
 * This module imports nothing from `bun:test` (so it stays inside the shell's own `tsc --noEmit`,
 * which excludes `*.test.ts`) and nothing from `electron` (it stands in for it). It is never
 * bundled: no production entrypoint reaches it.
 */

/** The `showOpenDialog` options a spec reads back — the slice stubbed, not Electron's full type. */
export interface StubOpenDialogOptions {
  properties?: string[];
  title?: string;
  filters?: { name: string; extensions: string[] }[];
}

/** What a stubbed `showOpenDialog` answers. */
export interface StubOpenDialogResult {
  canceled: boolean;
  filePaths: string[];
}

/** An `ipcMain.handle` listener, in the shape a spec invokes it. */
export type StubIpcHandler = (event: unknown, ...args: unknown[]) => unknown;

/** The channels registered through the stubbed `ipcMain`, keyed like the real one. */
export const ipcHandlers = new Map<string, StubIpcHandler>();

/** Every options object handed to the stubbed `showOpenDialog`, in call order. */
export const dialogCalls: StubOpenDialogOptions[] = [];

/** What the NEXT `showOpenDialog` answers. Cancelled until a spec scripts otherwise, so a spec
 *  that forgets to script one gets the harmless branch rather than a stale selection. */
let dialogAnswer: StubOpenDialogResult = { canceled: true, filePaths: [] };

/** Script what the next `showOpenDialog` answers. */
export function scriptOpenDialog(answer: StubOpenDialogResult): void {
  dialogAnswer = answer;
}

/**
 * The single path `app.getPath` answers with, whatever the name. A SENTENCE rather than a
 * plausible directory: no spec here may depend on a real userData dir, and a value that reads as
 * an instruction makes the leak obvious in the assertion that catches it.
 */
export const STUB_APP_PATH = "/should/not/be/read";

/**
 * Every `name` handed to the stubbed `app.getPath`, in call order. The stub answers STUB_APP_PATH
 * whatever the name, so the returned path alone cannot tell `getPath("userData")` from
 * `getPath("home")`; a spec that cares WHICH path was asked for reads this instead.
 */
export const appPathCalls: string[] = [];

/** The module `mock.module("electron", () => electronStub)` installs, for every spec. */
export const electronStub = {
  app: {
    getPath(name: string): string {
      appPathCalls.push(name);
      return STUB_APP_PATH;
    },
  },
  // `null` is the window-LESS branch, which every handler must survive: `showOpenDialog(opts)`
  // rather than `showOpenDialog(win, opts)`.
  BrowserWindow: { fromWebContents: (): null => null },
  // `./window` (reached from `./window-ipc` for `confirmWindowClose`) imports `shell` for its
  // external-link handler. Stubbed so the module graph resolves; no spec opens anything.
  shell: { openExternal: (): Promise<void> => Promise.resolve() },
  dialog: {
    showOpenDialog(...args: unknown[]): Promise<StubOpenDialogResult> {
      dialogCalls.push(args[args.length - 1] as StubOpenDialogOptions);
      return Promise.resolve(dialogAnswer);
    },
  },
  ipcMain: {
    handle(channel: string, listener: StubIpcHandler): void {
      // The real `ipcMain.handle` THROWS on a second registration of the same channel — the very
      // contract `window-ipc.ts` leans on for its idempotency ("called exactly once"). `ipcHandlers`
      // is a process-wide singleton, so a stub that overwrote in silence would let a second spec
      // re-register the same channels with ITS deps and have this spec's assertions read the other
      // one's disk — the inter-spec collision this file exists to close, one floor down.
      if (ipcHandlers.has(channel))
        throw new Error(`Attempted to register a second handler for '${channel}'`);
      ipcHandlers.set(channel, listener);
    },
  },
};
