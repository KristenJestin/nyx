import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";

import { SpecLinks } from "./spec-links";

/** One `annexe_link_list` row (the resolved `url` annexe the block renders). */
function link(id: string, title: string, url: string, created_at: number) {
  return { id, spec_id: "s1", title, url, created_at };
}

/** Fill one of the block's two fields. */
function fill(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } });
}

describe("SpecLinks", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("lists the Spec's links with their label, in the read model's order, keyed on the Spec", async () => {
    const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "annexe_link_list") {
        // The backend order is newest-first; the block renders it as given (PRD "ORDRE": no
        // re-sorting, no `position`).
        return [
          link("a2", "The PR", "https://github.test/o/r/pull/7", 20),
          link("a1", "https://jira.test/BROWSE-1", "https://jira.test/BROWSE-1", 10),
        ];
      }
      return undefined;
    });

    render(<SpecLinks specId="s1" />);

    // A labelled link reads as its LABEL; one stored WITHOUT a label carries the URL as its title
    // (the backend's fallback) and still reads as a link.
    expect(await screen.findByRole("link", { name: /The PR/ })).toBeTruthy();
    expect(screen.getByRole("link", { name: /BROWSE-1/ })).toBeTruthy();

    // Rendered in the order the read model returned them.
    const rendered = screen.getAllByRole("link").map((a) => a.textContent);
    expect(rendered[0]).toContain("The PR");
    expect(rendered[1]).toContain("BROWSE-1");

    const listCall = calls.find((c) => c.cmd === "annexe_link_list");
    expect(listCall?.args).toMatchObject({ specId: "s1" });
  });

  it("opens a link in the EXTERNAL browser: a plain anchor the Electron shell hands to the OS", async () => {
    mockIPC((cmd) => {
      if (cmd === "annexe_link_list") {
        return [link("a1", "Design doc", "https://docs.test/design", 10)];
      }
      return undefined;
    });

    render(<SpecLinks specId="s1" />);

    const anchor = (await screen.findByRole("link", { name: /Design doc/ })) as HTMLAnchorElement;
    // The whole mechanism: `href` + `target="_blank"`. The shell's `setWindowOpenHandler`
    // (apps/desktop/src/main/window.ts) DENIES the popup and calls `shell.openExternal` for an
    // http(s) target, and `will-navigate` does the same for a same-frame navigation — so the URL
    // opens in the user's real browser and NEVER in a view of the app.
    expect(anchor.getAttribute("href")).toBe("https://docs.test/design");
    expect(anchor.getAttribute("target")).toBe("_blank");
    expect(anchor.getAttribute("rel")).toContain("noopener");
  });

  it("shows an explicit empty state — not a phantom section — when the Spec has no link", async () => {
    mockIPC((cmd) => (cmd === "annexe_link_list" ? [] : undefined));

    render(<SpecLinks specId="s1" />);

    expect(await screen.findByText(/No external link on this Spec yet/)).toBeTruthy();
    // The block itself is still there (heading + a zero count) — the emptiness is SAID.
    expect(screen.getByRole("heading", { name: "Links" })).toBeTruthy();
    expect(screen.queryAllByRole("link")).toHaveLength(0);
  });

  it("adds a link through `annexe_add` — the same command+authority the MCP tool calls", async () => {
    const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
    let rows: ReturnType<typeof link>[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "annexe_link_list") return rows;
      if (cmd === "annexe_add") {
        rows = [link("a1", "Ticket", "https://jira.test/BROWSE-1", 10)];
        return { annexe: { id: "a1" } };
      }
      return undefined;
    });

    render(<SpecLinks specId="s1" />);
    await screen.findByText(/No external link on this Spec yet/);

    fill("Link URL", "https://jira.test/BROWSE-1");
    fill("Link label", "Ticket");
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));

    await waitFor(() => {
      const add = calls.find((c) => c.cmd === "annexe_add");
      expect(add?.args).toMatchObject({
        specId: "s1",
        kind: "url",
        title: "Ticket",
        content: "https://jira.test/BROWSE-1",
      });
    });
    // The write pushes `specs://changed`; the block re-pulls and the new link appears.
    await emit("specs://changed");
    expect(await screen.findByRole("link", { name: /Ticket/ })).toBeTruthy();
  });

  it("SHOWS the write in flight — spinner + a distinct label — and returns to rest after it", async () => {
    // The write is held open on purpose: the in-flight state is what this test is about, and on a
    // resolved-immediately mock it would flash past. `release()` settles it.
    let release = () => {};
    const inFlight = new Promise<{ annexe: { id: string } }>((resolve) => {
      release = () => resolve({ annexe: { id: "a1" } });
    });
    mockIPC((cmd) => {
      if (cmd === "annexe_link_list") return [];
      if (cmd === "annexe_add") return inFlight;
      return undefined;
    });

    render(<SpecLinks specId="s1" />);
    await screen.findByText(/No external link on this Spec yet/);

    // AT REST: the resting label, and nothing claiming to be busy.
    const atRest = screen.getByRole("button", { name: "Add link" });
    expect(atRest.getAttribute("aria-busy")).toBe("false");
    expect(atRest.querySelector("svg.animate-spin")).toBeNull();

    fill("Link URL", "https://jira.test/BROWSE-1");
    fireEvent.click(atRest);

    // IN FLIGHT: a DISTINCT label (so the state is readable, not just greyed), the spinner
    // primitive drawn inside the button, the state ANNOUNCED via `aria-busy` — the greying alone
    // says nothing to a screen reader — and the double-submit gate still shut.
    const busy = await screen.findByRole("button", { name: "Adding…" });
    expect(busy.getAttribute("aria-busy")).toBe("true");
    expect((busy as HTMLButtonElement).disabled).toBe(true);
    expect(busy.querySelector("svg.animate-spin")).toBeTruthy();

    // BACK TO REST when the write settles — the button is not left spinning.
    release();
    const settled = await screen.findByRole("button", { name: "Add link" });
    expect(settled.getAttribute("aria-busy")).toBe("false");
    expect(settled.querySelector("svg.animate-spin")).toBeNull();
  });

  it("does NOT claim to be adding while a link is being REMOVED — the gate is not the shown state", async () => {
    // The write is held open so the in-flight window is observable. `useAnnexeActions` raises ONE
    // gate for every write; the button must show the ADD only.
    let release = () => {};
    const inFlight = new Promise<{ removed: boolean }>((resolve) => {
      release = () => resolve({ removed: true });
    });
    mockIPC((cmd) => {
      if (cmd === "annexe_link_list") return [link("a1", "Ticket", "https://jira.test/B-1", 10)];
      if (cmd === "annexe_remove") return inFlight;
      return undefined;
    });

    render(<SpecLinks specId="s1" />);
    // A URL is typed first, so the submit button's `disabled` is driven by the GATE alone and not
    // by the empty-URL guard.
    fill("Link URL", "https://docs.test/x");
    fireEvent.click(await screen.findByRole("button", { name: "Remove link Ticket" }));

    // GATED — a removal in flight still shuts the form (no add racing a detach) — but the label
    // and `aria-busy` keep saying what is REALLY happening: nothing is being added.
    const submit = screen.getByRole("button", { name: "Add link" });
    expect((submit as HTMLButtonElement).disabled).toBe(true);
    expect(submit.getAttribute("aria-busy")).toBe("false");
    expect(submit.querySelector("svg.animate-spin")).toBeNull();

    // …and the gate lifts when the removal settles.
    release();
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "Add link" }) as HTMLButtonElement).disabled).toBe(
        false,
      );
    });
  });

  it("sends an EMPTY title when no label is given, so the backend stores the URL as the label", async () => {
    const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "annexe_link_list") return [];
      if (cmd === "annexe_add") return { annexe: { id: "a1" } };
      return undefined;
    });

    render(<SpecLinks specId="s1" />);
    await screen.findByText(/No external link on this Spec yet/);

    fill("Link URL", "https://docs.test/x");
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));

    await waitFor(() => {
      const add = calls.find((c) => c.cmd === "annexe_add");
      expect(add?.args).toMatchObject({ title: "", content: "https://docs.test/x" });
    });
  });

  it("removes a link through `annexe_remove`, keyed on the annexe id", async () => {
    const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "annexe_link_list") return [link("a1", "Ticket", "https://jira.test/B-1", 10)];
      if (cmd === "annexe_remove") return { removed: true, id: "a1" };
      return undefined;
    });

    render(<SpecLinks specId="s1" />);

    fireEvent.click(await screen.findByRole("button", { name: "Remove link Ticket" }));

    await waitFor(() => {
      const removed = calls.find((c) => c.cmd === "annexe_remove");
      expect(removed?.args).toMatchObject({ id: "a1" });
    });
  });

  it("DISPLAYS the backend's refusal verbatim (a bad URL / an already-attached one)", async () => {
    mockIPC((cmd) => {
      if (cmd === "annexe_link_list") return [];
      if (cmd === "annexe_add") {
        throw new Error(
          "invalid url 'not a url': a url annexe must be an ABSOLUTE http:// or https:// URL",
        );
      }
      return undefined;
    });

    render(<SpecLinks specId="s1" />);
    await screen.findByText(/No external link on this Spec yet/);

    fill("Link URL", "not a url");
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));

    // The authority's own sentence, not a re-worded one — and the field KEEPS the value so the
    // user can fix it instead of retyping it.
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("ABSOLUTE http:// or https:// URL");
    expect((screen.getByLabelText("Link URL") as HTMLInputElement).value).toBe("not a url");
  });
});
