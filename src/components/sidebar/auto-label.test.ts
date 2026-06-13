import { describe, expect, it } from "vitest";

import {
  autoLabel,
  isShellComm,
  resolveDisplayName,
} from "./auto-label";

describe("autoLabel", () => {
  it("uses the basename of the cwd when only the shell is in the foreground", () => {
    // /home/x/projetA → projetA; foreground is the shell → no program suffix.
    expect(autoLabel("/home/x/projetA", "bash")).toBe("projetA");
    expect(autoLabel("/home/x/projetA", "zsh")).toBe("projetA");
    expect(autoLabel("/home/x/projetA", null)).toBe("projetA");
  });

  it("appends the foreground program when it is NOT the shell", () => {
    // Running htop → reflected in the label.
    expect(autoLabel("/home/x/projetA", "htop")).toBe("projetA · htop");
    expect(autoLabel("/srv/api", "node")).toBe("api · node");
  });

  it("handles a trailing-slash cwd and the filesystem root", () => {
    expect(autoLabel("/home/x/projetA/", "bash")).toBe("projetA");
    // Root has no basename; fall back to the program (or the raw path).
    expect(autoLabel("/", "htop")).toBe("htop");
  });

  it("returns null when there is nothing to name (no cwd, no program)", () => {
    expect(autoLabel(null, null)).toBeNull();
    expect(autoLabel("", null)).toBeNull();
  });

  it("names by program alone when the cwd is unusable", () => {
    expect(autoLabel(null, "vim")).toBe("vim");
  });
});

describe("isShellComm", () => {
  it("recognises the common login shells", () => {
    for (const s of ["bash", "zsh", "sh", "fish", "-bash", "-zsh"]) {
      expect(isShellComm(s)).toBe(true);
    }
  });
  it("treats real programs as non-shell", () => {
    for (const p of ["htop", "node", "vim", "claude"]) {
      expect(isShellComm(p)).toBe(false);
    }
  });
});

describe("resolveDisplayName (precedence)", () => {
  const base = {
    id: "1",
    cwd: "/home/x/projetA",
    label: null as string | null,
    scrollback: "",
    status: "alive" as const,
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };

  it("MANUAL label wins over the auto label", () => {
    const rec = { ...base, label: "my-custom-name" };
    // Even with a live auto-name available, the manual override takes priority.
    expect(resolveDisplayName(rec, 0, "projetA · htop")).toBe("my-custom-name");
  });

  it("falls back to the auto label when there is no manual label", () => {
    expect(resolveDisplayName(base, 0, "projetA · htop")).toBe("projetA · htop");
  });

  it("falls back to the cwd basename when there is no manual or auto label", () => {
    expect(resolveDisplayName(base, 0, null)).toBe("projetA");
  });

  it("falls back to a numbered name when nothing else is usable", () => {
    const rec = { ...base, cwd: "" };
    expect(resolveDisplayName(rec, 4, null)).toBe("Terminal 5");
  });

  it("ignores a whitespace-only manual label (treats it as unset)", () => {
    const rec = { ...base, label: "   " };
    expect(resolveDisplayName(rec, 0, "projetA · node")).toBe("projetA · node");
  });
});
