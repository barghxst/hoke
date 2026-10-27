# ADR 0014: предложение о публикации browser SST и compaction

Статус: **пользователь выбрал направление B — один контейнер с журналом commit**, 27 октября 2026. Сравнение ниже сохраняется как обоснование. Выбор разрешает проектирование и реализацию нового container format; точные bytes/framing/recovery фиксируются отдельным ADR до записи production backend. Контейнер, его подключение к openDB и миграция пока не реализованы. Действующие WAL v1 и `mode: "wal-v1"` остаются в силе.

## Задача и граница решения

Browser backend уже подтверждает отдельные mutations после WAL flush. Нужно добавить SST, освобождение memtable, compaction и atomic batch так, чтобы успешные прежние ACK не исчезали после следующей публикации. Разрешение продолжать этап C не выбирает за пользователя долговременный browser commit format.

Native `Directory` синхронно создаёт immutable SST и новый WAL, синхронизирует файлы и имена, заменяет `MANIFEST`, синхронизирует каталог и только затем переключает состояние. `collect_garbage` отдельно синхронизирует видимый manifest перед удалением прежних файлов. Browser adapter не может возвращать успех вместо отсутствующего барьера.

Нужно разделять три требования: целостность после исключения JS/Worker termination; поведение после browser-process crash; физическую потерю питания. Сохранность при eviction/явном удалении пользователем — отдельная проблема. Прохождение тестов одной категории не доказывает остальные.

## Что предоставляет платформа

- В [WHATWG File System](https://fs.spec.whatwg.org/#api) создание/получение файлов и итерация каталогов асинхронны. `getFileHandle({create:true})` также возвращает уже существующий файл: это не `create_new`. У каталога нет `sync_directory`/flush. [`flush()` sync handle](https://fs.spec.whatwg.org/#api-filesystemsyncaccesshandle-flush) переносит изменения содержимого файла в underlying storage; это не описанный барьер каталога или транзакция нескольких файлов. `close()` не заменяет flush.
- Получение [sync access handle](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemFileHandle/createSyncAccessHandle) асинхронно, последующие операции синхронны в Dedicated Worker. Default `readwrite` исключает второго владельца данного файла, но сам по себе не блокирует весь каталог.
- [Chrome документирует `move()`](https://developer.chrome.com/docs/capabilities/web-apis/file-system-access#renaming-and-moving-files-and-folders) для OPFS файлов. Операция асинхронна; её наличие не задаёт переносимый commit barrier. [Проект предложения WHATWG](https://github.com/whatwg/fs/blob/main/proposals/MovingNonOpfsFiles.md#file-locking) описывает необходимость блокировок source/destination; это предложение, а не гарантия fsync.
- [wasm-bindgen поддерживает async Rust/JS promises](https://wasm-bindgen.github.io/wasm-bindgen/reference/js-promises-and-rust-futures.html). Это позволяет написать async coordinator, но не превращает нынешний синхронный `Directory` в async trait и не добавляет storage guarantees.
- [Storage Standard](https://storage.spec.whatwg.org/#persistence) отдельно описывает persistence/eviction и оценку quota. Оценка доступного места не резервирует его; persistence не заменяет commit-протокол.

Практические наблюдения Chrome155 и воспроизводимый probe — [OPFS_CAPABILITIES.md](../OPFS_CAPABILITIES.md). Успешная замена файла и reload не доказывают атомарную или устойчивую к потере питания замену.

## Варианты

| Вариант                                                                               | Что переиспользуется                                                                                        | Цена и оставшийся риск                                                                                                                                                                                                                                      |
| ------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A. Async OPFS coordinator, прежние WAL/SST/manifest v1                                | Существующие byte codecs, merge, scan, capacity checks. JS/Rust state machine явно ожидает create/list/move | Меняется архитектура I/O, но bytes форматов могут остаться прежними. `move` зависит от browser capabilities; эквивалента native directory barrier нет. Без отдельного доказательства нельзя включать destructive GC и заявлять native durability            |
| B. Один OPFS container с append-only commit journal                                   | WAL frames и SST payload можно использовать как вложенные сегменты, Rust merge/scan переиспользуются        | Нужны новый durable container/root format, recovery parser, limits, миграция и eventually внутренний allocator. Убирает зависимость каждой публикации от rename нескольких имён, но не устраняет quota/eviction или проблему первоначального создания файла |
| C. WAL v1 остаётся единственным источником истины, SST только восстанавливаемый cache | Существующий WAL без изменения, SST codec для дополнительных файлов                                         | Cache можно потерять и перестроить, пока весь WAL сохранён. Это не bounded database: replay/map остаются полными; нельзя безопасно удалять старый WAL или добавить atomic batch простым циклом put                                                          |

### A: возможен byte-compatible перенос, но не drop-in Directory

Сначала открыть lock-файл через default sync handle и удерживать его до закрытия всех data handles; все участники нового backend обязаны соблюдать эту блокировку. Затем async coordinator создаёт уникальные поколения, проверяет отсутствие collisions при одном владельце, получает handles, вызывает синхронные codecs, ожидает закрытие/публикацию там, где этого требует `move`. `getFileHandle({create:true})` без проверки и owner lock не является безопасным create-new.

Для flush: записать/flush SST; записать/flush новый WAL; записать/flush временный manifest; закрыть source/destination handles; дождаться move поверх `MANIFEST`; проверить опубликованный manifest; только затем ACK/switch. При ошибке после начала I/O — poison/reopen и неизвестный исход. Никакого remove старого manifest перед move. Batch использовал бы тот же переход с включением всей delta и операций.

Это сохраняет bytes native formats, но **не выполняет контракт native Directory**, поскольку отсутствует его directory durability barrier. Даже сохранение прежних файлов не даёт разрешения молча выбрать старый manifest: он может не содержать последующие ACK. Реализация допустима лишь после явного определения более узкого browser crash-контракта и проверки платформ; destructive GC до этого запрещён. Наличие `move()` в одном Chrome недостаточно для выбора A.

### B: выбранное направление полного backend

Пользователь выбрал container с журналом commit вместо отдельных SST/WAL/manifest. Все SST/WAL/commit records находятся в одном заранее открытом файле. Candidate immutable segments пишутся после подтверждённого конца; затем flush; затем append checksummed root record со списком segment offsets/lengths, generation и границей replay; затем flush; затем switch/ACK. До root publication прежний root и его диапазоны не меняются. Root record должен однозначно ограничивать целый batch; ACK отдельных его элементов нет.

Точный layout/magic/version, framing, CRC coverage, числа/endianness, запись пустой базы и terminal marker **здесь не выбираются**. Предварительные инварианты: checked offsets и lengths; конечные caps до allocations; strictly increasing generations; bounded root count/size; ссылки только на полностью записанные допустимые segments; неизвестная версия — явная ошибка до записи. Полный corrupt record не трактуется как незавершённый хвост. CRC обнаруживает ошибки, но не доказывает атомарность arbitrary torn writes.

Первый срез после принятия формата должен оставаться append-only: логическая compaction освобождает memtable и уменьшает число активных SST, однако **не уменьшает размер container**. Нельзя объявлять проблему disk growth решённой. Truncate/rewrite файла с подтверждёнными страницами или перенос в другой файл для reclamation требуют нового протокола. Для reclaim внутри файла понадобятся устойчивый root selection, защищённые ranges, журнал allocator и отдельный fault proof. Два mutable root slots сами по себе этого не доказывают: повреждение нового slot нельзя автоматически принять за разрешение откатить ACK.

Это направление сохраняет один sync handle и явный file flush, но задаёт новый долговременный формат и затрагивает recovery. Направление B принято пользователем; конкретный layout и протокол восстановления ещё предстоит зафиксировать перед реализацией.

### C: безопасный промежуточный предел

Новый SST cache не участвует в commit и может быть полностью восстановлен только из неизменённого WAL. При повреждении cache его rebuild допустим именно потому, что он не определяет подтверждённое состояние. Cache metadata не должен маскироваться под authoritative manifest. Уменьшение WAL или batch ACK требуют перехода к A/B либо отдельного WAL format decision. Без измеримого выигрыша такой cache сейчас не стоит реализовывать.

## Совместимость и владение

Существующий `OPFS/hoke-wal-v1/<name>.wal` остаётся читаемым прежним `WasmWal`; magic/version/ACK semantics не меняются. Native каталоги не импортируются автоматически. Для B предлагается явный export/import в **другое имя и namespace**, а не незаметная миграция при open. Нужно захватить legacy WAL handle на весь snapshot/export, построить destination, flush/verify/reopen его и сверить полный logical state. Ошибка оставляет source доступным после release lock; destination до завершения не выдаётся готовым.

Старый клиент не знает новый migration marker и после release lock сможет снова писать в прежний WAL. Поэтому export/import не обещает, что два backend с одним логическим именем синхронизированы. Запрет old-client reopen или автоматическое same-name переключение требует отдельно спроектированной совместимости. Source не удаляется автоматически даже после успешного импорта. При обнаружении обеих копий API требует явного backend/name; silent fallback/downgrade отсутствует.

Импорт проверяет bytes коллекций и tombstones без переадресации ключей; subscriptions не переносятся и создаются после reopen. Нет совместных locks через `readwrite-unsafe`, нет скрытого маршрутизатора нескольких вкладок. Lock collision — `LOCKED`; потеря ответа после commit означает неизвестный исход без автоматического повтора.

## Capacity, quota и ошибки

Наследуемые пределы: key64KiB/value16MiB, batch1024/32MiB, SST1M entries/index64MiB, manifest4096 tables. Они не ограничивают RSS/WASM heap и не доказывают возможность конкретного размера OPFS. Для нового coordinator дополнительно нужны caps суммарных открытых handles, metadata bytes, copy buffers и offsets в JS safe integer range. Предварительная оценка пикового места включает старые активные данные, новые SST/WAL/root и ещё не удалённые orphan segments.

Чистая ошибка аргументов/capacity до I/O оставляет базу writable. Quota/short write/flush/move exception после I/O не выдаёт success и требует reopen там, где исход неизвестен. Частичный batch не отображается; старые ACK сохраняются. Ошибка cleanup после commit — отдельный committed результат, без повторной записи batch. Storage estimate только подсказка; запрос persistence должен оставаться явным действием приложения, не скрытой предпосылкой корректности.

## Матрица обязательных отказов

| Точка отказа                                                               | Проверяемое поведение                                                                              |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Invalid input/capacity до I/O                                              | Нет файловых изменений, прежнее состояние writable                                                 |
| Создание/запись SST или container segment; short/zero write/quota          | Нет success ACK; прежние ACK доступны; неполный candidate не становится активным                   |
| Flush SST/нового WAL/segment                                               | Ошибка видна, неизвестный I/O исход не приводит к продолжению старого writer                       |
| Запись/flush candidate manifest/root                                       | После reopen только полное old/new состояние; batch целиком либо отсутствует                       |
| Перед/после move (A), перед/после final root flush (B)                     | Проверять отдельно Worker kill, browser-process crash и fault injection; все прежние ACK сохранены |
| Commit успешен, reply потерян/Abort/timeout                                | Возможен полный commit без ACK; нет rollback claim или auto retry                                  |
| Reopen после неопределённого исхода, затем новая mutation и ещё один crash | Новый ACK не теряется из-за fallback к старому поколению                                           |
| Referenced SST/WAL/segment отсутствует или CRC/version неверны             | Явная ошибка; не создание пустого файла и не автоматический old-root fallback                      |
| Cleanup error до/после удаления, quota при повторе                         | Опубликованное состояние остаётся читаемым; cleanup не повторяет mutation                          |
| Второй owner, stale worker, close/open race                                | Только один compliant writer, старый worker не публикует после release lock                        |
| Legacy import failure/unknown commit                                       | Source неизменён; destination не объявлен ready; повторный явный импорт не перезаписывает данные   |

Модель B должна формально описать допустимый незавершённый append tail и отличить его от full-record corruption. Fault harness проверяет каждую файловую операцию и частичный prefix записи; model-based trace чередует put/delete/batch/flush/compact/reopen. Crash test сохраняет independent ledger ACK, а не только последний запрос. Нельзя заменять browser-process crash вызовом `worker.terminate()`.

## Последовательность следующей работы

1. Зафиксировать API facts и ограничения этого предложения; текущие WAL v1 и C4 lifecycle развиваются независимо.
2. Выбор B сделан пользователем. Следующий срез фиксирует bytes/framing/recovery/capacity ADR и реализует append-only container journal. Возвращаться за повторным разрешением на само направление или новый формат не требуется.
3. По уточнению пользователя сначала реализовать связный код контейнера, затем интеграционно проверить fault matrix с independent ACK ledger, включая неизвестный commit и последующий reopen/write/crash. Отдельный simulator пока отложен.
4. Реализовать выбранный experimental backend отдельно от WAL v1: сначала create/open/put/reopen и format rejection, затем flush/SST reads/scan, затем atomic batch и logical compaction. Не включать destructive GC или automatic migration по умолчанию.
5. Прогнать реальные Chrome/Firefox/WebKit capabilities и engine tests отдельно; неподдерживаемая среда выдаёт явную ошибку. После fault/model/real-browser проверок обновить заявляемый контракт и benchmark. Space reclamation получает собственное решение и тесты.

Направление B разрешено для реализации после фиксации конкретного container codec/recovery контракта. Варианты A/C остаются историческими альтернативами; автоматического переключения на них нет. Этот ADR не завершает этап C.
