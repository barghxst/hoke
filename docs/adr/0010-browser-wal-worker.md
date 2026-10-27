# ADR 0010: первый браузерный Rust/WASM срез через WAL v1

Статус: принято для C1, экспериментальный browser API. Пользователь поручил продолжить WASM/Worker/OPFS после native-прототипа. Форматы файлов и native Database не меняются.

## Граница текущего среза

Native Directory требует синхронных open/create/list, atomic replace rename и sync_directory. Подмена этих операций успешными no-op не переносит прежний commit-протокол в браузер. Для публикации SST/manifest через OPFS нужен отдельный протокол и проверка гарантий.

Сравнены варианты:

1. Имитация Directory поверх async операций и памяти: синхронное ядро не может ожидать такой I/O; ранний ACK и no-op sync нарушили бы контракт. Отклонено.
2. Новый browser manifest/commit format: может решить multi-file publication, но задаёт долговременный формат и требует отдельного обсуждения и fault-модели. Отложено.
3. Существующий WAL-only Engine, один OPFS файл, sync handle внутри Dedicated Worker: выбран первый рабочий срез. WAL v1 и recovery повторно используются без конвертации. SST/compact/atomic batch здесь не доступны.

## Архитектура

hoke/db экспортирует async openDB(name) и BrowserDB с mode='wal-v1', get/put/delete/close. Этот experimental API не выдаётся за native Database целиком. Keys/values Uint8Array, ограничения прежние: key64KiB/value16MiB. Возвращаемые bytes принадлежат вызывающему коду; входные bytes копируются до postMessage.

Импорт модуля не создаёт Worker и не обращается к browser storage. openDB проверяет имя (ASCII буква/цифра, далее буквы/цифры/_/-, максимум64), создаёт module Worker. WASM и OPFS открываются только там. Хранилище: OPFS/hoke-wal-v1/<name>.wal. Это отдельная WAL-only база; native multi-file каталог не импортируется автоматически.

WasmWal реализует Storage через JS adapter вокруг FileSystemSyncAccessHandle. Rust перехватывает JS exceptions, проверяет safe offsets/counts, завершает short writes и обеспечивает zero-filled gap/extension. Storage.flush не имеет собственных буферов, sync_all вызывает OPFS handle.flush. Put/delete подтверждаются только после Engine commit. Ошибки I/O сохраняют исходный неизвестный результат и poison до reopen.

Default readwrite sync handle служит единственным владельцем файла. Второе открытие получает LOCKED; readwrite-unsafe не используется. Это отказ второго владельца, а не маршрутизация multi-tab. После close освобождаются Rust allocations, handle и Worker; ошибка закрытия также освобождает ресурсы.

## Сообщения и ошибки

Protocol version1, положительные safe integer request ids; последовательная очередь Worker. Ответ повторяет version/id и содержит success result (bytes либо null) или code/message. Неверный протокол отклоняется. Open/get/put/delete/close имеют timeout; по timeout/worker crash/messageerror неверному ответу клиент прекращает Worker, отклоняет все pending requests и не допускает дальнейшие операции. Close идемпотентен на клиенте.

Timeout или потеря Worker после отправки mutation не доказывают rollback: WAL мог уже пройти flush, а ответ потеряться. Автоматических повторов нет; требуется reopen и чтение результата. Отмена отдельного исполняемого sync вызова не обещается; transport cancellation прекращает весь Worker. Неподдерживаемая среда/ошибки доступа/квоты не заменяются памятью или localStorage.

## Проверки и последующие шаги

Build: wasm32-unknown-unknown + точная пара wasm-bindgen crate/CLI0.2.129, web target glue в package wasm/. Native платформенные adapters исключаются из WASM, общие Engine/Database/format codecs остаются portable.

Проверки C1: реальный Chrome, WASM в Worker, бинарные/пустые bytes, close/reopen/reload, независимость имён, второй владелец, восстановление после Worker termination, corruption/future version без переписывания, неполный хвост, quota/flush exceptions без ложного ACK, unsupported environment; unit tests lifecycle/protocol/timeout/copy.

Это прототип с полным WAL и map в памяти, без ограничителя общего размера, scan/collection/subscriptions/DB migrations и без SST compaction. Последующие C-срезы добавляются отдельно. Browser quota/eviction и физическая потеря питания не устранены вызовом flush; process/Worker termination не равны отключению питания. Проверяется конкретный Chrome/Linux, другие браузеры отдельно.

## Первичные источники

- [OPFS sync handles в Dedicated Worker и исключительная блокировка](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemFileHandle/createSyncAccessHandle).
- [OPFS flush](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemSyncAccessHandle/flush).
- [wasm-bindgen catch для JS imports](https://wasm-bindgen.github.io/wasm-bindgen/reference/attributes/on-js-imports/catch.html).
