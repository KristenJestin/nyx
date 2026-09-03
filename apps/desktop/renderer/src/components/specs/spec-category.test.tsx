import { useState } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";

import { SpecCategoryPicker } from "./spec-category";
import type { SpecCategory, SpecDetail } from "./types";

/**
 * `<SpecCategoryPicker>` — the Spec detail's CATEGORY block (v2.9, PDR-0017).
 *
 * What these tests hold, and what they deliberately do not: every DOMAIN rule (the same-project
 * refusal of an assignment, the project scope of a write, the resolution of a name, the
 * immutability of the slug) lives in `nyx_core::category` and is covered there and in the sidecar's
 * dispatch tests. What is proven HERE is the surface's own contract, as the second human review of
 * the block worded it (`01M1HEA29RRF962TY54BH736GG`): ONE LINE and ONE CONTROL, a click (or
 * Enter/Space) that opens the picker STRAIGHT AWAY, a "No category" row that retires the
 * assignment, an Escape that writes nothing, and NOT ONE curation gesture — rename, create and
 * delete live in Settings → Categories and nowhere else.
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
const FEATURE = category({
  id: "c-feat",
  slug: "feature",
  name: "Feature",
  color_token: "group-1",
});
/** An AGENT's mint. Valid as it stands since the 2026-09-02 amendment — no curation state. */
const DEBT = category({
  id: "c-debt",
  slug: "dette-technique",
  name: "Dette technique",
  created_by_type: "agent",
  system: false,
  color_token: "group-5",
});

/** A `spec_detail` fixture carrying (or not) a resolved category. */
function detail(current: SpecCategory | null): SpecDetail {
  return {
    spec: {
      id: "s1",
      project_id: "p1",
      workspace_id: null,
      title: "A spec",
      slug: null,
      number: 1,
      priority: "med",
      status: "in_progress",
      mode: "stepped",
      phases: "[]",
      category_id: current?.id ?? null,
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
      title: "A spec",
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
    category: current,
  };
}

/** Render the block over a scripted backend, capturing every command it sends. */
function mount(
  current: SpecCategory | null,
  backend: (cmd: string, args?: Record<string, unknown>) => unknown,
) {
  const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
  const refreshes: number[] = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return backend(cmd, args);
  });
  const { container } = render(
    <SpecCategoryPicker
      detail={detail(current)}
      refresh={async () => {
        refreshes.push(1);
      }}
    />,
  );
  return { calls, refreshes, container };
}

/** The scripted vocabulary read the block issues on mount. */
function vocabulary(rows: SpecCategory[]) {
  return (cmd: string) => (cmd === "category_list" ? rows : undefined);
}

/**
 * Every element a user can reach in the block AT REST — i.e. with the popup closed.
 *
 * # Scope: at rest, and nothing more
 *
 * The query is bounded to `container`, and the list is rendered through `Combobox.Portal` — OUTSIDE
 * it, at the document root. So this counts the controls of the CLOSED block and says nothing about
 * the opened popup, whose own reachable elements (search field, options) are the subject of the
 * “one click opens the picker” tests below.
 *
 * # Excluded by `aria-hidden`, not by `tabindex`
 *
 * Base UI's `Combobox.Root` always renders a hidden input for form submission and marks it BOTH
 * `tabIndex={-1}` and `aria-hidden` (`combobox/root/AriaCombobox.js`). Only the second states the
 * INTENTION — "this is not part of the accessible UI" — while the first is one implementation
 * detail of how they keep it out of the tab order, and a Base UI bump is free to spell that
 * differently. Filtering on `aria-hidden` therefore says what this helper means, and survives the
 * bump; `[tabindex="-1"]` would have silently started counting the hidden input.
 */
function reachable(container: HTMLElement) {
  return Array.from(
    container.querySelectorAll<HTMLElement>("button, a[href], select, textarea, input, [tabindex]"),
  ).filter((element) => element.closest('[aria-hidden="true"]') === null);
}

/**
 * The block's ONE control, named for the state it is in. Its role is `combobox` and not `button`:
 * with the search field living inside the popup, Base UI makes the TRIGGER the combobox itself
 * (`aria-haspopup="dialog"`) — which is precisely the shape asked for, a value that IS the picker.
 */
function trigger(current: SpecCategory | null) {
  return screen.getByRole("combobox", {
    name:
      current === null ? "Choose a category" : `Change the category — currently ${current.name}`,
  });
}

/**
 * THE TWO STATES OF THE BLOCK, AND THE ONE CONTROL IN BOTH. The first delivery drew two pencils,
 * the name AND the slug, `Rename`, `Remove`, and an edit icon that swapped the chip for a CLOSED
 * select. The user asked for one line and one gesture.
 */
describe("<SpecCategoryPicker> — one line, one control", () => {
  it("FILLED: the chip alone — no slug, no Rename, no Remove, no pencil, and exactly ONE reachable control", async () => {
    const { calls, container } = mount(DEBT, vocabulary([BUG, FEATURE, DEBT]));

    // THE LINE: the colour dot + the human name. The proposed/confirmed axis was RETIRED
    // (PDR-0017 amended 2026-09-02), so there is no badge either.
    expect(await screen.findByText("Dette technique")).toBeTruthy();
    expect(screen.queryByText("Proposed")).toBeNull();
    // NOTHING ELSE IS ON THE LINE.
    expect(screen.queryByText("dette-technique")).toBeNull();
    expect(screen.queryByRole("button", { name: /^Rename/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Remove/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Delete/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Confirm/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "Open the category list" })).toBeNull();
    // And NOTHING IS OPEN: no closed select standing where the chip was, no list waiting.
    expect(screen.queryByRole("option")).toBeNull();

    // THE PROOF: AT REST — popup closed — the block holds ONE reachable element, and it is the
    // chip itself. (Once the popup opens it carries a search field and its options, but they live
    // outside this container, through `Combobox.Portal`.)
    const controls = reachable(container);
    expect(controls).toHaveLength(1);
    expect(controls[0]).toBe(trigger(DEBT));
    // A NATIVE button — which is what makes Enter and Space open the picker without a handler —
    // and the picker's OWN trigger, not a control that reveals one.
    expect(controls[0]?.tagName).toBe("BUTTON");
    expect(controls[0]?.getAttribute("aria-haspopup")).toBe("dialog");
    expect(controls[0]?.getAttribute("aria-expanded")).toBe("false");
    expect(controls[0]?.textContent).toContain("Dette technique");

    expect(calls.find((c) => c.cmd === "category_list")?.args).toMatchObject({ projectId: "p1" });
  });

  it("EMPTY: the trigger itself reads “Choose a category”, and it is still the only control", async () => {
    const { container } = mount(null, vocabulary([BUG]));

    const button = await screen.findByRole("combobox", { name: "Choose a category" });
    expect(reachable(container)).toEqual([button]);
    expect(screen.queryByRole("option")).toBeNull();
  });
});

describe("<SpecCategoryPicker> — one click opens the picker", () => {
  it("clicking the chip opens the list STRAIGHT AWAY — every option visible, “No category” among them", async () => {
    mount(DEBT, vocabulary([BUG, FEATURE, DEBT]));

    fireEvent.click(await screen.findByRole("combobox", { name: /^Change the category/ }));

    // NO SECOND GESTURE: the options are on screen from the one click, not behind a closed select
    // that would have to be opened in turn.
    await waitFor(() => expect(screen.getAllByRole("option")).toHaveLength(4));
    expect(screen.getByRole("option", { name: /No category/ })).toBeTruthy();
    expect(screen.getByRole("option", { name: /Bug/ })).toBeTruthy();
    expect(screen.getByRole("option", { name: /Feature/ })).toBeTruthy();
    // …and the search field is there, in the popup, already focused.
    const search = screen.getByRole("combobox", { name: "Search a category" });
    await waitFor(() => expect(document.activeElement).toBe(search));
  });

  it("EMPTY: one click on “Choose a category” opens the same list", async () => {
    mount(null, vocabulary([BUG, FEATURE]));

    fireEvent.click(await screen.findByRole("combobox", { name: "Choose a category" }));

    await waitFor(() => expect(screen.getAllByRole("option")).toHaveLength(3));
    expect(screen.getByRole("combobox", { name: "Search a category" })).toBeTruthy();
  });

  /**
   * THE KEYBOARD PATH IS THE PLATFORM'S. The trigger is a native `<button>`, so Enter and Space are
   * dispatched by the browser as `keydown` followed by `click` — the exact sequence replayed here.
   * Nothing in the block intercepts them, which is the point: there is no second affordance to
   * reach first.
   */
  it("Enter and Space on the chip open the picker, with no pointer event at all", async () => {
    mount(DEBT, vocabulary([BUG, DEBT]));

    const chip = await screen.findByRole("combobox", { name: /^Change the category/ });
    chip.focus();
    expect(document.activeElement).toBe(chip);

    fireEvent.keyDown(chip, { key: "Enter" });
    fireEvent.click(chip);
    await waitFor(() => expect(screen.getAllByRole("option").length).toBeGreaterThan(0));

    // Close again, then the same journey with Space.
    fireEvent.keyDown(screen.getByRole("combobox", { name: "Search a category" }), {
      key: "Escape",
    });
    await waitFor(() => expect(screen.queryByRole("option")).toBeNull());

    const reopened = screen.getByRole("combobox", { name: /^Change the category/ });
    fireEvent.keyDown(reopened, { key: " " });
    fireEvent.click(reopened);
    await waitFor(() => expect(screen.getAllByRole("option").length).toBeGreaterThan(0));
  });
});

describe("<SpecCategoryPicker> — choosing, un-choosing, and cancelling", () => {
  it("choosing a category writes it and re-pulls the detail through the existing channel", async () => {
    const { calls, refreshes } = mount(DEBT, (cmd) => {
      if (cmd === "category_list") return [BUG, DEBT];
      if (cmd === "spec_category_assign") return { spec: {}, category: BUG };
      return undefined;
    });

    fireEvent.click(await screen.findByRole("combobox", { name: /^Change the category/ }));
    // KEYBOARD ONLY from here: narrow the list, walk to the item, take it.
    const search = await screen.findByRole("combobox", { name: "Search a category" });
    fireEvent.change(search, { target: { value: "bug" } });
    await waitFor(() => expect(screen.getAllByRole("option")).toHaveLength(1));
    fireEvent.keyDown(search, { key: "ArrowDown" });
    fireEvent.keyDown(search, { key: "Enter" });

    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "spec_category_assign")?.args).toMatchObject({
        specId: "s1",
        categoryId: "c-bug",
      }),
    );
    // The refresh is the SAME `spec_detail` channel the page already reads — no local patching.
    await waitFor(() => expect(refreshes.length).toBe(1));
    // A CHOICE CLOSES THE PICKER, and the focus comes back to the chip rather than falling to
    // `<body>`.
    await waitFor(() => expect(screen.queryByRole("option")).toBeNull());
    await waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByRole("combobox", { name: /^Change the category/ }),
      ),
    );
  });

  it("“No category” RETIRES the assignment — there is no Remove button, only this row", async () => {
    const { calls, refreshes } = mount(BUG, (cmd) => {
      if (cmd === "category_list") return [BUG, FEATURE];
      if (cmd === "spec_category_unassign") return { spec: {}, category: null };
      return undefined;
    });

    fireEvent.click(await screen.findByRole("combobox", { name: /^Change the category/ }));
    fireEvent.click(await screen.findByRole("option", { name: /No category/ }));

    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "spec_category_unassign")?.args).toMatchObject({
        specId: "s1",
      }),
    );
    await waitFor(() => expect(refreshes.length).toBe(1));
  });

  it("the block falls back to “Choose a category” once the detail comes back without one", async () => {
    // The detail is re-pulled FOR REAL here: `refresh()` drops the category exactly as
    // `spec_detail` would after the un-assignment.
    mockIPC((cmd) => {
      if (cmd === "category_list") return [DEBT];
      if (cmd === "spec_category_unassign") return { spec: {}, category: null };
      return undefined;
    });
    function Host() {
      const [current, setCurrent] = useState<SpecCategory | null>(DEBT);
      return (
        <SpecCategoryPicker
          detail={detail(current)}
          refresh={async () => {
            setCurrent(null);
          }}
        />
      );
    }
    render(<Host />);

    fireEvent.click(await screen.findByRole("combobox", { name: /^Change the category/ }));
    fireEvent.click(await screen.findByRole("option", { name: /No category/ }));

    expect(await screen.findByRole("combobox", { name: "Choose a category" })).toBeTruthy();
  });

  /**
   * ESCAPE IS A CANCEL, AND IT WRITES NOTHING. The combobox CLEARS ITS OWN VALUE on Escape, which
   * reaches `onValueChange(null)` — the same path a choice takes. Leaving that wired meant backing
   * out of the picker silently un-classified the Spec.
   */
  it("Escape closes the picker and sends no command at all", async () => {
    const { calls, refreshes } = mount(DEBT, vocabulary([BUG, DEBT]));

    fireEvent.click(await screen.findByRole("combobox", { name: /^Change the category/ }));
    const search = await screen.findByRole("combobox", { name: "Search a category" });
    fireEvent.keyDown(search, { key: "Escape" });

    await waitFor(() => expect(screen.queryByRole("option")).toBeNull());
    // The value is back on screen, untouched…
    expect(screen.getByText("Dette technique")).toBeTruthy();
    // …and nothing was written.
    expect(calls.some((c) => c.cmd === "spec_category_unassign")).toBe(false);
    expect(calls.some((c) => c.cmd === "spec_category_assign")).toBe(false);
    expect(refreshes.length).toBe(0);
  });

  it("filters the list on the human NAME and on the STABLE SLUG", async () => {
    mount(null, vocabulary([BUG, FEATURE, DEBT]));

    fireEvent.click(await screen.findByRole("combobox", { name: "Choose a category" }));
    const search = await screen.findByRole("combobox", { name: "Search a category" });

    // By slug — what the agent harness reads, and what a human may well know.
    fireEvent.change(search, { target: { value: "dette-tech" } });
    await waitFor(() => {
      expect(screen.getAllByRole("option")).toHaveLength(1);
      expect(screen.getByRole("option", { name: /Dette technique/ })).toBeTruthy();
    });

    // By name.
    fireEvent.change(search, { target: { value: "feat" } });
    await waitFor(() => {
      expect(screen.getAllByRole("option")).toHaveLength(1);
      expect(screen.getByRole("option", { name: /Feature/ })).toBeTruthy();
    });

    // Nothing matches — the popup says so rather than going blank.
    fireEvent.change(search, { target: { value: "zzz" } });
    expect(await screen.findByText("No category matches.")).toBeTruthy();
  });

  it("shows a refused assignment in the AUTHORITY'S OWN WORDS, and does not refresh", async () => {
    // The cross-project refusal, exactly as `nyx_core::category` words it.
    const refusal =
      "category belongs to project p2, the Spec to project p1 — a Spec may only carry a category of its OWN project";
    const { refreshes } = mount(null, (cmd) => {
      if (cmd === "category_list") return [BUG];
      if (cmd === "spec_category_assign") throw new Error(refusal);
      return undefined;
    });

    fireEvent.click(await screen.findByRole("combobox", { name: "Choose a category" }));
    fireEvent.click(await screen.findByRole("option", { name: /Bug/ }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(refusal);
    expect(refreshes.length).toBe(0);
  });
});

describe("<SpecCategoryPicker> — the vocabulary's own edge states", () => {
  it("renders the LOADING state, inside the picker, while the vocabulary has not answered", async () => {
    let release: ((rows: SpecCategory[]) => void) | null = null;
    mount(null, (cmd) =>
      cmd === "category_list"
        ? new Promise<SpecCategory[]>((resolve) => {
            release = resolve;
          })
        : undefined,
    );

    // The trigger is on screen from the first frame — the block never degrades into nothing — and
    // it SAYS it is busy.
    const button = await screen.findByRole("combobox", { name: "Choose a category" });
    expect(button.getAttribute("aria-busy")).toBe("true");
    expect(button.hasAttribute("disabled")).toBe(false);

    fireEvent.click(button);
    expect(await screen.findByText("Loading categories…")).toBeTruthy();
    expect(screen.queryByRole("option")).toBeNull();

    await waitFor(() => expect(release).not.toBeNull());
    release!([BUG]);
    expect(await screen.findByRole("option", { name: /Bug/ })).toBeTruthy();
  });

  it("renders the EMPTY VOCABULARY state — a project with no category is not a broken picker", async () => {
    mount(null, vocabulary([]));

    fireEvent.click(await screen.findByRole("combobox", { name: "Choose a category" }));
    expect(await screen.findByText(/This project has no category yet/)).toBeTruthy();
  });

  it("renders the ERROR state with the backend's own sentence when the vocabulary cannot be read", async () => {
    mount(null, (cmd) => {
      if (cmd === "category_list") throw new Error("db error: database is locked");
      return undefined;
    });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("database is locked");
  });

  it("says WHY the list is empty INSIDE the popup — a refused read is not an empty vocabulary", async () => {
    mount(null, (cmd) => {
      if (cmd === "category_list") throw new Error("db error: database is locked");
      return undefined;
    });

    fireEvent.click(await screen.findByRole("combobox", { name: "Choose a category" }));

    // The popup is portalled, `z-50`, anchored right under the trigger — it covers the sentence
    // below it. So the reason has to be IN the popup, next to the choice that is missing.
    const status = await screen.findByText(/The project's categories could not be read/);
    expect(status.getAttribute("role")).toBe("status");
    expect(status.textContent).toContain("database is locked");

    // …and it is NOT mistaken for an empty vocabulary, which is a different state entirely.
    expect(screen.queryByText(/This project has no category yet/)).toBeNull();

    // The only row left is the removal sentinel, and it no longer stands there unexplained.
    const options = screen.getAllByRole("option");
    expect(options).toHaveLength(1);
    expect(options[0]?.textContent).toContain("No category");
  });
});

/**
 * CURATION LIVES IN SETTINGS → CATEGORIES, AND NOWHERE ELSE. Creating, renaming and deleting a
 * vocabulary entry are deliberate acts on the PROJECT; this block acts on the SPEC. Not one of
 * those gestures is offered here, and not one of their commands can be sent from here.
 */
describe("<SpecCategoryPicker> — no curation gesture anywhere", () => {
  it("offers no rename, no create and no delete, and sends none of their commands", async () => {
    const { calls } = mount(DEBT, vocabulary([BUG, DEBT]));

    await screen.findByRole("combobox", { name: /^Change the category/ });
    fireEvent.click(screen.getByRole("combobox", { name: /^Change the category/ }));
    await waitFor(() => expect(screen.getAllByRole("option").length).toBeGreaterThan(0));

    // No field writes a NAME here: the one text field in the popup is the SEARCH field.
    expect(screen.queryByLabelText(/^New name for the category/)).toBeNull();
    expect(screen.queryByLabelText(/^New category name/)).toBeNull();
    expect(document.querySelectorAll('input[type="text"]')).toHaveLength(1);
    expect(screen.getByRole("combobox", { name: "Search a category" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Rename/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Delete/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();

    for (const forbidden of [
      "category_rename",
      "category_create",
      "category_delete",
      "category_confirm",
    ]) {
      expect(calls.some((c) => c.cmd === forbidden)).toBe(false);
    }
  });
});
