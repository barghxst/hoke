# ADR 0019: KV coordinator внутри container v1

Статус: принято для C6b, 28 октября 2026. Использует журнал ADR0018 и существующие WAL/SST v1 payload codecs. Не меняет container bytes; root уже умеет ссылаться на ordered SST/WAL segments.

## Запись и память

ContainerDatabase подтверждает put/delete/write_batch только после Journal commit. Каждая непустая mutation/batch — отдельный immutable WAL v1 segment: header16 и полные consecutive frames от1, затем root со всеми активными SST и WAL segments. Batch сохраняет входной порядок, последние операции ключа побеждают; caps1024 ops/32MiB key+value. Пустой batch не пишет root.

Memtable — производное volatile состояние, переиспользующее Engine map/tombstones без копирования всего persistent WAL в MemoryStorage. В него применяются только уже подтверждённые Journal root операции; после reopen он строится из явно перечисленных WAL segments. Immutable WAL payload не ремонтируется: partial frame, trailing bytes, неверная CRC/version/sequence/limits возвращают ошибку.

Пределы первого coordinator:64 active SST,64 active WAL segments,32MiB dirty key/value bytes. Перед mutation, которая превысит memtable byte cap, inherited SST cap1M dirty entries или WAL fan-in, выполняется flush прежнего состояния; requested mutation ещё не записана. Если нужен65-й SST, возвращается capacity error с требованием compact. Один допустимый batch всегда укладывается в пустую memtable. SST output staging ограничен256MiB, как Journal segment. Индексы/allocations не входят в memtable budget.

## SST и reads

Существующие SST читаются через read-only bounded storage views одного общего handle; values не копируются целиком в RAM при open. WAL map и SST используют общий mixed-source get/scan: WAL новее таблиц, таблицы oldest→newest, tombstones окончательны. Source error не вызывает stale fallback. Scan budget key+value плюс отдельный32MiB packet cap биндинга, live-row limit1000.

Flush сериализует dirty changes с tombstones в новый SST segment, готовит validated reader, публикует root с прежними SST+новым и без WAL refs, затем очищает производную memtable. Compact объединяет весь active state, включая memtable: поэтому здесь можно удалить tombstones, отсутствующие данные больше не имеют старых active versions. Результат — максимум один новый SST и root без WAL; пустой результат даёт empty root. Physical container не сокращается; excluded refs не возвращаются и bytes не удаляются.

Output временно строится в capped MemoryStorage до append, поэтому flush/compact пока требуют до256MiB staging плюс индексы/decoded record. Это осознанный первый срез без ложного streaming append claim. Persistent источники остаются immutable, writer не копирует все SST values в map.

## Открытие и неизвестный исход

Journal получает deferred-tail recovery: существующий outer prefix проверяется без truncate. Coordinator проверяет root caps, все active SST metadata и immutable WAL payload и только затем разрешает truncate/sync incomplete outer tail. Inner corruption не должна ремонтировать файл или переключать root. Файл без root инициализируется explicit empty root; orphan segments не становятся user state.

Pure invalid input/capacity до I/O сохраняет writable state. Ошибка persistent append/commit или known source corruption делает coordinator poisoned до reopen. Root может быть сохранён без ACK после failed final sync; local memtable не выдаёт такую запись за подтверждённую. В poisoned состоянии get/scan/mutations отклоняются, stats остаются diagnostics. Close/into_storage не создаёт commit.

## Доступ

Rust ContainerDatabase, WasmContainerDb и внутренний OPFS .hkc adapter. Существующий openDB/Worker protocol2 остаётся WAL-only до отдельного подключения явного mode. Старый .wal не интерпретируется как контейнер и автоматически не переносится. Native Directory Database остаётся отдельным backend.
