import { MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";

export const MAX_BATCH_OPERATIONS = 1024;
export const MAX_BATCH_BYTES = 32 * 1024 * 1024;
export type DBBatchOperation =
  | { type: "put"; key: Uint8Array; value: Uint8Array }
  | { type: "delete"; key: Uint8Array };

/** Snapshot the entire input before dispatch; repeated keys retain input order. */
export function normalizeBatch(operations: unknown): DBBatchOperation[] {
  if (!Array.isArray(operations) || operations.length > MAX_BATCH_OPERATIONS) {
    throw new TypeError("Batch must have at most 1024 operations");
  }
  let total = 0;
  const result: DBBatchOperation[] = [];
  for (const operation of operations) {
    if (typeof operation !== "object" || operation === null)
      throw new TypeError("Invalid batch operation");
    const type = operation.type;
    if (type !== "put" && type !== "delete")
      throw new TypeError("Expected put or delete operation");
    const key = operation.key;
    const value = type === "put" ? operation.value : undefined;
    if (!(key instanceof Uint8Array) || key.byteLength > MAX_KEY_BYTES)
      throw new TypeError("Invalid batch key");
    if (type === "put" && (!(value instanceof Uint8Array) || value.byteLength > MAX_VALUE_BYTES))
      throw new TypeError("Invalid batch value");
    total += key.byteLength + (value?.byteLength ?? 0);
    if (total > MAX_BATCH_BYTES) throw new TypeError("Batch payload exceeds 32 MiB");
    const ownedKey = new Uint8Array(key);
    result.push(
      type === "put"
        ? { type, key: ownedKey, value: new Uint8Array(value) }
        : { type, key: ownedKey },
    );
  }
  return result;
}

/** Ephemeral WASM request packet; the persistent payload uses the existing WAL codec. */
export function packBatch(operations: readonly DBBatchOperation[]): Uint8Array {
  const owned = normalizeBatch(operations);
  const payload = owned.reduce(
    (sum, op) => sum + op.key.length + (op.type === "put" ? op.value.length : 0),
    0,
  );
  const packet = new Uint8Array(4 + owned.length * 9 + payload);
  const view = new DataView(packet.buffer);
  view.setUint32(0, owned.length, true);
  let at = 4;
  for (const operation of owned) {
    const value = operation.type === "put" ? operation.value : new Uint8Array();
    packet[at] = operation.type === "put" ? 1 : 2;
    view.setUint32(at + 1, operation.key.length, true);
    view.setUint32(at + 5, value.length, true);
    packet.set(operation.key, at + 9);
    packet.set(value, at + 9 + operation.key.length);
    at += 9 + operation.key.length + value.length;
  }
  return packet;
}
