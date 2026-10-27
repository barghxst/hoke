# Scan и range в Rust Database

`scan` объединяет memtable и SSTable в отсортированный список уникальных живых ключей. Новое значение заменяет старое, tombstone скрывает удалённое значение. Форматы WAL/SST/manifest не меняются; API доступен в Rust и CLI --db, браузерный пакет его пока не использует.

## Ручная проверка

Используйте новый каталог, чтобы ожидаемый результат не зависел от прошлых примеров:

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo put user:1 Alice
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo put user:2 Bob
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo flush
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo put user:1 Alicia
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo delete user:2
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo put user:3 Carol
```

Теперь Alice/Bob лежат в SST, Alicia/удаление Bob/Carol — в memtable и WAL. Проверьте объединение:

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo prefix user:
```

Ожидается ровно:

```text
key="user:1", value="Alicia"
key="user:3", value="Carol"
```

Alice не показывается второй раз, а Bob не возвращается из старого SST.

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo range user:1 user:3
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo prefix user: --reverse --limit 1
cargo run -q -p hoke-core -- --db /tmp/hoke-scan-demo scan --limit 0
```

Первая команда вернёт только user:1: CLI range — [start,end), правая граница исключена. Вторая — user:3 с Carol. Третья — EMPTY. После ещё одного flush те же запросы должны дать те же результаты, уже объединяя несколько SST.

Флаги --limit N и --reverse идут после positional arguments; повторные/неизвестные флаги, отрицательные/переполненные limits и перевёрнутый range отклоняются до открытия/создания каталога. Обычные put/get/delete по-прежнему допускают буквальные ключи/значения вроде --reverse. В scan-командах positional prefix/bound, начинающиеся с --, пока не поддерживаются CLI-парсером; Rust API этого ограничения не имеет.

## Rust API

```rust
use std::ops::Bound::{Excluded, Included};
use hoke_core::ScanOptions;

// db: Database<NativeDirectory>, уже открыт.
let rows = db.scan(ScanOptions {
    prefix: Some(b"user:".to_vec()),
    reverse: true,
    limit: 20,
    ..ScanOptions::default()
})?;

let rows = db.range(ScanOptions {
    start: Included(b"user:1".to_vec()),
    end: Excluded(b"user:3".to_vec()),
    limit: 20,
    ..ScanOptions::default()
})?;
```

Оба метода возвращают `Result<Vec<KeyValue>, DbError>`, где KeyValue — `(Vec<u8>, Vec<u8>)`. range — alias scan с теми же настройками. Результат владеет данными, поэтому не зависит от последующих изменений базы.

| Опция   | Семантика                                          | Default   |
| ------- | -------------------------------------------------- | --------- |
| start   | Included/Excluded/Unbounded, нижняя граница bytes  | Unbounded |
| end     | Included/Excluded/Unbounded, верхняя граница bytes | Unbounded |
| prefix  | Пересечение prefix с bounds; None — без фильтра    | None      |
| reverse | Убывание ключей, приоритет версий не меняется      | false     |
| limit   | Максимум живых уникальных ключей после tombstones  | 100       |

Порядок — лексикографическое сравнение bytes. "10" сортируется раньше "2"; это не числовая и не locale-сортировка. Пустой ключ и binary keys разрешены. Пустой prefix соответствует всему диапазону, prefix из 0xff корректно обрабатывается без переполнения successor.

start > end вызывает DbError::InvalidRange. Равные Included/Included могут вернуть один ключ; равные границы с любой Excluded — пустой диапазон. Непересекающиеся prefix и bounds дают EMPTY, а не ошибку. Корректный limit 0 возвращает пустую выдачу без storage reads внутри scan. Открытие Database само по себе всё равно читает metadata/WAL; CLI также открывает базу перед вызовом scan.

## Чтение и ошибки

В heap находится один текущий key на источник: отдельные iterators memtable values/deleted и cursor каждого SST index. Сначала выбирается свежая версия ключа, затем читается только её frame/value. Все значения базы и копии всех индексов не материализуются ради scan. Индексы SST уже находятся в памяти после open.

Payload старой версии, скрытой новой версией/tombstone, не читается. Payload вне диапазона или после limit не читается. Но headers/indexes проверяются при open: это не обход проверки повреждённых метаданных.

Ошибка выбранной записи или tombstone возвращает Err целиком. Частичная выдача не возвращается, fallback к старому значению запрещён. Сканирование не записывает данные и не запускает flush/миграции. Во время синхронного вызова один экземпляр Database заимствован эксклюзивно, конкурентная mutation через него невозможна.

Это пока материализованный Vec с limit, не streaming iterator: память результата пропорциональна сумме выбранных ключей/значений. Большой limit с большими values может потребовать много памяти. Следующие шаги — compaction/GC, затем по необходимости streaming API, bloom и sparse/block index.

## Автоматические проверки

```sh
cargo test -p hoke-core --test scan
cargo test -p hoke-core --test scan_cli
pnpm check
```

Проверяются bounds/prefix/reverse/limits, tombstones и reinsertion, модель BTreeMap с flush/reopen, нулевой limit без чтений, ошибки I/O, повреждённые winning/shadowed/unselected payloads и публичный CLI.
