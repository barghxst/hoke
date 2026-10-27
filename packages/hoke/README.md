# hoke

Typed key-value storage, versioned reactive stores, and React hooks.

This is an unpublished early prototype. Memory and localStorage are implemented; a database engine, React Native, and cross-tab notifications are planned.

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

Only ESM exports `hoke/kv` and `hoke/react` are available. Node 24 and React 19.3.x are the current tested development environments. The package remains private while licensing, ownership, and release metadata are pending.
