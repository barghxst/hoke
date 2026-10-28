import { styleText } from "node:util";

export function createLogger(output = process.stdout) {
  const colors = output.isTTY && !process.env.NO_COLOR;
  const paint = (color, text) => (colors ? styleText(color, text) : text);
  return (level, message) => {
    const time = new Date().toLocaleTimeString("ru-RU", { hour12: false });
    const label = { info: "INFO ", warn: "WARN ", error: "ERROR", build: "BUILD" }[level] ?? level;
    const color = level === "error" ? "red" : level === "warn" ? "yellow" : "cyan";
    output.write(`${paint("dim", time)} ${paint(color, label)} ${message}\n`);
  };
}

export function printLinks(origin, output = process.stdout) {
  output.write(`
  Hoke · демо готово

  Главное меню   ${origin}/
  Список задач   ${origin}/examples/documents/
  Настройки      ${origin}/examples/web/
  Байтовая база  ${origin}/examples/opfs/

  Ctrl+C — остановить сервер.

`);
}
