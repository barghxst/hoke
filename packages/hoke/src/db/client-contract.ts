import type { DBBatchOperation } from "./batch.js";
import type { DBCollection } from "./collection.js";
import type { DBListener, DBRange } from "./notifications.js";
import type { DBErrorCode, DBMode } from "./protocol.js";
import type { DBEntry, DBScanOptions } from "./scan.js";

export class DBError extends Error {
  readonly code: DBErrorCode;

  constructor(code: DBErrorCode, message: string) {
    super(message);
    this.name = "DBError";
    this.code = code;
  }
}

export interface BrowserDB {
  readonly mode: DBMode;
  writeBatch(operations: readonly DBBatchOperation[], options?: DBOperationOptions): Promise<void>;
  flush(options?: DBOperationOptions): Promise<void>;
  compact(options?: DBOperationOptions): Promise<void>;
  stats(options?: DBOperationOptions): Promise<DBStats>;
  get(key: Uint8Array, options?: DBOperationOptions): Promise<Uint8Array | undefined>;
  put(key: Uint8Array, value: Uint8Array, options?: DBOperationOptions): Promise<void>;
  delete(key: Uint8Array, options?: DBOperationOptions): Promise<void>;
  scan(options?: DBScanOptions, operationOptions?: DBOperationOptions): Promise<DBEntry[]>;
  subscribe(key: Uint8Array, listener: DBListener): () => void;
  subscribeRange(range: DBRange, listener: DBListener): () => void;
  collection(prefix: Uint8Array): DBCollection;
  close(): Promise<void>;
}

export interface DBStats {
  generation: bigint;
  tableCount: number;
  walSegments: number;
  memtableEntries: number;
  memtableBytes: number;
}

export interface DBAbortSignal {
  readonly aborted: boolean;
  addEventListener(type: "abort", listener: () => void, options?: { once?: boolean }): void;
  removeEventListener(type: "abort", listener: () => void): void;
}

export interface DBOperationOptions {
  signal?: DBAbortSignal;
}

export interface OpenDBOptions extends DBOperationOptions {
  mode?: DBMode;
  timeoutMs?: number;
}
