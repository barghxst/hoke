import { test as base, expect, type Page } from "@playwright/test";
import type { BrowserDB, openDB } from "../../packages/hoke/src/db/index.js";
import { PROTOCOL_VERSION, type Response } from "../../packages/hoke/src/db/protocol.js";

type Harness = {
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
async function rawWorker(page: Page) {
  await page.evaluate((protocolVersion) => {
    const h = window as unknown as Harness;
    h.raw = new Worker("/packages/hoke/dist/db/worker.js", { type: "module" });
    let id = 0;
    h.request = (command) =>
      new Promise((resolve, reject) => {
        const next = ++id;
        const timer = setTimeout(() => reject(new Error("Worker response timed out")), 10_000);
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

test("WASM scan orders arbitrary bytes, intersects bounds and prefixes, and never returns tombstones", async ({
  page,
}) => {
  await prepare(page);
  const result = await page.evaluate(async () => {
    const db = await (window as unknown as Harness).openDB("scan-binary");
    const bytes = (key: number[]) => new Uint8Array(key);
    const rows = async (options: Parameters<BrowserDB["scan"]>[0] = {}) =>
      (await db.scan(options)).map((entry) => [Array.from(entry.key), Array.from(entry.value)]);
    for (const key of [[255, 255], [1], [], [255], [0, 255], [0], [0, 0], [128], [255, 0]])
      await db.put(bytes(key), bytes(key));
    await db.put(bytes([1]), bytes([42]));
    await db.delete(bytes([128]));
    await db.delete(bytes([99]));
    const all = await rows();
    const emptyPrefix = await rows({ prefix: bytes([]) });
    const ff = await rows({ prefix: bytes([255]) });
    const allFF = await rows({ prefix: bytes([255, 255]) });
    const range = await rows({
      start: { key: bytes([0]), inclusive: false },
      end: { key: bytes([1]), inclusive: true },
    });
    const reverse = await rows({
      start: { key: bytes([0]), inclusive: true },
      end: { key: bytes([255]), inclusive: false },
      reverse: true,
      limit: 3,
    });
    const intersection = await rows({
      prefix: bytes([0]),
      start: { key: bytes([0, 0]), inclusive: false },
      end: { key: bytes([255]), inclusive: true },
    });
    const equalIncluded = await rows({
      start: { key: bytes([1]), inclusive: true },
      end: { key: bytes([1]), inclusive: true },
    });
    const equalExcluded = await rows({
      start: { key: bytes([1]), inclusive: false },
      end: { key: bytes([1]), inclusive: true },
    });
    const zero = await rows({ limit: 0 });
    const emptyEnd = await rows({ end: { key: bytes([]), inclusive: true } });
    await db.close();
    const reopened = await (window as unknown as Harness).openDB("scan-binary");
    const restored = (await reopened.scan()).map((entry) => [
      Array.from(entry.key),
      Array.from(entry.value),
    ]);
    await reopened.close();
    return {
      all,
      emptyPrefix,
      ff,
      allFF,
      range,
      reverse,
      intersection,
      equalIncluded,
      equalExcluded,
      zero,
      emptyEnd,
      restored,
    };
  });
  const all = [
    [[], []],
    [[0], [0]],
    [
      [0, 0],
      [0, 0],
    ],
    [
      [0, 255],
      [0, 255],
    ],
    [[1], [42]],
    [[255], [255]],
    [
      [255, 0],
      [255, 0],
    ],
    [
      [255, 255],
      [255, 255],
    ],
  ];
  expect(result).toEqual({
    all,
    emptyPrefix: all,
    ff: all.slice(5),
    allFF: all.slice(7),
    range: all.slice(2, 5),
    reverse: [all[4], all[3], all[2]],
    intersection: [all[3]],
    equalIncluded: [all[4]],
    equalExcluded: [],
    zero: [],
    emptyEnd: [all[0]],
    restored: all,
  });
});

test("scan defaults to 100, bounds results after deletes, and isolates inputs and outputs", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("scan-default");
      for (let n = 0; n < 105; n++) await db.put(new Uint8Array([n]), new Uint8Array([n]));
      await db.delete(new Uint8Array([0]));
      const defaults = await db.scan();
      const start = new Uint8Array([100]);
      const options = { start: { key: start, inclusive: true }, reverse: false, limit: 2 };
      const pending = db.scan(options);
      start[0] = 200;
      options.start.inclusive = false;
      options.limit = 0;
      const first = await pending;
      const saved = first.map((entry) => [entry.key[0], entry.value[0]]);
      first[0]?.key.fill(9);
      first[0]?.value.fill(9);
      const again = await db.scan({
        start: { key: new Uint8Array([100]), inclusive: true },
        limit: 2,
      });
      await db.close();
      return {
        count: defaults.length,
        first: defaults[0]?.key[0],
        last: defaults.at(-1)?.key[0],
        saved,
        again: again.map((entry) => [entry.key[0], entry.value[0]]),
      };
    }),
  ).toEqual({
    count: 100,
    first: 1,
    last: 100,
    saved: [
      [100, 100],
      [101, 101],
    ],
    again: [
      [100, 100],
      [101, 101],
    ],
  });
});

test("collection framing avoids name collisions and scan returns scoped relative keys after reopen", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("scan-collections");
      const a = db.collection(new Uint8Array([97]));
      const ab = db.collection(new Uint8Array([97, 98]));
      const empty = new Uint8Array();
      await a.put(empty, new Uint8Array([10]));
      await a.put(new Uint8Array([98]), new Uint8Array([11]));
      await a.put(new Uint8Array([98, 255]), new Uint8Array([12]));
      await a.put(new Uint8Array([99]), new Uint8Array([13]));
      await ab.put(empty, new Uint8Array([20]));
      await ab.put(new Uint8Array([98]), new Uint8Array([21]));
      const compact = (entries: Awaited<ReturnType<BrowserDB["scan"]>>) =>
        entries.map((entry) => [Array.from(entry.key), Array.from(entry.value)]);
      const original = compact(await a.scan());
      const scoped = compact(
        await a.scan({
          prefix: new Uint8Array([98]),
          start: { key: new Uint8Array([98]), inclusive: false },
          end: { key: new Uint8Array([99]), inclusive: false },
          reverse: true,
        }),
      );
      const other = compact(await ab.scan());
      await a.delete(new Uint8Array([98]));
      await db.close();
      const reopened = await (window as unknown as Harness).openDB("scan-collections");
      const restored = compact(await reopened.collection(new Uint8Array([97])).scan());
      const otherGet = Array.from(
        (await reopened.collection(new Uint8Array([97, 98])).get(new Uint8Array([98]))) ?? [],
      );
      await reopened.close();
      return { original, scoped, other, restored, otherGet };
    }),
  ).toEqual({
    original: [
      [[], [10]],
      [[98], [11]],
      [[98, 255], [12]],
      [[99], [13]],
    ],
    scoped: [[[98, 255], [12]]],
    other: [
      [[], [20]],
      [[98], [21]],
    ],
    restored: [
      [[], [10]],
      [[98, 255], [12]],
      [[99], [13]],
    ],
    otherGet: [21],
  });
});

test("worker rejects stale protocol and malformed scan before OPFS creation, then remains usable", async ({
  page,
}) => {
  await prepare(page);
  await rawWorker(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const errors: string[] = [];
      for (const command of [
        { op: "open", name: "stale", version: 1 },
        ...[
          null,
          [],
          { limit: -1 },
          { limit: 1001 },
          { reverse: "yes" },
          { prefix: [1] },
          { prefix: new Uint8Array(65_537) },
          {
            start: { key: new Uint8Array([2]), inclusive: true },
            end: { key: new Uint8Array([1]), inclusive: true },
          },
          { start: { key: new Uint8Array([1]) } },
          { prefix: new Uint8Array(), unknown: true },
        ].map((options) => ({ op: "scan", options })),
      ]) {
        const response = await h.request(command);
        errors.push(response.ok ? "ACK" : response.error.code);
      }
      const root = await navigator.storage.getDirectory();
      let absent = false;
      try {
        await root.getDirectoryHandle("hoke-wal-v1");
      } catch (error) {
        absent = (error as Error).name === "NotFoundError";
      }
      const opened = await h.request({ op: "open", name: "valid" });
      const written = await h.request({
        op: "put",
        key: new Uint8Array([1]),
        value: new Uint8Array([8]),
      });
      const invalid = await h.request({ op: "scan", options: { limit: 0.5 } });
      const scanned = await h.request({
        op: "scan",
        options: { prefix: new Uint8Array(), limit: 1, reverse: false },
      });
      await h.request({ op: "close" });
      h.raw.terminate();
      const directory = await root.getDirectoryHandle("hoke-wal-v1");
      const file = await directory.getFileHandle("valid.wal");
      const size = (await file.getFile()).size;
      return {
        errors,
        absent,
        opened: opened.ok,
        written: written.ok,
        invalid: invalid.ok ? "ACK" : invalid.error.code,
        scanned: scanned.ok && scanned.result instanceof Uint8Array,
        size,
      };
    }),
  ).toEqual({
    errors: ["PROTOCOL", ...Array(10).fill("INVALID_ARGUMENT")],
    absent: true,
    opened: true,
    written: true,
    invalid: "INVALID_ARGUMENT",
    scanned: true,
    size: 50,
  });
});

test("oversized scan rejects the whole result without poisoning reads or writes", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("scan-budget");
      const large = new Uint8Array(16 * 1024 * 1024);
      large[0] = 42;
      await db.put(new Uint8Array([1]), large);
      await db.put(new Uint8Array([2]), large);
      let code = "ACK";
      try {
        await db.scan({ limit: 2 });
      } catch (error) {
        code = (error as { code: string }).code;
      }
      const small = await db.scan({ limit: 1 });
      const existing = await db.get(new Uint8Array([2]));
      await db.put(new Uint8Array([3]), new Uint8Array([9]));
      const later = await db.scan({ start: { key: new Uint8Array([3]), inclusive: true } });
      await db.close();
      return {
        code,
        small: small.map((entry) => [Array.from(entry.key), entry.value.length, entry.value[0]]),
        existing: existing?.[0],
        later: later.map((entry) => [Array.from(entry.key), Array.from(entry.value)]),
      };
    }),
  ).toEqual({
    code: "ENGINE",
    small: [[[1], 16 * 1024 * 1024, 42]],
    existing: 42,
    later: [[[3], [9]]],
  });
});

test("client rejects stale worker responses without opening a database", async ({ page }) => {
  await page.route("**/packages/hoke/dist/db/worker.js", (route) =>
    route.fulfill({
      contentType: "text/javascript",
      body: "self.onmessage = event => self.postMessage({ version: 1, id: event.data.id, ok: true, result: null });",
    }),
  );
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      let code = "ACK";
      try {
        await (window as unknown as Harness).openDB("stale-worker");
      } catch (error) {
        code = (error as { code: string }).code;
      }
      const root = await navigator.storage.getDirectory();
      let absent = false;
      try {
        await root.getDirectoryHandle("hoke-wal-v1");
      } catch (error) {
        absent = (error as Error).name === "NotFoundError";
      }
      return { code, absent };
    }),
  ).toEqual({ code: "PROTOCOL", absent: true });
});

test("invalid client scans preserve persisted bytes and allow later valid operations", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const original = await h.openDB("scan-invalid");
      await original.put(new Uint8Array([1]), new Uint8Array([7]));
      await original.close();
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle("hoke-wal-v1");
      const file = await dir.getFileHandle("scan-invalid.wal");
      const before = Array.from(new Uint8Array(await (await file.getFile()).arrayBuffer()));
      const db = await h.openDB("scan-invalid");
      const codes: string[] = [];
      for (const options of [
        { limit: -1 },
        { limit: Number.NaN },
        { reverse: 1 },
        {
          start: { key: new Uint8Array([2]), inclusive: true },
          end: { key: new Uint8Array([1]), inclusive: true },
        },
      ]) {
        try {
          await db.scan(options as Parameters<BrowserDB["scan"]>[0]);
          codes.push("ACK");
        } catch (error) {
          codes.push((error as { code: string }).code);
        }
      }
      const read = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      await db.close();
      const after = Array.from(new Uint8Array(await (await file.getFile()).arrayBuffer()));
      return { codes, read, unchanged: JSON.stringify(before) === JSON.stringify(after) };
    }),
  ).toEqual({ codes: Array(4).fill("INVALID_ARGUMENT"), read: [7], unchanged: true });
});
