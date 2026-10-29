import { isThenable } from "../sync-result.js";
import type { KVBackend, KVOptions, KVSource, Listener, Unsubscribe } from "./backend.js";
import { KVStorageError } from "./errors.js";
import { assertJSON } from "./json.js";
import { resolveBackend } from "./web/resolve-backend.js";

export class KV<Value = unknown> implements KVSource<Value> {
  private readonly backend: KVBackend;
  private readonly name: string;
  private readonly validate: KVOptions<Value>["validate"];
  private readonly snapshots = new Map<string, { raw: string; value: Value }>();
  private readonly listeners = new Map<string, Set<{ listener: Listener }>>();

  constructor(options: KVOptions<Value>) {
    if (typeof options.name !== "string" || options.name.length === 0) {
      throw new TypeError("KV name must not be empty");
    }
    if (options.validate !== undefined && typeof options.validate !== "function") {
      throw new TypeError("KV validator must be a function");
    }
    this.name = options.name;
    this.backend = resolveBackend(options.backend);
    this.validate = options.validate;
  }

  private storageKey(key: string): string {
    if (typeof key !== "string") throw new TypeError("KV keys must be strings");

    return `stowtree:${JSON.stringify([this.name, key])}`;
  }

  private check(value: unknown): asserts value is Value {
    assertJSON(value);
    if (this.validate) {
      const accepted = this.validate(value);
      if (isThenable(accepted) || accepted !== true) {
        throw new TypeError("Value failed synchronous validation");
      }
    }
  }

  private read(key: string): { value: Value } | undefined {
    const physicalKey = this.storageKey(key);
    let raw: string | undefined;
    try {
      raw = this.backend.read(physicalKey);
      if (isThenable(raw) || (raw !== undefined && typeof raw !== "string")) {
        throw new TypeError("Backend read must return a string or undefined");
      }
    } catch (cause) {
      this.snapshots.delete(key);
      throw new KVStorageError("READ_FAILED", `Failed to read KV key: ${key}`, { cause });
    }
    if (raw === undefined) {
      this.snapshots.delete(key);
      return undefined;
    }
    try {
      const cached = this.snapshots.get(key);
      const value: unknown = cached?.raw === raw ? cached.value : JSON.parse(raw);
      this.check(value);
      if (cached?.raw === raw) return cached;
      const snapshot = { raw, value };
      this.snapshots.set(key, snapshot);
      return snapshot;
    } catch (cause) {
      this.snapshots.delete(key);
      throw new KVStorageError("INVALID_DATA", `Invalid KV data for key: ${key}`, { cause });
    }
  }

  readonly get = (key: string): Value | undefined => this.read(key)?.value;

  readonly set = (key: string, value: Value): void => {
    const entry = this.read(key);
    if (entry && Object.is(entry.value, value)) return;
    let raw: string;
    try {
      this.check(value);
      raw = JSON.stringify(value);
    } catch (cause) {
      throw new KVStorageError("INVALID_VALUE", `Cannot persist KV value for key: ${key}`, {
        cause,
      });
    }
    try {
      const result = this.backend.write(this.storageKey(key), raw);
      if (isThenable(result)) throw new TypeError("Backend write must be synchronous");
    } catch (cause) {
      throw new KVStorageError("WRITE_FAILED", `Failed to write KV key: ${key}`, { cause });
    }
    this.notify(key);
  };

  readonly delete = (key: string): void => {
    if (!this.read(key)) return;
    try {
      const result = this.backend.remove(this.storageKey(key));
      if (isThenable(result)) throw new TypeError("Backend remove must be synchronous");
    } catch (cause) {
      throw new KVStorageError("DELETE_FAILED", `Failed to delete KV key: ${key}`, { cause });
    }
    this.snapshots.delete(key);
    this.notify(key);
  };

  readonly subscribe = (key: string, listener: Listener): Unsubscribe => {
    this.storageKey(key);
    let subscriptions = this.listeners.get(key);
    if (!subscriptions) {
      subscriptions = new Set();
      this.listeners.set(key, subscriptions);
    }
    const subscription = { listener };
    subscriptions.add(subscription);
    return () => {
      subscriptions.delete(subscription);
      if (subscriptions.size === 0 && this.listeners.get(key) === subscriptions) {
        this.listeners.delete(key);
      }
    };
  };

  private notify(key: string): void {
    const subscriptions = this.listeners.get(key);
    if (!subscriptions) return;
    const errors: unknown[] = [];
    for (const subscription of [...subscriptions]) {
      if (!subscriptions.has(subscription)) continue;
      try {
        subscription.listener();
      } catch (error) {
        errors.push(error);
      }
    }
    if (errors.length > 0) {
      throw new AggregateError(errors, `KV subscription failed for key: ${key}`);
    }
  }
}
