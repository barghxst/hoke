# ADR 0018: append-only container journal v1

Статус: принято для первого контейнерного среза C6a, 28 октября 2026. Направление нового формата выбрано пользователем в ADR0014. Кодек, journal и его биндинги реализуются по этому контракту. Старые WAL/SST bytes не меняются; переключение openDB и перенос данных в этот срез не входят.

## Bytes и пределы

Все числа little-endian; CRC32 IEEE, как в существующих codecs. File header32: magic8 `HOKECNT\0`, version u32=1, header length u32=32, reserved12 zero, CRC32 первых28 bytes. Неполный существующий file header — ошибка, не пустая база. Только файл нулевой длины инициализируется header+flush+sync.

Record header32: magic4 `HKR1`, kind u8 (1 WAL segment,2 SST segment,3 root), reserved3 zero, sequence u64, payload length u64, reserved4 zero, header CRC32 первых28 bytes. Затем payload и u32 CRC32 полного header+payload. Sequence строго consecutive от1 для всех записей, включая root. Header CRC/limits проверяются до доверия длине и allocations.

Segment payload пока opaque: coordinator впоследствии проверяет существующие WAL/SST codecs. Ref24: kind u8 (1/2), reserved7 zero, record offset u64, payload length u64. Offset указывает на начало record header, длина — только payload. Root payload24+24*N: generation u64, table count u32, WAL count u32, reserved8 zero, затем table refs и WAL refs; каждый список в логическом порядке от старого к новому. Generation строго consecutive от1. Табличные refs должны иметь kind2, WAL refs kind1, offsets уникальны во всём root.

Пределы: segment payload256MiB,4096 refs в root,4096 pending segments между roots, file offsets/end не выше2^53-1. Root payload максимум98328 bytes. Пустые segments допустимы как opaque bytes; пустой root допустим. Payload CRC проверяется streaming chunks при open; root metadata выделяется только после caps. CRC не защищает от злонамеренной переписи с пересчётом checksum.

## Источники root и bounded metadata

Root может ссылаться только на segments из непосредственно предыдущего root либо новые segments, записанные после него. Root не может ссылаться на себя, другой root, payload interior, будущий offset или ранее исключённый segment. Проверка идёт по реальным record boundaries и точному kind/offset/length, а не по наличию похожего header внутри payload.

После каждого root каталог метаданных сокращается до его refs; новые segments добавляются в pending. Это ограничивает metadata до4096 active+4096 pending refs без карты всех historical records. Complete uncommitted segments после последнего root сохраняются в файле; автоматически в active state они не включаются. Исключённые segments физически не удаляются и повторно не включаются.

## Commit и ошибки

Journal::append_segment пишет immutable record и возвращает ref, без flush/ACK пользовательской mutation. Journal::commit сначала валидирует списки/refs/caps/generation/sequence без I/O, затем flush+sync всех candidate bytes, append root record, ещё flush+sync, затем публикует root в памяти и возвращает успех. Это две file barriers; directory barrier не имитируется.

Любая I/O-ошибка после начала append/commit делает writer poisoned. Следующие mutations отклоняются до reopen, даже если полный root уже оказался в storage. Pure validation/capacity errors до I/O оставляют writer writable. Повреждение CRC/header при повторном чтении известного segment также делает journal poisoned; неизвестный ref отклоняется до I/O без poison. После poison доступны root/recovery diagnostics, а чтение segments и mutations требуют reopen. Commit без ответа имеет неизвестный исход; нет rollback/resend. Полный CRC-valid root после failed sync может восстановиться, даже если ACK не был отправлен.

## Recovery

Open сканирует файл строго вперёд, проверяет каждый полный record, sequence, payload CRC, root generation и references. Full-record corruption, неизвестная версия/kind, bad header CRC, неверный root или потерянный referenced segment — явная ошибка без fallback к более старому root и без изменения файла.

Только incomplete EOF append после валидного prefix отбрасывается: менее32 оставшихся bytes либо валидный header с недостаточным payload/trailer. Header с неверным CRC/длиной/kind не маскируется под incomplete tail. После проверки всего prefix хвост truncate+flush+sync до возврата writable journal. Последний полный root задаёт active refs; complete records после него остаются pending. Root отсутствует у только что инициализированного файла (generation0); первый logical DB root публикуется явно.

Допущение — append-only операции не меняют ранее записанные ranges; arbitrary torn overwrite/power loss не доказаны. Физическая потеря/eviction файла, устойчивость первоначального directory entry и page-level persistence требуют отдельных ограничений/проверок. Worker termination и fault adapters не равнозначны потере питания.

## Владение и первый API

Rust Journal<S:Storage> владеет одним storage. open/append_segment/commit/read_segment/root/recovery/into_storage; read_segment допускает только active/pending refs и перепроверяет CRC. into_storage не делает commit или sync. Нет automatic close commit. WASM wrapper владеет Rust object, JS caller — sync handle; close освобождает только Rust state. OPFS использует новое явное имя `.hkc`, без автоматического открытия прежнего `.wal` как контейнера.

Этот слой ещё не KV Database: нет автоматической сборки root из put/delete, SST flush, atomic batch API, reclaim или миграции. Он даёт настоящий durable publication primitive для следующего coordinator. Append-only compaction пока не уменьшает размер файла.
