import initWasm, { WasmWal } from "../../wasm/hoke_wasm.js";
import {
  type DBErrorCode,
  MAX_KEY_BYTES,
  MAX_VALUE_BYTES,
  PROTOCOL_VERSION,
  type Request,
  type Response,
  validName,
} from "./protocol.js";

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
interface WorkerScope {
  isSecureContext: boolean;
  FileSystemFileHandle?: { prototype?: { createSyncAccessHandle?: unknown } };
  navigator: { storage?: { getDirectory?: () => Promise<DirectoryHandle> } };
  postMessage(response: Response): void;
  addEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
}

const scope = globalThis as unknown as WorkerScope;
let engine: WasmWal | undefined;
let handle: SyncHandle | undefined;
let closed = false;

class WorkerError extends Error {
  constructor(
    readonly code: DBErrorCode,
    message: string,
  ) {
    super(message);
  }
}

function storageError(error: unknown): WorkerError {
  const name = error instanceof Error ? error.name : "";
  const message = error instanceof Error ? error.message : String(error);
  if (name === "NoModificationAllowedError") {
    return new WorkerError("LOCKED", "This database is already open in another worker or tab.");
  }
  if (name === "NotSupportedError") {
    return new WorkerError("UNSUPPORTED", message);
  }
  return new WorkerError("STORAGE", message);
}

function validate(data: unknown): Request {
  if (typeof data !== "object" || data === null) {
    throw new WorkerError("PROTOCOL", "Expected a request object.");
  }
  const request = data as Record<string, unknown>;
  if (
    request.version !== PROTOCOL_VERSION ||
    !Number.isSafeInteger(request.id) ||
    typeof request.id !== "number" ||
    request.id < 1
  ) {
    throw new WorkerError("PROTOCOL", "Invalid protocol version or request id.");
  }
  if (request.op === "open") {
    if (!validName(request.name)) {
      throw new WorkerError("INVALID_ARGUMENT", "Invalid database name.");
    }
  } else if (request.op === "get" || request.op === "put" || request.op === "delete") {
    if (!(request.key instanceof Uint8Array) || request.key.byteLength > MAX_KEY_BYTES) {
      throw new WorkerError("INVALID_ARGUMENT", "Key must be bytes within the key size limit.");
    }
    if (
      request.op === "put" &&
      (!(request.value instanceof Uint8Array) || request.value.byteLength > MAX_VALUE_BYTES)
    ) {
      throw new WorkerError("INVALID_ARGUMENT", "Value must be bytes within the value size limit.");
    }
  } else if (request.op !== "close") {
    throw new WorkerError("PROTOCOL", "Unknown database operation.");
  }
  return data as Request;
}

async function open(name: string): Promise<void> {
  if (closed || engine) {
    throw new WorkerError("CLOSED", "This worker can open a database only once.");
  }
  if (!scope.isSecureContext || typeof scope.navigator.storage?.getDirectory !== "function") {
    throw new WorkerError(
      "UNSUPPORTED",
      "OPFS requires a secure context and storage.getDirectory.",
    );
  }
  if (typeof scope.FileSystemFileHandle?.prototype?.createSyncAccessHandle !== "function") {
    throw new WorkerError(
      "UNSUPPORTED",
      "Synchronous OPFS handles are unavailable in this worker.",
    );
  }
  // Loading or compiling WASM must succeed before creating any OPFS entries.
  await initWasm();
  let acquired: SyncHandle | undefined;
  try {
    const root = await scope.navigator.storage.getDirectory();
    const directory = await root.getDirectoryHandle("hoke-wal-v1", { create: true });
    const file = await directory.getFileHandle(`${name}.wal`, { create: true });
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
    engine = new WasmWal({
      getSize: () => owned.getSize(),
      readAt: (buffer: Uint8Array, at: number) => owned.read(buffer, { at }),
      writeAt: (buffer: Uint8Array, at: number) => owned.write(buffer, { at }),
      truncate: (size: number) => owned.truncate(size),
      flush: () => owned.flush(),
    });
    handle = owned;
  } catch (error) {
    try {
      owned.close();
    } catch {
      // Preserve the initialization error; the worker no longer owns an engine.
    }
    throw error;
  }
}

function close(): void {
  closed = true;
  const ownedEngine = engine;
  const ownedHandle = handle;
  engine = undefined;
  handle = undefined;
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

async function execute(request: Request): Promise<Uint8Array | null> {
  if (request.op === "open") {
    await open(request.name);
    return null;
  }
  if (request.op === "close") {
    close();
    return null;
  }
  if (!engine || closed) throw new WorkerError("CLOSED", "The database is not open.");
  switch (request.op) {
    case "get":
      return engine.get(request.key) ?? null;
    case "put":
      engine.put(request.key, request.value);
      return null;
    case "delete":
      engine.delete(request.key);
      return null;
  }
}

async function respond(data: unknown): Promise<void> {
  const candidate = data as { id?: unknown } | null;
  const id =
    candidate && typeof candidate.id === "number" && Number.isSafeInteger(candidate.id)
      ? candidate.id
      : 0;
  try {
    const request = validate(data);
    const result = await execute(request);
    scope.postMessage({ version: PROTOCOL_VERSION, id: request.id, ok: true, result });
  } catch (error) {
    scope.postMessage({
      version: PROTOCOL_VERSION,
      id,
      ok: false,
      error: {
        code: error instanceof WorkerError ? error.code : "ENGINE",
        message: error instanceof Error ? error.message : String(error),
      },
    });
  }
}

// In particular, requests queued during WASM initialization cannot overtake open.
let queue = Promise.resolve();
let transportFailed = false;
scope.addEventListener("message", (event) => {
  if (transportFailed) return;
  queue = queue
    .then(() => respond(event.data))
    .catch((error: unknown) => {
      // Even the error response could not be delivered. Release ownership now,
      // then surface an error event so the client terminates every pending call.
      transportFailed = true;
      try {
        close();
      } catch {
        // Preserve the transport error; close already attempted every resource.
      }
      setTimeout(() => {
        throw error instanceof Error ? error : new Error("Database worker transport failed");
      }, 0);
    });
});
