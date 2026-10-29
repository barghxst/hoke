import { afterEach, describe, expect, it, vi } from "vitest";
import { KV } from "../../src/kv/index.js";
import { KV as SourceKV } from "../../src/kv/kv.js";
import { KV as WebKV } from "../../src/kv/web/index.js";
import { MemoryBackend } from "../helpers/memory-backend.js";

function mockStorage() {
  const entries = new Map<string, string>();
  return {
    getItem: vi.fn((key: string) => entries.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => {
      entries.set(key, value);
    }),
    removeItem: vi.fn((key: string) => {
      entries.delete(key);
    }),
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("web platform backend selection", () => {
  it("exports the single source class", () => {
    expect(KV).toBe(SourceKV);
    expect(WebKV).toBe(SourceKV);
  });

  it("selects the web backend from a platform map", () => {
    const web = new MemoryBackend();
    const native = new MemoryBackend();
    const kv = new KV({ name: "platform", backend: { web, native } });
    kv.set("key", "web");
    expect(web.read('stowtree:["platform","key"]')).toBe('"web"');
    expect(native.read('stowtree:["platform","key"]')).toBeUndefined();
  });

  it.each([{}, { native: new MemoryBackend() }])(
    "uses localStorage when a map omits web: %o",
    (backend) => {
      const storage = mockStorage();
      vi.stubGlobal("localStorage", storage);
      const kv = new KV({ name: "default", backend });
      kv.set("key", 1);
      expect(kv.get("key")).toBe(1);
      expect(storage.setItem).toHaveBeenCalledExactlyOnceWith('stowtree:["default","key"]', "1");
    },
  );

  it("never reads the unselected native getter", () => {
    const native = vi.fn(() => {
      throw new Error("unselected native");
    });
    const backend = Object.defineProperty({ web: new MemoryBackend() }, "native", { get: native });
    const kv = new KV({ name: "selected", backend });
    kv.set("key", 1);
    expect(kv.get("key")).toBe(1);
    expect(native).not.toHaveBeenCalled();
  });

  it("rejects an invalid selected backend without falling back to localStorage", () => {
    const storage = mockStorage();
    vi.stubGlobal("localStorage", storage);
    expect(() => new KV({ name: "invalid", backend: { web: null } as never })).toThrow(TypeError);
    expect(storage.getItem).not.toHaveBeenCalled();
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it("does not fall back when a selected getter throws", () => {
    const failure = new Error("web selection failed");
    const backend = Object.defineProperty({}, "web", {
      get: () => {
        throw failure;
      },
    });
    expect(() => new KV({ name: "selected", backend })).toThrow(failure);
  });

  it("does not fall back when the selected backend fails to write", () => {
    const storage = mockStorage();
    vi.stubGlobal("localStorage", storage);
    const failure = new Error("custom write failed");
    const web = {
      read: () => undefined,
      write: () => {
        throw failure;
      },
      remove: () => {},
    };
    const kv = new KV({ name: "selected", backend: { web } });
    expect(() => kv.set("key", 1)).toThrow(
      expect.objectContaining({ code: "WRITE_FAILED", cause: failure }),
    );
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it("validates options before accessing browser storage", () => {
    const storageAccess = vi.fn(() => {
      throw new Error("storage must not be accessed");
    });
    const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
    Object.defineProperty(globalThis, "localStorage", { configurable: true, get: storageAccess });
    try {
      expect(() => new KV({ name: "" })).toThrow(TypeError);
      expect(() => new KV({ name: "invalid", validate: false as never })).toThrow(TypeError);
      expect(storageAccess).not.toHaveBeenCalled();
    } finally {
      if (original) Object.defineProperty(globalThis, "localStorage", original);
      else Reflect.deleteProperty(globalThis, "localStorage");
    }
  });
});
