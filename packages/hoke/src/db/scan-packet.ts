import { MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";
import {
  compareBytes,
  type DBEntry,
  MAX_SCAN_BYTES,
  MAX_SCAN_LIMIT,
  type NormalizedScanOptions,
} from "./scan.js";

/** Decode bounded live rows shared by Worker responses and internal layered reads. */
export function decodeScan(value: unknown, options: NormalizedScanOptions): DBEntry[] {
  if (!(value instanceof Uint8Array) || value.byteLength < 4 || value.byteLength > MAX_SCAN_BYTES) {
    throw new Error("Invalid scan packet");
  }
  const view = new DataView(value.buffer, value.byteOffset, value.byteLength);
  const count = view.getUint32(0, true);
  if (count > MAX_SCAN_LIMIT || count > options.limit) throw new Error("Invalid scan count");
  let offset = 4;
  const entries: DBEntry[] = [];
  for (let index = 0; index < count; index++) {
    if (offset + 8 > value.byteLength) throw new Error("Truncated scan packet");
    const keyLength = view.getUint32(offset, true);
    const valueLength = view.getUint32(offset + 4, true);
    offset += 8;
    if (
      keyLength > MAX_KEY_BYTES ||
      valueLength > MAX_VALUE_BYTES ||
      offset + keyLength + valueLength > value.byteLength
    )
      throw new Error("Invalid scan lengths");
    const key = value.slice(offset, offset + keyLength);
    offset += keyLength;
    const entryValue = value.slice(offset, offset + valueLength);
    offset += valueLength;
    const previous = entries.at(-1);
    if (
      previous &&
      (options.reverse
        ? compareBytes(previous.key, key) <= 0
        : compareBytes(previous.key, key) >= 0)
    )
      throw new Error("Invalid scan order");
    if (options.start) {
      const comparison = compareBytes(key, options.start.key);
      if (comparison < 0 || (comparison === 0 && !options.start.inclusive))
        throw new Error("Scan below start");
    }
    if (options.end) {
      const comparison = compareBytes(key, options.end.key);
      if (comparison > 0 || (comparison === 0 && !options.end.inclusive))
        throw new Error("Scan above end");
    }
    if (
      options.prefix &&
      (key.length < options.prefix.length ||
        options.prefix.some((byte, index) => key[index] !== byte))
    )
      throw new Error("Scan outside prefix");
    entries.push({ key, value: entryValue });
  }
  if (offset !== value.byteLength) throw new Error("Trailing scan bytes");
  return entries;
}
