import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import type { AnnexListItem, SpecDetail } from "../types";
import { AnnexesTab } from "./annexes-tab";

/** A minimal `spec_detail` fixture — the Annexes tab only reads `detail.spec.id`. */
function detailFixture(): SpecDetail {
  return {
    spec: {
      id: "s1",
      project_id: "p1",
      workspace_id: "w1",
      title: "Spec",
      slug: null,
      number: 1,
      priority: "med",
      status: "in_progress",
      mode: "stepped",
      phases: "[]",
      current_revision_number: 1,
      position: 0,
      created_at: 0,
      updated_at: 0,
      completed_at: null,
      archived_at: null,
    },
    revision: {
      id: "r1",
      spec_id: "s1",
      revision_number: 1,
      revision_status: "active",
      title: "Spec",
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
    },
    tasks: [],
    dependencies: [],
    workspace_context: null,
  };
}

/** One `annexe_list` row (what `useAnnexes` serves the lower block). */
function annexe(id: string, kind: string, title: string, created_at: number): AnnexListItem {
  return {
    id,
    spec_id: "s1",
    kind,
    title,
    content_hash: `hash${id}beefcafe`,
    byte_len: 42,
    created_at,
  };
}

/** One `annexe_link_list` row (what `useSpecLinks` serves the Links block). */
function link(id: string, title: string, url: string, created_at: number) {
  return { id, spec_id: "s1", title, url, created_at };
}

/** Serve both reads of the tab: the annexes (every kind) and the resolved links. */
function serve(annexes: AnnexListItem[], links: ReturnType<typeof link>[]) {
  mockIPC((cmd) => {
    if (cmd === "annexe_list") return annexes;
    if (cmd === "annexe_link_list") return links;
    return undefined;
  });
}

/**
 * The "Documents" block (the lower one) — a `<section>` named by its heading. Named after what it
 * DRAWS since the second human review (F8): calling it "Annexes" made its zero read as a lie on a
 * Spec carrying links, since a link is an annexe too.
 */
const documentsBlock = () => screen.getByRole("region", { name: "Documents" });
/** The "Links" block (`<SpecLinks>`, the upper one). */
const linksBlock = () => screen.getByRole("region", { name: "Links" });

describe("AnnexesTab", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("shows a `url` annexe ONLY in Links and every other kind ONLY in Documents — never twice", async () => {
    serve(
      [
        annexe("a3", "url", "The PR", 30),
        annexe("a2", "prototype", "Mockup L1", 20),
        annexe("a1", "doc", "Design notes", 10),
      ],
      [link("a3", "The PR", "https://github.test/o/r/pull/7", 30)],
    );

    render(<AnnexesTab detail={detailFixture()} />);

    // The link is rendered ONCE, in the Links block, as a real anchor.
    const links = await within(linksBlock()).findByRole("link", { name: /The PR/ });
    expect(links.getAttribute("href")).toBe("https://github.test/o/r/pull/7");

    // …and NOT in the Documents block, which draws the same read model minus the url rows. This is
    // the component's own claim ("what is filtered here is displayed there"): an inverted predicate
    // would show the link twice, and this is what catches it.
    const documents = documentsBlock();
    expect(within(documents).queryByText("The PR")).toBeNull();
    expect(within(documents).getByText("Design notes")).toBeTruthy();
    expect(within(documents).getByText("Mockup L1")).toBeTruthy();
    expect(within(documents).getAllByRole("listitem")).toHaveLength(2);

    // The COUNTER of the Documents block excludes the url too — 3 rows read, 2 shown, 2 counted.
    expect(within(documents).getByText("2")).toBeTruthy();
    expect(within(linksBlock()).getByText("1")).toBeTruthy();
  });

  it("gives the Documents block its OWN empty state — a true zero — when the Spec has only links", async () => {
    serve(
      [annexe("a1", "url", "Ticket", 10)],
      [link("a1", "Ticket", "https://jira.test/BROWSE-1", 10)],
    );

    render(<AnnexesTab detail={detailFixture()} />);

    // The link is there…
    expect(await within(linksBlock()).findByRole("link", { name: /Ticket/ })).toBeTruthy();
    // …and the lower block says it is empty rather than listing the link a second time or
    // swallowing the whole tab in a full-screen empty state. Its zero and its sentence both speak
    // of DOCUMENTS: the Spec does carry an annexe, so "no annexe attached" would be false here —
    // that was the defect F8 closed.
    const documents = documentsBlock();
    expect(within(documents).getByText(/No document attached yet/)).toBeTruthy();
    expect(within(documents).queryByText(/annexe attached/)).toBeNull();
    expect(within(documents).getByText("0")).toBeTruthy();
    expect(within(documents).queryAllByRole("listitem")).toHaveLength(0);
  });

  it("gives the Links block its OWN empty state when the Spec has attachments but no link", async () => {
    serve([annexe("a1", "doc", "Design notes", 10)], []);

    render(<AnnexesTab detail={detailFixture()} />);

    expect(await within(documentsBlock()).findByText("Design notes")).toBeTruthy();
    const links = linksBlock();
    expect(within(links).getByText(/No external link on this Spec yet/)).toBeTruthy();
    expect(within(links).queryAllByRole("link")).toHaveLength(0);
  });

  it("keeps BOTH empty states on a Spec with nothing attached at all", async () => {
    serve([], []);

    render(<AnnexesTab detail={detailFixture()} />);

    expect(await screen.findByText(/No external link on this Spec yet/)).toBeTruthy();
    expect(screen.getByText(/No document attached yet/)).toBeTruthy();
    // The two headings name the two blocks after what each one draws — no overlap left.
    expect(screen.getByRole("heading", { name: "Links" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Documents" })).toBeTruthy();
  });
});
