/**
 * The PROJECT PICTURE rules, tested against an in-memory filesystem — never the real userData, and
 * never Electron. Same discipline as `identity.test.ts` next door: the logic in `project-image.ts`
 * takes its `fs` and its directory as arguments precisely so what is REFUSED, what is DELETED and
 * what SURVIVES can be pinned without a running app.
 *
 * The stake is asymmetric and that shapes the file: adopting the wrong picture is a mistake the
 * user undoes in two clicks, while deleting the right one is not undoable at all. So the sweep gets
 * more specs than the picker.
 *
 * Every path is built with `node:path` rather than spelled with `/`, so the specs read the same on
 * the three OSes the cross-OS gate runs (`\` vs `/` is exactly the kind of difference that turns a
 * green suite into a Windows-only red one).
 */
import { describe, expect, test } from "bun:test";
import { basename, dirname, join } from "node:path";

import {
  acceptedExtension,
  adoptProjectImage,
  discardProjectImage,
  PROJECT_IMAGE_EXTENSIONS,
  PROJECT_IMAGE_HOST,
  PROJECT_IMAGE_MAX_BYTES,
  PROJECT_IMAGE_ORIGIN,
  PROJECT_IMAGE_SCHEME,
  PROJECT_IMAGES_DIRNAME,
  projectImageMimeType,
  projectImageUrl,
  projectImagesDir,
  pruneProjectImages,
  resolveProjectImageUrl,
  type ImageFs,
} from "./project-image";

/**
 * A whole filesystem in a `Map` — path → byte size. `copyFileSync` records the copy so a spec can
 * assert WHAT was written where, and `rmSync` really removes, so "the old file is gone" is read off
 * the same structure the code walks.
 */
function memFs(seed: Record<string, number> = {}) {
  const files = new Map<string, number>(Object.entries(seed));
  const dirs = new Set<string>();
  const fs: ImageFs = {
    mkdirSync(path) {
      dirs.add(path);
    },
    readdirSync(path) {
      return [...files.keys()].filter((f) => dirname(f) === path).map((f) => basename(f));
    },
    statSync(path) {
      const size = files.get(path);
      if (size === undefined) throw new Error(`ENOENT: ${path}`);
      return { size };
    },
    copyFileSync(source, destination) {
      files.set(destination, files.get(source) ?? 0);
    },
    rmSync(path) {
      files.delete(path);
    },
    existsSync(path) {
      return (
        dirs.has(path) || files.has(path) || [...files.keys()].some((f) => dirname(f) === path)
      );
    },
  };
  return { fs, files };
}

/** The directory every spec below works in — a throwaway literal, never a real path. */
const USER_DATA = join("/data", "nyx-v2");
const DIR = projectImagesDir(USER_DATA);
/** A source picture, outside the app's dir — where the user's own file lives. */
const pictures = (name: string) => join("/home", "u", "pictures", name);

/**
 * The SHAPE of a copy's name — `<projectId>~<revision>.<ext>` — as a matcher, never as a literal.
 * The revision is a clock reading: what a spec may pin is the shape, and the fact that it MOVES
 * (the spec below), never the digits it happened to land on.
 */
const copyName = (projectId: string, ext: string) =>
  new RegExp(`^${projectId}~[0-9a-z]+\\.${ext}$`);

describe("where the copies live", () => {
  test("the copies hang off userData, in one named subdirectory", () => {
    expect(basename(DIR)).toBe(PROJECT_IMAGES_DIRNAME);
    expect(PROJECT_IMAGES_DIRNAME).toBe("project-icons");
    expect(dirname(DIR)).toBe(USER_DATA);
  });
});

describe("what a project picture may be", () => {
  test("the curated extensions are accepted, case-insensitively and normalised", () => {
    expect(acceptedExtension(pictures("logo.png"))).toBe("png");
    expect(acceptedExtension(pictures("Logo.PNG"))).toBe("png");
    expect(acceptedExtension(pictures("logo.JPEG"))).toBe("jpeg");
    expect(acceptedExtension(pictures("logo.jpg"))).toBe("jpg");
    expect(acceptedExtension(pictures("mark.svg"))).toBe("svg");
    expect(acceptedExtension(pictures("mark.webp"))).toBe("webp");
  });

  test("anything else is a MISS the caller handles — not a copy that fails to render later", () => {
    expect(acceptedExtension(pictures("anim.gif"))).toBeNull();
    expect(acceptedExtension(pictures("scan.tiff"))).toBeNull();
    expect(acceptedExtension(pictures("notes.txt"))).toBeNull();
    expect(acceptedExtension(pictures("noextension"))).toBeNull();
  });
});

describe("adopting a picture", () => {
  test("copies into the app's dir and returns the COPY's path, never the original's", () => {
    const source = pictures("logo.png");
    const { fs, files } = memFs({ [source]: 1024 });
    const copy = adoptProjectImage(fs, DIR, "proj-1", source);

    expect(dirname(copy)).toBe(DIR);
    expect(basename(copy)).toMatch(copyName("proj-1", "png"));
    expect(files.has(copy)).toBe(true);
    // THE POINT OF THE WHOLE FILE: the original is untouched, and losing it changes nothing.
    expect(files.has(source)).toBe(true);
    files.delete(source);
    expect(files.has(copy)).toBe(true);
  });

  test("a second picture REPLACES the first, including across an extension change", () => {
    const { fs, files } = memFs({ [pictures("a.png")]: 10, [pictures("b.svg")]: 20 });
    const first = adoptProjectImage(fs, DIR, "proj-1", pictures("a.png"));
    const second = adoptProjectImage(fs, DIR, "proj-1", pictures("b.svg"));
    // Replacing is TWO steps, and this is the second: adoption leaves the old copy alone so a
    // failed persist cannot strand the row on a file already deleted, and the caller drops it
    // once the write has landed — keeping the one it just referenced.
    discardProjectImage(fs, DIR, "proj-1", second);

    expect(basename(second)).toMatch(copyName("proj-1", "svg"));
    // The `.png` is GONE. This is the case a name-based overwrite silently misses: the two copies
    // have different names, so nothing would have collided, and the tile would then depend on
    // whichever the directory listing yielded first.
    expect(files.has(first)).toBe(false);
    expect(fs.readdirSync(DIR)).toEqual([basename(second)]);
  });

  test("ADOPTION ALONE LEAVES THE PREVIOUS COPY — the row still points at it", () => {
    // The half that makes a refused persist survivable. Between the copy and the write there is a
    // window where `image_path` still names the OLD file; deleting it there would turn a rejected
    // `set_project_mark` into a project whose picture is gone for good.
    const { fs, files } = memFs({ [pictures("a.png")]: 10, [pictures("b.png")]: 20 });
    const first = adoptProjectImage(fs, DIR, "proj-1", pictures("a.png"));
    const second = adoptProjectImage(fs, DIR, "proj-1", pictures("b.png"));

    expect(files.has(first)).toBe(true);
    expect(fs.readdirSync(DIR).sort()).toEqual([basename(first), basename(second)].sort());
  });

  /**
   * THE SPEC THIS FILE EXISTS FOR SINCE `01M1P81D1TA1RCAE9FJF17WS0G`. Two pictures of the SAME
   * format is the ordinary gesture — a human replacing a logo by a newer logo — and it used to
   * produce the same path twice: the same `image_path`, therefore the same `<img src>`, therefore
   * nothing repainted and a mark that had recorded that source as unloadable stayed on its
   * fallback for the rest of the session.
   *
   * This assertion is the one that reddens if the name ever goes back to being a pure function of
   * the project and the extension, and it needs no faked clock: `nextRevision` bumps past the
   * revision it reads on disk, so two adoptions inside one millisecond still differ.
   */
  test("A SECOND PICTURE OF THE SAME FORMAT LANDS ON A NEW PATH — not the one already on screen", () => {
    const { fs, files } = memFs({ [pictures("old.png")]: 10, [pictures("new.png")]: 20 });
    const first = adoptProjectImage(fs, DIR, "proj-1", pictures("old.png"));
    const second = adoptProjectImage(fs, DIR, "proj-1", pictures("new.png"));

    expect(second).not.toBe(first);
    expect(basename(second)).not.toBe(basename(first));
    expect(basename(second)).toMatch(copyName("proj-1", "png"));
    // And still exactly ONE copy once the caller's second step runs: a path that moves must not
    // be a way to accumulate files.
    discardProjectImage(fs, DIR, "proj-1", second);
    expect(files.has(first)).toBe(false);
    expect(fs.readdirSync(DIR)).toEqual([basename(second)]);
  });

  test("a copy that VANISHED from disk is still replaced by a path the old one cannot be", () => {
    // The hard case, and the reason the revision is a CLOCK rather than a counter read off the
    // listing: the previous copy is gone (a sync tool ate it, the user emptied the folder), so the
    // directory says nothing — yet `image_path` still points at the old name and the mark still
    // remembers it as unloadable. A counter would restart and hand back that very name.
    const { fs, files } = memFs({ [pictures("a.png")]: 10, [pictures("b.png")]: 10 });
    const first = adoptProjectImage(fs, DIR, "proj-1", pictures("a.png"));
    files.delete(first);

    const second = adoptProjectImage(fs, DIR, "proj-1", pictures("b.png"));
    expect(second).not.toBe(first);
    expect(fs.readdirSync(DIR)).toEqual([basename(second)]);
  });

  test("a PRE-REVISION copy is replaced, not left beside the new one", () => {
    // What earlier builds wrote is `<projectId>.<ext>` with no revision at all. Upgrading must not
    // strand it: it is still the project's copy, so adopting drops it like any other.
    const { fs, files } = memFs({ [join(DIR, "proj-1.png")]: 99, [pictures("new.png")]: 10 });
    const copy = adoptProjectImage(fs, DIR, "proj-1", pictures("new.png"));
    discardProjectImage(fs, DIR, "proj-1", copy);

    expect(files.has(join(DIR, "proj-1.png"))).toBe(false);
    expect(fs.readdirSync(DIR)).toEqual([basename(copy)]);
  });

  test("a picture for ANOTHER project is never touched", () => {
    const { fs } = memFs({ [pictures("a.png")]: 10, [join(DIR, "proj-2.png")]: 99 });
    const copy = adoptProjectImage(fs, DIR, "proj-1", pictures("a.png"));
    expect(fs.readdirSync(DIR).sort()).toEqual([basename(copy), "proj-2.png"].sort());
  });

  test("an id that merely PREFIXES another loses nothing when its neighbour adopts", () => {
    // The other half of the sweep's prefix precaution, on the write side: `proj` adopting must not
    // drop `proj-1`'s copy, whether that copy carries a revision or predates them.
    const { fs, files } = memFs({
      [pictures("a.png")]: 10,
      [join(DIR, "proj-1~7.png")]: 99,
      [join(DIR, "proj-12.svg")]: 99,
    });
    const copy = adoptProjectImage(fs, DIR, "proj", pictures("a.png"));

    expect(files.has(join(DIR, "proj-1~7.png"))).toBe(true);
    expect(files.has(join(DIR, "proj-12.svg"))).toBe(true);
    expect(basename(copy)).toMatch(copyName("proj", "png"));
  });

  test("an unsupported extension is refused with a sentence naming the file", () => {
    const { fs, files } = memFs({ [pictures("anim.gif")]: 10 });
    expect(() => adoptProjectImage(fs, DIR, "proj-1", pictures("anim.gif"))).toThrow(/anim\.gif/);
    expect(files.has(join(DIR, "proj-1.gif"))).toBe(false);
  });

  test("an oversized picture is refused — nyx resizes nothing, so the bound is the only guard", () => {
    const { fs, files } = memFs({ [pictures("scan.png")]: PROJECT_IMAGE_MAX_BYTES + 1 });
    expect(() => adoptProjectImage(fs, DIR, "proj-1", pictures("scan.png"))).toThrow(/2 MB/);
    expect(files.size).toBe(1);

    // Exactly at the bound is fine — the refusal is `>`, not `>=`.
    const { fs: atBound } = memFs({ [pictures("edge.png")]: PROJECT_IMAGE_MAX_BYTES });
    expect(basename(adoptProjectImage(atBound, DIR, "proj-1", pictures("edge.png")))).toMatch(
      copyName("proj-1", "png"),
    );
  });

  test("a source that cannot be READ is a refusal too — a sentence, not the system's error", () => {
    const { fs, files } = memFs({ [pictures("a.png")]: 10 });
    const kept = adoptProjectImage(fs, DIR, "proj-1", pictures("a.png"));

    // The picked file is gone by the time the copy runs (moved, deleted, or on a share that
    // dropped between the dialog and the OK): `statSync` throws. This refusal is the one the user
    // did NOT author, and it crosses `ipcMain.handle` on its way to a toast — so it must arrive as
    // a sentence naming the file.
    const source = pictures("gone.png");
    expect(() => adoptProjectImage(fs, DIR, "proj-1", source)).toThrow(/gone\.png/);
    let message = "";
    try {
      adoptProjectImage(fs, DIR, "proj-1", source);
    } catch (error) {
      message = (error as Error).message;
    }
    expect(message).toContain("could not be read");
    // THE ASSERTION THAT MAKES THIS SPEC WORTH ANYTHING: the raw system string does not survive.
    // Merely naming the file would pass against `ENOENT: /home/u/pictures/gone.png` too.
    expect(message).not.toContain("ENOENT");
    // And the refusal happened BEFORE the previous copy was dropped: an unreadable pick is not a
    // way to lose the picture the project already had.
    expect(files.has(kept)).toBe(true);
  });

  test("a refused picture leaves the PREVIOUS one in place", () => {
    const { fs, files } = memFs({
      [pictures("a.png")]: 10,
      [pictures("huge.png")]: PROJECT_IMAGE_MAX_BYTES + 1,
    });
    const kept = adoptProjectImage(fs, DIR, "proj-1", pictures("a.png"));
    expect(() => adoptProjectImage(fs, DIR, "proj-1", pictures("huge.png"))).toThrow();
    // The validation runs BEFORE the old copy is dropped, so a refusal is not a way to lose a
    // picture the user had already chosen.
    expect(files.has(kept)).toBe(true);
  });

  /**
   * THE SPEC FOR `01M1PAJF8DSR00PWMJ96XHDBN7`. `rmSync(path, { force: true })` swallows the file
   * that is ABSENT, never the file that is LOCKED — probed on Windows, a handle opened without
   * FILE_SHARE_DELETE (an antivirus, the indexer, Chromium's own `file://` reader) gives `EPERM`.
   * That throw crossed `ipcMain.handle`, was deliberately not swallowed by the bridge, and printed
   * `EPERM, Permission denied: \\?\C:\...` under the *Replace image…* button — the raw system
   * string the three refusals above exist to keep off the screen.
   *
   * Dropping the old copy is HYGIENE since the revision moved into the name: it is no longer what
   * makes the new copy land. So it may fail, and it must cost the user nothing.
   */
  test("A LOCKED PREVIOUS COPY DOES NOT ABORT THE REPLACEMENT, AND SAYS NOTHING TO THE USER", () => {
    const { fs, files } = memFs({ [pictures("old.png")]: 10, [pictures("new.png")]: 20 });
    const first = adoptProjectImage(fs, DIR, "proj-1", pictures("old.png"));

    let refused = 0;
    const locked: ImageFs = {
      ...fs,
      rmSync(path) {
        refused += 1;
        throw Object.assign(new Error(`EPERM, Permission denied: \\\\?\\${path}`), {
          code: "EPERM",
        });
      },
    };

    // The ADOPTION never touches the old copy any more, so it cannot fail on the lock at all.
    const second = adoptProjectImage(locked, DIR, "proj-1", pictures("new.png"));
    expect(refused).toBe(0);

    // The lock bites on the caller's SECOND step instead. `discardProjectImage` is the primitive
    // and reports what the filesystem said; the swallowing is written down one level up, at
    // `bridge/electron.ts`, so no `EPERM` string ever reaches `setImageError`.
    let thrown: unknown = null;
    try {
      discardProjectImage(locked, DIR, "proj-1", second);
    } catch (error) {
      thrown = error;
    }
    // The lock was really hit — without this the spec would go green just as well against a build
    // that stopped cleaning up at all, which is the other way to make the symptom disappear.
    expect(refused).toBe(1);
    expect(thrown).not.toBeNull();
    // And the picture the human asked for is on disk regardless: the new copy is written, and the
    // failure was only about reclaiming the old one.
    expect(second).not.toBe(first);
    expect(basename(second)).toMatch(copyName("proj-1", "png"));
    expect(files.has(second)).toBe(true);

    // The old copy survives as a stale file, and that is the accepted price. It is not permanent:
    // both gestures work off the DIRECTORY LISTING rather than a remembered name, so the very next
    // adoption takes it — one file per project again, without the user doing anything special.
    expect(files.has(first)).toBe(true);
    const third = adoptProjectImage(fs, DIR, "proj-1", pictures("old.png"));
    discardProjectImage(fs, DIR, "proj-1", third);
    expect(fs.readdirSync(DIR)).toEqual([basename(third)]);
  });

  /**
   * THE SPEC FOR `01M1PAJM5B6MMFCPQJKVS532EP`, the other half of the same reorder. The drop used to
   * run BEFORE the copy, so a write that failed left `projects.image_path` naming a file that had
   * just been deleted — the tile falling back to the initial although the user validated nothing.
   * Under the old invariant name that window only opened when the extension changed; the revision
   * opened it for EVERY replacement. Copying first closes it.
   */
  test("a copy that FAILS never costs the user the picture already in place", () => {
    const { fs, files } = memFs({ [pictures("a.png")]: 10, [pictures("b.png")]: 20 });
    const kept = adoptProjectImage(fs, DIR, "proj-1", pictures("a.png"));

    const fullDisk: ImageFs = {
      ...fs,
      copyFileSync() {
        throw Object.assign(new Error("ENOSPC: no space left on device"), { code: "ENOSPC" });
      },
    };
    // A failure to WRITE is not a refusal — nothing the user picked is wrong — so it propagates as
    // the system error it is. What must NOT happen is the row being left pointing at nothing.
    expect(() => adoptProjectImage(fullDisk, DIR, "proj-1", pictures("b.png"))).toThrow(/ENOSPC/);

    expect(files.has(kept)).toBe(true);
    expect(fs.readdirSync(DIR)).toEqual([basename(kept)]);
  });
});

describe("discarding a picture", () => {
  test("removes the project's copy whatever its extension, and only that project's", () => {
    const { fs } = memFs({ [join(DIR, "proj-1.svg")]: 1, [join(DIR, "proj-2.png")]: 1 });
    expect(discardProjectImage(fs, DIR, "proj-1")).toBe(1);
    expect(fs.readdirSync(DIR)).toEqual(["proj-2.png"]);
  });

  test("removes a REVISIONED copy, and a leftover from an interrupted write beside it", () => {
    // The listing IS the state: two revisions of the same project can only exist if a write was
    // interrupted, and the gesture that dereferences the picture takes both — otherwise the next
    // sweep would find nothing to do and the tile would depend on which one the listing yielded.
    const { fs } = memFs({
      [join(DIR, "proj-1~7.png")]: 1,
      [join(DIR, "proj-1~8.svg")]: 1,
      [join(DIR, "proj-1.png")]: 1,
      [join(DIR, "proj-12~9.png")]: 1,
    });
    expect(discardProjectImage(fs, DIR, "proj-1")).toBe(3);
    // `proj-12` merely STARTS WITH `proj-1` and keeps its picture: the claim is a whole id
    // segment, never a prefix.
    expect(fs.readdirSync(DIR)).toEqual(["proj-12~9.png"]);
  });

  test("is a silent no-op for a project that had none", () => {
    const { fs } = memFs({ [join(DIR, "proj-2.png")]: 1 });
    expect(discardProjectImage(fs, DIR, "proj-1")).toBe(0);
    expect(fs.readdirSync(DIR)).toEqual(["proj-2.png"]);
  });

  /**
   * THE DECISION, WRITTEN — the counterpart of the locked-copy spec above. `discardProjectImage`
   * does NOT swallow a locked file, on purpose: it is the primitive, it reports what the
   * filesystem said, and catching here would turn its returned count from "how many are gone" into
   * "how many I tried". The swallowing lives one level up, at `bridge/electron.ts`'s
   * `discardProjectImage` — "must never block the human from removing the identity" — where the
   * row, not the file, is the truth.
   *
   * The asymmetry with `adoptProjectImage` is not an oversight but the difference between the two
   * gestures: removing an identity is a ROW write that no file gates, while replacing a picture is
   * a FILE write that must not be gated by a delete of a different file.
   */
  test("a LOCKED file makes it throw — the primitive reports, the bridge is what forgives", () => {
    const { fs } = memFs({ [join(DIR, "proj-1~7.png")]: 1 });
    const locked: ImageFs = {
      ...fs,
      rmSync(path) {
        throw Object.assign(new Error(`EPERM, Permission denied: ${path}`), { code: "EPERM" });
      },
    };
    expect(() => discardProjectImage(locked, DIR, "proj-1")).toThrow(/EPERM/);
  });
});

describe("the boot sweep cannot destroy a live picture", () => {
  test("AN EMPTY LIST IS REFUSED — the no-op that stops a raced boot from wiping everything", () => {
    const { fs } = memFs({ [join(DIR, "proj-1.png")]: 1, [join(DIR, "proj-2.svg")]: 1 });
    // "There are no projects" and "I have not managed to read the projects" arrive here as the
    // same argument. Treating them alike would make a transient list failure delete every picture
    // the user ever chose — so the sweep declines rather than guesses.
    expect(pruneProjectImages(fs, DIR, [])).toEqual([]);
    expect(fs.readdirSync(DIR).sort()).toEqual(["proj-1.png", "proj-2.svg"]);
  });

  test("removes only the files no live project claims", () => {
    const { fs } = memFs({
      [join(DIR, "proj-1.png")]: 1,
      [join(DIR, "gone-1.svg")]: 1,
      [join(DIR, "gone-2.webp")]: 1,
    });
    expect(pruneProjectImages(fs, DIR, ["proj-1"]).sort()).toEqual([
      join(DIR, "gone-1.svg"),
      join(DIR, "gone-2.webp"),
    ]);
    expect(fs.readdirSync(DIR)).toEqual(["proj-1.png"]);
  });

  test("a live project's picture is never removed, whichever extension it wears", () => {
    const { fs } = memFs({
      [join(DIR, "proj-1.svg")]: 1,
      [join(DIR, "proj-2.jpeg")]: 1,
      [join(DIR, "proj-3.webp")]: 1,
    });
    expect(pruneProjectImages(fs, DIR, ["proj-1", "proj-2", "proj-3"])).toEqual([]);
    expect(fs.readdirSync(DIR).length).toBe(3);
  });

  test("a REVISIONED picture is claimed by its project, whatever revision it wears", () => {
    // The sweep reads a name it did not write: the revision is part of the file, not part of the
    // id, and a sweep that forgot that would delete every picture in the app at the next boot.
    const { fs } = memFs({
      [join(DIR, "proj-1~7.png")]: 1,
      [join(DIR, "proj-2~mfl1a2b3.svg")]: 1,
      // The pre-revision shape earlier builds wrote, alive too.
      [join(DIR, "proj-3.webp")]: 1,
      [join(DIR, "gone-1~4.png")]: 1,
    });
    expect(pruneProjectImages(fs, DIR, ["proj-1", "proj-2", "proj-3"])).toEqual([
      join(DIR, "gone-1~4.png"),
    ]);
    expect(fs.readdirSync(DIR).length).toBe(3);
  });

  test("an id that merely PREFIXES another is not a live claim", () => {
    // Matching is on a whole id segment, not on a prefix: `proj` being alive must not save
    // `proj-1`'s file. This is why the revision separator is `~` and not `-` — with a hyphen,
    // `proj-1.png` would read as "project `proj`, revision `1`" and this spec would go green for
    // the worst possible reason.
    const { fs } = memFs({ [join(DIR, "proj-1.png")]: 1, [join(DIR, "proj.png")]: 1 });
    expect(pruneProjectImages(fs, DIR, ["proj"])).toEqual([join(DIR, "proj-1.png")]);
    expect(fs.readdirSync(DIR)).toEqual(["proj.png"]);
  });

  test("a prefix is not a live claim once revisions are in play either", () => {
    const { fs } = memFs({
      [join(DIR, "proj-1~7.png")]: 1,
      [join(DIR, "proj~7.png")]: 1,
      [join(DIR, "proj-1~7.svg")]: 1,
    });
    expect(pruneProjectImages(fs, DIR, ["proj"]).sort()).toEqual(
      [join(DIR, "proj-1~7.png"), join(DIR, "proj-1~7.svg")].sort(),
    );
    expect(fs.readdirSync(DIR)).toEqual(["proj~7.png"]);
  });

  test("a name whose tail is NOT a revision is claimed by its whole stem", () => {
    // `~` only cuts when what follows it is the shape nyx emits. A stray file named `proj-1~beta
    // draft.png` belongs to nobody but `proj-1~beta draft`, so a live `proj-1` does not save it —
    // and, symmetrically, a project whose id genuinely ended in `~something` keeps its own.
    const { fs } = memFs({
      [join(DIR, "proj-1~beta draft.png")]: 1,
      [join(DIR, "proj-1~7.png")]: 1,
    });
    expect(pruneProjectImages(fs, DIR, ["proj-1"])).toEqual([join(DIR, "proj-1~beta draft.png")]);
    expect(fs.readdirSync(DIR)).toEqual(["proj-1~7.png"]);
  });

  test("a directory that does not exist yet sweeps nothing and throws nothing", () => {
    const { fs } = memFs();
    expect(pruneProjectImages(fs, DIR, ["proj-1"])).toEqual([]);
  });
});

/**
 * THE CHANNEL — `nyx-image://project-icons/<copy>`, the answer to finding
 * `01M1VKT3EBFE861DQSHBM885EV`: a picture that never appeared because the URL the renderer built
 * was a `file://` one, which Chromium refuses as a subresource of the `http://localhost` page the
 * DEVELOPMENT shell serves (`window.ts`'s `loadURL`). The packaged shell is at `file://` and did
 * work — which is precisely why nothing red ever showed.
 *
 * NO SPEC HERE CAN PROVE THE PICTURE IS DRAWN; that is the user's job in the real app, and it is
 * said out loud so nobody mistakes this block for the guarantee. What these specs pin is the half
 * that is decidable without a browser: the URL is well formed, and the gate that turns it back
 * into a path REFUSES everything that is not one plain picture inside the one directory.
 *
 * That gate is the security boundary of the whole channel. A custom protocol that joined
 * `request.url` onto a directory would hand the page an arbitrary filesystem read — strictly worse
 * than the `file://` it replaces — so the refusals below are the feature, not its trimmings.
 */
describe("the nyx-image channel", () => {
  /** The URL a renderer builds for a copy — the exact string `project-mark.tsx` produces. */
  const url = (name: string) => `${PROJECT_IMAGE_ORIGIN}/${name}`;

  test("the scheme and host are the two literals the renderer mirrors, spelled out", () => {
    // The renderer is a separate TS project and cannot import this module, so it re-declares these
    // (`project-mark.tsx`'s PROJECT_IMAGE_ORIGIN) and pins them in its own spec. Spelling them
    // here as literals rather than deriving them is what makes a one-sided rename red on both.
    expect(PROJECT_IMAGE_SCHEME).toBe("nyx-image");
    expect(PROJECT_IMAGE_HOST).toBe("project-icons");
    // The host IS the directory served — one namespace, and a URL that reads as what it is.
    expect(PROJECT_IMAGE_HOST).toBe(PROJECT_IMAGES_DIRNAME);
    expect(PROJECT_IMAGE_ORIGIN).toBe("nyx-image://project-icons");
    expect(projectImageUrl("proj-1~mfl1a2b3.png")).toBe(
      "nyx-image://project-icons/proj-1~mfl1a2b3.png",
    );
  });

  test("a copy nyx adopted resolves to its file inside the images dir, and nowhere else", () => {
    // The round trip the whole feature rests on: what `adoptProjectImage` wrote, named by the URL
    // the renderer builds, comes back as the very path that was written.
    const { fs } = memFs({ [pictures("logo.png")]: 10 });
    const copy = adoptProjectImage(fs, DIR, "proj-1", pictures("logo.png"));
    expect(resolveProjectImageUrl(DIR, projectImageUrl(basename(copy)))).toBe(copy);
  });

  test("every extension the picker offers is served with a DECLARED media type", () => {
    // `svg` is the one that would fail silently: Chromium draws an `<img>` pointing at SVG only
    // when the response says `image/svg+xml`. A widened extension list with no entry here would
    // copy the file, store the row, and render nothing.
    for (const ext of PROJECT_IMAGE_EXTENSIONS) {
      expect(projectImageMimeType(`proj-1~7.${ext}`), `${ext} must have a media type`).toMatch(
        /^image\//,
      );
    }
    expect(projectImageMimeType("proj-1~7.svg")).toBe("image/svg+xml");
    expect(projectImageMimeType("proj-1~7.jpeg")).toBe("image/jpeg");
    expect(projectImageMimeType("notes.txt")).toBeNull();
  });

  test("A CHEMIN HORS DOSSIER IS REFUSED — no separator, in either dialect, ever crosses", () => {
    // The name is ONE segment or it is nothing. Everything below would have escaped the directory
    // on some platform if the handler had simply joined it.
    expect(resolveProjectImageUrl(DIR, url("sub/dir/logo.png"))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("sub%2Fdir%2Flogo.png"))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("sub%5Cdir%5Clogo.png"))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("%2Fetc%2Fpasswd.png"))).toBeNull();
    // A Windows drive-relative head, which `basename` would silently strip.
    expect(resolveProjectImageUrl(DIR, url("C%3Alogo.png"))).toBeNull();
  });

  test("A `..` NEVER LEAVES THE DIRECTORY — collapsed when literal, refused when encoded", () => {
    // `image_path` is a `TEXT` an MCP agent may write, and `projectImageSrc` hands an
    // already-schemed value through untouched — so a crafted `nyx-image://…` reaches this gate
    // verbatim. It has two doors and they close differently, which is why both are pinned.
    //
    // A LITERAL `..` segment is normalised away by the URL parser itself (Chromium does the same
    // before `protocol.handle` is ever called), so it cannot climb: what is left names something
    // INSIDE the directory, subject to every other rule.
    expect(resolveProjectImageUrl(DIR, url(".."))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("."))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("../../secrets.png"))).toBe(join(DIR, "secrets.png"));
    // A PERCENT-ENCODED one is NOT normalised — `%2e%2e%2f` survives parsing as ordinary text —
    // so the gate decodes once and then refuses the separator it finds. This is the door.
    expect(resolveProjectImageUrl(DIR, url("%2e%2e%2f%2e%2e%2fsecrets.png"))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("..%5C..%5Csecrets.png"))).toBeNull();
  });

  test("ANOTHER HOST, another scheme, or an empty name is refused rather than mapped", () => {
    expect(resolveProjectImageUrl(DIR, "nyx-image://elsewhere/logo.png")).toBeNull();
    expect(resolveProjectImageUrl(DIR, "nyx-image://project-icons")).toBeNull();
    expect(resolveProjectImageUrl(DIR, "nyx-image://project-icons/")).toBeNull();
    expect(resolveProjectImageUrl(DIR, "file:///etc/passwd")).toBeNull();
    expect(resolveProjectImageUrl(DIR, "https://cdn.example/logo.png")).toBeNull();
    expect(resolveProjectImageUrl(DIR, "not a url at all")).toBeNull();
    // A malformed percent-escape is a refusal, never a throw crossing the protocol handler.
    expect(resolveProjectImageUrl(DIR, url("%zz.png"))).toBeNull();
  });

  test("only a PICTURE leaves the directory — the channel is confined a second time", () => {
    // Even a file another process dropped into `project-icons` can only be served if it is one of
    // the kinds the picker itself accepts. The directory holds nothing else today; this is what
    // keeps that true tomorrow.
    expect(resolveProjectImageUrl(DIR, url("proj-1~7.png"))).toBe(join(DIR, "proj-1~7.png"));
    expect(resolveProjectImageUrl(DIR, url("proj-1~7.svg"))).toBe(join(DIR, "proj-1~7.svg"));
    expect(resolveProjectImageUrl(DIR, url("secrets.db"))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("notes.txt"))).toBeNull();
    expect(resolveProjectImageUrl(DIR, url("noextension"))).toBeNull();
    // An NTFS alternate-data-stream tail is not an accepted extension either.
    expect(resolveProjectImageUrl(DIR, url("proj-1~7.png%3Astream"))).toBeNull();
  });

  test("a name with URL-hostile characters survives the round trip intact", () => {
    // The renderer encodes, the gate decodes once — so a copy whose name held a `+` or a space is
    // still found, and a DOUBLE-encoded traversal is read as the literal name it is (and missed),
    // never decoded a second time into a separator.
    expect(resolveProjectImageUrl(DIR, projectImageUrl("my logo+v2.png"))).toBe(
      join(DIR, "my logo+v2.png"),
    );
    // ONCE, never twice: a double-encoded traversal is read as the (absurd, non-existent) NAME it
    // literally is, inside the directory — it is not decoded a second time back into separators.
    expect(resolveProjectImageUrl(DIR, url("%252e%252e%252fsecrets.png"))).toBe(
      join(DIR, "%2e%2e%2fsecrets.png"),
    );
  });
});
