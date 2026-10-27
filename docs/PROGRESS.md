# Ход работ

Дата: 27 октября 2026.

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
- [ ] Внешний GitHub-репозиторий и публикация npm — не выполнялись.

## Последняя общая проверка

C1: `HOKE_BROWSER_CHANNEL=chrome pnpm check:all` прошёл после интеграции Rust/WASM/Worker/OPFS; после добавления ещё трёх browser cases повторены typecheck и полный browser suite:

- Biome, TypeScript build/typecheck, installed tarball KV/React/DB + JS/WASM assets;
- 159 JS tests в 8 файлах (включая 41 DB client/protocol/lifecycle case);
- 16 actual-WASM adapter tests в Node;
- rustfmt, строгий Clippy, 192 Rust tests;
- 11 Python benchmark tests;
- 25 настоящих Chrome155/Linux browser tests: 15 OPFS/WASM +1 OPFS demo +9 KV demo;
- отдельно 192 Rust tests на MSRV1.89.0/Linux, WASM release build на wasm32-unknown-unknown.

CI подготовлен для генерации WASM и установки браузера; внешнего запуска CI пока не было. Firefox/WebKit, Windows/macOS и физическая потеря питания не проверялись.

Ранее отдельно проверено сохранение/удаление между тремя отдельными процессами Node с Web Storage. CI подготовлен, но ещё не запускался на GitHub.

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
- Rust: этап B, работают WAL Engine и Database с SSTable/manifest и ручным flush. JS DB API/WASM/RN/devtools пока отсутствуют. Этапы C–F не начаты.
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
