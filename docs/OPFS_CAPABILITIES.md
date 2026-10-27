# Проверка возможностей OPFS для публикации SST

Дата: 27 октября 2026. Выполнено локально: Chrome155.0.8059.84/Linux, Playwright1.64.1, отдельный непостоянный browser context и dedicated localhost origin на порту4175. Рабочие базы и профиль пользователя не открывались. Scratch directory удалён; context, browser и HTTP server закрыты.

Команда из корня проекта:

```sh
node scripts/probe-opfs.mjs
```

По умолчанию используется установленный Chrome. Для установленного Playwright channel можно задать `HOKE_BROWSER_CHANNEL`. Занятый порт вызывает ошибку, чужой сервер не переиспользуется.

| Наблюдение                                                                            | Полученный результат                                                                             |
| ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Второй default sync handle на одном source файле                                      | `NoModificationAllowedError`                                                                     |
| `source.move("MANIFEST")` поверх существующего target при открытом source sync handle | `NoModificationAllowedError`                                                                     |
| Та же операция после закрытия source handle                                           | `ok`                                                                                             |
| Содержимое `MANIFEST` после move                                                      | `new` вместо `old`                                                                               |
| Имена в scratch directory после move                                                  | Только `MANIFEST`, прежнее `next` отсутствует                                                    |
| `getFileHandle("MANIFEST", {create:true})` после move                                 | Возвращает существующее содержимое `new`; не исключительное создание                             |
| Reload страницы после закрытия Worker                                                 | `MANIFEST` по-прежнему содержит `new`                                                            |
| Методы directory prototype                                                            | entries/getDirectoryHandle/getFileHandle/keys/removeEntry/resolve/values; flush/sync отсутствуют |
| Методы sync-handle prototype                                                          | close/flush/getSize/mode/read/truncate/write                                                     |

Это точечный API/reload probe. Он не проверяет destination lock, reader во время rename, Worker termination посреди операции, browser-process crash, power loss, quota, eviction или Firefox/WebKit. Нет выводов об атомарности rename и устойчивости directory metadata из успешного Promise. Список prototype — наблюдение реализации, не спецификация.

Проверенные платформенные источники и сравнение трёх архитектурных вариантов — [предложение ADR0014](adr/0014-opfs-multifile-publication-proposal.md). Форматы и production backend этим исследованием не меняются.
