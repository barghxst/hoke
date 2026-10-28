import { expect, type Page, test } from "@playwright/test";

async function inWorker(page: Page, body: string): Promise<unknown> {
  await page.goto("/examples/web/");
  return page.evaluate(async (body) => {
    const url = URL.createObjectURL(
      new Blob(
        [
          `
      import {openContainerDatabase} from ${JSON.stringify(`${location.origin}/packages/hoke/dist/db/container-db.js`)};
      import {openContainerJournal} from ${JSON.stringify(`${location.origin}/packages/hoke/dist/db/container-journal.js`)};
      const bytes=(...v)=>new Uint8Array(v);
      const enc=s=>new TextEncoder().encode(s);
      const dec=v=>v===undefined?null:new TextDecoder().decode(v);
      const rows=db=>db.scan().map(({key,value})=>[dec(key),dec(value)]);
      const root=await navigator.storage.getDirectory();
      const name='container-db-test-'+crypto.randomUUID();
      const directory=await root.getDirectoryHandle(name,{create:true});
      const readFile=async name=>new Uint8Array(await (await (await directory.getFileHandle(name)).getFile()).arrayBuffer());
      try {
        const result=await (async()=>{${body}})();
        await root.removeEntry(name,{recursive:true}); postMessage({ok:true,result});
      } catch(error) {
        await root.removeEntry(name,{recursive:true}).catch(()=>{});
        postMessage({ok:false,error:error.stack||String(error)});
      }
    `,
        ],
        { type: "text/javascript" },
      ),
    );
    const worker = new Worker(url, { type: "module" });
    try {
      return await new Promise((resolve, reject) => {
        const timeout = setTimeout(
          () => reject(new Error("Container DB Worker timed out")),
          20_000,
        );
        worker.onmessage = ({ data }) => {
          clearTimeout(timeout);
          data.ok ? resolve(data.result) : reject(new Error(data.error));
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
  }, body);
}

test("container database writes atomic batches, flushes and compacts before OPFS reopen", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    let db=await openContainerDatabase(directory,'db.hkc',{create:true});
    db.put(enc('theme'),enc('dark')); db.put(enc('count'),enc('7'));
    db.writeBatch([
      {type:'put',key:enc('dup'),value:enc('first')},
      {type:'delete',key:enc('theme')},
      {type:'put',key:enc('dup'),value:enc('last')},
      {type:'delete',key:enc('ghost')},
    ]);
    db.flush(); const flushed=db.stats();
    db.put(enc('theme'),enc('light')); db.delete(enc('count')); db.flush();
    const before=rows(db); const multiple=db.stats().tableCount;
    db.compact(); const compacted=db.stats();
    db.close(); db.close(); let closed=false; try{db.get(enc('theme'));}catch{closed=true;}
    db=await openContainerDatabase(directory,'db.hkc');
    const after=rows(db); const absent=db.get(enc('ghost'))===undefined&&db.get(enc('count'))===undefined;
    const restored=db.stats(); db.close();
    return {before,after,absent,closed,multiple,flushed:[flushed.tableCount,flushed.walSegments,flushed.memtableEntries],compacted:[compacted.tableCount,compacted.walSegments,compacted.memtableEntries],restored:[restored.tableCount,restored.walSegments,restored.memtableEntries]};
  `,
    ),
  ).toEqual({
    before: [
      ["dup", "last"],
      ["theme", "light"],
    ],
    after: [
      ["dup", "last"],
      ["theme", "light"],
    ],
    absent: true,
    closed: true,
    multiple: 2,
    flushed: [1, 0, 0],
    compacted: [1, 0, 0],
    restored: [1, 0, 0],
  });
});

test("container database resolves binary prefix and reverse scans across SST and WAL", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    let db=await openContainerDatabase(directory,'db.hkc',{create:true});
    db.put(bytes(),bytes()); db.put(bytes(0),bytes(7)); db.put(bytes(0,255),bytes()); db.put(bytes(255),bytes(1)); db.flush();
    db.delete(bytes(0)); db.put(bytes(0,255),bytes(2)); db.put(bytes(0,1),bytes(3));
    const compact=rows=>rows.map(({key,value})=>[Array.from(key),Array.from(value)]);
    const reverse=compact(db.scan({prefix:bytes(0),reverse:true,limit:1}));
    const bounded=compact(db.scan({start:{key:bytes(0),inclusive:false},end:{key:bytes(255),inclusive:false}}));
    const deleted=db.get(bytes(0))===undefined; const empty=Array.from(db.get(bytes()));
    const before=compact(db.scan()); db.close();
    db=await openContainerDatabase(directory,'db.hkc'); const after=compact(db.scan()); db.close();
    return {reverse,bounded,deleted,empty,unchanged:JSON.stringify(before)===JSON.stringify(after)};
  `,
    ),
  ).toEqual({
    reverse: [[[0, 255], [2]]],
    bounded: [
      [[0, 1], [3]],
      [[0, 255], [2]],
    ],
    deleted: true,
    empty: [],
    unchanged: true,
  });
});

test("container database rejects invalid input without a new root or hidden creation", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const options={create:false}; const pending=openContainerDatabase(directory,'missing.hkc',options); options.create=true;
    let missing=false; try{await pending;}catch{missing=true;}
    let absent=false; try{await directory.getFileHandle('missing.hkc');}catch(error){absent=error.name==='NotFoundError';}
    const db=await openContainerDatabase(directory,'db.hkc',{create:true}); const generation=db.stats().generation;
    let rejected=0;
    for(const action of [
      ()=>db.put(new Uint8Array(65537),bytes()),
      ()=>db.put(bytes(),new Uint8Array(16777217)),
      ()=>db.writeBatch(new Array(1025)),
      ()=>db.writeBatch([{type:'delete',key:'wrong'}]),
      ()=>db.scan({start:{key:bytes(2),inclusive:true},end:{key:bytes(1),inclusive:false}}),
    ]) {try{action();}catch{rejected++;}}
    db.writeBatch([]); db.flush(); db.compact(); const stable=generation===db.stats().generation;
    const healthy=!db.isPoisoned(); db.put(bytes(1),bytes(2)); const value=Array.from(db.get(bytes(1))); db.close();
    return {missing,absent,rejected,stable,healthy,value};
  `,
    ),
  ).toEqual({ missing: true, absent: true, rejected: 5, stable: true, healthy: true, value: [2] });
});

test("container database validates inner WAL before repairing incomplete outer tail", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    const journal=await openContainerJournal(directory,'db.hkc',{create:true});
    const bad=journal.append('wal',bytes(1,2,3)); journal.commit([], [bad]); journal.close();
    const file=await directory.getFileHandle('db.hkc'); const h=await file.createSyncAccessHandle();
    h.write(bytes(72,75),{at:h.getSize()}); h.flush(); h.close();
    const before=await readFile('db.hkc'); let rejected=false;
    try{await openContainerDatabase(directory,'db.hkc');}catch{rejected=true;}
    const after=await readFile('db.hkc'); const released=await file.createSyncAccessHandle(); released.close();
    return {rejected,unchanged:JSON.stringify(Array.from(before))===JSON.stringify(Array.from(after))};
  `,
    ),
  ).toEqual({ rejected: true, unchanged: true });
});

test("container database recovers the complete batch after a lost final flush acknowledgement", async ({
  page,
}) => {
  expect(
    await inWorker(
      page,
      `
    let flushes=0, failAt=-1;
    const failing={async getFileHandle(name,options){
      const file=await directory.getFileHandle(name,options);
      return {async createSyncAccessHandle(){
        const h=await file.createSyncAccessHandle();
        return {getSize:()=>h.getSize(),read:(b,o)=>h.read(b,o),write:(b,o)=>h.write(b,o),truncate:n=>h.truncate(n),close:()=>h.close(),flush:()=>{h.flush();if(++flushes===failAt)throw new Error('lost batch acknowledgement');}};
      }};
    }};
    let db=await openContainerDatabase(failing,'db.hkc',{create:true}); db.put(enc('old'),enc('before'));
    failAt=flushes+2; let failed=false;
    try{db.writeBatch([{type:'delete',key:enc('old')},{type:'put',key:enc('a'),value:enc('1')},{type:'put',key:enc('b'),value:enc('2')}]);}catch{failed=true;}
    const poisoned=db.isPoisoned(); let blocked=false;try{db.scan();}catch{blocked=true;} db.close();
    db=await openContainerDatabase(directory,'db.hkc');const restored=rows(db); db.close();
    return {failed,poisoned,blocked,restored};
  `,
    ),
  ).toEqual({
    failed: true,
    poisoned: true,
    blocked: true,
    restored: [
      ["a", "1"],
      ["b", "2"],
    ],
  });
});
