import { spawn } from "node:child_process";

export async function openBrowser(url) {
  const [command, ...args] =
    process.platform === "darwin"
      ? ["open", url]
      : process.platform === "win32"
        ? ["rundll32.exe", "url.dll,FileProtocolHandler", url]
        : ["xdg-open", url];
  await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: "ignore",
      detached: process.platform !== "win32",
      windowsHide: true,
    });

    const timer = setTimeout(resolve, 1500);
    const finish = (error) => {
      clearTimeout(timer);
      if (error) reject(error);
      else resolve();
    };
    child.once("error", finish);
    child.once("exit", (code) =>
      finish(code === 0 ? undefined : new Error(`${command} завершился с кодом ${code}`)),
    );
    child.unref();
  });
}
