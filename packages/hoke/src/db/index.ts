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
type Pending = {
  op: Request["op"];
  resolve: (value: Uint8Array | null) => void;
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
    return (await this.request({ op: "get", key: copyBytes(key, MAX_KEY_BYTES) })) ?? undefined;
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

  private request(command: Command): Promise<Uint8Array | null> {
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
      this.pending.set(id, { op: command.op, resolve, reject, timer });
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
      if (
        response.result !== null &&
        !(
          pending.op === "get" &&
          response.result instanceof Uint8Array &&
          response.result.byteLength <= MAX_VALUE_BYTES
        )
      ) {
        invalid();
        return;
      }
      clearTimeout(pending.timer);
      this.pending.delete(response.id);
      pending.resolve(
        response.result instanceof Uint8Array ? new Uint8Array(response.result) : null,
      );
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
