import { expect, test } from "@playwright/test";

test("OPFS demo saves through WASM and reads after close, reopen and reload", async ({ page }) => {
  const errors: Error[] = [];
  page.on("pageerror", (error) => errors.push(error));
  await page.goto("/examples/opfs/");
  await page.locator("#open").click();
  await expect(page.locator("#status")).toContainText("wal-v1");
  await page.locator("#value").fill("saved-by-rust");
  await page.locator("#put").click();
  await expect(page.locator("#result")).toContainText("подтверждена");
  await page.locator("#close").click();
  await expect(page.locator("#open")).toBeEnabled();
  await page.locator("#open").click();
  await expect(page.locator("#get")).toBeEnabled();
  await page.locator("#get").click();
  await expect(page.locator("#result")).toHaveText("saved-by-rust");
  await page.reload();
  await page.locator("#open").click();
  await expect(page.locator("#get")).toBeEnabled();
  await page.locator("#get").click();
  await expect(page.locator("#result")).toHaveText("saved-by-rust");
  await page.locator("#delete").click();
  await expect(page.locator("#result")).toContainText("Удаление подтверждено");
  await page.locator("#get").click();
  await expect(page.locator("#result")).toHaveText("Ключ отсутствует.");
  await expect(page.locator("#error")).toBeEmpty();
  expect(errors).toEqual([]);
});

test("OPFS demo lists live records with prefix, reverse and limit after reload", async ({
  page,
}) => {
  await page.goto("/examples/opfs/");
  await page.locator("#open").click();
  await expect(page.locator("#scan")).toBeEnabled();
  for (const [key, value] of [
    ["user:1", "Alice"],
    ["user:2", "Bob"],
    ["other", "outside"],
  ]) {
    await page.locator("#key").fill(key ?? "");
    await page.locator("#value").fill(value ?? "");
    await page.locator("#put").click();
    await expect(page.locator("#put")).toBeEnabled();
  }
  await page.reload();
  await page.locator("#open").click();
  await expect(page.locator("#scan")).toBeEnabled();
  await page.locator("#prefix").fill("user:");
  await page.locator("#reverse").check();
  await page.locator("#limit").fill("1");
  await page.locator("#scan").click();
  await expect(page.locator("#entries")).toHaveText(
    JSON.stringify([{ key: "user:2", value: "Bob" }], null, 2),
  );
  await page.locator("#key").fill("user:2");
  await page.locator("#delete").click();
  await expect(page.locator("#delete")).toBeEnabled();
  await page.locator("#scan").click();
  await expect(page.locator("#entries")).toHaveText(
    JSON.stringify([{ key: "user:1", value: "Alice" }], null, 2),
  );
  await expect(page.locator("#error")).toBeEmpty();
});

test("OPFS demo counts only confirmed mutations of the watched prefix and stops on close", async ({
  page,
}) => {
  await page.goto("/examples/opfs/");
  await page.locator("#open").click();
  await expect(page.locator("#watch")).toBeEnabled();
  await page.locator("#prefix").fill("user:");
  await page.locator("#watch").click();
  await expect(page.locator("#notifications")).toHaveText("0");
  await page.locator("#key").fill("other");
  await page.locator("#put").click();
  await expect(page.locator("#put")).toBeEnabled();
  await expect(page.locator("#notifications")).toHaveText("0");
  await page.locator("#key").fill("user:1");
  await page.locator("#put").click();
  await expect(page.locator("#notifications")).toHaveText("1");
  await page.locator("#get").click();
  await expect(page.locator("#get")).toBeEnabled();
  await expect(page.locator("#notifications")).toHaveText("1");
  await page.locator("#delete").click();
  await expect(page.locator("#notifications")).toHaveText("2");
  await page.locator("#unwatch").click();
  await page.locator("#put").click();
  await expect(page.locator("#put")).toBeEnabled();
  await expect(page.locator("#notifications")).toHaveText("2");
  await page.locator("#watch").click();
  await expect(page.locator("#notifications")).toHaveText("0");
  await page.locator("#close").click();
  await expect(page.locator("#watch-status")).toHaveText("Подписка выключена.");
  await page.locator("#open").click();
  await expect(page.locator("#put")).toBeEnabled();
  await page.locator("#put").click();
  await expect(page.locator("#put")).toBeEnabled();
  await expect(page.locator("#notifications")).toHaveText("0");
  await expect(page.locator("#error")).toBeEmpty();
});

test("OPFS demo cancels an opening worker and can open again without an automatic retry", async ({
  page,
}) => {
  const errors: Error[] = [];
  page.on("pageerror", (error) => errors.push(error));
  let release = () => {};
  let reached = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const wasmRequested = new Promise<void>((resolve) => {
    reached = resolve;
  });
  await page.route(
    "**/hoke_wasm_bg.wasm",
    async (route) => {
      reached();
      await gate;
      await route.continue();
    },
    { times: 1 },
  );
  try {
    await page.goto("/examples/opfs/");
    await expect(page.locator("#cancel")).toBeDisabled();
    await page.locator("#open").click();
    await wasmRequested;
    await expect(page.locator("#cancel")).toBeEnabled();
    await page.locator("#cancel").click();
    await expect(page.locator("#error")).toContainText("ABORTED");
    await expect(page.locator("#status")).toContainText("Откройте базу снова и проверьте данные");
    await expect(page.locator("#open")).toBeEnabled();
    await expect(page.locator("#put")).toBeDisabled();
    await expect(page.locator("#watch-status")).toHaveText("Подписка выключена.");
    await expect(page.locator("#cancel")).toBeDisabled();
    release();
    await page.locator("#open").click();
    await expect(page.locator("#status")).toContainText("wal-v1");
    await expect(page.locator("#cancel")).toBeDisabled();
    await page.locator("#value").fill("after-abort");
    await page.locator("#put").click();
    await expect(page.locator("#put")).toBeEnabled();
    await page.locator("#get").click();
    await expect(page.locator("#result")).toHaveText("after-abort");
    await expect(page.locator("#error")).toBeEmpty();
    expect(errors).toEqual([]);
  } finally {
    release();
  }
});
