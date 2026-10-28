import { describe, expect, it, vi } from "vitest";
import type { DBOperationOptions } from "./client-contract.js";
import type { DBCollection } from "./collection.js";
import { encodeString } from "./document-codec.js";
import { createDocumentCollection } from "./document-collection.js";
import type { CollectionOptions } from "./document-contract.js";

type User = { name: string };
const options: CollectionOptions<User> = {
  validate: (value): value is User =>
    typeof value === "object" && value !== null && typeof (value as User).name === "string",
};
function harness(queued = false) {
  const raw: DBCollection = {
    get: vi.fn(async () => undefined),
    put: vi.fn(async () => {}),
    delete: vi.fn(async () => {}),
    writeBatch: vi.fn(async () => {}),
    scan: vi.fn(async () => []),
    subscribe: vi.fn(() => () => {}),
    subscribeRange: vi.fn(() => () => {}),
  };
  const tasks: Array<() => Promise<void>> = [];
  const execute = vi.fn(
    <R>(
      operation: () => Promise<R>,
      _options?: DBOperationOptions,
      _mutation?: boolean,
    ): Promise<R> =>
      queued
        ? new Promise<R>((resolve, reject) => {
            tasks.push(async () => {
              try {
                resolve(await operation());
              } catch (error) {
                reject(error);
              }
            });
          })
        : operation(),
  );
  return {
    raw,
    execute,
    tasks,
    users: createDocumentCollection(
      raw,
      options,
      execute as <R>(
        operation: () => Promise<R>,
        options?: DBOperationOptions,
        mutation?: boolean,
      ) => Promise<R>,
    ),
  };
}

describe("typed document collection", () => {
  it("validates and snapshots documents and signals before queued writes", async () => {
    const { raw, tasks, users } = harness(true);
    const value = { name: "Alice" };
    const first = new AbortController();
    const second = new AbortController();
    const operationOptions = { signal: first.signal };
    const result = users.set("alice", value, operationOptions);
    value.name = "Bob";
    operationOptions.signal = second.signal;
    await tasks[0]!();
    await result;
    expect(raw.put).toHaveBeenCalledWith(
      encodeString("alice"),
      new TextEncoder().encode('{"name":"Alice"}'),
      { signal: first.signal },
    );
  });

  it("prepares the whole ordered batch before execution, and submits no partial invalid batch", async () => {
    const { raw, execute, users } = harness();
    await expect(
      users.writeBatch([
        { type: "set", key: "a", value: { name: "A" } },
        { type: "set", key: "b", value: { name: 3 } as unknown as User },
      ]),
    ).rejects.toMatchObject({ code: "INVALID_VALUE" });
    expect(execute).not.toHaveBeenCalled();
    expect(raw.writeBatch).not.toHaveBeenCalled();
    await users.writeBatch([
      { type: "set", key: "a", value: { name: "A" } },
      { type: "delete", key: "a" },
      { type: "set", key: "a", value: { name: "B" } },
    ]);
    expect(vi.mocked(raw.writeBatch).mock.calls[0]![0].map((operation) => operation.type)).toEqual([
      "put",
      "delete",
      "put",
    ]);
  });

  it("copies batch inputs before queued dispatch and handles empty batch without maintenance", async () => {
    const { raw, tasks, users, execute } = harness(true);
    const value = { name: "A" };
    const batch = [{ type: "set" as const, key: "a", value }];
    const result = users.writeBatch(batch);
    value.name = "B";
    batch[0]!.key = "b";
    batch.length = 0;
    await tasks[0]!();
    await result;
    expect(vi.mocked(raw.writeBatch).mock.calls[0]![0]).toEqual([
      { type: "put", key: encodeString("a"), value: new TextEncoder().encode('{"name":"A"}') },
    ]);
    const empty = users.writeBatch([]);
    await tasks[1]!();
    await empty;
    expect(execute.mock.calls[1]![2]).toBe(false);
  });

  it("reads fresh frozen snapshots and preserves absent values", async () => {
    const { raw, users } = harness();
    expect(await users.get("missing")).toBeUndefined();
    vi.mocked(raw.get).mockResolvedValue(new TextEncoder().encode('{"name":"A"}'));
    const first = await users.get("a");
    const second = await users.get("a");
    expect(first).toEqual({ name: "A" });
    expect(first).not.toBe(second);
    expect(Object.isFrozen(first)).toBe(true);
    vi.mocked(raw.get).mockResolvedValue(new TextEncoder().encode('{"name":3}'));
    await expect(users.get("a")).rejects.toMatchObject({ code: "INVALID_DATA" });
    expect(raw.put).not.toHaveBeenCalled();
    expect(raw.delete).not.toHaveBeenCalled();
  });

  it("translates Unicode bounds and snapshots list inputs before queueing", async () => {
    const { raw, users, tasks } = harness(true);
    const scan = {
      start: { key: "é", inclusive: false },
      end: { key: "😀", inclusive: true },
      prefix: "",
      reverse: true,
      limit: 7,
    };
    vi.mocked(raw.scan).mockResolvedValue([
      { key: encodeString("\uFEFFa"), value: new TextEncoder().encode('{"name":"A"}') },
    ]);
    const result = users.list(scan);
    scan.start.key = "z";
    scan.limit = 1;
    await tasks[0]!();
    expect(await result).toEqual([{ key: "\uFEFFa", value: { name: "A" } }]);
    expect(raw.scan).toHaveBeenCalledWith(
      {
        start: { key: encodeString("é"), inclusive: false },
        end: { key: encodeString("😀"), inclusive: true },
        prefix: new Uint8Array(),
        reverse: true,
        limit: 7,
      },
      undefined,
    );
    await expect(
      users.list({ start: { key: "😀", inclusive: true }, end: { key: "é", inclusive: true } }),
    ).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
  });

  it("checks physical namespace budgets before queueing any operation", async () => {
    const { raw, execute } = harness();
    const collection = createDocumentCollection(
      raw,
      options,
      execute as <R>(operation: () => Promise<R>) => Promise<R>,
      65536,
    );
    await expect(collection.set("a", { name: "A" })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    await expect(collection.list({ prefix: "a" })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    await expect(collection.writeBatch([{ type: "delete", key: "a" }])).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(() =>
      collection.subscribeRange({ end: { key: "a", inclusive: true } }, () => {}),
    ).toThrow();
    expect(execute).not.toHaveBeenCalled();
    const batchCollection = createDocumentCollection(
      raw,
      options,
      execute as <R>(operation: () => Promise<R>) => Promise<R>,
      65500,
    );
    await expect(
      batchCollection.writeBatch(
        Array.from({ length: 1024 }, () => ({ type: "delete" as const, key: "" })),
      ),
    ).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    expect(execute).not.toHaveBeenCalled();
  });

  it("translates subscriptions immediately and rejects invalid ranges or keys", async () => {
    const { raw, users } = harness();
    const listener = () => {};
    users.subscribe("😀", listener);
    users.subscribeRange({ prefix: "user:" }, listener);
    expect(raw.subscribe).toHaveBeenCalledWith(encodeString("😀"), listener);
    expect(raw.subscribeRange).toHaveBeenCalledWith({ prefix: encodeString("user:") }, listener);
    expect(() => users.subscribeRange({ limit: 1 } as never, listener)).toThrow(
      expect.objectContaining({ code: "INVALID_ARGUMENT" }),
    );
    await expect(users.set("\ud800", { name: "A" })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(raw.put).not.toHaveBeenCalled();
  });
});
