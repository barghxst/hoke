# Автоматический сброс memtable

Обычный put уже синхронизирует WAL. Flush переносит накопленные изменения в SST и освобождает значения memtable. Теперь можно задать порог, при превышении которого база сама выполняет flush перед следующей записью.

Default по-прежнему manual: Database::open и CLI без --memtable-limit не запускают автоматический flush. Форматы файлов v1 сохранены.

## Проверить через CLI

В отдельном временном каталоге, каждый вызов — новый процесс:

```sh
threshold_demo=$(mktemp -d /tmp/stowtree-threshold-demo.XXXXXX)
cargo run -q -p stowtree-core -- --db "$threshold_demo" --memtable-limit 8 put a 1234
cargo run -q -p stowtree-core -- --db "$threshold_demo" info
cargo run -q -p stowtree-core -- --db "$threshold_demo" --memtable-limit 8 put b 5678
cargo run -q -p stowtree-core -- --db "$threshold_demo" info
cargo run -q -p stowtree-core -- --db "$threshold_demo" scan
```

После первого put: memtable_entries=1, tables=0, memtable_bytes=5 (один byte ключа и четыре bytes значения). Перед вторым put размер стал бы 10 > 8: старые изменения автоматически уходят в SST. После второго put: memtable_entries=1, tables=1, memtable_bytes=5. Scan показывает a=1234 и b=5678 после reopen.

Флаг задаёт политику одного открытия и находится перед командой. Для записи в другом CLI-процессе его нужно указать снова; info/get/scan сами ничего не сбрасывают, даже с флагом. Настройка не сохраняется в manifest.

Замена значения меньшим учитывает новый размер, а не сумму всех исторических append:

```sh
cargo run -q -p stowtree-core -- --db "$threshold_demo" --memtable-limit 8 put b 0
cargo run -q -p stowtree-core -- --db "$threshold_demo" info
cargo run -q -p stowtree-core -- --db "$threshold_demo" --memtable-limit 8 delete a
cargo run -q -p stowtree-core -- --db "$threshold_demo" get a
cargo run -q -p stowtree-core -- --db "$threshold_demo" scan
```

После замены b: memtable_bytes=2, tables=1. После delete a: memtable_bytes=3, get a=NOT_FOUND. Tombstone хранит ключ, чтобы старое значение из SST не вернулось.

## Rust API

```rust
use stowtree_core::{Database, DatabaseOptions, NativeDirectory};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = DatabaseOptions {
        max_memtable_bytes: Some(8 * 1024 * 1024),
        ..Default::default()
    };
    options.validate()?;
    let mut db = Database::open_with_options(
        NativeDirectory::open("/tmp/stowtree-threshold-db")?, options,
    )?;
    db.put(b"theme", b"dark")?;
    println!("{} dirty payload bytes", db.stats().memtable_bytes);
    db.close()?;
    Ok(())
}
```

None отключает payload-порог, Some(0) отклоняется. Полностью ручной режим — оба порога max_memtable_bytes/max_wal_bytes равны None. Для API, которому нельзя создавать каталог при неверных options, вызовите validate до NativeDirectory::open: создание аргумента происходит до входа в open_with_options.

## Семантика и сбои

Счётчик равен сумме key.len + value.len живых изменений и key.len tombstones текущей WAL delta. Повторный overwrite/delete заменяет прежний вклад; при reopen счётчик восстанавливается из журнала. После flush/batch/compact новая memtable пуста и счётчик 0.

Перед put/delete база проверяет writable и допустимость входа. Если projected payload > порога и delta непуста, выполняется flush. Равенство порогу flush не вызывает. При уменьшении значения, возвращающем delta под порог, flush не нужен.

При ошибке preflush новая запрошенная операция ещё не записана и после reopen отсутствует. Старые подтверждённые изменения сохраняются через прежний manifest/WAL или опубликованный SST. Instance блокирует mutations до reopen, как при ошибке ручного flush. Если preflush завершился, а затем отказал append новой операции, применяется обычное правило WAL: исход этого append неизвестен и после recovery операция может присутствовать.

Неверные key/value проверяются до preflush: ошибка не меняет файлы, не сбрасывает delta и не poison instance. write_batch не дробится по порогу: весь пакет и предыдущая delta публикуются вместе по ADR 0006. Пустой batch ничего не сбрасывает.

## Ограничения

Это мягкий порог payload, не hard memory limit. Одиночная запись больше порога допустима при соблюдении key64KiB/value16MiB; её нельзя предварительно сбросить из пустой memtable. BTreeMap nodes, Vec capacity, allocator overhead, уже загруженные SST indexes и временная копия delta во время flush не входят в memtable_bytes.

Порог не ограничивает длину WAL: повторные изменения одного ключа могут увеличивать журнал, почти не меняя payload delta. Для явного сокращения активного WAL доступен flush. Максимум SST остаётся 4096, для слияния нужен compact. Automatic compaction/GC и фоновые потоки пока не добавлены. Отдельный [WAL-size threshold](WAL_THRESHOLD.md) включается через max_wal_bytes/--wal-limit. Flush синхронный и может увеличить задержку конкретного put/delete; latency benchmarks пока не выполнены.

[ADR 0007 и варианты](adr/0007-memtable-threshold.md).

## Автоматические проверки

```sh
cargo test -p stowtree-core --test threshold
cargo test -p stowtree-core --test threshold_process
cargo test -p stowtree-core --test cli_threshold
pnpm check
```

Проверяются границы/overwrite/delete/recovery, большие одиночные операции, zero-byte dirty keys, batch и no-IO при invalid input, ошибки каждого preflush шага, после preflush append с неизвестным исходом, модель с обслуживанием. Child-process kill до/после manifest rename подтверждает сохранение старых данных и отсутствие ещё не начатой записи. Проверено на Linux; это не доказательство power-loss safety на всех ФС.
