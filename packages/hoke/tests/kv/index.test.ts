import { describe, expect, it, vi } from "vitest";
import { KV } from "../../src/kv/index.js";
import { MemoryBackend } from "../helpers/memory-backend.js";

describe("in-memory KV", () => {
  it("reads, replaces and deletes values, including empty keys", () => {
    const kv = new KV<number>({ name: "test", backend: new MemoryBackend() });
    expect(kv.get("")).toBeUndefined();
    kv.set("", 0);
    expect(kv.get("")).toBe(0);
    kv.set("", 42);
    expect(kv.get("")).toBe(42);
    kv.delete("");
    expect(kv.get("")).toBeUndefined();
  });

  it("isolates store instances and safely accepts special property names", () => {
    const first = new KV<string>({ name: "test", backend: new MemoryBackend() });
    const second = new KV<string>({ name: "test", backend: new MemoryBackend() });
    first.set("__proto__", "value");
    expect(first.get("__proto__")).toBe("value");
    expect(second.get("__proto__")).toBeUndefined();
  });

  it("notifies only the affected key, after committing the value", () => {
    const kv = new KV<number>({ name: "test", backend: new MemoryBackend() });
    const seen: (number | undefined)[] = [];
    const other = vi.fn();
    kv.subscribe("count", () => seen.push(kv.get("count")));
    kv.subscribe("other", other);
    kv.set("count", 1);
    kv.set("count", 2);
    kv.delete("count");
    expect(seen).toEqual([1, 2, undefined]);
    expect(other).not.toHaveBeenCalled();
  });

  it("skips unchanged values and deletion of absent keys", () => {
    const kv = new KV<number>({ name: "test", backend: new MemoryBackend() });
    const listener = vi.fn();
    kv.subscribe("key", listener);
    kv.delete("key");
    kv.set("key", 1);
    kv.set("key", 1);
    expect(listener).toHaveBeenCalledTimes(1);
    kv.set("key", 0);
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it.each([undefined, NaN, Infinity, -0])("rejects non-JSON values even in memory: %s", (value) => {
    const kv = new KV({ name: "test", backend: new MemoryBackend() });
    const listener = vi.fn();
    kv.subscribe("key", listener);
    expect(() => kv.set("key", value)).toThrow();
    expect(kv.get("key")).toBeUndefined();
    expect(listener).not.toHaveBeenCalled();
  });

  it("allows duplicate callbacks to unsubscribe independently and repeatedly", () => {
    const kv = new KV<number>({ name: "test", backend: new MemoryBackend() });
    const listener = vi.fn();
    const stopFirst = kv.subscribe("key", listener);
    const stopSecond = kv.subscribe("key", listener);
    stopFirst();
    stopFirst();
    kv.set("key", 1);
    expect(listener).toHaveBeenCalledTimes(1);
    stopSecond();
    const stopThird = kv.subscribe("key", listener);
    stopFirst();
    kv.set("key", 2);
    expect(listener).toHaveBeenCalledTimes(2);
    stopThird();
  });

  it("defers new subscriptions and respects unsubscribe during notification", () => {
    const kv = new KV<number>({ name: "test", backend: new MemoryBackend() });
    const added = vi.fn();
    const removed = vi.fn();
    let stopRemoved = () => {};
    const stopFirst = kv.subscribe("key", () => {
      kv.subscribe("key", added);
      stopRemoved();
    });
    stopRemoved = kv.subscribe("key", removed);
    kv.set("key", 1);
    expect(added).not.toHaveBeenCalled();
    expect(removed).not.toHaveBeenCalled();
    stopFirst();
    kv.set("key", 2);
    expect(added).toHaveBeenCalledTimes(1);
  });

  it("keeps methods bound when passed as callbacks", () => {
    const kv = new KV<number>({ name: "callbacks", backend: new MemoryBackend() });
    const { get, set, delete: remove, subscribe } = kv;
    const listener = vi.fn();
    const unsubscribe = subscribe("key", listener);
    set("key", 1);
    expect(get("key")).toBe(1);
    remove("key");
    expect(get("key")).toBeUndefined();
    expect(listener).toHaveBeenCalledTimes(2);
    unsubscribe();
    set("key", 2);
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it("reports listener errors after notifying the remaining subscribers", () => {
    const kv = new KV<number>({ name: "test", backend: new MemoryBackend() });
    const failure = new Error("subscriber failed");
    const next = vi.fn();
    kv.subscribe("key", () => {
      throw failure;
    });
    kv.subscribe("key", next);
    expect(() => kv.set("key", 1)).toThrow(AggregateError);
    expect(kv.get("key")).toBe(1);
    expect(next).toHaveBeenCalledTimes(1);
  });
});

describe("custom string backend", () => {
  it("namespaces keys and serializes values before reaching the backend", () => {
    const entries = new Map<string, string>();
    const backend = {
      read: vi.fn((key: string) => entries.get(key)),
      write: vi.fn((key: string, value: string) => {
        entries.set(key, value);
      }),
      remove: vi.fn((key: string) => {
        entries.delete(key);
      }),
    };
    const first = new KV({ name: "first", backend });
    const second = new KV({ name: "second", backend });
    first.set("key", { nested: [1, true, null] });
    expect(backend.write).toHaveBeenCalledWith(
      'stowtree:["first","key"]',
      '{"nested":[1,true,null]}',
    );
    expect(second.get("key")).toBeUndefined();
    expect(new KV({ name: "first", backend }).get("key")).toEqual({ nested: [1, true, null] });
    first.delete("key");
    expect(backend.remove).toHaveBeenCalledWith('stowtree:["first","key"]');
  });

  it("keeps notifications local even when instances share a backend", () => {
    const backend = new MemoryBackend();
    const first = new KV({ name: "app", backend });
    const second = new KV({ name: "app", backend });
    const listener = vi.fn();
    second.subscribe("key", listener);
    first.set("key", 1);
    expect(second.get("key")).toBe(1);
    expect(listener).not.toHaveBeenCalled();
  });

  it("rechecks a custom backend instead of serving stale parsed data", () => {
    const backend = new MemoryBackend();
    const kv = new KV({ name: "app", backend });
    kv.set("key", { count: 1 });
    const first = kv.get("key");
    expect(kv.get("key")).toBe(first);
    backend.write('stowtree:["app","key"]', '{"count":2}');
    expect(kv.get("key")).toEqual({ count: 2 });
    backend.write('stowtree:["app","key"]', "{broken");
    expect(() => kv.get("key")).toThrow();
  });
});

describe("synchronous backend contract", () => {
  it("rejects promise-returning writes without reporting a successful change", async () => {
    const cause = new Error("async write failed");
    const backend = {
      read: () => undefined,
      write: async () => {
        throw cause;
      },
      remove: () => {},
    };
    const kv = new KV({ name: "async-write", backend });
    const listener = vi.fn();
    kv.subscribe("key", listener);
    expect(() => kv.set("key", 1)).toThrow(expect.objectContaining({ code: "WRITE_FAILED" }));
    expect(listener).not.toHaveBeenCalled();
    await new Promise((resolve) => setImmediate(resolve));
  });

  it("rejects promise-returning removals without notifying subscribers", async () => {
    const backend = {
      read: () => "1",
      write: () => {},
      remove: async () => {
        throw new Error("async removal failed");
      },
    };
    const kv = new KV({ name: "async-delete", backend });
    const listener = vi.fn();
    kv.subscribe("key", listener);
    expect(() => kv.delete("key")).toThrow(expect.objectContaining({ code: "DELETE_FAILED" }));
    expect(listener).not.toHaveBeenCalled();
    await new Promise((resolve) => setImmediate(resolve));
  });

  it("rejects promise-returning reads and observes their rejection", async () => {
    const backend = {
      read: () => Promise.reject(new Error("async read failed")),
      write: () => {},
      remove: () => {},
    };
    // JavaScript backends may violate the TypeScript contract at runtime.
    const kv = new KV({ name: "async-read", backend: backend as never });
    expect(() => kv.get("key")).toThrow(expect.objectContaining({ code: "READ_FAILED" }));
    await new Promise((resolve) => setImmediate(resolve));
  });
});
