// @vitest-environment jsdom

import { act, createElement, StrictMode } from "react";
import type { Root } from "react-dom/client";
import { createRoot, hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import type { Store } from "../store.js";
import { useStore, useValue } from "./index.js";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

function fakeStore<T>(defaults: T, initial = defaults) {
  let value = initial;
  const listeners = new Set<() => void>();
  const store: Store<T> = {
    getSnapshot: () => value,
    getServerSnapshot: () => defaults,
    set(next) {
      value = next;
      for (const listener of listeners) listener();
    },
    update(updater) {
      store.set(updater(value));
    },
    reset() {
      store.set(defaults);
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
  return { store, listeners };
}

const roots: Root[] = [];
function mount(component: ReturnType<typeof createElement>) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  roots.push(root);
  act(() => root.render(component));
  return { container, root };
}

afterEach(() => {
  act(() => {
    for (const root of roots) root.unmount();
  });
  roots.length = 0;
  document.body.replaceChildren();
});

describe("React store hooks", () => {
  it("reads and updates complete snapshots", () => {
    const { store } = fakeStore({ count: 0 });
    function Counter() {
      return createElement("span", null, useValue(store).count);
    }
    const { container } = mount(createElement(Counter));
    expect(container.textContent).toBe("0");
    act(() => store.update((previous) => ({ count: previous.count + 1 })));
    expect(container.textContent).toBe("1");
    act(() => store.reset());
    expect(container.textContent).toBe("0");
  });

  it("does not render when an unselected field changes", () => {
    const { store } = fakeStore({ count: 0, theme: "light" });
    let renders = 0;
    function Counter() {
      const count = useStore(store, (value) => value.count);
      renders++;
      return createElement("span", null, count);
    }
    const { container } = mount(createElement(Counter));
    const initialRenders = renders;
    act(() => store.set({ count: 0, theme: "dark" }));
    expect(renders).toBe(initialRenders);
    act(() => store.set({ count: 1, theme: "dark" }));
    expect(renders).toBe(initialRenders + 1);
    expect(container.textContent).toBe("1");
  });

  it("stabilizes newly allocated selector outputs using a comparator", () => {
    const { store } = fakeStore({ count: 0, theme: "light" });
    let renders = 0;
    const selections: { count: number }[] = [];
    function Counter() {
      const selected = useStore(
        store,
        (value) => ({ count: value.count }),
        (a, b) => a.count === b.count,
      );
      selections.push(selected);
      renders++;
      return createElement("span", null, selected.count);
    }
    const { root } = mount(createElement(Counter));
    const first = selections[0];
    const initialRenders = renders;
    act(() => store.set({ count: 0, theme: "dark" }));
    expect(renders).toBe(initialRenders);
    act(() => root.render(createElement(Counter)));
    expect(selections.at(-1)).toBe(first);
    act(() => store.set({ count: 1, theme: "dark" }));
    expect(selections.at(-1)).not.toBe(first);
  });

  it("uses the latest selector when props change", () => {
    const { store } = fakeStore({ left: 1, right: 2 });
    function Field({ field }: { field: "left" | "right" }) {
      return createElement(
        "span",
        null,
        useStore(store, (value) => value[field]),
      );
    }
    const { root, container } = mount(createElement(Field, { field: "left" }));
    expect(container.textContent).toBe("1");
    act(() => root.render(createElement(Field, { field: "right" })));
    expect(container.textContent).toBe("2");
    act(() => store.set({ left: 3, right: 4 }));
    expect(container.textContent).toBe("4");
  });

  it("switches stores and removes the old subscription", () => {
    const first = fakeStore(1);
    const second = fakeStore(2);
    function Value({ store }: { store: Store<number> }) {
      return createElement("span", null, useValue(store));
    }
    const { root, container } = mount(createElement(Value, { store: first.store }));
    expect(first.listeners.size).toBe(1);
    act(() => root.render(createElement(Value, { store: second.store })));
    expect(container.textContent).toBe("2");
    expect(first.listeners.size).toBe(0);
    expect(second.listeners.size).toBe(1);
    act(() => first.store.set(3));
    expect(container.textContent).toBe("2");
    act(() => second.store.set(4));
    expect(container.textContent).toBe("4");
  });

  it("uses the latest equality function", () => {
    const { store } = fakeStore(1);
    const selector = (value: number) => ({ value });
    const equal = () => true;
    const different = () => false;
    function Value({
      compare,
    }: {
      compare: (a: { value: number }, b: { value: number }) => boolean;
    }) {
      return createElement("span", null, useStore(store, selector, compare).value);
    }
    const { root, container } = mount(createElement(Value, { compare: equal }));
    act(() => store.set(2));
    expect(container.textContent).toBe("1");
    act(() => root.render(createElement(Value, { compare: different })));
    expect(container.textContent).toBe("2");
    act(() => store.set(3));
    expect(container.textContent).toBe("3");
  });

  it("updates multiple consumers and unsubscribes on unmount in StrictMode", () => {
    const { store, listeners } = fakeStore(0);
    function Value() {
      return createElement("span", null, useValue(store));
    }
    const { root, container } = mount(
      createElement(StrictMode, null, createElement(Value), createElement(Value)),
    );
    expect(listeners.size).toBe(2);
    act(() => store.set(5));
    expect(container.textContent).toBe("55");
    act(() => root.render(null));
    expect(listeners.size).toBe(0);
    act(() => store.set(6));
    expect(container.textContent).toBe("");
  });

  it("uses Object.is by default, including NaN", () => {
    const { store } = fakeStore({ value: NaN });
    let renders = 0;
    function Value() {
      const value = useStore(store, (snapshot) => snapshot.value);
      renders++;
      return createElement("span", null, String(value));
    }
    mount(createElement(Value));
    const initialRenders = renders;
    act(() => store.set({ value: NaN }));
    expect(renders).toBe(initialRenders);
  });

  it("renders server defaults without subscribing or reading client snapshots", () => {
    const { store, listeners } = fakeStore({ count: 1 }, { count: 9 });
    store.getSnapshot = () => {
      throw new Error("client snapshot during SSR");
    };
    function Value() {
      return createElement(
        "span",
        null,
        useStore(store, (value) => value.count),
      );
    }
    expect(renderToString(createElement(Value))).toBe("<span>1</span>");
    expect(listeners.size).toBe(0);
  });

  it("hydrates from server defaults then updates to persisted client state", () => {
    const { store } = fakeStore({ count: 1 }, { count: 9 });
    function Value() {
      return createElement("span", null, useValue(store).count);
    }
    const container = document.createElement("div");
    container.innerHTML = renderToString(createElement(Value));
    expect(container.textContent).toBe("1");
    document.body.append(container);
    act(() => {
      roots.push(hydrateRoot(container, createElement(Value)));
    });
    expect(container.textContent).toBe("9");
    act(() => store.set({ count: 10 }));
    expect(container.textContent).toBe("10");
  });
});
