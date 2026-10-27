import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const directory = await mkdtemp(join(tmpdir(), "stowtree-package-"));
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
  run("pnpm", ["--dir", "packages/stowtree", "pack", "--pack-destination", directory]);
  const archive = (await readdir(directory)).find((name) => name.endsWith(".tgz"));
  assert(archive, "pnpm pack did not produce a tarball");
  const contents = run("tar", ["-tzf", join(directory, archive)]);
  assert(contents.includes("package/dist/kv/index.d.ts"));
  assert(contents.includes("package/dist/react/index.js"));
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
    import { createKV, createStore, defineStore } from 'stowtree/kv';
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
  `;
  await writeFile(join(directory, "consumer.mjs"), consumer);
  run("node", ["consumer.mjs"], directory);

  // Declarations must work for a consumer without this workspace's @types/node.
  await writeFile(
    join(directory, "consumer.ts"),
    `
    import { createKV, createStore, defineStore } from 'stowtree/kv';
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
    await readFile(join(directory, "node_modules/stowtree/package.json"), "utf8"),
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
    import { useValue, useStore } from 'stowtree/react';
    import { createKV, createStore, defineStore } from 'stowtree/kv';
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
    import { createKV, createStore, defineStore } from 'stowtree/kv';
    import { useValue, useStore } from 'stowtree/react';
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
