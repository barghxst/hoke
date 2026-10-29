import { WasmReadView, type WasmWal } from "../../wasm/hoke_wasm.js";
import { MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";
import { type DBEntry, type DBScanOptions, normalizeScanOptions } from "./scan.js";
import { decodeScan } from "./scan-packet.js";
import { type SstDirectory, SstFileError, type SstSyncHandle } from "./sst-files.js";
import { initializeWasm } from "./wasm-init.js";

function keyBytes(key: Uint8Array): Uint8Array {
  if (!(key instanceof Uint8Array) || key.byteLength > MAX_KEY_BYTES) {
    throw new TypeError("Read key must be bytes within 64 KiB");
  }
  return new Uint8Array(key);
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

export class SstReadView {
  constructor(
    private engine: WasmReadView | undefined,
    private handles: SstSyncHandle[],
  ) {}
  private current(): WasmReadView {
    if (!this.engine) throw new SstFileError("SST read view is closed", undefined);
    return this.engine;
  }
  get(wal: WasmWal, key: Uint8Array): Uint8Array | undefined {
    const result = this.current().get(wal, keyBytes(key));
    if (result === undefined) return undefined;
    if (!(result instanceof Uint8Array) || result.byteLength > MAX_VALUE_BYTES) {
      throw new SstFileError("Invalid read view value", undefined);
    }
    return new Uint8Array(result);
  }
  scan(wal: WasmWal, options: DBScanOptions = {}): DBEntry[] {
    const normalized = normalizeScanOptions(options);
    const { start, end, prefix, reverse, limit } = normalized;
    const flags =
      (start && !start.inclusive ? 1 : 0) | (end?.inclusive ? 2 : 0) | (reverse ? 4 : 0);
    return decodeScan(
      this.current().scan(wal, start?.key, end?.key, prefix, flags, limit),
      normalized,
    );
  }

  close(): void {
    const engine = this.engine;
    const handles = this.handles;
    this.engine = undefined;
    this.handles = [];
    let failure: unknown;
    try {
      engine?.close();
    } catch (error) {
      failure = error;
    }
    try {
      engine?.free();
    } catch (error) {
      failure ??= error;
    }
    for (const handle of handles) {
      try {
        handle.close();
      } catch (error) {
        failure ??= error;
      }
    }
    if (failure !== undefined) throw failure;
  }
}

export async function openSstReadView(
  directory: SstDirectory,
  names: readonly string[],
): Promise<SstReadView> {
  if (!Array.isArray(names) || names.length > 64)
    throw new TypeError("Expected at most 64 SST filenames");
  const copied = [...names];
  const seen = new Set<string>();
  for (const name of copied) {
    if (typeof name !== "string" || !/^[A-Za-z0-9_-]+\.sst$/.test(name) || seen.has(name)) {
      throw new TypeError("Expected distinct single SST filenames ending in .sst");
    }
    seen.add(name);
  }
  await initializeWasm();
  const handles: SstSyncHandle[] = [];
  try {
    for (const name of copied) {
      const file = await directory.getFileHandle(name, { create: false });
      handles.push(await file.createSyncAccessHandle());
    }
    const engine = new WasmReadView(handles.map(adapter));
    return new SstReadView(engine, handles);
  } catch (error) {
    for (const handle of handles) {
      try {
        handle.close();
      } catch {}
    }
    throw new SstFileError("Cannot open SST read view", undefined, { cause: error });
  }
}
