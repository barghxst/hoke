# hoke

Typed key-value storage, versioned reactive stores, and React hooks.

This is an unpublished early prototype. Memory and localStorage are implemented. A native Rust database prototype lives in crates/hoke-core; it is not part of this npm package. An experimental hoke/db entry point runs the existing WAL-only Engine in WASM/Worker/OPFS. Browser scan and byte collections are available; full browser SST/compaction, React Native, and cross-tab routing remain future work.

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

ESM exports are hoke/kv, hoke/react and experimental hoke/db. The latter exposes async openDB/get/put/delete/scan/close and byte collections with mode wal-v1, byte keys/values and one exclusive owner. Node 24 and React 19.3.x are the current tested development environments. The package remains private while licensing, ownership, and release metadata are pending.

Runtime validators must synchronously return exactly true for accepted values. Promise/thenable results are rejected; native Promise rejections are observed without invoking custom thenable methods. Store defaults, updates and migrations are also synchronous.

Browser DB requires a secure context, Dedicated Worker, synchronous OPFS access handles and served JS/WASM assets. It has no fallback. A timeout or lost worker response may leave a committed mutation; reopen and read before retrying. Full WAL replay is unbounded; SST/batch are absent in this browser slice. Source builds require Rust WASM target and wasm-bindgen-cli0.2.129; the tarball contains the generated assets.

Browser scan supports explicit inclusive/exclusive byte bounds, prefix, reverse and limit0..1000 (default100), with a32MiB response cap. Collection namespaces use versioned length-prefixed byte keys; scans return local keys and share the database lifetime. Worker protocol2 does not change WAL version1.
