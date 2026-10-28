# Changelog

## Unreleased — experimental Hoke

- Подготовлен SST file bridge: существующий Rust SST v1 codec доступен WASM и внутреннему OPFS слою, с tombstones, bounded reads и cleanup. Browser openDB пока WAL-only; публикация SST не включена.

- Добавлено потоковое объединение SST-кандидатов через Rust/WASM/OPFS: newest wins, tombstones сохранены, исходники не удаляются; authoritative publication ещё не включена.

- Добавлен внутренний live read view поверх WAL и SST: единые Rust правила версий, bounded scan, tombstone masking и общее WASM initialization.

- Реализован новый append-only container journal v1: CRC framing/root refs, two-barrier commit, strict recovery, poisoned writer и внутренний OPFS `.hkc` adapter. Старые WAL/SST форматы и openDB mode сохранены.

- Реализован контейнерный KV coordinator через Rust/WASM/внутренний OPFS: atomic batch, get/scan, SST flush, full compaction и bounded source views. Добавлен native CLI example; публичный openDB пока WAL-only.

### JavaScript package

- Synchronous KV with memory and explicit localStorage adapters.
- Synchronous runtime validation (exact true), isolated namespaces, stable snapshots and storage errors with causes.
- Rejected Promise results are handled without unhandled rejections; custom thenable methods are not executed.
- Versioned stores, protected synchronous updates, defaults, subscriptions and reset.
- Migration chains with a single final write, preserved originals on pre-commit failure and explicit future-version errors.
- Optional React entry point with selectors, equality comparison and SSR snapshots.
- Real-browser settings demo with explicit v1 preview/migration, v3 protection and recovery by explicit clear, including malformed JSON.
- Package renamed to hoke; persisted localStorage prefix and envelope marker retained for compatibility.

### Native Rust prototype

- Byte Storage contract, locked native files and explicit volatile memory storage.
- Binary WAL v1 with CRC32, bounded frames, replay and incomplete-tail recovery.
- Indexed SSTables, checksummed manifest, exclusive ownership and synchronized publication.
- Ordered scans with byte ranges, prefix, reverse, limits, newest-wins resolution and tombstones.
- Manual compaction/GC and atomic write batch via SST/manifest publication.
- Optional payload/WAL pre-write thresholds with manual defaults.
- Fixed relative-directory ownership across process cwd changes; acknowledged writes stay in the original database.
- Safe pre-I/O capacity failures remain retryable; uncertain I/O outcomes still poison writes until reopen.
- Reproducible release benchmarks with raw samples, environment metadata and independent replicas.

### Verification

- Biome, TypeScript, installed tarball consumers, optional React/SSR and JS tests.
- Rust model/fault tests and real process-kill recovery tests on Linux; tested Rust toolchains 1.89.0 and 1.97.1.
- Playwright browser scenarios for built module loading, persistence/reload, migrations, reset/clear and unavailable storage.
- Python benchmark protocol/statistics tests. CI jobs prepared; external CI execution pending.

The npm package currently ships KV/store/React. Rust Database is a separate native prototype; the experimental browser WAL-only hoke/db binding now uses WASM/Worker/OPFS. Full browser multi-file DB, multi-tab routing, React Native and devtools are future stages. Native batch uses manifest commits, not WAL group commit. Tests do not prove safety under physical power loss or production readiness.

Publication is pending: license, npm/GitHub ownership and final version/metadata. No package has been published.

### Experimental browser WAL (C1)

- hoke-wasm with the existing Rust WAL Engine and checked synchronous byte adapter.
- hoke/db async openDB/get/put/delete/close, versioned protocol, copies, timeout and terminal transport handling.
- Dedicated Worker with real exclusive OPFS sync handles, flush-before-ACK and explicit errors without fallback.
- Browser reload/recovery/lock/corruption/quota-style fault tests and a runnable OPFS demo.
- Generated JS/WASM included in tarball; KV/React imports stay independent of browser globals and WASM initialization.
- No browser SST/scan/compaction/batch yet; full stage C remains incomplete.

### Browser scans and byte collections (C2)

- Bounded Rust WAL scans using native byte bounds/prefix/reverse/tombstone semantics, without storage I/O or full-map copies.
- BrowserDB.scan with100 default/1000 maximum rows and32MiB packet budget; explicit errors without partial results or poisoned state.
- Strict client packet/order/range validation and Worker protocol2, retaining durable WAL v1.
- Byte collections with versioned length-prefixed namespaces, relative scan keys and shared DB lifecycle.
- OPFS demo list controls and native/WASM/client/Chrome model, budget, protocol and reload coverage.

### Commit subscriptions (C3)

- BrowserDB/DBCollection exact-key and range subscriptions with copied byte filters.
- Local invalidations only after successful put/delete ACKs, before the mutation Promise settles.
- Postcommit callback errors reported as DBNotificationError/AggregateError with committed:true; asynchronous listener results rejected without unhandled native Promise rejection.
- Independent registration order, unsubscribe during dispatch and automatic cleanup at close/transport failure.
- Real OPFS successful/failing flush coverage and demo prefix subscription counter. Durable WAL and Worker protocol unchanged.

### Browser cancellation (C4)

- Public structural DBAbortSignal/DBOperationOptions, compatible with native AbortController and consumers without DOM types.
- Optional signals for openDB, get/put/delete and separate operation options for scan; byte collections forward the same options. Open signals apply only until open ACK.
- Valid pre-aborted requests and registration races reject only the unsent request with ABORTED; pre-aborted open creates no Worker.
- Abort after dispatch terminates the whole Worker, clears subscriptions and rejects all pending work with ABORTED. Mutation outcomes may be committed; no rollback, retries or automatic resend.
- Centralized signal/timer cleanup on all settlement paths, before subscriber callbacks. Throwing custom signal methods cannot lose an ACK or retain active pending requests.
- Existing WAL v1, OPFS layout and Worker protocol2 retained.
