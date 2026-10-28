import { expect, test } from "@playwright/test";

test("demo hub opens each interactive example", async ({ page }) => {
  await page.goto("/examples/");
  await expect(page).toHaveTitle("Hoke — демо");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Данные остаются в браузере.");
  const demos = [
    { name: "Открыть список задач →", path: "/examples/documents/", current: "Задачи" },
    { name: "Открыть настройки →", path: "/examples/web/", current: "Настройки" },
    { name: "Открыть базу →", path: "/examples/opfs/", current: "Байтовая база" },
  ];
  for (const demo of demos) {
    await page.getByRole("link", { name: demo.name, exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`${demo.path}$`));
    const navigation = page.getByRole("navigation", { name: "Демонстрации", exact: true });
    await expect(navigation.getByRole("link", { name: demo.current, exact: true })).toHaveAttribute(
      "aria-current",
      "page",
    );
    await expect(navigation.getByRole("link", { name: "Все демо", exact: true })).toHaveAttribute(
      "href",
      "/",
    );
    await page.goto("/examples/");
  }
});

test("tasks demo provides working navigation back to the demo entry point", async ({ page }) => {
  await page.goto("/examples/documents/");
  await page.getByRole("link", { name: "Все демо", exact: true }).click();
  await expect(page).toHaveURL(/\/$/);
  const path = new URL(page.url()).pathname;
  expect(path).toBe("/");
  await expect(page).toHaveTitle("Hoke — демо");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Данные остаются в браузере.");
});
