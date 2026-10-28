# ADR 0020: явный container-v1 в публичном BrowserDB

Статус: принято для C6c, 28 октября 2026. Завершает подключение coordinator ADR0019 к существующим Worker/client lifecycle. Форматы WAL/SST/container не меняются.

## Выбор backend

openDB(name,{mode:"container-v1"}) открывает OPFS/hoke-container-v1/<name>.hkc. Без mode остаётся wal-v1 и прежний OPFS/hoke-wal-v1/<name>.wal. Одинаковое логическое имя в разных modes — две независимые базы; нет импорта, миграции, downgrade или fallback. Неизвестный mode отклоняется до Worker/OPFS. Оба режима используют исключительного владельца файла и текущие timeout/AbortSignal/close правила.

Worker protocol повышен с2 до3, независимо от persistent versions. Request open принимает optional mode; batch передаёт структурные byte operations, flush/compact/stats — отдельные requests. Старый несовместимый client/Worker получает явный PROTOCOL. Success response сохраняет Uint8Array|null shape, существующие scan bytes прежние.

## API и batch

BrowserDB.mode — wal-v1|container-v1. Новые методы writeBatch(operations,options?),flush(options?),compact(options?),stats(options?) принимают DBOperationOptions с signal. В wal-v1 они отклоняются UNSUPPORTED без dispatch, с сохранением соединения. Обычные get/put/delete/scan, subscriptions и collections работают в обоих modes.

DBBatchOperation — type:"put" с byte key/value или type:"delete" с byte key. До dispatch копируются все массивы и проверяются1024 input ops, key64KiB/value16MiB и32MiB общего key+value payload. Дубликаты сохраняют входной порядок и тоже считаются в бюджете. Worker повторяет валидацию независимо от клиента. Collection.writeBatch добавляет namespace к каждому local key и учитывает его bytes в physical key/batch caps.

Непустой batch подтверждается одним root. Пустой batch не меняет root; ACK допустим, но notifications нет. После успешного ACK notifyMany выбирает registrations один раз: каждая подходящая регистрация вызывается ровно один раз за batch, даже если совпало несколько keys. Global registration order, duplicates registrations, snapshot/unsubscribe/close и DBNotificationError committed=true сохраняют прежнюю семантику. Никаких уведомлений до commit, при unknown outcome или от flush/compact/stats.

## Stats, отмена и ошибки

Stats wire packet40 bytes — пять u64LE: generation,tableCount,walSegments,memtableEntries,memtableBytes. Клиент проверяет длину, положительную generation и caps counters; generation остаётся bigint, счётчики безопасными numbers. Повреждённый ответ завершает сессию с PROTOCOL. Generation — commit root counter, не версия схемы стора.

Pending batch keys копируются отдельно от payload transport. Signal снимается до callbacks; abort собственного уже подтверждённого запроса не закрывает здоровую базу. Abort после dispatch или timeout закрывает Worker и все pending, при этом batch мог сохраниться целиком. Recovery/read, а не automatic retry, определяет дальнейшее действие приложения. Callback error не отменяет root и не poison соединение.

Контейнер после failed persistent publication/source corruption требует reopen для get/scan/mutations; stats остаётся диагностикой. Logical flush/compaction не обещают уменьшение append-only file. Существующие backend limits сохраняются; первоначальное имя, eviction и power loss этим API не доказываются.

## Демо и доставка

Демо выбирает mode до открытия; default WAL сохранён. Контейнер показывает atomic pair, stats, flush/compact и reopen/reload. Нет скрытого переноса старых данных. npm остаётся private; разрешённый пользователем Git push относится к origin/main и не публикует npm пакет.
