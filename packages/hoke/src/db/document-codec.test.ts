import { describe, expect, it, vi } from "vitest";
import { decodeDocument, decodeString, encodeDocument, encodeString } from "./document-codec.js";

const anything = (_value: unknown): _value is unknown => true;
const encode = (value: unknown) => encodeDocument(value, anything);

describe("document codec", () => {
  it("preserves JSON and freezes validator inputs and returned snapshots", () => {
    const input = { name: "Алиса", nested: [1, null, true], missing: {} };
    const guard = (value: unknown): value is typeof input => {
      expect(Object.isFrozen(value)).toBe(true);
      expect(Object.isFrozen((value as typeof input).nested)).toBe(true);
      return true;
    };
    const bytes = encodeDocument(input, guard);
    input.nested.push(2);
    const decoded = decodeDocument(bytes, guard);
    expect(decoded).toEqual({ name: "Алиса", nested: [1, null, true], missing: {} });
    expect(Object.isFrozen(input)).toBe(false);
    expect(Object.isFrozen(decoded)).toBe(true);
  });

  it.each([
    undefined,
    () => {},
    Symbol("x"),
    1n,
    NaN,
    Infinity,
    -Infinity,
    -0,
    new Date(),
    new Map(),
    Object.create(null),
    [undefined],
    { bad: undefined },
    Array(2),
  ])("rejects values JSON would change: %s", (value) => {
    expect(() => encode(value)).toThrow(expect.objectContaining({ code: "INVALID_VALUE" }));
  });

  it("rejects cycles, hidden data, custom prototypes and conversion hooks without invoking them", () => {
    const cycle: unknown[] = [];
    cycle.push(cycle);
    const getter = vi.fn(() => 1);
    const toJSON = vi.fn(() => ({}));
    const array = [1];
    Object.defineProperty(array, "extra", { value: 2 });
    for (const value of [
      cycle,
      Object.defineProperty({}, "x", { get: getter, enumerable: true }),
      { [Symbol("x")]: 1 },
      Object.defineProperty({}, "x", { value: 1 }),
      Object.create({ x: 1 }),
      { toJSON },
      array,
    ]) {
      expect(() => encode(value)).toThrow(expect.objectContaining({ code: "INVALID_VALUE" }));
    }
    expect(getter).not.toHaveBeenCalled();
    expect(toJSON).not.toHaveBeenCalled();
  });

  it("preserves ordinary data named toJSON and never invokes its accessor", () => {
    const value = { toJSON: "ordinary data", nested: { toJSON: null } };
    expect(decodeDocument(encode(value), anything)).toEqual(value);
    const getter = vi.fn(() => "data");
    expect(() =>
      encode(Object.defineProperty({}, "toJSON", { get: getter, enumerable: true })),
    ).toThrow(expect.objectContaining({ code: "INVALID_VALUE" }));
    expect(getter).not.toHaveBeenCalled();
  });

  it("requires synchronous exact true and preserves validator failures as causes", async () => {
    const failure = new Error("schema");
    const rejected = Promise.reject(failure);
    // biome-ignore lint/suspicious/noThenProperty: adversarial Promise must still have its rejection observed.
    const disguised = Object.defineProperty(Promise.reject(failure), "then", {
      get() {
        throw new Error("then accessor");
      },
    });
    for (const guard of [
      () => false,
      () => 1,
      () => rejected,
      () => disguised,
      () => {
        throw failure;
      },
    ]) {
      expect(() => encodeDocument({}, guard as unknown as typeof anything)).toThrow(
        expect.objectContaining({ code: "INVALID_VALUE", cause: expect.any(Error) }),
      );
    }
    await Promise.resolve();
    expect(() =>
      encodeDocument({}, (() => {
        throw failure;
      }) as unknown as typeof anything),
    ).toThrow(expect.objectContaining({ cause: failure }));
  });

  it("rejects invalid stored UTF-8, JSON and schema without changing source bytes", () => {
    for (const bytes of [
      new Uint8Array([0xff]),
      new TextEncoder().encode("{"),
      new TextEncoder().encode("null"),
    ]) {
      const copy = bytes.slice();
      expect(() => decodeDocument(bytes, (_value): _value is never => false)).toThrow(
        expect.objectContaining({ code: "INVALID_DATA" }),
      );
      expect(bytes).toEqual(copy);
    }
  });

  it("rejects nonfinite numbers decoded from otherwise valid JSON", () => {
    expect(() => decodeDocument(new TextEncoder().encode("1e400"), anything)).toThrow(
      expect.objectContaining({ code: "INVALID_DATA" }),
    );
  });

  it("encodes keys without normalization, preserves BOM and rejects surrogate collisions", () => {
    for (const key of ["", "\uFEFFkey", "😀", "é", "e\u0301"])
      expect(decodeString(encodeString(key))).toBe(key);
    expect(encodeString("é")).not.toEqual(encodeString("e\u0301"));
    for (const key of ["\ud800", "\udc00", "a\ud800z", 4])
      expect(() => encodeString(key)).toThrow(
        expect.objectContaining({ code: "INVALID_ARGUMENT" }),
      );
    expect(() => decodeString(new Uint8Array([0xc0, 0x80]))).toThrow(
      expect.objectContaining({ code: "INVALID_DATA" }),
    );
  });
});
