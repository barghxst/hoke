import { expect, type Page, test } from "@playwright/test";
import type { openDB } from "../../packages/hoke/src/db/index.js";

type Harness = { openDB: typeof openDB };

async function prepare(page: Page) {
  await page.goto("/examples/web/");
  await page.evaluate(async () => {
    const path = "/packages/hoke/dist/db/index.js";
    Object.assign(window, await import(path));
  });
}

test("public container mode persists batches and SST state across close and reload", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const { openDB } = window as unknown as Harness;
      const enc = (value: string) => new TextEncoder().encode(value);
      const db = await openDB("container-public", { mode: "container-v1" });
      await db.put(enc("a"), enc("old"));
      await db.writeBatch([
        { type: "put", key: enc("b"), value: enc("two") },
        { type: "delete", key: enc("a") },
        { type: "put", key: enc("a"), value: enc("new") },
      ]);
      await db.flush();
      const flushed = await db.stats();
      await db.put(enc("c"), enc("three"));
      await db.delete(enc("b"));
      await db.compact();
      const compacted = await db.stats();
      const rows = (await db.scan()).map(({ key, value }) => [
        new TextDecoder().decode(key),
        new TextDecoder().decode(value),
      ]);
      await db.close();
      return {
        mode: db.mode,
        rows,
        flushed: [flushed.tableCount, flushed.walSegments, flushed.memtableEntries],
        compacted: [compacted.tableCount, compacted.walSegments, compacted.memtableEntries],
      };
    }),
  ).toEqual({
    mode: "container-v1",
    rows: [
      ["a", "new"],
      ["c", "three"],
    ],
    flushed: [1, 0, 0],
    compacted: [1, 0, 0],
  });
  await page.reload();
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("container-public", {
        mode: "container-v1",
      });
      const rows = (await db.scan({ reverse: true })).map(({ key, value }) => [
        new TextDecoder().decode(key),
        new TextDecoder().decode(value),
      ]);
      const stats = await db.stats();
      await db.close();
      return { rows, generation: String(stats.generation), tables: stats.tableCount };
    }),
  ).toEqual({
    rows: [
      ["c", "three"],
      ["a", "new"],
    ],
    generation: "7",
    tables: 1,
  });
});

test("WAL and container with the same name remain isolated and lock independently", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const { openDB } = window as unknown as Harness;
      const key = new Uint8Array([1]);
      const wal = await openDB("same-name");
      await wal.put(key, new Uint8Array([7]));
      const db = await openDB("same-name", { mode: "container-v1" });
      const before = await db.get(key);
      await db.put(key, new Uint8Array([9]));
      let locked = "";
      try {
        await openDB("same-name", { mode: "container-v1" });
      } catch (error) {
        locked = (error as { code: string }).code;
      }
      const values = [
        Array.from((await wal.get(key)) ?? []),
        Array.from((await db.get(key)) ?? []),
      ];
      await db.close();
      await wal.close();
      const root = await navigator.storage.getDirectory();
      const raw = async (directory: string, file: string) =>
        Array.from(
          new Uint8Array(
            await (
              await (await (await root.getDirectoryHandle(directory)).getFileHandle(file)).getFile()
            ).arrayBuffer(),
          ).slice(0, 8),
        );
      return {
        before: before === undefined,
        locked,
        values,
        wal: await raw("hoke-wal-v1", "same-name.wal"),
        container: await raw("hoke-container-v1", "same-name.hkc"),
      };
    }),
  ).toEqual({
    before: true,
    locked: "LOCKED",
    values: [[7], [9]],
    wal: [83, 84, 79, 87, 87, 65, 76, 0],
    container: [72, 79, 75, 69, 67, 78, 84, 0],
  });
});

test("batch subscriptions run once per matching registration after full publication", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("batch-watch", {
        mode: "container-v1",
      });
      const a = new Uint8Array([1]),
        b = new Uint8Array([2]);
      const events: string[] = [];
      db.subscribe(a, () => {
        events.push("a");
      });
      db.subscribeRange({}, () => {
        events.push("all");
      });
      db.subscribe(b, () => {
        events.push("b");
      });
      await db.writeBatch([
        { type: "put", key: a, value: a },
        { type: "put", key: b, value: b },
        { type: "delete", key: a },
      ]);
      const first = events.slice();
      await db.writeBatch([]);
      await db.flush();
      await db.compact();
      const afterMaintenance = events.slice();
      let committed = false;
      db.subscribe(a, () => {
        throw new Error("subscriber");
      });
      try {
        await db.writeBatch([{ type: "put", key: a, value: b }]);
      } catch (error) {
        committed = (error as { committed?: boolean }).committed === true;
      }
      const value = Array.from((await db.get(a)) ?? []);
      await db.close();
      return { first, afterMaintenance, events, committed, value };
    }),
  ).toEqual({
    first: ["a", "all", "b"],
    afterMaintenance: ["a", "all", "b"],
    events: ["a", "all", "b", "a", "all"],
    committed: true,
    value: [2],
  });
});

test("collection batches use isolated physical keys and forward cancellation", async ({ page }) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("collection-batch", {
        mode: "container-v1",
      });
      const collection = db.collection(new Uint8Array([255]));
      const key = new Uint8Array([1]);
      let events = 0;
      collection.subscribeRange({}, () => {
        events++;
      });
      await collection.writeBatch([
        { type: "put", key, value: new Uint8Array([7]) },
        { type: "put", key: new Uint8Array([2]), value: new Uint8Array([8]) },
      ]);
      const controller = new AbortController();
      controller.abort();
      let aborted = "";
      try {
        await collection.writeBatch([{ type: "delete", key }], { signal: controller.signal });
      } catch (error) {
        aborted = (error as { code: string }).code;
      }
      await db.flush();
      const rows = (await collection.scan()).map(({ key, value }) => [
        Array.from(key),
        Array.from(value),
      ]);
      const rawAbsent = (await db.get(key)) === undefined;
      await db.close();
      return { events, aborted, rows, rawAbsent };
    }),
  ).toEqual({
    events: 1,
    aborted: "ABORTED",
    rows: [
      [[1], [7]],
      [[2], [8]],
    ],
    rawAbsent: true,
  });
});

test("pre-aborted maintenance preserves connection and invalid batches preserve root", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("container-invalid", {
        mode: "container-v1",
      });
      await db.put(new Uint8Array([1]), new Uint8Array([2]));
      const before = await db.stats();
      const codes: string[] = [];
      const controller = new AbortController();
      controller.abort();
      for (const operation of [
        () => db.flush({ signal: controller.signal }),
        () => db.compact({ signal: controller.signal }),
        () => db.stats({ signal: controller.signal }),
      ]) {
        try {
          await operation();
        } catch (error) {
          codes.push((error as { code: string }).code);
        }
      }
      try {
        await db.writeBatch([
          { type: "put", key: new Uint8Array([1]), value: new Uint8Array([3]) },
          { type: "delete", key: new Uint8Array(65537) },
        ]);
      } catch (error) {
        codes.push((error as { code: string }).code);
      }
      const after = await db.stats();
      const value = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      await db.close();
      return { codes, unchanged: before.generation === after.generation, value };
    }),
  ).toEqual({
    codes: ["ABORTED", "ABORTED", "ABORTED", "INVALID_ARGUMENT"],
    unchanged: true,
    value: [2],
  });
});

test("corrupt container errors preserve bytes and never fall back to matching WAL", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const { openDB } = window as unknown as Harness;
      const wal = await openDB("corrupt-public");
      await wal.put(new Uint8Array([1]), new Uint8Array([7]));
      await wal.close();
      const db = await openDB("corrupt-public", { mode: "container-v1" });
      await db.put(new Uint8Array([1]), new Uint8Array([9]));
      await db.close();
      const root = await navigator.storage.getDirectory();
      const directory = await root.getDirectoryHandle("hoke-container-v1");
      const file = await directory.getFileHandle("corrupt-public.hkc");
      const bytes = new Uint8Array(await (await file.getFile()).arrayBuffer());
      bytes[bytes.length - 1] = (bytes[bytes.length - 1] ?? 0) ^ 1;
      const writable = await file.createWritable();
      await writable.write(bytes);
      await writable.close();
      let code = "";
      try {
        await openDB("corrupt-public", { mode: "container-v1" });
      } catch (error) {
        code = (error as { code: string }).code;
      }
      const after = new Uint8Array(await (await file.getFile()).arrayBuffer());
      const original = await openDB("corrupt-public");
      const value = Array.from((await original.get(new Uint8Array([1]))) ?? []);
      await original.close();
      return {
        code,
        unchanged:
          bytes.length === after.length && bytes.every((byte, index) => byte === after[index]),
        value,
      };
    }),
  ).toEqual({ code: "ENGINE", unchanged: true, value: [7] });
});

test("aborted batch with a lost ACK recovers the complete committed state", async ({ page }) => {
  await page.route("**/packages/hoke/dist/db/worker.js", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: `{
      const send=globalThis.postMessage.bind(globalThis);
      globalThis.postMessage=function(message,...args) {
        if(message.id===2 && message.ok) {
          const channel=new BroadcastChannel("container-batch-committed");
          channel.postMessage("committed");channel.close();return;
        }
        return send(message,...args);
      };
    }\n${await response.text()}`,
    });
  });
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("container-unknown", {
        mode: "container-v1",
      });
      let events = 0;
      db.subscribeRange({}, () => {
        events++;
      });
      const side = new BroadcastChannel("container-batch-committed");
      const committed = new Promise<void>((resolve) => {
        side.onmessage = () => resolve();
      });
      const controller = new AbortController();
      const result = db
        .writeBatch(
          [
            { type: "put", key: new Uint8Array([1]), value: new Uint8Array([11]) },
            { type: "put", key: new Uint8Array([2]), value: new Uint8Array([22]) },
          ],
          { signal: controller.signal },
        )
        .then(
          () => "unexpected success",
          (error) => error.code,
        );
      await committed;
      side.close();
      controller.abort();
      return { code: await result, events };
    }),
  ).toEqual({ code: "ABORTED", events: 0 });
  await page.unroute("**/packages/hoke/dist/db/worker.js");
  await expect
    .poll(async () =>
      page.evaluate(async () => {
        try {
          const db = await (window as unknown as Harness).openDB("container-unknown", {
            mode: "container-v1",
          });
          const rows = (await db.scan()).map(({ key, value }) => [
            Array.from(key),
            Array.from(value),
          ]);
          await db.close();
          return rows;
        } catch (error) {
          if ((error as { code: string }).code === "LOCKED") return "locked";
          throw error;
        }
      }),
    )
    .toEqual([
      [[1], [11]],
      [[2], [22]],
    ]);
});

test("raw Worker rejects invalid modes before storage and WAL-only operations remain healthy", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const worker = new Worker("/packages/hoke/dist/db/worker.js", { type: "module" });
      let id = 0;
      const call = (request: object) =>
        new Promise<{ ok: boolean; error?: { code: string }; result?: Uint8Array }>(
          (resolve, reject) => {
            worker.onmessage = (event) => resolve(event.data);
            worker.onerror = (event) => reject(new Error(event.message));
            worker.postMessage({ version: 3, id: ++id, ...request });
          },
        );
      try {
        const invalid = await call({ op: "open", name: "invalid-mode", mode: "future" });
        const root = await navigator.storage.getDirectory();
        let created = false;
        try {
          await root.getDirectoryHandle("hoke-container-v1");
          created = true;
        } catch {}
        const open = await call({ op: "open", name: "raw-wal" });
        const codes = [];
        for (const op of ["batch", "flush", "compact", "stats"]) {
          const response = await call({ op, ...(op === "batch" ? { operations: [] } : {}) });
          codes.push(response.error?.code);
        }
        const put = await call({ op: "put", key: new Uint8Array([1]), value: new Uint8Array([2]) });
        const get = await call({ op: "get", key: new Uint8Array([1]) });
        await call({ op: "close" });
        return {
          invalid: invalid.error?.code,
          created,
          open: open.ok,
          codes,
          put: put.ok,
          value: Array.from(get.result ?? []),
        };
      } finally {
        worker.terminate();
      }
    }),
  ).toEqual({
    invalid: "INVALID_ARGUMENT",
    created: false,
    open: true,
    codes: ["UNSUPPORTED", "UNSUPPORTED", "UNSUPPORTED", "UNSUPPORTED"],
    put: true,
    value: [2],
  });
});

test("raw container batches validate every operation before touching the root", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const worker = new Worker("/packages/hoke/dist/db/worker.js", { type: "module" });
      let id = 0;
      const call = (request: object) =>
        new Promise<{ ok: boolean; error?: { code: string }; result?: Uint8Array }>(
          (resolve, reject) => {
            worker.onmessage = (event) => resolve(event.data);
            worker.onerror = (event) => reject(new Error(event.message));
            worker.postMessage({ version: 3, id: ++id, ...request });
          },
        );
      try {
        await call({ op: "open", name: "raw-container", mode: "container-v1" });
        const before = await call({ op: "stats" });
        const codes = [];
        for (const operations of [
          null,
          {},
          [
            { type: "put", key: new Uint8Array([1]), value: new Uint8Array([2]) },
            { type: "delete", key: new Uint8Array(65537) },
          ],
          [{ type: "future", key: new Uint8Array([1]) }],
          Array.from({ length: 1025 }, () => ({ type: "delete", key: new Uint8Array() })),
        ]) {
          codes.push((await call({ op: "batch", operations })).error?.code);
        }
        const after = await call({ op: "stats" });
        const absent = await call({ op: "get", key: new Uint8Array([1]) });
        const valid = await call({
          op: "batch",
          operations: [{ type: "put", key: new Uint8Array([1]), value: new Uint8Array([7]) }],
        });
        const get = await call({ op: "get", key: new Uint8Array([1]) });
        await call({ op: "close" });
        return {
          codes,
          unchanged:
            Array.from(before.result ?? []).join() === Array.from(after.result ?? []).join(),
          absent: absent.result === null,
          valid: valid.ok,
          value: Array.from(get.result ?? []),
        };
      } finally {
        worker.terminate();
      }
    }),
  ).toEqual({
    codes: [
      "INVALID_ARGUMENT",
      "INVALID_ARGUMENT",
      "INVALID_ARGUMENT",
      "INVALID_ARGUMENT",
      "INVALID_ARGUMENT",
    ],
    unchanged: true,
    absent: true,
    valid: true,
    value: [7],
  });
});
