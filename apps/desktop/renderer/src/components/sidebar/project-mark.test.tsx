import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  ProjectMark,
  projectIconFor,
  projectImageSrc,
  projectInitial,
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

  it("keeps an already-schemed image source and wraps a bare absolute path", () => {
    expect(projectImageSrc("https://cdn.example/logo.png")).toBe("https://cdn.example/logo.png");
    expect(projectImageSrc("data:image/png;base64,AAA")).toBe("data:image/png;base64,AAA");
    expect(projectImageSrc("file:///D:/logo.png")).toBe("file:///D:/logo.png");
    expect(projectImageSrc("D:\\logo.png")).toBe("file:///D:/logo.png");
    expect(projectImageSrc("/home/me/logo.png")).toBe("file:///home/me/logo.png");
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
});
