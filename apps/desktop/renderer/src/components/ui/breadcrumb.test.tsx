import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Breadcrumb, BreadcrumbCurrent } from "./breadcrumb";

/**
 * FINDING 47 — the SHARED breadcrumb chassis the three Work headers (board / Spec detail / review)
 * now render, replacing the board's bare `<h1>`, the Spec detail's hand-rolled nav and the review's
 * lone `←` button. jsdom guards the STRUCTURE the harmonisation depends on: a labelled `nav`, one
 * `/` separator woven BETWEEN crumbs (never leading/trailing), and an `aria-current` last crumb.
 * (The links themselves are plain `<Link>`s at each call site — driven by the CDP gate.)
 */
describe("<Breadcrumb> (shared Work breadcrumb — Finding 47)", () => {
  it("is a labelled nav and marks the current (last) crumb aria-current", () => {
    render(
      <Breadcrumb>
        <a href="/work">Work</a>
        <BreadcrumbCurrent mono>NYX-3</BreadcrumbCurrent>
      </Breadcrumb>,
    );
    const nav = screen.getByRole("navigation", { name: "Breadcrumb" });
    expect(nav).toBeTruthy();
    expect(nav.getAttribute("data-testid")).toBe("breadcrumb");
    const current = screen.getByText("NYX-3");
    expect(current.getAttribute("aria-current")).toBe("page");
    expect(current.className).toContain("font-mono");
  });

  it("weaves ONE separator between crumbs (none leading, none trailing)", () => {
    const { container } = render(
      <Breadcrumb>
        <a href="/work">Work</a>
        <a href="/work/x">NYX-3</a>
        <BreadcrumbCurrent>Review</BreadcrumbCurrent>
      </Breadcrumb>,
    );
    // Three crumbs → exactly two `/` separators (n-1).
    const separators = Array.from(container.querySelectorAll("span[aria-hidden]")).filter(
      (s) => (s.textContent || "").trim() === "/",
    );
    expect(separators).toHaveLength(2);
  });

  it("renders a single crumb with no separator (the board's `Work`)", () => {
    const { container } = render(
      <Breadcrumb>
        <BreadcrumbCurrent>Work</BreadcrumbCurrent>
      </Breadcrumb>,
    );
    const separators = Array.from(container.querySelectorAll("span[aria-hidden]")).filter(
      (s) => (s.textContent || "").trim() === "/",
    );
    expect(separators).toHaveLength(0);
    expect(screen.getByText("Work").getAttribute("aria-current")).toBe("page");
  });
});
