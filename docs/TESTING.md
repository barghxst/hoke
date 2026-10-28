# Как запускать и проверять Hoke

## 1. Проверить проект целиком

Для полной проверки нужны Node.js 24, pnpm 11.22.0 и Rust с rustfmt/Clippy (проверено на Rust 1.97.1, Linux). Все команды выполняются из корня репозитория. Сборка JS-пакета теперь также включает Rust/WASM: нужны target wasm32-unknown-unknown и wasm-bindgen-cli0.2.129. Команды установки ниже.

```sh
cd /home/barghest/hoke
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
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

Таблица проверена автоматическими сценариями в настоящем Chrome 155.0.8059.84. Повторяемая проверка через Playwright описана ниже; jsdom остаётся отдельной проверкой.

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

## 12. Проверить native benchmarks

`pnpm test:bench` и `cargo test -p hoke-core --test benchmark_cli` проверяют harness без требований к скорости. `pnpm bench` собирает release и сохраняет три реальных прогона. Параметры, единицы и ограничения тёплого кэша — [BENCHMARKS.md](BENCHMARKS.md). Полный `pnpm check` включает Python-проверки harness.

## 13. Автоматическая проверка настоящего браузера

Playwright запускает собранное демо в изолированных browser contexts и сам поднимает HTTP-сервер. Установка браузера и запуск:

```sh
pnpm exec playwright install chromium
pnpm test:browser
pnpm check:all
```

На Linux для системных библиотек может потребоваться `pnpm exec playwright install --with-deps chromium`; описание установки — [Playwright](https://playwright.dev/docs/browsers). В CI браузер устанавливается отдельным job; внешнего запуска CI пока не было.

Если Chrome уже установлен, можно проверить его явно:

```sh
HOKE_BROWSER_CHANNEL=chrome pnpm test:browser
```

Именно этот вариант прошёл локально с Chrome 155.0.8059.84/Linux. Bundled Chromium — стандартный вариант конфигурации, его локальный запуск здесь отдельно не подтверждён. Firefox/WebKit и мобильные браузеры не заявлены проверенными.

По умолчанию сервер использует 127.0.0.1:4173 и отклоняет занятый порт. Задать другой: `HOKE_BROWSER_PORT=4174 pnpm test:browser`. Существующий сервер не переиспользуется, чтобы не проверить чужую/старую сборку. Python 3 должен быть доступен.

9 browser tests проверяют реальные module imports, сохранение и reload, видимую v1 до явной миграции, v2 после миграции, сохранность v3, полное удаление reset/clear, повреждённый JSON и SecurityError/отсутствие localStorage без fallback. Очистка повреждённой записи выполняется только явной кнопкой «Удалить данные демо»; обычные чтения сохраняют её. Неожиданные page errors проваливают тест.

При ошибке trace/screenshot сохраняются в игнорируемый test-results; CI прикладывает этот каталог. Данные обычного браузерного профиля пользователя не используются. `pnpm check` выполняет JS/Rust/Python без установленного браузера; `pnpm check:all` добавляет browser suite.

Минимальная Rust-среда проверяется отдельно: `cargo +1.89.0 test --workspace --locked`. Рабочий toolchain для Clippy/rustfmt остаётся 1.97.1; Windows/macOS и физическая потеря питания не проверялись.

## 14. WASM/Worker/OPFS C1

Для сборки установите `rustup target add wasm32-unknown-unknown` и `cargo install wasm-bindgen-cli --version 0.2.129 --locked`. `pnpm build` теперь сначала генерирует wasm assets, затем JS/declarations; публикационный tarball содержит оба каталога. Это build-time требование для checkout, импорт KV/React не загружает WASM.

`pnpm check:js` дополнительно запускает 20 actual-WASM adapter tests через `pnpm test:wasm`. Полный browser suite теперь также проверяет 22 OPFS/WASM scenarios и два OPFS demo scenarios. Данные и ожидаемые исходы — [OPFS.md](OPFS.md). Browser CI устанавливает Rust target/CLI перед сборкой. Старые девять KV-демо тестов остаются отдельными сценариями.

## 15. Проверить browser scan/collection (C2)

Демо OPFS позволяет редактировать key/value и читать список по prefix/reverse/limit; сценарий user:1/user:2 и API коллекций описаны в [OPFS.md](OPFS.md#c2-scan-и-байтовые-коллекции). Весь browser suite: `HOKE_BROWSER_CHANNEL=chrome pnpm test:browser`. Native WAL scan отдельно: `cargo test -p hoke-core --test wal_scan`; actual WASM: `pnpm test:wasm` после сборки.

Лимиты:100 строк по умолчанию, максимум1000, packet32MiB. Большая выдача возвращает явную ошибку и оставляет базу доступной. Worker protocol2 не меняет WAL v1; смешанные версии клиент/worker отвергаются. Результат имеет owned byte arrays; DBCollection выдаёт относительные ключи и разделяет lifecycle базы.

## 16. Подписки DB после commit (C3)

`pnpm test` включает13 registry и91 DB client/collection tests. `HOKE_BROWSER_CHANNEL=chrome pnpm test:browser` включает8 новых OPFS subscription scenarios и UI counter в демо; полный browser suite —42 tests. `pnpm check:all` объединяет их с существующими JS/Rust/WASM/Python/tarball checks.

В OPFS демо откройте базу, задайте prefix user:, включите «Наблюдать prefix», запишите user:1 и other: счётчик должен увеличиться только от первого ключа. Read/scan не увеличивают счётчик; unsubscribe и close прекращают наблюдение. Сохранение одинакового значения и delete отсутствующего ключа после ACK тоже считаются invalidation.

Для ошибок callback проверьте экспортированный DBNotificationError: committed=true означает уже сохранённую mutation, а не необходимость повтора. Реальный Chrome-тест проверяет дальнейшее чтение/reopen и сохранённые bytes после такого reject. Ошибка OPFS flush с unknown outcome не вызывает callback даже при обнаружении записи после recovery. Формат/protocol остаются прежними; [контракт](adr/0012-browser-commit-subscriptions.md).

## 17. Отмена browser DB (C4)

`pnpm test` включает121 DB client/collection tests, из них30 новых регрессионных сценариев отмены. `tests/browser/db-abort.spec.ts` добавляет7 настоящих OPFS cases: отмена до отправки без изменения WAL, отмена open с освобождением владельца, потерянный ACK после commit, несколько pending запросов и очистка signal до callback. Installed tarball проверяет структурные типы без DOM и pre-aborted open в Node без Worker.

В `/examples/opfs/` кнопка «Прервать текущий запрос» доступна во время open/get/put/delete/scan. Быстрый запрос может завершиться раньше клика; детерминированный UI-тест удерживает загрузку WASM, отменяет open и затем проверяет повторное открытие и сохранение. После отмены отправленного запроса заново откройте базу и прочитайте состояние: запись могла сохраниться, автоматического повтора нет.

Полный прогон: `HOKE_BROWSER_CHANNEL=chrome pnpm check:all`. Контракт и сигнатуры — [ADR0013](adr/0013-browser-cancellation.md). Отдельное исследование будущей публикации SST — [OPFS capabilities](OPFS_CAPABILITIES.md); probe не доказывает crash/power-loss durability.

## 18. Внутренний SST file bridge (C5a)

Этот срез переносит существующие SST файлы через WASM в OPFS; openDB пока не переключается на SST. `pnpm test:wasm` после сборки включает28 сценариев, из них8 новых для SST: tombstones/empty/missing, сортировка и framing до I/O, native CRC, short writes, failed sync, indexed scan, границы packet/limit и close.

`tests/browser/sst-files.spec.ts` добавляет4 настоящих OPFS Worker cases: чтение после reopen, preflight без создания файлов, collision/corrupt-open без переписывания и освобождение handle после failed flush с сохранением orphan name. Полный browser suite —54 scenarios. Код файлового слоя внутренний, отдельного пользовательского UI пока нет.

Проверки выполняются после интеграции цельного среза: `HOKE_BROWSER_CHANNEL=chrome pnpm check:all`. Эти сценарии не подтверждают root publication, browser-process crash или power-loss safety. Контракт и ограничения — [ADR0015](adr/0015-sst-wasm-files.md).

## 19. Потоковое объединение SST (C5b)

После реализации и интеграции выполнены9 Rust merge cases (`cargo test -p hoke-core --test sst_merge`),6 новых actual-WASM cases и9 новых OPFS browser cases. Общие результаты:205 Rust,34 WASM,63 Chrome;252 JS и11 Python остаются зелёными. Native тесты проверяют источник без mutations, winning read failure, output write failures, cap64 и deterministic model.

Actual-WASM дополнительно проверяет output больше32MiB/1000 записей, сохранение источников, metadata corruption до destination calls и winning payload CRC во время streaming. Browser cases проверяют reopen/ordering/tombstones, preflight без I/O, missing/locked sources без candidate, corrupt source с пустым orphan, close/flush failures и create с неизвестным исходом. Полные команды остаются `pnpm check:all` или последовательностью `pnpm check:js`, `pnpm check:rust`, `pnpm test:bench`, `HOKE_BROWSER_CHANNEL=chrome pnpm test:browser`.

Слияние ещё не переключает openDB active state, поэтому нельзя проверять его кнопкой compaction в текущем демо. Это внутренний файловый примитив по [ADR0016](adr/0016-sst-merge-candidates.md), не доказательство root publication или power-loss safety.

## 20. Чтение WAL и SST (C5c)

После реализации среза общий `HOKE_BROWSER_CHANNEL=chrome pnpm check:all` проверяет252 JS,212 Rust,41 actual-WASM,11 Python и69 Chrome cases. Новые сценарии:7 native read_sources,7 WASM read-view и6 OPFS Worker.

Проверяются WAL/SST precedence, tombstones/reinsert, byte bounds/FF prefix/reverse/live-row limit, selected CRC/I/O failures без устаревшего fallback, zero/точный byte budget, закрытие view/WAL, immutable source bytes и cleanup при missing/corrupt/locked sources. Actual-WASM проверяет полный packet32MiB отдельно от payload budget; реальный Worker проверяет first-init concurrency с одной загрузкой WASM и сохранением Rust allocations.

View использует переданный живой WAL и не сохраняет snapshot. Он пока внутренний, в openDB UI не подключён; publication/root selection этим набором не проверяются. Контракт — [ADR0017](adr/0017-layered-reads.md).

## 21. Container journal v1 (C6a)

После реализации и интеграции прошли `HOKE_BROWSER_CHANNEL=chrome pnpm check:all` и `cargo +1.89.0 test --workspace --locked`:252 JS,235 Rust,52 actual-WASM,11 Python,74 Chrome cases;235 Rust также на MSRV. Новые journal проверки:10 codec,13 core integration,11 actual-WASM и5 actual OPFS.

Проверяются golden bytes, CRC-repaired invalid semantics, строгие refs/sequence/generation, все partial record tails, оба commit barriers и ошибки write/flush/sync/truncate, unknown final-sync outcome, отсутствие fallback/repair при полной corruption, pending без auto publication, dropped/interior/future/forged refs, caps до I/O и poisoning после CRC reread. Recovery читает payload порциями64KiB, metadata ограничены active/pending sets.

OPFS Worker проверяет explicit roots/reopen, staged opaque segments, incomplete EOF truncate, неизменность файла после full corruption, ownership, validation/options copy до создания и полный root после потери финального flush acknowledgement. Это file publication tests, а не KV batch API, process kill или power-loss proof. Формат/порядок commit — [ADR0018](adr/0018-container-journal-v1.md).

## 22. Контейнерная KV Database (C6b)

Запускаемый Rust-пример использует один новый `.hkc`, существующие WAL-каталоги не трогает. Команды используют UTF-8 keys/values; scan ограничен1000 строками и32MiB payload:

```sh
container_demo=$(mktemp -d)
cargo run -q -p hoke-core --example container -- "$container_demo/demo.hkc" batch put theme dark put count 7
cargo run -q -p hoke-core --example container -- "$container_demo/demo.hkc" flush
cargo run -q -p hoke-core --example container -- "$container_demo/demo.hkc" delete count
cargo run -q -p hoke-core --example container -- "$container_demo/demo.hkc" compact
cargo run -q -p hoke-core --example container -- "$container_demo/demo.hkc" get theme
cargo run -q -p hoke-core --example container -- "$container_demo/demo.hkc" scan
cargo run -q -p hoke-core --example container -- "$container_demo/demo.hkc" info
```

Ожидается value="dark", одна строка theme/dark; generation=5,tables=1,wal_segments=0,memtable_entries=0,memtable_bytes=0. Каждый вызов — отдельный процесс и reopen файла. Compact публикует активный набор, но не уменьшает размер append-only container. Ошибочные команды/размеры проверяются до создания файла; get/scan/info/maintenance не создают отсутствующий файл.

Общий прогон после реализации прошёл: `HOKE_BROWSER_CHANNEL=chrome pnpm check:all` —252 JS/254 Rust/60 WASM/11 Python/79 Chrome. MSRV — `cargo +1.89.0 test --workspace --locked`, все254 Rust tests прошли. Новые OPFS cases в container-db.spec.ts проверяют batch/flush/compact/reopen, binary range/reverse/WAL masking, invalid input без нового root, inner corruption до ремонта outer tail, failed final sync с целым batch после reopen. Файловый пример — отдельная проверка сохранения между процессами, не power-loss proof. Public openDB UI ещё не переключён; [ADR0019](adr/0019-container-kv.md).
