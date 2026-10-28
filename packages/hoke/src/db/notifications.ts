import { isThenable, observePromise } from "../sync-result.js";
import { DBError } from "./index.js";
import { MAX_KEY_BYTES } from "./protocol.js";
import { compareBytes, type DBScanOptions, normalizeScanOptions } from "./scan.js";

export type DBRange = Pick<DBScanOptions, "start" | "end" | "prefix">;
export type DBListener = () => void;

/** Subscriber failures do not undo the already committed database mutation. */
export class DBNotificationError extends AggregateError {
  readonly code = "SUBSCRIBER_FAILED";
  readonly committed = true;

  constructor(errors: unknown[]) {
    super(errors, "Database commit succeeded, but one or more subscribers failed");
    this.name = "DBNotificationError";
  }
}

type Subscription = { listener: DBListener; matches: (key: Uint8Array) => boolean };

function invalid(): DBError {
  return new DBError("INVALID_ARGUMENT", "Invalid database subscription key, range or listener");
}

function copyKey(key: Uint8Array): Uint8Array {
  try {
    if (!(key instanceof Uint8Array) || key.byteLength > MAX_KEY_BYTES) throw invalid();
    return new Uint8Array(key);
  } catch {
    throw invalid();
  }
}

export function normalizeWatchRange(range: DBRange): DBRange {
  try {
    if (typeof range !== "object" || range === null || Array.isArray(range)) throw invalid();
    if (Reflect.ownKeys(range).some((key) => key !== "start" && key !== "end" && key !== "prefix"))
      throw invalid();
    // Pass only range fields: subscriptions never inherit scan ordering or limits.
    const normalized = normalizeScanOptions({
      start: range.start,
      end: range.end,
      prefix: range.prefix,
    });
    return {
      ...(normalized.start ? { start: normalized.start } : {}),
      ...(normalized.end ? { end: normalized.end } : {}),
      ...(normalized.prefix ? { prefix: normalized.prefix } : {}),
    };
  } catch {
    throw invalid();
  }
}

/** Connection-local synchronous invalidations, in global registration order. */
export class WatchRegistry {
  private readonly subscriptions = new Set<Subscription>();

  subscribe(key: Uint8Array, listener: DBListener): () => void {
    const captured = copyKey(key);
    return this.add(listener, (changed) => compareBytes(captured, changed) === 0);
  }

  subscribeRange(range: DBRange, listener: DBListener): () => void {
    const captured = normalizeWatchRange(range);
    return this.add(listener, (key) => {
      if (captured.start) {
        const order = compareBytes(key, captured.start.key);
        if (order < 0 || (order === 0 && !captured.start.inclusive)) return false;
      }
      if (captured.end) {
        const order = compareBytes(key, captured.end.key);
        if (order > 0 || (order === 0 && !captured.end.inclusive)) return false;
      }
      return (
        !captured.prefix ||
        (key.length >= captured.prefix.length &&
          captured.prefix.every((byte, index) => key[index] === byte))
      );
    });
  }

  notify(key: Uint8Array): DBNotificationError | undefined {
    return this.notifyMany([key]);
  }

  notifyMany(keys: readonly Uint8Array[]): DBNotificationError | undefined {
    // Determine matching registrations before invoking user code. Reentrant calls
    // get their own snapshot; additions cannot join the event already in flight.
    const snapshot = [...this.subscriptions].filter((entry) =>
      keys.some((key) => entry.matches(key)),
    );
    const errors: unknown[] = [];
    for (const entry of snapshot) {
      if (!this.subscriptions.has(entry)) continue;
      try {
        const result: unknown = entry.listener();
        const nativePromise = observePromise(result);
        if (nativePromise || isThenable(result))
          throw new TypeError("Database subscribers must return synchronously");
      } catch (error) {
        errors.push(error);
      }
    }
    return errors.length ? new DBNotificationError(errors) : undefined;
  }

  clear(): void {
    this.subscriptions.clear();
  }

  private add(listener: DBListener, matches: Subscription["matches"]): () => void {
    if (typeof listener !== "function") throw invalid();
    const entry = { listener, matches };
    this.subscriptions.add(entry);
    return () => {
      this.subscriptions.delete(entry);
    };
  }
}
