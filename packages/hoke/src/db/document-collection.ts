import { type DBBatchOperation, MAX_BATCH_BYTES, normalizeBatch } from "./batch.js";
import { cancellation } from "./client-cancellation.js";
import { DBError, type DBOperationOptions } from "./client-contract.js";
import type { DBCollection } from "./collection.js";
import { decodeDocument, decodeString, encodeDocument, encodeString } from "./document-codec.js";
import { type Collection, type CollectionOptions, DocumentError } from "./document-contract.js";
import { documentRange, documentScan } from "./document-range.js";
import { MAX_KEY_BYTES } from "./protocol.js";
import type { DBScanOptions } from "./scan.js";

function snapshotOptions(options?: DBOperationOptions): DBOperationOptions | undefined {
  if (options === undefined) return undefined;
  if (typeof options !== "object" || options === null)
    throw new DBError("INVALID_ARGUMENT", "Expected operation options");
  const signal = options.signal;
  const captured = signal === undefined ? {} : { signal };
  cancellation(captured);
  return captured;
}

export function createDocumentCollection<T>(
  raw: DBCollection,
  options: CollectionOptions<T>,
  execute: <R>(
    operation: () => Promise<R>,
    options?: DBOperationOptions,
    mutation?: boolean,
  ) => Promise<R>,
  namespaceBytes = 0,
): Collection<T> {
  if (typeof options !== "object" || options === null || typeof options.validate !== "function")
    throw new DocumentError(
      "INVALID_ARGUMENT",
      "Collection requires a synchronous validation function",
    );
  const guard = options.validate;
  if (!Number.isSafeInteger(namespaceBytes) || namespaceBytes < 0 || namespaceBytes > MAX_KEY_BYTES)
    throw new DocumentError("INVALID_ARGUMENT", "Invalid collection namespace size");
  function keyBytes(key: unknown): Uint8Array {
    const bytes = encodeString(key);
    if (bytes.length + namespaceBytes > MAX_KEY_BYTES)
      throw new DocumentError("INVALID_ARGUMENT", "Collection key exceeds physical key limit");
    return bytes;
  }
  function checkRange(range: DBScanOptions): void {
    for (const bytes of [range.start?.key, range.end?.key, range.prefix]) {
      if (bytes && bytes.length + namespaceBytes > MAX_KEY_BYTES)
        throw new DocumentError("INVALID_ARGUMENT", "Collection range exceeds physical key limit");
    }
  }
  return {
    async get(key, operationOptions) {
      operationOptions = snapshotOptions(operationOptions);
      const encoded = keyBytes(key);
      return execute(async () => {
        const value = await raw.get(encoded, operationOptions);
        return value === undefined ? undefined : decodeDocument(value, guard);
      }, operationOptions);
    },
    async set(key, value, operationOptions) {
      operationOptions = snapshotOptions(operationOptions);
      const encoded = keyBytes(key);
      const bytes = encodeDocument(value, guard);
      return execute(() => raw.put(encoded, bytes, operationOptions), operationOptions, true);
    },
    async delete(key, operationOptions) {
      operationOptions = snapshotOptions(operationOptions);
      const encoded = keyBytes(key);
      return execute(() => raw.delete(encoded, operationOptions), operationOptions, true);
    },
    async list(options, operationOptions) {
      operationOptions = snapshotOptions(operationOptions);
      const scan = documentScan(options);
      checkRange(scan);
      return execute(
        async () =>
          (await raw.scan(scan, operationOptions)).map(({ key, value }) => ({
            key: decodeString(key),
            value: decodeDocument(value, guard),
          })),
        operationOptions,
      );
    },
    async writeBatch(operations, operationOptions) {
      operationOptions = snapshotOptions(operationOptions);
      let batch: DBBatchOperation[];
      try {
        if (!Array.isArray(operations) || operations.length > 1024)
          throw new TypeError("Expected at most 1024 batch operations");
        batch = normalizeBatch(
          operations.map((operation) => {
            if (typeof operation !== "object" || operation === null)
              throw new TypeError("Invalid collection batch operation");
            const key = keyBytes(operation.key);
            if (operation.type === "set")
              return { type: "put" as const, key, value: encodeDocument(operation.value, guard) };
            if (operation.type === "delete") return { type: "delete" as const, key };
            throw new TypeError("Expected set or delete batch operation");
          }),
        );
        const physicalBytes = batch.reduce(
          (sum, operation) =>
            sum +
            namespaceBytes +
            operation.key.length +
            (operation.type === "put" ? operation.value.length : 0),
          0,
        );
        if (physicalBytes > MAX_BATCH_BYTES) throw new TypeError("Physical batch exceeds 32 MiB");
      } catch (cause) {
        if (cause instanceof DocumentError) throw cause;
        throw new DocumentError("INVALID_ARGUMENT", "Invalid collection batch", cause);
      }
      return execute(
        () => raw.writeBatch(batch, operationOptions),
        operationOptions,
        batch.length > 0,
      );
    },
    subscribe(key, listener) {
      return raw.subscribe(keyBytes(key), listener);
    },
    subscribeRange(range, listener) {
      const encoded = documentRange(range);
      checkRange(encoded);
      return raw.subscribeRange(encoded, listener);
    },
  };
}
