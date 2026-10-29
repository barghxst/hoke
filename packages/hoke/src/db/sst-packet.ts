import { MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";
import { compareBytes, MAX_SCAN_BYTES } from "./scan.js";
import { SstFileError } from "./sst-storage.js";

export interface SstEntry {
  key: Uint8Array;

  value: Uint8Array | null;
}
export function validKey(key: Uint8Array): void {
  if (!(key instanceof Uint8Array) || key.byteLength > MAX_KEY_BYTES) {
    throw new TypeError("SST key must be bytes within 64 KiB");
  }
}
export function pack(entries: readonly SstEntry[]): Uint8Array {
  if (!Array.isArray(entries)) throw new TypeError("Expected SST entries array");
  if (entries.length > 1_000_000) throw new TypeError("SST input exceeds 1,000,000 entries");
  let size = 4;
  let previous: Uint8Array | undefined;
  for (const entry of entries) {
    if (typeof entry !== "object" || entry === null) throw new TypeError("Invalid SST entry");
    validKey(entry.key);
    if (
      entry.value !== null &&
      (!(entry.value instanceof Uint8Array) || entry.value.byteLength > MAX_VALUE_BYTES)
    ) {
      throw new TypeError("SST value must be null or bytes within 16 MiB");
    }
    if (previous && compareBytes(previous, entry.key) >= 0) {
      throw new TypeError("SST entries must have strictly increasing, unique byte keys");
    }
    previous = entry.key;
    size += 8 + entry.key.byteLength + (entry.value?.byteLength ?? 0);
    if (size > MAX_SCAN_BYTES) throw new TypeError("SST input packet exceeds 32 MiB");
  }
  const packet = new Uint8Array(size);
  const view = new DataView(packet.buffer);
  view.setUint32(0, entries.length, true);
  let offset = 4;
  for (const entry of entries) {
    view.setUint32(offset, entry.key.byteLength, true);
    view.setUint32(offset + 4, entry.value === null ? 0xffffffff : entry.value.byteLength, true);
    packet.set(entry.key, offset + 8);
    offset += 8 + entry.key.byteLength;
    if (entry.value !== null) {
      packet.set(entry.value, offset);
      offset += entry.value.byteLength;
    }
  }
  return packet;
}
export function decode(packet: Uint8Array, limit: number): SstEntry[] {
  if (packet.byteLength < 4 || packet.byteLength > MAX_SCAN_BYTES) {
    throw new SstFileError("Invalid SST scan packet length", undefined);
  }
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  const count = view.getUint32(0, true);
  if (count > limit) throw new SstFileError("Invalid SST scan entry count", undefined);
  const result: SstEntry[] = [];
  let offset = 4;
  for (let i = 0; i < count; i++) {
    if (offset + 8 > packet.length) throw new SstFileError("Truncated SST scan", undefined);
    const keyLength = view.getUint32(offset, true);
    const valueLength = view.getUint32(offset + 4, true);
    offset += 8;
    const deleted = valueLength === 0xffffffff;
    const end = offset + keyLength + (deleted ? 0 : valueLength);
    if (
      keyLength > MAX_KEY_BYTES ||
      (!deleted && valueLength > MAX_VALUE_BYTES) ||
      end > packet.length
    ) {
      throw new SstFileError("Invalid SST scan entry lengths", undefined);
    }
    result.push({
      key: packet.slice(offset, offset + keyLength),
      value: deleted ? null : packet.slice(offset + keyLength, end),
    });
    offset = end;
  }
  if (offset !== packet.length) throw new SstFileError("Trailing SST scan bytes", undefined);
  return result;
}
