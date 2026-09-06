/**
 * THE PROJECT PICTURE, ON DISK — the main-process half of a project's visual identity.
 *
 * A project may wear an IMAGE instead of a glyph, and `projects.image_path` stores a path. The
 * decision that shapes this whole file: **a chosen image is COPIED, never referenced in place**.
 * The column stays a `TEXT` (no blob in SQLite), and the file the mark points at is one nyx owns —
 * so moving, renaming or deleting the picture the user picked cannot silently blank a project's
 * tile weeks later.
 *
 * The copy lives at `userData/project-icons/<projectId>~<revision>.<ext>`: ONE file per project,
 * named by the project, which is what makes the three gestures below total rather than
 * best-effort. There is no index to keep in sync — the directory listing IS the state.
 *
 * THE REVISION IS NOT DECORATION. Without it the name is a pure function of the project and the
 * extension, so REPLACING a `.png` by another `.png` writes the same path, stores the same
 * `image_path`, and hands the renderer the same `<img src>` — nothing repaints, and the browser may
 * legitimately serve the previous bytes from its cache. Worse, a mark that has already recorded
 * that source as UNLOADABLE (`<ProjectMark>`'s `failedSrc`, the picker thumbnail's `failedThumb`)
 * stays on its fallback for the rest of the session: after a copy went missing, the project would
 * remain visibly broken even once a valid picture was put back. Both readers were already right —
 * they remember the failed SOURCE, not a boolean — and both were defeated by a write that produced
 * the same source twice. So the fix is here, at the WRITE, once, and every surface that draws a
 * project (rail tile, home header, cross-project corner mark, picker thumbnail) gets it without
 * knowing anything about it. See `project-mark.tsx` — it needs no cache-busting of its own.
 *
 * The revision is a base-36 clock reading, never the picked file's name (`project-identity.tsx`'s
 * "NO FILENAME, ANYWHERE" — the user's filename must not survive into a column an agent can read),
 * and never a hash of the bytes: re-choosing the SAME picture after its copy went missing has to
 * produce a new path too, which content-addressing could not do.
 *
 * WHY HERE. The renderer has no Node and no native dialog (`contextIsolation`, `nodeIntegration:
 * false`); the Rust sidecar has a filesystem but no dialog either, and it is not the process the OS
 * would parent a modal to. The main process is the only one holding both, exactly as it already
 * does for `pickDirectory`.
 *
 * Everything here takes its filesystem and its directory as ARGUMENTS ([`ImageFs`]) rather than
 * importing `node:fs` and reading `app.getPath`. That is the `resolveDataDirFrom` reflex of
 * `sidecar.ts`: the rules that matter — what is refused, what is deleted, what survives — are then
 * testable without Electron and without touching a real userData dir.
 */
import { basename, extname, join, resolve, sep } from "node:path";

/** The subdirectory of `userData` that holds every project picture nyx has adopted. */
export const PROJECT_IMAGES_DIRNAME = "project-icons";

/**
 * The extensions a project picture may have — a CURATED list, the `PROJECT_ICONS` reflex: what is
 * not named here is refused, rather than copied and then failing to render as a broken square.
 *
 * `svg` is in, and it is safe BECAUSE the mark renders it through `<img src>`: a picture loaded
 * that way is passive — no script runs, no external resource is fetched, and it cannot reach the
 * page's DOM. Rendering it INLINE would invert that property, which is why `<ProjectMark>` does
 * not, and must not.
 */
export const PROJECT_IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "webp", "svg"] as const;

/**
 * The size beyond which a picture is refused. It is NOT a performance budget — the mark is drawn
 * at 20 to 32 px and `object-cover` handles the rest — it is an ABSURDITY guard: nyx does no
 * resizing and no recompression (that would mean an image-processing dependency for a favicon),
 * so nothing else would stop a 200 MB raw scan from being copied into the app's data dir.
 */
export const PROJECT_IMAGE_MAX_BYTES = 2 * 1024 * 1024;

/** The slice of `node:fs` these gestures need — injected so the rules are testable. */
export interface ImageFs {
  mkdirSync(path: string, options: { recursive: true }): void;
  readdirSync(path: string): string[];
  statSync(path: string): { size: number };
  copyFileSync(source: string, destination: string): void;
  rmSync(path: string, options: { force: true }): void;
  existsSync(path: string): boolean;
}

/** Where a given `userData` root keeps the copies. */
export function projectImagesDir(userData: string): string {
  return join(userData, PROJECT_IMAGES_DIRNAME);
}

/**
 * The normalised extension of `filePath` if it is one nyx accepts, else `null`. Lowercased, without
 * the dot — `Logo.PNG` and `logo.png` are the same kind of file, and the copy is always named in
 * lowercase so the "one file per project" rule cannot be defeated by case.
 */
export function acceptedExtension(filePath: string): string | null {
  const ext = extname(filePath).replace(/^\./, "").toLowerCase();
  return (PROJECT_IMAGE_EXTENSIONS as readonly string[]).includes(ext) ? ext : null;
}

/**
 * ─────────────────────────────────────────────────────────────────────────────────────────────
 * THE CHANNEL — how a copy on disk becomes something the renderer can actually draw.
 * ─────────────────────────────────────────────────────────────────────────────────────────────
 *
 * WHY THIS EXISTS AT ALL. Copying the picture into `userData` made the file nyx's own, but it left
 * the renderer holding an absolute filesystem path — and a path is not a URL. `<ProjectMark>` used
 * to wrap it in `file:///…`, which works from a page that is ITSELF at the `file://` origin (the
 * packaged build, `window.ts`'s `loadFile`) and CANNOT work in development, where `window.ts`
 * loads the Vite server by `loadURL` and the page is at `http://localhost:<port>`. Chromium
 * refuses a `file://` subresource fetched from an `http://` document, unconditionally: it is a
 * rule of the web platform, not a flag. So the feature was invisible for the only person who
 * develops the app, and "works once packaged" is not a shipped feature.
 *
 * THE THREE WAYS OUT, AND WHY THIS ONE. Turning `webSecurity` off, or widening a CSP, buys the
 * picture by unpicking the security posture `window.ts` spells out line by line — the whole page
 * would gain the right to read the disk, to pay for one favicon. Inlining the bytes as a `data:`
 * URL means reading a 2 MB file into the row the PRD froze as a `TEXT` holding a PATH. What is
 * left is the ordinary Electron answer: an APPLICATION SCHEME, registered by the main process,
 * that serves ONE directory and nothing else. It is the same URL in dev and packaged — the page's
 * own origin stops mattering, which is exactly the property that was missing.
 *
 * WHAT MAKES IT SAFE is not the scheme, it is {@link resolveProjectImageUrl}: the channel accepts
 * ONE PLAIN FILE NAME inside `project-icons`, with an extension nyx itself accepts, and refuses
 * everything else — a separator, a `..`, a decoded traversal, another host, another extension. A
 * custom protocol that concatenated `request.url` onto a directory would be a filesystem read
 * primitive handed to the page, which is strictly worse than the `file://` it replaces.
 */

/** The application scheme project pictures are served on. Mirrored in `project-mark.tsx`. */
export const PROJECT_IMAGE_SCHEME = "nyx-image";

/**
 * The single host the scheme answers on, spelled as the DIRECTORY it serves — one namespace, and
 * a URL that reads as what it is. Any other host is refused rather than mapped somewhere else.
 */
export const PROJECT_IMAGE_HOST = PROJECT_IMAGES_DIRNAME;

/** The origin every project-picture URL carries: `nyx-image://project-icons`. */
export const PROJECT_IMAGE_ORIGIN = `${PROJECT_IMAGE_SCHEME}://${PROJECT_IMAGE_HOST}`;

/**
 * The `Content-Type` each accepted extension is served with. DECLARED rather than sniffed, and the
 * reason is `svg`: Chromium renders an `<img>` pointing at SVG only when the response says
 * `image/svg+xml`, so a channel that let the type be guessed would ship a format the picker
 * explicitly offers and then draw nothing for it. `project-image.test.ts` pins that every accepted
 * extension has an entry here, so widening {@link PROJECT_IMAGE_EXTENSIONS} cannot half-land.
 */
const PROJECT_IMAGE_MIME: Readonly<Record<string, string>> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  webp: "image/webp",
  svg: "image/svg+xml",
};

/** The media type `filePath` is served as, or `null` when it is not a kind nyx accepts. */
export function projectImageMimeType(filePath: string): string | null {
  const ext = acceptedExtension(filePath);
  return ext == null ? null : (PROJECT_IMAGE_MIME[ext] ?? null);
}

/**
 * The URL that draws the copy named `fileName` — the exact string `project-mark.tsx` builds on the
 * renderer side, written here so the two can be compared in a spec rather than in a reader's head.
 */
export function projectImageUrl(fileName: string): string {
  return `${PROJECT_IMAGE_ORIGIN}/${encodeURIComponent(fileName)}`;
}

/**
 * THE GATE. The absolute path `url` may be served from `dir`, or `null` when it may not.
 *
 * This function IS the security boundary of the channel, so it is written as an allowlist: a URL
 * passes only by being `nyx-image://project-icons/<one plain file name with an accepted
 * extension>`. Everything below is a way of saying that once.
 *
 *  - The SCHEME and the HOST are checked, not assumed. `protocol.handle` only ever hands us our
 *    own scheme today, but this function is also reachable from a spec and from any future caller,
 *    and a gate that trusts its input is not a gate.
 *  - The name is DECODED ONCE and then required to be a single segment. Chromium normalises
 *    literal `..` segments out of a standard URL before the handler sees it — but it does NOT
 *    decode `%2e%2e%2f`, and `image_path` is a column an MCP agent can write, so a crafted
 *    `nyx-image://…` value reaches here verbatim through `projectImageSrc`'s already-schemed
 *    passthrough. Decoding first and refusing any separator is what closes that door.
 *  - The EXTENSION must be one nyx accepts, which confines the channel a second time: even a file
 *    another process dropped into `project-icons` can only leave through it if it is a picture.
 *  - The final `resolve` containment check is belt and braces. It cannot fail given the rules
 *    above; it is there so that relaxing one of them does not silently open the directory.
 */
export function resolveProjectImageUrl(dir: string, url: string): string | null {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  if (parsed.protocol !== `${PROJECT_IMAGE_SCHEME}:`) return null;
  if (parsed.host !== PROJECT_IMAGE_HOST) return null;
  let name: string;
  try {
    name = decodeURIComponent(parsed.pathname.replace(/^\/+/, ""));
  } catch {
    // A malformed percent-escape (`%zz`) — not a name, and never a reason to throw at the handler.
    return null;
  }
  if (name.length === 0) return null;
  // ONE SEGMENT: no separator in EITHER dialect, no relative step, and no colon — which covers a
  // Windows drive-relative head (`C:x.png`) and an NTFS alternate data stream (`x.png:evil`) in
  // one rule. The colon is refused on every platform ON PURPOSE, even though POSIX would treat it
  // as an ordinary character: the channel must mean the same thing on the three OSes the gate
  // runs, and no copy nyx writes has ever contained one (`<projectId>~<base36>.<ext>`).
  // `basename` is then belt and braces — it is what would catch a platform prefix rule this
  // regex has not heard of.
  if (/[\\/:]/.test(name) || name === "." || name === ".." || basename(name) !== name) return null;
  if (acceptedExtension(name) == null) return null;
  const file = join(dir, name);
  const root = resolve(dir);
  if (!resolve(file).startsWith(root.endsWith(sep) ? root : root + sep)) return null;
  return file;
}

/**
 * What separates a project id from the revision stamp in a copy's name. RESERVED, and chosen for
 * what it is NOT: a project id is a UUID v7 (`db::create_project`), so it holds hex digits and
 * hyphens and can never contain a `~`. A hyphen would have been the natural-looking choice and is
 * the trap — `proj-1.png` would then read as "project `proj`, revision `1`", which is exactly the
 * collision {@link claimantsOf} exists to refuse.
 */
const REVISION_SEPARATOR = "~";

/** The shape {@link nextRevision} emits: base-36 digits, lowercase. */
const REVISION_PATTERN = /^[0-9a-z]+$/;

/**
 * The project ids that could own `fileName`: its STEM, plus — when the stem ends in a well-formed
 * `~<stamp>` — the stem without it. Zero, one or two whole ids, never a prefix.
 *
 * THE PRECAUTION THIS CARRIES, and the reason the split is on a delimiter rather than on a
 * `startsWith`: two project ids where one starts with the other must not collide. `proj` being
 * alive must not save `proj-1`'s picture from the sweep, and adopting for `proj` must not delete
 * `proj-1`'s copy. Because a claimant is always a COMPLETE segment, that holds by construction —
 * `proj-1~7.png` yields `proj-1~7` and `proj-1`, and neither is `proj`.
 *
 * Returning candidates instead of parsing ONE id is what makes the sweep safe when a name is
 * ambiguous (an id that itself ended in `~<base36>`): the file is kept as soon as ANY live project
 * could claim it, and a picture that is still in use is never the thing that gets deleted.
 *
 * A pre-revision copy (`<projectId>.<ext>`, what earlier builds wrote) has no stamp and yields its
 * stem alone — so it is still found by {@link discardProjectImage} and still survives the sweep.
 */
function claimantsOf(fileName: string): string[] {
  const stem = basename(fileName, extname(fileName));
  const cut = stem.lastIndexOf(REVISION_SEPARATOR);
  if (cut <= 0) return [stem];
  return REVISION_PATTERN.test(stem.slice(cut + 1)) ? [stem, stem.slice(0, cut)] : [stem];
}

/**
 * Every copy currently held for `projectId` — normally zero or one, but the listing is what is
 * read rather than a guess, so a leftover from an interrupted write is still found and removed.
 * Matching is on a whole id segment via {@link claimantsOf}, never on a prefix.
 */
function copiesOf(fs: ImageFs, dir: string, projectId: string): string[] {
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir)
    .filter((name) => claimantsOf(name).includes(projectId))
    .map((name) => join(dir, name));
}

/**
 * The highest revision this process has already handed out — the ONLY mutable state in this file,
 * and it is what makes the guarantee "no path nyx has issued in this session comes back" rather
 * than the weaker "no path currently on disk comes back".
 *
 * The distinction is the whole hard case: the readers' memory of a source that failed to load is
 * React state, so it lives exactly as long as one app run. A copy that vanished from disk mid-run
 * (a sync tool ate it, the user emptied the folder) leaves nothing in the listing to bump past,
 * while `image_path` still names it and the mark still refuses it — and a clock read twice in the
 * same millisecond would hand that name straight back.
 */
let lastRevision = 0;

/**
 * The stamp the NEXT copy of `projectId` wears — a base-36 clock reading, bumped past every
 * revision already on disk and past every one this process has issued.
 *
 * THE CLOCK IS WHAT MAKES IT SURVIVE A RESTART: a counter restarted from the listing would restart
 * at the same value the previous run ended on, and hand back a name a stale `image_path` still
 * points at. A clock cannot go back over a restart.
 *
 * THE BUMPS ARE WHAT MAKE IT DETERMINISTIC: two adoptions inside one millisecond — a spec doing
 * them back to back, or a clock that stepped backwards — would otherwise produce the same name and
 * resurrect the very defect this file guards against. They are not a fallback, they are the reason
 * "adopting twice yields two different paths" can be asserted without faking time.
 */
function nextRevision(existingCopies: string[], projectId: string): string {
  let next = Math.max(Date.now(), lastRevision + 1);
  const prefix = `${projectId}${REVISION_SEPARATOR}`;
  for (const path of existingCopies) {
    const stem = basename(path, extname(path));
    if (!stem.startsWith(prefix)) continue;
    const previous = Number.parseInt(stem.slice(prefix.length), 36);
    if (Number.isFinite(previous) && previous >= next) next = previous + 1;
  }
  lastRevision = next;
  return next.toString(36);
}

/**
 * The WRITE-SIDE gate, symmetric to {@link resolveProjectImageUrl}. `projectId` arrives from the
 * renderer and is interpolated into the copy's file name, so it decides WHERE the file lands:
 * `path.join` resolves `..`, and a `/`, `\` or `:` would put the picked image somewhere no
 * `discardProjectImage` or `pruneProjectImages` would ever find it again. The read gate refuses
 * exactly these characters; leaving the write path open was the asymmetry, not a decision.
 *
 * Deliberately a SHAPE rule and not a UUID match: the ids nyx generates are UUID v7, but the
 * property that matters here is only "this is one path segment, and it names nothing but itself".
 */
function safeProjectId(projectId: string): boolean {
  return (
    projectId.length > 0 &&
    !/[\\/:]/.test(projectId) &&
    projectId !== "." &&
    projectId !== ".." &&
    basename(projectId) === projectId
  );
}

/**
 * Drop every copy held for `projectId` — the gesture that runs when the human REMOVES the identity:
 * the file does not survive the row that referenced it.
 *
 * Returns how many files were removed, which is what makes "changing the extension left the old
 * one behind" an observable failure rather than a silent one.
 *
 * THIS ONE THROWS, and that is the decision rather than an oversight. It is the primitive: it
 * reports what the filesystem said, and each caller chooses what to do with a locked file. There is
 * exactly one — `window-ipc.ts`'s `discardProjectImage` channel — and the swallowing is written down
 * one level up, at `bridge/electron.ts`'s `discardProjectImage`: the row is the truth, the file is a
 * cache of it, and a failed delete must never block the human from removing the identity. Catching
 * here instead would buy nothing and would turn the returned count into a lie — "how many I tried"
 * dressed up as "how many are gone". {@link adoptProjectImage} makes the OPPOSITE choice, on its own
 * copies, for a reason spelled out there.
 */
export function discardProjectImage(
  fs: ImageFs,
  dir: string,
  projectId: string,
  keep?: string,
): number {
  const stale = copiesOf(fs, dir, projectId).filter((path) => path !== keep);
  for (const path of stale) fs.rmSync(path, { force: true });
  return stale.length;
}

/**
 * ADOPT `sourcePath` as `projectId`'s picture: validate, copy in, drop any previous copy, and
 * return the absolute path of the COPY — the only value that may reach `projects.image_path`.
 *
 * THE THREE REFUSALS THROW WITH A SENTENCE A HUMAN CAN READ — an extension nyx does not accept, a
 * file past the size bound, and a source it cannot READ. The first two are things a human did on
 * purpose (picked a `.gif`, picked a 40 MB photo); the third is not, but it arrives by the same
 * door and must leave by it: this throw crosses `ipcMain.handle`, so letting the raw `ENOENT` /
 * `EACCES` through would put a system string in the message the settings modal shows IN LINE under
 * the control that produced it — a `role="alert"` node, explicitly not a toast (see
 * `project-identity.tsx`'s "It is NOT a toast"). A silent `null`, in all three cases, would be
 * indistinguishable from cancelling the dialog.
 *
 * The refusal is about the SOURCE only. A failure to WRITE the copy (a full disk, a userData dir
 * nyx cannot create) is not a refusal — nothing the user picked is wrong — and is left to
 * propagate as the system error it is.
 *
 * THE PREVIOUS COPY IS DROPPED AFTER THE NEW ONE IS WRITTEN, AND ITS FAILURE COSTS NOTHING. Two
 * things follow from the revision being in the name, and they point the same way:
 *
 *  - The drop is no longer needed for the new copy to LAND. Under the old invariant name, the
 *    destination WAS the previous file, so the write depended on the delete. Since the revision
 *    moved, `copyFileSync` writes a path nothing else holds — so ordering the drop first only
 *    bought a window in which the row still named a file that had just been deleted, for EVERY
 *    replacement rather than only across an extension change.
 *
 *    COPYING FIRST CLOSES ONE OF THE TWO CAUSES, NOT BOTH. Closed: the COPY that fails (a full
 *    disk, a source that turned unreadable). The old file is then still there, because nothing has
 *    been dropped yet, and the row keeps naming a picture that still exists. STILL OPEN: the
 *    PERSIST that fails. The row is written after this function returns, from the renderer
 *    (`project-identity.tsx` → `use-projects.ts`'s `setProjectMark` → `set_project_mark`), so on
 *    the nominal path — copy fine, drop fine, previous file really gone — a rejected invoke (the
 *    project was deleted meanwhile, the sidecar errored) leaves the row pointing at a file this
 *    call has just removed. So: no window for a failed copy, a window for a failed persist. The
 *    removal gesture DOES have the stronger property, and says so where it lives
 *    (`use-projects.ts`: "Ordered AFTER the write so a failed persist never leaves a row pointing
 *    at a file that is gone"); adoption cannot borrow it, because the delete it would have to
 *    defer happens in the process that does not own the write.
 *
 *    THAT RESIDUE IS ACCEPTED, WITH ITS REASON. Closing it means one more channel — the renderer
 *    asking the main process to drop the previous copy only once the persist has landed — i.e. a
 *    new IPC contract and a fourth gesture, arrived at late in the phase for a window whose worst
 *    outcome degrades cleanly: the mark falls back to the icon, then to the initial, never to a
 *    broken square, and re-picking a picture repairs the row. It is left to the phase-3 gate
 *    (`01M1CA0E0W9JBQT96BJGAPCJHQ`) to observe under real driving rather than closed blind here.
 *
 *  - What the drop still buys is HYGIENE — one file per project, so the renderer never has to pick
 *    between two — and hygiene must not cost the human their gesture. On Windows a handle held by
 *    an antivirus, the indexer or Chromium's own `file://` reader (none of which open with
 *    FILE_SHARE_DELETE) makes `rmSync` throw `EPERM`/`EBUSY` — `force: true` only ever swallowed
 *    `ENOENT`. That throw would cross `ipcMain.handle` and land as a raw system string under the
 *    *Replace image…* button, which is exactly what the three refusals above go out of their way
 *    to avoid. So it is caught, and NOT reported: nothing is lost when it happens. The new picture
 *    is in place, the returned path is the one the row will carry, and the file left behind is a
 *    stale copy that the NEXT gesture on this project takes — both this function and
 *    {@link discardProjectImage} work off the listing rather than off a remembered name, so the
 *    retry is automatic — and that {@link pruneProjectImages} collects once the project itself is
 *    gone. It cannot accumulate past the lock. Same bargain `bridge/electron.ts` already struck for
 *    the removal gesture: the row is the truth, the file is a cache of it.
 *
 * The drop works off the listing taken BEFORE the copy, by ID rather than by name, so it can never
 * reach the file just written — and the explicit guard below says so out loud, because the one way
 * that could stop being true is a regression in {@link nextRevision}, which would then turn a
 * replacement into a project with no picture at all.
 */
export function adoptProjectImage(
  fs: ImageFs,
  dir: string,
  projectId: string,
  sourcePath: string,
): string {
  // Refused BEFORE anything is read or written: this id decides the destination path, and the
  // read gate refuses the same characters. Not a sentence for a human — no gesture in the app can
  // produce it, so reaching here means the caller, not the picker, is wrong.
  if (!safeProjectId(projectId)) throw new Error("A project id cannot name a path.");
  const ext = acceptedExtension(sourcePath);
  if (!ext) {
    throw new Error(
      `A project picture must be one of ${PROJECT_IMAGE_EXTENSIONS.join(", ")} — ` +
        `${basename(sourcePath)} is not.`,
    );
  }
  // The file was there when the dialog listed it; it may not be there a click later — moved,
  // deleted, or on a share that dropped. That is the ONE refusal the user did not author, which
  // is exactly why it needs a sentence rather than the `ENOENT` string `statSync` throws.
  let size: number;
  try {
    size = fs.statSync(sourcePath).size;
  } catch {
    throw new Error(
      `A project picture must be readable — ${basename(sourcePath)} could not be read. ` +
        `It may have been moved or renamed, or sit on a drive that is no longer connected.`,
    );
  }
  if (size > PROJECT_IMAGE_MAX_BYTES) {
    const mb = (size / (1024 * 1024)).toFixed(1);
    throw new Error(
      `A project picture must stay under ${PROJECT_IMAGE_MAX_BYTES / (1024 * 1024)} MB — ` +
        `${basename(sourcePath)} is ${mb} MB.`,
    );
  }
  fs.mkdirSync(dir, { recursive: true });
  // The listing is where the previous revision is written down, so it is read once — before the
  // copy — and serves twice: it dictates the new stamp, and it is the exact set to clean up after.
  const previous = copiesOf(fs, dir, projectId);
  const revision = nextRevision(previous, projectId);
  const destination = join(dir, `${projectId}${REVISION_SEPARATOR}${revision}.${ext}`);
  fs.copyFileSync(sourcePath, destination);
  // AND THE PREVIOUS COPY STAYS, on purpose. Adoption is only half a gesture: the row still
  // points at the old file until the renderer persists the new one, and a persist can fail
  // (sidecar restarting, project deleted meanwhile). Deleting here would make that failure
  // destructive — a row naming a file nyx had already removed, i.e. a project whose picture is
  // gone for good. So the cleanup is the caller's SECOND step, after the write lands
  // (`use-projects.ts`, which spells the same rule for the removal path). Until then the old copy
  // costs a few kilobytes, and any later gesture on this project sweeps it: `copiesOf` reads the
  // directory, never a remembered name.
  return destination;
}

/**
 * THE SAFETY NET, and the most dangerous function in this file. Remove every copy in `dir` whose
 * stem is not one of `liveProjectIds` — the sweep that reclaims the picture of a project deleted
 * while nyx was closed, or deleted by an agent that has no business knowing about the disk.
 *
 * AN EMPTY LIST IS REFUSED, and that refusal is the whole point: "no live projects" and "I do not
 * know the live projects yet" are indistinguishable here, and treating them alike turns a boot
 * that raced the project list — or one that failed to load it — into a sweep of EVERY picture the
 * user ever chose. So an empty list is an explicit NO-OP, and the caller's own rule (the renderer
 * only sweeps after a SUCCESSFUL list) is the other half of the same guard. Returns the paths
 * actually removed.
 *
 * EACH ENTRY IS SWEPT ON ITS OWN. `readdirSync` lists whatever is in the directory, not only the
 * files nyx wrote: a sync tool's `.stfolder`, a `@eaDir`, a folder someone dropped in. None of
 * them is claimed by a live project, so all of them are "orphans" — and `rmSync` without
 * `recursive` throws `ERR_FS_EISDIR` on a directory. Letting that escape would abort the loop on
 * the FIRST such entry, leaving every genuine orphan sorted after it behind, at every boot, with
 * nothing to show for it (the caller swallows the rejection). So a failure to remove one entry is
 * skipped rather than fatal, and the returned list stays honest about what is actually gone.
 */
export function pruneProjectImages(fs: ImageFs, dir: string, liveProjectIds: string[]): string[] {
  if (liveProjectIds.length === 0) return [];
  if (!fs.existsSync(dir)) return [];
  const live = new Set(liveProjectIds);
  const orphans = fs
    .readdirSync(dir)
    // A file survives as soon as ONE live project could claim it — the same whole-segment rule
    // `copiesOf` uses, read the conservative way round, because the asymmetry is total here:
    // sparing an orphan costs a few kilobytes, deleting a live picture is not undoable.
    .filter((name) => !claimantsOf(name).some((id) => live.has(id)))
    .map((name) => join(dir, name));
  const removed: string[] = [];
  for (const path of orphans) {
    try {
      fs.rmSync(path, { force: true });
      removed.push(path);
    } catch {
      /* A directory, or a file held by something else: skip it and keep sweeping the rest. */
    }
  }
  return removed;
}
