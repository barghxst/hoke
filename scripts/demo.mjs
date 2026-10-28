import { access } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { openBrowser } from "./demo/browser.mjs";
import { buildProject } from "./demo/build.mjs";
import { createLogger, printLinks } from "./demo/log.mjs";
import { startDemoServer } from "./demo/server.mjs";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const log = createLogger();

function options(args) {
  const result = { port: 8000, open: true, build: true, strictPort: false };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--") continue;
    if (arg === "--help" || arg === "-h") return { help: true };
    if (arg === "--no-open") result.open = false;
    else if (arg === "--no-build") result.build = false;
    else if (arg === "--strict-port") result.strictPort = true;
    else if (arg === "--port" || arg.startsWith("--port=")) {
      const value = arg === "--port" ? args[++i] : arg.slice(7);
      if (!/^\d+$/.test(value ?? "") || Number(value) > 65535) {
        throw new Error("--port требует целое число от 0 до 65535.");
      }
      result.port = Number(value);
    } else
      throw new Error(
        `Неизвестный параметр: ${JSON.stringify(arg)}. Используйте pnpm demo --help.`,
      );
  }
  return result;
}

async function main() {
  const config = options(process.argv.slice(2));
  if (config.help) {
    console.log(`Hoke · запуск демо

  pnpm demo                    Сборка, сервер и открытие браузера
  pnpm demo --no-open          Только сборка, сервер и ссылки
  pnpm demo --port 8080        Начать поиск свободного порта с 8080
  pnpm demo --strict-port      Завершиться, если выбранный порт занят
  pnpm demo --no-build         Использовать уже собранные JS/WASM

  По умолчанию: http://127.0.0.1:8000/. Ctrl+C — завершить.`);
    return;
  }
  const controller = new AbortController();
  let running;
  let stopping;
  const shutdown = () => {
    if (stopping) return stopping;
    controller.abort();
    log("info", "Останавливаем демо…");
    stopping = (async () => {
      try {
        await running?.close();
      } catch (error) {
        log("error", error.message);
        process.exitCode = 1;
      }
    })();
    return stopping;
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
  try {
    if (config.build) {
      const started = performance.now();
      log("info", "Собираем Rust/WASM и TypeScript…");
      await buildProject(root, log, controller.signal);
      if (controller.signal.aborted) return;
      log("info", `Сборка готова за ${((performance.now() - started) / 1000).toFixed(1)} с.`);
    } else log("info", "Используем готовую сборку (--no-build).");
    for (const path of [
      "packages/hoke/dist/db/index.js",
      "packages/hoke/dist/kv/index.js",
      "packages/hoke/wasm/hoke_wasm_bg.wasm",
    ]) {
      try {
        await access(join(root, path));
      } catch {
        throw new Error(`Не найден ${path}. Запустите pnpm demo без --no-build.`);
      }
    }
    if (controller.signal.aborted) return;
    running = await startDemoServer({
      root,
      port: config.port,
      strictPort: config.strictPort,
      log,
    });
    if (controller.signal.aborted) {
      await running.close();
      return;
    }
    printLinks(running.origin);
    if (config.port !== 0 && running.port !== config.port) {
      log(
        "warn",
        `Выбран порт ${running.port}. Данные браузера привязаны к адресу и порту; прежняя база остаётся на прежнем адресе.`,
      );
    }
    if (config.open) {
      log("info", "Открываем главное меню в браузере…");
      try {
        await openBrowser(`${running.origin}/`);
      } catch (error) {
        if (!controller.signal.aborted)
          log(
            "warn",
            `Браузер не удалось открыть автоматически: ${error.message}. Перейдите по ссылке выше.`,
          );
      }
    }
    if (!controller.signal.aborted)
      log("info", "Сервер работает. Изменили библиотеку — перезапустите pnpm demo для пересборки.");
  } catch (error) {
    if (!controller.signal.aborted) {
      log("error", error.message);
      process.exitCode = 1;
    }
    await running?.close();
  } finally {
    if (!running || controller.signal.aborted || process.exitCode) {
      process.off("SIGINT", shutdown);
      process.off("SIGTERM", shutdown);
    }
  }
}

await main().catch((error) => {
  log("error", error.message);
  process.exitCode = 1;
});
