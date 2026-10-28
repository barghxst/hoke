import { expect, test } from "@playwright/test";

test("document demo adds, toggles, persists and deletes a task", async ({ page }) => {
  const errors: Error[] = [];
  page.on("pageerror", (error) => errors.push(error));
  await page.goto("/examples/documents/");
  await expect(page.getByLabel("Новая задача")).toBeEnabled();
  await page.getByLabel("Новая задача").fill("Разобраться с Hoke");
  await page.getByRole("button", { name: "Добавить", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Задача сохранена.");
  const done = page.getByRole("checkbox", { name: "Выполнено: Разобраться с Hoke" });
  await done.check();
  await expect(page.getByRole("status")).toHaveText("Задача обновлена.");
  await page.reload();
  await expect(done).toBeChecked();
  await page.getByRole("button", { name: "Удалить: Разобраться с Hoke" }).click();
  await expect(page.getByRole("status")).toHaveText("Задача удалена.");
  await expect(page.locator("#empty")).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Новая задача")).toBeEnabled();
  await expect(page.locator("#tasks li")).toHaveCount(0);
  await expect(page.getByRole("alert")).toBeEmpty();
  expect(errors).toEqual([]);
});

test("document demo saves an atomic pair and renders stored text safely", async ({ page }) => {
  const errors: Error[] = [];
  page.on("pageerror", (error) => errors.push(error));
  await page.goto("/examples/documents/");
  await page.getByRole("button", { name: "Добавить две задачи атомарно" }).click();
  await expect(page.getByRole("status")).toHaveText(
    "Обе задачи сохранены одной атомарной группой.",
  );
  await expect(page.locator("#tasks li")).toHaveCount(2);
  await page.getByLabel("Новая задача").fill('<img src=x onerror="window.injected=true">');
  await page.getByRole("button", { name: "Добавить", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Задача сохранена.");
  await page.reload();
  await expect(page.locator("#tasks li")).toHaveCount(3);
  await expect(page.locator("#tasks")).toContainText("Изучить Hoke");
  await expect(page.locator("#tasks")).toContainText("Собрать приложение");
  await expect(page.locator("#tasks")).toContainText('<img src=x onerror="window.injected=true">');
  await expect(page.locator("#tasks img")).toHaveCount(0);
  await expect(page.getByRole("alert")).toBeEmpty();
  expect(errors).toEqual([]);
});
