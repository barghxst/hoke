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
    } catch {}
    throw new SstFileError(
      packet === undefined
        ? `Cannot open SST ${name}`
        : `Cannot create SST candidate ${name}; an incomplete orphan may remain`,
      packet === undefined ? undefined : name,
      { cause: error },
    );
  }
}

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

export async function createSstCandidate(
  directory: SstDirectory,
  entries: readonly SstEntry[],
): Promise<SstFile> {
  const packet = pack(entries);
  await initializeWasm();
  return acquire(directory, await candidateName(directory), packet);
}

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
    for (const name of sources) {
      const file = await directory.getFileHandle(name, { create: false });
      inputs.push(await file.createSyncAccessHandle());
    }
    const name = await candidateName(directory);
    createdName = name;
    const file = await directory.getFileHandle(name, { create: true });
    output = await file.createSyncAccessHandle();

    engine = WasmSst.merge(adapter(output), inputs.map(adapter));

    while (inputs.length > 0) inputs.pop()?.close();
    return new SstFile(name, engine, output);
  } catch (error) {
    try {
      engine?.close();
    } catch {}
    try {
      engine?.free();
    } catch {}
    try {
      output?.close();
    } catch {}
    for (const input of inputs) {
      try {
        input.close();
      } catch {}
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
