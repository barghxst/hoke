# ADR 0013: отмена browser DB на границе dispatch и ACK

Статус: принято для C4 этапа C. Это клиентское расширение hoke/db. Rust/WASM, WAL v1, OPFS layout и Worker protocol2 остаются прежними.

## Решение и альтернативы

1. Отменять только Promise отдельной операции после dispatch, оставляя Worker открытым. Отклонено: синхронный Rust/OPFS вызов продолжит выполнение, очередь изменит состояние, а поздний ACK придётся отдельно маршрутизировать. Такой API легко принять за rollback.
2. Добавить cancel messages и кооперативное прерывание движка. Отложено: Worker не обрабатывает новое сообщение во время синхронного вызова; необходимы новые границы исполнения и отдельный протокол.
3. До dispatch отклонять только запрос; после dispatch завершать всю Worker-сессию. Выбрано: это существующая terminal transport policy с явным кодом ABORTED. Остановка Worker не означает откат или отсутствие сохранённой записи.

## Публичный API

```ts
interface DBAbortSignal {
  readonly aborted: boolean;
  addEventListener(type: "abort", listener: () => void, options?: { once?: boolean }): void;
  removeEventListener(type: "abort", listener: () => void): void;
}
interface DBOperationOptions {
  signal?: DBAbortSignal;
}
interface OpenDBOptions extends DBOperationOptions {
  timeoutMs?: number;
}
```

DBAbortSignal — structural type: native AbortController.signal подходит, глобальный AbortSignal и DOM declarations потребителю не нужны. Custom signals должны соблюдать синхронный event-listener контракт. Runtime проверяет boolean aborted и callable методы; исключения getters/registration до dispatch возвращаются как INVALID_ARGUMENT без отправки операции.

openDB(name, options?) принимает signal только на время открытия. BrowserDB и DBCollection принимают get(key, options?), put(key,value,options?), delete(key,options?) и scan(scanOptions?,operationOptions?). Второй аргумент scan отделяет lifetime от диапазона. close() остаётся без signal: он освобождает всю сессию.

## Границы и результат

- Сначала проверяются имя/timeout или аргументы операции. Для валидного pre-aborted open Worker не создаётся. Валидная pre-aborted операция возвращает ABORTED без postMessage, завершения соединения, воздействия на другие pending запросы и notifications.
- Listener регистрируется до dispatch, затем aborted проверяется повторно. Событие или изменение флага во время регистрации отменяет только ещё не отправленный запрос; его timer/listener/pending metadata очищаются.
- Abort после dispatch завершает Worker, очищает subscriptions и отклоняет все pending requests с ABORTED, включая запросы без signal и ожидающий close. Последующие операции получают CLOSED; close закрытого экземпляра идемпотентен.
- Нельзя определить, была ли уже опубликована mutation: flush мог завершиться до abort или потери ACK. Отмена не означает rollback. Автоматических retry/resend нет; reopen и чтение позволяют приложению проверить состояние.
- При проверенном ACK pending сначала помечается settled, удаляется из map, timer/listener снимаются; только затем запускаются postcommit callbacks. Abort собственного signal из callback или после ACK не отменяет подтверждение и не закрывает здоровое соединение. DBNotificationError с committed=true сохраняет прежний смысл.
- Сигнал open снимается на ACK open и не управляет дальнейшими операциями базы. Сигналы остальных операций действуют только до settlement соответствующего запроса. Поздний ответ после terminal abort игнорируется, synthetic notifications при recovery не создаются.

Первое обработанное событие определяет результат гонки ACK/abort. Если ACK уже обработан, он сохраняется; если первым обработан abort после dispatch, исход записи неизвестен. Таймаут сохраняет код TIMEOUT и ту же terminal policy.

## Cleanup и сломанные custom signals

Единый cleanup используется на success ACK, error ACK, timeout, transport/protocol failure, закрытии и отмене. settled устанавливается до вызова пользовательского removeEventListener, чтобы его reentrant callback был инертен. Исключение removal не должно терять уже полученный ACK, оставлять pending/timer или мешать отклонению остальных запросов. Если custom removeEventListener сам не удалил listener и выбросил исключение, библиотека не может заставить сторонний объект удалить его: оставшийся callback уже инертен. Для стандартных signals listeners снимаются полностью.

## Проверка и границы

Unit tests покрывают native/structural/отсутствующий global AbortSignal, pre-abort и валидацию без Worker, event/flag registration races, all-pending termination, cleanup каждого terminal path, throwing getters/add/remove, reentrancy, callback abort после ACK и forwarding коллекций. Реальные browser scenarios отдельно проверяют Worker/OPFS lifecycle и чтение после reopen; tarball проверяет публичные типы без DOM. Полные команды и фактические результаты фиксируются в TESTING.md/PROGRESS.md.

Это отмена сессии после dispatch, не кооперативная отмена отдельного Rust вызова и не гарантия физического прерывания записи. Multi-tab ownership, browser SST, atomic batch и DB migrations не добавлены. Worker termination не доказывает power-loss safety.
