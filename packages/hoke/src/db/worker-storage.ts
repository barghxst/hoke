import { WasmContainerDb, WasmWal } from "../../wasm/hoke_wasm.js";
import type { DBMode } from "./protocol.js";
import { initializeWasm } from "./wasm-init.js";
import { storageError, WorkerError } from "./worker-errors.js";

interface SyncHandle {
  getSize(): number;
  read(buffer: Uint8Array, options: { at: number }): number;
  write(buffer: Uint8Array, options: { at: number }): number;
  truncate(size: number): void;
  flush(): void;
  close(): void;
}
interface FileHandle {
  createSyncAccessHandle?: () => Promise<SyncHandle>;
}
interface DirectoryHandle {
  getDirectoryHandle(name: string, options: { create: boolean }): Promise<DirectoryHandle>;
  getFileHandle(name: string, options: { create: boolean }): Promise<FileHandle>;
}
export interface WorkerStorageEnvironment {
  isSecureContext: boolean;
  FileSystemFileHandle?: { prototype?: { createSyncAccessHandle?: unknown } };
  navigator: { storage?: { getDirectory?: () => Promise<DirectoryHandle> } };
}

/** Owns the Rust engine together with its exclusive OPFS file handle. */
export class WorkerDatabase {
  private engine: WasmWal | WasmContainerDb | undefined;
  private handle: SyncHandle | undefined;
  private closed = false;

  constructor(private readonly scope: WorkerStorageEnvironment) {}

  requireEngine(): WasmWal | WasmContainerDb {
    if (!this.engine || this.closed) throw new WorkerError("CLOSED", "The database is not open.");
    return this.engine;
  }

  async open(name: string, mode: DBMode = "wal-v1"): Promise<void> {
    if (this.closed || this.engine) {
      throw new WorkerError("CLOSED", "This worker can open a database only once.");
    }
    if (
      !this.scope.isSecureContext ||
      typeof this.scope.navigator.storage?.getDirectory !== "function"
    ) {
      throw new WorkerError(
        "UNSUPPORTED",
        "OPFS requires a secure context and storage.getDirectory.",
      );
    }
    if (typeof this.scope.FileSystemFileHandle?.prototype?.createSyncAccessHandle !== "function") {
      throw new WorkerError(
        "UNSUPPORTED",
        "Synchronous OPFS handles are unavailable in this worker.",
      );
    }
    // Loading or compiling WASM must succeed before creating any OPFS entries.
    await initializeWasm();
    let acquired: SyncHandle | undefined;
    try {
      const root = await this.scope.navigator.storage.getDirectory();
      const container = mode === "container-v1";
      const directory = await root.getDirectoryHandle(
        container ? "hoke-container-v1" : "hoke-wal-v1",
        { create: true },
      );
      const file = await directory.getFileHandle(`${name}.${container ? "hkc" : "wal"}`, {
        create: true,
      });
      if (typeof file.createSyncAccessHandle !== "function") {
        throw new WorkerError(
          "UNSUPPORTED",
          "Synchronous OPFS handles are unavailable in this worker.",
        );
      }
      acquired = await file.createSyncAccessHandle();
    } catch (error) {
      throw error instanceof WorkerError ? error : storageError(error);
    }
    const owned = acquired;
    try {
      const Engine = mode === "container-v1" ? WasmContainerDb : WasmWal;
      this.engine = new Engine({
        getSize: () => owned.getSize(),
        readAt: (buffer: Uint8Array, at: number) => owned.read(buffer, { at }),
        writeAt: (buffer: Uint8Array, at: number) => owned.write(buffer, { at }),
        truncate: (size: number) => owned.truncate(size),
        flush: () => owned.flush(),
      });
      this.handle = owned;
    } catch (error) {
      try {
        owned.close();
      } catch {
        // Preserve the initialization error; the worker no longer owns an engine.
      }
      throw error;
    }
  }

  close(): void {
    this.closed = true;
    const ownedEngine = this.engine;
    const ownedHandle = this.handle;
    this.engine = undefined;
    this.handle = undefined;
    let failure: unknown;
    try {
      ownedEngine?.close();
    } catch (error) {
      failure = error;
    } finally {
      try {
        ownedEngine?.free();
      } catch (error) {
        failure ??= error;
      } finally {
        try {
          ownedHandle?.close();
        } catch (error) {
          failure ??= storageError(error);
        }
      }
    }
    if (failure !== undefined) throw failure;
  }
}
