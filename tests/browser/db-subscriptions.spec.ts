import { test as base, expect, type Page } from "@playwright/test";
import type { BrowserDB, DBNotificationError, openDB } from "../../packages/hoke/src/db/index.js";

type Harness = {
  openDB: typeof openDB;
  DBNotificationError: typeof DBNotificationError;
  db: BrowserDB;
  events: number;
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
    const path = "/packages/hoke/dist/db/index.js";
    const api = await import(path);
    Object.assign(window, { openDB: api.openDB, DBNotificationError: api.DBNotificationError });
  });
}

test("key notifications follow successful ACKs, copy keys, and expose committed data", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("watch-keys");
      let calls = 0;
      const reads: Promise<number[]>[] = [];
      const subscribed = new Uint8Array([0, 255]);
      const stop = db.subscribe(subscribed, () => {
        calls++;
        reads.push(db.get(new Uint8Array([0, 255])).then((value) => Array.from(value ?? [])));
      });
      subscribed.fill(9);
      const initial = calls;
      await db.get(new Uint8Array([0, 255]));
      await db.scan();
      await db.put(new Uint8Array([1]), new Uint8Array([99]));
      const afterReads = calls;
      const key = new Uint8Array([0, 255]);
      const pending = db.put(key, new Uint8Array([7]));
      key.fill(8);
      await pending;
      const afterAck = calls;
      await db.put(new Uint8Array([0, 255]), new Uint8Array([7]));
      await db.delete(new Uint8Array([0, 255]));
      await db.delete(new Uint8Array([0, 255]));
      stop();
      stop();
      await db.put(new Uint8Array([0, 255]), new Uint8Array([8]));
      const values = await Promise.all(reads);
      await db.close();
      return { initial, afterReads, afterAck, calls, values };
    }),
  ).toEqual({ initial: 0, afterReads: 0, afterAck: 1, calls: 4, values: [[7], [7], [], []] });
});

test("range subscriptions intersect binary prefixes and exclusive bounds with copied options", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("watch-ranges");
      const events: string[] = [];
      const range = {
        prefix: new Uint8Array([255]),
        start: { key: new Uint8Array([255, 1]), inclusive: false },
        end: { key: new Uint8Array([255, 3]), inclusive: true },
      };
      db.subscribeRange(range, () => events.push("bounded"));
      db.subscribeRange({ prefix: new Uint8Array([255]) }, () => events.push("prefix"));
      const stopAll = db.subscribeRange({}, () => events.push("all"));
      range.prefix.fill(0);
      range.start.key.fill(0);
      range.end.key.fill(0);
      range.start.inclusive = true;
      range.end.inclusive = false;
      for (const key of [[], [254, 2], [255, 1], [255, 2], [255, 3], [255, 4]]) {
        await db.put(new Uint8Array(key), new Uint8Array());
      }
      stopAll();
      await db.delete(new Uint8Array([255, 2]));
      await db.close();
      return events;
    }),
  ).toEqual([
    "all",
    "all",
    "prefix",
    "all",
    "bounded",
    "prefix",
    "all",
    "bounded",
    "prefix",
    "all",
    "prefix",
    "all",
    "bounded",
    "prefix",
  ]);
});

test("listeners added during dispatch start on the next event and unsubscribe remains effective", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("watch-dispatch");
      const key = new Uint8Array([1]);
      const events: string[] = [];
      let added = false;
      let stopLater = () => {};
      const stopFirst = db.subscribe(key, () => {
        events.push("first");
        if (!added) {
          added = true;
          stopLater = db.subscribe(key, () => events.push("later"));
        }
      });
      await db.put(key, new Uint8Array([1]));
      await db.put(key, new Uint8Array([2]));
      stopFirst();
      stopLater();
      await db.delete(key);
      await db.close();
      return events;
    }),
  ).toEqual(["first", "first", "later"]);
});

test("subscriber failures reject after commit, run all listeners, and do not poison the database", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("watch-errors");
      const key = new Uint8Array([1]);
      const events: string[] = [];
      const stop = db.subscribe(key, () => {
        events.push("throw");
        throw new Error("subscriber boom");
      });
      db.subscribeRange({}, () => events.push("range"));
      let result: unknown;
      try {
        await db.put(key, new Uint8Array([41]));
      } catch (error) {
        const failure = error as DBNotificationError;
        result = {
          exported: error instanceof h.DBNotificationError,
          aggregate: error instanceof AggregateError,
          code: failure.code,
          committed: failure.committed,
          messages: failure.errors.map((item: Error) => item.message),
        };
      }
      const committed = Array.from((await db.get(key)) ?? []);
      stop();
      await db.put(key, new Uint8Array([42]));
      await db.close();
      const reopened = await h.openDB("watch-errors");
      const persisted = Array.from((await reopened.get(key)) ?? []);
      await reopened.put(key, new Uint8Array([43]));
      await reopened.close();
      return { result, committed, persisted, events };
    }),
  ).toEqual({
    result: {
      exported: true,
      aggregate: true,
      code: "SUBSCRIBER_FAILED",
      committed: true,
      messages: ["subscriber boom"],
    },
    committed: [41],
    persisted: [42],
    events: ["throw", "range", "range"],
  });
});

test("unsupported async callbacks reject after commit without unhandled browser errors", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("watch-async");
      const key = new Uint8Array([1]);
      let others = 0;
      const stop = db.subscribe(key, async () => {
        throw new Error("async subscriber boom");
      });
      db.subscribeRange({}, () => {
        others++;
      });
      let failure: unknown;
      try {
        await db.put(key, new Uint8Array([6]));
      } catch (error) {
        failure = {
          code: (error as DBNotificationError).code,
          committed: (error as DBNotificationError).committed,
        };
      }
      await new Promise((resolve) => setTimeout(resolve, 30));
      const value = Array.from((await db.get(key)) ?? []);
      stop();
      await db.delete(key);
      await db.close();
      const reopened = await h.openDB("watch-async");
      const absent = (await reopened.get(key)) === undefined;
      await reopened.close();
      return { failure, value, others, absent };
    }),
  ).toEqual({
    failure: { code: "SUBSCRIBER_FAILED", committed: true },
    value: [6],
    others: 2,
    absent: true,
  });
});

test("collection subscriptions use relative keys and ranges without namespace collisions", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("watch-collections");
      const prefix = new Uint8Array([5]);
      const first = db.collection(prefix);
      prefix.fill(9);
      const same = db.collection(new Uint8Array([5]));
      const other = db.collection(new Uint8Array([5, 0]));
      const empty = db.collection(new Uint8Array());
      const events: string[] = [];
      first.subscribe(new Uint8Array([1, 2]), () => events.push("key"));
      first.subscribeRange(
        {
          prefix: new Uint8Array([1]),
          start: { key: new Uint8Array([1, 1]), inclusive: false },
          end: { key: new Uint8Array([1, 3]), inclusive: true },
        },
        () => events.push("range"),
      );
      first.subscribeRange({}, () => events.push("first"));
      other.subscribeRange({}, () => events.push("other"));
      empty.subscribeRange({}, () => events.push("empty"));
      await same.put(new Uint8Array([1, 2]), new Uint8Array([1]));
      await first.put(new Uint8Array([1, 1]), new Uint8Array([1]));
      await first.delete(new Uint8Array([1, 3]));
      await other.put(new Uint8Array([1, 2]), new Uint8Array([2]));
      await empty.put(new Uint8Array([1, 2]), new Uint8Array([3]));
      await db.put(new Uint8Array([1, 2]), new Uint8Array([4]));
      await db.close();
      return events;
    }),
  ).toEqual(["key", "range", "first", "first", "range", "first", "other", "empty"]);
});

test("close clears watchers immediately, pending ACKs and reopened databases cannot notify old listeners", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("watch-close");
      let events = 0;
      db.subscribeRange({}, () => {
        events++;
      });
      const pending = db.put(new Uint8Array([1]), new Uint8Array([5]));
      const close = db.close();
      await pending;
      await close;
      const afterClose = events;
      const reopened = await h.openDB("watch-close");
      const persisted = Array.from((await reopened.get(new Uint8Array([1]))) ?? []);
      await reopened.put(new Uint8Array([1]), new Uint8Array([6]));
      await reopened.scan();
      await reopened.close();
      return { afterClose, events, persisted };
    }),
  ).toEqual({ afterClose: 0, events: 0, persisted: [5] });
});

test("failed OPFS flush emits no invalidation even when recovery finds the unknown write", async ({
  page,
}) => {
  await page.route("**/packages/hoke/dist/db/worker.js", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: `{
      const original = FileSystemSyncAccessHandle.prototype.flush;
      let calls = 0;
      FileSystemSyncAccessHandle.prototype.flush = function () {
        if (++calls === 2) throw new DOMException("injected flush failure", "QuotaExceededError");
        return original.call(this);
      };
    }\n${await response.text()}`,
    });
  });
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      h.db = await h.openDB("watch-flush");
      h.events = 0;
      h.db.subscribeRange({}, () => {
        h.events++;
      });
      const codes: string[] = [];
      for (const key of [1, 2]) {
        try {
          await h.db.put(new Uint8Array([key]), new Uint8Array([key + 10]));
        } catch (error) {
          codes.push((error as { code: string }).code);
        }
      }
      try {
        await h.db.close();
      } catch {}
      return { codes, events: h.events };
    }),
  ).toEqual({ codes: ["ENGINE", "ENGINE"], events: 0 });
  await page.unroute("**/packages/hoke/dist/db/worker.js");
  await expect
    .poll(async () =>
      page.evaluate(async () => {
        const h = window as unknown as Harness;
        try {
          h.db = await h.openDB("watch-flush");
          return "open";
        } catch (error) {
          return (error as { code: string }).code;
        }
      }),
    )
    .toBe("open");
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const recovered = Array.from((await h.db.get(new Uint8Array([1]))) ?? []);
      const absent = (await h.db.get(new Uint8Array([2]))) === undefined;
      await h.db.put(new Uint8Array([3]), new Uint8Array([33]));
      await h.db.close();
      return { recovered, absent, events: h.events };
    }),
  ).toEqual({ recovered: [11], absent: true, events: 0 });
});
