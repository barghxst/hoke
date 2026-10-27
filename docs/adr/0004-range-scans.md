# ADR 0004: ordered scan/range

Статус: принято для неопубликованного Rust API, 27 октября 2026. Дисковые форматы не меняются.

## Варианты

1. Загрузить все значения всех SST в BTreeMap: просто, но отменяет смысл чтения values с диска и требует памяти под всю базу.
2. Слить упорядоченные потоки ключей через heap, читать только победившее значение: выбрано. Используем уже загруженные key indexes, приоритет определяется свежестью источника.
3. Сразу публичный streaming iterator с заимствованием Database: лучше для больших выдач, но усложняет lifecycle/ошибки и будущий async API. Отложено; текущая выдача ограничена limit и собирается в Vec.

## API и семантика

`Database::scan(ScanOptions) -> Result<Vec<KeyValue>, DbError>`.
`Database::range(ScanOptions)` — явный alias того же чтения с bounds.
`KeyValue = (Vec<u8>, Vec<u8>)`.

ScanOptions: start/end как std::ops::Bound<Vec<u8>>, prefix Option<Vec<u8>>, reverse bool, limit usize. Default: оба bound Unbounded, prefix None, reverse false, limit 100. Ключи — байты, порядок лексикографический, без Unicode/locale normalization.

- Included/Excluded работают с обеих сторон независимо; end не всегда exclusive.
- start > end — InvalidRange до чтения values. Равные границы с любой Excluded — пусто, с обеими Included — точечный диапазон.
- Prefix пересекается с заданными bounds; пустой prefix соответствует всему диапазону. Верхняя граница prefix — первый лексикографический successor; у all-FF prefix верхней границы нет.
- limit=0 возвращает пустую выдачу без storage reads для корректных bounds.
- limit считает только живые уникальные ключи, после разрешения версий и tombstones.
- Reverse меняет порядок выдачи, но не приоритет версий.
- Memtable новее всех SST; SST проверяются по порядку generations от новой к старой. Tombstone подавляет все старые значения.
- Данные проигравших версий не читаются; их скрытое повреждение не мешает более новой версии. Ошибка чтения победившего frame возвращает Err без частичного результата и без fallback к старой версии.

## Ресурсы

На каждый SST — cursor по индексу и один heap key. Для memtable отдельные отсортированные iterators values/deleted. Нельзя собирать копию всей базы/индексов ради scan. Стоимость key merge зависит от просмотренных версий и числа runs; values читаются только для победивших живых ключей до limit. Ограничения текущего point reader сохраняются: SST tombstone frame также проверяется, а сами индексы уже в памяти. Result Vec требует памяти под выбранные values; это пока не streaming API.

## CLI

Только режим --db: scan, prefix <prefix>, range <start> <end> (CLI range — [start,end)). Общие флаги --reverse и --limit N. Аргументы/порядок границ/limit проверяются до открытия каталога. Каждая строка печатает key и value в quoted escaped виде. Пустая выдача печатает EMPTY. Rust API поддерживает arbitrary bytes; CLI принимает UTF-8.

## Проверки

Перезаписи/удаления между несколькими SST и memtable, Included/Excluded, пустые/бинарные/all-FF prefix, reverse, limit до/после tombstones, zero/no I/O, invalid bounds, ошибки/коррупция winner без fallback, shadowed corruption, flush/reopen и сравнение с BTreeMap на детерминированных операциях. Проверка реальных CLI-команд и отрицательных аргументов без создания каталога.
