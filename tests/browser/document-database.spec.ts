import { expect, type Page, test } from "@playwright/test";

type Harness = typeof import("../../packages/hoke/src/db/index.js");

async function prepare(page: Page) {
  await page.goto("/examples/web/");
  await page.evaluate(async () => {
    const path = "/packages/hoke/dist/db/index.js";
    Object.assign(window, await import(path));
  });
}

test("document collections preserve Unicode keys, isolate names, batch atomically and reopen", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const { openDatabase } = window as unknown as Harness;
      type User = { name: string };
      const schema = {
        validate: (value: unknown): value is User =>
          typeof value === "object" && value !== null && typeof (value as User).name === "string",
      };
      const db = await openDatabase("documents-unicode");
      const users = db.collection("\uFEFFлюди😀", schema);
      const separate = db.collection("люди😀", schema);
      await users.set("é", { name: "Alice" });
      await users.set("e\u0301", { name: "Combining" });
      await users.set("\uFEFFalice", { name: "BOM" });
      await users.set("😀", { name: "Emoji" });
      await separate.set("é", { name: "Separate" });
      let invalidations = 0;
      let isolatedInvalidations = 0;
      users.subscribeRange({}, () => {
        invalidations++;
      });
      separate.subscribeRange({}, () => {
        isolatedInvalidations++;
      });
      await users.writeBatch([
        { type: "set", key: "é", value: { name: "Old" } },
        { type: "set", key: "other", value: { name: "Removed" } },
        { type: "delete", key: "other" },
        { type: "delete", key: "é" },
        { type: "set", key: "é", value: { name: "Latest" } },
      ]);
      const snapshot = await users.get("é");
      const range = await users.list({
        start: { key: "é", inclusive: true },
        end: { key: "😀", inclusive: false },
        reverse: true,
      });
      const prefix = await users.list({ prefix: "\uFEFF" });
      const result = {
        value: snapshot,
        frozen: Object.isFrozen(snapshot),
        range,
        prefix,
        invalidations,
        isolatedInvalidations,
        separate: await separate.get("é"),
        deleted: (await users.get("other")) === undefined,
      };
      await db.close();
      return result;
    }),
  ).toEqual({
    value: { name: "Latest" },
    frozen: true,
    range: [
      { key: "\uFEFFalice", value: { name: "BOM" } },
      { key: "é", value: { name: "Latest" } },
    ],
    prefix: [{ key: "\uFEFFalice", value: { name: "BOM" } }],
    invalidations: 1,
    isolatedInvalidations: 0,
    separate: { name: "Separate" },
    deleted: true,
  });
  await page.reload();
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const { openDatabase } = window as unknown as Harness;
      const db = await openDatabase("documents-unicode");
      const users = db.collection("\uFEFFлюди😀", {
        validate: (value: unknown): value is { name: string } =>
          typeof value === "object" &&
          value !== null &&
          typeof (value as { name: unknown }).name === "string",
      });
      const rows = await users.list();
      await db.close();
      return rows;
    }),
  ).toEqual([
    { key: "e\u0301", value: { name: "Combining" } },
    { key: "é", value: { name: "Latest" } },
    { key: "\uFEFFalice", value: { name: "BOM" } },
    { key: "😀", value: { name: "Emoji" } },
  ]);
});

test("automatic document maintenance preserves overwrites and tombstones across more than 100 writes", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const { openDatabase, openDB } = window as unknown as Harness;
      const schema = {
        validate: (value: unknown): value is { version: number } =>
          typeof value === "object" &&
          value !== null &&
          Number.isInteger((value as { version: unknown }).version),
      };
      const db = await openDatabase("documents-maintenance");
      const values = db.collection("values", schema);
      await values.set("deleted", { version: -1 });
      for (let version = 0; version < 110; version++) {
        await values.set("latest", { version });
        if (version === 2) await values.delete("deleted");
      }
      const latest = await values.get("latest");
      const deleted = (await values.get("deleted")) === undefined;
      await db.close();
      const raw = await openDB("documents-maintenance", { mode: "container-v1" });
      const stats = await raw.stats();
      await raw.close();
      const reopened = await openDatabase("documents-maintenance");
      const rows = await reopened.collection("values", schema).list();
      await reopened.close();
      return {
        latest,
        deleted,
        rows,
        tables: stats.tableCount,
        walSegments: stats.walSegments,
        generation: Number(stats.generation),
      };
    }),
  ).toMatchObject({
    latest: { version: 109 },
    deleted: true,
    rows: [{ key: "latest", value: { version: 109 } }],
  });
  // Read persisted stats independently after ownership has returned from the facade.
  const stats = await page.evaluate(async () => {
    const raw = await (window as unknown as Harness).openDB("documents-maintenance", {
      mode: "container-v1",
    });
    const stats = await raw.stats();
    await raw.close();
    return {
      tables: stats.tableCount,
      walSegments: stats.walSegments,
      generation: Number(stats.generation),
    };
  });
  expect(stats.tables).toBeGreaterThan(0);
  expect(stats.walSegments).toBeLessThan(48);
  expect(stats.generation).toBeGreaterThan(112);
});

test("invalid JSON injected through the byte API is reported without rewriting the container", async ({
  page,
}) => {
  await prepare(page);
  expect(
    await page.evaluate(async () => {
      const { openDB, openDatabase } = window as unknown as Harness;
      const encode = (text: string) => new TextEncoder().encode(text);
      const name = "documents-corruption";
      const raw = await openDB(name, { mode: "container-v1" });
      const bytes = new Uint8Array([0xff, 0x7b]);
      await raw.collection(encode("users")).put(encode("alice"), bytes);
      await raw.close();
      const readFile = async () => {
        const root = await navigator.storage.getDirectory();
        const folder = await root.getDirectoryHandle("hoke-container-v1");
        return new Uint8Array(
          await (await (await folder.getFileHandle(`${name}.hkc`)).getFile()).arrayBuffer(),
        );
      };
      const before = await readFile();
      const db = await openDatabase(name);
      const users = db.collection("users", {
        validate: (value: unknown): value is { name: string } =>
          typeof value === "object" &&
          value !== null &&
          typeof (value as { name: unknown }).name === "string",
      });
      const errors = [];
      for (const operation of [() => users.get("alice"), () => users.list()]) {
        try {
          await operation();
          errors.push("unexpected success");
        } catch (error) {
          errors.push((error as { code: string }).code);
        }
      }
      await db.close();
      const after = await readFile();
      const reopened = await openDB(name, { mode: "container-v1" });
      const persisted = await reopened.collection(encode("users")).get(encode("alice"));
      await reopened.close();
      return {
        errors,
        unchanged:
          before.length === after.length && before.every((byte, index) => byte === after[index]),
        persisted: Array.from(persisted ?? []),
      };
    }),
  ).toEqual({ errors: ["INVALID_DATA", "INVALID_DATA"], unchanged: true, persisted: [255, 123] });
});
