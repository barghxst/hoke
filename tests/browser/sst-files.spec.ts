import { expect, type Page, test } from "@playwright/test";

/** Execute in a dedicated Worker: OPFS sync handles are unavailable on Window. */
async function inWorker(page: Page, body: string): Promise<unknown> {
  await page.goto("/examples/web/");
  return page.evaluate(async (body) => {
    const url = URL.createObjectURL(
      new Blob(
        [
          `import { createSstCandidate, mergeSstCandidates, openSstFile, SstFileError } from ${JSON.stringify(`${location.origin}/packages/hoke/dist/db/sst-files.js`)};
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

test("SST merge uses newest versions, preserves tombstones and leaves sources immutable", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const older = await createSstCandidate(directory, [
      {key: bytes(), value: bytes(1)}, {key: bytes(0), value: bytes(2)},
      {key: bytes(1), value: null}, {key: bytes(255), value: bytes(3)},
    ]);
    const olderName = older.name; older.close();
    const newer = await createSstCandidate(directory, [
      {key: bytes(), value: null}, {key: bytes(0), value: bytes()},
      {key: bytes(1), value: bytes(4)},
    ]);
    const newerName = newer.name; newer.close();
    const before = await Promise.all([read(olderName), read(newerName)]);
    const names = [olderName, newerName];
    const pending = mergeSstCandidates(directory, names);
    names.reverse();
    const merged = await pending;
    const name = merged.name;
    const rows = compact(merged.scan());
    merged.close();
    const reopened = await openSstFile(directory, name);
    const restored = compact(reopened.scan()); reopened.close();
    const after = await Promise.all([read(olderName), read(newerName)]);
    // Reacquisition also proves that merge released both exclusive input handles.
    for (const source of [olderName, newerName]) {
      const opened = await openSstFile(directory, source); opened.close();
    }
    return {rows, restored, unchanged: JSON.stringify(before) === JSON.stringify(after), fresh: !names.includes(name)};
  `,
    ),
  ).toEqual({
    rows: [
      [[], null],
      [[0], []],
      [[1], [4]],
      [[255], [3]],
    ],
    restored: [
      [[], null],
      [[0], []],
      [[1], [4]],
      [[255], [3]],
    ],
    unchanged: true,
    fresh: true,
  });
});

test("SST merge validates names, duplicates and count before directory access", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    let accesses = 0;
    const forbidden = {getFileHandle() { accesses++; throw new Error('Unexpected I/O'); }};
    const invalid = [['../a.sst'], ['a.wal'], ['a.sst', 'a.sst'], Array(65).fill('a.sst'), [null], 'a.sst'];
    const errors = [];
    for (const names of invalid) {
      try { await mergeSstCandidates(forbidden, names); errors.push('ACK'); }
      catch (error) { errors.push(error.name); }
    }
    return {accesses, errors};
  `,
    ),
  ).toEqual({ accesses: 0, errors: Array(6).fill("TypeError") });
});

test("empty SST merge builds an empty reusable candidate", async ({ page }) => {
  expect(
    await inWorker(
      page,
      `
    const merged = await mergeSstCandidates(directory, []);
    const name = merged.name;
    const empty = merged.scan().length === 0 && merged.lookup(bytes()) === undefined;
    merged.close();
    const reopened = await openSstFile(directory, name);
    const restored = reopened.scan().length; reopened.close();
    return {empty, restored};
  `,
    ),
  ).toEqual({ empty: true, restored: 0 });
});

test("missing and locked merge sources create no candidate and release earlier inputs", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const first = await createSstCandidate(directory, []);
    const firstName = first.name; first.close();
    const second = await createSstCandidate(directory, []);
    const secondName = second.name;
    let created = 0;
    const watched = {getFileHandle(name, options) {
      if (options.create) created++;
      return directory.getFileHandle(name, options);
    }};
    const failures = [];
    for (const names of [[firstName, 'missing.sst'], [firstName, secondName]]) {
      try { await mergeSstCandidates(watched, names); failures.push('ACK'); }
      catch(error) { failures.push(error instanceof SstFileError && error.candidateName === undefined); }
      const released = await openSstFile(directory, firstName); released.close();
    }
    second.close();
    return {created, failures};
  `,
    ),
  ).toEqual({ created: 0, failures: [true, true] });
});

test("corrupt SST merge reports an empty orphan and never changes either source", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const valid = await createSstCandidate(directory, [{key: bytes(1), value: bytes(2)}]);
    const validName = valid.name; valid.close();
    const broken = await directory.getFileHandle('broken.sst', {create: true});
    const writer = await broken.createSyncAccessHandle(); writer.write(bytes(1,2,3)); writer.flush(); writer.close();
    const before = await Promise.all([read(validName), read('broken.sst')]);
    let orphan;
    try { await mergeSstCandidates(directory, [validName, 'broken.sst']); }
    catch(error) { if (!(error instanceof SstFileError)) throw error; orphan = error.candidateName; }
    if (!orphan) throw new Error('Missing merge orphan name');
    const destination = await (await directory.getFileHandle(orphan)).createSyncAccessHandle();
    const size = destination.getSize(); destination.close();
    for (const source of [validName, 'broken.sst']) {
      const handle = await (await directory.getFileHandle(source)).createSyncAccessHandle(); handle.close();
    }
    const after = await Promise.all([read(validName), read('broken.sst')]);
    return {size, unchanged: JSON.stringify(before) === JSON.stringify(after)};
  `,
    ),
  ).toEqual({ size: 0, unchanged: true });
});

test("merge source close failure rejects, releases output and preserves the first failure", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const source = await createSstCandidate(directory, [{key: bytes(1), value: bytes(2)}]);
    const sourceName = source.name; source.close();
    const injected = new Error('source close failed');
    const failing = {async getFileHandle(name, options) {
      const file = await directory.getFileHandle(name, options);
      return {async createSyncAccessHandle() {
        const handle = await file.createSyncAccessHandle();
        return {
          getSize: () => handle.getSize(), read: (buffer, options) => handle.read(buffer, options),
          write: (buffer, options) => handle.write(buffer, options), truncate: size => handle.truncate(size),
          flush: () => handle.flush(), close: () => {handle.close(); if (name === sourceName) throw injected;},
        };
      }};
    }};
    let orphan, preserved = false;
    try { await mergeSstCandidates(failing, [sourceName]); }
    catch(error) { orphan = error.candidateName; preserved = error.cause === injected; }
    if (!orphan) throw new Error('Missing merge candidate name');
    const result = await openSstFile(directory, orphan);
    const value = Array.from(result.lookup(bytes(1))); result.close();
    const reopened = await openSstFile(directory, sourceName); reopened.close();
    return {preserved, value};
  `,
    ),
  ).toEqual({ preserved: true, value: [2] });
});

test("SST merge streams more than the scan row limit", async ({ page }) => {
  expect(
    await inWorker(
      page,
      `
    const names = [];
    for (let group = 0; group < 2; group++) {
      const source = await createSstCandidate(directory, Array.from({length: 700}, (_, i) => {
        const number = group * 700 + i;
        return {key: bytes(number >> 8, number & 255), value: bytes(group)};
      }));
      names.push(source.name); source.close();
    }
    const result = await mergeSstCandidates(directory, names);
    const first = result.scan({limit: 1000}).length;
    const last = Array.from(result.lookup(bytes(1399 >> 8, 1399 & 255)));
    const tail = result.scan({start: {key: bytes(1000 >> 8, 1000 & 255), inclusive: true}, limit: 1000}).length;
    result.close();
    return {first, tail, last};
  `,
    ),
  ).toEqual({ first: 1000, tail: 400, last: [1] });
});

test("merge flush failure retains named orphan and releases every exclusive handle", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const source = await createSstCandidate(directory, [{key: bytes(1), value: bytes(2)}]);
    const sourceName = source.name; source.close();
    const injected = new Error('merge flush failed');
    const failing = {async getFileHandle(name, options) {
      const file = await directory.getFileHandle(name, options);
      return {async createSyncAccessHandle() {
        const handle = await file.createSyncAccessHandle();
        return {
          getSize: () => handle.getSize(), read: (buffer, options) => handle.read(buffer, options),
          write: (buffer, options) => handle.write(buffer, options), truncate: size => handle.truncate(size),
          flush: () => {if (name !== sourceName) throw injected; handle.flush();}, close: () => handle.close(),
        };
      }};
    }};
    let orphan, rejected = false;
    try { await mergeSstCandidates(failing, [sourceName]); }
    catch(error) { orphan = error.candidateName; rejected = error instanceof SstFileError; }
    if (!orphan) throw new Error('Missing merge orphan name');
    for (const name of [sourceName, orphan]) {
      const handle = await (await directory.getFileHandle(name)).createSyncAccessHandle(); handle.close();
    }
    const reopened = await openSstFile(directory, sourceName);
    const value = Array.from(reopened.lookup(bytes(1))); reopened.close();
    return {rejected, value};
  `,
    ),
  ).toEqual({ rejected: true, value: [2] });
});

test("candidate creation with an unknown outcome reports its name and releases merge inputs", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const source = await createSstCandidate(directory, [{key: bytes(1), value: bytes(2)}]);
    const sourceName = source.name; source.close();
    const injected = new Error('create reply failed');
    const failing = {async getFileHandle(name, options) {
      const file = await directory.getFileHandle(name, options);
      if (options.create) throw injected;
      return file;
    }};
    let orphan, preserved = false;
    try { await mergeSstCandidates(failing, [sourceName]); }
    catch(error) { orphan = error.candidateName; preserved = error.cause === injected; }
    if (!orphan) throw new Error('Missing uncertain candidate name');
    const handle = await (await directory.getFileHandle(orphan, {create: false})).createSyncAccessHandle();
    const size = handle.getSize(); handle.close();
    const reopened = await openSstFile(directory, sourceName); reopened.close();
    return {preserved, size};
  `,
    ),
  ).toEqual({ preserved: true, size: 0 });
});
