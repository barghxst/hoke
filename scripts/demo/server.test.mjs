import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { request } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { startDemoServer } from "./server.mjs";

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "hoke-demo-server-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const directory of ["examples/empty", "packages/hoke/dist", "packages/hoke/wasm"]) {
    await mkdir(join(root, directory), { recursive: true });
  }
  await writeFile(join(root, "examples/index.html"), "<h1>Hoke</h1>");
  await writeFile(join(root, "packages/hoke/wasm/core.wasm"), new Uint8Array([0, 97, 115, 109]));
  await writeFile(join(root, "packages/hoke/dist/index.js"), "export {};");
  await writeFile(join(root, "secret.txt"), "private");
  const entries = [];
  const instance = await startDemoServer({ root, port: 0, log: (...entry) => entries.push(entry) });
  t.after(() => instance.close());
  return { root, entries, ...instance };
}

function fetchRaw(origin, path, method = "GET") {
  return new Promise((resolve, reject) => {
    const outgoing = request(`${origin}`, { path, method }, (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () =>
        resolve({
          status: response.statusCode,
          headers: response.headers,
          body: Buffer.concat(chunks).toString(),
        }),
      );
      response.on("error", reject);
    });
    outgoing.on("error", reject);
    outgoing.end();
  });
}

test("serves demo index and built assets with MIME, no caching and HEAD", async (t) => {
  const { origin, entries } = await fixture(t);
  for (const path of ["/", "/examples", "/examples/", "/examples/index.html"]) {
    const result = await fetchRaw(origin, path);
    assert.equal(result.status, 200);
    assert.equal(result.body, "<h1>Hoke</h1>");
    assert.equal(result.headers["cache-control"], "no-store");
  }
  const wasm = await fetchRaw(origin, "/packages/hoke/wasm/core.wasm");
  assert.equal(wasm.status, 200);
  assert.equal(wasm.headers["content-type"], "application/wasm");
  const js = await fetchRaw(origin, "/packages/hoke/dist/index.js", "HEAD");
  assert.equal(js.status, 200);
  assert.equal(js.body, "");
  assert.equal(js.headers["content-length"], "10");
  assert.match(js.headers["content-type"], /javascript/);
  assert.ok(entries.some(([level, line]) => level === "info" && /^200 GET \/ \d+ms$/.test(line)));
});

test("does not expose repository files, listings, traversal or symlink targets", async (t) => {
  const { root, origin } = await fixture(t);
  await symlink(join(root, "secret.txt"), join(root, "examples/leak.txt"));
  await symlink(root, join(root, "examples/leak-directory"));
  for (const path of [
    "/secret.txt",
    "/examples/empty/",
    "/examples/missing",
    "/packages/hoke/wasm/missing.wasm",
    "/packages/hoke/distinct/index.js",
  ]) {
    assert.equal((await fetchRaw(origin, path)).status, 404, path);
  }
  for (const path of [
    "/examples/../secret.txt",
    "/examples/%2e%2e/secret.txt",
    "/examples/%2e%2e%2fsecret.txt",
    "/examples/%5c..%5csecret.txt",
    "/examples/%00index.html",
    "/examples/leak.txt",
    "/examples/leak-directory/secret.txt",
  ]) {
    assert.equal((await fetchRaw(origin, path)).status, 403, path);
  }
  assert.equal((await fetchRaw(origin, "/examples/%ZZ")).status, 400);
  const post = await fetchRaw(origin, "/", "POST");
  assert.equal(post.status, 405);
  assert.equal(post.headers.allow, "GET, HEAD");
});

test("selects another port without taking over an occupied listener", async (t) => {
  const first = await fixture(t);
  await assert.rejects(
    startDemoServer({ root: first.root, port: first.port, strictPort: true }),
    (error) => error.cause?.code === "EADDRINUSE" && /Cannot start demo server/.test(error.message),
  );
  const entries = [];
  const second = await startDemoServer({
    root: first.root,
    port: first.port,
    log: (...entry) => entries.push(entry),
  });
  t.after(() => second.close());
  assert.ok(second.port > first.port);
  assert.ok(second.port < first.port + 20);
  assert.equal((await fetchRaw(first.origin, "/")).status, 200);
  assert.equal((await fetchRaw(second.origin, "/")).status, 200);
  assert.equal(entries[0][0], "warn");
  assert.match(entries[0][1], /occupied/);
  await second.close();
  await second.close();
});
