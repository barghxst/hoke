import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { createServer } from "node:http";
import { chromium } from "@playwright/test";

// A disposable context and an empty, dedicated origin; never use a user profile.
const server = createServer((_request, response) => {
  response.writeHead(200, { "Content-Type": "text/html" });
  response.end("<!doctype html><title>Isolated OPFS capability probe</title>");
});
let browser;
let context;
try {
  server.listen(4175, "127.0.0.1");
  await once(server, "listening");
  browser = await chromium.launch({ channel: process.env.HOKE_BROWSER_CHANNEL ?? "chrome" });
  context = await browser.newContext();
  const page = await context.newPage();
  await page.goto("http://127.0.0.1:4175/");
  const name = `hoke-probe-${randomUUID()}`;
  const result = await page.evaluate(async (scratch) => {
    async function probe(directoryName) {
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle(directoryName, { create: true });
      const encode = (value) => new TextEncoder().encode(value);
      const text = async (handle) => (await handle.getFile()).text();
      const names = async () => {
        const entries = [];
        for await (const name of dir.keys()) entries.push(name);
        return entries.sort();
      };
      const attempt = async (callback) => {
        try {
          await callback();
          return "ok";
        } catch (error) {
          return error.name;
        }
      };
      const source = await dir.getFileHandle("next", { create: true });
      const target = await dir.getFileHandle("MANIFEST", { create: true });
      const write = async (file, value) => {
        const handle = await file.createSyncAccessHandle();
        try {
          handle.write(encode(value), { at: 0 });
          handle.flush();
        } finally {
          handle.close();
        }
      };
      await write(source, "new");
      await write(target, "old");
      const sourceHandle = await source.createSyncAccessHandle();
      let secondOwner;
      let moveWhileOpen;
      try {
        secondOwner = await attempt(async () => (await source.createSyncAccessHandle()).close());
        moveWhileOpen =
          typeof source.move === "function"
            ? await attempt(() => source.move("MANIFEST"))
            : "absent";
      } finally {
        sourceHandle.close();
      }
      const moveAfterClose =
        typeof source.move === "function" ? await attempt(() => source.move("MANIFEST")) : "absent";
      const existing = await dir.getFileHandle("MANIFEST", { create: true });
      return {
        directoryMethods: Object.getOwnPropertyNames(FileSystemDirectoryHandle.prototype).sort(),
        syncMethods: Object.getOwnPropertyNames(FileSystemSyncAccessHandle.prototype).sort(),
        moveAvailable: typeof source.move === "function",
        secondOwner,
        moveWhileOpen,
        moveAfterClose,
        content: await text(existing),
        entries: await names(),
      };
    }
    const url = URL.createObjectURL(
      new Blob(
        [
          `(${probe.toString()})(${JSON.stringify(scratch)}).then(result => postMessage({result}), error => postMessage({error: String(error)}))`,
        ],
        { type: "text/javascript" },
      ),
    );
    const worker = new Worker(url);
    try {
      return await new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("Probe timed out")), 15_000);
        worker.onmessage = ({ data }) => {
          clearTimeout(timeout);
          data.error ? reject(new Error(data.error)) : resolve(data.result);
        };
        worker.onerror = (event) => {
          clearTimeout(timeout);
          reject(new Error(event.message));
        };
      });
    } finally {
      worker.terminate();
      URL.revokeObjectURL(url);
    }
  }, name);
  assert.equal(result.secondOwner, "NoModificationAllowedError");
  if (result.moveAfterClose === "ok") {
    assert.equal(result.content, "new");
    assert.deepEqual(result.entries, ["MANIFEST"]);
  }
  await page.reload();
  const afterReload = await page.evaluate(async (scratch) => {
    const root = await navigator.storage.getDirectory();
    const directory = await root.getDirectoryHandle(scratch);
    const file = await directory.getFileHandle("MANIFEST");
    return (await file.getFile()).text();
  }, name);
  assert.equal(afterReload, result.content);
  await page.evaluate(async (scratch) => {
    await (await navigator.storage.getDirectory()).removeEntry(scratch, { recursive: true });
  }, name);
  console.log(
    JSON.stringify(
      {
        browser: browser.version(),
        origin: "http://127.0.0.1:4175",
        ...result,
        afterReload,
        limitations:
          "API/reload probe only; no crash, power-loss, atomicity or directory-durability proof",
      },
      null,
      2,
    ),
  );
} finally {
  await context?.close();
  await browser?.close();
  await new Promise((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
}
