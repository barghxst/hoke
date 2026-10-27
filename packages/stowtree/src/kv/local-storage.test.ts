import { afterEach, describe, expect, it, vi } from "vitest";
import type { StringStorage } from "./index.js";
import { createKV, createLocalStorage, KVStorageError } from "./index.js";

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
  return createKV({ storage: createLocalStorage({ name, storage, validate: isNumber }) });
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
    const kv = createKV({ storage: createLocalStorage({ name: "objects", storage, validate }) });
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
    const kv = createKV({
      storage: createLocalStorage({ name: "json", storage, validate: (v): v is unknown => true }),
    });
    const listener = vi.fn();
    kv.subscribe("key", listener);
    expectCode(() => kv.set("key", value), "INVALID_VALUE");
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(listener).not.toHaveBeenCalled();
  });

  it("rejects cycles but accepts repeated references as independent JSON objects", () => {
    const storage = fakeStorage();
    const kv = createKV({
      storage: createLocalStorage({ name: "json", storage, validate: (v): v is unknown => true }),
    });
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
    const kv = createKV({ storage: createLocalStorage({ name: "browser", validate: isNumber }) });
    kv.set("key", 1);
    expect(storage.setItem).toHaveBeenCalledTimes(1);
  });

  it("fails explicitly when localStorage is unavailable; memory still works", () => {
    vi.stubGlobal("localStorage", undefined);
    expectCode(() => createLocalStorage({ name: "browser", validate: isNumber }), "UNAVAILABLE");
    const kv = createKV<number>();
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
      expectCode(
        () => createLocalStorage({ name: "browser", validate: isNumber }),
        "UNAVAILABLE",
        failure,
      );
    } finally {
      if (original) Object.defineProperty(globalThis, "localStorage", original);
      else Reflect.deleteProperty(globalThis, "localStorage");
    }
  });

  it("rejects an empty namespace", () => {
    expect(() =>
      createLocalStorage({ name: "", storage: fakeStorage(), validate: isNumber }),
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
    const adapter = createLocalStorage({ name: "snapshots", storage, validate });
    adapter.write("key", { count: 1 });
    return { storage, adapter };
  }

  it("reuses object values and entries while still reading the backend each time", () => {
    const { storage, adapter } = setup();
    const first = adapter.read("key")!;
    expect(adapter.read("key")).toBe(first);
    expect(adapter.read("key")!.value).toBe(first.value);
    const kv = createKV({ storage: adapter });
    expect(kv.get("key")).toBe(first.value);
    expect(kv.get("key")).toBe(first.value);
    expect(storage.getItem).toHaveBeenCalledTimes(5);
  });

  it("detects external raw changes, including equivalent JSON with different formatting", () => {
    const { storage, adapter } = setup();
    const first = adapter.read("key")!;
    storage.data.set(persistedKey, '{ "count": 1 }');
    const reformatted = adapter.read("key")!;
    expect(reformatted.value).toEqual(first.value);
    expect(reformatted).not.toBe(first);
    expect(adapter.read("key")).toBe(reformatted);
    storage.data.set(persistedKey, '{"count":2}');
    const changed = adapter.read("key")!;
    expect(changed.value).toEqual({ count: 2 });
    expect(changed.value).not.toBe(reformatted.value);
  });

  it.each(["{broken", '{"count":"invalid"}'])(
    "throws on corrupted data and recovers after restoration: %s",
    (raw) => {
      const { storage, adapter } = setup();
      adapter.read("key");
      storage.data.set(persistedKey, raw);
      expectCode(() => adapter.read("key"), "INVALID_DATA");
      expectCode(() => adapter.read("key"), "INVALID_DATA");
      storage.data.set(persistedKey, '{"count":1}');
      const restored = adapter.read("key")!;
      expect(restored.value).toEqual({ count: 1 });
      expect(adapter.read("key")).toBe(restored);
    },
  );

  it("throws on backend read failure and reads fresh data after recovery", () => {
    const { storage, adapter } = setup();
    adapter.read("key");
    const failure = new Error("temporarily unavailable");
    storage.getItem.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.read("key"), "READ_FAILED", failure);
    storage.data.set(persistedKey, '{"count":2}');
    const recovered = adapter.read("key")!;
    expect(recovered.value).toEqual({ count: 2 });
    expect(adapter.read("key")).toBe(recovered);
  });

  it("runs changing validators even for cached values", () => {
    const storage = fakeStorage();
    let accepted = true;
    const check = vi.fn((value: unknown): value is Value => accepted && validate(value));
    const adapter = createLocalStorage({
      name: "snapshots",
      storage,
      validate: (value: unknown): value is Value => check(value),
    });
    adapter.write("key", { count: 1 });
    adapter.read("key");
    accepted = false;
    expectCode(() => adapter.read("key"), "INVALID_DATA");
    accepted = true;
    const recovered = adapter.read("key")!;
    expect(adapter.read("key")).toBe(recovered);
    expect(check).toHaveBeenCalledTimes(5);
    const failure = new Error("validator failed");
    check.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.read("key"), "INVALID_DATA", failure);
  });

  it("retains the previous snapshot after failed or invalid writes and failed removal", () => {
    const { storage, adapter } = setup();
    const first = adapter.read("key")!;
    const failure = new Error("quota exceeded");
    storage.setItem.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.write("key", { count: 2 }), "WRITE_FAILED", failure);
    expect(adapter.read("key")).toBe(first);
    expectCode(() => adapter.write("key", { count: "bad" } as unknown as Value), "INVALID_VALUE");
    expect(adapter.read("key")).toBe(first);
    storage.removeItem.mockImplementationOnce(() => {
      throw failure;
    });
    expectCode(() => adapter.remove("key"), "DELETE_FAILED", failure);
    expect(adapter.read("key")).toBe(first);
  });

  it("reuses a snapshot after writing the same raw JSON without retaining input references", () => {
    const { adapter } = setup();
    const first = adapter.read("key")!;
    adapter.write("key", { count: 1 });
    expect(adapter.read("key")).toBe(first);
    const next = { count: 2 };
    adapter.write("key", next);
    next.count = 3;
    const second = adapter.read("key")!;
    expect(second.value).toEqual({ count: 2 });
    expect(second.value).not.toBe(next);
    expect(second).not.toBe(first);
    expect(adapter.read("key")).toBe(second);
  });

  it("invalidates a removed key even if it is recreated with identical JSON before reading", () => {
    const { adapter } = setup();
    const first = adapter.read("key")!;
    adapter.remove("key");
    adapter.write("key", { count: 1 });
    expect(adapter.read("key")).not.toBe(first);
    adapter.remove("key");
    expect(adapter.read("key")).toBeUndefined();
    adapter.write("key", { count: 2 });
    expect(adapter.read("key")!.value).toEqual({ count: 2 });
  });

  it("detects external deletion and recreates a snapshot after the key returns", () => {
    const { storage, adapter } = setup();
    const first = adapter.read("key")!;
    storage.data.delete(persistedKey);
    expect(adapter.read("key")).toBeUndefined();
    storage.data.set(persistedKey, '{"count":1}');
    expect(adapter.read("key")!.value).toEqual(first.value);
    expect(adapter.read("key")).not.toBe(first);
  });

  it("keeps independent snapshots for different keys and adapter namespaces", () => {
    const { storage, adapter } = setup();
    const other = createLocalStorage({ name: "other", storage, validate });
    adapter.write("other", { count: 2 });
    other.write("key", { count: 3 });
    const first = adapter.read("key")!;
    const second = adapter.read("other")!;
    const third = other.read("key")!;
    expect([first.value.count, second.value.count, third.value.count]).toEqual([1, 2, 3]);
    expect(adapter.read("key")).toBe(first);
    expect(adapter.read("other")).toBe(second);
    expect(other.read("key")).toBe(third);
  });
});
