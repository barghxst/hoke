# Порог размера активного WAL

Payload threshold ограничивает текущую delta, но повторные put одного ключа добавляют frames в WAL даже при постоянном размере memtable. max_wal_bytes добавляет отдельное условие pre-write flush по длине активного журнала. Форматы файлов остаются v1.

## Проверить через CLI

В отдельном временном каталоге:

```sh
wal_demo=$(mktemp -d /tmp/stowtree-wal-limit-demo.XXXXXX)
cargo run -q -p stowtree-core -- --db "$wal_demo" --wal-limit 90 put a 1234
cargo run -q -p stowtree-core -- --db "$wal_demo" info
cargo run -q -p stowtree-core -- --db "$wal_demo" --wal-limit 90 put a 5678
cargo run -q -p stowtree-core -- --db "$wal_demo" info
cargo run -q -p stowtree-core -- --db "$wal_demo" --wal-limit 90 put a 9999
cargo run -q -p stowtree-core -- --db "$wal_demo" info
cargo run -q -p stowtree-core -- --db "$wal_demo" get a
```

После каждого put memtable_entries=1 и memtable_bytes=5. При этом WAL меняется:

| После put | wal_bytes | tables | Значение a |
| --------- | --------- | ------ | ---------- |
| a=1234    | 53        | 0      | 1234       |
| a=5678    | 90        | 0      | 5678       |
| a=9999    | 53        | 1      | 9999       |

Header файла занимает16bytes, каждый из этих frames: header28+key1+value4+CRC4=37bytes. Два frames дают ровно90 и не вызывают flush. Перед третьим projected size127>90: старая delta a=5678 уходит в SST, новый пустой WAL принимает a=9999. Get в новом процессе возвращает9999, без fallback к старому SST значению.

Порог относится к одному открытию, поэтому флаг повторяется в каждой write-команде. Его можно сочетать с payload threshold, порядок двух флагов свободный:

```sh
cargo run -q -p stowtree-core -- --db "$wal_demo" --memtable-limit 8388608 --wal-limit 67108864 put theme dark
cargo run -q -p stowtree-core -- --db "$wal_demo" --wal-limit 67108864 --memtable-limit 8388608 delete theme
```

При срабатывании любого условия выполняется один preflush. Оба флага находятся до команды, каждый максимум один раз. Минимум --wal-limit16, типu64. Invalid/missing/duplicate/negative/overflow значения проверяются до открытия каталога. Текст флагов в key/value — обычные литералы.

## Rust API

```rust
use stowtree_core::{Database, DatabaseOptions, NativeDirectory};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = DatabaseOptions {
        max_memtable_bytes: Some(8 * 1024 * 1024),
        max_wal_bytes: Some(64 * 1024 * 1024),
    };
    options.validate()?;
    let mut db = Database::open_with_options(
        NativeDirectory::open("/tmp/stowtree-wal-limit-db")?, options,
    )?;
    db.put(b"theme", b"dark")?;
    db.close()?;
    Ok(())
}
```

None отключает соответствующее условие, default обоих None означает manual mode. Настройки не записываются в manifest. Reopen восстанавливает реальную длину WAL, но сам flush не запускает. Reads/close и пустой batch тоже не запускают preflush. Непустой batch публикует всю delta атомарно и начинает новый WAL по ADR 0006, независимо от порогов.

## Ошибки и ограничения

Перед обоими условиями проверяются writable и key/value limits. Если preflush отказал, новая операция ещё не записана; последнее подтверждённое значение сохраняется после reopen. Instance poison до reopen. Если preflush завершился и затем отказал новый append, исход append неизвестен по прежнему WAL-контракту.

Порог мягкий: пустая delta допускает один валидный record больше порога. Учитываются все frame bytes, включая empty key/value и tombstones. max_wal_bytes не ограничивает SST и старые WAL/orphans: суммарный размер каталога может расти, для очистки нужны gc/compact. Автоматической compaction/GC и фоновых потоков нет. Flush синхронный и влияет на задержку конкретной записи.

[Решение и варианты](adr/0008-wal-threshold.md).

## Автоматические проверки

```sh
cargo test -p stowtree-core --test wal_threshold
cargo test -p stowtree-core --test wal_threshold_process
cargo test -p stowtree-core --test cli_wal_threshold
pnpm check
```

Тесты проверяют повторные overwrite, равенство порогу, min16/empty records, оба условия вместе, batch, invalid input без IO, каждый файловый отказ preflush и неизвестный исход rename/append, модель с обслуживанием/reopen. Реальные child-process kill до/после manifest rename сохраняют последнее ACK значение и исключают ещё не записанный overwrite. Проверено на Linux; process crash не доказывает power-loss safety.
