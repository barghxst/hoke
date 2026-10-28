# hoke

Local storage for applications: TypeScript KV and reactive stores, plus an experimental Rust database through WASM.

This is an unpublished early prototype. Memory and localStorage are implemented. A native Rust database prototype lives in crates/hoke-core; it is not part of this npm package. An experimental hoke/db entry point runs Rust in WASM/Worker/OPFS. Explicit container-v1 supports SST, atomic batches and compaction alongside the default WAL backend. React Native and cross-tab routing remain future work.

KV and Store run entirely in TypeScript. Rust is used by `hoke/db`: it owns WAL records, sorted immutable tables, ordered reads and crash recovery. The browser client sends requests to a Dedicated Worker, which runs the shared Rust engine as WebAssembly against an OPFS file. The native CLI uses the same core with ordinary filesystem adapters. The engine uses its own WAL/SST/container formats; it does not embed SQLite or RocksDB.

```ts
import { createKV, createStore, defineStore } from "hoke/kv";

const definition = defineStore({
  key: "counter",
  version: 1,
  defaults: () => 0,
  validate: (value: unknown): value is number =>
    typeof value === "number" && Number.isFinite(value),
});
const store = createStore(createKV<unknown>(), definition);
store.update((previous) => previous + 1);
```

React is optional for KV. To use hooks, install React 19.3.x and import `useValue` or `useStore` from `hoke/react`.

Use `createLocalStorage({ name, validate })` as the `storage` option of `createKV` for persistence. Validate raw JSON separately from the current store schema so migration inputs can be read. Store migrations are synchronous and keyed by destination version; all steps finish and validate before one final envelope write. Future versions throw instead of downgrading.

Snapshots and supplied values must be treated as immutable. Subscriptions apply only to writes through the same KV instance. `getServerSnapshot()` returns stable defaults; server and client defaults must match during hydration. Updates and migrations receive protected structured clones, so their inputs must be cloneable.

ESM exports are hoke/kv, hoke/react and experimental hoke/db. The latter exposes async openDB/get/put/delete/scan/close and byte collections with default mode wal-v1 or explicit container-v1, byte keys/values and one exclusive owner. Node 24 and React 19.3.x are the current tested development environments. The package remains private while licensing, ownership, and release metadata are pending.

Runtime validators must synchronously return exactly true for accepted values. Promise/thenable results are rejected; native Promise rejections are observed without invoking custom thenable methods. Store defaults, updates and migrations are also synchronous.

Browser DB requires a secure context, Dedicated Worker, synchronous OPFS access handles and served JS/WASM assets. It has no fallback. A timeout or lost worker response may leave a committed mutation; reopen and read before retrying. Default wal-v1 replays the full journal into memory. Container mode reads values from SST and replays only active WAL segments; it remains an experimental append-only file without physical space reclamation. Source builds require Rust WASM target and wasm-bindgen-cli0.2.129; the tarball contains the generated assets.

Browser scan supports explicit inclusive/exclusive byte bounds, prefix, reverse and limit0..1000 (default100), with a32MiB response cap. Collection namespaces use versioned length-prefixed byte keys; scans return local keys and share the database lifetime. Worker protocol3 is versioned separately from durable WAL/container formats.

Browser DB and collections support subscribe(key, listener) and subscribeRange({start,end,prefix}, listener). They synchronously invalidate after confirmed put/delete ACKs, with no initial/read/replay event. Callback failures reject the already committed mutation with DBNotificationError (AggregateError, committed:true, code:SUBSCRIBER_FAILED); the connection stays usable. Listeners must be synchronous. close/transport failure remove subscriptions; reopen requires registration again. Notifications are local to one DB connection and its collections.

Browser cancellation accepts a structural DBAbortSignal (aborted:boolean plus addEventListener/removeEventListener for "abort"); native AbortController.signal is compatible, without requiring global AbortSignal or DOM consumer types. DBOperationOptions contains signal. Pass options to get(key,options?), put(key,value,options?), delete(key,options?) and scan(scanOptions?,operationOptions?), including collections. openDB(name,{signal,timeoutMs}) uses the signal only until its open ACK; close() has no signal argument.

A valid pre-aborted request rejects with DBError code ABORTED before dispatch and leaves the connection and other requests usable; pre-aborted open creates no Worker. Abort after dispatch terminates the entire Worker, removes subscriptions and rejects every pending request with ABORTED. A mutation may already be committed: there is no rollback or automatic retry. Reopen and inspect saved state before deciding whether to write again. Listeners detach before postcommit subscriber callbacks, so aborting a mutation's own signal from its callback cannot undo an ACK or terminate the healthy connection. Arguments are validated before cancellation; invalid signals or registration failures reject with INVALID_ARGUMENT before dispatch.

Container mode is explicit; switching modes never imports or deletes an existing WAL database:

```ts
import { openDB } from "hoke/db";

const bytes = (text: string) => new TextEncoder().encode(text);
const db = await openDB("settings", { mode: "container-v1" });
try {
  await db.writeBatch([
    { type: "put", key: bytes("theme"), value: bytes("dark") },
    { type: "delete", key: bytes("old-setting") },
  ]);
  await db.flush();
  await db.compact();
  const stats = await db.stats();
  console.log(stats.generation.toString(), stats.tableCount);
} finally {
  await db.close();
}
```

Collection writeBatch accepts local keys, encoded into its namespace; flush/compact/stats apply to the whole BrowserDB. writeBatch/flush/compact/stats accept optional DBOperationOptions with signal and reject UNSUPPORTED in wal-v1. Batch limits are1024 operations and32MiB key/value payload; repeated keys execute in order, the last operation wins. Subscriptions invalidate once per matching registration for the whole successful batch, not once per row; a callback error still reports committed:true. Empty batches perform no commit or notification. Flush and compaction do not notify subscribers. Stats returns generation:bigint and numeric tableCount, walSegments, memtableEntries and memtableBytes; stringify generation explicitly when serializing to JSON.

Container limits include64 active SST,64 active WAL segments,32MiB dirty payload and256MiB staged SST output. Automatic preflush bounds dirty state; compact is required at the table cap. Compaction removes old versions from active state but does not shrink the append-only file. The two commit flush barriers and root recovery do not prove safety under physical power loss, browser eviction or origin deletion.
