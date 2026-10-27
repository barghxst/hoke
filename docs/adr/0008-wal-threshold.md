# ADR 0008: порог размера активного WAL

Статус: принято по запросу пользователя продолжить pre-write обслуживание. Форматы WAL/SST/manifest v1 и порядок commit не меняются.

## Варианты

1. Только payload threshold: экономит memtable, но повторные overwrite одного ключа могут неограниченно растить журнал.
2. Flush после превышающего append: усложняет Err после уже выполненного commit; не выбран по той же причине, что в ADR 0007.
3. Проверять projected WAL length перед append вместе с payload threshold: выбрано. Выполняется один preflush при срабатывании любого условия.

## Контракт

DatabaseOptions получает max_wal_bytes: Option<u64>. Default None. Some(N) должно быть не меньше 16 bytes (header WAL v1). Настройка относится только к текущему экземпляру и активному WAL, не ко всему каталогу, не сохраняется в manifest. options.validate проверяет оба порога до открытия файлов при использовании CLI.

После writable/input validation вычисляется projected length = current WAL bytes + frame header28 + key.len + value.len (0 для delete) + CRC4. Текущая delta непуста и projected length > N — выполнить существующий flush перед новой операцией. Равенство не вызывает flush. При переполнении projected u64 с включённым порогом тоже нужен preflush, а не wraparound. Условия WAL/payload соединяются через OR, без двух последовательных flush.

Один валидный record может быть больше мягкого порога: пустой WAL нельзя предварительно сбросить. После такого record следующая операция проверяется обычным образом. Пустые ключи/значения и повторные tombstones занимают frame bytes и учитываются, даже если memtable payload равен нулю. Это ограничение размера активного журнала между операциями с допустимым overshoot одной записью, не hard disk quota.

Open/read/close и empty batch сами flush не выполняют. Reopen читает фактическую восстановленную WAL length, без новых metadata/formats. Непустой batch по-прежнему атомарно публикует всю delta и начинает пустой WAL по ADR 0006; по порогу batch не разбивается.

## Ошибки и ресурсы

Ошибка preflush: запрошенная операция ещё не appended, instance poison до reopen, старые ACK сохраняются. После успешного preflush ошибка append имеет прежний unknown-outcome контракт. Invalid input проверяется до обоих порогов и не выполняет IO. Сохраняются limits SST/tablecount, синхронная задержка flush и временная копия delta. Старые WAL/orphans и таблицы могут накапливаться: нужны явные gc/compact, автоматическое удаление не добавлено.

## CLI

--db <path> [--memtable-limit N] [--wal-limit N] <command> ...; два флага до команды в любом порядке, каждый максимум один раз. --wal-limit принимает ASCII u64 >=16; malformed/missing/duplicate/overflow значения отклоняются до NativeDirectory::open. Текст флагов в key/value остаётся литералом. WAL-only режим новые флаги не принимает.

## Проверки

Много overwrite одного ключа при постоянном payload; точное равенство и следующий record, delete/empty key frames, min16 и огромные значения, recovered WAL, оба порога вместе, default и zero IO при invalid input. Fault каждого preflush шага с сохранением старого значения и отсутствием нового overwrite, последующий append error с неизвестным исходом, model и batch/compact/gc. Реальный child kill вокруг manifest rename до новой записи. Linux process tests не доказывают power-loss safety.
