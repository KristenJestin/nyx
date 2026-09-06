/**
 * SAFETY test for the EXPLICIT v2 app identity + the data-dir resolution (review
 * `01KWD3R7CYK5JNYKHCZZSW0284`, findings #1/#2 — `01KWD3S4DKESTBW40VW7V6HGGK`).
 *
 * The whole point of v2's in-code identity is that booting WITHOUT `NYX_DATA_DIR` resolves to a
 * `nyx-v2`-namespaced userData dir (e.g. `~/.config/nyx-v2`), NEVER the live v1 dir
 * `~/.config/@nyx/electron`. These are PURE tests of the resolver/helper — they NEVER launch
 * Electron and NEVER read or write the real userData. The Electron `app` is a FAKE in-memory
 * stub, and the OS `appData` root is a throwaway literal.
 *
 * Run via `bun test` (the `smoke:data-dir` npm script). `bun:test`'s `mock.module` stubs the
 * `electron` import so `sidecar.ts`'s pure `resolveDataDirFrom` is reachable without the real
 * Electron runtime.
 */
import { describe, expect, mock, test } from "bun:test";

import {
  APP_NAME,
  APP_USER_MODEL_ID,
  applyAppIdentity,
  deriveUserDataDir,
  type IdentityApp,
  V1_USERDATA_DIR_NAME,
} from "./identity";
import { electronStub } from "./test-electron-stub";

// Stub `electron` so importing `sidecar.ts` (which `import`s `app`) does not need the real
// runtime — the test only exercises the PURE `resolveDataDirFrom`, which takes its inputs
// explicitly (never reads the real `app`; the stub's `getPath` answers `/should/not/be/read`).
// The stub is SHARED with the other specs of this folder on purpose: `mock.module` is global to
// the test process and last-write-wins, so a narrower one declared here would win over theirs
// whenever `bun test src/main` runs the folder in one process, and break them.
mock.module("electron", () => electronStub);
const { resolveDataDirFrom } = await import("./sidecar");

/** An in-memory fake of the slice of Electron's `app` the identity logic drives. Records the
 *  applied name / AUMID / userData, and answers `getPath('appData')` from a seeded root — so
 *  NOTHING touches the real Electron app nor the real per-user config dir. */
function fakeApp(appDataRoot: string): IdentityApp & {
  readonly applied: { name?: string; aumid?: string; userData?: string };
} {
  const applied: { name?: string; aumid?: string; userData?: string } = {};
  return {
    applied,
    setName(name) {
      applied.name = name;
    },
    setAppUserModelId(id) {
      applied.aumid = id;
    },
    getPath(_name) {
      return appDataRoot;
    },
    setPath(_name, value) {
      applied.userData = value;
    },
  };
}

describe("v2 app identity is distinct from v1", () => {
  test("the identity literals are the v2 names, not v1", () => {
    expect(APP_NAME).toBe("nyx-v2");
    expect(APP_USER_MODEL_ID).toBe("com.netsirk.nyxv2");
    // Documented invariant: the v1 userData dir name we must never resolve to.
    expect(V1_USERDATA_DIR_NAME).toBe("@nyx/electron");
    expect(APP_NAME).not.toBe(V1_USERDATA_DIR_NAME);
  });

  test("deriveUserDataDir yields a nyx-v2 dir, distinct from the v1 @nyx/electron dir", () => {
    const v2 = deriveUserDataDir("/home/u/.config", "/");
    expect(v2).toBe("/home/u/.config/nyx-v2");
    // The v1 dir v2 must NEVER resolve to.
    const v1 = ["/home/u/.config", V1_USERDATA_DIR_NAME].join("/");
    expect(v2).not.toBe(v1);
    expect(v2.endsWith("/nyx-v2")).toBe(true);
    expect(v2).not.toContain("@nyx/electron");
  });
});

describe("applyAppIdentity pins the v2 identity in code", () => {
  test("sets name + AUMID + an explicit nyx-v2 userData (never @nyx/electron)", () => {
    const app = fakeApp("/home/u/.config");
    const userData = applyAppIdentity(app, "/");

    expect(app.applied.name).toBe("nyx-v2");
    expect(app.applied.aumid).toBe("com.netsirk.nyxv2");
    // userData was pinned EXPLICITLY (so it can never fall through to a name-derived
    // @nyx/electron) to the nyx-v2 dir.
    expect(app.applied.userData).toBe("/home/u/.config/nyx-v2");
    expect(userData).toBe("/home/u/.config/nyx-v2");
    expect(userData).not.toContain("@nyx/electron");
  });
});

describe("resolveDataDir falls back to the pinned nyx-v2 userData when NYX_DATA_DIR is unset", () => {
  // Simulate the real boot: main pinned userData via applyAppIdentity, then the sidecar
  // data-dir resolver runs.
  const pinnedUserData = deriveUserDataDir("/home/u/.config", "/"); // /home/u/.config/nyx-v2

  test("UNSET NYX_DATA_DIR -> the nyx-v2 userData, NOT @nyx/electron", () => {
    const resolved = resolveDataDirFrom(undefined, pinnedUserData);
    expect(resolved).toBe("/home/u/.config/nyx-v2");
    expect(resolved).not.toContain("@nyx/electron");
    expect(resolved.endsWith("/nyx-v2")).toBe(true);
  });

  test("EMPTY NYX_DATA_DIR -> still the nyx-v2 userData (empty override ignored)", () => {
    expect(resolveDataDirFrom("", pinnedUserData)).toBe("/home/u/.config/nyx-v2");
  });

  test("SET NYX_DATA_DIR -> the explicit override wins (dev/test pin)", () => {
    expect(resolveDataDirFrom("/tmp/throwaway-nyx", pinnedUserData)).toBe("/tmp/throwaway-nyx");
  });
});
