# Проверить compaction и очистку

Compaction — ручное слияние всех активных SST и memtable в один SST с последними живыми значениями. Удалённые ключи не воскресают; устаревшие значения и tombstones из покрытых источников удаляются. Форматы WAL/SST/manifest остаются v1. Это Rust API/CLI, веб-демо пока использует localStorage.

## Запускаемый сценарий

Из корня репозитория создайте отдельный временный каталог. Каждая команда ниже — новый процесс; после неё файлы открываются заново.

```sh
compact_demo=$(mktemp -d /tmp/hoke-compact-demo.XXXXXX)
cargo run -q -p hoke-core -- --db "$compact_demo" put theme light
cargo run -q -p hoke-core -- --db "$compact_demo" put old obsolete
cargo run -q -p hoke-core -- --db "$compact_demo" flush
cargo run -q -p hoke-core -- --db "$compact_demo" put theme dark
cargo run -q -p hoke-core -- --db "$compact_demo" delete old
cargo run -q -p hoke-core -- --db "$compact_demo" flush
cargo run -q -p hoke-core -- --db "$compact_demo" put newest unflushed
cargo run -q -p hoke-core -- --db "$compact_demo" info
ls -l "$compact_demo"
cargo run -q -p hoke-core -- --db "$compact_demo" compact
cargo run -q -p hoke-core -- --db "$compact_demo" scan
cargo run -q -p hoke-core -- --db "$compact_demo" get old
ls -l "$compact_demo"
cargo run -q -p hoke-core -- --db "$compact_demo" gc
```

До compact: memtable_entries=1, tables=2. После: memtable_entries=0, tables=1, wal_bytes=16. Scan показывает newest=unflushed и theme=dark, get old — NOT_FOUND. В каталоге остаются LOCK, MANIFEST, один SST и один WAL. Повторный compact при одном SST без tombstones и пустой memtable только проверяет мусор и не создаёт поколение. gc после успешной очистки удаляет 0 файлов.

На маленьких значениях новые headers/index могут перекрывать экономию payload; количество obsolete files уменьшается, но уменьшение bytes не гарантируется для каждого набора данных. Автотест отдельно проверяет освобождение места после перезаписи крупных values.

## Rust API и ошибки

```rust
let report = db.compact()?;
println!("{} -> {} tables", report.tables_before, report.tables_after);
for error in report.cleanup.errors {
    eprintln!("{} {:?}: {}", error.operation, error.file, error.source);
}
let cleanup = db.collect_garbage()?;
```

Ошибка compact после начала файловых операций до публикации возвращает Err и блокирует mutations/GC до drop/reopen. Исчерпание generation до создания файла возвращает Limit без блокировки: GC может убрать orphan с предельным generation, после чего compact можно повторить. Подтверждённые данные сохраняются в старом или новом целом состоянии. Успешный результат содержит отдельный CleanupReport: removed_files и errors с именем файла, операцией и io::Error. Ошибки cleanup не означают откат опубликованной compaction; база остаётся доступной. Повторите collect_garbage/gc после устранения причины. CLI при cleanup errors печатает, что публикация уже произошла, и завершает команду с ошибкой.

GC удаляет только строго распознанные obsolete g<20 digits>.sst/.wal/.manifest.tmp. Файлы текущего manifest, LOCK, MANIFEST и неизвестные имена сохраняются. Не используйте зарезервированные generation-имена для своих файлов. GC не исправляет повреждения активных файлов. Перед удалением синхронизируется видимый manifest, после удаления — каталог.

## Ресурсы и проверки

Values передаются в writer по одному через heap merge; лимит обычного scan=100 не применяется. В памяти остаются исходные key indexes/memtable и индекс нового SST. На время операции требуется дополнительное место под полный живой результат. Один выходной SST ограничен 1M entries и 64 MiB index: сверх лимита compact возвращает ошибку без удаления исходных файлов. Фоновых потоков/автоматической compaction пока нет.

```sh
cargo test -p hoke-core --test compaction
cargo test -p hoke-core --test compaction_process
cargo test -p hoke-core --test cli_compaction
pnpm check
```

Fault injection проверяет ошибки файловых шагов и очистки; реальные child-process kill покрывают границы до/после manifest rename и удаления obsolete files. Это Linux process-crash проверки, не доказательство устойчивости к потере питания. [Решение и компромиссы](adr/0005-compaction.md).
