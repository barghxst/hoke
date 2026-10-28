import { spawn } from "node:child_process";

/** Open the loopback URL through the OS, without shell interpolation. */
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
    // Some launchers remain attached to a running browser. Do not hold startup
    // or shutdown on them; accepting the OS launch request is all we can confirm.
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
