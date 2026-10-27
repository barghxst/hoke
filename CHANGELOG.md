# Changelog

## Unreleased — candidate v0.1

- Synchronous KV with memory and explicit localStorage adapters.
- Runtime validation, isolated namespaces, stable snapshots and storage errors with causes.
- Versioned stores, defaults, subscriptions, reset and protected synchronous updates.
- Migration chains with a single final write and explicit future-version errors.
- Optional React entry point with selectors, equality comparison and SSR snapshots.
- Settings/migration demo, automated checks and tarball consumer verification.

Not yet included: DB/LSM engine, WASM/OPFS, multi-tab notifications, React Native, devtools.

Publication is pending: license, npm/GitHub ownership and final version/metadata. Nothing has been published.

## Experimental Rust WAL prototype (unpublished)

- Native Cargo workspace and platform-independent byte Storage contract.
- Locked file backend and explicit volatile memory backend.
- Binary WAL v1 with CRC32, bounded frames, replay and incomplete-tail recovery.
- WAL-only Engine with put/get/delete, sync-before-publication and poisoned writes after I/O failure.
- CLI persistence across processes, fault injection and real process-kill tests on Unix.
- Separate JS/Rust checks and CI jobs.

Not included: SSTable, compaction, transaction batches, JS DB bindings or power-loss guarantees.

## Experimental SSTable and directory flush (unpublished)

- Immutable sorted tables with checksummed indexes, lazy value reads and tombstones.
- Exclusive NativeDirectory ownership, existing-only referenced files and checksummed manifest.
- Manual flush publishes synchronized SST/WAL through atomic manifest replacement before clearing the memtable.
- Fault/model checks and real process-kill tests around manifest publication.
- Directory CLI mode --db with info/flush; the single-WAL CLI remains available.

Not included: compaction, orphan/old-WAL garbage collection, range scans, automatic flush thresholds or JS DB bindings.

## Ordered scans (unpublished)

- Rust Database scan/range with Included/Excluded byte bounds, prefix intersection, reverse and live-key limits.
- Heap merge across SST indexes and memtable, resolving versions and tombstones before reading values.
- Explicit winner read errors, no fallback, skipped shadowed/unselected payload reads.
- Directory CLI scan/prefix/range with argument validation, --reverse and --limit.
- Model, boundary, corruption, no-read and CLI integration tests. Disk formats unchanged.
