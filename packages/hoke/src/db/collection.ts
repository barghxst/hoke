import { type BrowserDB, DBError, type DBOperationOptions } from "./index.js";
import { type DBListener, type DBRange, normalizeWatchRange } from "./notifications.js";
import { MAX_KEY_BYTES } from "./protocol.js";
import {
  type DBEntry,
  type DBScanOptions,
  type NormalizedScanOptions,
  normalizeScanOptions,
} from "./scan.js";

/** A byte-key namespace sharing its database's ownership and lifetime. */
export interface DBCollection {
  get(key: Uint8Array, options?: DBOperationOptions): Promise<Uint8Array | undefined>;
  put(key: Uint8Array, value: Uint8Array, options?: DBOperationOptions): Promise<void>;
  delete(key: Uint8Array, options?: DBOperationOptions): Promise<void>;
  scan(options?: DBScanOptions, operationOptions?: DBOperationOptions): Promise<DBEntry[]>;
  subscribe(key: Uint8Array, listener: DBListener): () => void;
  subscribeRange(range: DBRange, listener: DBListener): () => void;
}

export function createCollection(db: BrowserDB, prefix: Uint8Array): DBCollection {
  if (!(prefix instanceof Uint8Array) || prefix.byteLength > MAX_KEY_BYTES - 5) {
    throw new DBError(
      "INVALID_ARGUMENT",
      "Collection prefix must be Uint8Array within 65531 bytes",
    );
  }
  let namespace: Uint8Array;
  try {
    namespace = new Uint8Array(5 + prefix.length);
    namespace[0] = 1;
    new DataView(namespace.buffer).setUint32(1, prefix.length, false);
    namespace.set(new Uint8Array(prefix), 5);
  } catch {
    throw new DBError("INVALID_ARGUMENT", "Cannot read collection prefix");
  }
  function physical(key: Uint8Array): Uint8Array {
    if (!(key instanceof Uint8Array) || key.byteLength > MAX_KEY_BYTES - namespace.length) {
      throw new DBError("INVALID_ARGUMENT", "Collection key exceeds physical key limit");
    }
    try {
      const result = new Uint8Array(namespace.length + key.length);
      result.set(namespace);
      result.set(new Uint8Array(key), namespace.length);
      return result;
    } catch {
      throw new DBError("INVALID_ARGUMENT", "Cannot read collection key");
    }
  }
  return {
    async get(key, options) {
      const translated = physical(key);
      return options === undefined ? db.get(translated) : db.get(translated, options);
    },
    async put(key, value, options) {
      const translated = physical(key);
      if (options === undefined) await db.put(translated, value);
      else await db.put(translated, value, options);
    },
    async delete(key, options) {
      const translated = physical(key);
      if (options === undefined) await db.delete(translated);
      else await db.delete(translated, options);
    },
    subscribe(key, listener) {
      return db.subscribe(physical(key), listener);
    },
    subscribeRange(range, listener) {
      const normalized = normalizeWatchRange(range);
      const translated: DBRange = {
        prefix: physical(normalized.prefix ?? new Uint8Array()),
      };
      if (normalized.start)
        translated.start = {
          key: physical(normalized.start.key),
          inclusive: normalized.start.inclusive,
        };
      if (normalized.end)
        translated.end = {
          key: physical(normalized.end.key),
          inclusive: normalized.end.inclusive,
        };
      return db.subscribeRange(translated, listener);
    },
    async scan(options, operationOptions) {
      let normalized: NormalizedScanOptions;
      try {
        normalized = normalizeScanOptions(options);
      } catch {
        throw new DBError("INVALID_ARGUMENT", "Invalid collection scan options");
      }
      const translated: DBScanOptions = {
        prefix: physical(normalized.prefix ?? new Uint8Array()),
        reverse: normalized.reverse,
        limit: normalized.limit,
      };
      if (normalized.start)
        translated.start = {
          key: physical(normalized.start.key),
          inclusive: normalized.start.inclusive,
        };
      if (normalized.end)
        translated.end = { key: physical(normalized.end.key), inclusive: normalized.end.inclusive };
      const entries = await (operationOptions === undefined
        ? db.scan(translated)
        : db.scan(translated, operationOptions));
      return entries.map(({ key, value }) => ({ key: key.slice(namespace.length), value }));
    },
  };
}
