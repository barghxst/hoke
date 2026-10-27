# hoke-core

Экспериментальный Rust-движок с бинарным WAL. Реализованы `Engine<S>`, `Storage`, файловый и volatile memory backend, put/get/delete, CRC32, replay и обработка неполного EOF append. WAL-only Engine хранит данные в памяти; Database добавляет SSTable, ручной или настраиваемый flush, scan, compaction и atomic batch.

```rust
use hoke_core::Engine;
use hoke_core::storage::FileStorage;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut db = Engine::open(FileStorage::open("/tmp/hoke-example.wal")?)?;
    db.put(b"theme", b"dark")?;
    assert_eq!(db.get(b"theme"), Some(b"dark".as_slice()));
    db.close()?;
    Ok(())
}
```

Сценарий через CLI, каждый вызов — отдельный процесс:

```sh
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal put theme dark
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal get theme
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal delete theme
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal get theme
```

Ожидается: `OK put (synced)`, `value="dark"`, `OK delete (synced)`, `NOT_FOUND`. В stderr показаны replayed и discarded_tail_bytes. CLI принимает UTF-8 аргументы и показывает значения как строки для диагностики; Rust API принимает произвольные bytes.

Ошибки I/O не маскируются. После ошибки write/flush/sync дальнейшие изменения дают Poisoned до reopen; исход операции на диске может быть неизвестен. Файловый lock не даёт второму кооперативному писателю открыть тот же файл, но не защищает от внешнего кода, обходящего locks.

Проверки: `pnpm check:rust` или rustfmt + cargo clippy + cargo test. Есть инъекции ошибок и реальные process-kill тесты на Unix. Проверенная среда — Linux, Rust 1.97.1. Заявленный API baseline 1.89 отдельно не проверен; Windows/macOS и WASM пока не тестировались. Новый файл синхронизируется, родительский каталог — пока нет, полной гарантии power-loss durability нет.

Подробности: docs/adr/0002-native-storage-and-wal.md и docs/WAL_FORMAT.md в корне репозитория. Публикация crates.io отключена.

## Database: отдельный каталог с SST

```rust
use hoke_core::{Database, NativeDirectory};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut db = Database::open(NativeDirectory::open("/tmp/hoke-db")?)?;
    db.put(b"theme", b"dark")?;
    db.flush()?;
    assert_eq!(db.stats().memtable_entries, 0);
    assert_eq!(db.get(b"theme")?, Some(b"dark".to_vec()));
    db.close()?;
    Ok(())
}
```

CLI: `cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db put theme dark`, затем с тем же префиксом команды info, flush, get theme, delete theme. Отсутствующий каталог создаётся при существующем parent; непустой каталог без manifest не очищается и не переинициализируется. Это отдельный формат каталога, старый единичный WAL автоматически не переносится.

FileStorage::open_existing не создаёт missing referenced file, create_new не перезаписывает существующий. NativeDirectory удерживает LOCK и выполняет directory sync на Unix. Database освобождает значения delta после flush, но индексы ключей SST остаются в памяти. Чтение SST может вернуть I/O/corruption error, поэтому get возвращает Result<Option<Vec<u8>>>. Полный CRC records проверяется лениво, а не scrub всех values на open.

Есть opt-in pre-write flush по payload threshold; bloom и интерактивных транзакций нет. compact/gc явно удаляют старые WAL/orphans. После flush error новые mutations запрещены до reopen; ошибки после rename могут означать, что новое состояние уже опубликовано.

## Scan/range

Database::scan(ScanOptions) и range(ScanOptions) возвращают Result<Vec<KeyValue>> с owned byte pairs. Default limit100, bounds Unbounded, prefixNone, reversefalse. Стандартные Included/Excluded bounds пересекаются с prefix; limit применяется к живым уникальным ключам после разрешения версий. CLI --db поддерживает scan, prefix <prefix>, range <start> <end> и флаги --reverse/--limit N.

[Описание и проверяемый сценарий](../../docs/SCANS.md). Heap merge не собирает все values; payload проигравших версий не читается. Ошибка выбранного frame возвращает Err без частичного результата и без fallback. Результат пока Vec, streaming iterator не реализован.

## Compaction/GC

Database::compact() сливает все активные источники, включая WAL delta, потоково в один SST с живыми значениями; полностью пустой результат не оставляет SST. Возвращает CompactionReport с tables_before/after и отдельным CleanupReport { removed_files, errors }. После ошибки до публикации нужен reopen; ошибки cleanup не отменяют опубликованные данные. collect_garbage() повторяет очистку без переписывания. Операции требуют writable instance. Старые handles закрываются до удаления; неизвестные файлы сохраняются.

CLI --db: compact и gc. Ограничения: один output SST максимум 1M entries/64 MiB index, временное место под живой результат, без фоновой политики. [Сценарий проверки и ресурсы](../../docs/COMPACTION.md) · [ADR 0005](../../docs/adr/0005-compaction.md).

## Atomic batch

Database::write_batch(&[BatchOperation]) атомарно публикует ordered put/delete через SST и manifest. Повторы одного ключа разрешены; последнее действие побеждает. Текущие WAL changes включаются в новую delta до переключения на пустой WAL. Форматы файлов v1 сохранены, WAL-only Engine не получает batch API.

Вход: максимум 1024 операции/32 MiB key+value bytes, 64 KiB key/16 MiB value. validate_batch проверяет вход до открытия базы; write_batch делает ту же проверку. Input/capacity ошибки оставляют базу writable; IO publication error требует reopen, batch может быть целиком записан или отсутствовать. Пустой batch — no IO на здоровой открытой базе.

Каждый непустой batch создаёт SST; это пока более дорогой протокол, чем WAL append. Compact уменьшает число таблиц. [API, runnable CLI и проверки](../../docs/BATCHES.md) · [ADR 0006](../../docs/adr/0006-atomic-batch.md).

## Memtable threshold

DatabaseOptions { max_memtable_bytes: Some(bytes), ..Default::default() } и open_with_options включают синхронный pre-write flush. Default с обоими порогами None сохраняет manual mode; нулевой порог недопустим. Перед валидным put/delete projected key/value payload проверяется с учётом overwrite/tombstones, и непустая delta сбрасывается, если результат превысит порог. При ошибке preflush новая операция ещё не добавлена в WAL. После успешного preflush отказ append имеет прежний unknown-outcome контракт.

stats.memtable_bytes — текущий payload delta, а не heap footprint и не WAL bytes. Один большой record может превысить мягкий порог; open/read/close и empty batch сами flush не запускают. write_batch остаётся атомарным. CLI --db <path> --memtable-limit N <command>; настройка действует на один open. [Сценарий и ограничения](../../docs/MEMTABLE.md) · [ADR 0007](../../docs/adr/0007-memtable-threshold.md).

## WAL threshold

DatabaseOptions.max_wal_bytes: Option<u64> задаёт мягкий порог активного WAL, включая header и frame overhead. Минимум16, defaultNone. Перед append непустая delta сбрасывается, если projected WAL length превысит порог; поэтому повторные overwrite с постоянным memtable payload тоже ограничиваются. Одиночный record больше порога допускается, старые WAL/orphans убираются явным GC, это не disk quota.

CLI --db <path> [--memtable-limit N] [--wal-limit N] <command>: флаги до команды в любом порядке, каждый один раз. Любое превышение вызывает один preflush; ошибка preflush не применяет новую запись. [Сценарий, точные byte counts и проверки](../../docs/WAL_THRESHOLD.md) · [ADR 0008](../../docs/adr/0008-wal-threshold.md).

Native benchmark: `pnpm bench` из корня workspace. Методика, JSONL и ограничения — [BENCHMARKS.md](../../docs/BENCHMARKS.md). CLI базы остаётся default-run hoke.
