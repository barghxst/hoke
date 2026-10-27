import { createCollection, type DBCollection } from "./collection.js";
import { type DBListener, type DBRange, WatchRegistry } from "./notifications.js";
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
export { type DBListener, DBNotificationError, type DBRange } from "./notifications.js";
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
  get(key: Uint8Array, options?: DBOperationOptions): Promise<Uint8Array | undefined>;
  put(key: Uint8Array, value: Uint8Array, options?: DBOperationOptions): Promise<void>;
  delete(key: Uint8Array, options?: DBOperationOptions): Promise<void>;
  scan(options?: DBScanOptions, operationOptions?: DBOperationOptions): Promise<DBEntry[]>;
  subscribe(key: Uint8Array, listener: DBListener): () => void;
  subscribeRange(range: DBRange, listener: DBListener): () => void;
  collection(prefix: Uint8Array): DBCollection;
  close(): Promise<void>;
}

/** Structural so consumers do not need DOM types or a global AbortSignal. */
export interface DBAbortSignal {
  readonly aborted: boolean;
  addEventListener(type: "abort", listener: () => void, options?: { once?: boolean }): void;
  removeEventListener(type: "abort", listener: () => void): void;
}

export interface DBOperationOptions {
  signal?: DBAbortSignal;
}

export interface OpenDBOptions extends DBOperationOptions {
  timeoutMs?: number;
}

type Cancellation = {
  aborted: () => boolean;
  add: (listener: () => void) => void;
  remove: (listener: () => void) => void;
};

function cancellation(options?: DBOperationOptions): Cancellation | undefined {
  try {
    if (options === undefined) return undefined;
    if (typeof options !== "object" || options === null) throw new Error();
    const signal = options.signal;
    if (signal === undefined) return undefined;
    if (typeof signal !== "object" || signal === null || typeof signal.aborted !== "boolean")
      throw new Error();
    const add = signal.addEventListener;
    const remove = signal.removeEventListener;
    if (typeof add !== "function" || typeof remove !== "function") throw new Error();
    return {
      aborted: () => {
        const aborted = signal.aborted;
        if (typeof aborted !== "boolean") throw new Error();
        return aborted;
      },
      add: (listener) => add.call(signal, "abort", listener, { once: true }),
      remove: (listener) => remove.call(signal, "abort", listener),
    };
  } catch {
    throw new DBError("INVALID_ARGUMENT", "Expected a structural abort signal");
  }
}

function abortedError(): DBError {
  return new DBError(
    "ABORTED",
    "Database request aborted; dispatched mutation outcome may be unknown",
  );
}

type Command = Request extends infer R
  ? R extends Request
    ? Omit<R, "version" | "id">
    : never
  : never;
type Result = Uint8Array | DBEntry[] | null;
type Pending = {
  scanOptions?: NormalizedScanOptions;
  changedKey?: Uint8Array;
  op: Request["op"];
  resolve: (value: Result) => void;
  reject: (error: Error) => void;
  timer?: ReturnType<typeof setTimeout>;
  settled: boolean;
  detach?: () => void;
};
const ERROR_CODES = new Set<DBErrorCode>([
  "INVALID_ARGUMENT",
  "UNSUPPORTED",
  "LOCKED",
  "STORAGE",
  "ENGINE",
  "CLOSED",
  "TIMEOUT",
  "ABORTED",
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
  private readonly watchers = new WatchRegistry();
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

  async open(name: string, signal?: Cancellation): Promise<void> {
    try {
      await this.request({ op: "open", name }, signal);
      if (this.state !== "opening") {
        throw this.terminalError ?? new DBError("CLOSED", "Database is closed");
      }
      this.state = "open";
    } catch (error) {
      this.fail(error instanceof DBError ? error : new DBError("WORKER", "Cannot open database"));
      throw error;
    }
  }

  async get(key: Uint8Array, options?: DBOperationOptions): Promise<Uint8Array | undefined> {
    this.assertOpen();
    return (
      ((await this.request(
        {
          op: "get",
          key: copyBytes(key, MAX_KEY_BYTES),
        },
        cancellation(options),
      )) as Uint8Array | null) ?? undefined
    );
  }

  async put(key: Uint8Array, value: Uint8Array, options?: DBOperationOptions): Promise<void> {
    this.assertOpen();
    await this.request(
      {
        op: "put",
        key: copyBytes(key, MAX_KEY_BYTES),
        value: copyBytes(value, MAX_VALUE_BYTES),
      },
      cancellation(options),
    );
  }

  async delete(key: Uint8Array, options?: DBOperationOptions): Promise<void> {
    this.assertOpen();
    await this.request({ op: "delete", key: copyBytes(key, MAX_KEY_BYTES) }, cancellation(options));
  }

  async scan(options?: DBScanOptions, operationOptions?: DBOperationOptions): Promise<DBEntry[]> {
    this.assertOpen();
    let normalized: NormalizedScanOptions;
    try {
      normalized = normalizeScanOptions(options);
    } catch {
      throw new DBError("INVALID_ARGUMENT", "Invalid scan options");
    }
    return (await this.request(
      { op: "scan", options: normalized },
      cancellation(operationOptions),
    )) as DBEntry[];
  }

  subscribe(key: Uint8Array, listener: DBListener): () => void {
    this.assertOpen();
    return this.watchers.subscribe(key, listener);
  }

  subscribeRange(range: DBRange, listener: DBListener): () => void {
    this.assertOpen();
    return this.watchers.subscribeRange(range, listener);
  }

  collection(prefix: Uint8Array): DBCollection {
    this.assertOpen();
    return createCollection(this, prefix);
  }

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    if (this.state === "closed") return Promise.resolve();
    this.state = "closing";
    this.watchers.clear();
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
    this.watchers.clear();
    this.terminalError = error;
    this.worker.terminate();
    for (const [id, pending] of this.pending) {
      this.finish(id, pending);
      pending.reject(error);
    }
  }

  /** Mark settled before calling user-owned signal methods, which may reenter. */
  private finish(id: number, pending: Pending): void {
    pending.settled = true;
    this.pending.delete(id);
    clearTimeout(pending.timer);
    try {
      pending.detach?.();
    } catch {
      // A broken custom signal cannot change an ACK or prevent pending cleanup.
    }
  }

  private request(command: Command, signal?: Cancellation): Promise<Result> {
    if (this.state === "closed") return Promise.reject(new DBError("CLOSED", "Database is closed"));
    try {
      if (signal?.aborted()) return Promise.reject(abortedError());
    } catch {
      return Promise.reject(new DBError("INVALID_ARGUMENT", "Cannot read abort signal"));
    }
    if (!Number.isSafeInteger(this.nextId)) {
      const error = new DBError("PROTOCOL", "Database request identifiers exhausted");
      this.fail(error);
      return Promise.reject(error);
    }
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const pending: Pending = {
        op: command.op,
        resolve,
        reject,
        settled: false,
        ...(command.op === "scan" ? { scanOptions: command.options } : {}),
        ...(command.op === "put" || command.op === "delete"
          ? { changedKey: new Uint8Array(command.key) }
          : {}),
      };
      this.pending.set(id, pending);
      let dispatched = false;
      const abort = () => {
        if (pending.settled) return;
        const error = abortedError();
        if (dispatched) this.fail(error);
        else {
          this.finish(id, pending);
          reject(error);
        }
      };
      if (signal) {
        pending.detach = () => signal.remove(abort);
        try {
          signal.add(abort);
          if (!pending.settled && signal.aborted()) abort();
        } catch {
          if (!pending.settled) {
            this.finish(id, pending);
            reject(new DBError("INVALID_ARGUMENT", "Cannot register abort signal"));
          }
        }
      }
      if (pending.settled) return;
      pending.timer = setTimeout(() => {
        this.fail(
          new DBError("TIMEOUT", "Database request timed out; mutation outcome may be unknown"),
        );
      }, this.timeoutMs);
      dispatched = true;
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
      this.finish(response.id, pending);
      const notificationError =
        this.state === "open" && pending.changedKey
          ? this.watchers.notify(pending.changedKey)
          : undefined;
      if (notificationError) pending.reject(notificationError);
      else pending.resolve(result);
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
      this.finish(response.id, pending);
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
  const client = new Client(worker, timeoutMs);
  await client.open(name, signal);
  return client;
}
