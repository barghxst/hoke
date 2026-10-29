import { Client } from "./client.js";
import { abortedError, cancellation } from "./client-cancellation.js";
import { type BrowserDB, DBError, type OpenDBOptions } from "./client-contract.js";
import { validName } from "./protocol.js";

export async function openDB(name: string, options: OpenDBOptions = {}): Promise<BrowserDB> {
  if (!validName(name)) throw new DBError("INVALID_ARGUMENT", "Invalid database name");
  if (options === null || typeof options !== "object") {
    throw new DBError("INVALID_ARGUMENT", "Expected database options object");
  }
  const mode = options.mode === undefined ? "wal-v1" : options.mode;
  if (mode !== "wal-v1" && mode !== "container-v1") {
    throw new DBError("INVALID_ARGUMENT", "Invalid database mode");
  }
  const timeoutMs = options.timeoutMs === undefined ? 30_000 : options.timeoutMs;
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 600_000) {
    throw new DBError("INVALID_ARGUMENT", "timeoutMs must be an integer from 1 to 600000");
  }
  const signal = cancellation(options);
  try {
    if (signal?.aborted()) throw abortedError();
  } catch (error) {
    if (error instanceof DBError) throw error;
    throw new DBError("INVALID_ARGUMENT", "Cannot read abort signal");
  }
  if (typeof Worker === "undefined") throw new DBError("UNSUPPORTED", "Worker is unavailable");
  let worker: Worker;
  try {
    worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  } catch {
    throw new DBError("WORKER", "Cannot start database worker");
  }
  const client = new Client(worker, timeoutMs, mode);
  await client.open(name, signal);
  return client;
}
