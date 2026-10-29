import { abortedError, type Cancellation } from "./client-cancellation.js";
import { DBError } from "./client-contract.js";
import { decodeError, decodeResult, type Result } from "./client-response.js";
import { type DBMode, PROTOCOL_VERSION, type Request } from "./protocol.js";
import type { NormalizedScanOptions } from "./scan.js";

type Command = Request extends infer R
  ? R extends Request
    ? Omit<R, "version" | "id">
    : never
  : never;
type Pending = {
  scanOptions?: NormalizedScanOptions;
  changedKeys?: Uint8Array[];
  op: Request["op"];
  resolve: (value: Result) => void;
  reject: (error: Error) => void;
  timer?: ReturnType<typeof setTimeout>;
  settled: boolean;
  detach?: () => void;
};

export class WorkerSession {
  private state: "opening" | "open" | "closing" | "closed" = "opening";
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private closePromise: Promise<void> | undefined;
  private terminalError: DBError | undefined;

  constructor(
    private readonly worker: Worker,
    private readonly timeoutMs: number,
    readonly mode: DBMode,
    private readonly onCommitted: (keys: Uint8Array[]) => Error | undefined,
    private readonly onClosing: () => void,
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
      await this.request(
        this.mode === "container-v1" ? { op: "open", name, mode: this.mode } : { op: "open", name },
        signal,
      );
      if (this.state !== "opening") {
        throw this.terminalError ?? new DBError("CLOSED", "Database is closed");
      }
      this.state = "open";
    } catch (error) {
      this.fail(error instanceof DBError ? error : new DBError("WORKER", "Cannot open database"));
      throw error;
    }
  }

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    if (this.state === "closed") return Promise.resolve();
    this.state = "closing";
    this.onClosing();
    this.closePromise = this.request({ op: "close" }).then(
      () => this.fail(new DBError("CLOSED", "Database is closed")),
      (error: unknown) => {
        this.fail(new DBError("CLOSED", "Database is closed"));
        throw error;
      },
    );
    return this.closePromise;
  }

  assertOpen(): void {
    if (this.state !== "open") throw new DBError("CLOSED", "Database is closed or closing");
  }

  private fail(error: DBError): void {
    if (this.state === "closed") return;
    this.state = "closed";
    this.onClosing();
    this.terminalError = error;
    this.worker.terminate();
    for (const [id, pending] of this.pending) {
      this.finish(id, pending);
      pending.reject(error);
    }
  }

  private finish(id: number, pending: Pending): void {
    pending.settled = true;
    this.pending.delete(id);
    clearTimeout(pending.timer);
    try {
      pending.detach?.();
    } catch {}
  }

  request(command: Command, signal?: Cancellation): Promise<Result> {
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
          ? { changedKeys: [new Uint8Array(command.key)] }
          : command.op === "batch"
            ? { changedKeys: command.operations.map((operation) => new Uint8Array(operation.key)) }
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
        result = decodeResult(pending.op, response.result, pending.scanOptions);
      } catch {
        invalid();
        return;
      }
      this.finish(response.id, pending);
      const notificationError =
        this.state === "open" && pending.changedKeys
          ? this.onCommitted(pending.changedKeys)
          : undefined;
      if (notificationError) pending.reject(notificationError);
      else pending.resolve(result);
    } else {
      let error: DBError;
      try {
        error = decodeError(response.error);
      } catch {
        invalid();
        return;
      }
      this.finish(response.id, pending);
      pending.reject(error);
    }
  }
}
