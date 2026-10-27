# hoke

Хранилище данных для веба и React Native: простой KV для настроек и будущая встроенная база на общем Rust-ядре.

Сейчас работает прототип этапа A: KV в памяти/localStorage, реактивные сторы с версиями и миграциями, React-хуки. В Rust работают WAL Engine и Database с SSTable, manifest, настраиваемым flush, scan, compaction и атомарным batch; Экспериментальный hoke/db уже работает через Rust/WASM/Worker/OPFS в режиме WAL-only (get/put/delete/scan/close и byte collections); браузерные SST/batch, React Native и devtools ещё не реализованы. Пакет не опубликован.

[Подробный план](docs/PLAN.md) · [Ход работ](docs/PROGRESS.md) · [Решения по API](docs/adr/0001-store-and-react-contract.md) · [Changelog](CHANGELOG.md)

[Как запускать и проверять: команды и ручные сценарии](docs/TESTING.md)

## Запуск и проверка

Проверенная среда разработки: Node.js 24, pnpm 11.22.0, Rust 1.97.1 (rustfmt и Clippy). React-хуки протестированы на React 19.3.x. Более широкий диапазон поддержки пока не заявлен.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
pnpm install
pnpm check
node examples/web/smoke.ts
```

`pnpm check` выполняет JS- и Rust-проверки. Отдельно: `pnpm check:js` и `pnpm check:rust`. JS-часть проверяет код через Biome, собирает пакет, проверяет типы, запускает тесты и устанавливает tarball во временный проект. Внешний потребитель проверяет declarations, публичный импорт и работу KV/store без установленного React.

Для демо после сборки:

```sh
pnpm demo
```

Откройте http://127.0.0.1:8000/examples/web/.

1. Измените тему и счётчик, перезагрузите страницу: данные сохраняются.
2. Нажмите «Вернуть defaults»: запись удалится, стор вернёт значения по умолчанию.
3. «1. Создать и показать v1» сохранит старую запись с darkMode. Она видна внизу и остаётся v1 после перезагрузки. Нажмите отдельно «2. Мигрировать v1 → v2»: darkMode превратится в theme, а версия — в 2.
4. «Загрузить v3» покажет future-version и оставит новую запись нетронутой.
5. «Удалить данные демо» — явное удаление записи через KV для восстановления примера.

## KV, адаптер и стор

- **KV** хранит значения по строковым ключам и уведомляет об изменениях одного ключа.
- **Адаптер** отвечает за физическое хранение: память или localStorage.
- **Стор** задаёт схему, defaults и миграции одного значения поверх KV.
- **React-хук** подписывается на snapshot стора или результат селектора.

```ts
import { createKV } from "hoke/kv";

const kv = createKV<number>(); // отдельное хранилище в памяти
const unsubscribe = kv.subscribe("count", () => console.log(kv.get("count")));
kv.set("count", 1);
kv.delete("count");
unsubscribe();
```

Операции синхронные. get отсутствующего ключа возвращает undefined. Подписки вызываются после успешной записи, не вызываются при регистрации и относятся только к одному экземпляру KV. Повторная запись сравнивается через Object.is; удаление отсутствующего ключа не уведомляет подписчиков. Каждая подписка независима, отписку можно вызывать повторно.

## Сторы и сохранение

```ts
import { createKV, createLocalStorage, defineStore, createStore } from "hoke/kv";

type Settings = { theme: "system" | "light" | "dark"; count: number };
const isSettings = (value: unknown): value is Settings =>
  typeof value === "object" &&
  value !== null &&
  "theme" in value &&
  ["system", "light", "dark"].includes(String(value.theme)) &&
  "count" in value &&
  typeof value.count === "number" &&
  Number.isSafeInteger(value.count) &&
  value.count >= 0;

const settings = defineStore<Settings>({
  key: "settings",
  version: 1,
  defaults: () => ({ theme: "system", count: 0 }),
  validate: isSettings,
});

// Адаптер читает raw JSON/envelopes; схему значения проверяет Store.
// Проверять здесь только текущую Settings нельзя: миграции должны прочитать старую схему.
const kv = createKV<unknown>({
  storage: createLocalStorage<unknown>({
    name: "my-app",
    validate: (value: unknown): value is unknown => true,
  }),
});
const store = createStore(kv, settings);
const unsubscribe = store.subscribe(() => console.log(store.getSnapshot()));
store.update((previous) => ({ ...previous, count: previous.count + 1 }));
store.set({ theme: "dark", count: 5 });
store.reset();
unsubscribe();
```

Пустой стор возвращает стабильные defaults без записи. defaults — фабрика, создающая отдельное начальное значение для каждого экземпляра. set сохраняет envelope `{ __stowtree: 1, version, value }` одной операцией KV. reset удаляет запись. Несколько сторов над одним KV получают общие уведомления.

Считайте все snapshots и переданные значения immutable. В памяти объекты хранятся по ссылке. В localStorage повторное чтение неизменной JSON-строки возвращает тот же объект; каждый get всё равно читает backend и обнаруживает внешнее изменение. Изменение объекта без set не является обновлением стора. update получает защищённую structuredClone-копию: для объектов/массивов используйте возврат нового значения, например `{ ...previous, count: 1 }`. Значения для update и миграций должны поддерживать structuredClone.

## Миграции

```ts
type SettingsV2 = Settings & { compact: boolean };
const isSettingsV2 = (value: unknown): value is SettingsV2 =>
  isSettings(value) && "compact" in value && typeof value.compact === "boolean";

const settingsV2 = defineStore<SettingsV2>({
  key: "settings",
  version: 2,
  defaults: () => ({ theme: "system", count: 0, compact: false }),
  validate: isSettingsV2,
  migrations: {
    2: (previous: unknown) => {
      if (!isSettings(previous)) throw new Error("Invalid settings v1");
      return { ...previous, compact: false };
    },
  },
});
const migrated = createStore(kv, settingsV2);
```

Номера версий — положительные безопасные целые числа. migrations[2] переводит v1 в v2, migrations[3] — v2 в v3. Цепочка запускается при createStore, все шаги обязательны. После завершения проверяется текущая схема и сохраняется одна конечная запись. Промежуточные версии не пишутся. Миграции чистые и синхронные: Promise/thenable отклоняются.

Если шага нет, преобразование/валидация не прошли или backend отклонил запись, исходные данные сохраняются. Исключение подписчика происходит уже после commit и не означает rollback. Изменённая извне старая версия в уже открытом сторе вызывает migration-required: откройте стор заново. Будущая версия вызывает future-version, включая попытки set/update/reset из старого экземпляра. Автоматического downgrade нет.

## React

```ts
import { createElement } from "react";
import { useStore, useValue } from "hoke/react";

function Counter() {
  const count = useStore(store, (settings) => settings.count);
  return createElement(
    "button",
    {
      onClick: () => store.update((previous) => ({ ...previous, count: previous.count + 1 })),
    },
    count,
  );
}

// Полный snapshot:
// const settings = useValue(store);
// Объектный selector с собственным сравнением:
// const selected = useStore(store, s => ({ theme: s.theme }), (a, b) => a.theme === b.theme);
```

Создавайте KV/store вне рендера компонента либо сохраняйте экземпляр на весь lifecycle. По умолчанию селекторы сравниваются через Object.is; изменение невыбранного поля не перерисовывает потребителя. Для selector, создающего новый объект, можно передать isEqual.

Хуки используют useSyncExternalStore. Его требования к стабильным snapshots и SSR описаны в [документации React](https://react.dev/reference/react/useSyncExternalStore). getServerSnapshot отдаёт стабильные defaults; defaults на сервере и клиенте должны совпадать. Сохранённое значение появляется после hydration. В SSR создавайте memory KV на запрос; localStorage-адаптер открывайте только на клиенте.

React — optional peer dependency. Импорт hoke/kv не загружает React. Импорт hoke/react требует React из приложения.

## localStorage: ограничения и ошибки

Для простого KV без Store передайте валидатор собственного значения прямо в createLocalStorage. name — непустое пространство ключей; физический ключ выглядит как `stowtree:["my-app","count"]`. Разные имена изолированы.

Поддерживаются JSON-значения: строки, конечные числа кроме -0, boolean, null, плотные массивы и простые объекты. undefined, NaN, Infinity, bigint, функции, циклы, Date, Map, символы, геттеры и скрытые свойства отклоняются, чтобы JSON не менял данные молча. Для отсутствующего значения используйте delete. Повторная запись объекта может уведомить подписчиков даже при равном содержимом.

KVStorageError.code: UNAVAILABLE, READ_FAILED, WRITE_FAILED, DELETE_FAILED, INVALID_DATA, INVALID_VALUE. Исходная ошибка хранится в cause. Без доступного localStorage нет перехода в память. Модуль можно импортировать в SSR: браузерные globals читаются только при создании адаптера.

StoreError.code: invalid-definition, invalid-defaults, invalid-envelope, invalid-value, future-version, migration-required, missing-migration, migration-failed, async-result, update-failed, clone-failed.

Ошибка backend не отдаёт устаревший snapshot и не вызывает подписчиков. Ошибки самих подписчиков собираются в AggregateError после вызова остальных подписчиков; запись к этому моменту выполнена. Не повторяйте update автоматически после AggregateError: значение уже могло измениться.

Повреждённые записи не затираются автоматически. Для ручного восстановления можно удалить конкретную сырую запись через инструменты браузера. Это не обещание транзакций между ключами: localStorage не является LSM-движком.

## Проверки и известные границы

- Тесты KV/storage/store: повреждение данных, квота/доступ, стабильность snapshots, миграции и провал commit.
- React DOM tests в jsdom: selectors, смена store, StrictMode, unsubscribe, SSR и hydration.
- Tarball: exports, declarations, установленные JS-модули без React и browser globals.
- Полная проверка в настоящем браузере пока не выполнена: локальный инструмент браузера не смог запустить runtime. jsdom не заменяет проверку реального браузера.
- Уведомления между вкладками и разными экземплярами KV пока отсутствуют. get обнаружит изменённую raw запись, но внешнее изменение само не разбудит подписчика.
- Нативные платформы, настоящий browser matrix и будущие гарантии DB пока не заявляются.

Пакет оставлен private. До публикации нужно определить лицензию, владельца npm/GitHub, финальную версию и метаданные. Автоматической публикации нет.

## Rust: первый движок с WAL

Это отдельный crate `hoke-core`, ещё не подключённый к веб-демо. Он записывает операции в бинарный журнал, синхронизирует файл перед подтверждением и при открытии восстанавливает актуальные значения в память.

```sh
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal put theme dark
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal get theme
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal delete theme
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal get theme
```

Ожидаемый результат: OK put, value="dark", OK delete, NOT_FOUND. Каждый вызов запускает отдельный процесс, поэтому get читает восстановленные из WAL данные. Состояние recovery выводится в stderr.

[Решение о WAL](docs/adr/0002-native-storage-and-wal.md) · [Бинарный формат](docs/WAL_FORMAT.md) · [Rust crate](crates/hoke-core/README.md). CRC32 проверяет повреждения; неполный последний append удаляется, полная повреждённая запись вызывает ошибку без очистки файла. После I/O-ошибки записи движок требует reopen. Новые данные после ошибки могут оказаться на диске, даже если вызывающий код не получил успех.

Старый WAL-only Engine по-прежнему хранит данные в памяти. Для хранения значений в SST используйте Database/режим --db ниже. Интерактивные транзакции и JS API DB ещё предстоят. Process-kill тесты не доказывают устойчивость к потере питания; платформы кроме Linux пока не проверены.

## Rust: SSTable и flush

Режим `--db` использует отдельный каталог. Старые команды с путём к одному WAL остаются прежними.

```sh
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db put theme dark
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db info
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db flush
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db get theme
```

На новом каталоге info до flush покажет memtable_entries=1, tables=0. После flush: memtable_entries=0, tables=1, wal_bytes=16. Значение dark читается из SST после повторного открытия. Flush не является первой записью на диск: put уже синхронизирован через WAL; flush переносит delta в отсортированный файл и освобождает значения memtable.

Database читает сначала memtable, затем SST от новой к старой. Delete сохраняет tombstone, поэтому удалённое значение не возвращается из старой таблицы. Заголовки/индексы проверяются при open, полный CRC значения — при get. Индексы ключей остаются в памяти; во время flush создаётся копия delta.

[Протокол flush](docs/adr/0003-sstable-and-flush.md) · [Форматы SST/manifest](docs/SST_FORMAT.md). Новые файлы синхронизируются до atomic manifest rename. После ошибки flush нужны drop/reopen. Старые WAL и orphan files удаляются явными compact/gc; flush сам очистку не запускает. Проверки включают отказ каждого шага flush и kill процесса до/после manifest rename.

## Rust: scan/range

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-db scan --limit 10
cargo run -q -p hoke-core -- --db /tmp/hoke-db prefix theme
cargo run -q -p hoke-core -- --db /tmp/hoke-db range a z --reverse --limit 10
```

Scan сливает ключи memtable и SST, учитывает новые значения и tombstones. Values читаются только для выбранных версий. Default limit 100; Rust API поддерживает Included/Excluded с обеих сторон и пересечение с prefix. CLI range — [start,end). Форматы файлов не менялись, JS DB API пока не подключён. [Сценарий проверки с ожидаемым выводом](docs/SCANS.md).

## Rust: compaction и GC

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-db compact
cargo run -q -p hoke-core -- --db /tmp/hoke-db gc
```

compact сливает SST и текущую memtable, сохраняет последние значения и удаляет покрытые tombstones. После durable manifest publication удаляются obsolete SST/WAL/orphans. gc повторяет только очистку. Ошибки cleanup показываются отдельно от публикации данных. Один выходной SST ограничен 1M ключей/64 MiB index; фоновой compaction нет. [Проверка и контракт ошибок](docs/COMPACTION.md).

## Rust: atomic batch

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-batch-db batch put theme dark put count 7 delete old
cargo run -q -p hoke-core -- --db /tmp/hoke-batch-db scan
```

Database::write_batch применяет несколько put/delete целиком через публикацию manifest; операции с одинаковым ключом выполняются по порядку. Первый вариант создаёт новую SST и сбрасывает текущую WAL delta, сохраняя форматы v1. Максимум 1024 операции/32 MiB input. Это гарантия атомарности, ускорение массовых записей не заявляется. [Сценарий проверки, API и ошибки](docs/BATCHES.md).

## Rust: порог memtable

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-db --memtable-limit 8388608 put theme dark
cargo run -q -p hoke-core -- --db /tmp/hoke-db info
```

DatabaseOptions.max_memtable_bytes включает автоматический flush перед записью, которая превысит порог текущей delta. По умолчанию режим ручной. memtable_bytes считает key/value payload и tombstones, не всю память процесса или длину WAL; один большой record может превысить мягкий порог. [API и воспроизводимая проверка на маленьком пороге](docs/MEMTABLE.md).

## Rust: порог активного WAL

```sh
cargo run -q -p hoke-core -- --db /tmp/hoke-db --wal-limit 67108864 put theme dark
```

max_wal_bytes/--wal-limit проверяет длину журнала с новым frame перед append, поэтому срабатывает и при overwrite одного ключа. Можно одновременно задать --memtable-limit; любой порог вызывает один preflush. Default обоих None сохраняет ручной режим. Старые WAL/orphans не входят в этот лимит и удаляются явным gc/compact. [Проверка на WAL из 90 bytes](docs/WAL_THRESHOLD.md).

## Измерения Rust API

`pnpm bench` собирает release и выполняет три независимых прогона. Параметры, сырые замеры и сведения о среде сохраняются вместе; методика и ограничения — [BENCHMARKS.md](docs/BENCHMARKS.md). Переименование пакетов и совместимость данных — [RENAMING.md](docs/RENAMING.md).

## Полная проверка перед релизом

`pnpm check` проверяет JS, установленный tarball, Rust и benchmark harness. Для настоящего браузера установите `pnpm exec playwright install chromium` и выполните `pnpm check:all`. Если Chrome уже установлен: `HOKE_BROWSER_CHANNEL=chrome pnpm check:all`. Локально проверен Chrome 155.0.8059.84/Linux; это отдельный прогон от jsdom. Инструкции и ограничения — [TESTING.md](docs/TESTING.md).

## Rust/WASM в OPFS — экспериментальный C1

`hoke/db` теперь предоставляет async openDB/get/put/delete/close через Dedicated Worker и существующий Rust WAL v1. Демо после `pnpm demo`: http://127.0.0.1:8000/examples/opfs/. Для сборки нужны WASM target и wasm-bindgen CLI0.2.129; команды, API и границы гарантий — [OPFS.md](docs/OPFS.md).

Это WAL-only браузерный срез с одним владельцем файла. Browser scan/collection добавлены в C2. Native SST/compaction/atomic batch не включены в него автоматически; полный этап C ещё предстоит завершить.
