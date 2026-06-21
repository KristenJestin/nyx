import { describe, expect, it } from "vitest";

import { basename, relativeToWorkspace } from "./folder-picker";

describe("basename", () => {
  it("returns the last segment of a POSIX path", () => {
    expect(basename("/home/kris/my-app")).toBe("my-app");
    expect(basename("/home/kris/my-app/")).toBe("my-app");
  });

  it("returns the last segment of a Windows path", () => {
    expect(basename("C:\\Users\\kris\\my-app")).toBe("my-app");
  });

  it("returns empty for a filesystem root (no segment)", () => {
    expect(basename("/")).toBe("");
  });
});

describe("relativeToWorkspace", () => {
  it("returns the relative path for a folder strictly inside the workspace", () => {
    expect(relativeToWorkspace("/home/kris/repo", "/home/kris/repo/packages/api")).toBe(
      "packages/api",
    );
  });

  it("returns '' (workspace root) when the picked folder IS the workspace", () => {
    expect(relativeToWorkspace("/home/kris/repo", "/home/kris/repo")).toBe("");
    // Trailing separators do not change the answer.
    expect(relativeToWorkspace("/home/kris/repo/", "/home/kris/repo")).toBe("");
  });

  it("relativizes a one-level subfolder", () => {
    expect(relativeToWorkspace("/home/kris/repo", "/home/kris/repo/web")).toBe("web");
  });

  it("returns null when the picked folder is OUTSIDE the workspace (sibling)", () => {
    expect(relativeToWorkspace("/home/kris/repo", "/home/kris/other")).toBeNull();
  });

  it("returns null when the picked folder is an ANCESTOR of the workspace", () => {
    expect(relativeToWorkspace("/home/kris/repo", "/home/kris")).toBeNull();
  });

  it("returns null when the picked folder is on a different root/drive", () => {
    expect(relativeToWorkspace("/home/kris/repo", "/var/tmp/repo/api")).toBeNull();
  });

  it("does NOT match a sibling whose name shares a prefix segment", () => {
    // `/home/kris/repo-2` must NOT be treated as inside `/home/kris/repo`.
    expect(relativeToWorkspace("/home/kris/repo", "/home/kris/repo-2/api")).toBeNull();
  });

  it("matches across separator styles (Windows workspace, POSIX-ish pick)", () => {
    expect(
      relativeToWorkspace("C:\\Users\\kris\\repo", "C:\\Users\\kris\\repo\\packages\\api"),
    ).toBe("packages/api");
  });

  it("matches a Windows pick case-INSENSITIVELY against a lowercased workspace path", () => {
    // The backend stores the workspace path lowercased (pathnorm) while the native
    // picker returns OS casing; on Windows the compare must fold case or every pick
    // is wrongly rejected at the drive letter (`C:` vs `c:`).
    expect(
      relativeToWorkspace("c:\\users\\kris\\repo", "C:\\Users\\Kris\\repo\\packages\\api"),
    ).toBe("packages/api");
    expect(relativeToWorkspace("c:\\users\\kris\\repo", "C:\\Users\\Kris\\repo")).toBe("");
  });

  it("keeps POSIX comparisons case-SENSITIVE (case-distinct dirs are different folders)", () => {
    // No case folding on `/`-only paths: `/home/kris/Repo` and `/home/kris/repo`
    // are genuinely different directories on a case-sensitive filesystem.
    expect(relativeToWorkspace("/home/kris/Repo", "/home/kris/repo/api")).toBeNull();
  });
});
