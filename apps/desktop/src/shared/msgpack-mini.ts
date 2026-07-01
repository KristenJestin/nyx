/**
 * Minimal, dependency-free MessagePack codec covering exactly the value subset
 * the nyx pipe protocol uses (PDR-0007):
 *   - integers: positive fixint, uint8/16/32/64, negative fixint, int8/16/32/64;
 *   - nil (encoded from `null`/`undefined`, decoded to `null`) — used for the
 *     OPTIONAL fields the protocol carries (e.g. a PTY exit code that is unknown);
 *   - booleans (true/false) — used for the managed-command status flags;
 *   - strings: fixstr, str8/16/32 (UTF-8);
 *   - binary: bin8/16/32;
 *   - arrays: fixarray, array16/32.
 *
 * Encoding is CANONICAL/minimal (smallest representation for each value), which
 * is exactly what rmp-serde produces on the Rust side — so the bytes match
 * byte-for-byte (proven by `contract/fixtures.json`). Keeping this self-contained
 * (no `@msgpack/msgpack`) makes the contract test reproducible with zero install
 * and keeps the sidecar transport free of an extra runtime dependency.
 *
 * Out-of-subset inputs (maps, floats, ext) throw — the protocol never emits them;
 * an attempt to is a bug we want surfaced loudly.
 */

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder("utf-8", { fatal: false });

/** A growable byte sink. */
class Writer {
  private bytes: number[] = [];
  push(b: number): void {
    this.bytes.push(b & 0xff);
  }
  pushAll(arr: Uint8Array): void {
    for (let i = 0; i < arr.length; i++) this.bytes.push(arr[i]);
  }
  pushU16(n: number): void {
    this.push(n >>> 8);
    this.push(n);
  }
  pushU32(n: number): void {
    this.push(n >>> 24);
    this.push(n >>> 16);
    this.push(n >>> 8);
    this.push(n);
  }
  pushU64(n: number): void {
    // Split into hi/lo 32-bit halves; n is assumed a non-negative safe integer.
    const hi = Math.floor(n / 0x1_0000_0000);
    const lo = n >>> 0;
    this.pushU32(hi);
    this.pushU32(lo);
  }
  toUint8Array(): Uint8Array {
    return Uint8Array.from(this.bytes);
  }
}

/** Encode an integer (signed) with the minimal MessagePack form. Non-negative values
 * take the canonical uint form; negative values take the canonical signed form
 * (negative fixint, int8/16/32/64) — exactly what rmp-serde produces for a Rust `i64`,
 * so the bytes match for the protocol's optional signed fields (e.g. exit codes). */
function encodeInt(w: Writer, n: number): void {
  if (!Number.isInteger(n)) throw new Error(`not an integer: ${n}`);
  if (n >= 0) {
    encodeUint(w, n);
    return;
  }
  if (n >= -32) {
    w.push(0xe0 | (n & 0x1f)); // negative fixint
  } else if (n >= -0x80) {
    w.push(0xd0);
    w.push(n & 0xff);
  } else if (n >= -0x8000) {
    w.push(0xd1);
    w.pushU16(n & 0xffff);
  } else if (n >= -0x8000_0000) {
    w.push(0xd2);
    w.pushU32(n >>> 0);
  } else {
    // int64: split the two's-complement 64-bit value into hi/lo halves.
    w.push(0xd3);
    const u = BigInt.asUintN(64, BigInt(n));
    w.pushU32(Number(u >> 32n));
    w.pushU32(Number(u & 0xffff_ffffn));
  }
}

/** Encode a non-negative integer with the minimal MessagePack uint form. */
function encodeUint(w: Writer, n: number): void {
  if (n < 0 || !Number.isInteger(n)) throw new Error(`not a non-negative integer: ${n}`);
  if (n <= 0x7f) {
    w.push(n); // positive fixint
  } else if (n <= 0xff) {
    w.push(0xcc);
    w.push(n);
  } else if (n <= 0xffff) {
    w.push(0xcd);
    w.pushU16(n);
  } else if (n <= 0xffff_ffff) {
    w.push(0xce);
    w.pushU32(n);
  } else {
    w.push(0xcf);
    w.pushU64(n);
  }
}

function encodeStr(w: Writer, s: string): void {
  const utf8 = textEncoder.encode(s);
  const len = utf8.length;
  if (len <= 31) {
    w.push(0xa0 | len); // fixstr
  } else if (len <= 0xff) {
    w.push(0xd9);
    w.push(len);
  } else if (len <= 0xffff) {
    w.push(0xda);
    w.pushU16(len);
  } else {
    w.push(0xdb);
    w.pushU32(len);
  }
  w.pushAll(utf8);
}

function encodeBin(w: Writer, b: Uint8Array): void {
  const len = b.length;
  if (len <= 0xff) {
    w.push(0xc4);
    w.push(len);
  } else if (len <= 0xffff) {
    w.push(0xc5);
    w.pushU16(len);
  } else {
    w.push(0xc6);
    w.pushU32(len);
  }
  w.pushAll(b);
}

function encodeArrayHeader(w: Writer, len: number): void {
  if (len <= 15) {
    w.push(0x90 | len); // fixarray
  } else if (len <= 0xffff) {
    w.push(0xdc);
    w.pushU16(len);
  } else {
    w.push(0xdd);
    w.pushU32(len);
  }
}

/** Encode a value from the supported subset. */
export function encode(value: unknown): Uint8Array {
  const w = new Writer();
  encodeValue(w, value);
  return w.toUint8Array();
}

function encodeValue(w: Writer, value: unknown): void {
  if (value === null || value === undefined) {
    w.push(0xc0); // nil — the protocol's "absent optional" (e.g. unknown exit code)
  } else if (typeof value === "boolean") {
    w.push(value ? 0xc3 : 0xc2); // true / false
  } else if (typeof value === "number") {
    encodeInt(w, value);
  } else if (typeof value === "bigint") {
    encodeInt(w, Number(value));
  } else if (typeof value === "string") {
    encodeStr(w, value);
  } else if (value instanceof Uint8Array) {
    encodeBin(w, value);
  } else if (Array.isArray(value)) {
    encodeArrayHeader(w, value.length);
    for (const item of value) encodeValue(w, item);
  } else {
    throw new Error(`msgpack-mini: unsupported value type: ${typeof value} (${String(value)})`);
  }
}

/** A cursor over the input bytes. */
class Reader {
  constructor(
    private readonly buf: Uint8Array,
    private pos = 0,
  ) {}
  u8(): number {
    return this.buf[this.pos++];
  }
  u16(): number {
    const v = (this.buf[this.pos] << 8) | this.buf[this.pos + 1];
    this.pos += 2;
    return v >>> 0;
  }
  u32(): number {
    const v =
      this.buf[this.pos] * 0x1000000 +
      ((this.buf[this.pos + 1] << 16) | (this.buf[this.pos + 2] << 8) | this.buf[this.pos + 3]);
    this.pos += 4;
    return v >>> 0 === v ? v >>> 0 : v;
  }
  u64(): number {
    const hi = this.u32();
    const lo = this.u32();
    return hi * 0x1_0000_0000 + lo;
  }
  slice(n: number): Uint8Array {
    const s = this.buf.subarray(this.pos, this.pos + n);
    this.pos += n;
    return s;
  }
}

/** Decode a single MessagePack value from the supported subset. */
export function decode(bytes: Uint8Array): unknown {
  const r = new Reader(bytes);
  return decodeValue(r);
}

function decodeValue(r: Reader): unknown {
  const b = r.u8();
  // positive fixint
  if (b <= 0x7f) return b;
  // negative fixint (0xe0..0xff -> -32..-1)
  if (b >= 0xe0) return b - 0x100;
  // fixstr
  if (b >= 0xa0 && b <= 0xbf) return textDecoder.decode(r.slice(b & 0x1f));
  // fixarray
  if (b >= 0x90 && b <= 0x9f) return readArray(r, b & 0x0f);
  switch (b) {
    case 0xc0:
      return null; // nil
    case 0xc2:
      return false;
    case 0xc3:
      return true;
    case 0xcc:
      return r.u8();
    case 0xcd:
      return r.u16();
    case 0xce:
      return r.u32();
    case 0xcf:
      return r.u64();
    case 0xd0: {
      const v = r.u8();
      return v < 0x80 ? v : v - 0x100; // int8
    }
    case 0xd1: {
      const v = r.u16();
      return v < 0x8000 ? v : v - 0x10000; // int16
    }
    case 0xd2: {
      const v = r.u32();
      return v < 0x8000_0000 ? v : v - 0x1_0000_0000; // int32
    }
    case 0xd3: {
      // int64: reassemble two's-complement from hi/lo halves.
      const hi = r.u32();
      const lo = r.u32();
      const u = (BigInt(hi) << 32n) | BigInt(lo);
      return Number(BigInt.asIntN(64, u));
    }
    case 0xd9:
      return textDecoder.decode(r.slice(r.u8()));
    case 0xda:
      return textDecoder.decode(r.slice(r.u16()));
    case 0xdb:
      return textDecoder.decode(r.slice(r.u32()));
    case 0xc4:
      return r.slice(r.u8());
    case 0xc5:
      return r.slice(r.u16());
    case 0xc6:
      return r.slice(r.u32());
    case 0xdc:
      return readArray(r, r.u16());
    case 0xdd:
      return readArray(r, r.u32());
    default:
      throw new Error(`msgpack-mini: unsupported lead byte 0x${b.toString(16)}`);
  }
}

function readArray(r: Reader, len: number): unknown[] {
  const out = new Array(len);
  for (let i = 0; i < len; i++) out[i] = decodeValue(r);
  return out;
}
