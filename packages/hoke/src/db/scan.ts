import { MAX_KEY_BYTES } from "./protocol.js";

export const DEFAULT_SCAN_LIMIT = 100;
export const MAX_SCAN_LIMIT = 1000;
export const MAX_SCAN_BYTES = 32 * 1024 * 1024;

export interface ScanBound {
  key: Uint8Array;
  inclusive: boolean;
}
export interface DBScanOptions {
  start?: ScanBound;
  end?: ScanBound;
  prefix?: Uint8Array;
  reverse?: boolean;
  limit?: number;
}
export interface DBEntry {
  key: Uint8Array;
  value: Uint8Array;
}
export interface NormalizedScanOptions {
  start?: ScanBound;
  end?: ScanBound;
  prefix?: Uint8Array;
  reverse: boolean;
  limit: number;
}

export function compareBytes(left: Uint8Array, right: Uint8Array): number {
  for (let i = 0; i < Math.min(left.length, right.length); i++) {
    const difference = (left[i] ?? 0) - (right[i] ?? 0);
    if (difference) return difference;
  }
  return left.length - right.length;
}

function bytes(value: unknown): Uint8Array {
  if (!(value instanceof Uint8Array) || value.byteLength > MAX_KEY_BYTES) {
    throw new TypeError("Scan key/prefix must be Uint8Array within 64 KiB");
  }
  return new Uint8Array(value);
}
function bound(value: unknown): ScanBound {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("Scan bound requires key and inclusive");
  }
  const input = value as Record<string, unknown>;
  if (
    typeof input.inclusive !== "boolean" ||
    Object.keys(input).some((key) => key !== "key" && key !== "inclusive")
  ) {
    throw new TypeError("Scan bound requires a boolean inclusive flag");
  }
  return { key: bytes(input.key), inclusive: input.inclusive };
}

export function normalizeScanOptions(value: unknown = {}): NormalizedScanOptions {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("Expected scan options object");
  }
  const input = value as Record<string, unknown>;
  const fields = new Set(["start", "end", "prefix", "reverse", "limit"]);
  if (Object.keys(input).some((key) => !fields.has(key)))
    throw new TypeError("Unknown scan option");
  const result: NormalizedScanOptions = {
    reverse: input.reverse === undefined ? false : (input.reverse as boolean),
    limit: input.limit === undefined ? DEFAULT_SCAN_LIMIT : (input.limit as number),
  };
  if (
    typeof result.reverse !== "boolean" ||
    !Number.isInteger(result.limit) ||
    result.limit < 0 ||
    result.limit > MAX_SCAN_LIMIT
  ) {
    throw new TypeError("Scan requires boolean reverse and integer limit from 0 to 1000");
  }
  if (input.start !== undefined) result.start = bound(input.start);
  if (input.end !== undefined) result.end = bound(input.end);
  if (input.prefix !== undefined) result.prefix = bytes(input.prefix);
  if (result.start && result.end && compareBytes(result.start.key, result.end.key) > 0) {
    throw new TypeError("Scan start must not exceed end");
  }
  return result;
}
