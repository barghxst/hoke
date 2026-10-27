# ADR 0009: воспроизводимые native benchmarks

Статус: принято по запросу пользователя измерить работающий Rust-прототип. Движок и форматы файлов не меняются.

## Варианты

1. Внешний процесс CLI на каждую операцию: измеряет cargo/process startup и open вместе с API, плохо изолирует стоимость движка.
2. Criterion и статистический harness: полезен для микроизмерений, добавляет dependency и сам не определяет durability/cache/workload.
3. Отдельный std-only Rust binary + Python stdlib runner: выбрано. API измеряется внутри процесса, runner делает release build один раз, повторяет независимые datasets и сохраняет raw results/environment/summary.

## Нагрузки и границы замеров

Отдельный свежий каталог каждой реплики. Database на NativeDirectory, синхронный single writer, manual flush (оба порога None), обязательный sync_all на mutation. Seed задаёт воспроизводимую перестановку ключей; keys/values/order готовятся до таймера. Данные проверяются по модели вне таймера. Результаты выдаются только после успешной проверки всего прогона; ошибка запуска не выглядит успешным benchmark.

Cases: put_sequential, overwrite_random, reopen_wal, flush, get_sst_random, get_sst_missing, scan_forward, scan_reverse, batch_put, compact, reopen_sst, gc. Обычный batch по ADR 0006 создаёт SST/manifest, это не WAL group commit. Перед compact создаются устаревшие версии/tombstones вне таймера. GC получает отдельные obsolete files. Reopen измеряет NativeDirectory::open + Database::open, без запуска нового процесса и без page-cache eviction.

Каждая API call измеряется Instant вокруг вызова, error/result validation после остановки таймера. elapsed_ns — сумма отдельных samples_ns, а не wall-time всего сценария. Включены allocations/CRC/IO внутри API, исключены подготовка данных, сравнения с моделью, сбор metadata, close и печать результата. scan samples возвращают owned Vec с rows до scan-limit, items — фактически прочитанные строки; batch samples считают API calls отдельно от количества mutations. Точки get — heap-resident key indexes + файловый value reader, не JS KV/localStorage.

Кэш: warm OS/page cache, перед SST reads выполняется untimed validation; cache eviction и cold-start гарантии не заявляются. Flush/compact включают sync и manifest publication/cleanup. Снимки bytes/files — сумма apparent file lengths всего benchmark database directory, не allocated disk blocks и не peak RSS. Перед/после каждой фазы metadata собирается вне таймера.

## Запуск и raw data

hoke-bench: --dir <existing-empty-directory> --keys N --key-bytes N --value-bytes N --scan-limit N --scan-iterations N --batch-size N --seed U64. Defaults keys1000/key16/value256/scan100/scaniterations50/batch100/seed1. Validate все аргументы до filesystem mutations: keys1..100000, key8..65536, value0..16777216, scan-limit1..keys, scaniterations1..10000, batch1..1024, batch rawbytes<=32MiB и prepared budget keys*(3*key_bytes+2*value_bytes+16)<=128MiB. Число batch SST плюс два подготовительных flush не превышает 4096: ceil(keys/batch-size)+2<=4096. Только новые дочерние каталоги; существующий непустой root отклоняется, пользовательские DB не перезаписываются.

Stdout JSON Lines schema1: один kind=config record со всеми числовыми параметрами/profile/cache_policy/durability, затем kind=measurement записи с case, calls, items, elapsed_ns, samples_ns, before/after. Снимок содержит memtable_entries, memtable_bytes, table_count, wal_bytes, files, bytes. Имена cases/строк фиксированы; для чисел не нужен JSON dependency. Binary сохраняет measurements до полной верификации, не печатает partial success. default-run Cargo остаётся hoke для прежних команд.

scripts/bench.py: release build --locked, несколько независимых прогонов (default3), raw run-NNN.jsonl и stderr, metadata.json и summary.json в новом output directory. Отдельный --workdir определяет ФС временных данных; очищается только созданный runner tempdir. Environment: UTC, rustc/cargo, OS/CPU, выбранная ФС/workdir, source fingerprint, git revision/dirty when available, config. Build time вне measurement. Metadata state running/failed/complete, summary появляется только при успешных валидных репликах.

Summary: pooled p50 API latency, p95 при>=20 samples, p99 при>=100; min/median/max API time/rate между репликами, calls и items явно разделены. Нет заявлений о превосходстве или confidence intervals по нескольким запускам. Zero-duration rate — null, а не Infinity. Детальные выводы требуют реплик, фиксированной машины/нагрузки и storage; smoke-run в /tmp не доказывает производительность SSD.

## Проверки

Rust process smoke: small dataset, 12 cases, config/schema/calls/items/sum(samples), disk reclamation и validation; invalid/duplicate/hostile arguments и непустой root не меняют пользовательские файлы. Python summary tests на известных samples, malformed/partial/duplicate/mismatched results, zero duration и complete/failed state. CI проверяет harness, не timing thresholds. Полный pnpm check и реальный release-run с несколькими репликами; исходные local results сохраняются отдельно от временных DB.
