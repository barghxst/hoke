# Hoke

Hoke — библиотека локального хранения данных внутри приложения. Сейчас есть два отдельных API: простой KV для настроек и экспериментальная база с собственным движком на Rust.

| API          | Для чего                                                     | Где хранятся данные                        | Что выполняет код               |
| ------------ | ------------------------------------------------------------ | ------------------------------------------ | ------------------------------- |
| `hoke/kv`    | Настройки, флаги, небольшие значения                         | Память или localStorage                    | TypeScript                      |
| `hoke/react` | Подписка React-компонента на KV-стор                         | Использует выбранный KV                    | TypeScript + React              |
| `hoke/db`    | Упорядоченные byte keys/values, диапазоны, атомарные batches | Файл OPFS в браузере                       | TypeScript → Worker → Rust/WASM |
| `hoke-core`  | Сам движок и его native CLI                                  | Обычные файлы или явное хранилище в памяти | Rust                            |

KV, его версии схемы и миграции уже работают. Browser DB умеет чтение, запись, scan, коллекции, подписки и отмену; явный режим `container-v1` добавляет atomic batch, flush и compaction. По умолчанию пока остаётся прежний `wal-v1`.

Пакет ещё не опубликован. React Native запланирован, но не реализован. База экспериментальная; расширенная проверка восстановления после сбоев остаётся следующим этапом.

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

Откройте [демо KV и миграций](http://127.0.0.1:8000/examples/web/) или [демо DB в OPFS](http://127.0.0.1:8000/examples/opfs/). Если порт 8000 занят, после `pnpm build` можно запустить `python3 -m http.server 8001 --bind 127.0.0.1` и открыть тот же путь на 8001.

Простой KV в памяти:

```ts
import { createKV } from "hoke/kv";

const settings = createKV<string>();
settings.set("theme", "dark");
console.log(settings.get("theme"));
```

База в браузере с сохранением batch:

```ts
import { openDB } from "hoke/db";

const bytes = new TextEncoder();
const db = await openDB("app", { mode: "container-v1" });
await db.writeBatch([
  { type: "put", key: bytes.encode("theme"), value: bytes.encode("dark") },
  { type: "put", key: bytes.encode("language"), value: bytes.encode("ru") },
]);
const theme = await db.get(bytes.encode("theme"));
console.log(theme === undefined ? undefined : new TextDecoder().decode(theme));
await db.close();
```

DB хранит байты; преобразование строк или JSON выбирает приложение. Нужны secure context, Dedicated Worker, OPFS sync handles и доступные JS/WASM assets. При timeout/abort после отправки запись могла завершиться: нужно открыть базу заново и прочитать состояние, автоматического retry нет.

## Где читать дальше

- [Архитектура и карта исходников](docs/ARCHITECTURE.md) — начать здесь, чтобы понять движок.
- [Примеры KV, сторов, React и native CLI](docs/USAGE.md).
- [Browser DB: API, OPFS и ограничения](docs/OPFS.md).
- [Запуск проверок и ручные сценарии](docs/TESTING.md).
- [План](docs/PLAN.md), [фактический прогресс](docs/PROGRESS.md), [решения ADR](docs/adr/), [изменения](CHANGELOG.md).

`pnpm check` проверяет JS/WASM, установленный tarball, Rust и benchmark harness. Полный прогон с установленным Chrome: `HOKE_BROWSER_CHANNEL=chrome pnpm check:all`. Другие браузеры, ОС и физическая потеря питания пока не проверены.
