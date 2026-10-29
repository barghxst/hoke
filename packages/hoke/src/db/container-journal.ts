import { WasmJournal } from "../../wasm/hoke_wasm.js";
import type { SstDirectory, SstSyncHandle } from "./sst-files.js";
import { initializeWasm } from "./wasm-init.js";

const MAX_SEGMENT_BYTES = 256 * 1024 * 1024;
const MAX_REFS = 4096;
const REF_BYTES = 24;

export class ContainerFileError extends Error {
  constructor(
    readonly fileName: string,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "ContainerFileError";
  }
}
function bytes(value: Uint8Array, maximum: number): Uint8Array {
  if (!(value instanceof Uint8Array) || value.byteLength > maximum) {
    throw new TypeError(`Expected byte array of at most ${maximum} bytes`);
  }
  return new Uint8Array(value);
}
function reference(value: Uint8Array, kind?: number): Uint8Array {
  const copy = bytes(value, REF_BYTES);
  if (
    copy.length !== REF_BYTES ||
    (copy[0] !== 1 && copy[0] !== 2) ||
    (kind !== undefined && copy[0] !== kind)
  ) {
    throw new TypeError("Expected a 24-byte container segment reference of the requested kind");
  }
  if (copy.subarray(1, 8).some((byte) => byte !== 0))
    throw new TypeError("Reference reserved bytes must be zero");
  const view = new DataView(copy.buffer);
  const offset = view.getBigUint64(8, true);
  const length = view.getBigUint64(16, true);
  if (
    offset < 32n ||
    length > BigInt(MAX_SEGMENT_BYTES) ||
    offset + 36n + length > BigInt(Number.MAX_SAFE_INTEGER)
  ) {
    throw new TypeError("Invalid container segment reference range");
  }
  return copy;
}
function packedRefs(values: readonly Uint8Array[], kind: number, offsets: Set<bigint>): Uint8Array {
  if (!Array.isArray(values) || values.length > MAX_REFS)
    throw new TypeError("Expected at most 4096 segment references");
  const result = new Uint8Array(values.length * REF_BYTES);
  values.forEach((value, index) => {
    const encoded = reference(value, kind);
    const offset = new DataView(encoded.buffer).getBigUint64(8, true);
    if (offsets.has(offset)) throw new TypeError("Duplicate container segment reference");
    offsets.add(offset);
    result.set(encoded, index * REF_BYTES);
  });
  return result;
}

export class ContainerJournal {
  constructor(
    readonly name: string,
    private engine: WasmJournal | undefined,
    private handle: SstSyncHandle | undefined,
  ) {}
  private current(): WasmJournal {
    if (!this.engine) throw new ContainerFileError(this.name, "Container journal is closed");
    return this.engine;
  }

  append(kind: "wal" | "sst", payload: Uint8Array): Uint8Array {
    if (kind !== "wal" && kind !== "sst") throw new TypeError("Expected wal or sst segment kind");
    return this.current().append(kind === "wal" ? 1 : 2, bytes(payload, MAX_SEGMENT_BYTES));
  }
  commit(tables: readonly Uint8Array[], wal: readonly Uint8Array[]): void {
    if (!Array.isArray(tables) || !Array.isArray(wal) || tables.length + wal.length > MAX_REFS) {
      throw new TypeError("Expected at most 4096 references per root");
    }
    const offsets = new Set<bigint>();
    const tableRefs = packedRefs(tables, 2, offsets);
    const walRefs = packedRefs(wal, 1, offsets);
    this.current().commit(tableRefs, walRefs);
  }
  read(segment: Uint8Array): Uint8Array {
    return this.current().read(reference(segment));
  }

  root(): Uint8Array {
    return this.current().root();
  }
  recovery(): { discardedTailBytes: bigint; recordsScanned: bigint } {
    const current = this.current();
    return {
      discardedTailBytes: current.discarded_tail_bytes(),
      recordsScanned: current.records_scanned(),
    };
  }
  isPoisoned(): boolean {
    return this.current().is_poisoned();
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

export async function openContainerJournal(
  directory: SstDirectory,
  name: string,
  options: { create?: boolean } = {},
): Promise<ContainerJournal> {
  if (typeof name !== "string" || !/^[A-Za-z0-9_-]+\.hkc$/.test(name))
    throw new TypeError("Expected a single .hkc container filename");
  if (options === null || typeof options !== "object") {
    throw new TypeError("Expected container open options");
  }
  const create = options.create;
  if (create !== undefined && typeof create !== "boolean") {
    throw new TypeError("Expected optional boolean create");
  }
  await initializeWasm();
  let handle: SstSyncHandle | undefined;
  try {
    const file = await directory.getFileHandle(name, { create: create ?? false });
    handle = await file.createSyncAccessHandle();
    const owned = handle;
    const engine = new WasmJournal({
      getSize: () => owned.getSize(),
      readAt: (buffer: Uint8Array, at: number) => owned.read(buffer, { at }),
      writeAt: (buffer: Uint8Array, at: number) => owned.write(buffer, { at }),
      truncate: (size: number) => owned.truncate(size),
      flush: () => owned.flush(),
    });
    return new ContainerJournal(name, engine, handle);
  } catch (error) {
    try {
      handle?.close();
    } catch {}
    throw new ContainerFileError(
      name,
      "Cannot open container journal; file outcome may be unknown",
      { cause: error },
    );
  }
}
