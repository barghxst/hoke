# Имя Hoke и совместимость данных

По решению пользователя библиотека называется hoke. npm imports: hoke/kv и hoke/react; Rust crate hoke-core, Rust import hoke_core; CLI hoke; native benchmarks hoke-bench. Репозиторий находится в /home/barghest/hoke. Пакеты остаются private/publish=false; внешняя публикация не выполнялась.

Имена package/crate, директории исходников, примеры, tests и tooling обновлены. Старые команды cargo -p stowtree-core и npm imports stowtree/* заменяются новыми именами; compatibility aliases пакетов не создаются.

Переименование не является миграцией хранилища. Сохраняются:

- localStorage prefix `stowtree:` и envelope marker `__stowtree: 1`, чтобы новое имя пакета читало прежние данные на том же origin и namespace.
- WAL magic `STOWWAL\0`, SST magic `STOWSST\0`, manifest magic `STOWMAN\0` и версии v1. Golden codec fixtures и CRC по-прежнему проверяются.
- Имена generation-файлов, MANIFEST/LOCK и протокол публикации. Существующий каталог открывается через hoke-core без конвертации.

Store schema versions не меняются. В исходниках и DevTools прежнее слово остаётся только как часть persisted wire format или явно отмеченной истории переименования, а не актуальное npm имя. Tarball smoke устанавливает hoke во внешний consumer, читает старую запись и обновляет её по прежнему физическому ключу без создания дубликата.
