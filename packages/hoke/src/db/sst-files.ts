import initWasm, { WasmSst } from "../../wasm/hoke_wasm.js";
import { MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";
import { compareBytes, type DBScanOptions, MAX_SCAN_BYTES, normalizeScanOptions } from "./scan.js";

/** Internal building block only: no manifest, publication, or garbage collection. */
export interface SstEntry {
  key: Uint8Array;
  /** null is a tombstone; a zero-length array is a stored empty value. */
  value: Uint8Array | null;
}
export interface SstSyncHandle {
  getSize(): number;
  read(buffer: Uint8Array, options: { at: number }): number;
  write(buffer: Uint8Array, options: { at: number }): number;
  truncate(size: number): void;
  flush(): void;
  close(): void;
}
export interface SstFileHandle {
  createSyncAccessHandle(): Promise<SstSyncHandle>;
}
export interface SstDirectory {
  getFileHandle(name: string, options: { create: boolean }): Promise<SstFileHandle>;
}
export class SstFileError extends Error {
  constructor(
    message: string,
    readonly candidateName: string | undefined,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "SstFileError";
  }
}

let loading: Promise<unknown> | undefined;
function initialize(): Promise<unknown> {
  loading ??= initWasm().catch((error: unknown) => {
    loading = undefined;
    throw error;
  });
  return loading;
}
function validName(name: string): void {
  if (typeof name !== "string" || !/^[A-Za-z0-9_-]+\.sst$/.test(name)) {
    throw new TypeError("Expected a single SST filename ending in .sst");
  }
}
function validKey(key: Uint8Array): void {
  if (!(key instanceof Uint8Array) || key.byteLength > MAX_KEY_BYTES) {
    throw new TypeError("SST key must be bytes within 64 KiB");
  }
}
function pack(entries: readonly SstEntry[]): Uint8Array {
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
function adapter(handle: SstSyncHandle) {
  return {
    getSize: () => handle.getSize(),
    readAt: (buffer: Uint8Array, at: number) => handle.read(buffer, { at }),
    writeAt: (buffer: Uint8Array, at: number) => handle.write(buffer, { at }),
    truncate: (size: number) => handle.truncate(size),
    flush: () => handle.flush(),
  };
}
function decode(packet: Uint8Array, limit: number): SstEntry[] {
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

/** Owns one exclusive sync handle. Closing never deletes the file. */
export class SstFile {
  private engine: WasmSst | undefined;
  private handle: SstSyncHandle | undefined;
  constructor(
    readonly name: string,
    engine: WasmSst,
    handle: SstSyncHandle,
  ) {
    this.engine = engine;
    this.handle = handle;
  }
  private current(): WasmSst {
    if (!this.engine) throw new SstFileError("SST file is closed", undefined);
    return this.engine;
  }
  /** undefined means missing; null means a stored deletion marker. */
  lookup(key: Uint8Array): Uint8Array | null | undefined {
    validKey(key);
    const result = this.current().lookup(key);
    if (result.length === 1 && result[0] === 0) return undefined;
    if (result.length === 1 && result[0] === 1) return null;
    if (result.length >= 1 && result.length <= MAX_VALUE_BYTES + 1 && result[0] === 2) {
      return result.slice(1);
    }
    throw new SstFileError("Invalid SST lookup tag", undefined);
  }
  scan(options: DBScanOptions = {}): SstEntry[] {
    const { start, end, prefix, reverse, limit } = normalizeScanOptions(options);
    const flags =
      (start && !start.inclusive ? 1 : 0) | (end?.inclusive ? 2 : 0) | (reverse ? 4 : 0);
    return decode(this.current().scan(start?.key, end?.key, prefix, flags, limit), limit);
  }
  close(): void {
    const engine = this.engine;
    const handle = this.handle;
    this.engine = undefined;
    this.handle = undefined;
    let failure: unknown;
    try {
      engine?.close();
    } catch (error) {
      failure = error;
    } finally {
      try {
        engine?.free();
      } catch (error) {
        failure ??= error;
      } finally {
        try {
          handle?.close();
        } catch (error) {
          failure ??= error;
        }
      }
    }
    if (failure !== undefined) throw failure;
  }
}

async function acquire(
  directory: SstDirectory,
  name: string,
  packet?: Uint8Array,
): Promise<SstFile> {
  let handle: SstSyncHandle | undefined;
  try {
    const file = await directory.getFileHandle(name, { create: packet !== undefined });
    handle = await file.createSyncAccessHandle();
    const storage = adapter(handle);
    const engine = packet === undefined ? new WasmSst(storage) : WasmSst.create(storage, packet);
    return new SstFile(name, engine, handle);
  } catch (error) {
    try {
      handle?.close();
    } catch {
      // Preserve the original failure, including failed creation/flush.
    }
    throw new SstFileError(
      packet === undefined
        ? `Cannot open SST ${name}`
        : `Cannot create SST candidate ${name}; an incomplete orphan may remain`,
      packet === undefined ? undefined : name,
      { cause: error },
    );
  }
}

/** Caller must exclusively own the directory while selecting and creating candidates. */
export async function createSstCandidate(
  directory: SstDirectory,
  entries: readonly SstEntry[],
): Promise<SstFile> {
  const packet = pack(entries);
  await initialize();
  for (let attempt = 0; attempt < 16; attempt++) {
    const name = `${crypto.randomUUID()}.sst`;
    try {
      await directory.getFileHandle(name, { create: false });
    } catch (error) {
      if (
        typeof error === "object" &&
        error !== null &&
        "name" in error &&
        error.name === "NotFoundError"
      ) {
        return acquire(directory, name, packet);
      }
      throw new SstFileError("Cannot check SST candidate name", name, { cause: error });
    }
  }
  throw new SstFileError("Cannot allocate a unique SST candidate name", undefined);
}
export async function openSstFile(directory: SstDirectory, name: string): Promise<SstFile> {
  validName(name);
  await initialize();
  return acquire(directory, name);
}
