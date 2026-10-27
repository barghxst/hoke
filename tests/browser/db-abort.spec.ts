import { test as base, expect, type Page } from "@playwright/test";
import type { BrowserDB, openDB } from "../../packages/hoke/src/db/index.js";

type Harness = { openDB: typeof openDB; db: BrowserDB; events: number };

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
    (window as unknown as Harness).openDB = (await import(path)).openDB;
  });
}

async function walBytes(page: Page, name: string) {
  return page.evaluate(async (name) => {
    const root = await navigator.storage.getDirectory();
    const directory = await root.getDirectoryHandle("hoke-wal-v1");
    const handle = await directory.getFileHandle(`${name}.wal`);
    return Array.from(new Uint8Array(await (await handle.getFile()).arrayBuffer()));
  }, name);
}

async function reopen(page: Page, name: string) {
  await expect
    .poll(() =>
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

test("pre-aborted open creates no OPFS entry and a fresh open remains usable", async ({ page }) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const controller = new AbortController();
      controller.abort();
      let code = "unexpected success";
      try {
        await h.openDB("abort-unopened", { signal: controller.signal });
      } catch (error) {
        code = (error as { code: string }).code;
      }
      let absent = false;
      try {
        const root = await navigator.storage.getDirectory();
        const directory = await root.getDirectoryHandle("hoke-wal-v1");
        await directory.getFileHandle("abort-unopened.wal");
      } catch (error) {
        absent = (error as DOMException).name === "NotFoundError";
      }
      const db = await h.openDB("abort-unopened");
      await db.put(new Uint8Array([1]), new Uint8Array([42]));
      const value = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      await db.close();
      return { code, absent, value };
    }),
  ).toEqual({ code: "ABORTED", absent: true, value: [42] });
});

test("pre-aborted reads and mutations preserve WAL bytes and the active database", async ({
  page,
}) => {
  await prepare(page);
  await page.evaluate(async () => {
    const h = window as unknown as Harness;
    const db = await h.openDB("abort-nochange");
    await db.put(new Uint8Array([1]), new Uint8Array([11]));
    await db.close();
  });
  const before = await walBytes(page, "abort-nochange");
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const db = await h.openDB("abort-nochange");
      const controller = new AbortController();
      controller.abort();
      const options = { signal: controller.signal };
      let events = 0;
      db.subscribeRange({}, () => events++);
      const results = await Promise.allSettled([
        db.get(new Uint8Array([1]), options),
        db.put(new Uint8Array([2]), new Uint8Array([22]), options),
        db.delete(new Uint8Array([1]), options),
        db.scan({}, options),
      ]);
      const codes = results.map((result) =>
        result.status === "rejected" ? result.reason.code : "unexpected success",
      );
      const value = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      const absent = (await db.get(new Uint8Array([2]))) === undefined;
      const entries = (await db.scan()).length;
      await db.close();
      return { codes, value, absent, entries, events };
    }),
  ).toEqual({
    codes: ["ABORTED", "ABORTED", "ABORTED", "ABORTED"],
    value: [11],
    absent: true,
    entries: 1,
    events: 0,
  });
  expect(await walBytes(page, "abort-nochange")).toEqual(before);
});

test("collection operations forward abort options and leave collection data unchanged", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const db = await (window as unknown as Harness).openDB("abort-collection");
      const collection = db.collection(new Uint8Array([255, 0]));
      await collection.put(new Uint8Array([1]), new Uint8Array([9]));
      const controller = new AbortController();
      controller.abort();
      const options = { signal: controller.signal };
      let events = 0;
      collection.subscribeRange({}, () => events++);
      const results = await Promise.allSettled([
        collection.get(new Uint8Array([1]), options),
        collection.put(new Uint8Array([2]), new Uint8Array([8]), options),
        collection.delete(new Uint8Array([1]), options),
        collection.scan({}, options),
      ]);
      const codes = results.map((result) =>
        result.status === "rejected" ? result.reason.code : "unexpected success",
      );
      const value = Array.from((await collection.get(new Uint8Array([1]))) ?? []);
      const absent = (await collection.get(new Uint8Array([2]))) === undefined;
      await collection.put(new Uint8Array([3]), new Uint8Array([7]));
      const entries = (await collection.scan()).length;
      await db.close();
      return { codes, value, absent, entries, events };
    }),
  ).toEqual({
    codes: ["ABORTED", "ABORTED", "ABORTED", "ABORTED"],
    value: [9],
    absent: true,
    entries: 2,
    events: 1,
  });
});

for (const multiple of [false, true]) {
  test(`active abort terminates pending requests without false notifications (${multiple ? "multiple pending" : "lost ACK"})`, async ({
    page,
  }) => {
    const channel = `abort-commit-${multiple}`;
    await page.route("**/packages/hoke/dist/db/worker.js", async (route) => {
      const response = await route.fetch();
      await route.fulfill({
        response,
        body: `{
          const send = globalThis.postMessage.bind(globalThis);
          globalThis.postMessage = function(message, ...args) {
            if (message.id >= 3) {
              if (message.id === 3 && message.ok) {
                const channel = new BroadcastChannel(${JSON.stringify(channel)});
                channel.postMessage("committed");
                channel.close();
              }
              return;
            }
            return send(message, ...args);
          };
        }\n${await response.text()}`,
      });
    });
    await prepare(page);
    expect(
      await page.evaluate(
        async ({ channel, multiple }) => {
          const h = window as unknown as Harness;
          h.db = await h.openDB("abort-active");
          await h.db.put(new Uint8Array([1]), new Uint8Array([11]));
          h.events = 0;
          h.db.subscribeRange({}, () => h.events++);
          const side = new BroadcastChannel(channel);
          const committed = new Promise<void>((resolve) => {
            side.onmessage = () => resolve();
          });
          const controller = new AbortController();
          const calls: Promise<unknown>[] = [
            h.db.put(new Uint8Array([2]), new Uint8Array([22, 0, 255]), {
              signal: controller.signal,
            }),
          ];
          if (multiple) {
            calls.push(h.db.get(new Uint8Array([1])));
            calls.push(h.db.delete(new Uint8Array([99])));
          }
          const settled = Promise.allSettled(calls);
          await committed;
          side.close();
          controller.abort();
          const codes = (await settled).map((result) =>
            result.status === "rejected" ? result.reason.code : "unexpected success",
          );
          let closed = "unexpected success";
          try {
            await h.db.get(new Uint8Array([1]));
          } catch (error) {
            closed = (error as { code: string }).code;
          }
          return { codes, closed, events: h.events };
        },
        { channel, multiple },
      ),
    ).toEqual({
      codes: multiple ? ["ABORTED", "ABORTED", "ABORTED"] : ["ABORTED"],
      closed: "CLOSED",
      events: 0,
    });
    await page.unroute("**/packages/hoke/dist/db/worker.js");
    await reopen(page, "abort-active");
    expect(
      await page.evaluate(async () => {
        const h = window as unknown as Harness;
        const prior = Array.from((await h.db.get(new Uint8Array([1]))) ?? []);
        const committed = Array.from((await h.db.get(new Uint8Array([2]))) ?? []);
        await h.db.put(new Uint8Array([3]), new Uint8Array([33]));
        await h.db.close();
        return { prior, committed, events: h.events };
      }),
    ).toEqual({ prior: [11], committed: [22, 0, 255], events: 0 });
  });
}

test("aborting after open and inside an ACK callback does not terminate the healthy database", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const opening = new AbortController();
      const db = await h.openDB("abort-detached", { signal: opening.signal });
      opening.abort();
      const controller = new AbortController();
      let events = 0;
      db.subscribe(new Uint8Array([1]), () => {
        events++;
        controller.abort();
      });
      await db.put(new Uint8Array([1]), new Uint8Array([11]), { signal: controller.signal });
      const value = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      await db.put(new Uint8Array([2]), new Uint8Array([22]));
      await db.close();
      const reopened = await h.openDB("abort-detached");
      const persisted = Array.from((await reopened.get(new Uint8Array([2]))) ?? []);
      await reopened.close();
      return { value, persisted, events };
    }),
  ).toEqual({ value: [11], persisted: [22], events: 1 });
});

test("aborting an opening worker releases its already acquired OPFS ownership", async ({
  page,
}) => {
  await page.route("**/packages/hoke/dist/db/worker.js", async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: `{
        const send = globalThis.postMessage.bind(globalThis);
        globalThis.postMessage = function(message, ...args) {
          if (message.id === 1 && message.ok) {
            const channel = new BroadcastChannel("abort-open-owner");
            channel.postMessage("opened");
            channel.close();
            return;
          }
          return send(message, ...args);
        };
      }\n${await response.text()}`,
    });
  });
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const h = window as unknown as Harness;
      const controller = new AbortController();
      const side = new BroadcastChannel("abort-open-owner");
      const owned = new Promise<void>((resolve) => {
        side.onmessage = () => resolve();
      });
      const result = h.openDB("abort-opening", { signal: controller.signal }).then(
        async (db) => {
          await db.close();
          return "unexpected success";
        },
        (error: { code: string }) => error.code,
      );
      await owned;
      side.close();
      controller.abort();
      return result;
    }),
  ).toBe("ABORTED");
  await page.unroute("**/packages/hoke/dist/db/worker.js");
  await reopen(page, "abort-opening");
  expect(
    await page.evaluate(async () => {
      const db = (window as unknown as Harness).db;
      await db.put(new Uint8Array([1]), new Uint8Array([7]));
      const value = Array.from((await db.get(new Uint8Array([1]))) ?? []);
      await db.close();
      return value;
    }),
  ).toEqual([7]);
});
