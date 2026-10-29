// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  createStore,
  defineStore,
  KV,
  LocalStorageBackend,
} from "../../packages/hoke/src/kv/index.js";

const html = readFileSync("examples/web/index.html", "utf8");
const inline = html.match(/<script type="module">([\s\S]*?)<\/script>/)?.[1];
if (!inline) throw new Error("Demo module script missing");
// Exercise the actual example script with its imports supplied from source.
const run = new Function(
  "KV",
  "LocalStorageBackend",
  "createStore",
  "defineStore",
  inline.replace(/^\s*import\s+[\s\S]*?\s+from\s+["'][^"']+["'];?\s*/m, ""),
);
const persistedKey = 'stowtree:["store-demo","settings"]';
const element = (id: string) => document.getElementById(id)!;
function mount() {
  const page = new DOMParser().parseFromString(html, "text/html");
  document.body.innerHTML = page.body.innerHTML;
  run(KV, LocalStorageBackend, createStore, defineStore);
}
function click(id: string) {
  (element(id) as HTMLButtonElement).click();
}

beforeEach(() => localStorage.clear());
afterEach(() => {
  localStorage.clear();
  document.body.replaceChildren();
});

describe("shipped web example", () => {
  it.each(["clear", "reset"])(
    "%s removes the complete versioned record; the next write uses the code's schema version",
    (button) => {
      mount();
      click("increment");
      expect(JSON.parse(localStorage.getItem(persistedKey)!).version).toBe(2);

      click(button);
      expect(localStorage.getItem(persistedKey)).toBeNull();
      expect(element("record").textContent).toBe("Запись отсутствует — используются defaults.");
      expect(element("count").textContent).toBe("0");

      mount();
      expect(localStorage.getItem(persistedKey)).toBeNull();
      expect(element("record").textContent).toBe("Запись отсутствует — используются defaults.");

      click("increment");
      expect(JSON.parse(localStorage.getItem(persistedKey)!)).toEqual({
        __stowtree: 1,
        version: 2,
        value: { theme: "system", count: 1 },
      });
    },
  );

  it("persists updates across reload, migrates v1, protects v3 and explicitly recovers", () => {
    mount();
    expect(localStorage.length).toBe(0);
    expect(element("count").textContent).toBe("0");
    click("increment");
    expect(element("count").textContent).toBe("1");
    const theme = element("theme") as HTMLSelectElement;
    theme.value = "dark";
    theme.dispatchEvent(new Event("change"));
    mount();
    expect(element("count").textContent).toBe("1");
    expect((element("theme") as HTMLSelectElement).value).toBe("dark");

    click("legacy");
    expect(JSON.parse(localStorage.getItem(persistedKey)!)).toEqual({
      __stowtree: 1,
      version: 1,
      value: { darkMode: true, count: 7 },
    });
    expect(JSON.parse(element("record").textContent!)).toEqual(
      JSON.parse(localStorage.getItem(persistedKey)!),
    );
    expect((element("increment") as HTMLButtonElement).disabled).toBe(true);
    expect((element("migrate") as HTMLButtonElement).disabled).toBe(false);
    mount();
    expect(JSON.parse(localStorage.getItem(persistedKey)!).version).toBe(1);
    expect(JSON.parse(element("record").textContent!).version).toBe(1);
    click("migrate");
    expect(element("error").textContent).toBe("");
    expect(element("count").textContent).toBe("7");
    expect(JSON.parse(localStorage.getItem(persistedKey)!)).toEqual({
      __stowtree: 1,
      version: 2,
      value: { theme: "dark", count: 7 },
    });
    expect(JSON.parse(element("record").textContent!).version).toBe(2);
    expect((element("migrate") as HTMLButtonElement).disabled).toBe(true);
    click("future");
    expect(element("error").textContent).toContain("future-version");
    expect((element("increment") as HTMLButtonElement).disabled).toBe(true);
    expect(JSON.parse(localStorage.getItem(persistedKey)!).version).toBe(3);
    click("clear");
    expect(element("error").textContent).toBe("");
    expect(element("count").textContent).toBe("0");
    expect(localStorage.getItem(persistedKey)).toBeNull();
    click("increment");
    click("reset");
    expect(element("count").textContent).toBe("0");
    expect(localStorage.length).toBe(0);
  });

  it("shows corrupt JSON without silently overwriting the record", () => {
    localStorage.setItem(persistedKey, "{broken");
    mount();
    expect(element("error").textContent).toContain("INVALID_DATA");
    expect((element("increment") as HTMLButtonElement).disabled).toBe(true);
    expect(localStorage.getItem(persistedKey)).toBe("{broken");
    localStorage.setItem("other-app", "keep");
    click("clear");
    expect(element("error").textContent).toBe("");
    expect(element("count").textContent).toBe("0");
    expect(localStorage.getItem(persistedKey)).toBeNull();
    expect(localStorage.getItem("other-app")).toBe("keep");
    mount();
    expect(element("count").textContent).toBe("0");
  });
});
