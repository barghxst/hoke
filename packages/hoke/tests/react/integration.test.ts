// @vitest-environment jsdom

import { act, createElement } from "react";
import type { Root } from "react-dom/client";
import { createRoot, hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import { createStore, defineStore, KV, LocalStorageBackend } from "../../src/kv/index.js";
import { useStore, useValue } from "../../src/react/index.js";
import { MemoryBackend } from "../helpers/memory-backend.js";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

interface Settings {
  count: number;
  theme: string;
}
const settings = defineStore<Settings>({
  key: "settings",
  version: 1,
  defaults: () => ({ count: 0, theme: "light" }),
  validate: (value): value is Settings =>
    value !== null &&
    typeof value === "object" &&
    typeof (value as Settings).count === "number" &&
    typeof (value as Settings).theme === "string",
});
function openPersistedStore() {
  const kv = new KV({
    name: "react-integration",
    backend: new LocalStorageBackend({ storage: localStorage }),
  });
  return { kv, store: createStore(kv, settings) };
}

const roots: Root[] = [];
afterEach(() => {
  act(() => {
    for (const root of roots) root.unmount();
  });
  roots.length = 0;
  document.body.replaceChildren();
  localStorage.clear();
  vi.restoreAllMocks();
});

it("connects selectors to persistent snapshots, reloads updates and resets to stable defaults", () => {
  const error = vi.spyOn(console, "error");
  const warning = vi.spyOn(console, "warn");
  const { kv, store } = openPersistedStore();
  const defaults = store.getSnapshot();
  let renders = 0;
  function Counter() {
    const count = useStore(store, (value) => value.count);
    renders++;
    return createElement("span", null, count);
  }
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  roots.push(root);
  act(() => root.render(createElement(Counter)));
  const initialRenders = renders;
  act(() => store.update((previous) => ({ ...previous, theme: "dark" })));
  expect(renders).toBe(initialRenders);
  expect(store.getSnapshot()).toBe(store.getSnapshot());
  act(() => store.update((previous) => ({ ...previous, count: previous.count + 1 })));
  expect(container.textContent).toBe("1");
  expect(renders).toBe(initialRenders + 1);

  const reopened = openPersistedStore().store;
  expect(reopened.getSnapshot()).toEqual({ count: 1, theme: "dark" });
  expect(reopened.getSnapshot()).toBe(reopened.getSnapshot());

  act(() => store.reset());
  expect(container.textContent).toBe("0");
  expect(store.getSnapshot()).toBe(defaults);
  expect(kv.get(settings.key)).toBeUndefined();
  expect(openPersistedStore().store.getSnapshot()).toEqual(defaults);
  expect(error).not.toHaveBeenCalled();
  expect(warning).not.toHaveBeenCalled();
});

it("hydrates matching real Store defaults before exposing persisted client state", () => {
  const error = vi.spyOn(console, "error");
  const serverStore = createStore(
    new KV({ name: "server", backend: new MemoryBackend() }),
    settings,
  );
  const { store: clientStore } = openPersistedStore();
  clientStore.set({ count: 9, theme: "dark" });
  function Value({ store }: { store: typeof serverStore }) {
    return createElement("span", null, useValue(store).count);
  }
  const container = document.createElement("div");
  container.innerHTML = renderToString(createElement(Value, { store: serverStore }));
  expect(container.textContent).toBe("0");
  expect(clientStore.getServerSnapshot()).toEqual(serverStore.getServerSnapshot());
  document.body.append(container);
  act(() => {
    roots.push(hydrateRoot(container, createElement(Value, { store: clientStore })));
  });
  expect(container.textContent).toBe("9");
  act(() => clientStore.update((previous) => ({ ...previous, count: 10 })));
  expect(container.textContent).toBe("10");
  expect(openPersistedStore().store.getSnapshot().count).toBe(10);
  expect(error).not.toHaveBeenCalled();
});
