import { WasmContainerDb } from "../../wasm/hoke_wasm.js";
import { packBatch } from "./batch.js";
import { PROTOCOL_VERSION, type Request, type Response } from "./protocol.js";
import { MAX_SCAN_BYTES } from "./scan.js";
import { WorkerError } from "./worker-errors.js";
import { validate } from "./worker-request.js";
import { WorkerDatabase, type WorkerStorageEnvironment } from "./worker-storage.js";

interface WorkerScope extends WorkerStorageEnvironment {
  postMessage(response: Response): void;
  addEventListener(type: "message", listener: (event: MessageEvent<unknown>) => void): void;
}

const scope = globalThis as unknown as WorkerScope;
const database = new WorkerDatabase(scope);

async function execute(request: Request): Promise<Uint8Array | null> {
  if (request.op === "open") {
    await database.open(request.name, request.mode);
    return null;
  }
  if (request.op === "close") {
    database.close();
    return null;
  }
  const engine = database.requireEngine();
  switch (request.op) {
    case "batch":
    case "flush":
    case "compact":
    case "stats": {
      if (!(engine instanceof WasmContainerDb)) {
        throw new WorkerError("UNSUPPORTED", "This operation requires container-v1 mode.");
      }
      if (request.op === "batch") engine.write_batch(packBatch(request.operations));
      else if (request.op === "flush") engine.flush();
      else if (request.op === "compact") engine.compact();
      else return engine.stats();
      return null;
    }
    case "get":
      return engine.get(request.key) ?? null;
    case "put":
      engine.put(request.key, request.value);
      return null;
    case "delete":
      engine.delete(request.key);
      return null;
    case "scan": {
      const { start, end, prefix, reverse, limit } = request.options;
      const flags =
        (start && !start.inclusive ? 1 : 0) | (end?.inclusive ? 2 : 0) | (reverse ? 4 : 0);
      const result = engine.scan(start?.key, end?.key, prefix, flags, limit);
      if (result.byteLength > MAX_SCAN_BYTES) {
        throw new WorkerError("ENGINE", "Scan response exceeds the byte budget");
      }
      return result;
    }
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

let queue = Promise.resolve();
let transportFailed = false;
scope.addEventListener("message", (event) => {
  if (transportFailed) return;
  queue = queue
    .then(() => respond(event.data))
    .catch((error: unknown) => {
      transportFailed = true;
      try {
        database.close();
      } catch {}
      setTimeout(() => {
        throw error instanceof Error ? error : new Error("Database worker transport failed");
      }, 0);
    });
});
