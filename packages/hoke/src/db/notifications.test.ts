import { runInNewContext } from "node:vm";
import { describe, expect, it, vi } from "vitest";
import { DBError } from "./index.js";
import { DBNotificationError, type DBRange, WatchRegistry } from "./notifications.js";

const bytes = (...values: number[]) => new Uint8Array(values);

describe("database notification registry", () => {
  it("matches exact binary and empty keys without an initial event and copies registrations", () => {
    const registry = new WatchRegistry();
    const listener = vi.fn();
    const empty = vi.fn();
    const key = bytes(0, 255);
    registry.subscribe(key, listener);
    registry.subscribe(bytes(), empty);
    key[0] = 9;
    expect(listener).not.toHaveBeenCalled();
    registry.notify(bytes(0));
    registry.notify(bytes(0, 255, 0));
    registry.notify(bytes(0, 255));
    registry.notify(bytes());
    expect(listener).toHaveBeenCalledExactlyOnceWith();
    expect(empty).toHaveBeenCalledExactlyOnceWith();
  });

  it("intersects copied binary bounds and prefixes with exact inclusivity", () => {
    const registry = new WatchRegistry();
    const listener = vi.fn();
    const start = { key: bytes(0, 1), inclusive: false };
    const end = { key: bytes(0, 3), inclusive: true };
    const prefix = bytes(0);
    registry.subscribeRange({ start, end, prefix }, listener);
    start.key[0] = 9;
    start.inclusive = true;
    end.key[0] = 9;
    prefix[0] = 9;
    for (const key of [bytes(), bytes(0, 1), bytes(0, 3, 0), bytes(1, 2)]) registry.notify(key);
    expect(listener).not.toHaveBeenCalled();
    registry.notify(bytes(0, 1, 0));
    registry.notify(bytes(0, 3));
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it("handles empty/all-FF prefixes and equal bounds without a synthetic successor", () => {
    const registry = new WatchRegistry();
    const all = vi.fn();
    const ff = vi.fn();
    const exact = vi.fn();
    const empty = vi.fn();
    registry.subscribeRange({ prefix: bytes() }, all);
    registry.subscribeRange({ prefix: bytes(255, 255) }, ff);
    registry.subscribeRange(
      { start: { key: bytes(255), inclusive: true }, end: { key: bytes(255), inclusive: true } },
      exact,
    );
    registry.subscribeRange(
      { start: { key: bytes(255), inclusive: false }, end: { key: bytes(255), inclusive: true } },
      empty,
    );
    for (const key of [bytes(), bytes(255), bytes(255, 255), bytes(255, 255, 0)])
      registry.notify(key);
    expect(all).toHaveBeenCalledTimes(4);
    expect(ff).toHaveBeenCalledTimes(2);
    expect(exact).toHaveBeenCalledTimes(1);
    expect(empty).not.toHaveBeenCalled();
  });

  it("uses global registration order and independent duplicate callback registrations", () => {
    const registry = new WatchRegistry();
    const order: string[] = [];
    const callback = () => {
      order.push("key");
    };
    const stop = registry.subscribe(bytes(1), callback);
    registry.subscribeRange({}, () => {
      order.push("range");
    });
    registry.subscribe(bytes(1), callback);
    registry.notify(bytes(1));
    expect(order).toEqual(["key", "range", "key"]);
    stop();
    stop();
    order.length = 0;
    registry.notify(bytes(1));
    expect(order).toEqual(["range", "key"]);
  });

  it("defers additions, skips removed listeners, and cancels remaining listeners on clear", () => {
    const registry = new WatchRegistry();
    const order: string[] = [];
    let remove = () => {};
    registry.subscribeRange({}, () => {
      order.push("first");
      remove();
      registry.subscribeRange({}, () => {
        order.push("added");
      });
    });
    remove = registry.subscribeRange({}, () => {
      order.push("removed");
    });
    registry.notify(bytes());
    expect(order).toEqual(["first"]);
    registry.notify(bytes());
    expect(order).toEqual(["first", "first", "added"]);
    registry.clear();
    registry.subscribeRange({}, () => {
      registry.clear();
    });
    const skipped = vi.fn();
    registry.subscribeRange({}, skipped);
    registry.notify(bytes());
    expect(skipped).not.toHaveBeenCalled();
  });

  it("gives reentrant notifications independent snapshots and stable matching", () => {
    const registry = new WatchRegistry();
    const order: string[] = [];
    let nested = false;
    const changed = bytes(1);
    registry.subscribeRange({}, () => {
      order.push(nested ? "nested-first" : "first");
      if (!nested) {
        changed[0] = 2;
        nested = true;
        registry.notify(bytes(1));
        nested = false;
      }
    });
    registry.subscribe(bytes(1), () => {
      order.push(nested ? "nested-key" : "key");
    });
    registry.notify(changed);
    expect(order).toEqual(["first", "nested-first", "nested-key", "key"]);
  });

  it("aggregates thrown values after visiting every matching subscriber", () => {
    const registry = new WatchRegistry();
    const failure = new Error("subscriber");
    registry.subscribeRange({}, () => {
      throw failure;
    });
    registry.subscribeRange({}, () => {
      throw undefined;
    });
    const last = vi.fn();
    registry.subscribeRange({}, last);
    const error = registry.notify(bytes());
    expect(error).toBeInstanceOf(DBNotificationError);
    expect(error).toBeInstanceOf(AggregateError);
    expect(error).toMatchObject({
      name: "DBNotificationError",
      code: "SUBSCRIBER_FAILED",
      committed: true,
      errors: [failure, undefined],
    });
    expect(error?.message).toContain("commit succeeded");
    expect(last).toHaveBeenCalledExactlyOnceWith();
    registry.clear();
    expect(registry.notify(bytes())).toBeUndefined();
  });

  it("rejects async callbacks, observes local/cross-realm rejections, and never invokes custom then", async () => {
    const registry = new WatchRegistry();
    const then = vi.fn();
    registry.subscribeRange({}, () => Promise.reject(new Error("local")));
    registry.subscribeRange({}, () => runInNewContext("Promise.reject(new Error('foreign'))"));
    registry.subscribeRange({}, () => ({ then }));
    const getterError = new Error("then getter");
    registry.subscribeRange({}, () =>
      // biome-ignore lint/suspicious/noThenProperty: Exercise a hostile then getter without assimilating it.
      Object.defineProperty({}, "then", {
        get() {
          throw getterError;
        },
      }),
    );
    const error = registry.notify(bytes());
    expect(error?.errors).toHaveLength(4);
    expect(error?.errors[3]).toBe(getterError);
    expect(then).not.toHaveBeenCalled();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  it("observes a genuine rejected promise even when its then getter throws", async () => {
    const registry = new WatchRegistry();
    registry.subscribeRange({}, () =>
      // biome-ignore lint/suspicious/noThenProperty: Native promise must still be observed when then is shadowed.
      Object.defineProperty(Promise.reject(new Error("hidden")), "then", {
        get() {
          throw new Error("getter");
        },
      }),
    );
    expect(registry.notify(bytes())?.errors).toHaveLength(1);
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  it.each([undefined, null, 0])("rejects native promises with shadowed then %s", async (then) => {
    const registry = new WatchRegistry();
    registry.subscribeRange({}, () =>
      // biome-ignore lint/suspicious/noThenProperty: Regression for a native promise with a nonfunction then.
      Object.defineProperty(Promise.reject(new Error("hidden async")), "then", { value: then }),
    );
    registry.subscribeRange({}, () =>
      // biome-ignore lint/suspicious/noThenProperty: Cross-realm native promises also need the brand check.
      Object.defineProperty(runInNewContext("Promise.reject(new Error('foreign async'))"), "then", {
        value: then,
      }),
    );
    const error = registry.notify(bytes());
    expect(error).toBeInstanceOf(DBNotificationError);
    expect(error?.committed).toBe(true);
    expect(error?.errors).toHaveLength(2);
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  it("validates listeners, key sizes, detached arrays, ranges and scan-only flags", () => {
    const registry = new WatchRegistry();
    const detached = bytes(1);
    structuredClone(detached.buffer, { transfer: [detached.buffer] });
    for (const key of [null, [], "key", new Uint8Array(65537), detached]) {
      expect(() => registry.subscribe(key as Uint8Array, () => {})).toThrow(DBError);
    }
    expect(() => registry.subscribe(new Uint8Array(65536), () => {})).not.toThrow();
    expect(() => registry.subscribe(bytes(), null as never)).toThrow(DBError);
    expect(() => registry.subscribeRange({}, null as never)).toThrow(DBError);
    for (const range of [
      null,
      [],
      "range",
      { reverse: false },
      { limit: undefined },
      { typo: true },
      { [Symbol("unknown")]: true },
      { prefix: detached },
      { prefix: new Uint8Array(65537) },
      { start: { key: bytes(), inclusive: 1 } },
      { start: { key: bytes(), inclusive: true, typo: true } },
      { start: { key: bytes(2), inclusive: true }, end: { key: bytes(1), inclusive: true } },
    ]) {
      expect(() => registry.subscribeRange(range as DBRange, () => {})).toThrow(
        expect.objectContaining({ code: "INVALID_ARGUMENT" }),
      );
    }
  });
});
