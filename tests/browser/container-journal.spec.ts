import { expect, type Page, test } from "@playwright/test";

async function inWorker(page: Page, body: string): Promise<unknown> {
  await page.goto("/examples/web/");
  return page.evaluate(async (body) => {
    const url = URL.createObjectURL(
      new Blob(
        [
          `
      import { openContainerJournal } from ${JSON.stringify(`${location.origin}/packages/hoke/dist/db/container-journal.js`)};
      const bytes = (...values) => new Uint8Array(values);
      const root = await navigator.storage.getDirectory();
      const name = 'container-test-' + crypto.randomUUID();
      const directory = await root.getDirectoryHandle(name, {create: true});
      const readFile = async name => new Uint8Array(await (await (await directory.getFileHandle(name)).getFile()).arrayBuffer());
      try {
        const result = await (async () => {${body}})();
        await root.removeEntry(name, {recursive: true});
        postMessage({ok: true, result});
      } catch(error) {
        await root.removeEntry(name, {recursive: true}).catch(() => {});
        postMessage({ok: false, error: error.stack || String(error)});
      }
    `,
        ],
        { type: "text/javascript" },
      ),
    );
    const worker = new Worker(url, { type: "module" });
    try {
      return await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("Container Worker timed out")), 20_000);
        worker.onmessage = ({ data }) => {
          clearTimeout(timer);
          data.ok ? resolve(data.result) : reject(new Error(data.error));
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

test("container commits explicit roots and preserves staged segments across OPFS reopen", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    let journal = await openContainerJournal(directory, 'db.hkc', {create: true});
    const initial = journal.root().length;
    const wal = journal.append('wal', bytes(0,255));
    const table = journal.append('sst', bytes(1,2));
    journal.commit([table], [wal]);
    const committed = Array.from(journal.root());
    const staged = journal.append('wal', bytes(42));
    journal.close(); journal.close();
    journal = await openContainerJournal(directory, 'db.hkc');
    const stable = JSON.stringify(Array.from(journal.root())) === JSON.stringify(committed);
    const values = [Array.from(journal.read(table)), Array.from(journal.read(wal)), Array.from(journal.read(staged))];
    const recovery = journal.recovery();
    journal.commit([table], [wal, staged]);
    const generation = Number(new DataView(journal.root().buffer).getBigUint64(0, true));
    journal.close();
    return {initial, stable, values, generation, tail: String(recovery.discardedTailBytes), magic: Array.from((await readFile('db.hkc')).slice(0,8))};
  `,
    ),
  ).toEqual({
    initial: 0,
    stable: true,
    values: [[1, 2], [0, 255], [42]],
    generation: 2,
    tail: "0",
    magic: [72, 79, 75, 69, 67, 78, 84, 0],
  });
});

test("container discards incomplete EOF only and keeps committed root", async ({ page }) => {
  expect(
    await inWorker(
      page,
      `
    let journal = await openContainerJournal(directory, 'db.hkc', {create: true});
    const segment = journal.append('wal', bytes(7));
    journal.commit([], [segment]);
    const saved = Array.from(journal.root());
    journal.close();
    const file = await directory.getFileHandle('db.hkc');
    const handle = await file.createSyncAccessHandle();
    const originalSize = handle.getSize();
    handle.write(bytes(72,75), {at: originalSize}); handle.flush(); handle.close();
    journal = await openContainerJournal(directory, 'db.hkc');
    const tail = String(journal.recovery().discardedTailBytes);
    const stable = JSON.stringify(saved) === JSON.stringify(Array.from(journal.root()));
    const value = Array.from(journal.read(segment));
    journal.close();
    return {tail, stable, value, restoredSize: (await readFile('db.hkc')).length === originalSize};
  `,
    ),
  ).toEqual({ tail: "2", stable: true, value: [7], restoredSize: true });
});

test("container corruption fails without fallback or file repair and releases ownership", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const journal = await openContainerJournal(directory, 'db.hkc', {create: true});
    const first = journal.append('wal', bytes(1)); journal.commit([], [first]);
    const second = journal.append('wal', bytes(2)); journal.commit([], [first, second]); journal.close();
    const file = await directory.getFileHandle('db.hkc');
    const handle = await file.createSyncAccessHandle();
    const end = handle.getSize()-1;
    const byte = bytes(0); handle.read(byte,{at:end}); byte[0]^=1;
    handle.write(byte,{at:end}); handle.flush(); handle.close();
    const before = await readFile('db.hkc');
    let rejected = false;
    try {await openContainerJournal(directory, 'db.hkc');} catch {rejected = true;}
    const after = await readFile('db.hkc');
    const released = await file.createSyncAccessHandle(); released.close();
    return {rejected, unchanged: JSON.stringify(Array.from(before))===JSON.stringify(Array.from(after))};
  `,
    ),
  ).toEqual({ rejected: true, unchanged: true });
});

test("container validates opens before storage and preserves exclusive ownership", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    let calls = 0;
    const fake = {getFileHandle() {calls++; throw new Error('unexpected storage access');}};
    let invalid = 0;
    for (const [name, options] of [['../db.hkc',{}],['old.wal',{}],['db.hkc',{create:1}]]) {
      try {await openContainerJournal(fake,name,options);} catch(error) {if(error instanceof TypeError) invalid++;}
    }
    const options = {create:false};
    const opening = openContainerJournal(directory,'missing.hkc',options); options.create=true;
    let missing = false; try {await opening;} catch {missing=true;}
    let absent = false; try {await directory.getFileHandle('missing.hkc');} catch(error) {absent=error.name==='NotFoundError';}
    const journal = await openContainerJournal(directory,'db.hkc',{create:true});
    let locked = false; try {await openContainerJournal(directory,'db.hkc');} catch {locked=true;}
    journal.close();
    const reopened = await openContainerJournal(directory,'db.hkc'); reopened.close();
    return {calls,invalid,missing,absent,locked};
  `,
    ),
  ).toEqual({ calls: 0, invalid: 3, missing: true, absent: true, locked: true });
});

test("container commit may survive a failed final flush acknowledgement", async ({ page }) => {
  expect(
    await inWorker(
      page,
      `
    let flushes = 0;
    const failing = {async getFileHandle(name,options) {
      const file = await directory.getFileHandle(name,options);
      return {async createSyncAccessHandle() {
        const h=await file.createSyncAccessHandle();
        return {getSize:()=>h.getSize(),read:(b,o)=>h.read(b,o),write:(b,o)=>h.write(b,o),truncate:n=>h.truncate(n),close:()=>h.close(),flush:()=>{h.flush(); if(++flushes===3) throw new Error('lost final flush acknowledgement');}};
      }};
    }};
    let journal = await openContainerJournal(failing,'db.hkc',{create:true});
    const segment = journal.append('wal',bytes(9));
    let failed = false; try {journal.commit([], [segment]);} catch {failed=true;}
    const poisoned=journal.isPoisoned();
    const oldRoot=journal.root().length;
    let blocked = false; try {journal.append('wal',bytes(10));} catch {blocked=true;}
    journal.close();
    journal=await openContainerJournal(directory,'db.hkc');
    const generation=Number(new DataView(journal.root().buffer).getBigUint64(0,true));
    const value=Array.from(journal.read(segment)); journal.close();
    return {failed,poisoned,oldRoot,blocked,generation,value};
  `,
    ),
  ).toEqual({ failed: true, poisoned: true, oldRoot: 0, blocked: true, generation: 1, value: [9] });
});
