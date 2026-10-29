export type StorageErrorCode =
  | "UNAVAILABLE"
  | "READ_FAILED"
  | "WRITE_FAILED"
  | "DELETE_FAILED"
  | "INVALID_DATA"
  | "INVALID_VALUE";

export class KVStorageError extends Error {
  override readonly name = "KVStorageError";

  constructor(
    readonly code: StorageErrorCode,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
  }
}
