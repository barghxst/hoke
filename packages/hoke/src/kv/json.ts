import { observePromise } from "../sync-result.js";

/** Reject values that JSON would silently alter or omit. */
export function assertJSON(value: unknown, ancestors = new Set<object>()): void {
  if (value === null || typeof value === "string" || typeof value === "boolean") return;
  if (typeof value === "number" && Number.isFinite(value) && !Object.is(value, -0)) return;
  if (typeof value !== "object" || value === null) {
    throw new TypeError(
      "Expected JSON data (finite numbers, strings, booleans, null, arrays or plain objects)",
    );
  }
  if (ancestors.has(value)) throw new TypeError("Circular JSON data");
  const array = Array.isArray(value);
  if (
    !array &&
    Object.getPrototypeOf(value) !== Object.prototype &&
    Object.getPrototypeOf(value) !== null
  ) {
    observePromise(value);
    throw new TypeError("Only plain objects and arrays can be persisted");
  }
  ancestors.add(value);
  try {
    const keys = Reflect.ownKeys(value);
    if (array && Object.keys(value).length !== value.length) {
      throw new TypeError("Sparse arrays and custom array properties cannot be persisted");
    }
    for (const key of keys) {
      if (array && key === "length") continue;
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor) throw new TypeError("Object properties changed during serialization");
      if (typeof key === "symbol" || !descriptor.enumerable || !("value" in descriptor)) {
        throw new TypeError("Symbol keys, hidden properties and accessors cannot be persisted");
      }
      if (array && (!/^(0|[1-9]\d*)$/.test(key) || Number(key) >= value.length)) {
        throw new TypeError("Custom array properties cannot be persisted");
      }
      assertJSON(descriptor.value, ancestors);
    }
  } finally {
    ancestors.delete(value);
  }
}
