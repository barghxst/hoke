import { afterEach, describe, expect, it, vi } from "vitest";
import type { StringStorage } from "../../src/kv/index.js";
import { KV, KVStorageError, LocalStorageBackend } from "../../src/kv/index.js";
import { MemoryBackend } from "../helpers/memory-backend.js";

function fakeStorage() {
  const data = new Map<string, string>();
  return {
    data,
    getItem: vi.fn((key: string) => data.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => {
      data.set(key, value);
    }),
    removeItem: vi.fn((key: string) => {
      data.delete(key);
    }),
  };
}

const isNumber = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value);
function open(storage: StringStorage, name = "app") {
  return new KV({ name, backend: new LocalStorageBackend({ storage }), validate: isNumber });
}

function expectCode(action: () => unknown, code: string, cause?: unknown) {
  try {
    action();
    expect.unreachable("Expected a storage error");
  } catch (error) {
    expect(error).toBeInstanceOf(KVStorageError);
    expect(error).toMatchObject({ code });
    if (cause !== undefined) expect((error as Error).cause).toBe(cause);
  }
}

afterEach(() => vi.unstubAllGlobals());

describe("localStorage adapter", () => {
  it("persists values and deletions across KV instances", () => {
    const storage = fakeStorage();
    const kv = open(storage);
    kv.set("", 0);
    kv.set("__proto__", 42);
    const reopened = open(storage);
    expect(reopened.get("")).toBe(0);
    expect(reopened.get("__proto__")).toBe(42);
    reopened.delete("__proto__");
    expect(kv.get("__proto__")).toBeUndefined();
    expect(open(storage).get("")).toBe(0);
  });

  it("isolates namespaces, including ambiguous separators", () => {
    const storage = fakeStorage();
    open(storage, "a:b").set("c", 1);
    open(storage, "a").set("b:c", 2);
    expect(open(storage, "a:b").get("c")).toBe(1);
    expect(open(storage, "a").get("b:c")).toBe(2);
    expect(open(storage, "other").get("c")).toBeUndefined();
  });

  it("notifies after successful writes and skips unchanged primitive values", () => {
    const storage = fakeStorage();
    const kv = open(storage);
    const seen: (number | undefined)[] = [];
    kv.subscribe("key", () => seen.push(open(storage).get("key")));
    kv.set("key", 1);
    kv.set("key", 1);
    kv.delete("key");
    kv.delete("key");
    expect(seen).toEqual([1, undefined]);
    expect(storage.setItem).toHaveBeenCalledTimes(1);
  });

  it("round-trips JSON objects and null without retaining caller references", () => {
    type Value = { items: (string | null)[] } | null;
    const validate = (value: unknown): value is Value =>
      value === null ||
      (typeof value === "object" &&
        value !== null &&
        "items" in value &&
        Array.isArray(value.items) &&
        value.items.every((item: unknown) => item === null || typeof item === "string"));
    const storage = fakeStorage();
    const kv = new KV({ name: "objects", backend: new LocalStorageBackend({ storage }), validate });
    const value = { items: ["hello", null] };
    kv.set("key", value);
    value.items.push("outside");
    expect(kv.get("key")).toEqual({ items: ["hello", null] });
    kv.set("key", null);
    expect(kv.get("key")).toBeNull();
  });

  it.each(["{broken", '"wrong type"', "1e400"])(
    "reports corrupt data without overwriting it: %s",
    (raw) => {
      const storage = fakeStorage();
      const kv = open(storage);
      kv.set("key", 1);
      const persistedKey = [...storage.data.keys()][0]!;
      storage.data.set(persistedKey, raw);
      expectCode(() => kv.get("key"), "INVALID_DATA");
      expectCode(() => kv.set("key", 2), "INVALID_DATA");
      expect(storage.data.get(persistedKey)).toBe(raw);
    },
  );

  it("validates the schema on every read, including external edits", () => {
    const storage = fakeStorage();
    const kv = open(storage);
    kv.set("key", 1);
    storage.data.set([...storage.data.keys()][0]!, "2");
    expect(kv.get("key")).toBe(2);
  });

  it("reports read failures with the original cause", () => {
    const storage = fakeStorage();
    const failure = new Error("access denied");
    storage.getItem.mockImplementation(() => {
      throw failure;
    });
    expectCode(() => open(storage).get("key"), "READ_FAILED", failure);
  });

  it("keeps the previous value and emits no notification when a write fails", () => {
    const storage = fakeStorage();
    const kv = open(storage);
    kv.set("key", 1);
    const listener = vi.fn();
    kv.subscribe("key", listener);
    const failure = new Error("quota exceeded");
    storage.setItem.mockImplementation(() => {
      throw failure;
    });
    expectCode(() => kv.set("key", 2), "WRITE_FAILED", failure);
    expect(kv.get("key")).toBe(1);
    expect(listener).not.toHaveBeenCalled();
  });

  it("keeps the value and emits no notification when deletion fails", () => {
    const storage = fakeStorage();
    const kv = open(storage);
    kv.set("key", 1);
    const listener = vi.fn();
    kv.subscribe("key", listener);
    const failure = new Error("access denied");
    storage.removeItem.mockImplementation(() => {
      throw failure;
    });
    expectCode(() => kv.delete("key"), "DELETE_FAILED", failure);
    expect(kv.get("key")).toBe(1);
    expect(listener).not.toHaveBeenCalled();
  });

  it.each([
    undefined,
    NaN,
    Infinity,
    -0,
    BigInt(1),
    () => {},
    Symbol("value"),
    { nested: undefined },
    new Date(),
    new Map(),
    [undefined],
    Array(1),
    Object.defineProperty({}, "hidden", { value: 1 }),
    { [Symbol("key")]: 1 },
    Object.assign([], { custom: 1 }),
    {
      get value() {
        return 1;
      },
    },
  ])("rejects values that JSON would lose or alter: %s", (value) => {
    const storage = fakeStorage();
    const kv = new KV({ name: "json", backend: new LocalStorageBackend({ storage }) });
    const listener = vi.fn();
    kv.subscribe("key", listener);
    expectCode(() => kv.set("key", value), "INVALID_VALUE");
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(listener).not.toHaveBeenCalled();
  });

  it("rejects cycles but accepts repeated references as independent JSON objects", () => {
    const storage = fakeStorage();
    const kv = new KV({ name: "json", backend: new LocalStorageBackend({ storage }) });
    const cycle: Record<string, unknown> = {};
    cycle.self = cycle;
    expectCode(() => kv.set("key", cycle), "INVALID_VALUE");
    const shared = { n: 1 };
    kv.set("key", [shared, shared]);
    expect(kv.get("key")).toEqual([{ n: 1 }, { n: 1 }]);
  });

  it("checks the validator before writing, even if TypeScript is bypassed", () => {
    const storage = fakeStorage();
    expectCode(() => open(storage).set("key", "invalid" as unknown as number), "INVALID_VALUE");
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it("uses browser localStorage when no storage override is supplied", () => {
    const storage = fakeStorage();
    vi.stubGlobal("localStorage", storage);
    const kv = new KV({ name: "browser", validate: isNumber });
    kv.set("key", 1);
    expect(storage.setItem).toHaveBeenCalledTimes(1);
  });

  it("fails explicitly when localStorage is unavailable; memory still works", () => {
    vi.stubGlobal("localStorage", undefined);
    expectCode(() => new KV({ name: "browser", validate: isNumber }), "UNAVAILABLE");
    const kv = new KV<number>({ name: "memory", backend: new MemoryBackend() });
    kv.set("key", 1);
    expect(kv.get("key")).toBe(1);
  });

  it("wraps errors thrown by the localStorage getter", () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
    const failure = new Error("SecurityError");
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      get() {
        throw failure;
      },
    });
    try {
      expectCode(() => new KV({ name: "browser", validate: isNumber }), "UNAVAILABLE", failure);
    } finally {
      if (original) Object.defineProperty(globalThis, "localStorage", original);
      else Reflect.deleteProperty(globalThis, "localStorage");
    }
  });

  it("rejects an empty namespace", () => {
    expect(
      () =>
        new KV({
          name: "",
          backend: new LocalStorageBackend({ storage: fakeStorage() }),
          validate: isNumber,
        }),
    ).toThrow(TypeError);
  });
});

describe("localStorage snapshots", () => {
  type Value = { count: number };
  const validate = (value: unknown): value is Value =>
    typeof value === "object" && value !== null && "count" in value && isNumber(value.count);
  const persistedKey = 'stowtree:["snapshots","key"]';
  function setup() {
    const storage = fakeStorage();
    const adapter = new KV({
      name: "snapshots",
      backend: new LocalStorageBackend({ storage }),
      validate,
    });
    adapter.set("key", { count: 1 });
    return { storage, adapter };
  }

  it("reuses object values and entries while still reading the backend each time", () => {
    const { storage, adapter } = setup();
    const first = adapter.get("key")!;
    expect(adapter.get("key")).toBe(first);
    expect(adapter.get("key")!).toBe(first);
    const kv = adapter;
    expect(kv.get("key")).toBe(first);
    expect(kv.get("key")).toBe(first);
    expect(storage.getItem).toHaveBeenCalledTimes(6);
  });

  it("detects external raw changes, including equivalent JSON with different formatting", () => {
    const { storage, adapter } = setup();
    const first = adapter.get("key")!;
    storage.data.set(persistedKey, '{ "count": 1 }');
    const reformatted = adapter.get("key")!;
    expect(reformatted).toEqual(first);
    expect(reformatted).not.toBe(first);
    expect(adapter.get("key")).toBe(reformatted);
    storage.data.set(persistedKey, '{"count":2}');
    const changed = adapter.get("key")!;
    expect(changed).toEqual({ count: 2 });
    expect(changed).not.toBe(reformatted);
  });

  it.each(["{broken", '{"count":"invalid"}'])(
    "throws on corrupted data and recovers after restoration: %s",
    (raw) => {
      const { storage, adapter } = setup();
      adapter.get("key");
      storage.data.set(persistedKey, raw);
      expectCode(() => adapter.get("key"), "INVALID_DATA");
      expectCode(() => adapter.get("key"), "INVALID_DATA");
      storage.data.set(persistedKey, '{"count":1}');
      const restored = adapter.get("key")!;
      expect(restored).toEqual({ count: 1 });
      expect(adapter.get("key")).toBe(restored);
    },
  );

  it("throws on backend read failure and reads fresh data after recovery", () => {
    const { storage, adapter } = setup();
    adapter.get("key");
    const failure = new Error("temporarily unavailable");
    storage.getItem.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.get("key"), "READ_FAILED", failure);
    storage.data.set(persistedKey, '{"count":2}');
    const recovered = adapter.get("key")!;
    expect(recovered).toEqual({ count: 2 });
    expect(adapter.get("key")).toBe(recovered);
  });

  it("runs changing validators even for cached values", () => {
    const storage = fakeStorage();
    let accepted = true;
    const check = vi.fn((value: unknown): value is Value => accepted && validate(value));
    const adapter = new KV({
      name: "snapshots",
      backend: new LocalStorageBackend({ storage }),
      validate: (value: unknown): value is Value => check(value),
    });
    adapter.set("key", { count: 1 });
    adapter.get("key");
    accepted = false;
    expectCode(() => adapter.get("key"), "INVALID_DATA");
    accepted = true;
    const recovered = adapter.get("key")!;
    expect(adapter.get("key")).toBe(recovered);
    expect(check).toHaveBeenCalledTimes(5);
    const failure = new Error("validator failed");
    check.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.get("key"), "INVALID_DATA", failure);
  });

  it("retains the previous snapshot after failed or invalid writes and failed removal", () => {
    const { storage, adapter } = setup();
    const first = adapter.get("key")!;
    const failure = new Error("quota exceeded");
    storage.setItem.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.set("key", { count: 2 }), "WRITE_FAILED", failure);
    expect(adapter.get("key")).toBe(first);
    expectCode(() => adapter.set("key", { count: "bad" } as unknown as Value), "INVALID_VALUE");
    expect(adapter.get("key")).toBe(first);
    storage.removeItem.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.delete("key"), "DELETE_FAILED", failure);
    expect(adapter.get("key")).toBe(first);
  });

  it("reuses a snapshot after writing the same raw JSON without retaining input references", () => {
    const { adapter } = setup();
    const first = adapter.get("key")!;
    adapter.set("key", { count: 1 });
    expect(adapter.get("key")).toBe(first);
    const next = { count: 2 };
    adapter.set("key", next);
    next.count = 3;
    const second = adapter.get("key")!;
    expect(second).toEqual({ count: 2 });
    expect(second).not.toBe(next);
    expect(second).not.toBe(first);
    expect(adapter.get("key")).toBe(second);
  });

  it("invalidates a removed key even if it is recreated with identical JSON before reading", () => {
    const { adapter } = setup();
    const first = adapter.get("key")!;
    adapter.delete("key");
    adapter.set("key", { count: 1 });
    expect(adapter.get("key")).not.toBe(first);
    adapter.delete("key");
    expect(adapter.get("key")).toBeUndefined();
    adapter.set("key", { count: 2 });
    expect(adapter.get("key")!).toEqual({ count: 2 });
  });

  it("detects external deletion and recreates a snapshot after the key returns", () => {
    const { storage, adapter } = setup();
    const first = adapter.get("key")!;
    storage.data.delete(persistedKey);
    expect(adapter.get("key")).toBeUndefined();
    storage.data.set(persistedKey, '{"count":1}');
    expect(adapter.get("key")!).toEqual(first);
    expect(adapter.get("key")).not.toBe(first);
  });

  it("keeps independent snapshots for different keys and adapter namespaces", () => {
    const { storage, adapter } = setup();
    const other = new KV({
      name: "other",
      backend: new LocalStorageBackend({ storage }),
      validate,
    });
    adapter.set("other", { count: 2 });
    other.set("key", { count: 3 });
    const first = adapter.get("key")!;
    const second = adapter.get("other")!;
    const third = other.get("key")!;
    expect([first.count, second.count, third.count]).toEqual([1, 2, 3]);
    expect(adapter.get("key")).toBe(first);
    expect(adapter.get("other")).toBe(second);
    expect(other.get("key")).toBe(third);
  });
});

it("rejects async injected storage mutations directly and through KV", async () => {
  const storage = fakeStorage();
  const backend = new LocalStorageBackend({ storage });
  const kv = new KV({ name: "async-storage", backend });
  kv.set("key", 1);
  const listener = vi.fn();
  kv.subscribe("key", listener);
  storage.setItem.mockImplementation((async () => {
    throw new Error("async injected write");
  }) as never);
  expect(() => backend.write("direct", "1")).toThrow(TypeError);
  expectCode(() => kv.set("key", 2), "WRITE_FAILED");
  storage.removeItem.mockImplementation((async () => {
    throw new Error("async injected delete");
  }) as never);
  expect(() => backend.remove("direct")).toThrow(TypeError);
  expectCode(() => kv.delete("key"), "DELETE_FAILED");
  expect(kv.get("key")).toBe(1);
  expect(listener).not.toHaveBeenCalled();
  await new Promise((resolve) => setImmediate(resolve));
});

it("rejects async injected storage reads and observes rejected promises", async () => {
  const storage = fakeStorage();
  storage.getItem.mockImplementation((() =>
    Promise.reject(new Error("async injected read"))) as never);
  const backend = new LocalStorageBackend({ storage });
  const kv = new KV({ name: "async-storage", backend });
  expect(() => backend.read("direct")).toThrow(TypeError);
  expectCode(() => kv.get("key"), "READ_FAILED");
  await new Promise((resolve) => setImmediate(resolve));
});
