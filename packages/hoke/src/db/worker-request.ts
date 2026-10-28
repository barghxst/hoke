import { normalizeBatch } from "./batch.js";
import {
  MAX_KEY_BYTES,
  MAX_VALUE_BYTES,
  PROTOCOL_VERSION,
  type Request,
  validName,
} from "./protocol.js";
import { normalizeScanOptions } from "./scan.js";
import { WorkerError } from "./worker-errors.js";

export function validate(data: unknown): Request {
  if (typeof data !== "object" || data === null) {
    throw new WorkerError("PROTOCOL", "Expected a request object.");
  }
  const request = data as Record<string, unknown>;
  if (
    request.version !== PROTOCOL_VERSION ||
    !Number.isSafeInteger(request.id) ||
    typeof request.id !== "number" ||
    request.id < 1
  ) {
    throw new WorkerError("PROTOCOL", "Invalid protocol version or request id.");
  }
  if (request.op === "open") {
    if (
      request.mode !== undefined &&
      request.mode !== "wal-v1" &&
      request.mode !== "container-v1"
    ) {
      throw new WorkerError("INVALID_ARGUMENT", "Unknown database mode.");
    }
    if (!validName(request.name)) {
      throw new WorkerError("INVALID_ARGUMENT", "Invalid database name.");
    }
  } else if (request.op === "get" || request.op === "put" || request.op === "delete") {
    if (!(request.key instanceof Uint8Array) || request.key.byteLength > MAX_KEY_BYTES) {
      throw new WorkerError("INVALID_ARGUMENT", "Key must be bytes within the key size limit.");
    }
    if (
      request.op === "put" &&
      (!(request.value instanceof Uint8Array) || request.value.byteLength > MAX_VALUE_BYTES)
    ) {
      throw new WorkerError("INVALID_ARGUMENT", "Value must be bytes within the value size limit.");
    }
  } else if (request.op === "scan") {
    try {
      return { ...request, options: normalizeScanOptions(request.options) } as Request;
    } catch (error) {
      throw new WorkerError(
        "INVALID_ARGUMENT",
        error instanceof Error ? error.message : "Invalid scan options",
      );
    }
  } else if (request.op === "batch") {
    try {
      return { ...request, operations: normalizeBatch(request.operations) } as Request;
    } catch (error) {
      throw new WorkerError(
        "INVALID_ARGUMENT",
        error instanceof Error ? error.message : "Invalid batch operations",
      );
    }
  } else if (
    request.op !== "close" &&
    request.op !== "flush" &&
    request.op !== "compact" &&
    request.op !== "stats"
  ) {
    throw new WorkerError("PROTOCOL", "Unknown database operation.");
  }
  return data as Request;
}
