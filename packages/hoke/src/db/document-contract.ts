import type { DBOperationOptions } from "./client-contract.js";
import type { DBListener } from "./notifications.js";

export type DocumentErrorCode = "INVALID_ARGUMENT" | "INVALID_VALUE" | "INVALID_DATA";

/** Input errors and stored-data errors never rewrite the source record. */
export class DocumentError extends Error {
  constructor(
    readonly code: DocumentErrorCode,
    message: string,
    cause?: unknown,
  ) {
    super(message, cause === undefined ? undefined : { cause });
    this.name = "DocumentError";
  }
}

export interface CollectionOptions<T> {
  validate: (value: unknown) => value is T;
}

export interface CollectionBound {
  key: string;
  inclusive: boolean;
}

/** Bounds use encoded UTF-8 byte order, without locale collation or normalization. */
export interface CollectionRange {
  start?: CollectionBound;
  end?: CollectionBound;
  prefix?: string;
}

export interface CollectionScanOptions extends CollectionRange {
  reverse?: boolean;
  limit?: number;
}

export type CollectionBatchOperation<T> =
  | { type: "set"; key: string; value: T }
  | { type: "delete"; key: string };

export interface Collection<T> {
  get(key: string, options?: DBOperationOptions): Promise<T | undefined>;
  set(key: string, value: T, options?: DBOperationOptions): Promise<void>;
  delete(key: string, options?: DBOperationOptions): Promise<void>;
  list(
    options?: CollectionScanOptions,
    operationOptions?: DBOperationOptions,
  ): Promise<Array<{ key: string; value: T }>>;
  writeBatch(
    operations: readonly CollectionBatchOperation<T>[],
    options?: DBOperationOptions,
  ): Promise<void>;
  subscribe(key: string, listener: DBListener): () => void;
  subscribeRange(range: CollectionRange, listener: DBListener): () => void;
}
