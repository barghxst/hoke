# ADR 0017: общий путь чтения WAL и SST

Статус: принято для C5c. Источники выбирает вызывающий coordinator; этот срез не переключает active root и не публикует SST.

## Порядок версий

Общий Rust модуль read реализует get_from_sources и scan_sources_with_budget. SST передаются от старого к новому, WAL новее всех этих SST. Значение или tombstone WAL окончательны для ключа; иначе проверяются таблицы от новой к старой. Ошибка выбранного источника возвращается вызывающему коду без fallback на устаревшее значение. Native Database::get использует тот же helper.

Scan переиспользует существующее heap merge ядра: byte order, пересечение prefix/bounds, reverse и предел именно живых строк. Дубликаты разрешаются до чтения payload; удалённые ключи не занимают результат и не воскрешаются при reverse. Budget считает суммарные key/value bytes с checked arithmetic, переполнение возвращает ошибку без частичного результата. Payload выбранной записи может быть декодирован до проверки бюджета.

## WASM view

WasmReadView открывает до64 SST и удерживает их validated indexes. Каждая get/scan получает ссылку на живой WasmWal, а не копию его map или storage: последующие подтверждённые put/delete видны при следующем чтении. View не является snapshot. Возвращённые byte arrays независимы от последующих изменений.

View закрывает только собственные Rust reader objects. WAL и JS handles принадлежат вызывающему коду. Закрытый WAL или view не могут участвовать в чтении. Ограничения биндинга: key64KiB, value16MiB, scan до1000 строк и32MiB полного packet. Tombstones не выдаются как пользовательские строки. Тот же live-row encoder используется прежним WasmWal.scan.

## OPFS и общий lifecycle

Внутренний openSstReadView получает каталог и упорядоченные имена. До I/O копирует список, проверяет count, duplicates и допустимые single-component .sst names. Открывает только существующие файлы, получает exclusive sync handles и держит их до close. Сбой любого открытия инициирует cleanup всех уже приобретённых handles. Close пытается освободить Rust object и каждый handle, сохраняя первую ошибку; WAL не закрывается.

Worker, SST files и read view используют одну initializeWasm promise в пределах realm. Независимые одновременные initialize могли бы заново создать instance и сбросить generated globals при уже существующих Rust allocations. Неудачный init допускает последующую явную попытку, без storage fallback и создания файлов до загрузки WASM.

Общий scan-packet decoder используется клиентом Worker и внутренним read view: framing, lengths, caps, порядок, уникальность, bounds/prefix и trailing bytes проверяются одинаково. Протокол и bytes ответа прежние.

## Границы

WAL должен соответствовать выбранным SST и быть новее их. Directory ownership, порядок поколений и консистентное переключение набора файлов — ответственность coordinator. View не выводит поколение из случайного имени, не выбирает root, не создаёт/исправляет SST и не выполняет compaction или GC. Payload CRC проверяется лениво только при чтении выбранной записи; открытие не является полным scrub.

Публичный hoke/db по-прежнему WAL-only; новые файловые модули внутренние. Durable форматы, Worker protocol2, subscriptions и package exports не меняются. Следующий контейнерный backend использует эти же правила версий, но получает отдельный журнал публикации.
