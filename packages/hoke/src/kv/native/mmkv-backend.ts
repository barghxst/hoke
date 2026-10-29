import { createMMKV } from "react-native-mmkv";
import { isThenable } from "../../sync-result.js";
import type { KVBackend } from "../backend.js";
import { KVStorageError } from "../errors.js";

/** Minimal MMKV surface; applications may inject an already configured instance. */
export interface MMKVInstance {
  getString(key: string): string | undefined;
  contains(key: string): boolean;
  set(key: string, value: string): void;
  remove(key: string): boolean;
}

export interface MMKVBackendOptions {
  instance?: MMKVInstance;
  id?: string;
}

export class MMKVBackend implements KVBackend {
  private readonly instance: MMKVInstance;

  constructor(options: MMKVBackendOptions = {}) {
    try {
      this.instance = options.instance ?? createMMKV({ id: options.id ?? "hoke.kv" });
    } catch (cause) {
      throw new KVStorageError("UNAVAILABLE", "MMKV is unavailable", { cause });
    }
  }

  read(key: string): string | undefined {
    const value = this.instance.getString(key);
    if (isThenable(value) || (value !== undefined && typeof value !== "string")) {
      throw new TypeError("MMKV getString must synchronously return a string or undefined");
    }
    if (value === undefined) {
      const exists = this.instance.contains(key);
      if (isThenable(exists) || typeof exists !== "boolean") {
        throw new TypeError("MMKV contains must synchronously return a boolean");
      }
      if (exists) throw new TypeError("Expected a JSON string in MMKV");
    }
    return value;
  }

  write(key: string, value: string): void {
    const result = this.instance.set(key, value);
    if (isThenable(result)) throw new TypeError("MMKV set must be synchronous");
  }

  remove(key: string): void {
    const result = this.instance.remove(key);
    if (isThenable(result)) throw new TypeError("MMKV remove must be synchronous");
  }
}
