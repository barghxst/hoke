import { encodeString } from "./document-codec.js";
import {
  type CollectionRange,
  type CollectionScanOptions,
  DocumentError,
} from "./document-contract.js";
import { type DBRange, normalizeWatchRange } from "./notifications.js";
import { type DBScanOptions, normalizeScanOptions } from "./scan.js";

function translate(input: unknown, scan: boolean): DBScanOptions {
  if (typeof input !== "object" || input === null || Array.isArray(input))
    throw new TypeError("Expected collection range object");
  const fields = scan ? ["start", "end", "prefix", "reverse", "limit"] : ["start", "end", "prefix"];
  if (Reflect.ownKeys(input).some((key) => typeof key !== "string" || !fields.includes(key)))
    throw new TypeError("Unknown collection range field");
  const source = input as Record<string, unknown>;
  const result: DBScanOptions = {};
  if (source.prefix !== undefined) result.prefix = encodeString(source.prefix);
  for (const field of ["start", "end"] as const) {
    const value = source[field];
    if (value === undefined) continue;
    if (
      typeof value !== "object" ||
      value === null ||
      Array.isArray(value) ||
      Reflect.ownKeys(value).some((key) => key !== "key" && key !== "inclusive")
    )
      throw new TypeError("Invalid string bound");
    const bound = value as Record<string, unknown>;
    if (typeof bound.inclusive !== "boolean")
      throw new TypeError("Bound inclusive must be boolean");
    result[field] = { key: encodeString(bound.key), inclusive: bound.inclusive };
  }
  if (scan) {
    if (source.reverse !== undefined) result.reverse = source.reverse as boolean;
    if (source.limit !== undefined) result.limit = source.limit as number;
  }
  return result;
}

export function documentScan(options: CollectionScanOptions = {}): DBScanOptions {
  try {
    return normalizeScanOptions(translate(options, true));
  } catch (cause) {
    throw new DocumentError("INVALID_ARGUMENT", "Invalid collection list options", cause);
  }
}

export function documentRange(range: CollectionRange): DBRange {
  try {
    return normalizeWatchRange(translate(range, false));
  } catch (cause) {
    throw new DocumentError("INVALID_ARGUMENT", "Invalid collection subscription range", cause);
  }
}
