# ADR 0012: подписки BrowserDB после подтверждённого commit

Статус: принято для C3 по этапу C. Подписки локальны одному BrowserDB, используют уже существующий ACK point mutations. Формат WAL v1, WASM и Worker protocol2 не меняются.

## Варианты

1. Опрос через get/scan: создаёт лишние запросы, задержки и не определяет границу commit. Отклонено.
2. Отдельные subscription messages и value events из Worker: нужны для server-side queries/multi-tab, но дублируют известную клиенту метаинформацию и усложняют протокол. Отложено.
3. Клиентский реестр invalidation callbacks, запуск на проверенный успешный ACK put/delete: выбран для одного владельца. Изменённый ключ копируется до отправки запроса и сохраняется в pending metadata, callback не получает данные пользователя.

## API и фильтрация

BrowserDB и DBCollection:

- subscribe(key:Uint8Array, listener:()=>void):()=>void;
- subscribeRange(range:DBRange, listener:()=>void):()=>void.

DBRange содержит только start/end как {key,inclusive} и prefix bytes. limit/reverse отклоняются: подписка описывает логическое множество ключей, а не страницу scan. Пустой range соответствует всем ключам. Bounds/prefix пересекаются; start>end отклоняется, равные excluded bounds дают пустое множество. Пустые/all-FF prefixes и binary keys поддержаны. Входные ключи/границы копируются при регистрации.

Collection переводит относительные ключи/range в физический namespace тем же кодированием C2. Подписка на весь collection использует его prefix и не видит соседние пространства. Несколько collection wrappers над одним BrowserDB видят общие подтверждения.

## Граница уведомления

Нет начального callback и событий на get/scan/open/replay/reopen. Каждая подтверждённая put/delete операция вызывает каждый подходящий активный callback один раз, включая put того же значения и delete отсутствующего ключа. Это invalidation, не сравнение старого/нового значения. Callback, поставивший следующий async get/scan, читает состояние на момент исполнения того запроса; более поздние queued mutations уже могут быть видны.

Уведомление запускается после проверки successful response и удаления pending/timer, до завершения Promise данной mutation. Ошибка движка/timeout/потерянный ACK/неверный response не вызывают callbacks, даже если после reopen найдётся полная неподтверждённая запись. Автоматический retry/синтетическое уведомление при recovery не вводится. Worker остаётся единственным последовательным владельцем операций.

Callback registrations независимы даже для одной function. Порядок общий по регистрации, active snapshot фиксируется на начало dispatch. Новые регистрации ждут следующую операцию; unsubscribe до очереди callback отменяет вызов. Unsubscribe идемпотентен. close прекращает подписки сразу, не дожидаясь ACK close; transport fail очищает реестр. После reopen подписки регистрируются заново.

## Ошибки подписчиков после commit

Listener обязан быть синхронным. Все подходящие active callbacks получают попытку вызова; исключения собираются. После dispatch mutation Promise отклоняется DBNotificationError extends AggregateError с code=SUBSCRIBER_FAILED и committed=true. Данные уже подтверждены, соединение остаётся доступным. Эту ошибку нельзя интерпретировать как rollback или автоматически повторять mutation. errors сохраняет исходные callback errors.

Promise/thenable, возвращённый callback, считается ошибкой синхронного контракта. Genuine Promise rejection наблюдается, в том числе cross-realm; custom then method не вызывается. Асинхронную работу можно запустить отдельно с собственным catch и вернуть void. Библиотека не ждёт async listener и не отменяет начатые им операции.

## Ограничения и проверка

Реестр O(number of subscriptions) на ACK; в нём нет values/scan snapshots и обращения к диску. Это не cross-tab notification, не persistence подписок и не React live-query hook. Отдельного batch event пока нет: browser atomic batch ещё не реализован.

Проверки: registry binary/range/copy/order/reentrancy/errors, client fake Worker ACK/no-error-event/timeout/close, коллекции, actual Chrome OPFS successful ACK/read, postcommit exception, async rejection, scope, close/reopen и flush failure с неизвестным результатом. Демо показывает счётчик подтверждённых изменений выбранного prefix. После интеграции pnpm check:all, tarball consumer types и отдельный локальный commit под настроенным автором пользователя.
