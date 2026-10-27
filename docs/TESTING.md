# Как запускать и проверять Hoke

## 1. Проверить проект целиком

Для полной проверки нужны Node.js 24, pnpm 11.22.0 и Rust с rustfmt/Clippy (проверено на Rust 1.97.1, Linux). Все команды выполняются из корня репозитория. Только JS-часть можно проверить через pnpm check:js.

```sh
cd /home/barghest/hoke
pnpm install --frozen-lockfile
pnpm check
```

Успешный результат: Biome без ошибок и предупреждений, сборка и проверка типов без ошибок, все тесты зелёные, в конце `Tarball OK`.

`pnpm check` выполняет по порядку:

1. `pnpm lint` — форматирование, линтинг и порядок импортов через Biome.
2. `pnpm build` — JavaScript и TypeScript declarations.
3. `pnpm typecheck` — типы исходников, примеров и тестов.
4. `pnpm test` — unit/integration tests.
5. `pnpm test:package` — упаковка и установка tarball во временный проект, проверка публичных импортов и типов без React, затем React entry point и SSR. Для установки зависимостей нужен доступ к npm registry; временные файлы удаляются после проверки.

После JS-части выполняется `pnpm check:rust`: cargo fmt --check, строгий Clippy и cargo test --workspace --locked. В GitHub CI JS- и Rust-проверки разделены на jobs; Rust job использует проверенную версию 1.97.1. Сам workflow пока не выполнялся на GitHub.

## 2. Запустить демо в браузере

```sh
pnpm demo
```

Команда собирает пакет и запускает HTTP-сервер Python 3. Оставьте терминал открытым и перейдите на http://127.0.0.1:8000/examples/web/.

Демо использует настоящий localStorage браузера. Оно показывает KV и Store; React-хуки сейчас проверяются отдельными автоматическими тестами.

| Действие                                | Ожидаемый результат                                                                                                        |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| Нажать «Удалить данные демо»            | Счётчик 0, тема «Системная», сохранённая запись отсутствует.                                                               |
| Нажать «Добавить 1», выбрать «Тёмная»   | Счётчик 1, тема тёмная, в записи видны version 2 и новые значения.                                                         |
| Перезагрузить страницу                  | Счётчик и тема остаются сохранёнными.                                                                                      |
| Нажать «Вернуть defaults»               | Счётчик 0, системная тема, запись удалена.                                                                                 |
| Нажать «1. Создать и показать v1»       | В сохранённой записи видны version 1, darkMode true и count 7. Миграция ещё не выполнена, обычные настройки заблокированы. |
| Перезагрузить страницу с v1             | Запись остаётся v1 и показывается без автоматической миграции.                                                             |
| Нажать «2. Мигрировать v1 → v2»         | Теперь запись имеет version 2, theme dark вместо darkMode и count 7. Настройки снова доступны.                             |
| Нажать «Загрузить v3 → получить ошибку» | Ошибка future-version, основные элементы управления заблокированы, запись остаётся с version 3 и count 99.                 |
| Нажать «Удалить данные демо»            | Ошибка исчезает, снова доступны defaults и обычные изменения.                                                              |

В DevTools → Application/Storage → Local Storage запись имеет ключ `stowtree:["store-demo","settings"]`. Демо меняет только своё пространство ключей. Откройте Console: при обычных действиях неожиданных ошибок быть не должно.

При ошибке доступа к localStorage демо показывает ошибку вместо перехода в память. Обновление из другой вкладки само не вызывает подписчиков: multi-tab пока не реализован.

Остановить сервер: Ctrl+C. Если порт 8000 занят, запустите `python3 -m http.server 8001 --bind 127.0.0.1` после `pnpm build` и откройте соответствующий адрес.

## 3. Работать над кодом

```sh
pnpm format     # отформатировать JS/TS/HTML/docs/YAML
pnpm format:rust # отформатировать Rust через rustfmt
pnpm lint       # проверить формат, правила и импорты без изменения файлов
pnpm lint:fix   # применить безопасные исправления и форматирование
pnpm test       # разовый запуск тестов
pnpm exec vitest # тесты в watch-режиме
```

После изменения кода при уже открытом демо выполните в другом терминале `pnpm build`, затем перезагрузите страницу. Автоматической пересборки демо пока нет.

Конфигурация Biome — `biome.json`: два пробела, LF, двойные кавычки, semicolons, ширина 100. Форматируются также HTML, документация и YAML. node_modules, dist, coverage, Rust target и сгенерированный pnpm-lock.yaml исключены. Режим HTML в установленной версии Biome экспериментальный; он включён явно.

Тестовые overrides разрешают non-null assertions в заранее созданных fixtures и определения React-компонентов внутри test cases. В коде библиотеки эти правила не отключены. Единственное локальное подавление noThenProperty относится к намеренно созданному thenable в тесте отказа миграции.

## 4. Что делать дальше

Сначала пройти таблицу в настоящем браузере и зафиксировать браузер/версию и результат. Автотесты уже покрывают эту последовательность в jsdom, но это не реальный браузер.

Если сценарий расходится с таблицей — записать шаги, текст ошибки и содержимое записи, исправить дефект с регрессионным тестом. По решению пользователя начат Rust-этап: Storage trait, WAL v1 и process-crash recovery реализованы, см. ADR 0002 и WAL_FORMAT.md. Полная проверка веб-сценария в реальном браузере остаётся отдельным пунктом. SSTable/manifest и ручной flush реализованы по ADR 0003, сценарий ниже. Scan/range и ручные compaction/GC реализованы; atomic write batch реализован через manifest, см. раздел 9.

Для публикации отдельно остаются лицензия, владелец npm/GitHub, версия и метаданные. Для локальной разработки аккаунты и публикация не требуются.

## 5. Проверить Rust WAL в отдельных процессах

```sh
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal put theme dark
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal get theme
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal delete theme
cargo run -p hoke-core --bin hoke -- /tmp/hoke-example.wal get theme
```

По порядку: `OK put (synced)`, `value="dark"`, `OK delete (synced)`, `NOT_FOUND`. Каждый вызов заново открывает файл: второй и четвёртый читают состояние из журнала, а не память предыдущего процесса. В stderr выводятся количество восстановленных записей и число отброшенных байтов неполного хвоста. При повторении сценария WAL продолжает расти — compaction ещё нет.

```sh
pnpm check:rust
cargo test -p hoke-core --test process -- --nocapture
```

Три process-сценария: второй процесс не может захватить lock; подтверждённая запись переживает kill; kill после частичной записи нового frame сохраняет предыдущие данные и допускает последующий append после recovery. Тестовый child_driver — вспомогательный entry point дочернего процесса. Эти проверки используют принудительное завершение процессов на Unix, а не потерю питания устройства.

WAL-only API пока доступен только в Rust. Установленный JS-пакет и браузерное демо по-прежнему работают на KV/localStorage.

## 6. Проверить SSTable и безопасный flush

Для чистого сценария выберите новый каталог; команды ниже рассчитаны на первый запуск /tmp/hoke-db. Не удаляйте существующую базу для повторения: можно выбрать другое имя каталога.

```sh
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db put theme dark
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db info
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db flush
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db get theme
```

До flush: memtable_entries=1, tables=0. После: memtable_entries=0, tables=1, wal_bytes=16. Get вернёт value="dark", хотя memtable уже пуста: значение прочитано из SST в новом процессе. В каталоге появились MANIFEST, LOCK и файлы .sst/.wal.

```sh
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db delete theme
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db flush
cargo run -p hoke-core --bin hoke -- --db /tmp/hoke-db get theme
```

Ожидается NOT_FOUND. Старая таблица всё ещё содержит theme, но tombstone в новой запрещает его возвращать. Повторный пустой flush не создаёт файлы или новые поколения. После flush суммарное место само не уменьшается: для old WAL/orphan cleanup и слияния SST выполните compact, см. раздел ниже.

```sh
cargo test -p hoke-core --test flush
cargo test -p hoke-core --test flush_process -- --nocapture
```

Fault harness проверяет каждый шаг flush, включая ошибку после видимой записи bytes, неизвестный исход rename, poisoned до reopen, missing references и модель из 200 операций. Process tests останавливают child до manifest rename и после rename перед directory sync; подтверждённые update/delete сохраняются в обоих случаях. Это проверки Linux/process crash, не физических отказов накопителя.

## 7. Проверить scan/range

Подробный сценарий с ключами user:1/user:2/user:3 и ожидаемой выдачей — [SCANS.md](SCANS.md). Он специально сочетает данные SST и memtable, перезапись и удаление: user:1 обновляется, user:2 скрывается tombstone, user:3 добавляется. Проверьте prefix, CLI range [start,end), reverse+limit и повторение после flush.

```sh
cargo test -p hoke-core --test scan
cargo test -p hoke-core --test scan_cli
```

По умолчанию scan возвращает до 100 живых ключей. Пустая выдача CLI печатается как EMPTY. limit0 не читает values при scan, но открытие базы в CLI всё равно проверяет manifest/индексы и восстанавливает WAL.

## 8. Проверить compaction/GC

Выполните [сценарий с отдельным временным каталогом](COMPACTION.md). Он создаёт две SST, перезапись, tombstone и unflushed значение; compact должен оставить одну SST и пустой WAL, сохранив два живых ключа. get удалённого ключа остаётся NOT_FOUND после reopen. Повторный gc удаляет 0 файлов. Автоматические проверки: compaction, compaction_process и cli_compaction; затем полный pnpm check.

## 9. Проверить atomic batch

Выполните [сценарий в отдельном временном каталоге](BATCHES.md). Одна команда batch публикует theme/count и удаление old; ранее подтверждённый pending из WAL остаётся после reopen. Неверный последний аргумент оставляет прежние данные. Тесты batch/batch_process/cli_batch проверяют all-or-none при ошибках и убийстве процесса, затем выполняется полный pnpm check.

## 10. Проверить автоматический flush

[Сценарий с порогом 8 bytes](MEMTABLE.md): первый put a=1234 оставляет 5 bytes delta без SST. Второй put b=5678 с тем же порогом предварительно сбрасывает a: теперь одна SST и 5 bytes delta. Оба значения читаются после reopen. Порог задаётся перед командой и не сохраняется между CLI-процессами; без флага режим остаётся ручным. Проверки threshold/threshold_process/cli_threshold и полный pnpm check.

## 11. Проверить порог WAL

[Сценарий с --wal-limit 90](WAL_THRESHOLD.md): две перезаписи a со значением длиной4 дают WAL53 и90 без SST, третья предварительно сбрасывает delta и оставляет WAL53/одну SST/последнее значение. Порог учитывает framing, даже если payload memtable не растёт. Тесты wal_threshold/wal_threshold_process/cli_wal_threshold и полный pnpm check.
