# Native benchmarks

Измеряем Rust API `hoke-core`, один синхронный writer. Это локальная исходная точка для дальнейших изменений движка. JS/localStorage, WASM, Worker и React Native в замеры не входят.

## Запуск

Нужны Rust/Cargo и Python 3. Runner сам собирает release binary один раз, затем запускает три независимых набора данных:

```sh
pnpm bench
```

По умолчанию временные базы создаются в системном временном каталоге, результаты — в новом `bench-results/<UTC>-<id>`. Чтобы выбрать файловую систему и сохранить результаты по явному пути:

```sh
pnpm bench --runs 3 --keys 1000 --key-bytes 16 --value-bytes 256 --scan-limit 100 --scan-iterations 50 --batch-size 100 --seed 1 --workdir . --output bench-results/local-baseline
```

`--workdir` должен существовать; `--output` должен быть новым каталогом. Runner удаляет только собственные временные базы. Повторный запуск с тем же output отклоняется. `--storage-label` позволяет добавить описание накопителя от пользователя; сам runner не определяет модель физического диска.

Число batch SST и двух подготовительных flush ограничено: ceil(keys/batch-size)+2 <= 4096. Каждый параметр имеет фиксированные границы, см. `pnpm bench --help` и ADR 0009. При уменьшении keys уменьшайте также scan-limit. Бюджеты подготовленных данных и batch ограничивают заведомо чрезмерные нагрузки, но не задают предел RSS: модель, индексы движка и накладные расходы памяти считаются отдельно.

## Что измеряется

| Case                        | Операции                                              | items                          |
| --------------------------- | ----------------------------------------------------- | ------------------------------ |
| put_sequential              | N последовательных put с sync_all                     | N записей                      |
| overwrite_random            | N перезаписей в порядке seed                          | N записей                      |
| reopen_wal                  | Одно открытие с восстановлением WAL                   | N живых ключей                 |
| flush                       | Один сброс delta в SST/manifest                       | N ключей delta                 |
| get_sst_random              | N успешных get после flush                            | N чтений                       |
| get_sst_missing             | N get отсутствующих ключей                            | N проверок отсутствия          |
| scan_forward / scan_reverse | scan-iterations вызовов с лимитом scan-limit          | Фактически возвращённые строки |
| batch_put                   | ceil(N/batch-size) atomic batch в отдельной базе      | N mutations                    |
| compact                     | Одно слияние после подготовленных update/delete/flush | Оставшиеся живые ключи         |
| reopen_sst                  | Одно открытие после compaction                        | Оставшиеся живые ключи         |
| gc                          | Одна очистка устаревших файлов после point flush      | Удалённые файлы                |

Put/overwrite измеряют подтверждение после sync_all. Batch использует нынешний SST/manifest commit: он обеспечивает атомарность, его нельзя интерпретировать как WAL group commit. Пороги автоматического flush выключены. Подготовка устаревших версий для compact выполняется вне таймера.

Instant охватывает отдельный вызов API; elapsed_ns — сумма таких samples, а не время всего процесса. Подготовка ключей, values, порядка и batch chunks, проверка по BTreeMap, snapshots, close и печать выполняются вне таймера. Выделение памяти и I/O внутри API входят в измерение. У коротких get заметна цена самого таймера.

Кэш ОС прогрет: перед SST reads база проверяется вне таймера. Page-cache eviction не выполняется. Reopen показывает повторное открытие базы внутри того же процесса с тёплым файловым кэшем; это не cold start и не проверка потери питания. Файловая система временного каталога может быть tmpfs/overlay; смотрите metadata и используйте выбранный workdir для воспроизводимых сравнений.

## Файлы результата

- metadata.json: параметры, состояние running/failed/complete, UTC, CPU/OS, Rust/Cargo/Python, workdir/ФС, git status/revision при наличии и SHA256 исходников.
- run-NNN.jsonl: config и 12 measurements с samples_ns, calls, items, before/after.
- run-NNN.stderr, build.stdout, build.stderr: диагностика.
- summary.json: сводка только после успешной проверки всех реплик.

Snapshot включает memtable_entries, memtable_bytes, table_count, wal_bytes и количество/суммарную длину файлов каталога базы. Это apparent bytes, не занятые блоки диска и не память процесса. Старые файлы до GC также учитываются.

Сводка содержит min/median/max API-time и rates между репликами. p50 вычисляется по объединённым samples; p95 только при 20 samples, p99 при 100. Для одиночных flush/compact/reopen трёх реплик недостаточно для хвостовых percentiles. Нулевая длительность даёт rate null. Items/s имеют разные единицы по таблице: строки, mutations либо удалённые файлы.

Сравнивайте одинаковые параметры, сборку, ОС, нагрузку и файловую систему. Несколько локальных реплик не доказывают превосходство над другими библиотеками. CI проверяет корректность harness без порогов скорости.

## Проверка harness

```sh
pnpm test:bench
cargo test -p hoke-core --test benchmark_cli
pnpm check
```

Rust process tests проверяют завершённый сценарий, счётчики и отклонение неверных аргументов без изменения файлов. Python tests проверяют JSON-протокол, статистику и обработку ошибок. Успех benchmark означает, что состояние проверено моделью; аварийный сценарий не выдаёт частичный успешный stdout.

Локальная исходная точка с тремя release-прогонами и raw data: [27 октября 2026](../benchmarks/results/2026-10-27-native-baseline/README.md).
