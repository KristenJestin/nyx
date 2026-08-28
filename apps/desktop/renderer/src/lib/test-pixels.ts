import { page } from "vitest/browser";

/**
 * READING THE REAL PIXELS — the shared instrument of the browser specs that judge a design claim
 * ("it delimits without dominating", "this tint sits on top of that one") on what actually reaches
 * the screen rather than on a class string.
 *
 * It lives here because four browser specs had each grown their own copy of the same three
 * functions, which is the duplication the design specs themselves are meant to prevent. Test code
 * that measures the design system deserves the design system's own rule.
 *
 * Browser-only: `page.screenshot` exists in the `browser` project, so importing this from a jsdom
 * unit spec will fail at run time — by design.
 */

/** One sRGB pixel. */
export interface Pixel {
  r: number;
  g: number;
  b: number;
}

/** WCAG relative luminance of an sRGB pixel. */
export function luminance({ r, g, b }: Pixel): number {
  const lin = (c: number) => {
    const v = c / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/** WCAG contrast ratio between two pixels. */
export function contrast(a: Pixel, b: Pixel): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * Grab the page as PIXELS, and hand back a reader in CSS coordinates.
 *
 * `page.screenshot` returns the PNG as raw bytes here, so it is decoded through a blob URL rather
 * than a data URL. The capture is in DEVICE pixels; the returned reader rescales, so callers pass
 * the coordinates `getBoundingClientRect` gave them and never think about the ratio.
 */
export async function capture(): Promise<(x: number, y: number) => Pixel> {
  const shot = (await page.screenshot({ base64: true, save: false })) as unknown;
  const bytes =
    typeof shot === "string"
      ? Uint8Array.from(atob(shot), (c) => c.charCodeAt(0))
      : typeof (shot as { base64?: unknown }).base64 === "string"
        ? Uint8Array.from(atob((shot as { base64: string }).base64), (c) => c.charCodeAt(0))
        : Uint8Array.from(shot as ArrayLike<number>);
  const url = URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
  const image = new Image();
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error("the capture did not decode"));
    image.src = url;
  });
  const canvas = document.createElement("canvas");
  canvas.width = image.width;
  canvas.height = image.height;
  const ctx = canvas.getContext("2d") as CanvasRenderingContext2D;
  ctx.drawImage(image, 0, 0);
  URL.revokeObjectURL(url);
  const scale = image.width / window.innerWidth;
  return (x, y) => {
    const data = ctx.getImageData(Math.round(x * scale), Math.round(y * scale), 1, 1).data;
    return { r: data[0], g: data[1], b: data[2] };
  };
}

/**
 * Capture ONCE THE PAINT HAS SETTLED — the same reader as {@link capture}, taken after the pixel at
 * `probe` has stopped moving.
 *
 * Every surface in the design system that changes colour does so through `transition-colors`, which
 * is the point: a hover is not supposed to snap. A screenshot taken the instant after a hover
 * therefore catches the transition MID-FLIGHT, and a spec comparing two such captures compares two
 * arbitrary frames — it passes or fails on timing rather than on design. This polls until two
 * consecutive captures agree at the probe point, so what is measured is the state the user ends up
 * looking at.
 */
export async function captureSettled(
  probe: () => readonly [number, number],
  tries = 30,
): Promise<(x: number, y: number) => Pixel> {
  let previous: Pixel | null = null;
  let at = await capture();
  for (let i = 0; i < tries; i += 1) {
    const [x, y] = probe();
    const now = at(x, y);
    if (previous != null && previous.r === now.r && previous.g === now.g && previous.b === now.b) {
      return at;
    }
    previous = now;
    at = await capture();
  }
  return at;
}
