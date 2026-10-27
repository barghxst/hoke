import { describe, expect, it, vi } from "vitest";
import { createKV } from "./index.js";

describe("in-memory KV", () => {
  it("reads, replaces and deletes values, including empty keys", () => {
    const kv = createKV<number>();
    expect(kv.get("")).toBeUndefined();
    kv.set("", 0);
    expect(kv.get("")).toBe(0);
    kv.set("", 42);
    expect(kv.get("")).toBe(42);
    kv.delete("");
    expect(kv.get("")).toBeUndefined();
  });

  it("isolates store instances and safely accepts special property names", () => {
    const first = createKV<string>();
    const second = createKV<string>();
    first.set("__proto__", "value");
    expect(first.get("__proto__")).toBe("value");
    expect(second.get("__proto__")).toBeUndefined();
  });

  it("notifies only the affected key, after committing the value", () => {
    const kv = createKV<number>();
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
    const kv = createKV<number>();
    const listener = vi.fn();
    kv.subscribe("key", listener);
    kv.delete("key");
    kv.set("key", NaN);
    kv.set("key", NaN);
    expect(listener).toHaveBeenCalledTimes(1);
    kv.set("key", 0);
    kv.set("key", -0);
    expect(listener).toHaveBeenCalledTimes(3);
  });

  it("distinguishes a missing key from a stored undefined for notifications", () => {
    const kv = createKV<undefined>();
    const listener = vi.fn();
    kv.subscribe("key", listener);
    kv.set("key", undefined);
    kv.set("key", undefined);
    kv.delete("key");
    kv.delete("key");
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it("allows duplicate callbacks to unsubscribe independently and repeatedly", () => {
    const kv = createKV<number>();
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
    const kv = createKV<number>();
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

  it("reports listener errors after notifying the remaining subscribers", () => {
    const kv = createKV<number>();
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
