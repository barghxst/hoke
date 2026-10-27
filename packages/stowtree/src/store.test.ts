import { describe, expect, it, vi } from "vitest";
import type { KV } from "./kv/index.js";
import { createKV } from "./kv/index.js";
import { createLocalStorage } from "./kv/local-storage.js";
import type { StoreDefinition } from "./store.js";
import { createStore, defineStore, StoreError } from "./store.js";

type Settings = { count: number; nested?: { label: string } };
const valid = (value: unknown): value is Settings =>
  typeof value === "object" &&
  value !== null &&
  typeof (value as Settings).count === "number" &&
  Number.isFinite((value as Settings).count);
const definition = (options: Partial<StoreDefinition<Settings>> = {}): StoreDefinition<Settings> =>
  defineStore<Settings>({
    key: "settings",
    version: 1,
    defaults: () => ({ count: 0 }),
    validate: valid,
    ...options,
  });
const record = <T>(version: number, value: T) => ({ __stowtree: 1, version, value });
function errorCode(fn: () => unknown, code: string) {
  try {
    fn();
  } catch (error) {
    expect(error).toBeInstanceOf(StoreError);
    expect((error as StoreError).code).toBe(code);
    return;
  }
  throw new Error(`Expected StoreError(${code})`);
}

describe("store definitions and snapshots", () => {
  it("returns stable defaults without writing and isolates defaults between instances", () => {
    const kv = createKV();
    const write = vi.spyOn(kv, "set");
    const a = createStore(kv, definition());
    const b = createStore(kv, definition());
    expect(a.getSnapshot()).toBe(a.getSnapshot());
    expect(a.getSnapshot()).toBe(a.getServerSnapshot());
    expect(b.getSnapshot()).not.toBe(a.getSnapshot());
    expect(write).not.toHaveBeenCalled();
    expect(kv.get("settings")).toBeUndefined();
  });
  it("writes envelopes, updates, resets and preserves the server snapshot", () => {
    const kv = createKV();
    const store = createStore(kv, definition());
    const defaults = store.getServerSnapshot();
    const next = { count: 4 };
    store.set(next);
    expect(kv.get("settings")).toEqual(record(1, next));
    expect(store.getSnapshot()).toBe(next);
    expect(store.getSnapshot()).toBe(store.getSnapshot());
    store.update((previous) => ({ count: previous.count + 2 }));
    expect(store.getSnapshot()).toEqual({ count: 6 });
    expect(store.getServerSnapshot()).toBe(defaults);
    store.reset();
    expect(kv.get("settings")).toBeUndefined();
    expect(store.getSnapshot()).toBe(defaults);
  });
  it("shares notifications and snapshots through a KV and unsubscribes", () => {
    const kv = createKV();
    const a = createStore(kv, definition());
    const b = createStore(kv, definition());
    const listener = vi.fn(() => b.getSnapshot());
    const stop = b.subscribe(listener);
    a.set({ count: 1 });
    expect(listener).toHaveLastReturnedWith({ count: 1 });
    expect(a.getSnapshot()).toBe(b.getSnapshot());
    a.reset();
    expect(listener).toHaveBeenCalledTimes(2);
    stop();
    a.set({ count: 2 });
    expect(listener).toHaveBeenCalledTimes(2);
  });
  it("does not notify when validation fails", () => {
    const kv = createKV();
    const store = createStore(kv, definition());
    const listener = vi.fn();
    store.subscribe(listener);
    errorCode(() => store.set({ count: NaN }), "invalid-value");
    expect(listener).not.toHaveBeenCalled();
    expect(kv.get("settings")).toBeUndefined();
  });
  it.each([0, -1, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1])(
    "rejects invalid version %s",
    (version) => {
      errorCode(() => definition({ version }), "invalid-definition");
    },
  );
  it("rejects invalid definitions, default factories, default values and validators", () => {
    errorCode(() => definition({ key: "" }), "invalid-definition");
    errorCode(() => definition({ migrations: { 1: (x) => x } }), "invalid-definition");
    errorCode(() => definition({ migrations: { 2: 1 as never } }), "invalid-definition");
    errorCode(
      () =>
        createStore(
          createKV(),
          definition({
            defaults: () => {
              throw Error("bad");
            },
          }),
        ),
      "invalid-defaults",
    );
    errorCode(
      () => createStore(createKV(), definition({ defaults: () => ({ count: NaN }) })),
      "invalid-defaults",
    );
    errorCode(
      () =>
        createStore(
          createKV(),
          definition({
            validate: (() => {
              throw Error("bad");
            }) as never,
          }),
        ),
      "invalid-defaults",
    );
  });
  it("copies and freezes definition and migration configuration", () => {
    const migrations = { 2: (x: unknown) => x };
    const d = definition({ version: 2, migrations });
    migrations[2] = () => "oops";
    expect(d.migrations![2]).not.toBe(migrations[2]);
    expect(Object.isFrozen(d)).toBe(true);
  });
  it.each([
    null,
    [],
    {},
    { version: 1, value: { count: 2 } },
    record(0, {}),
    record(1.2, {}),
    { __stowtree: 2, version: 1, value: {} },
    { __stowtree: 1, version: 1 },
  ])("rejects malformed stored envelope %#", (raw) => {
    const kv = createKV();
    kv.set("settings", raw);
    errorCode(() => createStore(kv, definition()), "invalid-envelope");
    expect(kv.get("settings")).toBe(raw);
  });
  it("validates persisted current values", () => {
    const kv = createKV();
    kv.set("settings", record(1, { count: "no" }));
    errorCode(() => createStore(kv, definition()), "invalid-value");
  });
  it("detects external writes, deletion and corruption without serving stale cache", () => {
    const kv = createKV();
    const store = createStore(kv, definition());
    store.set({ count: 1 });
    const first = store.getSnapshot();
    kv.set("settings", record(1, { count: 2 }));
    expect(store.getSnapshot()).toEqual({ count: 2 });
    expect(store.getSnapshot()).not.toBe(first);
    kv.set("settings", "corrupt");
    errorCode(store.getSnapshot, "invalid-envelope");
    errorCode(store.getSnapshot, "invalid-envelope");
    kv.delete("settings");
    expect(store.getSnapshot()).toBe(store.getServerSnapshot());
  });
  it("protects an existing snapshot and defaults from a mutating updater failure", () => {
    const kv = createKV();
    const store = createStore(
      kv,
      definition({ defaults: () => ({ count: 0, nested: { label: "safe" } }) }),
    );
    errorCode(
      () =>
        store.update((previous) => {
          previous.nested!.label = "bad";
          throw Error("failure");
        }),
      "update-failed",
    );
    expect(store.getSnapshot().nested!.label).toBe("safe");
    store.set({ count: 4, nested: { label: "saved" } });
    const original = kv.get("settings");
    errorCode(
      () =>
        store.update((previous) => {
          previous.count++;
          return previous;
        }),
      "update-failed",
    );
    expect(store.getSnapshot().count).toBe(4);
    expect(kv.get("settings")).toBe(original);
  });
  it("rejects invalid and async updater output without committing", () => {
    const kv = createKV();
    const store = createStore(kv, definition());
    errorCode(() => store.update(() => ({ count: NaN })), "invalid-value");
    errorCode(() => store.update((() => Promise.resolve({ count: 1 })) as never), "async-result");
    expect(kv.get("settings")).toBeUndefined();
  });
});

describe("migrations", () => {
  it("executes every step and commits exactly once; does not rerun after reopening", () => {
    const kv = createKV();
    kv.set("settings", record(1, { old: 2 }));
    const write = vi.spyOn(kv, "set");
    const step2 = vi.fn((value: unknown) => ({ count: (value as { old: number }).old }));
    const step3 = vi.fn((value: unknown) => ({ count: (value as Settings).count + 1 }));
    const d = definition({ version: 3, migrations: { 2: step2, 3: step3 } });
    const store = createStore(kv, d);
    expect(store.getSnapshot()).toEqual({ count: 3 });
    expect(kv.get("settings")).toEqual(record(3, { count: 3 }));
    expect(write).toHaveBeenCalledTimes(1);
    createStore(kv, d);
    expect(step2).toHaveBeenCalledTimes(1);
    expect(step3).toHaveBeenCalledTimes(1);
  });
  it("checks the complete migration path before invoking any step", () => {
    const kv = createKV();
    const raw = record(1, { count: 3 });
    kv.set("settings", raw);
    const step = vi.fn((x: unknown) => x);
    errorCode(
      () => createStore(kv, definition({ version: 3, migrations: { 2: step } })),
      "missing-migration",
    );
    expect(step).not.toHaveBeenCalled();
    expect(kv.get("settings")).toBe(raw);
  });
  it.each([
    [
      "migration-failed",
      () => {
        throw Error("failure");
      },
    ],
    [
      "migration-failed",
      (x: unknown) => {
        (x as Settings).count = 8;
        return x;
      },
    ],
    ["invalid-value", () => ({ count: "invalid" })],
    ["async-result", () => Promise.resolve({ count: 4 })],
    // biome-ignore lint/suspicious/noThenProperty: Intentionally tests rejection of a custom thenable.
    ["async-result", () => ({ then() {} })],
  ] as const)("leaves original record intact on %s", (code, migration) => {
    const kv = createKV();
    const raw = record(1, { count: 1 });
    kv.set("settings", raw);
    const write = vi.spyOn(kv, "set");
    errorCode(
      () => createStore(kv, definition({ version: 2, migrations: { 2: migration } })),
      code,
    );
    expect(kv.get("settings")).toBe(raw);
    expect(raw.value.count).toBe(1);
    expect(write).not.toHaveBeenCalled();
  });
  it("protects nested source data even when a later migration fails", () => {
    const kv = createKV();
    const raw = record(1, { count: 2, nested: { label: "safe" } });
    kv.set("settings", raw);
    errorCode(
      () =>
        createStore(
          kv,
          definition({
            version: 3,
            migrations: {
              2: (value) => ({ ...(value as Settings), count: 4 }),
              3: (value) => {
                (value as Settings).nested!.label = "bad";
                return value;
              },
            },
          }),
        ),
      "migration-failed",
    );
    expect(kv.get("settings")).toBe(raw);
    expect(raw.value.nested.label).toBe("safe");
  });
  it("rejects future versions on opening and later reads", () => {
    const kv = createKV();
    const store = createStore(kv, definition());
    const raw = record(2, { count: 1 });
    kv.set("settings", raw);
    errorCode(store.getSnapshot, "future-version");
    errorCode(() => createStore(kv, definition()), "future-version");
    expect(kv.get("settings")).toBe(raw);
  });
  it("does not migrate or write during getSnapshot after an incompatible external version change", () => {
    const kv = createKV();
    const step = vi.fn((x: unknown) => x);
    const store = createStore(kv, definition({ version: 2, migrations: { 2: step } }));
    const raw = record(1, { count: 1 });
    kv.set("settings", raw);
    const write = vi.spyOn(kv, "set");
    errorCode(store.getSnapshot, "migration-required");
    expect(write).not.toHaveBeenCalled();
    expect(step).not.toHaveBeenCalled();
    expect(kv.get("settings")).toBe(raw);
  });
});

describe("storage failures", () => {
  function setup() {
    const memory = createKV();
    const kv: KV<unknown> = { ...memory };
    return { memory, kv };
  }
  it("propagates failed migration commit and preserves original data", () => {
    const { memory, kv } = setup();
    const raw = record(1, { count: 1 });
    memory.set("settings", raw);
    const failure = Error("quota");
    kv.set = () => {
      throw failure;
    };
    expect(() =>
      createStore(
        kv,
        definition({ version: 2, migrations: { 2: (x) => ({ ...(x as Settings), count: 2 }) } }),
      ),
    ).toThrow(failure);
    expect(memory.get("settings")).toBe(raw);
  });
  it("never substitutes the last good cache after a read error", () => {
    const { kv } = setup();
    const store = createStore(kv, definition());
    store.set({ count: 1 });
    expect(store.getSnapshot()).toEqual({ count: 1 });
    const failure = Error("read denied");
    kv.get = () => {
      throw failure;
    };
    expect(store.getSnapshot).toThrow(failure);
    expect(store.getSnapshot).toThrow(failure);
    expect(store.getServerSnapshot()).toEqual({ count: 0 });
  });
  it("does not notify or change snapshots after failed writes and deletions", () => {
    const { memory, kv } = setup();
    const store = createStore(kv, definition());
    store.set({ count: 1 });
    const snapshot = store.getSnapshot();
    const listener = vi.fn();
    store.subscribe(listener);
    const failure = Error("storage denied");
    kv.set = () => {
      throw failure;
    };
    kv.delete = () => {
      throw failure;
    };
    expect(() => store.set({ count: 2 })).toThrow(failure);
    expect(() => store.update((previous) => ({ count: previous.count + 1 }))).toThrow(failure);
    expect(store.reset).toThrow(failure);
    expect(store.getSnapshot()).toBe(snapshot);
    expect(memory.get("settings")).toEqual(record(1, { count: 1 }));
    expect(listener).not.toHaveBeenCalled();
  });
});

describe("store edge cases", () => {
  it("keeps serialized localStorage snapshots stable and observes external raw changes", () => {
    const data = new Map<string, string>();
    const backend = {
      getItem: (key: string) => data.get(key) ?? null,
      setItem: (key: string, value: string) => {
        data.set(key, value);
      },
      removeItem: (key: string) => {
        data.delete(key);
      },
    };
    const kv = createKV({
      storage: createLocalStorage({
        storage: backend,
        name: "test",
        validate: (value: unknown): value is unknown => true,
      }),
    });
    const store = createStore(kv, definition());
    store.set({ count: 1 });
    const snapshot = store.getSnapshot();
    expect(store.getSnapshot()).toBe(snapshot);
    const key = [...data.keys()][0]!;
    data.set(key, JSON.stringify(record(1, { count: 2 })));
    expect(store.getSnapshot()).toEqual({ count: 2 });
    data.set(key, "broken");
    expect(store.getSnapshot).toThrow();
    expect(store.getSnapshot).toThrow();
    data.delete(key);
    expect(store.getSnapshot()).toBe(store.getServerSnapshot());
  });
  it("allows intermediate unknown schemas and validates only the final migration value", () => {
    const kv = createKV();
    kv.set("settings", record(1, "old"));
    const store = createStore(
      kv,
      definition({
        version: 3,
        migrations: {
          2: () => undefined,
          3: (value) => ({ count: value === undefined ? 4 : -1 }),
        },
      }),
    );
    expect(store.getSnapshot()).toEqual({ count: 4 });
  });
  it("does not run migrations for a missing record", () => {
    const step = vi.fn((x: unknown) => x);
    const kv = createKV();
    createStore(kv, definition({ version: 2, migrations: { 2: step } }));
    expect(step).not.toHaveBeenCalled();
    expect(kv.get("settings")).toBeUndefined();
  });
  it("fails cloning without invoking an updater or damaging original data", () => {
    const kv = createKV();
    const raw = record(1, { count: 1, callback: () => 1 });
    kv.set("settings", raw);
    const store = createStore(kv, definition());
    const updater = vi.fn((x: Settings) => x);
    errorCode(() => store.update(updater), "clone-failed");
    expect(updater).not.toHaveBeenCalled();
    expect(kv.get("settings")).toBe(raw);
  });
  it("preserves source Map contents on a throwing updater despite Map internal mutability", () => {
    const kv = createKV();
    const d = defineStore({
      key: "map",
      version: 1,
      defaults: () => new Map<string, number>(),
      validate: (value: unknown): value is Map<string, number> => value instanceof Map,
    });
    const store = createStore(kv, d);
    store.set(new Map([["safe", 1]]));
    errorCode(
      () =>
        store.update((previous) => {
          previous.set("safe", 2);
          throw Error("fail");
        }),
      "update-failed",
    );
    expect(store.getSnapshot().get("safe")).toBe(1);
  });
  it("propagates a backend read error while opening", () => {
    const kv = createKV();
    const failure = Error("blocked");
    kv.get = () => {
      throw failure;
    };
    expect(() => createStore(kv, definition())).toThrow(failure);
  });
});

describe("stale store write guards", () => {
  it.each([
    ["future-version", 1, record(2, { count: 5 })],
    ["migration-required", 2, record(1, { count: 5 })],
    ["invalid-envelope", 1, { broken: true }],
    ["invalid-value", 1, record(1, { count: "bad" })],
  ] as const)("preserves external %s data on set, update and reset", (code, version, raw) => {
    const kv = createKV();
    const store = createStore(kv, definition({ version, migrations: { 2: (value) => value } }));
    kv.set("settings", raw);
    const write = vi.spyOn(kv, "set");
    const remove = vi.spyOn(kv, "delete");
    const updater = vi.fn((previous: Settings) => ({ count: previous.count + 1 }));
    const listener = vi.fn();
    store.subscribe(listener);
    errorCode(() => store.set({ count: 99 }), code);
    errorCode(() => store.update(updater), code);
    errorCode(store.reset, code);
    expect(kv.get("settings")).toBe(raw);
    expect(write).not.toHaveBeenCalled();
    expect(remove).not.toHaveBeenCalled();
    expect(updater).not.toHaveBeenCalled();
    expect(listener).not.toHaveBeenCalled();
  });
});
