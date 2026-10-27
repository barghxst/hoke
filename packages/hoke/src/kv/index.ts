import type { KVStorage } from "./storage.js";
import { createMemoryStorage } from "./storage.js";

export type {
  Store,
  StoreDefinition,
  StoreEnvelope,
  StoreErrorCode,
  StoreMigration,
} from "../store.js";
export { createStore, defineStore, StoreError } from "../store.js";
export type { LocalStorageOptions, StorageErrorCode, StringStorage } from "./local-storage.js";
export { createLocalStorage, KVStorageError } from "./local-storage.js";
export type { KVStorage, StorageEntry } from "./storage.js";

export type Unsubscribe = () => void;
export type Listener = () => void;

/** Synchronous key/value storage. Missing keys return undefined. */
export interface KV<Value> {
  get(key: string): Value | undefined;
  set(key: string, value: Value): void;
  delete(key: string): void;
  subscribe(key: string, listener: Listener): Unsubscribe;
}

/** Uses isolated memory unless an explicit platform adapter is supplied. */
export function createKV<Value = unknown>(
  options: { storage: KVStorage<Value> } = { storage: createMemoryStorage<Value>() },
): KV<Value> {
  const storage = options.storage;
  // Each subscription has its own identity, even for the same callback.
  const listeners = new Map<string, Set<{ listener: Listener }>>();

  function notify(key: string): void {
    const subscriptions = listeners.get(key);
    if (!subscriptions) return;

    const errors: unknown[] = [];
    // New subscriptions begin with the next change.
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

  return {
    get(key) {
      return storage.read(key)?.value;
    },
    set(key, value) {
      const entry = storage.read(key);
      if (entry && Object.is(entry.value, value)) return;
      storage.write(key, value);
      notify(key);
    },
    delete(key) {
      if (!storage.read(key)) return;
      storage.remove(key);
      notify(key);
    },
    subscribe(key, listener) {
      let subscriptions = listeners.get(key);
      if (!subscriptions) {
        subscriptions = new Set();
        listeners.set(key, subscriptions);
      }
      const subscription = { listener };
      subscriptions.add(subscription);
      return () => {
        subscriptions.delete(subscription);
        if (subscriptions.size === 0 && listeners.get(key) === subscriptions) {
          listeners.delete(key);
        }
      };
    },
  };
}
