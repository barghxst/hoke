# ADR 0007: настраиваемый порог memtable и pre-write flush

Статус: принято для экспериментального Rust API в рамках незавершённого B3. Дисковые форматы v1 не меняются.

## Варианты

1. Только ручной flush: простая семантика, но потребитель должен сам следить за растущей delta.
2. Flush после успешного put: порог удобен, но ошибка flush приходит уже после commit пользовательской операции; обычный Err двусмысленен и провоцирует повтор. Отложено до отдельного committed-with-maintenance-error контракта.
3. Синхронный flush перед следующей записью, которая превысит порог: выбрано. Существующая delta уже подтверждена в WAL, новая операция ещё не записана. Отказ flush не приводит к скрытому применению новой операции.

## Контракт

DatabaseOptions { max_memtable_bytes: Option<usize> }; default None сохраняет manual flush. Some(0) недопустим; options.validate() можно вызвать до открытия каталога. Database::open_with_options(directory, options) проверяет options и открывает существующие форматы. Настройка локальна экземпляру, не сохраняется в manifest. open/get/scan/info/close не запускают автоматический flush.

Engine учитывает точную сумму длин ключей/значений текущих изменений: live key.len+value.len; tombstone key.len. Повторный put/delete заменяет прежний вклад; записи SST не входят. Это payload bytes, не оценка всей heap memory: BTreeMap nodes, Vec capacity, allocator и индексы SST добавляют накладные расходы. DatabaseStats.memtable_bytes показывает эту сумму. Длина WAL не ограничивается этим порогом: повторные overwrite могут растить журнал при небольшом текущем payload; отдельного WAL-size threshold пока нет.

Перед put/delete:

1. Проверить writable и входные key/value limits без IO.
2. Рассчитать размер delta после операции, учитывая overwrite, delete и повторный tombstone.
3. Если текущая delta непуста и projected bytes > max_memtable_bytes, выполнить обычный flush и только после его успеха append новой операции в WAL.
4. Если порог не превышен, записать в текущий WAL. Равенство порогу flush не вызывает.

Одна операция больше порога допустима, если проходит существующие key/value limits: пустая delta не может быть предварительно сброшена. Поэтому это мягкий payload threshold, не hard memory cap. Максимальный overshoot одной записи ограничен max key64KiB+value16MiB. Замена большого значения меньшим может вернуть delta под порог без flush. Порог не дробит atomic batch: write_batch и так публикует всю delta вместе с пакетом по ADR 0006, пустой batch остаётся no-op.

## Сбои

IO-ошибка preflush требует reopen и блокирует mutations как обычный flush. Новая запрошенная операция не добавляется ни в старый, ни в новый WAL. Ранее подтверждённые изменения сохраняются в старом или новом manifest. После успешного preflush обычный append может завершиться IO-ошибкой с неизвестным исходом по прежнему WAL контракту; это явно другая стадия.

Неверный input проверяется до preflush, не изменяет файлы и не poison базу. Перезапуск восстанавливает payload count из WAL; очередная valid mutation при необходимости выполняет preflush. Ошибки лимитов/публикации самого flush сохраняют прежнюю семантику, automatic compaction/GC не добавляются.

## CLI

--db <path> [--memtable-limit N] <command> ...; N — положительное число bytes, проверяется до NativeDirectory::open. Flag находится до команды, чтобы command/key/value с похожим текстом оставались литералами на прежних местах. Отсутствие flag — manual mode. info/stats дополняются memtable_bytes без удаления прежних полей.

## Проверки

Порог/равенство/oversized single op, overwrite/delete/repeat tombstone/emptybinarykey, recovered delta, manual default, invalid options/input без IO. Fault injection каждого preflush шага с проверкой отсутствия новой операции и сохранности старых подтверждений; all-or-none batch при малом пороге. Ordered model с flush/compact/gc/reopen и разными порогами, child kill вокруг preflush manifest publication. Process kill не доказывает power-loss safety. Настоящие memory/latency benchmarks и фоновый flush остаются отдельными задачами.
