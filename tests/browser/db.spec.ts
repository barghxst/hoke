import { test as base, expect, type Page } from "@playwright/test";
import type { BrowserDB, openDB } from "../../packages/hoke/src/db/index.js";
import { PROTOCOL_VERSION, type Response } from "../../packages/hoke/src/db/protocol.js";

type Harness = {
  db: BrowserDB;
  openDB: typeof openDB;
  raw: Worker;
  request: (command: Record<string, unknown>) => Promise<Response>;
};

const test = base.extend<{ pageErrors: Error[] }>({
  pageErrors: [
    async ({ page }, use) => {
      const errors: Error[] = [];
      page.on("pageerror", (error) => errors.push(error));
      await use(errors);
      expect(errors).toEqual([]);
    },
    { auto: true },
  ],
});

async function prepare(page: Page) {
  await page.goto("/examples/web/");
  await page.evaluate(async () => {
    const modulePath = "/packages/hoke/dist/db/index.js";
    (window as unknown as Harness).openDB = (await import(modulePath)).openDB;
  });
}

async function readWal(page: Page, name: string): Promise<number[]> {
  return page.evaluate(async (name) => {
    const root = await navigator.storage.getDirectory();
    const directory = await root.getDirectoryHandle("hoke-wal-v1");
    const file = await directory.getFileHandle(`${name}.wal`);
    return Array.from(new Uint8Array(await (await file.getFile()).arrayBuffer()));
  }, name);
}

async function writeWal(page: Page, name: string, bytes: number[]) {
  await page.evaluate(
    async ({ name, bytes }) => {
      const root = await navigator.storage.getDirectory();
      const directory = await root.getDirectoryHandle("hoke-wal-v1");
      const file = await directory.getFileHandle(`${name}.wal`);
      const writable = await file.createWritable();
      await writable.write(new Uint8Array(bytes));
      await writable.close();
    },
    { name, bytes },
  );
}

async function reopen(page: Page, name: string) {
  await expect
    .poll(async () =>
      page.evaluate(async (name) => {
        const h = window as unknown as Harness;
        try {
          h.db = await h.openDB(name);
          return "open";
        } catch (error) {
          return (error as { code: string }).code;
        }
      }, name),
    )
    .toBe("open");
}

async function rawWorker(page: Page) {
  await page.evaluate((protocolVersion) => {
    const h = window as unknown as Harness;
    h.raw = new Worker("/packages/hoke/dist/db/worker.js", { type: "module" });
    let id = 0;
    h.request = (command) =>
      new Promise((resolve, reject) => {
        const next = ++id;
        const timer = setTimeout(() => reject(new Error("Raw worker response timed out")), 10_000);
        const receive = (event: MessageEvent<Response>) => {
          if (event.data.id !== next) return;
          clearTimeout(timer);
          h.raw.removeEventListener("message", receive);
          resolve(event.data);
        };
        h.raw.addEventListener("message", receive);
        h.raw.postMessage({ version: protocolVersion, id: next, ...command });
      });
  }, PROTOCOL_VERSION);
}

async function fault(page: Page, script: string) {
  await page.route("**/packages/hoke/dist/db/worker.js", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: `${script}\n${await response.text()}` });
  });
}

test("real WASM OPFS supports binary and empty bytes, copies inputs and outputs, and persists deletes", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("bytes");
      const key = new Uint8Array([0, 255, 128]);
      const value = new Uint8Array([255, 0, 1]);
      const pending = db.put(key, value);
      key.fill(7);
      value.fill(8);
      await pending;
      const first = await db.get(new Uint8Array([0, 255, 128]));
      const saved = Array.from(first ?? []);
      first?.fill(9);
      const second = Array.from((await db.get(new Uint8Array([0, 255, 128]))) ?? []);
      await db.put(new Uint8Array(), new Uint8Array());
      const empty = Array.from((await db.get(new Uint8Array())) ?? [99]);
      await db.delete(new Uint8Array([0, 255, 128]));
      await db.delete(new Uint8Array([88]));
      await db.close();
      const reopened = await h.openDB("bytes");
      const deleted = (await reopened.get(new Uint8Array([0, 255, 128]))) === undefined;
      const retainedEmpty = Array.from((await reopened.get(new Uint8Array())) ?? [99]);
      await reopened.close();
      return { mode: db.mode, saved, second, empty, deleted, retainedEmpty };
    }),
  ).toEqual({
    mode: "wal-v1",
    saved: [255, 0, 1],
    second: [255, 0, 1],
    empty: [],
    deleted: true,
    retainedEmpty: [],
  });
});

test("ACK survives page reload and releases the previous worker owner", async ({ page }) => {
  await prepare(page);
  await page.evaluate(async () => {
    const h = window as unknown as Harness;
    h.db = await h.openDB("reload");
    await h.db.put(new Uint8Array([1]), new Uint8Array([42]));
  });
  await prepare(page);
  await reopen(page, "reload");
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const value = Array.from((await h.db.get(new Uint8Array([1]))) ?? []);
      await h.db.close();
      return value;
    }),
  ).toEqual([42]);
});

test("exclusive OPFS ownership rejects a second owner while distinct names remain isolated", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const first = await h.openDB("owner");
      await first.put(new Uint8Array([1]), new Uint8Array([2]));
      let duplicate = "unexpected success";
      try {
        await h.openDB("owner");
      } catch (error) {
        duplicate = (error as { code: string }).code;
      }
      const other = await h.openDB("other");
      const absent = (await other.get(new Uint8Array([1]))) === undefined;
      await other.put(new Uint8Array([1]), new Uint8Array([9]));
      const original = Array.from((await first.get(new Uint8Array([1]))) ?? []);
      await other.close();
      await first.close();
      const next = await h.openDB("owner");
      await next.close();
      return { duplicate, absent, original };
    }),
  ).toEqual({ duplicate: "LOCKED", absent: true, original: [2] });
});

for (const kind of ["checksum", "future-header"] as const) {
  test(`${kind} is rejected without byte changes and a failed open releases its lock`, async ({
    page,
  }) => {
    await prepare(page);
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("invalid");
      await db.put(new Uint8Array([1]), new Uint8Array([2]));
      await db.close();
    });
    const bytes = await readWal(page, "invalid");
    if (kind === "checksum") bytes[bytes.length - 1] = (bytes.at(-1) ?? 0) ^ 1;
    else {
      bytes[8] = 2;

      let crc = 0xffff_ffff;
      for (const byte of bytes.slice(0, 12)) {
        crc ^= byte;
        for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb8_8320 : 0);
      }
      crc ^= 0xffff_ffff;
      for (let index = 0; index < 4; index++) bytes[12 + index] = (crc >>> (8 * index)) & 0xff;
    }
    await writeWal(page, "invalid", bytes);
    const codes = await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const codes: string[] = [];
      for (let i = 0; i < 2; i++) {
        try {
          await h.openDB("invalid");
          codes.push("unexpected success");
        } catch (error) {
          codes.push((error as { code: string }).code);
        }
      }
      return codes;
    });
    expect(codes).toEqual(["ENGINE", "ENGINE"]);
    expect(await readWal(page, "invalid")).toEqual(bytes);
  });
}

test("incomplete final WAL frame is repaired while the earlier ACK survives", async ({ page }) => {
  await prepare(page);
  await page.evaluate(async () => {
    const h = window as unknown as Harness;
    const db = await h.openDB("tail");
    await db.put(new Uint8Array([1]), new Uint8Array([11]));
    await db.close();
  });
  const first = await readWal(page, "tail");
  await page.evaluate(async () => {
    const h = window as unknown as Harness;
    const db = await h.openDB("tail");
    await db.put(new Uint8Array([2]), new Uint8Array([22]));
    await db.close();
  });
  const both = await readWal(page, "tail");
  await writeWal(page, "tail", both.slice(0, -2));
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("tail");
      const old = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      const absent = (await db.get(new Uint8Array([2]))) === undefined;
      await db.close();
      return { old, absent };
    }),
  ).toEqual({ old: [11], absent: true });
  expect(await readWal(page, "tail")).toEqual(first);
});

test("a failed flush sends no ACK, poisons mutations, and recovery resolves the unknown outcome", async ({
  page,
}) => {
  await fault(
    page,
    `{
    const original = FileSystemSyncAccessHandle.prototype.flush;
    let calls = 0;
    FileSystemSyncAccessHandle.prototype.flush = function () {
      if (++calls === 2) throw new DOMException("injected flush failure", "QuotaExceededError");
      return original.call(this);
    };
  }`,
  );
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("flush-fail");
      const codes: string[] = [];
      for (const n of [1, 2]) {
        try {
          await db.put(new Uint8Array([n]), new Uint8Array([n + 10]));
          codes.push("ACK");
        } catch (error) {
          codes.push((error as { code: string }).code);
        }
      }
      const uncommitted = (await db.get(new Uint8Array([1]))) === undefined;
      let close = "success";
      try {
        await db.close();
      } catch (error) {
        close = (error as { code: string }).code;
      }
      return { codes, uncommitted, close };
    }),
  ).toEqual({ codes: ["ENGINE", "ENGINE"], uncommitted: true, close: "ENGINE" });
  await page.unroute("**/packages/hoke/dist/db/worker.js");
  await reopen(page, "flush-fail");
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const recovered = Array.from((await h.db.get(new Uint8Array([1]))) ?? []);
      const blocked = (await h.db.get(new Uint8Array([2]))) === undefined;
      await h.db.put(new Uint8Array([3]), new Uint8Array([33]));
      await h.db.close();
      return { recovered, blocked };
    }),
  ).toEqual({ recovered: [11], blocked: true });
});

test("a write failure preserves old ACK bytes and prevents later mutation ACKs", async ({
  page,
}) => {
  await prepare(page);
  await page.evaluate(async () => {
    const h = window as unknown as Harness;
    const db = await h.openDB("write-fail");
    await db.put(new Uint8Array([1]), new Uint8Array([7]));
    await db.close();
  });
  const original = await readWal(page, "write-fail");
  await fault(
    page,
    `{
    FileSystemSyncAccessHandle.prototype.write = function () {
      throw new DOMException("injected write failure", "QuotaExceededError");
    };
  }`,
  );
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("write-fail");
      const codes: string[] = [];
      for (const n of [2, 3]) {
        try {
          await db.put(new Uint8Array([n]), new Uint8Array([8]));
          codes.push("ACK");
        } catch (error) {
          codes.push((error as { code: string }).code);
        }
      }
      const value = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      try {
        await db.close();
      } catch {}
      return { codes, value };
    }),
  ).toEqual({ codes: ["ENGINE", "ENGINE"], value: [7] });
  expect(await readWal(page, "write-fail")).toEqual(original);
});

test("a replay read failure leaves WAL intact and releases its acquired handle", async ({
  page,
}) => {
  await prepare(page);
  await page.evaluate(async () => {
    const h = window as unknown as Harness;
    const db = await h.openDB("read-fail");
    await db.put(new Uint8Array([1]), new Uint8Array([7]));
    await db.close();
  });
  const original = await readWal(page, "read-fail");
  await fault(
    page,
    `{
    FileSystemSyncAccessHandle.prototype.read = function () {
      throw new DOMException("injected read failure", "NotReadableError");
    };
  }`,
  );
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      try {
        await h.openDB("read-fail");
        return "ACK";
      } catch (error) {
        return (error as { code: string }).code;
      }
    }),
  ).toBe("ENGINE");
  expect(await readWal(page, "read-fail")).toEqual(original);
  await page.unroute("**/packages/hoke/dist/db/worker.js");
  await reopen(page, "read-fail");
  await page.evaluate(() => (window as unknown as Harness).db.close());
});

test("worker termination after confirmed ACK retains data and eventually releases ownership", async ({
  page,
}) => {
  await prepare(page);
  await rawWorker(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const opened = await h.request({ op: "open", name: "terminated" });
      const written = await h.request({
        op: "put",
        key: new Uint8Array([1]),
        value: new Uint8Array([91]),
      });
      h.raw.terminate();
      return [opened.ok, written.ok];
    }),
  ).toEqual([true, true]);
  await reopen(page, "terminated");
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const value = Array.from((await h.db.get(new Uint8Array([1]))) ?? []);
      await h.db.close();
      return value;
    }),
  ).toEqual([91]);
});

test("raw protocol validates malformed requests before storage and serializes open with put", async ({
  page,
}) => {
  await prepare(page);
  await rawWorker(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const errors: string[] = [];
      for (const request of [
        { op: "open", name: "../escape" },
        { op: "put", key: new Uint8Array(65_537), value: new Uint8Array() },
        { op: "open", name: "safe", version: 9 },
      ]) {
        const response = await h.request(request);
        errors.push(response.ok ? "ACK" : response.error.code);
      }
      let absent = false;
      const root = await navigator.storage.getDirectory();
      try {
        await root.getDirectoryHandle("hoke-wal-v1");
      } catch (error) {
        absent = (error as Error).name === "NotFoundError";
      }
      const [opened, written] = await Promise.all([
        h.request({ op: "open", name: "safe" }),
        h.request({ op: "put", key: new Uint8Array([1]), value: new Uint8Array([8]) }),
      ]);
      const result = await h.request({ op: "get", key: new Uint8Array([1]) });
      await h.request({ op: "close" });
      h.raw.terminate();
      return {
        errors,
        absent,
        opened: opened.ok,
        written: written.ok,
        result: result.ok ? Array.from(result.result ?? []) : [],
      };
    }),
  ).toEqual({
    errors: ["INVALID_ARGUMENT", "INVALID_ARGUMENT", "PROTOCOL"],
    absent: true,
    opened: true,
    written: true,
    result: [8],
  });
});

test("response transport failure releases the OPFS owner and preserves an unknown mutation outcome", async ({
  page,
}) => {
  await fault(
    page,
    `{
    const originalPostMessage = globalThis.postMessage.bind(globalThis);
    globalThis.postMessage = function (message, ...options) {
      if (message.id === 2) throw new Error("injected response transport failure");
      return originalPostMessage(message, ...options);
    };
  }`,
  );
  await prepare(page);
  await rawWorker(page);
  expect(
    await page.evaluate(async (protocolVersion) => {
      const h = window as unknown as Harness;
      const opened = await h.request({ op: "open", name: "transport-fail" });
      const failure = await new Promise<string>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("Worker error timed out")), 10_000);
        h.raw.addEventListener(
          "error",
          (event) => {
            event.preventDefault();
            clearTimeout(timer);
            resolve(event.message);
          },
          { once: true },
        );

        h.raw.postMessage({
          version: protocolVersion,
          id: 2,
          op: "put",
          key: new Uint8Array([1]),
          value: new Uint8Array([77]),
        });
      });
      return { opened: opened.ok, failure };
    }, PROTOCOL_VERSION),
  ).toEqual({
    opened: true,
    failure: expect.stringContaining("injected response transport failure"),
  });
  await page.unroute("**/packages/hoke/dist/db/worker.js");

  await reopen(page, "transport-fail");
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const value = Array.from((await h.db.get(new Uint8Array([1]))) ?? []);
      await h.db.close();
      h.raw.terminate();
      return value;
    }),
  ).toEqual([77]);
});

test("missing synchronous OPFS support rejects without creating storage", async ({ page }) => {
  await prepare(page);
  await fault(
    page,
    `Object.defineProperty(FileSystemFileHandle.prototype, 'createSyncAccessHandle', { value: undefined });`,
  );
  const result = await page.evaluate(async () => {
    const h = window as unknown as Harness;
    try {
      await h.openDB("unsupported");
      return "unexpected success";
    } catch (error) {
      return (error as { code: string }).code;
    }
  });
  expect(result).toBe("UNSUPPORTED");
  expect(
    await page.evaluate(async () => {
      const root = await navigator.storage.getDirectory();
      try {
        await root.getDirectoryHandle("hoke-wal-v1");
        return "created";
      } catch (error) {
        return (error as DOMException).name;
      }
    }),
  ).toBe("NotFoundError");
});

test("OPFS quota error is explicit and reopening after its removal succeeds", async ({ page }) => {
  await prepare(page);
  await fault(
    page,
    `FileSystemDirectoryHandle.prototype.getDirectoryHandle = async () => { throw new DOMException('Quota exhausted', 'QuotaExceededError'); };`,
  );
  expect(
    await page.evaluate(async () => {
      try {
        await (window as unknown as Harness).openDB("quota");
        return "unexpected success";
      } catch (error) {
        return (error as { code: string }).code;
      }
    }),
  ).toBe("STORAGE");
  await page.unroute("**/packages/hoke/dist/db/worker.js");
  await reopen(page, "quota");
  await page.evaluate(() => (window as unknown as Harness).db.close());
});

test("failed WASM loading leaves OPFS untouched and can be retried in a fresh session", async ({
  page,
}) => {
  await prepare(page);
  await page.route("**/hoke_wasm_bg.wasm", (route) => route.abort());
  expect(
    await page.evaluate(async () => {
      try {
        await (window as unknown as Harness).openDB("fetchfail");
        return "unexpected success";
      } catch (error) {
        return (error as { code: string }).code;
      }
    }),
  ).toBe("ENGINE");
  expect(
    await page.evaluate(async () => {
      const root = await navigator.storage.getDirectory();
      try {
        await root.getDirectoryHandle("hoke-wal-v1");
        return "created";
      } catch (error) {
        return (error as DOMException).name;
      }
    }),
  ).toBe("NotFoundError");
  await page.unroute("**/hoke_wasm_bg.wasm");
  await reopen(page, "fetchfail");
  await page.evaluate(() => (window as unknown as Harness).db.close());
});
