import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { IDENTITY_COLOR_COUNT } from "@/components/panes/pane-groups";
import {
  projectAccentColor,
  ProjectMark,
  projectIconFor,
  projectImageSrc,
  projectInitial,
  PROJECT_ACCENTS,
  PROJECT_ICONS,
} from "./project-mark";

/**
 * The project MARK — the square that says which project without a line of text. Identity is
 * CHOSEN (accent + icon OR image), all three optional, and the ABSENCE is a first-class case: a
 * project nobody dressed renders its initial on the neutral tile. These tests pin that repli and
 * the fall-through order (image → icon → initial), including the two ways a chosen source can fail
 * to render — an unknown icon token and a picture that will not load.
 */

/** A bare project — the shape `<ProjectMark>` reads, with no identity at all. */
function bare(name: string) {
  return { name, accent: null, icon: null, image_path: null };
}

describe("project identity resolvers", () => {
  it("takes the uppercase initial, and stays total on a blank name", () => {
    expect(projectInitial("nyx")).toBe("N");
    expect(projectInitial("  depot")).toBe("D");
    expect(projectInitial("   ")).toBe("?");
    expect(projectInitial("")).toBe("?");
  });

  it("resolves a known icon token and MISSES an unknown / absent one", () => {
    expect(projectIconFor("rocket")).toBe(PROJECT_ICONS.rocket);
    expect(projectIconFor("not-a-token")).toBeUndefined();
    expect(projectIconFor(null)).toBeUndefined();
    expect(projectIconFor(undefined)).toBeUndefined();
    expect(projectIconFor("")).toBeUndefined();
  });

  it("resolves EVERY token of the registry to a component — the grid can render all of them", () => {
    const tokens = Object.keys(PROJECT_ICONS);
    // Twenty-eight, four rows of seven: the ceiling the prototype arbitrated. Beyond it the grid
    // stops being a choice at a glance and becomes a catalogue that would need a filter.
    expect(tokens).toHaveLength(28);
    expect(new Set(tokens).size).toBe(28);
    for (const token of tokens) {
      expect(projectIconFor(token), `${token} must resolve`).toBeDefined();
    }
    // The thirteen that predate the widening are still there, still under their own names: a
    // project already wearing one keeps exactly its glyph.
    for (const kept of [
      "box",
      "circle",
      "code",
      "compass",
      "flask",
      "folder",
      "globe",
      "layers",
      "rocket",
      "server",
      "sparkles",
      "terminal",
      "wrench",
    ]) {
      expect(tokens).toContain(kept);
    }
  });

  /**
   * THE ACCENT IS A TOKEN NAME, NOT A COLOUR (ADR-0012) — and the fallback to a raw colour is not
   * politeness: the MCP `set_project_settings` schema describes `accent` without a format, so an
   * agent legitimately writes a hex, and the core has always stored the string opaquely.
   */
  it("resolves a palette token, and passes anything else through as a raw CSS colour", () => {
    expect(projectAccentColor("identity-3")).toBe("var(--identity-3)");
    expect(projectAccentColor(`identity-${IDENTITY_COLOR_COUNT}`)).toBe(
      `var(--identity-${IDENTITY_COLOR_COUNT})`,
    );
    // An agent's hex, a CSS colour, a bare name: untouched, exactly as before the rename.
    expect(projectAccentColor("#4f46e5")).toBe("#4f46e5");
    expect(projectAccentColor("oklch(0.76 0.09 152)")).toBe("oklch(0.76 0.09 152)");
    expect(projectAccentColor("rebeccapurple")).toBe("rebeccapurple");
  });

  it("treats identity-0 and an index past the palette as NO accent at all", () => {
    // `identity-0` is the render-side neutral. The UI never writes it — its first swatch ERASES —
    // but an agent could, and it must then render exactly what the ABSENCE renders rather than a
    // grey wash that is almost, but not quite, the neutral tile.
    expect(projectAccentColor("identity-0")).toBeUndefined();
    // A token the stylesheet does not declare would paint `var(--identity-99)`, i.e. nothing.
    expect(projectAccentColor(`identity-${IDENTITY_COLOR_COUNT + 1}`)).toBeUndefined();
    expect(projectAccentColor(null)).toBeUndefined();
    expect(projectAccentColor("   ")).toBeUndefined();
  });

  it("offers the picker ONE list, the ERASER first — spelled null, the value that is stored", () => {
    // THE ABSENCE HAS ONE SPELLING. The first swatch is `null`, not `"identity-0"`: a picker that
    // maps over this list and hands each entry to `setProjectAccent` erases on the first one, and
    // the second spelling the PRD forbids is not even reachable from the catalogue. Nothing on
    // screen could have caught it — `identity-0` renders EXACTLY like the neutral (spec below).
    expect(PROJECT_ACCENTS[0]).toBeNull();
    expect(PROJECT_ACCENTS).not.toContain("identity-0");
    expect(PROJECT_ACCENTS).toHaveLength(IDENTITY_COLOR_COUNT + 1);
    // Every offered accent but the first resolves to a token the stylesheet declares; the first
    // resolves to nothing, because it IS the absence of a choice.
    expect(projectAccentColor(PROJECT_ACCENTS[0])).toBeUndefined();
    for (const accent of PROJECT_ACCENTS.slice(1)) {
      expect(projectAccentColor(accent), `${accent} must resolve`).toBe(`var(--${accent})`);
    }
  });

  /**
   * THE CHANNEL, pinned from the renderer's side — the half of finding
   * `01M1VKT3EBFE861DQSHBM885EV` that jsdom CAN prove.
   *
   * What jsdom cannot prove is that the picture APPEARS: its resource loader never fetches an
   * `<img>`, so no spec in this file has ever loaded one and none can. What is provable, and is
   * the exact thing that was wrong, is the URL: it used to be `file://…`, which a page served over
   * `http://localhost` — the development shell, `window.ts`'s `loadURL` — may not load at all.
   *
   * The scheme and host below are LITERALS on purpose. They are the contract with
   * `apps/desktop/src/main/project-image.ts`, which the renderer cannot import (separate TS
   * project, no path into `apps/desktop/src`), and which pins the same two literals in
   * `project-image.test.ts`. Spelling them out on both sides is what makes a one-sided rename red.
   */
  it("names the copy in the app's own CHANNEL — never a `file://` a dev renderer cannot load", () => {
    // The shape the backend stores, on both platforms: only the NAME crosses, so the renderer
    // never carries a directory and the channel never receives one.
    expect(projectImageSrc("D:\\data\\nyx-v2\\project-icons\\p1~mfl1a2b3.png")).toBe(
      "nyx-image://project-icons/p1~mfl1a2b3.png",
    );
    expect(projectImageSrc("/home/me/.config/nyx-v2/project-icons/p1~mfl1a2b3.svg")).toBe(
      "nyx-image://project-icons/p1~mfl1a2b3.svg",
    );
    // NOT a `file://` URL any more — the whole defect in one assertion.
    expect(projectImageSrc("/home/me/logo.png")).not.toContain("file:");
    // An already-schemed value is still handed over untouched: `image_path` is a column an MCP
    // agent writes, and an agent's `https://` / `data:` picture kept working throughout.
    expect(projectImageSrc("https://cdn.example/logo.png")).toBe("https://cdn.example/logo.png");
    expect(projectImageSrc("data:image/png;base64,AAA")).toBe("data:image/png;base64,AAA");
    // A name with characters a URL would mangle is ENCODED, not concatenated.
    expect(projectImageSrc("/icons/my logo+v2.png")).toBe(
      "nyx-image://project-icons/my%20logo%2Bv2.png",
    );
    expect(projectImageSrc(null)).toBeUndefined();
    expect(projectImageSrc("   ")).toBeUndefined();
  });
});

describe("<ProjectMark>", () => {
  it("THE REPLI: no identity renders the initial on the neutral tile, without error", () => {
    const { container } = render(<ProjectMark project={bare("nyx")} />);
    const tile = container.firstElementChild as HTMLElement;
    expect(tile.textContent).toBe("N");
    expect(tile.className).toContain("bg-muted");
    expect(tile.querySelector("img")).toBeNull();
    // Nothing inline: the neutral tile is token classes only.
    expect(tile.getAttribute("style")).toBeNull();
  });

  it("a chosen icon replaces the initial", () => {
    const { container } = render(<ProjectMark project={{ ...bare("nyx"), icon: "rocket" }} />);
    const tile = container.firstElementChild as HTMLElement;
    expect(tile.textContent).toBe("");
    expect(tile.querySelector("svg")).not.toBeNull();
  });

  it("an UNKNOWN icon token falls back to the initial rather than an empty tile", () => {
    const { container } = render(
      <ProjectMark project={{ ...bare("depot"), icon: "not-a-token" }} />,
    );
    expect((container.firstElementChild as HTMLElement).textContent).toBe("D");
  });

  it("a chosen image outranks the icon, and a failed load degrades to it", () => {
    const { container } = render(
      <ProjectMark
        project={{ ...bare("nyx"), icon: "rocket", image_path: "https://cdn.example/logo.png" }}
      />,
    );
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    expect(img!.getAttribute("src")).toBe("https://cdn.example/logo.png");
    // The picture cannot load (moved / unreadable): the tile falls back to the icon, never a
    // broken-image square.
    fireEvent.error(img!);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("svg")).not.toBeNull();
  });

  it("a REPLACEMENT of the same format repaints, and un-blacklists a source that had failed", () => {
    // The two paths are what the main process now hands back for ONE project across two adoptions
    // — `<projectId>~<revision>.<ext>`, revision minted in `project-image.ts`. Before it existed,
    // replacing a `.png` by a `.png` produced the same string twice: this tile never repainted,
    // and once it had recorded that source as unloadable it stayed on the icon for the rest of the
    // session, however many valid pictures the user then chose. The remembered SOURCE was always
    // the right design; what it needed was a writer that stops lying about the picture being new.
    const before = "/data/project-icons/p1~mfl1a2b3.png";
    const after = "/data/project-icons/p1~mfl9z8y7.png";
    const { container, rerender } = render(
      <ProjectMark project={{ ...bare("nyx"), icon: "rocket", image_path: before }} />,
    );
    expect(container.querySelector("img")!.getAttribute("src")).toBe(
      "nyx-image://project-icons/p1~mfl1a2b3.png",
    );
    fireEvent.error(container.querySelector("img")!);
    expect(container.querySelector("img")).toBeNull();

    rerender(<ProjectMark project={{ ...bare("nyx"), icon: "rocket", image_path: after }} />);
    const repainted = container.querySelector("img");
    expect(repainted).not.toBeNull();
    // The revision moved, so the CHANNEL URL moved with it — which is the whole point: the name is
    // what crosses, so a writer that reused it would leave this reading the previous one.
    expect(repainted!.getAttribute("src")).toBe("nyx-image://project-icons/p1~mfl9z8y7.png");
  });

  /**
   * THE MARK IS A DRAWING BY DEFAULT, AND A NAME ON DEMAND. Inside a control that already names the
   * project (the rail's tile button, the home header's `<h1>`) a second name is noise for a reader,
   * so the tile is `aria-hidden`. `labelled` is for the one case where this square is the ONLY
   * thing saying whose item a row is — the corner mark of a cross-project row — and it lives here
   * rather than in a wrapper each surface writes for itself.
   */
  it("is aria-hidden by default — the surrounding control carries the name", () => {
    const { container } = render(<ProjectMark project={bare("nyx")} />);
    const tile = container.firstElementChild as HTMLElement;
    expect(tile.getAttribute("aria-hidden")).toBe("true");
    expect(tile.getAttribute("role")).toBeNull();
    expect(tile.getAttribute("title")).toBeNull();
  });

  it("`labelled` NAMES the project on the mark itself, without writing it as text", () => {
    const { container } = render(<ProjectMark project={bare("nyx")} labelled />);
    const tile = container.firstElementChild as HTMLElement;
    expect(tile.getAttribute("role")).toBe("img");
    expect(tile.getAttribute("title")).toBe("nyx");
    expect(tile.getAttribute("aria-label")).toBe("Project nyx");
    expect(tile.getAttribute("aria-hidden")).toBeNull();
    // The name is NOT written on the tile: it still draws the identity, here the initial.
    expect(tile.textContent).toBe("N");
  });

  it("a chosen accent tints the tile instead of the neutral token classes", () => {
    const { container } = render(<ProjectMark project={{ ...bare("nyx"), accent: "#4f46e5" }} />);
    const tile = container.firstElementChild as HTMLElement;
    expect(tile.className).not.toContain("bg-muted");
    // The CSSOM normalises the hex it accepted; what matters is that the CHOSEN colour drives
    // both the wash and the foreground.
    const style = tile.getAttribute("style") ?? "";
    expect(style).toContain("color-mix");
    expect(style).toContain("rgb(79, 70, 229)");
    // The identity is a colour only — the initial still carries the name.
    expect(tile.textContent).toBe("N");
  });

  it("a palette TOKEN tints the tile through the design system, not through a stored colour", () => {
    const { container } = render(
      <ProjectMark project={{ ...bare("nyx"), accent: "identity-3" }} />,
    );
    const tile = container.firstElementChild as HTMLElement;
    expect(tile.className).not.toContain("bg-muted");
    const style = tile.getAttribute("style") ?? "";
    // The token drives BOTH halves — the wash and the foreground — and it goes through
    // `var(--identity-3)`, so the tile follows the theme instead of a colour frozen in the row.
    expect(style).toContain("var(--identity-3)");
    expect(style).toContain("color-mix");
    // Mixed `in oklch`, like every other surface of this palette — a project tile and a group
    // pastille of the same token are then literally comparable.
    expect(style).toContain("in oklch");
    expect(tile.textContent).toBe("N");
  });

  it("identity-0 from an agent renders EXACTLY what no accent renders", () => {
    const { container: neutral } = render(<ProjectMark project={bare("nyx")} />);
    const { container: zero } = render(
      <ProjectMark project={{ ...bare("nyx"), accent: "identity-0" }} />,
    );
    const a = neutral.firstElementChild as HTMLElement;
    const b = zero.firstElementChild as HTMLElement;
    expect(b.className).toBe(a.className);
    expect(b.getAttribute("style")).toBe(a.getAttribute("style"));
    expect(b.className).toContain("bg-muted");
  });

  it("an unintelligible accent leaves no colour behind — no exception, no broken square", () => {
    // Not a token and not a colour: the style is built, the CSSOM drops what it cannot parse, and
    // the tile still draws the project's initial. The render must not throw.
    const { container } = render(
      <ProjectMark project={{ ...bare("nyx"), accent: "not-a-colour-at-all" }} />,
    );
    const tile = container.firstElementChild as HTMLElement;
    expect(tile.getAttribute("style") ?? "").not.toContain("not-a-colour-at-all");
    expect(tile.textContent).toBe("N");
    expect(tile.querySelector("img")).toBeNull();
  });
});
