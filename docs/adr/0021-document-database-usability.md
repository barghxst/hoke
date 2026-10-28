# ADR 0021: удобный слой JSON-коллекций над byte DB

Статус: принято, 28 октября 2026. Пользователь запросил завершить удобство использования DB после обсуждения byte API, выбора backend и ручного обслуживания. Это дополнительный TypeScript API; WAL/SST/container formats и Worker protocol3 не меняются.

## Публичный путь

`openDatabase(name,{timeoutMs?,signal?})` всегда открывает существующий container-v1 namespace. `database.collection(name,{validate})` возвращает типизированные get/set/delete/list/writeBatch/subscribe/subscribeRange со строковыми ключами и JSON-значениями. `close` завершает lifetime всей базы. Старый `openDB` и byte collections сохраняют свои defaults и контракты.

Один новый путь убирает выбор режимов из обычного сценария. Менять default старого openDB нельзя: одинаковое имя в WAL и container — разные базы. Нет автоматического импорта или переинтерпретации прежних файлов.

Имена и ключи кодируются UTF-8; lone surrogates отклоняются, BOM сохраняется. Порядок — byte order UTF-8, без locale sorting или Unicode normalization. JSON записывается как пользовательский value без envelope и без новой версии формата движка. Приложение задаёт синхронный runtime validator; generic TypeScript не проверяет сохранённые данные.

Codec отклоняет значения, которые JSON молча меняет или теряет: undefined, BigInt, functions, symbols, nonfinite numbers, negative zero, holes, cycles, custom prototypes/toJSON/accessors. Вход сериализуется до очереди, валидируется сохранённое JSON-представление. Повреждённый UTF-8/JSON или несовпадение схемы при чтении дают ошибку без перезаписи. Snapshots immutable. Миграций схемы JSON-коллекции в этом срезе нет.

Batch кодируется и проверяется целиком до отправки первой записи. set/delete сохраняют порядок, включая повторяющиеся ключи. Пустой batch не запускает maintenance. Нет read-modify-write update с ложным обещанием атомарности.

## Очередь и обслуживание

DocumentSession последовательно исполняет принятые async операции одной базы, включая чтение и закрытие. Аргументы фиксируются до постановки в очередь. close перестаёт принимать новые операции, дожидается уже принятых и закрывает raw DB; повторный close возвращает тот же Promise. Подписки остаются connection-local.

Перед непустой mutation запрашиваются stats. При tableCount>=32 либо walSegments>=48 выполняется полная compaction; запас остаётся ниже core source caps64. Проверка добавляет один Worker round trip на mutation. Это простая политика первого удобного API, а не заявление об оптимальной производительности. Она снимает необходимость вручную следить за количеством источников; общие staging/size limits и ошибки I/O сохраняются.

Maintenance выполняется до requested write, без автоматического повторения mutation после ошибки. Успешная compaction не сокращает физический контейнер. Ошибка subscriber после ACK остаётся DBNotificationError(committed:true), timeout/abort после dispatch может оставить сохранённую mutation. Abort пока действует по контракту raw DB и может завершить всё соединение.

## React и сценарий использования

`useDBValue(collection,key)` из hoke/react возвращает loading/success/error. Подписка устанавливается до чтения; новые invalidations и смена ключа отбрасывают устаревшие ответы. Cleanup не вызывает abort отправленного запроса, чтобы размонтирование компонента не закрывало общую базу. SSR возвращает loading, Worker при импорте не создаётся.

Демо задач использует этот API без byte encoding, выбора mode и кнопок flush/compact. Проверяем создание, изменение, удаление, атомарный batch, повторное открытие и безопасный вывод сохранённого текста. Это не новый framework запросов: query cache, indexes, cross-tab routing, миграции DB и React Native остаются отдельными задачами.
