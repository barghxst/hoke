import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { afterAll, expect, it } from "vitest";

// Compile current source in isolation; never depend on or rewrite the dist build.
const buildDir = mkdtempSync(join(tmpdir(), "hoke-store-process-"));
try {
  const compiler = spawnSync(
    process.execPath,
    [
      fileURLToPath(new URL("./bin/tsc", import.meta.resolve("typescript/package.json"))),
      "--ignoreConfig",
      "--target",
      "ES2022",
      "--module",
      "ESNext",
      "--skipLibCheck",
      "--types",
      "node",
      "--outDir",
      buildDir,
      fileURLToPath(new URL("./store.ts", import.meta.url)),
    ],
    { encoding: "utf8", timeout: 30_000 },
  );
  if (compiler.error || compiler.status !== 0) {
    throw new Error(
      `Store process fixture compilation failed: ${compiler.stdout}${compiler.stderr}`,
      {
        cause: compiler.error,
      },
    );
  }
  writeFileSync(join(buildDir, "package.json"), '{"type":"module"}');
} catch (error) {
  rmSync(buildDir, { recursive: true, force: true });
  throw error;
}
afterAll(() => rmSync(buildDir, { recursive: true, force: true }));

it.each(["local", "cross-realm", "custom thenable"])(
  "handles %s results without unhandled rejections or custom then calls",
  (kind) => {
    const child = spawnSync(
      process.execPath,
      ["--unhandled-rejections=strict", "--input-type=module", "-"],
      {
        encoding: "utf8",
        timeout: 10_000,
        input: `import assert from "node:assert/strict";
import { runInNewContext } from "node:vm";
import { createKV, createStore, defineStore, StoreError, createLocalStorage, KVStorageError }
  from ${JSON.stringify(pathToFileURL(join(buildDir, "kv/index.js")).href)};
const kind = ${JSON.stringify(kind)};
let customCalls = 0;
const rejected = () => kind === "cross-realm"
  ? runInNewContext('Promise.reject(new Error("async failure"))')
  : kind === "custom thenable"
    ? { then() { customCalls++; throw new Error("must not execute"); } }
    : Promise.reject(new Error("async failure"));
const makeKV = () => createKV();
const definition = extra => defineStore({
  key: "counter", version: 1, defaults: () => 0,
  validate: value => typeof value === "number", ...extra,
});
const expectCode = (fn, code) => assert.throws(fn, error =>
  error instanceof StoreError && error.code === code);

expectCode(() => createStore(makeKV(), definition({ defaults: rejected })), "invalid-defaults");

const kv = makeKV();
const store = createStore(kv, definition());
expectCode(() => store.set(rejected()), "invalid-value");
expectCode(() => store.update(rejected), "async-result");
assert.equal(kv.get("counter"), undefined);
assert.equal(store.getSnapshot(), 0);

const persistedKV = makeKV();
const persisted = { __stowtree: 1, version: 1, value: rejected() };
persistedKV.set("counter", persisted);
expectCode(() => createStore(persistedKV, definition()), "invalid-value");
assert.equal(persistedKV.get("counter"), persisted);

const migrationKV = makeKV();
const original = { __stowtree: 1, version: 1, value: 1 };
migrationKV.set("counter", original);
expectCode(() => createStore(migrationKV, definition({
  version: 2, migrations: { 2: rejected },
})), "async-result");
assert.equal(migrationKV.get("counter"), original);

expectCode(() => createStore(makeKV(), definition({ validate: rejected })), "invalid-defaults");
const validationKV = makeKV();
const checked = createStore(validationKV, definition({
  validate: value => value === 0 ? true : rejected(),
}));
expectCode(() => checked.set(1), "invalid-value");
assert.equal(validationKV.get("counter"), undefined);

expectCode(() => createStore(migrationKV, definition({
  version: 2, migrations: { 2: () => 2 },
  validate: value => value === 0 ? true : rejected(),
})), "invalid-value");
assert.equal(migrationKV.get("counter"), original);

for (const invalid of [Promise.resolve(true), Promise.resolve(false), 1, "true"]) {
  expectCode(() => createStore(makeKV(), definition({ validate: () => invalid })), "invalid-defaults");
}

const data = new Map();
const storage = {
  getItem: key => data.get(key) ?? null,
  setItem: (key, value) => data.set(key, value),
  removeItem: key => data.delete(key),
};
const physicalKey = 'stowtree:["async-validation","key"]';
data.set(physicalKey, "42");
let accepts = true;
const adapter = createLocalStorage({
  name: "async-validation", storage,
  validate: () => accepts ? true : rejected(),
});
const oldSnapshot = adapter.read("key");
accepts = false;
assert.throws(() => adapter.read("key"), error =>
  error instanceof KVStorageError && error.code === "INVALID_DATA");
assert.throws(() => adapter.write("key", 99), error =>
  error instanceof KVStorageError && error.code === "INVALID_VALUE");
assert.equal(data.get(physicalKey), "42");
accepts = true;
assert.equal(adapter.read("key").value, 42);
assert.notEqual(adapter.read("key"), oldSnapshot);
assert.throws(() => adapter.write("key", rejected()), error =>
  error instanceof KVStorageError && error.code === "INVALID_VALUE");
assert.equal(data.get(physicalKey), "42");
for (const invalid of [Promise.resolve(true), Promise.resolve(false), 1, "true"]) {
  const invalidAdapter = createLocalStorage({ name: "async-validation", storage, validate: () => invalid });
  assert.throws(() => invalidAdapter.read("key"), error => error.code === "INVALID_DATA");
  assert.throws(() => invalidAdapter.write("key", 99), error => error.code === "INVALID_VALUE");
  assert.equal(data.get(physicalKey), "42");
}
assert.equal(customCalls, 0);
await new Promise(resolve => setImmediate(resolve));
console.log("async rejection checks passed");
`,
      },
    );
    expect(child.error).toBeUndefined();
    expect(child.status, child.stderr).toBe(0);
    expect(child.stdout).toBe("async rejection checks passed\n");
    expect(child.stderr).toBe("");
  },
);
