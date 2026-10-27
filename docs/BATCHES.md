# Атомарная запись нескольких ключей

Database::write_batch публикует несколько put/delete вместе. До публикации видны прежние данные, после — результат всего пакета. Обычные последовательные вызовы put/delete сами по себе не образуют транзакцию.

Первый вариант использует существующий commit manifest: новая SST включает текущую WAL delta и batch, затем публикуется новый manifest с пустым WAL. Форматы v1 сохранены. Engine с отдельным WAL не получает batch API. Это Rust-часть; браузерное localStorage-демо не менялось.

## Проверить через CLI

Из корня проекта:

```sh
batch_demo=$(mktemp -d /tmp/hoke-batch-demo.XXXXXX)
cargo run -q -p hoke-core -- --db "$batch_demo" put old obsolete
cargo run -q -p hoke-core -- --db "$batch_demo" flush
cargo run -q -p hoke-core -- --db "$batch_demo" put pending retained
cargo run -q -p hoke-core -- --db "$batch_demo" batch put theme dark put count 7 delete old
cargo run -q -p hoke-core -- --db "$batch_demo" scan
cargo run -q -p hoke-core -- --db "$batch_demo" get old
```

Batch выводит OK batch (published), operations=3, memtable_entries=0, tables=2, wal_bytes=16. Scan в новом процессе показывает count=7, pending=retained и theme=dark. get old возвращает NOT_FOUND. pending был подтверждён до batch только в WAL и тоже сохранился после публикации.

Операции идут в порядке аргументов. Здесь последнее действие побеждает:

```sh
cargo run -q -p hoke-core -- --db "$batch_demo" batch put theme light delete theme put theme dark
cargo run -q -p hoke-core -- --db "$batch_demo" get theme
cargo run -q -p hoke-core -- --db "$batch_demo" compact
cargo run -q -p hoke-core -- --db "$batch_demo" scan
```

get theme показывает dark; compact объединяет таблицы, сохраняя те же живые значения. Пустая команда batch допустима и не сбрасывает существующую memtable. На новом пути обычный open при этом создаёт новую пустую базу.

Парсер проверяет весь список операций до открытия каталога. Неизвестное действие или неполный put/delete вызывают ошибку без записи первой половины:

```sh
cargo run -q -p hoke-core -- --db "$batch_demo" batch put theme light invalid
cargo run -q -p hoke-core -- --db "$batch_demo" get theme
```

Первая команда завершается с ошибкой, вторая по-прежнему показывает dark. После put следующие два аргумента всегда key/value, после delete следующий аргумент всегда key, даже если они выглядят как команды или флаги. Для пробелов используйте обычное quoting shell. CLI принимает UTF-8, Rust API — произвольные bytes.

## Rust API

```rust
use hoke_core::{BatchOperation, Database, NativeDirectory};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut db = Database::open(NativeDirectory::open("/tmp/hoke-batch-db")?)?;
    db.write_batch(&[
        BatchOperation::Put { key: b"theme".to_vec(), value: b"dark".to_vec() },
        BatchOperation::Put { key: b"count".to_vec(), value: b"7".to_vec() },
        BatchOperation::Delete { key: b"old".to_vec() },
    ])?;
    assert_eq!(db.get(b"theme")?, Some(b"dark".to_vec()));
    assert_eq!(db.get(b"old")?, None);
    db.close()?;
    Ok(())
}
```

BatchOperation — owned enum, метод принимает slice. validate_batch позволяет заранее проверить input, не открывая базу; write_batch повторяет проверку. Максимум 1024 операции и 32 MiB суммы key/value bytes, включая операции с повторяющимися ключами. Ключ максимум 64 KiB, значение 16 MiB. Даже если последняя операция перекрывает oversized input, весь batch отклоняется.

## Ошибки и ограничения

Неверный input и превышение table/SST capacity не меняют файлы или видимые данные и не блокируют дальнейшие изменения. Пустой batch на здоровой базе не выполняет IO. Poisoned database отклоняет любой batch, даже пустой.

После IO-ошибки публикации исход неизвестен: текущий instance сохраняет прежнюю видимую map, mutations/flush/compact/gc запрещены до drop/reopen. При reopen виден либо весь batch, либо ни одна его операция; прежние подтверждённые изменения сохранены в обоих случаях. Не повторяйте автоматически прикладное действие после ошибки: пакет мог уже попасть на диск.

Каждый непустой batch создаёт SST и новый WAL и сбрасывает текущую WAL delta. Нужна память под копию memtable + input и дополнительное место на диске. Цена операции включает создание файлов и несколько sync; ускорение вставок относительно отдельных put не заявляется. Лимит output SST — 1M entries/64 MiB index, активных SST — 4096. Для уменьшения их количества используйте compact. Старый WAL и orphan files удаляются только явными gc/compact.

Интерактивных read/write транзакций, нескольких писателей и group commit пока нет. Более быстрый batch WAL требует отдельного решения о формате. [ADR 0006](adr/0006-atomic-batch.md).

## Автоматические проверки

```sh
cargo test -p hoke-core --test batch
cargo test -p hoke-core --test batch_process
cargo test -p hoke-core --test cli_batch
pnpm check
```

Тесты внедряют ошибку каждого IO-шага, включая запись/rename с неизвестным исходом; проверяют all-or-none, порядок повторов, лимиты без IO, модель с flush/compact/gc/reopen. Child-process kill проверяет границы до/после manifest rename и после ACK. Проверено на Linux; process crash не доказывает power-loss safety.
