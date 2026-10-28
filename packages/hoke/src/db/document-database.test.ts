import { describe, expect, it, vi } from "vitest";
import type { BrowserDB, DBStats } from "./client-contract.js";
import type { DBCollection } from "./collection.js";
import { createDatabase, openDatabase } from "./document-database.js";
import { DocumentSession } from "./document-session.js";

function backend() {
  const stats: DBStats = {
    generation: 1n,
    tableCount: 0,
    walSegments: 0,
    memtableEntries: 0,
    memtableBytes: 0,
  };
  const values = new Map<string, Uint8Array>();
  const collection: DBCollection = {
    get: vi.fn(async (key) => values.get(String(key))),
    put: vi.fn(async (key, value) => {
      values.set(String(key), value);
    }),
    delete: vi.fn(async (key) => {
      values.delete(String(key));
    }),
    scan: vi.fn(async () => []),
    writeBatch: vi.fn(async () => {}),
    subscribe: vi.fn(() => () => {}),
    subscribeRange: vi.fn(() => () => {}),
  };
  const raw = {
    mode: "container-v1" as const,
    stats: vi.fn(async () => stats),
    compact: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    collection: vi.fn(() => collection),
  } as unknown as BrowserDB;
  return { raw, collection, stats };
}
const schema = {
  validate: (value: unknown): value is { count: number } =>
    typeof value === "object" &&
    value !== null &&
    "count" in value &&
    typeof value.count === "number",
};

describe("document database lifecycle and automatic maintenance", () => {
  it("encodes collection names and preserves a value snapshot while queued", async () => {
    const { raw, collection } = backend();
    const db = createDatabase(raw);
    const docs = db.collection("задачи", schema);
    const value = { count: 1 };
    const written = docs.set("first", value);
    value.count = 99;
    const read = docs.get("first");
    await written;
    expect(await read).toEqual({ count: 1 });
    expect(raw.collection).toHaveBeenCalledWith(new TextEncoder().encode("задачи"));
    expect(collection.put).toHaveBeenCalledTimes(1);
    expect(raw.compact).not.toHaveBeenCalled();
    await db.close();
  });

  it.each([
    { tableCount: 32, walSegments: 0 },
    { tableCount: 1, walSegments: 48 },
  ])(
    "maintains sources before a requested mutation at $tableCount tables / $walSegments WALs",
    async (counts) => {
      const { raw, stats } = backend();
      Object.assign(stats, counts);
      const session = new DocumentSession(raw);
      const events: string[] = [];
      vi.mocked(raw.compact).mockImplementation(async () => {
        events.push("compact");
      });
      await session.execute(
        async () => {
          events.push("write");
        },
        undefined,
        true,
      );
      expect(events).toEqual(["compact", "write"]);
      await session.close();
    },
  );

  it("never starts a write after maintenance fails and never retries a rejected write", async () => {
    const { raw, stats } = backend();
    stats.walSegments = 48;
    const session = new DocumentSession(raw);
    const io = new Error("sync outcome unknown");
    vi.mocked(raw.compact).mockRejectedValueOnce(io);
    const mutation = vi.fn(async () => {});
    await expect(session.execute(mutation, undefined, true)).rejects.toBe(io);
    expect(mutation).not.toHaveBeenCalled();
    stats.walSegments = 0;
    const failed = vi.fn(async () => {
      throw io;
    });
    await expect(session.execute(failed, undefined, true)).rejects.toBe(io);
    expect(failed).toHaveBeenCalledTimes(1);
    await expect(session.execute(async () => "diagnostic read")).resolves.toBe("diagnostic read");
    await session.close();
  });

  it("serializes accepted operations and drains them before one idempotent close", async () => {
    const { raw } = backend();
    const session = new DocumentSession(raw);
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const events: string[] = [];
    const first = session.execute(async () => {
      events.push("first");
      await gate;
    });
    const second = session.execute(async () => {
      events.push("second");
    });
    const close = session.close();
    expect(session.close()).toBe(close);
    await expect(session.execute(async () => {})).rejects.toMatchObject({ code: "CLOSED" });
    await Promise.resolve();
    expect(events).toEqual(["first"]);
    expect(raw.close).not.toHaveBeenCalled();
    release?.();
    await Promise.all([first, second, close]);
    expect(events).toEqual(["first", "second"]);
    expect(raw.close).toHaveBeenCalledTimes(1);
  });

  it("avoids maintenance for reads and empty batches and rejects invalid physical input before I/O", async () => {
    const { raw, stats } = backend();
    stats.tableCount = 32;
    const db = createDatabase(raw);
    const docs = db.collection("tasks", schema);
    await docs.get("missing");
    await docs.writeBatch([]);
    await expect(docs.set("a".repeat(65536), { count: 1 })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(raw.stats).not.toHaveBeenCalled();
    expect(raw.compact).not.toHaveBeenCalled();
    const closed = db.close();
    expect(() => docs.subscribe("key", () => {})).toThrow(/closing|closed/);
    expect(() => db.collection("later", schema)).toThrow(/closing|closed/);
    await closed;
  });

  it("does not create a worker for invalid convenience options or pre-aborted opens", async () => {
    await expect(openDatabase("tasks", { mode: "wal-v1" } as never)).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    const signal = { aborted: true, addEventListener() {}, removeEventListener() {} };
    await expect(openDatabase("tasks", { signal })).rejects.toMatchObject({ code: "ABORTED" });
  });
});
