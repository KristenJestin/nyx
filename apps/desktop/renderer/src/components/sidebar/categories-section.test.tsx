import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import type { SpecCategory } from "@/components/specs/types";

import { CategoriesSection } from "./categories-section";

/**
 * `<CategoriesSection>` — the project settings' CATEGORIES pane (v2.9 / PDR-0017, amended
 * 2026-09-02: the human curates the VOCABULARY, the agent classifies inside it).
 *
 * What these tests hold, and what they deliberately do not: every DOMAIN rule — the slug
 * derivation, the reservation of the four system slugs, the refusal of a blank name, and the two
 * refusals of a deletion (a system category; one a Spec still carries, whose sentence names the
 * count) — lives in `nyx_core::category` and is covered there and in the sidecar's dispatch tests.
 * What is proven HERE is the surface's own contract: which command it sends with which arguments,
 * that it shows the backend's sentence VERBATIM, that the Delete control is ABSENT (not disabled)
 * on a system row, that the four states are rendered, and that the whole pane is operable from the
 * keyboard with accessible names.
 */

function category(
  over: Partial<SpecCategory> & Pick<SpecCategory, "id" | "slug" | "name">,
): SpecCategory {
  return {
    color_token: "group-2",
    created_by_type: "system",
    confirmed_at: 1,
    system: true,
    ...over,
  };
}

const BUG = category({ id: "c-bug", slug: "bug", name: "Bug" });
/** An agent's mint: NOT a system slug, so it is the one row that can be deleted. */
const DEBT = category({
  id: "c-debt",
  slug: "dette-technique",
  name: "Dette technique",
  created_by_type: "agent",
  system: false,
  color_token: "group-5",
});

/** Render the pane over a scripted backend, capturing every command it sends. */
function mount(backend: (cmd: string, args?: Record<string, unknown>) => unknown) {
  const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return backend(cmd, args);
  });
  render(<CategoriesSection projectId="p1" />);
  return { calls };
}

/** The scripted vocabulary read the pane issues on mount. */
function vocabulary(rows: SpecCategory[]) {
  return (cmd: string) => (cmd === "category_list" ? rows : undefined);
}

describe("<CategoriesSection> — reading the project's vocabulary", () => {
  it("lists the project's categories with their stable slug, and reads the current project alone", async () => {
    const { calls } = mount(vocabulary([BUG, DEBT]));

    expect(await screen.findByText("Bug")).toBeTruthy();
    expect(screen.getByText("Dette technique")).toBeTruthy();
    // THE STABLE IDENTITY IS PRESENTED — a human renaming "Bug" must see that agents still read
    // `bug` — and there is NO field to edit it with at rest.
    expect(screen.getByText("bug")).toBeTruthy();
    expect(screen.getByText("dette-technique")).toBeTruthy();
    expect(calls.find((c) => c.cmd === "category_list")?.args).toMatchObject({ projectId: "p1" });
  });

  it("renders the LOADING state while the vocabulary has not answered", async () => {
    let release: ((rows: SpecCategory[]) => void) | null = null;
    mount((cmd) =>
      cmd === "category_list"
        ? new Promise<SpecCategory[]>((resolve) => {
            release = resolve;
          })
        : undefined,
    );

    expect(await screen.findByText("Loading categories…")).toBeTruthy();
    await waitFor(() => expect(release).not.toBeNull());
    release!([BUG]);
    expect(await screen.findByText("Bug")).toBeTruthy();
  });

  it("renders the EMPTY state — an empty vocabulary is not a broken list", async () => {
    mount(vocabulary([]));
    expect(await screen.findByText("This project has no category yet.")).toBeTruthy();
  });

  it("renders the ERROR state with the backend's own sentence", async () => {
    mount((cmd) => {
      if (cmd === "category_list") throw new Error("db error: database is locked");
      return undefined;
    });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("database is locked");
  });
});

describe("<CategoriesSection> — creating", () => {
  it("CREATES from the name alone and never sends a slug", async () => {
    const { calls } = mount((cmd, args) => {
      if (cmd === "category_list") return [BUG];
      if (cmd === "category_create") {
        return { category: { ...DEBT, name: String(args?.name) } };
      }
      return undefined;
    });

    const field = await screen.findByLabelText("New category name");
    fireEvent.change(field, { target: { value: "Dette technique" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "category_create")?.args).toEqual({
        projectId: "p1",
        name: "Dette technique",
      }),
    );
    // THE SLUG IS THE SERVICE'S: it is derived from the name, and this surface has no way to send
    // one even by mistake.
    expect(calls.find((c) => c.cmd === "category_create")?.args).not.toHaveProperty("slug");
    // The field is cleared for the next entry.
    await waitFor(() => expect((field as HTMLInputElement).value).toBe(""));
  });

  it("shows a refused creation in the AUTHORITY'S OWN WORDS", async () => {
    mount((cmd) => {
      if (cmd === "category_list") return [BUG];
      if (cmd === "category_create") throw new Error("a category name cannot be blank");
      return undefined;
    });

    fireEvent.change(await screen.findByLabelText("New category name"), {
      target: { value: "   x" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("a category name cannot be blank");
  });
});

describe("<CategoriesSection> — renaming", () => {
  it("RENAMES the label and never the slug: the row's form has a name field and no slug field", async () => {
    const { calls } = mount((cmd, args) => {
      if (cmd === "category_list") return [BUG];
      if (cmd === "category_rename") {
        return { category: { ...BUG, name: String(args?.name) }, homonym_slugs: [] };
      }
      return undefined;
    });

    fireEvent.click(await screen.findByRole("button", { name: "Rename the category Bug" }));
    const field = screen.getByLabelText("New name for the category bug");
    expect(screen.queryByLabelText(/^Slug/)).toBeNull();
    fireEvent.change(field, { target: { value: "Défaut" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "category_rename")?.args).toEqual({
        projectId: "p1",
        categoryId: "c-bug",
        name: "Défaut",
      }),
    );
    expect(calls.find((c) => c.cmd === "category_rename")?.args).not.toHaveProperty("slug");
  });

  it("renders the RENAME CONFLICT explicitly — the rename landed, and only the slugs tell the two apart", async () => {
    mount((cmd) => {
      if (cmd === "category_list") return [BUG];
      if (cmd === "category_rename") {
        return { category: { ...BUG, name: "Défaut" }, homonym_slugs: ["bug", "chore"] };
      }
      return undefined;
    });

    fireEvent.click(await screen.findByRole("button", { name: "Rename the category Bug" }));
    fireEvent.change(screen.getByLabelText("New name for the category bug"), {
      target: { value: "Défaut" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    // Queried by TEXT and read back through its live region: `role="status"` alone would also
    // match the submit button's in-flight spinner, which carries the same role.
    const notice = await screen.findByText(/now names 2 categories/);
    expect(notice.getAttribute("role")).toBe("status");
    expect(notice.textContent).toContain("bug, chore");
    // It is NOT a refusal — nothing is in the alert channel.
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("<CategoriesSection> — deleting", () => {
  /**
   * THE SYSTEM ROWS OFFER NO DELETE AT ALL. The control is ABSENT, not disabled: there is no
   * gesture to offer on nyx's own vocabulary, whose slug carries the discipline. The screen reads
   * the BACKEND's `system` verdict rather than a copy of the four slugs, and the refusal itself
   * still lives in `category::delete` — this is the courtesy on top of the guarantee.
   */
  it("offers NO Delete control on a system category, and one on everything else", async () => {
    mount(vocabulary([BUG, DEBT]));

    await screen.findByRole("button", { name: "Rename the category Bug" });
    expect(screen.queryByRole("button", { name: "Delete the category Bug" })).toBeNull();
    expect(
      screen.getByRole("button", { name: "Delete the category Dette technique" }),
    ).toBeTruthy();
    // Rename stays offered on a system row: the LABEL is the human's, the identity is not.
    expect(screen.getByRole("button", { name: "Rename the category Bug" })).toBeTruthy();
  });

  it("DELETES through its own command", async () => {
    const { calls } = mount((cmd) => {
      if (cmd === "category_list") return [BUG, DEBT];
      if (cmd === "category_delete") return { deleted: true, category: DEBT };
      return undefined;
    });

    fireEvent.click(
      await screen.findByRole("button", { name: "Delete the category Dette technique" }),
    );
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "category_delete")?.args).toEqual({
        projectId: "p1",
        categoryId: "c-debt",
      }),
    );
  });

  it("shows the IN-USE refusal verbatim — the count is the service's, and it is what is displayed", async () => {
    const refusal =
      "'Dette technique' is still carried by 3 Specs — re-classify them before deleting the category";
    mount((cmd) => {
      if (cmd === "category_list") return [BUG, DEBT];
      if (cmd === "category_delete") throw new Error(refusal);
      return undefined;
    });

    fireEvent.click(
      await screen.findByRole("button", { name: "Delete the category Dette technique" }),
    );
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(refusal);
    // The row is untouched — a refusal is not a removal.
    expect(screen.getByText("Dette technique")).toBeTruthy();
  });
});

describe("<CategoriesSection> — keyboard and accessible names", () => {
  it("gives every control an accessible name and reaches them all from the keyboard", async () => {
    mount(vocabulary([BUG, DEBT]));

    // The create field and its submit.
    expect(await screen.findByLabelText("New category name")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add" }).tagName).toBe("BUTTON");

    // The row actions are real BUTTONS whose names say which category they act on — so they are
    // tabbable even while the hover-revealed group is transparent.
    for (const name of [
      "Rename the category Bug",
      "Rename the category Dette technique",
      "Delete the category Dette technique",
    ]) {
      const button = screen.getByRole("button", { name });
      expect(button.tagName).toBe("BUTTON");
      expect(button.hasAttribute("disabled")).toBe(false);
    }

    // The list itself is a named list, so a screen-reader user knows what they landed in.
    expect(screen.getByRole("list", { name: "Project categories" })).toBeTruthy();
  });

  it("ESCAPE leaves the rename form without writing, and hands the focus back", async () => {
    const { calls } = mount(vocabulary([BUG]));
    const renameButton = () => screen.getByRole("button", { name: "Rename the category Bug" });

    fireEvent.click(await screen.findByRole("button", { name: "Rename the category Bug" }));
    fireEvent.keyDown(screen.getByLabelText("New name for the category bug"), { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByLabelText("New name for the category bug")).toBeNull(),
    );
    // The control that opened the form gets the focus back — otherwise a keyboard user is dropped
    // on `<body>` and tabs in from the top of the modal after every cancel.
    await waitFor(() => expect(document.activeElement).toBe(renameButton()));
    expect(calls.some((c) => c.cmd === "category_rename")).toBe(false);
  });
});
