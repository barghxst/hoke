import { type DBBatchOperation, normalizeBatch } from "./batch.js";
import { type Cancellation, cancellation } from "./client-cancellation.js";
import {
  type BrowserDB,
  DBError,
  type DBOperationOptions,
  type DBStats,
} from "./client-contract.js";
import { WorkerSession } from "./client-session.js";
import { createCollection, type DBCollection } from "./collection.js";
import { type DBListener, type DBRange, WatchRegistry } from "./notifications.js";
import { type DBMode, MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";
import {
  type DBEntry,
  type DBScanOptions,
  type NormalizedScanOptions,
  normalizeScanOptions,
} from "./scan.js";

function copyBytes(value: Uint8Array, maximum: number): Uint8Array {
  if (!(value instanceof Uint8Array) || value.byteLength > maximum) {
    throw new DBError("INVALID_ARGUMENT", `Expected Uint8Array of at most ${maximum} bytes`);
  }
  try {
    return new Uint8Array(value);
  } catch {
    throw new DBError("INVALID_ARGUMENT", "Cannot read detached byte array");
  }
}

export class Client implements BrowserDB {
  private readonly watchers = new WatchRegistry();
  private readonly session: WorkerSession;

  constructor(
    worker: Worker,
    timeoutMs: number,
    readonly mode: DBMode,
  ) {
    this.session = new WorkerSession(
      worker,
      timeoutMs,
      mode,
      (keys) => this.watchers.notifyMany(keys),
      () => this.watchers.clear(),
    );
  }

  open(name: string, signal?: Cancellation): Promise<void> {
    return this.session.open(name, signal);
  }

  async get(key: Uint8Array, options?: DBOperationOptions): Promise<Uint8Array | undefined> {
    this.session.assertOpen();
    return (
      ((await this.session.request(
        {
          op: "get",
          key: copyBytes(key, MAX_KEY_BYTES),
        },
        cancellation(options),
      )) as Uint8Array | null) ?? undefined
    );
  }

  async put(key: Uint8Array, value: Uint8Array, options?: DBOperationOptions): Promise<void> {
    this.session.assertOpen();
    await this.session.request(
      {
        op: "put",
        key: copyBytes(key, MAX_KEY_BYTES),
        value: copyBytes(value, MAX_VALUE_BYTES),
      },
      cancellation(options),
    );
  }

  async delete(key: Uint8Array, options?: DBOperationOptions): Promise<void> {
    this.session.assertOpen();
    await this.session.request(
      { op: "delete", key: copyBytes(key, MAX_KEY_BYTES) },
      cancellation(options),
    );
  }

  async scan(options?: DBScanOptions, operationOptions?: DBOperationOptions): Promise<DBEntry[]> {
    this.session.assertOpen();
    let normalized: NormalizedScanOptions;
    try {
      normalized = normalizeScanOptions(options);
    } catch {
      throw new DBError("INVALID_ARGUMENT", "Invalid scan options");
    }
    return (await this.session.request(
      { op: "scan", options: normalized },
      cancellation(operationOptions),
    )) as DBEntry[];
  }

  async writeBatch(
    operations: readonly DBBatchOperation[],
    options?: DBOperationOptions,
  ): Promise<void> {
    this.assertContainer();
    let normalized: DBBatchOperation[];
    try {
      normalized = normalizeBatch(operations);
    } catch {
      throw new DBError("INVALID_ARGUMENT", "Invalid batch operations");
    }
    await this.session.request({ op: "batch", operations: normalized }, cancellation(options));
  }

  async flush(options?: DBOperationOptions): Promise<void> {
    this.assertContainer();
    await this.session.request({ op: "flush" }, cancellation(options));
  }

  async compact(options?: DBOperationOptions): Promise<void> {
    this.assertContainer();
    await this.session.request({ op: "compact" }, cancellation(options));
  }

  async stats(options?: DBOperationOptions): Promise<DBStats> {
    this.assertContainer();
    return (await this.session.request({ op: "stats" }, cancellation(options))) as DBStats;
  }

  private assertContainer(): void {
    this.session.assertOpen();
    if (this.mode !== "container-v1") {
      throw new DBError("UNSUPPORTED", "This operation requires container-v1 mode");
    }
  }

  subscribe(key: Uint8Array, listener: DBListener): () => void {
    this.session.assertOpen();
    return this.watchers.subscribe(key, listener);
  }

  subscribeRange(range: DBRange, listener: DBListener): () => void {
    this.session.assertOpen();
    return this.watchers.subscribeRange(range, listener);
  }

  collection(prefix: Uint8Array): DBCollection {
    this.session.assertOpen();
    return createCollection(this, prefix);
  }

  close(): Promise<void> {
    return this.session.close();
  }
}
