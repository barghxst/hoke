# Hoke

Hoke — библиотека локального хранения данных внутри приложения. Сейчас есть два отдельных API: простой KV для настроек и экспериментальная база с собственным движком на Rust.

| API          | Для чего                                                | Где хранятся данные                        | Что выполняет код               |
| ------------ | ------------------------------------------------------- | ------------------------------------------ | ------------------------------- |
| `hoke/kv`    | Настройки, флаги, небольшие значения                    | localStorage (web) или MMKV (RN)           | TypeScript                      |
| `hoke/react` | React-подписки на KV-стор или документ DB               | Использует выбранный KV/DB                 | TypeScript + React              |
| `hoke/db`    | Типизированные JSON-коллекции и низкоуровневый byte API | Файл OPFS в браузере                       | TypeScript → Worker → Rust/WASM |
| `hoke-core`  | Сам движок и его native CLI                             | Обычные файлы или явное хранилище в памяти | Rust                            |

KV, его версии схемы и миграции уже работают. Для DB используйте `openDatabase`: строковые ключи, JSON-объекты, runtime-валидация, atomic batch и автоматическое логическое обслуживание. `useDBValue` связывает документ с React. Низкоуровневый `openDB` сохранён отдельно; его default остаётся `wal-v1`.

Пакет ещё не опубликован. KV разделён на web и React Native backend; запуск на настоящих iOS/Android пока не проверен. React Native DB ещё не реализована. База экспериментальная; расширенная проверка восстановления после сбоев остаётся следующим этапом.

## Зачем здесь Rust

В режиме DB Hoke самостоятельно решает, как представить записи в файле, когда подтвердить запись и как восстановить состояние после прерванной операции. Этот код находится в `crates/hoke-core`. Он использует стандартную библиотеку Rust, упорядоченную map в памяти и собственные форматы WAL/SST/container; готовая SQLite или RocksDB внутри не используется.

Rust-ядро не знает о React, DOM или OPFS. Оно получает интерфейс байтового хранилища: читать, писать, менять длину, синхронизировать. На компьютере этот интерфейс работает с обычными файлами. В браузере тот же код собирается в WebAssembly, а адаптер передаёт операции файлу OPFS из Worker.

Движок устроен по принципу LSM: новые изменения попадают в журнал и память, затем записываются в отсортированные неизменяемые таблицы. Compaction объединяет таблицы и убирает устаревшие версии. Сейчас это простой вариант без уровней LSM и фоновой compaction. В контейнере compaction пока не уменьшает физический файл.

**`hoke/kv` Rust не запускает.** Это самостоятельный лёгкий путь для настроек. Его сторы и React-хуки пока не являются обёртками над DB.

[Как устроен проект: компоненты, путь записи и карта кода](docs/ARCHITECTURE.md)

## Попробовать

Текущая среда разработки: Node.js 24, pnpm 11.22.0, Rust 1.97.1, React 19.3.x. Для WASM нужны target и совпадающий wasm-bindgen CLI:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
pnpm install
pnpm demo
```

`pnpm demo` собирает проект, запускает локальный сервер и сам открывает браузер с меню примеров. В консоли выводятся кликабельные HTTP-ссылки на главное меню, список задач, настройки и byte DB; Python для запуска демо не нужен. По умолчанию используется порт 8000, занятый порт заменяется следующим свободным. Данные браузера привязаны к адресу и порту, поэтому для повторного доступа к прежней базе используйте тот же адрес.

Без открытия браузера: `pnpm demo --no-open`. Свой порт: `pnpm demo --port 8080`; `--strict-port` запрещает автоматическую замену. Для готовой сборки: `pnpm demo --no-build`. Остановка: Ctrl+C. Команда `pnpm demo --help` показывает все параметры.

Простой KV с платформенным backend:

```ts
import { KV } from "hoke/kv";

const settings = new KV<string>({ name: "settings" });
settings.set("theme", "dark");
console.log(settings.get("theme"));
```

В браузере KV по умолчанию использует localStorage, в React Native — MMKV. `name` задаёт пространство ключей. JSON-кодирование и подписки общие, свой синхронный backend можно передать через `backend`. Публичного memory backend нет. Для RN нужны `react-native-mmkv` и `react-native-nitro-modules`; нативная сборка приложения обязательна. Импорт модуля не обращается к localStorage, создание KV без custom backend в среде без localStorage завершается ошибкой. [Контракт backend и примеры](docs/USAGE.md).

База в браузере без ручного кодирования байтов:

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

const db = await openDatabase("app");
const tasks = db.collection("tasks", { validate: isTask });
await tasks.set("first", { text: "Попробовать Hoke", done: false });
const task = await tasks.get("first");
const rows = await tasks.list({ limit: 100 });
await db.close();
```

Проверка `validate` нужна и при записи, и при чтении сохранённых данных. Даты/BigInt и другие значения вне строгого JSON явно отклоняются. Для строк и произвольных байтов остаётся `openDB`. Нужны secure context, Dedicated Worker и OPFS sync handles. [Полный пример, batch, React и обработка ошибок](docs/DATABASE.md).

## Где читать дальше

- [Архитектура и карта исходников](docs/ARCHITECTURE.md) — начать здесь, чтобы понять движок.
- [Удобный DB API и React](docs/DATABASE.md).
- [Примеры KV, сторов, React и native CLI](docs/USAGE.md).
- [Browser DB: API, OPFS и ограничения](docs/OPFS.md).
- [Запуск проверок и ручные сценарии](docs/TESTING.md).
- [План](docs/PLAN.md), [фактический прогресс](docs/PROGRESS.md), [решения ADR](docs/adr/), [изменения](CHANGELOG.md).

`pnpm check` проверяет JS/WASM, установленный tarball, Rust и benchmark harness. Полный прогон с установленным Chrome: `HOKE_BROWSER_CHANNEL=chrome pnpm check:all`. Другие браузеры, ОС и физическая потеря питания пока не проверены.
