/**
 * PURE derivation tests for the sidecar BINARY / RESOURCE path resolution (review
 * `01KWD96WAD7MAPWM9Y5S56G138`, finding `01KWD96X29WJN4G00E8SJ88CK6`).
 *
 * The packaging bug these tests now CATCH: `deriveSidecarBin` joined `<dirname>/../sidecar/<exe>`
 * and string-replaced `app.asar/` → `app.asar.unpacked/` but NEVER normalized the `..`. In the
 * installed app the bundled main runs from `…/app.asar/dist/main`, so the derived path was
 * `…/app.asar.unpacked/dist/main/../sidecar/nyx-sidecar`. Under `app.asar.unpacked/` ONLY
 * `dist/sidecar/**` and `dist/resources/**` are unpacked — `dist/main` does NOT exist there —
 * so the OS could not traverse `dist/main/..` (ENOENT) and the sidecar "binary not found",
 * making the packaged app unusable without the `NYX_SIDECAR_BIN` override.
 *
 * The OLD test (had there been one) only asserted the asar→unpacked string replace, which the
 * buggy code DID do — so it passed while the app was broken. These tests assert the STRONGER
 * invariant: the returned path contains NO `..` segment, NO `dist/main` segment, and EQUALS the
 * canonical `…/app.asar.unpacked/dist/sidecar/<exe>` — for BOTH the POSIX `/` and Windows `\`
 * cases. They are PURE (no Electron, no disk); the `electron` import is stubbed so importing
 * `sidecar.ts` is cheap.
 *
 * Run via `bun test` (the `smoke:sidecar-derivation` npm script).
 */
import { describe, expect, mock, test } from "bun:test";

// Stub `electron` so importing `sidecar.ts` (which `import`s `app`) does not need the real
// runtime — these tests exercise only the PURE derivation helpers, which take their inputs
// explicitly and never read the real `app`.
mock.module("electron", () => ({ app: { getPath: () => "/should/not/be/read" } }));
const { deriveSidecarBin, deriveResourceDir, sidecarExeName } = await import("./sidecar");

describe("sidecarExeName picks the per-OS executable name", () => {
  test("POSIX → bare name, Windows → .exe", () => {
    expect(sidecarExeName("linux")).toBe("nyx-sidecar");
    expect(sidecarExeName("darwin")).toBe("nyx-sidecar");
    expect(sidecarExeName("win32")).toBe("nyx-sidecar.exe");
  });
});

describe("deriveSidecarBin packaged: redirects into app.asar.unpacked AND normalizes the `..`", () => {
  // The dir the bundled main runs from in the INSTALLED app: inside the asar, under dist/main.
  const posixMainDir = "/opt/nyx-v2/resources/app.asar/dist/main";

  test("POSIX: resolves to app.asar.unpacked/dist/sidecar/<exe> with NO `..` and NO dist/main", () => {
    const bin = deriveSidecarBin(posixMainDir, true, "nyx-sidecar", undefined, "/");
    // (a) the `..` from the `<dirname>/../sidecar` join is COLLAPSED — this is the bug fix.
    expect(bin).not.toContain("..");
    // (b) the un-unpacked `dist/main` segment is GONE (it does not exist under the unpacked layout).
    expect(bin).not.toContain("/dist/main");
    expect(bin).not.toContain("dist/main");
    // (c) the path is EXACTLY the real on-disk unpacked binary.
    expect(bin).toBe("/opt/nyx-v2/resources/app.asar.unpacked/dist/sidecar/nyx-sidecar");
    // …and it really did redirect OUTSIDE the asar (a native binary can't spawn from the archive).
    expect(bin).toContain("app.asar.unpacked/");
    expect(bin).not.toContain("app.asar/dist");
  });

  test("Windows-on-POSIX: same invariants with the `\\` separator (cross-OS branch)", () => {
    const winMainDir = "C:\\Program Files\\nyx-v2\\resources\\app.asar\\dist\\main";
    const bin = deriveSidecarBin(winMainDir, true, "nyx-sidecar.exe", undefined, "\\");
    // (a) NO `..` segment after normalization (collapsed honouring the injected `\\`).
    expect(bin).not.toContain("..");
    // (b) NO `dist\\main` segment.
    expect(bin).not.toContain("dist\\main");
    // (c) EXACTLY the canonical unpacked Windows path.
    expect(bin).toBe(
      "C:\\Program Files\\nyx-v2\\resources\\app.asar.unpacked\\dist\\sidecar\\nyx-sidecar.exe",
    );
    expect(bin).toContain("app.asar.unpacked\\");
    expect(bin).not.toContain("app.asar\\dist");
  });

  test("the normalized path has no `..` even though the join produced one (regression guard)", () => {
    // Prove the join WOULD have produced a `..` (the source of the bug) but the RETURN does not.
    const raw = [posixMainDir, "..", "sidecar", "nyx-sidecar"].join("/");
    expect(raw).toContain("/..");
    const bin = deriveSidecarBin(posixMainDir, true, "nyx-sidecar", undefined, "/");
    expect(bin).not.toContain("/..");
  });
});

describe("deriveSidecarBin override + dev branches keep working", () => {
  test("override (NYX_SIDECAR_BIN) wins unconditionally and verbatim", () => {
    const override = "/home/u/code/nyx/target/debug/nyx-sidecar";
    expect(deriveSidecarBin("/anything", true, "nyx-sidecar", override, "/")).toBe(override);
    // An override that itself contains `..` is returned untouched (caller owns it).
    const rel = "../target/debug/nyx-sidecar";
    expect(deriveSidecarBin("/anything", false, "nyx-sidecar", rel, "/")).toBe(rel);
  });

  test("dev (isPackaged=false): dist/sidecar next to the bundled main, normalized", () => {
    // Dev staging: bundled main at <repo>/apps/desktop/dist/main → sibling dist/sidecar.
    const devMainDir = "/repo/apps/desktop/dist/main";
    const bin = deriveSidecarBin(devMainDir, false, "nyx-sidecar", undefined, "/");
    expect(bin).not.toContain("..");
    expect(bin).toBe("/repo/apps/desktop/dist/sidecar/nyx-sidecar");
  });
});

describe("deriveResourceDir derives from resourcesPath directly (no `..`, no normalization bug)", () => {
  // Confirms the SAME class of bug does NOT exist in deriveResourceDir: it joins
  // resourcesPath + app.asar.unpacked + dist with no `..` to collapse.
  test("packaged: <resources>/app.asar.unpacked/dist, clean (no `..`)", () => {
    const dir = deriveResourceDir(true, "/opt/nyx-v2/resources", "/");
    expect(dir).toBe("/opt/nyx-v2/resources/app.asar.unpacked/dist");
    expect(dir).not.toContain("..");
    expect(dir).not.toContain("dist/main");
  });

  test("Windows packaged: same, with `\\`, clean", () => {
    const dir = deriveResourceDir(true, "C:\\app\\resources", "\\");
    expect(dir).toBe("C:\\app\\resources\\app.asar.unpacked\\dist");
    expect(dir).not.toContain("..");
  });

  test("dev (isPackaged=false): null (sidecar falls back to its source tree)", () => {
    expect(deriveResourceDir(false, "/whatever", "/")).toBeNull();
  });
});
