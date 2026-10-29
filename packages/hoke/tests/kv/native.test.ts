import { beforeEach, describe, expect, it, vi } from "vitest";
import { KV as SourceKV } from "../../src/kv/kv.js";
import type { MMKVInstance } from "../../src/kv/native/index.js";
import { KV, KVStorageError, MMKVBackend } from "../../src/kv/native/index.js";
import { KV as WebKV } from "../../src/kv/web/index.js";
import { MemoryBackend } from "../helpers/memory-backend.js";

const { createMMKV } = vi.hoisted(() => ({ createMMKV: vi.fn() }));
vi.mock("react-native-mmkv", () => ({ createMMKV }));
vi.mock(
  "../../src/kv/web/resolve-backend.js",
  async () => import("../../src/kv/native/resolve-backend.js"),
);
const constructionCallsAtImport = createMMKV.mock.calls.length;

function mockInstance() {
  const entries = new Map<string, unknown>();
  return {
    entries,
    getString: vi.fn((key: string): string | undefined => {
      const value = entries.get(key);
      return typeof value === "string" ? value : undefined;
    }),
    contains: vi.fn((key: string) => entries.has(key)),
    set: vi.fn((key: string, value: string) => {
      entries.set(key, value);
    }),
    remove: vi.fn((key: string) => entries.delete(key)),
  } satisfies MMKVInstance & { entries: Map<string, unknown> };
}

beforeEach(() => {
  createMMKV.mockReset();
  createMMKV.mockImplementation(() => mockInstance());
});

describe("native KV with an MMKV test double", () => {
  it("does not construct native storage at module import", () => {
    expect(constructionCallsAtImport).toBe(0);
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("uses the default MMKV id and JSON storage", () => {
    const instance = mockInstance();
    createMMKV.mockReturnValue(instance);
    const kv = new KV({ name: "settings" });
    expect(createMMKV).toHaveBeenCalledExactlyOnceWith({ id: "hoke.kv" });
    kv.set("theme", "dark");
    expect(instance.set).toHaveBeenCalledWith('stowtree:["settings","theme"]', '"dark"');
    expect(kv.get("theme")).toBe("dark");
    kv.delete("theme");
    expect(kv.get("theme")).toBeUndefined();
  });

  it("isolates namespaces and reopens data through one injected instance", () => {
    const instance = mockInstance();
    const backend = new MMKVBackend({ instance });
    const first = new KV({ name: "a:b", backend });
    const second = new KV({ name: "a", backend });
    first.set("c", { count: 1 });
    second.set("b:c", { count: 2 });
    expect(first.get("c")).toEqual({ count: 1 });
    expect(second.get("b:c")).toEqual({ count: 2 });
    const reopened = new KV({ name: "a:b", backend: new MMKVBackend({ instance }) });
    expect(reopened.get("c")).toEqual({ count: 1 });
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("accepts a custom MMKV id", () => {
    new MMKVBackend({ id: "user-42" });
    expect(createMMKV).toHaveBeenCalledExactlyOnceWith({ id: "user-42" });
  });

  it("uses an injected MMKV instance without constructing another", () => {
    const instance = mockInstance();
    const backend = new MMKVBackend({ instance });
    backend.write("physical-key", "42");
    expect(backend.read("physical-key")).toBe("42");
    backend.remove("physical-key");
    expect(backend.read("physical-key")).toBeUndefined();
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("allows a custom backend without constructing MMKV", () => {
    const kv = new KV({ name: "custom", backend: new MemoryBackend() });
    kv.set("key", [1, 2]);
    expect(kv.get("key")).toEqual([1, 2]);
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("reports native construction errors as UNAVAILABLE with the original cause", () => {
    const cause = new Error("native module unavailable");
    createMMKV.mockImplementation(() => {
      throw cause;
    });
    expect(() => new KV({ name: "app" })).toThrow(
      expect.objectContaining({ name: "KVStorageError", code: "UNAVAILABLE", cause }),
    );
  });

  it("does not treat an existing non-string MMKV value as a missing key", () => {
    const instance = mockInstance();
    const key = 'stowtree:["app","key"]';
    instance.entries.set(key, 42);
    const kv = new KV({ name: "app", backend: new MMKVBackend({ instance }) });
    expect(() => kv.get("key")).toThrow(
      expect.objectContaining({ code: "READ_FAILED", cause: expect.any(TypeError) }),
    );
    expect(() => kv.set("key", "replacement")).toThrow(KVStorageError);
    expect(instance.entries.get(key)).toBe(42);
    expect(instance.set).not.toHaveBeenCalled();
  });

  it("preserves data and suppresses notifications when native mutations fail", () => {
    const instance = mockInstance();
    const kv = new KV({ name: "app", backend: new MMKVBackend({ instance }) });
    kv.set("key", 1);
    const listener = vi.fn();
    kv.subscribe("key", listener);
    const cause = new Error("native I/O failed");
    instance.set.mockImplementationOnce(() => {
      throw cause;
    });
    expect(() => kv.set("key", 2)).toThrow(
      expect.objectContaining({ code: "WRITE_FAILED", cause }),
    );
    instance.remove.mockImplementationOnce(() => {
      throw cause;
    });
    expect(() => kv.delete("key")).toThrow(
      expect.objectContaining({ code: "DELETE_FAILED", cause }),
    );
    expect(kv.get("key")).toBe(1);
    expect(listener).not.toHaveBeenCalled();
  });

  it("propagates native reads as READ_FAILED with the original cause", () => {
    const instance = mockInstance();
    const cause = new Error("native read failed");
    instance.getString.mockImplementation(() => {
      throw cause;
    });
    const kv = new KV({ name: "app", backend: new MMKVBackend({ instance }) });
    expect(() => kv.get("key")).toThrow(expect.objectContaining({ code: "READ_FAILED", cause }));
  });
});

it("rejects an invalid namespace before constructing MMKV", () => {
  expect(() => new KV({ name: "" })).toThrow(TypeError);
  expect(createMMKV).not.toHaveBeenCalled();
});

it("rejects async injected MMKV mutations directly and through KV", async () => {
  const instance = mockInstance();
  const backend = new MMKVBackend({ instance });
  const kv = new KV({ name: "async-instance", backend });
  kv.set("key", 1);
  const listener = vi.fn();
  kv.subscribe("key", listener);
  instance.set.mockImplementation((async () => {
    throw new Error("async MMKV write");
  }) as never);
  expect(() => backend.write("direct", "1")).toThrow(TypeError);
  expect(() => kv.set("key", 2)).toThrow(expect.objectContaining({ code: "WRITE_FAILED" }));
  instance.remove.mockImplementation((async () => {
    throw new Error("async MMKV delete");
  }) as never);
  expect(() => backend.remove("direct")).toThrow(TypeError);
  expect(() => kv.delete("key")).toThrow(expect.objectContaining({ code: "DELETE_FAILED" }));
  expect(kv.get("key")).toBe(1);
  expect(listener).not.toHaveBeenCalled();
  await new Promise((resolve) => setImmediate(resolve));
});

it("rejects async injected MMKV reads and observes rejected promises", async () => {
  const instance = mockInstance();
  instance.getString.mockImplementation((() =>
    Promise.reject(new Error("async MMKV read"))) as never);
  const backend = new MMKVBackend({ instance });
  const kv = new KV({ name: "async-instance", backend });
  expect(() => backend.read("direct")).toThrow(TypeError);
  expect(() => kv.get("key")).toThrow(expect.objectContaining({ code: "READ_FAILED" }));
  await new Promise((resolve) => setImmediate(resolve));
});

it("rejects async MMKV existence checks and observes rejected promises", async () => {
  const instance = mockInstance();
  instance.contains.mockImplementation((() =>
    Promise.reject(new Error("async MMKV contains"))) as never);
  const backend = new MMKVBackend({ instance });
  const kv = new KV({ name: "async-instance", backend });
  expect(() => backend.read("missing")).toThrow(TypeError);
  expect(() => kv.get("missing")).toThrow(expect.objectContaining({ code: "READ_FAILED" }));
  await new Promise((resolve) => setImmediate(resolve));
});

describe("native platform backend selection", () => {
  it("exports the same source class from both platform entries", () => {
    expect(KV).toBe(SourceKV);
    expect(WebKV).toBe(SourceKV);
  });

  it("selects the native backend from a platform map", () => {
    const web = new MemoryBackend();
    const native = new MemoryBackend();
    const kv = new KV({ name: "platform", backend: { web, native } });
    kv.set("key", "native");
    expect(native.read('stowtree:["platform","key"]')).toBe('"native"');
    expect(web.read('stowtree:["platform","key"]')).toBeUndefined();
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it.each([{}, { web: new MemoryBackend() }])(
    "uses MMKV when a map omits native: %o",
    (backend) => {
      const kv = new KV({ name: "default", backend });
      kv.set("key", 1);
      expect(kv.get("key")).toBe(1);
      expect(createMMKV).toHaveBeenCalledExactlyOnceWith({ id: "hoke.kv" });
    },
  );

  it("never reads the unselected web getter", () => {
    const web = vi.fn(() => {
      throw new Error("unselected web");
    });
    const native = new MemoryBackend();
    const backend = Object.defineProperty({ native }, "web", { get: web });
    const kv = new KV({ name: "selected", backend });
    kv.set("key", 1);
    expect(kv.get("key")).toBe(1);
    expect(web).not.toHaveBeenCalled();
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("rejects an invalid selected backend without constructing the default", () => {
    expect(() => new KV({ name: "invalid", backend: { native: null } as never })).toThrow(
      TypeError,
    );
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("does not fall back when a selected getter throws", () => {
    const failure = new Error("native selection failed");
    const backend = Object.defineProperty({}, "native", {
      get: () => {
        throw failure;
      },
    });
    expect(() => new KV({ name: "selected", backend })).toThrow(failure);
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("does not fall back when the selected backend fails to write", () => {
    const failure = new Error("custom write failed");
    const native = {
      read: () => undefined,
      write: () => {
        throw failure;
      },
      remove: () => {},
    };
    const kv = new KV({ name: "selected", backend: { native } });
    expect(() => kv.set("key", 1)).toThrow(
      expect.objectContaining({ code: "WRITE_FAILED", cause: failure }),
    );
    expect(createMMKV).not.toHaveBeenCalled();
  });

  it("checks constructor options before creating the default and defers value validation", () => {
    expect(() => new KV({ name: "invalid", validate: false as never })).toThrow(TypeError);
    expect(createMMKV).not.toHaveBeenCalled();
    const validate = vi.fn((value: unknown): value is number => typeof value === "number");
    const kv = new KV<number>({
      name: "valid",
      validate: (value: unknown): value is number => validate(value),
    });
    expect(createMMKV).toHaveBeenCalledTimes(1);
    expect(validate).not.toHaveBeenCalled();
    kv.set("key", 1);
    expect(validate).toHaveBeenCalledExactlyOnceWith(1);
  });
});
