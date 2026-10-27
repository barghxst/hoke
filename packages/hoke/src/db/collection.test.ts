import { describe, expect, it, vi } from "vitest";
import { createCollection } from "./collection.js";
import { type BrowserDB, DBError } from "./index.js";
import { type DBListener, type DBRange, WatchRegistry } from "./notifications.js";
import { type DBEntry, type DBScanOptions, normalizeScanOptions } from "./scan.js";

function database() {
  const entries = new Map<string, Uint8Array>();
  const token = (key: Uint8Array) => Array.from(key).join(",");
  let closed = false;
  const watchers = new WatchRegistry();
  function open() {
    if (closed) throw new DBError("CLOSED", "Database is closed");
  }
  const db: BrowserDB = {
    mode: "wal-v1",
    get: vi.fn(async (key: Uint8Array) => {
      open();
      return entries.get(token(key));
    }),
    put: vi.fn(async (key: Uint8Array, value: Uint8Array) => {
      open();
      entries.set(token(key), new Uint8Array(value));
      const error = watchers.notify(key);
      if (error) throw error;
    }),
    delete: vi.fn(async (key: Uint8Array) => {
      open();
      entries.delete(token(key));
      const error = watchers.notify(key);
      if (error) throw error;
    }),
    scan: vi.fn(async (_options?: DBScanOptions): Promise<DBEntry[]> => {
      open();
      return [];
    }),
    subscribe: vi.fn((key: Uint8Array, listener: DBListener) => {
      open();
      return watchers.subscribe(key, listener);
    }),
    subscribeRange: vi.fn((range: DBRange, listener: DBListener) => {
      open();
      return watchers.subscribeRange(range, listener);
    }),
    collection(prefix) {
      open();
      return createCollection(db, prefix);
    },
    async close() {
      closed = true;
      watchers.clear();
    },
  };
  return { db, entries };
}
const bytes = (...values: number[]) => new Uint8Array(values);

describe("byte collections", () => {
  it("length-frames binary prefixes and isolates empty namespaces and keys", async () => {
    const { db, entries } = database();
    const empty = db.collection(bytes());
    const first = db.collection(bytes(0));
    const second = db.collection(bytes(0, 1));
    await empty.put(bytes(), bytes(5));
    await first.put(bytes(1), bytes(6));
    await second.put(bytes(), bytes(7));
    await db.put(bytes(), bytes(8));
    expect(entries.size).toBe(4);
    expect(db.put).toHaveBeenNthCalledWith(1, bytes(1, 0, 0, 0, 0), bytes(5));
    expect(db.put).toHaveBeenNthCalledWith(2, bytes(1, 0, 0, 0, 1, 0, 1), bytes(6));
    expect(db.put).toHaveBeenNthCalledWith(3, bytes(1, 0, 0, 0, 2, 0, 1), bytes(7));
    await expect(empty.get(bytes())).resolves.toEqual(bytes(5));
    await expect(first.get(bytes(1))).resolves.toEqual(bytes(6));
    await expect(second.get(bytes())).resolves.toEqual(bytes(7));
    await first.delete(bytes(1));
    await expect(first.get(bytes(1))).resolves.toBeUndefined();
    await expect(second.get(bytes())).resolves.toEqual(bytes(7));
  });

  it("captures namespace and point keys before awaiting", async () => {
    const { db } = database();
    const prefix = bytes(9);
    const collection = db.collection(prefix);
    prefix[0] = 7;
    const key = bytes(8);
    const reading = collection.get(key);
    key[0] = 6;
    expect(db.get).toHaveBeenCalledWith(bytes(1, 0, 0, 0, 1, 9, 8));
    await reading;
  });

  it("translates local bounds and prefix, preserves inclusivity and strips physical namespace", async () => {
    const { db } = database();
    const namespace = [1, 0, 0, 0, 2, 9, 0];
    vi.mocked(db.scan).mockResolvedValue([{ key: bytes(...namespace, 2, 3), value: bytes(4) }]);
    const collection = db.collection(bytes(9, 0));
    const local = bytes(2);
    const scanning = collection.scan({
      start: { key: bytes(1), inclusive: false },
      end: { key: bytes(3), inclusive: true },
      prefix: local,
      reverse: true,
      limit: 1,
    });
    local[0] = 8;
    expect(db.scan).toHaveBeenCalledWith({
      start: { key: bytes(...namespace, 1), inclusive: false },
      end: { key: bytes(...namespace, 3), inclusive: true },
      prefix: bytes(...namespace, 2),
      reverse: true,
      limit: 1,
    });
    await expect(scanning).resolves.toEqual([{ key: bytes(2, 3), value: bytes(4) }]);
    await collection.scan();
    expect(db.scan).toHaveBeenLastCalledWith({
      prefix: bytes(...namespace),
      reverse: false,
      limit: 100,
    });
    await collection.scan({ limit: 0, prefix: bytes() });
    expect(db.scan).toHaveBeenLastCalledWith({
      prefix: bytes(...namespace),
      reverse: false,
      limit: 0,
    });
  });

  it("allows the exact physical key cap and rejects oversized inputs before DB calls", async () => {
    const { db } = database();
    const collection = db.collection(new Uint8Array(65531));
    await collection.get(bytes());
    expect(vi.mocked(db.get).mock.calls[0]?.[0]).toHaveLength(65536);
    vi.mocked(db.get).mockClear();
    await expect(collection.get(bytes(1))).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    await expect(collection.put(bytes(1), bytes())).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    await expect(collection.delete(bytes(1))).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    for (const options of [
      { prefix: bytes(1) },
      { start: { key: bytes(1), inclusive: true } },
      { end: { key: bytes(1), inclusive: false } },
      { limit: 1001 },
      { unknown: true },
    ]) {
      await expect(collection.scan(options as DBScanOptions)).rejects.toMatchObject({
        code: "INVALID_ARGUMENT",
      });
    }
    expect(db.get).not.toHaveBeenCalled();
    expect(db.put).not.toHaveBeenCalled();
    expect(db.delete).not.toHaveBeenCalled();
    expect(db.scan).not.toHaveBeenCalled();
    expect(() => db.collection(new Uint8Array(65532))).toThrow(DBError);
    expect(() => db.collection("name" as unknown as Uint8Array)).toThrow(DBError);
  });

  it("rejects detached namespace/key/bounds without posting", async () => {
    const { db } = database();
    const detached = bytes(1);
    structuredClone(detached.buffer, { transfer: [detached.buffer] });
    expect(() => db.collection(detached)).toThrow(DBError);
    const collection = db.collection(bytes());
    await expect(collection.get(detached)).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    await expect(collection.scan({ prefix: detached })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(db.get).not.toHaveBeenCalled();
    expect(db.scan).not.toHaveBeenCalled();
  });

  it("shares DB closure and does not create its own owner", async () => {
    const { db } = database();
    const collection = db.collection(bytes(1));
    await db.close();
    await expect(collection.get(bytes())).rejects.toMatchObject({ code: "CLOSED" });
    await expect(collection.put(bytes(), bytes())).rejects.toMatchObject({ code: "CLOSED" });
    await expect(collection.delete(bytes())).rejects.toMatchObject({ code: "CLOSED" });
    await expect(collection.scan()).rejects.toMatchObject({ code: "CLOSED" });
    expect(() => db.collection(bytes())).toThrow(DBError);
    expect(() => collection.subscribe(bytes(), vi.fn())).toThrow(DBError);
    expect(() => collection.subscribeRange({}, vi.fn())).toThrow(DBError);
  });

  it("preserves big-endian namespace length across the one-byte boundary", async () => {
    const { db } = database();
    await db.collection(new Uint8Array(256)).get(bytes());
    const key = vi.mocked(db.get).mock.calls[0]?.[0];
    expect(key?.slice(0, 5)).toEqual(bytes(1, 0, 0, 1, 0));
  });

  it("normalizes translated local scans to copied physical byte options", async () => {
    const { db } = database();
    await db.collection(bytes(4)).scan({
      start: { key: bytes(), inclusive: true },
      end: { key: bytes(255), inclusive: false },
    });
    const translated = vi.mocked(db.scan).mock.calls[0]?.[0];
    const normalized = normalizeScanOptions(translated);
    expect(normalized.start?.key).toEqual(bytes(1, 0, 0, 0, 1, 4));
    expect(normalized.end?.key).toEqual(bytes(1, 0, 0, 0, 1, 4, 255));
  });
});

describe("collection subscriptions", () => {
  it("scopes whole ranges, exact keys and relative bounds to their namespace", async () => {
    const { db } = database();
    const first = db.collection(bytes(1));
    const second = db.collection(bytes(1, 2));
    const exact = vi.fn();
    const range = vi.fn();
    const all = vi.fn();
    first.subscribe(bytes(2), exact);
    first.subscribeRange(
      {
        start: { key: bytes(2), inclusive: true },
        end: { key: bytes(4), inclusive: false },
        prefix: bytes(),
      },
      range,
    );
    first.subscribeRange({}, all);
    await first.put(bytes(2), bytes(9));
    await first.put(bytes(4), bytes(9));
    await second.put(bytes(2), bytes(9));
    await db.put(bytes(2), bytes(9));
    await first.delete(bytes(2));
    expect(exact).toHaveBeenCalledTimes(2);
    expect(range).toHaveBeenCalledTimes(2);
    expect(all).toHaveBeenCalledTimes(3);
    expect(db.subscribeRange).toHaveBeenLastCalledWith({ prefix: bytes(1, 0, 0, 0, 1, 1) }, all);
  });

  it("copies namespace, keys and range metadata and supports idempotent unsubscribe", async () => {
    const { db } = database();
    const namespace = bytes(7);
    const collection = db.collection(namespace);
    const key = bytes(3);
    const prefix = bytes(3);
    const start = { key: bytes(3), inclusive: true };
    const exact = vi.fn();
    const range = vi.fn();
    const off = collection.subscribe(key, exact);
    const offRange = collection.subscribeRange({ prefix, start }, range);
    namespace[0] = 8;
    key[0] = 9;
    prefix[0] = 9;
    start.key[0] = 9;
    start.inclusive = false;
    await collection.put(bytes(3), bytes());
    expect(exact).toHaveBeenCalledOnce();
    expect(range).toHaveBeenCalledOnce();
    off();
    off();
    offRange();
    offRange();
    await collection.delete(bytes(3));
    expect(exact).toHaveBeenCalledOnce();
    expect(range).toHaveBeenCalledOnce();
    await db.close();
    off();
    offRange();
  });

  it("rejects scan-only options and oversized physical range keys synchronously", () => {
    const { db } = database();
    const collection = db.collection(new Uint8Array(65531));
    for (const range of [
      { limit: 1 },
      { reverse: false },
      { prefix: bytes(1) },
      { start: { key: bytes(1), inclusive: true } },
      null,
    ]) {
      expect(() => collection.subscribeRange(range as DBRange, vi.fn())).toThrow(DBError);
    }
    expect(db.subscribeRange).not.toHaveBeenCalled();
    expect(() => collection.subscribe(bytes(1), vi.fn())).toThrow(DBError);
    expect(db.subscribe).not.toHaveBeenCalled();
  });
});
