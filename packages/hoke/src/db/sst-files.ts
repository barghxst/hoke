import { WasmSst } from "../../wasm/hoke_wasm.js";
import { pack, type SstEntry } from "./sst-packet.js";
import { SstFile } from "./sst-reader.js";
import {
  adapter,
  type SstDirectory,
  SstFileError,
  type SstSyncHandle,
  validName,
} from "./sst-storage.js";
import { initializeWasm } from "./wasm-init.js";

export type { SstEntry } from "./sst-packet.js";
export { SstFile } from "./sst-reader.js";
export {
  type SstDirectory,
  SstFileError,
  type SstFileHandle,
  type SstSyncHandle,
} from "./sst-storage.js";

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

/** Directory ownership is required: lookup and creation are separate OPFS calls. */
async function candidateName(directory: SstDirectory): Promise<string> {
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
        return name;
      }
      throw new SstFileError("Cannot check SST candidate name", undefined, { cause: error });
    }
  }
  throw new SstFileError("Cannot allocate a unique SST candidate name", undefined);
}

/** Caller must exclusively own the directory while selecting and creating candidates. */
export async function createSstCandidate(
  directory: SstDirectory,
  entries: readonly SstEntry[],
): Promise<SstFile> {
  const packet = pack(entries);
  await initializeWasm();
  return acquire(directory, await candidateName(directory), packet);
}

/**
 * Build an unpublished merge candidate, oldest source first and newest last.
 * Source files remain untouched; tombstones are retained. No manifest is changed.
 * Caller must own the directory and release any SstFile on these source names first.
 */
export async function mergeSstCandidates(
  directory: SstDirectory,
  names: readonly string[],
): Promise<SstFile> {
  if (!Array.isArray(names) || names.length > 64) {
    throw new TypeError("Expected at most 64 SST source filenames");
  }
  const sources = [...names];
  const unique = new Set<string>();
  for (const name of sources) {
    validName(name);
    if (unique.has(name)) throw new TypeError("SST source filenames must be unique");
    unique.add(name);
  }
  await initializeWasm();
  const inputs: SstSyncHandle[] = [];
  let output: SstSyncHandle | undefined;
  let engine: WasmSst | undefined;
  let createdName: string | undefined;
  try {
    // Acquire every source before creating output: a missing/locked input leaves no orphan.
    for (const name of sources) {
      const file = await directory.getFileHandle(name, { create: false });
      inputs.push(await file.createSyncAccessHandle());
    }
    const name = await candidateName(directory);
    createdName = name;
    const file = await directory.getFileHandle(name, { create: true });
    output = await file.createSyncAccessHandle();
    // WASM validates source metadata before destination writes. Winning record
    // payloads are checked while streaming; merge is not a full source scrub.
    engine = WasmSst.merge(adapter(output), inputs.map(adapter));
    // Do not transfer output ownership until every source handle has closed.
    // Pop first: cleanup must not close a handle twice when its close throws.
    while (inputs.length > 0) inputs.pop()?.close();
    return new SstFile(name, engine, output);
  } catch (error) {
    try {
      engine?.close();
    } catch {
      // Preserve the first failure.
    }
    try {
      engine?.free();
    } catch {
      // Preserve the first failure.
    }
    try {
      output?.close();
    } catch {
      // Preserve the first failure.
    }
    for (const input of inputs) {
      try {
        input.close();
      } catch {
        // Continue releasing other handles; preserve the first failure.
      }
    }
    throw new SstFileError(
      createdName === undefined
        ? "Cannot acquire SST merge sources or candidate"
        : `Cannot merge SST candidate ${createdName}; an incomplete orphan may remain`,
      createdName,
      { cause: error },
    );
  }
}
export async function openSstFile(directory: SstDirectory, name: string): Promise<SstFile> {
  validName(name);
  await initializeWasm();
  return acquire(directory, name);
}
