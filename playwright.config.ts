import { defineConfig } from "@playwright/test";

const port = Number(process.env.HOKE_BROWSER_PORT ?? "4173");
if (!Number.isInteger(port) || port < 1024 || port > 65535) {
  throw new Error("HOKE_BROWSER_PORT must be an integer between 1024 and 65535");
}
const baseURL = `http://127.0.0.1:${port}`;
const channel = process.env.HOKE_BROWSER_CHANNEL;

export default defineConfig({
  testDir: "./tests/browser",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  workers: 2,
  reporter: "list",
  use: {
    baseURL,
    browserName: "chromium",
    ...(channel ? { channel } : {}),
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: `python3 -m http.server ${port} --bind 127.0.0.1`,
    url: `${baseURL}/examples/web/`,
    reuseExistingServer: false,
    timeout: 15_000,
    stdout: "ignore",
    stderr: "ignore",
  },
});
