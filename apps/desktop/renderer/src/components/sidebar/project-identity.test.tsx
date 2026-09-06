import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";

// The pane toasts a REFUSED WRITE (never a successful one — the mark repainting is the
// acknowledgement). Mocked so the assertion does not need the toast manager mounted.
const toastMock = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  info: vi.fn(),
  warning: vi.fn(),
}));
vi.mock("@/components/ui/toast", () => ({ toast: toastMock }));

import { IDENTITY_COLOR_COUNT } from "@/components/panes/pane-groups";
import { PROJECT_ICONS } from "./project-mark";
import {
  MARK_CONTROL_SELECTED,
  ProjectIdentity,
  type ProjectIdentityProps,
} from "./project-identity";

/**
 * THE GESTURE, under test. Every criterion of the task is a press in this file: an accent, a
 * glyph, a picture, the exclusion between the last two, the removal, and the two ways a picture
 * can fail (refused at selection, unreadable at render).
 *
 * The block is presentational-with-callbacks, exactly like the rest of the Global pane, so the
 * WRITES are asserted here as callback arguments; that the callbacks reach `set_project_accent` /
 * `set_project_mark` is asserted end-to-end on the fake `nyxBridge` in `base-layout.test.tsx`.
 */

function mount(overrides: Partial<ProjectIdentityProps> = {}) {
  const props: ProjectIdentityProps = {
    projectId: "p1",
    projectName: "nyx",
    accent: null,
    icon: null,
    imagePath: null,
    onAccentChange: vi.fn(),
    onMarkChange: vi.fn(),
    pickImage: vi.fn(async () => null),
    ...overrides,
  };
  return { props, ...render(<ProjectIdentity {...props} />) };
}

/** The IMAGE sub-block's ONE control — thumbnail and label, since finding `01M1VP0PM0PVRAKV88K098RP8N`. */
function imageControl(): HTMLElement {
  return screen.getByTestId("identity-image-control");
}

/**
 * Does `el` wear THE RING — the selection state of the mark slot?
 *
 * It asks for the component's OWN constant, class by class, rather than for a re-spelled copy: the
 * whole point of the ring on the image control is that it is the grid's ring, so a spec holding its
 * own idea of what the ring looks like would stay green while the two drifted apart.
 */
function wearsRing(el: HTMLElement): boolean {
  return MARK_CONTROL_SELECTED.split(" ").every((token) => el.classList.contains(token));
}

beforeEach(() => {
  vi.clearAllMocks();
});
afterEach(() => {
  vi.restoreAllMocks();
});

describe("<ProjectIdentity> — the accent row", () => {
  it("offers the neutral plus every palette tone, named without inventing colour names", () => {
    mount();
    const row = screen.getByRole("group", { name: "Accent" });
    const swatches = within(row).getAllByRole("button");
    expect(swatches).toHaveLength(IDENTITY_COLOR_COUNT + 1);
    expect(swatches[0]).toHaveAttribute("aria-label", "No accent");
    expect(swatches[0]).toHaveAttribute("title", "No accent");
    expect(swatches[3]).toHaveAttribute("aria-label", "Accent 3");
    // The `title` carries the STORED TOKEN — the only place a human can read what a press writes.
    expect(swatches[3]).toHaveAttribute("title", "identity-3");
  });

  it("writes the TOKEN NAME on a press, immediately and with no Save", () => {
    const { props } = mount();
    fireEvent.click(screen.getByRole("button", { name: "Accent 3" }));
    expect(props.onAccentChange).toHaveBeenCalledWith("identity-3");
    // The block owns no Save of its own; the Name field's is the pre-existing rename.
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("ERASES with the first swatch — `null`, never `identity-0`", () => {
    const { props } = mount({ accent: "identity-3" });
    fireEvent.click(screen.getByRole("button", { name: "No accent" }));
    expect(props.onAccentChange).toHaveBeenCalledWith(null);
    expect(props.onAccentChange).not.toHaveBeenCalledWith("identity-0");
  });

  it("emits nothing when the worn accent is pressed again (the shared row's no-op rule)", () => {
    const { props } = mount({ accent: "identity-2" });
    const worn = screen.getByRole("button", { name: "Accent 2" });
    expect(worn).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(worn);
    expect(props.onAccentChange).not.toHaveBeenCalled();
  });

  it("toasts the backend's own reason when the write is refused", async () => {
    const { props } = mount();
    (props.onAccentChange as ReturnType<typeof vi.fn>).mockRejectedValue({
      kind: "command",
      message: "project not found",
    });
    fireEvent.click(screen.getByRole("button", { name: "Accent 1" }));
    await waitFor(() => expect(toastMock.error).toHaveBeenCalledWith("project not found"));
  });
});

describe("<ProjectIdentity> — the mark, one slot in two spellings", () => {
  it("paints the whole curated registry as toggles inside ONE Mark group", () => {
    mount();
    const group = screen.getByRole("group", { name: "Mark" });
    const tiles = within(group)
      .getAllByRole("button")
      .filter((t) => t.hasAttribute("data-project-icon"));
    // Twenty-eight glyphs — four rows of seven, the ceiling the registry documents.
    expect(tiles).toHaveLength(Object.keys(PROJECT_ICONS).length);
    expect(Object.keys(PROJECT_ICONS)).toHaveLength(28);
    // TOGGLES, not radios: pressing the worn one removes the mark, and a radio that un-checks
    // itself on a second press would lie to a screen reader.
    expect(tiles[0]).toHaveAttribute("aria-pressed", "false");
  });

  /**
   * FINDING `01M1VKVXY8VV0R76ZK0S2DCGCN` — the picture used to sit under the glyph grid with no
   * heading of its own, reading as a twenty-ninth tile. It now has one, of the same rank as
   * *Mark*'s. The risk that came with splitting is what this spec pins: the ONE sentence that
   * carried the exclusion was cut in two, and neither half may lose it.
   */
  it("gives the image its OWN heading and sentence, of the same rank as the glyphs'", () => {
    mount();
    const group = screen.getByRole("group", { name: "Mark" });
    // TWO headings, both inside the one group, both built by the same component — so "same rank"
    // is a fact about the markup, not a coincidence of two hand-written `<div>`s.
    const mark = within(group).getByText("Mark");
    const image = within(group).getByText("Image");
    expect(mark.className).toBe(image.className);
    expect(mark.parentElement!.className).toContain("flex items-baseline gap-2");
    expect(image.parentElement!.className).toContain("flex items-baseline gap-2");
    // BOTH descriptions carry the exclusion. The single sentence that used to say it — "An icon,
    // or an image — one slot, one choice" — is gone, so if either half dropped it the rule would
    // now be written nowhere a human reads.
    expect(within(group).getByText(/clears the image/i)).toBeTruthy();
    expect(within(group).getByText(/clears the icon/i)).toBeTruthy();
    expect(within(group).getAllByText(/one slot, one choice/i)).toHaveLength(2);
  });

  it("stays ONE setting for a screen reader — the split is presentation, not structure", () => {
    mount({ imagePath: "/data/project-icons/p1~mfl1a2b3.png" });
    const group = screen.getByRole("group", { name: "Mark" });
    // The glyphs AND the picture controls are inside the SAME group: a reader entering it hears
    // one setting, not two independent ones.
    expect(
      within(group)
        .getAllByRole("button")
        .filter((t) => t.hasAttribute("data-project-icon")),
    ).toHaveLength(Object.keys(PROJECT_ICONS).length);
    expect(within(group).getByRole("button", { name: "Replace image…" })).toBe(imageControl());
    // …and no THIRD group appeared with the second heading. Accent and Mark, still two.
    expect(screen.getAllByRole("group")).toHaveLength(2);
  });

  it("THE EXCLUSION HOLDS AT RUNTIME, both ways — one write, and the other spelling drops", async () => {
    // The heading split must not become a second slot. Each direction is a SINGLE `onMarkChange`
    // — the write `set_project_mark` uses to clear the other column, the one the `projects` CHECK
    // backs up — and the re-render with what the backend then holds shows the other control give
    // up its worn state.
    const copy = "/data/project-icons/p1~mfl9z8y7.png";
    const pickImage = vi.fn(async () => copy);
    const { props, rerender } = mount({ icon: "rocket", pickImage });
    expect(screen.getByRole("button", { name: "Rocket" })).toHaveAttribute("aria-pressed", "true");

    // GLYPH → PICTURE.
    fireEvent.click(screen.getByRole("button", { name: "Choose image…" }));
    await waitFor(() => expect(props.onMarkChange).toHaveBeenCalledWith({ image: copy }));
    expect(props.onMarkChange).toHaveBeenCalledTimes(1);
    rerender(<ProjectIdentity {...props} icon={null} imagePath={copy} />);
    // The tile gave up its pressed state, and THE RING moved onto the image control — the two
    // halves of one slot changing hands, which is what the ring is there to show.
    expect(screen.getByRole("button", { name: "Rocket" })).toHaveAttribute("aria-pressed", "false");
    expect(wearsRing(screen.getByRole("button", { name: "Rocket" }))).toBe(false);
    expect(wearsRing(imageControl())).toBe(true);

    // PICTURE → GLYPH, the same slot the other way round.
    fireEvent.click(screen.getByRole("button", { name: "Folder" }));
    expect(props.onMarkChange).toHaveBeenCalledTimes(2);
    expect(props.onMarkChange).toHaveBeenLastCalledWith({ icon: "folder" });
    rerender(<ProjectIdentity {...props} icon="folder" imagePath={null} />);
    // The control does not vanish — it is the discoverable way IN to a picture — it loses the ring.
    expect(imageControl()).toBeTruthy();
    expect(wearsRing(imageControl())).toBe(false);
    expect(wearsRing(screen.getByRole("button", { name: "Folder" }))).toBe(true);
    expect(screen.getByRole("button", { name: "Folder" })).toHaveAttribute("aria-pressed", "true");
  });

  it("sets a glyph on a press, and REMOVES it when the worn one is pressed again", () => {
    const { props, rerender } = mount();
    fireEvent.click(screen.getByRole("button", { name: "Rocket" }));
    expect(props.onMarkChange).toHaveBeenCalledWith({ icon: "rocket" });

    rerender(<ProjectIdentity {...props} icon="rocket" />);
    const worn = screen.getByRole("button", { name: "Rocket" });
    expect(worn).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(worn);
    expect(props.onMarkChange).toHaveBeenLastCalledWith(null);
  });

  it("a glyph replaces a picture — one write, and it is the one that clears the other column", () => {
    const { props } = mount({ imagePath: "/data/project-icons/p1.png" });
    fireEvent.click(screen.getByRole("button", { name: "Folder" }));
    expect(props.onMarkChange).toHaveBeenCalledTimes(1);
    expect(props.onMarkChange).toHaveBeenCalledWith({ icon: "folder" });
  });

  it("a picture replaces a glyph, and it is the COPY's path that is written", async () => {
    const pickImage = vi.fn(async () => "/data/project-icons/p1.png");
    const { props } = mount({ icon: "rocket", pickImage });
    fireEvent.click(screen.getByRole("button", { name: "Choose image…" }));
    await waitFor(() => expect(props.onMarkChange).toHaveBeenCalledTimes(1));
    expect(pickImage).toHaveBeenCalledWith("p1");
    expect(props.onMarkChange).toHaveBeenCalledWith({ image: "/data/project-icons/p1.png" });
  });

  /**
   * FINDING `01M1VP0PM0PVRAKV88K098RP8N` — the block used to offer TWO controls for one gesture: a
   * clickable thumbnail and a button beside it, both opening the same picker. The user drove the
   * real app and said so. The thumbnail could not just be deleted, because it carried the RING; so
   * the two were merged and the ring came along. The four specs below are that merge.
   */
  it("offers exactly ONE clickable thing in the IMAGE block — thumbnail AND label on it", () => {
    mount({ imagePath: "/data/project-icons/p1~mfl1a2b3.png" });
    const block = screen.getByTestId("identity-image-block");
    // A COUNT, not a `queryBy…` for the control that went: the defect was a duplicate, so only a
    // count can fail when one comes back under another shape.
    const clickable = within(block).getAllByRole("button");
    expect(clickable).toHaveLength(1);
    expect(clickable[0]).toBe(imageControl());
    // …and it is BOTH halves of what it replaced: the picture, and the words.
    expect(imageControl().querySelector("img")).not.toBeNull();
    expect(imageControl()).toHaveTextContent("Replace image…");
  });

  it("wears THE GRID'S ring when the picture is the mark — and only then", () => {
    // NAKED: nothing is worn, so nothing is ringed.
    const { unmount } = mount();
    expect(wearsRing(imageControl())).toBe(false);
    unmount();

    // A GLYPH is worn: the ring is on the tile, and the image control must not echo it.
    const glyph = mount({ icon: "rocket", accent: "identity-4" });
    expect(wearsRing(screen.getByRole("button", { name: "Rocket" }))).toBe(true);
    expect(wearsRing(imageControl())).toBe(false);
    glyph.unmount();

    // A PICTURE is worn: the ring is on the image control, in the accent the mark actually wears.
    mount({ imagePath: "/data/project-icons/p1~mfl1a2b3.png", accent: "identity-4" });
    expect(wearsRing(imageControl())).toBe(true);
    // Not a lookalike — the very classes a selected tile wears, from the component's own constant.
    expect(MARK_CONTROL_SELECTED).toContain("--identity-sel");
  });

  it("stays DISCOVERABLE with no picture, and its label says CHOOSE rather than replace", () => {
    // The control is the only way into a picture now that the clickable thumbnail is gone, so the
    // empty state may not be an empty space: the button is there, ringless, saying what a press
    // will do. The two labels are asserted TOGETHER — one spec asserting only the empty one would
    // stay green on a control that said "Choose image…" forever.
    const { props, rerender } = mount();
    expect(imageControl()).toHaveTextContent("Choose image…");
    expect(screen.getByRole("button", { name: "Choose image…" })).toBe(imageControl());
    expect(imageControl().querySelector("img")).toBeNull();
    expect(wearsRing(imageControl())).toBe(false);

    rerender(<ProjectIdentity {...props} imagePath="/data/project-icons/p1~mfl1a2b3.png" />);
    expect(imageControl()).toHaveTextContent("Replace image…");
    expect(imageControl()).not.toHaveTextContent("Choose image…");
  });

  it("names the merged control after its ACTION, and never calls it a toggle", () => {
    mount({ imagePath: "/data/project-icons/p1.png" });
    // Pressing it opens the OS picker; it inverts nothing, so it must not announce a pressed state
    // — that is the very lie the glyph tiles avoid by not being radios, and an audit already
    // settled it once. Merging the thumbnail in did not turn it into a toggle.
    expect(imageControl()).not.toHaveAttribute("aria-pressed");
    expect(imageControl()).not.toHaveAttribute("aria-checked");
    // THE NAME IS THE ACTION, pinned: it is what a voice user says out loud, and what a reader
    // needs in order to know what a press does. The thumbnail's `alt` is empty on purpose — a
    // decorative picture inside a named button, not a second name.
    expect(screen.getByRole("button", { name: "Replace image…" })).toBe(imageControl());
    expect(imageControl().querySelector("img")).toHaveAttribute("alt", "");
  });

  it("says the SELECTED state in a description, not in the name — and only while it holds", () => {
    // What the ring says to an eye has to reach a reader too, and `aria-pressed` is not how: the
    // control is not a toggle. A DESCRIPTION carries it, so the name stays the action.
    const { props, rerender } = mount({ imagePath: "/data/project-icons/p1~mfl1a2b3.png" });
    const describedBy = imageControl().getAttribute("aria-describedby");
    expect(describedBy).toBeTruthy();
    expect(document.getElementById(describedBy!)!.textContent).toBe(
      "This picture is the mark this project wears.",
    );

    // NAKED, and the claim goes with the ring: nothing may describe an unworn control as worn.
    rerender(<ProjectIdentity {...props} imagePath={null} />);
    expect(imageControl()).not.toHaveAttribute("aria-describedby");
    expect(document.getElementById(describedBy!)).toBeNull();
  });

  it("CANCELLING the picker changes nothing and says nothing", async () => {
    const pickImage = vi.fn(async () => null);
    const { props } = mount({ pickImage });
    fireEvent.click(screen.getByRole("button", { name: "Choose image…" }));
    await waitFor(() => expect(pickImage).toHaveBeenCalled());
    expect(props.onMarkChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows a REFUSAL in line under the control, and writes nothing", async () => {
    const pickImage = vi.fn(async () => {
      throw new Error("A project picture must stay under 2 MB — hero.png is 4.7 MB.");
    });
    const { props } = mount({ pickImage });
    fireEvent.click(screen.getByRole("button", { name: "Choose image…" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "A project picture must stay under 2 MB — hero.png is 4.7 MB.",
      ),
    );
    expect(props.onMarkChange).not.toHaveBeenCalled();
    // A refusal is NOT a toast: it belongs beside the button that produced it.
    expect(toastMock.error).not.toHaveBeenCalled();
  });
});

describe("<ProjectIdentity> — the four states", () => {
  it("NEUTRAL: the initial on the neutral tile, and nothing to remove", () => {
    mount();
    expect(screen.getByTestId("identity-preview-mark")).toHaveTextContent("N");
    expect(screen.getByRole("button", { name: /remove identity/i })).toBeDisabled();
  });

  it("ICON: the glyph is worn and the removal is available", () => {
    mount({ icon: "rocket", accent: "identity-4" });
    expect(screen.getByRole("button", { name: "Rocket" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: /remove identity/i })).toBeEnabled();
  });

  it("IMAGE: the control shows it, its label becomes Replace, and the FILENAME is nowhere", () => {
    const { container } = mount({
      accent: "identity-2",
      imagePath: "D:\\brand\\acme-logo.svg",
    });
    expect(imageControl().querySelector("img")).not.toBeNull();
    expect(screen.getByRole("button", { name: "Replace image…" })).toBe(imageControl());
    // The copy is stored as `<projectId>~<revision>.<ext>`: the original name does not exist in
    // the database and printing it would be a lie with a delay on it.
    expect(container.textContent).not.toContain("acme-logo");
    // THE ACCENT SURVIVES THE PICTURE — still selected, still live, never greyed out.
    const worn = screen.getByRole("button", { name: "Accent 2" });
    expect(worn).toHaveAttribute("aria-pressed", "true");
    expect(worn).toBeEnabled();
  });

  it("BROKEN IMAGE: the mark falls back to the initial, never to a broken square", () => {
    mount({ icon: null, imagePath: "/gone/p1.png" });
    const mark = screen.getByTestId("identity-preview-mark");
    const img = mark.querySelector("img");
    expect(img).not.toBeNull();
    fireEvent.error(img!);
    expect(mark.querySelector("img")).toBeNull();
    expect(mark).toHaveTextContent("N");
  });

  it("BROKEN IMAGE: the CONTROL falls back to the destructive glyph, and still says a picture is set", () => {
    // The state renders TWO pictures with two DIFFERENT fallbacks, on purpose. The 80 px mark in
    // the left column retreats to the initial; the image control must not, or `Remove identity`
    // would look like it has nothing to remove. Under jsdom an `<img>` never loads, so the branch
    // is only ever reached by firing the event by hand — which is why it needs its own press here.
    mount({ icon: null, accent: "identity-2", imagePath: "/gone/p1.png" });
    const img = imageControl().querySelector("img");
    expect(img).not.toBeNull();
    fireEvent.error(img!);

    expect(imageControl().querySelector("img")).toBeNull();
    const fallback = imageControl().querySelector("svg");
    expect(fallback).not.toBeNull();
    expect(fallback!.getAttribute("class")).toContain("text-destructive");
    // The control goes on witnessing that a picture is what this project wears — ring, label…
    expect(wearsRing(imageControl())).toBe(true);
    expect(screen.getByRole("button", { name: "Replace image…" })).toBe(imageControl());
    // …and, for a reader, the destructive colour is spelled out rather than merely coloured.
    const describedBy = imageControl().getAttribute("aria-describedby")!;
    expect(document.getElementById(describedBy)!.textContent).toBe(
      "This picture is the mark this project wears, but its file could not be loaded.",
    );
    expect(screen.getByRole("button", { name: /remove identity/i })).toBeEnabled();
  });

  it("BROKEN IMAGE: pointing the project at ANOTHER picture retries instead of staying blacklisted", () => {
    // The failure is remembered as a SOURCE, not as a boolean — so a new `imagePath` from the
    // parent paints again with no reset of its own.
    //
    // EVERY ASSERTION BELOW IS SYNCHRONOUS, ON PURPOSE, AND MUST STAY SO. A review pass suspected
    // this test of belonging to the suite's "sensitive to machine load" family and it does not:
    // every member of that family is an `async` test whose sync assertion runs after an `await`,
    // where an update can still be in flight. NOTHING is in flight here. Under jsdom an `<img>` is
    // never fetched — the default resource loader is a no-op, so neither `load` nor `error` is ever
    // emitted — hence the only `error` this component can ever see is the one fired by hand on the
    // line below, and the memorised source can only ever be `/gone/p1.png`. Between the `rerender`
    // and the assertion that follows it there is no await, so no timer, microtask or event can run:
    // what that assertion reads is exactly what the rerender painted. Wrapping it in a `waitFor`
    // would be waiting for an event that cannot come, and would trade a named failure for a
    // five-second deadline.
    //
    // THE TWO FIXTURES DIFFER BY NAME, not merely by directory, and that is now load-bearing:
    // since the picture is drawn through the app's channel (`nyx-image://project-icons/<name>`)
    // only the NAME crosses, so two copies that shared one would be one source. Nothing has to be
    // arranged for that — `project-image.ts` names every copy `<projectId>~<revision>.<ext>` and
    // the revision moves on every adoption — but a fixture that ignored it would test nothing.
    const { props, rerender } = mount({ imagePath: "/gone/p1~mfl1a2b3.png" });
    fireEvent.error(imageControl().querySelector("img")!);
    expect(imageControl().querySelector("img")).toBeNull();

    rerender(<ProjectIdentity {...props} imagePath="/data/project-icons/p1~mfl9z8y7.png" />);
    // Not merely "an image is back": it is the NEW one. A memory that forgot the source but
    // repainted the DEAD path would satisfy a bare `not.toBeNull()` and is a different defect —
    // the src is what says the retry went to the picture the parent now points at.
    const retried = imageControl().querySelector("img");
    expect(retried).not.toBeNull();
    expect(retried!.getAttribute("src")).toBe("nyx-image://project-icons/p1~mfl9z8y7.png");
  });

  /**
   * THE SAME-FORMAT REPLACEMENT — a `.png` swapped for another `.png`, the ordinary "here is my
   * new logo" of the second user story, and the case finding `01M1P81D1TA1RCAE9FJF17WS0G` proved
   * did nothing at all on screen.
   *
   * NOTHING IN THIS COMPONENT WAS WRONG: the paths below are the two the main process now hands
   * back for one project across two adoptions (`<projectId>~<revision>.<ext>`), and this block
   * already draws whatever it is given. What is pinned here is the CONSEQUENCE — that a moving
   * path repaints both pictures the state renders — so that a regression at the writer
   * (`project-image.ts`, where the revision is minted) has a named victim in the renderer too.
   * Every assertion is synchronous, for the reason spelled out in the spec above.
   */
  it("REPLACING with the same format repaints the control AND the preview mark", () => {
    const before = "/data/project-icons/p1~mfl1a2b3.png";
    const after = "/data/project-icons/p1~mfl9z8y7.png";
    const url = (path: string) => `nyx-image://project-icons/${path.split("/").pop()}`;
    const { props, rerender } = mount({ imagePath: before });
    const srcOf = (host: HTMLElement) => host.querySelector("img")?.getAttribute("src");
    const previewMark = () => screen.getByTestId("identity-preview-mark");

    expect(srcOf(imageControl())).toBe(url(before));
    expect(srcOf(previewMark())).toBe(url(before));

    rerender(<ProjectIdentity {...props} imagePath={after} />);

    // Both pictures moved. A writer that reused the path would leave these reading `before` — and
    // the browser free to serve the previous bytes from its cache, with no attribute change to
    // even tell it to look again.
    expect(srcOf(imageControl())).toBe(url(after));
    expect(srcOf(previewMark())).toBe(url(after));
  });

  it("REPLACING with the same format lifts a failure the control had already recorded", () => {
    // The visible half of the same defect, and the one that outlived the gesture: after a copy
    // went missing the control keeps its destructive glyph, because the guard compares the
    // failed SOURCE with the current one. Same format, same project — so under the old naming the
    // two were the same string and the project stayed broken until the next launch.
    const broken = "/data/project-icons/p1~mfl1a2b3.png";
    const fixed = "/data/project-icons/p1~mfl9z8y7.png";
    const { props, rerender } = mount({ imagePath: broken });
    fireEvent.error(imageControl().querySelector("img")!);
    expect(imageControl().querySelector("img")).toBeNull();
    expect(imageControl().querySelector("svg")).not.toBeNull();

    rerender(<ProjectIdentity {...props} imagePath={fixed} />);

    const repainted = imageControl().querySelector("img");
    expect(repainted).not.toBeNull();
    expect(repainted!.getAttribute("src")).toBe(`nyx-image://project-icons/${fixed.split("/").pop()}`);
  });
});

/**
 * THE LEFT COLUMN, after finding `01M1VKTZY6PYZJX3CXC0Q0VTAD`. It used to end on an *In context*
 * preview — the mark among three anonymous rail neighbours, then on a home row — meant to show
 * that an accent survives under a picture. The user drove the real app and asked for it out: it
 * demonstrated, in half a column, something already visible on the rail behind the modal.
 *
 * This spec is the REORIENTATION of what pinned that preview, not a new claim: it says what the
 * column holds NOW, so the removal cannot be undone by accident and so nothing here is left
 * asserting the absence of something (a `queryBy…` that is null forever, and green forever, even
 * if the whole block disappeared).
 */
describe("<ProjectIdentity> — the left column", () => {
  it("holds the mark, the name and the removal, CENTRED — and no *In context* preview", () => {
    mount({ accent: "identity-4", icon: "rocket" });
    expect(screen.queryByText(/in context/i)).toBeNull();

    const column = screen.getByTestId("identity-preview-mark").parentElement!;
    // THREE children, no more. The preview was a fourth, and a bare `queryByText` would have gone
    // on passing had it come back under another label — the count is what actually holds.
    expect(column.children).toHaveLength(3);
    expect(column.children[1]).toHaveTextContent("nyx");
    expect(column.children[2]).toHaveTextContent(/remove identity/i);
    // The 80 px mark is untouched — the size the task prescribes, and the one thing of the three
    // that says what the choices produce.
    expect(column.children[0]!.className).toContain("size-20");
    // CENTRED on both axes: what the user asked to be left with, once the preview went.
    expect(column.className).toContain("items-center");
    expect(column.className).toContain("justify-center");
  });
});

describe("<ProjectIdentity> — the single removal gesture", () => {
  it("puts the three fields back to nothing", async () => {
    const { props } = mount({ accent: "identity-5", icon: "rocket" });
    fireEvent.click(screen.getByRole("button", { name: /remove identity/i }));
    await waitFor(() => expect(props.onAccentChange).toHaveBeenCalledWith(null));
    expect(props.onMarkChange).toHaveBeenCalledWith(null);
  });

  it("is inert while the project has no id at all", () => {
    mount({ projectId: null, accent: "identity-5", imagePath: null });
    expect(screen.getByRole("button", { name: /remove identity/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Choose image…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Rocket" })).toBeDisabled();
    // EVERY control, the shared accent row included — it used to be the one live thing in a block
    // of greyed-out ones, which made the prop's contract a lie.
    expect(screen.getByRole("button", { name: "Accent 1" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "No accent" })).toBeDisabled();
  });

  it("presses nothing on the wire when an inert accent swatch is clicked", () => {
    const { props } = mount({ projectId: null });
    fireEvent.click(screen.getByRole("button", { name: "Accent 1" }));
    expect(props.onAccentChange).not.toHaveBeenCalled();
  });
});
