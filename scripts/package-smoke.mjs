import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const directory = await mkdtemp(join(tmpdir(), "hoke-package-"));
function run(command, args, cwd = root) {
  try {
    return execFileSync(command, args, { cwd, encoding: "utf8", stdio: "pipe" });
  } catch (error) {
    if (error.stdout) process.stderr.write(error.stdout);
    if (error.stderr) process.stderr.write(error.stderr);
    throw error;
  }
}

try {
  run("pnpm", ["--dir", "packages/hoke", "pack", "--pack-destination", directory]);
  const archive = (await readdir(directory)).find((name) => name.endsWith(".tgz"));
  assert(archive, "pnpm pack did not produce a tarball");
  const contents = run("tar", ["-tzf", join(directory, archive)]);
  assert(contents.includes("package/dist/kv/index.d.ts"));
  assert(contents.includes("package/dist/react/index.js"));
  assert(contents.includes("package/dist/db/worker.js"));
  assert(contents.includes("package/dist/db/sst-files.js"));
  assert(contents.includes("package/wasm/hoke_wasm.js"));
  assert(contents.includes("package/wasm/hoke_wasm_bg.wasm"));
  assert(!/\.test\.|node_modules\/|package\/src\//.test(contents), "Unexpected files in tarball");

  await writeFile(
    join(directory, "package.json"),
    JSON.stringify({ private: true, type: "module" }),
  );
  run(
    "npm",
    [
      "install",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--package-lock=false",
      join(directory, archive),
    ],
    directory,
  );
  const consumer = `
    import assert from 'node:assert/strict';
    import { createKV, createStore, defineStore, createLocalStorage } from 'hoke/kv';
    import { openDB, DBError, DBNotificationError } from 'hoke/db';
    const subscriberError = new DBNotificationError([new Error('listener')]);
    assert(subscriberError instanceof AggregateError);
    assert.equal(subscriberError.committed, true);
    assert.equal(subscriberError.code, 'SUBSCRIBER_FAILED');
    assert.equal(typeof globalThis.Worker, 'undefined');
    const controller = new AbortController();
    controller.abort();
    await assert.rejects(openDB('preaborted', { signal: controller.signal }),
      error => error instanceof DBError && error.code === 'ABORTED');
    await assert.rejects(openDB('server'), error => error instanceof DBError && error.code === 'UNSUPPORTED');
    assert.equal(typeof globalThis.window, 'undefined');
    assert.throws(() => import.meta.resolve('react'), { code: 'ERR_MODULE_NOT_FOUND' });
    const kv = createKV();
    const definition = defineStore({
      key: 'counter', version: 1, defaults: () => 0,
      validate: (value) => typeof value === 'number' && Number.isFinite(value),
    });
    const store = createStore(kv, definition);
    let notified = 0;
    const unsubscribe = store.subscribe(() => { notified++; });
    store.update((value) => value + 1);
    assert.equal(store.getSnapshot(), 1);
    assert.equal(notified, 1);
    unsubscribe();
    const reopened = createStore(kv, definition);
    assert.equal(reopened.getSnapshot(), 1);
    assert.equal(reopened.getServerSnapshot(), 0);
    reopened.reset();
    assert.equal(reopened.getSnapshot(), 0);

    // A package rename must still read and update pre-rename persisted records.
    const legacyKey = 'stowtree:["rename-compat","counter"]';
    const persisted = new Map([[legacyKey, JSON.stringify({ __stowtree: 1, version: 1, value: 41 })]]);
    const backend = {
      getItem: (key) => persisted.get(key) ?? null,
      setItem: (key, value) => { persisted.set(key, value); },
      removeItem: (key) => { persisted.delete(key); },
    };
    const durableKV = createKV({ storage: createLocalStorage({
      name: 'rename-compat', storage: backend, validate: () => true,
    }) });
    const legacy = createStore(durableKV, definition);
    assert.equal(legacy.getSnapshot(), 41);
    legacy.update((value) => value + 1);
    assert.deepEqual(JSON.parse(persisted.get(legacyKey)), { __stowtree: 1, version: 1, value: 42 });
    assert.equal(persisted.size, 1);
  `;
  await writeFile(join(directory, "consumer.mjs"), consumer);
  run("node", ["consumer.mjs"], directory);

  // Declarations must work for a consumer without this workspace's @types/node.
  await writeFile(
    join(directory, "consumer.ts"),
    `
    import { createKV, createStore, defineStore } from 'hoke/kv';
    import { openDB, DBNotificationError, type DBRange, type DBListener, type BrowserDB, type DBCollection, type DBEntry, type DBAbortSignal, type DBOperationOptions } from 'hoke/db';
    const signal: DBAbortSignal = {
      aborted: false,
      addEventListener(_type, _listener, _options) {},
      removeEventListener(_type, _listener) {},
    };
    const options: DBOperationOptions = { signal };
    const pending: Promise<BrowserDB> = openDB('typed', { ...options, timeoutMs: 100 });
    void pending;
    async function inspect(db: BrowserDB) {
      await db.get(new Uint8Array(), options);
      await db.put(new Uint8Array(), new Uint8Array(), options);
      await db.delete(new Uint8Array(), options);
      await db.scan(undefined, options);
      const rows: DBEntry[] = await db.scan({ start: { key: new Uint8Array(), inclusive: true }, reverse: true, limit: 5 });
      const collection: DBCollection = db.collection(Uint8Array.of(0, 255));
      const relative: DBEntry[] = await collection.scan({ prefix: Uint8Array.of(1) }, options);
      await collection.get(new Uint8Array(), options);
      await collection.put(new Uint8Array(), new Uint8Array(), options);
      await collection.delete(new Uint8Array(), options);
      // @ts-expect-error Cancellation requires a signal, not a boolean.
      db.get(new Uint8Array(), { signal: true });
      const range: DBRange = { prefix: Uint8Array.of(1) };
      const listener: DBListener = () => {};
      const stops: (() => void)[] = [
        db.subscribe(new Uint8Array(), listener), db.subscribeRange(range, listener),
        collection.subscribe(new Uint8Array(), listener), collection.subscribeRange(range, listener),
      ];
      stops.forEach(stop => stop());
      const failure = new DBNotificationError([new Error('listener')]);
      const committed: true = failure.committed;
      const code: 'SUBSCRIBER_FAILED' = failure.code;
      void committed; void code;
      // @ts-expect-error Ranges do not accept scan pagination options.
      db.subscribeRange({ limit: 1 }, listener);
      // @ts-expect-error Subscription keys are bytes.
      collection.subscribe('key', listener);
      // @ts-expect-error Bounds require an explicit inclusive boolean.
      db.scan({ start: { key: new Uint8Array() } });
      // @ts-expect-error Collection values are bytes.
      collection.put(new Uint8Array(), 'invalid');
      return rows.length + relative.length;
    }
    void inspect;
    const definition = defineStore({
      key: 'typed', version: 1, defaults: () => ({ count: 0 }),
      validate: (value: unknown): value is { count: number } =>
        typeof value === 'object' && value !== null && 'count' in value && typeof value.count === 'number',
    });
    const store = createStore(createKV<unknown>(), definition);
    const count: number = store.getSnapshot().count;
    store.set({ count: count + 1 });
    // @ts-expect-error The store must reject a wrong value type.
    store.set({ count: 'wrong' });
  `,
  );
  await writeFile(
    join(directory, "tsconfig.json"),
    JSON.stringify({
      compilerOptions: {
        strict: true,
        noEmit: true,
        target: "ES2022",
        module: "NodeNext",
        moduleResolution: "NodeNext",
        lib: ["ES2022"],
        types: [],
      },
      files: ["consumer.ts"],
    }),
  );
  run("pnpm", ["exec", "tsc", "-p", join(directory, "tsconfig.json")]);
  const packed = JSON.parse(
    await readFile(join(directory, "node_modules/hoke/package.json"), "utf8"),
  );
  assert.equal(packed.peerDependenciesMeta.react.optional, true);

  const workspace = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
  run(
    "npm",
    [
      "install",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--package-lock=false",
      `react@${workspace.devDependencies.react}`,
      `react-dom@${workspace.devDependencies["react-dom"]}`,
      `@types/react@${workspace.devDependencies["@types/react"]}`,
    ],
    directory,
  );
  await writeFile(
    join(directory, "react-consumer.mjs"),
    `
    import assert from 'node:assert/strict';
    import { createElement } from 'react';
    import { renderToString } from 'react-dom/server';
    import { useValue, useStore } from 'hoke/react';
    import { createKV, createStore, defineStore } from 'hoke/kv';
    const store = createStore(createKV(), defineStore({
      key: 'ssr', version: 1, defaults: () => 3,
      validate: (value) => typeof value === 'number',
    }));
    function Counter() { return createElement('span', null, useValue(store) + useStore(store, value => value)); }
    assert.equal(renderToString(createElement(Counter)), '<span>6</span>');
  `,
  );
  run("node", ["react-consumer.mjs"], directory);
  await writeFile(
    join(directory, "consumer.ts"),
    `
    import { createKV, createStore, defineStore } from 'hoke/kv';
    import { useValue, useStore } from 'hoke/react';
    const store = createStore(createKV<unknown>(), defineStore({
      key: 'react-types', version: 1, defaults: () => ({ count: 0 }),
      validate: (value: unknown): value is { count: number } => typeof value === 'object' && value !== null && 'count' in value && typeof value.count === 'number',
    }));
    function Counter() {
      const count: number = useStore(store, value => value.count);
      const whole: { count: number } = useValue(store);
      return count + whole.count;
    }
    // @ts-expect-error Selector must see the declared store value type.
    useStore(store, value => value.missing);
  `,
  );
  run("pnpm", ["exec", "tsc", "-p", join(directory, "tsconfig.json")]);
  console.log(
    "Tarball OK: exports, files, declarations, KV/store without React, React entry + SSR",
  );
} finally {
  await rm(directory, { recursive: true, force: true });
}
