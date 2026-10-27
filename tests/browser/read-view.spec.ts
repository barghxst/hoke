import { expect, type Page, test } from "@playwright/test";

/** Execute in a dedicated Worker: OPFS sync handles are unavailable on Window. */
async function inWorker(page: Page, body: string): Promise<unknown> {
  await page.goto("/examples/web/");
  return page.evaluate(async (body) => {
    const url = URL.createObjectURL(
      new Blob(
        [
          `import { createSstCandidate, mergeSstCandidates, openSstFile, SstFileError } from ${JSON.stringify(`${location.origin}/packages/hoke/dist/db/sst-files.js`)};
      import { openSstReadView } from ${JSON.stringify(`${location.origin}/packages/hoke/dist/db/read-view.js`)};
      import { WasmWal } from ${JSON.stringify(`${location.origin}/packages/hoke/wasm/hoke_wasm.js`)};
      const bytes = (...values) => new Uint8Array(values);
      const root = await navigator.storage.getDirectory();
      const name = 'sst-test-' + crypto.randomUUID();
      const directory = await root.getDirectoryHandle(name, { create: true });
      let wal; let walHandle;
      const openWal = async () => {
        const file = await directory.getFileHandle('wal', {create:true});
        walHandle = await file.createSyncAccessHandle();
        wal = new WasmWal({getSize:()=>walHandle.getSize(), readAt:(buffer,at)=>walHandle.read(buffer,{at}), writeAt:(buffer,at)=>walHandle.write(buffer,{at}),truncate:size=>walHandle.truncate(size),flush:()=>walHandle.flush()});
        return wal;
      };
      const closeWal = () => {try {wal?.close();} finally {wal?.free();wal=undefined;walHandle?.close();walHandle=undefined;}};
      const read = async name => Array.from(new Uint8Array(await (await (await directory.getFileHandle(name)).getFile()).arrayBuffer()));
      const compact = entries => entries.map(entry => [Array.from(entry.key), entry.value === null ? null : Array.from(entry.value)]);
      try {
        const result = await (async () => { ${body} })();
        closeWal();
        await root.removeEntry(name, { recursive: true });
        self.postMessage({ ok: true, result });
      } catch (error) {
        try {closeWal();} catch {}
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

test("read view overlays live WAL updates, deletions and reinsertion over SST tombstones", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const old = await createSstCandidate(directory,[{key:bytes(1),value:bytes(10)},{key:bytes(2),value:bytes(20)},{key:bytes(3),value:bytes(30)},{key:bytes(4),value:bytes(40)}]);
    const oldName=old.name; old.close();
    const next = await createSstCandidate(directory,[{key:bytes(1),value:bytes(11)},{key:bytes(2),value:null}]);
    const nextName=next.name; next.close();
    const before=await Promise.all([read(oldName),read(nextName)]);
    const view=await openSstReadView(directory,[oldName,nextName]);
    await openWal();
    const initial=compact(view.scan(wal));
    let sourceLocked=false;try{const handle=await (await directory.getFileHandle(oldName)).createSyncAccessHandle();handle.close()}catch{sourceLocked=true}
    wal.put(bytes(1),bytes(12)); wal.delete(bytes(3)); wal.put(bytes(5),bytes());
    const updated=compact(view.scan(wal));
    const hidden=view.get(wal,bytes(3))===undefined && view.get(wal,bytes(2))===undefined;
    wal.put(bytes(3),bytes(31));
    const reinserted=Array.from(view.get(wal,bytes(3)));
    const value=view.get(wal,bytes(1));value.fill(99);
    const copied=Array.from(view.get(wal,bytes(1)));
    view.close(); view.close();
    let closed=false;try {view.scan(wal)}catch {closed=true;}
    wal.put(bytes(6),bytes(60));
    const walUsable=Array.from(wal.get(bytes(6)));
    const after=await Promise.all([read(oldName),read(nextName)]);
    const reopened=await openSstReadView(directory,[oldName,nextName]);
    const restored=compact(reopened.scan(wal));reopened.close();
    return {sourceLocked,initial,updated,hidden,reinserted,copied,closed,walUsable,restored,unchanged:JSON.stringify(before)===JSON.stringify(after)};
  `,
    ),
  ).toEqual({
    sourceLocked: true,
    initial: [
      [[1], [11]],
      [[3], [30]],
      [[4], [40]],
    ],
    updated: [
      [[1], [12]],
      [[4], [40]],
      [[5], []],
    ],
    hidden: true,
    reinserted: [31],
    copied: [12],
    closed: true,
    walUsable: [60],
    restored: [
      [[1], [12]],
      [[3], [31]],
      [[4], [40]],
      [[5], []],
      [[6], [60]],
    ],
    unchanged: true,
  });
});

test("read view applies range, prefix, reverse and live-row limits after resolving tombstones", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const source=await createSstCandidate(directory,[{key:bytes(),value:bytes()},{key:bytes(0),value:bytes(10)},{key:bytes(0,1),value:bytes(11)},{key:bytes(0,2),value:bytes(12)},{key:bytes(1),value:bytes(20)}]);
    const name=source.name;source.close();
    const view=await openSstReadView(directory,[name]);await openWal();
    wal.delete(bytes(0,1));wal.put(bytes(0,3),bytes(13));
    const forward=compact(view.scan(wal,{prefix:bytes(0),limit:2}));
    const reverse=compact(view.scan(wal,{prefix:bytes(0),reverse:true,limit:2}));
    const bounded=compact(view.scan(wal,{start:{key:bytes(0),inclusive:false},end:{key:bytes(1),inclusive:false}}));
    const empty=view.scan(wal,{limit:0});
    const same=view.scan(wal,{start:{key:bytes(0),inclusive:false},end:{key:bytes(0),inclusive:true}});
    const rows=view.scan(wal); rows[0].key.fill(99);rows[1].value.fill(99);
    const copied=compact(view.scan(wal,{limit:2}));
    view.close();return {forward,reverse,bounded,empty,same,copied};
  `,
    ),
  ).toEqual({
    forward: [
      [[0], [10]],
      [[0, 2], [12]],
    ],
    reverse: [
      [[0, 3], [13]],
      [[0, 2], [12]],
    ],
    bounded: [
      [[0, 2], [12]],
      [[0, 3], [13]],
    ],
    empty: [],
    same: [],
    copied: [
      [[], []],
      [[0], [10]],
    ],
  });
});

test("read view validates and copies source names before directory access", async ({ page }) => {
  expect(
    await inWorker(
      page,
      `
    let accesses=0;
    const rejecting={getFileHandle(){accesses++;throw new Error('unexpected I/O')}};
    const inputs=[null,{},['../bad.sst'],['a.sst','a.sst'],Array(65).fill('a.sst'),['a.txt'],[42]];
    const errors=[];for(const names of inputs){try{await openSstReadView(rejecting,names);errors.push('ACK')}catch(error){errors.push(error.name)}}
    const source=await createSstCandidate(directory,[{key:bytes(1),value:bytes(2)}]);const name=source.name;source.close();
    const names=[name];const opening=openSstReadView(directory,names);names[0]='missing.sst';
    const view=await opening;await openWal();const rows=compact(view.scan(wal));view.close();
    const empty=await openSstReadView(directory,[]);wal.put(bytes(3),bytes(4));const walOnly=compact(empty.scan(wal));
    const invalid=[];
    for(const key of [null,[],new Uint8Array(65537)]) {try{empty.get(wal,key);invalid.push('ACK')}catch(error){invalid.push(error.name)}}
    for(const options of [{limit:1001},{prefix:new Uint8Array(65537)},{start:{key:bytes(2),inclusive:true},end:{key:bytes(1),inclusive:true}}]) {try{empty.scan(wal,options);invalid.push('ACK')}catch(error){invalid.push(error.name)}}
    empty.close();
    return {accesses,errors,rows,walOnly,invalid};
  `,
    ),
  ).toEqual({
    accesses: 0,
    errors: Array(7).fill("TypeError"),
    rows: [[[1], [2]]],
    walOnly: [[[3], [4]]],
    invalid: Array(6).fill("TypeError"),
  });
});

test("missing, corrupt and locked sources release earlier read-view handles without creating files", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const source=await createSstCandidate(directory,[{key:bytes(1),value:bytes(2)}]);const name=source.name;source.close();
    const corruptFile=await directory.getFileHandle('bad.sst',{create:true});const corrupt=await corruptFile.createSyncAccessHandle();corrupt.write(bytes(1,2));corrupt.close();
    const errors=[];const released=[];
    for(const failure of ['missing.sst','bad.sst',name]){
      let lock;let names=[name,failure];
      if(failure===name){const locked=await createSstCandidate(directory,[]);const lockedName=locked.name;locked.close();lock=await (await directory.getFileHandle(lockedName)).createSyncAccessHandle();names=[name,lockedName];}
      try {await openSstReadView(directory,names);errors.push('ACK')}catch(error){errors.push(error.name)}finally{lock?.close()}
      const probe=await (await directory.getFileHandle(name)).createSyncAccessHandle();probe.close();released.push(true);
    }
    let missing=false;try{await directory.getFileHandle('missing.sst')}catch(error){missing=error.name==='NotFoundError'}
    return {errors,released,missing};
  `,
    ),
  ).toEqual({ errors: Array(3).fill("SstFileError"), released: [true, true, true], missing: true });
});

test("read view closes every source after a close failure and never closes the borrowed WAL", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const names=[];for(const key of [1,2]){const source=await createSstCandidate(directory,[{key:bytes(key),value:bytes(key)}]);names.push(source.name);source.close()}
    const closed=[];
    const wrapped={async getFileHandle(name,options){const file=await directory.getFileHandle(name,options);return {async createSyncAccessHandle(){const handle=await file.createSyncAccessHandle();return {getSize:()=>handle.getSize(),read:(b,o)=>handle.read(b,o),write:(b,o)=>handle.write(b,o),truncate:n=>handle.truncate(n),flush:()=>handle.flush(),close:()=>{handle.close();closed.push(name);if(name===names[0])throw new Error('close failed')}}}}}};
    const view=await openSstReadView(wrapped,names);await openWal();let failure=false;try{view.close()}catch{failure=true}view.close();
    wal.put(bytes(3),bytes(4));const value=Array.from(wal.get(bytes(3)));
    for(const name of names){const handle=await (await directory.getFileHandle(name)).createSyncAccessHandle();handle.close()}
    return {failure,closedAll:closed.length===2,value};
  `,
    ),
  ).toEqual({ failure: true, closedAll: true, value: [4] });
});

test("concurrent first SST creation and read-view opening share one WASM initialization", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const originalFetch=globalThis.fetch;let wasmFetches=0;
    globalThis.fetch=(input,options)=>{if(String(input instanceof Request ? input.url : input).includes('hoke_wasm_bg.wasm'))wasmFetches++;return originalFetch(input,options)};
    const [candidate,view]=await Promise.all([
      createSstCandidate(directory,[{key:bytes(1),value:bytes(2)}]),
      openSstReadView(directory,[]),
    ]);
    await openWal();wal.put(bytes(3),bytes(4));
    const stored=Array.from(candidate.lookup(bytes(1)));
    const rows=compact(view.scan(wal));
    candidate.close();view.close();
    globalThis.fetch=originalFetch;
    return {wasmFetches,stored,rows};
  `,
    ),
  ).toEqual({ wasmFetches: 1, stored: [2], rows: [[[3], [4]]] });
});
