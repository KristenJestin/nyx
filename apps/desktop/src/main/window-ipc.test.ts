/**
 * THE PICTURE HANDLER — `nyx:window/pick-project-image`, tested through the registered
 * `ipcMain.handle` itself, with BOTH a fake `dialog` and a fake filesystem.
 *
 * `project-image.test.ts` next door pins the RULES (what is refused, what is deleted, what
 * survives) on the pure functions. What it cannot reach is the handler that joins them to the
 * native picker, and that seam carries a decision of its own: the value handed back to the
 * renderer — the one that ends up in `projects.image_path` — is the path of the COPY nyx owns,
 * never `result.filePaths[0]`, the file the user picked and may move tomorrow. A regression there
 * type-checks, renders, and quietly stores a path outside the app's data dir.
 *
 * Nothing here touches a real userData dir, and nothing here replaces a module the rest of the
 * folder relies on. Two different tools, deliberately:
 *   - `electron` is the SHARED `./test-electron-stub`, the one object every spec in this folder
 *     registers — `mock.module` is process-global and last-write-wins, so a stub declared inline
 *     here would fight `identity.test.ts` and `sidecar.test.ts` rather than coexist with them.
 *     It is applied BEFORE `./window-ipc` is imported, which is why that import is dynamic.
 *   - the filesystem is INJECTED, not mocked: `registerWindowIpc` takes its `ImageFs` and its
 *     images dir as arguments, so the `Map` below stands in for the disk of this file ALONE.
 *     A `mock.module("node:fs", …)` would have replaced `node:fs` for every other spec in the
 *     process too, and handed them an imaginary disk that answers instead of failing.
 *
 * Run via `bun test` (the `smoke:window-ipc` script).
 */
import { describe, expect, mock, test } from "bun:test";
import { copyFileSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { basename, dirname, join } from "node:path";

import { WINDOW_CHANNELS } from "../shared/ipc";
import {
  PROJECT_IMAGE_EXTENSIONS,
  PROJECT_IMAGE_MAX_BYTES,
  projectImagesDir,
  type ImageFs,
} from "./project-image";
import {
  appPathCalls,
  dialogCalls,
  electronStub,
  ipcHandlers,
  scriptOpenDialog,
  STUB_APP_PATH,
} from "./test-electron-stub";

mock.module("electron", () => electronStub);

/** A throwaway userData root — never a real path, and never read from `app.getPath` here. */
const USER_DATA = join("/data", "nyx-v2");
/** Where the copies live for that userData: the directory every assertion below reads. */
const DIR = projectImagesDir(USER_DATA);
/** A source picture, outside the app's dir — where the user's own file lives. */
const pictures = (name: string) => join("/home", "u", "pictures", name);

/** The whole filesystem, path → byte size, reseeded per spec. */
let files = new Map<string, number>();
let dirs = new Set<string>();

function seedFs(seed: Record<string, number> = {}): void {
  files = new Map<string, number>(Object.entries(seed));
  dirs = new Set<string>();
}

/** The names the copies directory currently holds, sorted — the state every spec reads off. */
function copies(): string[] {
  return [...files.keys()]
    .filter((f) => dirname(f) === DIR)
    .map((f) => basename(f))
    .sort();
}

// The in-memory disk the handlers are wired to — the same shape `project-image.test.ts` uses:
// `copyFileSync` records the copy, `rmSync` really removes. Handed to `registerWindowIpc` below,
// so it is this file's disk and nobody else's.
const memoryFs: ImageFs = {
  mkdirSync(path: string) {
    dirs.add(path);
  },
  readdirSync(path: string) {
    return [...files.keys()].filter((f) => dirname(f) === path).map((f) => basename(f));
  },
  statSync(path: string) {
    const size = files.get(path);
    if (size === undefined) throw new Error(`ENOENT: ${path}`);
    return { size };
  },
  copyFileSync(source: string, destination: string) {
    files.set(destination, files.get(source) ?? 0);
  },
  rmSync(path: string) {
    files.delete(path);
  },
  existsSync(path: string) {
    return dirs.has(path) || files.has(path) || [...files.keys()].some((f) => dirname(f) === path);
  },
};

const { registerWindowIpc, realProjectImageDeps } = await import("./window-ipc");
registerWindowIpc({ fs: memoryFs, imagesDir: () => DIR });

/** Invoke the picture channel exactly as the renderer's `invoke` would. */
function pickProjectImage(projectId: string): Promise<string | null> {
  const handler = ipcHandlers.get(WINDOW_CHANNELS.pickProjectImage);
  if (!handler) throw new Error("pick-project-image was never registered");
  return Promise.resolve(handler({}, projectId) as Promise<string | null>);
}

/** The reclaim channel, with the copy the caller just persisted and wants kept. */
function discardProjectImage(projectId: string, keep?: string): Promise<void> {
  const handler = ipcHandlers.get(WINDOW_CHANNELS.discardProjectImage);
  if (!handler) throw new Error("discard-project-image was never registered");
  return Promise.resolve(handler({}, projectId, keep) as Promise<void>);
}

describe("the DEFAULT deps are the production wiring", () => {
  // Injecting the filesystem bought this file its isolation, but it also took the only test that
  // ever traversed `realProjectImageDeps`: every production call site is `registerWindowIpc()` with
  // no argument (src/main/index.ts, scripts/smoke-window.cjs, scripts/smoke-dev-launch.cjs), and the
  // only caller that passes deps is this spec. So the default is inspected here as an OBJECT — no
  // handler is invoked with it, nothing is written to a real disk — which is enough to keep the one
  // claim the feature rests on under a test: the copy lands in the app's own data dir.
  test("`imagesDir()` is `<userData>/project-icons`, resolved through `app.getPath('userData')`", () => {
    const before = appPathCalls.length;

    expect(realProjectImageDeps.imagesDir()).toBe(join(STUB_APP_PATH, "project-icons"));
    // The stub answers the same path for every name, so the return value alone would survive a
    // slip to `getPath("home")`. This is what makes that slip red.
    expect(appPathCalls.slice(before)).toEqual(["userData"]);
  });

  test("`imagesDir()` resolves LATE — a second call re-reads `app.getPath`", () => {
    // The handlers call `deps.imagesDir()` per invocation precisely so that nothing pins a path at
    // import time (`registerWindowIpc` runs before `app` is ready in the real main). A memoised
    // implementation would stop asking, and would freeze whatever userData was at import.
    const before = appPathCalls.length;
    realProjectImageDeps.imagesDir();
    realProjectImageDeps.imagesDir();
    expect(appPathCalls.slice(before)).toEqual(["userData", "userData"]);
  });

  test("`fs` is the real `node:fs`, not a stand-in", () => {
    // Identity, not shape: a slice that type-checks but points at anything other than node:fs would
    // satisfy `ImageFs` and copy nowhere.
    expect(realProjectImageDeps.fs.copyFileSync).toBe(copyFileSync);
    expect(realProjectImageDeps.fs.mkdirSync).toBe(mkdirSync);
    expect(realProjectImageDeps.fs.readdirSync).toBe(readdirSync);
    expect(realProjectImageDeps.fs.rmSync).toBe(rmSync);
  });
});

describe("the picture the user picks is not the picture nyx stores", () => {
  test("a valid pick resolves the COPY's path, never the file that was picked", async () => {
    const source = pictures("logo.png");
    seedFs({ [source]: 1024 });
    scriptOpenDialog({ canceled: false, filePaths: [source] });

    const stored = await pickProjectImage("proj-1");

    // THE DECISION OF THE WHOLE FEATURE: what reaches `projects.image_path` is a file inside the
    // app's data dir, so moving or deleting the original changes nothing.
    expect(dirname(stored!)).toBe(DIR);
    expect(basename(stored!)).toMatch(/^proj-1~[0-9a-z]+\.png$/);
    expect(stored).not.toBe(source);
    expect(copies()).toEqual([basename(stored!)]);
    expect(files.has(source)).toBe(true);
  });

  test("PICKING TWICE resolves two different paths — the channel never repeats itself", async () => {
    // The seam the renderer actually consumes. `<ProjectMark>` and the picker thumbnail draw
    // whatever `image_path` holds and remember a source that failed to load; both are defeated by
    // a channel that answers the same path twice, which is what replacing a `.png` by a `.png`
    // used to do. Two PNGs, same project, back to back — the ordinary "here is my new logo".
    seedFs({ [pictures("old.png")]: 10, [pictures("new.png")]: 20 });
    scriptOpenDialog({ canceled: false, filePaths: [pictures("old.png")] });
    const first = await pickProjectImage("proj-1");
    scriptOpenDialog({ canceled: false, filePaths: [pictures("new.png")] });
    const second = await pickProjectImage("proj-1");

    expect(second).not.toBe(first);
    // Both copies are on disk at this point, ON PURPOSE: the row still names the first until the
    // renderer's `set_project_mark` lands, and a picking channel that deleted it first would turn
    // a refused write into a lost picture. The reclaim is the renderer's second step — the
    // `discardProjectImage` channel, told which copy to KEEP.
    expect(copies().sort()).toEqual([basename(first!), basename(second!)].sort());
    await discardProjectImage("proj-1", second!);
    // …and then exactly ONE: a moving path must not be a way to accumulate orphans.
    expect(copies()).toEqual([basename(second!)]);
  });

  test("the picker is opened on the CURATED extensions, one file at a time", async () => {
    seedFs({ [pictures("logo.png")]: 10 });
    scriptOpenDialog({ canceled: false, filePaths: [pictures("logo.png")] });
    await pickProjectImage("proj-1");

    // `dialogCalls` is the shared stub's recorder — every options object, in call order.
    const opts = dialogCalls.at(-1)!;
    expect(opts.properties).toEqual(["openFile"]);
    // The dialog offers exactly what `adoptProjectImage` accepts — a filter that drifted from the
    // validation would let the OS present files the handler then refuses.
    expect(opts.filters?.[0]?.extensions).toEqual([...PROJECT_IMAGE_EXTENSIONS]);
  });
});

describe("CANCELLING is the only null, and it touches nothing", () => {
  test("`canceled: true` resolves null and leaves the previous copy alone", async () => {
    seedFs({ [join(DIR, "proj-1.png")]: 10, [pictures("logo.svg")]: 10 });
    // A cancelled dialog can still carry paths on some platforms — `canceled` is what decides.
    scriptOpenDialog({ canceled: true, filePaths: [pictures("logo.svg")] });

    expect(await pickProjectImage("proj-1")).toBeNull();
    // Nothing written, nothing deleted: the project keeps the picture it already had.
    expect(copies()).toEqual(["proj-1.png"]);
    expect(files.has(join(DIR, "proj-1.svg"))).toBe(false);
  });

  test("an EMPTY selection is the same null, not a crash on `filePaths[0]`", async () => {
    seedFs({ [join(DIR, "proj-1.png")]: 10 });
    scriptOpenDialog({ canceled: false, filePaths: [] });

    expect(await pickProjectImage("proj-1")).toBeNull();
    expect(copies()).toEqual(["proj-1.png"]);
  });
});

describe("a REFUSAL rejects — it never resolves null", () => {
  // Resolving `null` here would make "your 40 MB scan was refused" indistinguishable from "you
  // pressed Cancel", and the settings modal would have nothing to toast.
  test("an unsupported extension rejects with a sentence naming the file", async () => {
    seedFs({ [join(DIR, "proj-1.png")]: 10, [pictures("anim.gif")]: 10 });
    scriptOpenDialog({ canceled: false, filePaths: [pictures("anim.gif")] });

    await expect(pickProjectImage("proj-1")).rejects.toThrow(/anim\.gif/);
    // And the refusal did not cost the user the picture they had already chosen.
    expect(copies()).toEqual(["proj-1.png"]);
  });

  test("an oversized picture rejects with the readable bound", async () => {
    seedFs({ [pictures("scan.png")]: PROJECT_IMAGE_MAX_BYTES + 1 });
    scriptOpenDialog({ canceled: false, filePaths: [pictures("scan.png")] });

    await expect(pickProjectImage("proj-1")).rejects.toThrow(/2 MB/);
    expect(copies()).toEqual([]);
  });

  test("a source that cannot be READ reaches the renderer as a sentence, not as ENOENT", async () => {
    // The picked file vanished between the dialog and the copy. `ipcMain.handle` turns this throw
    // into the renderer's rejection verbatim, so what the contract promises — "a readable message
    // the caller toasts" — is decided right here.
    seedFs({ [join(DIR, "proj-1.png")]: 10 });
    scriptOpenDialog({ canceled: false, filePaths: [pictures("gone.png")] });

    const refused = await pickProjectImage("proj-1").then(
      () => null,
      (error: Error) => error,
    );
    expect(refused?.message).toContain("gone.png");
    expect(refused?.message).toContain("could not be read");
    expect(refused?.message).not.toContain("ENOENT");
    expect(copies()).toEqual(["proj-1.png"]);
  });
});
