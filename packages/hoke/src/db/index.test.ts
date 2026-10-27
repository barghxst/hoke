import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DBError, openDB } from "./index.js";
import { MAX_KEY_BYTES, MAX_VALUE_BYTES, type Request } from "./protocol.js";

class FakeWorker {
  static instances: FakeWorker[] = [];
  readonly requests: Request[] = [];
  readonly terminate = vi.fn();
  onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  throwOnPost = false;

  constructor(
    readonly url: URL,
    readonly options: WorkerOptions,
  ) {
    FakeWorker.instances.push(this);
  }
  postMessage(message: Request) {
    if (this.throwOnPost) throw new Error("transport failure");
    this.requests.push(message);
  }
  respond(id: number, result: Uint8Array | null = null) {
    this.message({ version: 1, id, ok: true, result });
  }
  message(data: unknown) {
    this.onmessage?.({ data } as MessageEvent<unknown>);
  }
  error(id: number, code = "STORAGE") {
    this.message({
      version: 1,
      id,
      ok: false,
      error: { code, message: "storage failed" },
    });
  }
}
function latest(): FakeWorker {
  const worker = FakeWorker.instances.at(-1);
  if (!worker) throw new Error("No worker");
  return worker;
}
async function connected(timeoutMs = 30_000) {
  const opening = openDB("test-db", { timeoutMs });
  const worker = latest();
  expect(worker.requests).toEqual([{ version: 1, id: 1, op: "open", name: "test-db" }]);
  worker.respond(1);
  return { db: await opening, worker };
}

beforeEach(() => {
  FakeWorker.instances = [];
  vi.stubGlobal("Worker", FakeWorker);
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("browser DB connection", () => {
  it("imports without Worker and rejects unsupported environments only when opening", async () => {
    vi.stubGlobal("Worker", undefined);
    vi.resetModules();
    const module = await import("./index.js");
    await expect(module.openDB("test")).rejects.toMatchObject({ code: "UNSUPPORTED" });
  });

  it.each(["", "../database", "a/b", "with spaces", "a".repeat(65)])(
    "rejects invalid name %j before creating a worker",
    async (name) => {
      await expect(openDB(name)).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
      expect(FakeWorker.instances).toHaveLength(0);
    },
  );

  it.each([0, -1, 600_001, 0.5, Number.NaN, Number.POSITIVE_INFINITY, null, "100"])(
    "rejects invalid timeout %j before creating a worker",
    async (timeoutMs) => {
      await expect(openDB("test", { timeoutMs: timeoutMs as number })).rejects.toMatchObject({
        code: "INVALID_ARGUMENT",
      });
      expect(FakeWorker.instances).toHaveLength(0);
    },
  );

  it("does not resurrect an open connection after an immediate transport failure", async () => {
    const opening = openDB("test");
    const worker = latest();
    worker.respond(1);
    worker.onerror?.({ preventDefault: vi.fn() } as unknown as ErrorEvent);
    await expect(opening).rejects.toMatchObject({ code: "WORKER" });
    expect(worker.terminate).toHaveBeenCalledOnce();
  });

  it("wraps worker constructor failure", async () => {
    vi.stubGlobal(
      "Worker",
      class {
        constructor() {
          throw new Error("blocked by CSP");
        }
      },
    );
    await expect(openDB("test")).rejects.toMatchObject({ code: "WORKER" });
  });

  it("terminates a close that never acknowledges", async () => {
    vi.useFakeTimers();
    const { db, worker } = await connected(10);
    const closing = db.close();
    const checked = expect(closing).rejects.toMatchObject({ code: "TIMEOUT" });
    await vi.advanceTimersByTimeAsync(10);
    await checked;
    expect(db.close()).toBe(closing);
    expect(worker.terminate).toHaveBeenCalledOnce();
  });

  it("uses the module worker and exposes WAL mode", async () => {
    const { db, worker } = await connected();
    expect(worker.url.pathname.endsWith("/worker.js")).toBe(true);
    expect(worker.options).toEqual({ type: "module" });
    expect(db.mode).toBe("wal-v1");
    const closing = db.close();
    worker.respond(2);
    await closing;
    expect(worker.terminate).toHaveBeenCalledOnce();
  });

  it("assigns concurrent requests unique IDs and matches out-of-order responses", async () => {
    const { db, worker } = await connected();
    const first = db.get(new Uint8Array([1]));
    const second = db.get(new Uint8Array([2]));
    expect(worker.requests.slice(1).map((request) => request.id)).toEqual([2, 3]);
    worker.respond(3, new Uint8Array([9]));
    worker.respond(2);
    await expect(second).resolves.toEqual(new Uint8Array([9]));
    await expect(first).resolves.toBeUndefined();
    const closing = db.close();
    worker.respond(4);
    await closing;
  });

  it("copies keys, values and results without detaching caller buffers", async () => {
    const { db, worker } = await connected();
    const key = new Uint8Array([1]);
    const value = new Uint8Array([2]);
    const written = db.put(key, value);
    key[0] = 10;
    value[0] = 20;
    expect(worker.requests[1]).toMatchObject({
      key: new Uint8Array([1]),
      value: new Uint8Array([2]),
    });
    expect(key.byteLength).toBe(1);
    expect(value.byteLength).toBe(1);
    worker.respond(2);
    await written;
    const read = db.get(key);
    const result = new Uint8Array([42]);
    worker.respond(3, result);
    result[0] = 99;
    await expect(read).resolves.toEqual(new Uint8Array([42]));
    const closing = db.close();
    worker.respond(4);
    await closing;
  });

  it("validates byte arguments before posting and keeps connection usable", async () => {
    const { db, worker } = await connected();
    await expect(db.get(new Uint8Array(MAX_KEY_BYTES + 1))).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    await expect(
      db.put(new Uint8Array(), new Uint8Array(MAX_VALUE_BYTES + 1)),
    ).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    await expect(db.delete("bad" as unknown as Uint8Array)).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    const detached = new Uint8Array([1]);
    structuredClone(detached.buffer, { transfer: [detached.buffer] });
    await expect(db.get(detached)).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    expect(worker.requests).toHaveLength(1);
    const deleted = db.delete(new Uint8Array());
    worker.respond(2);
    await expect(deleted).resolves.toBeUndefined();
    const closing = db.close();
    worker.respond(3);
    await closing;
  });

  it("propagates operation errors without retry or terminating the connection", async () => {
    const { db, worker } = await connected();
    const put = db.put(new Uint8Array([1]), new Uint8Array([2]));
    worker.error(2);
    await expect(put).rejects.toBeInstanceOf(DBError);
    await expect(put).rejects.toMatchObject({ code: "STORAGE", message: "storage failed" });
    expect(worker.terminate).not.toHaveBeenCalled();
    const get = db.get(new Uint8Array([1]));
    worker.respond(3);
    await expect(get).resolves.toBeUndefined();
    const closing = db.close();
    worker.respond(4);
    await closing;
  });

  it("rejects every pending call on timeout, terminates, and rejects future writes", async () => {
    vi.useFakeTimers();
    const { db, worker } = await connected(10);
    const first = db.put(new Uint8Array([1]), new Uint8Array());
    const second = db.get(new Uint8Array([1]));
    const checked = Promise.all([
      expect(first).rejects.toMatchObject({ code: "TIMEOUT" }),
      expect(second).rejects.toMatchObject({ code: "TIMEOUT" }),
    ]);
    await vi.advanceTimersByTimeAsync(10);
    await checked;
    expect(worker.terminate).toHaveBeenCalledOnce();
    await expect(db.put(new Uint8Array(), new Uint8Array())).rejects.toMatchObject({
      code: "CLOSED",
    });
    worker.respond(2);
    expect(worker.requests).toHaveLength(3);
  });

  it("cleans up a timed-out open and a worker-rejected open", async () => {
    vi.useFakeTimers();
    const opening = openDB("test", { timeoutMs: 10 });
    const timeout = expect(opening).rejects.toMatchObject({ code: "TIMEOUT" });
    await vi.advanceTimersByTimeAsync(10);
    await timeout;
    expect(latest().terminate).toHaveBeenCalledOnce();
    const rejected = openDB("test");
    latest().error(1, "LOCKED");
    await expect(rejected).rejects.toMatchObject({ code: "LOCKED" });
    expect(latest().terminate).toHaveBeenCalledOnce();
  });

  it.each(["error", "messageerror", "post"])("terminates after %s failure", async (kind) => {
    const { db, worker } = await connected();
    const pending = db.get(new Uint8Array());
    const checked = expect(pending).rejects.toMatchObject({
      code: kind === "messageerror" ? "PROTOCOL" : "WORKER",
    });
    if (kind === "error") worker.onerror?.({ preventDefault: vi.fn() } as unknown as ErrorEvent);
    if (kind === "messageerror") worker.onmessageerror?.();
    if (kind === "post") {
      worker.throwOnPost = true;
      await expect(db.delete(new Uint8Array())).rejects.toMatchObject({ code: "WORKER" });
    }
    await checked;
    expect(worker.terminate).toHaveBeenCalledOnce();
    await expect(db.get(new Uint8Array())).rejects.toMatchObject({ code: "CLOSED" });
  });

  it("closes after a worker failure while idle", async () => {
    const { db, worker } = await connected();
    worker.onerror?.({ preventDefault: vi.fn() } as unknown as ErrorEvent);
    await expect(db.get(new Uint8Array())).rejects.toMatchObject({ code: "CLOSED" });
    await db.close();
    expect(worker.requests).toHaveLength(1);
    expect(worker.terminate).toHaveBeenCalledOnce();
  });

  it.each([
    null,
    {},
    { version: 2, id: 2, ok: true, result: null },
    { version: 1, id: 0, ok: true, result: null },
    { version: 1, id: 999, ok: true, result: null },
    { version: 1, id: 2.5, ok: true, result: null },
    { version: 1, id: 2, ok: "yes", result: null },
    { version: 1, id: 2, ok: true, result: "bytes" },
    { version: 1, id: 2, ok: false, error: { code: "NOPE", message: "error" } },
    { version: 1, id: 2, ok: false, error: { code: "STORAGE", message: 4 } },
  ])("rejects malformed response %j and poisons the connection", async (response) => {
    const { db, worker } = await connected();
    const get = db.get(new Uint8Array());
    worker.message(response);
    await expect(get).rejects.toMatchObject({ code: "PROTOCOL" });
    expect(worker.terminate).toHaveBeenCalledOnce();
  });

  it("rejects non-null mutation success and duplicate response IDs", async () => {
    const { db, worker } = await connected();
    const put = db.put(new Uint8Array(), new Uint8Array());
    worker.respond(2, new Uint8Array());
    await expect(put).rejects.toMatchObject({ code: "PROTOCOL" });
    const other = await connected();
    other.worker.respond(1);
    await expect(other.db.get(new Uint8Array())).rejects.toMatchObject({ code: "CLOSED" });
    expect(other.worker.terminate).toHaveBeenCalledOnce();
  });

  it("shares pending close, blocks operations, and terminates only after acknowledgment", async () => {
    const { db, worker } = await connected();
    const closing = db.close();
    expect(db.close()).toBe(closing);
    expect(worker.terminate).not.toHaveBeenCalled();
    await expect(db.delete(new Uint8Array())).rejects.toMatchObject({ code: "CLOSED" });
    worker.respond(2);
    await closing;
    await db.close();
    expect(worker.terminate).toHaveBeenCalledOnce();
    expect(worker.requests).toHaveLength(2);
  });

  it("terminates on close error and rejects all close callers", async () => {
    const { db, worker } = await connected();
    const closing = db.close();
    worker.error(2);
    await expect(closing).rejects.toMatchObject({ code: "STORAGE" });
    expect(db.close()).toBe(closing);
    expect(worker.terminate).toHaveBeenCalledOnce();
  });
});
