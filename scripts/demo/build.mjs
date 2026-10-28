import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

/** Build before listening; keep tool output visible and terminate its process tree on cancellation. */
export function buildProject(root, log, signal) {
  return new Promise((resolve, reject) => {
    const executable = process.env.npm_execpath;
    const command = executable
      ? process.execPath
      : process.platform === "win32"
        ? "cmd.exe"
        : "pnpm";
    const args = executable
      ? [executable, "run", "build"]
      : process.platform === "win32"
        ? ["/d", "/c", "pnpm run build"]
        : ["run", "build"];
    const child = spawn(command, args, {
      cwd: root,
      stdio: ["ignore", "pipe", "pipe"],
      detached: process.platform !== "win32",
      windowsHide: true,
    });
    for (const stream of [child.stdout, child.stderr]) {
      createInterface({ input: stream }).on("line", (line) => log("build", line));
    }
    let forceKill;
    const stop = () => {
      if (!child.pid) return;
      if (process.platform === "win32") {
        const killer = spawn("taskkill", ["/pid", String(child.pid), "/t", "/f"], {
          stdio: "ignore",
        });
        killer.on("error", () => child.kill());
      } else {
        try {
          process.kill(-child.pid, "SIGTERM");
          forceKill = setTimeout(() => {
            try {
              process.kill(-child.pid, "SIGKILL");
              log("warn", "Зависшие процессы сборки остановлены принудительно.");
            } catch (error) {
              if (error.code !== "ESRCH")
                log("warn", `Не удалось завершить сборку: ${error.message}`);
            }
          }, 1500);
        } catch (error) {
          if (error.code !== "ESRCH") log("warn", `Не удалось остановить сборку: ${error.message}`);
        }
      }
    };
    signal.addEventListener("abort", stop, { once: true });
    if (signal.aborted) stop();
    child.once("error", (error) => {
      signal.removeEventListener("abort", stop);
      reject(new Error(`Не удалось запустить сборку: ${error.message}`, { cause: error }));
    });
    child.once("close", (code) => {
      signal.removeEventListener("abort", stop);
      if (forceKill) {
        // The direct child may exit before a stubborn descendant. Keep the
        // bounded group cleanup alive until no member of that group remains.
        try {
          process.kill(-child.pid, 0);
        } catch (error) {
          if (error.code === "ESRCH") clearTimeout(forceKill);
        }
      }
      if (signal.aborted || code === 0) resolve();
      else reject(new Error(`Сборка завершилась с кодом ${code}; сервер не запущен.`));
    });
  });
}
