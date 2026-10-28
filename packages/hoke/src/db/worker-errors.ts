import type { DBErrorCode } from "./protocol.js";

export class WorkerError extends Error {
  constructor(
    readonly code: DBErrorCode,
    message: string,
  ) {
    super(message);
  }
}

export function storageError(error: unknown): WorkerError {
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
