/**
 * LINKS IN THE TERMINAL — the one place that decides what a clickable URL is, and what clicking it
 * does.
 *
 * Two kinds of link reach a terminal and they are found in completely different ways:
 *   * a URL the shell simply PRINTED (`echo https://example.com`), which nothing marks up and
 *     which is found by scanning the text — `@xterm/addon-web-links`' job;
 *   * an OSC 8 HYPERLINK, where the emitting program explicitly wrapped a label around a URI
 *     (`\x1b]8;;https://…\x1b\\label\x1b]8;;\x1b\\`) — xterm parses those itself and hands them to
 *     the terminal's `linkHandler`.
 * Both funnel into {@link openTerminalLink} here, so the ONE thing that matters — what nyx is
 * willing to open — is written once and testable without a terminal.
 *
 * # Why `window.open` and no new IPC
 *
 * Electron main already has the policy: `setWindowOpenHandler` (see `apps/desktop/src/main/window.ts`)
 * DENIES every programmatic window and hands `http:`/`https:` URLs to `shell.openExternal`, so a
 * link opens in the user's real browser and never in a Chromium popup we would then have to
 * secure. Adding a channel of our own would mean a second door to the same room, with its own
 * allowlist to keep in sync with that one.
 *
 * # The renderer filters ANYWAY
 *
 * {@link isExternalHttpUrl} is not a duplicate of that policy, it is defence in depth against the
 * terminal specifically: the strings arriving here are ATTACKER-CONTROLLED in the most literal
 * sense — anything any program prints into any pane, including a `cat` of a hostile file. So
 * `javascript:`, `data:`, `file:` and friends are refused HERE, at the point where a terminal
 * byte becomes a navigation, rather than trusted to be refused two processes away.
 */

import type { ILinkHandler } from "@xterm/xterm";

/** The only protocols a terminal link may open. Everything else is refused, silently. */
const ALLOWED_PROTOCOLS = ["http:", "https:"];

/**
 * Whether `raw` is a URL nyx will open externally — parseable, and `http:`/`https:`.
 *
 * Pure and total: a malformed string is `false`, never a throw, because this runs on whatever a
 * shell happened to print.
 */
export function isExternalHttpUrl(raw: string): boolean {
  try {
    return ALLOWED_PROTOCOLS.includes(new URL(raw).protocol);
  } catch {
    // Not a URL at all (a bare word the scanner over-matched, a relative path): not a link.
    return false;
  }
}

/**
 * OPEN a terminal link in the user's browser, if it is one we allow. Returns whether it was
 * opened, so a caller can tell a refusal from a success (the tests do; the UI does not need to —
 * a refused link is simply inert).
 *
 * `noopener,noreferrer` costs nothing and is the right shape to ask for even though main denies
 * the window outright: if the policy there ever changed, this call would not be the hole.
 */
export function openTerminalLink(raw: string): boolean {
  if (!isExternalHttpUrl(raw)) return false;
  window.open(raw, "_blank", "noopener,noreferrer");
  return true;
}

/**
 * Build the OSC 8 link handler for one terminal — the `linkHandler` xterm option.
 *
 * `hover`/`leave` report the link under the cursor to `onHover`, which is what lets the RIGHT-CLICK
 * menu know whether it is over a link: a context menu has only coordinates, and xterm exposes no
 * "what link is at (x, y)" query, so the last hovered link is the honest answer to "what am I
 * about to right-click". `null` on leave, so the menu never offers a link the cursor has left.
 *
 * `allowNonHttpProtocols` is left at its default (`false`): xterm then drops non-HTTP OSC 8 URIs
 * before they ever reach `activate`, which is the same verdict {@link isExternalHttpUrl} would
 * give — belt and braces, and the braces are xterm's own documented guard against XSS here.
 */
export function createTerminalLinkHandler(onHover: (uri: string | null) => void): ILinkHandler {
  return {
    activate: (_event, text) => {
      openTerminalLink(text);
    },
    hover: (_event, text) => onHover(isExternalHttpUrl(text) ? text : null),
    leave: () => onHover(null),
  };
}
