# ADR 0011: ограниченные browser scans и байтовые коллекции

Статус: принято для C2 по плану этапа C. Дисковый WAL v1 и native Database не меняются. Новый код читает уже восстановленную WAL map через Rust, не повторяет движок на JS.

## Выбор

1. Скопировать всю map в JS и фильтровать: избыточные копии, неограниченная выдача, отдельная семантика bounds/tombstones. Отклонено.
2. Публичный streaming cursor: полезен для больших выборок, но требует нового протокола lifecycle/cancellation/consistency. Отложено.
3. Ограниченный Rust scan с общим merge/range кодом и пакетным ответом Worker: выбран. Для текущего WAL-only backend SST sources пусты; используются те же правила, что в native scan.

## API

BrowserDB.scan(options?) возвращает Promise<DBEntry[]>; DBEntry содержит owned key/value Uint8Array. DBScanOptions: start/end как {key,inclusive}, prefix bytes, reverse boolean, limit integer0..1000 (default100). Bounds не меняются местами при reverse. start>end — INVALID_ARGUMENT до запроса Worker. Равные bounds с Excluded дают пусто, с обеими Included — точечное чтение. Prefix пересекается с bounds; пустой/all-FF prefix допустимы. Лимит считает живые записи после tombstones. Входные bytes копируются до async boundary.

В ядре Engine::scan_with_budget использует существующий visit_sources с пустым списком SST, без I/O и копирования всей map. Budget считает key+value payload и проверяется перед добавлением очередной строки. Даже нулевой limit валидирует bounds. Ошибка не изменяет состояние/файл, не выдаёт частичную выдачу и не poison экземпляр. Scan от poisoned Engine показывает то же ранее опубликованное in-memory state, что get; recovery может вернуть более новое состояние после неизвестной записи.

## Ресурсы и wire protocol

Максимум1000 строк и32MiB полного scan packet, включая заголовки. Каждый key/value сохраняет прежние пределы64KiB/16MiB. Большая строка/выборка отклоняется с ENGINE и сообщением о byte budget; автоматического усечения по байтам нет. Уменьшите limit или сузьте диапазон. Это ограничитель результата, не общего WAL/map/RSS: временные копии на Rust/JS/structured clone остаются.

Worker protocol повышен до version2. Старые request/response версии отвергаются без скрытого fallback. WAL header остаётся version1. Временный scan packet: u32LE rowcount, затем для каждой строки u32LE keylen, u32LE valuelen и соответствующие bytes. Пакет не записывается на диск. Применение existing wasm-bindgen Vec<u8> не требует новой зависимости.

Client проверяет лимит bytes/строк, requested limit, lengths, точный конец пакета, порядок/уникальность и принадлежность bounds/prefix до успешного ответа. Неверный ответ — terminal PROTOCOL с завершением Worker и отклонением pending calls. Все выданные arrays принадлежат caller.

## Коллекции

BrowserDB.collection(prefix:Uint8Array) возвращает DBCollection с get/put/delete/scan; данные остаются bytes. Отдельного Worker/owner/close нет — lifecycle принадлежит базе. Generic JSON codecs здесь не вводятся.

Простая конкатенация prefix+key допускает коллизию a+bc и ab+c. Физический namespace выбран как byte0x01 + u32BE prefix.length + prefix bytes, затем local key. Tag1 задаёт версию key encoding для новых коллекций, без преобразования прежних raw keys. Prefix может содержать любые bytes и быть пустым; максимум65531bytes. Local key вместе с namespace обязан помещаться в64KiB.

Collection scan переводит local bounds/prefix в physical, дополнительно ограничивает результат своим namespace и возвращает относительные keys. Root DB.scan видит physical keys коллекций. Raw writes в те же физические ключи доступны владельцу BrowserDB; используйте единый способ адресации приложения. Существующие ключи не перемещаются автоматически.

## Проверки

Native map model/budget/no-I/O, actual WASM packet/bounds/budget/lifecycle, client invalid packet/copy/timeout, real Chrome OPFS reload/tombstones/ranges/collections, stale protocol и превышение32MiB без блокировки базы. Демонстрация prefix/reverse/limit в examples/opfs. После интеграции pnpm check:all и MSRV; отдельный локальный commit C2 без стороннего авторства.

SST/compaction, streaming cursor, atomic batch, subscriptions и DB migrations в браузере остаются следующими срезами. Этот шаг не меняет обещания C1 о durability/одном владельце.
