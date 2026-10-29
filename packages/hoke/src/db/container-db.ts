import { WasmContainerDb } from "../../wasm/hoke_wasm.js";
import { type DBBatchOperation, packBatch } from "./batch.js";
import { ContainerFileError } from "./container-journal.js";
import { MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";
import { type DBEntry, type DBScanOptions, normalizeScanOptions } from "./scan.js";
import { decodeScan } from "./scan-packet.js";
import type { SstDirectory, SstSyncHandle } from "./sst-files.js";
import { initializeWasm } from "./wasm-init.js";

export type ContainerOperation = DBBatchOperation;
export interface ContainerStats {
  generation: bigint;
  tableCount: number;
  walSegments: number;
  memtableEntries: number;
  memtableBytes: number;
}
function copy(value: Uint8Array, maximum: number): Uint8Array {
  if (!(value instanceof Uint8Array) || value.byteLength > maximum) {
    throw new TypeError(`Expected bytes within ${maximum}`);
  }
  return new Uint8Array(value);
}

export class ContainerDB {
  readonly mode = "container-v1" as const;
  constructor(
    readonly name: string,
    private engine: WasmContainerDb | undefined,
    private handle: SstSyncHandle | undefined,
  ) {}
  private current(): WasmContainerDb {
    if (!this.engine) throw new ContainerFileError(this.name, "Container database is closed");
    return this.engine;
  }
  get(key: Uint8Array): Uint8Array | undefined {
    const value = this.current().get(copy(key, MAX_KEY_BYTES));
    return value === undefined ? undefined : copy(value, MAX_VALUE_BYTES);
  }
  put(key: Uint8Array, value: Uint8Array): void {
    this.current().put(copy(key, MAX_KEY_BYTES), copy(value, MAX_VALUE_BYTES));
  }
  delete(key: Uint8Array): void {
    this.current().delete(copy(key, MAX_KEY_BYTES));
  }
  writeBatch(operations: readonly ContainerOperation[]): void {
    this.current().write_batch(packBatch(operations));
  }
  scan(options: DBScanOptions = {}): DBEntry[] {
    const normalized = normalizeScanOptions(options);
    const { start, end, prefix, reverse, limit } = normalized;
    const flags =
      (start && !start.inclusive ? 1 : 0) | (end?.inclusive ? 2 : 0) | (reverse ? 4 : 0);
    return decodeScan(this.current().scan(start?.key, end?.key, prefix, flags, limit), normalized);
  }
  flush(): void {
    this.current().flush();
  }
  compact(): void {
    this.current().compact();
  }
  isPoisoned(): boolean {
    return this.current().is_poisoned();
  }
  stats(): ContainerStats {
    const packet = this.current().stats();
    if (packet.length !== 40) throw new Error("Invalid container stats packet");
    const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
    const count = (at: number) => {
      const value = view.getBigUint64(at, true);
      if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("Unsafe container counter");
      return Number(value);
    };
    return {
      generation: view.getBigUint64(0, true),
      tableCount: count(8),
      walSegments: count(16),
      memtableEntries: count(24),
      memtableBytes: count(32),
    };
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
    }
    try {
      engine?.free();
    } catch (error) {
      failure ??= error;
    }
    try {
      handle?.close();
    } catch (error) {
      failure ??= error;
    }
    if (failure !== undefined) throw failure;
  }
}
export async function openContainerDatabase(
  directory: SstDirectory,
  name: string,
  options: { create?: boolean } = {},
): Promise<ContainerDB> {
  if (typeof name !== "string" || !/^[A-Za-z0-9_-]+\.hkc$/.test(name))
    throw new TypeError("Expected a single .hkc filename");
  if (typeof options !== "object" || options === null)
    throw new TypeError("Expected container options");
  const create = options.create;
  if (create !== undefined && typeof create !== "boolean")
    throw new TypeError("Expected boolean create");
  await initializeWasm();
  let handle: SstSyncHandle | undefined;
  try {
    const file = await directory.getFileHandle(name, { create: create ?? false });
    const owned = await file.createSyncAccessHandle();
    handle = owned;
    const engine = new WasmContainerDb({
      getSize: () => owned.getSize(),
      readAt: (buffer: Uint8Array, at: number) => owned.read(buffer, { at }),
      writeAt: (buffer: Uint8Array, at: number) => owned.write(buffer, { at }),
      truncate: (size: number) => owned.truncate(size),
      flush: () => owned.flush(),
    });
    return new ContainerDB(name, engine, owned);
  } catch (error) {
    try {
      handle?.close();
    } catch {}
    throw new ContainerFileError(name, "Cannot open container database", { cause: error });
  }
}
