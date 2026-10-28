import { describe, expect, it } from "vitest";
import { MAX_BATCH_BYTES, normalizeBatch, packBatch } from "./batch.js";

const bytes = (...values: number[]) => new Uint8Array(values);

describe("database batch normalization", () => {
  it("preserves repeated-key order and snapshots each input independently", () => {
    const key = bytes(0, 255),
      value = bytes(7);
    const input = [
      { type: "put" as const, key, value },
      { type: "delete" as const, key },
    ];
    const owned = normalizeBatch(input);
    key.fill(9);
    value.fill(8);
    input.pop();
    expect(owned).toEqual([
      { type: "put", key: bytes(0, 255), value: bytes(7) },
      { type: "delete", key: bytes(0, 255) },
    ]);
    owned[0]?.key.fill(3);
    expect(owned[1]?.key).toEqual(bytes(0, 255));
    const packet = packBatch([
      { type: "put", key: bytes(2), value: bytes() },
      { type: "delete", key: bytes(2) },
    ]);
    expect(Array.from(packet)).toEqual([
      2, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0, 2,
    ]);
  });
  it("rejects malformed, excessive, detached and oversized input", () => {
    const invalid: unknown[] = [
      null,
      {},
      true,
      new Array(1025),
      new Array(1),
      [{ type: "clear", key: bytes() }],
      [{ type: "put", key: bytes(), value: "bad" }],
      [{ type: "delete", key: new Uint8Array(65537) }],
      [{ type: "put", key: bytes(), value: new Uint8Array(16 * 1024 * 1024 + 1) }],
    ];
    const key = bytes(1);
    structuredClone(key.buffer, { transfer: [key.buffer] });
    invalid.push([{ type: "delete", key }]);
    for (const input of invalid) expect(() => normalizeBatch(input)).toThrow(TypeError);
  });
  it("counts every duplicate input against byte caps and retains exact boundaries", () => {
    const value = new Uint8Array(16 * 1024 * 1024);
    expect(
      normalizeBatch([
        { type: "put", key: bytes(), value },
        { type: "put", key: bytes(), value },
      ]),
    ).toHaveLength(2);
    expect(() =>
      normalizeBatch([
        { type: "put", key: bytes(1), value },
        { type: "put", key: bytes(), value },
      ]),
    ).toThrow(/32 MiB/);
    expect(MAX_BATCH_BYTES).toBe(value.length * 2);
    expect(
      normalizeBatch(Array.from({ length: 1024 }, () => ({ type: "delete", key: bytes() }))),
    ).toHaveLength(1024);
    expect(packBatch([])).toEqual(bytes(0, 0, 0, 0));
  });
});
