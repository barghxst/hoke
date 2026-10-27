# Локальная исходная точка: 27 октября 2026

Команда из корня репозитория:

```sh
pnpm bench --runs 3 --workdir . --output benchmarks/results/2026-10-27-native-baseline
```

Параметры: 1000 ключей по 16 bytes, values 256 bytes, batch 100, scan 100 строк × 50 вызовов, seed 1; три независимых набора. Release, sync_all, тёплый кэш ОС. Временные базы удалены после проверки по модели.

Среда: Linux-7.2.11-arch1-1-x86_64-with-glibc2.44; CPU AMD Ryzen 5 5600 6-Core Processor. Подробности файловой системы, версии инструментов, параметры и fingerprint исходников находятся в [metadata.json](metadata.json). Тип ФС сам по себе не определяет физический накопитель.

| Case             | p50 API, мкс | Медиана суммы API, мс |
| ---------------- | -----------: | --------------------: |
| put_sequential   |      1818.57 |               1859.61 |
| overwrite_random |      1812.91 |               1851.22 |
| reopen_wal       |      7375.54 |                  7.38 |
| flush            |     18279.64 |                 18.28 |
| get_sst_random   |         1.96 |                  1.93 |
| get_sst_missing  |         0.13 |                  0.16 |
| scan_forward     |       184.69 |                  9.97 |
| scan_reverse     |       183.88 |                  9.34 |
| batch_put        |     12211.96 |                122.31 |
| compact          |     22427.56 |                 22.43 |
| reopen_sst       |      1181.27 |                  1.18 |
| gc               |      5013.25 |                  5.01 |

Полные статистики и rates — [summary.json](summary.json); отдельные samples и snapshots — run-001.jsonl, run-002.jsonl, run-003.jsonl. Для одноразовых операций всего три samples, p95/p99 не выводятся. Сумма времени API исключает подготовку, проверку и close. Эти результаты относятся к данному окружению; сравнение с другими движками не проводилось.

Методика и значения calls/items — [BENCHMARKS.md](../../../docs/BENCHMARKS.md).

Этот сохранённый прогон выполнен до последующего аудита путей NativeDirectory и retryable capacity errors. Fingerprint metadata относится к измеренным исходникам; результаты не переименовываются в замеры более позднего кода.
