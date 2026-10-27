import { createCollection, type DBCollection } from "./collection.js";
import {
  compareBytes,
  type DBEntry,
  type DBScanOptions,
  MAX_SCAN_BYTES,
  MAX_SCAN_LIMIT,
  type NormalizedScanOptions,
  normalizeScanOptions,
} from "./scan.js";

export type { DBCollection } from "./collection.js";
export type { DBEntry, DBScanOptions, ScanBound } from "./scan.js";

import {
  type DBErrorCode,
  MAX_KEY_BYTES,
  MAX_VALUE_BYTES,
  PROTOCOL_VERSION,
  type Request,
  validName,
} from "./protocol.js";

export class DBError extends Error {
  readonly code: DBErrorCode;

  constructor(code: DBErrorCode, message: string) {
    super(message);
    this.name = "DBError";
    this.code = code;
  }
}

export interface BrowserDB {
  /** This first browser engine persists the existing WAL v1 format. */
  readonly mode: "wal-v1";
  get(key: Uint8Array): Promise<Uint8Array | undefined>;
  put(key: Uint8Array, value: Uint8Array): Promise<void>;
  delete(key: Uint8Array): Promise<void>;
  scan(options?: DBScanOptions): Promise<DBEntry[]>;
  collection(prefix: Uint8Array): DBCollection;
  close(): Promise<void>;
}

export interface OpenDBOptions {
  timeoutMs?: number;
}

type Command = Request extends infer R
  ? R extends Request
    ? Omit<R, "version" | "id">
    : never
  : never;
type Result = Uint8Array | DBEntry[] | null;
type Pending = {
  scanOptions?: NormalizedScanOptions;
  op: Request["op"];
  resolve: (value: Result) => void;
  reject: (error: DBError) => void;
  timer: ReturnType<typeof setTimeout>;
};
const ERROR_CODES = new Set<DBErrorCode>([
  "INVALID_ARGUMENT",
  "UNSUPPORTED",
  "LOCKED",
  "STORAGE",
  "ENGINE",
  "CLOSED",
  "TIMEOUT",
  "WORKER",
  "PROTOCOL",
]);

function copyBytes(value: Uint8Array, maximum: number): Uint8Array {
  if (!(value instanceof Uint8Array) || value.byteLength > maximum) {
    throw new DBError("INVALID_ARGUMENT", `Expected Uint8Array of at most ${maximum} bytes`);
  }
  try {
    return new Uint8Array(value);
  } catch {
    throw new DBError("INVALID_ARGUMENT", "Cannot read detached byte array");
  }
}

function decodeScan(value: unknown, options: NormalizedScanOptions): DBEntry[] {
  if (!(value instanceof Uint8Array) || value.byteLength < 4 || value.byteLength > MAX_SCAN_BYTES) {
    throw new Error("Invalid scan packet");
  }
  const view = new DataView(value.buffer, value.byteOffset, value.byteLength);
  const count = view.getUint32(0, true);
  if (count > MAX_SCAN_LIMIT || count > options.limit) throw new Error("Invalid scan count");
  let offset = 4;
  const entries: DBEntry[] = [];
  for (let index = 0; index < count; index++) {
    if (offset + 8 > value.byteLength) throw new Error("Truncated scan packet");
    const keyLength = view.getUint32(offset, true);
    const valueLength = view.getUint32(offset + 4, true);
    offset += 8;
    if (
      keyLength > MAX_KEY_BYTES ||
      valueLength > MAX_VALUE_BYTES ||
      offset + keyLength + valueLength > value.byteLength
    )
      throw new Error("Invalid scan lengths");
    const key = value.slice(offset, offset + keyLength);
    offset += keyLength;
    const entryValue = value.slice(offset, offset + valueLength);
    offset += valueLength;
    const previous = entries.at(-1);
    if (
      previous &&
      (options.reverse
        ? compareBytes(previous.key, key) <= 0
        : compareBytes(previous.key, key) >= 0)
    )
      throw new Error("Invalid scan order");
    if (options.start) {
      const comparison = compareBytes(key, options.start.key);
      if (comparison < 0 || (comparison === 0 && !options.start.inclusive))
        throw new Error("Scan below start");
    }
    if (options.end) {
      const comparison = compareBytes(key, options.end.key);
      if (comparison > 0 || (comparison === 0 && !options.end.inclusive))
        throw new Error("Scan above end");
    }
    if (
      options.prefix &&
      (key.length < options.prefix.length ||
        options.prefix.some((byte, index) => key[index] !== byte))
    )
      throw new Error("Scan outside prefix");
    entries.push({ key, value: entryValue });
  }
  if (offset !== value.byteLength) throw new Error("Trailing scan bytes");
  return entries;
}

class Client implements BrowserDB {
  readonly mode = "wal-v1" as const;
  private state: "opening" | "open" | "closing" | "closed" = "opening";
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private closePromise: Promise<void> | undefined;
  private terminalError: DBError | undefined;

  constructor(
    private readonly worker: Worker,
    private readonly timeoutMs: number,
  ) {
    worker.onmessage = (event: MessageEvent<unknown>) => this.receive(event.data);
    worker.onerror = (event) => {
      event.preventDefault();
      this.fail(new DBError("WORKER", "Database worker failed; mutation outcome may be unknown"));
    };
    worker.onmessageerror = () =>
      this.fail(new DBError("PROTOCOL", "Cannot decode database worker message"));
  }

  async open(name: string): Promise<void> {
    try {
      await this.request({ op: "open", name });
      if (this.state !== "opening") {
        throw this.terminalError ?? new DBError("CLOSED", "Database is closed");
      }
      this.state = "open";
    } catch (error) {
      this.fail(error instanceof DBError ? error : new DBError("WORKER", "Cannot open database"));
      throw error;
    }
  }

  async get(key: Uint8Array): Promise<Uint8Array | undefined> {
    this.assertOpen();
    return (
      ((await this.request({
        op: "get",
        key: copyBytes(key, MAX_KEY_BYTES),
      })) as Uint8Array | null) ?? undefined
    );
  }

  async put(key: Uint8Array, value: Uint8Array): Promise<void> {
    this.assertOpen();
    await this.request({
      op: "put",
      key: copyBytes(key, MAX_KEY_BYTES),
      value: copyBytes(value, MAX_VALUE_BYTES),
    });
  }

  async delete(key: Uint8Array): Promise<void> {
    this.assertOpen();
    await this.request({ op: "delete", key: copyBytes(key, MAX_KEY_BYTES) });
  }

  async scan(options?: DBScanOptions): Promise<DBEntry[]> {
    this.assertOpen();
    let normalized: NormalizedScanOptions;
    try {
      normalized = normalizeScanOptions(options);
    } catch {
      throw new DBError("INVALID_ARGUMENT", "Invalid scan options");
    }
    return (await this.request({ op: "scan", options: normalized })) as DBEntry[];
  }

  collection(prefix: Uint8Array): DBCollection {
    this.assertOpen();
    return createCollection(this, prefix);
  }

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    if (this.state === "closed") return Promise.resolve();
    this.state = "closing";
    this.closePromise = this.request({ op: "close" }).then(
      () => this.fail(new DBError("CLOSED", "Database is closed")),
      (error: unknown) => {
        this.fail(new DBError("CLOSED", "Database is closed"));
        throw error;
      },
    );
    return this.closePromise;
  }

  private assertOpen(): void {
    if (this.state !== "open") throw new DBError("CLOSED", "Database is closed or closing");
  }

  private fail(error: DBError): void {
    if (this.state === "closed") return;
    this.state = "closed";
    this.terminalError = error;
    this.worker.terminate();
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  private request(command: Command): Promise<Result> {
    if (this.state === "closed") return Promise.reject(new DBError("CLOSED", "Database is closed"));
    if (!Number.isSafeInteger(this.nextId)) {
      const error = new DBError("PROTOCOL", "Database request identifiers exhausted");
      this.fail(error);
      return Promise.reject(error);
    }
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.fail(
          new DBError("TIMEOUT", "Database request timed out; mutation outcome may be unknown"),
        );
      }, this.timeoutMs);
      this.pending.set(id, {
        op: command.op,
        resolve,
        reject,
        timer,
        ...(command.op === "scan" ? { scanOptions: command.options } : {}),
      });
      try {
        this.worker.postMessage({ ...command, version: PROTOCOL_VERSION, id });
      } catch {
        this.fail(
          new DBError("WORKER", "Cannot send database request; mutation outcome may be unknown"),
        );
      }
    });
  }

  private receive(message: unknown): void {
    if (this.state === "closed") return;
    const invalid = () => this.fail(new DBError("PROTOCOL", "Invalid database worker response"));
    if (typeof message !== "object" || message === null) {
      invalid();
      return;
    }
    const response = message as Record<string, unknown>;
    if (
      response.version !== PROTOCOL_VERSION ||
      typeof response.id !== "number" ||
      !Number.isSafeInteger(response.id) ||
      response.id <= 0 ||
      typeof response.ok !== "boolean"
    ) {
      invalid();
      return;
    }
    const pending = this.pending.get(response.id);
    if (!pending) {
      invalid();
      return;
    }
    if (response.ok) {
      let result: Result;
      try {
        if (pending.op === "scan" && pending.scanOptions) {
          result = decodeScan(response.result, pending.scanOptions);
        } else if (response.result === null) {
          result = null;
        } else if (
          pending.op === "get" &&
          response.result instanceof Uint8Array &&
          response.result.byteLength <= MAX_VALUE_BYTES
        ) {
          result = new Uint8Array(response.result);
        } else {
          throw new Error("Invalid response result");
        }
      } catch {
        invalid();
        return;
      }
      clearTimeout(pending.timer);
      this.pending.delete(response.id);
      pending.resolve(result);
    } else {
      const error = response.error;
      if (typeof error !== "object" || error === null) {
        invalid();
        return;
      }
      const detail = error as Record<string, unknown>;
      if (
        typeof detail.code !== "string" ||
        !ERROR_CODES.has(detail.code as DBErrorCode) ||
        typeof detail.message !== "string"
      ) {
        invalid();
        return;
      }
      clearTimeout(pending.timer);
      this.pending.delete(response.id);
      pending.reject(new DBError(detail.code as DBErrorCode, detail.message));
    }
  }
}

/** Timeout or transport failure closes the connection. Never automatically retry a mutation. */
export async function openDB(name: string, options: OpenDBOptions = {}): Promise<BrowserDB> {
  if (!validName(name)) throw new DBError("INVALID_ARGUMENT", "Invalid database name");
  if (options === null || typeof options !== "object") {
    throw new DBError("INVALID_ARGUMENT", "Expected database options object");
  }
  const timeoutMs = options.timeoutMs === undefined ? 30_000 : options.timeoutMs;
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 600_000) {
    throw new DBError("INVALID_ARGUMENT", "timeoutMs must be an integer from 1 to 600000");
  }
  if (typeof Worker === "undefined") throw new DBError("UNSUPPORTED", "Worker is unavailable");
  let worker: Worker;
  try {
    worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  } catch {
    throw new DBError("WORKER", "Cannot start database worker");
  }
  const client = new Client(worker, timeoutMs);
  await client.open(name);
  return client;
}
