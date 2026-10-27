import { expect, type Page, test } from "@playwright/test";

/** Execute in a dedicated Worker: OPFS sync handles are unavailable on Window. */
async function inWorker(page: Page, body: string): Promise<unknown> {
  await page.goto("/examples/web/");
  return page.evaluate(async (body) => {
    const url = URL.createObjectURL(
      new Blob(
        [
          `import { createSstCandidate, openSstFile, SstFileError } from ${JSON.stringify(`${location.origin}/packages/hoke/dist/db/sst-files.js`)};
      const bytes = (...values) => new Uint8Array(values);
      const root = await navigator.storage.getDirectory();
      const name = 'sst-test-' + crypto.randomUUID();
      const directory = await root.getDirectoryHandle(name, { create: true });
      const read = async name => Array.from(new Uint8Array(await (await (await directory.getFileHandle(name)).getFile()).arrayBuffer()));
      const compact = entries => entries.map(entry => [Array.from(entry.key), entry.value === null ? null : Array.from(entry.value)]);
      try {
        const result = await (async () => { ${body} })();
        await root.removeEntry(name, { recursive: true });
        self.postMessage({ ok: true, result });
      } catch (error) {
        await root.removeEntry(name, { recursive: true }).catch(() => {});
        self.postMessage({ ok: false, error: error.stack || String(error) });
      }`,
        ],
        { type: "text/javascript" },
      ),
    );
    const worker = new Worker(url, { type: "module" });
    try {
      return await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("SST Worker timed out")), 20_000);
        worker.onmessage = (event) => {
          clearTimeout(timer);
          if (event.data.ok) resolve(event.data.result);
          else reject(new Error(event.data.error));
        };
        worker.onerror = (event) => {
          clearTimeout(timer);
          reject(new Error(event.message));
        };
      });
    } finally {
      worker.terminate();
      URL.revokeObjectURL(url);
    }
  }, body);
}

test("internal SST files preserve binary keys, empty values and tombstones across reopen", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const key = bytes(0);
    const value = bytes(0, 255);
    const creating = createSstCandidate(directory, [
      {key: bytes(), value: bytes()},
      {key, value},
      {key: bytes(0, 255), value: null},
      {key: bytes(255), value: bytes(42)},
    ]);
    key.fill(9); value.fill(9);
    const candidate = await creating;
    const name = candidate.name;
    const original = compact(candidate.scan());
    const reverse = compact(candidate.scan({prefix: bytes(0), reverse: true, limit: 1}));
    const bounded = compact(candidate.scan({start: {key: bytes(0), inclusive: false}, end: {key: bytes(255), inclusive: false}}));
    const lookup = [candidate.lookup(bytes(42)) === undefined, candidate.lookup(bytes(0, 255)) === null, Array.from(candidate.lookup(bytes()))];
    candidate.close(); candidate.close();
    let closed = false;
    try {candidate.scan()} catch {closed = true}
    const before = await read(name);
    const reopened = await openSstFile(directory, name);
    const restored = compact(reopened.scan());
    reopened.close();
    const after = await read(name);
    return {original, restored, reverse, bounded, lookup, closed, magic: before.slice(0,8), unchanged: JSON.stringify(before) === JSON.stringify(after)};
  `,
    ),
  ).toEqual({
    original: [
      [[], []],
      [[0], [0, 255]],
      [[0, 255], null],
      [[255], [42]],
    ],
    restored: [
      [[], []],
      [[0], [0, 255]],
      [[0, 255], null],
      [[255], [42]],
    ],
    reverse: [[[0, 255], null]],
    bounded: [[[0, 255], null]],
    lookup: [true, true, []],
    closed: true,
    magic: [83, 84, 79, 87, 83, 83, 84, 0],
    unchanged: true,
  });
});

test("invalid SST candidates never access the caller directory", async ({ page }) => {
  expect(
    await inWorker(
      page,
      `
    let accesses = 0;
    const rejectingDirectory = {getFileHandle() { accesses++; throw new Error('Must not access directory'); }};
    const inputs = [
      new Array(1_000_001),
      [{key: bytes(1), value: bytes()}, {key: bytes(0), value: bytes()}],
      [{key: bytes(1), value: bytes()}, {key: bytes(1), value: null}],
      [{key: new Uint8Array(65537), value: bytes()}],
      [{key: bytes(), value: new Uint8Array(16777217)}],
      [{key: bytes(), value: undefined}],
      [{key: bytes(0), value: new Uint8Array(16777216)}, {key: bytes(1), value: new Uint8Array(16777216)}],
    ];
    const errors = [];
    for (const entries of inputs) {
      try { await createSstCandidate(rejectingDirectory, entries); errors.push('ACK'); }
      catch(error) { errors.push(error.name); }
    }
    try { await openSstFile(rejectingDirectory, '../evil.sst'); errors.push('ACK'); }
    catch(error) { errors.push(error.name); }
    return {accesses, errors};
  `,
    ),
  ).toEqual({ accesses: 0, errors: Array(8).fill("TypeError") });
});

test("candidate collisions never overwrite existing bytes and corrupt open releases its handle", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const existing = await directory.getFileHandle('existing.sst', {create: true});
    const write = await existing.createSyncAccessHandle();
    write.write(bytes(1,2,3)); write.flush(); write.close();
    const before = await read('existing.sst');
    let first = true;
    const collisionDirectory = {async getFileHandle(name, options) {
      if (!options.create && first) {first = false; return existing;}
      return directory.getFileHandle(name, options);
    }};
    const candidate = await createSstCandidate(collisionDirectory, [{key: bytes(1), value: bytes(2)}]);
    candidate.close();
    let rejected = false;
    try {await openSstFile(directory, 'existing.sst');} catch(error) {rejected = error instanceof SstFileError;}
    const released = await existing.createSyncAccessHandle();
    released.close();
    const after = await read('existing.sst');
    return {rejected, unchanged: JSON.stringify(before) === JSON.stringify(after), validCandidate: candidate.name !== 'existing.sst'};
  `,
    ),
  ).toEqual({ rejected: true, unchanged: true, validCandidate: true });
});

test("failed candidate flush reports the orphan name and releases the exclusive handle", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const failingDirectory = {async getFileHandle(name, options) {
      const file = await directory.getFileHandle(name, options);
      return {async createSyncAccessHandle() {
        const handle = await file.createSyncAccessHandle();
        return {
          getSize: () => handle.getSize(),
          read: (buffer, options) => handle.read(buffer, options),
          write: (buffer, options) => handle.write(buffer, options),
          truncate: size => handle.truncate(size),
          flush: () => {handle.flush(); throw new Error('injected flush failure');},
          close: () => handle.close(),
        };
      }};
    }};
    let candidateName;
    let typed = false;
    try {await createSstCandidate(failingDirectory, [{key: bytes(1), value: bytes(2)}]);}
    catch(error) {typed = error instanceof SstFileError; candidateName = error.candidateName;}
    if (!candidateName) throw new Error('Missing orphan name');
    const file = await directory.getFileHandle(candidateName, {create: false});
    const released = await file.createSyncAccessHandle();
    const size = released.getSize(); released.close();
    return {typed, named: candidateName.endsWith('.sst'), retained: size > 0};
  `,
    ),
  ).toEqual({ typed: true, named: true, retained: true });
});
