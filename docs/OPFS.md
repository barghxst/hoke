# Экспериментальный Rust/WASM WAL в браузере

C1 переносит существующий WAL-only Rust Engine в Dedicated Worker с настоящим OPFS. Это отдельный backend hoke/db; KV/localStorage и React entry points не загружают WASM при импорте. Native Database с SST/manifest/compaction остаётся отдельным Rust API.

## Подготовка

Нужны Node24, pnpm, Python3, Rust1.97.1 для сборки, WASM target и wasm-bindgen CLI той же версии, что crate:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
pnpm install --frozen-lockfile
pnpm build
pnpm demo
```

Откройте http://127.0.0.1:8000/examples/opfs/. Нажмите «Открыть базу», запишите значение, закройте и откройте базу, прочитайте. Затем перезагрузите страницу, снова откройте базу и прочитайте. Удаление ключа также сохраняется. Демо использует имя browser-demo, key theme и не очищает весь OPFS.

Если порт8000 занят, после сборки запустите `python3 -m http.server 8001 --bind 127.0.0.1` и откройте тот же путь на порту8001. Другой порт означает другой origin и отдельное хранилище.

`pnpm build:wasm` собирает release hoke-wasm и генерирует JS/WASM/declarations в packages/hoke/wasm. Этот каталог игнорируется Git, но включается в tarball вместе с dist. CLI version проверяется до сборки. CARGO_TARGET_DIR учитывается через cargo metadata. Сборка из исходников требует Rust; установленный npm tarball уже содержит скомпилированный WASM.

## API

```ts
import { openDB } from "hoke/db";

const db = await openDB("settings", { timeoutMs: 30_000 });
const key = new TextEncoder().encode("theme");
try {
  await db.put(key, new TextEncoder().encode("dark"));
  const bytes = await db.get(key); // Uint8Array | undefined
  await db.delete(key);
} finally {
  await db.close();
}
```

mode равен wal-v1. Все методы асинхронны, близкие запросы последовательно выполняются Worker. close идемпотентен. Имя: 1–64 ASCII символа, первый буква/цифра, далее буквы/цифры/_/-. Оно отображается в OPFS/hoke-wal-v1/<name>.wal. Ключи до64KiB, значения до16MiB; пустые и бинарные bytes разрешены. Входные и возвращаемые буферы копируются, caller buffers не detached.

Валидное имя с другим регистром не нормализуется. Origin и имя определяют хранилище; native multi-file каталоги не подключаются автоматически. Второй владелец того же файла получает LOCKED; другой name независим. Никакого readwrite-unsafe и fallback в память/localStorage.

## Подтверждение и ошибки

Put/delete вызывают Rust Engine с существующими WAL v1/CRC32/recovery. ACK возвращается после успешного OPFS handle.flush. Неполный финальный frame обрезается по native recovery; полный corrupt frame и future version отклоняются без переписывания. Номер версии WAL не является номером JS store schema.

DBError содержит code/message. INVALID_ARGUMENT — имя/bytes/options; UNSUPPORTED — нет Worker/secure context/sync OPFS; LOCKED — второй владелец; STORAGE — открытие OPFS/доступ/квота; ENGINE — Rust/WASM/операция adapter; CLOSED — закрытый экземпляр; TIMEOUT/WORKER/PROTOCOL — транспорт. Ошибка I/O движка блокирует последующие mutations до close/reopen, не выдавая ложный ACK; чтение ранее опубликованного in-memory state остаётся возможно.

Таймаут (1..600000ms, default30000), worker crash и повреждённый ответ прекращают Worker и отклоняют все pending запросы. Нельзя отменить отдельный уже выполняемый синхронный вызов; прекращение Worker закрывает всю сессию. Запись могла уже пройти flush до потери ответа. Не повторяйте mutation автоматически: reopen и прочитайте состояние. При транспортном сбое до отправки ответа Worker также пытается освободить engine/handle; при невозможности сигнализации клиент завершит сессию по timeout.

Flush браузера не устраняет quota/eviction, удаление данных пользователем, физическое повреждение или все особенности питания/ФС. Origin storage не является резервной копией. Этот prototype не заявлен production-ready.

## Проверка

```sh
pnpm test:wasm
HOKE_BROWSER_CHANNEL=chrome pnpm test:browser
HOKE_BROWSER_CHANNEL=chrome pnpm check:all
```

`test:wasm` ожидает результат `pnpm build:wasm` и выполняет 16 тестов настоящего WASM с управляемым byte adapter, short I/O и ошибками. Это не замена OPFS-тестам. Browser suite включает 15 низкоуровневых OPFS/WASM сценариев, сценарий запускаемого OPFS демо и 9 прежних KV-демо сценариев. Локально проверен Chrome155/Linux. Установка bundled Chromium и CI описаны в TESTING.md; внешний CI пока не запускался.

Проверяются binary/empty/copy, delete, close/reopen/reload, exclusive ownership, namespace isolation, Worker termination после ACK, corrupt/future version с корректной CRC без переписывания, incomplete tail, ошибки read/write/flush и невозможность отправить ответ. Проверка client protocol/timeouts выполняется отдельными unit tests с fake Worker. Worker termination не равен browser-process crash или отключению питания.

## Границы C1 и следующий шаг

Весь WAL проигрывается в map при open, общий размер базы не ограничивается API. SST, scan/collection, compaction/GC, atomic batch, subscriptions и DB migrations в браузерном API пока отсутствуют. Не используйте этот срез для неограниченного журнала. Полный C ещё не завершён: перенос multi-file publication потребует отдельного ADR и fault tests. Multi-tab здесь означает явный отказ второму владельцу, не совместный доступ.

Архитектура и альтернативы — [ADR0010](adr/0010-browser-wal-worker.md). OPFS требует Dedicated Worker и secure context; источники: [sync handles](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemFileHandle/createSyncAccessHandle), [flush](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemSyncAccessHandle/flush).
