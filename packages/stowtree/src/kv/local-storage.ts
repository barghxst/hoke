import type { KVStorage, StorageEntry } from "./storage.js";

/** Compatible with browser Storage; injectable for tests or non-browser hosts. */
export interface StringStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface LocalStorageOptions<Value> {
  name: string;
  /** Checks persisted data at runtime; TypeScript alone cannot validate JSON. */
  validate: (value: unknown) => value is Value;
  storage?: StringStorage;
}

export type StorageErrorCode =
  | "UNAVAILABLE"
  | "READ_FAILED"
  | "WRITE_FAILED"
  | "DELETE_FAILED"
  | "INVALID_DATA"
  | "INVALID_VALUE";

export class KVStorageError extends Error {
  override readonly name = "KVStorageError";

  constructor(
    readonly code: StorageErrorCode,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
  }
}

function browserStorage(): StringStorage {
  try {
    // No browser globals are accessed at module import time (SSR-safe).
    const storage = (globalThis as { localStorage?: StringStorage }).localStorage;
    if (!storage) throw new Error("localStorage is not present in this environment");
    return storage;
  } catch (cause) {
    throw new KVStorageError("UNAVAILABLE", "localStorage is unavailable", { cause });
  }
}

/** Reject values that JSON would silently alter or omit. */
function assertJSON(value: unknown, ancestors = new Set<object>()): void {
  if (value === null || typeof value === "string" || typeof value === "boolean") return;
  if (typeof value === "number" && Number.isFinite(value) && !Object.is(value, -0)) return;
  if (typeof value !== "object" || value === null) {
    throw new TypeError(
      "Expected JSON data (finite numbers, strings, booleans, null, arrays or plain objects)",
    );
  }
  if (ancestors.has(value)) throw new TypeError("Circular JSON data");
  const array = Array.isArray(value);
  if (
    !array &&
    Object.getPrototypeOf(value) !== Object.prototype &&
    Object.getPrototypeOf(value) !== null
  ) {
    throw new TypeError("Only plain objects and arrays can be persisted");
  }
  ancestors.add(value);
  try {
    const keys = Reflect.ownKeys(value);
    if (array && Object.keys(value).length !== value.length) {
      throw new TypeError("Sparse arrays and custom array properties cannot be persisted");
    }
    for (const key of keys) {
      if (array && key === "length") continue;
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor) throw new TypeError("Object properties changed during serialization");
      if (typeof key === "symbol" || !descriptor.enumerable || !("value" in descriptor)) {
        throw new TypeError("Symbol keys, hidden properties and accessors cannot be persisted");
      }
      if (array && (!/^(0|[1-9]\d*)$/.test(key) || Number(key) >= value.length)) {
        throw new TypeError("Custom array properties cannot be persisted");
      }
      assertJSON(descriptor.value, ancestors);
    }
  } finally {
    ancestors.delete(value);
  }
}

/** JSON storage, with a separate namespace for each name. No memory fallback. */
export function createLocalStorage<Value>(options: LocalStorageOptions<Value>): KVStorage<Value> {
  if (!options.name) throw new TypeError("localStorage name must not be empty");
  const storage = options.storage ?? browserStorage();
  // A JSON tuple prevents namespace/key collisions, even with punctuation.
  const storageKey = (key: string) => `stowtree:${JSON.stringify([options.name, key])}`;

  const snapshots = new Map<string, { raw: string; entry: StorageEntry<Value> }>();

  return {
    read(key) {
      let raw: string | null;
      try {
        raw = storage.getItem(storageKey(key));
      } catch (cause) {
        snapshots.delete(key);
        throw new KVStorageError("READ_FAILED", `Failed to read localStorage key: ${key}`, {
          cause,
        });
      }
      if (raw === null) {
        snapshots.delete(key);
        return undefined;
      }
      try {
        // Always consult the backend; only the parsed snapshot is cached.
        const cached = snapshots.get(key);
        const value: unknown = cached?.raw === raw ? cached.entry.value : JSON.parse(raw);
        assertJSON(value);
        // Validators may depend on changing application state, so run on every read.
        if (!options.validate(value)) throw new TypeError("Stored value failed validation");
        if (cached?.raw === raw) return cached.entry;
        const entry = { value };
        snapshots.set(key, { raw, entry });
        return entry;
      } catch (cause) {
        snapshots.delete(key);
        throw new KVStorageError("INVALID_DATA", `Invalid localStorage data for key: ${key}`, {
          cause,
        });
      }
    },
    write(key, value) {
      let raw: string;
      try {
        assertJSON(value);
        if (!options.validate(value)) throw new TypeError("Value failed validation");
        raw = JSON.stringify(value);
      } catch (cause) {
        throw new KVStorageError(
          "INVALID_VALUE",
          `Cannot persist localStorage value for key: ${key}`,
          { cause },
        );
      }
      try {
        storage.setItem(storageKey(key), raw);
      } catch (cause) {
        throw new KVStorageError("WRITE_FAILED", `Failed to write localStorage key: ${key}`, {
          cause,
        });
      }
    },
    remove(key) {
      try {
        storage.removeItem(storageKey(key));
        snapshots.delete(key);
      } catch (cause) {
        throw new KVStorageError("DELETE_FAILED", `Failed to delete localStorage key: ${key}`, {
          cause,
        });
      }
    },
  };
}
