import { isThenable } from "../../sync-result.js";
import type { KVBackend } from "../backend.js";
import { KVStorageError } from "../errors.js";

/** Compatible with browser Storage without requiring DOM types. */
export interface StringStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface LocalStorageBackendOptions {
  storage?: StringStorage;
}

export class LocalStorageBackend implements KVBackend {
  private readonly storage: StringStorage;

  constructor(options: LocalStorageBackendOptions = {}) {
    try {
      const storage =
        options.storage ?? (globalThis as { localStorage?: StringStorage }).localStorage;
      if (!storage) throw new Error("localStorage is not present in this environment");
      this.storage = storage;
    } catch (cause) {
      throw new KVStorageError("UNAVAILABLE", "localStorage is unavailable", { cause });
    }
  }

  read(key: string): string | undefined {
    const value = this.storage.getItem(key);
    if (isThenable(value) || (value !== null && typeof value !== "string")) {
      throw new TypeError("Web Storage getItem must synchronously return a string or null");
    }
    return value ?? undefined;
  }

  write(key: string, value: string): void {
    const result = this.storage.setItem(key, value);
    if (isThenable(result)) throw new TypeError("Web Storage setItem must be synchronous");
  }

  remove(key: string): void {
    const result = this.storage.removeItem(key);
    if (isThenable(result)) throw new TypeError("Web Storage removeItem must be synchronous");
  }
}
