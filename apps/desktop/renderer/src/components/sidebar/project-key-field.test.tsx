import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  isValidProjectKey,
  PROJECT_KEY_MAX_LEN,
  PROJECT_KEY_MIN_LEN,
  ProjectKeyField,
} from "./project-key-field";

/**
 * THE PROJECT KEY FIELD — the prefix every Spec of a project is referenced by, editable for the
 * first time.
 *
 * Two things are pinned here, and they are different in kind. The SHAPE RULE is pinned against its
 * ORIGINAL in Rust: this field keeps a mirror of `db::validate_project_key` so Save can light up as
 * the user types, and a mirror nobody checks is a mirror that drifts. The BEHAVIOUR is pinned
 * against the gesture: what makes the button available, what the field does to a lowercase
 * keystroke, when the re-keying warning appears, and that the backend's refusal is PRINTED rather
 * than swallowed.
 */

const REPO = join(process.cwd(), "..", "..", "..");
const DB_RS = join(REPO, "crates", "nyx-core", "src", "db.rs");

/** A `const NAME: usize = N;` read straight out of `db.rs`. */
function rustConst(name: string): number {
  const source = readFileSync(DB_RS, "utf8");
  const match = new RegExp(`pub const ${name}: usize = (\\d+);`).exec(source);
  if (!match) throw new Error(`no '${name}' in db.rs`);
  return Number(match[1]);
}

describe("the project-key shape rule, mirrored from Rust", () => {
  it("the bounds are the ones the domain declares", () => {
    expect(PROJECT_KEY_MIN_LEN).toBe(rustConst("PROJECT_KEY_MIN_LEN"));
    expect(PROJECT_KEY_MAX_LEN).toBe(rustConst("PROJECT_KEY_MAX_LEN"));
  });

  it("accepts what the domain accepts", () => {
    for (const ok of ["NY", "NYX", "WEB2", "A1", "ABCDEFGHIJ", "N0000"]) {
      expect(isValidProjectKey(ok), ok).toBe(true);
    }
  });

  it("refuses what the domain refuses, clause by clause", () => {
    // The SAME cases `db::tests::a_project_key_is_an_uppercase_letter_then_letters_or_digits`
    // replays on the other side.
    for (const bad of [
      "",
      "N",
      "ABCDEFGHIJK",
      "nyx",
      "1NYX",
      "NY X",
      "NY-X",
      "NY_X",
      "NYX!",
      "NYXÉ",
    ]) {
      expect(isValidProjectKey(bad), bad).toBe(false);
    }
  });
});

describe("<ProjectKeyField>", () => {
  const props = {
    projectId: "p1",
    projectKey: "NYX",
    onSave: vi.fn(async () => {}),
    specCount: 0,
  };

  it("upper-cases as you type, so the case-sensitive index never sees two spellings", () => {
    render(<ProjectKeyField {...props} onSave={vi.fn(async () => {})} />);
    const input = screen.getByLabelText("Project key") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "web2" } });
    expect(input.value).toBe("WEB2");
  });

  it("Save is off for an unchanged key and for a malformed one, on for a valid change", () => {
    render(<ProjectKeyField {...props} onSave={vi.fn(async () => {})} />);
    const input = screen.getByLabelText("Project key");
    const save = screen.getByRole("button", { name: "Save project key" });

    expect(save).toBeDisabled(); // unchanged
    fireEvent.change(input, { target: { value: "N" } });
    expect(save).toBeDisabled(); // too short
    fireEvent.change(input, { target: { value: "WEB" } });
    expect(save).toBeEnabled();
  });

  it("submits the new key", async () => {
    const onSave = vi.fn(async () => {});
    render(<ProjectKeyField {...props} onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("Project key"), { target: { value: "WEB" } });
    fireEvent.click(screen.getByRole("button", { name: "Save project key" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith("WEB"));
  });

  /**
   * THE WARNING IS CONDITIONAL ON BOTH HALVES — there is something to re-key, AND the field
   * actually differs. A warning that is always on screen is a warning nobody reads, and one shown
   * on a project with no Spec would be describing a consequence that cannot happen.
   */
  it("warns about the re-keying only when Specs exist and the key actually changes", () => {
    const { rerender } = render(
      <ProjectKeyField {...props} specCount={4} onSave={vi.fn(async () => {})} />,
    );
    expect(screen.queryByText(/reference this project/i)).toBeNull();

    fireEvent.change(screen.getByLabelText("Project key"), { target: { value: "WEB" } });
    // The warning names BOTH keys — it is the before/after that makes the consequence concrete,
    // not the count alone. Scoped to the warning itself: the field's help line also prints an
    // example reference, and matching it here would pass on the wrong element.
    const warning = screen.getByText(/reference this project/i);
    expect(warning).toHaveTextContent("4 Specs reference this project");
    // The verb agrees with the count too — one Spec REFERENCES, several REFERENCE.
    expect(warning).toHaveTextContent("NYX-1");
    expect(warning).toHaveTextContent("WEB-1");

    // THE VERB AGREES WITH THE COUNT, not only the noun: the real app shipped "1 Spec
    // reference this project" for one frame, which is what a `?:` on the noun alone leaves.
    rerender(<ProjectKeyField {...props} specCount={1} onSave={vi.fn(async () => {})} />);
    fireEvent.change(screen.getByLabelText("Project key"), { target: { value: "WEB" } });
    expect(screen.getByText(/references this project/i)).toHaveTextContent(
      "1 Spec references this project",
    );

    // No Specs, same edit — nothing is at stake, so nothing is said.
    rerender(<ProjectKeyField {...props} specCount={0} onSave={vi.fn(async () => {})} />);
    fireEvent.change(screen.getByLabelText("Project key"), { target: { value: "WEB" } });
    expect(screen.queryByText(/reference this project/i)).toBeNull();
  });

  /**
   * UNIQUENESS is the one rule no client can check, so the backend's sentence is the only place the
   * user can learn that a key is taken. Swallowing it would leave a form that says nothing while
   * the project keeps its old key.
   */
  it("prints the backend's refusal under the input", async () => {
    const onSave = vi.fn(async () => {
      throw new Error("another project already uses that key");
    });
    render(<ProjectKeyField {...props} onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("Project key"), { target: { value: "WEB" } });
    fireEvent.click(screen.getByRole("button", { name: "Save project key" }));

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(/already uses that key/i),
    );
  });
});
