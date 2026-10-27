import { test as base, expect, type Page } from "@playwright/test";

const persistedKey = 'stowtree:["store-demo","settings"]';
const absentRecord = "Запись отсутствует — используются defaults.";

// Fail each scenario if the actual module import or an event handler throws.
const test = base.extend<{ pageErrors: Error[] }>({
  pageErrors: [
    async ({ page }, use) => {
      const errors: Error[] = [];
      page.on("pageerror", (error) => errors.push(error));
      await use(errors);
      expect(errors).toEqual([]);
    },
    { auto: true },
  ],
});

const readRaw = (page: Page) => page.evaluate((key) => localStorage.getItem(key), persistedKey);

async function expectDefaults(page: Page) {
  await expect(page.locator("#increment")).toBeEnabled();
  await expect(page.locator("#count")).toHaveText("0");
  await expect(page.locator("#theme")).toHaveValue("system");
  await expect(page.locator("#record")).toHaveText(absentRecord);
  await expect(page.locator("#error")).toBeEmpty();
  expect(await readRaw(page)).toBeNull();
}

async function expectSuspended(page: Page) {
  for (const id of ["theme", "increment", "reset"]) {
    await expect(page.locator(`#${id}`)).toBeDisabled();
  }
}

test("built browser modules persist clicks and theme across a real reload", async ({ page }) => {
  await page.goto("/examples/web/");
  await expectDefaults(page);
  await page.locator("#increment").click();
  await page.locator("#theme").selectOption("dark");
  const expected = { __stowtree: 1, version: 2, value: { theme: "dark", count: 1 } };
  expect(JSON.parse((await readRaw(page)) ?? "null")).toEqual(expected);

  await page.reload();
  await expect(page.locator("#increment")).toBeEnabled();
  await expect(page.locator("#count")).toHaveText("1");
  await expect(page.locator("#theme")).toHaveValue("dark");
  await expect(page.locator("html")).toHaveCSS("color-scheme", "dark");
  await expect(page.locator("#error")).toBeEmpty();
  expect(JSON.parse((await readRaw(page)) ?? "null")).toEqual(expected);
});

test("v1 stays visible after reload until explicitly migrated to v2", async ({ page }) => {
  await page.goto("/examples/web/");
  await page.locator("#legacy").click();
  const legacy = await readRaw(page);
  expect(JSON.parse(legacy ?? "null")).toEqual({
    __stowtree: 1,
    version: 1,
    value: { darkMode: true, count: 7 },
  });
  await expectSuspended(page);
  await expect(page.locator("#migrate")).toBeEnabled();
  expect(JSON.parse((await page.locator("#record").textContent()) ?? "null")).toEqual(
    JSON.parse(legacy ?? "null"),
  );

  await page.reload();
  await expect(page.locator("#migrate")).toBeEnabled();
  await expectSuspended(page);
  expect(await readRaw(page)).toBe(legacy);
  expect(JSON.parse((await page.locator("#record").textContent()) ?? "null")).toEqual(
    JSON.parse(legacy ?? "null"),
  );
  await page.locator("#migrate").click();
  await expect(page.locator("#migrate")).toBeDisabled();
  await expect(page.locator("#increment")).toBeEnabled();
  await expect(page.locator("#count")).toHaveText("7");
  await expect(page.locator("#theme")).toHaveValue("dark");
  await expect(page.locator("#error")).toBeEmpty();
  const migrated = { __stowtree: 1, version: 2, value: { theme: "dark", count: 7 } };
  expect(JSON.parse((await readRaw(page)) ?? "null")).toEqual(migrated);
  expect(JSON.parse((await page.locator("#record").textContent()) ?? "null")).toEqual(migrated);
  await page.reload();
  await expect(page.locator("#count")).toHaveText("7");
  await expect(page.locator("#theme")).toHaveValue("dark");
  expect(JSON.parse((await readRaw(page)) ?? "null")).toEqual(migrated);
});

test("future v3 remains byte-identical across reload and explicit clear recovers", async ({
  page,
}) => {
  await page.goto("/examples/web/");
  await page.locator("#future").click();
  await expect(page.locator("#error")).toContainText("future-version");
  await expectSuspended(page);
  const future = await readRaw(page);
  expect(JSON.parse(future ?? "null")).toEqual({
    __stowtree: 1,
    version: 3,
    value: { theme: "dark", count: 99 },
  });

  await page.reload();
  await expect(page.locator("#error")).toContainText("future-version");
  await expectSuspended(page);
  await expect(page.locator("#migrate")).toBeDisabled();
  expect(await readRaw(page)).toBe(future);
  expect(JSON.parse((await page.locator("#record").textContent()) ?? "null")).toEqual(
    JSON.parse(future ?? "null"),
  );
  await page.locator("#clear").click();
  await expectDefaults(page);
  await page.locator("#increment").click();
  expect(JSON.parse((await readRaw(page)) ?? "null")).toEqual({
    __stowtree: 1,
    version: 2,
    value: { theme: "system", count: 1 },
  });
});

for (const button of ["reset", "clear"]) {
  test(`${button} removes the complete envelope; reload stays empty and next write uses v2`, async ({
    page,
  }) => {
    await page.goto("/examples/web/");
    await page.locator("#increment").click();
    await page.locator("#theme").selectOption("dark");
    expect(await readRaw(page)).not.toBeNull();
    await page.locator(`#${button}`).click();
    await expectDefaults(page);
    await page.reload();
    await expectDefaults(page);
    await page.locator("#increment").click();
    expect(JSON.parse((await readRaw(page)) ?? "null")).toEqual({
      __stowtree: 1,
      version: 2,
      value: { theme: "system", count: 1 },
    });
  });
}

test("corrupt JSON is preserved across reload and can be explicitly cleared", async ({ page }) => {
  await page.goto("/examples/web/");
  await expectDefaults(page);
  await page.evaluate((key) => localStorage.setItem(key, "{broken"), persistedKey);
  await page.reload();
  await expect(page.locator("#error")).toContainText("INVALID_DATA");
  await expectSuspended(page);
  expect(await readRaw(page)).toBe("{broken");
  await page.reload();
  await expect(page.locator("#error")).toContainText("INVALID_DATA");
  await expectSuspended(page);
  expect(await readRaw(page)).toBe("{broken");
  await page.locator("#clear").click();
  await expectDefaults(page);
});

for (const failure of ["missing", "security-error"] as const) {
  test(`unavailable localStorage (${failure}) reports an error without memory fallback`, async ({
    page,
  }) => {
    await page.addInitScript((kind) => {
      Object.defineProperty(window, "localStorage", {
        configurable: true,
        get() {
          if (kind === "security-error") throw new DOMException("Blocked", "SecurityError");
          return undefined;
        },
      });
    }, failure);
    await page.goto("/examples/web/");
    await expect(page.locator("#error")).toContainText("UNAVAILABLE");
    await expectSuspended(page);
    for (const id of ["legacy", "migrate", "future", "clear"]) {
      await expect(page.locator(`#${id}`)).toBeDisabled();
    }
    await expect(page.locator("#count")).toHaveText("0");
    await page.reload();
    await expect(page.locator("#error")).toContainText("UNAVAILABLE");
    await expectSuspended(page);
  });
}

test("SecurityError from a storage read is reported without memory fallback", async ({ page }) => {
  await page.addInitScript(() => {
    Storage.prototype.getItem = () => {
      throw new DOMException("Blocked", "SecurityError");
    };
  });
  await page.goto("/examples/web/");
  await expect(page.locator("#error")).toContainText("READ_FAILED");
  await expectSuspended(page);
  await expect(page.locator("#migrate")).toBeDisabled();
  await expect(page.locator("#count")).toHaveText("0");
  await page.reload();
  await expect(page.locator("#error")).toContainText("READ_FAILED");
  await expectSuspended(page);
});
