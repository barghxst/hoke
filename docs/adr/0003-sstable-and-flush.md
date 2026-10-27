# ADR 0003: SSTable, manifest и безопасный flush

Статус: принято как обратимое решение для неопубликованного прототипа по запросу пользователя реализовать следующий шаг. Формат WAL v1 и существующий Engine не меняются. Новые SST/manifest имеют независимые experimental версии.

## Варианты

1. Переписывать единственный snapshot на месте: проще, но ошибка записи может повредить единственную копию. Отклонено.
2. Неизменяемые SST + атомарно заменяемый manifest: больше файлов и явный протокол публикации, зато до commit работает старое состояние, после — новое. Выбрано.
3. Сразу leveled LSM с блоками, bloom, compaction и фоновым flush: потенциально лучше масштабируется, но слишком много новых состояний и форматов для первого проверяемого шага. Отложено.

## Границы

Добавляется Database<D: Directory> с отдельным каталогом и эксклюзивным LOCK. Это отдельный API поверх WAL Engine и SSTable, старый WAL-only Engine и CLI-команды остаются совместимы. Нет автоматической миграции старого WAL в каталог.

Синхронный единственный писатель. Ручной flush, без фоновых потоков. Memtable содержит изменения после последнего flush, включая tombstones. Чтение: memtable, затем SST от новой к старой. Tombstone прекращает поиск, чтобы старое значение не воскресло. Дубликаты ключей между таблицами допустимы, внутри одной таблицы ключи строго упорядочены.

После успешного flush значения memtable освобождаются. В памяти остаются индексы ключей SST, значения читаются из файла по запросу. Первый индекс содержит каждый ключ/offset; sparse/block index, bloom и scan будут следующими шагами. Во время flush создаётся копия delta, поэтому пиковая память ещё не ограничена размером streaming buffer.

## Файлы

- LOCK: lock каталога, удерживается до drop Database.
- MANIFEST: checksummed binary list активных SST generations и активной WAL generation.
- g<20-digit-u64>.sst: immutable sorted run.
- g<20-digit-u64>.wal: WAL v1 новой memtable; sequence внутри нового WAL начинается с 1.
- g<20-digit-u64>.manifest.tmp: готовый новый manifest до atomic rename.

Generation выбирается выше всех существующих имён, включая orphan files. create_new не перезаписывает файлы. Referenced files открываются строго existing-only: missing WAL/SST — ошибка, не создание пустого файла. База без manifest создаётся только в пустом каталоге (кроме LOCK); непустой каталог без manifest — явная ошибка.

SST: magic/version/count/index offset + header CRC; записи в WAL-frame encoding с ordinal sequence 1..N; sorted key/offset index + CRC. Max index 64 MiB, max 1M entries, key/value limits как в WAL. Заголовки и индекс проверяются при открытии; полный CRC записи — при чтении соответствующего ключа. Никаких неограниченных allocations по длинам из файла.

Manifest: magic/version, WAL generation, количество SST, возрастающий список поколений и CRC. Ограничение 4096 таблиц; сверх лимита нужен будущий compaction, ошибка не теряет данные.

## Протокол flush

0. Проверить writable state. При пустой delta flush — no-op.
1. Выделить новое generation, создать SST через create_new, записать sorted delta с tombstones, flush + sync_all.
2. Открыть и проверить готовый SST из того же handle.
3. Создать новый WAL и Engine, sync нового заголовка. sync_directory для новых имён файлов.
4. Записать новый manifest.tmp с полным списком SST и новой WAL generation; flush + sync_all.
5. rename manifest.tmp → MANIFEST без удаления старого manifest перед rename.
6. sync_directory. Только после успеха переключить активные handles и освободить старую memtable.

Ошибки flush после начала файловой операции блокируют новые mutations до reopen. Чистая проверка SST capacity и ошибки лимита числа таблиц/поколений до создания файлов оставляют базу доступной: можно выполнить compact/GC или уменьшить delta и повторить. После ошибки rename/dir sync исход публикации может быть неизвестен; reopen выбирает единственный видимый MANIFEST и проверяет все его ссылки. База не продолжает писать в старый WAL после неясного результата.

## Сбои и мусор

До manifest commit старое состояние остаётся полным: old SST + старый WAL. После commit новое состояние полное: new SST + пустой new WAL. Старый WAL и orphan files не участвуют в replay. Старые файлы пока не удаляются; garbage collection и compaction отдельно, это сохраняет простой проверяемый протокол. Активный WAL после flush короткий, суммарное место каталога пока может расти.

Полный corrupt SST/manifest и missing referenced file вызывают ошибку. Не используем fallback к прошлому manifest: он может не содержать подтверждённых после flush записей. Corrupt payload обнаруживается при чтении соответствующего ключа; open не выполняет scrub всех value bytes.

NativeDirectory фиксирует абсолютный canonical path при open, поэтому последующая смена cwd не перенаправляет операции в другой каталог. Каталог синхронизируется после создания/rename, ошибки передаются. Проверяется Linux/process crashes и fault injection; вызовы fsync не равнозначны доказательству power-loss safety на всех ФС/ОС. Windows/macOS и OPFS не заявляются протестированными.

## Проверки

Roundtrip нескольких flush/reopen, overwrite/delete/tombstones, пустые и бинарные ключи, освобождение memtable, corruption и bounds SST/manifest, missing references, orphan collision, эксклюзивный directory lock. Инъекция отказа каждой файловой операции flush, reopen старого или нового целого состояния, сохранение всех подтверждённых данных. Принудительное завершение child до и после manifest rename.
