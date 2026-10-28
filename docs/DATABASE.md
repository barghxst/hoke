# Хранить объекты в Hoke

`openDatabase` открывает локальную базу в браузере. Вы работаете со строковыми ключами и типизированными JSON-объектами; библиотека выбирает контейнерный движок, кодирует байты и следит за количеством активных источников. Низкоуровневый `openDB` остаётся для byte API.

## Открыть и сохранить

```ts
import { openDatabase } from "hoke/db";

type Task = { text: string; done: boolean };
const isTask = (value: unknown): value is Task =>
  typeof value === "object" &&
  value !== null &&
  "text" in value &&
  typeof value.text === "string" &&
  "done" in value &&
  typeof value.done === "boolean";

// Создайте базу и коллекции один раз на lifetime приложения.
const db = await openDatabase("my-app");
const tasks = db.collection("tasks", { validate: isTask });
await tasks.set("first", { text: "Попробовать Hoke", done: false });
const task = await tasks.get("first"); // Task | undefined
await tasks.delete("first");
```

`validate` проверяет сохранённое значение при записи и чтении. Generic TypeScript сам по себе не проверяет файл. Validator должен синхронно вернуть ровно true; Promise не поддерживается. Разные коллекции могут иметь разные типы. Одинаковые имя базы и имя коллекции обращаются к тем же данным после повторного открытия.

Значения — строгий JSON: plain objects, массивы, строки, конечные числа, boolean и null. undefined, Date, Map/Set, BigInt, NaN/Infinity, negative zero, cycles, holes и getters/toJSON отклоняются явно. Значение фиксируется при вызове set/batch, до ожидания очереди; изменение исходного объекта после вызова не меняет запись. Прочитанные snapshots глубоко заморожены. Чтобы изменить документ, создайте новое значение:

```ts
const previous = await tasks.get("first");
if (previous) await tasks.set("first", { ...previous, done: true });
```

Эти get и set — две отдельные операции. Это не атомарный read-modify-write: для группы заранее известных изменений используйте batch.

## Список и атомарный batch

```ts
await tasks.writeBatch([
  { type: "set", key: "first", value: { text: "Первая", done: false } },
  { type: "set", key: "second", value: { text: "Вторая", done: false } },
  { type: "delete", key: "old" },
]);

const page = await tasks.list({ prefix: "project:", limit: 50 });
const last = page.at(-1);
const next = last
  ? await tasks.list({ prefix: "project:", start: { key: last.key, inclusive: false }, limit: 50 })
  : [];
```

Batch подтверждается целиком. Последняя операция одного ключа побеждает; вся группа проверяется до записи. Максимум 1024 операции и 32 MiB input с учётом кодирования и namespace. Значение ограничено 16 MiB, physical key — 64 KiB вместе с namespace.

`list` возвращает `{ key, value }[]`. По умолчанию выдаётся максимум 100 записей; limit можно задать от 0 до 1000, общий byte response ограничен 32 MiB. Есть prefix, start/end с явным inclusive и reverse. Сортировка идёт по байтам UTF-8, без локали и Unicode normalization. Перелистывание не является snapshot-транзакцией между запросами.

## Подписки и React

```ts
const stop = tasks.subscribe("first", () => {
  void tasks.get("first").then(renderTask).catch(showError);
});
const stopList = tasks.subscribeRange({ prefix: "project:" }, () => {
  void tasks.list({ prefix: "project:" }).then(renderList).catch(showError);
});
// При завершении использования:
stop();
stopList();
```

Сами callbacks синхронные и сообщают об успешном локальном commit; чтение выполняется отдельно. Подписка не выдаёт начальное значение, не связывает разные вкладки/подключения и не уведомляет от maintenance. За один batch подходящая регистрация вызывается один раз.

В React начальное чтение и последующие обновления объединены:

```tsx
import { useDBValue } from "hoke/react";

function TaskView() {
  // tasks — стабильный экземпляр коллекции, созданный вне render.
  const state = useDBValue(tasks, "first");
  if (state.status === "loading") return <p>Загрузка…</p>;
  if (state.status === "error") return <p>Не удалось прочитать задачу.</p>;
  return <p>{state.value?.text ?? "Задачи нет"}</p>;
}
```

Хук подписывается до чтения, игнорирует устаревшие ответы при смене ключа или новом commit и очищает подписку при unmount. При SSR возвращается loading; Worker во время импорта или server render не создаётся. Создавайте DB в клиентском lifecycle и передавайте стабильную коллекцию компонентам. Unmount хука не закрывает общую базу.

## Ошибки и завершение

- `DocumentError` с INVALID_VALUE: запись не соответствует строгому JSON или validator.
- INVALID_DATA: сохранённые байты не являются допустимым JSON/UTF-8 или не проходят схему. Исходная запись не перезаписывается.
- INVALID_ARGUMENT: неверный ключ, диапазон, batch или определение коллекции.
- `DBError`: transport, locking, capacity и storage errors низкоуровневой базы.
- `DBNotificationError` с `committed: true`: данные сохранены, callback подписчика завершился ошибкой. Повторять запись из-за этого не нужно.

`openDatabase(name,{timeoutMs,signal})` и операции поддерживают прежние правила AbortSignal. До отправки отмена не закрывает соединение; после отправки abort/timeout может завершить весь Worker. Mutation могла сохраниться целиком. Автоматического retry нет: откройте базу заново и проверьте состояние. Отмена ожидающей в document-очереди операции проверяется при наступлении её очереди; это не мгновенное удаление из очереди.

Все async вызовы через одну document DB исполняются последовательно, включая вызовы разных коллекций. `await db.close()` перестаёт принимать новые операции, дожидается уже принятых и закрывает файл; повторное закрытие безопасно. Для React обычно закрывайте базу после размонтирования потребителей.

## Что обслуживается автоматически

Перед непустой записью слой проверяет stats и выполняет полную compaction при 32 активных таблицах или 48 WAL segments. Это происходит до requested mutation без повторного выполнения записи. Пользователю не нужно вручную вызывать flush/compact для контроля числа источников. Цена простой политики — дополнительный Worker round trip на запись.

Это логическое обслуживание: физический append-only файл пока не уменьшается. Квота, staging limits и ошибки I/O не скрываются. Миграции схемы JSON-коллекций, совместное владение несколькими вкладками и React Native ещё не реализованы.

Нужны secure context, Dedicated Worker и OPFS synchronous handles. `openDatabase` использует тот же container namespace, что `openDB(name,{mode:"container-v1"})`. Прежний WAL с тем же именем остаётся отдельной базой, автоматического переноса нет. Если записывать в document namespace через byte API, именно приложение отвечает за совместимый UTF-8/JSON.

После `pnpm demo` откройте [демо задач](http://127.0.0.1:8000/examples/documents/): добавьте, отметьте и удалите задачу, сохраните пару, перезагрузите страницу. [Архитектура](ARCHITECTURE.md) · [Byte API и гарантии](OPFS.md) · [ADR решения](adr/0021-document-database-usability.md).
