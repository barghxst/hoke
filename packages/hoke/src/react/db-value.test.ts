// @vitest-environment jsdom

import { act, createElement, StrictMode } from "react";
import type { Root } from "react-dom/client";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import type { Collection } from "../db/document-contract.js";
import { type DBValueState, useDBValue } from "./db-value.js";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

function fakeCollection() {
  const listeners = new Map<string, Set<() => void>>();
  const reads: {
    key: string;
    resolve(value: string | undefined): void;
    reject(error: unknown): void;
  }[] = [];
  const get = vi.fn(
    (key: string) =>
      new Promise<string | undefined>((resolve, reject) => reads.push({ key, resolve, reject })),
  );
  const subscribe = vi.fn((key: string, listener: () => void) => {
    let matching = listeners.get(key);
    if (!matching) {
      matching = new Set();
      listeners.set(key, matching);
    }
    matching.add(listener);
    return () => matching.delete(listener);
  });
  const collection = { get, subscribe } as unknown as Collection<string>;
  return {
    collection,
    get,
    subscribe,
    reads,
    count: () => [...listeners.values()].reduce((total, group) => total + group.size, 0),
    emit: (key: string) => {
      for (const listener of listeners.get(key) ?? []) listener();
    },
  };
}

const roots: Root[] = [];
const states: DBValueState<string>[] = [];
function Value({ collection, itemKey }: { collection: Collection<string>; itemKey: string }) {
  const state = useDBValue(collection, itemKey);
  states.push(state);
  return createElement("span", null, `${state.status}:${state.value ?? "missing"}`);
}
async function mount(collection: Collection<string>, itemKey = "a", strict = false) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  roots.push(root);
  await act(async () => {
    const child = createElement(Value, { collection, itemKey });
    root.render(strict ? createElement(StrictMode, null, child) : child);
  });
  return { container, root };
}

afterEach(() => {
  act(() => {
    for (const root of roots) root.unmount();
  });
  roots.length = 0;
  states.length = 0;
  document.body.replaceChildren();
});

it("loads asynchronously, subscribes before reading, and reloads committed changes", async () => {
  const fake = fakeCollection();
  const { container } = await mount(fake.collection);
  expect(container.textContent).toBe("loading:missing");
  expect(fake.subscribe.mock.invocationCallOrder[0]).toBeLessThan(
    fake.get.mock.invocationCallOrder[0] ?? 0,
  );
  await act(async () => fake.reads[0]?.resolve("first"));
  expect(container.textContent).toBe("success:first");
  await act(async () => fake.emit("a"));
  expect(container.textContent).toBe("loading:missing");
  await act(async () => fake.reads[1]?.resolve(undefined));
  expect(states.at(-1)).toEqual({ status: "success", value: undefined, error: undefined });
});

it("does not miss a commit while the first read is being dispatched", async () => {
  const fake = fakeCollection();
  const original = fake.get.getMockImplementation();
  fake.get.mockImplementationOnce((key) => {
    fake.emit(key);
    return original?.(key) ?? Promise.resolve(undefined);
  });
  const { container } = await mount(fake.collection);
  expect(fake.reads).toHaveLength(2);
  await act(async () => fake.reads[1]?.resolve("committed"));
  await act(async () => fake.reads[0]?.resolve("before commit"));
  expect(container.textContent).toBe("success:committed");
});

it("reports rejected reads and recovers on the next commit", async () => {
  const fake = fakeCollection();
  await mount(fake.collection);
  const error = new Error("closed");
  await act(async () => fake.reads[0]?.reject(error));
  expect(states.at(-1)).toEqual({ status: "error", value: undefined, error });
  await act(async () => fake.emit("a"));
  await act(async () => fake.reads[1]?.resolve("recovered"));
  expect(states.at(-1)?.value).toBe("recovered");
});

it("ignores both stale values and stale failures after invalidation", async () => {
  const fake = fakeCollection();
  const { container } = await mount(fake.collection);
  await act(async () => fake.emit("a"));
  await act(async () => fake.reads[1]?.resolve("new"));
  await act(async () => fake.reads[0]?.resolve("old"));
  expect(container.textContent).toBe("success:new");
  await act(async () => fake.emit("a"));
  await act(async () => fake.emit("a"));
  await act(async () => fake.reads[3]?.resolve("newest"));
  await act(async () => fake.reads[2]?.reject(new Error("stale")));
  expect(container.textContent).toBe("success:newest");
});

it("hides the previous key's value on the very first render after switching", async () => {
  const fake = fakeCollection();
  const { root } = await mount(fake.collection);
  await act(async () => fake.reads[0]?.resolve("old"));
  states.length = 0;
  await act(async () =>
    root.render(createElement(Value, { collection: fake.collection, itemKey: "b" })),
  );
  expect(states[0]).toEqual({ status: "loading", value: undefined, error: undefined });
  expect(fake.count()).toBe(1);
  expect(fake.reads[1]?.key).toBe("b");
  await act(async () => fake.reads[1]?.resolve("new"));
  expect(states.at(-1)?.value).toBe("new");
});

it("ignores in-flight reads from a replaced collection and removes its listener", async () => {
  const first = fakeCollection();
  const second = fakeCollection();
  const { root, container } = await mount(first.collection);
  await act(async () =>
    root.render(createElement(Value, { collection: second.collection, itemKey: "a" })),
  );
  expect(first.count()).toBe(0);
  expect(second.count()).toBe(1);
  await act(async () => second.reads[0]?.resolve("current"));
  await act(async () => first.reads[0]?.reject(new Error("old connection")));
  expect(container.textContent).toBe("success:current");
});

it("cleans up subscriptions and ignores pending completion in StrictMode", async () => {
  const fake = fakeCollection();
  const { root } = await mount(fake.collection, "a", true);
  expect(fake.count()).toBe(1);
  await act(async () => root.render(null));
  expect(fake.count()).toBe(0);
  states.length = 0;
  await act(async () => {
    for (const read of fake.reads) read.reject(new Error("after unmount"));
  });
  expect(states).toEqual([]);
});

it("captures synchronous read and subscribe errors without unhandled rejections", async () => {
  const readFailure = fakeCollection();
  const error = new Error("sync read");
  readFailure.get.mockImplementation(() => {
    throw error;
  });
  await mount(readFailure.collection);
  expect(states.at(-1)?.error).toBe(error);

  const subscribeFailure = fakeCollection();
  const subscriptionError = new Error("sync subscribe");
  subscribeFailure.subscribe.mockImplementation(() => {
    throw subscriptionError;
  });
  await mount(subscribeFailure.collection);
  expect(states.at(-1)?.error).toBe(subscriptionError);
  expect(subscribeFailure.get).not.toHaveBeenCalled();
});

it("renders stable loading on the server without any reads or subscriptions", () => {
  const fake = fakeCollection();
  expect(renderToString(createElement(Value, { collection: fake.collection, itemKey: "a" }))).toBe(
    "<span>loading:missing</span>",
  );
  expect(fake.get).not.toHaveBeenCalled();
  expect(fake.subscribe).not.toHaveBeenCalled();
});
