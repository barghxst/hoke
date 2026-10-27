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
