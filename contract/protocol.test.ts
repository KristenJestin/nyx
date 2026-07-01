/**
 * TS-side unit tests for the pipe-protocol codec (framing, incremental decode,
 * version-mismatch detection) — the behaviours that mirror the Rust unit tests in
 * `crates/nyx-sidecar/src/pipe/{codec,wire}.rs`. Dependency-free; run with:
 *   cd contract && bun test protocol.test.ts
 */
import { describe, expect, test } from "bun:test";
import {
  decodeMessage,
  encodeFrame,
  encodeMessage,
  encodeMessageFrame,
  FrameDecoder,
  FrameTooLargeError,
  MAX_FRAME_LEN,
  PROTOCOL_VERSION,
  VersionMismatchError,
  type Message,
} from "../apps/desktop/src/shared/pipe-protocol.ts";
import { encode as mpEncode } from "../apps/desktop/src/shared/msgpack-mini.ts";

const handshake: Message = {
  category: "request",
  kind: "handshake",
  id: 1,
  protocolVersion: PROTOCOL_VERSION,
};

describe("frame codec", () => {
  test("frame has a u32 big-endian length prefix", () => {
    const frame = encodeFrame(new Uint8Array([1, 2, 3]));
    expect(Array.from(frame.subarray(0, 4))).toEqual([0, 0, 0, 3]);
  });

  test("incremental decode across one-byte chunks", () => {
    const a = encodeMessageFrame(handshake);
    const b = encodeMessageFrame({ category: "request", kind: "ping", id: 2, nonce: 5 });
    const stream = new Uint8Array(a.length + b.length);
    stream.set(a, 0);
    stream.set(b, a.length);

    const dec = new FrameDecoder();
    const payloads: Uint8Array[] = [];
    for (const byte of stream) {
      dec.push(new Uint8Array([byte]));
      let p = dec.nextFrame();
      while (p) {
        payloads.push(p);
        p = dec.nextFrame();
      }
    }
    expect(payloads.length).toBe(2);
    expect(decodeMessage(payloads[0])).toEqual(handshake);
    expect((decodeMessage(payloads[1]) as { nonce: number }).nonce).toBe(5);
  });

  test("multiple frames in one push", () => {
    const a = encodeMessageFrame(handshake);
    const b = encodeMessageFrame({ category: "request", kind: "shutdown", id: 9 });
    const both = new Uint8Array(a.length + b.length);
    both.set(a, 0);
    both.set(b, a.length);
    const dec = new FrameDecoder();
    dec.push(both);
    expect(decodeMessage(dec.nextFrame()!)).toEqual(handshake);
    expect(decodeMessage(dec.nextFrame()!)).toEqual({
      category: "request",
      kind: "shutdown",
      id: 9,
    });
    expect(dec.nextFrame()).toBeNull();
  });

  test("oversize length prefix is rejected", () => {
    const frame = new Uint8Array(12);
    new DataView(frame.buffer).setUint32(0, MAX_FRAME_LEN + 1, false);
    const dec = new FrameDecoder();
    dec.push(frame);
    expect(() => dec.nextFrame()).toThrow(FrameTooLargeError);
  });
});

describe("version incompatibility", () => {
  test("decoding a wrong-version envelope throws a readable VersionMismatchError", () => {
    // Hand-build an envelope stamped with a wrong protocol version.
    const env = mpEncode([PROTOCOL_VERSION + 5, 0, 1, [1, 2]]);
    let err: unknown;
    try {
      decodeMessage(env);
    } catch (e) {
      err = e;
    }
    expect(err).toBeInstanceOf(VersionMismatchError);
    expect((err as VersionMismatchError).theirs).toBe(PROTOCOL_VERSION + 5);
    expect((err as Error).message).toContain("protocol version mismatch");
  });
});

describe("message round-trips", () => {
  const cases: Message[] = [
    handshake,
    { category: "response", kind: "pong", id: 2, nonce: 0xbeef },
    { category: "response", kind: "error", id: 3, code: "quota_exceeded", message: "slow down" },
    {
      category: "event",
      kind: "terminal_output",
      terminalId: 4,
      seq: 99,
      data: new Uint8Array([0, 255, 127, 128]),
    },
  ];
  for (const msg of cases) {
    test(`${msg.category}/${msg.kind} round-trips`, () => {
      const back = decodeMessage(encodeMessage(msg));
      // Normalize binary payload for structural equality.
      const norm = (m: Message) =>
        m.category === "event" && m.kind === "terminal_output"
          ? { ...m, data: Array.from(m.data) }
          : m;
      expect(norm(back as Message)).toEqual(norm(msg));
    });
  }
});
