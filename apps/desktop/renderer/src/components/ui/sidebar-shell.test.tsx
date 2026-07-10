import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SidebarShell } from "./sidebar-shell";

describe("<SidebarShell> (shared left sidebar chassis)", () => {
  it("renders as a LEFT-bordered, fixed-width, full-height rail (terminal-sidebar geometry)", () => {
    render(
      <SidebarShell aria-label="Rail">
        <p>content</p>
      </SidebarShell>,
    );
    const aside = screen.getByRole("complementary", { name: "Rail" });
    // Same shell as the terminal sidebar: fixed 260px width, left position (right border),
    // full height, muted surface.
    expect(aside.className).toContain("w-[260px]");
    expect(aside.className).toContain("border-r");
    expect(aside.className).toContain("h-svh");
    expect(aside.className).toContain("bg-muted/40");
    expect(aside.className).toContain("shrink-0");
    // No left border (it is a LEFT rail, not a right one).
    expect(aside.className).not.toContain("border-l");
  });

  it("puts the header in a bordered non-scrolling block and children in a scrollable region", () => {
    render(
      <SidebarShell aria-label="Rail" header={<span>Project name</span>}>
        <p>scrollable body</p>
      </SidebarShell>,
    );
    const header = screen.getByText("Project name").parentElement!;
    expect(header.className).toContain("border-b");
    const body = screen.getByText("scrollable body").parentElement!;
    expect(body.className).toContain("overflow-y-auto");
    expect(body.className).toContain("flex-1");
  });

  it("omits the header block entirely when no header is passed (spec detail rail)", () => {
    render(
      <SidebarShell aria-label="Rail">
        <p>only body</p>
      </SidebarShell>,
    );
    const aside = screen.getByRole("complementary", { name: "Rail" });
    // The only child is the scrollable content region — no header border block.
    expect(aside.querySelector(".border-b")).toBeNull();
  });
});
