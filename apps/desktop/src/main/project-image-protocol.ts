/**
 * THE `nyx-image://` CHANNEL, wired into Electron — the twenty lines that stand between a copy in
 * `userData/project-icons` and an `<img>` the renderer can actually draw.
 *
 * The RULES are not here: they are in `project-image.ts`'s {@link resolveProjectImageUrl}, which
 * takes a directory and a URL and answers with a path or a refusal, without Electron and without a
 * filesystem. This file is the seam — the two registrations Electron demands, at the two moments
 * it demands them — and it is deliberately thin, because a seam that grows rules is a rule nobody
 * can test.
 *
 * TWO MOMENTS, AND THEY ARE NOT INTERCHANGEABLE:
 *
 *  - {@link registerProjectImageScheme} must run BEFORE `app` is ready. It teaches every process,
 *    the renderer included, that `nyx-image:` is a real scheme — without it Blink never issues the
 *    request, the handler below is never called, and the picture fails exactly as silently as the
 *    `file://` URL it replaces. Called from `index.ts` next to the Wayland flags, which is the
 *    other thing that has to happen before Chromium initialises.
 *  - {@link serveProjectImages} must run AFTER it. `protocol.handle` needs a ready session.
 *
 * THE PRIVILEGES ARE THE MINIMUM THAT WORKS, and each one is load-bearing:
 *
 *  - `standard: true` gives the scheme URL parsing with a host and an origin, which is what makes
 *    `nyx-image://project-icons/<name>` mean one directory rather than an opaque blob of text —
 *    and what lets Chromium normalise the path before the handler ever sees it.
 *  - `secure: true` puts it on the same footing as `https:` for the platform's trust rules, so a
 *    page served over either origin the shell uses (dev `http://localhost`, packaged `file://`)
 *    treats it as a first-class subresource.
 *
 * `bypassCSP` is NOT set, and the app declares no CSP to widen. `corsEnabled`, `supportFetchAPI`,
 * `allowServiceWorkers` and `stream` are NOT set: an `<img>` is a no-cors subresource load, so
 * nothing the mark does needs them, and a privilege granted "just in case" is a privilege granted.
 */
import { net, protocol } from "electron";
import { pathToFileURL } from "node:url";

import {
  PROJECT_IMAGE_SCHEME,
  projectImageMimeType,
  resolveProjectImageUrl,
} from "./project-image";
import { realProjectImageDeps } from "./window-ipc";

/**
 * Teach Chromium the scheme. MUST be called before `app.whenReady()` — Electron throws if it is
 * called after, and Blink would otherwise treat `nyx-image://…` as an unknown scheme and never
 * request it.
 */
export function registerProjectImageScheme(): void {
  protocol.registerSchemesAsPrivileged([
    { scheme: PROJECT_IMAGE_SCHEME, privileges: { standard: true, secure: true } },
  ]);
}

/** A refusal, shaped so the `<img>`'s `onError` fires and `<ProjectMark>` falls back as designed. */
function noBody(status: number): Response {
  return new Response(null, { status });
}

/**
 * Answer one `nyx-image://` request out of `dir`.
 *
 * THE TWO FAILURES ARE SPELLED DIFFERENTLY ON PURPOSE, and the renderer treats them identically
 * (any non-2xx makes the `<img>` fall back) — the distinction is for the human driving the app
 * with DevTools open: **403** is the channel REFUSING a URL (wrong host, a traversal, an extension
 * nyx does not serve), **404** is a name the channel would have served but the disk does not hold —
 * the copy a sync tool ate, the fallback path the mark is built for.
 *
 * The body is streamed from `net.fetch` on a `file:` URL rather than read into a Buffer: Electron's
 * net stack supports `file:` from the main process, and a 2 MB picture never needs to sit in main's
 * heap to reach the page.
 */
async function respond(dir: string, url: string): Promise<Response> {
  const file = resolveProjectImageUrl(dir, url);
  if (file == null) return noBody(403);
  // Guaranteed non-null by the gate above (it refuses anything not in the accepted list); the
  // fallback is here so a future relaxation of that rule cannot serve an unlabelled body.
  const mime = projectImageMimeType(file) ?? "application/octet-stream";
  try {
    const local = await net.fetch(pathToFileURL(file).toString());
    if (!local.ok || local.body == null) return noBody(404);
    return new Response(local.body, {
      status: 200,
      headers: {
        "Content-Type": mime,
        // The type is DECLARED, so forbid sniffing it into something else.
        "X-Content-Type-Options": "nosniff",
        // NOT a performance choice. The copy's name already carries a revision, so a REPLACED
        // picture is a new URL and caching would be harmless there — but a copy that DISAPPEARS
        // keeps its name, and the fallback to the icon/initial is a promise the mark makes. A
        // cached 200 would keep drawing a file that is gone. The reads are local and small.
        "Cache-Control": "no-store",
      },
    });
  } catch {
    // `net.fetch` rejects on a missing / unreadable file. That is the ordinary fallback path, not
    // an incident: the mark degrades to the icon, then to the initial.
    return noBody(404);
  }
}

/**
 * Serve `userData/project-icons` on the scheme. Call once, after `app` is ready.
 *
 * `imagesDir` is taken as a function and resolved PER REQUEST — the `realProjectImageDeps` reflex
 * of `window-ipc.ts`: nothing pins a path at import time.
 *
 * The default IS `realProjectImageDeps.imagesDir`, the writer's own resolver, rather than a second
 * expression that happens to agree with it today. Two copies of "where the copies live" is exactly
 * how a channel ends up serving 404s for files the writer put somewhere else.
 */
export function serveProjectImages(
  imagesDir: () => string = realProjectImageDeps.imagesDir,
): void {
  protocol.handle(PROJECT_IMAGE_SCHEME, (request) => respond(imagesDir(), request.url));
}
