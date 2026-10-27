# ADR 0006: atomic write batch через manifest

Статус: принято для текущего неопубликованного прототипа. Пользователь поручил продолжить движок после B4. WAL, SST и manifest остаются v1.

## Варианты

1. Цикл обычных put/delete с одним sync в конце: быстрее отдельных операций, но replay может восстановить половину batch. Отклонено.
2. Новый batch record или begin/commit markers в WAL: эффективный будущий путь, но меняет долговременный формат и правила совместимости WAL. Требует отдельного обсуждения, сейчас не вводится.
3. Публиковать batch как одну новую SST delta через существующий atomic manifest: выбрано для первого проверяемого API. Использует уже реализованные форматы и протокол публикации, дороже обычного WAL append.

## API и семантика

Database::write_batch(&[BatchOperation]) -> Result<(), DbError>. BatchOperation: Put { key: Vec<u8>, value: Vec<u8> } или Delete { key: Vec<u8> }. Операции применяются по порядку; последнее действие над повторным ключом побеждает. Ключи и значения — произвольные bytes. Пустой batch — no-op без IO на здоровой базе; poisoned instance отклоняет даже пустой batch.

validate_batch проверяет все операции до любых IO и копирования delta. Максимум 1024 операции и 32 MiB суммы key/value bytes (повторы также считаются), max key 64 KiB/value 16 MiB как в WAL. Неизвестные действия CLI, неверные размеры и превышение лимитов не изменяют базу и не poison instance. Ограничения числа таблиц/выходной SST проверяются до создания файлов.

## Публикация

1. Проверить writable, batch input и table limit. Скопировать текущие WAL changes в отдельную sorted delta.
2. Применить batch к этой копии, сохраняя tombstones. Проверить ограничения SST: 1M entries/64 MiB index. Активная memtable пока прежняя.
3. Создать новую SST, записать, flush/sync, проверить готовый индекс. Создать новый пустой WAL, sync его header и sync_directory новых имён.
4. Записать/sync manifest.tmp со старыми SST + новой SST и новым WAL; atomic rename и sync_directory.
5. Только после успеха переключить handles и освободить старую memtable. Все операции batch видимы вместе; последующие обычные put/delete идут в новый WAL.

До commit manifest ссылается на старые SST/WAL, не содержащие batch. После commit — на всю новую delta. Даже если sync/rename вернули ошибку после действия, reopen видит весь batch или ни одной его операции. Старые подтверждённые WAL changes сохранены в обоих состояниях. Активная память при ошибке остаётся прежней, mutations/flush/compact/GC запрещены до reopen. Ошибка с неизвестным исходом не означает rollback и не разрешает слепой повтор прикладной операции.

Удаление tombstones здесь недопустимо: старые SST не покрыты batch целиком. Cleanup автоматически не запускается; старый WAL/orphans убираются явными gc/compact. Старые readers понимают результат, потому что форматы v1 не расширяются. Engine в WAL-only режиме не получает batch API; интерактивных транзакций, read/write callbacks, изоляции нескольких писателей нет.

## Цена и границы

Каждый непустой batch создаёт SST и новый WAL, сбрасывает всю текущую WAL delta, требует нескольких sync и публикации manifest. Количество SST растёт до 4096; затем нужен compact. Стоимость подготовки — копия memtable и batch, не всей базы. Это atomicity-first реализация, не group commit и не обещание ускорения массовых вставок. Оптимизация через batch WAL — отдельное следующее решение о формате.

CLI --db <path> batch (put <key> <value> | delete <key>)*. Полный список парсится и валидируется до открытия каталога; флаги внутри key/value остаются литералами. Пустая команда batch допустима как no-op. Старый WAL-only режим batch отклоняет до создания файла.

## Проверки

Несколько ключей/дубликаты/delete/reinsert/binary bytes, существующие SST и unflushed WAL, empty/no IO, лимиты без записи и poison. Fault injection каждого шага публикации проверяет отсутствие partial batch, старое live-state при ошибке и all-or-none после reopen. Неоднозначные write/rename, последующие flush/compact/gc и ordered model. Реальные child-process kill до/после manifest rename и после ACK. Linux process crash не заменяет проверку потери питания.
