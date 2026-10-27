# Экспериментальный Rust/WASM WAL в браузере

C1–C4 переносят существующий WAL-only Rust Engine в Dedicated Worker с настоящим OPFS. Это отдельный backend hoke/db; KV/localStorage и React entry points не загружают WASM при импорте. Native Database с SST/manifest/compaction остаётся отдельным Rust API.

## Подготовка

Нужны Node24, pnpm, Python3, Rust1.97.1 для сборки, WASM target и wasm-bindgen CLI той же версии, что crate:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
pnpm install --frozen-lockfile
pnpm build
pnpm demo
```

Откройте http://127.0.0.1:8000/examples/opfs/. Нажмите «Открыть базу», запишите значение, закройте и откройте базу, прочитайте. Затем перезагрузите страницу, снова откройте базу и прочитайте. Удаление ключа также сохраняется. Демо использует имя browser-demo, key theme и не очищает весь OPFS.

Если порт8000 занят, после сборки запустите `python3 -m http.server 8001 --bind 127.0.0.1` и откройте тот же путь на порту8001. Другой порт означает другой origin и отдельное хранилище.

`pnpm build:wasm` собирает release hoke-wasm и генерирует JS/WASM/declarations в packages/hoke/wasm. Этот каталог игнорируется Git, но включается в tarball вместе с dist. CLI version проверяется до сборки. CARGO_TARGET_DIR учитывается через cargo metadata. Сборка из исходников требует Rust; установленный npm tarball уже содержит скомпилированный WASM.

## API

```ts
import { openDB } from "hoke/db";

const db = await openDB("settings", { timeoutMs: 30_000 });
const key = new TextEncoder().encode("theme");
try {
  await db.put(key, new TextEncoder().encode("dark"));
  const bytes = await db.get(key); // Uint8Array | undefined
  await db.delete(key);
} finally {
  await db.close();
}
```

mode равен wal-v1. Методы чтения/записи асинхронны, близкие запросы последовательно выполняются Worker. close идемпотентен. Имя: 1–64 ASCII символа, первый буква/цифра, далее буквы/цифры/_/-. Оно отображается в OPFS/hoke-wal-v1/<name>.wal. Ключи до64KiB, значения до16MiB; пустые и бинарные bytes разрешены. Входные и возвращаемые буферы копируются, caller buffers не detached.

Валидное имя с другим регистром не нормализуется. Origin и имя определяют хранилище; native multi-file каталоги не подключаются автоматически. Второй владелец того же файла получает LOCKED; другой name независим. Никакого readwrite-unsafe и fallback в память/localStorage.

## Подтверждение и ошибки

Put/delete вызывают Rust Engine с существующими WAL v1/CRC32/recovery. ACK возвращается после успешного OPFS handle.flush. Неполный финальный frame обрезается по native recovery; полный corrupt frame и future version отклоняются без переписывания. Номер версии WAL не является номером JS store schema.

DBError содержит code/message. INVALID_ARGUMENT — имя/bytes/options; UNSUPPORTED — нет Worker/secure context/sync OPFS; LOCKED — второй владелец; STORAGE — открытие OPFS/доступ/квота; ENGINE — Rust/WASM/операция adapter; CLOSED — закрытый экземпляр; ABORTED — отмена через signal; TIMEOUT/WORKER/PROTOCOL — транспорт. Ошибка I/O движка блокирует последующие mutations до close/reopen, не выдавая ложный ACK; чтение ранее опубликованного in-memory state остаётся возможно.

Таймаут (1..600000ms, default30000), worker crash и повреждённый ответ прекращают Worker и отклоняют все pending запросы. Нельзя отменить отдельный уже выполняемый синхронный вызов; прекращение Worker закрывает всю сессию. Запись могла уже пройти flush до потери ответа. Не повторяйте mutation автоматически: reopen и прочитайте состояние. При транспортном сбое до отправки ответа Worker также пытается освободить engine/handle; при невозможности сигнализации клиент завершит сессию по timeout.

Flush браузера не устраняет quota/eviction, удаление данных пользователем, физическое повреждение или все особенности питания/ФС. Origin storage не является резервной копией. Этот prototype не заявлен production-ready.

## Проверка

```sh
pnpm test:wasm
HOKE_BROWSER_CHANNEL=chrome pnpm test:browser
HOKE_BROWSER_CHANNEL=chrome pnpm check:all
```

`test:wasm` ожидает результат `pnpm build:wasm` и выполняет 20 тестов настоящего WASM с управляемым byte adapter, short I/O и ошибками. Это не замена OPFS-тестам. Browser suite включает низкоуровневые OPFS/WASM сценарии и проверки запускаемых OPFS/KV демо. Актуальные количества и результаты — в PROGRESS.md. Локально проверен Chrome155/Linux. Установка bundled Chromium и CI описаны в TESTING.md; внешний CI пока не запускался.

Проверяются binary/empty/copy, delete, close/reopen/reload, exclusive ownership, namespace isolation, Worker termination после ACK, corrupt/future version с корректной CRC без переписывания, incomplete tail, ошибки read/write/flush и невозможность отправить ответ. Проверка client protocol/timeouts выполняется отдельными unit tests с fake Worker. Worker termination не равен browser-process crash или отключению питания.

## Текущие границы и следующий шаг

Весь WAL проигрывается в map при open, общий размер базы не ограничивается API. SST, compaction/GC, atomic batch и DB migrations в браузерном API пока отсутствуют. Scan и байтовые коллекции добавлены в C2, см. ниже. Не используйте этот срез для неограниченного журнала. Полный C ещё не завершён: перенос multi-file publication потребует отдельного ADR и fault tests. Multi-tab здесь означает явный отказ второму владельцу, не совместный доступ.

Архитектура и альтернативы — [ADR0010](adr/0010-browser-wal-worker.md). OPFS требует Dedicated Worker и secure context; источники: [sync handles](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemFileHandle/createSyncAccessHandle), [flush](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemSyncAccessHandle/flush).

## C2: scan и байтовые коллекции

```ts
const rows = await db.scan({
  start: { key: new TextEncoder().encode("user:1"), inclusive: true },
  end: { key: new TextEncoder().encode("user:9"), inclusive: false },
  prefix: new TextEncoder().encode("user:"),
  reverse: true,
  limit: 50,
});
// rows: { key: Uint8Array; value: Uint8Array }[]
```

Start/end всегда задаются в прямом порядке, reverse меняет только выдачу. Все фильтры пересекаются. Без опций возвращается максимум100 живых записей, limit допускает0..1000; limit0 даёт пустой результат после проверки bounds. Ключи сравниваются побайтово, prefix может быть пустым или содержать0xff. Удалённые keys не выдаются. Result arrays принадлежат caller; входные bounds/prefix копируются при вызове.

Полный scan packet ограничен32MiB, включая счётчики/длины. Превышение возвращает ENGINE с сообщением о byte budget без частичной выдачи. База остаётся открытой: уменьшите limit/диапазон. Лимит результата не ограничивает общий WAL/map/RSS и не делает scan streaming.

```ts
const encode = (text: string) => new TextEncoder().encode(text);
const users = db.collection(encode("users"));
await users.put(encode("001"), encode("Alice"));
const selected = await users.scan({
  start: { key: encode("001"), inclusive: true },
  limit: 10,
});
// selected[0].key === bytes("001"), без физического namespace.
```

DBCollection предоставляет get/put/delete/scan и подписки с bytes и разделяет срок жизни/ошибки своей базы. Отдельного close нет. Имя коллекции — любые bytes, включая пустые; копируется при создании. Безопасный namespace:0x01 + u32BE длина prefix + prefix. Коллекции a/ab с ключами bc/c не сливаются. Максимум prefix65531bytes; namespace+local key должен помещаться в64KiB. Collection scan использует относительные bounds/prefix и не выходит за свою коллекцию. Root db.scan показывает физические ключи всех коллекций, существующие raw keys автоматически не переносятся.

Worker protocol теперь v2: старые request/response отклоняются. WAL по-прежнему v1, ранее сохранённые данные открываются без миграции. Scan читает Rust map через общий native range-код без файловых операций; это согласованное состояние на момент выполнения запроса в очереди Worker, не snapshot на момент вызова из main thread.

В демо можно сохранить user:1=Alice и user:2=Bob, установить prefix user:, reverse и limit1, затем «Прочитать список». После reload данные сохраняются; после удаления user:2 выдаётся user:1. Архитектура, лимиты и key encoding — [ADR0011](adr/0011-browser-scans-collections.md).

## C3: подписки после подтверждения

```ts
import { DBNotificationError } from "hoke/db";

let changes = 0;
const stop = db.subscribeRange({ prefix: new TextEncoder().encode("user:") }, () => {
  changes++;
});
const stopKey = db.subscribe(new TextEncoder().encode("user:1"), () => {
  // Синхронный callback: здесь можно пометить UI для обновления.
});
try {
  await db.put(new TextEncoder().encode("user:1"), new Uint8Array([1]));
} catch (error) {
  if (error instanceof DBNotificationError) {
    // committed === true: запись сохранена, errors содержит ошибки callbacks.
    // Не повторяйте mutation автоматически.
  } else {
    throw error;
  }
} finally {
  stop();
  stopKey();
}
```

subscribe принимает точный byte key; subscribeRange — {start,end,prefix} с теми же inclusive/exclusive правилами, что scan. Без фильтров `{}` наблюдает все ключи. limit/reverse не принимаются: это множество ключей, а не текущая страница scan. Входные bytes копируются. DBCollection поддерживает те же методы с относительными ключами/range, которые остаются внутри её namespace.

Callback вызывается синхронно после проверенного successful ACK put/delete, до завершения Promise этой mutation. Начального callback нет; get/scan/replay не уведомляют. Повторная запись того же значения и удаление отсутствующего ключа тоже дают invalidation. Отдельный get/scan из callback увидит состояние на момент обработки нового запроса в Worker, а не снимок исходной записи.

Callbacks должны возвращать void синхронно. Если нужна async работа, запускайте её отдельно с собственным catch, не возвращая Promise из callback. Promise/thenable result считается ошибкой; native rejected Promise наблюдается без unhandled rejection, custom then method не вызывается. Все подходящие активные callbacks получают попытку вызова, а их ошибки собираются в DBNotificationError extends AggregateError: code SUBSCRIBER_FAILED, committed true. Это ошибка уведомления после сохранения; rollback и автоматического retry нет. База остаётся доступной.

Регистрации независимы и идут в порядке добавления. Новые внутри dispatch ждут следующую запись; unsubscribe до очереди callback отменяет его. close и terminal transport failure сразу удаляют все подписки, в том числе когда ACK ранее отправленной записи ещё ожидается. При reopen подписки регистрируются заново. Ошибка/timeout/потерянный ACK не создаёт notification даже при обнаружении записи после recovery.

Подписки действуют для операций одного BrowserDB и его collection wrappers. Между вкладками события пока не маршрутизируются. В демо задайте prefix user:, нажмите «Наблюдать prefix», сохраните user:1, затем other: счётчик изменится только от user:1. «Остановить подписку» и закрытие базы прекращают наблюдение. Решение и проверки — [ADR0012](adr/0012-browser-commit-subscriptions.md).

## C4: отмена через AbortSignal

```ts
import { openDB, type DBOperationOptions } from "hoke/db";

const opening = new AbortController();
const db = await openDB("settings", { signal: opening.signal });
// После ACK open этот signal уже не управляет соединением.
const request = new AbortController();
const operation: DBOperationOptions = { signal: request.signal };
try {
  const rows = await db.scan({ prefix: new Uint8Array([1]), limit: 10 }, operation);
  console.log(rows);
} finally {
  await db.close();
}
// request.abort() из UI до settlement отменяет запрос по правилам ниже.
```

Сигнатуры BrowserDB и DBCollection: get(key,options?), put(key,value,options?), delete(key,options?), scan(scanOptions?,operationOptions?). DBOperationOptions содержит signal; OpenDBOptions также принимает timeoutMs. close() не принимает signal и закрывает всю сессию.

DBAbortSignal — публичный structural type с readonly aborted:boolean, addEventListener("abort",listener,options?) и removeEventListener("abort",listener). Native AbortController.signal подходит без преобразования; импорт hoke/db не требует глобального AbortSignal. Публичные declarations пригодны для потребителя без DOM types. Некорректные signals/getters/registration возвращают INVALID_ARGUMENT до dispatch.

Уже отменённый signal у валидного open возвращает DBError с code ABORTED без создания Worker. У get/put/delete/scan он отменяет только этот запрос до postMessage: соединение, подписки и другие pending requests остаются доступны. Аргументы проверяются прежде отмены; невалидный ключ/диапазон остаётся INVALID_ARGUMENT. Событие или изменение aborted во время регистрации listener также отменяет только ещё не отправленный запрос.

**После dispatch abort завершает весь Worker и отклоняет все pending requests с ABORTED**, включая запросы без signal. Подписки очищаются, последующие операции получают CLOSED. Синхронный Rust/OPFS вызов нельзя отдельно прервать cancel message. Запись могла уже пройти flush: ABORTED не означает rollback или отсутствие данных. Автоматических повторов нет. Откройте базу заново, проверьте сохранённое состояние, затем решайте, нужна ли новая mutation. close после terminal abort идемпотентен.

Успешный ACK снимает listener и timer до callbacks подписчиков. Abort signal уже подтверждённой mutation, в том числе из её callback, не отменяет commit и не закрывает соединение. Ошибка callback по-прежнему возвращается как DBNotificationError с committed:true. Если abort обработан раньше ACK после dispatch, соединение завершается с неизвестным исходом; поздний ответ игнорируется и notifications не синтезируются. Signal открытия действует только до ACK open, затем его abort не влияет на открытую базу.

Listeners снимаются также при error ACK, timeout, transport/protocol failure и close. Сломанный custom removeEventListener, бросающий исключение, не меняет ACK и не оставляет pending/timer; если сторонний объект сохранил callback, тот уже инертен. Стандартные signals освобождают listeners полностью. Контракт и альтернативы — [ADR0013](adr/0013-browser-cancellation.md).

## Подготовка SST (C5a)

В ядре и WASM реализованы чтение/построение существующих SST v1; внутренний файловый слой `packages/hoke/src/db/sst-files.ts` создаёт кандидатов и открывает их через OPFS sync handles. Он сохраняет tombstones, различает пустые значения и missing, поддерживает bounded scan и освобождает handles при close/ошибке. Файлы совместимы с native codec, новый durable format не введён.

Это подготовка файловой части, а не переключение openDB на SST. WAL остаётся источником истины; flush/compaction/batch в браузерном API ещё не подключены. Публикация кандидатов и reclamation требуют отдельного решения. Контракт — [ADR0015](adr/0015-sst-wasm-files.md), варианты commit — [ADR0014](adr/0014-opfs-multifile-publication-proposal.md).
