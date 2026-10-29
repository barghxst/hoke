# Ход работ

Дата: 28 октября 2026.

## Исправление CI после добавления favicon

Запуск [CI #7](https://github.com/barghxst/hoke/actions/runs/40672756953) для ca5f764 упал на общем Biome check: в новом SVG отсутствовал title и отличалось форматирование. Browser, Rust и MSRV jobs этого запуска прошли. Ошибка воспроизведена локально; добавлен title Hoke и применён Biome, затем весь pnpm check:js прошёл повторно (327 JS,60 WASM,9 launcher/HTTP, tarball/types/build). Файлы данных и API не менялись.

## Запуск и навигация демо

`pnpm demo` заменён на Node launcher: сборка JS/WASM с отдельными timestamp BUILD logs, HTTP-сервер, автоматическое открытие системного браузера после readiness и четыре кликабельные ссылки. Занятый порт заменяется следующим свободным с предупреждением о смене origin; --strict-port сохраняет фиксированный порт, --no-open подходит для headless/CI, --no-build использует существующие assets. Ctrl+C останавливает сервер/сборку; ошибка сборки не запускает сервер, ошибка browser opener оставляет сервер и ссылки доступными.

Добавлено главное меню с тремя примерами, навигация во всех демо и отсутствие просмотра каталогов. Сервер отдаёт только examples и собранные JS/WASM, поддерживает MIME/HEAD и пишет status/method/URL/duration logs. Playwright теперь использует тот же launcher вместо Python server; test:demo включён в check:js. Python остаётся нужен для benchmark harness, но не для demo.

## Удобный document API

Добавлен `openDatabase(name,{timeoutMs?,signal?})` поверх существующего container-v1. Прежние openDB/default WAL и byte collections сохранены. Новый API предоставляет типизированные JSON-коллекции со строковыми ключами, get/set/delete/list, atomic writeBatch и локальными подписками. Strict JSON codec фиксирует вход до очереди, проверяет сохранённое представление синхронным validator, возвращает frozen snapshots и не перезаписывает повреждённые/несовместимые данные. Physical namespace учитывается в лимитах до persistent I/O.

Общая FIFO document-сессия выполняет maintenance перед mutation при 32 SST или 48 WAL segments и завершает принятые операции до close. Автоматического retry нет; физический файл не сокращается. Дополнительный stats request на mutation — явная цена простой политики. Миграций схемы DB, cross-tab routing и нового долговременного формата этот срез не добавляет.

`useDBValue(collection,key)` из hoke/react объединяет async чтение и подписку: loading/success/error, защита от stale responses, cleanup/StrictMode и SSR без Worker. Новый [пример задач](../examples/documents/index.html) проверяет обычный пользовательский сценарий без выбора режимов и кодирования байтов. [DATABASE.md](DATABASE.md) — основной guide для приложений, [ADR0021](adr/0021-document-database-usability.md) фиксирует контракты.

## Рефакторинг доменов и описание проекта

По запросу пользователя перед дальнейшим развитием DB проведён рефакторинг структуры:

- SST: публичный фасад, формат, reader, writer, merge и существующие тесты разделены.
- WAL engine: recovery, durable writes, производные изменения и ошибки вынесены в собственные модули.
- Native Database: lifecycle, операции/пороги, publication, maintenance и GC имеют отдельные границы; данные и lock остаются в одном координаторе.
- Container: journal отделён от codec и KV coordinator; recovery, checked I/O, mutation, maintenance и bounded staging расположены рядом со своим владельцем.
- WASM: отдельные биндинги WAL/SST/read view/journal/container, общий Storage bridge и scan codec.
- Browser DB: публичный contract, операции, Worker session, signal и response validation разделены; runtime imports из внутренних modules обратно в публичный index устранены.
- Worker: request validation/routing, OPFS/WASM ownership и error translation разделены; SST adapters выделяют packet codec и reader отдельно от candidate lifecycle.
- Native CLI и benchmark: аргументы, исполнение, deterministic datasets и measurement/reporting разделены с сохранением preflight и границ замеров.

[README](../README.md) объясняет назначение проекта и различие TypeScript KV / Rust DB. [ARCHITECTURE.md](ARCHITECTURE.md) описывает LSM-подобный движок, Storage, WAL/memtable/SST, три координатора, путь commit/recovery, различие версий и карту исходников. Подробные примеры перенесены в [USAGE.md](USAGE.md). Правило разделения по ответственности закреплено в AGENTS.md; произвольный лимит строк не вводился.

Публичные entry points, форматы файлов, Worker protocol3 и существующие сценарии сохранены. Новых dependencies или возможностей DB этот срез не добавляет. Неполные crash/model проверки полного container DB остаются следующим этапом; рефакторинг их не заменяет.

## Этап A / кандидат v0.1

- [x] Базовый KV, память и localStorage.
- [x] Подробный план самостоятельной работы: [PLAN.md](PLAN.md), этапы A–F.
- [x] A1: стабильные snapshots localStorage, повторное чтение backend, ошибки не скрываются кэшем.
- [x] A2–A3: defineStore/createStore, defaults, snapshots, миграции, защита future version при чтении и изменениях.
- [x] A4: useValue/useStore, selectors/comparator, StrictMode, unsubscribe, SSR/hydration.
- [x] Интеграция реальных localStorage → KV → Store → React.
- [x] A5: демо настроек, миграции v1 → v2, защита v3, явный сброс; обновлён README.
- [x] A6, локальная часть: CI-файл, exports, tarball consumer без React, React consumer + SSR и declarations.
- [x] Подготовлен CHANGELOG с незавершёнными релизными условиями.
- [x] Biome 2.6.3: форматирование и линтинг проекта, безопасные исправления, обязательная проверка в pnpm check/CI.
- [x] Команда pnpm demo и [инструкция проверки](TESTING.md) с ожидаемыми результатами.
- [x] Проверка настоящего браузера: 9 Playwright scenarios прошли в Chrome 155.0.8059.84/Linux; jsdom остаётся отдельной проверкой.
- [ ] Выбрать лицензию и владельца npm/GitHub, версию и метаданные для публикации.
- [x] Существующий GitHub origin barghxst/hoke доступен, проверенные commits отправляются в main по прямому разрешению пользователя.
- [ ] Публикация npm — не выполнялась.

## Последняя общая проверка

После обновления launcher и навигации демо `HOKE_BROWSER_CHANNEL=chrome pnpm check:all` прошёл:

- Biome, JS/WASM build, TypeScript исходников/browser harness и установленного tarball без DOM/React;
- 327 JS tests в15 файлах: прежние283 и44 новых document/React cases;
- 60 actual-WASM tests,254 Rust tests,11 Python benchmark tests;
- 9 Node CLI/HTTP integration tests: auto-open readiness, ошибка opener/build, занятый порт, MIME, ограничения static routes и Ctrl+C в том числе при зависшей сборке;
- rustfmt, Clippy с -D warnings;
- 96 настоящих Chrome155/Linux browser tests:78 OPFS/WASM/SST/read-view/container/document +5 OPFS demo +2 document demo +9 KV demo +2 demo navigation. Все browser cases используют тот же Node launcher, что pnpm demo.

Persistent SST v1/WAL v1/container v1 не менялись; Worker transport повышен до protocol3 для публичных container операций. Все254 Rust tests также повторно прошли на MSRV1.89.0 после рефакторинга. Внешний CI теперь проверяется через GitHub connector; причина последнего падения и исправление описаны выше. Другие браузеры/ОС и физическая потеря питания пока не проверялись.

Ранее отдельно проверено сохранение/удаление между тремя отдельными процессами Node с Web Storage. Remote CI проверяется отдельно от локального прогона.

После сообщения о версии при удалении добавлены два регрессионных сценария демо: обе кнопки reset/clear удаляют envelope целиком и после reload запись отсутствует. Следующее изменение создаёт новую запись с версией схемы 2, заданной в коде. Автоматические сценарии проходят; точная последовательность действий пользователя в настоящем браузере пока не установлена.

## Параллельная работа

- Агент store: сторы, миграции и тесты.
- Агент snapshots: стабильность localStorage snapshots и read-only ревью интеграции.
- Агент react: хуки, SSR и тесты полной цепочки с реальным Store.
- Основной агент: контракт/ADR, интеграция, защита stale-store writes по ревью, exports/deps, документация, демо, CI и tarball checks.

Все изменения интегрированы в общую рабочую папку. Баг, найденный при ревью: открытый старый стор мог записать поверх будущей версии через set/reset. Исправлено, регрессионные тесты добавлены.

## Ограничения и следующий рубеж

- Пакет остаётся private и имеет версию 0.0.0: это кандидат функциональности v0.1, а не опубликованный релиз.
- Реальная browser matrix и расширенная совместимость React/Node пока не проверены.
- События между вкладками и разными KV-экземплярами не реализованы.
- Native Rust и browser DB через WASM/Worker/OPFS реализованы; typed document API добавлен поверх container-v1. React Native/devtools, DB migrations, physical reclamation и расширенные container crash/model проверки остаются впереди.
- Atomic write batch реализован по ADR 0006, см. текущий срез ниже. Браузерная проверка этапа A остаётся открытой и не подменяется Rust-тестами.

## Исправление сценария просмотра миграции

Создание тестовой v1 и миграция разделены на два явных действия. Демо сохраняет и показывает v1, в том числе после reload; createStore с кодом v2 вызывается только по кнопке миграции. Интеграционный тест проверяет обе отображаемые записи и блокировку обычных изменений до миграции. Семантика библиотеки не менялась: createStore по-прежнему мигрирует при открытии.

## Завершённый первый Rust-срез: B1–B2 / WAL-only прототип

- [x] Пользователь выбрал бинарный WAL с CRC32 вместо JSON Lines или готового codec. ADR 0002 принят.
- [x] Cargo workspace, hoke-core, publish=false, unsafe_code=forbid.
- [x] Storage trait, FileStorage с эксклюзивным lock, MemoryStorage.
- [x] WAL v1: magic/version, CRC header и frame, limits до allocation, put/delete, sequence.
- [x] Engine: open/put/get/delete/close, запись+flush+sync перед обновлением памяти, poisoned после I/O-ошибки mutation.
- [x] Recovery: проверенный префикс, repair неполного EOF tail, corruption/future format без очистки.
- [x] Fault tests и реальные kill child после ACK/в середине append, lock release after kill.
- [x] CLI в отдельных процессах: put/get/overwrite/delete/NOT_FOUND, неверные аргументы не создают файл. Проверено вручную во временном каталоге.
- [x] pnpm check:js / check:rust / check, CI jobs, rustfmt вместо Biome для Rust.
- [x] SSTable/manifest и ручной flush — следующий срез ниже.
- [x] Ordered scan/range — срез ниже.
- [x] Compaction/GC — срез ниже.
- [x] Atomic write batch — срез ниже; интерактивных транзакций нет.
- [x] Directory-backed Database синхронизирует новые имена и manifest на Unix. WAL-only Engine по-прежнему не владеет parent-directory lifecycle.
- [ ] Дополнительная проверка durability, без заявления о power-loss safety.
- [ ] MSRV 1.89, Windows/macOS, WASM — пока не проверены. Проверено Linux/Rust 1.97.1.

Формат: WAL_FORMAT.md. Прототип хранит актуальную map целиком в памяти и перечитывает журнал при open. Полностью записанная неподтверждённая операция после I/O-ошибки может появиться после recovery — неизвестный исход, не rollback. Process-kill тесты не подтверждают power-loss safety.

Агенты Rust работали над memory, file, codec, fault/recovery и process tests; root интегрировал engine, CLI, документацию и проверки. При read-only ревью найдено различие чтения за EOF при больших offsets: приведено к общему контракту возврата 0.

## Текущий Rust-рубеж: B3 / SSTable и manual flush

- [x] ADR 0003 с тремя вариантами и протоколом атомарной публикации, отдельный SST_FORMAT.md.
- [x] SSTable v1: sorted unique byte keys, значения/tombstones, CRC headers/records/index, bounded parsing, ленивое чтение values.
- [x] Directory trait + NativeDirectory: lifetime LOCK, existing-only refs, create_new, validated names, atomic rename, Unix directory sync.
- [x] Manifest v1: список SST и активный WAL, CRC, limits, missing/empty refs без silent initialize.
- [x] Database: memtable → новые/старые SST, tombstones, flush, stats, poison до reopen после ошибки flush.
- [x] Flush освобождает значения delta и начинает новый WAL с 16-байтного заголовка; ключевые индексы остаются в памяти.
- [x] Fault harness отказа каждого шага flush + неизвестные исходы write/rename. Модель 200 операций с flush/reopen.
- [x] Настоящий kill child до rename и после rename до directory sync; подтверждённые изменения сохраняются.
- [x] CLI --db: вручную проверены put/info/flush/get/delete/flush/reopen, tombstone и повторная запись. Старый WAL CLI работает.
- [x] Полный pnpm check: 115 JS + 73 Rust, Biome, build/typecheck, tarball, rustfmt, Clippy.
- [x] Scan/range — срез ниже.
- [ ] Sparse/block index, bloom.
- [x] Настраиваемый порог flush — срез ниже.
- [x] Compaction и garbage collection — срез ниже; автоматического запуска нет.

Это отдельный каталог DB, а не изменение формата WAL v1. Перенос старого WAL не автоматизирован. После отказа первой инициализации непустой каталог без manifest выдаёт ошибку — подтверждённых пользовательских операций на этом этапе ещё нет. Проверены Linux/process crashes; MSRV и другие ОС по-прежнему не заявляются.

Параллельные агенты реализовали SST codec, NativeDirectory, fault/model tests и process tests; root — manifest, Database, tombstone-слой Engine, CLI и интеграцию. Read-only ревью не обнаружило блокирующих проблем протокола публикации.

## Текущий Rust-рубеж: ordered scan/range

- [x] ADR0004: byte bounds, prefix intersection, live limit/default100, reverse и приоритет версий.
- [x] Heap merge cursors SST + iterators memtable; не загружаются все values или копии всех key indexes.
- [x] Database::scan/range, ScanOptions, owned KeyValue pairs. Included/Excluded, binary/empty/FF prefix.
- [x] Tombstones/перезаписи между источниками; winner читается после устранения дубликатов.
- [x] Ошибка winning payload/tombstone =>Err без fallback; shadowed/unselected payload не читается. limit0 без scan I/O.
- [x] CLI scan/prefix/range, --reverse/--limit, отрицательные случаи до создания каталога.
- [x] BTreeMap модель 160 шагов с flush/reopen, 10 scan integration +5 CLI tests,3 index bounds tests.
- [x] Read-only ревью merge/prefix/reverse не обнаружило actionable bugs.

Дисковые форматы неизменны. Результат — Vec, ограниченный limit, не streaming iterator. Расход памяти выдачи зависит от размеров выбранных values. Браузерный DB API не добавлялся. Сценарии и ожидаемый вывод — SCANS.md.

## Текущий Rust-рубеж: B4 / full compaction и GC

- [x] ADR 0005: ручное полное слияние всех SST и memtable без изменения форматов v1.
- [x] Общий heap merge передаёт values по одному streaming SST writer; default scan limit100 не ограничивает compaction.
- [x] Новые значения побеждают, покрытые tombstones удаляются; пустой результат не оставляет SST. Повтор без изменений не переписывает чистую единственную SST.
- [x] Новый SST/WAL синхронизируются до manifest publication; старые handles закрываются и obsolete files удаляются только после commit/barrier.
- [x] collect_garbage удаляет только строго распознанные obsolete generation-файлы, сохраняет активные ссылки и неизвестные имена. Явный Directory::remove_file с default Unsupported.
- [x] CleanupReport отдельно передаёт ошибки удаления/list/sync, не выдаёт уже опубликованное состояние за rollback. Повторная очистка и дальнейшие записи допустимы; до публикации ошибки poison до reopen.
- [x] CLI compact/gc с проверкой аргументов до открытия, явным статусом публикации и ненулевым exit при cleanup failure.
- [x] Проверки >100/binary keys, disk reclaim, BTreeMap модель 240 шагов, corruption без fallback, отказы каждого I/O шага и неоднозначные rename/remove.
- [x] Реальные kill child до/после manifest rename и первого obsolete unlink; reopen сохраняет подтверждённые данные и допускает дальнейшие записи.

Ограничения: один выходной SST максимум 1M entries/64 MiB index; нужно временное место под живой результат. Исходные key indexes и memtable остаются в памяти, значения всей базы не собираются. Нет фоновой compaction, levels или автоматического GC. Linux/process tests не доказывают power-loss safety. Сценарий проверки: COMPACTION.md. Atomic write batch реализован следующим срезом по ADR 0006.

Параллельные агенты реализовали streaming writer, безопасное удаление, integration/fault/model/process и CLI tests; root — Database publication/GC, общий merge visitor, ADR и документацию.

Итоговая проверка этого среза: полный pnpm check прошёл — 115 JS и 121 Rust tests, Biome, build/typecheck, tarball, rustfmt и строгий Clippy. Ручной CLI smoke в отдельном временном каталоге: 2 SST → 1, удалено 5 obsolete files, theme=dark и newest=unflushed сохранились, get old=NOT_FOUND; повторный gc удалил 0. Пользовательский /tmp/hoke-db не изменялся.

## Текущий Rust-рубеж: B5 / atomic write batch

- [x] ADR 0006: выбран существующий SST/manifest commit, без расширения WAL или смены форматов v1; альтернативы и цена зафиксированы.
- [x] Database::write_batch, BatchOperation Put/Delete, validate_batch и публичные лимиты. Последнее действие над повторным ключом побеждает; binary/empty keys и values поддержаны.
- [x] Предварительная проверка всех input операций, count=1024, sum key/value bytes=32 MiB, key64KiB/value16MiB, combined SST capacity и tablecount. Ошибки input/capacity не записывают файлы и не poison базу.
- [x] Копия memtable включает прежнюю WAL delta и batch, сохраняет tombstones; новая SST/WAL публикуются тем же flush-протоколом. Ошибка IO сохраняет старое live-state и блокирует mutations до reopen.
- [x] Recovery видит весь batch или ничего из него, прежние подтверждённые записи сохраняются в обоих состояниях. При неизвестном исходе rollback не обещается.
- [x] Empty batch — no IO на здоровой базе, poisoned instance отклоняет его. Старые Engine/put/delete и дисковые форматы совместимы.
- [x] CLI batch с полным parse/validation до открытия; неизвестные/неполные действия не применяют префикс, command/flag-shaped key/value остаются литералами.
- [x] Fault injection каждого шага публикации, write-then-error/rename-then-error, ordered model с batch/flush/compact/gc/reopen, лимиты SST/tablecount без IO.
- [x] Реальные child kill до rename, после rename до directory sync и после ACK; all-or-none state и дальнейшая запись после восстановления.

Каждый непустой batch создаёт SST и новый WAL, копирует текущую memtable, требует нескольких sync. Это атомарность, не быстрый WAL group commit; tombstones здесь не удаляются. Для file/table reclamation нужны явные compact/gc. Более быстрый batch record в WAL потребует отдельного решения о формате; интерактивных транзакций/нескольких писателей нет. Linux/process tests не заменяют потерю питания. Сценарий: BATCHES.md.

Параллельные агенты реализовали fault/model/integration, process и CLI; root — Batch API/validation, повторное использование SST preflight/manifest publication, ADR, документацию и интеграцию.

Итоговая проверка B5: полный pnpm check прошёл — 115 JS + 141 Rust tests, Biome, сборка/типы, tarball, rustfmt, строгий Clippy. Ручной CLI smoke в отдельном временном каталоге: batch из 3 операций, сохранение старой WAL delta, reopen scan/get, ошибка последнего аргумента с byte-for-byte сохранением файлов, compact и повторный gc. Read-only ревью не нашло дополнительных ошибок; generation overflow исправлен как retryable Limit без poison и покрыт тестом. Пользовательский каталог /tmp/hoke-db не изменялся.

## Текущий Rust-рубеж: B3 / настраиваемый порог memtable

- [x] ADR 0007: синхронный pre-write flush, opt-in policy, прежний manual default, форматы v1 без изменений.
- [x] DatabaseOptions.max_memtable_bytes, validate/open_with_options и stats.memtable_bytes. Порог локален экземпляру, нулевой отклоняется.
- [x] Точный payload counter Engine: live key+value, tombstone key; overwrite/reinsert/delete/replay, counter меняется только при apply после commit.
- [x] Projected size проверяется до append: непустая delta сбрасывается, если новая операция превысит порог; равенство и уменьшающий overwrite без лишнего flush.
- [x] Invalid input проверяется до IO; ошибка preflush не применяет новую операцию, старая подтверждённая delta сохраняется. Append после удачного preflush сохраняет прежний неизвестный исход при IO error.
- [x] Empty/binary/zero-byte keys, большие одиночные записи, recovered delta, no flush on open/read/close, batch не дробится по порогу.
- [x] CLI --db <path> [--memtable-limit N] <command> с положительным usize и validation до открытия; stats дополняются payload bytes.
- [x] Fault каждого preflush шага/rename-after-error, append-before/after-write errors, deterministic model с обслуживанием/reopen.
- [x] Реальный child kill до/после preflush manifest rename: старые ACK сохранены, новая операция отсутствует, lock освобождается.

Это мягкий payload threshold: накладные расходы map/allocator/SST indexes не учитываются, одиночный record может быть больше порога. Один payload-порог не ограничивает WAL при повторных overwrite; отдельный WAL-size threshold реализован следующим срезом. Во время flush копируется delta; фоновой compaction/GC и latency benchmarks пока нет. Сценарий: MEMTABLE.md. Linux process tests не заменяют power-loss verification.

Параллельные агенты реализовали Engine accounting, integration/model/fault/process и CLI; root — options/pre-write policy, ADR, документацию и интеграцию. Read-only ревью не выявило дополнительных ошибок.

Итоговая проверка порога: полный pnpm check прошёл — 115 JS + 162 Rust tests, Biome, сборка/типы, tarball, rustfmt, строгий Clippy. Ручной CLI smoke в отдельном временном каталоге подтвердил 5 bytes delta до/после preflush при пороге 8, переход 0→1 SST, overwrite до2bytes, delete с tombstone до3bytes и корректный scan/get после каждого reopen. Файлы пользовательского /tmp/hoke-db и имя проекта не менялись.

## Текущий Rust-рубеж: порог активного WAL

- [x] ADR 0008: дополнительное pre-write условие по длине активного WAL, форматы v1 и sync/manifest commit сохранены.
- [x] DatabaseOptions.max_wal_bytes: Option<u64>, минимум header16, defaultNone, политика на один open.
- [x] Projected length включает current WAL и header28/key/value/CRC4 нового frame. Equality без flush; checked_add overflow с включённым порогом вызывает preflush.
- [x] WAL/payload условия объединены OR, один flush. Повторные overwrite/tombstone/zero-byte ключи учитываются по frame bytes независимо от маленькой memtable.
- [x] Invalid input/options до IO; пустой WAL допускает один oversized record. Open/read/close/empty batch не запускают flush, непустой batch не дробится.
- [x] CLI --wal-limit N вместе с --memtable-limit в любом порядке до команды, duplicate/missing/negative/overflow/<16 отклоняются до открытия. Flag-shaped key/value остаются литералами.
- [x] Integration/model/fault тесты каждого preflush шага, неизвестный rename/append, recovery и combined thresholds.
- [x] Реальный child kill до/после manifest rename при preflush от повторного overwrite: последнее ACK значение сохраняется, новая версия отсутствует.

Это soft threshold активного журнала, а не всего каталога; один record может быть больше порога. Старые WAL/orphans остаются до явных gc/compact, фоновая compaction и автоматический GC не добавлены. Настройка не сохраняется между CLI-процессами. Linux/process tests не доказывают power-loss safety. Сценарий: WAL_THRESHOLD.md.

Параллельные агенты реализовали integration/model/fault, process и CLI; root — options/общую pre-write policy, ADR, документацию и интеграцию. Read-only ревью подтвердило формулу frame bytes и порядок ошибок; уточнены rustdoc о двух независимых порогах.

Итоговая проверка WAL threshold: полный pnpm check прошёл — 115 JS + 181 Rust tests, Biome, сборка/типы, tarball, rustfmt и строгий Clippy. Ручной CLI smoke в отдельном временном каталоге: три overwrite при --wal-limit90 дали WAL53→90→53, tables0→0→1 и постоянный memtable_bytes5; get/scan после reopen показали последнее значение9999. Файлы пользовательских баз и название проекта не менялись.

## Native benchmarks и переименование в Hoke

- [x] npm-пакет hoke, импорты hoke/kv и hoke/react; Rust hoke-core/hoke_core, CLI hoke; каталоги packages/hoke и crates/hoke-core, workspace/lockfiles/examples/docs обновлены.
- [x] Persisted markers localStorage/envelope/WAL/SST/manifest сохранены; переименование не сбрасывает данные и не меняет schema versions. Установленный tarball читает/обновляет старую запись без дубликата; отдельная независимо собранная WAL v1 fixture с CRC32 читается новым CLI без изменения bytes.
- [x] ADR 0009, std-only hoke-bench: 12 API-сценариев, проверка по модели вне таймеров, JSONL только после полного успеха, snapshots bytes/files и ограничение ресурсов до изменений ФС.
- [x] Python stdlib runner: release build один раз, независимые replicates, environment/source metadata, raw samples, summary с явными calls/items и ограничениями percentiles; failed state без summary.
- [x] Rust process tests и Python unittest включены в локальную проверку/CI. CI на внешнем сервере не запускался.
- [x] Полный pnpm check: 115 JS + 186 Rust + 11 Python tests, Biome, build/typecheck, установленный tarball, rustfmt, строгий Clippy. После добавления проверки table cap повторены 5 benchmark process tests и строгий Clippy этого target.
- [x] Три реальных release-прогона по 1000 ключей, key16/value256/batch100/scan100×50/seed1. Raw results и environment сохранены в benchmarks/results/2026-10-27-native-baseline, временные базы удалены. Методика: BENCHMARKS.md, совместимость: RENAMING.md.

Локальный p50: synced put около1.82ms, успешный SST get около1.96µs. Измеряется Rust API с прогретым файловым кэшем; это не cold storage, не JS/WASM и не сравнительный benchmark. Модель накопителя не определялась. Нагрузки не измеряют peak RSS и не доказывают power-loss safety. Форматы данных и API движка при этом срезе не изменялись. Никакой публикации или создания внешнего репозитория не выполнялось.

## Итоговый аудит KV и native-прототипа

- [x] Исправлен NativeDirectory с относительным путём: absolute/canonical path фиксируется при open. Изолированный child process меняет cwd, выполняет flush/compact и доказывает сохранение обоих ACK в исходной базе; соседняя заблокированная база не меняется.
- [x] Чистые ошибки capacity до файловых операций не poison базу. Проверки числа таблиц для manual/threshold flush, лимитов SST и generation overflow разрешают compact/GC/повтор. Неизвестные исходы файловых операций сохраняют прежний poison/reopen контракт.
- [x] Runtime validators Store/localStorage принимают только синхронный true. Promise/thenable отклоняются, native Promise rejections наблюдаются без unhandled rejection; custom thenable.then не вызывается. Проверены local/cross-realm Promise, defaults/set/updater/migration, adapter read/write и сохранение исходных записей.
- [x] Исправлена явная очистка повреждённого JSON в демо: storage.remove без предварительного чтения, затем reopen. Чужие ключи сохраняются. Обычное чтение не очищает повреждения.
- [x] Playwright с изолированными contexts, trace/screenshot при ошибке, отдельным сервером и без переиспользования занятого порта. pnpm check:all — полный прогон, pnpm test:browser — browser suite.
- [x] Rust MSRV 1.89.0 проверен реальным полным прогоном; latest CI toolchain остаётся 1.97.1. Подготовлены browser/MSRV CI jobs.
- [x] Финальный прогон: 118 JS + 190 Rust + 11 Python + 9 browser tests; сборка/типы, tarball, Biome, rustfmt и Clippy прошли.

Текущие KV/store/React и native B реализованы и проверены как экспериментальный прототип. Это не завершение всех этапов A–F. Следующий продуктовый рубеж — C: WASM-сборка, Worker-протокол и реальное OPFS. До записи базы в браузере нужен ADR, как перенести существующий Directory commit/ownership контракт, не подменяя отсутствующие гарантии успешными no-op. Затем multi-tab, React Native и devtools по плану. Лицензия, авторство, внешние ресурсы и публикация остаются отдельными решениями пользователя.

## C1: первый Rust/WASM/Worker/OPFS срез

- [x] ADR0010: существующий WAL-only Engine и отдельный OPFS файл; native Directory не имитируется no-op rename/sync. Форматы файлов не менялись. Полный browser Database/SST commit остаётся отдельным решением.
- [x] hoke-wasm, точная пара wasm-bindgen crate/CLI0.2.129, release wasm32 build и cfg gates native adapters. Storage bridge ловит JS exceptions, проверяет числа/offsets, завершает short writes и вызывает OPFS flush перед ACK.
- [x] hoke/db: openDB(name), mode wal-v1, async get/put/delete/close, typed version1 protocol, limits/copy/timeout/terminal transport errors. Без browser globals/WASM initialization при импорте KV/React/DB.
- [x] Dedicated Worker сериализует запросы, загружает WASM до изменений ФС, открывает OPFS/hoke-wal-v1/<name>.wal с исключительным sync handle. Второй владелец получает LOCKED. Ошибки не заменяются памятью/localStorage.
- [x] close/error cleanup освобождает Rust allocations/handle. Дополнительно исправлена остановка очереди при двойной ошибке postMessage: terminal catch освобождает owner и сигнализирует client error, pending calls прекращаются.
- [x] Сборка и tarball включают generated JS/WASM/declarations, smoke проверяет установленный hoke/db без Worker и отсутствие React-зависимости KV.
- [x] 41 client unit case,16 настоящих WASM adapter tests,15 real OPFS cases и одно запускаемое демо. Браузерные сценарии включают corrupt/future version с CRC без переписывания, incomplete tail, confirmed ACK после reload/termination, unknown flush outcome, read/write/transport/quota failures, unsupported sync handles и failed WASM fetch без создания OPFS.
- [x] Полная проверка прошла; итоговые количества приведены выше. Никакой публикации не было.

Запуск: pnpm demo → /examples/opfs/. Подготовка, API и ограничения — OPFS.md. Tested Chrome155/Linux. C1 не является завершением C: пока отсутствуют browser SST/compaction, scan/collection, atomic batch, subscriptions и DB migrations. Весь WAL хранится/проигрывается целиком; общий размер не ограничен API, mode экспериментальный. Timeout/lost reply может означать commit без ACK; автоматического повтора нет. Worker termination не моделирует потерю питания или browser-process crash.

## C2: ограниченные browser scans и байтовые коллекции

- [x] ADR0011: общий native Rust range/merge код для WAL map без I/O/копирования всей базы. Engine::scan_with_budget ограничивает key/value payload; WasmWal.scan дополнительно ограничивает полный packet.
- [x] BrowserDB.scan со start/end {key,inclusive}, prefix, reverse и limit0..1000 (default100). Результат максимум32MiB с заголовками; ошибки без частичной выдачи, файловых операций и poison.
- [x] Worker protocol2 с отклонением старых request/response, WAL остаётся v1. Client проверяет framing/lengths/limit/order/unique/range/prefix и завершает сессию при неверном ответе.
- [x] BrowserDB.collection(bytes) / DBCollection get/put/delete/scan. Namespace0x01+u32BE length+prefix устраняет неоднозначную конкатенацию, relative keys/ranges, копии и общий DB lifecycle. Предел физического key64KiB сохраняется.
- [x] 4 native model/budget/no-I/O cases,4 новых actual-WASM cases,36 новых JS client/collection cases,7 новых real OPFS cases и один новый UI scenario prefix/reverse/limit/reload/delete. Tarball smoke проверяет новые публичные типы без DOM.
- [x] Полный прогон:195 JS +196 Rust +20 WASM +11 Python +33 browser tests. Все196 Rust tests также прошли на1.89.0. Документация и демо обновлены.
- [x] По просьбе пользователя начата фиксация отдельных законченных задач: переименование —64fc2cd; накопленный C1/WASM/backend/verification —90099b4. Его staged snapshot отдельно экспортирован и проверен полным check:all до commit. C2 фиксируется отдельным коммитом после проверок. Автор/committer — уже настроенный barghxst, без assistant/agent trailers/CONTRIBUTORS и без push.

Сценарий: pnpm demo → /examples/opfs/; сохранить user:1=Alice,user:2=Bob, prefix user:, reverse, limit1 → user:2. После удаления user:2 → user:1; reload сохраняет результат. Форматы данных и dependencies не менялись. Collection key encoding применяется только к новым адресациям, прежние raw keys не перемещаются.

Браузерный backend по-прежнему WAL-only, весь журнал/map остаётся в памяти, byte cap — предел результата, не RSS. Полный C не завершён: нужны multi-file SST publication/compaction, streaming при необходимости, atomic batch, subscriptions и DB migrations. Изменение durable publication/формата требует отдельного решения; этот C2 его не подменяет.

## C3: key/range subscriptions после commit

- [x] ADR0012: клиентский локальный registry на существующих successful put/delete ACK. WAL v1/Worker protocol2/Rust/WASM/dependencies не меняются.
- [x] BrowserDB и DBCollection subscribe(key,listener), subscribeRange({start,end,prefix},listener). Копии inputs, binary/equal/FF bounds, пересечение prefix/range; limit/reverse отклоняются.
- [x] Нет initial/read/scan/replay/error notification. Каждый подтверждённый put/delete инвалидирует подходящие registrations, включая одинаковое значение и удаление отсутствующего ключа.
- [x] Stable dispatch по регистрации, независимые duplicate callbacks, добавление со следующей операции, unsubscribe до очереди, immediate close/fail cleanup. Reopen требует новых subscriptions.
- [x] DBNotificationError extends AggregateError с code SUBSCRIBER_FAILED и committed=true: callback failures отдельно от сохранения, без rollback/retry/poison. Native Promise rejections наблюдаются; async/custom thenables отклоняются без вызова custom then.
- [x] Read-only ревью нашло shadowed nonfunction then у native Promise; observePromise возвращает результат native brand check, registry отклоняет также такие Promise. Добавлены3 регрессионных local/cross-realm cases. Обычные void результаты не требуют Promise brand exception.
- [x] 27 новых JS tests (13 registry +14 client/collection),8 новых actual OPFS browser cases и1 UI scenario счётчика подтверждений. Failed flush не уведомляет, после recovery может обнаружиться неизвестная запись; callback error не мешает дальнейшим операциям.
- [x] Installed tarball smoke проверяет runtime/class и DBRange/DBListener/committed types без DOM/React. Демо наблюдает выбранный prefix и прекращает подписку при unsubscribe/close.
- [x] Полный check:222 JS +196 Rust +20 WASM +11 Python +42 Chrome tests. Изменения оформляются отдельным локальным C3 commit под настроенным пользователем, без assistant/agent attribution и без push.

Ручной сценарий: pnpm demo → /examples/opfs/, открыть базу, prefix user:, «Наблюдать prefix», записать user:1 →1, other →по-прежнему1, удалить user:1 →2. Unsubscribe/close прекращают callbacks. API/ошибки/границы — OPFS.md.

C3 не завершает C: browser SST publication/compaction, atomic batch и DB migrations ещё впереди; multi-tab routing относится к D. Подписки видят операции только одного BrowserDB и его wrappers, не другие вкладки/процессы. Все прежние ограничения WAL-only/RSS/durability остаются.

## C4: AbortSignal и завершение соединения при отмене

- [x] ADR0013: structural DBAbortSignal/DBOperationOptions без требования DOM declarations; open/get/put/delete/scan и collection wrappers принимают signal.
- [x] Pre-aborted open не создаёт Worker/OPFS; отмена до dispatch отклоняет только запрос. После dispatch завершается Worker, очищаются subscriptions и все pending получают ABORTED. Уже отправленная запись могла сохраниться.
- [x] Listener регистрируется до отправки, signal перепроверяется; ACK/ошибка/timeout/close очищают timers/listeners до callbacks. Abort после open ACK или из собственного commit callback не завершает здоровое соединение.
- [x] 30 новых unit regressions,7 настоящих OPFS cases, UI отмены с детерминированной задержкой WASM и reopen. Tarball runtime/типы проверены без DOM/React.
- [x] Полный check:252 JS +196 Rust +20 WASM +11 Python +50 Chrome tests; Biome, typecheck, rustfmt и strict Clippy прошли. WAL v1, Worker protocol2 и зависимости не менялись.

Код и демо фиксируются отдельным локальным коммитом под barghxst без assistant/agent attribution и push. Следующая файловая архитектура подготовлена отдельно как предложенный ADR0014: три варианта публикации SST/compaction, реальные API-наблюдения и fault matrix. Это исследование, не принятое решение о новом формате. Probe воспроизведён root на isolated Chrome155; successful move/reload не доказывают atomic durability или потерю питания.

## C5a: реальные SST файлы через WASM/OPFS

- [x] Публичный экспериментальный Rust модуль sstable для платформенных биндингов: существующие reader/writer, lookup Missing/Deleted/Value, index metadata, preflight. Native codec bytes и поведение не менялись.
- [x] WasmSst создаёт пустой candidate, синхронизирует файл, открывает immutable reader; lookup и scan сохраняют tombstones. Strict sorted packet проверяется до I/O; limits32MiB/1M input entries/key64KiB/value16MiB.
- [x] Внутренний OPFS слой создаёт случайное свободное имя с обязательством caller directory ownership, держит exclusive sync handle, закрывает Rust/handle на success/error и сообщает имя возможного orphan после failed create. Missing/corrupt open не создаёт/не переписывает файл.
- [x] 8 actual-WASM cases и4 настоящих OPFS cases добавлены после реализации среза; общий прогон после интеграции прошёл:252 JS +196 Rust +28 WASM +11 Python +54 Chrome tests. Biome, build, typecheck, tarball, rustfmt и strict Clippy прошли.
- [x] ADR0015 описывает реализацию; package exports и openDB не расширялись. Подготовленные файлы не объявляются commit; WAL/memtable не удаляются и не переключаются.

По уточнению пользователя сначала реализуем связный код базы, затем проводим интеграционную проверку; отдельный publication simulator отложен. Следующая граница — authoritative publication SST/compaction по предложенному ADR0014, где выбор нового durable container ещё не принят. Этот срез не подменяет это решение и не завершает C. Packet cap не ограничивает heap; scan может декодировать одну выбранную запись до обнаружения переполнения бюджета. Browser batch и DB migrations пока отсутствуют.

## C5b: потоковая compaction в отдельный SST-кандидат

- [x] Rust merge_tables с fan-in64: старые источники первыми, новые последними; winner только один на ключ, tombstones остаются, shadowed values не читаются. Fixed cursors и TableWriter без материализации всех values.
- [x] WasmSst.merge открывает метаданные всех источников до destination I/O, создаёт и flush синхронизирует output, оставляет JS handle ownership вызывающему коду. Результат может превышать transfer cap32MiB/scan limit1000; SST caps остаются.
- [x] OPFS mergeSstCandidates: preflight names/copy/count/duplicates, исключительные источники до создания candidate, освобождение input/output handles при ошибке, selected candidateName при неизвестном исходе create. Source bytes не меняются и файлы не удаляются.
- [x] 9 Rust,6 actual-WASM и9 настоящих OPFS cases добавлены после реализации; состав проверок зелёный:252 JS +205 Rust +34 WASM +11 Python +63 Chrome. Внешний CI не запускался.
- [x] ADR0016 фиксирует newest-wins/tombstone/order/lifetime и partial-compaction границы. Durable формат и публичный hoke/db API не менялись; native Database использует прежнюю полную compaction.

Это файловая часть compaction, без authoritative publication. Полный output после flush может остаться orphan при потере результата или cleanup failure, и это не основание удалять sources. Lazy CRC проверяет только выбранные payload; merge не является полным scrub. Следующий шаг с переключением root всё ещё требует выбора по ADR0014. Код оформляется отдельными локальными коммитами пользователя, без assistant attribution и push.

## C5c: живой WAL поверх SST

- [x] Общие Rust get_from_sources и scan_sources_with_budget. Native Database::get делегирует тому же helper; existing heap scan разрешает версии до чтения payload. Winner error не вызывает fallback, tombstones скрывают старые values в обоих направлениях.
- [x] WasmReadView держит до64 SST indexes, заимствует текущий WasmWal на каждый get/scan, не клонирует его состояние. Scan выдаёт только живые строки, limits1000/32MiB полного packet.
- [x] Внутренний OPFS read view владеет SST handles, снимает все при failure/close и не закрывает WAL. Общее чтение не является snapshot или root selection.
- [x] Убрана копия scan decoder: Worker client и read-view используют scan-packet.ts. Исправлена concurrent-first-init гонка через единую wasm-init.ts promise для Worker/SST/read-view; rejected init допускает явную новую попытку.
- [x] 7 Rust +7 actual-WASM +6 real OPFS cases добавлены после реализации. Полный check:252 JS +212 Rust +41 WASM +11 Python +69 Chrome. Browser regression подтверждает одну WASM загрузку при параллельном первом create/view и корректность живых allocations.
- [x] ADR0017 описывает порядок источников, live lifetime, бюджеты, ошибки и отсутствие publication. Durable formats и публичный hoke/db API не менялись.

Код фиксируется отдельными локальными коммитами пользователя без push и assistant attribution. OpenDB остаётся WAL-only до включения container coordinator; закрытие read view не затрагивает текущий WAL.

## Выбрано направление контейнерного backend

27 октября пользователь явно выбрал один OPFS-контейнер с журналом commit (вариант B ADR0014). Новый долговременный container format разрешён; повторное согласование направления не нужно. Следующий рабочий срез: зафиксировать точные header/record/root bytes, caps и recovery, затем реализовать append-only journal поверх Storage. Code first, затем fault/integration проверки целого среза.

Это выбор будущего backend, не заявление о готовой реализации: openDB сейчас WAL-only. SST/WAL payload codecs переиспользуются; root/container bytes будут новыми. Первоначальное создание имени, eviction/power loss и reclaim остаются явно ограниченными. Старый OPFS WAL не переписывается и не удаляется: перенос будет явным в новый namespace/имя с сохранением source. Destructive GC и автоматическая миграция не включаются.

## C6a: container journal v1 / 28 октября

- [x] ADR0018 фиксирует новый header/record/root/ref формат с CRC32, checked lengths/offsets, sequence/generation и zero reserved bytes. File magic HOKECNT, payload WAL/SST пока opaque. Старые codecs и существующие базы не менялись.
- [x] Journal<S> пишет immutable segments и roots. Commit сначала sync данных, затем append root и sync; root публикуется в памяти только после успеха. Pure validation оставляет writer usable, I/O и известное повреждение при reread требуют reopen.
- [x] Streaming recovery проверяет каждый полный record, реальные boundaries и refs только из предыдущего root/new pending. Исключённый ref не возвращается; metadata ограничены4096 active+4096 pending. Full corruption/future version не вызывают fallback или переписывание; только incomplete EOF tail truncate+sync.
- [x] WasmJournal и внутренний OPFS openContainerJournal с явным .hkc/create, exclusive handle, no auto commit on close и cleanup при failed open. Root/ref transfer использует тот же codec; offsets/counters не теряют точность в JS.
- [x] 10 codec +13 core Journal +11 actual-WASM +5 real OPFS cases. После реализации и интеграции полный check:252 JS +235 Rust +52 WASM +11 Python +74 Chrome; MSRV235 Rust также прошёл.
- [x] При интеграции уточнён terminal CRC reread contract: старый WASM сценарий пытался commit после внешнего восстановления байта в том же poisoned object. Обновлён на явный reopen и проверку отсутствия записи до него; повторный общий прогон прошёл.

Journal — настоящий publication primitive, ещё не контейнерная KV Database. Он не декодирует segment payload как WAL/SST, не собирает root из put/delete и не подключён к openDB. Следующий срез — KV coordinator поверх journal и ранее реализованного SST/read path. Read_segment копирует выбранный payload до256MiB, recovery читает CRC порциями64KiB. Append-only file growth, первоначальное имя, eviction и power loss остаются явно ограниченными; reclamation/automatic migration не включены.

## C6b: KV coordinator в одном контейнере

- [x] ContainerDatabase<S>: атомарные put/delete/write_batch, get/scan, flush, full compaction, stats и poison. Immutable WAL v1 segment на mutation/batch, новый root — единственная граница подтверждения; memtable меняется только после root success.
- [x] SharedStorage и ReadSegment дают bounded readonly SST views одного handle. Mixed-source read helpers используют volatile derived Engine без растущего второго WAL buffer; MemoryStorage::into_bytes позволяет передать SST staging без второй полной копии.
- [x] Journal deferred recovery не ремонтирует outer tail до проверки всех active inner SST metadata/WAL payload. Immutable partial/future/corrupt WAL не ремонтируется, root не откатывается.
- [x] Preflush прежних данных до превышения32MiB dirty/64WAL/inherited SST entrycap;65-й SST требует compact. Flush сохраняет tombstones, полная compaction исключает удалённые ключи только из всего active state. Root switch подготовлен до публикации; ошибки после persistent append требуют reopen.
- [x] WasmContainerDb и внутренний OPFS ContainerDB: строгий byte batch packet, live scan32MiB/1000, stats, lifecycle/ownership. Публичный openDB/Worker protocol2 ещё WAL-only.
- [x] Запускаемый native example container.rs: batch → flush → отдельный get → delete → compact → scan/info. Реально выполнено между отдельными процессами: theme=dark сохранилась, count удалён, generation5/tables1/wal0/memtable0.
- [x] 6 unit foundation +13 coordinator Rust tests,8 actual-WASM и5 real OPFS cases. Полный check:252 JS +254 Rust +60 WASM +11 Python +79 Chrome. Все254 Rust tests также прошли на1.89.0.

ADR0019 фиксирует payload reuse и coordinator. Браузерные unknown-final-sync проверки восстанавливают целый batch без partial publication; invalid input не меняет root и не poison. Последующие source errors не возвращают старые values. File всё ещё append-only; staging256MiB и cap64 таблиц ограничивают первую реализацию. Не выполнены browser-process crash/power-loss proof, physical reclaim, automatic migration и публичное переключение openDB. Следующий срез — явный container-v1 mode в Worker/client с существующим lifecycle/subscriptions и demo.

## C6c: публичный container-v1 и отправка коммитов

- [x] openDB(name,{mode:"container-v1"}) подключает контейнер через Worker. Default wal-v1 сохранён, namespace разделены, имя не запускает migration/fallback. Protocol3 отклоняет несовместимый client/Worker без изменения persistent versions.
- [x] Public writeBatch/flush/compact/stats с AbortSignal. WAL получает UNSUPPORTED без dispatch; container использует существующий root coordinator. Stats generation bigint и counters проверяются до возврата клиенту.
- [x] DBCollection.writeBatch переводит все keys в namespace до dispatch, учитывает physical key+batch bytes. Shared normalize/pack helper переиспользуется низкоуровневым adapter; inputs/pending keys независимо копируются.
- [x] WatchRegistry.notifyMany вызывается только после batch ACK: один вызов на matching registration, snapshot/global order и DBNotificationError committed=true сохранены. Empty batch/flush/compact/stats не уведомляют.
- [x] Демо с default-WAL selector, atomic pair, flush/compact/stats и существующей отменой. Публичные README/OPFS/ADR0020 и tarball consumer без DOM обновлены. Исправлены исторически устаревшие README фразы о непроверенном браузере.
- [x] После параллельной реализации прошёл полный check:283 JS +254 Rust +60 actual-WASM +11 Python +89 Chrome. Read-only review blocker не нашёл. Rust source не менялся;254 MSRV tests проверены в C6b.
- [x] Пользователь явно разрешил commit и push; это отражено в AGENTS.md. Первые23 накопленных проверенных commits уже отправлены origin/main (до6b98b3b). Новые законченные задачи фиксируются и отправляются тем же обычным push без force. Все author/committer — barghxst, без assistant attribution.

Публичный контейнер теперь доступен через demo/API. Дальше остаются DB schema migrations, дополнительная browser matrix/ошибки, explicit legacy import и отдельный протокол physical reclamation; multi-tab/RN/devtools идут последующими этапами плана. npm остаётся private, лицензия и внешняя публикация отдельно не согласованы. GitHub CI не объявляется зелёным без remote проверки.

## KV: классы и web/native backend / 29 октября

По запросу пользователя KV переводится с фабрик на классы и название backend. `new KV({ name, backend?, validate? })` выбирает localStorage на web и MMKV в React Native. Общий слой отвечает за строгий JSON, проверку значений, namespace, snapshots и локальные подписки; backend читает/пишет строки через синхронные read/write/remove. Физические localStorage ключи и Store envelope сохраняются; формат DB не меняется.

Публичный memory backend исключён. Входы `kv/web` и `kv/native` разделены, index.ts содержит только экспорты. Тесты KV вынесены из runtime. SecureKV, шифрование, TTL и async backend не входят в этот срез. Причины и альтернативы — ADR0022.

Проверки: `pnpm check` прошёл — 353 JS, 60 actual-WASM, 254 Rust и 11 Python tests, tarball/SSR/native declarations и 9 demo-launcher tests. Настоящий Chrome: 96 tests прошли. Дополнительно проверено отклонение случайных async backend без ложного уведомления и unhandled rejection. По уточнению пользователя поля/методы классов используют TypeScript private; промежуточный shared barrel удалён, фасады содержат явные экспорты. Нативная сборка и выполнение на настоящих iOS/Android пока не проверены; mock MMKV и проверки типов не заменяют device tests.


## Удаление комментариев / 29 октября

По запросу пользователя удалены поясняющие комментарии из Rust, JS/TS/MJS, Python и примеров документации. Сохранены исполняемые shebang и служебные директивы TypeScript, Vitest и Biome. Лексическая проверка Rust и сравнение AST JS/TS подтвердили сохранение кода; CLI description benchmark runner перенесено из docstring в аргумент argparse, чтобы сохранить help.

`pnpm check` прошёл: 353 JS, 60 actual-WASM, 254 Rust и 11 Python tests; tarball и 9 demo-launcher tests также прошли. Настоящий Chrome: 96 tests прошли.
