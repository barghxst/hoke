/** The wrapper distinguishes an absent key from a stored undefined. */
export interface StorageEntry<Value> {
  value: Value;
}

/** Platform adapter. Successful mutations must be visible to the next read. */
export interface KVStorage<Value> {
  read(key: string): StorageEntry<Value> | undefined;
  write(key: string, value: Value): void;
  remove(key: string): void;
}

export function createMemoryStorage<Value>(): KVStorage<Value> {
  const entries = new Map<string, StorageEntry<Value>>();
  return {
    read: (key) => entries.get(key),
    write: (key, value) => {
      entries.set(key, { value });
    },
    remove: (key) => {
      entries.delete(key);
    },
  };
}
