import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type DBAbortSignal, DBError, DBNotificationError, type DBRange, openDB } from "./index.js";
import { MAX_KEY_BYTES, MAX_VALUE_BYTES, PROTOCOL_VERSION, type Request } from "./protocol.js";

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
    this.message({ version: PROTOCOL_VERSION, id, ok: true, result });
  }
  message(data: unknown) {
    this.onmessage?.({ data } as MessageEvent<unknown>);
  }
  error(id: number, code = "STORAGE") {
    this.message({
      version: PROTOCOL_VERSION,
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
  expect(worker.requests).toEqual([
    { version: PROTOCOL_VERSION, id: 1, op: "open", name: "test-db" },
  ]);
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
    { version: PROTOCOL_VERSION + 1, id: 2, ok: true, result: null },
    { version: PROTOCOL_VERSION, id: 0, ok: true, result: null },
    { version: PROTOCOL_VERSION, id: 999, ok: true, result: null },
    { version: PROTOCOL_VERSION, id: 2.5, ok: true, result: null },
    { version: PROTOCOL_VERSION, id: 2, ok: "yes", result: null },
    { version: PROTOCOL_VERSION, id: 2, ok: true, result: "bytes" },
    { version: PROTOCOL_VERSION, id: 2, ok: false, error: { code: "NOPE", message: "error" } },
    { version: PROTOCOL_VERSION, id: 2, ok: false, error: { code: "STORAGE", message: 4 } },
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

function packet(entries: Array<[number[], number[]]>): Uint8Array {
  const result = new Uint8Array(
    4 + entries.reduce((sum, [key, value]) => sum + 8 + key.length + value.length, 0),
  );
  const view = new DataView(result.buffer);
  view.setUint32(0, entries.length, true);
  let offset = 4;
  for (const [key, value] of entries) {
    view.setUint32(offset, key.length, true);
    view.setUint32(offset + 4, value.length, true);
    offset += 8;
    result.set(key, offset);
    offset += key.length;
    result.set(value, offset);
    offset += value.length;
  }
  return result;
}

async function finish(db: Awaited<ReturnType<typeof openDB>>, worker: FakeWorker) {
  const closing = db.close();
  worker.respond(worker.requests.at(-1)?.id ?? 0);
  await closing;
}

describe("browser scans", () => {
  it("shares collection request IDs, operation errors and the connection lifetime", async () => {
    const { db, worker } = await connected();
    const collection = db.collection(new Uint8Array([7]));
    const read = collection.get(new Uint8Array());
    expect(worker.requests[1]).toMatchObject({
      op: "get",
      key: new Uint8Array([1, 0, 0, 0, 1, 7]),
    });
    worker.error(2);
    await expect(read).rejects.toMatchObject({ code: "STORAGE" });
    const scanning = collection.scan();
    worker.respond(3, packet([[[1, 0, 0, 0, 1, 7, 8], [9]]]));
    await expect(scanning).resolves.toEqual([
      { key: new Uint8Array([8]), value: new Uint8Array([9]) },
    ]);
    const closing = db.close();
    await expect(collection.scan()).rejects.toMatchObject({ code: "CLOSED" });
    expect(() => db.collection(new Uint8Array())).toThrow(DBError);
    worker.respond(4);
    await closing;
    await expect(collection.put(new Uint8Array(), new Uint8Array())).rejects.toMatchObject({
      code: "CLOSED",
    });
    expect(worker.requests).toHaveLength(4);
  });

  it("copies bounds/prefix synchronously and decodes independent byte slices", async () => {
    const { db, worker } = await connected();
    const key = new Uint8Array([2]);
    const scanning = db.scan({
      start: { key, inclusive: true },
      end: { key, inclusive: true },
      prefix: key,
      limit: 1,
    });
    key[0] = 8;
    expect(worker.requests[1]).toMatchObject({
      op: "scan",
      options: {
        start: { key: new Uint8Array([2]), inclusive: true },
        end: { key: new Uint8Array([2]), inclusive: true },
        prefix: new Uint8Array([2]),
        reverse: false,
        limit: 1,
      },
    });
    const bytes = packet([[[2], [3]]]);
    worker.respond(2, bytes);
    bytes.fill(9);
    await expect(scanning).resolves.toEqual([
      { key: new Uint8Array([2]), value: new Uint8Array([3]) },
    ]);
    await finish(db, worker);
  });

  it("handles reverse order, exclusive endpoints, zero limit and empty keys", async () => {
    const { db, worker } = await connected();
    const scanning = db.scan({
      reverse: true,
      start: { key: new Uint8Array([0]), inclusive: false },
      end: { key: new Uint8Array([3]), inclusive: false },
    });
    worker.respond(
      2,
      packet([
        [[2], []],
        [[1], []],
      ]),
    );
    expect(await scanning).toHaveLength(2);
    const zero = db.scan({ limit: 0 });
    worker.respond(3, packet([]));
    await expect(zero).resolves.toEqual([]);
    const empty = db.scan();
    worker.respond(4, packet([[[], []]]));
    await expect(empty).resolves.toEqual([{ key: new Uint8Array(), value: new Uint8Array() }]);
    await finish(db, worker);
  });

  it.each([
    null,
    [],
    { limit: -1 },
    { limit: 1001 },
    { limit: 1.5 },
    { limit: NaN },
    { reverse: 1 },
    { start: { key: new Uint8Array() } },
    { prefix: "str" },
    { unknown: true },
    {
      start: { key: new Uint8Array([2]), inclusive: true },
      end: { key: new Uint8Array([1]), inclusive: true },
    },
  ])("rejects invalid scan options %j before posting", async (options) => {
    const { db, worker } = await connected();
    await expect(db.scan(options as Parameters<typeof db.scan>[0])).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(worker.requests).toHaveLength(1);
    await finish(db, worker);
  });

  it.each([
    null,
    new Uint8Array(3),
    new Uint8Array([1, 0, 0, 0]),
    new Uint8Array([0, 0, 0, 0, 0]),
    packet([
      [[2], []],
      [[1], []],
    ]),
    packet([
      [[1], []],
      [[1], []],
    ]),
    (() => {
      const value = new Uint8Array(12);
      const view = new DataView(value.buffer);
      view.setUint32(0, 1, true);
      view.setUint32(4, MAX_KEY_BYTES + 1, true);
      return value;
    })(),
    (() => {
      const value = new Uint8Array(12);
      const view = new DataView(value.buffer);
      view.setUint32(0, 1, true);
      view.setUint32(8, MAX_VALUE_BYTES + 1, true);
      return value;
    })(),
    new Uint8Array(32 * 1024 * 1024 + 1),
  ])("rejects malformed scan packets and all pending requests", async (bytes) => {
    const { db, worker } = await connected();
    const scanning = db.scan();
    const reading = db.get(new Uint8Array());
    worker.respond(2, bytes);
    await expect(scanning).rejects.toMatchObject({ code: "PROTOCOL" });
    await expect(reading).rejects.toMatchObject({ code: "PROTOCOL" });
    expect(worker.terminate).toHaveBeenCalledOnce();
  });

  it.each([
    { options: { limit: 0 }, entries: [[[1], []]] },
    {
      options: { reverse: true },
      entries: [
        [[1], []],
        [[2], []],
      ],
    },
    { options: { prefix: new Uint8Array([2]) }, entries: [[[1], []]] },
    { options: { start: { key: new Uint8Array([1]), inclusive: false } }, entries: [[[1], []]] },
    { options: { end: { key: new Uint8Array([1]), inclusive: false } }, entries: [[[1], []]] },
  ])("rejects results outside the requested range", async ({ options, entries }) => {
    const { db, worker } = await connected();
    const scanning = db.scan(options);
    worker.respond(2, packet(entries as Array<[number[], number[]]>));
    await expect(scanning).rejects.toMatchObject({ code: "PROTOCOL" });
  });
});

describe("browser DB subscriptions", () => {
  const bytes = (...values: number[]) => new Uint8Array(values);

  it("notifies only on successful mutation ACK, before promise settlement, with copied keys", async () => {
    const { db, worker } = await connected();
    const order: string[] = [];
    const key = bytes(1);
    db.subscribe(key, () => order.push("key"));
    db.subscribeRange({ prefix: bytes(1) }, () => order.push("range"));
    key[0] = 2;
    const changed = bytes(1);
    const put = db.put(changed, bytes(3)).then(() => order.push("resolved"));
    changed[0] = 2;
    expect(order).toEqual([]);
    worker.respond(2);
    expect(order).toEqual(["key", "range"]);
    await put;
    expect(order).toEqual(["key", "range", "resolved"]);
    const get = db.get(bytes(1));
    worker.respond(3);
    await get;
    expect(order).toHaveLength(3);
    const deletion = db.delete(bytes(1));
    worker.respond(4);
    await deletion;
    expect(order.slice(3)).toEqual(["key", "range"]);
    const close = db.close();
    worker.respond(5);
    await close;
  });

  it("reports listener failures after commit, invokes remaining listeners and keeps DB usable", async () => {
    const { db, worker } = await connected();
    const failure = new Error("listener failed");
    db.subscribe(bytes(1), () => {
      throw failure;
    });
    const remaining = vi.fn();
    db.subscribeRange({}, remaining);
    const put = db.put(bytes(1), bytes());
    worker.respond(2);
    await expect(put).rejects.toBeInstanceOf(DBNotificationError);
    await expect(put).rejects.toMatchObject({
      code: "SUBSCRIBER_FAILED",
      committed: true,
      errors: [failure],
    });
    expect(remaining).toHaveBeenCalledOnce();
    expect(worker.terminate).not.toHaveBeenCalled();
    const get = db.get(bytes(1));
    worker.respond(3, bytes(4));
    await expect(get).resolves.toEqual(bytes(4));
    const close = db.close();
    worker.respond(4);
    await close;
  });

  it("does not notify on rejected writes, timeout or malformed success", async () => {
    vi.useFakeTimers();
    const { db, worker } = await connected(10);
    const listener = vi.fn();
    db.subscribeRange({}, listener);
    const failed = db.put(bytes(1), bytes());
    worker.error(2);
    await expect(failed).rejects.toMatchObject({ code: "STORAGE" });
    const timed = db.delete(bytes(1));
    const checked = expect(timed).rejects.toMatchObject({ code: "TIMEOUT" });
    await vi.advanceTimersByTimeAsync(10);
    await checked;
    worker.respond(3);
    expect(listener).not.toHaveBeenCalled();
    const second = await connected();
    second.db.subscribeRange({}, listener);
    const malformed = second.db.put(bytes(1), bytes());
    second.worker.respond(2, bytes(1));
    await expect(malformed).rejects.toMatchObject({ code: "PROTOCOL" });
    expect(listener).not.toHaveBeenCalled();
  });

  it("clears watchers synchronously on close even when mutation ACK is already queued", async () => {
    const { db, worker } = await connected();
    const listener = vi.fn();
    const off = db.subscribeRange({}, listener);
    const put = db.put(bytes(1), bytes());
    const close = db.close();
    expect(() => db.subscribe(bytes(1), listener)).toThrow(DBError);
    expect(() => db.subscribeRange({}, listener)).toThrow(DBError);
    worker.respond(2);
    await put;
    worker.respond(3);
    await close;
    off();
    off();
    expect(listener).not.toHaveBeenCalled();
  });

  it("never dispatches a duplicate ACK twice", async () => {
    const { db, worker } = await connected();
    const listener = vi.fn();
    db.subscribe(bytes(1), listener);
    const put = db.put(bytes(1), bytes());
    worker.respond(2);
    await put;
    worker.respond(2);
    expect(listener).toHaveBeenCalledOnce();
    expect(worker.terminate).toHaveBeenCalledOnce();
    await expect(db.get(bytes(1))).rejects.toMatchObject({ code: "CLOSED" });
  });

  it("treats identical puts and deletion of missing keys as confirmed invalidations", async () => {
    const { db, worker } = await connected();
    const listener = vi.fn();
    const off = db.subscribe(bytes(1), listener);
    for (let id = 2; id <= 4; id++) {
      const request = id === 4 ? db.delete(bytes(1)) : db.put(bytes(1), bytes(2));
      worker.respond(id);
      await request;
    }
    expect(listener).toHaveBeenCalledTimes(3);
    off();
    off();
    const put = db.put(bytes(1), bytes(2));
    worker.respond(5);
    await put;
    expect(listener).toHaveBeenCalledTimes(3);
    const close = db.close();
    worker.respond(6);
    await close;
  });

  it("copies ranges, validates registrations locally and snapshots listeners during dispatch", async () => {
    const { db, worker } = await connected();
    const prefix = bytes(1);
    const later = vi.fn();
    let registered = false;
    db.subscribeRange({ prefix }, () => {
      if (!registered) {
        db.subscribe(bytes(1), later);
        registered = true;
      }
    });
    prefix[0] = 9;
    expect(() => db.subscribeRange({ limit: 1 } as DBRange, vi.fn())).toThrow(DBError);
    expect(() => db.subscribe(bytes(1), null as never)).toThrow(DBError);
    expect(worker.requests).toHaveLength(1);
    const put = db.put(bytes(1), bytes());
    worker.respond(2);
    await put;
    expect(later).not.toHaveBeenCalled();
    const deleted = db.delete(bytes(1));
    worker.respond(3);
    await deleted;
    expect(later).toHaveBeenCalledOnce();
    const close = db.close();
    worker.respond(4);
    await close;
  });
});

describe("subscription callback lifecycle", () => {
  it("keeps request metadata private from the posted message", async () => {
    const { db, worker } = await connected();
    const listener = vi.fn();
    db.subscribe(new Uint8Array([1]), listener);
    const put = db.put(new Uint8Array([1]), new Uint8Array());
    const posted = worker.requests[1];
    if (posted?.op !== "put") throw new Error("Expected put");
    posted.key[0] = 8;
    worker.respond(2);
    await put;
    expect(listener).toHaveBeenCalledOnce();
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it("rejects asynchronous subscribers after commit without unhandled rejections", async () => {
    const { db, worker } = await connected();
    db.subscribeRange({}, async () => {
      throw new Error("async callback failure");
    });
    const other = vi.fn();
    db.subscribeRange({}, other);
    const put = db.put(new Uint8Array([1]), new Uint8Array());
    worker.respond(2);
    await expect(put).rejects.toMatchObject({ code: "SUBSCRIBER_FAILED", committed: true });
    await Promise.resolve();
    expect(other).toHaveBeenCalledOnce();
    expect(worker.terminate).not.toHaveBeenCalled();
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it("supports reentrant requests and suppresses later listeners when callback closes", async () => {
    const { db, worker } = await connected();
    let reading: Promise<Uint8Array | undefined> | undefined;
    let closing: Promise<void> | undefined;
    db.subscribeRange({}, () => {
      reading = db.get(new Uint8Array([1]));
      closing = db.close();
    });
    const skipped = vi.fn();
    db.subscribeRange({}, skipped);
    const put = db.put(new Uint8Array([1]), new Uint8Array());
    worker.respond(2);
    await put;
    expect(worker.requests.slice(2).map((request) => request.op)).toEqual(["get", "close"]);
    worker.respond(3);
    await reading;
    worker.respond(4);
    await closing;
    expect(skipped).not.toHaveBeenCalled();
  });

  it("does not notify on empty scans or reads and remains silent after worker errors", async () => {
    const { db, worker } = await connected();
    const listener = vi.fn();
    db.subscribeRange({}, listener);
    const scan = db.scan();
    worker.respond(2, new Uint8Array(4));
    await scan;
    const get = db.get(new Uint8Array());
    worker.respond(3);
    await get;
    worker.onerror?.({ preventDefault: vi.fn() } as unknown as ErrorEvent);
    expect(listener).not.toHaveBeenCalled();
    expect(() => db.subscribeRange({}, listener)).toThrow(DBError);
  });
});

class TestAbortSignal implements DBAbortSignal {
  aborted = false;
  readonly listeners = new Set<() => void>();
  readonly addEventListener = vi.fn((_type: "abort", listener: () => void) => {
    this.listeners.add(listener);
  });
  readonly removeEventListener = vi.fn((_type: "abort", listener: () => void) => {
    this.listeners.delete(listener);
  });
  abort() {
    this.aborted = true;
    for (const listener of [...this.listeners]) listener();
  }
}

describe("browser DB cancellation", () => {
  it("validates before handling a pre-aborted open and creates no worker", async () => {
    const signal = new TestAbortSignal();
    signal.abort();
    await expect(openDB("../bad", { signal })).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    await expect(openDB("test", { timeoutMs: 0, signal })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    await expect(openDB("test", { signal })).rejects.toMatchObject({ code: "ABORTED" });
    expect(FakeWorker.instances).toHaveLength(0);
    expect(signal.addEventListener).not.toHaveBeenCalled();
  });

  it("does not require a global AbortSignal and accepts native controllers", async () => {
    const controller = new AbortController();
    vi.stubGlobal("AbortSignal", undefined);
    controller.abort();
    await expect(openDB("test", { signal: controller.signal })).rejects.toMatchObject({
      code: "ABORTED",
    });
    expect(FakeWorker.instances).toHaveLength(0);
  });

  it.each([
    null,
    {},
    { aborted: false },
    { aborted: 1, addEventListener() {}, removeEventListener() {} },
  ])("rejects invalid structural signal %j before creating a worker", async (signal) => {
    await expect(openDB("test", { signal: signal as DBAbortSignal })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(FakeWorker.instances).toHaveLength(0);
  });

  it("rejects pre-aborted valid operations locally while preserving unrelated work", async () => {
    const { db, worker } = await connected();
    const signal = new TestAbortSignal();
    signal.abort();
    const other = db.get(new Uint8Array([1]));
    await expect(db.get(new Uint8Array(), { signal })).rejects.toMatchObject({ code: "ABORTED" });
    await expect(db.put(new Uint8Array(), new Uint8Array(), { signal })).rejects.toMatchObject({
      code: "ABORTED",
    });
    await expect(db.delete(new Uint8Array(), { signal })).rejects.toMatchObject({
      code: "ABORTED",
    });
    await expect(db.scan({}, { signal })).rejects.toMatchObject({ code: "ABORTED" });
    await expect(db.scan({ limit: -1 }, { signal })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    await expect(
      db.put(new Uint8Array(), null as unknown as Uint8Array, { signal }),
    ).rejects.toMatchObject({ code: "INVALID_ARGUMENT" });
    expect(worker.requests).toHaveLength(2);
    expect(worker.terminate).not.toHaveBeenCalled();
    worker.respond(2);
    await other;
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it("aborts a dispatched opening request and detaches its signal", async () => {
    vi.useFakeTimers();
    const signal = new TestAbortSignal();
    const opening = openDB("test", { signal });
    const worker = latest();
    signal.abort();
    await expect(opening).rejects.toMatchObject({ code: "ABORTED" });
    expect(worker.terminate).toHaveBeenCalledOnce();
    expect(signal.listeners.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    worker.respond(1);
  });

  it("only binds the open signal until its ACK", async () => {
    const signal = new TestAbortSignal();
    const opening = openDB("test", { signal });
    const worker = latest();
    worker.respond(1);
    signal.abort();
    const db = await opening;
    expect(signal.listeners.size).toBe(0);
    expect(worker.terminate).not.toHaveBeenCalled();
    const reading = db.get(new Uint8Array());
    worker.respond(2);
    await reading;
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it("terminates all in-flight work, clears listeners/timers and suppresses late ACK notifications", async () => {
    vi.useFakeTimers();
    const { db, worker } = await connected();
    const signal = new TestAbortSignal();
    const secondSignal = new TestAbortSignal();
    const callback = vi.fn();
    db.subscribeRange({}, callback);
    const mutation = db.put(new Uint8Array([1]), new Uint8Array([2]), { signal });
    const reading = db.get(new Uint8Array([2]), { signal: secondSignal });
    const scanning = db.scan();
    const checking = Promise.all(
      [mutation, reading, scanning].map((request) =>
        expect(request).rejects.toMatchObject({ code: "ABORTED" }),
      ),
    );
    signal.abort();
    await checking;
    expect(worker.terminate).toHaveBeenCalledOnce();
    expect(signal.listeners.size).toBe(0);
    expect(secondSignal.listeners.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    worker.respond(2);
    worker.respond(3);
    worker.respond(4);
    expect(callback).not.toHaveBeenCalled();
    await expect(db.get(new Uint8Array())).rejects.toMatchObject({ code: "CLOSED" });
    await db.close();
  });

  it.each(["event", "flag"] as const)(
    "handles a registration %s race without dispatch or closing the connection",
    async (race) => {
      vi.useFakeTimers();
      const { db, worker } = await connected();
      const signal = new TestAbortSignal();
      signal.addEventListener.mockImplementation((_type, listener) => {
        signal.listeners.add(listener);
        signal.aborted = true;
        if (race === "event") listener();
      });
      await expect(db.get(new Uint8Array(), { signal })).rejects.toMatchObject({ code: "ABORTED" });
      expect(worker.requests).toHaveLength(1);
      expect(worker.terminate).not.toHaveBeenCalled();
      expect(signal.listeners.size).toBe(0);
      expect(vi.getTimerCount()).toBe(0);
      const reading = db.get(new Uint8Array());
      worker.respond(worker.requests.at(-1)?.id as number);
      await reading;
      const close = db.close();
      worker.respond(worker.requests.at(-1)?.id as number);
      await close;
    },
  );

  it("settles and detaches before subscription callbacks can abort the same signal", async () => {
    const { db, worker } = await connected();
    const signal = new TestAbortSignal();
    db.subscribeRange({}, () => signal.abort());
    const mutation = db.put(new Uint8Array(), new Uint8Array(), { signal });
    const other = db.get(new Uint8Array());
    worker.respond(2);
    await mutation;
    expect(signal.listeners.size).toBe(0);
    expect(worker.terminate).not.toHaveBeenCalled();
    worker.respond(3);
    await other;
    const close = db.close();
    worker.respond(4);
    await close;
  });

  it.each(["success", "storage", "timeout", "post", "protocol", "worker", "close"] as const)(
    "detaches listeners on %s",
    async (outcome) => {
      vi.useFakeTimers();
      const { db, worker } = await connected(10);
      const signal = new TestAbortSignal();
      if (outcome === "post") worker.throwOnPost = true;
      const reading = db.get(new Uint8Array(), { signal });
      const expected =
        outcome === "success" || outcome === "close"
          ? expect(reading).resolves.toBeUndefined()
          : expect(reading).rejects.toBeInstanceOf(DBError);
      if (outcome === "success") worker.respond(2);
      if (outcome === "storage") worker.error(2);
      if (outcome === "timeout") await vi.advanceTimersByTimeAsync(10);
      if (outcome === "protocol") worker.message(null);
      if (outcome === "worker") worker.onmessageerror?.();
      if (outcome === "close") {
        const closing = db.close();
        worker.respond(2);
        worker.respond(3);
        await closing;
      }
      await expected;
      expect(signal.listeners.size).toBe(0);
      expect(vi.getTimerCount()).toBe(0);
      const calls = worker.terminate.mock.calls.length;
      signal.abort();
      expect(worker.terminate).toHaveBeenCalledTimes(calls);
      if (outcome === "success" || outcome === "storage") {
        const close = db.close();
        worker.respond(3);
        await close;
      }
    },
  );

  it("recovers cleanly from a signal that registers then throws", async () => {
    vi.useFakeTimers();
    const { db, worker } = await connected();
    const signal = new TestAbortSignal();
    signal.addEventListener.mockImplementation((_type, listener) => {
      signal.listeners.add(listener);
      throw new Error("bad add");
    });
    await expect(db.get(new Uint8Array(), { signal })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(signal.listeners.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    expect(worker.requests).toHaveLength(1);
    expect(worker.terminate).not.toHaveBeenCalled();
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it.each([true, false])(
    "ignores throwing removal after ACK=%s and leaves retained callbacks inert",
    async (success) => {
      vi.useFakeTimers();
      const { db, worker } = await connected();
      const signal = new TestAbortSignal();
      signal.removeEventListener.mockImplementation(() => {
        throw new Error("bad remove");
      });
      const reading = db.get(new Uint8Array(), { signal });
      if (success) {
        worker.respond(2);
        await reading;
      } else {
        const checking = expect(reading).rejects.toMatchObject({ code: "STORAGE" });
        worker.error(2);
        await checking;
      }
      expect(vi.getTimerCount()).toBe(0);
      signal.abort();
      expect(worker.terminate).not.toHaveBeenCalled();
      const close = db.close();
      worker.respond(3);
      await close;
    },
  );

  it("treats signal getter failures as local invalid arguments", async () => {
    const { db, worker } = await connected();
    const signal = {
      get aborted(): boolean {
        throw new Error("getter");
      },
      addEventListener() {},
      removeEventListener() {},
    };
    await expect(db.get(new Uint8Array(), { signal })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(worker.requests).toHaveLength(1);
    expect(worker.terminate).not.toHaveBeenCalled();
    const close = db.close();
    worker.respond(2);
    await close;
  });
});

describe("adversarial cancellation cleanup", () => {
  it("detaches after the aborted getter starts throwing during registration", async () => {
    vi.useFakeTimers();
    const { db, worker } = await connected();
    const signal = new TestAbortSignal();
    signal.addEventListener.mockImplementation((_type, listener) => {
      signal.listeners.add(listener);
      Object.defineProperty(signal, "aborted", {
        get() {
          throw new Error("late getter");
        },
      });
    });
    await expect(db.get(new Uint8Array(), { signal })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(signal.listeners.size).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    expect(worker.requests).toHaveLength(1);
    expect(worker.terminate).not.toHaveBeenCalled();
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it("rejects every pending promise even if signal removal throws during cancellation", async () => {
    vi.useFakeTimers();
    const { db, worker } = await connected();
    const first = new TestAbortSignal();
    const second = new TestAbortSignal();
    first.removeEventListener.mockImplementation(() => {
      throw new Error("remove first");
    });
    second.removeEventListener.mockImplementation(() => {
      throw new Error("remove second");
    });
    const reading = db.get(new Uint8Array(), { signal: first });
    const mutation = db.delete(new Uint8Array(), { signal: second });
    const checking = Promise.all(
      [reading, mutation].map((request) =>
        expect(request).rejects.toMatchObject({ code: "ABORTED" }),
      ),
    );
    first.abort();
    await checking;
    second.abort();
    expect(worker.terminate).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not replace committed notification failures when a callback aborts its own signal", async () => {
    const { db, worker } = await connected();
    const signal = new TestAbortSignal();
    db.subscribeRange({}, () => {
      signal.abort();
      throw new Error("notification");
    });
    const mutation = db.delete(new Uint8Array(), { signal });
    const checking = expect(mutation).rejects.toMatchObject({
      code: "SUBSCRIBER_FAILED",
      committed: true,
    });
    worker.respond(2);
    await checking;
    expect(worker.terminate).not.toHaveBeenCalled();
    expect(signal.listeners.size).toBe(0);
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it("does not let a reentrant removal invoke its settled abort callback", async () => {
    const { db, worker } = await connected();
    const signal = new TestAbortSignal();
    signal.removeEventListener.mockImplementation((_type, listener) => {
      listener();
      signal.listeners.delete(listener);
    });
    const reading = db.get(new Uint8Array(), { signal });
    worker.respond(2);
    await reading;
    expect(worker.terminate).not.toHaveBeenCalled();
    const close = db.close();
    worker.respond(3);
    await close;
  });

  it("cleans up failed open registration and open error acknowledgements", async () => {
    vi.useFakeTimers();
    const broken = new TestAbortSignal();
    broken.addEventListener.mockImplementation((_type, listener) => {
      broken.listeners.add(listener);
      throw new Error("registration");
    });
    await expect(openDB("test", { signal: broken })).rejects.toMatchObject({
      code: "INVALID_ARGUMENT",
    });
    expect(latest().requests).toHaveLength(0);
    expect(latest().terminate).toHaveBeenCalledOnce();
    expect(broken.listeners.size).toBe(0);
    const signal = new TestAbortSignal();
    const opening = openDB("test", { signal });
    const checking = expect(opening).rejects.toMatchObject({ code: "LOCKED" });
    latest().error(1, "LOCKED");
    await checking;
    expect(signal.listeners.size).toBe(0);
    expect(latest().terminate).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  });
});
