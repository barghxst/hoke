import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
const testOptions = { timeout: 15000 };

async function temporaryDirectory(t) {
  const directory = await mkdtemp(join(tmpdir(), "hoke-demo-launcher-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

function launch(t, args, env = {}) {
  let output = "";
  let ended = false;
  const child = spawn(process.execPath, [join(root, "scripts/demo.mjs"), ...args], {
    cwd: root,
    env: { ...process.env, NO_COLOR: "1", ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (chunk) => {
    output += chunk;
  });
  child.stderr.on("data", (chunk) => {
    output += chunk;
  });
  const closed = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => {
      ended = true;
      resolve({ code, signal });
    });
  });
  t.after(async () => {
    if (!ended) child.kill("SIGTERM");
    const killer = setTimeout(() => {
      if (!ended) child.kill("SIGKILL");
    }, 2000);
    try {
      await closed;
    } finally {
      clearTimeout(killer);
    }
  });
  return {
    child,
    closed,
    get output() {
      return output;
    },
    async waitFor(pattern) {
      const deadline = Date.now() + 8000;
      while (!pattern.test(output)) {
        assert.ok(!ended, `Demo exited before ${pattern}:\n${output}`);
        assert.ok(Date.now() < deadline, `Demo timed out waiting for ${pattern}:\n${output}`);
        await delay(20);
      }
      return output;
    },
  };
}

test(
  "CLI prints clickable URLs, serves the hub, and SIGINT releases its port",
  testOptions,
  async (t) => {
    const process = launch(t, ["--no-build", "--no-open", "--port", "0"]);
    await process.waitFor(/Сервер работает/);
    const links = [...process.output.matchAll(/http:\/\/127\.0\.0\.1:\d+\/\S*/g)].map(
      (match) => match[0],
    );
    assert.equal(links.length, 4, process.output);
    assert.deepEqual(
      links.map((link) => new URL(link).pathname),
      ["/", "/examples/documents/", "/examples/web/", "/examples/opfs/"],
    );
    const response = await fetch(links[0]);
    assert.equal(response.status, 200);
    assert.match(await response.text(), /Hoke/);
    process.child.kill("SIGINT");
    assert.deepEqual(await process.closed, { code: 0, signal: null });
    assert.match(process.output, /Останавливаем демо/);
    const listener = createServer();
    await new Promise((resolve, reject) => {
      listener.once("error", reject);
      listener.listen(Number(new URL(links[0]).port), "127.0.0.1", resolve);
    });
    await new Promise((resolve, reject) =>
      listener.close((error) => (error ? reject(error) : resolve())),
    );
  },
);

test("unknown arguments fail before build or browser launch", testOptions, async (t) => {
  const directory = await temporaryDirectory(t);
  const fakeBuild = join(directory, "unexpected-build.mjs");
  const marker = join(directory, "build-ran");
  await writeFile(
    fakeBuild,
    `import {writeFileSync} from 'node:fs'; writeFileSync(${JSON.stringify(marker)}, 'ran');`,
  );
  const process = launch(t, ["--not-a-demo-option"], { npm_execpath: fakeBuild });
  assert.deepEqual(await process.closed, { code: 1, signal: null });
  assert.match(process.output, /ERROR.*Неизвестный параметр/);
  assert.doesNotMatch(process.output, /BUILD|Открываем|http:\/\//);
  await assert.rejects(readFile(marker), { code: "ENOENT" });
});

test(
  "failed build logs tool output and exits without starting the server",
  testOptions,
  async (t) => {
    const directory = await temporaryDirectory(t);
    const fakeBuild = join(directory, "failed-build.mjs");
    await writeFile(
      fakeBuild,
      'console.log("fake build stdout"); console.error("fake build stderr"); process.exit(7);',
    );
    const process = launch(t, ["--no-open", "--port", "0"], { npm_execpath: fakeBuild });
    assert.deepEqual(await process.closed, { code: 1, signal: null });
    assert.match(process.output, /\d{2}:\d{2}:\d{2} BUILD fake build stdout/);
    assert.match(process.output, /\d{2}:\d{2}:\d{2} BUILD fake build stderr/);
    assert.match(process.output, /ERROR.*кодом 7.*сервер не запущен/);
    assert.doesNotMatch(process.output, /демо готово|Открываем|http:\/\//);
  },
);

test("default browser launch receives an HTTP URL after the server is ready", {
  ...testOptions,
  skip: process.platform !== "linux",
}, async (t) => {
  const directory = await temporaryDirectory(t);
  const marker = join(directory, "opened.json");
  const opener = join(directory, "xdg-open");
  await writeFile(
    opener,
    `#!${process.execPath}
import { writeFileSync } from 'node:fs';
const url = process.argv[2];
try {
  const response = await fetch(url);
  const body = await response.text();
  writeFileSync(${JSON.stringify(marker)}, JSON.stringify({url, status: response.status, hub: body.includes('Hoke')}));
} catch (error) {
  writeFileSync(${JSON.stringify(marker)}, JSON.stringify({url, error: error.message}));
  process.exitCode = 1;
}
`,
  );
  await chmod(opener, 0o755);
  const child = launch(t, ["--no-build", "--port", "0"], {
    PATH: `${directory}${delimiter}${process.env.PATH ?? ""}`,
  });
  await child.waitFor(/Сервер работает/);
  let contents;
  const deadline = Date.now() + 8000;
  while (contents === undefined) {
    try {
      contents = await readFile(marker, "utf8");
    } catch (error) {
      if (error.code !== "ENOENT" || Date.now() >= deadline) throw error;
      await delay(20);
    }
  }
  const result = JSON.parse(contents);
  assert.match(result.url, /^http:\/\/127\.0\.0\.1:\d+\/$/);
  assert.equal(result.status, 200);
  assert.equal(result.hub, true);
  assert.match(child.output, /Открываем главное меню/);
});

test("SIGINT kills a build that ignores SIGTERM without opening the browser", {
  ...testOptions,
  skip: process.platform !== "linux",
}, async (t) => {
  const directory = await temporaryDirectory(t);
  const fakeBuild = join(directory, "stubborn-build.mjs");
  await writeFile(
    fakeBuild,
    `
process.on('SIGTERM', () => {});
console.log('stubborn-build-ready', process.pid);
setInterval(() => {}, 1000);
`,
  );
  const child = launch(t, ["--port", "0"], { npm_execpath: fakeBuild });
  await child.waitFor(/stubborn-build-ready \d+/);
  const buildPid = Number(/stubborn-build-ready (\d+)/.exec(child.output)[1]);
  t.after(() => {
    try {
      process.kill(-buildPid, "SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  });
  let timeout;
  const deadline = new Promise((_, reject) => {
    timeout = setTimeout(
      () => reject(new Error(`SIGINT did not stop the build:\n${child.output}`)),
      5000,
    );
  });
  child.child.kill("SIGINT");
  try {
    assert.deepEqual(await Promise.race([child.closed, deadline]), { code: 0, signal: null });
  } finally {
    clearTimeout(timeout);
  }
  assert.match(child.output, /Останавливаем демо/);
  assert.doesNotMatch(child.output, /демо готово|Открываем|http:\/\//);
  assert.throws(() => process.kill(buildPid, 0), { code: "ESRCH" });
});

test("a failed browser opener warns and leaves the HTTP demo available", {
  ...testOptions,
  skip: process.platform !== "linux",
}, async (t) => {
  const directory = await temporaryDirectory(t);
  const opener = join(directory, "xdg-open");
  await writeFile(opener, `#!${process.execPath}\nprocess.exit(1);\n`);
  await chmod(opener, 0o755);
  const child = launch(t, ["--no-build", "--port", "0"], {
    PATH: `${directory}${delimiter}${process.env.PATH ?? ""}`,
  });
  await child.waitFor(/Сервер работает/);
  assert.match(child.output, /WARN.*Браузер не удалось открыть автоматически/);
  assert.match(child.output, /Перейдите по ссылке выше/);
  const url = /http:\/\/127\.0\.0\.1:\d+\//.exec(child.output)[0];
  const response = await fetch(url);
  assert.equal(response.status, 200);
  assert.match(await response.text(), /Hoke/);
  child.child.kill("SIGINT");
  assert.deepEqual(await child.closed, { code: 0, signal: null });
});
